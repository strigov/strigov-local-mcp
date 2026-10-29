//! In-memory container members; names are never used as host filesystem paths.
use crate::{
    documents::{Source, ZIP_ENTRY_LIMIT, ZIP_RATIO_LIMIT},
    filesystem::{Result, blocked, error},
};
use std::{io::Read, path::Path};
use zip::{CompressionMethod, ZipArchive};

pub(crate) const MEMBER_LIMIT: u64 = 256 * 1024 * 1024;
pub(crate) struct MemberInfo {
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    pub is_dir: bool,
    pub readable: bool,
    pub reason: Option<&'static str>,
}
pub(crate) fn decode_name(raw: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.into();
    }
    let cp866 = encoding_rs::IBM866.decode(raw).0;
    if cp866
        .chars()
        .any(|c| ('\u{2500}'..='\u{259f}').contains(&c))
    {
        let cp1251 = encoding_rs::WINDOWS_1251.decode(raw).0;
        if !cp1251
            .chars()
            .any(|c| ('\u{2500}'..='\u{259f}').contains(&c))
        {
            return cp1251.into_owned();
        }
    }
    cp866.into_owned()
}
pub(crate) fn normalize_name(name: &str) -> Option<String> {
    if name.contains('\0') || name.contains("::") {
        return None;
    }
    let name = name.replace('\\', "/");
    let mut parts = Vec::new();
    for part in name.split('/') {
        if part == ".." || part.as_bytes().get(1) == Some(&b':') {
            return None;
        }
        if !part.is_empty() && part != "." {
            parts.push(part);
        }
    }
    Some(parts.join("/"))
}
fn invalid() -> crate::filesystem::FsError {
    error("ZIP_INVALID", "Cannot read ZIP container")
}
fn archive(source: Source) -> Result<ZipArchive<Source>> {
    let zip = ZipArchive::new(source).map_err(|_| invalid())?;
    if zip.len() > ZIP_ENTRY_LIMIT {
        return Err(error("ZIP_BOMB", "ZIP has too many entries"));
    }
    Ok(zip)
}
fn info(file: &zip::read::ZipFile<'_, Source>) -> MemberInfo {
    let decoded = decode_name(file.name_raw());
    let normalized = normalize_name(&decoded);
    let name = normalized.clone().unwrap_or(decoded);
    let reason = if blocked(Path::new(&name.replace('\\', "/"))) {
        Some("BLOCKED_PATH")
    } else if normalized.is_none() {
        Some("UNSAFE_MEMBER_NAME")
    } else if file.encrypted() {
        Some("ENCRYPTED_MEMBER")
    } else if !matches!(
        file.compression(),
        CompressionMethod::Stored | CompressionMethod::Deflated
    ) {
        Some("UNSUPPORTED_COMPRESSION")
    } else if file.size() > MEMBER_LIMIT {
        Some("FILE_TOO_LARGE")
    } else if file.size() > 1024 * 1024
        && file.size() > file.compressed_size().saturating_mul(ZIP_RATIO_LIMIT)
    {
        Some("ZIP_BOMB")
    } else {
        None
    };
    MemberInfo {
        name,
        size: file.size(),
        compressed_size: file.compressed_size(),
        is_dir: file.is_dir(),
        readable: reason.is_none(),
        reason,
    }
}
pub(crate) fn list_members(source: Source) -> Result<Vec<MemberInfo>> {
    crate::documents::guarded(|| {
        let mut zip = archive(source)?;
        let mut members = Vec::with_capacity(zip.len());
        for i in 0..zip.len() {
            // Raw reads expose central-directory encryption/compression flags without
            // asking the decoder to open encrypted or unsupported members.
            let file = zip.by_index_raw(i).map_err(|_| invalid())?;
            let member = info(&file);
            if member.reason != Some("BLOCKED_PATH") {
                members.push(member);
            }
        }
        members.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(members)
    })
}
pub(crate) fn read_member(source: Source, name: &str, limit: u64) -> Result<(MemberInfo, Vec<u8>)> {
    crate::documents::guarded(|| {
        let normalized = normalize_name(name)
            .ok_or_else(|| error("UNSAFE_MEMBER_NAME", "Unsafe ZIP member name"))?;
        if blocked(Path::new(&normalized)) {
            return Err(error(
                "BLOCKED_PATH",
                "Credential or service path is blocked",
            ));
        }
        let mut zip = archive(source)?;
        for i in 0..zip.len() {
            let member = info(&zip.by_index_raw(i).map_err(|_| invalid())?);
            if member.name != normalized {
                continue;
            }
            if let Some(reason) = member.reason {
                return Err(error(reason, "ZIP member cannot be read"));
            }
            if member.is_dir {
                return Err(error("MEMBER_NOT_FOUND", "ZIP member is a directory"));
            }
            if member.size > limit.min(MEMBER_LIMIT) {
                return Err(error(
                    "FILE_TOO_LARGE",
                    "ZIP member exceeds the format size limit",
                ));
            }
            let mut file = zip.by_index(i).map_err(|_| invalid())?;
            let mut bytes = Vec::with_capacity(member.size as usize + 1);
            (&mut file)
                .take(member.size + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| invalid())?;
            if bytes.len() as u64 != member.size {
                return Err(invalid());
            }
            return Ok((member, bytes));
        }
        Err(error("MEMBER_NOT_FOUND", "ZIP member not found"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    fn source(bytes: Vec<u8>) -> Source {
        Source::Memory(Cursor::new(bytes))
    }
    fn central(bytes: &[u8]) -> usize {
        bytes.windows(4).rposition(|s| s == b"PK\x01\x02").unwrap()
    }
    #[test]
    fn russian_names_and_normalization_confine_every_member() {
        let name = "Иск.pdf";
        assert_eq!(decode_name(&encoding_rs::IBM866.encode(name).0), name);
        assert_eq!(decode_name(name.as_bytes()), name);
        assert_eq!(
            decode_name(&encoding_rs::WINDOWS_1251.encode("Тест.pdf").0),
            "Тест.pdf"
        );
        assert_eq!(normalize_name("/./a\\b//c.txt"), Some("a/b/c.txt".into()));
        for name in ["../x", "a/../x", "C:\\x", "/C:/x", "a::b", "a\0b"] {
            assert!(normalize_name(name).is_none(), "{name:?}");
        }
        let bytes = zip(&[
            (".env", b"secret"),
            ("a/.ssh/key", b"secret"),
            ("ok.txt", b"text"),
            ("../outside.txt", b"outside"),
        ]);
        let listed = list_members(source(bytes.clone())).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].reason, Some("UNSAFE_MEMBER_NAME"));
        assert_eq!(
            read_member(source(bytes.clone()), "ok.txt", 4).unwrap().1,
            b"text"
        );
        assert_eq!(
            read_member(source(bytes.clone()), ".env", 100)
                .err()
                .unwrap()
                .code,
            "BLOCKED_PATH"
        );
        assert_eq!(
            read_member(source(bytes), "../outside.txt", 100)
                .err()
                .unwrap()
                .code,
            "UNSAFE_MEMBER_NAME"
        );
    }
    #[test]
    fn metadata_guards_and_actual_read_cap() {
        let original = zip(&[("x.txt", b"hello")]);
        let offset = central(&original);
        for (field, value, reason) in [
            (8, 1u16, "ENCRYPTED_MEMBER"),
            (10, 12, "UNSUPPORTED_COMPRESSION"),
        ] {
            let mut bytes = original.clone();
            bytes[offset + field..offset + field + 2].copy_from_slice(&value.to_le_bytes());
            let listed = list_members(source(bytes.clone())).unwrap();
            assert!(!listed[0].readable);
            assert_eq!(listed[0].reason, Some(reason));
            assert_eq!(
                read_member(source(bytes), "x.txt", 100).err().unwrap().code,
                reason
            );
        }
        assert_eq!(
            read_member(source(original.clone()), "x.txt", 4)
                .err()
                .unwrap()
                .code,
            "FILE_TOO_LARGE"
        );
        for (size, reason) in [
            (MEMBER_LIMIT as u32 + 1, "FILE_TOO_LARGE"),
            (2 * 1024 * 1024, "ZIP_BOMB"),
        ] {
            let mut bytes = original.clone();
            bytes[offset + 24..offset + 28].copy_from_slice(&size.to_le_bytes());
            assert_eq!(
                list_members(source(bytes.clone())).unwrap()[0].reason,
                Some(reason)
            );
            assert_eq!(
                read_member(source(bytes), "x.txt", MEMBER_LIMIT)
                    .err()
                    .unwrap()
                    .code,
                reason
            );
        }
        let mut bytes = original;
        bytes[offset + 24..offset + 28].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            read_member(source(bytes), "x.txt", 100).err().unwrap().code,
            "ZIP_INVALID"
        );
        assert_eq!(
            list_members(source(b"truncated".to_vec()))
                .err()
                .unwrap()
                .code,
            "ZIP_INVALID"
        );
    }
    #[test]
    fn entry_count_is_bounded() {
        let names: Vec<_> = (0..=ZIP_ENTRY_LIMIT).map(|i| format!("{i}.txt")).collect();
        let entries: Vec<_> = names.iter().map(|n| (n.as_str(), b"".as_slice())).collect();
        assert_eq!(
            list_members(source(zip(&entries))).err().unwrap().code,
            "ZIP_BOMB"
        );
    }
}
