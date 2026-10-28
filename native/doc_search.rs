use crate::{
    documents::TextDocument,
    filesystem::{Result, error},
};
use regex::Regex;
use serde_json::Value;
use std::time::Instant;

pub(crate) const DOC_SCAN_LIMIT: u64 = 512 * 1024 * 1024;
pub(crate) const SEARCH_DOC_FILE_LIMIT: u64 = 64 * 1024 * 1024;
#[cfg(test)]
thread_local! { pub(crate) static TEST_SCAN_BUDGET: std::cell::Cell<Option<(u64,u64)>> = const { std::cell::Cell::new(None) }; }
pub(crate) struct DocHit {
    pub locator: Value,
    pub snippet: String,
}
fn snippet(text: &str, found: regex::Match<'_>, locator: &Value) -> String {
    let mut from = found.start().saturating_sub(360);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let mut to = found.end().saturating_add(640).min(text.len());
    while !text.is_char_boundary(to) {
        to -= 1;
    }
    let prefix = if let Some(page) = locator.get("page") {
        format!("[p. {page}] ")
    } else if let (Some(sheet), Some(row)) = (locator["sheet"].as_str(), locator.get("row")) {
        format!("[sheet «{sheet}» row {row}] ")
    } else if let Some(paragraph) = locator
        .get("paragraph")
        .filter(|_| locator.get("part").is_none())
    {
        format!("[¶{paragraph}] ")
    } else if let Some(paragraph) = locator
        .get("paragraphs")
        .and_then(Value::as_array)
        .and_then(|p| p.first())
    {
        format!("[¶{paragraph}] ")
    } else if locator.get("part").is_some() {
        "[mail] ".into()
    } else {
        String::new()
    };
    format!(
        "{prefix}{}",
        text[from..to]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    )
}
type FindResult = (Vec<DocHit>, Option<(usize, usize)>);

