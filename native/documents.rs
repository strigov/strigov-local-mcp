use crate::{
    cursor::{self, Cursor, CursorKind, Fingerprint},
    docx, email,
    filesystem::{
        self, OUTPUT_LIMIT, Result, RootDir, TEXT_READ_LIMIT, error, io_error, number, string,
    },
    imaging, msg, pdf, pdf_page, sheets,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    ops::Range,
    path::{Path, PathBuf},
    time::Instant,
};

pub(crate) const DOC_DEFAULT_CHARS: usize = if OUTPUT_LIMIT < 100_000 {
    OUTPUT_LIMIT
} else {
    100_000
};
pub(crate) const ZIP_ENTRY_LIMIT: usize = 20_000;
pub(crate) const ZIP_RATIO_LIMIT: u64 = 200;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Tiff,
    Bmp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Text,
    Pdf,
    Docx,
    Xlsx,
    Xls,
    Eml,
    Msg,
    Zip,
    Image(ImageFormat),
    Heic,
    LegacyOffice,
    Other,
}
pub(crate) fn format_of(name: &str) -> Format {
    let p = Path::new(name);
    match p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => Format::Pdf,
        "docx" | "docm" => Format::Docx,
        "xlsx" | "xlsm" => Format::Xlsx,
        "xls" => Format::Xls,
        "eml" => Format::Eml,
        "msg" => Format::Msg,
        "zip" => Format::Zip,
        "png" => Format::Image(ImageFormat::Png),
        "jpg" | "jpeg" => Format::Image(ImageFormat::Jpeg),
        "gif" => Format::Image(ImageFormat::Gif),
        "webp" => Format::Image(ImageFormat::Webp),
        "tif" | "tiff" => Format::Image(ImageFormat::Tiff),
        "bmp" => Format::Image(ImageFormat::Bmp),
        "heic" | "heif" => Format::Heic,
        "doc" | "ppt" | "pptx" | "odt" | "ods" | "rtf" | "rar" | "7z" => Format::LegacyOffice,
        _ if filesystem::is_text(p) => Format::Text,
        _ => Format::Other,
    }
}
impl Format {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Xls => "xls",
            Self::Eml => "eml",
            Self::Msg => "msg",
            Self::Zip => "zip",
            Self::Heic => "heic",
            Self::LegacyOffice => "legacy_office",
            Self::Other => "other",
            Self::Image(i) => match i {
                ImageFormat::Png => "png",
                ImageFormat::Jpeg => "jpeg",
                ImageFormat::Gif => "gif",
                ImageFormat::Webp => "webp",
                ImageFormat::Tiff => "tiff",
                ImageFormat::Bmp => "bmp",
            },
        }
    }
    pub(crate) fn has_text_reader(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Pdf | Self::Docx | Self::Xlsx | Self::Xls | Self::Eml | Self::Msg
        )
    }
}
pub(crate) fn unsupported(format: Format, name: &str) -> filesystem::FsError {
    let hint = match format {
        Format::Heic => "convert to JPEG/PNG",
        Format::LegacyOffice
            if matches!(
                Path::new(name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str(),
                "rar" | "7z"
            ) =>
        {
            "extract it or repack as ZIP"
        }
        _ => "save as DOCX or PDF, or provide a UTF-8 text copy",
    };
    error(
        "UNSUPPORTED_FORMAT",
        format!("Unsupported {} format ({name}); {hint}", format.name()),
    )
}
pub(crate) struct Location {
    pub root: String,
    pub disk: PathBuf,
    pub members: Vec<String>,
}
impl Location {
    pub(crate) fn display(&self) -> String {
        let mut s = filesystem::public(&self.root, &self.disk);
        for member in &self.members {
            s.push_str("::");
            s.push_str(member);
        }
        s
    }
    pub(crate) fn id(&self) -> String {
        let mut s = format!("{}:{}", self.root, filesystem::display(&self.disk));
        for member in &self.members {
            s.push_str("::");
            s.push_str(member);
        }
        s
    }
    pub(crate) fn uri(&self) -> String {
        filesystem::uri(&self.root, &self.disk)
    }
}
pub(crate) enum Source {
    File(BufReader<cap_std::fs::File>),
    Memory(std::io::Cursor<Vec<u8>>),
}
impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::File(f) => f.read(buf),
            Self::Memory(f) => f.read(buf),
        }
    }
}
impl Seek for Source {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match self {
            Self::File(f) => f.seek(pos),
            Self::Memory(f) => f.seek(pos),
        }
    }
}
pub(crate) struct Opened {
    pub location: Location,
    pub name: String,
    pub format: Format,
    pub size: u64,
    pub fingerprint: Fingerprint,
    pub source: Source,
}
impl Opened {
    pub(crate) fn into_bytes(mut self, limit: u64) -> Result<Vec<u8>> {
        if self.size > limit {
            return Err(error(
                "FILE_TOO_LARGE",
                "Document exceeds the format size limit",
            ));
        }
        let mut bytes = Vec::with_capacity(self.size as usize + 1);
        (&mut self.source)
            .take(self.size + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() as u64 > self.size {
            return Err(error("FILE_TOO_LARGE", "File grew beyond the read limit"));
        }
        Ok(bytes)
    }
}
pub(crate) const CONTAINER_DEPTH: usize = 3;
pub(crate) fn format_limit(format: Format) -> u64 {
    match format {
        Format::Text => TEXT_READ_LIMIT,
        Format::Pdf => pdf::PDF_LIMIT,
        Format::Docx => docx::DOCX_LIMIT,
        Format::Xlsx | Format::Xls => sheets::SHEET_LIMIT,
        Format::Eml => email::EML_LIMIT,
        Format::Msg => msg::MSG_LIMIT,
        Format::Image(_) => imaging::IMAGE_SOURCE_LIMIT,
        _ => u64::MAX,
    }
}
pub(crate) fn check_attachment(info: &email::AttachmentInfo) -> Result<()> {
    if filesystem::blocked(Path::new(&info.name.replace('\\', "/"))) {
        return Err(error(
            "BLOCKED_PATH",
            "Credential or service path is blocked",
        ));
    }
    let name = crate::archive::normalize_name(&info.name)
        .ok_or_else(|| error("UNSAFE_MEMBER_NAME", "Unsafe attachment name"))?;
    if filesystem::blocked(Path::new(&name)) {
        return Err(error(
            "BLOCKED_PATH",
            "Credential or service path is blocked",
        ));
    }
    if info.size
        > crate::archive::MEMBER_LIMIT.min(format_limit(if info.embedded_message {
            Format::Eml
        } else {
            format_of(&info.name)
        }))
    {
        return Err(error(
            "FILE_TOO_LARGE",
            "Attachment exceeds the format size limit",
        ));
    }
    Ok(())
}
pub(crate) fn attachment_reason(
    info: &email::AttachmentInfo,
    format: Format,
) -> Option<&'static str> {
    check_attachment(info)
        .err()
        .map(|e| e.code)
        .or((format == Format::Msg && info.embedded_message).then_some("UNSUPPORTED_FORMAT"))
}
pub(crate) fn open(root: &RootDir, root_id: &str, raw: &str) -> Result<Opened> {
    guarded(|| {
        let parts: Vec<_> = raw.split("::").collect();
        if parts.len() > CONTAINER_DEPTH + 1 {
            return Err(error(
                "NESTING_TOO_DEEP",
                "At most three container levels are supported",
            ));
        }
        let disk = filesystem::relative(parts[0])?;
        let real = root.resolve(parts[0])?;
        let name = filesystem::display(&real);
        let format = format_of(&name);
        let (file, fingerprint) = root.open_file(&real, format_limit(format))?;
        let mut opened = Opened {
            location: Location {
                root: root_id.into(),
                disk,
                members: vec![],
            },
            name,
            format,
            size: fingerprint.size,
            fingerprint,
            source: Source::File(BufReader::new(file)),
        };
        for member in &parts[1..] {
            let mut location = Location {
                root: opened.location.root.clone(),
                disk: opened.location.disk.clone(),
                members: opened.location.members.clone(),
            };
            let (name, bytes, component, embedded) = match opened.format {
                Format::Zip => {
                    let name = crate::archive::normalize_name(member)
                        .filter(|n| !n.is_empty())
                        .ok_or_else(|| error("UNSAFE_MEMBER_NAME", "Unsafe ZIP member name"))?;
                    let (info, bytes) = crate::archive::read_member(
                        opened.source,
                        &name,
                        format_limit(format_of(&name)),
                    )?;
                    (info.name.clone(), bytes, info.name, false)
                }
                Format::Eml | Format::Msg => {
                    if member.is_empty() || !member.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(error("MEMBER_NOT_FOUND", "Use a decimal attachment index"));
                    }
                    let index: usize = member
                        .parse()
                        .map_err(|_| error("MEMBER_NOT_FOUND", "Invalid attachment index"))?;
                    let (info, bytes) = if opened.format == Format::Eml {
                        email::eml_attachment(&opened.into_bytes(email::EML_LIMIT)?, index)?
                    } else {
                        msg::msg_attachment(opened.source, index)?
                    };
                    let name = crate::archive::normalize_name(&info.name)
                        .ok_or_else(|| error("UNSAFE_MEMBER_NAME", "Unsafe attachment name"))?;
                    (name, bytes, index.to_string(), info.embedded_message)
                }
                _ => {
                    return Err(error(
                        "NOT_A_CONTAINER",
                        "Selected member is not a container",
                    ));
                }
            };
            // Every post-disk level is bytes only. No member ever reaches RootDir.
            // Preserve the outer on-disk fingerprint across the entire chain.
            location.members.push(component);
            opened = Opened {
                location,
                format: if embedded {
                    Format::Eml
                } else {
                    format_of(&name)
                },
                name,
                size: bytes.len() as u64,
                fingerprint,
                source: Source::Memory(std::io::Cursor::new(bytes)),
            };
        }
        Ok(opened)
    })
}
fn check_child_depth(opened: &Opened) -> Result<()> {
    if opened.location.members.len() >= CONTAINER_DEPTH
        && matches!(opened.format, Format::Zip | Format::Eml | Format::Msg)
    {
        return Err(error(
            "NESTING_TOO_DEEP",
            "At most three container levels are supported",
        ));
    }
    Ok(())
}
/// Container listings use the same Walk position semantics as filesystem listings.
pub(crate) fn list_container(root: &RootDir, root_id: &str, args: &Value) -> Result<Value> {
    guarded(|| {
        let deadline = call_deadline();
        let raw = string(args, "path", None)?;
        let (opened, prefix) = match open(root, root_id, raw) {
            Ok(opened) => (opened, String::new()),
            Err(e) if e.code == "MEMBER_NOT_FOUND" => {
                let (parent, prefix) = raw.rsplit_once("::").ok_or(e)?;
                let opened = open(root, root_id, parent)?;
                if opened.format != Format::Zip {
                    return Err(error("NOT_A_CONTAINER", "Selected path is not a container"));
                }
                let prefix = crate::archive::normalize_name(prefix)
                    .ok_or_else(|| error("UNSAFE_MEMBER_NAME", "Unsafe ZIP prefix"))?;
                if filesystem::blocked(Path::new(&prefix)) {
                    return Err(error(
                        "BLOCKED_PATH",
                        "Credential or service path is blocked",
                    ));
                }
                (opened, prefix)
            }
            Err(e) => return Err(e),
        };
        check_child_depth(&opened)?;
        let depth = number(args, "depth", 1, 5)?;
        let limit = number(args, "limit", 500, 2000)?;
        let base = if prefix.is_empty() {
            opened.location.display()
        } else {
            format!("{}::{prefix}", opened.location.display())
        };
        let hash = cursor::request_hash(&["list_directory", root_id, &base, &depth.to_string()]);
        let resume = args
            .get("cursor")
            .map(|_| cursor::decode(string(args, "cursor", None)?, CursorKind::Walk, hash))
            .transpose()?;
        if resume
            .as_ref()
            .is_some_and(|c| c.p.first().map(String::as_str) != Some(root_id))
        {
            return Err(error("INVALID_CURSOR", "Invalid container position"));
        }
        let after = resume.as_ref().map(|c| &c.p[1..]);
        let display = opened.location.display();
        let mut entries = std::collections::BTreeMap::<Vec<String>, Value>::new();
        match opened.format {
            Format::Zip => {
                let members = crate::archive::list_members(opened.source)?;
                let prefix_slash = format!("{prefix}/");
                let mut found_prefix = prefix.is_empty();
                for member in members {
                    if member.name == prefix {
                        found_prefix = true;
                        continue;
                    }
                    let relative = if prefix.is_empty() {
                        member.name.as_str()
                    } else if let Some(s) = member.name.strip_prefix(&prefix_slash) {
                        found_prefix = true;
                        s
                    } else {
                        continue;
                    };
                    let parts: Vec<String> = if member.reason == Some("UNSAFE_MEMBER_NAME") {
                        vec![relative.into()]
                    } else {
                        relative
                            .split('/')
                            .filter(|p| !p.is_empty())
                            .map(str::to_owned)
                            .collect()
                    };
                    for n in 1..=parts.len().min(depth) {
                        let components = parts[..n].to_vec();
                        let path = format!(
                            "{base}{}{}",
                            if prefix.is_empty() { "::" } else { "/" },
                            components.join("/")
                        );
                        let is_dir = n < parts.len() || member.is_dir;
                        let entry = json!({"name":parts[n-1],"path":path,"relativePath":path.strip_prefix(&format!("{root_id}/")).unwrap_or(&path),"type":if is_dir {"directory"} else {"file"},"size":if is_dir {None} else {Some(member.size)},"compressedSize":if is_dir {None} else {Some(member.compressed_size)},"readable":if is_dir && n < parts.len() {true} else {member.readable},"reason":if is_dir && n < parts.len() {None} else {member.reason}});
                        entries.entry(components).or_insert(entry);
                    }
                }
                if !found_prefix {
                    return Err(error("MEMBER_NOT_FOUND", "ZIP prefix not found"));
                }
            }
            Format::Eml | Format::Msg => {
                let format = opened.format;
                let mail = if format == Format::Eml {
                    email::parse_eml(&opened.into_bytes(email::EML_LIMIT)?)?
                } else {
                    msg::parse_msg(opened.source)?
                };
                for info in mail.attachments {
                    let reason = attachment_reason(&info, format);
                    if reason == Some("BLOCKED_PATH") {
                        continue;
                    }
                    let component = info.index.to_string();
                    let path = format!("{display}::{component}");
                    entries.insert(vec![component],json!({"name":info.name,"path":path,"relativePath":path.strip_prefix(&format!("{root_id}/")).unwrap_or(&path),"type":"file","size":info.size,"index":info.index,"mimeType":info.mime,"readable":reason.is_none(),"reason":reason}));
                }
            }
            _ => return Err(error("NOT_A_CONTAINER", "Selected path is not a container")),
        }
        let mut output = Vec::new();
        let mut skipped = 0;
        let mut pending = 0;
        let mut omitted_directory: Option<Vec<String>> = None;
        let mut last = None;
        let mut incomplete = false;
        for (components, entry) in entries {
            if omitted_directory
                .as_ref()
                .is_some_and(|p| components.starts_with(p))
            {
                continue;
            }
            omitted_directory = None;
            if after.is_some_and(|p| components.as_slice() <= p) {
                continue;
            }
            let (name, parent) = components.split_last().unwrap();
            if !cursor::walk_child_fits(root_id, parent, name) {
                pending += 1;
                if entry["type"] == "directory" {
                    omitted_directory = Some(components);
                }
                continue;
            }
            // Skips belong to the next yielded entry, never to the final peek.
            if !output.is_empty() && (output.len() >= limit || Instant::now() >= deadline) {
                incomplete = true;
                break;
            }
            skipped += pending;
            pending = 0;
            let mut p = vec![root_id.to_owned()];
            p.extend(components);
            last = Some(Cursor {
                v: 1,
                k: CursorKind::Walk,
                h: hash,
                f: None,
                p,
                u: 0,
                o: 0,
            });
            output.push(entry);
        }
        if !incomplete {
            skipped += pending;
        }
        let mut result = json!({"root":root_id,"path":base,"entries":output,"truncated":incomplete,"skippedTooDeep":skipped});
        if incomplete {
            result["nextCursor"] = json!(cursor::encode(&last.unwrap()));
        }
        Ok(result)
    })
}
pub(crate) struct Unit {
    pub locator: Value,
    pub heading: Option<String>,
    pub text: String,
}
pub(crate) trait TextDocument {
    fn unit_name(&self) -> &'static str;
    fn unit_count(&self) -> usize;
    fn unit(&mut self, index: usize) -> Result<Unit>;
    fn unit_within(&mut self, index: usize, _deadline: Option<Instant>) -> Result<Option<Unit>> {
        self.unit(index).map(Some)
    }
    fn unit_range(&self, _args: &Value) -> Result<Range<usize>> {
        Ok(0..self.unit_count())
    }
    fn summary(&self) -> Value;
}
struct PlainText {
    reader: BufReader<Source>,
    lines: usize,
    next: usize,
    size: u64,
    encoding: crate::text::TextEncoding,
}
impl PlainText {
    fn open(opened: Opened) -> Result<Self> {
        if opened.size > TEXT_READ_LIMIT {
            return Err(error("FILE_TOO_LARGE", "Text exceeds 64 MiB"));
        }
        let mut reader = BufReader::new(opened.source);
        let (encoding, lines) = crate::text::scan(&mut reader, opened.size, true)?;
        reader.rewind().map_err(io_error)?;
        Ok(Self {
            reader,
            lines,
            next: 0,
            size: opened.size,
            encoding,
        })
    }
}
impl TextDocument for PlainText {
    fn unit_name(&self) -> &'static str {
        "line"
    }
    fn unit_count(&self) -> usize {
        self.lines
    }
    fn unit(&mut self, index: usize) -> Result<Unit> {
        if index >= self.lines {
            return Err(error("INVALID_CURSOR", "Invalid line position"));
        }
        if index < self.next {
            self.reader.rewind().map_err(io_error)?;
            self.next = 0;
        }
        let mut line = Vec::new();
        while self.next <= index {
            line.clear();
            self.reader.read_until(b'\n', &mut line).map_err(io_error)?;
            self.next += 1;
        }
        if line.ends_with(b"\n") {
            line.pop();
        }
        let text = crate::text::decode(self.encoding, &line)?;
        let text = text.strip_suffix('\r').unwrap_or(&text).to_string();
        Ok(Unit {
            locator: json!({"line":index+1}),
            heading: None,
            text,
        })
    }
    fn summary(&self) -> Value {
        json!({"size":self.size,"lines":self.lines,"encoding":self.encoding.label()})
    }
}
pub(crate) fn open_text_document(opened: Opened, args: &Value) -> Result<Box<dyn TextDocument>> {
    guarded(|| match opened.format {
        Format::Text => Ok(Box::new(PlainText::open(opened)?) as Box<dyn TextDocument>),
        Format::Pdf => Ok(Box::new(pdf::PdfText::open(
            &opened.into_bytes(pdf::PDF_LIMIT)?,
            args,
        )?) as Box<dyn TextDocument>),
        Format::Docx => {
            if opened.size > docx::DOCX_LIMIT {
                return Err(error("FILE_TOO_LARGE", "DOCX exceeds 256 MiB"));
            }
            Ok(Box::new(docx::open(opened.source)?) as Box<dyn TextDocument>)
        }
        Format::Xlsx | Format::Xls => {
            if opened.size > sheets::SHEET_LIMIT {
                return Err(error("FILE_TOO_LARGE", "Spreadsheet exceeds 64 MiB"));
            }
            Ok(Box::new(sheets::open(opened.source, opened.format)?) as Box<dyn TextDocument>)
        }
        Format::Eml => {
            let bytes = opened.into_bytes(email::EML_LIMIT)?;
            let mail = email::parse_eml(&bytes)?;
            drop(bytes);
            Ok(Box::new(email::mail_document(mail, Format::Eml)) as Box<dyn TextDocument>)
        }
        Format::Msg => {
            if opened.size > msg::MSG_LIMIT {
                return Err(error("FILE_TOO_LARGE", "MSG exceeds 256 MiB"));
            }
            Ok(Box::new(email::mail_document(
                msg::parse_msg(opened.source)?,
                Format::Msg,
            )) as Box<dyn TextDocument>)
        }
        _ => Err(unsupported(opened.format, &opened.name)),
    })
}
pub(crate) fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(error("DOCUMENT_INVALID", "Cannot parse document")))
}
fn sequence_hash(tool: &str, location: &Location, args: &Value) -> u64 {
    let start = args
        .get("startPage")
        .map(Value::to_string)
        .unwrap_or_default();
    let end = args
        .get("endPage")
        .map(Value::to_string)
        .unwrap_or_default();
    let sheet = args.get("sheet").map(Value::to_string).unwrap_or_default();
    // Preserve the released disk-only sequence hash; append members to its path.
    let id = location.id();
    cursor::request_hash(&[
        tool,
        &location.root,
        id.split_once(':').unwrap().1,
        &start,
        &end,
        &sheet,
    ])
}
fn continuation(
    args: &Value,
    kind: CursorKind,
    hash: u64,
    fingerprint: Fingerprint,
) -> Result<Option<Cursor>> {
    let Some(_) = args.get("cursor") else {
        return Ok(None);
    };
    let c = cursor::decode(string(args, "cursor", None)?, kind, hash)?;
    cursor::check_fingerprint(&c, fingerprint)?;
    Ok(Some(c))
}
fn position(kind: CursorKind, hash: u64, fingerprint: Fingerprint, u: usize, o: usize) -> Cursor {
    Cursor {
        v: 1,
        k: kind,
        h: hash,
        f: Some(fingerprint),
        p: vec![],
        u: u as u64,
        o: o as u64,
    }
}
#[cfg(test)]
thread_local! { pub(crate) static TEST_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) }; }
fn call_deadline() -> Instant {
    let deadline = Instant::now() + filesystem::SEARCH_DEADLINE;
    #[cfg(test)]
    let deadline = TEST_DEADLINE.with(|value| value.get().unwrap_or(deadline));
    deadline
}
pub(crate) fn read_document(root: &RootDir, root_id: &str, args: &Value) -> Result<Value> {
    guarded(|| {
        let deadline = call_deadline();
        let opened = open(root, root_id, string(args, "path", None)?)?;
        if opened.format == Format::Zip {
            return inspect_document(root, root_id, args);
        }
        let max = number(args, "maxChars", DOC_DEFAULT_CHARS, OUTPUT_LIMIT)?;
        let hash = sequence_hash("read_document", &opened.location, args);
        let resume = continuation(args, CursorKind::Doc, hash, opened.fingerprint)?;
        let fingerprint = opened.fingerprint;
        let format = opened.format;
        let location = Location {
            root: opened.location.root.clone(),
            disk: opened.location.disk.clone(),
            members: opened.location.members.clone(),
        };
        let mut doc = open_text_document(opened, args)?;
        let range = doc.unit_range(args)?;
        let mut u = resume
            .as_ref()
            .map(|c| usize::try_from(c.u))
            .transpose()
            .map_err(|_| error("INVALID_CURSOR", "Invalid unit position"))?
            .unwrap_or(range.start);
        let mut o = resume
            .as_ref()
            .map(|c| usize::try_from(c.o))
            .transpose()
            .map_err(|_| error("INVALID_CURSOR", "Invalid text offset"))?
            .unwrap_or(0);
        if u < range.start || u >= range.end && !(range.is_empty() && resume.is_none()) {
            return Err(error("INVALID_CURSOR", "Invalid unit position"));
        }
        let first = u;
        let total = doc.unit_count();
        let display = location.display().replace('\n', "\\n").replace('\r', "\\r");
        let unit_name = doc.unit_name();
        let heading = |last| {
            format!(
                "[source: {display} — {unit_name}s {}–{last} of {total}]",
                first + 1
            )
        };
        let reserved = heading(total).chars().count() + 1;
        let mut budget = max.saturating_sub(reserved);
        let separator = if format == Format::Text { "\n" } else { "\n\n" };
        let mut body = String::new();
        let mut units = Vec::new();
        let mut last = first;
        while u < range.end {
            let unit = if units.is_empty() {
                doc.unit(u)?
            } else {
                if Instant::now() >= deadline {
                    break;
                }
                let Some(unit) = doc.unit_within(u, Some(deadline))? else {
                    break;
                };
                unit
            };
            let length = unit.text.chars().count();
            if o > length || (o == length && o != 0) {
                return Err(error("INVALID_CURSOR", "Invalid text offset"));
            }
            let prefix = if o == 0 {
                unit.heading
                    .as_ref()
                    .map(|h| format!("{h}\n"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let sep = if units.is_empty() { "" } else { separator };
            let framing = prefix.chars().count() + sep.chars().count();
            let needed = framing + usize::from(length > o);
            if needed > budget {
                if !units.is_empty() {
                    break;
                }
                // Authoritative progress floor for small maxChars requests.
                budget = needed;
            }
            body.push_str(sep);
            body.push_str(&prefix);
            budget -= framing;
            let take = (length - o).min(budget);
            body.extend(unit.text.chars().skip(o).take(take));
            budget -= take;
            let mut locator = unit.locator;
            locator["chars"] = json!(take);
            units.push(locator);
            last = u;
            if o + take < length {
                o += take;
                break;
            }
            u += 1;
            o = 0;
        }
        let text = format!(
            "{}\n{body}",
            heading(if units.is_empty() { 0 } else { last + 1 })
        );
        if text.chars().count() > OUTPUT_LIMIT {
            return Err(error(
                "INVALID_ARGUMENT",
                "Source framing exceeds the response limit",
            ));
        }
        let mut result = json!({"kind":"document","format":format.name(),"root":root_id,"path":location.display(),"id":location.id(),"uri":location.uri(),"text":text,"units":units,"unitName":doc.unit_name(),"totalUnits":total,"truncated":u < range.end});
        if let Value::Object(summary) = doc.summary() {
            result.as_object_mut().unwrap().extend(summary);
        }
        attachment_paths(&mut result, &location.display(), location.members.len());
        if u < range.end {
            result["nextCursor"] = json!(cursor::encode(&position(
                CursorKind::Doc,
                hash,
                fingerprint,
                u,
                o
            )));
        }
        Ok(result)
    })
}
pub(crate) fn read_file(root: &RootDir, root_id: &str, args: &Value) -> Result<Value> {
    guarded(|| {
        let mut opened = open(root, root_id, string(args, "path", None)?)?;
        let page = if args.get("page").is_some() {
            Some(number(args, "page", 1, 1_000_000)? as u32)
        } else {
            None
        };
        let region = imaging::parse_region(args)?;
        let location = &opened.location;
        let mut result = json!({"root":root_id,"path":location.display(),"id":location.id(),"uri":location.uri()});
        let view = match opened.format {
            Format::Image(format) => pdf_page::PageView::Image(imaging::view_file(
                &mut opened.source,
                format,
                page,
                region,
            )?),
            Format::Pdf => {
                let Some(page) = page else {
                    return read_document(root, root_id, args);
                };
                pdf_page::view_page(&opened.into_bytes(pdf::PDF_LIMIT)?, page, region)?
            }
            _ => {
                return Err(error(
                    "INVALID_ARGUMENT",
                    "page and region are only available for images and PDF pages",
                ));
            }
        };
        match view {
            pdf_page::PageView::Image(view) => {
                use base64::Engine;
                result["kind"] = json!("image");
                result["mimeType"] = json!(view.mime);
                result["size"] = json!(view.data.len());
                result["data"] =
                    json!(base64::engine::general_purpose::STANDARD.encode(&view.data));
                result["width"] = json!(view.width);
                result["height"] = json!(view.height);
                result["sourceWidth"] = json!(view.source_width);
                result["sourceHeight"] = json!(view.source_height);
                result["passthrough"] = json!(view.passthrough);
                if let Some(page) = view.page {
                    result["page"] = json!(page);
                }
                if let Some(pages) = view.pages {
                    result["pages"] = json!(pages);
                }
                if let Some(region) = view.region {
                    result["region"] = json!(region);
                }
                result["note"] = json!(format!(
                    "View from {}×{}; pass region {{x,y,width,height}} (fractions 0–1 of the oriented image) to zoom",
                    view.source_width, view.source_height
                ));
            }
            pdf_page::PageView::Pdf(data) => {
                use base64::Engine;
                result["kind"] = json!("binary");
                result["mimeType"] = json!("application/pdf");
                result["size"] = json!(data.len());
                result["page"] = json!(page);
                result["pageCount"] = json!(1);
                result["uri"] = json!(format!(
                    "{}#page={}",
                    result["uri"].as_str().unwrap(),
                    page.unwrap()
                ));
                result["blob"] = json!(base64::engine::general_purpose::STANDARD.encode(data));
                result["note"] = json!(
                    "Single-page PDF fallback; approve ChatGPT's Allow file materialization? prompt to view the page (approval applies only to this chat)"
                );
            }
            pdf_page::PageView::NotViewable(reason) => {
                result["kind"] = json!("not_viewable");
                result["reason"] = json!(reason);
                if let Some(page) = page {
                    result["page"] = json!(page);
                }
                result["text"] = json!(
                    "This page cannot be viewed by this connector; use inspect_document for its classification and ask the user for a text copy"
                );
            }
        }
        Ok(result)
    })
}
pub(crate) fn inspect_document(root: &RootDir, root_id: &str, args: &Value) -> Result<Value> {
    guarded(|| {
        let deadline = call_deadline();
        let opened = open(root, root_id, string(args, "path", None)?)?;
        check_child_depth(&opened)?;
        let member_depth = opened.location.members.len();
        let hash = sequence_hash("inspect_document", &opened.location, args);
        let resume = continuation(args, CursorKind::Inspect, hash, opened.fingerprint)?;
        let fingerprint = opened.fingerprint;
        let mut result = json!({"kind":"inspect","format":opened.format.name(),"root":root_id,"path":opened.location.display(),"id":opened.location.id()});
        let (summary, next) = match opened.format {
            Format::Zip => {
                if resume.is_some() {
                    return Err(error(
                        "INVALID_CURSOR",
                        "ZIP inspection is complete in one call",
                    ));
                }
                let members = crate::archive::list_members(opened.source)?;
                (
                    json!({"entries":members.len(),"files":members.iter().filter(|m| !m.is_dir).count(),"directories":members.iter().filter(|m| m.is_dir).count(),"totalUncompressed":members.iter().fold(0u64,|sum,m| sum.saturating_add(m.size)),"unreadable":members.iter().filter(|m| !m.readable).count(),"hint":"use list_directory"}),
                    None,
                )
            }
            Format::Text => {
                if resume.is_some() {
                    return Err(error(
                        "INVALID_CURSOR",
                        "Text inspection is complete in one call",
                    ));
                }
                (PlainText::open(opened)?.summary(), None)
            }
            Format::Image(format) => {
                if resume.is_some() {
                    return Err(error(
                        "INVALID_CURSOR",
                        "Image inspection is complete in one call",
                    ));
                }
                let mut opened = opened;
                (imaging::info(&mut opened.source, format)?, None)
            }
            Format::Pdf => {
                let from = resume
                    .as_ref()
                    .map(|c| {
                        u32::try_from(c.u)
                            .ok()
                            .and_then(|u| u.checked_add(1))
                            .ok_or_else(|| error("INVALID_CURSOR", "Invalid page position"))
                    })
                    .transpose()?
                    .unwrap_or(1);
                pdf::inspect(&opened.into_bytes(pdf::PDF_LIMIT)?, from, deadline)?
            }
            Format::Docx | Format::Xlsx | Format::Xls | Format::Eml | Format::Msg => {
                if resume.is_some() {
                    return Err(error(
                        "INVALID_CURSOR",
                        "Document inspection is complete in one call",
                    ));
                }
                (open_text_document(opened, args)?.summary(), None)
            }
            _ => return Err(unsupported(opened.format, &opened.name)),
        };
        result
            .as_object_mut()
            .unwrap()
            .extend(summary.as_object().unwrap().clone());
        let display = result["path"].as_str().unwrap().to_owned();
        attachment_paths(&mut result, &display, member_depth);
        if let Some(page) = next {
            result["nextCursor"] = json!(cursor::encode(&position(
                CursorKind::Inspect,
                hash,
                fingerprint,
                (page - 1) as usize,
                0
            )));
        }
        Ok(result)
    })
}
fn attachment_paths(value: &mut Value, display: &str, member_depth: usize) {
    if let Some(attachments) = value.get_mut("attachments").and_then(Value::as_array_mut) {
        for attachment in attachments {
            if member_depth >= CONTAINER_DEPTH {
                attachment["readable"] = json!(false);
                attachment["reason"] = json!("NESTING_TOO_DEEP");
            } else if attachment["readable"] == true {
                attachment["path"] = json!(format!("{display}::{}", attachment["index"]));
            }
        }
    }
}
pub(crate) fn fetch(root: &RootDir, root_id: &str, raw_path: &str, id: &str) -> Result<Value> {
    let doc = read_document(
        root,
        root_id,
        &json!({"path":raw_path,"maxChars":DOC_DEFAULT_CHARS}),
    )?;
    if doc["kind"] != "document" {
        return Err(error(
            "UNSUPPORTED_FORMAT",
            "ZIP container; use list_directory",
        ));
    }
    let format = doc["format"].as_str().unwrap_or("other");
    let attachment_name = format!(
        "attachment.{}",
        if format == "text" { "txt" } else { format }
    );
    let mime_name = if raw_path
        .rsplit_once("::")
        .is_some_and(|(_, last)| !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()))
    {
        &attachment_name
    } else {
        raw_path
    };
    let mut metadata = json!({"root":root_id,"path":doc["path"],"mimeType":filesystem::mime(Path::new(mime_name)),"format":doc["format"],"truncated":doc["truncated"]});
    if let Some(cursor) = doc.get("nextCursor") {
        metadata["nextCursor"] = cursor.clone();
    }
    if let Some(encoding) = doc.get("encoding") {
        metadata["encoding"] = encoding.clone();
    }
    Ok(
        json!({"id":id,"title":Path::new(raw_path).file_name().unwrap_or_default().to_string_lossy(),"text":doc["text"],"url":doc["uri"],"metadata":metadata}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Root;
    fn root(tmp: &tempfile::TempDir) -> RootDir {
        RootDir::open(&Root {
            id: "work".into(),
            path: tmp.path().into(),
            nickname: None,
        })
        .unwrap()
    }
    fn depth_cap_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(
                *name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    fn depth_cap_mail(format: Format) -> Vec<u8> {
        use std::io::Write;
        if format == Format::Eml {
            return b"Subject: synthetic\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: text/plain\r\n\r\nparent body\r\n--m\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=child.txt\r\n\r\nSAFE CHILD\r\n--m--\r\n".to_vec();
        }
        let mut cfb = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
        cfb.create_stream("/__substg1.0_1000001F")
            .unwrap()
            .write_all(
                &"parent body"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let base = "/__attach_version1.0_#00000000";
        cfb.create_storage(base).unwrap();
        cfb.create_stream(format!("{base}/__substg1.0_3707001F"))
            .unwrap()
            .write_all(
                &"child.txt"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        cfb.create_stream(format!("{base}/__substg1.0_37010102"))
            .unwrap()
            .write_all(b"SAFE CHILD")
            .unwrap();
        cfb.into_inner().into_inner()
    }
    fn depth_cap_fixture(
        tmp: &tempfile::TempDir,
        format: Format,
        depth: usize,
        corrupt: bool,
    ) -> String {
        let mut bytes = if corrupt {
            b"truncated".to_vec()
        } else if format == Format::Zip {
            depth_cap_zip(&[("child.txt", b"SAFE CHILD")])
        } else {
            depth_cap_mail(format)
        };
        let mut name = format!("leaf.{}", format.name());
        let mut raw = name.clone();
        for n in (0..depth).rev() {
            bytes = depth_cap_zip(&[(&name, &bytes)]);
            name = format!("depth-{depth}-{n}-{}.zip", format.name());
            raw = format!("{name}::{raw}");
        }
        std::fs::write(tmp.path().join(name), bytes).unwrap();
        raw
    }
    #[test]
    fn attachment_metadata_matches_readability_without_renumbering() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let root = root(&tmp);
        let cases = [
            (".env", Some("BLOCKED_PATH")),
            ("key.pem", Some("BLOCKED_PATH")),
            ("../escape.txt", Some("UNSAFE_MEMBER_NAME")),
            ("safe.txt", None),
            ("nested.eml", None),
        ];
        for format in [Format::Eml, Format::Msg] {
            let bytes = if format == Format::Eml {
                let mut mail = "Subject: synthetic\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: text/plain\r\n\r\nparent body\r\n".to_owned();
                for (i, (name, _)) in cases.iter().enumerate() {
                    let mime = if i == 4 {
                        "message/rfc822"
                    } else {
                        "text/plain"
                    };
                    mail.push_str(&format!("--m\r\nContent-Type: {mime}\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n\r\nSubject: child\r\n\r\nSAFE CHILD\r\n"));
                }
                mail.push_str("--m--\r\n");
                mail.into_bytes()
            } else {
                let mut cfb = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
                for (i, (name, _)) in cases.iter().enumerate() {
                    let base = format!("/__attach_version1.0_#{i:08X}");
                    cfb.create_storage(&base).unwrap();
                    cfb.create_stream(format!("{base}/__substg1.0_3707001F"))
                        .unwrap()
                        .write_all(
                            &name
                                .encode_utf16()
                                .flat_map(u16::to_le_bytes)
                                .collect::<Vec<_>>(),
                        )
                        .unwrap();
                    if i == 4 {
                        cfb.create_storage(format!("{base}/__substg1.0_3701000D"))
                            .unwrap();
                    } else {
                        cfb.create_stream(format!("{base}/__substg1.0_37010102"))
                            .unwrap()
                            .write_all(b"SAFE CHILD")
                            .unwrap();
                    }
                }
                cfb.into_inner().into_inner()
            };
            let name = format!("mail.{}", format.name());
            std::fs::write(tmp.path().join(&name), bytes).unwrap();
            let args = json!({"path":name});
            let listing = list_container(&root, "work", &args).unwrap();
            for metadata in [
                inspect_document(&root, "work", &args).unwrap(),
                read_document(&root, "work", &args).unwrap(),
            ] {
                let attachments = metadata["attachments"].as_array().unwrap();
                assert_eq!(attachments.len(), cases.len());
                for (i, (_, expected)) in cases.iter().enumerate() {
                    let expected = if format == Format::Msg && i == 4 {
                        Some("UNSUPPORTED_FORMAT")
                    } else {
                        *expected
                    };
                    let attachment = &attachments[i];
                    assert_eq!(attachment["index"], i + 1);
                    let path = format!("{name}::{}", i + 1);
                    let child = read_document(&root, "work", &json!({"path":path}));
                    let entry = listing["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|e| e["index"] == i + 1);
                    if let Some(reason) = expected {
                        assert_eq!(child.unwrap_err().code, reason);
                        assert!(attachment.get("path").is_none(), "{format:?}: {attachment}");
                        assert_eq!(attachment["readable"], false);
                        assert_eq!(attachment["reason"], reason);
                        if reason == "BLOCKED_PATH" {
                            assert!(entry.is_none());
                        } else {
                            assert_eq!(entry.unwrap()["reason"], reason);
                            assert_eq!(entry.unwrap()["readable"], false);
                        }
                    } else {
                        assert!(
                            child.unwrap()["text"]
                                .as_str()
                                .unwrap()
                                .contains("SAFE CHILD")
                        );
                        assert_eq!(attachment["path"], format!("work/{path}"));
                        assert_eq!(attachment["readable"], true);
                        assert!(attachment["reason"].is_null());
                        assert_eq!(attachment["path"], entry.unwrap()["path"]);
                    }
                }
            }
        }
    }
    #[test]
    fn container_depth_cap_rejects_listing_at_three_before_parsing_children() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root(&tmp);
        for format in [Format::Zip, Format::Eml, Format::Msg] {
            for corrupt in [false, true] {
                let raw = depth_cap_fixture(&tmp, format, 3, corrupt);
                let failure = list_container(&root, "work", &json!({"path":raw})).unwrap_err();
                assert_eq!(
                    failure.code, "NESTING_TOO_DEEP",
                    "{format:?}, corrupt={corrupt}"
                );
                assert_eq!(
                    failure.message,
                    "At most three container levels are supported"
                );
            }
        }
    }
    #[test]
    fn container_depth_cap_rejects_inspection_at_three_before_parsing_children() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root(&tmp);
        for format in [Format::Zip, Format::Eml, Format::Msg] {
            for corrupt in [false, true] {
                let raw = depth_cap_fixture(&tmp, format, 3, corrupt);
                assert_eq!(
                    inspect_document(&root, "work", &json!({"path":raw}))
                        .unwrap_err()
                        .code,
                    "NESTING_TOO_DEEP",
                    "{format:?}, corrupt={corrupt}"
                );
            }
        }
    }
    #[test]
    fn container_depth_cap_keeps_children_readable_below_three() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root(&tmp);
        for format in [Format::Zip, Format::Eml, Format::Msg] {
            for depth in 0..=2 {
                let raw = depth_cap_fixture(&tmp, format, depth, false);
                let args = json!({"path":raw});
                let listed = list_container(&root, "work", &args).unwrap();
                let entries = listed["entries"].as_array().unwrap();
                assert_eq!(entries.len(), 1, "{format:?}, depth={depth}");
                assert_eq!(entries[0]["readable"], true);
                let child = entries[0]["relativePath"].as_str().unwrap();
                assert_eq!(child.matches("::").count(), depth + 1);
                assert!(
                    read_document(&root, "work", &json!({"path":child})).unwrap()["text"]
                        .as_str()
                        .unwrap()
                        .contains("SAFE CHILD")
                );
                let info = inspect_document(&root, "work", &args).unwrap();
                if format == Format::Zip {
                    assert_eq!(info["entries"], 1);
                } else {
                    assert_eq!(info["attachments"][0]["path"], entries[0]["path"]);
                }
            }
        }
    }
    #[test]
    fn container_depth_cap_keeps_terminal_mail_text_without_child_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root(&tmp);
        for format in [Format::Eml, Format::Msg] {
            let raw = depth_cap_fixture(&tmp, format, 3, false);
            let doc = read_document(&root, "work", &json!({"path":raw})).unwrap();
            assert!(doc["text"].as_str().unwrap().contains("parent body"));
            assert_eq!(doc["attachments"].as_array().unwrap().len(), 1);
            assert!(doc["attachments"][0].get("path").is_none(), "{format:?}");
            assert_eq!(doc["attachments"][0]["readable"], false);
            assert_eq!(doc["attachments"][0]["reason"], "NESTING_TOO_DEEP");
        }
    }
    #[test]
    fn text_cursor_chains_preserve_unicode_and_lines_even_at_one_char() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root(&tmp);
        let lines = [
            "Привет 😀 длинная строка".repeat(8),
            String::new(),
            "Конец".into(),
            String::new(),
        ];
        std::fs::write(tmp.path().join("a.txt"), lines.join("\r\n")).unwrap();
        for max in [1, 80, 100, 200_000] {
            let mut args = json!({"path":"a.txt","maxChars":max});
            let mut recovered = vec![String::new(); lines.len()];
            let mut calls = 0;
            loop {
                let result = read_document(&root, "work", &args).unwrap();
                let text = result["text"].as_str().unwrap();
                assert!(text.starts_with("[source: work/a.txt — lines "));
                assert!(text.chars().count() <= OUTPUT_LIMIT);
                if max >= 80 {
                    assert!(text.chars().count() <= max);
                }
                let mut body = text.split_once('\n').unwrap().1.chars();
                for (i, unit) in result["units"].as_array().unwrap().iter().enumerate() {
                    if i > 0 {
                        assert_eq!(body.next(), Some('\n'));
                    }
                    let line = unit["line"].as_u64().unwrap() as usize - 1;
                    recovered[line]
                        .extend(body.by_ref().take(unit["chars"].as_u64().unwrap() as usize));
                }
                assert_eq!(body.next(), None);
                calls += 1;
                assert!(calls < 500);
                assert_eq!(result["truncated"], result.get("nextCursor").is_some());
                let Some(cursor) = result.get("nextCursor") else {
                    break;
                };
                args["cursor"] = cursor.clone();
                if calls % 2 == 0 {
                    args["maxChars"] = json!(max);
                }
            }
            assert_eq!(recovered, lines);
        }
        let info = inspect_document(&root, "work", &json!({"path":"a.txt"})).unwrap();
        assert_eq!(info["lines"], 4);
    }
    #[test]
    fn changed_files_and_mismatched_requests_reject_cursors_before_reading() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root(&tmp);
        std::fs::write(tmp.path().join("a.txt"), "я".repeat(200)).unwrap();
        std::fs::write(tmp.path().join("b.txt"), "я".repeat(200)).unwrap();
        let chunk = read_document(&root, "work", &json!({"path":"a.txt","maxChars":1})).unwrap();
        let cursor = &chunk["nextCursor"];
        assert_eq!(
            read_document(&root, "work", &json!({"path":"b.txt","cursor":cursor}))
                .unwrap_err()
                .code,
            "INVALID_CURSOR"
        );
        // maxChars may change during the same sequence.
        read_document(
            &root,
            "work",
            &json!({"path":"a.txt","cursor":cursor,"maxChars":1000}),
        )
        .unwrap();
        std::fs::write(tmp.path().join("a.txt"), [0xff]).unwrap();
        assert_eq!(
            read_document(&root, "work", &json!({"path":"a.txt","cursor":cursor}))
                .unwrap_err()
                .code,
            "STALE_CURSOR"
        );
    }
    #[test]
    fn validates_utf8_beyond_the_requested_output_and_guards_panics() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root(&tmp);
        std::fs::write(tmp.path().join("bad.txt"), b"valid\n\xff\x00").unwrap();
        assert_eq!(
            read_document(&root, "work", &json!({"path":"bad.txt","maxChars":1}))
                .unwrap_err()
                .code,
            "NOT_TEXT"
        );
        assert_eq!(
            inspect_document(&root, "work", &json!({"path":"bad.txt"}))
                .unwrap_err()
                .code,
            "NOT_TEXT"
        );
        assert_eq!(
            guarded::<()>(|| panic!("synthetic parser panic"))
                .unwrap_err()
                .code,
            "DOCUMENT_INVALID"
        );
    }
    #[test]
    fn framing_counts_toward_the_absolute_unicode_response_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root(&tmp);
        let input = "\u{1f600}".repeat(OUTPUT_LIMIT + 100);
        std::fs::write(tmp.path().join("limit.txt"), &input).unwrap();
        let first = read_document(
            &root,
            "work",
            &json!({"path":"limit.txt","maxChars":OUTPUT_LIMIT}),
        )
        .unwrap();
        assert_eq!(
            first["text"].as_str().unwrap().chars().count(),
            OUTPUT_LIMIT
        );
        let second = read_document(
            &root,
            "work",
            &json!({"path":"limit.txt","maxChars":1000,"cursor":first["nextCursor"]}),
        )
        .unwrap();
        assert_eq!(second["truncated"], false);
        let body = |result: &Value| {
            result["text"]
                .as_str()
                .unwrap()
                .split_once('\n')
                .unwrap()
                .1
                .to_owned()
        };
        assert_eq!(body(&first) + &body(&second), input);
        for max in [0, OUTPUT_LIMIT + 1] {
            assert_eq!(
                read_document(&root, "work", &json!({"path":"limit.txt","maxChars":max}))
                    .unwrap_err()
                    .code,
                "INVALID_ARGUMENT"
            );
        }
    }
    #[test]
    fn formats_and_logical_locations_are_stable() {
        for (name, format) in [
            ("a.DOCM", Format::Docx),
            ("a.XLSM", Format::Xlsx),
            ("a.EML", Format::Eml),
            ("a.rtf", Format::LegacyOffice),
            ("a.HEIF", Format::Heic),
            ("a.txt", Format::Text),
        ] {
            assert_eq!(format_of(name), format);
        }
        let loc = Location {
            root: "work".into(),
            disk: "Дело/a.zip".into(),
            members: vec!["Папка/b.pdf".into()],
        };
        assert_eq!(loc.display(), "work/Дело/a.zip::Папка/b.pdf");
        assert_eq!(loc.id(), "work:Дело/a.zip::Папка/b.pdf");
        assert!(!loc.uri().contains("::"));
        let opened = Opened {
            location: loc,
            name: "a.txt".into(),
            format: Format::Text,
            size: 2,
            fingerprint: Fingerprint {
                size: 2,
                modified_ns: 0,
            },
            source: Source::Memory(std::io::Cursor::new(vec![1, 2, 3])),
        };
        assert_eq!(opened.into_bytes(10).unwrap_err().code, "FILE_TOO_LARGE");
    }
}
