use crate::config::{Config, Root};
use crate::{
    cursor::{self, Cursor, CursorKind, Fingerprint},
    doc_search,
    documents::{self, Format},
    walk::{WalkEvent, Walker},
};
use base64::Engine;
use cap_std::fs::{Dir, Metadata};
use chrono::{DateTime, Utc};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Seek},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

const TEXT_LIMIT: u64 = 5 * 1024 * 1024;
const BINARY_LIMIT: u64 = 15 * 1024 * 1024;
const SCAN_LIMIT: u64 = 128 * 1024 * 1024;
pub(crate) const OUTPUT_LIMIT: usize = 200_000;
const WALK_LIMIT: usize = 20_000;
pub(crate) const TEXT_READ_LIMIT: u64 = 64 * 1024 * 1024;
pub(crate) const SEARCH_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct FsError {
    pub code: &'static str,
    pub message: String,
}
pub(crate) type Result<T> = std::result::Result<T, FsError>;
impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for FsError {}
pub(crate) fn error(code: &'static str, message: impl Into<String>) -> FsError {
    FsError {
        code,
        message: message.into(),
    }
}
pub(crate) fn io_error(e: std::io::Error) -> FsError {
    let code = if e.kind() == std::io::ErrorKind::NotFound {
        "NOT_FOUND"
    } else {
        "ACCESS_ERROR"
    };
    // OS errors may contain host paths; expose only the kind.
    error(
        code,
        format!("Cannot access selected path ({:?})", e.kind()),
    )
}
pub(crate) fn blocked(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy().to_lowercase();
        matches!(
            s.as_str(),
            ".git"
                | ".ssh"
                | ".aws"
                | "node_modules"
                | ".ds_store"
                | ".env"
                | "id_rsa"
                | "id_ed25519"
        ) || s.starts_with(".env.")
            || [".key", ".p12", ".pfx", ".pem"]
                .iter()
                .any(|ext| s.ends_with(ext))
    })
}
pub(crate) fn relative(raw: &str) -> Result<PathBuf> {
    if raw.contains('\0') || raw.contains(':') {
        return Err(error(
            "INVALID_PATH",
            "Use a relative path without NUL or colon",
        ));
    }
    let normalized = raw.replace('\\', "/");
    let p = Path::new(&normalized);
    if p.is_absolute() {
        return Err(error("ABSOLUTE_PATH", "Absolute paths are not accepted"));
    }
    if p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    }) {
        return Err(error("PATH_ESCAPE", "Parent paths are not accepted"));
    }
    if blocked(p) {
        return Err(error(
            "BLOCKED_PATH",
            "Credential or service path is blocked",
        ));
    }
    Ok(if normalized.is_empty() {
        PathBuf::from(".")
    } else {
        p.into()
    })
}
pub(crate) fn display(path: &Path) -> String {
    path.components()
        .filter_map(|c| {
            if let Component::Normal(s) = c {
                Some(s.to_string_lossy())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}
pub(crate) fn public(root: &str, path: &Path) -> String {
    let p = display(path);
    if p.is_empty() {
        root.into()
    } else {
        format!("{root}/{p}")
    }
}
pub(crate) fn uri(root: &str, path: &Path) -> String {
    let encoded = display(path)
        .split('/')
        .map(|p| utf8_percent_encode(p, NON_ALPHANUMERIC).to_string())
        .collect::<Vec<_>>()
        .join("/");
    format!("mcp://local-files/{root}/{encoded}")
}
fn iso(t: std::io::Result<cap_std::time::SystemTime>) -> Option<String> {
    t.ok().map(|t| {
        DateTime::<Utc>::from(t.into_std()).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    })
}
pub(crate) fn is_text(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "c" | "cc"
            | "conf"
            | "cpp"
            | "css"
            | "csv"
            | "h"
            | "hpp"
            | "htm"
            | "html"
            | "ini"
            | "java"
            | "js"
            | "json"
            | "jsonl"
            | "jsx"
            | "log"
            | "md"
            | "mjs"
            | "py"
            | "rb"
            | "rst"
            | "sh"
            | "sql"
            | "svg"
            | "toml"
            | "ts"
            | "tsx"
            | "txt"
            | "xml"
            | "yaml"
            | "yml"
    )
}

pub(crate) fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "md" => "text/markdown",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "rtf" => "application/rtf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odt" => "application/vnd.oasis.opendocument.text",
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "tif" | "tiff" => "image/tiff",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}
pub(crate) fn string<'a>(v: &'a Value, key: &str, default: Option<&'a str>) -> Result<&'a str> {
    match v.get(key) {
        Some(Value::String(s)) => Ok(s),
        None => default.ok_or_else(|| error("INVALID_ARGUMENT", format!("Missing {key}"))),
        _ => Err(error("INVALID_ARGUMENT", format!("{key} must be a string"))),
    }
}
pub(crate) fn number(v: &Value, key: &str, default: usize, max: usize) -> Result<usize> {
    match v.get(key) {
        None => Ok(default),
        Some(x) => x
            .as_u64()
            .filter(|n| *n >= 1 && *n <= max as u64)
            .map(|n| n as usize)
            .ok_or_else(|| {
                error(
                    "INVALID_ARGUMENT",
                    format!("{key} must be between 1 and {max}"),
                )
            }),
    }
}
pub(crate) struct RootDir {
    pub(crate) dir: Dir,
}
impl RootDir {
    pub(crate) fn open(root: &Root) -> Result<Self> {
        let dir =
            Dir::open_ambient_dir(&root.path, cap_std::ambient_authority()).map_err(|_| {
                error(
                    "ROOT_UNAVAILABLE",
                    format!("Folder {} is unavailable", root.id),
                )
            })?;
        Ok(Self { dir })
    }
    pub(crate) fn resolve(&self, raw: &str) -> Result<PathBuf> {
        let path = relative(raw)?;
        // cap-std canonicalize opens the final entry on macOS, which can wait
        // forever on a FIFO. Resolve symlinks with metadata/read_link instead;
        // all subsequent opens remain relative to the capability directory.
        let mut pending: std::collections::VecDeque<_> = path
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect();
        let mut real = PathBuf::new();
        let mut links = 0;
        while let Some(part) = pending.pop_front() {
            if part == "." {
                continue;
            }
            if part == ".." {
                if !real.pop() {
                    return Err(error("SYMLINK_ESCAPE", "Path cannot escape its root"));
                }
                continue;
            }
            let next = real.join(part);
            if blocked(&next) {
                return Err(error("BLOCKED_PATH", "Resolved path is blocked"));
            }
            let metadata = self.dir.symlink_metadata(&next).map_err(io_error)?;
            if metadata.is_symlink() {
                links += 1;
                if links > 40 {
                    return Err(error("SYMLINK_LOOP", "Too many symbolic links"));
                }
                let target = self.dir.read_link(&next).map_err(io_error)?;
                if target.is_absolute()
                    || target
                        .components()
                        .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
                {
                    return Err(error(
                        "SYMLINK_ESCAPE",
                        "Absolute symbolic links are not accepted",
                    ));
                }
                for component in target.components().rev() {
                    pending.push_front(component.as_os_str().to_owned());
                }
            } else if metadata.is_file() || metadata.is_dir() {
                real = next;
            } else {
                return Err(error(
                    "NOT_FILE",
                    "Path must be a regular file or directory",
                ));
            }
        }
        Ok(if real.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            real
        })
    }
    pub(crate) fn open_file(
        &self,
        path: &Path,
        limit: u64,
    ) -> Result<(cap_std::fs::File, Fingerprint)> {
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = self.dir.open_with(path, &options).map_err(io_error)?;
        let meta = file.metadata().map_err(io_error)?;
        if !meta.is_file() {
            return Err(error("NOT_FILE", "Path must be a regular file"));
        }
        if meta.len() > limit {
            return Err(error(
                "FILE_TOO_LARGE",
                format!("File exceeds {limit} bytes"),
            ));
        }
        let modified_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.into_std().duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
            .unwrap_or(0);
        Ok((
            file,
            Fingerprint {
                size: meta.len(),
                modified_ns,
            },
        ))
    }
    fn read(&self, path: &Path, limit: u64, buffer: &mut Vec<u8>) -> Result<()> {
        let (file, fingerprint) = self.open_file(path, limit)?;
        buffer.clear();
        buffer.reserve_exact(fingerprint.size as usize + 1);
        file.take(fingerprint.size + 1)
            .read_to_end(buffer)
            .map_err(io_error)?;
        if buffer.len() as u64 > fingerprint.size {
            return Err(error("FILE_TOO_LARGE", "File grew beyond the read limit"));
        }
        Ok(())
    }
}

