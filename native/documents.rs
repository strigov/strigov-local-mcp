use crate::{
    cursor::{self, Cursor, CursorKind, Fingerprint},
    filesystem::{
        self, OUTPUT_LIMIT, Result, RootDir, TEXT_READ_LIMIT, error, io_error, number, string,
    },
    pdf,
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
        matches!(self, Self::Text | Self::Pdf)
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
    // Contract for in-memory readers and future container members (C6).
    #[allow(dead_code)]
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
pub(crate) fn open(root: &RootDir, root_id: &str, raw: &str) -> Result<Opened> {
    let disk = filesystem::relative(raw)?;
    let real = root.resolve(raw)?;
    let name = filesystem::display(&real);
    let format = format_of(&name);
    let limit = match format {
        Format::Text => TEXT_READ_LIMIT,
        Format::Pdf => pdf::PDF_LIMIT,
        _ => u64::MAX,
    };
    let (file, fingerprint) = root.open_file(&real, limit)?;
    Ok(Opened {
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
}
impl PlainText {
    fn open(opened: Opened) -> Result<Self> {
        if opened.size > TEXT_READ_LIMIT {
            return Err(error("FILE_TOO_LARGE", "Text exceeds 64 MiB"));
        }
        let mut reader = BufReader::new(opened.source);
        let mut buf = Vec::new();
        let mut lines = 1;
        let mut bytes = 0u64;
        let mut bounded = (&mut reader).take(opened.size + 1);
        loop {
            buf.clear();
            let n = bounded.read_until(b'\n', &mut buf).map_err(io_error)?;
            bytes += n as u64;
            if bytes > opened.size {
                return Err(error("FILE_TOO_LARGE", "File grew beyond the read limit"));
            }
            std::str::from_utf8(&buf).map_err(|_| {
                error(
                    "NOT_TEXT",
                    "File is not valid UTF-8. Use read_document for document formats.",
                )
            })?;
            if n == 0 {
                break;
            }
            if buf.ends_with(b"\n") {
                lines += 1;
            }
        }
        reader.rewind().map_err(io_error)?;
        Ok(Self {
            reader,
            lines,
            next: 0,
            size: opened.size,
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
        let mut text =
            String::from_utf8(line).map_err(|_| error("NOT_TEXT", "File is not valid UTF-8"))?;
        if text.ends_with('\r') {
            text.pop();
        }
        Ok(Unit {
            locator: json!({"line":index+1}),
            heading: None,
            text,
        })
    }
    fn summary(&self) -> Value {
        json!({"size":self.size,"lines":self.lines})
    }
}
pub(crate) fn open_text_document(opened: Opened, args: &Value) -> Result<Box<dyn TextDocument>> {
    guarded(|| match opened.format {
        Format::Text => Ok(Box::new(PlainText::open(opened)?) as Box<dyn TextDocument>),
        Format::Pdf => Ok(Box::new(pdf::PdfText::open(
            &opened.into_bytes(pdf::PDF_LIMIT)?,
            args,
        )?) as Box<dyn TextDocument>),
        _ => Err(unsupported(opened.format, &opened.name)),
    })
}
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
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
    cursor::request_hash(&[
        tool,
        &location.root,
        &filesystem::display(&location.disk),
        &start,
        &end,
        "",
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
pub(crate) fn inspect_document(root: &RootDir, root_id: &str, args: &Value) -> Result<Value> {
    guarded(|| {
        let deadline = call_deadline();
        let opened = open(root, root_id, string(args, "path", None)?)?;
        let hash = sequence_hash("inspect_document", &opened.location, args);
        let resume = continuation(args, CursorKind::Inspect, hash, opened.fingerprint)?;
        let fingerprint = opened.fingerprint;
        let mut result = json!({"kind":"inspect","format":opened.format.name(),"root":root_id,"path":opened.location.display(),"id":opened.location.id()});
        let (summary, next) = match opened.format {
            Format::Text => {
                if resume.is_some() {
                    return Err(error(
                        "INVALID_CURSOR",
                        "Text inspection is complete in one call",
                    ));
                }
                (PlainText::open(opened)?.summary(), None)
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
            _ => return Err(unsupported(opened.format, &opened.name)),
        };
        result
            .as_object_mut()
            .unwrap()
            .extend(summary.as_object().unwrap().clone());
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
pub(crate) fn fetch(root: &RootDir, root_id: &str, raw_path: &str, id: &str) -> Result<Value> {
    let doc = read_document(
        root,
        root_id,
        &json!({"path":raw_path,"maxChars":DOC_DEFAULT_CHARS}),
    )?;
    let mut metadata = json!({"root":root_id,"path":doc["path"],"mimeType":filesystem::mime(Path::new(raw_path)),"format":doc["format"],"truncated":doc["truncated"]});
    if let Some(cursor) = doc.get("nextCursor") {
        metadata["nextCursor"] = cursor.clone();
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
        std::fs::write(tmp.path().join("bad.txt"), b"valid\n\xff").unwrap();
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
