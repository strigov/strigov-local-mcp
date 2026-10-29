use crate::{
    documents::{Format, Source, TextDocument, Unit, ZIP_ENTRY_LIMIT, ZIP_RATIO_LIMIT},
    filesystem::{Result, error, string},
};
use calamine::{Data, DataRef, Reader as _, SheetType, SheetVisible, Sheets};
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    io::{Cursor, Read, Seek, SeekFrom, Write},
    ops::Range,
};

pub(crate) const SHEET_LIMIT: u64 = 64 * 1024 * 1024;
pub(crate) const XLSX_UNPACKED_LIMIT: u64 = 256 * 1024 * 1024;
pub(crate) const SHEET_CELL_LIMIT: u64 = 4_000_000;
pub(crate) const SHEET_TEXT_LIMIT: u64 = 64 * 1024 * 1024;
#[derive(Clone, Copy)]
struct Limits {
    unpacked: u64,
    cells: u64,
    text: u64,
    strings: usize,
}
const LIMITS: Limits = Limits {
    unpacked: XLSX_UNPACKED_LIMIT,
    cells: SHEET_CELL_LIMIT,
    text: SHEET_TEXT_LIMIT,
    strings: (SHEET_TEXT_LIMIT / 48) as usize,
};
fn invalid() -> crate::filesystem::FsError {
    error("DOCUMENT_INVALID", "Cannot parse spreadsheet")
}
fn large(message: &'static str) -> crate::filesystem::FsError {
    error("FILE_TOO_LARGE", message)
}
fn bomb() -> crate::filesystem::FsError {
    error("ZIP_BOMB", "XLSX exceeds archive limits")
}
// Grow geometrically up to the derived ZIP budget (payload plus headers).
// The actual inflated-byte check remains independent of buffer capacity.
struct ExactBuffer(Cursor<Vec<u8>>, usize);
impl Write for ExactBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let end = usize::try_from(self.0.position())
            .ok()
            .and_then(|p| p.checked_add(bytes.len()))
            .ok_or_else(|| std::io::Error::other("Invalid buffer position"))?;
        if end > self.0.get_ref().capacity() {
            let capacity = end.max(self.0.get_ref().capacity().saturating_mul(2).min(self.1));
            let additional = capacity - self.0.get_ref().len();
            self.0.get_mut().reserve_exact(additional);
        }
        self.0.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Seek for ExactBuffer {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}
fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(at..at + 2)
            .ok_or_else(invalid)?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or_else(invalid)?
            .try_into()
            .unwrap(),
    ))
}
fn charge(sum: &mut u64, value: u64, limit: u64, message: &'static str) -> Result<()> {
    *sum = sum.saturating_add(value);
    if *sum > limit {
        return Err(large(message));
    }
    Ok(())
}
// Keep the original XML slices except the two untrusted reservation attributes.
fn shared_strings(bytes: &[u8], output: &mut impl Write, limit: usize) -> Result<()> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    let mut buf = Vec::new();
    let mut found = false;
    let mut strings = 0usize;
    let mut written = 0usize;
    let mut depth = 0usize;
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event_into(&mut buf).map_err(|_| invalid())?;
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.checked_sub(1).ok_or_else(invalid)?,
            Event::Eof if depth != 0 => return Err(invalid()),
            _ => {}
        }
        match &event {
            Event::Start(tag) | Event::Empty(tag) if tag.local_name().as_ref() == b"sst" => {
                found = true;
                output
                    .write_all(&bytes[written..start])
                    .map_err(|_| invalid())?;
                let name = std::str::from_utf8(tag.name().as_ref())
                    .map_err(|_| invalid())?
                    .to_owned();
                let mut clean = BytesStart::new(name);
                for a in tag.attributes() {
                    let a = a.map_err(|_| invalid())?;
                    if !matches!(a.key.local_name().as_ref(), b"count" | b"uniqueCount") {
                        clean.push_attribute(a);
                    }
                }
                let clean = if matches!(event, Event::Empty(_)) {
                    Event::Empty(clean)
                } else {
                    Event::Start(clean)
                };
                Writer::new(&mut *output)
                    .write_event(clean)
                    .map_err(|_| invalid())?;
                written = reader.buffer_position() as usize;
            }
            Event::Start(tag) | Event::Empty(tag) if tag.local_name().as_ref() == b"si" => {
                strings += 1;
                if strings > limit {
                    return Err(large("too many shared strings"));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if !found {
        return Err(invalid());
    }
    output.write_all(&bytes[written..]).map_err(|_| invalid())?;
    Ok(())
}
fn xlsx_copy(source: Source, limits: Limits) -> Result<Source> {
    let mut zip = zip::ZipArchive::new(source).map_err(|_| invalid())?;
    if zip.len() > ZIP_ENTRY_LIMIT {
        return Err(bomb());
    }
    let mut declared = 0u64;
    let mut payload = 0u64;
    let mut headers = 22u64; // Empty ZIP end record; no archive comment.
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|_| invalid())?;
        declared = declared.saturating_add(entry.size());
        if declared > limits.unpacked
            || (entry.size() > 1024 * 1024
                && entry.size() > entry.compressed_size().saturating_mul(ZIP_RATIO_LIMIT))
        {
            return Err(bomb());
        }
        let canonical = entry.name().replace('\\', "/").to_ascii_lowercase();
        if canonical.ends_with(".xml") || canonical.ends_with(".rels") {
            payload += entry.size();
            // Stored entries, default options: local header 30 + central header 46,
            // with the UTF-8 filename written twice; no extra fields or comments.
            headers += 76 + 2 * entry.name().len() as u64;
        }
    }
    let mut writer = zip::ZipWriter::new(ExactBuffer(
        Cursor::new(Vec::with_capacity(
            (payload.min(limits.unpacked) + headers) as usize,
        )),
        (limits.unpacked + headers) as usize,
    ));
    let mut actual = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|_| invalid())?;
        let name = entry.name().to_owned();
        let canonical = name.replace('\\', "/").to_ascii_lowercase();
        if !(canonical.ends_with(".xml") || canonical.ends_with(".rels")) {
            continue;
        }
        let remaining = limits.unpacked - actual;
        writer
            .start_file(
                &name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .map_err(|_| invalid())?;
        let needed = (entry.size().min(remaining) + 1) as usize;
        let copied = if canonical.ends_with("sharedstrings.xml") {
            let mut bytes = Vec::with_capacity(needed);
            (&mut entry)
                .take(remaining + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| invalid())?;
            if bytes.len() as u64 > remaining {
                return Err(bomb());
            }
            shared_strings(&bytes, &mut writer, limits.strings)?;
            bytes.len() as u64
        } else {
            std::io::copy(&mut (&mut entry).take(remaining + 1), &mut writer)
                .map_err(|_| invalid())?
        };
        if copied > remaining {
            return Err(bomb());
        }
        actual += copied;
    }
    Ok(Source::Memory(writer.finish().map_err(|_| invalid())?.0))
}
#[derive(Default, Clone, Copy)]
struct Extent {
    bounds: Option<((u32, u32), (u32, u32))>,
}
impl Extent {
    fn cell(&mut self, row: u32, col: u32) {
        self.bounds = Some(match self.bounds {
            None => ((row, col), (row, col)),
            Some((a, b)) => ((a.0.min(row), a.1.min(col)), (b.0.max(row), b.1.max(col))),
        });
    }
    fn cells(self) -> u64 {
        self.bounds.map_or(0, |(a, b)| {
            (b.0 as u64 - a.0 as u64 + 1).saturating_mul(b.1 as u64 - a.1 as u64 + 1)
        })
    }
    fn check(self, limit: u64) -> Result<()> {
        if self.cells() > limit {
            Err(large("sheet range too large"))
        } else {
            Ok(())
        }
    }
}
struct Record<'a> {
    typ: u16,
    parts: Vec<&'a [u8]>,
    end: usize,
}
fn record(bytes: &[u8], offset: usize) -> Result<Record<'_>> {
    let typ = u16_at(bytes, offset)?;
    let mut pos = offset;
    let mut parts = Vec::new();
    loop {
        let len = u16_at(bytes, pos + 2)? as usize;
        let end = pos.checked_add(4 + len).ok_or_else(invalid)?;
        parts.push(bytes.get(pos + 4..end).ok_or_else(invalid)?);
        pos = end;
        if bytes.len().saturating_sub(pos) <= 4 || u16_at(bytes, pos)? != 0x003c {
            break;
        }
    }
    Ok(Record {
        typ,
        parts,
        end: pos,
    })
}
// SST strings alone change character width at a CONTINUE boundary.
struct Segments<'a> {
    parts: &'a [&'a [u8]],
    part: usize,
    offset: usize,
}
impl<'a> Segments<'a> {
    fn normalize(&mut self) {
        while self.part < self.parts.len() && self.offset == self.parts[self.part].len() {
            self.part += 1;
            self.offset = 0;
        }
    }
    fn byte(&mut self) -> Result<u8> {
        self.normalize();
        let b = *self
            .parts
            .get(self.part)
            .and_then(|p| p.get(self.offset))
            .ok_or_else(invalid)?;
        self.offset += 1;
        Ok(b)
    }
    fn word(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes([self.byte()?, self.byte()?]))
    }
    fn dword(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes([
            self.byte()?,
            self.byte()?,
            self.byte()?,
            self.byte()?,
        ]))
    }
    fn skip(&mut self, mut n: usize) -> Result<()> {
        while n > 0 {
            self.normalize();
            let part = self.parts.get(self.part).ok_or_else(invalid)?;
            let take = n.min(part.len() - self.offset);
            self.offset += take;
            n -= take;
        }
        Ok(())
    }
}
fn sst_lengths(r: &Record<'_>, limit: usize) -> Result<Vec<u32>> {
    if r.parts[0].len() < 8 {
        return Err(invalid());
    }
    let mut input = Segments {
        parts: &r.parts,
        part: 0,
        offset: 8,
    };
    let mut lengths = Vec::new();
    loop {
        input.normalize();
        if input.part == input.parts.len() {
            break;
        }
        if lengths.len() >= limit {
            return Err(large("too many shared strings"));
        }
        let cch = input.word()?;
        let flags = input.byte()?;
        let runs = if flags & 8 != 0 {
            input.word()? as usize
        } else {
            0
        };
        let ext = if flags & 4 != 0 {
            input.dword()? as usize
        } else {
            0
        };
        let mut wide = flags & 1 != 0;
        let mut chars = cch as usize;
        while chars > 0 {
            if input.part >= input.parts.len() {
                return Err(invalid());
            }
            let available =
                (input.parts[input.part].len() - input.offset) / if wide { 2 } else { 1 };
            let take = chars.min(available);
            input.offset += take * if wide { 2 } else { 1 };
            chars -= take;
            if chars > 0 {
                if input.offset != input.parts[input.part].len() {
                    return Err(invalid());
                }
                input.normalize();
                wide = input.byte()? & 1 != 0;
            }
        }
        input.skip(runs * 4)?;
        input.skip(ext)?;
        lengths.push(cch as u32 * 3);
    }
    Ok(lengths)
}
struct Length(usize);
impl std::fmt::Write for Length {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0 += s.len();
        Ok(())
    }
}

