use crate::{
    documents::{TextDocument, Unit},
    filesystem::{Result, error},
};
use mail_parser::{Address, MessageParser, MimeHeaders, PartType};
use serde_json::{Value, json};

pub(crate) const EML_LIMIT: u64 = 64 * 1024 * 1024;
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachmentInfo {
    pub index: usize,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub inline: bool,
    pub embedded_message: bool,
}
pub(crate) struct Mail {
    pub subject: Option<String>,
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub date: Option<String>,
    pub body: String,
    pub body_source: &'static str,
    pub attachments: Vec<AttachmentInfo>,
}
fn addresses(address: Option<&Address<'_>>) -> Vec<String> {
    address
        .map(|a| {
            a.iter()
                .map(|a| match (a.name(), a.address()) {
                    (Some(n), Some(a)) => format!("{n} <{a}>"),
                    (Some(n), None) => n.into(),
                    (_, Some(a)) => a.into(),
                    _ => String::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}
pub(crate) fn parse_eml(bytes: &[u8]) -> Result<Mail> {
    if bytes.len() as u64 > EML_LIMIT {
        return Err(error("FILE_TOO_LARGE", "EML exceeds 64 MiB"));
    }
    let message = MessageParser::default()
        .parse(bytes)
        .ok_or_else(|| error("DOCUMENT_INVALID", "Cannot parse EML"))?;
    let body_source = match message.text_part(0).map(|p| &p.body) {
        Some(PartType::Text(_)) => "text/plain",
        Some(PartType::Html(_)) => "text/html",
        _ => "none",
    };
    let mut body = String::new();
    for (i, part) in (0..message.text_body.len())
        .filter_map(|i| message.body_text(i))
        .enumerate()
    {
        if i > 0 {
            body.push_str("\n\n");
        }
        body.push_str(&part);
    }
    let attachments = message
        .attachments()
        .enumerate()
        .map(|(i, part)| attachment_info(part, i + 1))
        .collect();
    Ok(Mail {
        subject: message.subject().map(str::to_owned),
        from: addresses(message.from()).into_iter().next(),
        to: addresses(message.to()),
        cc: addresses(message.cc()),
        date: message.date().map(|d| d.to_rfc3339()),
        body,
        body_source,
        attachments,
    })
}
fn attachment_info(part: &mail_parser::MessagePart<'_>, index: usize) -> AttachmentInfo {
    AttachmentInfo {
        index,
        name: part.attachment_name().unwrap_or("attachment").to_owned(),
        mime: part
            .content_type()
            .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")))
            .unwrap_or_else(|| "application/octet-stream".into()),
        size: part.contents().len() as u64,
        inline: part.content_disposition().is_some_and(|c| c.is_inline())
            || part.content_id().is_some(),
        embedded_message: part.is_message(),
    }
}
pub(crate) fn eml_attachment(bytes: &[u8], index: usize) -> Result<(AttachmentInfo, Vec<u8>)> {
    if bytes.len() as u64 > EML_LIMIT {
        return Err(error("FILE_TOO_LARGE", "EML exceeds 64 MiB"));
    }
    let message = MessageParser::default()
        .parse(bytes)
        .ok_or_else(|| error("DOCUMENT_INVALID", "Cannot parse EML"))?;
    let part = index
        .checked_sub(1)
        .and_then(|i| u32::try_from(i).ok())
        .and_then(|i| message.attachment(i))
        .ok_or_else(|| error("MEMBER_NOT_FOUND", "Attachment not found"))?;
    let info = attachment_info(part, index);
    crate::documents::check_attachment(&info)?;
    Ok((info, part.contents().to_vec()))
}
pub(crate) struct MailDocument {
    blocks: Vec<Unit>,
    summary: Value,
}
pub(crate) fn mail_document(mut mail: Mail) -> MailDocument {
    if mail.body.contains("\r\n") {
        mail.body = mail.body.replace("\r\n", "\n");
    }
    let mut headers = Vec::new();
    if let Some(s) = &mail.subject {
        headers.push(format!("Subject: {s}"));
    }
    if let Some(s) = &mail.from {
        headers.push(format!("From: {s}"));
    }
    if !mail.to.is_empty() {
        headers.push(format!("To: {}", mail.to.join(", ")));
    }
    if !mail.cc.is_empty() {
        headers.push(format!("Cc: {}", mail.cc.join(", ")));
    }
    if let Some(s) = &mail.date {
        headers.push(format!("Date: {s}"));
    }
    let mut blocks = vec![Unit {
        locator: json!({"part":"headers"}),
        heading: Some("Mail headers".into()),
        text: headers.join("\n"),
    }];
    if mail.body_source == "rtf_only" {
        blocks.push(Unit {
            locator: json!({"part":"body","source":"rtf_only"}),
            heading: None,
            text: "body only in RTF; save as EML or a text copy".into(),
        });
    } else {
        // Paragraph boundaries remain stable across cursor calls and CRLF/LF bodies.
        for (i, text) in mail
            .body
            .split("\n\n")
            .filter(|s| !s.is_empty())
            .enumerate()
        {
            blocks.push(Unit {
                locator: json!({"part":"body","paragraph":i+1,"source":mail.body_source}),
                heading: (i == 0).then(|| format!("Mail body · {}", mail.body_source)),
                text: text.into(),
            });
        }
    }
    if !mail.attachments.is_empty() {
        blocks.push(Unit {
            locator: json!({"part":"attachments"}),
            heading: Some("Attachments (metadata only)".into()),
            text: mail
                .attachments
                .iter()
                .map(|a| {
                    format!(
                        "[{}] {} · {} · {} bytes{}{}",
                        a.index,
                        a.name,
                        a.mime,
                        a.size,
                        if a.inline { " · inline" } else { "" },
                        if a.embedded_message {
                            " · embedded message"
                        } else {
                            ""
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }
    MailDocument {
        blocks,
        summary: json!({"subject":mail.subject,"from":mail.from,"to":mail.to,"cc":mail.cc,"date":mail.date,"bodySource":mail.body_source,"attachments":mail.attachments}),
    }
}
impl TextDocument for MailDocument {
    fn unit_name(&self) -> &'static str {
        "part"
    }
    fn unit_count(&self) -> usize {
        self.blocks.len()
    }
    fn unit(&mut self, index: usize) -> Result<Unit> {
        let u = self
            .blocks
            .get(index)
            .ok_or_else(|| error("INVALID_CURSOR", "Invalid mail part"))?;
        Ok(Unit {
            locator: u.locator.clone(),
            heading: u.heading.clone(),
            text: u.text.clone(),
        })
    }
    fn summary(&self) -> Value {
        self.summary.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachment_bytes_and_embedded_eml_are_read_by_index() {
        let raw = b"Subject: parent\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: text/plain\r\n\r\nbody\r\n--m\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=x.txt\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--m\r\nContent-Type: message/rfc822\r\nContent-Disposition: attachment; filename=message.bin\r\n\r\nSubject: nested\r\n\r\nnested body\r\n--m--\r\n";
        let (info, bytes) = eml_attachment(raw, 1).unwrap();
        assert_eq!(info.name, "x.txt");
        assert_eq!(bytes, b"hello");
        let (info, bytes) = eml_attachment(raw, 2).unwrap();
        assert!(info.embedded_message);
        assert_eq!(
            parse_eml(&bytes).unwrap().subject.as_deref(),
            Some("nested")
        );
        for index in [0, 3, usize::MAX] {
            assert_eq!(
                eml_attachment(raw, index).err().unwrap().code,
                "MEMBER_NOT_FOUND"
            );
        }
        let blocked = String::from_utf8_lossy(raw).replace("x.txt", ".env");
        assert_eq!(
            eml_attachment(blocked.as_bytes(), 1).err().unwrap().code,
            "BLOCKED_PATH"
        );
    }
    #[test]
    fn russian_charsets_and_encoded_headers() {
        for (charset, enc) in [
            ("koi8-r", encoding_rs::KOI8_R),
            ("windows-1251", encoding_rs::WINDOWS_1251),
            ("ibm866", encoding_rs::IBM866),
            ("iso-8859-5", encoding_rs::ISO_8859_5),
        ] {
            let (bytes, _, bad) = enc.encode("Привет мир");
            assert!(!bad);
            let qp = bytes
                .iter()
                .map(|b| format!("={b:02X}"))
                .collect::<String>();
            let raw = format!(
                "Subject: =?{charset}?Q?{qp}?=\r\nFrom: Sender <sender@example.test>\r\nTo: User <user@example.test>\r\nDate: Sat, 17 Oct 2026 12:00:00 +0000\r\nContent-Type: text/plain; charset={charset}\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{qp}"
            );
            let mail = parse_eml(raw.as_bytes()).unwrap();
            assert_eq!(mail.subject.as_deref(), Some("Привет мир"));
            assert_eq!(mail.body, "Привет мир");
            assert_eq!(mail.body_source, "text/plain");
            assert_eq!(mail.date.as_deref(), Some("2026-10-17T12:00:00Z"));
        }
    }
    #[test]
    fn mixed_selected_bodies_keep_order_and_convert_html() {
        for (mime, second) in [
            ("text/plain", "second text"),
            ("text/html", "<p>second text</p>"),
        ] {
            let raw = format!(
                "Subject: mixed\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nfirst text\r\n--m\r\nContent-Type: image/png\r\nContent-Disposition: inline; filename=pixel.png\r\nContent-ID: <pixel>\r\nContent-Transfer-Encoding: base64\r\n\r\nU0VDUkVUX0FUVEFDSE1FTlQ=\r\n--m\r\nContent-Type: {mime}; charset=utf-8\r\n\r\n{second}\r\n--m--\r\n"
            );
            let mail = parse_eml(raw.as_bytes()).unwrap();
            assert!(
                mail.body.contains("second text"),
                "missing second {mime} body: {:?}",
                mail.body
            );
            assert!(mail.body.find("first text").unwrap() < mail.body.find("second text").unwrap());
            assert!(!mail.body.contains("<p>"));
            assert!(!mail.body.contains("SECRET_ATTACHMENT"));
            assert_eq!(mail.attachments.len(), 1);
            assert_eq!(mail.attachments[0].name, "pixel.png");
            assert!(mail.attachments[0].inline);
            let mut doc = mail_document(mail);
            assert_eq!(doc.unit(1).unwrap().text.trim(), "first text");
            assert_eq!(doc.unit(2).unwrap().text.trim(), "second text");
            assert_eq!(doc.unit(2).unwrap().locator["paragraph"], 2);
        }
    }
    #[test]
    fn multipart_alternative_body_is_selected_once() {
        let raw = b"MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=a\r\n\r\n--a\r\nContent-Type: text/plain\r\n\r\nsame content\r\n--a\r\nContent-Type: text/html\r\n\r\n<p>same content</p>\r\n--a--\r\n";
        let mail = parse_eml(raw).unwrap();
        assert_eq!(mail.body.matches("same content").count(), 1);
        assert_eq!(mail.body_source, "text/plain");
        assert!(mail.attachments.is_empty());
        let mut doc = mail_document(mail);
        assert_eq!(doc.unit_count(), 2);
        assert_eq!(doc.unit(1).unwrap().text.trim(), "same content");
    }
    #[test]
    fn nested_mime_lists_inline_and_regular_attachments() {
        let raw=b"Subject: mixed\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: multipart/related; boundary=r\r\n\r\n--r\r\nContent-Type: multipart/alternative; boundary=a\r\n\r\n--a\r\nContent-Type: text/plain\r\n\r\nplain body\r\n--a\r\nContent-Type: text/html\r\n\r\n<p>html body</p>\r\n--a--\r\n--r\r\nContent-Type: image/png\r\nContent-ID: <pixel>\r\nContent-Disposition: inline; filename=pixel.png\r\nContent-Transfer-Encoding: base64\r\n\r\nAQID\r\n--r--\r\n--m\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=test.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nBAUGBw==\r\n--m--\r\n";
        let mail = parse_eml(raw).unwrap();
        assert_eq!(mail.body.trim(), "plain body");
        assert_eq!(mail.attachments.len(), 2);
        assert_eq!(mail.attachments[0].index, 1);
        assert!(mail.attachments[0].inline);
        assert_eq!(mail.attachments[0].size, 3);
        assert_eq!(mail.attachments[1].name, "test.pdf");
        assert_eq!(mail.attachments[1].size, 4);
        let mut doc = mail_document(mail);
        assert_eq!(doc.unit(0).unwrap().locator, json!({"part":"headers"}));
        assert!(
            doc.unit(doc.unit_count() - 1)
                .unwrap()
                .text
                .contains("test.pdf")
        );
        let mail =
            parse_eml(b"Content-Type: text/html; charset=utf-8\r\n\r\n<p>Hello <b>world</b></p>")
                .unwrap();
        assert_eq!(mail.body_source, "text/html");
        assert!(mail.body.contains("Hello world"));
    }
}