pub(crate) fn find_in_document(
    doc: &mut dyn TextDocument,
    matcher: &Regex,
    start_unit: usize,
    skip_in_unit: usize,
    max_hits: usize,
    deadline: Instant,
) -> Result<FindResult> {
    #[cfg(test)]
    let deadline = crate::documents::TEST_DEADLINE.with(|c| c.get().unwrap_or(deadline));
    crate::documents::guarded(|| {
        if start_unit > doc.unit_count() || (start_unit == doc.unit_count() && skip_in_unit != 0) {
            return Err(error("INVALID_CURSOR", "Invalid search unit"));
        }
        let mut hits = Vec::new();
        for u in start_unit..doc.unit_count() {
            let unit = if u == start_unit {
                doc.unit(u)?
            } else {
                if Instant::now() >= deadline {
                    return Ok((hits, Some((u, 0))));
                }
                let Some(unit) = doc.unit_within(u, Some(deadline))? else {
                    return Ok((hits, Some((u, 0))));
                };
                unit
            };
            let skip = if u == start_unit { skip_in_unit } else { 0 };
            let mut count = 0;
            for found in matcher.find_iter(&unit.text) {
                count += 1;
                if count <= skip {
                    continue;
                }
                if hits.len() >= max_hits {
                    return Ok((hits, Some((u, count - 1))));
                }
                hits.push(DocHit {
                    snippet: snippet(&unit.text, found, &unit.locator),
                    locator: unit.locator.clone(),
                });
            }
            if count < skip {
                return Err(error("INVALID_CURSOR", "Invalid search hit offset"));
            }
            if u + 1 < doc.unit_count() && (hits.len() >= max_hits || Instant::now() >= deadline) {
                return Ok((hits, Some((u + 1, 0))));
            }
        }
        Ok((hits, None))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Config, Root},
        cursor::{self, CursorKind},
        documents::{self, Format, Location, Opened, Source, Unit},
        filesystem::Filesystem,
    };
    use serde_json::json;
    use std::{
        io::{Cursor, Write},
        path::PathBuf,
        time::Duration,
    };

    fn zip(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in parts {
            writer
                .start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(text.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    fn docx(text: &str) -> Vec<u8> {
        zip(&[(
            "word/document.xml",
            &format!("<w:document xmlns:w='w'><w:body>{text}</w:body></w:document>"),
        )])
    }
    fn paragraph(text: &str) -> String {
        format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
    }
    fn xlsx() -> Vec<u8> {
        zip(&[
            (
                "[Content_Types].xml",
                r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row><c r="A1" t="inlineStr"><is><t>Needle Needle</t></is></c></row></sheetData></worksheet>"#,
            ),
        ])
    }
    fn xls() -> Vec<u8> {
        fn rec(out: &mut Vec<u8>, tag: u16, data: &[u8]) {
            out.extend(tag.to_le_bytes());
            out.extend((data.len() as u16).to_le_bytes());
            out.extend(data);
        }
        fn bof(out: &mut Vec<u8>, kind: u16) {
            let mut data = vec![0; 16];
            data[..2].copy_from_slice(&0x0600u16.to_le_bytes());
            data[2..4].copy_from_slice(&kind.to_le_bytes());
            rec(out, 0x0809, &data);
        }
        let mut out = Vec::new();
        bof(&mut out, 5);
        let patch = out.len() + 4;
        rec(&mut out, 0x85, &[0, 0, 0, 0, 0, 0, 1, 0, b'S']);
        rec(&mut out, 0x0a, &[]);
        let start = out.len() as u32;
        out[patch..patch + 4].copy_from_slice(&start.to_le_bytes());
        bof(&mut out, 0x10);
        let mut label = vec![0; 6];
        label.extend(13u16.to_le_bytes());
        label.push(1);
        label.extend("Needle Needle".encode_utf16().flat_map(u16::to_le_bytes));
        rec(&mut out, 0x204, &label);
        rec(&mut out, 0x0a, &[]);
        let mut cfb =
            cfb::CompoundFile::create_with_version(cfb::Version::V3, Cursor::new(Vec::new()))
                .unwrap();
        cfb.create_stream("Workbook")
            .unwrap()
            .write_all(&out)
            .unwrap();
        cfb.into_inner().into_inner()
    }
    fn msg() -> Vec<u8> {
        let mut cfb = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        cfb.create_stream("__substg1.0_1000001F")
            .unwrap()
            .write_all(
                &"Needle Needle"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        cfb.into_inner().into_inner()
    }
    fn pdf() -> Vec<u8> {
        use lopdf::{Document, Object, Stream, dictionary};
        let mut doc = Document::with_version("1.5");
        let pages = doc.new_object_id();
        let font =
            doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
        let content = doc.add_object(Stream::new(
            dictionary! {},
            b"BT /F 12 Tf (Needle Needle) Tj ET".to_vec(),
        ));
        let page=doc.add_object(dictionary!{"Type"=>"Page","Parent"=>pages,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()],"Contents"=>content,"Resources"=>dictionary!{"Font"=>dictionary!{"F"=>font}}});
        doc.objects.insert(
            pages,
            Object::Dictionary(dictionary! {"Type"=>"Pages","Count"=>1,"Kids"=>vec![page.into()]}),
        );
        let catalog = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
        doc.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }
    #[test]
    fn all_real_readers_return_every_hit_and_locator_across_find_positions() {
        let matcher = regex::RegexBuilder::new("needle")
            .case_insensitive(true)
            .build()
            .unwrap();
        for (format, bytes, key, prefix) in [
            (Format::Text, b"Needle Needle".to_vec(), "line", ""),
            (Format::Pdf, pdf(), "page", "[p. 1]"),
            (
                Format::Docx,
                docx(&paragraph("Needle Needle")),
                "paragraph",
                "[¶1]",
            ),
            (Format::Xlsx, xlsx(), "sheet", "[sheet «S» row 1]"),
            (Format::Xls, xls(), "sheet", "[sheet «S» row 1]"),
            (
                Format::Eml,
                b"Subject: none\r\n\r\nNeedle Needle".to_vec(),
                "part",
                "[mail]",
            ),
            (Format::Msg, msg(), "part", "[mail]"),
        ] {
            let opened = Opened {
                location: Location {
                    root: "work".into(),
                    disk: PathBuf::from("a"),
                    members: vec![],
                },
                name: "a".into(),
                format,
                size: bytes.len() as u64,
                fingerprint: cursor::Fingerprint {
                    size: bytes.len() as u64,
                    modified_ns: 0,
                },
                source: Source::Memory(Cursor::new(bytes)),
            };
            let mut doc = documents::open_text_document(opened, &json!({})).unwrap();
            let mut position = (0, 0);
            let mut hits = Vec::new();
            loop {
                let (found, next) = find_in_document(
                    doc.as_mut(),
                    &matcher,
                    position.0,
                    position.1,
                    1,
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
                for hit in &found {
                    assert!(hit.locator.get(key).is_some(), "{format:?}");
                    assert!(
                        hit.snippet.starts_with(prefix),
                        "{format:?}: {}",
                        hit.snippet
                    );
                }
                hits.extend(found);
                if let Some(next) = next {
                    position = next;
                } else {
                    break;
                }
            }
            assert_eq!(hits.len(), 2, "{format:?}");
        }
    }
    #[test]
    fn source_and_per_file_search_budgets_preserve_the_next_file() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = docx(&paragraph("needle"));
        for name in ["a.docx", "b.docx", "c.docx"] {
            std::fs::write(tmp.path().join(name), &bytes).unwrap();
        }
        let fs = Filesystem::new(Config {
            version: 1,
            roots: vec![Root {
                id: "work".into(),
                path: tmp.path().into(),
                nickname: None,
            }],
            next_root_id: 1,
        });
        TEST_SCAN_BUDGET.with(|c| c.set(Some((bytes.len() as u64 * 2, bytes.len() as u64))));
        let mut args = json!({"root":"work","query":"needle"});
        let first = fs.call("search_text", &args).unwrap();
        assert_eq!(first["results"].as_array().unwrap().len(), 2);
        assert_eq!(first["scannedBytes"], bytes.len() * 2);
        assert_eq!(first["documentsSearched"], 2);
        assert_eq!(first["truncated"], true);
        args["cursor"] = first["nextCursor"].clone();
        let second = fs.call("search_text", &args).unwrap();
        assert_eq!(second["results"].as_array().unwrap().len(), 1);
        assert_eq!(second["results"][0]["path"], "work/c.docx");
        assert_eq!(second["truncated"], false);
        std::fs::write(
            tmp.path().join("z.docx"),
            docx(&paragraph(&"needle".repeat(20))),
        )
        .unwrap();
        args.as_object_mut().unwrap().remove("cursor");
        TEST_SCAN_BUDGET.with(|c| c.set(Some((DOC_SCAN_LIMIT, bytes.len() as u64))));
        let result = fs.call("search_text", &args).unwrap();
        assert_eq!(result["skippedLargeFiles"], 1);
        assert_eq!(result["documentsSearched"], 3);
        TEST_SCAN_BUDGET.with(|c| c.set(None));
    }
    struct Units {
        read: Vec<usize>,
    }
    impl TextDocument for Units {
        fn unit_name(&self) -> &'static str {
            "page"
        }
        fn unit_count(&self) -> usize {
            3
        }
        fn unit(&mut self, index: usize) -> Result<Unit> {
            self.read.push(index);
            Ok(Unit {
                locator: json!({"page":index+1}),
                heading: None,
                text: if index == 2 {
                    "😀 needle needle\t конец".into()
                } else {
                    "no match".into()
                },
            })
        }
        fn summary(&self) -> Value {
            json!({})
        }
    }
    #[test]
    fn expired_deadline_makes_progress_and_resume_does_not_repeat_units() {
        let mut doc = Units { read: vec![] };
        let matcher = Regex::new("needle").unwrap();
        let expired = Instant::now() - Duration::from_secs(1);
        let (hits, next) = find_in_document(&mut doc, &matcher, 0, 0, 10, expired).unwrap();
        assert!(hits.is_empty());
        assert_eq!(next, Some((1, 0)));
        let (hits, next) = find_in_document(&mut doc, &matcher, 1, 0, 10, expired).unwrap();
        assert!(hits.is_empty());
        assert_eq!(next, Some((2, 0)));
        let (hits, next) = find_in_document(&mut doc, &matcher, 2, 0, 1, expired).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(next, Some((2, 1)));
        assert!(hits[0].snippet.contains("😀 needle needle конец"));
        let (hits, next) = find_in_document(&mut doc, &matcher, 2, 1, 1, expired).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(next, None);
        assert_eq!(doc.read, vec![0, 1, 2, 2]);
    }
    #[test]
    fn folder_cursor_resumes_unfinished_document_and_restarts_changed_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("a.docx"),
            docx(
                &[
                    paragraph("no match"),
                    paragraph("still no"),
                    paragraph("needle"),
                ]
                .join(""),
            ),
        )
        .unwrap();
        std::fs::write(tmp.path().join("z.txt"), "needle").unwrap();
        let fs = Filesystem::new(Config {
            version: 1,
            roots: vec![Root {
                id: "work".into(),
                path: tmp.path().into(),
                nickname: None,
            }],
            next_root_id: 1,
        });
        let mut args = json!({"root":"work","query":"needle"});
        documents::TEST_DEADLINE.with(|c| c.set(Some(Instant::now() - Duration::from_secs(1))));
        let a = fs.call("search_text", &args).unwrap();
        assert!(a["results"].as_array().unwrap().is_empty());
        let raw = a["nextCursor"].as_str().unwrap();
        let hash = cursor::request_hash(&["search_text", "work", "", "needle"]);
        let c = cursor::decode(raw, CursorKind::Walk, hash).unwrap();
        assert!(c.f.is_some());
        assert_eq!(c.u, 1);
        args["cursor"] = a["nextCursor"].clone();
        let b = fs.call("search_text", &args).unwrap();
        assert!(b["results"].as_array().unwrap().is_empty());
        args["cursor"] = b["nextCursor"].clone();
        let c = fs.call("search_text", &args).unwrap();
        assert_eq!(c["results"].as_array().unwrap().len(), 2);
        assert!(c.get("nextCursor").is_none());
        std::fs::write(
            tmp.path().join("a.docx"),
            docx(&paragraph("needle changed")),
        )
        .unwrap();
        args["cursor"] = a["nextCursor"].clone();
        let changed = fs.call("search_text", &args).unwrap();
        assert_eq!(changed["results"][0]["locator"]["paragraph"], 1);
        std::fs::remove_file(tmp.path().join("a.docx")).unwrap();
        let gone = fs.call("search_text", &args).unwrap();
        assert_eq!(gone["results"].as_array().unwrap().len(), 1);
        assert_eq!(gone["results"][0]["path"], "work/z.txt");
        documents::TEST_DEADLINE.with(|c| c.set(None));
    }
}