// Calamine 0.36.1 (MIT), utils.rs FTAB_ARGC. Kept with the version-specific token scan.
const FUNCTION_ARITY: [u8; 485] = [
    255, 3, 1, 1, 255, 255, 255, 255, 1, 1, 0, 254, 255, 2, 3, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1,
    2, 3, 4, 2, 3, 1, 1, 0, 0, 255, 255, 1, 2, 3, 3, 3, 3, 3, 3, 255, 3, 2, 4, 4, 4, 4, 1, 1, 1, 5,
    5, 5, 5, 6, 3, 2, 0, 3, 3, 3, 1, 1, 1, 2, 1, 1, 1, 0, 1, 1, 1, 5, 2, 2, 3, 3, 1, 2, 0, 1, 1, 2,
    0, 1, 2, 2, 2, 0, 0, 1, 2, 1, 1, 255, 4, 4, 2, 7, 1, 1, 2, 2, 2, 4, 1, 1, 1, 1, 2, 2, 2, 1, 4,
    4, 1, 3, 1, 3, 2, 1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 2, 2, 2, 2, 1, 1, 3, 4, 5, 3, 2, 2, 2, 255,
    255, 1, 4, 5, 5, 5, 5, 1, 3, 4, 3, 1, 1, 1, 1, 1, 2, 6, 6, 255, 2, 4, 1, 0, 0, 2, 2, 3, 2, 1,
    1, 1, 4, 255, 1, 2, 1, 2, 2, 3, 1, 3, 4, 255, 255, 3, 3, 2, 1, 3, 1, 1, 0, 0, 2, 3, 3, 4, 2, 2,
    3, 3, 2, 2, 1, 1, 3, 0, 0, 5, 3, 0, 7, 0, 1, 0, 3, 255, 255, 1, 1, 1, 1, 1, 1, 3, 11, 1, 0, 2,
    3, 5, 4, 4, 1, 0, 5, 5, 1, 0, 0, 1, 2, 2, 1, 255, 1, 1, 2, 3, 3, 1, 1, 1, 2, 3, 3, 3, 2, 255,
    5, 1, 5, 4, 2, 2, 2, 3, 3, 1, 3, 3, 3, 1, 1, 2, 4, 3, 2, 4, 3, 3, 3, 4, 1, 3, 1, 3, 1, 2, 3, 3,
    4, 2, 2, 2, 2, 2, 2, 3, 2, 2, 2, 2, 2, 2, 4, 4, 255, 255, 255, 255, 255, 255, 3, 2, 2, 2, 2, 3,
    255, 2, 2, 4, 4, 3, 255, 2, 9, 2, 3, 4, 1, 1, 255, 3, 2, 1, 2, 1, 4, 3, 1, 2, 2, 4, 5, 2, 128,
    2, 1, 255, 255, 255, 255, 255, 255, 255, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 255, 255, 3, 3, 4, 2,
    1, 2, 2, 2, 2, 2, 2, 1, 1, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 255, 255, 4,
    1, 1, 2, 2, 2, 1, 1, 2, 2, 1, 2, 2, 2, 2, 3, 3, 6, 6, 5, 5, 5, 5, 5, 3, 3, 3, 7, 7, 2, 2, 2, 2,
    6, 6, 2, 2, 3, 4, 4, 4, 4, 4, 4, 6, 6, 8, 8, 8, 8, 2, 2, 7, 7, 8, 8, 5, 3, 3, 255, 255, 255, 2,
    4, 5, 1, 2, 128, 129, 3, 129,
];

