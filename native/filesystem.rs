use crate::config::{Config, Root};
use base64::Engine;
use cap_std::fs::{Dir, Metadata};
use chrono::{DateTime, Utc};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

const TEXT_LIMIT: u64 = 5 * 1024 * 1024;
const BINARY_LIMIT: u64 = 15 * 1024 * 1024;
const SCAN_LIMIT: u64 = 128 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 200_000;
const WALK_LIMIT: usize = 20_000;

#[derive(Debug)]
pub struct FsError {
    pub code: &'static str,
    pub message: String,
}
type Result<T> = std::result::Result<T, FsError>;
impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for FsError {}
fn error(code: &'static str, message: impl Into<String>) -> FsError {
    FsError {
        code,
        message: message.into(),
    }
}
fn io_error(e: std::io::Error) -> FsError {
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
fn blocked(path: &Path) -> bool {
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
fn relative(raw: &str) -> Result<PathBuf> {
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
fn display(path: &Path) -> String {
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
fn public(root: &str, path: &Path) -> String {
    let p = display(path);
    if p.is_empty() {
        root.into()
    } else {
        format!("{root}/{p}")
    }
}
fn uri(root: &str, path: &Path) -> String {
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
fn is_text(path: &Path) -> bool {
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
            | "eml"
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
            | "rtf"
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
fn image_mime(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}
fn mime(path: &Path) -> &'static str {
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
fn string<'a>(v: &'a Value, key: &str, default: Option<&'a str>) -> Result<&'a str> {
    match v.get(key) {
        Some(Value::String(s)) => Ok(s),
        None => default.ok_or_else(|| error("INVALID_ARGUMENT", format!("Missing {key}"))),
        _ => Err(error("INVALID_ARGUMENT", format!("{key} must be a string"))),
    }
}
fn number(v: &Value, key: &str, default: usize, max: usize) -> Result<usize> {
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
struct RootDir {
    dir: Dir,
}
impl RootDir {
    fn open(root: &Root) -> Result<Self> {
        let dir =
            Dir::open_ambient_dir(&root.path, cap_std::ambient_authority()).map_err(|_| {
                error(
                    "ROOT_UNAVAILABLE",
                    format!("Folder {} is unavailable", root.id),
                )
            })?;
        Ok(Self { dir })
    }
    fn resolve(&self, raw: &str) -> Result<PathBuf> {
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
    fn read(&self, path: &Path, limit: u64, buffer: &mut Vec<u8>) -> Result<()> {
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
        buffer.clear();
        file.take(limit + 1).read_to_end(buffer).map_err(io_error)?;
        if buffer.len() as u64 > limit {
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
                    json!({"root":id,"path":public(id,&relative(raw)?),"relativePath":display(&relative(raw)?),"type":kind(&m),"size":m.len(),"mimeType":if m.is_file(){Some(mime(&p))}else{None},"createdAt":iso(m.created()),"modifiedAt":iso(m.modified()),"uri":uri(id,&relative(raw)?)}),
                )
            }
            "read_text_file" | "read_file" => self.read(args, name == "read_file"),
            "list_directory" => self.list(args),
            "search_files" | "search_text" => self.search(args, name == "search_text"),
            "search" => self.connector_search(args),
            "fetch" => {
                let id = string(args, "id", None)?;
                let (root, path) = id
                    .split_once(':')
                    .ok_or_else(|| error("INVALID_ID", "Document id must use root:path"))?;
                let d = self.read(&json!({"root":root,"path":path}), false)?;
                Ok(
                    json!({"id":id,"title":Path::new(path).file_name().unwrap_or_default().to_string_lossy(),"text":d["text"],"url":d["uri"],"metadata":{"root":root,"path":d["path"],"mimeType":d["mimeType"],"totalLines":d["totalLines"],"truncated":d["truncated"]}}),
                )
            }
            _ => Err(error("UNKNOWN_TOOL", "Unknown tool")),
        }
    }
    fn read(&self, args: &Value, binary: bool) -> Result<Value> {
        let id = self.canonical_root(string(args, "root", None)?)?;
        let raw = string(args, "path", None)?;
        let root = self.root(id)?;
        let p = root.resolve(raw)?;
        let logical = relative(raw)?;
        let mut buf = Vec::new();
        let is_binary = binary && !is_text(&p);
        root.read(
            &p,
            if is_binary { BINARY_LIMIT } else { TEXT_LIMIT },
            &mut buf,
        )?;
        if binary && let Some(mime_type) = image_mime(&p) {
            return Ok(
                json!({"kind":"image","root":id,"path":public(id,&logical),"uri":uri(id,&logical),"mimeType":mime_type,"size":buf.len(),"data":base64::engine::general_purpose::STANDARD.encode(&buf)}),
            );
        }
        let text = match std::str::from_utf8(&buf) {
            Ok(s) => s,
            Err(_) if is_binary => {
                return Ok(
                    json!({"kind":"binary","root":id,"path":public(id,&logical),"uri":uri(id,&logical),"mimeType":mime(&p),"size":buf.len(),"blob":base64::engine::general_purpose::STANDARD.encode(&buf)}),
                );
            }
            Err(_) => {
                return Err(error(
                    "NOT_TEXT",
                    "File is not valid UTF-8. Use read_file or a Markdown sidecar.",
                ));
            }
        };
        let start = number(args, "startLine", 1, usize::MAX)?;
        let total = memchr::memchr_iter(b'\n', text.as_bytes()).count() + 1;
        let end = number(args, "endLine", total.max(1), usize::MAX)?.min(total);
        if end < start {
            return Err(error(
                "INVALID_RANGE",
                "endLine must be greater than or equal to startLine",
            ));
        }
        let max = number(args, "maxChars", OUTPUT_LIMIT, OUTPUT_LIMIT)?;
        let mut out = String::with_capacity(text.len().min(max));
        let mut chars = 0;
        let mut truncated = false;
        'lines: for (i, line) in text
            .split('\n')
            .enumerate()
            .skip(start - 1)
            .take(end - start + 1)
        {
            if i > start - 1 {
                if chars == max {
                    truncated = true;
                    break;
                }
                out.push('\n');
                chars += 1;
            }
            for c in line.strip_suffix('\r').unwrap_or(line).chars() {
                if chars == max {
                    truncated = true;
                    break 'lines;
                }
                out.push(c);
                chars += 1;
            }
        }
        Ok(
            json!({"kind":"text","root":id,"path":public(id,&logical),"uri":uri(id,&logical),"mimeType":mime(&p),"text":out,"startLine":start,"endLine":start+memchr::memchr_iter(b'\n',out.as_bytes()).count(),"totalLines":total,"truncated":truncated}),
        )
    }
    fn list(&self, args: &Value) -> Result<Value> {
        let id = string(args, "root", None)?;
        let raw = string(args, "path", Some(""))?;
        let root = self.root(id)?;
        let base = root.resolve(raw)?;
        let depth = number(args, "depth", 1, 5)?;
        let limit = number(args, "limit", 500, 2000)?;
        let mut stack = vec![(base, relative(raw)?, depth)];
        let mut entries = Vec::new();
        let mut truncated = false;
        while let Some((real, logical, remaining)) = stack.pop() {
            let mut children = root
                .dir
                .read_dir(&real)
                .map_err(io_error)?
                .take(WALK_LIMIT + 1)
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(io_error)?;
            if children.len() > WALK_LIMIT {
                children.truncate(WALK_LIMIT);
                truncated = true;
            }
            children.sort_unstable_by_key(|e| e.file_name());
            for child in children {
                let p = logical.join(child.file_name());
                if blocked(&p) {
                    continue;
                }
                if entries.len() == limit {
                    truncated = true;
                    break;
                }
                let actual = real.join(child.file_name());
                let m = root.dir.symlink_metadata(&actual).map_err(io_error)?;
                entries.push(json!({"name":child.file_name().to_string_lossy(),"path":public(id,&p),"relativePath":display(&p),"type":kind(&m),"size":if m.is_file(){Some(m.len())}else{None},"modifiedAt":iso(m.modified())}));
                if m.is_dir() && remaining > 1 {
                    stack.push((actual, p, remaining - 1));
                }
            }
            if truncated {
                break;
            }
        }
        Ok(
            json!({"root":id,"path":public(id,&relative(raw)?),"entries":entries,"truncated":truncated}),
        )
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
        let base = relative(string(args, "path", Some(""))?)?;
        if selected != "all" && !self.config.roots.iter().any(|r| r.id == selected) {
            return Err(error("UNKNOWN_ROOT", "Unknown folder alias"));
        }
        let mut results = Vec::new();
        let mut unavailable = Vec::new();
        let mut scanned = 0u64;
        let mut visited = 0;
        let mut skipped = 0;
        let mut inaccessible = 0;
        let mut truncated = false;
        let mut buffer = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        'roots: for definition in &self.config.roots {
            if selected != "all" && definition.id != selected {
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
            let mut stack = vec![root.resolve(&display(&base))?];
            while let Some(dir) = stack.pop() {
                let children = match root.dir.read_dir(&dir) {
                    Ok(c) => c,
                    Err(_) => {
                        inaccessible += 1;
                        continue;
                    }
                };
                for child in children {
                    if visited >= WALK_LIMIT || Instant::now() >= deadline {
                        truncated = true;
                        break 'roots;
                    }
                    visited += 1;
                    let child = match child {
                        Ok(c) => c,
                        Err(_) => {
                            inaccessible += 1;
                            continue;
                        }
                    };
                    let p = dir.join(child.file_name());
                    if blocked(&p) {
                        continue;
                    }
                    let ft = match child.file_type() {
                        Ok(ft) => ft,
                        Err(_) => {
                            inaccessible += 1;
                            continue;
                        }
                    };
                    if ft.is_symlink() {
                        continue;
                    }
                    if ft.is_dir() {
                        stack.push(p);
                        continue;
                    }
                    if !ft.is_file() {
                        continue;
                    }
                    if text_search {
                        if !is_text(&p) {
                            continue;
                        }
                        let meta = match root.dir.metadata(&p) {
                            Ok(m) => m,
                            Err(_) => {
                                inaccessible += 1;
                                continue;
                            }
                        };
                        if meta.len() > TEXT_LIMIT {
                            skipped += 1;
                            continue;
                        }
                        if scanned + meta.len() > SCAN_LIMIT {
                            truncated = true;
                            break 'roots;
                        }
                        if root.read(&p, TEXT_LIMIT, &mut buffer).is_err() {
                            inaccessible += 1;
                            continue;
                        }
                        scanned += buffer.len() as u64;
                        if scanned > SCAN_LIMIT {
                            truncated = true;
                            break 'roots;
                        }
                        let text = match std::str::from_utf8(&buffer) {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        if let Some(found) = matcher.find(text) {
                            let mut from = found.start().saturating_sub(360);
                            while !text.is_char_boundary(from) {
                                from += 1;
                            }
                            let mut to = (found.end() + 640).min(text.len());
                            while !text.is_char_boundary(to) {
                                to -= 1;
                            }
                            results.push(json!({"root":definition.id,"path":public(&definition.id,&p),"relativePath":display(&p),"uri":uri(&definition.id,&p),"line":memchr::memchr_iter(b'\n',&buffer[..found.start()]).count()+1,"snippet":text[from..to].split_whitespace().collect::<Vec<_>>().join(" ")}));
                        }
                    } else if matcher.is_match(&display(&p)) {
                        let meta = match root.dir.metadata(&p) {
                            Ok(m) => m,
                            Err(_) => {
                                inaccessible += 1;
                                continue;
                            }
                        };
                        results.push(json!({"root":definition.id,"path":public(&definition.id,&p),"relativePath":display(&p),"uri":uri(&definition.id,&p),"size":meta.len(),"modifiedAt":iso(meta.modified())}));
                    }
                    if results.len() >= limit {
                        truncated = true;
                        break 'roots;
                    }
                }
            }
        }
        Ok(
            json!({"query":query,"results":results,"scannedBytes":scanned,"skippedLargeFiles":skipped,"skippedInaccessible":inaccessible,"unavailableRoots":unavailable,"truncated":truncated}),
        )
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
