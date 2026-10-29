use crate::{
    documents::{Source, TextDocument, Unit},
    filesystem::{Result, error},
};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
    path::{Component, Path},
};

pub(crate) const DOCX_LIMIT: u64 = 256 * 1024 * 1024;
pub(crate) const DOCX_PART_LIMIT: u64 = 64 * 1024 * 1024;

fn invalid() -> crate::filesystem::FsError {
    error("DOCUMENT_INVALID", "Cannot parse DOCX")
}
fn attr(tag: &BytesStart<'_>, name: &[u8]) -> Result<Option<String>> {
    for a in tag.attributes() {
        let a = a.map_err(|_| invalid())?;
        if a.key.local_name().as_ref() == name {
            let value = std::str::from_utf8(&a.value).map_err(|_| invalid())?;
            return Ok(Some(
                quick_xml::escape::unescape(value)
                    .map_err(|_| invalid())?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}
fn part(zip: &mut zip::ZipArchive<Source>, name: &str) -> Result<Vec<u8>> {
    let mut file = zip.by_name(name).map_err(|_| invalid())?;
    if file.size() > DOCX_PART_LIMIT {
        return Err(error("FILE_TOO_LARGE", "DOCX part exceeds 64 MiB"));
    }
    let mut bytes = Vec::with_capacity(file.size() as usize + 1);
    (&mut file)
        .take(DOCX_PART_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > DOCX_PART_LIMIT {
        return Err(error("FILE_TOO_LARGE", "DOCX part exceeds 64 MiB"));
    }
    Ok(bytes)
}
// Relationships select ZIP entries only; they are never resolved on the host filesystem.
fn target(base: &str, raw: &str) -> Result<String> {
    let raw = raw.replace('\\', "/");
    let joined = if raw.starts_with('/') {
        raw.trim_start_matches('/').to_owned()
    } else {
        format!("{base}{raw}")
    };
    let mut components = Vec::new();
    for c in Path::new(&joined).components() {
        match c {
            Component::Normal(n) => components.push(n.to_str().ok_or_else(invalid)?),
            Component::ParentDir => {
                components.pop().ok_or_else(invalid)?;
            }
            Component::CurDir => {}
            _ => return Err(invalid()),
        }
    }
    Ok(components.join("/"))
}
fn relationship(bytes: &[u8], suffix: &str) -> Result<Option<String>> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf).map_err(|_| invalid())? {
            Event::Start(t) | Event::Empty(t) if t.local_name().as_ref() == b"Relationship" => {
                if attr(&t, b"Type")?.is_some_and(|t| t.ends_with(suffix)) {
                    if attr(&t, b"TargetMode")?.as_deref() == Some("External") {
                        return Err(invalid());
                    }
                    return attr(&t, b"Target");
                }
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
        buf.clear();
    }
}
fn text_event(event: &Event<'_>) -> Result<Option<String>> {
    Ok(match event {
        Event::Text(t) => Some(t.xml10_content().map_err(|_| invalid())?.into_owned()),
        Event::GeneralRef(r) => {
            let name = r.decode().map_err(|_| invalid())?;
            Some(
                quick_xml::escape::unescape(&format!("&{name};"))
                    .map_err(|_| invalid())?
                    .into_owned(),
            )
        }
        Event::CData(t) => Some(t.decode().map_err(|_| invalid())?.into_owned()),
        _ => None,
    })
}

pub(crate) struct DocxDocument {
    blocks: Vec<Unit>,
    summary: Value,
}
impl TextDocument for DocxDocument {
    fn unit_name(&self) -> &'static str {
        "block"
    }
    fn unit_count(&self) -> usize {
        self.blocks.len()
    }
    fn unit(&mut self, index: usize) -> Result<Unit> {
        let u = self.blocks.get(index).ok_or_else(invalid)?;
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
#[derive(Default)]
struct Table {
    number: usize,
    row: usize,
    cells: Vec<String>,
    cell: String,
    paragraphs: Vec<usize>,
}

fn document(bytes: &[u8]) -> Result<DocxDocument> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    let mut buf = Vec::new();
    let mut blocks = Vec::new();
    let mut tables = Vec::<Table>::new();
    let (mut paragraphs, mut table_count, mut insertions, mut deletions) =
        (0usize, 0usize, 0usize, 0usize);
    let mut authors = BTreeMap::<String, usize>::new();
    let mut paragraph = String::new();
    let mut in_paragraph = false;
    let mut in_body = false;
    let mut saw_body = false;
    let mut text_box_depth = 0usize;
    let mut in_text = false;
    let mut revision = Vec::<&str>::new();
    let mut suppressed_revisions = 0usize;
    let mut property_depth = 0usize;
    loop {
        let event = reader.read_event_into(&mut buf).map_err(|_| invalid())?;
        // Text boxes are outside C10's body paragraph/table reader. Skip iteratively,
        // so their nested paragraphs cannot replace an enclosing body paragraph.
        if text_box_depth > 0 {
            match event {
                Event::Start(_) => text_box_depth += 1,
                Event::End(_) => text_box_depth -= 1,
                Event::Eof => return Err(invalid()),
                _ => {}
            }
            buf.clear();
            continue;
        }
        if matches!(&event, Event::Start(t) if t.local_name().as_ref() == b"txbxContent") {
            text_box_depth = 1;
            buf.clear();
            continue;
        }
        match &event {
            Event::Start(t) | Event::Empty(t) => {
                let empty = matches!(event, Event::Empty(_));
                let local = t.local_name();
                match local.as_ref() {
                    b"body" => {
                        saw_body = true;
                        in_body = !empty;
                    }
                    b"p" if in_body => {
                        if in_paragraph {
                            return Err(invalid());
                        }
                        paragraphs += 1;
                        in_paragraph = true;
                        paragraph.clear();
                        for mark in &revision {
                            paragraph.push_str(mark);
                        }
                        if let Some(table) = tables.last_mut() {
                            table.paragraphs.push(paragraphs);
                        }
                    }
                    b"tbl" if in_body => {
                        table_count += 1;
                        tables.push(Table {
                            number: table_count,
                            ..Default::default()
                        });
                    }
                    b"tr" if in_body => {
                        if let Some(table) = tables.last_mut() {
                            table.row += 1;
                            table.cells.clear();
                            table.paragraphs.clear();
                        }
                    }
                    b"tc" if in_body => {
                        if let Some(table) = tables.last_mut() {
                            table.cell.clear();
                        }
                    }
                    b"t" | b"delText" if in_paragraph => in_text = !empty,
                    b"tab" if in_paragraph => paragraph.push('\t'),
                    b"br" | b"cr" if in_paragraph => paragraph.push('\n'),
                    b"noBreakHyphen" if in_paragraph => paragraph.push('-'),
                    b"pPr" | b"rPr" | b"trPr" if !empty => property_depth += 1,
                    b"ins" | b"moveTo" | b"del" | b"moveFrom" if in_body && property_depth == 0 => {
                        let insertion = matches!(local.as_ref(), b"ins" | b"moveTo");
                        if insertion {
                            insertions += 1;
                        } else {
                            deletions += 1;
                        }
                        if let Some(author) = attr(t, b"author")? {
                            *authors.entry(author).or_default() += 1;
                        }
                        // Keep parsing/counting deeper marks without replaying them
                        // around every later paragraph. Ends unwind suppression first.
                        if revision.len() >= 8 {
                            if !empty {
                                suppressed_revisions += 1;
                            }
                            buf.clear();
                            continue;
                        }
                        let mark = if insertion { "{++" } else { "{--" };
                        if in_paragraph {
                            paragraph.push_str(mark);
                        }
                        if !empty {
                            revision.push(mark);
                        } else if in_paragraph {
                            paragraph.push_str(if insertion { "++}" } else { "--}" });
                        }
                    }
                    b"commentRangeStart" if in_paragraph => paragraph.push_str("{=="),
                    b"commentRangeEnd" if in_paragraph => {
                        paragraph
                            .push_str(&format!("==}}[c{}]", attr(t, b"id")?.ok_or_else(invalid)?));
                    }
                    _ => {}
                }
                if empty && local.as_ref() == b"p" && in_body {
                    finish_paragraph(
                        &mut blocks,
                        &mut tables,
                        paragraphs,
                        &mut paragraph,
                        &revision,
                    );
                    in_paragraph = false;
                }
            }
            Event::End(t) => match t.local_name().as_ref() {
                b"body" => in_body = false,
                b"t" | b"delText" => in_text = false,
                b"pPr" | b"rPr" | b"trPr" => property_depth = property_depth.saturating_sub(1),
                b"p" if in_body => {
                    finish_paragraph(
                        &mut blocks,
                        &mut tables,
                        paragraphs,
                        &mut paragraph,
                        &revision,
                    );
                    in_paragraph = false;
                }
                b"tc" => {
                    if let Some(table) = tables.last_mut() {
                        table.cells.push(std::mem::take(&mut table.cell));
                    }
                }
                b"tr" => {
                    if let Some(table) = tables.last_mut() {
                        let text = format!(
                            "[table {}, row {}] | {} |",
                            table.number,
                            table.row,
                            table.cells.join(" | ")
                        );
                        blocks.push(Unit { locator: json!({"table":table.number,"row":table.row,"paragraphs":table.paragraphs}), heading: None, text });
                    }
                }
                b"tbl" => {
                    tables.pop().ok_or_else(invalid)?;
                }
                b"ins" | b"moveTo" | b"del" | b"moveFrom" if in_body && property_depth == 0 => {
                    if suppressed_revisions > 0 {
                        suppressed_revisions -= 1;
                        buf.clear();
                        continue;
                    }
                    let mark = revision.pop().ok_or_else(invalid)?;
                    if in_paragraph {
                        paragraph.push_str(if mark == "{++" { "++}" } else { "--}" });
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ if in_text => {
                if let Some(text) = text_event(&event)? {
                    paragraph.push_str(&text);
                }
            }
            _ => {}
        }
        buf.clear();
    }
    if !saw_body
        || in_body
        || in_paragraph
        || !tables.is_empty()
        || !revision.is_empty()
        || suppressed_revisions != 0
    {
        return Err(invalid());
    }
    Ok(DocxDocument {
        blocks,
        summary: json!({"paragraphs":paragraphs,"tables":table_count,"comments":0,"insertions":insertions,"deletions":deletions,"revisionAuthors":authors}),
    })
}
fn finish_paragraph(
    blocks: &mut Vec<Unit>,
    tables: &mut [Table],
    number: usize,
    text: &mut String,
    revision: &[&str],
) {
    for mark in revision.iter().rev() {
        text.push_str(if *mark == "{++" { "++}" } else { "--}" });
    }
    if let Some(table) = tables.last_mut() {
        if !table.cell.is_empty() {
            table.cell.push('\n');
        }
        table.cell.push_str(text);
    } else {
        blocks.push(Unit {
            locator: json!({"paragraph":number}),
            heading: None,
            text: format!("¶{number}: {text}"),
        });
    }
    text.clear();
}
fn comments(bytes: &[u8], doc: &mut DocxDocument) -> Result<()> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    let mut buf = Vec::new();
    let mut current = None::<(String, String, String, String)>;
    let mut in_text = false;
    let mut count = 0;
    loop {
        let event = reader.read_event_into(&mut buf).map_err(|_| invalid())?;
        match &event {
            Event::Start(t) | Event::Empty(t) => match t.local_name().as_ref() {
                b"comment" => {
                    current = Some((
                        attr(t, b"id")?.ok_or_else(invalid)?,
                        attr(t, b"author")?.unwrap_or_default(),
                        attr(t, b"date")?.unwrap_or_default(),
                        String::new(),
                    ));
                    if matches!(event, Event::Empty(_)) {
                        doc.blocks
                            .push(comment_unit(current.take().unwrap(), count));
                        count += 1;
                    }
                }
                b"t" => in_text = !matches!(event, Event::Empty(_)),
                b"tab" => {
                    if let Some(c) = &mut current {
                        c.3.push('\t');
                    }
                }
                b"br" | b"cr" => {
                    if let Some(c) = &mut current {
                        c.3.push('\n');
                    }
                }
                _ => {}
            },
            Event::End(t) => match t.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" => {
                    if let Some(c) = &mut current {
                        c.3.push('\n');
                    }
                }
                b"comment" => {
                    doc.blocks
                        .push(comment_unit(current.take().ok_or_else(invalid)?, count));
                    count += 1;
                }
                _ => {}
            },
            Event::Eof => break,
            _ if in_text => {
                if let Some(text) = text_event(&event)?
                    && let Some(c) = &mut current
                {
                    c.3.push_str(&text);
                }
            }
            _ => {}
        }
        buf.clear();
    }
    if current.is_some() {
        return Err(invalid());
    }
    doc.summary["comments"] = json!(count);
    Ok(())
}
fn comment_unit((id, author, date, text): (String, String, String, String), index: usize) -> Unit {
    Unit {
        locator: json!({"comment":id}),
        heading: (index == 0).then(|| "Comments".into()),
        text: format!(
            "[c{id}] {author} ({}): {}",
            date.get(..10).unwrap_or(&date),
            text.trim_end()
        ),
    }
}
pub(crate) fn open(source: Source) -> Result<DocxDocument> {
    let mut zip = zip::ZipArchive::new(source).map_err(|_| invalid())?;
    let main = if zip.index_for_name("_rels/.rels").is_some() {
        relationship(&part(&mut zip, "_rels/.rels")?, "/officeDocument")?
            .map(|p| target("", &p))
            .transpose()?
            .unwrap_or_else(|| "word/document.xml".into())
    } else {
        "word/document.xml".into()
    };
    let mut doc = document(&part(&mut zip, &main)?)?;
    let (base, name) = main
        .rsplit_once('/')
        .map_or((String::new(), main.as_str()), |(base, name)| {
            (format!("{base}/"), name)
        });
    let rels = format!("{base}_rels/{name}.rels");
    if zip.index_for_name(&rels).is_some()
        && let Some(comments_part) = relationship(&part(&mut zip, &rels)?, "/comments")?
    {
        comments(&part(&mut zip, &target(&base, &comments_part)?)?, &mut doc)?;
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::io::Write;
    fn zip(parts: &[(&str, &str)]) -> Source {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in parts {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        Source::Memory(zip.finish().unwrap())
    }
    #[test]
    fn revisions_tables_entities_and_comments_keep_locations() {
        let mut doc=open(zip(&[
            ("_rels/.rels",r#"<Relationships><Relationship Type="x/officeDocument" Target="word/custom.xml"/></Relationships>"#),
            ("word/_rels/custom.xml.rels",r#"<Relationships><Relationship Type="x/comments" Target="comments.xml"/></Relationships>"#),
            ("word/custom.xml",r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>A &amp; B &#x1F600;</w:t><w:tab/><w:instrText>HIDDEN FIELD</w:instrText></w:r><w:ins w:author="Иван"><w:r><w:t>новое</w:t></w:r></w:ins><w:del w:author="Анна"><w:r><w:delText>старое</w:delText></w:r></w:del><w:commentRangeStart w:id="7"/><w:r><w:t>спор</w:t></w:r><w:commentRangeEnd w:id="7"/></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>один</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>два</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:moveTo w:author="Иван"><w:r><w:t>сюда</w:t></w:r></w:moveTo><w:moveFrom><w:r><w:delText>оттуда</w:delText></w:r></w:moveFrom></w:p></w:body></w:document>"#),
            ("word/comments.xml",r#"<w:comments xmlns:w="w"><w:comment w:id="7" w:author="Юрист" w:date="2026-10-17T12:00:00Z"><w:p><w:r><w:t>Проверить &amp; уточнить</w:t></w:r></w:p></w:comment></w:comments>"#),
        ])).unwrap();
        assert_eq!(doc.unit_count(), 4);
        assert_eq!(
            doc.unit(0).unwrap().text,
            "¶1: A & B 😀\t{++новое++}{--старое--}{==спор==}[c7]"
        );
        assert_eq!(doc.unit(1).unwrap().text, "[table 1, row 1] | один | два |");
        assert_eq!(
            doc.unit(1).unwrap().locator,
            json!({"table":1,"row":1,"paragraphs":[2,3]})
        );
        assert_eq!(doc.unit(2).unwrap().text, "¶4: {++сюда++}{--оттуда--}");
        assert_eq!(
            doc.unit(3).unwrap().text,
            "[c7] Юрист (2026-10-17): Проверить & уточнить"
        );
        assert_eq!(
            doc.summary(),
            json!({"paragraphs":4,"tables":1,"comments":1,"insertions":2,"deletions":2,"revisionAuthors":{"Иван":2,"Анна":1}})
        );
    }
    #[test]
    fn deep_body_revisions_keep_output_bounded_and_resume_run_marks() {
        let depth = 3000;
        let mut xml = String::from("<w:body>");
        for _ in 0..depth {
            xml.push_str("<w:ins><w:del>");
        }
        for i in 0..100 {
            write!(xml, "<w:p><w:r><w:t>text{i}</w:t></w:r></w:p>").unwrap();
        }
        for _ in 0..depth {
            xml.push_str("</w:del></w:ins>");
        }
        xml.push_str("<w:p><w:ins><w:r><w:t>new</w:t></w:r></w:ins><w:del><w:r><w:delText>old</w:delText></w:r></w:del></w:p></w:body>");
        let mut doc = document(xml.as_bytes()).unwrap();
        assert_eq!(doc.unit_count(), 101);
        let output: String = (0..doc.unit_count())
            .map(|i| doc.unit(i).unwrap().text)
            .collect();
        assert!(output.len() < 10_000, "rendered {} bytes", output.len());
        assert!(output.contains("text0"));
        assert!(output.contains("text99"));
        assert_eq!(doc.unit(100).unwrap().text, "¶101: {++new++}{--old--}");
        assert_eq!(doc.summary()["insertions"], depth + 1);
        assert_eq!(doc.summary()["deletions"], depth + 1);
    }
    #[test]
    fn revision_depth_cap_handles_aliases_and_empty_marks() {
        let tags = ["ins", "del", "moveTo", "moveFrom"];
        let mut xml = String::from("<w:body><w:p>");
        for i in 0..12 {
            write!(xml, "<w:{}>", tags[i % 4]).unwrap();
        }
        // Empty marks above the cap must not render or unbalance the open marks.
        xml.push_str("<w:ins/><w:del/><w:moveTo/><w:moveFrom/><w:r><w:t>deep</w:t></w:r>");
        for i in (0..12).rev() {
            write!(xml, "</w:{}>", tags[i % 4]).unwrap();
        }
        xml.push_str("<w:ins><w:r><w:t>next</w:t></w:r></w:ins></w:p></w:body>");
        let mut doc = document(xml.as_bytes()).unwrap();
        let text = doc.unit(0).unwrap().text;
        assert_eq!(text.matches("{++").count(), 5);
        assert_eq!(text.matches("{--").count(), 4);
        assert_eq!(text.matches("++}").count(), 5);
        assert_eq!(text.matches("--}").count(), 4);
        assert!(text.ends_with("{++next++}"));
        assert!(text.contains("deep"));
        assert_eq!(doc.summary()["insertions"], 9);
        assert_eq!(doc.summary()["deletions"], 8);
    }
    #[test]
    fn paragraph_mark_revisions_do_not_render_or_double_counts() {
        let mut doc = document("<w:body><w:p><w:pPr><w:rPr><w:ins w:author=\"A\"/><w:del w:author=\"B\"></w:del></w:rPr></w:pPr><w:ins w:author=\"A\"><w:r><w:rPr><w:ins/></w:rPr><w:t>Новый абзац</w:t></w:r></w:ins></w:p></w:body>".as_bytes()).unwrap();
        assert_eq!(doc.unit(0).unwrap().text, "¶1: {++Новый абзац++}");
        assert_eq!(doc.summary()["insertions"], 1);
        assert_eq!(doc.summary()["deletions"], 0);
        assert_eq!(doc.summary()["revisionAuthors"], json!({"A":1}));
    }
    #[test]
    fn table_row_property_revisions_do_not_double_content_counts() {
        for (tag, text_tag, text, expected, insertions, deletions) in [
            ("ins", "t", "Новая строка", "{++Новая строка++}", 1, 0),
            (
                "del",
                "delText",
                "Старая строка",
                "{--Старая строка--}",
                0,
                1,
            ),
        ] {
            let xml = format!(
                "<w:body><w:tbl><w:tr><w:trPr><w:{tag} w:author=\"A\"/></w:trPr><w:tc><w:p><w:{tag} w:author=\"A\"><w:r><w:{text_tag}>{text}</w:{text_tag}></w:r></w:{tag}></w:p></w:tc></w:tr></w:tbl></w:body>"
            );
            let mut doc = document(xml.as_bytes()).unwrap();
            assert_eq!(
                doc.unit(0).unwrap().text,
                format!("[table 1, row 1] | {expected} |")
            );
            assert_eq!(doc.summary()["insertions"], insertions);
            assert_eq!(doc.summary()["deletions"], deletions);
            assert_eq!(doc.summary()["revisionAuthors"], json!({"A":1}));
        }
    }
    #[test]
    fn invalid_and_external_relationships_are_not_resolved() {
        for source in [
            zip(&[("word/document.xml", "<w:body><w:p>")]),
            zip(&[(
                "_rels/.rels",
                r#"<Relationships><Relationship Type="x/officeDocument" TargetMode="External" Target="file:///private/secret"/></Relationships>"#,
            )]),
        ] {
            let e = open(source).err().unwrap();
            assert_eq!(e.code, "DOCUMENT_INVALID");
            assert!(!e.message.contains("private"));
        }
    }
    #[test]
    fn empty_body_comments_and_out_of_scope_text_boxes() {
        assert_eq!(
            document(b"<w:document><w:body/></w:document>")
                .unwrap()
                .unit_count(),
            0
        );
        assert_eq!(
            document(b"<w:document/>").err().unwrap().code,
            "DOCUMENT_INVALID"
        );
        let mut doc=document(b"<w:body><w:p><w:r><w:t>before</w:t></w:r><w:txbxContent><w:p><w:r><w:t>box</w:t></w:r></w:p></w:txbxContent><w:r><w:t>after</w:t></w:r></w:p></w:body>").unwrap();
        assert_eq!(doc.unit(0).unwrap().text, "\u{b6}1: beforeafter");
        comments(
            br#"<w:comments><w:comment w:id="2" w:author="A"/></w:comments>"#,
            &mut doc,
        )
        .unwrap();
        assert_eq!(doc.summary()["comments"], 1);
        assert!(doc.unit(1).unwrap().text.starts_with("[c2] A"));
    }
    #[test]
    fn part_limit_precedes_inflation() {
        let mut source = zip(&[("word/document.xml", "<w:document/>")]);
        let Source::Memory(ref mut cursor) = source else {
            unreachable!()
        };
        let bytes = cursor.get_mut();
        let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        bytes[central + 24..central + 28]
            .copy_from_slice(&(DOCX_PART_LIMIT as u32 + 1).to_le_bytes());
        assert_eq!(open(source).err().unwrap().code, "FILE_TOO_LARGE");
    }
}