pub struct Filesystem {
    config: Config,
}
impl Filesystem {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
    fn root(&self, id: &str) -> Result<RootDir> {
        RootDir::open(
            self.config
                .roots
                .iter()
                .find(|r| r.id == id)
                .ok_or_else(|| error("UNKNOWN_ROOT", "Unknown folder alias"))?,
        )
    }
    fn canonical_root(&self, name: &str) -> Result<&str> {
        self.config
            .find_root(name)
            .map(|r| r.id.as_str())
            .map_err(|e| error("UNKNOWN_ROOT", e.to_string()))
    }
    pub fn call(&self, name: &str, args: &Value) -> Result<Value> {
        let mut normalized = args.clone();
        if let Some(root) = args
            .get("root")
            .and_then(Value::as_str)
            .filter(|r| *r != "all")
        {
            normalized["root"] = json!(self.canonical_root(root)?);
        }
        let args = &normalized;
        match name {
            "list_roots" => Ok(
                json!({"roots": self.config.roots.iter().map(|r| json!({"name":self.config.selector(r),"id":r.id,"nickname":r.display_name(),"path":r.id,"uri":uri(&r.id,Path::new("")),"readOnly":true,"available":r.path.is_dir()})).collect::<Vec<_>>()}),
            ),
            "get_file_info" => {
                let id = string(args, "root", None)?;
                let raw = string(args, "path", None)?;
                let root = self.root(id)?;
                let p = root.resolve(raw)?;
                let m = root.dir.metadata(&p).map_err(io_error)?;
                Ok(
                    json!({"root":id,"path":public(id,&relative(raw)?),"relativePath":display(&relative(raw)?),"type":kind(&m),"size":m.len(),"format":documents::format_of(&display(&p)).name(),"mimeType":if m.is_file(){Some(mime(&p))}else{None},"createdAt":iso(m.created()),"modifiedAt":iso(m.modified()),"uri":uri(id,&relative(raw)?)}),
                )
            }
            "read_text_file" | "read_file" => self.read(args, name == "read_file"),
            "read_document" | "inspect_document" => {
                let id = string(args, "root", None)?;
                let root = self.root(id)?;
                if name == "read_document" {
                    documents::read_document(&root, id, args)
                } else {
                    documents::inspect_document(&root, id, args)
                }
            }
            "list_directory" => self.list(args),
            "search_files" | "search_text" => self.search(args, name == "search_text"),
            "search" => self.connector_search(args),
            "fetch" => {
                let id = string(args, "id", None)?;
                let (root, path) = id
                    .split_once(':')
                    .ok_or_else(|| error("INVALID_ID", "Document id must use root:path"))?;
                let root = self.canonical_root(root)?;
                documents::fetch(&self.root(root)?, root, path, id)
            }
            _ => Err(error("UNKNOWN_TOOL", "Unknown tool")),
        }
    }
    fn read(&self, args: &Value, binary: bool) -> Result<Value> {
        let id = self.canonical_root(string(args, "root", None)?)?;
        let raw = string(args, "path", None)?;
        let root = self.root(id)?;
        let mut opened = documents::open(&root, id, raw)?;
        let p = PathBuf::from(&opened.name);
        let location_display = opened.location.display();
        let location_uri = opened.location.uri();
        let format = opened.format;
        if matches!(format, Format::Heic | Format::LegacyOffice)
            && (binary || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rtf")))
        {
            return Err(documents::unsupported(format, &display(&p)));
        }
        if binary && matches!(format, Format::Image(_) | Format::Pdf) {
            return documents::read_file(&root, id, args);
        }
        if binary && (args.get("page").is_some() || args.get("region").is_some()) {
            return Err(error(
                "INVALID_ARGUMENT",
                "page and region are only available for images and PDF pages",
            ));
        }
        if binary && format.has_text_reader() && format != Format::Text {
            return documents::read_document(&root, id, args);
        }
        let is_binary = binary && !is_text(&p);
        if is_binary {
            if opened.size > BINARY_LIMIT {
                return Err(error(
                    "FILE_TOO_LARGE",
                    "File exceeds the binary size limit",
                ));
            }
            let mut buf = Vec::with_capacity(opened.size as usize + 1);
            (&mut opened.source)
                .take(opened.size + 1)
                .read_to_end(&mut buf)
                .map_err(io_error)?;
            if buf.len() as u64 > opened.size {
                return Err(error("FILE_TOO_LARGE", "File grew beyond the read limit"));
            }
            if std::str::from_utf8(&buf).is_err() {
                return Ok(
                    json!({"kind":"binary","root":id,"path":location_display,"uri":location_uri,"mimeType":mime(&p),"size":buf.len(),"blob":base64::engine::general_purpose::STANDARD.encode(&buf)}),
                );
            }
            opened.source = documents::Source::Memory(std::io::Cursor::new(buf));
        }
        if opened.size
            > if is_binary {
                BINARY_LIMIT
            } else {
                TEXT_READ_LIMIT
            }
        {
            return Err(error("FILE_TOO_LARGE", "File exceeds the text size limit"));
        }
        let read_size = opened.size;
        let start = number(args, "startLine", 1, usize::MAX)?;
        let requested_end = number(args, "endLine", usize::MAX, usize::MAX)?;
        let max = number(args, "maxChars", OUTPUT_LIMIT, OUTPUT_LIMIT)?;
        let (encoding, _) =
            crate::text::scan(BufReader::new(&mut opened.source), read_size, is_text(&p))?;
        opened.source.rewind().map_err(io_error)?;
        let mut reader = BufReader::new(opened.source.take(read_size + 1));
        let mut out = String::new();
        let mut chars = 0;
        let mut truncated = false;
        let mut total = 0;
        let mut bytes = 0u64;
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line).map_err(io_error)?;
            bytes += n as u64;
            if bytes > read_size {
                return Err(error("FILE_TOO_LARGE", "File grew beyond the read limit"));
            }
            total += 1;
            let last = !line.ends_with(b"\n");
            if !last {
                line.pop();
            }
            if total >= start && total <= requested_end {
                let text = crate::text::decode(encoding, &line)?;
                if total > start {
                    if chars < max {
                        out.push('\n');
                        chars += 1;
                    } else {
                        truncated = true;
                    }
                }
                for c in text.strip_suffix('\r').unwrap_or(&text).chars() {
                    if chars < max {
                        out.push(c);
                        chars += 1;
                    } else {
                        truncated = true;
                        break;
                    }
                }
            }
            if last {
                break;
            }
        }
        let end = requested_end.min(total);
        if end < start {
            return Err(error(
                "INVALID_RANGE",
                "endLine must be greater than or equal to startLine",
            ));
        }
        Ok(
            json!({"kind":"text","root":id,"path":location_display,"uri":location_uri,"mimeType":mime(&p),"encoding":encoding.label(),"text":out,"startLine":start,"endLine":start+memchr::memchr_iter(b'\n',out.as_bytes()).count(),"totalLines":total,"truncated":truncated}),
        )
    }
    fn list(&self, args: &Value) -> Result<Value> {
        let id = string(args, "root", None)?;
        let raw = string(args, "path", Some(""))?;
        let root = self.root(id)?;
        if raw.contains("::") {
            return documents::list_container(&root, id, args);
        }
        let base = root.resolve(raw)?;
        if root.dir.metadata(&base).map_err(io_error)?.is_file() {
            return documents::list_container(&root, id, args);
        }
        let depth = number(args, "depth", 1, 5)?;
        let limit = number(args, "limit", 500, 2000)?;
        let logical = relative(raw)?;
        let depth_s = depth.to_string();
        let hash = cursor::request_hash(&["list_directory", id, &display(&logical), &depth_s]);
        let resume = walk_cursor(args, hash)?;
        if resume
            .as_ref()
            .is_some_and(|c| c.p.first().map(String::as_str) != Some(id))
        {
            return Err(error("INVALID_CURSOR", "Folder list changed; restart"));
        }
        let mut walker = Walker::new(
            &root,
            id,
            base,
            logical.clone(),
            depth,
            resume.map(|c| c.p[1..].to_vec()),
            false,
        )
        .peekable();
        let mut entries = Vec::new();
        let mut last = None;
        let mut visited = 0;
        let mut skipped_too_deep = 0;
        let deadline = Instant::now() + SEARCH_DEADLINE;
        for event in walker.by_ref() {
            match event {
                WalkEvent::Entry(e) => {
                    visited += 1;
                    skipped_too_deep += e.skipped_before;
                    let m = root.dir.symlink_metadata(&e.real).map_err(io_error)?;
                    entries.push(json!({"name":e.logical.file_name().unwrap_or_default().to_string_lossy(),"path":public(id,&e.logical),"relativePath":display(&e.logical),"type":kind(&m),"size":if m.is_file(){Some(m.len())}else{None},"modifiedAt":iso(m.modified())}));
                    debug_assert!(e.depth <= depth);
                    last = Some(walk_position(hash, id, e.components));
                }
                WalkEvent::TailSkipped(n) => {
                    skipped_too_deep += n;
                }
                WalkEvent::Inaccessible => {
                    return Err(error("ACCESS_ERROR", "Cannot list selected directory"));
                }
            }
            if entries.len() >= limit || visited >= WALK_LIMIT || Instant::now() >= deadline {
                break;
            }
        }
        let incomplete = walker.peek().is_some();
        let mut result = json!({"root":id,"path":public(id,&logical),"entries":entries,"truncated":incomplete,"skippedTooDeep":skipped_too_deep});
        if incomplete && let Some(c) = last {
            result["nextCursor"] = json!(cursor::encode(&c));
        }
        Ok(result)
    }

    fn search(&self, args: &Value, text_search: bool) -> Result<Value> {
        let query = string(args, "query", None)?;
        if query.chars().count() < if text_search { 2 } else { 1 } || query.len() > 4096 {
            return Err(error(
                "INVALID_ARGUMENT",
                "Query length is outside the allowed range",
            ));
        }
        let matcher = regex::RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .build()
            .map_err(|_| error("INVALID_ARGUMENT", "Invalid search query"))?;
        let limit = number(
            args,
            "limit",
            if text_search { 30 } else { 50 },
            if text_search { 100 } else { 200 },
        )?;
        let selected = string(args, "root", Some("all"))?;
        let raw = string(args, "path", Some(""))?;
        if text_search && selected != "all" {
            let root = self.root(selected)?;
            if raw.contains("::")
                || root
                    .dir
                    .metadata(root.resolve(raw)?)
                    .map_err(io_error)?
                    .is_file()
            {
                return self.search_document(args, &matcher, limit);
            }
        }
        let base = relative(raw)?;
        if selected != "all" && !self.config.roots.iter().any(|r| r.id == selected) {
            return Err(error("UNKNOWN_ROOT", "Unknown folder alias"));
        }
        let tool = if text_search {
            "search_text"
        } else {
            "search_files"
        };
        let hash = cursor::request_hash(&[tool, selected, &display(&base), query]);
        let resume = walk_cursor(args, hash)?;
        let resume_root = resume.as_ref().and_then(|c| c.p.first());
        let resume_index = match resume_root {
            Some(id) => Some(
                self.config
                    .roots
                    .iter()
                    .position(|r| r.id == *id && (selected == "all" || selected == id))
                    .ok_or_else(|| error("INVALID_CURSOR", "Folder list changed; restart"))?,
            ),
            None if resume.is_some() => {
                return Err(error("INVALID_CURSOR", "Invalid walk position"));
            }
            None => None,
        };
        let (doc_scan_limit, doc_file_limit) = (
            doc_search::DOC_SCAN_LIMIT,
            doc_search::SEARCH_DOC_FILE_LIMIT,
        );
        #[cfg(test)]
        let (doc_scan_limit, doc_file_limit) = doc_search::TEST_SCAN_BUDGET
            .with(|c| c.get().unwrap_or((doc_scan_limit, doc_file_limit)));
        let mut results = Vec::new();
        let mut unavailable = Vec::new();
        let mut scanned = 0u64;
        let mut doc_scanned = 0u64;
        let mut documents_searched = 0;
        let mut visited = 0;
        let mut skipped = 0;
        let mut skipped_too_deep = 0;
        let mut inaccessible = 0;
        let mut next = None;
        let mut buffer = Vec::new();
        let deadline = Instant::now() + SEARCH_DEADLINE;
        'roots: for (index, definition) in self.config.roots.iter().enumerate() {
            if selected != "all" && definition.id != selected {
                continue;
            }
            if resume_index.is_some_and(|r| index < r) {
                continue;
            }
            let root = match RootDir::open(definition) {
                Ok(r) => r,
                Err(e) => {
                    if selected != "all" {
                        return Err(e);
                    }
                    unavailable.push(definition.id.clone());
                    continue;
                }
            };
            let real = root.resolve(&display(&base))?;
            let after = if resume_index == Some(index) {
                resume.as_ref().map(|c| c.p[1..].to_vec())
            } else {
                None
            };
            let mut walker = Walker::new(
                &root,
                &definition.id,
                real,
                base.clone(),
                usize::MAX,
                after,
                text_search
                    && resume_index == Some(index)
                    && resume.as_ref().is_some_and(|c| c.f.is_some()),
            )
            .peekable();
            let mut last = if resume_index == Some(index) {
                resume.clone()
            } else {
                None
            };
            for event in walker.by_ref() {
                // A byte-budget interruption leaves this entry for the next call.
                // Cursor paths only compare names; they never open a path.
                if !matches!(event, WalkEvent::Inaccessible)
                    && visited > 0
                    && (visited >= WALK_LIMIT
                        || Instant::now() >= deadline
                        || results.len() >= limit)
                {
                    next = last.or_else(|| Some(walk_position(hash, &definition.id, vec![])));
                    break 'roots;
                }
                let entry = match event {
                    WalkEvent::Entry(e) => e,
                    WalkEvent::Inaccessible => {
                        inaccessible += 1;
                        continue;
                    }
                    WalkEvent::TailSkipped(n) => {
                        skipped_too_deep += n;
                        continue;
                    }
                };
                if text_search && entry.file_type.is_file() && is_text(&entry.real) {
                    let meta = match root.dir.metadata(&entry.real) {
                        Ok(m) => m,
                        Err(_) => {
                            inaccessible += 1;
                            skipped_too_deep += entry.skipped_before;
                            visited += 1;
                            last = Some(walk_position(hash, &definition.id, entry.components));
                            continue;
                        }
                    };
                    if meta.len() <= TEXT_LIMIT && scanned + meta.len() > SCAN_LIMIT && visited > 0
                    {
                        next = last.or_else(|| Some(walk_position(hash, &definition.id, vec![])));
                        break 'roots;
                    }
                    if meta.len() > TEXT_LIMIT {
                        skipped += 1;
                    } else if root.read(&entry.real, TEXT_LIMIT, &mut buffer).is_err() {
                        inaccessible += 1;
                    } else {
                        scanned += buffer.len() as u64;
                        match crate::text::decode_buffer(&buffer, true) {
                            Ok((text, _)) => {
                                if let Some(found) = matcher.find(&text) {
                                    results.push(json!({"root":definition.id,"path":public(&definition.id,&entry.logical),"relativePath":display(&entry.logical),"uri":uri(&definition.id,&entry.logical),"line":memchr::memchr_iter(b'\n',text[..found.start()].as_bytes()).count()+1,"snippet":crate::doc_search::text_snippet(&text, found)}));
                                }
                            }
                            Err(_) => inaccessible += 1,
                        }
                    }
                } else if text_search
                    && entry.file_type.is_file()
                    && documents::format_of(&display(&entry.real)).has_text_reader()
                {
                    let meta = match root.dir.metadata(&entry.real) {
                        Ok(m) => m,
                        Err(_) => {
                            inaccessible += 1;
                            skipped_too_deep += entry.skipped_before;
                            visited += 1;
                            last = Some(walk_position(hash, &definition.id, entry.components));
                            continue;
                        }
                    };
                    if meta.len() > doc_file_limit {
                        skipped += 1;
                    } else {
                        if doc_scanned + meta.len() > doc_scan_limit && visited > 0 {
                            next =
                                last.or_else(|| Some(walk_position(hash, &definition.id, vec![])));
                            break 'roots;
                        }
                        // Charge source bytes even when a parser rejects the document.
                        doc_scanned += meta.len();
                        let document =
                            documents::open(&root, &definition.id, &display(&entry.real));
                        let found = document.and_then(|opened| {
                            if opened.size > meta.len() {
                                return Err(error(
                                    "FILE_TOO_LARGE",
                                    "Document grew beyond the search budget",
                                ));
                            }
                            let fingerprint = opened.fingerprint;
                            let mut doc = documents::open_text_document(opened, args)?;
                            let current =
                                walk_position(hash, &definition.id, entry.components.clone());
                            let unfinished = resume
                                .as_ref()
                                .filter(|c| c.p == current.p && c.f == Some(fingerprint));
                            let start = unfinished
                                .map(|c| usize::try_from(c.u))
                                .transpose()
                                .map_err(|_| error("INVALID_CURSOR", "Invalid search unit"))?
                                .unwrap_or(0);
                            let offset = unfinished
                                .map(|c| usize::try_from(c.o))
                                .transpose()
                                .map_err(|_| error("INVALID_CURSOR", "Invalid search offset"))?
                                .unwrap_or(0);
                            documents_searched += 1;
                            doc_search::find_in_document(
                                doc.as_mut(),
                                &matcher,
                                start,
                                offset,
                                1,
                                deadline,
                            )
                            .map(|r| (fingerprint, r))
                        });
                        match found {
                            Ok((fingerprint, (hits, position))) => {
                                if let Some(hit) = hits.into_iter().next() {
                                    results.push(json!({"root":definition.id,"path":public(&definition.id,&entry.logical),"relativePath":display(&entry.logical),"uri":uri(&definition.id,&entry.logical),"locator":hit.locator,"snippet":hit.snippet}));
                                } else if let Some((u, o)) = position {
                                    let mut c =
                                        walk_position(hash, &definition.id, entry.components);
                                    c.f = Some(fingerprint);
                                    c.u = u as u64;
                                    c.o = o as u64;
                                    skipped_too_deep += entry.skipped_before;
                                    next = Some(c);
                                    break 'roots;
                                }
                            }
                            Err(e) if e.code == "INVALID_CURSOR" => return Err(e),
                            Err(_) => inaccessible += 1,
                        }
                    }
                } else if !text_search
                    && entry.file_type.is_file()
                    && matcher.is_match(&display(&entry.logical))
                {
                    match root.dir.metadata(&entry.real) {
                        Ok(meta) => results.push(json!({"root":definition.id,"path":public(&definition.id,&entry.logical),"relativePath":display(&entry.logical),"uri":uri(&definition.id,&entry.logical),"size":meta.len(),"modifiedAt":iso(meta.modified())})),
                        Err(_) => inaccessible += 1,
                    }
                }
                skipped_too_deep += entry.skipped_before;
                visited += 1;
                last = Some(walk_position(hash, &definition.id, entry.components));
            }
        }
        let mut output = json!({"query":query,"results":results,"scannedBytes":scanned + doc_scanned,"documentsSearched":documents_searched,"skippedLargeFiles":skipped,"skippedInaccessible":inaccessible,"skippedTooDeep":skipped_too_deep,"unavailableRoots":unavailable,"truncated":next.is_some()});
        if let Some(c) = next {
            output["nextCursor"] = json!(cursor::encode(&c));
        }
        Ok(output)
    }

    fn search_document(&self, args: &Value, matcher: &regex::Regex, limit: usize) -> Result<Value> {
        documents::guarded(|| {
            let id = string(args, "root", None)?;
            let query = string(args, "query", None)?;
            let root = self.root(id)?;
            let deadline = Instant::now() + SEARCH_DEADLINE;
            let opened = documents::open(&root, id, string(args, "path", None)?)?;
            let hash = cursor::request_hash(&["search_text", id, &opened.location.id(), query]);
            let resume = args
                .get("cursor")
                .map(|_| cursor::decode(string(args, "cursor", None)?, CursorKind::Find, hash))
                .transpose()?;
            if let Some(c) = &resume {
                cursor::check_fingerprint(c, opened.fingerprint)?;
            }
            let fingerprint = opened.fingerprint;
            let size = opened.size;
            let path = opened.location.display();
            let relative_path = path
                .strip_prefix(&format!("{id}/"))
                .unwrap_or(&path)
                .to_owned();
            let uri = opened.location.uri();
            let mut doc = documents::open_text_document(opened, args)?;
            let start = resume
                .as_ref()
                .map(|c| usize::try_from(c.u))
                .transpose()
                .map_err(|_| error("INVALID_CURSOR", "Invalid search unit"))?
                .unwrap_or(0);
            let offset = resume
                .as_ref()
                .map(|c| usize::try_from(c.o))
                .transpose()
                .map_err(|_| error("INVALID_CURSOR", "Invalid search offset"))?
                .unwrap_or(0);
            let (hits, next) = doc_search::find_in_document(
                doc.as_mut(),
                matcher,
                start,
                offset,
                limit,
                deadline,
            )?;
            let results: Vec<_> = hits.into_iter().map(|hit| json!({"root":id,"path":path,"relativePath":relative_path,"uri":uri,"locator":hit.locator,"snippet":hit.snippet})).collect();
            let mut result = json!({"query":query,"results":results,"documentsSearched":1,"scannedBytes":size,"skippedLargeFiles":0,"skippedInaccessible":0,"skippedTooDeep":0,"unavailableRoots":[],"truncated":next.is_some()});
            if let Some((u, o)) = next {
                result["nextCursor"] = json!(cursor::encode(&Cursor {
                    v: 1,
                    k: CursorKind::Find,
                    h: hash,
                    f: Some(fingerprint),
                    p: vec![],
                    u: u as u64,
                    o: o as u64
                }));
            }
            Ok(result)
        })
    }
    fn connector_search(&self, args: &Value) -> Result<Value> {
        let query = string(args, "query", None)?;
        let names = self.search(&json!({"query":query,"limit":20}), false)?;
        let texts = if names["results"].as_array().unwrap().len() < 20 && query.chars().count() >= 2
        {
            self.search(&json!({"query":query,"limit":20}), true)?
        } else {
            json!({"results":[],"unavailableRoots":[],"truncated":false})
        };
        let mut matches = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut add = |item: &Value| {
            let id = format!(
                "{}:{}",
                item["root"].as_str().unwrap_or(""),
                item["relativePath"].as_str().unwrap_or("")
            );
            if seen.insert(id.clone()) {
                matches.push(json!({"id":id,"title":Path::new(item["relativePath"].as_str().unwrap_or("")).file_name().unwrap_or_default().to_string_lossy(),"text":item.get("snippet").unwrap_or(&item["path"]),"url":item["uri"]}));
            }
        };
        for item in names["results"].as_array().unwrap() {
            add(item);
        }
        for item in texts["results"].as_array().unwrap() {
            add(item);
        }
        let truncated =
            matches.len() > 20 || names["truncated"] == true || texts["truncated"] == true;
        matches.truncate(20);
        let mut unavailable = names["unavailableRoots"].as_array().unwrap().clone();
        for root in texts["unavailableRoots"].as_array().unwrap() {
            if !unavailable.contains(root) {
                unavailable.push(root.clone());
            }
        }
        Ok(
            json!({"results":matches,"unavailableRoots":unavailable,"truncated":truncated,
            "scannedBytes":texts["scannedBytes"].as_u64().unwrap_or(0),
            "skippedLargeFiles":texts["skippedLargeFiles"].as_u64().unwrap_or(0),
            "skippedInaccessible":names["skippedInaccessible"].as_u64().unwrap_or(0)+texts["skippedInaccessible"].as_u64().unwrap_or(0)}),
        )
    }
}
fn kind(meta: &Metadata) -> &'static str {
    if meta.is_symlink() {
        "symlink"
    } else if meta.is_file() {
        "file"
    } else if meta.is_dir() {
        "directory"
    } else {
        "other"
    }
}

