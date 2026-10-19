use crate::{
    documents::{TextDocument, Unit},
    filesystem::{Result, error, number},
};
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId, content::Content};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
    ops::Range,
    time::Instant,
};

pub(crate) const PDF_LIMIT: u64 = 512 * 1024 * 1024;
pub(crate) const PDF_ENCRYPTED_LIMIT: u64 = 256 * 1024 * 1024;
pub(crate) const PDF_STREAM_LIMIT: usize = 64 * 1024 * 1024;
pub(crate) const PDF_CONTENT_LIMIT: usize = 2 * 1024 * 1024;
pub(crate) const TOUNICODE_CODE_LIMIT: u64 = 65_536;
pub(crate) const TOUNICODE_PAGE_CODE_LIMIT: u64 = 4 * TOUNICODE_CODE_LIMIT;
pub(crate) const PDF_PAGE_WORK_LIMIT: u64 = 1 << 26; // C0 calibration: distinct-glyph Differences exceeded 2.5 s.
pub(crate) const PDF_PAGE_GLYPH_LIMIT: u64 = 4 * 1024 * 1024;
pub(crate) const PDF_DEADLINE_CHECK_UNITS: u64 = 65_536;
pub(crate) const PDF_Q_DEPTH_LIMIT: usize = 64;
pub(crate) const PDF_PAGE_FONT_LIMIT: usize = 1024;
const ENCODING_HOP_LIMIT: usize = 8;
pub(crate) const SCAN_IMAGE_PIXELS: u64 = 1_000_000;
pub(crate) const MIN_TEXT_CHARS: usize = 16;
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PageKind {
    Text,
    OcrText,
    NoText,
    Empty,
}
impl PageKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::OcrText => "ocr_text",
            Self::NoText => "no_text",
            Self::Empty => "empty",
        }
    }
}
#[derive(Default)]
pub(crate) struct PageText {
    pub text: String,
    pub visible_chars: usize,
    pub invisible_chars: usize,
    pub undecodable: usize,
    pub content_too_large: bool,
}
#[derive(Default)]
pub(crate) struct PagePaint {
    pub images: Vec<ObjectId>,
    pub inline_images: usize,
    pub visible_text: bool,
    pub vector_paint: bool,
    pub clipped: bool,
}
pub(crate) struct PageImage {
    #[allow(dead_code)] // C7 image descriptor consumed in the next phase.
    pub id: ObjectId,
    pub width: u32,
    pub height: u32,
}
pub(crate) struct PageInfo {
    pub page: u32,
    pub kind: PageKind,
    pub suspicious: Option<&'static str>,
    pub chars: usize,
    pub scan_image: bool,
    pub content_too_large: bool,
}
pub(crate) struct PdfDocument {
    doc: Document,
    pages: Vec<ObjectId>,
    encrypted: bool,
}
thread_local! { static KEEP_IMAGES: RefCell<HashSet<ObjectId>> = RefCell::new(HashSet::new()); }
#[cfg(test)]
thread_local! { static CONTENT_DECODE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
thread_local! {
    static FONT_RESOLUTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FONT_MAPS: RefCell<Vec<(u64, usize)>> = const { RefCell::new(Vec::new()) };
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum FontKey {
    Object(ObjectId),
    Direct { scope: usize, name: Vec<u8> },
}
enum PageStop {
    Limit(PageLimit),
    Deadline,
    Unextractable,
    Invalid(crate::filesystem::FsError),
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PageLimit {
    Content,
    Work,
    GlyphOutput,
    Q,
}
type PageResult<T> = std::result::Result<T, PageStop>;
impl From<crate::filesystem::FsError> for PageStop {
    fn from(value: crate::filesystem::FsError) -> Self {
        Self::Invalid(value)
    }
}
#[cfg(test)]
#[derive(Clone, Copy, Default, Debug)]
struct PageCounters {
    work: u64,
    lookups: u64,
    fonts: usize,
    decode_attempts: usize,
    differences: u64,
    font_bytes_left: usize,
    glyph_units: u64,
    shown_bytes: usize,
    stopped_by: Option<PageLimit>,
}
#[cfg(test)]
thread_local! {
    static PAGE_COUNTERS: std::cell::Cell<PageCounters> = const { std::cell::Cell::new(PageCounters { work: 0, lookups: 0, fonts: 0, decode_attempts: 0, differences: 0, font_bytes_left: 0, glyph_units: 0, shown_bytes: 0, stopped_by: None }) };
    static TEST_WORK_LIMIT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static TEST_PAGE_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}
type FormCache<'a> = BTreeMap<usize, BTreeMap<Vec<u8>, Option<(&'a lopdf::Stream, usize)>>>;
struct PageFont<'a> {
    encoding: Option<lopdf::Encoding<'a>>,
    max_destination: u64,
}
impl Default for PageFont<'_> {
    fn default() -> Self {
        Self {
            encoding: None,
            max_destination: 1,
        }
    }
}
struct PageState<'a> {
    doc: &'a Document,
    fonts: Vec<PageFont<'a>>,
    font_keys: BTreeMap<FontKey, u32>,
    font_names: BTreeMap<usize, BTreeMap<Vec<u8>, Option<u32>>>,
    forms: FormCache<'a>,
    scopes: Vec<Vec<&'a Dictionary>>,
    scope_ids: BTreeMap<usize, usize>,
    codes_left: u64,
    font_bytes_left: usize,
    content_left: usize,
    work_left: u64,
    glyph_left: u64,
    since_check: u64,
    deadline: Option<Instant>,
    paint: Option<PagePaint>,
    #[cfg(test)]
    counters: PageCounters,
}
impl<'a> PageState<'a> {
    fn new(doc: &'a Document, deadline: Option<Instant>) -> Self {
        #[cfg(test)]
        {
            CONTENT_DECODE_BYTES.with(|c| c.set(0));
            FONT_RESOLUTIONS.with(|c| c.set(0));
            FONT_MAPS.with(|m| m.borrow_mut().clear());
        }
        #[cfg(test)]
        let deadline = deadline
            .map(|original| TEST_PAGE_DEADLINE.with(|value| value.get().unwrap_or(original)));
        let work_left = PDF_PAGE_WORK_LIMIT;
        #[cfg(test)]
        let work_left = TEST_WORK_LIMIT.with(|limit| limit.get().unwrap_or(work_left));
        Self {
            doc,
            fonts: Vec::new(),
            font_keys: BTreeMap::new(),
            font_names: BTreeMap::new(),
            forms: BTreeMap::new(),
            scopes: vec![Vec::new()],
            scope_ids: BTreeMap::new(),
            codes_left: TOUNICODE_PAGE_CODE_LIMIT,
            font_bytes_left: PDF_STREAM_LIMIT,
            content_left: PDF_CONTENT_LIMIT,
            work_left,
            glyph_left: PDF_PAGE_GLYPH_LIMIT,
            since_check: 0,
            deadline,
            paint: None,
            #[cfg(test)]
            counters: PageCounters::default(),
        }
    }
    fn charge(&mut self, units: u64) -> PageResult<()> {
        #[cfg(test)]
        {
            self.counters.work = self.counters.work.saturating_add(units);
        }
        self.since_check = self.since_check.saturating_add(units);
        if self.since_check >= PDF_DEADLINE_CHECK_UNITS {
            self.since_check = 0;
            if self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Err(PageStop::Deadline);
            }
        }
        if units > self.work_left {
            return Err(PageStop::Limit(PageLimit::Work));
        }
        self.work_left -= units;
        Ok(())
    }
    fn lookup(&mut self) -> PageResult<()> {
        #[cfg(test)]
        {
            self.counters.lookups += 1;
        }
        self.charge(1)
    }
    // Read the object table directly: Document::get_object would hide reference hops.
    fn deref(
        &mut self,
        mut object: &'a Object,
        hops: usize,
    ) -> PageResult<Option<(Option<ObjectId>, &'a Object)>> {
        let mut id = None;
        for _ in 0..hops {
            let Object::Reference(next) = object else {
                return Ok(Some((id, object)));
            };
            self.lookup()?;
            id = Some(*next);
            let Some(next) = self.doc.objects.get(next) else {
                return Ok(None);
            };
            object = next;
        }
        if matches!(object, Object::Reference(_)) {
            Ok(None)
        } else {
            Ok(Some((id, object)))
        }
    }
    fn page_scope(&mut self, page: ObjectId) -> PageResult<()> {
        let (direct, inherited) = self.doc.get_page_resources(page).map_err(invalid)?;
        self.scopes[0].extend(direct);
        for id in inherited {
            self.lookup()?;
            if let Some(object) = self.doc.objects.get(&id)
                && let Some((_, Object::Dictionary(dict))) = self.deref(object, 127)?
            {
                self.scopes[0].push(dict);
            }
        }
        Ok(())
    }
    fn own_scope(&mut self, dict: &'a Dictionary) -> usize {
        let key = dict as *const Dictionary as usize;
        if let Some(scope) = self.scope_ids.get(&key) {
            return *scope;
        }
        let scope = self.scopes.len();
        self.scopes.push(vec![dict]);
        self.scope_ids.insert(key, scope);
        scope
    }
    fn resource_dict(
        &mut self,
        scope: usize,
        index: usize,
        kind: &[u8],
    ) -> PageResult<Option<&'a Dictionary>> {
        self.lookup()?;
        let dict = self.scopes[scope][index];
        let Ok(object) = dict.get(kind) else {
            return Ok(None);
        };
        Ok(self.deref(object, 128)?.and_then(|(_, o)| o.as_dict().ok()))
    }
    fn font(&mut self, scope: usize, name: &[u8]) -> PageResult<Option<u32>> {
        if let Some(handle) = self
            .font_names
            .get(&scope)
            .and_then(|names| names.get(name))
        {
            return Ok(*handle);
        }
        let mut selected = None;
        for index in 0..self.scopes[scope].len() {
            let Some(dict) = self.resource_dict(scope, index, b"Font")? else {
                continue;
            };
            self.lookup()?;
            let Ok(object) = dict.get(name) else {
                continue;
            };
            if let Some((id, Object::Dictionary(font))) = self.deref(object, 128)? {
                let key = id.map(FontKey::Object).unwrap_or_else(|| FontKey::Direct {
                    scope,
                    name: name.to_vec(),
                });
                selected = if let Some(handle) = self.font_keys.get(&key) {
                    Some(*handle)
                } else if self.fonts.len() < PDF_PAGE_FONT_LIMIT {
                    let handle = self.fonts.len() as u32;
                    self.font_keys.insert(key, handle);
                    self.fonts.push(PageFont::default());
                    #[cfg(test)]
                    {
                        self.counters.fonts += 1;
                    }
                    let encoding = self.resolve(font)?;
                    self.fonts[handle as usize] = encoding;
                    Some(handle)
                } else {
                    None
                };
                break;
            }
        }
        self.font_names
            .entry(scope)
            .or_default()
            .insert(name.to_vec(), selected);
        Ok(selected)
    }
    fn xobjects(&mut self, scope: usize) -> PageResult<Option<&'a Dictionary>> {
        for index in 0..self.scopes[scope].len() {
            // The first resources dictionary containing XObject takes precedence,
            // including an invalid or empty entry (unlike the per-name font lookup).
            let has = self.scopes[scope][index].has(b"XObject");
            let dict = self.resource_dict(scope, index, b"XObject")?;
            if has {
                return Ok(dict);
            }
        }
        Ok(None)
    }
    fn form_scope(&mut self, stream: &'a lopdf::Stream, inherited: usize) -> PageResult<usize> {
        if let Ok(object) = stream.dict.get(b"Resources")
            && let Some((_, Object::Dictionary(dict))) = self.deref(object, 128)?
        {
            return Ok(self.own_scope(dict));
        }
        Ok(inherited)
    }
    fn form(
        &mut self,
        scope: usize,
        name: &[u8],
    ) -> PageResult<Option<(&'a lopdf::Stream, usize)>> {
        if let Some(form) = self.forms.get(&scope).and_then(|names| names.get(name)) {
            return Ok(*form);
        }
        let mut form = None;
        if let Some(dict) = self.xobjects(scope)? {
            self.lookup()?;
            if let Ok(object) = dict.get(name)
                && let Some((_, Object::Stream(stream))) = self.deref(object, 128)?
                && stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form")
            {
                form = Some((stream, self.form_scope(stream, scope)?));
            }
        }
        self.forms
            .entry(scope)
            .or_default()
            .insert(name.to_vec(), form);
        Ok(form)
    }
    fn resolve(&mut self, font: &'a Dictionary) -> PageResult<PageFont<'a>> {
        // The hop limit applies before Differences accounting or the pre-scan decision.
        let mut encoding = font.get(b"Encoding").ok();
        let mut hops = 0;
        while let Some(Object::Reference(id)) = encoding {
            if hops == ENCODING_HOP_LIMIT {
                return Ok(PageFont::default());
            }
            self.lookup()?;
            hops += 1;
            encoding = self.doc.objects.get(id);
        }
        if let Some(Object::Dictionary(dict)) = encoding
            && let Ok(array) = dict.get(b"Differences").and_then(Object::as_array)
        {
            #[cfg(test)]
            {
                self.counters.differences += array.len() as u64;
            }
            self.charge(array.len() as u64)?;
        }
        let identity = |name: &[u8]| matches!(name, b"Identity-H" | b"Identity-V");
        let definite_non_identity = match encoding {
            Some(Object::Name(name)) => !identity(name),
            Some(Object::Dictionary(dict)) => match dict.get(b"BaseEncoding") {
                Err(_) => true,
                Ok(Object::Name(name)) => !identity(name),
                _ => false,
            },
            _ => false,
        };
        let stream = if !definite_non_identity {
            match font.get(b"ToUnicode") {
                Ok(object) => self
                    .deref(object, 128)?
                    .and_then(|(_, o)| o.as_stream().ok()),
                Err(_) => None,
            }
        } else {
            None
        };
        let mut max_destination = 1;
        if let Some(stream) = stream {
            if self.codes_left == 0 || self.font_bytes_left == 0 {
                return Ok(PageFont::default());
            }
            #[cfg(test)]
            {
                self.counters.decode_attempts += 1;
            }
            let data = match stream.get_plain_content_with_limit(PDF_STREAM_LIMIT) {
                Ok(data) => data,
                Err(e) if memory_limit(&e) => return Err(PageStop::Invalid(invalid(e))),
                Err(_) => {
                    self.font_bytes_left = 0;
                    self.charge(PDF_STREAM_LIMIT as u64)?;
                    return Ok(PageFont::default());
                }
            };
            let length = data.len();
            let over_bytes = length > self.font_bytes_left;
            self.font_bytes_left = self.font_bytes_left.saturating_sub(length);
            self.charge(length as u64)?;
            if over_bytes {
                return Ok(PageFont::default());
            }
            let Some((codes, destination)) = scan_cmap(&data) else {
                return Ok(PageFont::default());
            };
            if codes > TOUNICODE_CODE_LIMIT || codes > self.codes_left {
                return Ok(PageFont::default());
            }
            self.codes_left -= codes;
            max_destination = destination;
            drop(data);
            self.charge(length as u64)?; // lopdf decodes this map again.
            #[cfg(test)]
            FONT_MAPS.with(|maps| maps.borrow_mut().push((codes, length)));
        }
        #[cfg(test)]
        FONT_RESOLUTIONS.with(|count| count.set(count.get() + 1));
        match font.get_font_encoding_with_limit(self.doc, PDF_STREAM_LIMIT) {
            Ok(encoding) => Ok(PageFont {
                encoding: Some(encoding),
                max_destination,
            }),
            Err(e) if memory_limit(&e) => Err(PageStop::Invalid(invalid(e))),
            Err(_) => Ok(PageFont::default()),
        }
    }
}
#[cfg(test)]
impl Drop for PageState<'_> {
    fn drop(&mut self) {
        self.counters.font_bytes_left = self.font_bytes_left;
        PAGE_COUNTERS.with(|c| c.set(self.counters));
    }
}

// Counting-only CMap scan: no code-to-character mappings or range expansion.
enum CMapToken<'a> {
    Word(&'a [u8]),
    Hex { value: u32, length: usize },
    ArrayStart,
    ArrayEnd,
    Other,
}
struct CMapScan<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> CMapScan<'a> {
    fn next(&mut self) -> Option<CMapToken<'a>> {
        loop {
            let byte = *self.data.get(self.pos)?;
            if byte.is_ascii_whitespace() || byte == 0 {
                self.pos += 1;
                continue;
            }
            if byte == b'%' {
                while self
                    .data
                    .get(self.pos)
                    .is_some_and(|b| !matches!(b, b'\n' | b'\r'))
                {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
        let start = self.pos;
        let byte = self.data[self.pos];
        self.pos += 1;
        match byte {
            b'<' if self.data.get(self.pos) != Some(&b'<') => {
                let mut value = 0u32;
                let mut digits = 0usize;
                let mut valid = true;
                loop {
                    let b = *self.data.get(self.pos)?;
                    self.pos += 1;
                    if b == b'>' {
                        break;
                    }
                    if b.is_ascii_whitespace() {
                        continue;
                    }
                    let nibble = match b {
                        b'0'..=b'9' => b - b'0',
                        b'a'..=b'f' => b - b'a' + 10,
                        b'A'..=b'F' => b - b'A' + 10,
                        _ => {
                            valid = false;
                            0
                        }
                    };
                    value = value.wrapping_shl(4) | u32::from(nibble);
                    digits += 1;
                }
                if !valid || digits == 0 || !digits.is_multiple_of(2) {
                    return Some(CMapToken::Other);
                }
                Some(CMapToken::Hex {
                    value,
                    length: digits / 2,
                })
            }
            b'<' | b'>' => {
                if self.data.get(self.pos) == Some(&byte) {
                    self.pos += 1;
                }
                Some(CMapToken::Other)
            }
            b'[' => Some(CMapToken::ArrayStart),
            b']' => Some(CMapToken::ArrayEnd),
            b'(' => {
                let mut depth = 1;
                while depth > 0 {
                    match *self.data.get(self.pos)? {
                        b'\\' => {
                            self.pos += 1;
                            self.data.get(self.pos)?;
                        }
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    self.pos += 1;
                }
                Some(CMapToken::Other)
            }
            b'/' => {
                while self
                    .data
                    .get(self.pos)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b"()<>[]{}/%".contains(b))
                {
                    self.pos += 1;
                }
                Some(CMapToken::Other)
            }
            b'{' | b'}' | b')' => Some(CMapToken::Other),
            _ => {
                while self
                    .data
                    .get(self.pos)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b"()<>[]{}/%".contains(b))
                {
                    self.pos += 1;
                }
                Some(CMapToken::Word(&self.data[start..self.pos]))
            }
        }
    }
}
fn scan_cmap(data: &[u8]) -> Option<(u64, u64)> {
    let mut scan = CMapScan { data, pos: 0 };
    let mut total = 0u64;
    let mut max_destination = 1;
    while let Some(token) = scan.next() {
        let ranges = match token {
            CMapToken::Word(b"beginbfchar") => false,
            CMapToken::Word(b"beginbfrange") => true,
            _ => continue,
        };
        loop {
            let (lo, length) = match scan.next()? {
                CMapToken::Word(b"endbfchar") if !ranges => break,
                CMapToken::Word(b"endbfrange") if ranges => break,
                CMapToken::Hex {
                    value,
                    length: length @ 1..=4,
                } => (value, length),
                _ => return None,
            };
            let span = if ranges {
                let CMapToken::Hex {
                    value: hi,
                    length: hi_length,
                } = scan.next()?
                else {
                    return None;
                };
                if hi_length != length || hi < lo {
                    return None;
                }
                u64::from(hi) - u64::from(lo) + 1
            } else {
                1
            };
            match scan.next()? {
                CMapToken::Hex { length, .. } => {
                    max_destination = max_destination.max(length.div_ceil(2) as u64);
                }
                CMapToken::ArrayStart if ranges => {
                    let mut entries = 0u64;
                    loop {
                        match scan.next()? {
                            CMapToken::ArrayEnd if entries > 0 => break,
                            CMapToken::Hex { length, .. } => {
                                max_destination = max_destination.max(length.div_ceil(2) as u64);
                                entries += 1;
                                if entries > span {
                                    return None;
                                }
                            }
                            _ => return None,
                        }
                    }
                }
                _ => return None,
            }
            total = total.saturating_add(span);
        }
    }
    Some((total, max_destination))
}