fn formula_charge(r: &Record<'_>, max_sheet: u64, names: &[u64], old_biff: bool) -> Result<u64> {
    let data = r.parts[0];
    let cce = u16_at(data, 20)? as usize;
    // calamine only reads the first segment; charge CONTINUE bytes too per C10.
    let rgce_len = r
        .parts
        .iter()
        .map(|p| p.len() as u64)
        .sum::<u64>()
        .saturating_sub(20);
    let mut token_bound = 0u64;
    let Some(tokens) = data.get(22..22 + cce) else {
        return Err(invalid());
    };
    let mut pos = 0usize;
    while pos < tokens.len() {
        let ptg = tokens[pos];
        pos += 1;
        let t = &tokens[pos..];
        let (size, bound) = match ptg {
            0x3a | 0x5a | 0x7a | 0x3c | 0x5c | 0x7c => (6, max_sheet + 32),
            0x3b | 0x5b | 0x7b | 0x3d | 0x5d | 0x7d => (10, max_sheet + 32),
            0x23 | 0x43 | 0x63 => {
                if t.len() < 4 {
                    break;
                }
                let i = u32_at(t, 0)?;
                (
                    4,
                    i.checked_sub(1)
                        .and_then(|i| names.get(i as usize))
                        .copied()
                        .unwrap_or(5),
                )
            }
            0x17 => {
                if t.len() < 2 {
                    break;
                }
                let cch = t[0] as usize;
                (2 + cch, cch as u64 * 3 + 2)
            }
            0x19 => {
                if t.len() < 3 {
                    break;
                }
                match t[0] {
                    0x01 | 0x02 | 0x08 | 0x20 | 0x21 => (3, 0),
                    0x04 => (3 + 2 * (u16_at(t, 1)? as usize + 1), 0),
                    0x10 => (3, 13),
                    0x40 | 0x41 => (3, t[2] as u64),
                    _ => break,
                }
            }
            0x1f => {
                if t.len() < 8 {
                    break;
                }
                let mut len = Length(0);
                write!(
                    &mut len,
                    "{}",
                    f64::from_le_bytes(t[..8].try_into().unwrap())
                )
                .unwrap();
                (8, len.0 as u64)
            }
            0x21 | 0x41 | 0x61 => {
                if t.len() < 2 {
                    break;
                }
                let Some(argc) = FUNCTION_ARITY.get(u16_at(t, 0)? as usize) else {
                    break;
                };
                (2, 20 + *argc as u64)
            }
            0x22 | 0x42 | 0x62 => {
                if t.len() < 3 {
                    break;
                }
                (3, 20 + t[0] as u64)
            }
            0x24 | 0x44 | 0x64 | 0x2a | 0x4a | 0x6a => (if old_biff { 3 } else { 4 }, 24),
            0x25 | 0x45 | 0x65 | 0x2b | 0x4b | 0x6b => (if old_biff { 6 } else { 8 }, 24),
            0x1c | 0x1d => (1, 13),
            0x1e => (2, 13),
            0x20 | 0x40 | 0x60 => (7, 13),
            0x39 | 0x59 => (6, 13),
            0x03..=0x15 => (0, 2),
            0x01 => (4, 0),
            0x16 => (0, 0),
            0x18 => (5, 0),
            _ => break,
        };
        if size > t.len() {
            break;
        }
        token_bound = token_bound.saturating_add(bound);
        pos += size;
    }
    Ok(rgce_len.saturating_add(2 * token_bound).saturating_add(256))
}
fn xls_prescan(bytes: &mut [u8], limits: Limits) -> Result<()> {
    let mut pos = 0;
    let mut offsets = Vec::new();
    let mut names = Vec::new();
    let mut strings = Vec::new();
    let mut max_sheet = 4u64;
    let mut old_biff = false;
    loop {
        let r = record(bytes, pos)?;
        let d = r.parts[0];
        pos = r.end;
        match r.typ {
            0x0809 => {
                // Match calamine 0.36.1 parse_bof, including unknown-version fallback.
                let version = u16_at(d, 0)?;
                let dt = if d.len() >= 4 { u16_at(d, 2)? } else { 0 };
                old_biff = matches!(version, 0x0002 | 0x0007 | 0x0200 | 0x0300 | 0x0400 | 0x0500)
                    || (version == 0 && dt == 0x1000);
            }
            0x0085 => {
                offsets.push(u32_at(d, 0)? as usize);
                max_sheet = max_sheet.max(*d.get(6).ok_or_else(invalid)? as u64 * 3);
            }
            0x0018 => names.push(*d.get(3).ok_or_else(invalid)? as u64 * 3),
            0x00fc => strings = sst_lengths(&r, limits.strings)?,
            0x000a => break,
            _ => {}
        }
        if pos >= bytes.len() {
            return Err(invalid());
        }
    }
    offsets.sort_unstable();
    let mut previous_end = pos;
    let mut cells = 0u64;
    let mut text = 0u64;
    for offset in offsets {
        if offset < previous_end || offset >= bytes.len() {
            return Err(error("DOCUMENT_INVALID", "overlapping sheet substreams"));
        }
        pos = offset;
        let mut actual = Extent::default();
        let mut dimension = 0u64;
        let mut seen_formula = false;
        while pos < bytes.len() {
            let start = pos;
            let r = record(bytes, pos)?;
            let d = r.parts[0];
            pos = r.end;
            match r.typ {
                0x0200 => {
                    let (rf, rl, mut cf, cl) = match d.len() {
                        10 => (
                            u16_at(d, 0)? as u32,
                            u16_at(d, 2)? as u32,
                            u16_at(d, 4)? as u32,
                            u16_at(d, 6)? as u32,
                        ),
                        14 => (
                            u32_at(d, 0)?,
                            u32_at(d, 4)?,
                            u16_at(d, 8)? as u32,
                            u16_at(d, 10)? as u32,
                        ),
                        _ => return Err(invalid()),
                    };
                    if cf > 255 {
                        cf = 0;
                    }
                    if rl < rf || cl < cf {
                        return Err(invalid());
                    }
                    let extent = (rl - rf) as u64 * (cl - cf) as u64;
                    dimension = dimension.max(extent);
                    if dimension > limits.cells {
                        return Err(large("sheet range too large"));
                    }
                    if extent == 0 {
                        // calamine subtracts inclusive endpoints while reserving;
                        // normalize empty dimensions in our copy to avoid debug underflow.
                        let len = d.len();
                        bytes[start + 4..start + 4 + len].fill(0);
                    }
                }
                0x0203 | 0x0204 | 0x00fd | 0x027e | 0x00bd | 0x00be | 0x0006 | 0x0201 | 0x0205
                | 0x00d6 => {
                    let row = u16_at(d, 0)? as u32;
                    let col = u16_at(d, 2)? as u32;
                    actual.cell(row, col);
                    if matches!(r.typ, 0x00bd | 0x00be) {
                        let last = u16_at(d, d.len().checked_sub(2).ok_or_else(invalid)?)? as u32;
                        if last < col {
                            return Err(invalid());
                        }
                        actual.cell(row, last);
                    }
                    actual.check(limits.cells)?;
                    let amount = match r.typ {
                        0x00fd => *strings.get(u32_at(d, 6)? as usize).ok_or_else(invalid)? as u64,
                        0x0204 | 0x00d6 => u16_at(d, 6)? as u64 * 3,
                        0x0006 => {
                            seen_formula = true;
                            formula_charge(&r, max_sheet, &names, old_biff)?
                        }
                        _ => 0,
                    };
                    charge(&mut text, amount, limits.text, "sheet text too large")?;
                }
                0x0207 => {
                    // calamine otherwise inserts this at its initial formula position
                    // (0, 0), which is absent from our charged cell extent.
                    if !seen_formula {
                        return Err(invalid());
                    }
                    charge(
                        &mut text,
                        u16_at(d, 0)? as u64 * 3,
                        limits.text,
                        "sheet text too large",
                    )?;
                }
                0x000a => break,
                _ => {}
            }
        }
        previous_end = pos;
        charge(
            &mut cells,
            dimension.max(actual.cells()),
            limits.cells,
            "sheet range too large",
        )?;
    }
    Ok(())
}
fn xls_copy(source: Source, limits: Limits) -> Result<Source> {
    let mut cfb = cfb::CompoundFile::open(source).map_err(|_| invalid())?;
    let name = ["/Workbook", "/Book", "/WORKBOOK", "/BOOK"]
        .into_iter()
        .find(|n| cfb.is_stream(n))
        .ok_or_else(invalid)?;
    let len = cfb.entry(name).map_err(|_| invalid())?.len();
    if len > SHEET_LIMIT {
        return Err(large("XLS Workbook exceeds 64 MiB"));
    }
    let mut bytes = Vec::with_capacity(len as usize + 1);
    cfb.open_stream(name)
        .map_err(|_| invalid())?
        .take(SHEET_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > SHEET_LIMIT {
        return Err(large("XLS Workbook exceeds 64 MiB"));
    }
    xls_prescan(&mut bytes, limits)?;
    drop(cfb);
    // calamine 0.36.1 rejects V4 files without a mini stream (EmptyRootDir).
    let mut copy =
        cfb::CompoundFile::create_with_version(cfb::Version::V3, Cursor::new(Vec::new()))
            .map_err(|_| invalid())?;
    copy.create_stream("/Workbook")
        .map_err(|_| invalid())?
        .write_all(&bytes)
        .map_err(|_| invalid())?;
    copy.flush().map_err(|_| invalid())?;
    drop(bytes);
    Ok(Source::Memory(copy.into_inner()))
}
struct SheetInfo {
    name: String,
    visible: bool,
    extent: Extent,
    offset: usize,
    rows: usize,
}
pub(crate) struct SheetDocument {
    book: Sheets<Source>,
    sheets: Vec<SheetInfo>,
    count: usize,
    cache: Option<(usize, calamine::Range<Data>)>,
}
pub(crate) fn open(source: Source, format: Format) -> Result<SheetDocument> {
    open_with_limits(source, format, LIMITS)
}
fn open_with_limits(source: Source, format: Format, limits: Limits) -> Result<SheetDocument> {
    let mut book = match format {
        Format::Xlsx => Sheets::Xlsx(
            calamine::open_workbook_from_rs::<calamine::Xlsx<_>, _>(xlsx_copy(source, limits)?)
                .map_err(|_| invalid())?,
        ),
        Format::Xls => Sheets::Xls(
            calamine::open_workbook_from_rs::<calamine::Xls<_>, _>(xls_copy(source, limits)?)
                .map_err(|_| invalid())?,
        ),
        _ => return Err(invalid()),
    };
    let metadata = book.sheets_metadata().to_vec();
    let mut sheets = Vec::new();
    let mut count = 0usize;
    for sheet in metadata {
        let mut extent = Extent::default();
        match &mut book {
            Sheets::Xlsx(xlsx) if sheet.typ == SheetType::WorkSheet => {
                let mut reader = xlsx
                    .worksheet_cells_reader(&sheet.name)
                    .map_err(|_| invalid())?;
                let mut text = 0u64;
                while let Some(cell) = reader.next_cell().map_err(|_| invalid())? {
                    // worksheet_range_ref skips these before Range::from_sparse.
                    if matches!(cell.get_value(), DataRef::Empty) {
                        continue;
                    }
                    let (row, col) = cell.get_position();
                    extent.cell(row, col);
                    extent.check(limits.cells)?;
                    let len = match cell.get_value() {
                        DataRef::SharedString(s) => s.len(),
                        DataRef::String(s) | DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => {
                            s.len()
                        }
                        _ => 0,
                    };
                    charge(&mut text, len as u64, limits.text, "sheet text too large")?;
                }
            }
            Sheets::Xlsx(_) => {} // calamine also exposes non-worksheets as empty ranges.
            _ => {
                let range = book.worksheet_range(&sheet.name).map_err(|_| invalid())?;
                if let (Some(a), Some(b)) = (range.start(), range.end()) {
                    extent.cell(a.0, a.1);
                    extent.cell(b.0, b.1);
                }
            }
        }
        let rows = extent.bounds.map_or(0, |(a, b)| (b.0 - a.0 + 1) as usize);
        sheets.push(SheetInfo {
            name: sheet.name,
            visible: sheet.visible == SheetVisible::Visible,
            extent,
            offset: count,
            rows,
        });
        count += rows;
    }
    Ok(SheetDocument {
        book,
        sheets,
        count,
        cache: None,
    })
}
fn column(mut index: u32) -> String {
    let mut name = Vec::new();
    loop {
        name.push(b'A' + (index % 26) as u8);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    name.reverse();
    String::from_utf8(name).unwrap()
}
fn cell_text(value: &Data) -> String {
    let text = match value {
        Data::DateTime(d) => {
            let (y, m, day, h, min, s, ms) = d.to_ymd_hms_milli();
            format!("{y:04}-{m:02}-{day:02}T{h:02}:{min:02}:{s:02}.{ms:03}")
        }
        _ => value.to_string(),
    };
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', " ")
        .replace('\n', " ⏎ ")
}
impl TextDocument for SheetDocument {
    fn unit_name(&self) -> &'static str {
        "row"
    }
    fn unit_count(&self) -> usize {
        self.count
    }
    fn unit_range(&self, args: &Value) -> Result<Range<usize>> {
        if args.get("sheet").is_none() {
            return Ok(0..self.count);
        }
        let selected = string(args, "sheet", None)?;
        if selected.is_empty() || selected.chars().count() > 256 {
            return Err(error("INVALID_ARGUMENT", "Invalid sheet selection"));
        }
        let index = self
            .sheets
            .iter()
            .position(|s| s.name == selected)
            .or_else(|| {
                selected
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .filter(|n| *n < self.sheets.len())
            })
            .ok_or_else(|| error("INVALID_ARGUMENT", "Sheet not found"))?;
        let sheet = &self.sheets[index];
        Ok(sheet.offset..sheet.offset + sheet.rows)
    }
    fn unit(&mut self, index: usize) -> Result<Unit> {
        let i = self
            .sheets
            .iter()
            .position(|s| index >= s.offset && index < s.offset + s.rows)
            .ok_or_else(|| error("INVALID_CURSOR", "Invalid sheet row"))?;
        if self.cache.as_ref().is_none_or(|(cached, _)| *cached != i) {
            self.cache = None;
            self.cache = Some((
                i,
                self.book
                    .worksheet_range(&self.sheets[i].name)
                    .map_err(|_| invalid())?,
            ));
        }
        let sheet = &self.sheets[i];
        let (a, b) = sheet.extent.bounds.ok_or_else(invalid)?;
        let row = a.0 + (index - sheet.offset) as u32;
        let range = &self.cache.as_ref().unwrap().1;
        let mut cells: Vec<_> = (a.1..=b.1)
            .map(|col| {
                range
                    .get_value((row, col))
                    .map(cell_text)
                    .unwrap_or_default()
            })
            .collect();
        while cells.last().is_some_and(|s| s.is_empty()) {
            cells.pop();
        }
        let heading = (index == sheet.offset).then(|| {
            format!(
                "=== sheet {}/{} «{}» (rows {}–{}, columns {}–{}){} ===",
                i + 1,
                self.sheets.len(),
                sheet.name,
                a.0 + 1,
                b.0 + 1,
                column(a.1),
                column(b.1),
                if sheet.visible { "" } else { " hidden" },
            )
        });
        let line = if cells.is_empty() {
            format!("{}", row + 1)
        } else {
            format!("{}\t{}", row + 1, cells.join("\t"))
        };
        // The column line is text so even a very wide row can be continued by C6.
        let text = if index == sheet.offset {
            format!(
                "row\t{}\n{line}",
                (a.1..=b.1).map(column).collect::<Vec<_>>().join("\t")
            )
        } else {
            line
        };
        Ok(Unit {
            locator: json!({"sheet":sheet.name,"row":row+1}),
            heading,
            text,
        })
    }
    fn summary(&self) -> Value {
        json!({"sheets":self.sheets.iter().enumerate().map(|(i,s)|json!({"index":i+1,"name":s.name,"visible":s.visible,"rows":s.rows,"columns":s.extent.bounds.map_or(0,|(a,b)|b.1-a.1+1)})).collect::<Vec<_>>()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn zip(parts: &[(&str, String)], deflate: bool) -> Source {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in parts {
            zip.start_file(
                *name,
                zip::write::SimpleFileOptions::default().compression_method(if deflate {
                    zip::CompressionMethod::Deflated
                } else {
                    zip::CompressionMethod::Stored
                }),
            )
            .unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        Source::Memory(zip.finish().unwrap())
    }
    fn xlsx(sst: &str, cells: &str) -> Source {
        zip(&[
            ("[Content_Types].xml",r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/></Types>"#.into()),
            ("_rels/.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.into()),
            ("xl/workbook.xml",r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Первый" sheetId="1" r:id="rId1"/><sheet name="Второй" sheetId="2" state="hidden" r:id="rId2"/></sheets></workbook>"#.into()),
            ("xl/_rels/workbook.xml.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#.into()),
            ("xl/sharedStrings.xml",sst.into()),
            ("xl/styles.xml",r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14"/></cellXfs></styleSheet>"#.into()),
            ("xl/worksheets/sheet1.xml",format!(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{cells}</sheetData></worksheet>"#)),
            ("xl/worksheets/sheet2.xml",r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="3"><c r="B3" s="1"><v>45292</v></c></row></sheetData></worksheet>"#.into()),
        ],false)
    }
    const SST: &str = r#"<sst count="1099511627776" uniqueCount="1099511627776" xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>Сумма</t></si><si><t>Два</t></si></sst>"#;
    const CELLS: &str = r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row><row r="2"><c r="B2"><v>1234.5</v></c></row>"#;
    fn rec(out: &mut Vec<u8>, typ: u16, data: &[u8]) {
        assert!(data.len() <= 65535);
        out.extend(typ.to_le_bytes());
        out.extend((data.len() as u16).to_le_bytes());
        out.extend(data);
    }
    fn bof(out: &mut Vec<u8>, kind: u16) {
        let mut data = vec![0; 16];
        data[..2].copy_from_slice(&0x0600u16.to_le_bytes());
        data[2..4].copy_from_slice(&kind.to_le_bytes());
        rec(out, 0x0809, &data);
    }
    fn workbook(globals: &[u8], sheets: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        bof(&mut out, 5);
        out.extend(globals);
        let mut patches = Vec::new();
        for (name, _) in sheets {
            let mut data = vec![0; 6];
            data.push(name.encode_utf16().count() as u8);
            data.push(1);
            data.extend(name.encode_utf16().flat_map(u16::to_le_bytes));
            patches.push(out.len() + 4);
            rec(&mut out, 0x0085, &data);
        }
        rec(&mut out, 0x000a, &[]);
        for ((_, sheet), patch) in sheets.iter().zip(patches) {
            let pos = out.len() as u32;
            out[patch..patch + 4].copy_from_slice(&pos.to_le_bytes());
            bof(&mut out, 0x10);
            out.extend(sheet);
            rec(&mut out, 0x000a, &[]);
        }
        out
    }
    fn cfb(bytes: &[u8]) -> Source {
        let mut cfb = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        cfb.create_stream("/Workbook")
            .unwrap()
            .write_all(bytes)
            .unwrap();
        Source::Memory(cfb.into_inner())
    }
    fn basic() -> Vec<u8> {
        let mut sheet = Vec::new();
        let mut dimensions = vec![0; 14];
        dimensions[4..8].copy_from_slice(&2u32.to_le_bytes());
        dimensions[10..12].copy_from_slice(&2u16.to_le_bytes());
        rec(&mut sheet, 0x0200, &dimensions);
        let mut label = vec![0; 6];
        label.extend(5u16.to_le_bytes());
        label.push(1);
        label.extend("Сумма".encode_utf16().flat_map(u16::to_le_bytes));
        rec(&mut sheet, 0x0204, &label);
        let mut number = vec![0; 6];
        number[..2].copy_from_slice(&1u16.to_le_bytes());
        number[2..4].copy_from_slice(&1u16.to_le_bytes());
        number.extend(1234.5f64.to_le_bytes());
        rec(&mut sheet, 0x0203, &number);
        sheet
    }
    fn formula(row: u16, tokens: &[u8], padding: usize) -> Vec<u8> {
        let mut data = vec![0; 20];
        data[..2].copy_from_slice(&row.to_le_bytes());
        data[6..14].copy_from_slice(&1f64.to_le_bytes());
        data.extend((tokens.len() as u16).to_le_bytes());
        data.extend(tokens);
        data.resize(data.len() + padding, 0);
        data
    }
    fn repeated_sst(split: bool) -> (Vec<u8>, Vec<u8>) {
        let mut globals = Vec::new();
        let mut sst = Vec::new();
        sst.extend(200u32.to_le_bytes());
        sst.extend(1u32.to_le_bytes());
        sst.extend(10_000u16.to_le_bytes());
        sst.push(1);
        sst.extend("Я".repeat(10_000).encode_utf16().flat_map(u16::to_le_bytes));
        if split {
            rec(&mut globals, 0xfc, &sst[..8011]);
            let mut continuation = vec![1];
            continuation.extend(&sst[8011..]);
            rec(&mut globals, 0x3c, &continuation);
        } else {
            rec(&mut globals, 0xfc, &sst);
        }
        let mut sheet = Vec::new();
        for row in 0..200u16 {
            let mut label = vec![0; 10];
            label[..2].copy_from_slice(&row.to_le_bytes());
            rec(&mut sheet, 0xfd, &label);
        }
        (globals, sheet)
    }
    #[test]
    fn xlsx_rows_dates_sheet_selection_and_stripped_counts() {
        let copy = xlsx_copy(xlsx(SST, CELLS), LIMITS).unwrap();
        let mut archive = zip::ZipArchive::new(copy).unwrap();
        let mut text = String::new();
        archive
            .by_name("xl/sharedStrings.xml")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert!(!text.contains("uniqueCount"));
        assert!(!text.contains("count="));
        let mut doc = open(xlsx(SST, CELLS), Format::Xlsx).unwrap();
        assert_eq!(doc.unit_count(), 3);
        assert!(doc.unit(0).unwrap().text.ends_with("1\tСумма\tДва"));
        assert_eq!(doc.unit(1).unwrap().text, "2\t\t1234.5");
        assert_eq!(doc.unit_range(&json!({"sheet":"2"})).unwrap(), 2..3);
        assert_eq!(doc.unit_range(&json!({"sheet":"Второй"})).unwrap(), 2..3);
        let date = doc.unit(2).unwrap();
        assert_eq!(date.locator, json!({"sheet":"Второй","row":3}));
        assert!(date.text.contains("2024-01-01T00:00:00.000"));
        assert!(date.heading.unwrap().contains("hidden"));
        assert_eq!(doc.summary()["sheets"][1]["visible"], false);
        assert_eq!(
            open_with_limits(
                xlsx(SST, CELLS),
                Format::Xlsx,
                Limits {
                    strings: 1,
                    ..LIMITS
                }
            )
            .err()
            .unwrap()
            .code,
            "FILE_TOO_LARGE"
        );
    }
    #[test]
    fn xlsx_chart_sheets_remain_listed_and_empty() {
        let source = zip(&[
            ("_rels/.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.into()),
            ("xl/workbook.xml",r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/><sheet name="Chart" sheetId="2" r:id="rId2"/></sheets></workbook>"#.into()),
            ("xl/_rels/workbook.xml.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet" Target="chartsheets/sheet2.xml"/></Relationships>"#.into()),
            ("xl/worksheets/sheet1.xml",r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>42</v></c></row></sheetData></worksheet>"#.into()),
            ("xl/chartsheets/sheet2.xml",r#"<chartsheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><drawing r:id="chart1"/></chartsheet>"#.into()),
        ],false);
        let mut doc = open(source, Format::Xlsx).unwrap();
        assert_eq!(
            doc.summary()["sheets"][1],
            json!({"index":2,"name":"Chart","visible":true,"rows":0,"columns":0})
        );
        assert_eq!(doc.unit_range(&json!({"sheet":"Chart"})).unwrap(), 1..1);
        assert_eq!(doc.unit_count(), 1);
        assert!(doc.unit(0).unwrap().text.ends_with("1\t42"));
    }
    #[test]
    fn xlsx_styled_empty_cells_do_not_expand_value_extent() {
        for last in ["Z2000", "XFD1048576"] {
            let cells = format!(
                r#"<row r="1"><c r="A1"><v>42</v></c><c r="C1"><v>7</v></c></row><row r="2000"><c r="{last}" s="1"/></row>"#
            );
            let mut doc = open(xlsx(SST, &cells), Format::Xlsx).unwrap();
            assert_eq!(doc.summary()["sheets"][0]["rows"], 1);
            assert_eq!(doc.summary()["sheets"][0]["columns"], 3);
            assert_eq!(doc.unit_count(), 2); // One value row in each worksheet.
            assert_eq!(doc.unit(0).unwrap().text, "row\tA\tB\tC\n1\t42\t\t7");
        }
    }
    #[test]
    fn derived_zip_buffer_grows_amortized_and_preserves_seek_writes() {
        let ceiling = 10 * 1024 * 1024;
        let mut buffer = ExactBuffer(Cursor::new(Vec::new()), ceiling);
        let mut growths = 0;
        for _ in 0..1024 {
            let old = buffer.0.get_ref().capacity();
            buffer.write_all(&[7; 8192]).unwrap();
            growths += usize::from(buffer.0.get_ref().capacity() != old);
        }
        assert!(growths < 20, "buffer grew {growths} times for 8 MiB");
        buffer.write_all(&[7]).unwrap();
        assert!(buffer.0.get_ref().capacity() <= ceiling);
        buffer.seek(SeekFrom::Start(4)).unwrap();
        buffer.write_all(&[1, 2, 3]).unwrap();
        assert_eq!(&buffer.0.get_ref()[..8], &[7, 7, 7, 7, 1, 2, 3, 7]);
        assert_eq!(buffer.0.get_ref().len(), 8 * 1024 * 1024 + 1);
    }
    #[test]
    fn xlsx_copy_reserves_declared_parts_and_zip_headers_once() {
        let parts: Vec<_> = (0..256)
            .map(|i| {
                (
                    format!("Папка/part{i}.xml"),
                    format!("<part>{}</part>", "x".repeat(8192)),
                )
            })
            .collect();
        let entries: Vec<_> = parts
            .iter()
            .map(|(name, text)| (name.as_str(), text.clone()))
            .collect();
        let source = zip(&entries, false);
        let Source::Memory(ref original) = source else {
            unreachable!()
        };
        let original_len = original.get_ref().len();
        let Source::Memory(copy) = xlsx_copy(source, LIMITS).unwrap() else {
            unreachable!()
        };
        assert_eq!(copy.get_ref().len(), original_len);
        // Accurate declared sizes, including Unicode names: no doubling or tail growth.
        assert_eq!(copy.get_ref().capacity(), original_len);
        let mut archive = zip::ZipArchive::new(copy).unwrap();
        assert_eq!(archive.len(), 256);
        let mut text = String::new();
        archive
            .by_name("Папка/part255.xml")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, parts[255].1);
    }
    #[test]
    fn xlsx_extent_expanded_text_and_inflation_limits() {
        let far = r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="1048576"><c r="XFD1048576"><v>2</v></c></row>"#;
        assert_eq!(
            open(xlsx(SST, far), Format::Xlsx).err().unwrap().code,
            "FILE_TOO_LARGE"
        );
        let sst = format!("<sst><si><t>{}</t></si></sst>", "x".repeat(10_000));
        let cells = (1..=200)
            .map(|r| format!(r#"<row r="{r}"><c r="A{r}" t="s"><v>0</v></c></row>"#))
            .collect::<String>();
        assert_eq!(
            open_with_limits(
                xlsx(&sst, &cells),
                Format::Xlsx,
                Limits {
                    text: 1024 * 1024,
                    ..LIMITS
                }
            )
            .err()
            .unwrap()
            .code,
            "FILE_TOO_LARGE"
        );
        assert_eq!(
            open(xlsx(&sst, &cells), Format::Xlsx).unwrap().unit_count(),
            201
        );
        let payload = "z".repeat(100_000);
        assert_eq!(
            xlsx_copy(
                zip(&[("huge.xml", payload)], true),
                Limits {
                    unpacked: 1000,
                    ..LIMITS
                }
            )
            .err()
            .unwrap()
            .code,
            "ZIP_BOMB"
        );
        // Lie in the directory so the actual-inflation guard, not the declared sum, stops the entry.
        let Source::Memory(mut source) = zip(&[("huge.xml", "z".repeat(100_000))], true) else {
            unreachable!()
        };
        let bytes = source.get_mut();
        let c = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        bytes[c + 24..c + 28].copy_from_slice(&10u32.to_le_bytes());
        assert_eq!(
            xlsx_copy(
                Source::Memory(source),
                Limits {
                    unpacked: 1000,
                    ..LIMITS
                }
            )
            .err()
            .unwrap()
            .code,
            "ZIP_BOMB"
        );
    }
    #[test]
    fn archive_entry_ratio_and_shared_string_xml_guards() {
        assert_eq!(
            xlsx_copy(
                zip(&[("bomb.xml", "z".repeat(2 * 1024 * 1024))], true),
                LIMITS
            )
            .err()
            .unwrap()
            .code,
            "ZIP_BOMB"
        );
        let mut many = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..=ZIP_ENTRY_LIMIT {
            many.start_file(format!("{i}.bin"), zip::write::SimpleFileOptions::default())
                .unwrap();
        }
        assert_eq!(
            xlsx_copy(Source::Memory(many.finish().unwrap()), LIMITS)
                .err()
                .unwrap()
                .code,
            "ZIP_BOMB"
        );
        for text in ["<wrong/>", "<sst><si>", "<sst><si></sst>"] {
            assert_eq!(
                xlsx_copy(zip(&[("xl/sharedStrings.xml", text.into())], false), LIMITS)
                    .err()
                    .unwrap()
                    .code,
                "DOCUMENT_INVALID"
            );
        }
        let mut clean = Vec::new();
        shared_strings(br#"<x:sst xmlns:x="x" x:count="1099511627776" uniqueCount="3" keep="yes"><x:si/></x:sst>"#,&mut clean,1).unwrap();
        let clean = String::from_utf8(clean).unwrap();
        assert!(!clean.contains("count="));
        assert!(!clean.contains("uniqueCount"));
        assert!(clean.contains("keep=\"yes\""));
        let mut doc = open(
            xlsx(
                "<sst><si><t>x&#x9;y&#xA;z</t></si></sst>",
                r#"<row><c r="A1" t="s"><v>0</v></c></row>"#,
            ),
            Format::Xlsx,
        )
        .unwrap();
        assert!(doc.unit(0).unwrap().text.ends_with("x y ⏎ z"));
    }
    #[test]
    fn cyclic_cfb_chain_and_truncated_workbook_fail_before_calamine() {
        let bytes = formula_workbook(2, 8192, &[0x1e, 1, 0], false);
        let Source::Memory(copy) = xls_copy(cfb(&bytes), LIMITS).unwrap() else {
            unreachable!()
        };
        let mut bytes = copy.into_inner();
        let directory = u32_at(&bytes, 48).unwrap() as usize;
        let fat = u32_at(&bytes, 76).unwrap() as usize;
        let at = (fat + 1) * 512 + directory * 4;
        bytes[at..at + 4].copy_from_slice(&(directory as u32).to_le_bytes());
        assert_eq!(
            open(Source::Memory(Cursor::new(bytes)), Format::Xls)
                .err()
                .unwrap()
                .code,
            "DOCUMENT_INVALID"
        );
        let bytes = workbook(&[], &[("Лист".into(), basic())]);
        for cut in [0, 1, 20, bytes.len() - 3] {
            assert_eq!(
                open(cfb(&bytes[..cut]), Format::Xls).err().unwrap().code,
                "DOCUMENT_INVALID"
            );
        }
    }
    #[test]
    fn xls_values_extents_and_rebuilt_forged_cfb() {
        let bytes = workbook(&[], &[("Лист".into(), basic())]);
        let Source::Memory(mut source) = cfb(&bytes) else {
            unreachable!()
        };
        source.get_mut()[44..48].copy_from_slice(&u32::MAX.to_le_bytes());
        let copy = xls_copy(Source::Memory(source), LIMITS).unwrap();
        let Source::Memory(ref memory) = copy else {
            unreachable!()
        };
        assert!(u32_at(memory.get_ref(), 44).unwrap() as u64 * 4 <= 1024 * 1024);
        assert!(u32_at(memory.get_ref(), 62).unwrap() as u64 * 4 <= 1024 * 1024);
        let mut doc = open(copy, Format::Xls).unwrap();
        assert!(doc.unit(0).unwrap().text.ends_with("1\tСумма"));
        assert_eq!(doc.unit(1).unwrap().text, "2\t\t1234.5");
        let mut sheet = basic();
        let mut far = vec![0; 14];
        far[..2].copy_from_slice(&65535u16.to_le_bytes());
        far[2..4].copy_from_slice(&255u16.to_le_bytes());
        rec(&mut sheet, 0x0203, &far);
        assert_eq!(
            open(cfb(&workbook(&[], &[("Лист".into(), sheet)])), Format::Xls)
                .err()
                .unwrap()
                .code,
            "FILE_TOO_LARGE"
        );
        let mut sheet = Vec::new();
        let mut dims = vec![0; 14];
        dims[4..8].copy_from_slice(&1_000_000u32.to_le_bytes());
        dims[10..12].copy_from_slice(&200u16.to_le_bytes());
        rec(&mut sheet, 0x0200, &dims);
        assert_eq!(
            open(cfb(&workbook(&[], &[("Лист".into(), sheet)])), Format::Xls)
                .err()
                .unwrap()
                .code,
            "FILE_TOO_LARGE"
        );
    }
    #[test]
    fn xls_empty_dimensions_are_accepted_without_charging_cells() {
        let mut empty = Vec::new();
        rec(&mut empty, 0x0200, &[0; 14]);
        let mut doc = open(
            cfb(&workbook(
                &[],
                &[("Data".into(), basic()), ("Empty".into(), empty.clone())],
            )),
            Format::Xls,
        )
        .unwrap();
        assert_eq!(doc.summary()["sheets"][1]["rows"], 0);
        assert_eq!(doc.summary()["sheets"][1]["columns"], 0);
        assert_eq!(doc.unit_range(&json!({"sheet":"Empty"})).unwrap(), 2..2);
        assert!(doc.unit(0).unwrap().text.contains("Сумма"));
        // Equal nonzero endpoints are also empty, rather than reversed.
        for (rf, rl, cf, cl) in [(0u32, 0u32, 0u16, 0u16), (1, 1, 0, 1), (0, 1, 1, 1)] {
            let mut dims = Vec::new();
            dims.extend(rf.to_le_bytes());
            dims.extend(rl.to_le_bytes());
            dims.extend(cf.to_le_bytes());
            dims.extend(cl.to_le_bytes());
            dims.extend([0, 0]);
            let mut sheet = Vec::new();
            rec(&mut sheet, 0x0200, &dims);
            xls_prescan(
                &mut workbook(&[], &[("Empty".into(), sheet)]),
                Limits { cells: 0, ..LIMITS },
            )
            .unwrap();
        }
    }
    #[test]
    fn xls_nonzero_empty_dimensions_open_in_debug_and_release() {
        for (rf, rl, cf, cl) in [(1u32, 1u32, 0u16, 1u16), (0, 1, 1, 1)] {
            let mut dims = Vec::new();
            dims.extend(rf.to_le_bytes());
            dims.extend(rl.to_le_bytes());
            dims.extend(cf.to_le_bytes());
            dims.extend(cl.to_le_bytes());
            dims.extend([0, 0]);
            let mut sheet = Vec::new();
            rec(&mut sheet, 0x0200, &dims);
            let doc = open(cfb(&workbook(&[], &[("Empty".into(), sheet)])), Format::Xls).unwrap();
            assert_eq!(doc.summary()["sheets"][0]["rows"], 0);
            assert_eq!(doc.unit_count(), 0);
        }
    }
    #[test]
    fn xls_orphan_string_cannot_expand_a_far_cell_range() {
        let mut dimensions = Vec::new();
        dimensions.extend(65_535u32.to_le_bytes());
        dimensions.extend(65_536u32.to_le_bytes());
        dimensions.extend(255u16.to_le_bytes());
        dimensions.extend(256u16.to_le_bytes());
        dimensions.extend([0, 0]);
        let mut string = vec![6, 0, 1];
        string.extend("orphan".encode_utf16().flat_map(u16::to_le_bytes));
        let mut number = vec![0; 6];
        number[..2].copy_from_slice(&65_535u16.to_le_bytes());
        number[2..4].copy_from_slice(&255u16.to_le_bytes());
        number.extend(42f64.to_le_bytes());
        let mut sheet = Vec::new();
        rec(&mut sheet, 0x0200, &dimensions);
        rec(&mut sheet, 0x0207, &string);
        rec(&mut sheet, 0x0203, &number);
        // Stop at the shield in RED; never let the old code allocate 16M cells.
        for prior_formula_sheet in [false, true] {
            let mut sheets = Vec::new();
            if prior_formula_sheet {
                let mut earlier = Vec::new();
                rec(&mut earlier, 0x0006, &formula(0, &[0x1e, 1, 0], 0));
                sheets.push(("Earlier".into(), earlier));
            }
            sheets.push(("Orphan".into(), sheet.clone()));
            let bytes = workbook(&[], &sheets);
            assert_eq!(
                xls_copy(cfb(&bytes), LIMITS).err().map(|e| e.code),
                Some("DOCUMENT_INVALID")
            );
            assert_eq!(
                open(cfb(&bytes), Format::Xls).err().map(|e| e.code),
                Some("DOCUMENT_INVALID")
            );
        }
        // A STRING following its FORMULA has a bounded, already charged position.
        let mut valid = Vec::new();
        rec(&mut valid, 0x0200, &dimensions);
        let mut f = formula(65_535, &[0x1e, 1, 0], 0);
        f[2..4].copy_from_slice(&255u16.to_le_bytes());
        f[6..14].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0xff, 0xff]);
        rec(&mut valid, 0x0006, &f);
        rec(&mut valid, 0x0207, &string);
        let mut doc = open(cfb(&workbook(&[], &[("Valid".into(), valid)])), Format::Xls).unwrap();
        assert_eq!(doc.unit_count(), 1);
        assert_eq!(doc.summary()["sheets"][0]["columns"], 1);
        assert_eq!(
            doc.unit(0).unwrap().locator,
            json!({"sheet":"Valid","row":65536})
        );
        assert!(doc.unit(0).unwrap().text.ends_with("65536\torphan"));
    }
    #[test]
    fn xls_shared_string_references_continue_and_entry_count() {
        for split in [false, true] {
            let (g, s) = repeated_sst(split);
            let bytes = workbook(&g, &[("Лист".into(), s)]);
            assert_eq!(
                open_with_limits(
                    cfb(&bytes),
                    Format::Xls,
                    Limits {
                        text: 1024 * 1024,
                        ..LIMITS
                    }
                )
                .err()
                .unwrap()
                .code,
                "FILE_TOO_LARGE"
            );
            assert_eq!(open(cfb(&bytes), Format::Xls).unwrap().unit_count(), 200);
        }
        let mut g = Vec::new();
        rec(&mut g, 0xfc, &[2, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            open_with_limits(
                cfb(&workbook(&g, &[("Лист".into(), basic())])),
                Format::Xls,
                Limits {
                    strings: 1,
                    ..LIMITS
                }
            )
            .err()
            .unwrap()
            .code,
            "FILE_TOO_LARGE"
        );
    }
    #[test]
    fn xls_rejects_every_overlapping_substream_before_open() {
        let bytes = workbook(&[], &[("Один".into(), basic()), ("Два".into(), basic())]);
        let mut at = 0;
        let mut patches = Vec::new();
        while at < bytes.len() {
            let r = record(&bytes, at).unwrap();
            if r.typ == 0x85 {
                patches.push(at + 4);
            }
            if r.typ == 0xa {
                break;
            }
            at = r.end;
        }
        let first = u32_at(&bytes, patches[0]).unwrap();
        for offset in [first, first + 20, 0, bytes.len() as u32 + 1] {
            let mut bad = bytes.clone();
            bad[patches[1]..patches[1] + 4].copy_from_slice(&offset.to_le_bytes());
            let e = open(cfb(&bad), Format::Xls).err().unwrap();
            assert_eq!(e.code, "DOCUMENT_INVALID");
            assert_eq!(e.message, "overlapping sheet substreams");
        }
    }
    fn formula_workbook(count: usize, padding: usize, tokens: &[u8], continued: bool) -> Vec<u8> {
        let mut sheet = Vec::new();
        for row in 0..count {
            let mut data = formula(row as u16, tokens, padding);
            data[2..4].copy_from_slice(&((row / 65_536) as u16).to_le_bytes());
            if continued && row == 0 {
                let cut = data.len() / 2;
                rec(&mut sheet, 6, &data[..cut]);
                rec(&mut sheet, 0x3c, &data[cut..]);
            } else {
                rec(&mut sheet, 6, &data);
            }
        }
        let mut globals = Vec::new();
        rec(&mut globals, 0x17, &[1, 0, 0, 0, 0, 0, 0, 0]); // XTI 0 refers to this sheet.
        workbook(&globals, &[("法".repeat(255), sheet)])
    }
    fn biff8_reference_tokens() -> Vec<u8> {
        // BIFF8 Ref's fourth byte is a token error if read using pre-BIFF8 widths.
        let mut tokens = vec![0x24, 1, 0, 0, 0xff];
        for _ in 0..1000 {
            tokens.extend([0x3a, 0, 0, 1, 0, 1, 0, 3]);
        }
        tokens
    }
    #[test]
    fn xls_bof_token_widths_match_known_and_short_bof_records() {
        let tokens = biff8_reference_tokens();
        for (version, dt, old) in [
            (2u16, Some(5u16), true),
            (7, Some(5), true),
            (0x0200, Some(5), true),
            (0x0300, Some(5), true),
            (0x0400, Some(5), true),
            (0x0500, Some(5), true),
            (0, Some(0x1000), true),
            (0, Some(5), false),
            (0, None, false),
            (1, None, false),
            (0x0600, Some(5), false),
        ] {
            let mut bytes = formula_workbook(1, 0, &tokens, false);
            let mut data = version.to_le_bytes().to_vec();
            if let Some(dt) = dt {
                data.extend(dt.to_le_bytes());
            } else if version == 0 {
                data.push(0x10); // Truncated dt: calamine still uses zero, not this byte.
            }
            let mut header = Vec::new();
            rec(&mut header, 0x0809, &data);
            let removed = 20 - header.len();
            bytes.splice(..20, header);
            // Relocate the sheet after shortening the global BOF.
            let mut pos = 0;
            loop {
                let r = record(&bytes, pos).unwrap();
                let (typ, end) = (r.typ, r.end);
                if typ == 0x85 {
                    let offset = u32_at(&bytes, pos + 4).unwrap() - removed as u32;
                    bytes[pos + 4..pos + 8].copy_from_slice(&offset.to_le_bytes());
                }
                if typ == 0xa {
                    break;
                }
                pos = end;
            }
            let outcome = xls_prescan(
                &mut bytes,
                Limits {
                    text: 64 * 1024,
                    ..LIMITS
                },
            );
            assert_eq!(outcome.is_ok(), old, "BOF {version:#06x}, dt {dt:?}");
        }
    }
    #[test]
    fn xls_bof_fallback_versions_charge_biff8_formulas() {
        let tokens = biff8_reference_tokens();
        for version in [0u16, 1, 0x0600, 0xffff] {
            let mut bytes = formula_workbook(2, 0, &tokens, false);
            bytes[4..6].copy_from_slice(&version.to_le_bytes()); // dt remains 5.
            assert_eq!(
                open_with_limits(
                    cfb(&bytes),
                    Format::Xls,
                    Limits {
                        text: 64 * 1024,
                        ..LIMITS
                    }
                )
                .err()
                .map(|e| e.code),
                Some("FILE_TOO_LARGE"),
                "BOF {version:#06x}"
            );
            let mut doc = open(cfb(&bytes), Format::Xls).unwrap();
            let formulas = doc.book.worksheet_formula(&"法".repeat(255)).unwrap();
            assert!(formulas.used_cells().next().unwrap().2.len() > 765_000);
        }
    }
    #[test]
    fn xls_padded_and_error_formulas_are_charged_even_when_text_is_empty() {
        let bytes = formula_workbook(200, 8192, &[0x1e, 1, 0], true);
        assert!(bytes.len() > 4096); // Regression: a regular Workbook stream, with no mini stream.
        assert_eq!(
            open_with_limits(
                cfb(&bytes),
                Format::Xls,
                Limits {
                    text: 1024 * 1024,
                    ..LIMITS
                }
            )
            .err()
            .unwrap()
            .code,
            "FILE_TOO_LARGE"
        );
        open(cfb(&bytes), Format::Xls).unwrap();
        let mut sheet = Vec::new();
        let mut charged = 0;
        for (row, tokens) in [vec![], vec![0xff], vec![3]].iter().enumerate() {
            let data = formula(row as u16, tokens, 0);
            rec(&mut sheet, 6, &data);
            let r = Record {
                typ: 6,
                parts: vec![&data],
                end: 0,
            };
            let c = formula_charge(&r, 4, &[], false).unwrap();
            assert!(c >= 256);
            charged += c;
        }
        let mut doc = open(cfb(&workbook(&[], &[("Лист".into(), sheet)])), Format::Xls).unwrap();
        let formulas = doc.book.worksheet_formula("Лист").unwrap();
        let actual: usize = formulas.used_cells().map(|(_, _, s)| s.len()).sum();
        assert!(charged >= actual as u64);
        assert!(actual > 0);
    }
    fn mixed_tokens() -> Vec<u8> {
        let mut t = vec![
            0x23, 1, 0, 0, 0, 0x3a, 0, 0, 1, 0, 1, 0, 3, 0x17, 3, 0, b'a', b'b', b'c', 3, 0x1f,
        ];
        t.extend(1e300f64.to_le_bytes());
        t.push(3);
        t.extend([
            0x19, 0x40, 0, 200, 0x22, 1, 4, 0, 0x25, 0, 0, 1, 0, 0xff, 0xff, 0xff, 0xff, 3,
        ]);
        t
    }
    #[test]
    fn xls_mixed_formula_tokens_bound_real_calamine_strings() {
        let mut globals = Vec::new();
        let mut name = vec![0; 14];
        name[3] = 255;
        name[4..6].copy_from_slice(&0u16.to_le_bytes());
        name.push(1);
        name.extend("Н".repeat(255).encode_utf16().flat_map(u16::to_le_bytes));
        rec(&mut globals, 0x18, &name);
        rec(&mut globals, 0x17, &[1, 0, 0, 0, 0, 0, 0, 0]);
        let tokens = mixed_tokens();
        let mut sheet = Vec::new();
        let mut charged = 0;
        for row in 0..200u16 {
            let data = formula(row, &tokens, 0);
            charged += formula_charge(
                &Record {
                    typ: 6,
                    parts: vec![&data],
                    end: 0,
                },
                765,
                &[765],
                false,
            )
            .unwrap();
            rec(&mut sheet, 6, &data);
        }
        let bytes = workbook(&globals, &[("Я".repeat(255), sheet)]);
        assert_eq!(
            open_with_limits(
                cfb(&bytes),
                Format::Xls,
                Limits {
                    text: 64 * 1024,
                    ..LIMITS
                }
            )
            .err()
            .unwrap()
            .code,
            "FILE_TOO_LARGE"
        );
        let mut doc = open(cfb(&bytes), Format::Xls).unwrap();
        let f = doc.book.worksheet_formula(&"Я".repeat(255)).unwrap();
        assert!(charged >= f.used_cells().map(|(_, _, s)| s.len() as u64).sum());
    }
    #[test]
    #[ignore = "release-only RSS measurement with synthetic near-limit XLS allocations"]
    fn xls_formula_memory_budget() {
        // Isolate one real read from fixture construction and from libmalloc's
        // retained blocks after earlier cases. This branch exists only in tests.
        if let Some(path) = std::env::var_os("LOCAL_FILES_TEST_XLS_PROBE") {
            let path = std::path::PathBuf::from(path);
            let dir = cap_std::fs::Dir::open_ambient_dir(
                path.parent().unwrap(),
                cap_std::ambient_authority(),
            )
            .unwrap();
            let start = std::time::Instant::now();
            let mut doc = open(
                Source::File(std::io::BufReader::new(
                    dir.open(path.file_name().unwrap()).unwrap(),
                )),
                Format::Xls,
            )
            .unwrap();
            assert!(doc.unit_count() > 0);
            assert!(!doc.unit(0).unwrap().text.is_empty());
            if matches!(
                path.file_stem().unwrap().to_str(),
                Some("3d" | "bof0" | "bof1")
            ) {
                let formulas = doc.book.worksheet_formula(&"法".repeat(255)).unwrap();
                assert!(formulas.used_cells().next().unwrap().2.len() > 765_000);
            }
            println!(
                "reader rows={} elapsed={:?}",
                doc.unit_count(),
                start.elapsed()
            );
            #[cfg(target_os = "macos")]
            {
                let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
                // SAFETY: getrusage writes this properly sized output struct on success.
                assert_eq!(
                    unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
                    0
                );
                // SAFETY: the successful call initialized the output.
                let bytes = unsafe { usage.assume_init() }.ru_maxrss;
                println!("reader peakRssBytes={bytes}");
                // C0 sheet maximum: inflated parts 256 MiB + expanded text 64 MiB
                // + shared-string headers 64 MiB + working set 64 MiB.
                assert!(
                    bytes <= 448 * 1024 * 1024,
                    "reader exceeds the C0 sheet budget"
                );
            }
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let cases = [
            ("padding", vec![0x1e, 1, 0], 8192usize, 0x0600u16),
            (
                "3d",
                {
                    let mut t = Vec::new();
                    for i in 0..1000 {
                        t.extend([0x3a, 0, 0, 1, 0, 1, 0]);
                        if i > 0 {
                            t.push(3);
                        }
                    }
                    t
                },
                0,
                0x0600,
            ),
            ("bof0", biff8_reference_tokens(), 0, 0),
            ("bof1", biff8_reference_tokens(), 0, 1),
            ("errors", vec![0xff], 0, 0x0600),
            ("empty", vec![], 0, 0x0600),
            ("stack_error", vec![3], 0, 0x0600),
        ];
        for (name, tokens, padding, version) in cases {
            let data = formula(0, &tokens, padding);
            let per = formula_charge(
                &Record {
                    typ: 6,
                    parts: vec![&data],
                    end: 0,
                },
                765,
                &[],
                false,
            )
            .unwrap();
            let count = (SHEET_TEXT_LIMIT / per) as usize;
            let mut bytes = formula_workbook(count, padding, &tokens, false);
            bytes[4..6].copy_from_slice(&version.to_le_bytes());
            let path = tmp.path().join(format!("{name}.xls"));
            let Source::Memory(source) = cfb(&bytes) else {
                unreachable!()
            };
            std::fs::write(&path, source.into_inner()).unwrap();
            drop(bytes);
            let disk_size = std::fs::metadata(&path).unwrap().len();
            assert!(disk_size <= SHEET_LIMIT);
            println!(
                "{name}: disk={disk_size} charge={} formulas={count}",
                count as u64 * per,
            );
            assert!(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "sheets::tests::xls_formula_memory_budget",
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1"
                    ])
                    .env("LOCAL_FILES_TEST_XLS_PROBE", &path)
                    .status()
                    .unwrap()
                    .success()
            );
            // The over-limit file must be refused by the pre-scan, not parsed by calamine.
            let mut over = formula_workbook(count + 1, padding, &tokens, false);
            over[4..6].copy_from_slice(&version.to_le_bytes());
            assert_eq!(
                open(cfb(&over), Format::Xls).err().unwrap().code,
                "FILE_TOO_LARGE"
            );
        }
        let mut overlap = workbook(&[], &[("One".into(), basic()), ("Two".into(), basic())]);
        let mut pos = 0;
        let mut first = None;
        loop {
            let r = record(&overlap, pos).unwrap();
            let end = r.end;
            let typ = r.typ;
            drop(r);
            if typ == 0x85 {
                if let Some(first) = first {
                    overlap[pos + 4..pos + 8].copy_from_slice(&u32::to_le_bytes(first));
                    break;
                }
                first = Some(u32_at(&overlap, pos + 4).unwrap());
            }
            pos = end;
        }
        assert_eq!(
            open(cfb(&overlap), Format::Xls).err().unwrap().code,
            "DOCUMENT_INVALID"
        );
    }
}