fn walk_cursor(args: &Value, hash: u64) -> Result<Option<Cursor>> {
    args.get("cursor")
        .map(|_| cursor::decode(string(args, "cursor", None)?, CursorKind::Walk, hash))
        .transpose()
}
fn walk_position(hash: u64, id: &str, components: Vec<String>) -> Cursor {
    let mut p = vec![id.to_owned()];
    p.extend(components);
    Cursor {
        v: 1,
        k: CursorKind::Walk,
        h: hash,
        f: None,
        p,
        u: 0,
        o: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    const TEXT: &str = "Постановление Верховного Суда РФ\r\nЗамещающие сделки: взыскание убытков.\r\nОпределение от 03.03.2023 N 305-ЭС22-2377\r\n";
    const TYPO: &str = "«Определение Судебной коллегии» — дело № 305-ЭС22-2377…\r\nУчёт: ЁМКОСТЬ, ёмкость; „цитата“ – § 3,\u{00a0}стр. 2\r\nЁлкин А.Ё. и ёлкин а.ё.\r\n";

    fn cp1251_encode(s: &str) -> Vec<u8> {
        let (bytes, _, unmappable) = encoding_rs::WINDOWS_1251.encode(s);
        assert!(!unmappable, "CP1251 encode failed for: {s}");
        bytes.into_owned()
    }

    fn new_fs(tmp: &tempfile::TempDir) -> Filesystem {
        Filesystem::new(Config {
            version: 1,
            roots: vec![Root {
                id: "work".into(),
                path: tmp.path().into(),
                nickname: None,
            }],
            next_root_id: 1,
        })
    }

    fn call_ok(fs: &Filesystem, tool: &str, args: &Value) -> Value {
        let r = fs.call(tool, args);
        assert!(r.is_ok(), "{tool} {args}: {:?}", r.as_ref().err().unwrap());
        r.unwrap()
    }

    fn call_err(fs: &Filesystem, tool: &str, args: &Value) -> FsError {
        let r = fs.call(tool, args);
        assert!(r.is_err(), "{tool} {args}: expected error, got {r:?}");
        r.unwrap_err()
    }

    fn depth_cap_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
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

    fn write_main_fixtures(dir: &Path) {
        let cp1251_bytes = cp1251_encode(TEXT);
        assert_eq!(cp1251_bytes.len(), 116);
        std::fs::write(dir.join("cp1251.txt"), &cp1251_bytes).unwrap();

        let utf8_bytes = TEXT.as_bytes();
        assert_eq!(utf8_bytes.len(), 192);
        std::fs::write(dir.join("utf8.txt"), utf8_bytes).unwrap();

        let mut utf8_bom = vec![0xef, 0xbb, 0xbf];
        utf8_bom.extend_from_slice(utf8_bytes);
        assert_eq!(utf8_bom.len(), 195);
        std::fs::write(dir.join("utf8-bom.txt"), &utf8_bom).unwrap();

        let typo_bytes = cp1251_encode(TYPO);
        std::fs::write(dir.join("cp1251-typography.txt"), &typo_bytes).unwrap();

        std::fs::write(dir.join("utf8-also-cp1251.txt"), [0xc2, 0xbb]).unwrap();

        let mut bom_cp1251 = vec![0xef, 0xbb, 0xbf];
        bom_cp1251.extend_from_slice(&cp1251_bytes);
        std::fs::write(dir.join("bom-cp1251.txt"), &bom_cp1251).unwrap();

        let crlf_idx = utf8_bytes.windows(2).position(|w| w == b"\r\n").unwrap() + 2;
        let mut damaged = Vec::new();
        damaged.extend_from_slice(&utf8_bytes[..crlf_idx]);
        damaged.push(0xff);
        damaged.extend_from_slice(&utf8_bytes[crlf_idx..]);
        assert_eq!(damaged.len(), 193);
        std::fs::write(dir.join("damaged-utf8.txt"), &damaged).unwrap();

        let nbsp_bytes: [u8; 20] = [
            0xC2, 0xA0, 0x32, 0x30, 0x32, 0x34, 0x2C, 0x20, 0xC2, 0xA0, 0x32, 0x30, 0x32, 0x35,
            0x20, 0x97, 0x20, 0x31, 0x30, 0x30,
        ];
        std::fs::write(dir.join("cp1251-nbsp.txt"), nbsp_bytes).unwrap();

        let binary_bytes: Vec<u8> = (0..=255u8).collect();
        std::fs::write(dir.join("binary.txt"), &binary_bytes).unwrap();
        std::fs::write(dir.join("binary.bin"), &binary_bytes).unwrap();

        std::fs::write(dir.join("cp1251.dat"), &cp1251_bytes).unwrap();
    }

    fn write_controls_fixtures(dir: &Path) {
        let cp1251_bytes = cp1251_encode(TEXT);
        let mut nul = cp1251_bytes.clone();
        nul.insert(10, 0x00);
        std::fs::write(dir.join("nul-control.txt"), &nul).unwrap();

        let mut x98 = cp1251_bytes.clone();
        x98.insert(10, 0x98);
        std::fs::write(dir.join("x98.txt"), &x98).unwrap();

        let mut sub = cp1251_bytes;
        sub.push(0x1a);
        std::fs::write(dir.join("sub.txt"), &sub).unwrap();

        let mut utf16 = vec![0xff, 0xfe];
        for c in TEXT.encode_utf16() {
            utf16.extend_from_slice(&c.to_le_bytes());
        }
        std::fs::write(dir.join("utf16.txt"), &utf16).unwrap();
    }

    fn write_ambiguous_fixtures(dir: &Path) {
        std::fs::write(dir.join("truncated-utf8.txt"), b"text: \xd1").unwrap();
        std::fs::write(
            dir.join("ambiguous-bytes.txt"),
            [0xb8, 0xff, 0xff, 0xff, 0xff, 0xc3],
        )
        .unwrap();
        std::fs::write(dir.join("utf16-no-ascii.txt"), [0xff, 0xfe, 0xac, 0x20]).unwrap();
        std::fs::write(dir.join("utf8-nul.txt"), b"A\x00B").unwrap();
    }

    fn write_large_fixtures(dir: &Path) {
        let chunk = cp1251_encode("Большой документ ОГРОМНЫЙ\r\n");
        let target_len = 5_242_881;
        let reps = (target_len / chunk.len()) + 1;
        let mut large = chunk.repeat(reps);
        large.truncate(target_len);
        assert_eq!(large.len(), target_len);
        std::fs::write(dir.join("large-cp1251.txt"), &large).unwrap();
    }

    fn write_huge_fixtures(dir: &Path) {
        let f = File::create(dir.join("huge.txt")).unwrap();
        f.set_len(67_108_865).unwrap();
    }

    fn write_zip_fixtures(dir: &Path) {
        let cp1251_bytes = cp1251_encode(TEXT);
        let zip_bytes = depth_cap_zip(&[("cp1251.txt", &cp1251_bytes)]);
        std::fs::write(dir.join("archive.zip"), &zip_bytes).unwrap();
    }

    #[test]
    fn cp1251_text_reads_match_utf8_in_every_tool() {
        let tmp = tempfile::tempdir().unwrap();
        write_main_fixtures(tmp.path());
        let fs = new_fs(&tmp);
        let expected_text = TEXT.replace("\r\n", "\n");

        let cp_read = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "cp1251.txt"}),
        );
        let utf8_read = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "utf8.txt"}),
        );
        assert_eq!(cp_read["text"], expected_text);
        assert_eq!(utf8_read["text"], expected_text);
        assert_eq!(cp_read["totalLines"], 4);
        assert_eq!(cp_read["endLine"], 4);
        assert_eq!(utf8_read["totalLines"], 4);
        assert_eq!(utf8_read["endLine"], 4);
        assert_eq!(cp_read["encoding"], "windows-1251");
        assert_eq!(utf8_read["encoding"], "utf-8");

        let cp_file = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "cp1251.txt"}),
        );
        assert_eq!(cp_file["kind"], "text");
        assert_eq!(cp_file["mimeType"], "text/plain");
        assert_eq!(cp_file["encoding"], "windows-1251");
        assert_eq!(cp_file["text"], expected_text);
        assert!(cp_file.get("blob").is_none());

        let utf8_file = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "utf8.txt"}),
        );
        assert_eq!(utf8_file["encoding"], "utf-8");

        let cp_doc = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "cp1251.txt"}),
        );
        let utf8_doc = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "utf8.txt"}),
        );
        assert_eq!(cp_doc["format"], "text");
        assert_eq!(cp_doc["encoding"], "windows-1251");
        assert_eq!(cp_doc["size"], 116);
        assert_eq!(cp_doc["lines"], 4);
        assert_eq!(cp_doc["totalUnits"], 4);
        assert_eq!(utf8_doc["encoding"], "utf-8");

        let cp_doc_text = cp_doc["text"].as_str().unwrap();
        let utf8_doc_text = utf8_doc["text"].as_str().unwrap();
        let cp_doc_body = &cp_doc_text[cp_doc_text.find('\n').unwrap() + 1..];
        let utf8_doc_body = &utf8_doc_text[utf8_doc_text.find('\n').unwrap() + 1..];
        assert_eq!(cp_doc_body, utf8_doc_body);
        assert_eq!(cp_doc_body, expected_text);

        let cp_fetch = call_ok(&fs, "fetch", &json!({"id": "work:cp1251.txt"}));
        let utf8_fetch = call_ok(&fs, "fetch", &json!({"id": "work:utf8.txt"}));
        assert_eq!(cp_fetch["metadata"]["format"], "text");
        assert_eq!(cp_fetch["metadata"]["encoding"], "windows-1251");
        assert_eq!(utf8_fetch["metadata"]["encoding"], "utf-8");

        let cp_fetch_text = cp_fetch["text"].as_str().unwrap();
        let utf8_fetch_text = utf8_fetch["text"].as_str().unwrap();
        let cp_fetch_body = &cp_fetch_text[cp_fetch_text.find('\n').unwrap() + 1..];
        let utf8_fetch_body = &utf8_fetch_text[utf8_fetch_text.find('\n').unwrap() + 1..];
        assert_eq!(cp_fetch_body, utf8_fetch_body);

        let search = call_ok(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "cp1251.txt", "query": "Замещающие"}),
        );
        let results = search["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["locator"]["line"], 2);
        assert_eq!(
            results[0]["snippet"],
            "Замещающие сделки: взыскание убытков."
        );

        let cp_inspect = call_ok(
            &fs,
            "inspect_document",
            &json!({"root": "work", "path": "cp1251.txt"}),
        );
        assert_eq!(cp_inspect["format"], "text");
        assert_eq!(cp_inspect["size"], 116);
        assert_eq!(cp_inspect["lines"], 4);
        assert_eq!(cp_inspect["encoding"], "windows-1251");

        let utf8_inspect = call_ok(
            &fs,
            "inspect_document",
            &json!({"root": "work", "path": "utf8.txt"}),
        );
        assert_eq!(utf8_inspect["encoding"], "utf-8");

        let b_read = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "utf8-bom.txt"}),
        );
        assert_eq!(b_read["encoding"], "utf-8");
        let b_file = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "utf8-bom.txt"}),
        );
        assert_eq!(b_file["encoding"], "utf-8");
        let b_doc = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "utf8-bom.txt"}),
        );
        assert_eq!(b_doc["encoding"], "utf-8");
        let b_inspect = call_ok(
            &fs,
            "inspect_document",
            &json!({"root": "work", "path": "utf8-bom.txt"}),
        );
        assert_eq!(b_inspect["encoding"], "utf-8");
        let b_fetch = call_ok(&fs, "fetch", &json!({"id": "work:utf8-bom.txt"}));
        assert_eq!(b_fetch["metadata"]["encoding"], "utf-8");

        let a_read = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "utf8-also-cp1251.txt"}),
        );
        assert_eq!(a_read["encoding"], "utf-8");
        let a_file = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "utf8-also-cp1251.txt"}),
        );
        assert_eq!(a_file["encoding"], "utf-8");
        let a_doc = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "utf8-also-cp1251.txt"}),
        );
        assert_eq!(a_doc["encoding"], "utf-8");
        let a_inspect = call_ok(
            &fs,
            "inspect_document",
            &json!({"root": "work", "path": "utf8-also-cp1251.txt"}),
        );
        assert_eq!(a_inspect["encoding"], "utf-8");
        let a_fetch = call_ok(&fs, "fetch", &json!({"id": "work:utf8-also-cp1251.txt"}));
        assert_eq!(a_fetch["metadata"]["encoding"], "utf-8");
    }

    #[test]
    fn cp1251_ranges_max_chars_and_cursors() {
        let tmp = tempfile::tempdir().unwrap();
        write_main_fixtures(tmp.path());
        let fs = new_fs(&tmp);
        let expected_text = TEXT.replace("\r\n", "\n");

        let r1 = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "cp1251.txt", "startLine": 2, "endLine": 2}),
        );
        assert_eq!(r1["text"], "Замещающие сделки: взыскание убытков.");
        assert_eq!(r1["startLine"], 2);
        assert_eq!(r1["endLine"], 2);

        let r2 = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "cp1251.txt", "maxChars": 5}),
        );
        assert_eq!(r2["text"], "Поста");
        assert_eq!(r2["truncated"], true);

        let err = call_err(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "cp1251.txt", "startLine": 3, "endLine": 2}),
        );
        assert_eq!(err.code, "INVALID_RANGE");

        let mut doc_args = json!({"root": "work", "path": "cp1251.txt", "maxChars": 1});
        let mut first_cursor: Option<String> = None;
        let mut recovered_lines: std::collections::BTreeMap<u64, String> =
            std::collections::BTreeMap::new();
        loop {
            let res = call_ok(&fs, "read_document", &doc_args);
            if first_cursor.is_none() {
                first_cursor = res
                    .get("nextCursor")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string());
            }
            let t = res["text"].as_str().unwrap();
            let body = &t[t.find('\n').unwrap() + 1..];
            let units = res["units"].as_array().unwrap();
            let mut char_offset = 0;
            for u in units {
                let line_num = u["line"].as_u64().unwrap();
                let chars_count = u["chars"].as_u64().unwrap() as usize;
                let chunk_chars: String =
                    body.chars().skip(char_offset).take(chars_count).collect();
                char_offset += chars_count;
                recovered_lines
                    .entry(line_num)
                    .or_default()
                    .push_str(&chunk_chars);
            }
            if let Some(cursor) = res.get("nextCursor").and_then(|c| c.as_str()) {
                doc_args["cursor"] = json!(cursor);
            } else {
                break;
            }
        }
        let original_lines: Vec<&str> = expected_text.split('\n').collect();
        assert_eq!(recovered_lines.len(), original_lines.len());
        for (idx, orig_line) in original_lines.iter().enumerate() {
            let line_num = (idx + 1) as u64;
            assert_eq!(
                recovered_lines
                    .get(&line_num)
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                *orig_line
            );
        }
        let reconstructed_text = (1..=original_lines.len())
            .map(|i| {
                recovered_lines
                    .get(&(i as u64))
                    .map(|s| s.as_str())
                    .unwrap_or("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(reconstructed_text, expected_text);

        std::fs::write(tmp.path().join("cp1251.txt"), b"different length content").unwrap();
        let stale_err = call_err(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "cp1251.txt", "cursor": first_cursor.unwrap()}),
        );
        assert_eq!(stale_err.code, "STALE_CURSOR");

        let mut search_args = json!({
            "root": "work",
            "path": "cp1251-typography.txt",
            "query": "ёмкость",
            "limit": 1
        });
        let mut snippets = Vec::new();
        let mut first = true;
        loop {
            let res = call_ok(&fs, "search_text", &search_args);
            if first {
                assert_eq!(res["results"].as_array().unwrap().len(), 1);
                assert!(res.get("nextCursor").and_then(|c| c.as_str()).is_some());
                first = false;
            }
            if let Some(arr) = res["results"].as_array() {
                for m in arr {
                    snippets.push(m["snippet"].as_str().unwrap().to_string());
                }
            }
            if let Some(cursor) = res.get("nextCursor").and_then(|c| c.as_str()) {
                search_args["cursor"] = json!(cursor);
            } else {
                assert_eq!(res["truncated"], false);
                break;
            }
        }
        assert_eq!(snippets.len(), 2);
        assert!(snippets[0].contains("ЁМКОСТЬ"));
        assert!(snippets[1].contains("ёмкость"));
    }

    #[test]
    fn cp1251_typography_and_yo() {
        let tmp = tempfile::tempdir().unwrap();
        write_main_fixtures(tmp.path());
        let fs = new_fs(&tmp);
        let expected_typo = TYPO.replace("\r\n", "\n");

        let r = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "cp1251-typography.txt"}),
        );
        assert_eq!(r["text"], expected_typo);
        assert_eq!(r["encoding"], "windows-1251");
        for ch in [
            '«', '»', '—', '–', '…', '№', '„', '“', '§', '\u{00a0}', 'Ё', 'ё',
        ] {
            assert!(r["text"].as_str().unwrap().contains(ch));
        }

        let doc = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "cp1251-typography.txt"}),
        );
        assert_eq!(doc["encoding"], "windows-1251");
        let doc_text = doc["text"].as_str().unwrap();
        let doc_body = &doc_text[doc_text.find('\n').unwrap() + 1..];
        assert_eq!(doc_body, expected_typo);
        for ch in [
            '«', '»', '—', '–', '…', '№', '„', '“', '§', '\u{00a0}', 'Ё', 'ё',
        ] {
            assert!(doc_body.contains(ch));
        }

        let fetch = call_ok(&fs, "fetch", &json!({"id": "work:cp1251-typography.txt"}));
        assert_eq!(fetch["metadata"]["encoding"], "windows-1251");
        let fetch_text = fetch["text"].as_str().unwrap();
        let fetch_body = &fetch_text[fetch_text.find('\n').unwrap() + 1..];
        assert_eq!(fetch_body, expected_typo);
        for ch in [
            '«', '»', '—', '–', '…', '№', '„', '“', '§', '\u{00a0}', 'Ё', 'ё',
        ] {
            assert!(fetch_body.contains(ch));
        }

        let search = call_ok(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "cp1251-typography.txt", "query": "ЁЛКИН"}),
        );
        let results = search["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["locator"]["line"], 3);
        assert_eq!(results[1]["locator"]["line"], 3);
    }

    #[test]
    fn cp1251_folder_search_counts_undecodable() {
        let tmp = tempfile::tempdir().unwrap();
        write_main_fixtures(tmp.path());
        let fs = new_fs(&tmp);

        let search = call_ok(
            &fs,
            "search_text",
            &json!({"root": "work", "query": "Замещающие"}),
        );
        let results = search["results"].as_array().unwrap();
        let rel_paths: Vec<&str> = results
            .iter()
            .map(|r| r["relativePath"].as_str().unwrap())
            .collect();

        assert!(rel_paths.contains(&"cp1251.txt"));
        assert!(rel_paths.contains(&"utf8.txt"));
        assert!(rel_paths.contains(&"utf8-bom.txt"));

        assert!(!rel_paths.contains(&"bom-cp1251.txt"));
        assert!(!rel_paths.contains(&"damaged-utf8.txt"));
        assert!(!rel_paths.contains(&"binary.txt"));

        let cp_match = results
            .iter()
            .find(|r| r["relativePath"] == "cp1251.txt")
            .unwrap();
        assert_eq!(cp_match["line"], 2);
        assert!(
            cp_match["snippet"]
                .as_str()
                .unwrap()
                .contains("Замещающие сделки")
        );

        assert_eq!(search["skippedInaccessible"], 2);
        assert_eq!(search["skippedLargeFiles"], 0);

        let connector = call_ok(&fs, "search", &json!({"query": "Замещающие"}));
        let conn_results = connector["results"].as_array().unwrap();
        assert!(conn_results.iter().any(|r| r["id"] == "work:cp1251.txt"));

        let fetch = call_ok(&fs, "fetch", &json!({"id": "work:cp1251.txt"}));
        let text = fetch["text"].as_str().unwrap();
        assert!(text.contains("Замещающие сделки"));
    }

    #[test]
    fn cp1251_zip_member_and_limits() {
        let zip_tmp = tempfile::tempdir().unwrap();
        write_zip_fixtures(zip_tmp.path());
        let zip_fs = new_fs(&zip_tmp);

        let zip_doc = call_ok(
            &zip_fs,
            "read_document",
            &json!({"root": "work", "path": "archive.zip::cp1251.txt"}),
        );
        assert_eq!(zip_doc["format"], "text");
        assert_eq!(zip_doc["encoding"], "windows-1251");
        let expected_text = TEXT.replace("\r\n", "\n");
        let t = zip_doc["text"].as_str().unwrap();
        let body = &t[t.find('\n').unwrap() + 1..];
        assert_eq!(body, expected_text);

        let large_tmp = tempfile::tempdir().unwrap();
        write_large_fixtures(large_tmp.path());
        let large_fs = new_fs(&large_tmp);

        let search = call_ok(
            &large_fs,
            "search_text",
            &json!({"root": "work", "query": "ОГРОМНЫЙ"}),
        );
        assert_eq!(search["skippedLargeFiles"], 1);
        let results = search["results"].as_array().unwrap();
        assert!(
            !results
                .iter()
                .any(|r| r["relativePath"] == "large-cp1251.txt")
        );

        let read_large = call_ok(
            &large_fs,
            "read_text_file",
            &json!({"root": "work", "path": "large-cp1251.txt", "maxChars": 1000}),
        );
        let s = read_large["text"].as_str().unwrap();
        assert_eq!(s.chars().count(), 1000);
        assert_eq!(read_large["truncated"], true);
        assert_eq!(read_large["encoding"], "windows-1251");

        let huge_tmp = tempfile::tempdir().unwrap();
        write_huge_fixtures(huge_tmp.path());
        let huge_fs = new_fs(&huge_tmp);

        let err1 = call_err(
            &huge_fs,
            "read_text_file",
            &json!({"root": "work", "path": "huge.txt"}),
        );
        assert_eq!(err1.code, "FILE_TOO_LARGE");

        let err2 = call_err(
            &huge_fs,
            "read_document",
            &json!({"root": "work", "path": "huge.txt"}),
        );
        assert_eq!(err2.code, "FILE_TOO_LARGE");

        let err3 = call_err(
            &large_fs,
            "read_text_file",
            &json!({"root": "work", "path": "large-cp1251.txt", "maxChars": 200_001}),
        );
        assert_eq!(err3.code, "INVALID_ARGUMENT");
    }

    #[test]
    fn cp1251_damaged_and_binary_messages() {
        let tmp = tempfile::tempdir().unwrap();
        write_main_fixtures(tmp.path());
        let fs = new_fs(&tmp);

        for tool in [
            "read_text_file",
            "read_file",
            "read_document",
            "inspect_document",
        ] {
            let err = call_err(
                &fs,
                tool,
                &json!({"root": "work", "path": "bom-cp1251.txt"}),
            );
            assert_eq!(err.code, "NOT_TEXT", "{tool}");
            assert_eq!(err.message, "File looks like damaged UTF-8", "{tool}");
        }
        let fetch_err = call_err(&fs, "fetch", &json!({"id": "work:bom-cp1251.txt"}));
        assert_eq!(fetch_err.code, "NOT_TEXT");
        assert_eq!(fetch_err.message, "File looks like damaged UTF-8");

        let search_err = call_err(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "bom-cp1251.txt", "query": "Замещающие"}),
        );
        assert_eq!(search_err.code, "NOT_TEXT");
        assert_eq!(search_err.message, "File looks like damaged UTF-8");

        let ctrl_tmp = tempfile::tempdir().unwrap();
        write_controls_fixtures(ctrl_tmp.path());
        let ctrl_fs = new_fs(&ctrl_tmp);

        for name in ["nul-control.txt", "x98.txt", "sub.txt", "utf16.txt"] {
            for tool in [
                "read_text_file",
                "read_file",
                "read_document",
                "inspect_document",
            ] {
                let err = call_err(&ctrl_fs, tool, &json!({"root": "work", "path": name}));
                assert_eq!(err.code, "NOT_TEXT", "{tool} {name}");
                assert_eq!(
                    err.message, "File is neither UTF-8 nor Windows-1251 text",
                    "{tool} {name}"
                );
            }
            let f_err = call_err(&ctrl_fs, "fetch", &json!({"id": format!("work:{name}")}));
            assert_eq!(f_err.code, "NOT_TEXT", "fetch {name}");
            assert_eq!(
                f_err.message, "File is neither UTF-8 nor Windows-1251 text",
                "fetch {name}"
            );

            let s_err = call_err(
                &ctrl_fs,
                "search_text",
                &json!({"root": "work", "path": name, "query": "Замещающие"}),
            );
            assert_eq!(s_err.code, "NOT_TEXT", "search_text {name}");
            assert_eq!(
                s_err.message, "File is neither UTF-8 nor Windows-1251 text",
                "search_text {name}"
            );
        }

        for tool in [
            "read_text_file",
            "read_file",
            "read_document",
            "inspect_document",
        ] {
            let err = call_err(&fs, tool, &json!({"root": "work", "path": "binary.txt"}));
            assert_eq!(err.code, "NOT_TEXT", "{tool} binary.txt");
            assert_eq!(
                err.message, "File is neither UTF-8 nor Windows-1251 text",
                "{tool} binary.txt"
            );
        }
        let b_fetch = call_err(&fs, "fetch", &json!({"id": "work:binary.txt"}));
        assert_eq!(b_fetch.code, "NOT_TEXT");
        assert_eq!(
            b_fetch.message,
            "File is neither UTF-8 nor Windows-1251 text"
        );

        let b_search = call_err(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "binary.txt", "query": "Замещающие"}),
        );
        assert_eq!(b_search.code, "NOT_TEXT");
        assert_eq!(
            b_search.message,
            "File is neither UTF-8 nor Windows-1251 text"
        );
    }

    #[test]
    fn cp1251_nbsp_and_ambiguous_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        write_main_fixtures(tmp.path());
        let fs = new_fs(&tmp);

        let r1 = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "cp1251-nbsp.txt"}),
        );
        assert_eq!(r1["text"], "В\u{00a0}2024, В\u{00a0}2025 — 100");
        assert_eq!(r1["encoding"], "windows-1251");
        assert_eq!(r1["totalLines"], 1);

        let r2 = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "cp1251-nbsp.txt"}),
        );
        assert_eq!(r2["text"], "В\u{00a0}2024, В\u{00a0}2025 — 100");
        assert_eq!(r2["encoding"], "windows-1251");
        assert_eq!(r2["kind"], "text");

        let doc = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "cp1251-nbsp.txt"}),
        );
        assert_eq!(doc["format"], "text");
        assert_eq!(doc["encoding"], "windows-1251");
        assert_eq!(doc["size"], 20);
        assert_eq!(doc["lines"], 1);
        let doc_text = doc["text"].as_str().unwrap();
        let doc_body = &doc_text[doc_text.find('\n').unwrap() + 1..];
        assert_eq!(doc_body, "В\u{00a0}2024, В\u{00a0}2025 — 100");

        let inspect = call_ok(
            &fs,
            "inspect_document",
            &json!({"root": "work", "path": "cp1251-nbsp.txt"}),
        );
        assert_eq!(inspect["format"], "text");
        assert_eq!(inspect["encoding"], "windows-1251");
        assert_eq!(inspect["size"], 20);
        assert_eq!(inspect["lines"], 1);

        let fetch = call_ok(&fs, "fetch", &json!({"id": "work:cp1251-nbsp.txt"}));
        assert_eq!(fetch["metadata"]["encoding"], "windows-1251");
        let fetch_text = fetch["text"].as_str().unwrap();
        let fetch_body = &fetch_text[fetch_text.find('\n').unwrap() + 1..];
        assert_eq!(fetch_body, "В\u{00a0}2024, В\u{00a0}2025 — 100");

        let search = call_ok(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "cp1251-nbsp.txt", "query": "2025 — 100"}),
        );
        let res = search["results"].as_array().unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0]["locator"]["line"], 1);

        let folder = call_ok(
            &fs,
            "search_text",
            &json!({"root": "work", "query": "2025 — 100"}),
        );
        let f_res = folder["results"].as_array().unwrap();
        assert_eq!(f_res.len(), 1);
        assert_eq!(f_res[0]["relativePath"], "cp1251-nbsp.txt");
        assert_eq!(f_res[0]["line"], 1);

        let conn = call_ok(&fs, "search", &json!({"query": "2025 — 100"}));
        let c_res = conn["results"].as_array().unwrap();
        assert!(c_res.iter().any(|r| r["id"] == "work:cp1251-nbsp.txt"));

        let damaged_bytes = std::fs::read(tmp.path().join("damaged-utf8.txt")).unwrap();
        let (damaged_decoded, _, _) = encoding_rs::WINDOWS_1251.decode(&damaged_bytes);
        let expected_damaged = damaged_decoded.replace("\r\n", "\n");

        let damaged_read = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "damaged-utf8.txt"}),
        );
        assert_eq!(damaged_read["text"], expected_damaged);
        assert_eq!(damaged_read["encoding"], "windows-1251");
        assert!(
            !damaged_read["text"]
                .as_str()
                .unwrap()
                .contains("Замещающие")
        );
        assert!(
            !damaged_read["text"]
                .as_str()
                .unwrap()
                .contains("Постановление")
        );

        let damaged_file = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "damaged-utf8.txt"}),
        );
        assert_eq!(damaged_file["kind"], "text");
        assert_eq!(damaged_file["text"], expected_damaged);
        assert_eq!(damaged_file["encoding"], "windows-1251");

        let damaged_doc = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "damaged-utf8.txt"}),
        );
        assert_eq!(damaged_doc["format"], "text");
        assert_eq!(damaged_doc["encoding"], "windows-1251");
        let dd_text = damaged_doc["text"].as_str().unwrap();
        assert_eq!(
            &dd_text[dd_text.find('\n').unwrap() + 1..],
            expected_damaged
        );

        let damaged_inspect = call_ok(
            &fs,
            "inspect_document",
            &json!({"root": "work", "path": "damaged-utf8.txt"}),
        );
        assert_eq!(damaged_inspect["format"], "text");
        assert_eq!(damaged_inspect["encoding"], "windows-1251");

        let damaged_fetch = call_ok(&fs, "fetch", &json!({"id": "work:damaged-utf8.txt"}));
        assert_eq!(damaged_fetch["metadata"]["encoding"], "windows-1251");
        let df_text = damaged_fetch["text"].as_str().unwrap();
        assert_eq!(
            &df_text[df_text.find('\n').unwrap() + 1..],
            expected_damaged
        );

        let damaged_search = call_ok(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "damaged-utf8.txt", "query": "03.03.2023"}),
        );
        let d_res = damaged_search["results"].as_array().unwrap();
        assert_eq!(d_res.len(), 1);
        assert_eq!(d_res[0]["locator"]["line"], 3);

        let amb_tmp = tempfile::tempdir().unwrap();
        write_ambiguous_fixtures(amb_tmp.path());
        let amb_fs = new_fs(&amb_tmp);

        let trunc_read = call_ok(
            &amb_fs,
            "read_text_file",
            &json!({"root": "work", "path": "truncated-utf8.txt"}),
        );
        assert_eq!(trunc_read["text"], "text: С");
        assert_eq!(trunc_read["encoding"], "windows-1251");

        let trunc_file = call_ok(
            &amb_fs,
            "read_file",
            &json!({"root": "work", "path": "truncated-utf8.txt"}),
        );
        assert_eq!(trunc_file["kind"], "text");
        assert_eq!(trunc_file["text"], "text: С");
        assert_eq!(trunc_file["encoding"], "windows-1251");

        let trunc_doc = call_ok(
            &amb_fs,
            "read_document",
            &json!({"root": "work", "path": "truncated-utf8.txt"}),
        );
        assert_eq!(trunc_doc["format"], "text");
        assert_eq!(trunc_doc["encoding"], "windows-1251");
        let td_text = trunc_doc["text"].as_str().unwrap();
        assert_eq!(&td_text[td_text.find('\n').unwrap() + 1..], "text: С");

        let trunc_inspect = call_ok(
            &amb_fs,
            "inspect_document",
            &json!({"root": "work", "path": "truncated-utf8.txt"}),
        );
        assert_eq!(trunc_inspect["format"], "text");
        assert_eq!(trunc_inspect["encoding"], "windows-1251");

        let trunc_fetch = call_ok(&amb_fs, "fetch", &json!({"id": "work:truncated-utf8.txt"}));
        assert_eq!(trunc_fetch["metadata"]["encoding"], "windows-1251");
        let tf_text = trunc_fetch["text"].as_str().unwrap();
        assert_eq!(&tf_text[tf_text.find('\n').unwrap() + 1..], "text: С");

        let trunc_search = call_ok(
            &amb_fs,
            "search_text",
            &json!({"root": "work", "path": "truncated-utf8.txt", "query": "text"}),
        );
        let t_res = trunc_search["results"].as_array().unwrap();
        assert_eq!(t_res.len(), 1);
        assert_eq!(t_res[0]["locator"]["line"], 1);
        assert_eq!(t_res[0]["snippet"], "text: С");

        let amb_read = call_ok(
            &amb_fs,
            "read_text_file",
            &json!({"root": "work", "path": "ambiguous-bytes.txt"}),
        );
        assert_eq!(amb_read["text"], "ёяяяяГ");
        assert_eq!(amb_read["encoding"], "windows-1251");

        let amb_file = call_ok(
            &amb_fs,
            "read_file",
            &json!({"root": "work", "path": "ambiguous-bytes.txt"}),
        );
        assert_eq!(amb_file["text"], "ёяяяяГ");
        assert_eq!(amb_file["encoding"], "windows-1251");

        let amb_doc = call_ok(
            &amb_fs,
            "read_document",
            &json!({"root": "work", "path": "ambiguous-bytes.txt"}),
        );
        assert_eq!(amb_doc["encoding"], "windows-1251");
        let amb_t = amb_doc["text"].as_str().unwrap();
        assert_eq!(&amb_t[amb_t.find('\n').unwrap() + 1..], "ёяяяяГ");

        let amb_inspect = call_ok(
            &amb_fs,
            "inspect_document",
            &json!({"root": "work", "path": "ambiguous-bytes.txt"}),
        );
        assert_eq!(amb_inspect["encoding"], "windows-1251");

        let amb_fetch = call_ok(&amb_fs, "fetch", &json!({"id": "work:ambiguous-bytes.txt"}));
        assert_eq!(amb_fetch["metadata"]["encoding"], "windows-1251");
        let a_t = amb_fetch["text"].as_str().unwrap();
        assert_eq!(&a_t[a_t.find('\n').unwrap() + 1..], "ёяяяяГ");

        let amb_search = call_ok(
            &amb_fs,
            "search_text",
            &json!({"root": "work", "path": "ambiguous-bytes.txt", "query": "ёяяяяГ"}),
        );
        let a_res = amb_search["results"].as_array().unwrap();
        assert_eq!(a_res.len(), 1);
        assert_eq!(a_res[0]["locator"]["line"], 1);

        let amb_folder = call_ok(
            &amb_fs,
            "search_text",
            &json!({"root": "work", "query": "ёяяяяГ"}),
        );
        let af_res = amb_folder["results"].as_array().unwrap();
        assert!(
            af_res
                .iter()
                .any(|r| r["relativePath"] == "ambiguous-bytes.txt")
        );
        assert_eq!(amb_folder["skippedInaccessible"], 0);

        let u16_read = call_ok(
            &amb_fs,
            "read_text_file",
            &json!({"root": "work", "path": "utf16-no-ascii.txt"}),
        );
        assert_eq!(u16_read["text"], "яю¬ ");
        assert_eq!(u16_read["encoding"], "windows-1251");

        let u16_file = call_ok(
            &amb_fs,
            "read_file",
            &json!({"root": "work", "path": "utf16-no-ascii.txt"}),
        );
        assert_eq!(u16_file["kind"], "text");
        assert_eq!(u16_file["text"], "яю¬ ");
        assert_eq!(u16_file["encoding"], "windows-1251");

        let u16_doc = call_ok(
            &amb_fs,
            "read_document",
            &json!({"root": "work", "path": "utf16-no-ascii.txt"}),
        );
        assert_eq!(u16_doc["format"], "text");
        assert_eq!(u16_doc["encoding"], "windows-1251");
        let u16_d_text = u16_doc["text"].as_str().unwrap();
        assert_eq!(&u16_d_text[u16_d_text.find('\n').unwrap() + 1..], "яю¬ ");

        let u16_inspect = call_ok(
            &amb_fs,
            "inspect_document",
            &json!({"root": "work", "path": "utf16-no-ascii.txt"}),
        );
        assert_eq!(u16_inspect["format"], "text");
        assert_eq!(u16_inspect["encoding"], "windows-1251");

        let u16_fetch = call_ok(&amb_fs, "fetch", &json!({"id": "work:utf16-no-ascii.txt"}));
        assert_eq!(u16_fetch["metadata"]["encoding"], "windows-1251");
        let u16_f_text = u16_fetch["text"].as_str().unwrap();
        assert_eq!(&u16_f_text[u16_f_text.find('\n').unwrap() + 1..], "яю¬ ");

        let u16_search = call_ok(
            &amb_fs,
            "search_text",
            &json!({"root": "work", "path": "utf16-no-ascii.txt", "query": "яю¬"}),
        );
        let u_res = u16_search["results"].as_array().unwrap();
        assert_eq!(u_res.len(), 1);
        assert_eq!(u_res[0]["locator"]["line"], 1);
    }

    #[test]
    fn cp1251_lock_utf8_and_bom_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        write_main_fixtures(tmp.path());
        let fs = new_fs(&tmp);
        let expected_text = TEXT.replace("\r\n", "\n");

        let r1 = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "utf8.txt"}),
        );
        assert_eq!(r1["text"], expected_text);
        assert_eq!(r1["totalLines"], 4);
        assert_eq!(r1["endLine"], 4);

        let r2 = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "utf8.txt"}),
        );
        assert_eq!(r2["text"], expected_text);
        assert_eq!(r2["totalLines"], 4);
        assert_eq!(r2["endLine"], 4);

        let doc1 = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "utf8.txt"}),
        );
        assert_eq!(doc1["lines"], 4);
        assert_eq!(doc1["size"], 192);
        let d1_text = doc1["text"].as_str().unwrap();
        assert_eq!(&d1_text[d1_text.find('\n').unwrap() + 1..], expected_text);

        // Verification of cursor reconstruction on utf8.txt
        let mut u_doc_args = json!({"root": "work", "path": "utf8.txt", "maxChars": 1});
        let mut u_first_cursor: Option<String> = None;
        let mut u_recovered_lines: std::collections::BTreeMap<u64, String> =
            std::collections::BTreeMap::new();
        loop {
            let res = call_ok(&fs, "read_document", &u_doc_args);
            if u_first_cursor.is_none() {
                u_first_cursor = res
                    .get("nextCursor")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string());
            }
            let t = res["text"].as_str().unwrap();
            let body = &t[t.find('\n').unwrap() + 1..];
            let units = res["units"].as_array().unwrap();
            let mut char_offset = 0;
            for u in units {
                let line_num = u["line"].as_u64().unwrap();
                let chars_count = u["chars"].as_u64().unwrap() as usize;
                let chunk_chars: String =
                    body.chars().skip(char_offset).take(chars_count).collect();
                char_offset += chars_count;
                u_recovered_lines
                    .entry(line_num)
                    .or_default()
                    .push_str(&chunk_chars);
            }
            if let Some(cursor) = res.get("nextCursor").and_then(|c| c.as_str()) {
                u_doc_args["cursor"] = json!(cursor);
            } else {
                break;
            }
        }
        let u_orig_lines: Vec<&str> = expected_text.split('\n').collect();
        assert_eq!(u_recovered_lines.len(), u_orig_lines.len());
        for (idx, orig_line) in u_orig_lines.iter().enumerate() {
            let line_num = (idx + 1) as u64;
            assert_eq!(
                u_recovered_lines
                    .get(&line_num)
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                *orig_line
            );
        }
        let u_reconstructed = (1..=u_orig_lines.len())
            .map(|i| {
                u_recovered_lines
                    .get(&(i as u64))
                    .map(|s| s.as_str())
                    .unwrap_or("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(u_reconstructed, expected_text);

        let stale_file = tmp.path().join("stale_test.txt");
        std::fs::write(&stale_file, b"content before rewrite\n").unwrap();
        let s_res = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "stale_test.txt", "maxChars": 1}),
        );
        let s_cursor = s_res["nextCursor"].as_str().unwrap().to_string();
        std::fs::write(&stale_file, b"different length content now\n").unwrap();
        let stale_err = call_err(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "stale_test.txt", "cursor": s_cursor}),
        );
        assert_eq!(stale_err.code, "STALE_CURSOR");

        let fetch1 = call_ok(&fs, "fetch", &json!({"id": "work:utf8.txt"}));
        let f1_text = fetch1["text"].as_str().unwrap();
        assert_eq!(&f1_text[f1_text.find('\n').unwrap() + 1..], expected_text);

        let s1 = call_ok(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "utf8.txt", "query": "Замещающие"}),
        );
        let res1 = s1["results"].as_array().unwrap();
        assert_eq!(res1.len(), 1);
        assert_eq!(res1[0]["locator"]["line"], 2);
        assert_eq!(res1[0]["snippet"], "Замещающие сделки: взыскание убытков.");

        let bom_expected = format!("\u{feff}{expected_text}");
        let b1 = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "utf8-bom.txt"}),
        );
        assert_eq!(b1["text"], bom_expected);
        assert_eq!(b1["totalLines"], 4);
        assert_eq!(b1["endLine"], 4);

        let b2 = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "utf8-bom.txt"}),
        );
        assert_eq!(b2["text"], bom_expected);
        assert_eq!(b2["totalLines"], 4);
        assert_eq!(b2["endLine"], 4);

        let b_doc = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "utf8-bom.txt"}),
        );
        assert_eq!(b_doc["lines"], 4);
        assert_eq!(b_doc["size"], 195);
        let bd_text = b_doc["text"].as_str().unwrap();
        let bd_body = &bd_text[bd_text.find('\n').unwrap() + 1..];
        assert!(bd_body.starts_with('\u{feff}'));
        assert_eq!(bd_body, bom_expected);

        let b_fetch = call_ok(&fs, "fetch", &json!({"id": "work:utf8-bom.txt"}));
        let bf_text = b_fetch["text"].as_str().unwrap();
        let bf_body = &bf_text[bf_text.find('\n').unwrap() + 1..];
        assert!(bf_body.starts_with('\u{feff}'));
        assert_eq!(bf_body, bom_expected);

        let b_search = call_ok(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "utf8-bom.txt", "query": "Замещающие"}),
        );
        let b_res = b_search["results"].as_array().unwrap();
        assert_eq!(b_res.len(), 1);
        assert_eq!(b_res[0]["locator"]["line"], 2);
        assert_eq!(b_res[0]["snippet"], "Замещающие сделки: взыскание убытков.");

        let also1 = call_ok(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "utf8-also-cp1251.txt"}),
        );
        assert_eq!(also1["text"], "»");

        let also2 = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "utf8-also-cp1251.txt"}),
        );
        assert_eq!(also2["text"], "»");

        let also_doc = call_ok(
            &fs,
            "read_document",
            &json!({"root": "work", "path": "utf8-also-cp1251.txt"}),
        );
        let ad_text = also_doc["text"].as_str().unwrap();
        assert_eq!(&ad_text[ad_text.find('\n').unwrap() + 1..], "»");

        let also_fetch = call_ok(&fs, "fetch", &json!({"id": "work:utf8-also-cp1251.txt"}));
        let af_text = also_fetch["text"].as_str().unwrap();
        assert_eq!(&af_text[af_text.find('\n').unwrap() + 1..], "»");

        let also_err = call_err(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "utf8-also-cp1251.txt", "query": "»"}),
        );
        assert_eq!(also_err.code, "INVALID_ARGUMENT");

        let amb_tmp = tempfile::tempdir().unwrap();
        write_ambiguous_fixtures(amb_tmp.path());
        let amb_fs = new_fs(&amb_tmp);

        let nul1 = call_ok(
            &amb_fs,
            "read_text_file",
            &json!({"root": "work", "path": "utf8-nul.txt"}),
        );
        assert_eq!(nul1["text"], "A\u{0}B");

        let nul2 = call_ok(
            &amb_fs,
            "read_file",
            &json!({"root": "work", "path": "utf8-nul.txt"}),
        );
        assert_eq!(nul2["text"], "A\u{0}B");

        let nul_doc = call_ok(
            &amb_fs,
            "read_document",
            &json!({"root": "work", "path": "utf8-nul.txt"}),
        );
        let nd_text = nul_doc["text"].as_str().unwrap();
        assert_eq!(&nd_text[nd_text.find('\n').unwrap() + 1..], "A\u{0}B");

        let nul_fetch = call_ok(&amb_fs, "fetch", &json!({"id": "work:utf8-nul.txt"}));
        let nf_text = nul_fetch["text"].as_str().unwrap();
        assert_eq!(&nf_text[nf_text.find('\n').unwrap() + 1..], "A\u{0}B");

        let nul_search = call_ok(
            &amb_fs,
            "search_text",
            &json!({"root": "work", "path": "utf8-nul.txt", "query": "A\u{0}B"}),
        );
        let n_res = nul_search["results"].as_array().unwrap();
        assert_eq!(n_res.len(), 1);
        assert_eq!(n_res[0]["locator"]["line"], 1);
    }

    #[test]
    fn cp1251_lock_rejects_binary_and_controls() {
        let tmp = tempfile::tempdir().unwrap();
        write_main_fixtures(tmp.path());
        let fs = new_fs(&tmp);

        for tool in [
            "read_text_file",
            "read_file",
            "read_document",
            "inspect_document",
        ] {
            let err = call_err(
                &fs,
                tool,
                &json!({"root": "work", "path": "bom-cp1251.txt"}),
            );
            assert_eq!(err.code, "NOT_TEXT", "{tool}");
        }
        let b_fetch = call_err(&fs, "fetch", &json!({"id": "work:bom-cp1251.txt"}));
        assert_eq!(b_fetch.code, "NOT_TEXT");
        let b_search = call_err(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "bom-cp1251.txt", "query": "Замещающие"}),
        );
        assert_eq!(b_search.code, "NOT_TEXT");

        for tool in [
            "read_text_file",
            "read_file",
            "read_document",
            "inspect_document",
        ] {
            let err = call_err(&fs, tool, &json!({"root": "work", "path": "binary.txt"}));
            assert_eq!(err.code, "NOT_TEXT", "{tool} binary.txt");
        }
        let bin_fetch = call_err(&fs, "fetch", &json!({"id": "work:binary.txt"}));
        assert_eq!(bin_fetch.code, "NOT_TEXT");
        let bin_search = call_err(
            &fs,
            "search_text",
            &json!({"root": "work", "path": "binary.txt", "query": "Замещающие"}),
        );
        assert_eq!(bin_search.code, "NOT_TEXT");

        let ctrl_tmp = tempfile::tempdir().unwrap();
        write_controls_fixtures(ctrl_tmp.path());
        let ctrl_fs = new_fs(&ctrl_tmp);

        for name in ["nul-control.txt", "x98.txt", "sub.txt", "utf16.txt"] {
            for tool in [
                "read_text_file",
                "read_file",
                "read_document",
                "inspect_document",
            ] {
                let err = call_err(&ctrl_fs, tool, &json!({"root": "work", "path": name}));
                assert_eq!(err.code, "NOT_TEXT", "{tool} {name}");
            }
            let err = call_err(&ctrl_fs, "fetch", &json!({"id": format!("work:{name}")}));
            assert_eq!(err.code, "NOT_TEXT", "fetch {name}");
            let s_err = call_err(
                &ctrl_fs,
                "search_text",
                &json!({"root": "work", "path": name, "query": "Замещающие"}),
            );
            assert_eq!(s_err.code, "NOT_TEXT", "search_text {name}");
        }

        let bin_res = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "binary.bin"}),
        );
        assert_eq!(bin_res["kind"], "binary");
        let expected_blob =
            base64::engine::general_purpose::STANDARD.encode((0..=255u8).collect::<Vec<u8>>());
        assert_eq!(bin_res["blob"], expected_blob);

        let dat_read = call_err(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "cp1251.dat"}),
        );
        assert_eq!(dat_read.code, "NOT_TEXT");
        assert_eq!(
            dat_read.message,
            "File is not valid UTF-8. Use read_document for document formats."
        );

        let dat_file = call_ok(
            &fs,
            "read_file",
            &json!({"root": "work", "path": "cp1251.dat"}),
        );
        assert_eq!(dat_file["kind"], "binary");
        let cp_bytes = cp1251_encode(TEXT);
        let cp_blob = base64::engine::general_purpose::STANDARD.encode(&cp_bytes);
        assert_eq!(dat_file["blob"], cp_blob);

        let search = call_ok(
            &fs,
            "search_text",
            &json!({"root": "work", "query": "Замещающие"}),
        );
        let results = search["results"].as_array().unwrap();
        assert!(!results.iter().any(|r| r["relativePath"] == "cp1251.dat"));

        let mc_err = call_err(
            &fs,
            "read_text_file",
            &json!({"root": "work", "path": "cp1251.txt", "maxChars": 200_001}),
        );
        assert_eq!(mc_err.code, "INVALID_ARGUMENT");
    }
}
