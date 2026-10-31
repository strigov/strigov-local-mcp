use crate::filesystem::{Result, error, io_error};
use std::borrow::Cow;
use std::io::BufRead;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextEncoding {
    Utf8,
    Windows1251,
}

impl TextEncoding {
    pub(crate) fn label(self) -> &'static str {
        match self {
            TextEncoding::Utf8 => "utf-8",
            TextEncoding::Windows1251 => "windows-1251",
        }
    }
}

#[inline]
fn is_forbidden(b: u8) -> bool {
    matches!(b, 0x00..=0x08 | 0x0B | 0x0E..=0x1F | 0x7F | 0x98)
}

pub(crate) struct Detector {
    legacy: bool,
    utf8_ok: bool,
    forbidden_seen: bool,
    prefix: Vec<u8>,
}

impl Detector {
    pub(crate) fn new(legacy: bool) -> Self {
        Self {
            legacy,
            utf8_ok: true,
            forbidden_seen: false,
            prefix: Vec::with_capacity(3),
        }
    }

    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        if self.prefix.len() < 3 {
            let take = (3 - self.prefix.len()).min(chunk.len());
            self.prefix.extend_from_slice(&chunk[..take]);
        }
        if self.utf8_ok && std::str::from_utf8(chunk).is_err() {
            self.utf8_ok = false;
        }
        if !self.forbidden_seen {
            for &b in chunk {
                if is_forbidden(b) {
                    self.forbidden_seen = true;
                    break;
                }
            }
        }
    }

    pub(crate) fn finish(&self) -> Result<TextEncoding> {
        if self.utf8_ok {
            return Ok(TextEncoding::Utf8);
        }
        if !self.legacy {
            return Err(error(
                "NOT_TEXT",
                "File is not valid UTF-8. Use read_document for document formats.",
            ));
        }
        if self.prefix.starts_with(b"\xef\xbb\xbf") {
            return Err(error("NOT_TEXT", "File looks like damaged UTF-8"));
        }
        if self.forbidden_seen {
            return Err(error(
                "NOT_TEXT",
                "File is neither UTF-8 nor Windows-1251 text",
            ));
        }
        Ok(TextEncoding::Windows1251)
    }
}

pub(crate) fn scan<R: BufRead>(
    reader: R,
    size: u64,
    legacy: bool,
) -> Result<(TextEncoding, usize)> {
    let mut bounded = reader.take(size.saturating_add(1));
    let mut detector = Detector::new(legacy);
    let mut buf = Vec::new();
    let mut bytes = 0u64;
    let mut newlines = 0usize;

    loop {
        buf.clear();
        let n = bounded.read_until(b'\n', &mut buf).map_err(io_error)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > size {
            return Err(error("FILE_TOO_LARGE", "File grew beyond the read limit"));
        }
        if buf.ends_with(b"\n") {
            newlines += 1;
        }
        detector.feed(&buf);
    }

    let encoding = detector.finish()?;
    Ok((encoding, 1 + newlines))
}

pub(crate) fn decode<'a>(encoding: TextEncoding, bytes: &'a [u8]) -> Result<Cow<'a, str>> {
    match encoding {
        TextEncoding::Utf8 => std::str::from_utf8(bytes).map(Cow::Borrowed).map_err(|_| {
            error(
                "NOT_TEXT",
                "File is not valid UTF-8. Use read_document for document formats.",
            )
        }),
        TextEncoding::Windows1251 => {
            if bytes.iter().any(|&b| is_forbidden(b)) {
                return Err(error(
                    "NOT_TEXT",
                    "File is neither UTF-8 nor Windows-1251 text",
                ));
            }
            let (cow, _had_errors) = encoding_rs::WINDOWS_1251.decode_without_bom_handling(bytes);
            Ok(cow)
        }
    }
}

