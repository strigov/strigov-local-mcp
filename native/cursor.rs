use crate::filesystem::{Result, error};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::hash::{DefaultHasher, Hasher};

pub(crate) const CURSOR_MAX_LEN: usize = 2048;
const RAW_MAX_LEN: usize = 1536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    pub size: u64,
    pub modified_ns: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorKind {
    Walk,
    Doc,
    Inspect,
    Find,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub v: u8,
    pub k: CursorKind,
    pub h: u64,
    pub f: Option<Fingerprint>,
    pub p: Vec<String>,
    pub u: u64,
    pub o: u64,
}
pub(crate) fn request_hash(parts: &[&str]) -> u64 {
    let mut hash = DefaultHasher::new();
    for part in parts {
        hash.write(part.as_bytes());
        hash.write_u8(0xff);
    }
    hash.finish()
}
pub(crate) fn encode(cursor: &Cursor) -> String {
    let mut bytes = vec![
        cursor.v,
        match cursor.k {
            CursorKind::Walk => 0,
            CursorKind::Doc => 1,
            CursorKind::Inspect => 2,
            CursorKind::Find => 3,
        },
    ];
    bytes.extend_from_slice(&cursor.h.to_le_bytes());
    bytes.push(u8::from(cursor.f.is_some()));
    if let Some(f) = cursor.f {
        bytes.extend_from_slice(&f.size.to_le_bytes());
        bytes.extend_from_slice(&f.modified_ns.to_le_bytes());
    }
    write_varint(&mut bytes, cursor.u);
    write_varint(&mut bytes, cursor.o);
    write_varint(&mut bytes, cursor.p.len() as u64);
    for component in &cursor.p {
        write_varint(&mut bytes, component.len() as u64);
        bytes.extend_from_slice(component.as_bytes());
    }
    let raw = URL_SAFE_NO_PAD.encode(bytes);
    debug_assert!(raw.len() <= CURSOR_MAX_LEN);
    raw
}
pub(crate) fn decode(raw: &str, kind: CursorKind, hash: u64) -> Result<Cursor> {
    let invalid = || error("INVALID_CURSOR", "Invalid cursor; restart the request");
    if raw.is_empty() || raw.len() > CURSOR_MAX_LEN {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| invalid())?;
    let cursor = unpack(&bytes).ok_or_else(invalid)?;
    if cursor.v != 1 || cursor.k != kind || cursor.h != hash {
        return Err(invalid());
    }
    Ok(cursor)
}
fn write_varint(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        bytes.push(value as u8 | 0x80);
        value >>= 7;
    }
    bytes.push(value as u8);
}
fn varint_len(mut value: u64) -> usize {
    let mut length = 1;
    while value >= 128 {
        length += 1;
        value >>= 7;
    }
    length
}
fn take<'a>(input: &mut &'a [u8], length: usize) -> Option<&'a [u8]> {
    let (bytes, remaining) = input.split_at_checked(length)?;
    *input = remaining;
    Some(bytes)
}
fn read_u64(input: &mut &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(take(input, 8)?.try_into().ok()?))
}
fn read_varint(input: &mut &[u8]) -> Option<u64> {
    let mut value = 0;
    for index in 0..10 {
        let byte = take(input, 1)?[0];
        if index == 9 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return (index == 0 || byte != 0).then_some(value);
        }
    }
    None
}
fn unpack(mut input: &[u8]) -> Option<Cursor> {
    if input.len() > RAW_MAX_LEN {
        return None;
    }
    let v = take(&mut input, 1)?[0];
    let k = match take(&mut input, 1)?[0] {
        0 => CursorKind::Walk,
        1 => CursorKind::Doc,
        2 => CursorKind::Inspect,
        3 => CursorKind::Find,
        _ => return None,
    };
    let h = read_u64(&mut input)?;
    let flags = take(&mut input, 1)?[0];
    if flags & !1 != 0 {
        return None;
    }
    let f = if flags == 1 {
        Some(Fingerprint {
            size: read_u64(&mut input)?,
            modified_ns: read_u64(&mut input)?,
        })
    } else {
        None
    };
    let u = read_varint(&mut input)?;
    let o = read_varint(&mut input)?;
    let count = usize::try_from(read_varint(&mut input)?).ok()?;
    if count > input.len() {
        return None;
    }
    let mut p = Vec::with_capacity(count);
    for _ in 0..count {
        let length = usize::try_from(read_varint(&mut input)?).ok()?;
        if length == 0 {
            return None;
        }
        p.push(
            std::str::from_utf8(take(&mut input, length)?)
                .ok()?
                .to_owned(),
        );
    }
    input.is_empty().then_some(Cursor {
        v,
        k,
        h,
        f,
        p,
        u,
        o,
    })
}
pub(crate) fn walk_child_fits(root_id: &str, parent: &[String], name: &str) -> bool {
    // Exact position, reserving the fingerprint and maximal u/o fields.
    let mut length = 11 + 16 + 10 + 10 + varint_len(parent.len().saturating_add(2) as u64);
    for component in std::iter::once(root_id)
        .chain(parent.iter().map(String::as_str))
        .chain(std::iter::once(name))
    {
        length = length.saturating_add(varint_len(component.len() as u64));
        length = length.saturating_add(component.len());
    }
    length <= RAW_MAX_LEN
}
pub(crate) fn check_fingerprint(cursor: &Cursor, current: Fingerprint) -> Result<()> {
    if cursor.f != Some(current) {
        return Err(error("STALE_CURSOR", "File changed; restart the request"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(kind: CursorKind) -> Cursor {
        Cursor {
            v: 1,
            k: kind,
            h: 0,
            f: None,
            p: vec![],
            u: 0,
            o: 0,
        }
    }
    #[test]
    fn every_cursor_kind_round_trips_canonically() {
        for kind in [
            CursorKind::Walk,
            CursorKind::Doc,
            CursorKind::Inspect,
            CursorKind::Find,
        ] {
            for value in [0, 127, 128, u64::MAX] {
                let mut cursor = sample(kind);
                cursor.u = value;
                cursor.o = u64::MAX - value;
                cursor.f = Some(Fingerprint {
                    size: value,
                    modified_ns: u64::MAX,
                });
                let raw = encode(&cursor);
                assert_eq!(decode(&raw, kind, 0).unwrap(), cursor);
                assert_eq!(encode(&decode(&raw, kind, 0).unwrap()), raw);
            }
        }
    }
    #[test]
    fn binary_cursor_layout_rejects_noncanonical_and_malformed_input() {
        let valid = URL_SAFE_NO_PAD
            .decode(encode(&sample(CursorKind::Walk)))
            .unwrap();
        let mut bad = (0..valid.len())
            .map(|length| valid[..length].to_vec())
            .collect::<Vec<_>>();
        for (index, value) in [(0, 2), (1, 4), (10, 2), (10, 1)] {
            let mut bytes = valid.clone();
            bytes[index] = value;
            bad.push(bytes);
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        bad.push(trailing);
        for varint in [
            vec![0x80, 0],
            vec![0x80; 10],
            [vec![0xff; 9], vec![2]].concat(),
        ] {
            let mut bytes = valid[..11].to_vec();
            bytes.extend(varint);
            bytes.extend_from_slice(&[0, 0]);
            bad.push(bytes);
        }
        for tail in [vec![1], vec![1, 0], vec![1, 1, 0xff], vec![1, 2, b'a']] {
            let mut bytes = valid[..13].to_vec();
            bytes.extend(tail);
            bad.push(bytes);
        }
        let mut huge_count = valid[..13].to_vec();
        write_varint(&mut huge_count, u64::MAX);
        bad.push(huge_count);
        for bytes in bad {
            assert_eq!(
                decode(&URL_SAFE_NO_PAD.encode(bytes), CursorKind::Walk, 0)
                    .unwrap_err()
                    .code,
                "INVALID_CURSOR"
            );
        }
        let raw = encode(&sample(CursorKind::Walk));
        assert_eq!(
            decode(&format!("{raw}="), CursorKind::Walk, 0)
                .unwrap_err()
                .code,
            "INVALID_CURSOR"
        );
        let mut noncanonical = raw;
        assert_eq!(noncanonical.pop(), Some('A'));
        noncanonical.push('B');
        assert_eq!(
            decode(&noncanonical, CursorKind::Walk, 0).unwrap_err().code,
            "INVALID_CURSOR"
        );
    }
    #[test]
    fn walk_child_fit_switches_at_the_exact_raw_cursor_limit() {
        let ascii = "x".repeat(255);
        assert!(walk_child_fits("work", &vec![ascii.clone(); 4], &ascii));
        assert!(!walk_child_fits("work", &vec![ascii.clone(); 5], &ascii));
        let cyrillic = vec!["я".repeat(255); 2];
        assert!(walk_child_fits("work", &cyrillic, "short.txt"));
        assert!(!walk_child_fits("work", &cyrillic, &"x".repeat(502)));
        for root in ["work".to_owned(), "r".repeat(32)] {
            for parent in [vec![ascii.clone(); 4], cyrillic.clone()] {
                let limit = (1..1536)
                    .find(|length| !walk_child_fits(&root, &parent, &"x".repeat(*length)))
                    .unwrap()
                    - 1;
                let mut worst = sample(CursorKind::Walk);
                worst.f = Some(Fingerprint {
                    size: u64::MAX,
                    modified_ns: u64::MAX,
                });
                worst.u = u64::MAX;
                worst.o = u64::MAX;
                worst.p.push(root.clone());
                worst.p.extend(parent.clone());
                worst.p.push("x".repeat(limit));
                let raw = encode(&worst);
                assert!(walk_child_fits(&root, &parent, worst.p.last().unwrap()));
                assert_eq!(raw.len(), CURSOR_MAX_LEN);
                assert_eq!(URL_SAFE_NO_PAD.decode(&raw).unwrap().len(), RAW_MAX_LEN);
                assert_eq!(decode(&raw, CursorKind::Walk, 0).unwrap(), worst);
                worst.p.last_mut().unwrap().push('x');
                assert!(!walk_child_fits(&root, &parent, worst.p.last().unwrap()));
                let over = std::panic::catch_unwind(|| encode(&worst));
                if cfg!(debug_assertions) {
                    assert!(over.is_err());
                } else {
                    assert!(over.unwrap().len() > CURSOR_MAX_LEN);
                }
            }
        }
    }
    #[test]
    fn walk_positions_with_escaping_names_fit_and_round_trip() {
        let mut components = vec!["r".repeat(32)];
        for i in 0..5 {
            components.push(["\"", "\\", "\u{1}"][i % 3].repeat(255));
        }
        let cursor = Cursor {
            v: 1,
            k: CursorKind::Walk,
            h: 99,
            f: Some(Fingerprint {
                size: u64::MAX,
                modified_ns: u64::MAX,
            }),
            p: components,
            u: u64::MAX,
            o: u64::MAX,
        };
        let raw = encode(&cursor);
        assert!(raw.len() <= CURSOR_MAX_LEN);
        assert_eq!(encode(&cursor), raw);
        let decoded = decode(&raw, CursorKind::Walk, 99).unwrap();
        assert_eq!(decoded.p, cursor.p);
        assert_eq!(decoded.f, cursor.f);
        assert_eq!((decoded.u, decoded.o), (cursor.u, cursor.o));
    }
    #[test]
    fn cursors_bind_the_sequence_and_fingerprint() {
        let hash = request_hash(&["read_document", "work", "a.txt"]);
        assert_ne!(hash, request_hash(&["read_document", "worka", ".txt"]));
        let f = Fingerprint {
            size: 10,
            modified_ns: 99,
        };
        let mut c = Cursor {
            v: 1,
            k: CursorKind::Doc,
            h: hash,
            f: Some(f),
            p: vec![],
            u: 3,
            o: 8,
        };
        let raw = encode(&c);
        let decoded = decode(&raw, CursorKind::Doc, hash).unwrap();
        assert_eq!((decoded.u, decoded.o), (3, 8));
        check_fingerprint(&decoded, f).unwrap();
        assert_eq!(
            check_fingerprint(&decoded, Fingerprint { size: 11, ..f })
                .unwrap_err()
                .code,
            "STALE_CURSOR"
        );
        for raw in [
            String::new(),
            "!".into(),
            format!("{raw}!"),
            "a".repeat(CURSOR_MAX_LEN + 1),
            URL_SAFE_NO_PAD.encode(b"{}"),
            URL_SAFE_NO_PAD.encode(b"null"),
        ] {
            assert_eq!(
                decode(&raw, CursorKind::Doc, hash).unwrap_err().code,
                "INVALID_CURSOR"
            );
        }
        assert!(decode(&raw, CursorKind::Walk, hash).is_err());
        assert!(decode(&raw, CursorKind::Doc, hash + 1).is_err());
        c.v = 2;
        assert!(decode(&encode(&c), CursorKind::Doc, hash).is_err());
    }
}
