use crate::{
    documents::Source,
    email::{AttachmentInfo, Mail},
    filesystem::{Result, error},
};
use std::io::Read;

pub(crate) const MSG_LIMIT: u64 = 256 * 1024 * 1024;
fn invalid() -> crate::filesystem::FsError {
    error("DOCUMENT_INVALID", "Cannot parse MSG")
}
type Compound = cfb::CompoundFile<Source>;
fn stream(cfb: &mut Compound, path: &str) -> Result<Option<Vec<u8>>> {
    if !cfb.exists(path) {
        return Ok(None);
    }
    let size = cfb.entry(path).map_err(|_| invalid())?.len();
    if size > MSG_LIMIT {
        return Err(error("FILE_TOO_LARGE", "MSG stream exceeds 256 MiB"));
    }
    let mut bytes = Vec::with_capacity(size as usize + 1);
    cfb.open_stream(path)
        .map_err(|_| invalid())?
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 != size {
        return Err(invalid());
    }
    Ok(Some(bytes))
}
fn property_path(base: &str, tag: u16, typ: u16) -> String {
    format!("{base}/__substg1.0_{tag:04X}{typ:04X}")
}
fn text(
    cfb: &mut Compound,
    base: &str,
    tag: u16,
    encoding: &'static encoding_rs::Encoding,
) -> Result<Option<String>> {
    if let Some(bytes) = stream(cfb, &property_path(base, tag, 0x001f))? {
        if bytes.len() % 2 != 0 {
            return Err(invalid());
        }
        let words: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|s| u16::from_le_bytes([s[0], s[1]]))
            .collect();
        return Ok(Some(
            String::from_utf16_lossy(&words)
                .trim_end_matches('\0')
                .into(),
        ));
    }
    Ok(stream(cfb, &property_path(base, tag, 0x001e))?
        .map(|s| encoding.decode(&s).0.trim_end_matches('\0').into()))
}
fn first_text(
    cfb: &mut Compound,
    base: &str,
    tags: &[u16],
    enc: &'static encoding_rs::Encoding,
) -> Result<Option<String>> {
    for tag in tags {
        if let Some(s) = text(cfb, base, *tag, enc)?
            && !s.is_empty()
        {
            return Ok(Some(s));
        }
    }
    Ok(None)
}
fn fixed(props: &[u8], header: usize, tag: u32) -> Option<u64> {
    props
        .get(header..)?
        .as_chunks::<16>()
        .0
        .iter()
        .find(|s| u32::from_le_bytes(s[..4].try_into().unwrap()) == tag)
        .map(|s| u64::from_le_bytes(s[8..16].try_into().unwrap()))
}
fn encoding(props: &[u8], tag: u32) -> Option<&'static encoding_rs::Encoding> {
    fixed(props, 32, tag)
        .and_then(|v| u16::try_from(v).ok())
        .and_then(codepage::to_encoding)
}
fn date(value: u64) -> Option<String> {
    let seconds = i64::try_from(value / 10_000_000)
        .ok()?
        .checked_sub(11_644_473_600)?;
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, ((value % 10_000_000) * 100) as u32)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
}
pub(crate) fn parse_msg(source: Source) -> Result<Mail> {
    let mut cfb = cfb::CompoundFile::open(source).map_err(|_| invalid())?;
    parse_compound(&mut cfb)
}
fn parse_compound(cfb: &mut Compound) -> Result<Mail> {
    let props = stream(cfb, "/__properties_version1.0")?.unwrap_or_default();
    if !props.is_empty() && (props.len() < 32 || (props.len() - 32) % 16 != 0) {
        return Err(invalid());
    }
    let enc = encoding(&props, 0x3ffd0003)
        .or_else(|| encoding(&props, 0x3fde0003))
        .unwrap_or(encoding_rs::WINDOWS_1251);
    let html_enc = encoding(&props, 0x3fde0003).unwrap_or(enc);
    let body = text(cfb, "", 0x1000, enc)?;
    let (body, body_source) = if let Some(body) = body.filter(|s| !s.is_empty()) {
        (body, "text/plain")
    } else if let Some(html) = stream(cfb, &property_path("", 0x1013, 0x0102))? {
        (
            mail_parser::decoders::html::html_to_text(&html_enc.decode(&html).0),
            "text/html",
        )
    } else if let Some(html) = text(cfb, "", 0x1013, html_enc)? {
        (
            mail_parser::decoders::html::html_to_text(&html),
            "text/html",
        )
    } else if cfb.exists(property_path("", 0x1009, 0x0102)) {
        (String::new(), "rtf_only")
    } else {
        (String::new(), "none")
    };
    let sender_name = first_text(cfb, "", &[0x0c1a, 0x0042], enc)?;
    let sender_address = first_text(cfb, "", &[0x5d01, 0x0c1f, 0x0065], enc)?;
    let from = match (sender_name, sender_address) {
        (Some(n), Some(a)) => Some(format!("{n} <{a}>")),
        (n, a) => n.or(a),
    };
    let mut paths: Vec<_> = cfb
        .read_storage("/")
        .map_err(|_| invalid())?
        .filter(|e| e.is_storage() && e.name().starts_with("__attach_version1.0_#"))
        .map(|e| e.path().to_string_lossy().replace('\\', "/"))
        .collect();
    paths.sort();
    let mut attachments = Vec::new();
    for (i, base) in paths.iter().enumerate() {
        let data = property_path(base, 0x3701, 0x0102);
        // Only directory metadata is queried: attachment data/embedded storages are never opened.
        let size = if cfb.exists(&data) {
            cfb.entry(&data).map_err(|_| invalid())?.len()
        } else {
            0
        };
        let props = stream(cfb, &format!("{base}/__properties_version1.0"))?.unwrap_or_default();
        let embedded = fixed(&props, 8, 0x37050003) == Some(5)
            || cfb.is_storage(property_path(base, 0x3701, 0x000d));
        attachments.push(AttachmentInfo {
            index: i + 1,
            name: first_text(cfb, base, &[0x3707, 0x3704, 0x3001], enc)?
                .unwrap_or_else(|| "attachment".into()),
            mime: text(cfb, base, 0x370e, enc)?.unwrap_or_else(|| {
                if embedded {
                    "message/rfc822".into()
                } else {
                    "application/octet-stream".into()
                }
            }),
            size,
            inline: false,
            embedded_message: embedded,
        });
    }
    Ok(Mail {
        subject: text(cfb, "", 0x0037, enc)?,
        from,
        to: text(cfb, "", 0x0e04, enc)?.into_iter().collect(),
        cc: text(cfb, "", 0x0e03, enc)?.into_iter().collect(),
        date: fixed(&props, 32, 0x0e060040)
            .or_else(|| fixed(&props, 32, 0x00390040))
            .and_then(date),
        body,
        body_source,
        attachments,
    })
}