pub(crate) fn decode_buffer(bytes: &[u8], legacy: bool) -> Result<(Cow<'_, str>, TextEncoding)> {
    let mut detector = Detector::new(legacy);
    detector.feed(bytes);
    let encoding = detector.finish()?;
    let text = decode(encoding, bytes)?;
    Ok((text, encoding))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp1251_detector_utf8_priority() {
        let mut d = Detector::new(true);
        d.feed(b"Hello world\n");
        assert_eq!(d.finish().unwrap(), TextEncoding::Utf8);

        let mut d = Detector::new(true);
        d.feed(b"\xc2\xbb"); // "»" in UTF-8
        assert_eq!(d.finish().unwrap(), TextEncoding::Utf8);

        let mut d = Detector::new(true);
        d.feed(b"A\x00B"); // UTF-8 with NUL
        assert_eq!(d.finish().unwrap(), TextEncoding::Utf8);

        let mut d = Detector::new(true);
        d.feed(b"abc\x1a"); // UTF-8 with Ctrl-Z
        assert_eq!(d.finish().unwrap(), TextEncoding::Utf8);
    }

    #[test]
    fn cp1251_detector_bom() {
        let mut d = Detector::new(true);
        d.feed(b"\xef\xbb\xbfHello world\n");
        assert_eq!(d.finish().unwrap(), TextEncoding::Utf8);

        let mut d = Detector::new(true);
        d.feed(b"\xef\xbb\xbf\xff\xfe"); // Damaged UTF-8 with BOM
        let err = d.finish().unwrap_err();
        assert_eq!(err.code, "NOT_TEXT");
        assert_eq!(err.message, "File looks like damaged UTF-8");

        let mut d = Detector::new(false);
        d.feed(b"\xef\xbb\xbf\xff\xfe"); // Non-legacy with invalid UTF-8
        let err = d.finish().unwrap_err();
        assert_eq!(err.code, "NOT_TEXT");
        assert_eq!(
            err.message,
            "File is not valid UTF-8. Use read_document for document formats."
        );
    }

    #[test]
    fn cp1251_detector_fixtures_and_ambiguous() {
        // cp1251-nbsp.txt
        let mut d = Detector::new(true);
        d.feed(b"\xc2\xa02024, \xc2\xa02025 \x97 100");
        assert_eq!(d.finish().unwrap(), TextEncoding::Windows1251);

        // text: + \xd1
        let mut d = Detector::new(true);
        d.feed(b"text: \xd1");
        assert_eq!(d.finish().unwrap(), TextEncoding::Windows1251);

        // ambiguous-bytes.txt: B8 FF FF FF FF C3
        let mut d = Detector::new(true);
        d.feed(b"\xb8\xff\xff\xff\xff\xc3");
        assert_eq!(d.finish().unwrap(), TextEncoding::Windows1251);

        // utf16-no-ascii.txt: FF FE AC 20
        let mut d = Detector::new(true);
        d.feed(b"\xff\xfe\xac\x20");
        assert_eq!(d.finish().unwrap(), TextEncoding::Windows1251);

        // utf8-nul.txt: 41 00 42
        let mut d = Detector::new(true);
        d.feed(b"A\x00B");
        assert_eq!(d.finish().unwrap(), TextEncoding::Utf8);
    }

    #[test]
    fn cp1251_detector_forbidden_and_allowed_neighbors() {
        let forbidden = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x0B, 0x0E, 0x0F, 0x10, 0x11,
            0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
            0x7F, 0x98,
        ];
        for &b in &forbidden {
            let mut d = Detector::new(true);
            d.feed(&[0xff, b]);
            let err = d.finish().unwrap_err();
            assert_eq!(err.code, "NOT_TEXT");
            assert_eq!(
                err.message, "File is neither UTF-8 nor Windows-1251 text",
                "byte {:#04x} should be forbidden",
                b
            );
        }

        let allowed = [0x09, 0x0A, 0x0C, 0x0D, 0x97, 0x99];
        for &b in &allowed {
            let mut d = Detector::new(true);
            d.feed(&[0xff, b]);
            assert_eq!(
                d.finish().unwrap(),
                TextEncoding::Windows1251,
                "byte {:#04x} should be allowed",
                b
            );
        }
    }

    #[test]
    fn cp1251_detector_chunked_vs_whole_feed() {
        let cases: Vec<(&[u8], &[&[u8]])> = vec![
            (b"line 1\nline 2\n", &[b"line 1\n", b"line 2\n"]),
            (
                b"\xcf\xee\xf1\xf2\xe0\xed\xee\xe2\xeb\xe5\xed\xe8\xe5\n\xc7\xe0\xec\xe5\xf9\xe0\xfe\xf9\xe8\xe5\n",
                &[
                    b"\xcf\xee\xf1\xf2\xe0\xed\xee\xe2\xeb\xe5\xed\xe8\xe5\n",
                    b"\xc7\xe0\xec\xe5\xf9\xe0\xfe\xf9\xe8\xe5\n",
                ],
            ),
            (
                b"\xef\xbb\xbf\xcf\xee\xf1\n\xc7\xe0\xec\n",
                &[b"\xef\xbb\xbf\xcf\xee\xf1\n", b"\xc7\xe0\xec\n"],
            ),
            (
                b"\xcf\xee\xf1\n\xc7\x00\xec\n",
                &[b"\xcf\xee\xf1\n", b"\xc7\x00\xec\n"],
            ),
        ];

        for (whole, chunks) in cases {
            let mut d_whole = Detector::new(true);
            d_whole.feed(whole);
            let res_whole = d_whole.finish().map_err(|e| (e.code, e.message));

            let mut d_chunked = Detector::new(true);
            for c in chunks {
                d_chunked.feed(c);
            }
            let res_chunked = d_chunked.finish().map_err(|e| (e.code, e.message));

            assert_eq!(res_whole, res_chunked);
        }
    }
}