struct ClearImages;
impl Drop for ClearImages {
    fn drop(&mut self) {
        KEEP_IMAGES.with(|keep| keep.borrow_mut().clear());
    }
}
fn image_filter(id: ObjectId, object: &mut Object) -> Option<(ObjectId, Object)> {
    if let Object::Stream(stream) = object {
        if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image")
            && !KEEP_IMAGES.with(|keep| keep.borrow().contains(&id))
        {
            stream.content = vec![0];
            stream.dict.set("LocalFilesPlaceholder", true);
        }
        Some((id, Object::Null))
    } else {
        Some((id, object.clone()))
    }
}
fn invalid(_: lopdf::Error) -> crate::filesystem::FsError {
    error("DOCUMENT_INVALID", "Cannot parse PDF document")
}
fn memory_limit(error: &lopdf::Error) -> bool {
    matches!(
        error,
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. })
    )
}
#[cfg(test)]
thread_local! {
    static DOCUMENT_LOAD_LIMITS: RefCell<Vec<Option<usize>>> = const { RefCell::new(Vec::new()) };
}
fn load_document(bytes: &[u8], options: LoadOptions) -> lopdf::Result<Document> {
    #[cfg(test)]
    DOCUMENT_LOAD_LIMITS.with(|calls| calls.borrow_mut().push(options.max_decompressed_size));
    Document::load_mem_with_options(bytes, options)
}
impl PdfDocument {
    #[allow(clippy::field_reassign_with_default)] // C7 configures fields on LoadOptions::default().
    pub(crate) fn load(bytes: &[u8], keep_images: &HashSet<ObjectId>) -> Result<Self> {
        if bytes.len() as u64 > PDF_LIMIT {
            return Err(error("FILE_TOO_LARGE", "PDF exceeds 512 MiB"));
        }
        if bytes.len() as u64 > PDF_ENCRYPTED_LIMIT
            && memchr::memmem::find(bytes, b"/Encrypt").is_some()
        {
            return Err(error("FILE_TOO_LARGE", "Encrypted PDF exceeds 256 MiB"));
        }
        KEEP_IMAGES.with(|keep| *keep.borrow_mut() = keep_images.clone());
        let _clear = ClearImages;
        let mut options = LoadOptions::default();
        options.filter = Some(image_filter);
        options.max_decompressed_size = Some(PDF_STREAM_LIMIT);
        let doc = load_document(bytes, options).map_err(|e| {
            if matches!(
                e,
                lopdf::Error::InvalidPassword | lopdf::Error::Decryption(_)
            ) {
                error("PDF_ENCRYPTED", "PDF requires a password")
            } else {
                invalid(e)
            }
        })?;
        let encrypted = doc.encryption_state.is_some() || doc.trailer.get(b"Encrypt").is_ok();
        if encrypted && bytes.len() as u64 > PDF_ENCRYPTED_LIMIT {
            return Err(error("FILE_TOO_LARGE", "Encrypted PDF exceeds 256 MiB"));
        }
        if encrypted && doc.encryption_state.is_none() {
            return Err(error("PDF_ENCRYPTED", "PDF requires a password"));
        }
        let pages = doc.get_pages().into_values().collect();
        Ok(Self {
            doc,
            pages,
            encrypted,
        })
    }
    pub(crate) fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }
    pub(crate) fn page_id(&self, page: u32) -> Result<ObjectId> {
        page.checked_sub(1)
            .and_then(|i| self.pages.get(i as usize))
            .copied()
            .ok_or_else(|| error("PAGE_OUT_OF_RANGE", "Page is outside the PDF page range"))
    }
    fn extract_text<'a>(&'a self, id: ObjectId, page: &mut PageState<'a>) -> PageResult<PageText> {
        let bytes = match self.doc.get_page_content_with_limit(id, PDF_CONTENT_LIMIT) {
            Ok(bytes) => bytes,
            Err(e) if memory_limit(&e) => return Err(PageStop::Limit(PageLimit::Content)),
            Err(e) => {
                page.charge(PDF_CONTENT_LIMIT as u64)?;
                return Err(PageStop::Invalid(invalid(e)));
            }
        };
        let mut text = PageText::default();
        let paint = page.paint.as_ref().map(|_| PaintState::default());
        self.walk_text(
            &bytes,
            0,
            0,
            (&mut TextState::default(), paint),
            page,
            &mut text,
        )?;
        Ok(text)
    }
    fn page_within(
        &self,
        page: u32,
        deadline: Option<Instant>,
        with_images: bool,
    ) -> Result<Option<(PageText, Vec<PageImage>)>> {
        let id = self.page_id(page)?;
        let mut state = PageState::new(&self.doc, deadline);
        let mut run = || -> PageResult<(PageText, Vec<PageImage>)> {
            state.page_scope(id)?;
            let text = match self.extract_text(id, &mut state) {
                Err(PageStop::Unextractable) => PageText::default(),
                other => other?,
            };
            let mut images = Vec::new();
            if with_images {
                self.collect_images(0, 0, &mut HashSet::new(), &mut images, &mut state)?;
            }
            Ok((text, images))
        };
        match run() {
            Ok(result) => Ok(Some(result)),
            Err(PageStop::Deadline) => Ok(None),
            Err(PageStop::Limit(_limit)) => {
                #[cfg(test)]
                {
                    state.counters.stopped_by = Some(_limit);
                }
                Ok(Some((
                    PageText {
                        content_too_large: true,
                        ..PageText::default()
                    },
                    vec![],
                )))
            }
            Err(PageStop::Invalid(e)) => Err(e),
            Err(PageStop::Unextractable) => unreachable!(),
        }
    }
    #[allow(dead_code)] // C7 standalone API; MCP uses page_info_within.
    pub(crate) fn page_text(&self, page: u32) -> Result<PageText> {
        Ok(self.page_within(page, None, false)?.expect("no deadline").0)
    }
    fn walk_text<'a>(
        &'a self,
        bytes: &[u8],
        scope: usize,
        depth: usize,
        states: (&mut TextState, Option<PaintState>),
        page_state: &mut PageState<'a>,
        out: &mut PageText,
    ) -> PageResult<()> {
        if bytes.len() > page_state.content_left {
            return Err(PageStop::Limit(PageLimit::Content));
        }
        page_state.content_left -= bytes.len();
        page_state.charge(bytes.len() as u64)?;
        #[cfg(test)]
        CONTENT_DECODE_BYTES.with(|count| count.set(count.get() + bytes.len()));
        let content = Content::decode(bytes).map_err(|_| PageStop::Unextractable)?;
        let (state, mut paint_state) = states;
        // Text-only calls retain their original Vec<TextState> stack. Visual
        // calls use one stack containing both states, with the same q limit.
        let mut stack = Vec::new();
        let mut paint_stack = Vec::new();
        let mut path = ClipPath::Empty;
        let mut marked_clip = false;
        for op in content.operations {
            page_state.charge(1)?;
            let args = &op.operands;
            if let Some(paint) = &mut paint_state {
                let painted = page_state.paint.as_mut().expect("visual walk");
                match op.operator.as_str() {
                    "cm" => {
                        if !paint_numbers(args).is_some_and(|m| paint.concat(m)) {
                            painted.clipped = true;
                        }
                    }
                    "re" => {
                        path = if matches!(path, ClipPath::Empty) {
                            paint_numbers(args)
                                .and_then(|[x, y, w, h]| paint.clip_bounds([x, y, x + w, y + h]))
                                .map(ClipPath::Rectangle)
                                .unwrap_or(ClipPath::Other)
                        } else {
                            ClipPath::Other
                        };
                    }
                    "m" | "l" | "c" | "v" | "y" | "h" => path = ClipPath::Other,
                    "W" | "W*" => marked_clip = true,
                    "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                        if marked_clip {
                            match path {
                                ClipPath::Rectangle(rectangle) if op.operator == "n" => {
                                    paint.intersect(rectangle)
                                }
                                _ => painted.clipped = true,
                            }
                        }
                        path = ClipPath::Empty;
                        marked_clip = false;
                    }
                    _ => {}
                }
            }
            match op.operator.as_str() {
                "BT" => {
                    state.in_text = true;
                    state.y = None;
                }
                "ET" => state.in_text = false,
                "Tf" => {
                    if let Some(Ok(name)) = args.first().map(Object::as_name) {
                        state.font = page_state.font(scope, name)?;
                    }
                }
                "Tr" => {
                    if let Some(Ok(mode)) = args.first().map(Object::as_i64) {
                        state.mode = mode;
                    }
                }
                "q" => {
                    let len = if paint_state.is_some() {
                        paint_stack.len()
                    } else {
                        stack.len()
                    };
                    if len >= PDF_Q_DEPTH_LIMIT {
                        return Err(PageStop::Limit(PageLimit::Q));
                    }
                    if let Some(paint) = paint_state {
                        paint_stack.push((*state, paint));
                    } else {
                        stack.push(*state);
                    }
                }
                "Q" => {
                    if paint_state.is_some() {
                        if let Some((text, paint)) = paint_stack.pop() {
                            *state = text;
                            paint_state = Some(paint);
                        }
                    } else if let Some(saved) = stack.pop() {
                        *state = saved;
                    }
                }
                "T*" => newline(&mut out.text),
                "Td" | "TD" => {
                    if args
                        .get(1)
                        .and_then(|n| n.as_float().ok())
                        .is_some_and(|n| n != 0.0)
                    {
                        newline(&mut out.text);
                    } else if args
                        .first()
                        .and_then(|n| n.as_float().ok())
                        .is_some_and(|n| n > 0.0)
                    {
                        space(&mut out.text);
                    }
                }
                "Tm" => {
                    if let Some(y) = args.get(5).and_then(|n| n.as_float().ok()) {
                        if state.y.is_some_and(|previous| (previous - y).abs() > 0.5) {
                            newline(&mut out.text);
                        }
                        state.y = Some(y);
                    }
                }
                "Tj" | "'" | "\"" => {
                    if op.operator != "Tj" {
                        newline(&mut out.text);
                    }
                    if let Some(Ok(bytes)) = args.last().map(Object::as_str) {
                        show(bytes, state, page_state, out)?;
                    }
                }
                "TJ" => {
                    if let Some(Ok(array)) = args.first().map(Object::as_array) {
                        for item in array {
                            if let Ok(bytes) = item.as_str() {
                                show(bytes, state, page_state, out)?;
                            } else if item.as_float().is_ok_and(|n| n < -250.0) {
                                space(&mut out.text);
                            }
                        }
                    }
                }
                "BI" => {
                    if let Some(paint) = &mut page_state.paint {
                        paint.inline_images += 1;
                    }
                }
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "sh" => {
                    if let Some(paint) = &mut page_state.paint {
                        paint.vector_paint = true;
                    }
                }
                "Do" => {
                    let mut form_paint = paint_state;
                    // Only the visual walk resolves image Do operators. Text extraction's
                    // lookup/cache behavior and budgets are unchanged.
                    if page_state.paint.is_some() {
                        let mut painted = None;
                        if let Some(Ok(name)) = args.first().map(Object::as_name)
                            && let Some(dict) = page_state.xobjects(scope)?
                        {
                            page_state.lookup()?;
                            if let Ok(obj) = dict.get(name) {
                                painted = page_state.deref(obj, 128)?;
                            }
                        }
                        match painted {
                            Some((Some(id), Object::Stream(stream)))
                                if stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
                                    == Some(b"Image") =>
                            {
                                let paint = page_state.paint.as_mut().unwrap();
                                paint.images.push(id);
                                if paint_state.is_some_and(|state| !state.contains_image()) {
                                    paint.clipped = true;
                                }
                                continue;
                            }
                            Some((_, Object::Stream(stream)))
                                if depth < 3
                                    && stream
                                        .dict
                                        .get(b"Subtype")
                                        .and_then(Object::as_name)
                                        .ok()
                                        == Some(b"Form") =>
                            {
                                if let Some(paint) = &mut form_paint
                                    && paint.enter_form(&self.doc, &stream.dict).is_none()
                                {
                                    page_state.paint.as_mut().unwrap().clipped = true;
                                }
                            }
                            _ => {
                                page_state.paint.as_mut().unwrap().vector_paint = true;
                                continue;
                            }
                        }
                    }
                    if depth < 3
                        && let Some(Ok(name)) = args.first().map(Object::as_name)
                        && let Some((stream, form_scope)) = page_state.form(scope, name)?
                    {
                        let bytes =
                            match stream.decompressed_content_with_limit(page_state.content_left) {
                                Ok(bytes) => bytes,
                                Err(e) if memory_limit(&e) => {
                                    return Err(PageStop::Limit(PageLimit::Content));
                                }
                                Err(e) => {
                                    page_state.charge(page_state.content_left as u64)?;
                                    return Err(PageStop::Invalid(invalid(e)));
                                }
                            };
                        let mut form_state = *state;
                        self.walk_text(
                            &bytes,
                            form_scope,
                            depth + 1,
                            (&mut form_state, form_paint),
                            page_state,
                            out,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub(crate) fn page_paint(&self, page: u32) -> Result<PagePaint> {
        let id = self.page_id(page)?;
        let mut state = PageState::new(&self.doc, None);
        state.paint = Some(PagePaint::default());
        let result = (|| -> PageResult<PageText> {
            // lopdf get_page_resources omits direct dictionaries on ancestors.
            // Resolve them for visual access without altering the Ф2 text walk.
            let mut current = id;
            for depth in 0..128 {
                state.lookup()?;
                let dict = self.doc.get_dictionary(current).map_err(invalid)?;
                if let Ok(value) = dict.get(b"Resources")
                    && let Some((_, Object::Dictionary(resources))) = state.deref(value, 128)?
                {
                    state.scopes[0].push(resources);
                }
                let Ok(parent) = dict.get(b"Parent").and_then(Object::as_reference) else {
                    return self.extract_text(id, &mut state);
                };
                if depth == 127 {
                    return Err(PageStop::Unextractable);
                }
                current = parent;
            }
            unreachable!()
        })();
        match result {
            Ok(text) => state.paint.as_mut().unwrap().visible_text = text.visible_chars > 0,
            Err(PageStop::Invalid(e)) => return Err(e),
            // A budget, malformed content or a deeper form makes partial extraction unsafe.
            Err(_) => state.paint.as_mut().unwrap().vector_paint = true,
        }
        Ok(state.paint.take().unwrap())
    }
    pub(crate) fn image_stream(&self, id: ObjectId) -> Result<&lopdf::Stream> {
        self.doc
            .get_object(id)
            .and_then(Object::as_stream)
            .map_err(invalid)
    }
    pub(crate) fn page_attribute(&self, page: u32, key: &[u8]) -> Result<Option<Object>> {
        let mut id = self.page_id(page)?;
        for _ in 0..128 {
            let dict = self.doc.get_dictionary(id).map_err(invalid)?;
            if let Ok(value) = dict.get(key) {
                return Ok(Some(value.clone()));
            }
            let Ok(parent) = dict.get(b"Parent").and_then(Object::as_reference) else {
                return Ok(None);
            };
            id = parent;
        }
        Err(error("DOCUMENT_INVALID", "Invalid page inheritance"))
    }
    #[allow(dead_code)] // C7 standalone API; MCP uses page_info_within.
    pub(crate) fn page_images(&self, page: u32) -> Result<Vec<PageImage>> {
        let id = self.page_id(page)?;
        let mut state = PageState::new(&self.doc, None);
        let mut images = Vec::new();
        let mut run = || -> PageResult<()> {
            state.page_scope(id)?;
            self.collect_images(0, 0, &mut HashSet::new(), &mut images, &mut state)
        };
        match run() {
            Ok(()) => Ok(images),
            Err(PageStop::Invalid(e)) => Err(e),
            Err(_) => Err(error(
                "DOCUMENT_INVALID",
                "page resources exceed the extraction limits",
            )),
        }
    }
    fn collect_images<'a>(
        &'a self,
        scope: usize,
        depth: usize,
        seen: &mut HashSet<ObjectId>,
        images: &mut Vec<PageImage>,
        state: &mut PageState<'a>,
    ) -> PageResult<()> {
        if let Some(xobjects) = state.xobjects(scope)? {
            for (_, value) in xobjects {
                state.charge(1)?;
                let Ok(id) = value.as_reference() else {
                    continue;
                };
                if !seen.insert(id) {
                    continue;
                }
                let Some((_, Object::Stream(stream))) = state.deref(value, 128)? else {
                    continue;
                };
                match stream.dict.get(b"Subtype").and_then(Object::as_name).ok() {
                    Some(b"Image") => {
                        let dimension = |key| {
                            stream
                                .dict
                                .get(key)
                                .and_then(Object::as_i64)
                                .ok()
                                .and_then(|n| u32::try_from(n).ok())
                                .unwrap_or(0)
                        };
                        images.push(PageImage {
                            id,
                            width: dimension(b"Width"),
                            height: dimension(b"Height"),
                        });
                    }
                    Some(b"Form") if depth < 3 => {
                        let nested = state.form_scope(stream, scope)?;
                        self.collect_images(nested, depth + 1, seen, images, state)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
    #[allow(dead_code)] // C7 standalone API; MCP uses page_info_within.
    pub(crate) fn page_info(&self, page: u32) -> Result<(PageInfo, PageText)> {
        Ok(self.page_info_within(page, None)?.expect("no deadline"))
    }
    pub(crate) fn page_info_within(
        &self,
        page: u32,
        deadline: Option<Instant>,
    ) -> Result<Option<(PageInfo, PageText)>> {
        let Some((text, images)) = self.page_within(page, deadline, true)? else {
            return Ok(None);
        };
        let scan_image = images
            .iter()
            .any(|i| u64::from(i.width) * u64::from(i.height) >= SCAN_IMAGE_PIXELS);
        let (kind, suspicious) = assess(&text, !images.is_empty(), scan_image);
        let chars = text.text.chars().filter(|c| !c.is_whitespace()).count();
        Ok(Some((
            PageInfo {
                page,
                kind,
                suspicious,
                chars,
                scan_image,
                content_too_large: text.content_too_large,
            },
            text,
        )))
    }
    pub(crate) fn metadata(&self) -> Value {
        let info = self
            .doc
            .trailer
            .get_deref(b"Info", &self.doc)
            .and_then(Object::as_dict)
            .ok();
        let field = |key| {
            info.and_then(|d| d.get(key).ok())
                .and_then(|o| lopdf::decode_text_string(o).ok())
        };
        json!({"title":field(b"Title"),"producer":field(b"Producer"),"encrypted":self.encrypted})
    }
    #[allow(dead_code)] // C7 interface consumed by the following approved phase.
    pub(crate) fn document(&self) -> &Document {
        &self.doc
    }
    #[allow(dead_code)]
    pub(crate) fn into_document(self) -> Document {
        self.doc
    }
}
#[derive(Clone, Copy, Default)]
struct TextState {
    font: Option<u32>,
    mode: i64,
    in_text: bool,
    y: Option<f32>,
}
const _: () = assert!(std::mem::size_of::<TextState>() <= 32);
#[derive(Clone, Copy)]
struct PaintState {
    ctm: [f64; 6],
    clip: Option<[f64; 4]>,
}
impl Default for PaintState {
    fn default() -> Self {
        Self {
            ctm: [1., 0., 0., 1., 0., 0.],
            clip: None,
        }
    }
}
fn paint_numbers<const N: usize>(objects: &[Object]) -> Option<[f64; N]> {
    if objects.len() != N {
        return None;
    }
    let mut values = [0.; N];
    for (value, object) in values.iter_mut().zip(objects) {
        *value = match object {
            Object::Integer(n) => *n as f64,
            Object::Real(n) => f64::from(*n),
            _ => return None,
        };
        if !value.is_finite() {
            return None;
        }
    }
    Some(values)
}
impl PaintState {
    fn concat(&mut self, [a, b, c, d, e, f]: [f64; 6]) -> bool {
        let [g, h, i, j, k, l] = self.ctm;
        // ISO 32000-1 8.3.4: incoming M × current CTM (row-vector notation).
        self.ctm = [
            a * g + b * i,
            a * h + b * j,
            c * g + d * i,
            c * h + d * j,
            e * g + f * i + k,
            e * h + f * j + l,
        ];
        self.ctm.iter().all(|v| v.is_finite())
    }
    fn bounds(&self, [x0, y0, x1, y1]: [f64; 4]) -> Option<[f64; 4]> {
        if !self
            .ctm
            .iter()
            .chain([x0, y0, x1, y1].iter())
            .all(|v| v.is_finite())
        {
            return None;
        }
        let [a, b, c, d, e, f] = self.ctm;
        let mut bounds = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for (x, y) in [(x0, y0), (x0, y1), (x1, y0), (x1, y1)] {
            let (x, y) = (a * x + c * y + e, b * x + d * y + f);
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            bounds = [
                bounds[0].min(x),
                bounds[1].min(y),
                bounds[2].max(x),
                bounds[3].max(y),
            ];
        }
        Some(bounds)
    }
    fn clip_bounds(&self, rectangle: [f64; 4]) -> Option<[f64; 4]> {
        let [a, b, c, d, _, _] = self.ctm;
        if !(b == 0. && c == 0. || a == 0. && d == 0.) {
            return None;
        }
        self.bounds(rectangle)
    }
    fn intersect(&mut self, rectangle: [f64; 4]) {
        self.clip = Some(match self.clip {
            None => rectangle,
            Some(clip) => [
                clip[0].max(rectangle[0]),
                clip[1].max(rectangle[1]),
                clip[2].min(rectangle[2]),
                clip[3].min(rectangle[3]),
            ],
        });
    }
    fn contains_image(&self) -> bool {
        let Some([ix0, iy0, ix1, iy1]) = self.bounds([0., 0., 1., 1.]) else {
            return false;
        };
        self.clip.is_none_or(|[x0, y0, x1, y1]| {
            ix0 >= x0 - 0.5 && iy0 >= y0 - 0.5 && ix1 <= x1 + 0.5 && iy1 <= y1 + 0.5
        })
    }
    fn enter_form(&mut self, doc: &Document, dict: &Dictionary) -> Option<()> {
        let matrix = match dict.get(b"Matrix") {
            Ok(object) => paint_numbers(doc.dereference(object).ok()?.1.as_array().ok()?)?,
            Err(_) => Self::default().ctm,
        };
        let bbox = paint_numbers(
            doc.dereference(dict.get(b"BBox").ok()?)
                .ok()?
                .1
                .as_array()
                .ok()?,
        )?;
        self.concat(matrix).then_some(())?;
        let rectangle = self.clip_bounds(bbox)?;
        self.intersect(rectangle);
        Some(())
    }
}
#[derive(Default)]
enum ClipPath {
    #[default]
    Empty,
    Rectangle([f64; 4]),
    Other,
}
fn newline(text: &mut String) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
}
fn space(text: &mut String) {
    if !text.is_empty() && !text.ends_with(char::is_whitespace) {
        text.push(' ');
    }
}
fn show(
    bytes: &[u8],
    state: &TextState,
    page_state: &mut PageState<'_>,
    out: &mut PageText,
) -> PageResult<()> {
    if !state.in_text {
        return Ok(());
    }
    let max_destination = state
        .font
        .and_then(|handle| page_state.fonts.get(handle as usize))
        .map_or(1, |font| font.max_destination);
    let units = (bytes.len() as u64).saturating_mul(max_destination);
    if units > page_state.glyph_left {
        return Err(PageStop::Limit(PageLimit::GlyphOutput));
    }
    page_state.glyph_left -= units;
    #[cfg(test)]
    {
        page_state.counters.glyph_units += units;
    }
    page_state.charge(units)?;
    let mut decoded = String::new();
    let encoding = state
        .font
        .and_then(|handle| page_state.fonts.get(handle as usize))
        .and_then(|font| font.encoding.as_ref());
    let failed = if let Some(encoding) = encoding {
        #[cfg(test)]
        {
            page_state.counters.shown_bytes += bytes.len();
        }
        encoding.write_to_string(bytes, &mut decoded).is_err()
    } else {
        true
    };
    if failed {
        decoded.clear();
        decoded.extend(std::iter::repeat_n('\u{fffd}', bytes.len()));
    }
    for c in decoded.chars() {
        if c == '\u{fffd}' || c.is_control() && !matches!(c, '\n' | '\t') {
            out.undecodable += 1;
        }
        if !c.is_whitespace() {
            if matches!(state.mode, 3 | 7) {
                out.invisible_chars += 1;
            } else {
                out.visible_chars += 1;
            }
        }
    }
    out.text.push_str(&decoded);
    Ok(())
}
pub(crate) fn assess(
    text: &PageText,
    has_images: bool,
    scan_image: bool,
) -> (PageKind, Option<&'static str>) {
    if text.content_too_large {
        return (PageKind::NoText, None);
    }
    let chars = text.text.chars().filter(|c| !c.is_whitespace()).count();
    let kind = if chars < MIN_TEXT_CHARS {
        if has_images {
            PageKind::NoText
        } else {
            PageKind::Empty
        }
    } else if text.invisible_chars.saturating_mul(2) >= text.visible_chars + text.invisible_chars {
        PageKind::OcrText
    } else {
        PageKind::Text
    };
    let letters = text.text.chars().filter(|c| c.is_alphabetic()).count();
    let latin1 = text
        .text
        .chars()
        .filter(|c| c.is_alphabetic() && ('\u{c0}'..='\u{ff}').contains(c))
        .count();
    let cyrillic = text
        .text
        .chars()
        .filter(|c| c.is_alphabetic() && ('\u{400}'..='\u{4ff}').contains(c))
        .count();
    let suspicious = if text.undecodable > 0
        && text.undecodable.saturating_mul(100) >= chars.saturating_mul(5)
    {
        Some("undecodable")
    } else if letters > 0 && latin1 * 100 >= letters * 20 {
        Some("mojibake")
    } else if scan_image && letters >= 40 && cyrillic == 0 {
        Some("no_cyrillic_over_scan")
    } else {
        None
    };
    (kind, suspicious)
}
pub(crate) fn page_ranges(pages: impl IntoIterator<Item = u32>) -> String {
    let mut pages = pages.into_iter().collect::<Vec<_>>();
    pages.sort_unstable();
    pages.dedup();
    let mut result = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let start = pages[i];
        let mut end = start;
        i += 1;
        while i < pages.len() && pages[i] == end.saturating_add(1) {
            end = pages[i];
            i += 1;
        }
        result.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
    }
    result.join(",")
}
pub(crate) struct PdfText {
    pdf: PdfDocument,
    range: Range<usize>,
}
impl PdfText {
    pub(crate) fn open(bytes: &[u8], args: &Value) -> Result<Self> {
        let pdf = PdfDocument::load(bytes, &HashSet::new())?;
        let count = pdf.page_count() as usize;
        if count == 0 {
            if args.get("startPage").is_some() || args.get("endPage").is_some() {
                return Err(error("PAGE_OUT_OF_RANGE", "PDF has no pages"));
            }
            return Ok(Self { pdf, range: 0..0 });
        }
        let page = |key, default| -> Result<usize> {
            let n = number(args, key, default, usize::MAX)
                .map_err(|_| error("PAGE_OUT_OF_RANGE", "Page is outside the PDF page range"))?;
            if n > count {
                return Err(error(
                    "PAGE_OUT_OF_RANGE",
                    "Page is outside the PDF page range",
                ));
            }
            Ok(n)
        };
        let start = page("startPage", 1)?;
        let end = page("endPage", count)?;
        if start > end {
            return Err(error("PAGE_OUT_OF_RANGE", "startPage exceeds endPage"));
        }
        Ok(Self {
            pdf,
            range: start - 1..end,
        })
    }
}
impl TextDocument for PdfText {
    fn unit_name(&self) -> &'static str {
        "page"
    }
    fn unit_count(&self) -> usize {
        self.pdf.page_count() as usize
    }
    fn unit_range(&self, _args: &Value) -> Result<Range<usize>> {
        Ok(self.range.clone())
    }
    fn summary(&self) -> Value {
        let mut summary = self.pdf.metadata();
        summary["pageCount"] = json!(self.pdf.page_count());
        summary
    }
    fn unit(&mut self, index: usize) -> Result<Unit> {
        self.unit_within(index, None)
            .map(|unit| unit.expect("no deadline"))
    }
    fn unit_within(&mut self, index: usize, deadline: Option<Instant>) -> Result<Option<Unit>> {
        let Some((info, text)) = self.pdf.page_info_within(index as u32 + 1, deadline)? else {
            return Ok(None);
        };
        let layer = if info.content_too_large {
            "text not extracted (page content exceeds the extraction limits)"
        } else {
            match info.kind {
                PageKind::Text => "text_layer",
                PageKind::OcrText => {
                    "ocr_layer (text embedded by OCR software, may contain recognition errors)"
                }
                PageKind::NoText => "no text layer (scanned image)",
                PageKind::Empty => "empty",
            }
        };
        let mut heading = format!("--- page {} · {layer} ---", info.page);
        let mut locator = match info.kind {
            PageKind::Text => json!({"page":info.page,"source":"text_layer"}),
            PageKind::OcrText => json!({"page":info.page,"source":"ocr_layer"}),
            kind => json!({"page":info.page,"kind":kind.as_str()}),
        };
        if info.content_too_large {
            locator["contentTooLarge"] = json!(true);
        }
        if let Some(reason) = info.suspicious {
            heading.push_str(&format!(" · suspicious: {reason}"));
            locator["suspicious"] = json!(reason);
        }
        heading.push_str(&format!(" · view with read_file page={}", info.page));
        Ok(Some(Unit {
            locator,
            heading: Some(heading),
            text: text.text,
        }))
    }
}
pub(crate) fn inspect(
    bytes: &[u8],
    from_page: u32,
    deadline: Instant,
) -> Result<(Value, Option<u32>)> {
    let pdf = PdfDocument::load(bytes, &HashSet::new())?;
    if from_page == 0 || from_page > pdf.page_count().max(1) {
        return Err(error("INVALID_CURSOR", "Invalid inspection page position"));
    }
    let mut kinds: BTreeMap<&str, Vec<u32>> = ["text", "ocr_text", "no_text", "empty"]
        .into_iter()
        .map(|k| (k, vec![]))
        .collect();
    let mut suspicious: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
    let mut content_too_large = Vec::new();
    let mut classified = 0;
    let mut next = None;
    for page in from_page..=pdf.page_count() {
        let Some((info, _)) = pdf.page_info_within(
            page,
            if classified == 0 {
                None
            } else {
                Some(deadline)
            },
        )?
        else {
            next = Some(page);
            break;
        };
        kinds.get_mut(info.kind.as_str()).unwrap().push(page);
        if info.content_too_large {
            content_too_large.push(page);
        }
        if let Some(reason) = info.suspicious {
            suspicious.entry(reason).or_default().push(page);
        }
        // These fields remain useful to consumers of page_info without duplicating extraction.
        debug_assert!(
            info.chars < MIN_TEXT_CHARS || matches!(info.kind, PageKind::Text | PageKind::OcrText)
        );
        debug_assert!(!info.scan_image || !info.content_too_large);
        classified += 1;
        if Instant::now() >= deadline && page < pdf.page_count() {
            next = Some(page + 1);
            break;
        }
    }
    let mut result = pdf.metadata();
    result["pageCount"] = json!(pdf.page_count());
    result["viewHint"] = json!("view with read_file page=N");
    result["pages"] = json!(
        kinds
            .into_iter()
            .map(|(kind, pages)| (kind, page_ranges(pages)))
            .collect::<BTreeMap<_, _>>()
    );
    result["suspicious"] = json!(
        suspicious
            .into_iter()
            .map(|(reason, pages)| (reason, page_ranges(pages)))
            .collect::<BTreeMap<_, _>>()
    );
    result["contentTooLarge"] = json!(page_ranges(content_too_large));
    result["classified"] = json!(classified);
    result["complete"] = json!(next.is_none());
    result["fromPage"] = json!(from_page);
    Ok((result, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Stream, dictionary};
    fn fixture() -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages = doc.new_object_id();
        let font =
            doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
        let image = doc.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>1200,"Height"=>1000,"ColorSpace"=>"DeviceGray","BitsPerComponent"=>8}, vec![77; 64]));
        let cmap = doc.add_object(Stream::new(Dictionary::new(), b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n3 beginbfchar\n<0001> <041F>\n<0002> <0440>\n<0003> <0438>\nendbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend".to_vec()));
        let cyr = doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type0","BaseFont"=>"Synthetic","Encoding"=>"Identity-H","ToUnicode"=>cmap});
        let form = doc.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),100.into(),100.into()],"Resources"=>dictionary!{"Font"=>dictionary!{"Inner"=>font}}}, b"BT /Inner 12 Tf (Text inside a form XObject) Tj ET".to_vec()));
        let resources = doc.add_object(dictionary! {"Font"=>dictionary!{"F"=>font,"C"=>cyr}, "XObject"=>dictionary!{"Im"=>image,"Form"=>form}});
        let contents: &[&[u8]] = &[
            b"BT /F 12 Tf (Native Helvetica text on page one) Tj 0 -20 Td (Second line) Tj T* (Third line) Tj ET",
            b"/Im Do BT /F 12 Tf 3 Tr (Latin only invisible text over a large scanned image and more letters) Tj ET",
            b"/Im Do",
            b"",
            b"BT /C 12 Tf <000100020003000100020003000100020003000100020003000100020003000100020003> Tj ET",
            b"/Form Do",
            b"BT /F 12 Tf 3 Tr (hidden text) Tj q 0 Tr (visible text of sufficient length) Tj Q (hidden again) Tj ET",
        ];
        let mut kids = Vec::new();
        for content in contents {
            let stream = doc.add_object(Stream::new(Dictionary::new(), content.to_vec()));
            let page =
                doc.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,"Contents"=>stream});
            kids.push(page.into());
        }
        doc.objects.insert(pages, dictionary! {"Type"=>"Pages","Count"=>kids.len() as i64,"Kids"=>kids,"Resources"=>resources,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()]}.into());
        let catalog = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
        doc.trailer.set("Root", catalog);
        let info = doc.add_object(dictionary! {"Title"=>Object::string_literal("Synthetic PDF"),"Producer"=>Object::string_literal("local-files tests")});
        doc.trailer.set("Info", info);
        (doc, image)
    }
    fn bytes(mut doc: Document) -> Vec<u8> {
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }
    fn document_tools(doc: Document) -> (Value, Value) {
        let tmp = tempfile::tempdir().unwrap();
        let root = crate::filesystem::RootDir::open(&crate::config::Root {
            id: "work".into(),
            path: tmp.path().into(),
            nickname: None,
        })
        .unwrap();
        std::fs::write(tmp.path().join("fixture.pdf"), bytes(doc)).unwrap();
        let args = json!({"path":"fixture.pdf"});
        let read = crate::documents::read_document(&root, "work", &args).unwrap();
        let inspect = crate::documents::inspect_document(&root, "work", &args).unwrap();
        (read, inspect)
    }
    fn inherited_resources(doc: &Document) -> ObjectId {
        let page = doc.get_pages()[&1];
        let parent = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Parent")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_dictionary(parent)
            .unwrap()
            .get(b"Resources")
            .unwrap()
            .as_reference()
            .unwrap()
    }
    #[test]
    fn unresolved_font_encoding_is_undecodable_without_failing_document_tools() {
        let (mut doc, _) = fixture();
        let resources = inherited_resources(&doc);
        let font = doc
            .get_dictionary(resources)
            .unwrap()
            .get(b"Font")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"F")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_object_mut(font)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .remove(b"Type");
        let (read, inspect) = document_tools(doc);
        assert_eq!(read["units"][0]["source"], "text_layer");
        assert_eq!(read["units"][0]["suspicious"], "undecodable");
        assert!(
            inspect["suspicious"]["undecodable"]
                .as_str()
                .unwrap()
                .contains('1')
        );
        assert_eq!(inspect["classified"], 7);
    }
    #[test]
    fn dangling_and_nonstream_xobjects_do_not_fail_document_tools() {
        let (mut doc, _) = fixture();
        let resources = inherited_resources(&doc);
        let not_stream = doc.add_object(Object::Integer(7));
        let xobjects = doc
            .get_object_mut(resources)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .get_mut(b"XObject")
            .unwrap()
            .as_dict_mut()
            .unwrap();
        xobjects.set("Dangling", Object::Reference((999_999, 0)));
        xobjects.set("NotStream", not_stream);
        let (read, inspect) = document_tools(doc);
        assert!(
            read["text"]
                .as_str()
                .unwrap()
                .contains("Native Helvetica text")
        );
        assert_eq!(read["units"][0]["source"], "text_layer");
        assert_eq!(inspect["pages"]["text"], "1,5-7");
        assert_eq!(inspect["classified"], 7);
    }
    #[test]
    fn malformed_page_and_form_content_do_not_fail_other_pages() {
        for form in [false, true] {
            let (mut doc, _) = fixture();
            let page = doc.get_pages()[&1];
            let content = doc
                .get_dictionary(page)
                .unwrap()
                .get(b"Contents")
                .unwrap()
                .as_reference()
                .unwrap();
            let mut malformed = Stream::new(Dictionary::new(), b"BI".to_vec());
            if form {
                malformed.dict.set("Subtype", "Form");
                let bad = doc.add_object(malformed);
                let resources = inherited_resources(&doc);
                doc.get_object_mut(resources)
                    .unwrap()
                    .as_dict_mut()
                    .unwrap()
                    .get_mut(b"XObject")
                    .unwrap()
                    .as_dict_mut()
                    .unwrap()
                    .set("Bad", bad);
                doc.objects.insert(content, Stream::new(Dictionary::new(), b"BT /F 12 Tf (Discard partial page text before invalid form) Tj ET /Bad Do".to_vec()).into());
            } else {
                // Flate fails; lopdf falls back to the undecodable raw BI bytes.
                malformed.dict.set("Filter", "FlateDecode");
                doc.objects.insert(content, malformed.into());
            }
            let (read, inspect) = document_tools(doc);
            assert_eq!(read["units"][0]["kind"], "no_text");
            assert!(
                !read["text"]
                    .as_str()
                    .unwrap()
                    .contains("Discard partial page")
            );
            assert!(!read["text"].as_str().unwrap().contains("Second line"));
            assert_eq!(read["units"][4]["source"], "text_layer");
            assert_eq!(inspect["pages"]["text"], "5-7");
            assert_eq!(inspect["classified"], 7);
            assert_eq!(inspect["contentTooLarge"], "");
            assert!(read["units"][0].get("contentTooLarge").is_none());
        }
    }
    fn compressed(content: Vec<u8>) -> Stream {
        let mut stream = Stream::new(Dictionary::new(), content);
        stream.compress().unwrap();
        stream
    }
    fn dense_content(length: usize, suffix: &[u8]) -> Vec<u8> {
        let prefix_length = length - suffix.len();
        let mut content = b"0 0 m\n".repeat(prefix_length / 6);
        content.resize(prefix_length, b' ');
        content.extend_from_slice(suffix);
        content
    }
    fn amplification_fixture() -> Document {
        let mut doc = Document::with_version("1.5");
        let pages = doc.new_object_id();
        let font =
            doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
        let repeated = doc.add_object({
            let mut stream = compressed(dense_content(
                512 * 1024,
                b"BT /F 12 Tf (Discard repeated form partial text) Tj ET\n",
            ));
            stream.dict.set("Subtype", "Form");
            stream
        });
        let mut nested = None;
        for _ in 0..3 {
            let suffix: &[u8] = if nested.is_some() {
                b"/Nested Do\n"
            } else {
                b"BT /F 12 Tf (Nested form text) Tj ET\n"
            };
            let mut stream = compressed(dense_content(768 * 1024, suffix));
            stream.dict.set("Subtype", "Form");
            if let Some(id) = nested {
                stream.dict.set(
                    "Resources",
                    dictionary! {"XObject"=>dictionary! {"Nested"=>id}},
                );
            }
            nested = Some(doc.add_object(stream));
        }
        let contents = vec![
            doc.add_object(compressed(dense_content(64*1024*1024, b""))),
            doc.add_object(Stream::new(Dictionary::new(), b"BT /F 12 Tf (Normal Helvetica text on the second page) Tj ET".to_vec())),
            doc.add_object(Stream::new(Dictionary::new(), b"BT /F 12 Tf (Discard repeated page partial text) Tj ET /Repeated Do /Repeated Do /Repeated Do /Repeated Do /Repeated Do".to_vec())),
            doc.add_object(Stream::new(Dictionary::new(), b"/Nested Do".to_vec())),
            doc.add_object(compressed(dense_content(PDF_CONTENT_LIMIT-2, b"BT /F 12 Tf (Just under the page decode limit) Tj ET\n"))),
        ];
        let resources = doc.add_object(dictionary! {"Font"=>dictionary! {"F"=>font}, "XObject"=>dictionary! {"Repeated"=>repeated, "Nested"=>nested.unwrap()}});
        let mut kids = Vec::new();
        for content in contents {
            let page = doc.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,"Contents"=>content,"Resources"=>resources});
            kids.push(page.into());
        }
        doc.objects.insert(pages, dictionary! {"Type"=>"Pages","Kids"=>kids,"Count"=>5,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()]}.into());
        let catalog = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
        doc.trailer.set("Root", catalog);
        doc
    }
    #[test]
    fn cumulative_page_content_budget_flags_amplification_and_preserves_other_pages() {
        let doc = amplification_fixture();
        let file = bytes(doc.clone());
        assert!(
            file.len() < 128 * 1024,
            "synthetic compressed fixture must stay small"
        );
        let pdf = PdfDocument::load(&file, &HashSet::new()).unwrap();
        let mut counts = Vec::new();
        for page in 1..=5 {
            let (info, text) = pdf.page_info(page).unwrap();
            let count = CONTENT_DECODE_BYTES.with(|count| count.get());
            counts.push(count);
            assert!(
                count <= PDF_CONTENT_LIMIT,
                "page {page} decoded {count} bytes"
            );
            let flagged = matches!(page, 1 | 3 | 4);
            assert_eq!(info.content_too_large, flagged);
            assert_eq!(text.content_too_large, flagged);
            if flagged {
                assert_eq!(info.kind, PageKind::NoText);
                assert_eq!(info.suspicious, None);
                assert_eq!(info.chars, 0);
                assert_eq!(
                    (text.visible_chars, text.invisible_chars, text.undecodable),
                    (0, 0, 0)
                );
                assert!(text.text.is_empty());
            } else {
                assert_eq!(info.kind, PageKind::Text);
            }
        }
        assert_eq!(counts[0], 0);
        assert!(counts[2] > 3 * 512 * 1024);
        assert!(counts[3] > 2 * 768 * 1024);
        assert_eq!(counts[4], PDF_CONTENT_LIMIT - 1);
        let (read, inspect) = document_tools(doc);
        for page in [1usize, 3, 4] {
            let unit = &read["units"][page - 1];
            assert_eq!(unit["kind"], "no_text");
            assert_eq!(unit["contentTooLarge"], true);
            assert!(unit.get("source").is_none());
            assert!(read["text"].as_str().unwrap().contains(&format!("--- page {page} · text not extracted (page content exceeds the extraction limits) ---")));
        }
        assert!(
            read["text"]
                .as_str()
                .unwrap()
                .contains("Normal Helvetica text")
        );
        assert!(
            read["text"]
                .as_str()
                .unwrap()
                .contains("Just under the page decode limit")
        );
        assert!(!read["text"].as_str().unwrap().contains("Discard repeated"));
        assert_eq!(inspect["contentTooLarge"], "1,3-4");
        assert_eq!(inspect["pages"]["no_text"], "1,3-4");
        assert_eq!(inspect["pages"]["text"], "2,5");
        assert_eq!(inspect["suspicious"], json!({}));
        assert_eq!(inspect["complete"], true);
        eprintln!(
            "content amplification: bytes handed to Content::decode per page = {counts:?}; limit={PDF_CONTENT_LIMIT}"
        );
    }
    #[test]
    fn page_content_streams_share_the_budget_including_unfiltered_streams() {
        let (mut doc, _) = fixture();
        let page = doc.get_pages()[&1];
        let first = doc.add_object(Stream::new(
            Dictionary::new(),
            dense_content(PDF_CONTENT_LIMIT / 2 + 1, b""),
        ));
        let second = doc.add_object(Stream::new(
            Dictionary::new(),
            dense_content(PDF_CONTENT_LIMIT / 2, b""),
        ));
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Contents", vec![first.into(), second.into()]);
        let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
        assert!(pdf.page_text(1).unwrap().content_too_large);
        assert_eq!(CONTENT_DECODE_BYTES.with(|count| count.get()), 0);
        // lopdf appends a newline; that byte also must fit before Content::decode.
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Contents", first);
        doc.get_object_mut(first)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_content(dense_content(PDF_CONTENT_LIMIT, b""));
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        assert!(pdf.page_text(1).unwrap().content_too_large);
        assert_eq!(CONTENT_DECODE_BYTES.with(|count| count.get()), 0);
    }
    #[test]
    fn font_cmap_keeps_the_noncontent_limit_and_limit_errors_surface() {
        for (length, should_fail) in [(3 * 1024 * 1024, false), (PDF_STREAM_LIMIT + 1, true)] {
            let (mut doc, _) = fixture();
            let resources = inherited_resources(&doc);
            let font = doc
                .get_dictionary(resources)
                .unwrap()
                .get(b"Font")
                .unwrap()
                .as_dict()
                .unwrap()
                .get(b"C")
                .unwrap()
                .as_reference()
                .unwrap();
            let cmap = doc
                .get_dictionary(font)
                .unwrap()
                .get(b"ToUnicode")
                .unwrap()
                .as_reference()
                .unwrap();
            let mut content = doc
                .get_object(cmap)
                .unwrap()
                .as_stream()
                .unwrap()
                .content
                .clone();
            content.resize(length, b' ');
            doc.objects.insert(cmap, compressed(content).into());
            if should_fail {
                let tmp = tempfile::tempdir().unwrap();
                let root = crate::filesystem::RootDir::open(&crate::config::Root {
                    id: "work".into(),
                    path: tmp.path().into(),
                    nickname: None,
                })
                .unwrap();
                std::fs::write(tmp.path().join("font-limit.pdf"), bytes(doc)).unwrap();
                for tool in [
                    crate::documents::read_document,
                    crate::documents::inspect_document,
                ] {
                    assert_eq!(
                        tool(&root, "work", &json!({"path":"font-limit.pdf"}))
                            .unwrap_err()
                            .code,
                        "DOCUMENT_INVALID"
                    );
                }
            } else {
                let (read, inspect) = document_tools(doc);
                assert_eq!(read["units"][4]["source"], "text_layer");
                assert_eq!(inspect["contentTooLarge"], "");
            }
        }
    }
    fn cmap_bytes(sections: &str) -> Vec<u8> {
        format!("/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n{sections}\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend").into_bytes()
    }
    fn map_fixture(maps: Vec<Vec<u8>>, font_maps: &[usize], page_count: usize) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages = doc.new_object_id();
        let helvetica =
            doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
        let streams = maps
            .into_iter()
            .map(|map| doc.add_object(compressed(map)))
            .collect::<Vec<_>>();
        let mut fonts = dictionary! {"H"=>helvetica};
        let mut text = b"BT /H 12 Tf (Helvetica text survives font map guards: ) Tj ET ".to_vec();
        for (i, map) in font_maps.iter().enumerate() {
            let font = doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type0","BaseFont"=>"Synthetic","Encoding"=>"Identity-H","ToUnicode"=>streams[*map]});
            fonts.set(format!("F{i}"), font);
            text.extend_from_slice(format!("BT /F{i} 12 Tf <0041> Tj ET ").as_bytes());
        }
        let resources = doc.add_object(dictionary! {"Font"=>fonts});
        let content = doc.add_object(Stream::new(Dictionary::new(), text));
        let mut kids = Vec::new();
        for _ in 0..page_count {
            kids.push(
                doc.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,"Contents"=>content})
                    .into(),
            );
        }
        doc.objects.insert(pages, dictionary! {"Type"=>"Pages","Kids"=>kids,"Count"=>page_count as i64,"Resources"=>resources,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()]}.into());
        let catalog = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
        doc.trailer.set("Root", catalog);
        doc
    }
    fn resource_font(doc: &Document, name: &[u8]) -> ObjectId {
        doc.get_dictionary(inherited_resources(doc))
            .unwrap()
            .get(b"Font")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(name)
            .unwrap()
            .as_reference()
            .unwrap()
    }
    fn set_page_contents(doc: &mut Document, content: Vec<u8>) {
        let id = doc.add_object(Stream::new(Dictionary::new(), content));
        for (_, page) in doc.get_pages() {
            doc.get_object_mut(page)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("Contents", id);
        }
    }
    fn long_target_fixture(content: Vec<u8>, pages: usize) -> Document {
        let target = "0041".repeat(255) + "0000";
        let map = String::from_utf8(cmap_bytes(&format!(
            "1 beginbfrange\n<00> <FF> <{target}>\nendbfrange"
        )))
        .unwrap()
        .replace("<0000> <FFFF>", "<00> <FF>");
        let mut doc = map_fixture(vec![map.into_bytes()], &[0], pages);
        set_page_contents(&mut doc, content);
        let page = doc.get_pages()[&1];
        let content = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_object_mut(content)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .compress()
            .unwrap();
        doc
    }
    fn glyph_content(operator: &str, shown_bytes: usize, chunk: usize) -> Vec<u8> {
        let mut content = b"BT /F0 1 Tf ".to_vec();
        if operator == "TJ" {
            content.push(b'[');
        }
        let mut left = shown_bytes;
        while left > 0 {
            if operator == "\"" {
                content.extend_from_slice(b"0 0 ");
            }
            content.push(b'(');
            let length = left.min(chunk);
            content.extend(std::iter::repeat_n(b'A', length));
            content.push(b')');
            if operator == "TJ" {
                content.extend_from_slice(b" -300 ");
            } else {
                content.extend_from_slice(format!(" {operator} ").as_bytes());
            }
            left -= length;
        }
        if operator == "TJ" {
            content.extend_from_slice(b"] TJ ");
        }
        content.extend_from_slice(b"ET");
        content
    }
    #[test]
    fn glyph_output_also_obeys_the_work_budget_before_decoding() {
        let doc = long_target_fixture(glyph_content("Tj", 3 * 8192, 8192), 1);
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        let _limit = WorkLimitOverride::new(1 << 20);
        let text = pdf.page_text(1).unwrap();
        assert!(text.content_too_large);
        assert!(text.text.is_empty());
        let counters = PAGE_COUNTERS.with(|c| c.get());
        assert_eq!(counters.stopped_by, Some(PageLimit::Work));
        assert_eq!(counters.shown_bytes, 0);
        assert_eq!(counters.glyph_units, 8192 * 256);
    }
    #[test]
    fn glyph_output_rejects_large_strings_before_lopdf_and_inspects_three_pages() {
        let length = PDF_CONTENT_LIMIT - 4096;
        let content = glyph_content("Tj", length, length);
        assert!(content.len() < PDF_CONTENT_LIMIT);
        let doc = long_target_fixture(content, 3);
        let data = bytes(doc.clone());
        assert!(data.len() < 16_384);
        let started = Instant::now();
        let pdf = PdfDocument::load(&data, &HashSet::new()).unwrap();
        let (info, text) = pdf.page_info(1).unwrap();
        assert!(info.content_too_large);
        assert_eq!(info.kind, PageKind::NoText);
        assert!(text.text.is_empty());
        let counters = PAGE_COUNTERS.with(|c| c.get());
        assert_eq!(counters.stopped_by, Some(PageLimit::GlyphOutput));
        assert_eq!(counters.glyph_units, 0);
        assert_eq!(counters.shown_bytes, 0);
        assert!(counters.work <= PDF_PAGE_WORK_LIMIT);
        assert!(started.elapsed() < std::time::Duration::from_millis(2500));
        let started = Instant::now();
        let (read, inspect) = document_tools(doc);
        assert!(started.elapsed() < crate::filesystem::SEARCH_DEADLINE);
        assert_eq!(read["units"].as_array().unwrap().len(), 3);
        for unit in read["units"].as_array().unwrap() {
            assert_eq!(unit["contentTooLarge"], true);
            assert_eq!(unit["kind"], "no_text");
        }
        assert!(read["text"].as_str().unwrap().contains(
            "--- page 1 · text not extracted (page content exceeds the extraction limits) ---"
        ));
        assert_eq!(inspect["contentTooLarge"], "1-3");
        assert_eq!(inspect["pages"]["no_text"], "1-3");
        assert_eq!(inspect["classified"], 3);
        assert_eq!(inspect["complete"], true);
    }
    #[test]
    fn glyph_output_short_strings_are_cumulative_for_every_text_operator() {
        for operator in ["Tj", "'", "\"", "TJ"] {
            let content = glyph_content(operator, PDF_CONTENT_LIMIT - 4096, 8192);
            assert!(content.len() < PDF_CONTENT_LIMIT);
            let doc = long_target_fixture(content, 3);
            let data = bytes(doc.clone());
            let pdf = PdfDocument::load(&data, &HashSet::new()).unwrap();
            let text = pdf.page_text(1).unwrap();
            assert!(text.content_too_large, "{operator}");
            assert!(text.text.is_empty());
            let counters = PAGE_COUNTERS.with(|c| c.get());
            assert_eq!(counters.stopped_by, Some(PageLimit::GlyphOutput));
            assert_eq!(counters.shown_bytes, 16_384);
            assert_eq!(counters.glyph_units, PDF_PAGE_GLYPH_LIMIT);
            assert!(counters.shown_bytes as u64 * 256 <= PDF_PAGE_GLYPH_LIMIT);
            assert!(counters.work <= PDF_PAGE_WORK_LIMIT);
            let started = Instant::now();
            let (_, inspect) = document_tools(doc);
            assert!(started.elapsed() < crate::filesystem::SEARCH_DEADLINE);
            assert_eq!(inspect["contentTooLarge"], "1-3");
            assert_eq!(inspect["classified"], 3);
            assert_eq!(inspect["complete"], true);
        }
    }
    #[test]
    fn glyph_output_cap_boundary_and_small_strings_decode_normally() {
        for length in [64, 16_384, 16_385] {
            let doc = long_target_fixture(glyph_content("Tj", length, length), 1);
            let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
            let text = pdf.page_text(1).unwrap();
            let counters = PAGE_COUNTERS.with(|c| c.get());
            if length <= 16_384 {
                assert!(!text.content_too_large);
                assert_eq!(text.text, "A".repeat(length * 256));
                assert_eq!(text.text.encode_utf16().count(), length * 256);
                assert_eq!(counters.glyph_units, length as u64 * 256);
                assert_eq!(counters.shown_bytes, length);
                assert_eq!(counters.stopped_by, None);
            } else {
                assert!(text.content_too_large);
                assert!(text.text.is_empty());
                assert_eq!(counters.glyph_units, 0);
                assert_eq!(counters.shown_bytes, 0);
                assert_eq!(counters.stopped_by, Some(PageLimit::GlyphOutput));
            }
        }
    }
    #[test]
    fn glyph_output_preserves_ordinary_fonts_and_ligatures_with_exact_charge() {
        let map = String::from_utf8(cmap_bytes(
            "2 beginbfchar\n<41> <0041>\n<66> <006600660069>\nendbfchar",
        ))
        .unwrap()
        .replace("<0000> <FFFF>", "<00> <FF>");
        let mut doc = map_fixture(vec![map.into_bytes()], &[0], 1);
        let length = 512 * 1024;
        let mut content = b"BT /H 1 Tf (".to_vec();
        content.extend(std::iter::repeat_n(b'H', length));
        content.extend_from_slice(b") Tj /F0 1 Tf (");
        content.extend(std::iter::repeat_n(b'A', length - 1));
        content.extend_from_slice(b"f) Tj ET");
        set_page_contents(&mut doc, content);
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        let text = pdf.page_text(1).unwrap();
        assert!(!text.content_too_large);
        assert_eq!(
            text.text,
            "H".repeat(length) + &"A".repeat(length - 1) + "ffi"
        );
        let counters = PAGE_COUNTERS.with(|c| c.get());
        assert_eq!(counters.glyph_units, length as u64 + length as u64 * 3);
        assert!(counters.glyph_units < PDF_PAGE_GLYPH_LIMIT);
        assert_eq!(counters.shown_bytes, 2 * length);
        assert_eq!(counters.stopped_by, None);
    }
    #[test]
    fn glyph_output_charge_checks_deadlines_before_decoding() {
        let doc = long_target_fixture(glyph_content("Tj", 8192, 8192), 1);
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        let expired = Instant::now() - std::time::Duration::from_secs(1);
        assert!(pdf.page_info_within(1, Some(expired)).unwrap().is_none());
        let counters = PAGE_COUNTERS.with(|c| c.get());
        assert_eq!(counters.glyph_units, 8192 * 256);
        assert_eq!(counters.shown_bytes, 0);
        assert_eq!(counters.stopped_by, None);
        assert!(!pdf.page_text(1).unwrap().content_too_large);
    }
    #[test]
    fn glyph_output_fallback_and_text_outside_bt_keep_their_existing_behavior() {
        let mut doc = map_fixture(vec![], &[], 1);
        set_page_contents(
            &mut doc,
            b"(outside) Tj BT (ab) Tj /Missing 1 Tf (cd) Tj /H 1 Tf (ok) Tj ET (outside) Tj"
                .to_vec(),
        );
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        let text = pdf.page_text(1).unwrap();
        assert!(!text.content_too_large);
        assert_eq!(text.text, "\u{fffd}\u{fffd}\u{fffd}\u{fffd}ok");
        let counters = PAGE_COUNTERS.with(|c| c.get());
        assert_eq!(counters.glyph_units, 6);
        assert_eq!(counters.shown_bytes, 2);
    }
    // The 3,734 distinct glyph names recognized by lopdf (one per Unicode unit).
    const DIFFERENCES_GLYPHS: &str = "\
controlSTX controlSOT controlETX controlEOT controlENQ controlACK controlBEL controlBS controlHT controlLF controlVT controlFF controlCR controlSO controlSI controlDLE
controlDC1 controlDC2 controlDC3 controlDC4 controlNAK controlSYN controlETB controlCAN controlEM controlSUB controlESC controlFS controlGS controlRS controlUS space
exclam quotedbl numbersign dollar percent ampersand quotesingle parenleft parenright asterisk plus comma hyphen period slash zero
one two three four five six seven eight nine colon semicolon less equal greater question at
A B C D E F G H I J K L M N O P
Q R Germandbls T U V W X Y Z bracketleft backslash bracketright asciicircum underscore grave
a b c d e f g h i j k l m n o p
q r s t u v w x y z braceleft bar braceright asciitilde controlDEL nbspace
exclamdown cent sterling currency yen brokenbar section dieresis copyright ordfeminine guillemotleft logicalnot sfthyphen registered macron degree
plusminus twosuperior threesuperior acute mu paragraph middot cedilla onesuperior ordmasculine guillemotright onequarter onehalf threequarters questiondown Agrave
Aacute Acircumflex Atilde Adieresis Aring AE Ccedilla Egrave Eacute Ecircumflex Edieresis Igrave Iacute Icircumflex Idieresis Eth
Ntilde Ograve Oacute Ocircumflex Otilde Odieresis multiply Oslash Ugrave Uacute Ucircumflex Udieresis Yacute Thorn germandbls agrave
aacute acircumflex atilde adieresis aring ae ccedilla egrave eacute ecircumflex edieresis igrave iacute icircumflex idieresis eth
ntilde ograve oacute ocircumflex otilde odieresis divide oslash ugrave uacute ucircumflex udieresis yacute thorn ydieresis Amacron
amacron Abreve abreve Aogonek aogonek Cacute cacute Ccircumflex ccircumflex Cdot cdot Ccaron ccaron Dcaron dcaron Dbar
dbar Emacron emacron Ebreve ebreve Edot edot Eogonek eogonek Ecaron ecaron Gcircumflex gcircumflex Gbreve gbreve Gdot
gdot Gcedilla gcedilla Hcircumflex hcircumflex Hbar hbar Itilde itilde Imacron imacron Ibreve ibreve Iogonek iogonek Idot
dotlessi IJ ij Jcircumflex jcircumflex Kcedilla kcedilla kgreenlandic Lacute lacute Lcedilla lcedilla Lcaron lcaron Ldot ldot
Lslash lslash Nacute nacute Ncedilla ncedilla Ncaron ncaron napostrophe Eng eng Omacron omacron Obreve obreve Odblacute
odblacute OE oe Racute racute Rcedilla rcedilla Rcaron rcaron Sacute sacute Scircumflex scircumflex Scedilla scedilla Scaron
scaron Tcedilla tcedilla Tcaron tcaron Tbar tbar Utilde utilde Umacron umacron Ubreve ubreve Uring uring Udblacute
udblacute Uogonek uogonek Wcircumflex wcircumflex Ycircumflex ycircumflex Ydieresis Zacute zacute Zdot zdot Zcaron zcaron longs bstroke
Bhook Btopbar btopbar Tonesix tonesix Oopen Chook chook Dafrican Dhook Dtopbar dtopbar deltaturned Ereversed Schwa Eopen
Fhook florin Ghook Gammaafrican hv Iotaafrican Istroke Khook khook lbar lambdastroke Mturned Nhookleft nlegrightlong Ocenteredtilde Ohorn
ohorn Oi oi Phook phook yr Tonetwo tonetwo Esh eshreversedloop tpalatalhook Thook thook Tretroflexhook Uhorn uhorn
Upsilonafrican Vhook Yhook yhook Zstroke zstroke Ezh Ezhreversed ezhreversed ezhtail twostroke Tonefive tonefive glottalinvertedstroke wynn clickdental
clicklateral clickalveolar clickretroflex DZcaron Dzcaron dzcaron LJ Lj lj NJ Nj nj Acaron acaron Icaron icaron
Ocaron ocaron Ucaron ucaron Udieresismacron udieresismacron Udieresisacute udieresisacute Udieresiscaron udieresiscaron Udieresisgrave udieresisgrave eturned Adieresismacron adieresismacron Adotmacron
adotmacron AEmacron aemacron Gstroke gstroke Gcaron gcaron Kcaron kcaron Oogonek oogonek Oogonekmacron oogonekmacron Ezhcaron ezhcaron jcaron
DZ Dz dz Gacute gacute Aringacute aringacute AEacute aeacute Oslashacute oslashacute Adblgrave adblgrave Ainvertedbreve ainvertedbreve Edblgrave
edblgrave Einvertedbreve einvertedbreve Idblgrave idblgrave Iinvertedbreve iinvertedbreve Odblgrave odblgrave Oinvertedbreve oinvertedbreve Rdblgrave rdblgrave Rinvertedbreve rinvertedbreve Udblgrave
udblgrave Uinvertedbreve uinvertedbreve Scommaaccent scommaaccent dotlessj aturned ascript ascriptturned bhook oopen ccurl dtail dhook ereversed schwa
schwahook eopen eopenreversed eopenreversedhook eopenreversedclosed jdotlessstroke ghook gscript gammalatinsmall ramshorn hturned hhook henghook istroke iotalatin lmiddletilde
lbelt lhookretroflex lezh mturned mlonglegturned mhook nhookleft nhookretroflex obarred omegalatinclosed philatin rturned rlonglegturned rhookturned rlongleg rhook
rfishhook rfishhookreversed Rsmallinverted shook esh dotlessjstrokehook eshsquatreversed eshcurl tturned tretroflexhook ubar upsilonlatin vhook vturned wturned yturned
zretroflexhook zcurl ezh ezhcurl glottalstop glottalstopreversed glottalstopinverted cstretched bilabialclick eopenclosed Gsmallhook jcrossedtail kturned qhook glottalstopstroke glottalstopstrokereversed
dzaltone dezh dzcurl ts tesh tccurl hsuperior hhooksuperior jsuperior rturnedsuperior rhookturnedsuperior Rsmallinvertedsuperior wsuperior ysuperior primemod dblprimemod
commaturnedmod afii57929 afii64937 ringhalfright ringhalfleft glottalstopmod glottalstopreversedmod arrowheadleftmod arrowheadrightmod arrowheadupmod arrowheaddownmod circumflex caron verticallinemod firsttonechinese secondtonechinese
fourthtonechinese verticallinelowmod macronlowmod gravelowmod acutelowmod colontriangularmod colontriangularhalfmod ringhalfrightcentered ringhalfleftcentered uptackmod downtackmod plusmod minusmod breve dotaccent ring
ogonek ilde hungarumlaut rhotichookmod gammasuperior xsuperior glottalstopreversedsuperior tonebarextrahighmod tonebarhighmod tonebarmidmod tonebarlowmod tonebarextralowmod gravecmb acutecmb circumflexcmb tildecmb
macroncmb overlinecmb brevecmb dotaccentcmb dieresiscmb hookabovecomb ringcmb hungarumlautcmb caroncmb verticallineabovecmb dblverticallineabovecmb dblgravecmb candrabinducmb breveinvertedcmb commaturnedabovecmb commaabovecmb
commareversedabovecmb commaaboverightcmb gravebelowcmb acutebelowcmb lefttackbelowcmb righttackbelowcmb leftangleabovecmb horncmb ringhalfleftbelowcmb uptackbelowcmb downtackbelowcmb plusbelowcmb minusbelowcmb hookpalatalizedbelowcmb hookretroflexbelowcmb dotbelowcmb
dieresisbelowcmb ringbelowcmb cedillacmb ogonekcmb verticallinebelowcmb bridgebelowcmb dblarchinvertedbelowcmb caronbelowcmb circumflexbelowcmb brevebelowcmb breveinvertedbelowcmb tildebelowcmb macronbelowcmb lowlinecmb dbllowlinecmb tildeoverlaycmb
strokeshortoverlaycmb strokelongoverlaycmb solidusshortoverlaycmb negationslash ringhalfrightbelowcmb bridgeinvertedbelowcmb squarebelowcmb seagullbelowcmb xabovecmb tildeverticalcmb dbloverlinecmb gravetonecmb acutetonecmb perispomenigreekcmb koroniscmb dialytikatonoscmb
ypogegrammenigreekcmb tildedoublecmb breveinverteddoublecmb numeralsigngreek numeralsignlowergreek ypogegrammeni questiongreek tonos dialytikatonos Alphatonos anoteleia Epsilontonos Etatonos Iotatonos Omicrontonos Upsilontonos
Omegatonos iotadieresistonos Alpha Beta Gamma Deltagreek Epsilon Zeta Eta Theta Iota Kappa Lambda Mu Nu Xi
Omicron Pi Rho Sigma Tau Upsilon Phi Chi Psi Omegagreek Iotadieresis Upsilondieresis alphatonos epsilontonos etatonos iotatonos
upsilondieresistonos alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mugreek nu xi omicron
pi rho sigma1 sigma tau upsilon phi chi psi omega iotadieresis upsilondieresis omicrontonos upsilontonos omegatonos betasymbolgreek
theta1 Upsilon1 Upsilonacutehooksymbolgreek Upsilondieresishooksymbolgreek phi2 omega1 Stigmagreek Digammagreek Koppagreek Sampigreek Sheicoptic sheicoptic Feicoptic feicoptic Kheicoptic kheicoptic
Horicoptic horicoptic Gangiacoptic gangiacoptic Shimacoptic shimacoptic Deicoptic deicoptic kappasymbolgreek rho1 sigmalunatesymbolgreek yotgreek epsilon1 afii10023 afii10051 afii10052
afii10053 afii10054 afii10055 afii10056 afii10057 afii10058 afii10059 afii10060 afii10061 afii10062 afii10145 Acyrillic afii10018 afii10019 afii10020 afii10021
afii10022 afii10024 afii10025 afii10026 afii10027 afii10028 afii10029 afii10030 afii10031 afii10032 afii10033 afii10034 afii10035 afii10036 afii10037 afii10038
afii10039 afii10040 afii10041 afii10042 afii10043 afii10044 afii10045 afii10046 afii10047 afii10048 afii10049 acyrillic afii10066 afii10067 afii10068 afii10069
afii10070 afii10072 afii10073 afii10074 afii10075 afii10076 afii10077 afii10078 afii10079 afii10080 afii10081 afii10082 afii10083 afii10084 afii10085 afii10086
afii10087 afii10088 afii10089 afii10090 afii10091 afii10092 afii10093 afii10094 afii10095 afii10096 afii10097 afii10071 afii10099 afii10100 afii10101 afii10102
afii10103 afii10104 afii10105 afii10106 afii10107 afii10108 afii10109 afii10110 afii10193 Omegacyrillic omegacyrillic afii10146 afii10194 Eiotifiedcyrillic eiotifiedcyrillic Yuslittlecyrillic
yuslittlecyrillic Yuslittleiotifiedcyrillic yuslittleiotifiedcyrillic Yusbigcyrillic yusbigcyrillic Yusbigiotifiedcyrillic yusbigiotifiedcyrillic Ksicyrillic ksicyrillic Psicyrillic psicyrillic afii10147 afii10195 afii10148 afii10196 Izhitsadblgravecyrillic
izhitsadblgravecyrillic Ukcyrillic ukcyrillic Omegaroundcyrillic omegaroundcyrillic Omegatitlocyrillic omegatitlocyrillic Otcyrillic otcyrillic Koppacyrillic koppacyrillic thousandcyrillic titlocyrilliccmb palatalizationcyrilliccmb dasiapneumatacyrilliccmb psilipneumatacyrilliccmb
afii10050 afii10098 Ghestrokecyrillic ghestrokecyrillic Ghemiddlehookcyrillic ghemiddlehookcyrillic Zhedescendercyrillic zhedescendercyrillic Zedescendercyrillic zedescendercyrillic Kadescendercyrillic kadescendercyrillic Kaverticalstrokecyrillic kaverticalstrokecyrillic Kastrokecyrillic kastrokecyrillic
Kabashkircyrillic kabashkircyrillic Endescendercyrillic endescendercyrillic Enghecyrillic enghecyrillic Pemiddlehookcyrillic pemiddlehookcyrillic Haabkhasiancyrillic haabkhasiancyrillic Esdescendercyrillic esdescendercyrillic Tedescendercyrillic tedescendercyrillic Ustraightcyrillic ustraightcyrillic
Ustraightstrokecyrillic ustraightstrokecyrillic Hadescendercyrillic hadescendercyrillic Tetsecyrillic tetsecyrillic Chedescendercyrillic chedescendercyrillic Cheverticalstrokecyrillic cheverticalstrokecyrillic Shhacyrillic shhacyrillic Cheabkhasiancyrillic cheabkhasiancyrillic Chedescenderabkhasiancyrillic chedescenderabkhasiancyrillic
palochkacyrillic Zhebrevecyrillic zhebrevecyrillic Kahookcyrillic kahookcyrillic Enhookcyrillic enhookcyrillic Chekhakassiancyrillic chekhakassiancyrillic Abrevecyrillic abrevecyrillic Adieresiscyrillic adieresiscyrillic Aiecyrillic aiecyrillic Iebrevecyrillic
iebrevecyrillic Schwacyrillic afii10846 Schwadieresiscyrillic schwadieresiscyrillic Zhedieresiscyrillic zhedieresiscyrillic Zedieresiscyrillic zedieresiscyrillic Dzeabkhasiancyrillic dzeabkhasiancyrillic Imacroncyrillic imacroncyrillic Idieresiscyrillic idieresiscyrillic Odieresiscyrillic
odieresiscyrillic Obarredcyrillic obarredcyrillic Obarreddieresiscyrillic obarreddieresiscyrillic Umacroncyrillic umacroncyrillic Udieresiscyrillic udieresiscyrillic Uhungarumlautcyrillic uhungarumlautcyrillic Chedieresiscyrillic chedieresiscyrillic Yerudieresiscyrillic yerudieresiscyrillic Aybarmenian
Benarmenian Gimarmenian Daarmenian Echarmenian Zaarmenian Eharmenian Etarmenian Toarmenian Zhearmenian Iniarmenian Liwnarmenian Xeharmenian Caarmenian Kenarmenian Hoarmenian Jaarmenian
Ghadarmenian Cheharmenian Menarmenian Yiarmenian Nowarmenian Shaarmenian Voarmenian Chaarmenian Peharmenian Jheharmenian Raarmenian Seharmenian Vewarmenian Tiwnarmenian Reharmenian Coarmenian
Yiwnarmenian Piwrarmenian Keharmenian Oharmenian Feharmenian ringhalfleftarmenian apostrophearmenian emphasismarkarmenian exclamarmenian commaarmenian questionarmenian abbreviationmarkarmenian aybarmenian benarmenian gimarmenian daarmenian
echarmenian zaarmenian eharmenian etarmenian toarmenian zhearmenian iniarmenian liwnarmenian xeharmenian caarmenian kenarmenian hoarmenian jaarmenian ghadarmenian cheharmenian menarmenian
yiarmenian nowarmenian shaarmenian voarmenian chaarmenian peharmenian jheharmenian raarmenian seharmenian vewarmenian tiwnarmenian reharmenian coarmenian yiwnarmenian piwrarmenian keharmenian
oharmenian feharmenian echyiwnarmenian periodarmenian etnahtafoukhhebrew segoltahebrew shalshelethebrew zaqefqatanhebrew zaqefgadolhebrew tipehahebrew reviahebrew zarqahebrew pashtahebrew yetivhebrew tevirhebrew gereshaccenthebrew
gereshmuqdamhebrew gershayimaccenthebrew qarneyparahebrew telishagedolahebrew pazerhebrew munahhebrew mahapakhhebrew merkhahebrew merkhakefulahebrew dargahebrew qadmahebrew telishaqetanahebrew yerahbenyomohebrew olehebrew iluyhebrew dehihebrew
zinorhebrew masoracirclehebrew afii57799 afii57801 afii57800 afii57802 afii57793 afii57794 afii57795 afii57798 afii57797 afii57806 afii57796 afii57807 afii57839 afii57645
afii57841 afii57842 afii57804 afii57803 afii57658 upperdothebrew afii57664 afii57665 afii57666 afii57667 afii57668 afii57669 afii57670 afii57671 afii57672 afii57673
afii57674 afii57675 afii57676 afii57677 afii57678 afii57679 afii57680 afii57681 afii57682 afii57683 afii57684 afii57685 afii57686 afii57687 afii57688 afii57689
afii57690 afii57716 afii57717 afii57718 gereshhebrew gershayimhebrew afii57388 afii57403 afii57407 afii57409 afii57410 afii57411 afii57412 afii57413 afii57414 afii57415
afii57416 afii57417 afii57418 afii57419 afii57420 afii57421 afii57422 afii57423 afii57424 afii57425 afii57426 afii57427 afii57428 afii57429 afii57430 afii57431
afii57432 afii57433 afii57434 afii57440 afii57441 afii57442 afii57443 afii57444 afii57445 afii57446 afii57470 afii57448 afii57449 afii57450 afii57451 afii57452
afii57453 afii57454 afii57455 afii57456 afii57457 afii57458 afii57392 afii57393 afii57394 afii57395 afii57396 afii57397 afii57398 afii57399 afii57400 afii57401
afii57381 decimalseparatorarabic thousandsseparatorarabic afii63167 afii57511 afii57506 afii57507 afii57512 afii57513 afii57508 afii57505 afii57509 afii57514 haaltonearabic yehthreedotsbelowarabic afii57519
afii57534 zeropersian onepersian twopersian threepersian fourpersian fivepersian sixpersian sevenpersian eightpersian ninepersian candrabindudeva anusvaradeva visargadeva adeva aadeva
ideva iideva udeva uudeva rvocalicdeva lvocalicdeva ecandradeva eshortdeva edeva aideva ocandradeva oshortdeva odeva audeva kadeva khadeva
gadeva ghadeva ngadeva cadeva chadeva jadeva jhadeva nyadeva ttadeva tthadeva ddadeva ddhadeva nnadeva tadeva thadeva dadeva
dhadeva nadeva nnnadeva padeva phadeva badeva bhadeva madeva yadeva radeva rradeva ladeva lladeva llladeva vadeva shadeva
ssadeva sadeva hadeva nuktadeva avagrahadeva aavowelsigndeva ivowelsigndeva iivowelsigndeva uvowelsigndeva uuvowelsigndeva rvocalicvowelsigndeva rrvocalicvowelsigndeva ecandravowelsigndeva eshortvowelsigndeva evowelsigndeva aivowelsigndeva
ocandravowelsigndeva oshortvowelsigndeva ovowelsigndeva auvowelsigndeva viramadeva omdeva udattadeva anudattadeva gravedeva acutedeva qadeva khhadeva ghhadeva zadeva dddhadeva rhadeva
fadeva yyadeva rrvocalicdeva llvocalicdeva lvocalicvowelsigndeva llvocalicvowelsigndeva danda dbldanda zerodeva onedeva twodeva threedeva fourdeva fivedeva sixdeva sevendeva
eightdeva ninedeva abbreviationsigndeva candrabindubengali anusvarabengali visargabengali abengali aabengali ibengali iibengali ubengali uubengali rvocalicbengali lvocalicbengali ebengali aibengali
obengali aubengali kabengali khabengali gabengali ghabengali ngabengali cabengali chabengali jabengali jhabengali nyabengali ttabengali tthabengali ddabengali ddhabengali
nnabengali tabengali thabengali dabengali dhabengali nabengali pabengali phabengali babengali bhabengali mabengali yabengali rabengali labengali shabengali ssabengali
sabengali habengali nuktabengali aavowelsignbengali ivowelsignbengali iivowelsignbengali uvowelsignbengali uuvowelsignbengali rvocalicvowelsignbengali rrvocalicvowelsignbengali evowelsignbengali aivowelsignbengali ovowelsignbengali auvowelsignbengali viramabengali aulengthmarkbengali
rrabengali rhabengali yyabengali rrvocalicbengali llvocalicbengali lvocalicvowelsignbengali llvocalicvowelsignbengali zerobengali onebengali twobengali threebengali fourbengali fivebengali sixbengali sevenbengali eightbengali
ninebengali ramiddlediagonalbengali ralowerdiagonalbengali rupeemarkbengali rupeesignbengali onenumeratorbengali twonumeratorbengali threenumeratorbengali fournumeratorbengali denominatorminusonenumeratorbengali sixteencurrencydenominatorbengali issharbengali bindigurmukhi agurmukhi aagurmukhi igurmukhi
iigurmukhi ugurmukhi uugurmukhi eegurmukhi aigurmukhi oogurmukhi augurmukhi kagurmukhi khagurmukhi gagurmukhi ghagurmukhi ngagurmukhi cagurmukhi chagurmukhi jagurmukhi jhagurmukhi
nyagurmukhi ttagurmukhi tthagurmukhi ddagurmukhi ddhagurmukhi nnagurmukhi tagurmukhi thagurmukhi dagurmukhi dhagurmukhi nagurmukhi pagurmukhi phagurmukhi bagurmukhi bhagurmukhi magurmukhi
yagurmukhi ragurmukhi lagurmukhi vagurmukhi shagurmukhi sagurmukhi hagurmukhi nuktagurmukhi aamatragurmukhi imatragurmukhi iimatragurmukhi umatragurmukhi uumatragurmukhi eematragurmukhi aimatragurmukhi oomatragurmukhi
aumatragurmukhi halantgurmukhi khhagurmukhi ghhagurmukhi zagurmukhi rragurmukhi fagurmukhi zerogurmukhi onegurmukhi twogurmukhi threegurmukhi fourgurmukhi fivegurmukhi sixgurmukhi sevengurmukhi eightgurmukhi
ninegurmukhi tippigurmukhi addakgurmukhi irigurmukhi uragurmukhi ekonkargurmukhi candrabindugujarati anusvaragujarati visargagujarati agujarati aagujarati igujarati iigujarati ugujarati uugujarati rvocalicgujarati
ecandragujarati egujarati aigujarati ocandragujarati ogujarati augujarati kagujarati khagujarati gagujarati ghagujarati ngagujarati cagujarati chagujarati jagujarati jhagujarati nyagujarati
ttagujarati tthagujarati ddagujarati ddhagujarati nnagujarati tagujarati thagujarati dagujarati dhagujarati nagujarati pagujarati phagujarati bagujarati bhagujarati magujarati yagujarati
ragujarati lagujarati llagujarati vagujarati shagujarati ssagujarati sagujarati hagujarati nuktagujarati aavowelsigngujarati ivowelsigngujarati iivowelsigngujarati uvowelsigngujarati uuvowelsigngujarati rvocalicvowelsigngujarati rrvocalicvowelsigngujarati
ecandravowelsigngujarati evowelsigngujarati aivowelsigngujarati ocandravowelsigngujarati ovowelsigngujarati auvowelsigngujarati viramagujarati omgujarati rrvocalicgujarati zerogujarati onegujarati twogujarati threegujarati fourgujarati fivegujarati sixgujarati
sevengujarati eightgujarati ninegujarati kokaithai khokhaithai khokhuatthai khokhwaithai khokhonthai khorakhangthai ngonguthai chochanthai chochingthai chochangthai sosothai chochoethai yoyingthai
dochadathai topatakthai thothanthai thonangmonthothai thophuthaothai nonenthai dodekthai totaothai thothungthai thothahanthai thothongthai nonuthai bobaimaithai poplathai phophungthai fofathai
phophanthai fofanthai phosamphaothai momathai yoyakthai roruathai ruthai lolingthai luthai wowaenthai sosalathai sorusithai sosuathai hohipthai lochulathai oangthai
honokhukthai paiyannoithai saraathai maihanakatthai saraaathai saraamthai saraithai saraiithai sarauethai saraueethai sarauthai sarauuthai phinthuthai bahtthai saraethai saraaethai
saraothai saraaimaimuanthai saraaimaimalaithai lakkhangyaothai maiyamokthai maitaikhuthai maiekthai maithothai maitrithai maichattawathai thanthakhatthai nikhahitthai yamakkanthai fongmanthai zerothai onethai
twothai threethai fourthai fivethai sixthai seventhai eightthai ninethai angkhankhuthai khomutthai Aringbelow aringbelow Bdotaccent bdotaccent Bdotbelow bdotbelow
Blinebelow blinebelow Ccedillaacute ccedillaacute Ddotaccent ddotaccent Ddotbelow ddotbelow Dlinebelow dlinebelow Dcedilla dcedilla Dcircumflexbelow dcircumflexbelow Emacrongrave emacrongrave
Emacronacute emacronacute Ecircumflexbelow ecircumflexbelow Etildebelow etildebelow Ecedillabreve ecedillabreve Fdotaccent fdotaccent Gmacron gmacron Hdotaccent hdotaccent Hdotbelow hdotbelow
Hdieresis hdieresis Hcedilla hcedilla Hbrevebelow hbrevebelow Itildebelow itildebelow Idieresisacute idieresisacute Kacute kacute Kdotbelow kdotbelow Klinebelow klinebelow
Ldotbelow ldotbelow Ldotbelowmacron ldotbelowmacron Llinebelow llinebelow Lcircumflexbelow lcircumflexbelow Macute macute Mdotaccent mdotaccent Mdotbelow mdotbelow Ndotaccent ndotaccent
Ndotbelow ndotbelow Nlinebelow nlinebelow Ncircumflexbelow ncircumflexbelow Otildeacute otildeacute Otildedieresis otildedieresis Omacrongrave omacrongrave Omacronacute omacronacute Pacute pacute
Pdotaccent pdotaccent Rdotaccent rdotaccent Rdotbelow rdotbelow Rdotbelowmacron rdotbelowmacron Rlinebelow rlinebelow Sdotaccent sdotaccent Sdotbelow sdotbelow Sacutedotaccent sacutedotaccent
Scarondotaccent scarondotaccent Sdotbelowdotaccent sdotbelowdotaccent Tdotaccent tdotaccent Tdotbelow tdotbelow Tlinebelow tlinebelow Tcircumflexbelow tcircumflexbelow Udieresisbelow udieresisbelow Utildebelow utildebelow
Ucircumflexbelow ucircumflexbelow Utildeacute utildeacute Umacrondieresis umacrondieresis Vtilde vtilde Vdotbelow vdotbelow Wgrave wgrave Wacute wacute Wdieresis wdieresis
Wdotaccent wdotaccent Wdotbelow wdotbelow Xdotaccent xdotaccent Xdieresis xdieresis Ydotaccent ydotaccent Zcircumflex zcircumflex Zdotbelow zdotbelow Zlinebelow zlinebelow
hlinebelow tdieresis wring yring arighthalfring slongdotaccent Germandbls2 Adotbelow adotbelow Ahookabove ahookabove Acircumflexacute acircumflexacute Acircumflexgrave acircumflexgrave Acircumflexhookabove
acircumflexhookabove Acircumflextilde acircumflextilde Acircumflexdotbelow acircumflexdotbelow Abreveacute abreveacute Abrevegrave abrevegrave Abrevehookabove abrevehookabove Abrevetilde abrevetilde Abrevedotbelow abrevedotbelow Edotbelow
edotbelow Ehookabove ehookabove Etilde etilde Ecircumflexacute ecircumflexacute Ecircumflexgrave ecircumflexgrave Ecircumflexhookabove ecircumflexhookabove Ecircumflextilde ecircumflextilde Ecircumflexdotbelow ecircumflexdotbelow Ihookabove
ihookabove Idotbelow idotbelow Odotbelow odotbelow Ohookabove ohookabove Ocircumflexacute ocircumflexacute Ocircumflexgrave ocircumflexgrave Ocircumflexhookabove ocircumflexhookabove Ocircumflextilde ocircumflextilde Ocircumflexdotbelow
ocircumflexdotbelow Ohornacute ohornacute Ohorngrave ohorngrave Ohornhookabove ohornhookabove Ohorntilde ohorntilde Ohorndotbelow ohorndotbelow Udotbelow udotbelow Uhookabove uhookabove Uhornacute
uhornacute Uhorngrave uhorngrave Uhornhookabove uhornhookabove Uhorntilde uhorntilde Uhorndotbelow uhorndotbelow Ygrave ygrave Ydotbelow ydotbelow Yhookabove yhookabove Ytilde
ytilde enspace zerowidthspace afii61664 afii301 afii299 afii300 hyphen_alt figuredash endash emdash afii00208 bardblex dbllowline quoteleft quoteright
quotesinglbase quoteleftreversed quotedblleft quotedblright quotedblbase dagger daggerdbl bullet onedotenleader twodotenleader ellipsis afii61573 afii61574 afii61575 perthousand pertenthousand
minute second primereversed guilsinglleft guilsinglright referencemark exclamdbl interrobang overline tie asterism fraction zerosuperior foursuperior fivesuperior sixsuperior
sevensuperior eightsuperior ninesuperior plussuperior equalsuperior parenleftsuperior parenrightsuperior nsuperior zeroinferior oneinferior twoinferior threeinferior fourinferior fiveinferior sixinferior seveninferior
eightinferior nineinferior parenleftinferior parenrightinferior colonmonetary cruzeiro franc afii08941 peseta won afii57636 dong euro vector circlecopyrt centigrade
afii61248 fahrenheit Ifractur afii61289 afii61352 weierstrass Rfractur prescription telephone trademark Ohm angstrom estimated aleph onethird twothirds
oneeighth threeeighths fiveeighths seveneighths Oneroman Tworoman Threeroman Fourroman Fiveroman Sixroman Sevenroman Eightroman Nineroman Tenroman Elevenroman Twelveroman
oneroman tworoman threeroman fourroman fiveroman sixroman sevenroman eightroman nineroman tenroman elevenroman twelveroman arrowleft arrowtp arrowright arrowbt
arrowboth arrowbothv arrownorthwest arrownortheast arrowdownright arrowdownleft arrowupdnbse hookleftchar hookrightchar carriagereturn arrowlefttophalf arrowleftbothalf arrowrighttophalf arrowrightbothalf arrowrightoverleft arrowupleftofdown
arrowleftoverright arrowleftdblstroke arrowrightdblstroke arrowdblleft arrowdbltp arrowdblright arrowdbldown arrowdblboth arrowdblbothv pageup pagedown arrowdashleft arrowdashup arrowdashright arrowdashdown arrowtableft
arrowtabright arrowleftwhite arrowupwhite arrowrightwhite arrowdownwhite capslock forall partialdiff existential emptyset Delta gradient element notelement owner notcontains
product coproductdisplay summation minus minusplus divisionslash asteriskcentered bulletoperator radical proportional infinity orthogonal angle divides bardbl notparallel
logicaland logicalor intersection union integral dblintegral contintegraldisplay therefore because ratio proportion similar reversedtilde wreathproduct asymptoticallyequal approximatelyequal
approxequal allequal equivasymptotic approaches geometricallyequal approxequalorimage imageorapproximatelyequal notequal equivalence notidentical lessequal greaterequal lessoverequal greateroverequal lessmuch greatermuch
notless notgreater notlessnorequal notgreaternorequal lessorequivalent greaterorequivalent lessorgreater greaterorless notgreaternorless precedes follows precedesequal followsequal notprecedes notsucceeds propersubset
propersuperset notsubset notsuperset reflexsubset reflexsuperset subsetnotequal supersetnotequal unionmulti subsetsqequal supersetsqequal intersectionsq unionsq circleplus circleminus circlemultiply circledivide
circledot turnstileleft tackleft latticetop perpendicular righttriangle logicalanddisplay logicalordisplay intersectiondisplay uniondisplay diamondmath dotmath star curlyor curlyand lessequalorgreater
greaterequalorless ellipsisvertical house control projective ceilingleft ceilingright floorleft floorright logicalnotreversed arc propellor integraltop integralbottom slurabove slurbelow
option deleteright clear angleleft angleright deleteleft blank onecircle twocircle threecircle fourcircle fivecircle sixcircle sevencircle eightcircle ninecircle
tencircle elevencircle twelvecircle thirteencircle fourteencircle fifteencircle sixteencircle seventeencircle eighteencircle nineteencircle twentycircle oneparen twoparen threeparen fourparen fiveparen
sixparen sevenparen eightparen nineparen tenparen elevenparen twelveparen thirteenparen fourteenparen fifteenparen sixteenparen seventeenparen eighteenparen nineteenparen twentyparen oneperiod
twoperiod threeperiod fourperiod fiveperiod sixperiod sevenperiod eightperiod nineperiod tenperiod elevenperiod twelveperiod thirteenperiod fourteenperiod fifteenperiod sixteenperiod seventeenperiod
eighteenperiod nineteenperiod twentyperiod aparen bparen cparen dparen eparen fparen gparen hparen iparen jparen kparen lparen mparen
nparen oparen pparen qparen rparen sparen tparen uparen vparen wparen xparen yparen zparen Acircle Bcircle Ccircle
Dcircle Ecircle Fcircle Gcircle Hcircle Icircle Jcircle Kcircle Lcircle Mcircle Ncircle Ocircle Pcircle Qcircle Rcircle Scircle
Tcircle Ucircle Vcircle Wcircle Xcircle Ycircle Zcircle acircle bcircle ccircle dcircle ecircle fcircle gcircle hcircle icircle
jcircle kcircle lcircle mcircle ncircle ocircle pcircle qcircle rcircle scircle tcircle ucircle vcircle wcircle xcircle ycircle
zcircle SF100000 SF110000 SF010000 SF030000 SF020000 SF040000 SF080000 SF090000 SF060000 SF070000 SF050000 SF430000 SF240000 SF510000 SF520000
SF390000 SF220000 SF210000 SF250000 SF500000 SF490000 SF380000 SF280000 SF270000 SF260000 SF360000 SF370000 SF420000 SF190000 SF200000 SF230000
SF470000 SF480000 SF410000 SF450000 SF460000 SF400000 SF540000 SF530000 SF440000 upblock dnblock block lfblock rtblock ltshade shade
dkshade blacksquare H22073 squarewhitewithsmallblack squarehorizontalfill squareverticalfill squareorthogonalcrosshatchfill squareupperlefttolowerrightfill squareupperrighttolowerleftfill squarediagonalcrosshatchfill blacksmallsquare H18551 blackrectangle blackuppointingtriangle triangle blackuppointingsmalltriangle
whiteuppointingsmalltriangle blackrightpointingtriangle whiterightpointingtriangle triangleleft blackrightpointingpointer blackdownpointingtriangle triangleinv whitedownpointingsmalltriangle blackleftpointingtriangle whiteleftpointingtriangle triangleright blackleftpointingpointer blackdiamond whitediamond whitediamondcontainingblacksmalldiamond fisheye
lozenge circle dottedcircle bullseye blackcircle circlewithlefthalfblack circlewithrighthalfblack bulletinverse invcircle blacklowerrighttriangle blacklowerlefttriangle blackupperlefttriangle blackupperrighttriangle openbullet largecircle blackstar
whitestar telephoneblack whitetelephone pointingindexleftwhite pointingindexupwhite pointingindexrightwhite pointingindexdownwhite yinyang smileface blacksmilingface compass female earth male spade heart
diamond club spadesuitwhite heart2 diamond2 clubsuitwhite hotsprings quarternote musicalnote eighthnotebeamed beamedsixteenthnotes flat natural musicsharpsign checkmark onecircleinversesansserif
twocircleinversesansserif threecircleinversesansserif fourcircleinversesansserif fivecircleinversesansserif sixcircleinversesansserif sevencircleinversesansserif eightcircleinversesansserif ninecircleinversesansserif arrowrightheavy dblbracketleft dblbracketright angbracketleft angbracketright coproduct ideographicspace ideographiccomma
ideographicperiod dittomark jis ideographiciterationmark ideographicclose ideographiczero anglebracketleft anglebracketright dblanglebracketleft dblanglebracketright cornerbracketleft cornerbracketright whitecornerbracketleft whitecornerbracketright blacklenticularbracketleft blacklenticularbracketright
postalmark getamark tortoiseshellbracketleft tortoiseshellbracketright whitelenticularbracketleft whitelenticularbracketright whitetortoiseshellbracketleft whitetortoiseshellbracketright wavedash quotedblprimereversed quotedblprime postalmarkface onehangzhou twohangzhou threehangzhou fourhangzhou
fivehangzhou sixhangzhou sevenhangzhou eighthangzhou ninehangzhou circlepostalmark asmallhiragana ahiragana ismallhiragana ihiragana usmallhiragana uhiragana esmallhiragana ehiragana osmallhiragana ohiragana
kahiragana gahiragana kihiragana gihiragana kuhiragana guhiragana kehiragana gehiragana kohiragana gohiragana sahiragana zahiragana sihiragana zihiragana suhiragana zuhiragana
sehiragana zehiragana sohiragana zohiragana tahiragana dahiragana tihiragana dihiragana tusmallhiragana tuhiragana duhiragana tehiragana dehiragana tohiragana dohiragana nahiragana
nihiragana nuhiragana nehiragana nohiragana hahiragana bahiragana pahiragana hihiragana bihiragana pihiragana huhiragana buhiragana puhiragana hehiragana behiragana pehiragana
hohiragana bohiragana pohiragana mahiragana mihiragana muhiragana mehiragana mohiragana yasmallhiragana yahiragana yusmallhiragana yuhiragana yosmallhiragana yohiragana rahiragana rihiragana
ruhiragana rehiragana rohiragana wasmallhiragana wahiragana wihiragana wehiragana wohiragana nhiragana vuhiragana voicedmarkkana semivoicedmarkkana iterationhiragana voicediterationhiragana asmallkatakana akatakana
ismallkatakana ikatakana usmallkatakana ukatakana esmallkatakana ekatakana osmallkatakana okatakana kakatakana gakatakana kikatakana gikatakana kukatakana gukatakana kekatakana gekatakana
kokatakana gokatakana sakatakana zakatakana sikatakana zikatakana sukatakana zukatakana sekatakana zekatakana sokatakana zokatakana takatakana dakatakana tikatakana dikatakana
tusmallkatakana tukatakana dukatakana tekatakana dekatakana tokatakana dokatakana nakatakana nikatakana nukatakana nekatakana nokatakana hakatakana bakatakana pakatakana hikatakana
bikatakana pikatakana hukatakana bukatakana pukatakana hekatakana bekatakana pekatakana hokatakana bokatakana pokatakana makatakana mikatakana mukatakana mekatakana mokatakana
yasmallkatakana yakatakana yusmallkatakana yukatakana yosmallkatakana yokatakana rakatakana rikatakana rukatakana rekatakana rokatakana wasmallkatakana wakatakana wikatakana wekatakana wokatakana
nkatakana vukatakana kasmallkatakana kesmallkatakana vakatakana vikatakana vekatakana vokatakana dotkatakana prolongedkana iterationkatakana voicediterationkatakana bbopomofo pbopomofo mbopomofo fbopomofo
dbopomofo tbopomofo nbopomofo lbopomofo gbopomofo kbopomofo hbopomofo jbopomofo qbopomofo xbopomofo zhbopomofo chbopomofo shbopomofo rbopomofo zbopomofo cbopomofo
sbopomofo abopomofo obopomofo ebopomofo ehbopomofo aibopomofo eibopomofo aubopomofo oubopomofo anbopomofo enbopomofo angbopomofo engbopomofo erbopomofo ibopomofo ubopomofo
iubopomofo kiyeokkorean ssangkiyeokkorean kiyeoksioskorean nieunkorean nieuncieuckorean nieunhieuhkorean tikeutkorean ssangtikeutkorean rieulkorean rieulkiyeokkorean rieulmieumkorean rieulpieupkorean rieulsioskorean rieulthieuthkorean rieulphieuphkorean
rieulhieuhkorean mieumkorean pieupkorean ssangpieupkorean pieupsioskorean sioskorean ssangsioskorean ieungkorean cieuckorean ssangcieuckorean chieuchkorean khieukhkorean thieuthkorean phieuphkorean hieuhkorean akorean
aekorean yakorean yaekorean eokorean ekorean yeokorean yekorean okorean wakorean waekorean oekorean yokorean ukorean weokorean wekorean wikorean
yukorean eukorean yikorean ikorean hangulfiller ssangnieunkorean nieuntikeutkorean nieunsioskorean nieunpansioskorean rieulkiyeoksioskorean rieultikeutkorean rieulpieupsioskorean rieulpansioskorean rieulyeorinhieuhkorean mieumpieupkorean mieumsioskorean
mieumpansioskorean kapyeounmieumkorean pieupkiyeokkorean pieuptikeutkorean pieupsioskiyeokkorean pieupsiostikeutkorean pieupcieuckorean pieupthieuthkorean kapyeounpieupkorean kapyeounssangpieupkorean sioskiyeokkorean siosnieunkorean siostikeutkorean siospieupkorean sioscieuckorean pansioskorean
ssangieungkorean yesieungkorean yesieungsioskorean yesieungpansioskorean kapyeounphieuphkorean ssanghieuhkorean yeorinhieuhkorean yoyakorean yoyaekorean yoikorean yuyeokorean yuyekorean yuikorean araeakorean araeaekorean kiyeokparenkorean
nieunparenkorean tikeutparenkorean rieulparenkorean mieumparenkorean pieupparenkorean siosparenkorean ieungparenkorean cieucparenkorean chieuchparenkorean khieukhparenkorean thieuthparenkorean phieuphparenkorean hieuhparenkorean kiyeokaparenkorean nieunaparenkorean tikeutaparenkorean
rieulaparenkorean mieumaparenkorean pieupaparenkorean siosaparenkorean ieungaparenkorean cieucaparenkorean chieuchaparenkorean khieukhaparenkorean thieuthaparenkorean phieuphaparenkorean hieuhaparenkorean cieucuparenkorean oneideographicparen twoideographicparen threeideographicparen fourideographicparen
fiveideographicparen sixideographicparen sevenideographicparen eightideographicparen nineideographicparen tenideographicparen ideographicmoonparen ideographicfireparen ideographicwaterparen ideographicwoodparen ideographicmetalparen ideographicearthparen ideographicsunparen ideographicstockparen ideographichaveparen ideographicsocietyparen
ideographicnameparen ideographicspecialparen ideographicfinancialparen ideographiccongratulationparen ideographiclaborparen ideographicrepresentparen ideographiccallparen ideographicstudyparen ideographicsuperviseparen ideographicenterpriseparen ideographicresourceparen ideographicallianceparen ideographicfestivalparen ideographicselfparen ideographicreachparen kiyeokcirclekorean
nieuncirclekorean tikeutcirclekorean rieulcirclekorean mieumcirclekorean pieupcirclekorean sioscirclekorean ieungcirclekorean cieuccirclekorean chieuchcirclekorean khieukhcirclekorean thieuthcirclekorean phieuphcirclekorean hieuhcirclekorean kiyeokacirclekorean nieunacirclekorean tikeutacirclekorean
rieulacirclekorean mieumacirclekorean pieupacirclekorean siosacirclekorean ieungacirclekorean cieucacirclekorean chieuchacirclekorean khieukhacirclekorean thieuthacirclekorean phieuphacirclekorean hieuhacirclekorean koreanstandardsymbol ideographmooncircle ideographfirecircle ideographwatercircle ideographwoodcircle
ideographmetalcircle ideographearthcircle ideographsuncircle ideographnamecircle ideographicfinancialcircle ideographiclaborcircle ideographicsecretcircle ideographicexcellentcircle ideographicprintcircle ideographiccorrectcircle ideographichighcircle ideographiccentrecircle ideographiclowcircle ideographicleftcircle ideographicrightcircle ideographicmedicinecircle
apaatosquare aarusquare intisquare karoriisquare kirosquare kiroguramusquare kiromeetorusquare guramusquare kooposquare sentisquare sentosquare dorusquare tonsquare haitusquare paasentosquare birusquare
huiitosquare hekutaarusquare herutusquare peezisquare hoonsquare mansyonsquare mirisquare miribaarusquare meetorusquare yaadosquare rittorusquare wattosquare heiseierasquare syouwaerasquare taisyouerasquare meizierasquare
corporationsquare paampssquare nasquare muasquare masquare kasquare KBsquare MBsquare GBsquare calsquare kcalsquare pfsquare nfsquare mufsquare mugsquare squaremg
squarekg Hzsquare khzsquare mhzsquare ghzsquare thzsquare mulsquare mlsquare dlsquare klsquare fmsquare nmsquare mumsquare squaremm squarecm squarekm
mmsquaredsquare cmsquaredsquare squaremsquared kmsquaredsquare mmcubedsquare cmcubedsquare mcubedsquare kmcubedsquare moverssquare moverssquaredsquare pasquare kpasquare mpasquare gpasquare radsquare radoverssquare
radoverssquaredsquare pssquare nssquare mussquare mssquare pvsquare nvsquare muvsquare mvsquare kvsquare mvmegasquare pwsquare nwsquare muwsquare mwsquare kwsquare
mwmegasquare kohmsquare mohmsquare amsquare bqsquare squarecc cdsquare coverkgsquare cosquare dbsquare gysquare hasquare HPsquare KKsquare squarekmcapital ktsquare
lmsquare squareln squarelog lxsquare mbsquare squaremil molsquare pmsquare srsquare svsquare wbsquare twentyhangzhou emptyslot altselector Germandblssmall FFsmall
FIsmall FLsmall FFIsmall FFLsmall capitalcompwordmark ascendercompwordmark interrobangdown dotlessj2 LL ll commaaccent afii10063 afii10064 afii10192 afii10831 afii10832
Acute Caron Dieresis DieresisAcute DieresisGrave Grave Hungarumlaut Macron cyrBreve cyrFlex dblGrave cyrbreve cyrflex dblgrave dieresisacute dieresisgrave
copyrightserif registerserif trademarkserif onefitted rupiah threequartersemdash centinferior centsuperior commainferior commasuperior dollarinferior dollarsuperior hypheninferior hyphensuperior periodinferior periodsuperior
asuperior bsuperior dsuperior esuperior isuperior lsuperior msuperior osuperior rsuperior ssuperior tsuperior Brevesmall Caronsmall Circumflexsmall Dotaccentsmall Hungarumlautsmall
Lslashsmall OEsmall Ogoneksmall Ringsmall Scaronsmall Tildesmall Zcaronsmall exclamsmall dollaroldstyle ampersandsmall zerooldstyle oneoldstyle twooldstyle threeoldstyle fouroldstyle fiveoldstyle
sixoldstyle sevenoldstyle eightoldstyle nineoldstyle questionsmall Gravesmall Asmall Bsmall Csmall Dsmall Esmall Fsmall Gsmall Hsmall Ismall Jsmall
Ksmall Lsmall Msmall Nsmall Osmall Psmall Qsmall Rsmall Ssmall Tsmall Usmall Vsmall Wsmall Xsmall Ysmall Zsmall
exclamdownsmall centoldstyle Dieresissmall Macronsmall Acutesmall Cedillasmall questiondownsmall Agravesmall Aacutesmall Acircumflexsmall Atildesmall Adieresissmall Aringsmall AEsmall Ccedillasmall Egravesmall
Eacutesmall Ecircumflexsmall Edieresissmall Igravesmall Iacutesmall Icircumflexsmall Idieresissmall Ethsmall Ntildesmall Ogravesmall Oacutesmall Ocircumflexsmall Otildesmall Odieresissmall Oslashsmall Ugravesmall
Uacutesmall Ucircumflexsmall Udieresissmall Yacutesmall Thornsmall Ydieresissmall maihanakatleftthai saraileftthai saraiileftthai saraueleftthai saraueeleftthai maitaikhuleftthai maiekupperleftthai maieklowrightthai maieklowleftthai maithoupperleftthai
maitholowrightthai maitholowleftthai maitriupperleftthai maitrilowrightthai maitrilowleftthai maichattawaupperleftthai maichattawalowrightthai maichattawalowleftthai thanthakhatupperleftthai thanthakhatlowrightthai thanthakhatlowleftthai nikhahitleftthai radicalex arrowvertex2 arrowhorizex registersans
copyrightsans trademarksans parenlefttp parenleftex2 parenleftbt bracketlefttp bracketleftex bracketleftbt bracelefttp braceleftmid2 braceleftbt braceex2 integralex parenrighttp parenrightex2 parenrightbt
bracketrighttp bracketrightex bracketrightbt bracerighttp bracerightmid2 bracerightbt apple f_f f_i f_l f_f_i f_f_l afii57705 ayinaltonehebrew afii57694 afii57695
shindageshshindot shindageshsindot alefpatahhebrew alefqamatshebrew alefdageshhebrew betdagesh gimeldagesh daletdagesh hedagesh afii57723 zayindagesh tetdagesh yoddagesh finalkafdagesh kafdagesh lameddagesh
memdagesh nundagesh samekhdagesh pefinaldageshhebrew pedagesh tsadidagesh qofdagesh reshdageshhebrew shindagesh tavdages afii57700 betrafehebrew kafrafehebrew perafehebrew aleflamedhebrew pehfinalarabic
pehinitialarabic pehmedialarabic ttehfinalarabic ttehinitialarabic ttehmedialarabic vehfinalarabic vehinitialarabic vehmedialarabic tchehfinalarabic tchehinitialarabic tchehmedialarabic ddalfinalarabic jehfinalarabic rrehfinalarabic gaffinalarabic gafinitialarabic
gafmedialarabic noonghunnafinalarabic hehhamzaaboveisolatedarabic hehhamzaabovefinalarabic hehfinalaltonearabic hehinitialaltonearabic hehmedialaltonearabic yehbarreefinalarabic behmeemisolatedarabic tehjeemisolatedarabic tehhahisolatedarabic tehmeemisolatedarabic meemmeemisolatedarabic noonjeemisolatedarabic noonmeemisolatedarabic yehmeemisolatedarabic
shaddadammatanarabic shaddakasratanarabic shaddafathaarabic shaddadammaarabic shaddakasraarabic behnoonfinalarabic tehnoonfinalarabic noonnoonfinalarabic yehnoonfinalarabic behmeeminitialarabic tehjeeminitialarabic tehhahinitialarabic tehmeeminitialarabic lamjeeminitialarabic lamhahinitialarabic lamkhahinitialarabic
lammeeminitialarabic meemmeeminitialarabic noonjeeminitialarabic noonmeeminitialarabic yehmeeminitialarabic parenleftaltonearabic parenrightaltonearabic lammeemhahinitialarabic lamlamhehisolatedarabic sallallahoualayhewasallamarabic twodotleadervertical emdashvertical endashvertical underscorevertical wavyunderscorevertical parenleftvertical
parenrightvertical braceleftvertical bracerightvertical tortoiseshellbracketleftvertical tortoiseshellbracketrightvertical blacklenticularbracketleftvertical blacklenticularbracketrightvertical dblanglebracketleftvertical dblanglebracketrightvertical anglebracketleftvertical anglebracketrightvertical cornerbracketleftvertical cornerbracketrightvertical whitecornerbracketleftvertical whitecornerbracketrightvertical overlinedashed
overlinecenterline overlinewavy overlinedblwavy lowlinedashed lowlinecenterline underscorewavy commasmall periodsmall semicolonsmall colonsmall parenleftsmall parenrightsmall braceleftsmall bracerightsmall tortoiseshellbracketleftsmall tortoiseshellbracketrightsmall
numbersignsmall asterisksmall plussmall hyphensmall lesssmall greatersmall equalsmall dollarsmall percentsmall atsmall alefmaddaabovefinalarabic alefhamzaabovefinalarabic wawhamzaabovefinalarabic alefhamzabelowfinalarabic yehhamzaabovefinalarabic yehhamzaaboveinitialarabic
yehhamzaabovemedialarabic aleffinalarabic behfinalarabic behinitialarabic behmedialarabic tehmarbutafinalarabic tehfinalarabic tehinitialarabic tehmedialarabic thehfinalarabic thehinitialarabic thehmedialarabic jeemfinalarabic jeeminitialarabic jeemmedialarabic hahfinalarabic
hahinitialarabic hahmedialarabic khahfinalarabic khahinitialarabic khahmedialarabic dalfinalarabic thalfinalarabic rehfinalarabic zainfinalarabic seenfinalarabic seeninitialarabic seenmedialarabic sheenfinalarabic sheeninitialarabic sheenmedialarabic sadfinalarabic
sadinitialarabic sadmedialarabic dadfinalarabic dadinitialarabic dadmedialarabic tahfinalarabic tahinitialarabic tahmedialarabic zahfinalarabic zahinitialarabic zahmedialarabic ainfinalarabic aininitialarabic ainmedialarabic ghainfinalarabic ghaininitialarabic
ghainmedialarabic fehfinalarabic fehinitialarabic fehmedialarabic qaffinalarabic qafinitialarabic qafmedialarabic kaffinalarabic kafinitialarabic kafmedialarabic lamfinalarabic laminitialarabic lammedialarabic meemfinalarabic meeminitialarabic meemmedialarabic
noonfinalarabic noonhehinitialarabic noonmedialarabic hehfinalalttwoarabic hehinitialarabic hehmedialarabic wawfinalarabic alefmaksurafinalarabic yehfinalarabic alefmaksurainitialarabic alefmaksuramedialarabic lamalefmaddaaboveisolatedarabic lamalefmaddaabovefinalarabic lamalefhamzaaboveisolatedarabic lamalefhamzaabovefinalarabic lamalefhamzabelowisolatedarabic
lamalefhamzabelowfinalarabic lamalefisolatedarabic lamaleffinalarabic zerowidthjoiner exclammonospace quotedblmonospace numbersignmonospace dollarmonospace percentmonospace ampersandmonospace quotesinglemonospace parenleftmonospace parenrightmonospace asteriskmonospace plusmonospace commamonospace
hyphenmonospace periodmonospace slashmonospace zeromonospace onemonospace twomonospace threemonospace fourmonospace fivemonospace sixmonospace sevenmonospace eightmonospace ninemonospace colonmonospace semicolonmonospace lessmonospace
equalmonospace greatermonospace questionmonospace atmonospace Amonospace Bmonospace Cmonospace Dmonospace Emonospace Fmonospace Gmonospace Hmonospace Imonospace Jmonospace Kmonospace Lmonospace
Mmonospace Nmonospace Omonospace Pmonospace Qmonospace Rmonospace Smonospace Tmonospace Umonospace Vmonospace Wmonospace Xmonospace Ymonospace Zmonospace bracketleftmonospace backslashmonospace
bracketrightmonospace asciicircummonospace underscoremonospace gravemonospace amonospace bmonospace cmonospace dmonospace emonospace fmonospace gmonospace hmonospace imonospace jmonospace kmonospace lmonospace
mmonospace nmonospace omonospace pmonospace qmonospace rmonospace smonospace tmonospace umonospace vmonospace wmonospace xmonospace ymonospace zmonospace braceleftmonospace barmonospace
bracerightmonospace asciitildemonospace periodhalfwidth cornerbracketlefthalfwidth cornerbracketrighthalfwidth ideographiccommaleft middledotkatakanahalfwidth wokatakanahalfwidth asmallkatakanahalfwidth ismallkatakanahalfwidth usmallkatakanahalfwidth esmallkatakanahalfwidth osmallkatakanahalfwidth yasmallkatakanahalfwidth yusmallkatakanahalfwidth yosmallkatakanahalfwidth
tusmallkatakanahalfwidth katahiraprolongmarkhalfwidth akatakanahalfwidth ikatakanahalfwidth ukatakanahalfwidth ekatakanahalfwidth okatakanahalfwidth kakatakanahalfwidth kikatakanahalfwidth kukatakanahalfwidth kekatakanahalfwidth kokatakanahalfwidth sakatakanahalfwidth sikatakanahalfwidth sukatakanahalfwidth sekatakanahalfwidth
sokatakanahalfwidth takatakanahalfwidth tikatakanahalfwidth tukatakanahalfwidth tekatakanahalfwidth tokatakanahalfwidth nakatakanahalfwidth nikatakanahalfwidth nukatakanahalfwidth nekatakanahalfwidth nokatakanahalfwidth hakatakanahalfwidth hikatakanahalfwidth hukatakanahalfwidth hekatakanahalfwidth hokatakanahalfwidth
makatakanahalfwidth mikatakanahalfwidth mukatakanahalfwidth mekatakanahalfwidth mokatakanahalfwidth yakatakanahalfwidth yukatakanahalfwidth yokatakanahalfwidth rakatakanahalfwidth rikatakanahalfwidth rukatakanahalfwidth rekatakanahalfwidth rokatakanahalfwidth wakatakanahalfwidth nkatakanahalfwidth voicedmarkkanahalfwidth
semivoicedmarkkanahalfwidth centmonospace sterlingmonospace macronmonospace yenmonospace wonmonospace";
    struct WorkLimitOverride(Option<u64>);
    impl WorkLimitOverride {
        fn new(limit: u64) -> Self {
            Self(TEST_WORK_LIMIT.with(|value| value.replace(Some(limit))))
        }
    }
    impl Drop for WorkLimitOverride {
        fn drop(&mut self) {
            TEST_WORK_LIMIT.with(|value| value.set(self.0));
        }
    }
    struct DeadlineOverride(Option<Instant>);
    impl DeadlineOverride {
        fn expired() -> Self {
            Self(crate::documents::TEST_DEADLINE.with(|value| {
                value.replace(Some(Instant::now() - std::time::Duration::from_secs(1)))
            }))
        }
    }
    impl Drop for DeadlineOverride {
        fn drop(&mut self) {
            crate::documents::TEST_DEADLINE.with(|value| value.set(self.0));
        }
    }
    struct PageDeadlineOverride(Option<Instant>);
    impl PageDeadlineOverride {
        fn expired() -> Self {
            Self(TEST_PAGE_DEADLINE.with(|value| {
                value.replace(Some(Instant::now() - std::time::Duration::from_secs(1)))
            }))
        }
    }
    impl Drop for PageDeadlineOverride {
        fn drop(&mut self) {
            TEST_PAGE_DEADLINE.with(|value| value.set(self.0));
        }
    }
    fn reference_chain(doc: &mut Document, end: Object, hops: usize) -> Object {
        let mut object = end;
        for _ in 0..hops {
            object = doc.add_object(object).into();
        }
        object
    }
    #[test]
    fn encoding_hop_cap_precedes_map_scanning_and_preserves_other_fonts() {
        for (hops, identity) in [(129, true), (8, true), (2, false), (9, false)] {
            let mut doc = map_fixture(
                vec![cmap_bytes(if identity {
                    "1 beginbfrange\n<000000> <FFFFFF> <0041>\nendbfrange"
                } else {
                    "1 beginbfchar\n<0041> <0042>\nendbfchar"
                })],
                &[0],
                1,
            );
            let id = resource_font(&doc, b"F0");
            let name = if identity {
                b"Identity-H".as_slice()
            } else {
                b"WinAnsiEncoding"
            };
            let encoding = reference_chain(&mut doc, Object::Name(name.to_vec()), hops);
            doc.get_object_mut(id)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("Encoding", encoding);
            set_page_contents(
                &mut doc,
                b"BT /H 12 Tf (Helvetica text remains readable: ) Tj /F0 12 Tf (ab) Tj ET".to_vec(),
            );
            let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
            let text = pdf.page_text(1).unwrap();
            let counters = PAGE_COUNTERS.with(|c| c.get());
            assert!(!text.content_too_large);
            assert!(text.text.starts_with("Helvetica text remains readable: "));
            assert_eq!(counters.decode_attempts, usize::from(hops == 8));
            assert!(FONT_MAPS.with(|m| m.borrow().is_empty()));
            assert_eq!(
                FONT_RESOLUTIONS.with(|c| c.get()),
                if hops == 2 { 2 } else { 1 }
            );
            if hops == 2 {
                assert!(text.text.ends_with("ab"));
                assert_eq!(text.undecodable, 0);
            } else {
                assert_eq!(text.undecodable, 2);
            }
            let (read, inspect) = document_tools(doc);
            assert!(
                read["text"]
                    .as_str()
                    .unwrap()
                    .contains("Helvetica text remains readable")
            );
            assert_eq!(inspect["complete"], true);
            assert_eq!(inspect["contentTooLarge"], "");
            eprintln!(
                "encoding hops={hops}, decode_attempts={}, work={}",
                counters.decode_attempts, counters.work
            );
        }
    }
    #[test]
    fn differences_are_charged_only_after_the_encoding_hop_cap() {
        for hops in [9, 1] {
            let mut doc = map_fixture(vec![], &[], 1);
            let id = resource_font(&doc, b"H");
            let differences = std::iter::once(Object::Integer(0))
                .chain(std::iter::repeat_n(Object::Name(b"a".to_vec()), 99_999))
                .collect::<Vec<_>>();
            let encoding = reference_chain(&mut doc, dictionary! {"Type"=>"Encoding", "BaseEncoding"=>"WinAnsiEncoding", "Differences"=>differences}.into(), hops);
            doc.get_object_mut(id)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("Encoding", encoding);
            set_page_contents(&mut doc, b"BT /H 12 Tf (ab) Tj ET".to_vec());
            let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
            let text = pdf.page_text(1).unwrap();
            assert!(!text.content_too_large);
            let counters = PAGE_COUNTERS.with(|c| c.get());
            assert_eq!(counters.differences, if hops == 1 { 100_000 } else { 0 });
            assert_eq!(FONT_RESOLUTIONS.with(|c| c.get()), usize::from(hops == 1));
            assert_eq!(text.undecodable, if hops == 1 { 0 } else { 2 });
            eprintln!(
                "Differences hops={hops}, elements_charged={}, work={}",
                counters.differences, counters.work
            );
        }
    }
    #[test]
    fn font_cap_preserves_existing_handles_aliases_and_resets_each_page() {
        for differences in [false, true] {
            let mut doc = map_fixture(vec![], &[], 2);
            let encoding = if differences {
                let names = std::iter::once(Object::Integer(0))
                    .chain(
                        DIFFERENCES_GLYPHS
                            .split_ascii_whitespace()
                            .map(|name| Object::Name(name.as_bytes().to_vec())),
                    )
                    .collect::<Vec<_>>();
                doc.add_object(dictionary! {"Type"=>"Encoding", "BaseEncoding"=>"WinAnsiEncoding", "Differences"=>names}).into()
            } else {
                Object::Name(b"WinAnsiEncoding".to_vec())
            };
            let mut fonts = Dictionary::new();
            let mut content = Vec::new();
            let mut first = (0, 0);
            for i in 1..=PDF_PAGE_FONT_LIMIT + 8 {
                let id = doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica","Encoding"=>encoding.clone()});
                if i == 1 {
                    first = id;
                }
                fonts.set(format!("F{i}"), id);
                content.extend_from_slice(format!("BT /F{i} 1 Tf (ab) Tj ET ").as_bytes());
            }
            fonts.set("G1", first);
            content.extend_from_slice(b"BT /F1 1 Tf (ab) Tj /G1 1 Tf (ab) Tj ET");
            let resources = inherited_resources(&doc);
            doc.get_object_mut(resources)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("Font", fonts);
            set_page_contents(&mut doc, content);
            let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
            for page in 1..=2 {
                let text = pdf.page_text(page).unwrap();
                assert!(!text.content_too_large);
                assert_eq!(text.undecodable, 16);
                let decoded = if differences {
                    "\u{ff70}\u{ff71}"
                } else {
                    "ab"
                };
                assert_eq!(
                    text.text,
                    decoded.repeat(PDF_PAGE_FONT_LIMIT)
                        + &"\u{fffd}".repeat(16)
                        + &decoded.repeat(2)
                );
                let counters = PAGE_COUNTERS.with(|c| c.get());
                assert_eq!(counters.fonts, PDF_PAGE_FONT_LIMIT);
                assert_eq!(FONT_RESOLUTIONS.with(|c| c.get()), PDF_PAGE_FONT_LIMIT);
                assert!(counters.work <= PDF_PAGE_WORK_LIMIT);
                assert_eq!(
                    counters.differences,
                    if differences { 3735 * 1024 } else { 0 }
                );
                eprintln!(
                    "font cap: page={page}, Differences={differences}, fonts={}, work={}",
                    counters.fonts, counters.work
                );
            }
        }
    }
    #[test]
    fn large_font_dictionary_and_repeated_forms_only_look_up_used_names() {
        let mut doc = map_fixture(vec![], &[], 1);
        let id = resource_font(&doc, b"H");
        let mut fonts = Dictionary::new();
        for i in 1..=10_000 {
            fonts.set(format!("F{i}"), id);
        }
        let form = doc.add_object(Stream::new(
            dictionary! {"Subtype"=>"Form"},
            b"BT /F1 1 Tf (x) Tj ET".to_vec(),
        ));
        let resources = inherited_resources(&doc);
        doc.get_object_mut(resources)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Font", fonts);
        doc.get_object_mut(resources)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("XObject", dictionary! {"Fm"=>form});
        let mut content = b"BT /F1 1 Tf (x) Tj ET ".to_vec();
        content.extend_from_slice(&b"/Fm Do\n".repeat(50_000));
        set_page_contents(&mut doc, content);
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        let text = pdf.page_text(1).unwrap();
        assert!(!text.content_too_large);
        assert_eq!(text.text, "x".repeat(50_001));
        let counters = PAGE_COUNTERS.with(|c| c.get());
        assert_eq!(counters.fonts, 1);
        assert_eq!(FONT_RESOLUTIONS.with(|c| c.get()), 1);
        // One inherited-resource hop, one font pair and one form pair (one hop each).
        assert_eq!(counters.lookups, 7);
        assert!(counters.work <= PDF_PAGE_WORK_LIMIT);
        eprintln!(
            "10,000 fonts x 50,000 Do: lookups={}, fonts={}, work={}",
            counters.lookups, counters.fonts, counters.work
        );
    }
    #[test]
    fn failed_tounicode_decode_exhausts_its_page_byte_budget_once() {
        let mut stream = compressed(vec![0; 8 * 1024 * 1024]);
        stream.dict.set(
            "Filter",
            vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"Bogus".to_vec()),
            ],
        );
        let mut doc = map_fixture(vec![vec![]], &vec![0; 300], 1);
        let font = doc.get_dictionary(resource_font(&doc, b"F0")).unwrap();
        let map = font.get(b"ToUnicode").unwrap().as_reference().unwrap();
        doc.objects.insert(map, stream.into());
        let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
        {
            let _limit = WorkLimitOverride::new(1 << 27);
            let text = pdf.page_text(1).unwrap();
            assert!(!text.content_too_large);
            assert!(text.text.starts_with("Helvetica text survives"));
            assert_eq!(text.undecodable, 600);
            let counters = PAGE_COUNTERS.with(|c| c.get());
            assert_eq!(counters.decode_attempts, 1);
            assert_eq!(counters.font_bytes_left, 0);
            assert!(counters.work >= PDF_STREAM_LIMIT as u64);
            assert!(counters.work < PDF_STREAM_LIMIT as u64 + 100_000);
            assert!(FONT_MAPS.with(|m| m.borrow().is_empty()));
            let (read, inspect) = document_tools(doc);
            assert!(
                read["text"]
                    .as_str()
                    .unwrap()
                    .contains("Helvetica text survives")
            );
            assert_eq!(inspect["contentTooLarge"], "");
            eprintln!(
                "failed decode at test-only 2^27: attempts=1, ToUnicode bytes left=0, work={}",
                counters.work
            );
        }
        let text = pdf.page_text(1).unwrap();
        assert!(text.content_too_large);
        assert!(text.text.is_empty());
        assert_eq!(PAGE_COUNTERS.with(|c| c.get().decode_attempts), 1);
        eprintln!("failed decode at production 2^26: content_too_large=true, decode_attempts=1");
    }
    #[test]
    fn work_budget_discards_partial_text_and_bounds_image_resource_traversal() {
        let mut doc = map_fixture(vec![], &[], 2);
        let shown = b"partial text must be discarded";
        let mut content = b"BT /H 1 Tf (".to_vec();
        content.extend_from_slice(shown);
        content.extend_from_slice(b") Tj ET ");
        content.extend_from_slice(&b"n ".repeat(200));
        let length = content.len() as u64 + 1; // lopdf appends newline.
        let id = doc.add_object(Stream::new(Dictionary::new(), content));
        let page = doc.get_pages()[&1];
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Contents", id);
        let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
        {
            let work_limit = length + shown.len() as u64 + 20;
            let _limit = WorkLimitOverride::new(work_limit);
            let (info, text) = pdf.page_info(1).unwrap();
            assert!(info.content_too_large);
            assert_eq!(info.kind, PageKind::NoText);
            assert!(text.text.is_empty());
            assert_eq!(
                text.undecodable + text.visible_chars + text.invisible_chars,
                0
            );
            let counters = PAGE_COUNTERS.with(|c| c.get());
            assert!(counters.work <= work_limit + 1);
            assert_eq!(counters.glyph_units, shown.len() as u64);
            assert_eq!(counters.stopped_by, Some(PageLimit::Work));
            assert!(!pdf.page_info(2).unwrap().0.content_too_large);
            let (read, inspect) = document_tools(doc);
            assert_eq!(read["units"][0]["contentTooLarge"], true);
            assert_eq!(inspect["contentTooLarge"], "1");
        }
        let mut doc = map_fixture(vec![], &[], 1);
        set_page_contents(&mut doc, Vec::new());
        let resources = inherited_resources(&doc);
        let image = doc.add_object(Stream::new(
            dictionary! {"Subtype"=>"Image", "Width"=>1,"Height"=>1},
            vec![0],
        ));
        let form = doc.add_object(Stream::new(
            dictionary! {"Subtype"=>"Form", "Resources"=>resources},
            Vec::new(),
        ));
        let mut xobjects = Dictionary::new();
        for i in 0..1000 {
            xobjects.set(format!("Im{i}"), image);
        }
        for i in 0..100 {
            xobjects.set(format!("Fm{i}"), form);
        }
        doc.get_object_mut(resources)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("XObject", xobjects);
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        let _limit = WorkLimitOverride::new(128);
        assert!(pdf.page_info(1).unwrap().0.content_too_large);
        assert_eq!(PAGE_COUNTERS.with(|c| c.get().work), 129);
        let err = pdf.page_images(1).err().unwrap();
        assert_eq!(err.code, "DOCUMENT_INVALID");
        assert_eq!(err.message, "page resources exceed the extraction limits");
        assert_eq!(PAGE_COUNTERS.with(|c| c.get().work), 129);
    }
    #[test]
    fn in_page_deadlines_discard_work_and_document_cursors_reassemble_exactly() {
        let mut doc = map_fixture(vec![], &[], 3);
        let mut heavy = b"n\n".repeat(1_000_000);
        heavy.extend_from_slice(b"BT /H 1 Tf (heavy second page) Tj ET");
        let length = heavy.len() as u64 + 1;
        let id = doc.add_object(compressed(heavy));
        let page = doc.get_pages()[&2];
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Contents", id);
        let data = bytes(doc);
        let pdf = PdfDocument::load(&data, &HashSet::new()).unwrap();
        let expired = Instant::now() - std::time::Duration::from_secs(1);
        assert!(pdf.page_info_within(2, Some(expired)).unwrap().is_none());
        assert!(PAGE_COUNTERS.with(|c| c.get().work) <= PDF_DEADLINE_CHECK_UNITS + length);
        let (info, text) = pdf.page_info(2).unwrap();
        assert!(!info.content_too_large);
        assert_eq!(text.text, "heavy second page");
        let (summary, next) = inspect(&data, 1, expired).unwrap();
        assert_eq!(summary["classified"], 1);
        assert_eq!(summary["contentTooLarge"], "");
        assert_eq!(next, Some(2));
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("deadline.pdf"), &data).unwrap();
        let root = crate::filesystem::RootDir::open(&crate::config::Root {
            id: "work".into(),
            path: temp.path().to_path_buf(),
            nickname: None,
        })
        .unwrap();
        let args = json!({"path":"deadline.pdf", "maxChars":200000});
        let unlimited = crate::documents::read_document(&root, "work", &args).unwrap();
        {
            // The outer call deadline is still in the future, so these calls enter
            // page 2. Only the in-page deadline is expired; its partial work vanishes.
            let _page_deadline = PageDeadlineOverride::expired();
            let (inside, next) = inspect(
                &data,
                1,
                Instant::now() + std::time::Duration::from_secs(60),
            )
            .unwrap();
            assert_eq!(inside["classified"], 1);
            assert_eq!(next, Some(2));
            assert!(PAGE_COUNTERS.with(|c| c.get().work) >= length);
            let chunk = crate::documents::read_document(&root, "work", &args).unwrap();
            assert_eq!(chunk["units"].as_array().unwrap().len(), 1);
            assert_eq!(chunk["units"][0]["page"], 1);
            assert!(chunk["nextCursor"].is_string());
            assert!(PAGE_COUNTERS.with(|c| c.get().work) >= length);
            let inspected =
                crate::documents::inspect_document(&root, "work", &json!({"path":"deadline.pdf"}))
                    .unwrap();
            assert_eq!(inspected["classified"], 1);
            assert!(inspected["nextCursor"].is_string());
            assert!(PAGE_COUNTERS.with(|c| c.get().work) >= length);
            let fetched =
                crate::documents::fetch(&root, "work", "deadline.pdf", "work:deadline.pdf")
                    .unwrap();
            assert_eq!(fetched["metadata"]["truncated"], true);
            assert!(fetched["metadata"]["nextCursor"].is_string());
            assert!(PAGE_COUNTERS.with(|c| c.get().work) >= length);
        }
        let _deadline = DeadlineOverride::expired();
        let mut parts = Vec::new();
        let mut pages = Vec::new();
        let mut request = args.clone();
        for expected_page in 1..=3 {
            let chunk = crate::documents::read_document(&root, "work", &request).unwrap();
            assert_eq!(chunk["units"].as_array().unwrap().len(), 1);
            pages.push(chunk["units"][0]["page"].as_u64().unwrap());
            parts.push(
                chunk["text"]
                    .as_str()
                    .unwrap()
                    .split_once('\n')
                    .unwrap()
                    .1
                    .to_owned(),
            );
            if expected_page < 3 {
                let token = chunk["nextCursor"].as_str().unwrap();
                let hash = crate::cursor::request_hash(&[
                    "read_document",
                    "work",
                    "deadline.pdf",
                    "",
                    "",
                    "",
                ]);
                let cursor =
                    crate::cursor::decode(token, crate::cursor::CursorKind::Doc, hash).unwrap();
                assert_eq!(cursor.u, expected_page);
                assert_eq!(cursor.o, 0);
                request["cursor"] = json!(token);
            } else {
                assert_eq!(chunk["truncated"], false);
            }
        }
        assert_eq!(pages, vec![1, 2, 3]);
        assert_eq!(
            parts.join("\n\n"),
            unlimited["text"]
                .as_str()
                .unwrap()
                .split_once('\n')
                .unwrap()
                .1
        );
        let inspected =
            crate::documents::inspect_document(&root, "work", &json!({"path":"deadline.pdf"}))
                .unwrap();
        assert_eq!(inspected["classified"], 1);
        assert!(inspected["nextCursor"].is_string());
        let fetched =
            crate::documents::fetch(&root, "work", "deadline.pdf", "work:deadline.pdf").unwrap();
        assert_eq!(fetched["metadata"]["truncated"], true);
        assert!(fetched["metadata"]["nextCursor"].is_string());
    }
    #[test]
    fn long_font_names_have_fixed_size_q_states_and_a_depth_limit() {
        for depth in [64, 65] {
            let mut doc = map_fixture(vec![], &[], 2);
            let font = resource_font(&doc, b"H");
            let resources = inherited_resources(&doc);
            let name = "N".repeat(64 * 1024);
            doc.get_object_mut(resources)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .get_mut(b"Font")
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set(name.clone(), font);
            let content = format!(
                "BT /{name} 1 Tf {}(long-name text survives normally) Tj {} ET",
                "q ".repeat(depth),
                "Q ".repeat(depth)
            );
            let id = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
            let page = doc.get_pages()[&1];
            doc.get_object_mut(page)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("Contents", id);
            let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
            let text = pdf.page_text(1).unwrap();
            assert_eq!(text.content_too_large, depth == 65);
            if depth == 64 {
                assert_eq!(text.text, "long-name text survives normally");
            } else {
                assert!(text.text.is_empty());
            }
            assert!(!pdf.page_text(2).unwrap().content_too_large);
            let (read, inspect) = document_tools(doc);
            if depth == 65 {
                assert!(
                    read["text"].as_str().unwrap().contains(
                        "text not extracted (page content exceeds the extraction limits)"
                    )
                );
                assert_eq!(read["units"][0]["contentTooLarge"], true);
                assert_eq!(inspect["contentTooLarge"], "1");
            }
        }
    }
    #[test]
    fn repeated_forms_resolve_a_shared_font_once_per_page() {
        let mut doc = map_fixture(
            vec![cmap_bytes(
                "1 beginbfrange\n<0000> <FFFF> <0000>\nendbfrange",
            )],
            &[0],
            1,
        );
        let font = resource_font(&doc, b"F0");
        let resources = inherited_resources(&doc);
        doc.get_object_mut(resources)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .get_mut(b"Font")
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .remove(b"H");
        let form = doc.add_object(Stream::new(
            dictionary! {"Subtype"=>"Form"},
            b"BT /F0 12 Tf <0041> Tj ET".to_vec(),
        ));
        let alias = doc.add_object(Stream::new(dictionary! {"Subtype"=>"Form","Resources"=>dictionary! {"Font"=>dictionary! {"Alias"=>font}}}, b"BT /Alias 12 Tf <0041> Tj ET".to_vec()));
        doc.get_object_mut(resources)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("XObject", dictionary! {"Form"=>form,"Alias"=>alias});
        let mut content = b"/Form Do\n".repeat(1000);
        content.extend_from_slice(b"/Alias Do");
        set_page_contents(&mut doc, content);
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        let text = pdf.page_text(1).unwrap();
        assert_eq!(text.text, "A".repeat(1001));
        assert_eq!(FONT_RESOLUTIONS.with(|count| count.get()), 1);
        assert_eq!(FONT_MAPS.with(|maps| maps.borrow().len()), 1);
        assert_eq!(FONT_MAPS.with(|maps| maps.borrow()[0].0), 65_536);
        eprintln!("repeated Do: forms=1001, font resolutions=1, ToUnicode maps=1");
    }
    #[test]
    fn direct_font_dictionaries_use_their_resource_scope_in_the_page_cache() {
        let mut doc = map_fixture(
            vec![
                cmap_bytes("1 beginbfchar\n<0041> <0041>\nendbfchar"),
                cmap_bytes("1 beginbfchar\n<0041> <0042>\nendbfchar"),
            ],
            &[0, 1],
            1,
        );
        let a = doc
            .get_dictionary(resource_font(&doc, b"F0"))
            .unwrap()
            .clone();
        let b = doc
            .get_dictionary(resource_font(&doc, b"F1"))
            .unwrap()
            .clone();
        let resources = inherited_resources(&doc);
        doc.get_object_mut(resources)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Font", dictionary! {"F0"=>a});
        let inherited = doc.add_object(Stream::new(
            dictionary! {"Subtype"=>"Form"},
            b"BT /F0 12 Tf <0041> Tj ET".to_vec(),
        ));
        let own = doc.add_object(Stream::new(dictionary! {"Subtype"=>"Form","Resources"=>dictionary! {"Font"=>dictionary! {"F0"=>b}}},b"BT /F0 12 Tf <0041> Tj ET".to_vec()));
        doc.get_object_mut(resources)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("XObject", dictionary! {"Inherited"=>inherited,"Own"=>own});
        let mut content = b"/Inherited Do\n".repeat(1000);
        content.extend_from_slice(&b"/Own Do\n".repeat(500));
        set_page_contents(&mut doc, content);
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        assert_eq!(
            pdf.page_text(1).unwrap().text,
            "A".repeat(1000) + &"B".repeat(500)
        );
        assert_eq!(FONT_RESOLUTIONS.with(|count| count.get()), 2);
        assert_eq!(FONT_MAPS.with(|maps| maps.borrow().len()), 2);
    }
    #[test]
    fn oversized_and_unparseable_cmaps_skip_only_their_font() {
        let cases = [
            "1 beginbfrange\n<000000> <FFFFFF> <0041>\nendbfrange",
            "1 beginbfrange\n<00000000> <FFFFFFFF> <0041>\nendbfrange",
            "1 beginbfrange\n<0000> <FFFF> <0000>\nendbfrange\n1 beginbfchar\n<0000> <0041>\nendbfchar",
            "1 beginbfrange\n<0002> <0001> <0041>\nendbfrange",
            "1 beginbfrange\n<00> <0001> <0041>\nendbfrange",
            "1 beginbfrange\n<0001> <0001> [<0041> <0042>]\nendbfrange",
            "1 beginbfrange\n<0001> <0002> <0041>",
        ];
        for sections in cases {
            let doc = map_fixture(vec![cmap_bytes(sections)], &[0], 1);
            let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
            let text = pdf.page_text(1).unwrap();
            assert!(text.text.contains("Helvetica text survives"));
            assert!(text.undecodable > 0);
            assert!(FONT_MAPS.with(|maps| maps.borrow().is_empty()));
            let (read, inspect) = document_tools(doc);
            assert!(
                read["text"]
                    .as_str()
                    .unwrap()
                    .contains("Helvetica text survives")
            );
            assert_eq!(inspect["complete"], true);
            assert_eq!(inspect["suspicious"]["undecodable"], "1");
        }
    }
    #[test]
    fn exactly_65536_cmap_codes_are_accepted_and_decoded() {
        let doc = map_fixture(
            vec![cmap_bytes(
                "1 beginbfrange\n<0000> <FFFF> <0000>\nendbfrange",
            )],
            &[0],
            1,
        );
        let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
        let text = pdf.page_text(1).unwrap();
        assert_eq!(text.undecodable, 0);
        assert!(text.text.ends_with('A'));
        assert_eq!(
            FONT_MAPS.with(|maps| maps.borrow().iter().map(|m| m.0).sum::<u64>()),
            65_536
        );
        let (read, inspect) = document_tools(doc);
        assert!(read["text"].as_str().unwrap().ends_with('A'));
        assert_eq!(inspect["suspicious"], json!({}));
    }
    #[test]
    fn font_map_code_budget_is_shared_with_forms_and_resets_each_page() {
        for form_scope in [false, true] {
            let mut doc = map_fixture(
                vec![cmap_bytes(
                    "1 beginbfrange\n<0000> <FFFF> <0000>\nendbfrange",
                )],
                &[0, 0, 0, 0, 0],
                2,
            );
            if form_scope {
                let f3 = resource_font(&doc, b"F3");
                let f4 = resource_font(&doc, b"F4");
                let resources = inherited_resources(&doc);
                let fonts = doc
                    .get_object_mut(resources)
                    .unwrap()
                    .as_dict_mut()
                    .unwrap()
                    .get_mut(b"Font")
                    .unwrap()
                    .as_dict_mut()
                    .unwrap();
                fonts.remove(b"F3");
                fonts.remove(b"F4");
                let form = doc.add_object(Stream::new(dictionary! {"Subtype"=>"Form","Resources"=>dictionary! {"Font"=>dictionary! {"F3"=>f3,"F4"=>f4}}}, b"BT /F3 12 Tf <0041> Tj /F4 12 Tf <0041> Tj ET".to_vec()));
                doc.get_object_mut(resources)
                    .unwrap()
                    .as_dict_mut()
                    .unwrap()
                    .set("XObject", dictionary! {"Last"=>form});
                set_page_contents(&mut doc,b"BT /H 12 Tf (Helvetica text survives font map guards: ) Tj /F0 12 Tf <0041> Tj /F1 12 Tf <0041> Tj /F2 12 Tf <0041> Tj ET /Last Do".to_vec());
            }
            let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
            for page in 1..=2 {
                let text = pdf.page_text(page).unwrap();
                assert!(text.text.ends_with("AAAA\u{fffd}\u{fffd}"));
                assert_eq!(text.undecodable, 2);
                let maps = FONT_MAPS.with(|maps| maps.borrow().clone());
                assert_eq!(maps.len(), 4);
                assert_eq!(
                    maps.iter().map(|m| m.0).sum::<u64>(),
                    TOUNICODE_PAGE_CODE_LIMIT
                );
                assert!(maps.iter().map(|m| m.1).sum::<usize>() <= PDF_STREAM_LIMIT);
                eprintln!(
                    "page font budget: page={page}, form_scope={form_scope}, accepted_maps={}, codes={}",
                    maps.len(),
                    maps.iter().map(|m| m.0).sum::<u64>()
                );
            }
            let (read, inspect) = document_tools(doc);
            assert_eq!(read["units"].as_array().unwrap().len(), 2);
            assert_eq!(inspect["classified"], 2);
        }
    }
    #[test]
    fn font_map_decoded_bytes_are_cumulative_per_page() {
        let maps = ["0041", "0042", "0043"]
            .iter()
            .map(|destination| {
                let mut map =
                    cmap_bytes(&format!("1 beginbfchar\n<0041> <{destination}>\nendbfchar"));
                map.resize(24 * 1024 * 1024, b' ');
                map
            })
            .collect();
        let doc = map_fixture(maps, &[0, 1, 2], 1);
        let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
        {
            let work_limit = 1 << 27;
            let _limit = WorkLimitOverride::new(work_limit);
            let text = pdf.page_text(1).unwrap();
            assert!(!text.content_too_large);
            assert!(PAGE_COUNTERS.with(|c| c.get().work) <= work_limit);
            assert!(text.text.ends_with("AB\u{fffd}\u{fffd}"));
            assert_eq!(text.undecodable, 2);
            let maps = FONT_MAPS.with(|maps| maps.borrow().clone());
            assert_eq!(maps.len(), 2);
            assert_eq!(maps.iter().map(|m| m.1).sum::<usize>(), 48 * 1024 * 1024);
            assert!(maps.iter().map(|m| m.1).sum::<usize>() <= PDF_STREAM_LIMIT);
            eprintln!(
                "page font byte budget at test-only 2^27: accepted_maps=2, codes=2, bytes={}",
                48 * 1024 * 1024
            );
            let (read, inspect) = document_tools(doc);
            assert!(
                read["text"]
                    .as_str()
                    .unwrap()
                    .contains("AB\u{fffd}\u{fffd}")
            );
            assert_eq!(inspect["complete"], true);
        }
        let text = pdf.page_text(1).unwrap();
        assert!(text.content_too_large);
        assert!(text.text.is_empty());
        eprintln!("page font byte budget at production 2^26: content_too_large=true");
    }
    #[test]
    fn font_map_guard_covers_lopdf_tounicode_encoding_paths() {
        for path in 0..4 {
            let mut doc = map_fixture(
                vec![cmap_bytes("1 beginbfchar\n<0041> <0041>\nendbfchar")],
                &[0],
                1,
            );
            let font = resource_font(&doc, b"F0");
            let encoding = match path {
                0 => None,
                1 => Some(Object::Name(b"Identity-V".to_vec())),
                2 => Some(doc.add_object(Object::Name(b"Identity-H".to_vec())).into()),
                _ => Some(dictionary! {"Type"=>"Encoding","BaseEncoding"=>"Identity-H","Differences"=>Vec::<Object>::new()}.into()),
            };
            let font = doc.get_object_mut(font).unwrap().as_dict_mut().unwrap();
            if let Some(encoding) = encoding {
                font.set("Encoding", encoding);
            } else {
                font.remove(b"Encoding");
            }
            let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
            let text = pdf.page_text(1).unwrap();
            assert!(text.text.contains("Helvetica text survives"));
            if path < 3 {
                assert_eq!(text.undecodable, 0);
            }
            assert_eq!(
                FONT_MAPS.with(|maps| maps.borrow().len()),
                1,
                "encoding path {path}"
            );
        }
    }
    #[test]
    fn unused_tounicode_streams_are_not_scanned_for_other_encodings() {
        for dictionary in [false, true] {
            let mut doc = map_fixture(
                vec![cmap_bytes(
                    "1 beginbfrange\n<00000000> <FFFFFFFF> <0041>\nendbfrange",
                )],
                &[0],
                1,
            );
            let font = resource_font(&doc, b"F0");
            let font = doc.get_object_mut(font).unwrap().as_dict_mut().unwrap();
            font.set("Encoding",if dictionary { dictionary! {"Type"=>"Encoding","BaseEncoding"=>"WinAnsiEncoding","Differences"=>Vec::<Object>::new()}.into() } else { Object::Name(b"WinAnsiEncoding".to_vec()) });
            set_page_contents(
                &mut doc,
                b"BT /F0 12 Tf (ASCII text with unused ToUnicode) Tj ET".to_vec(),
            );
            let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
            assert_eq!(
                pdf.page_text(1).unwrap().text,
                "ASCII text with unused ToUnicode"
            );
            assert!(FONT_MAPS.with(|maps| maps.borrow().is_empty()));
        }
    }
    #[test]
    fn cmap_count_scan_counts_entries_without_expanding_ranges() {
        assert_eq!(scan_cmap(b"% 1 beginbfrange <00> <FF> <0000>\n(foo beginbfchar) /beginbfrange 2 beginbfrange <00> <01> [<0041> <0042>] <02> <03> <0043> endbfrange 1 beginbfchar <04> <0045> endbfchar"),Some((5, 1)));
        assert_eq!(
            scan_cmap(b"1 beginbfrange <00000000> <FFFFFFFF> <0041> endbfrange"),
            Some((4_294_967_296, 1))
        );
        for bad in [
            b"1 beginbfrange <> <00> <0041> endbfrange".as_slice(),
            b"1 beginbfrange <0000000000> <0000000001> <0041> endbfrange",
            b"1 beginbfchar <00> <0041>",
            b"1 beginbfrange <00> <01> [<0041>",
        ] {
            assert_eq!(scan_cmap(bad), None);
        }
    }
    #[test]
    fn cmap_scan_records_the_longest_destination_in_utf16_units() {
        for (data, expected) in [
            (b"begincmap endcmap".as_slice(), (0, 1)),
            (b"1 beginbfchar <41> <006600660069> endbfchar", (1, 3)),
            (b"1 beginbfchar <41> <004100> endbfchar", (1, 2)),
            (
                b"1 beginbfrange <00> <02> [<0041> <006600660069> <0042>] endbfrange",
                (3, 3),
            ),
        ] {
            assert_eq!(scan_cmap(data), Some(expected));
        }
        let target = "0041".repeat(256);
        assert_eq!(
            scan_cmap(format!("1 beginbfrange <00> <FF> <{target}> endbfrange").as_bytes()),
            Some((256, 256))
        );
    }
    #[test]
    fn reads_native_cyrillic_ocr_forms_and_line_breaks() {
        let (doc, _) = fixture();
        let bytes = bytes(doc);
        let pdf = PdfDocument::load(&bytes, &HashSet::new()).unwrap();
        assert_eq!(pdf.page_count(), 7);
        assert_eq!(pdf.page_info(1).unwrap().0.kind, PageKind::Text);
        assert_eq!(
            pdf.page_text(1).unwrap().text,
            "Native Helvetica text on page one\nSecond line\nThird line"
        );
        let (info, text) = pdf.page_info(2).unwrap();
        assert_eq!(info.kind, PageKind::OcrText);
        assert!(info.scan_image);
        assert!(info.chars >= 40);
        assert_eq!(info.suspicious, Some("no_cyrillic_over_scan"));
        assert_eq!(text.visible_chars, 0);
        assert_eq!(pdf.page_info(3).unwrap().0.kind, PageKind::NoText);
        // The inherited image resources are deliberately still reachable on this page.
        assert_eq!(pdf.page_info(4).unwrap().0.kind, PageKind::NoText);
        assert_eq!(pdf.page_text(5).unwrap().text, "ПриПриПриПриПриПри");
        assert!(pdf.page_text(6).unwrap().text.contains("inside a form"));
        let text = pdf.page_text(7).unwrap();
        assert!(text.visible_chars > 0);
        assert!(text.invisible_chars > 15);
        assert_eq!(pdf.metadata()["title"], "Synthetic PDF");
        assert_eq!(pdf.page_id(0).unwrap_err().code, "PAGE_OUT_OF_RANGE");
        assert!(pdf.page_id(8).is_err());
    }
    #[test]
    fn placeholders_keep_image_metadata_without_retaining_image_content() {
        let (doc, image) = fixture();
        let bytes = bytes(doc);
        let filtered = PdfDocument::load(&bytes, &HashSet::new()).unwrap();
        let stream = filtered
            .document()
            .get_object(image)
            .unwrap()
            .as_stream()
            .unwrap();
        assert_eq!(stream.content, [0]);
        assert!(
            stream
                .dict
                .get(b"LocalFilesPlaceholder")
                .unwrap()
                .as_bool()
                .unwrap()
        );
        assert!(
            filtered
                .page_images(1)
                .unwrap()
                .iter()
                .any(|i| i.id == image && i.width == 1200)
        );
        let kept = PdfDocument::load(&bytes, &HashSet::from([image])).unwrap();
        assert_eq!(
            kept.into_document()
                .get_object(image)
                .unwrap()
                .as_stream()
                .unwrap()
                .content,
            [77; 64]
        );
        let again = PdfDocument::load(&bytes, &HashSet::new()).unwrap();
        assert_eq!(
            again
                .document()
                .get_object(image)
                .unwrap()
                .as_stream()
                .unwrap()
                .content
                .len(),
            1
        );
    }
    #[test]
    fn blank_pages_assessment_and_suspicious_reason_precedence() {
        let empty = PageText {
            text: String::new(),
            visible_chars: 0,
            invisible_chars: 0,
            undecodable: 0,
            content_too_large: false,
        };
        assert_eq!(assess(&empty, false, false), (PageKind::Empty, None));
        assert_eq!(assess(&empty, true, true), (PageKind::NoText, None));
        let bad = PageText {
            text: "À".repeat(40),
            visible_chars: 40,
            invisible_chars: 0,
            undecodable: 2,
            content_too_large: false,
        };
        assert_eq!(assess(&bad, true, true).1, Some("undecodable"));
        assert_eq!(
            assess(
                &PageText {
                    undecodable: 0,
                    ..bad
                },
                true,
                true
            )
            .1,
            Some("mojibake")
        );
        let (mut doc, _) = fixture();
        let pages = doc.get_pages();
        let parent = doc
            .get_dictionary(pages[&4])
            .unwrap()
            .get(b"Parent")
            .unwrap()
            .as_reference()
            .unwrap();
        let resources = doc
            .get_dictionary(parent)
            .unwrap()
            .get(b"Resources")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_object_mut(resources)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .remove(b"XObject");
        let pdf = PdfDocument::load(&bytes(doc), &HashSet::new()).unwrap();
        assert_eq!(pdf.page_info(4).unwrap().0.kind, PageKind::Empty);
    }
    #[test]
    fn page_ranges_selection_and_expired_inspection_make_progress() {
        assert_eq!(page_ranges([5, 3, 2, 1, 3, 7, 8]), "1-3,5,7-8");
        assert_eq!(page_ranges([]), "");
        let (doc, _) = fixture();
        let bytes = bytes(doc);
        let mut reader = PdfText::open(&bytes, &json!({"startPage":2,"endPage":3})).unwrap();
        assert_eq!(reader.unit_range(&json!({})).unwrap(), 1..3);
        assert_eq!(reader.unit(1).unwrap().locator["source"], "ocr_layer");
        assert!(
            reader
                .unit(1)
                .unwrap()
                .heading
                .unwrap()
                .contains("may contain recognition errors")
        );
        for args in [
            json!({"startPage":0}),
            json!({"startPage":8}),
            json!({"startPage":3,"endPage":2}),
        ] {
            assert_eq!(
                PdfText::open(&bytes, &args).err().unwrap().code,
                "PAGE_OUT_OF_RANGE"
            );
        }
        let mut from = 1;
        let mut total = 0;
        loop {
            let (result, next) = inspect(&bytes, from, Instant::now()).unwrap();
            assert_eq!(result["classified"], 1);
            assert_eq!(result["fromPage"], from);
            assert_eq!(result["complete"], next.is_none());
            total += 1;
            if let Some(n) = next {
                from = n;
            } else {
                break;
            }
        }
        assert_eq!(total, 7);
    }
    #[test]
    fn pdf_cursor_offsets_count_text_and_never_repeat_an_atomic_unit_heading() {
        let tmp = tempfile::tempdir().unwrap();
        let root = crate::filesystem::RootDir::open(&crate::config::Root {
            id: "work".into(),
            path: tmp.path().into(),
            nickname: None,
        })
        .unwrap();
        let (doc, _) = fixture();
        let bytes = bytes(doc);
        std::fs::write(tmp.path().join("cursor.pdf"), &bytes).unwrap();
        let expected = PdfDocument::load(&bytes, &HashSet::new())
            .unwrap()
            .page_text(1)
            .unwrap()
            .text;
        let mut args = json!({"path":"cursor.pdf","startPage":1,"endPage":1,"maxChars":1});
        let mut recovered = String::new();
        let mut calls = 0;
        loop {
            let result = crate::documents::read_document(&root, "work", &args).unwrap();
            assert_eq!(result["units"][0]["chars"], 1);
            assert_eq!(result["units"][0]["page"], 1);
            let mut body = result["text"].as_str().unwrap().split_once('\n').unwrap().1;
            if calls == 0 {
                assert!(body.starts_with("--- page 1"));
                body = body.split_once('\n').unwrap().1;
            } else {
                assert!(!body.contains("---"));
            }
            recovered.push_str(body);
            calls += 1;
            assert!(calls < 200);
            if let Some(cursor) = result.get("nextCursor") {
                args["cursor"] = cursor.clone();
            } else {
                break;
            }
        }
        assert_eq!(recovered, expected);
    }
    #[test]
    fn production_document_loads_use_only_the_bounded_helper() {
        let production = include_str!("pdf.rs").split("mod tests {").next().unwrap();
        assert!(!production.contains("load_metadata"));
        let start = production
            .find("fn load_document(")
            .expect("one private document loader");
        let end = start + production[start..].find("\n}\n").unwrap() + 3;
        let pattern = regex::Regex::new(r"\bDocument\s*::\s*(load\w*)\s*\(").unwrap();
        let calls = pattern.captures_iter(production).collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(&calls[0][1], "load_mem_with_options");
        let call = calls[0].get(0).unwrap();
        assert!(call.start() >= start && call.end() <= end);
    }
    #[test]
    fn padded_compressed_xref_uses_the_bounded_loader() {
        let mut data = b"%PDF-1.5\n".to_vec();
        let content = b"BT /F 12 Tf (Synthetic XRef stream text) Tj ET";
        let objects = [
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 600 800] >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            [format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(), content.to_vec(), b"\nendstream".to_vec()].concat(),
        ];
        let mut offsets = vec![0];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(data.len() as u32);
            data.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            data.extend_from_slice(object);
            data.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = data.len();
        offsets.push(xref_offset as u32);
        let mut rows = vec![0; 4 * PDF_STREAM_LIMIT];
        for (index, offset) in offsets.iter().enumerate() {
            let row = &mut rows[index * 7..index * 7 + 7];
            row[0] = u8::from(index != 0);
            row[1..5].copy_from_slice(&offset.to_be_bytes());
            if index == 0 {
                row[5..7].copy_from_slice(&u16::MAX.to_be_bytes());
            }
        }
        let xref = compressed(rows);
        data.extend_from_slice(format!("6 0 obj\n<< /Type /XRef /Size 7 /Root 1 0 R /Index [0 7] /W [1 4 2] /Filter /FlateDecode /Length {} >>\nstream\n", xref.content.len()).as_bytes());
        data.extend_from_slice(&xref.content);
        data.extend_from_slice(
            format!("\nendstream\nendobj\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
        );
        assert!(data.len() < 300 * 1024);
        DOCUMENT_LOAD_LIMITS.with(|calls| calls.borrow_mut().clear());
        match PdfDocument::load(&data, &HashSet::new()) {
            Ok(pdf) => {
                assert_eq!(pdf.page_count(), 1);
                assert_eq!(pdf.page_text(1).unwrap().text, "Synthetic XRef stream text");
            }
            Err(error) => assert_eq!(error.code, "DOCUMENT_INVALID"),
        }
        DOCUMENT_LOAD_LIMITS.with(|calls| assert_eq!(*calls.borrow(), [Some(PDF_STREAM_LIMIT)]));
    }
    #[test]
    fn encrypted_metadata_and_invalid_documents_have_explicit_errors() {
        assert_eq!(
            PdfDocument::load(b"\xff\0\x01", &HashSet::new())
                .err()
                .unwrap()
                .code,
            "DOCUMENT_INVALID"
        );
        for user_password in ["user", ""] {
            let (mut doc, _) = fixture();
            doc.trailer.set(
                "ID",
                vec![
                    Object::string_literal("synthetic-id-0001"),
                    Object::string_literal("synthetic-id-0001"),
                ],
            );
            let state = lopdf::EncryptionState::try_from(lopdf::EncryptionVersion::V2 {
                document: &doc,
                owner_password: "owner",
                user_password,
                key_length: 128,
                permissions: lopdf::Permissions::all(),
            })
            .unwrap();
            doc.encrypt(&state).unwrap();
            let encrypted = bytes(doc);
            assert!(Document::load_metadata_mem(&encrypted).unwrap().encrypted);
            if user_password.is_empty() {
                let pdf = PdfDocument::load(&encrypted, &HashSet::new()).unwrap();
                assert_eq!(pdf.metadata()["encrypted"], true);
                assert_eq!(pdf.metadata()["title"], "Synthetic PDF");
                assert_eq!(pdf.metadata()["producer"], "local-files tests");
                assert!(
                    pdf.page_text(1)
                        .unwrap()
                        .text
                        .contains("Native Helvetica text")
                );
            } else {
                assert_eq!(
                    PdfDocument::load(&encrypted, &HashSet::new())
                        .err()
                        .unwrap()
                        .code,
                    "PDF_ENCRYPTED"
                );
            }
        }
    }
    #[test]
    #[ignore = "synthetic 306 MiB PDF; run in release with /usr/bin/time -l"]
    fn large_pdf_budget() {
        use std::io::{Seek, Write};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("large.pdf");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let pages = 980usize;
        let image_len = 320 * 1024usize;
        let mut offsets = vec![0u64; 4 + pages * 3];
        file.write_all(b"%PDF-1.5\n").unwrap();
        let write_object = |file: &mut std::io::BufWriter<std::fs::File>,
                            offsets: &mut [u64],
                            id: usize,
                            text: &str| {
            offsets[id] = file.stream_position().unwrap();
            writeln!(file, "{id} 0 obj\n{text}\nendobj").unwrap();
        };
        write_object(
            &mut file,
            &mut offsets,
            1,
            "<< /Type /Catalog /Pages 2 0 R >>",
        );
        let kids = (0..pages)
            .map(|i| format!("{} 0 R", 4 + i * 3))
            .collect::<Vec<_>>()
            .join(" ");
        write_object(
            &mut file,
            &mut offsets,
            2,
            &format!("<< /Type /Pages /Count {pages} /Kids [{kids}] /MediaBox [0 0 600 800] >>"),
        );
        write_object(
            &mut file,
            &mut offsets,
            3,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        let mut random = [0u8; 4096];
        let mut seed = 0x12345678u32;
        for i in 0..pages {
            let page = 4 + i * 3;
            let content = page + 1;
            let image = page + 2;
            write_object(
                &mut file,
                &mut offsets,
                page,
                &format!(
                    "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F 3 0 R >> /XObject << /Im {image} 0 R >> >> /Contents {content} 0 R >>"
                ),
            );
            let text = format!(
                "/Im Do BT /F 12 Tf 3 Tr (Synthetic invisible OCR text for page {}) Tj ET",
                i + 1
            );
            write_object(
                &mut file,
                &mut offsets,
                content,
                &format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()),
            );
            offsets[image] = file.stream_position().unwrap();
            writeln!(file,"{image} 0 obj\n<< /Type /XObject /Subtype /Image /Width 1200 /Height 1000 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode /Length {image_len} >>\nstream").unwrap();
            for _ in 0..image_len / random.len() {
                for b in &mut random {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    *b = seed as u8;
                }
                file.write_all(&random).unwrap();
            }
            file.write_all(b"\nendstream\nendobj\n").unwrap();
        }
        let xref = file.stream_position().unwrap();
        writeln!(file, "xref\n0 {}\n0000000000 65535 f ", offsets.len()).unwrap();
        for offset in &offsets[1..] {
            writeln!(file, "{offset:010} 00000 n ").unwrap();
        }
        writeln!(
            file,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF",
            offsets.len()
        )
        .unwrap();
        file.flush().unwrap();
        drop(file);
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size >= 300 * 1024 * 1024);
        let root = crate::filesystem::RootDir::open(&crate::config::Root {
            id: "work".into(),
            path: tmp.path().into(),
            nickname: None,
        })
        .unwrap();
        let start = Instant::now();
        let overview =
            crate::documents::inspect_document(&root, "work", &json!({"path":"large.pdf"}))
                .unwrap();
        let inspect_time = start.elapsed();
        assert!(inspect_time.as_secs_f64() < 10.0);
        assert_eq!(overview["complete"], true);
        assert_eq!(overview["pageCount"], 980);
        let start = Instant::now();
        let chunk = crate::documents::read_document(
            &root,
            "work",
            &json!({"path":"large.pdf","startPage":900,"endPage":900}),
        )
        .unwrap();
        let read_time = start.elapsed();
        assert!(read_time.as_secs_f64() < 10.0);
        assert!(chunk["text"].as_str().unwrap().contains("page 900"));
        eprintln!(
            "large_pdf_budget: bytes={size}, inspect={inspect_time:?}, read_page_900={read_time:?}"
        );
    }
}