pub(crate) fn msg_attachment(source: Source, index: usize) -> Result<(AttachmentInfo, Vec<u8>)> {
    let mut cfb = cfb::CompoundFile::open(source).map_err(|_| invalid())?;
    let mut paths: Vec<_> = cfb
        .read_storage("/")
        .map_err(|_| invalid())?
        .filter(|e| e.is_storage() && e.name().starts_with("__attach_version1.0_#"))
        .map(|e| e.path().to_string_lossy().replace('\\', "/"))
        .collect();
    paths.sort();
    let base = index
        .checked_sub(1)
        .and_then(|i| paths.get(i))
        .cloned()
        .ok_or_else(|| error("MEMBER_NOT_FOUND", "Attachment not found"))?;
    let mail = parse_compound(&mut cfb)?;
    let info = mail
        .attachments
        .into_iter()
        .nth(index - 1)
        .ok_or_else(invalid)?;
    if info.embedded_message {
        return Err(error(
            "UNSUPPORTED_FORMAT",
            "Cannot read embedded Outlook message",
        ));
    }
    crate::documents::check_attachment(&info)?;
    let mut bytes = Vec::with_capacity(info.size as usize + 1);
    cfb.open_stream(property_path(&base, 0x3701, 0x0102))
        .map_err(|_| invalid())?
        .take(crate::archive::MEMBER_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > crate::archive::MEMBER_LIMIT {
        return Err(error("FILE_TOO_LARGE", "Attachment exceeds 256 MiB"));
    }
    if bytes.len() as u64 != info.size {
        return Err(invalid());
    }
    Ok((info, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    fn fixture(ansi: bool, rtf: bool) -> Source {
        let mut cfb = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        let mut props = vec![0; 32];
        for (tag, value) in [
            (0x3ffd0003u32, 1251u64),
            (0x0e060040, 134_367_120_000_000_000),
        ] {
            props.extend(tag.to_le_bytes());
            props.extend([0; 4]);
            props.extend(value.to_le_bytes());
        }
        cfb.create_stream("/__properties_version1.0")
            .unwrap()
            .write_all(&props)
            .unwrap();
        for (tag, value) in [
            (0x0037, "Привет"),
            (0x1000, "Текст письма"),
            (0x0c1a, "Автор"),
            (0x5d01, "sender@example.test"),
            (0x0e04, "Получатель"),
        ] {
            if rtf && tag == 0x1000 {
                continue;
            }
            let (typ, bytes) = if ansi {
                (0x001e, encoding_rs::WINDOWS_1251.encode(value).0.to_vec())
            } else {
                (
                    0x001f,
                    value
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
            };
            cfb.create_stream(property_path("", tag, typ))
                .unwrap()
                .write_all(&bytes)
                .unwrap();
        }
        if rtf {
            cfb.create_stream(property_path("", 0x1009, 0x0102))
                .unwrap()
                .write_all(b"not decoded")
                .unwrap();
        }
        for i in 0..2 {
            let base = format!("/__attach_version1.0_#{i:08X}");
            cfb.create_storage(&base).unwrap();
            cfb.create_stream(property_path(&base, 0x3707, 0x001f))
                .unwrap()
                .write_all(
                    &"вложение.pdf"
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            if i == 0 {
                cfb.create_stream(property_path(&base, 0x3701, 0x0102))
                    .unwrap()
                    .set_len(1024 * 1024)
                    .unwrap();
            } else {
                cfb.create_storage(property_path(&base, 0x3701, 0x000d))
                    .unwrap();
            }
        }
        Source::Memory(cfb.into_inner())
    }
    #[test]
    fn attachment_stream_is_bounded_and_embedded_messages_are_explicit() {
        let (info, bytes) = msg_attachment(fixture(false, false), 1).unwrap();
        assert_eq!(info.size, 1024 * 1024);
        assert_eq!(bytes, vec![0; 1024 * 1024]);
        assert_eq!(
            msg_attachment(fixture(false, false), 2).err().unwrap().code,
            "UNSUPPORTED_FORMAT"
        );
        for index in [0, 3, usize::MAX] {
            assert_eq!(
                msg_attachment(fixture(false, false), index)
                    .err()
                    .unwrap()
                    .code,
                "MEMBER_NOT_FOUND"
            );
        }
        let mut info = parse_msg(fixture(false, false))
            .unwrap()
            .attachments
            .remove(0);
        info.size = crate::archive::MEMBER_LIMIT + 1;
        assert_eq!(
            crate::documents::check_attachment(&info)
                .err()
                .unwrap()
                .code,
            "FILE_TOO_LARGE"
        );
    }
    #[test]
    fn unicode_ansi_properties_time_and_attachment_metadata() {
        for ansi in [false, true] {
            let mail = parse_msg(fixture(ansi, false)).unwrap();
            assert_eq!(mail.subject.as_deref(), Some("Привет"));
            assert_eq!(mail.body, "Текст письма");
            assert_eq!(mail.from.as_deref(), Some("Автор <sender@example.test>"));
            assert_eq!(mail.to, vec!["Получатель"]);
            assert!(mail.date.is_some());
            assert_eq!(mail.attachments.len(), 2);
            assert_eq!(mail.attachments[0].size, 1024 * 1024);
            assert!(mail.attachments[1].embedded_message);
        }
        let mail = parse_msg(fixture(false, true)).unwrap();
        assert_eq!(mail.body_source, "rtf_only");
        assert!(mail.body.is_empty());
        assert_eq!(
            date(116_444_736_000_000_000).as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
    }
    #[test]
    fn html_and_truncated_cfb() {
        let mut cfb = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        cfb.create_stream(property_path("", 0x1013, 0x0102))
            .unwrap()
            .write_all(b"<p>HTML <b>body</b></p>")
            .unwrap();
        let mail = parse_msg(Source::Memory(cfb.into_inner())).unwrap();
        assert_eq!(mail.body_source, "text/html");
        assert!(mail.body.contains("HTML body"));
        assert_eq!(
            parse_msg(Source::Memory(Cursor::new(b"truncated".to_vec())))
                .err()
                .unwrap()
                .code,
            "DOCUMENT_INVALID"
        );
    }
}
