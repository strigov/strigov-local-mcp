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
    io::{BufRead, BufReader, Read},
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
        let mut reader = BufReader::new(opened.source.take(read_size + 1));
        let start = number(args, "startLine", 1, usize::MAX)?;
        let requested_end = number(args, "endLine", usize::MAX, usize::MAX)?;
        let max = number(args, "maxChars", OUTPUT_LIMIT, OUTPUT_LIMIT)?;
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
            let text = std::str::from_utf8(&line).map_err(|_| {
                error(
                    "NOT_TEXT",
                    "File is not valid UTF-8. Use read_document for document formats.",
                )
            })?;
            if total >= start && total <= requested_end {
                if total > start {
                    if chars < max {
                        out.push('\n');
                        chars += 1;
                    } else {
                        truncated = true;
                    }
                }
                for c in text.strip_suffix('\r').unwrap_or(text).chars() {
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
            json!({"kind":"text","root":id,"path":location_display,"uri":location_uri,"mimeType":mime(&p),"text":out,"startLine":start,"endLine":start+memchr::memchr_iter(b'\n',out.as_bytes()).count(),"totalLines":total,"truncated":truncated}),
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
                        if let Ok(text) = std::str::from_utf8(&buffer)
                            && let Some(found) = matcher.find(text)
                        {
                            let mut from = found.start().saturating_sub(360);
                            while !text.is_char_boundary(from) {
                                from += 1;
                            }
                            let mut to = (found.end() + 640).min(text.len());
                            while !text.is_char_boundary(to) {
                                to -= 1;
                            }
                            results.push(json!({"root":definition.id,"path":public(&definition.id,&entry.logical),"relativePath":display(&entry.logical),"uri":uri(&definition.id,&entry.logical),"line":memchr::memchr_iter(b'\n',&buffer[..found.start()]).count()+1,"snippet":text[from..to].split_whitespace().collect::<Vec<_>>().join(" ")}));
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
