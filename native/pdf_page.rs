use crate::{
    documents::{ImageFormat, Source},
    filesystem::{Result, error},
    imaging::{self, IMAGE_DECODE_ALLOC, IMAGE_MAX_SIDE, Region, ViewImage},
    pdf::{PDF_STREAM_LIMIT, PdfDocument},
};
use image::{DynamicImage, GrayImage, ImageDecoder, RgbImage};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use std::{
    collections::{HashMap, HashSet},
    io::{BufReader, Cursor},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // C9 vocabulary; this build implements the user's A+pdf outcome only.
pub(crate) enum VisualChannel {
    Image,
    ImageWithPdfFallback,
    Pdf,
    None,
}
pub(crate) const VISUAL_CHANNEL: VisualChannel = VisualChannel::ImageWithPdfFallback;
pub(crate) const PAGE_PDF_MIN_USABLE: usize = 128 * 1024;
pub(crate) const fn page_pdf_limit(pdf_max: usize) -> usize {
    let safe = pdf_max.saturating_mul(3) / 4;
    let safe = if safe > 1024 * 1024 {
        safe
    } else {
        1024 * 1024
    };
    let cap = if safe < pdf_max { safe } else { pdf_max };
    if cap < 8 * 1024 * 1024 {
        cap
    } else {
        8 * 1024 * 1024
    }
}
pub(crate) const PAGE_PDF_OUTPUT_LIMIT: usize = page_pdf_limit(262137);
const _: () = assert!(
    PAGE_PDF_OUTPUT_LIMIT >= PAGE_PDF_MIN_USABLE
        && matches!(VISUAL_CHANNEL, VisualChannel::ImageWithPdfFallback)
);
pub(crate) const BILEVEL_MAX_PIXELS: u64 = 1 << 28;
pub(crate) const JBIG2_TOTAL_PIXELS: u64 = 4 * BILEVEL_MAX_PIXELS;
pub(crate) const JBIG2_MAX_REFERRED: u32 = 1024;
pub(crate) const JBIG2_MAX_SEGMENTS: u32 = 65_536;
pub(crate) const JBIG2_MAX_SYMBOL_REFS: u64 = 1 << 20;
pub(crate) const JPX_MAX_PIXELS: u64 = 1 << 28;
pub(crate) const JPX_MAX_TILES: u64 = 65_535;
pub(crate) const JPX_SAMPLE_BYTES: u64 = 5;
#[derive(Debug)]
pub(crate) enum PageView {
    Image(ViewImage),
    Pdf(Vec<u8>),
    #[allow(dead_code)]
    NotViewable(&'static str),
}
fn invalid(_: impl std::fmt::Debug) -> crate::filesystem::FsError {
    error("DOCUMENT_INVALID", "Invalid PDF page image")
}
fn cap() -> crate::filesystem::FsError {
    error("IMAGE_LIMIT", "PDF image exceeds decoding limits")
}
#[cfg(test)]
thread_local! {
    static CODEC_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static JPX_DECODE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn codec_call() {
    #[cfg(test)]
    CODEC_CALLS.with(|n| n.set(n.get() + 1));
}
fn pixels(w: u32, h: u32, max: u64) -> Result<u64> {
    let n = u64::from(w) * u64::from(h);
    if n == 0 || n > max { Err(cap()) } else { Ok(n) }
}
fn integer(dict: &Dictionary, key: &[u8], default: i64) -> i64 {
    dict.get(key).and_then(Object::as_i64).unwrap_or(default)
}
fn dimension(dict: &Dictionary, key: &[u8]) -> Result<u32> {
    u32::try_from(integer(dict, key, 0))
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("dimension"))
}
fn deref<'a>(doc: &'a Document, obj: &'a Object) -> Result<&'a Object> {
    doc.dereference(obj).map(|(_, o)| o).map_err(invalid)
}
fn field<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Result<Option<&'a Object>> {
    dict.get(key).ok().map(|obj| deref(doc, obj)).transpose()
}
fn decode_inverted(doc: &Document, stream: &Stream) -> Result<bool> {
    let Some(value) = field(doc, &stream.dict, b"Decode")? else {
        return Ok(false);
    };
    let a = value.as_array().map_err(invalid)?;
    if a.len() == 2 && a[0].as_float().ok() == Some(1.) && a[1].as_float().ok() == Some(0.) {
        return Ok(true);
    }
    if !a.is_empty()
        && a.len().is_multiple_of(2)
        && a.as_chunks::<2>()
            .0
            .iter()
            .all(|pair| pair[0].as_float().ok() == Some(0.) && pair[1].as_float().ok() == Some(1.))
    {
        return Ok(false);
    }
    // Explicit default Decode ranges on Indexed samples are also identity.
    if let Some(Object::Array(space)) = field(doc, &stream.dict, b"ColorSpace")?
        && space
            .first()
            .and_then(|o| o.as_name().ok())
            .is_some_and(|n| matches!(n, b"Indexed" | b"I"))
    {
        let bits = integer(&stream.dict, b"BitsPerComponent", 8);
        if matches!(bits, 1 | 2 | 4 | 8)
            && a.len() == 2
            && a[0].as_float().ok() == Some(0.)
            && a[1].as_float().ok() == Some(((1u16 << bits) - 1) as f32)
        {
            return Ok(false);
        }
    }
    Err(invalid("unsupported Decode mapping"))
}
fn masked(dict: &Dictionary) -> bool {
    dict.has(b"SMask")
        || dict.has(b"Mask")
        || dict
            .get(b"SMaskInData")
            .is_ok_and(|v| v.as_i64().ok() != Some(0))
}
pub(crate) fn view_page(bytes: &[u8], page: u32, region: Option<Region>) -> Result<PageView> {
    let meta = PdfDocument::load(bytes, &HashSet::new())?;
    let page_id = meta.page_id(page)?;
    let paint = match meta.page_paint(page) {
        Ok(paint) => paint,
        Err(_) => {
            drop(meta);
            return single_page_pdf(bytes, page).map(PageView::Pdf);
        }
    };
    if paint.images.len() != 1
        || paint.inline_images != 0
        || paint.visible_text
        || paint.vector_paint
        || paint.clipped
        || !visible_annotations(meta.document(), page_id).is_empty()
        || masked(&meta.image_stream(paint.images[0])?.dict)
    {
        drop(meta);
        return single_page_pdf(bytes, page).map(PageView::Pdf);
    }
    let id = paint.images[0];
    let rotation = meta
        .page_attribute(page, b"Rotate")?
        .as_ref()
        .and_then(|o| meta.document().dereference(o).ok())
        .and_then(|(_, o)| o.as_i64().ok())
        .unwrap_or(0)
        .rem_euclid(360) as u16;
    drop(meta);
    let pdf = PdfDocument::load(bytes, &HashSet::from([id]))?;
    let stream = pdf.image_stream(id)?;
    let decoded = decode_image(&pdf, stream, rotation, region);
    let page_count = pdf.page_count();
    drop(pdf);
    match decoded {
        Ok(mut view) => {
            view.page = Some(page);
            view.pages = Some(page_count);
            Ok(PageView::Image(view))
        }
        Err(_) => single_page_pdf(bytes, page).map(PageView::Pdf),
    }
}
fn decode_image(
    pdf: &PdfDocument,
    stream: &Stream,
    rotation: u16,
    region: Option<Region>,
) -> Result<ViewImage> {
    if stream.dict.has(b"LocalFilesPlaceholder") || stream.content.len() > PDF_STREAM_LIMIT {
        return Err(cap());
    }
    let w = dimension(&stream.dict, b"Width")?;
    let h = dimension(&stream.dict, b"Height")?;
    let filters = if stream.dict.has(b"Filter") {
        stream.filters().map_err(invalid)?
    } else {
        Vec::new()
    };
    let doc = pdf.document();
    let inverted = decode_inverted(doc, stream)?;
    let empty = Dictionary::new();
    let parms = match field(doc, &stream.dict, b"DecodeParms")? {
        Some(Object::Array(a)) if filters.len() == 1 && a.len() == 1 => {
            deref(doc, &a[0])?.as_dict().map_err(invalid)?
        }
        Some(Object::Dictionary(d)) => d,
        None | Some(Object::Null) => &empty,
        _ if filters.len() != 1 => &empty,
        _ => return Err(invalid("DecodeParms")),
    };
    if filters == [b"DCTDecode".as_slice()] {
        if inverted {
            return Err(invalid("JPEG Decode mapping"));
        }
        // A single JPEG decode at most, even when page rotation is required.
        codec_call();
        if rotation == 0 {
            return imaging::view_file(
                &mut Source::Memory(Cursor::new(stream.content.clone())),
                ImageFormat::Jpeg,
                None,
                region,
            );
        }
        let mut reader = image::ImageReader::new(BufReader::new(Cursor::new(&stream.content)))
            .with_guessed_format()
            .map_err(invalid)?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(IMAGE_DECODE_ALLOC);
        reader.limits(limits);
        let mut decoder = reader.into_decoder().map_err(invalid)?;
        if decoder.total_bytes() > IMAGE_DECODE_ALLOC {
            return Err(cap());
        }
        let orientation = decoder.orientation().map_err(invalid)?;
        let mut image = DynamicImage::from_decoder(decoder).map_err(invalid)?;
        image.apply_orientation(orientation);
        return imaging::view_pixels(image, false, rotation, region);
    }
    let (image, lossless) = match filters.as_slice() {
        [b"CCITTFaxDecode"] => (
            DynamicImage::ImageLuma8(decode_ccitt(&stream.content, parms, w, h, inverted)?),
            true,
        ),
        [b"JBIG2Decode"] => {
            let globals = field(doc, parms, b"JBIG2Globals")?
                .map(|g| {
                    g.as_stream()
                        .map_err(invalid)?
                        .decompressed_content_with_limit(PDF_STREAM_LIMIT)
                        .map_err(invalid)
                })
                .transpose()?;
            let header = jbig2_prescan(&stream.content, globals.as_deref())?;
            if (header.width, header.height) != (w, h) {
                return Err(invalid("JBIG2 dimensions"));
            }
            (
                DynamicImage::ImageLuma8(decode_jbig2(
                    &stream.content,
                    globals.as_deref(),
                    inverted,
                )?),
                true,
            )
        }
        [b"JPXDecode"] => {
            if inverted {
                return Err(invalid("JPX Decode mapping"));
            }
            let siz = jpx_siz(&stream.content)?;
            if (siz.width, siz.height) != (w, h) {
                return Err(invalid("JPX dimensions"));
            }
            let want = region.map(|r| {
                let (rw, rh) = if matches!(rotation, 90 | 270) {
                    (r.height, r.width)
                } else {
                    (r.width, r.height)
                };
                (
                    (f64::from(w) * rw).ceil() as u32,
                    (f64::from(h) * rh).ceil() as u32,
                )
            });
            let image = decode_jpx(&stream.content, want)?;
            let channels = if matches!(image, DynamicImage::ImageLuma8(_)) {
                1
            } else {
                3
            };
            // JPX can omit PDF ColorSpace; an explicit value must agree with the codec.
            if let Some(space) = field(doc, &stream.dict, b"ColorSpace")? {
                let codec = hayro_jpeg2000::Image::new(&stream.content, &Default::default())
                    .map_err(invalid)?;
                if color_channels(doc, space)? != u64::from(codec.color_space().num_channels()) {
                    return Err(invalid("JPX ColorSpace"));
                }
            }
            (
                image,
                channels == 1 && integer(&stream.dict, b"BitsPerComponent", 8) == 1,
            )
        }
        _ => {
            if !filters.iter().all(|f| {
                matches!(
                    *f,
                    b"FlateDecode"
                        | b"LZWDecode"
                        | b"RunLengthDecode"
                        | b"ASCIIHexDecode"
                        | b"ASCII85Decode"
                )
            }) {
                return Err(invalid("image filter"));
            }
            (
                raw_image(doc, stream, w, h, inverted)?,
                integer(&stream.dict, b"BitsPerComponent", 8) == 1,
            )
        }
    };
    let mut view = imaging::view_pixels(image, lossless, rotation, region)?;
    (view.source_width, view.source_height) = if matches!(rotation, 90 | 270) {
        (h, w)
    } else {
        (w, h)
    };
    Ok(view)
}
fn color_channels(doc: &Document, space: &Object) -> Result<u64> {
    match deref(doc, space)? {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" => Ok(1),
            b"DeviceRGB" => Ok(3),
            b"DeviceCMYK" => Ok(4),
            _ => Err(invalid("ColorSpace")),
        },
        Object::Array(a)
            if a.first().and_then(|o| o.as_name().ok()) == Some(b"ICCBased") && a.len() == 2 =>
        {
            let n = integer(
                &deref(doc, &a[1])?.as_stream().map_err(invalid)?.dict,
                b"N",
                0,
            );
            if matches!(n, 1 | 3 | 4) {
                Ok(n as u64)
            } else {
                Err(invalid("ICC channels"))
            }
        }
        _ => Err(invalid("ColorSpace")),
    }
}
fn cmyk(c: &[u8; 4]) -> [u8; 3] {
    [0, 1, 2].map(|i| ((u16::from(255 - c[i]) * u16::from(255 - c[3]) + 127) / 255) as u8)
}
fn raw_image(
    doc: &Document,
    stream: &Stream,
    w: u32,
    h: u32,
    inverted: bool,
) -> Result<DynamicImage> {
    let count = pixels(w, h, IMAGE_DECODE_ALLOC)?;
    let mask = stream
        .dict
        .get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false);
    let bits = integer(&stream.dict, b"BitsPerComponent", if mask { 1 } else { 8 });
    let gray = Object::Name(b"DeviceGray".to_vec());
    let space = field(doc, &stream.dict, b"ColorSpace")?.unwrap_or(&gray);
    let indexed = space.as_array().ok().filter(|a| {
        a.first()
            .and_then(|v| v.as_name().ok())
            .is_some_and(|v| matches!(v, b"Indexed" | b"I"))
    });
    let channels = if let Some(a) = indexed {
        if a.len() != 4 {
            return Err(invalid("Indexed"));
        }
        color_channels(doc, &a[1])?
    } else if mask {
        1
    } else {
        color_channels(doc, space)?
    };
    if indexed.is_some() && !matches!(channels, 1 | 3) {
        return Err(invalid("Indexed base color space"));
    }
    if inverted && (channels != 1 || indexed.is_some()) {
        return Err(invalid("raw Decode mapping"));
    }
    if count * channels > IMAGE_DECODE_ALLOC {
        return Err(cap());
    }
    if !(bits == 8 || bits == 1 && channels == 1 || indexed.is_some() && matches!(bits, 1 | 2 | 4))
    {
        return Err(invalid("sample depth"));
    }
    let samples = if indexed.is_some() { 1 } else { channels };
    let row = (u64::from(w) * samples * bits as u64).div_ceil(8) as usize;
    let data = stream
        .decompressed_content_with_limit(IMAGE_DECODE_ALLOC as usize)
        .map_err(invalid)?;
    if data.len() < row * h as usize {
        return Err(invalid("sample length"));
    }
    let palette = if let Some(a) = indexed {
        let high = a[2].as_i64().map_err(invalid)?;
        if !(0..=255).contains(&high) || high >= 1 << bits {
            return Err(invalid("palette size"));
        }
        let size = (high as usize + 1) * channels as usize;
        let bytes = match deref(doc, &a[3])? {
            Object::String(s, _) => s.clone(),
            Object::Stream(s) => s
                .decompressed_content_with_limit(PDF_STREAM_LIMIT)
                .map_err(invalid)?,
            _ => return Err(invalid("palette")),
        };
        if bytes.len() < size {
            return Err(invalid("palette length"));
        }
        Some((bytes, high as usize))
    } else {
        None
    };
    codec_call();
    if let Some((palette, high)) = palette {
        let mut out = Vec::with_capacity((count * channels) as usize);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let bit = x * bits as usize;
                let index = ((data[y * row + bit / 8] >> (8 - bits as usize - bit % 8))
                    & ((1u16 << bits) - 1) as u8) as usize;
                if index > high {
                    return Err(invalid("palette index"));
                }
                out.extend_from_slice(
                    &palette[index * channels as usize..(index + 1) * channels as usize],
                );
            }
        }
        return if channels == 1 {
            Ok(DynamicImage::ImageLuma8(
                GrayImage::from_raw(w, h, out).ok_or_else(|| invalid("pixels"))?,
            ))
        } else {
            Ok(DynamicImage::ImageRgb8(
                RgbImage::from_raw(w, h, out).ok_or_else(|| invalid("pixels"))?,
            ))
        };
    }
    if bits == 1 {
        let data = (0..h as usize)
            .flat_map(|y| (0..w as usize).map(move |x| (y, x)))
            .map(|(y, x)| {
                let white = data[y * row + x / 8] & (128 >> (x % 8)) != 0;
                if white ^ inverted { 255 } else { 0 }
            })
            .collect();
        return Ok(DynamicImage::ImageLuma8(
            GrayImage::from_raw(w, h, data).ok_or_else(|| invalid("pixels"))?,
        ));
    }
    let mut out = data[..(count * channels) as usize].to_vec();
    if channels == 1 {
        if inverted {
            out.iter_mut().for_each(|b| *b = 255 - *b);
        }
        Ok(DynamicImage::ImageLuma8(
            GrayImage::from_raw(w, h, out).ok_or_else(|| invalid("pixels"))?,
        ))
    } else {
        if channels == 4 {
            out = out.as_chunks::<4>().0.iter().flat_map(cmyk).collect();
        }
        Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(w, h, out).ok_or_else(|| invalid("pixels"))?,
        ))
    }
}

fn visible_annotations(doc: &Document, page: ObjectId) -> Vec<Dictionary> {
    let Some(annots) = doc
        .get_dictionary(page)
        .ok()
        .and_then(|d| field(doc, d, b"Annots").ok().flatten())
        .and_then(|o| o.as_array().ok())
    else {
        return Vec::new();
    };
    annots
        .iter()
        .filter_map(|object| {
            let annotation = deref(doc, object).ok()?.as_dict().ok()?;
            if field(doc, annotation, b"Subtype")
                .ok()
                .flatten()
                .and_then(|o| o.as_name().ok())
                == Some(b"Popup")
            {
                return None;
            }
            let flags = match annotation.get(b"F") {
                Ok(value) => deref(doc, value).ok()?.as_i64().ok()?,
                Err(_) => 0,
            };
            if flags & (2 | 32) != 0 {
                return None;
            }
            let rect = field(doc, annotation, b"Rect").ok()??.as_array().ok()?;
            let mut coordinates = [0f64; 4];
            if rect.len() != 4 {
                return None;
            }
            for (coordinate, value) in coordinates.iter_mut().zip(rect) {
                *coordinate = match value {
                    Object::Integer(n) => *n as f64,
                    Object::Real(n) => f64::from(*n),
                    _ => return None,
                };
                if !coordinate.is_finite() {
                    return None;
                }
            }
            if coordinates[0] == coordinates[2] || coordinates[1] == coordinates[3] {
                return None;
            }
            let ap = field(doc, annotation, b"AP").ok()??.as_dict().ok()?;
            let normal = ap.get(b"N").ok()?;
            let normal = match deref(doc, normal).ok()? {
                Object::Stream(_) => normal.clone(),
                Object::Dictionary(states) => {
                    let state = field(doc, annotation, b"AS").ok()??.as_name().ok()?;
                    let appearance = states.get(state).ok()?;
                    deref(doc, appearance).ok()?.as_stream().ok()?;
                    let mut selected = Dictionary::new();
                    selected.set(state, appearance.clone());
                    selected.into()
                }
                _ => return None,
            };
            let mut rebuilt = Dictionary::new();
            for key in [
                b"Type".as_slice(),
                b"Subtype",
                b"Rect",
                b"F",
                b"AS",
                b"Border",
                b"BS",
                b"C",
                b"CA",
                b"BM",
            ] {
                if let Ok(value) = annotation.get(key) {
                    rebuilt.set(key, value.clone());
                }
            }
            rebuilt.set("AP", dictionary! {"N"=>normal});
            Some(rebuilt)
        })
        .collect()
}
fn prepare_page(pdf: PdfDocument, page: u32) -> Result<Document> {
    let id = pdf.page_id(page)?;
    let mut inherited = Vec::new();
    for key in [b"Resources".as_slice(), b"MediaBox", b"CropBox", b"Rotate"] {
        if let Some(value) = pdf.page_attribute(page, key)? {
            inherited.push((key, value));
        }
    }
    let mut doc = pdf.into_document();
    let annotations: Vec<_> = visible_annotations(&doc, id)
        .into_iter()
        .map(|annotation| Object::Reference(doc.add_object(annotation)))
        .collect();
    let pages = doc.new_object_id();
    let catalog = doc.new_object_id();
    let page_dict = doc
        .get_object_mut(id)
        .and_then(Object::as_dict_mut)
        .map_err(invalid)?;
    for (key, value) in inherited {
        page_dict.set(key, value);
    }
    for key in [b"B".as_slice(), b"Thumb"] {
        page_dict.remove(key);
    }
    if annotations.is_empty() {
        page_dict.remove(b"Annots");
    } else {
        page_dict.set("Annots", annotations);
    }
    page_dict.set("Parent", pages);
    doc.objects.insert(
        pages,
        dictionary! {"Type"=>"Pages","Kids"=>vec![Object::Reference(id)],"Count"=>1}.into(),
    );
    doc.objects.insert(
        catalog,
        dictionary! {"Type"=>"Catalog","Pages"=>pages}.into(),
    );
    doc.trailer.set("Root", catalog);
    doc.trailer.remove(b"Encrypt");
    doc.trailer.remove(b"Info");
    // Old xref streams must not keep a previous trailer or another page tree alive.
    doc.trailer.remove(b"Prev");
    doc.trailer.remove(b"XRefStm");
    doc.prune_objects();
    Ok(doc)
}
pub(crate) fn single_page_pdf(bytes: &[u8], page: u32) -> Result<Vec<u8>> {
    let meta = PdfDocument::load(bytes, &HashSet::new())?;
    // Keep every surviving image, including masks and annotation appearances.
    let pruned = prepare_page(meta, page)?;
    let keep: HashSet<ObjectId> = pruned
        .objects
        .iter()
        .filter_map(|(id, o)| {
            o.as_stream()
                .ok()
                .filter(|s| s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image"))
                .map(|_| *id)
        })
        .collect();
    drop(pruned);
    let pdf = PdfDocument::load(bytes, &keep)?;
    let mut doc = prepare_page(pdf, page)?;
    if doc.objects.values().any(|o| {
        o.as_stream()
            .is_ok_and(|s| s.dict.has(b"LocalFilesPlaceholder"))
    }) {
        return Err(invalid("placeholder"));
    }
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(invalid)?;
    if out.len() > PAGE_PDF_OUTPUT_LIMIT {
        return Err(error(
            "OUTPUT_TOO_LARGE",
            "Single-page PDF exceeds the calibrated response limit",
        ));
    }
    Ok(out)
}

fn take<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8]> {
    let end = pos.checked_add(n).ok_or_else(cap)?;
    let bytes = data
        .get(*pos..end)
        .ok_or_else(|| invalid("truncated header"))?;
    *pos = end;
    Ok(bytes)
}
fn be32(data: &[u8], pos: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        data.get(pos..pos + 4)
            .ok_or_else(|| invalid("header"))?
            .try_into()
            .map_err(invalid)?,
    ))
}
#[derive(Debug)]
struct Jbig2Header {
    width: u32,
    height: u32,
}
fn jbig2_exported_symbols(payload: &[u8]) -> Result<u32> {
    // T.88 7.4.2.1; hayro-jbig2 0.3.1 decode/symbol.rs:751-814.
    let mut pos = 0;
    let flags = u16::from_be_bytes(take(payload, &mut pos, 2)?.try_into().map_err(invalid)?);
    if flags & 1 == 0 {
        let template = (flags >> 10) & 3;
        take(payload, &mut pos, if template == 0 { 8 } else { 2 })?;
    }
    if flags & 2 != 0 && flags & (1 << 12) == 0 {
        take(payload, &mut pos, 4)?;
    }
    // Require both SDNUMEXSYMS and SDNUMNEWSYMS, including for unused dictionaries.
    be32(take(payload, &mut pos, 8)?, 0)
}
fn jbig2_prescan(data: &[u8], globals: Option<&[u8]>) -> Result<Jbig2Header> {
    let mut page = None;
    let mut total = 0u64;
    let mut segments = 0;
    let mut dictionaries = HashMap::<u32, u32>::new();
    let mut symbol_consumers = Vec::new();
    let reference = |bytes: &[u8]| bytes.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b));
    for (bytes, is_globals) in globals
        .map(|g| (g, true))
        .into_iter()
        .chain(std::iter::once((data, false)))
    {
        let mut pos = 0;
        while pos < bytes.len() {
            segments += 1;
            if segments > JBIG2_MAX_SEGMENTS {
                return Err(cap());
            }
            let number = u32::from_be_bytes(take(bytes, &mut pos, 4)?.try_into().map_err(invalid)?);
            let flags = take(bytes, &mut pos, 1)?[0];
            let kind = flags & 63;
            // T.88 7.3 / hayro-jbig2 0.3.1 SegmentType::from_type_value.
            if !matches!(
                kind,
                0 | 6 | 7 | 38 | 39 | 42 | 43 | 48 | 49 | 50 | 51 | 52 | 62
            ) {
                return Err(cap());
            }
            let count_byte = take(bytes, &mut pos, 1)?[0];
            let short = count_byte >> 5;
            let count = if short == 7 {
                let rest = take(bytes, &mut pos, 3)?;
                let count = u32::from_be_bytes([count_byte & 31, rest[0], rest[1], rest[2]]);
                if count > JBIG2_MAX_REFERRED {
                    return Err(cap());
                }
                take(bytes, &mut pos, (count as usize + 1).div_ceil(8))?;
                count
            } else if short <= 4 {
                u32::from(short)
            } else {
                return Err(invalid("referred count"));
            };
            let reference_width = if number <= 256 {
                1
            } else if number <= 65536 {
                2
            } else {
                4
            };
            let referred = take(bytes, &mut pos, count as usize * reference_width)?;
            let mut distinct = HashSet::with_capacity(count as usize);
            for bytes in referred.chunks_exact(reference_width) {
                if !distinct.insert(reference(bytes)) {
                    return Err(cap());
                }
            }
            take(bytes, &mut pos, if flags & 64 != 0 { 4 } else { 1 })?;
            let length = u32::from_be_bytes(take(bytes, &mut pos, 4)?.try_into().map_err(invalid)?);
            if length == u32::MAX {
                return Err(cap());
            }
            let payload = take(bytes, &mut pos, length as usize)?;
            if matches!(kind, 0 | 6 | 7) {
                symbol_consumers.push((number, reference_width, referred));
            }
            match kind {
                0 => {
                    let exported = jbig2_exported_symbols(payload)?;
                    dictionaries
                        .entry(number)
                        .and_modify(|n| *n = (*n).max(exported))
                        .or_insert(exported);
                }
                48 => {
                    if is_globals || page.is_some() {
                        return Err(invalid("JBIG2 page information"));
                    }
                    let width = be32(payload, 0)?;
                    let height = be32(payload, 4)?;
                    if height == u32::MAX {
                        return Err(cap());
                    }
                    total = total.saturating_add(pixels(width, height, BILEVEL_MAX_PIXELS)?);
                    page = Some(Jbig2Header { width, height });
                }
                6 | 7 | 38 | 39 | 42 | 43 => {
                    total = total.saturating_add(pixels(
                        be32(payload, 0)?,
                        be32(payload, 4)?,
                        BILEVEL_MAX_PIXELS,
                    )?);
                }
                _ => {}
            }
            if total > JBIG2_TOTAL_PIXELS {
                return Err(cap());
            }
        }
    }
    // hayro sorts segments by number, so wire order is not decode order. Keep
    // borrowed reference lists until all dictionary headers have been read.
    let mut symbol_refs = 0u64;
    for (number, width, referred) in symbol_consumers {
        for bytes in referred.chunks_exact(width) {
            let referred = reference(bytes);
            if referred < number {
                symbol_refs = symbol_refs
                    .saturating_add(u64::from(*dictionaries.get(&referred).unwrap_or(&0)));
                if symbol_refs > JBIG2_MAX_SYMBOL_REFS {
                    return Err(cap());
                }
            }
        }
    }
    page.ok_or_else(|| invalid("missing JBIG2 page information"))
}
struct GraySink {
    data: Vec<u8>,
    width: usize,
    row: usize,
    x: usize,
    inverted: bool,
}
impl GraySink {
    fn new(w: u32, h: u32, inverted: bool) -> Result<Self> {
        let n = pixels(w, h, BILEVEL_MAX_PIXELS)?;
        Ok(Self {
            data: vec![255; n as usize],
            width: w as usize,
            row: 0,
            x: 0,
            inverted,
        })
    }
    fn push(&mut self, white: bool, count: usize) {
        let start = self.row.saturating_mul(self.width).saturating_add(self.x);
        let count = count.min(self.width.saturating_sub(self.x));
        if let Some(target) = self.data.get_mut(start..start.saturating_add(count)) {
            target.fill(if white ^ self.inverted { 255 } else { 0 });
        }
        self.x = self.x.saturating_add(count);
    }
    fn next(&mut self) {
        self.row += 1;
        self.x = 0;
    }
}
impl hayro_ccitt::Decoder for GraySink {
    fn push_pixels(&mut self, white: bool, count: u32) {
        self.push(white, count as usize);
    }
    fn next_line(&mut self) {
        self.next();
    }
}
impl hayro_jbig2::Decoder for GraySink {
    fn push_pixel(&mut self, black: bool) {
        self.push(!black, 1);
    }
    fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
        self.push(!black, (chunk_count as usize).saturating_mul(8));
    }
    fn next_line(&mut self) {
        self.next();
    }
}
// Parameter mapping follows hayro-syntax filter/ccitt.rs and filter/jbig2.rs,
// Copyright Laurenz Stampfl, MIT OR Apache-2.0. APIs verified against released
// hayro-ccitt 0.4.0 and hayro-jbig2 0.3.1, not unreleased hayro main.
fn decode_ccitt(
    data: &[u8],
    parms: &Dictionary,
    width: u32,
    height: u32,
    inverted: bool,
) -> Result<GrayImage> {
    let columns = u32::try_from(integer(parms, b"Columns", 1728)).map_err(invalid)?;
    if columns != width {
        return Err(invalid("CCITT Columns"));
    }
    pixels(columns, height, BILEVEL_MAX_PIXELS)?;
    let k = integer(parms, b"K", 0);
    let boolean = |key, default| parms.get(key).and_then(Object::as_bool).unwrap_or(default);
    let settings = hayro_ccitt::DecodeSettings {
        columns,
        rows: height,
        end_of_block: boolean(b"EndOfBlock", true),
        end_of_line: boolean(b"EndOfLine", false),
        rows_are_byte_aligned: boolean(b"EncodedByteAlign", false),
        encoding: if k < 0 {
            hayro_ccitt::EncodingMode::Group4
        } else if k == 0 {
            hayro_ccitt::EncodingMode::Group3_1D
        } else {
            hayro_ccitt::EncodingMode::Group3_2D {
                k: u32::try_from(k).map_err(invalid)?,
            }
        },
        invert_black: boolean(b"BlackIs1", false),
    };
    let mut sink = GraySink::new(width, height, inverted)?;
    codec_call();
    hayro_ccitt::decode(
        data,
        &mut sink,
        &mut hayro_ccitt::DecoderContext::new(settings),
    )
    .map_err(invalid)?;
    if sink.row != height as usize {
        return Err(invalid("incomplete CCITT image"));
    }
    GrayImage::from_raw(width, height, sink.data).ok_or_else(|| invalid("CCITT pixels"))
}
fn decode_jbig2(data: &[u8], globals: Option<&[u8]>, inverted: bool) -> Result<GrayImage> {
    let header = jbig2_prescan(data, globals).map_err(|_| cap())?;
    codec_call();
    let image = hayro_jbig2::Image::new_embedded(data, globals).map_err(invalid)?;
    if (image.width(), image.height()) != (header.width, header.height) {
        return Err(invalid("JBIG2 dimensions"));
    }
    let mut sink = GraySink::new(header.width, header.height, inverted)?;
    image.decode(&mut sink).map_err(invalid)?;
    GrayImage::from_raw(header.width, header.height, sink.data)
        .ok_or_else(|| invalid("JBIG2 pixels"))
}
#[derive(Debug)]
struct JpxSiz {
    width: u32,
    height: u32,
    tiles: u64,
    components: u16,
    palette: bool,
}
fn jpx_box<'a>(data: &'a [u8], pos: &mut usize) -> Result<(&'a [u8], &'a [u8])> {
    let start = *pos;
    let head = take(data, pos, 8)?;
    let size = u32::from_be_bytes(head[..4].try_into().map_err(invalid)?);
    let size = if size == 1 {
        u64::from_be_bytes(take(data, pos, 8)?.try_into().map_err(invalid)?)
    } else if size == 0 {
        (data.len() - start) as u64
    } else {
        u64::from(size)
    };
    let size = usize::try_from(size).map_err(invalid)?;
    let end = start
        .checked_add(size)
        .filter(|end| *end >= *pos && *end <= data.len())
        .ok_or_else(|| invalid("JP2 box"))?;
    let body = &data[*pos..end];
    *pos = end;
    Ok((&head[4..], body))
}
fn jpx_siz(data: &[u8]) -> Result<JpxSiz> {
    let mut codestream = data;
    let mut palette = false;
    if !data.starts_with(&[255, 79, 255, 81]) {
        let mut pos = 0;
        let mut found = None;
        while pos < data.len() {
            let (kind, body) = jpx_box(data, &mut pos)?;
            if kind == b"jp2c" {
                // hayro-jpeg2000 0.4.1 parses every jp2c and keeps the last.
                // Reject ambiguity before any codestream reaches the codec.
                if found.is_some() {
                    return Err(invalid("multiple JP2 codestreams"));
                }
                found = Some(body);
            } else if kind == b"jp2h" {
                let mut child_pos = 0;
                while child_pos < body.len() {
                    let (kind, _) = jpx_box(body, &mut child_pos)?;
                    palette |= kind == b"pclr";
                }
            }
        }
        codestream = found.ok_or_else(|| invalid("missing JP2 codestream"))?;
    }
    if !codestream.starts_with(&[255, 79, 255, 81]) || codestream.len() < 42 {
        return Err(invalid("JPX SIZ"));
    }
    let length = u16::from_be_bytes([codestream[4], codestream[5]]) as usize;
    let c = u16::from_be_bytes([codestream[40], codestream[41]]);
    if c == 0 || c > 4 || length < 38 + 3 * c as usize || codestream.len() < 4 + length {
        return Err(cap());
    }
    let xs = be32(codestream, 8)?;
    let ys = be32(codestream, 12)?;
    let xo = be32(codestream, 16)?;
    let yo = be32(codestream, 20)?;
    let width = xs.checked_sub(xo).ok_or_else(cap)?;
    let height = ys.checked_sub(yo).ok_or_else(cap)?;
    pixels(width, height, JPX_MAX_PIXELS)?;
    let xt = be32(codestream, 24)?;
    let yt = be32(codestream, 28)?;
    let xto = be32(codestream, 32)?;
    let yto = be32(codestream, 36)?;
    if xt == 0
        || yt == 0
        || xto > xo
        || yto > yo
        || u64::from(xto) + u64::from(xt) <= u64::from(xo)
        || u64::from(yto) + u64::from(yt) <= u64::from(yo)
    {
        return Err(cap());
    }
    let tiles = u64::from(xs.checked_sub(xto).ok_or_else(cap)?).div_ceil(u64::from(xt))
        * u64::from(ys.checked_sub(yto).ok_or_else(cap)?).div_ceil(u64::from(yt));
    if tiles == 0 || tiles > JPX_MAX_TILES {
        return Err(cap());
    }
    Ok(JpxSiz {
        width,
        height,
        tiles,
        components: c,
        palette,
    })
}
fn decode_jpx(data: &[u8], want: Option<(u32, u32)>) -> Result<DynamicImage> {
    decode_jpx_with_side(data, want, IMAGE_MAX_SIDE)
}
// Parameter mapping follows hayro-syntax filter/jpx.rs (MIT OR Apache-2.0,
// Copyright Laurenz Stampfl). Verified on hayro-jpeg2000 0.4.1.
fn decode_jpx_with_side(data: &[u8], want: Option<(u32, u32)>, side: u32) -> Result<DynamicImage> {
    let siz = jpx_siz(data)?;
    debug_assert!(siz.tiles <= JPX_MAX_TILES);
    let reduced = |n: u32, k: u32| u64::from(n).div_ceil(1u64 << k) as u32;
    let mut k = 0;
    while u64::from(reduced(siz.width, k))
        * u64::from(reduced(siz.height, k))
        * u64::from(siz.components)
        * JPX_SAMPLE_BYTES
        > IMAGE_DECODE_ALLOC
    {
        k += 1;
    }
    let (vw, vh) = want.unwrap_or((siz.width, siz.height));
    let mut quality = 0;
    while quality < 31 && reduced(vw.max(vh), quality + 1) >= side {
        quality += 1;
    }
    k = k.max(quality);
    // hayro 0.4.1 selects min(ilog2(W/target_w), ilog2(H/target_h)).
    // Floor targets select k exactly when both axes can be reduced that far.
    // Palette indices must be resolved at full resolution (jp2/mod.rs).
    k = if siz.palette {
        0
    } else {
        k.min(siz.width.ilog2()).min(siz.height.ilog2())
    };
    let checked_width = reduced(siz.width, k);
    let checked_height = reduced(siz.height, k);
    let settings = hayro_jpeg2000::DecodeSettings {
        resolve_palette_indices: true,
        strict: false,
        target_resolution: (k > 0).then_some(((siz.width >> k).max(1), (siz.height >> k).max(1))),
    };
    codec_call();
    let image = hayro_jpeg2000::Image::new(data, &settings).map_err(invalid)?;
    let channels = image.color_space().num_channels();
    if matches!(
        image.color_space(),
        hayro_jpeg2000::ColorSpace::Unknown { .. }
    ) || !matches!(channels, 1 | 3 | 4)
    {
        return Err(invalid("JPX color space"));
    }
    let stride = channels + u8::from(image.has_alpha());
    let palette_expansion = siz.palette && siz.components == 1 && matches!(stride, 3 | 4);
    // If the codestream has too few decomposition levels, hayro clamps its
    // reduction. Retain the exact SIZ check before any sample decode.
    if image.width() != checked_width
        || image.height() != checked_height
        || (u16::from(stride) != siz.components && !palette_expansion)
    {
        return Err(invalid("JPX SIZ mismatch"));
    }
    if u64::from(image.width())
        .saturating_mul(u64::from(image.height()))
        .saturating_mul(u64::from(stride))
        .saturating_mul(JPX_SAMPLE_BYTES)
        > IMAGE_DECODE_ALLOC
    {
        return Err(cap());
    }
    #[cfg(test)]
    JPX_DECODE_CALLS.with(|n| n.set(n.get() + 1));
    let data = image
        .decode(&mut hayro_jpeg2000::DecoderContext::default())
        .map_err(invalid)?
        .data_u8();
    let samples: Vec<u8> = data
        .chunks_exact(stride as usize)
        .flat_map(|c| c[..channels as usize].iter().copied())
        .collect();
    let (w, h) = (image.width(), image.height());
    if channels == 1 {
        Ok(DynamicImage::ImageLuma8(
            GrayImage::from_raw(w, h, samples).ok_or_else(|| invalid("JPX samples"))?,
        ))
    } else {
        let samples = if channels == 4 {
            samples.as_chunks::<4>().0.iter().flat_map(cmyk).collect()
        } else {
            samples
        };
        Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(w, h, samples).ok_or_else(|| invalid("JPX samples"))?,
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calibration_preserves_the_delivered_ceiling() {
        assert_eq!(PAGE_PDF_OUTPUT_LIMIT, 262137);
        for p in [1, 4096, 100000] {
            assert!(page_pdf_limit(p) < PAGE_PDF_MIN_USABLE);
        }
        for p in [262137, 400000, 12000000] {
            assert!(page_pdf_limit(p) >= PAGE_PDF_MIN_USABLE);
            assert!(page_pdf_limit(p) <= p);
        }
    }
    fn bytes(mut doc: Document) -> Vec<u8> {
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }
    fn image_stream(filter: Option<&str>, w: u32, h: u32, bits: i64, data: Vec<u8>) -> Stream {
        let mut d = dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>i64::from(w),"Height"=>i64::from(h),"BitsPerComponent"=>bits,"ColorSpace"=>"DeviceGray"};
        if let Some(filter) = filter {
            d.set("Filter", filter);
        }
        Stream::new(d, data)
    }
    fn fixture(stream: Stream) -> (Document, ObjectId, ObjectId, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages = doc.new_object_id();
        let image = doc.add_object(stream);
        let content = doc.add_object(Stream::new(
            dictionary! {},
            b"q 64 0 0 8 0 0 cm /Im Do Q".to_vec(),
        ));
        let page = doc.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,"Contents"=>content});
        doc.objects.insert(pages,dictionary!{"Type"=>"Pages","Count"=>1,"Kids"=>vec![Object::Reference(page)],"MediaBox"=>vec![0.into(),0.into(),64.into(),8.into()],"Resources"=>dictionary!{"XObject"=>dictionary!{"Im"=>image}}}.into());
        let catalog = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
        doc.trailer.set("Root", catalog);
        (doc, page, image, content)
    }
    fn view(doc: Document) -> ViewImage {
        CODEC_CALLS.with(|n| n.set(0));
        match view_page(&bytes(doc), 1, None).unwrap() {
            PageView::Image(v) => {
                assert_eq!(CODEC_CALLS.with(|n| n.get()), 1);
                v
            }
            _ => panic!("Expected image, got PDF fallback"),
        }
    }
    fn probe(view: &ViewImage, inverted: bool) {
        let image = image::load_from_memory(&view.data).unwrap().to_luma8();
        let left = image.get_pixel(image.width() / 4, image.height() / 2)[0];
        let right = image.get_pixel(image.width() * 3 / 4, image.height() / 2)[0];
        if inverted {
            assert!(left > 220 && right < 35, "{left}, {right}");
        } else {
            assert!(left < 35 && right > 220, "{left}, {right}");
        }
    }
    fn g4() -> Vec<u8> {
        let mut encoder = fax::encoder::Encoder::new(fax::VecWriter::new());
        for _ in 0..8 {
            encoder
                .encode_line(
                    (0..64).map(|x| {
                        if x < 32 {
                            fax::Color::Black
                        } else {
                            fax::Color::White
                        }
                    }),
                    64,
                )
                .unwrap();
        }
        encoder.finish().unwrap().finish()
    }
    fn segment(number: u32, kind: u8, refs: &[u32], association: u8, data: &[u8]) -> Vec<u8> {
        let mut out = number.to_be_bytes().to_vec();
        out.push(kind);
        if refs.len() <= 4 {
            out.push((refs.len() as u8) << 5);
        } else {
            out.extend_from_slice(&(0xe0000000 | refs.len() as u32).to_be_bytes());
            out.resize(out.len() + (refs.len() + 1).div_ceil(8), 0);
        }
        for reference in refs {
            let bytes = reference.to_be_bytes();
            let width = if number <= 256 {
                1
            } else if number <= 65536 {
                2
            } else {
                4
            };
            out.extend_from_slice(&bytes[4 - width..]);
        }
        out.push(association);
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        out
    }
    fn page_info(w: u32, h: u32) -> Vec<u8> {
        let mut d = w.to_be_bytes().to_vec();
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[0; 11]);
        segment(2, 48, &[], 1, &d)
    }
    fn jbig2_generic() -> Vec<u8> {
        let mut data = page_info(64, 8);
        let mut region = 64u32.to_be_bytes().to_vec();
        region.extend_from_slice(&8u32.to_be_bytes());
        region.extend_from_slice(&[0; 9]);
        region.push(1);
        region.extend_from_slice(&g4());
        data.extend_from_slice(&segment(3, 39, &[], 1, &region));
        data.extend_from_slice(&segment(4, 49, &[], 1, &[]));
        data
    }
    fn bits(bits: &str) -> Vec<u8> {
        let b: Vec<_> = bits.bytes().filter(|b| matches!(b, b'0' | b'1')).collect();
        b.chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0, |a, (i, b)| a | ((b - b'0') << (7 - i)))
            })
            .collect()
    }
    #[test]
    fn jpeg_passthrough_raw_samples_indexed_and_rotation() {
        let image = DynamicImage::ImageLuma8(GrayImage::from_fn(64, 8, |x, _| {
            image::Luma([if x < 32 { 0 } else { 255 }])
        }));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100)
            .encode_image(&image)
            .unwrap();
        let (doc, _, _, _) = fixture(image_stream(Some("DCTDecode"), 64, 8, 8, jpeg.clone()));
        let v = view(doc);
        assert!(v.passthrough);
        assert_eq!(v.data, jpeg);
        probe(&v, false);
        let (mut doc, page, _, _) = fixture(image_stream(Some("DCTDecode"), 64, 8, 8, jpeg));
        let parent = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Parent")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_object_mut(parent)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Rotate", 90);
        let v = view(doc);
        assert_eq!((v.width, v.height), (8, 64));
        let mut stream = image_stream(
            None,
            64,
            8,
            8,
            (0..512)
                .map(|p| if p % 64 < 32 { 0 } else { 255 })
                .collect(),
        );
        stream.compress().unwrap();
        probe(&view(fixture(stream).0), false);
        let mut stream = image_stream(None, 64, 8, 1, [0xf0u8; 64].to_vec());
        stream.dict.set("Decode", vec![1.into(), 0.into()]);
        let v = view(fixture(stream).0);
        let p = image::load_from_memory(&v.data).unwrap().to_luma8();
        assert!(p.get_pixel(0, 0)[0] < 35 && p.get_pixel(4, 0)[0] > 220);
        for depth in [1, 2, 4, 8] {
            let mut stream = image_stream(None, 64, 8, depth, vec![0; 64 * 8 * depth as usize / 8]);
            let row = 64 * depth as usize / 8;
            for y in 0..8 {
                for x in 32..64 {
                    let bit = x * depth as usize;
                    stream.content[y * row + bit / 8] |= 1 << (8 - depth as usize - bit % 8);
                }
            }
            stream.dict.set(
                "ColorSpace",
                vec![
                    Object::Name(b"Indexed".to_vec()),
                    Object::Name(b"DeviceRGB".to_vec()),
                    1.into(),
                    Object::string_literal(vec![0, 0, 0, 255, 255, 255]),
                ],
            );
            probe(&view(fixture(stream).0), false);
        }
    }
    #[test]
    fn fax_modes_and_jbig2_generic_return_images() {
        let stream = |data, k, align| {
            let mut s = image_stream(Some("CCITTFaxDecode"), 64, 8, 1, data);
            s.dict.set(
                "DecodeParms",
                dictionary! {"K"=>k,"Columns"=>64,"EncodedByteAlign"=>align,"EndOfBlock"=>false},
            );
            s
        };
        probe(&view(fixture(stream(g4(), -1, false)).0), false);
        for black in [false, true] {
            let mut s = stream(g4(), -1, false);
            s.dict
                .get_mut(b"DecodeParms")
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("BlackIs1", black);
            probe(&view(fixture(s).0), black);
        }
        let mut s = stream(g4(), -1, false);
        s.dict.set("Decode", vec![1.into(), 0.into()]);
        probe(&view(fixture(s).0), true);
        // T.4 terminating codes: white(0)=00110101, black(32)=000001101010,
        // white(32)=00011011. Total 28 bits, padded to one byte boundary per row.
        let line = "00110101 000001101010 00011011 0000";
        probe(
            &view(fixture(stream(bits(&line.repeat(8)), 0, true)).0),
            false,
        );
        // G3 2D: EOL=000000000001, 1D tag=1 then the runs above;
        // next row tag=0, identical reference runs use three V(0)=1 bits.
        let first = "000000000001 1 00110101 000001101010 00011011";
        let second = "000000000001 0 111";
        let mixed = format!("{first}{second}").repeat(4);
        probe(&view(fixture(stream(bits(&mixed), 2, false)).0), false);
        probe(
            &view(fixture(image_stream(Some("JBIG2Decode"), 64, 8, 1, jbig2_generic())).0),
            false,
        );
    }
    /// Synthetic 64x8 PGM/PPM: left 32 columns zero, right 32 maximum.
    /// Generated once with opj_compress -i gray8.pgm -o gray8.jp2 -n 3
    /// (also rgb8.ppm/rgb8.jp2 and gray16.pgm/gray16.jp2; PGM16 is big endian).
    #[test]
    fn jpx_gray_rgb_16bit_and_resolution_reduction() {
        for (data, color) in [
            (
                include_bytes!("testdata/gray8.jp2").as_slice(),
                "DeviceGray",
            ),
            (include_bytes!("testdata/rgb8.jp2").as_slice(), "DeviceRGB"),
            (
                include_bytes!("testdata/gray16.jp2").as_slice(),
                "DeviceGray",
            ),
        ] {
            assert!(data.len() <= 8192);
            let mut s = image_stream(Some("JPXDecode"), 64, 8, 8, data.to_vec());
            s.dict.set("ColorSpace", color);
            let v = view(fixture(s).0);
            assert_eq!((v.width, v.height), (64, 8));
            probe(&v, false);
        }
        let reduced = decode_jpx_with_side(include_bytes!("testdata/gray8.jp2"), None, 16).unwrap();
        assert_eq!((reduced.width(), reduced.height()), (16, 2));
        let region =
            decode_jpx_with_side(include_bytes!("testdata/gray8.jp2"), Some((16, 8)), 16).unwrap();
        assert_eq!((region.width(), region.height()), (64, 8));
    }
    fn assert_fallback(doc: Document) -> Document {
        CODEC_CALLS.with(|n| n.set(0));
        let PageView::Pdf(out) = view_page(&bytes(doc), 1, None).unwrap() else {
            panic!("expected PDF fallback")
        };
        assert_eq!(CODEC_CALLS.with(|n| n.get()), 0);
        assert!(out.len() <= PAGE_PDF_OUTPUT_LIMIT);
        let doc = Document::load_mem(&out).unwrap();
        assert_eq!(doc.get_pages().len(), 1);
        assert!(doc.objects.values().all(|o| {
            !o.as_stream()
                .is_ok_and(|s| s.dict.has(b"LocalFilesPlaceholder"))
        }));
        doc
    }
    #[test]
    fn composite_pages_and_masks_are_never_partially_decoded() {
        for paint in [
            b"/Im Do /Im Do".as_slice(),
            b"/Im Do BT (Visible) Tj ET",
            b"/Im Do 0 0 1 1 re f",
            b"/Im Do BI /W 1 /H 1 /BPC 8 /CS /G ID x EI",
        ] {
            let (mut doc, _, _, content) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
            doc.get_object_mut(content)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .set_content(paint.to_vec());
            assert_fallback(doc);
        }
        for key in ["SMask", "Mask"] {
            let (mut doc, page, image, _) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
            let mask = doc.add_object(image_stream(Some("JBIG2Decode"), 64, 8, 1, jbig2_generic()));
            doc.get_object_mut(image)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .dict
                .set(key, mask);
            let extra = doc.add_object(dictionary!{"Type"=>"Page","Parent"=>doc.get_dictionary(page).unwrap().get(b"Parent").unwrap().clone()});
            let pages = doc
                .get_dictionary(page)
                .unwrap()
                .get(b"Parent")
                .unwrap()
                .as_reference()
                .unwrap();
            let parent = doc.get_object_mut(pages).unwrap().as_dict_mut().unwrap();
            parent.set("Count", 2);
            parent.set(
                "Kids",
                vec![Object::Reference(page), Object::Reference(extra)],
            );
            let out = assert_fallback(doc);
            assert!(
                out.get_object(mask)
                    .unwrap()
                    .as_stream()
                    .unwrap()
                    .content
                    .len()
                    > 1
            );
        }
        for mode in [3, 7] {
            let (mut doc, _, _, content) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
            doc.get_object_mut(content)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .set_content(format!("/Im Do BT {mode} Tr (OCR text allowed) Tj ET").into_bytes());
            view(doc);
        }
    }
    #[test]
    fn header_caps_and_corrupt_streams_route_to_pdf_before_codec_allocation() {
        let mut samples = vec![
            ("JBIG2Decode", 100000, 100000, page_info(100000, 100000)),
            ("JBIG2Decode", 64, 8, page_info(64, u32::MAX)),
        ];
        let mut region = page_info(64, 8);
        let mut data = 70000u32.to_be_bytes().repeat(2);
        data.extend_from_slice(&[0; 10]);
        region.extend_from_slice(&segment(3, 39, &[], 1, &data));
        samples.push(("JBIG2Decode", 64, 8, region));
        let mut refs = page_info(64, 8);
        refs.extend_from_slice(&[0, 0, 0, 3, 39, 0xf0, 0, 0, 0]);
        samples.push(("JBIG2Decode", 64, 8, refs));
        let mut unknown = page_info(64, 8);
        unknown.extend_from_slice(&[0, 0, 0, 3, 39, 0, 1, 255, 255, 255, 255]);
        samples.push(("JBIG2Decode", 64, 8, unknown));
        samples.push(("JBIG2Decode", 64, 8, vec![0, 0, 0]));
        samples.push(("JPXDecode", 64, 8, vec![255, 79, 255, 81, 0, 41]));
        for (filter, w, h, data) in samples {
            assert_fallback(fixture(image_stream(Some(filter), w, h, 1, data)).0);
        }
        let mut s = image_stream(Some("CCITTFaxDecode"), 100000, 100000, 1, vec![0]);
        s.dict
            .set("DecodeParms", dictionary! {"Columns"=>100000,"K"=>-1});
        assert_fallback(fixture(s).0);
        // SIZ with 10^10 pixels, and SIZ with 10^6 one-pixel tiles.
        for (w, h, tile) in [(100000u32, 100000u32, 100000u32), (1000, 1000, 1)] {
            let mut siz = vec![255, 79, 255, 81, 0, 41, 0, 0];
            for n in [w, h, 0, 0, tile, tile, 0, 0] {
                siz.extend_from_slice(&n.to_be_bytes());
            }
            siz.extend_from_slice(&[0, 1, 7, 1, 1]);
            assert_fallback(fixture(image_stream(Some("JPXDecode"), w, h, 8, siz)).0);
        }
        for filter in ["CCITTFaxDecode", "JBIG2Decode", "JPXDecode"] {
            let mut s = image_stream(Some(filter), 64, 8, 1, vec![0; 4]);
            s.dict
                .set("DecodeParms", dictionary! {"Columns"=>64,"K"=>-1});
            let PageView::Pdf(_) = view_page(&bytes(fixture(s).0), 1, None).unwrap() else {
                panic!("decode failure must fall back")
            };
        }
    }
    #[test]
    fn page_pdf_limit_and_out_of_range_are_explicit() {
        let (doc, _, _, _) = fixture(image_stream(None, 64, 8, 8, vec![0; 512]));
        assert_eq!(
            view_page(&bytes(doc.clone()), 2, None).unwrap_err().code,
            "PAGE_OUT_OF_RANGE"
        );
        let mut giant = image_stream(None, 64, 8, 8, vec![0; PAGE_PDF_OUTPUT_LIMIT]);
        giant.dict.set("Filter", "Unsupported");
        assert_eq!(
            single_page_pdf(&bytes(fixture(giant).0), 1)
                .unwrap_err()
                .code,
            "OUTPUT_TOO_LARGE"
        );
        let out = single_page_pdf(&bytes(doc), 1).unwrap();
        assert_eq!(Document::load_mem(&out).unwrap().get_pages().len(), 1);
    }

    // T.88 standard tables: B.4 DH=8:1110100; B.2 DW=32:11110010101,
    // OOB=111111; B.1 bitmap-size=0:00000, export runs 0:00000 and 1:00001.
    // The uncompressed collective bitmap is a 32x8 black rectangle.
    fn jbig2_with_globals() -> (Vec<u8>, Vec<u8>) {
        let mut dictionary = 1u16.to_be_bytes().to_vec(); // SDHUFF=1; standard tables.
        dictionary.extend_from_slice(&1u32.to_be_bytes());
        dictionary.extend_from_slice(&1u32.to_be_bytes());
        dictionary.extend_from_slice(&bits("1110100 11110010101 111111 00000"));
        dictionary.extend_from_slice(&[255; 32]);
        dictionary.extend_from_slice(&bits("00000 00001"));
        let globals = segment(1, 0, &[], 0, &dictionary);
        let mut text = 64u32.to_be_bytes().to_vec();
        text.extend_from_slice(&8u32.to_be_bytes());
        text.extend_from_slice(&[0; 9]);
        text.extend_from_slice(&0x11u16.to_be_bytes()); // Huffman, top-left reference, strip size 1.
        text.extend_from_slice(&0u16.to_be_bytes()); // Default B.6/B.8/B.11 tables.
        text.extend_from_slice(&1u32.to_be_bytes());
        // 35 run-code prefix lengths: only run-code 1 has length 1. Its code 0
        // gives symbol 0 a one-bit code, then the table aligns to a byte.
        text.extend_from_slice(&bits(&format!("0000 0001 {} 0", "0000".repeat(33))));
        // B.11 initial DT=1 and strip DT=1:0,0; B.6 FS=0:000000000;
        // symbol ID=0:0; B.8 DS=OOB:01, finishing the only strip.
        text.extend_from_slice(&bits("0 0 000000000 0 01"));
        let mut data = page_info(64, 8);
        data.extend_from_slice(&segment(3, 7, &[1], 1, &text));
        data.extend_from_slice(&segment(4, 49, &[], 1, &[]));
        (data, globals)
    }
    #[test]
    fn jbig2_symbol_dictionary_globals_are_decoded_and_required() {
        let (data, globals) = jbig2_with_globals();
        let parsed = hayro_jbig2::Image::new_embedded(&data, Some(&globals)).unwrap();
        let mut sink = GraySink::new(64, 8, false).unwrap();
        parsed.decode(&mut sink).unwrap();
        let (mut doc, _, id, _) =
            fixture(image_stream(Some("JBIG2Decode"), 64, 8, 1, data.clone()));
        let globals_id = doc.add_object(Stream::new(dictionary! {}, globals));
        doc.get_object_mut(id)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .dict
            .set("DecodeParms", dictionary! {"JBIG2Globals"=>globals_id});
        probe(&view(doc), false);
        let PageView::Pdf(_) = view_page(
            &bytes(fixture(image_stream(Some("JBIG2Decode"), 64, 8, 1, data)).0),
            1,
            None,
        )
        .unwrap() else {
            panic!("missing globals must fall back")
        };
    }
    #[test]
    fn painted_forms_share_limits_and_fallback_is_a_sanitized_single_page() {
        for depth in 1..=4 {
            let (mut doc, page, image, content) =
                fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
            let mut target = image;
            for _ in 0..depth {
                target = doc.add_object(Stream::new(dictionary!{"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),64.into(),8.into()],"Resources"=>dictionary!{"XObject"=>dictionary!{"Inner"=>target}}},b"/Inner Do".to_vec()));
            }
            doc.get_object_mut(page)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set(
                    "Resources",
                    dictionary! {"XObject"=>dictionary!{"Form"=>target}},
                );
            doc.get_object_mut(content)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .set_content(b"/Form Do".to_vec());
            if depth <= 3 {
                view(doc);
            } else {
                assert_fallback(doc);
            }
        }
        for paint in [
            ("q ".repeat(65) + "/Im Do" + &" Q".repeat(65)).into_bytes(),
            b"0 0 m\n".repeat(crate::pdf::PDF_CONTENT_LIMIT / 6 + 1),
        ] {
            let (mut doc, _, _, content) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
            let stream = doc
                .get_object_mut(content)
                .unwrap()
                .as_stream_mut()
                .unwrap();
            stream.set_content(paint);
            stream.compress().unwrap();
            assert_fallback(doc);
        }
        for (w, h) in [(8, 8), (64, 4)] {
            let (mut doc, page, _, content) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
            let stamp = doc.add_object(image_stream(None, w, h, 8, vec![0; (w * h) as usize]));
            let pages = doc
                .get_dictionary(page)
                .unwrap()
                .get(b"Parent")
                .unwrap()
                .as_reference()
                .unwrap();
            doc.get_object_mut(pages)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .get_mut(b"Resources")
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .get_mut(b"XObject")
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("Stamp", stamp);
            doc.get_object_mut(content)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .set_content(b"/Im Do /Stamp Do".to_vec());
            assert_fallback(doc);
        }
        let (mut doc, page, _, _) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
        let secret = doc.add_object(Object::string_literal("synthetic-unselected-metadata"));
        doc.trailer.set("Info", secret);
        let catalog = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
        doc.get_object_mut(catalog)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("OpenAction", secret);
        let page_dict = doc.get_object_mut(page).unwrap().as_dict_mut().unwrap();
        for key in ["Annots", "B", "Thumb"] {
            page_dict.set(key, secret);
        }
        let out = single_page_pdf(&bytes(doc), 1).unwrap();
        let doc = Document::load_mem(&out).unwrap();
        assert!(
            !out.windows(b"synthetic-unselected-metadata".len())
                .any(|s| s == b"synthetic-unselected-metadata")
        );
        assert!(!doc.trailer.has(b"Info"));
        let catalog = doc
            .get_dictionary(doc.trailer.get(b"Root").unwrap().as_reference().unwrap())
            .unwrap();
        assert_eq!(catalog.len(), 2);
        let page = doc
            .get_dictionary(*doc.get_pages().values().next().unwrap())
            .unwrap();
        for key in [b"Annots".as_slice(), b"B", b"Thumb"] {
            assert!(!page.has(key));
        }
    }
    #[test]
    fn aggregate_jbig2_region_cap_precedes_any_codec_work() {
        let mut data = page_info(64, 8);
        for n in 3..7 {
            let mut region = 16384u32.to_be_bytes().repeat(2);
            region.extend_from_slice(&[0; 10]);
            data.extend_from_slice(&segment(n, 39, &[], 1, &region));
        }
        assert_fallback(fixture(image_stream(Some("JBIG2Decode"), 64, 8, 1, data)).0);
        let mut striped = page_info(64, u32::MAX);
        striped.extend_from_slice(&segment(3, 50, &[], 1, &7u32.to_be_bytes()));
        assert_fallback(fixture(image_stream(Some("JBIG2Decode"), 64, 8, 1, striped)).0);
        let mut masked = image_stream(None, 64, 8, 8, vec![0; 512]);
        masked.dict.set("SMaskInData", 1);
        assert_fallback(fixture(masked).0);
        let mut chain = image_stream(None, 64, 8, 8, vec![0; 512]);
        chain.dict.set(
            "Filter",
            vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"DCTDecode".to_vec()),
            ],
        );
        assert_fallback(fixture(chain).0);
    }
    #[test]
    fn raw_cmyk_stencil_runlength_and_unsupported_decode_mapping() {
        let mut cmyk = image_stream(
            None,
            64,
            8,
            8,
            (0..512)
                .flat_map(|p| {
                    if p % 64 < 32 {
                        [0, 0, 0, 255]
                    } else {
                        [0, 0, 0, 0]
                    }
                })
                .collect(),
        );
        cmyk.dict.set("ColorSpace", "DeviceCMYK");
        probe(&view(fixture(cmyk).0), false);
        let mut mask = image_stream(
            None,
            64,
            8,
            1,
            (0..64).map(|p| if p % 8 < 4 { 0 } else { 255 }).collect(),
        );
        mask.dict.remove(b"ColorSpace");
        mask.dict.set("ImageMask", true);
        probe(&view(fixture(mask).0), false);
        let mut runs = Vec::new();
        for _ in 0..8 {
            runs.extend_from_slice(&[225, 0, 225, 255]);
        }
        runs.push(128);
        probe(
            &view(fixture(image_stream(Some("RunLengthDecode"), 64, 8, 8, runs)).0),
            false,
        );
        let mut inverted = image_stream(Some("DCTDecode"), 64, 8, 8, vec![0]);
        inverted.dict.set("Decode", vec![1.into(), 0.into()]);
        assert_fallback(fixture(inverted).0);
        let mut unsupported = image_stream(None, 64, 8, 8, vec![255; 512]);
        unsupported.dict.set("Decode", vec![0.into(), 2.into()]);
        assert_fallback(fixture(unsupported).0);
    }
    fn jp2_box(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = ((data.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out
    }
    fn jp2_boxes(data: &[u8]) -> Vec<(&[u8], &[u8])> {
        let mut pos = 0;
        let mut boxes = Vec::new();
        while pos < data.len() {
            let len = be32(data, pos).unwrap() as usize;
            assert!(len >= 8);
            boxes.push((&data[pos + 4..pos + 8], &data[pos + 8..pos + len]));
            pos += len;
        }
        boxes
    }
    #[test]
    fn fix_r1_jpx_rejects_multiple_codestreams_before_codec() {
        let fixture = include_bytes!("testdata/gray8.jp2");
        let boxes = jp2_boxes(fixture);
        let raw = boxes.iter().find(|(kind, _)| *kind == b"jp2c").unwrap().1;
        for oversized_components in [false, true] {
            let mut last = raw.to_vec();
            if oversized_components {
                let length = u16::from_be_bytes([last[4], last[5]]) as usize;
                last.splice(4 + length..4 + length, [7, 1, 1].repeat(4));
                last[4..6].copy_from_slice(&((length + 12) as u16).to_be_bytes());
                last[40..42].copy_from_slice(&5u16.to_be_bytes());
            } else {
                // 50 million one-pixel tiles, kept tiny on disk. Never decode this.
                last[8..12].copy_from_slice(&100000u32.to_be_bytes());
                last[12..16].copy_from_slice(&500u32.to_be_bytes());
                last[24..28].copy_from_slice(&1u32.to_be_bytes());
                last[28..32].copy_from_slice(&1u32.to_be_bytes());
            }
            for extended in [false, true] {
                let mut data = fixture.to_vec();
                if extended {
                    data.extend_from_slice(&1u32.to_be_bytes());
                    data.extend_from_slice(b"jp2c");
                    data.extend_from_slice(&((last.len() + 16) as u64).to_be_bytes());
                    data.extend_from_slice(&last);
                } else {
                    data.extend_from_slice(&jp2_box(b"jp2c", &last));
                }
                // RED stops here on the vulnerable implementation, before the
                // malicious last codestream can reach Image::new or decode.
                assert!(
                    jpx_siz(&data).is_err(),
                    "duplicate jp2c passed the header guard"
                );
                JPX_DECODE_CALLS.with(|n| n.set(0));
                assert_fallback(fixture_document_for_jpx(data));
                assert_eq!(JPX_DECODE_CALLS.with(|n| n.get()), 0);
            }
        }
    }
    fn fixture_document_for_jpx(data: Vec<u8>) -> Document {
        fixture(image_stream(Some("JPXDecode"), 64, 8, 8, data)).0
    }
    #[test]
    fn fix_r1_jpx_rejects_reduced_dimension_mismatch_before_decode() {
        let data = include_bytes!("testdata/gray8.jp2");
        // Only two decomposition levels exist: hayro cannot satisfy the
        // requested 1x1 reduction and reports 16x2 instead of the checked SIZ.
        let parsed = hayro_jpeg2000::Image::new(
            data,
            &hayro_jpeg2000::DecodeSettings {
                target_resolution: Some((1, 1)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((parsed.width(), parsed.height()), (16, 2));
        JPX_DECODE_CALLS.with(|n| n.set(0));
        assert!(
            decode_jpx_with_side(data, None, 1).is_err(),
            "unchecked reduced dimensions were decoded"
        );
        assert_eq!(JPX_DECODE_CALLS.with(|n| n.get()), 0);
    }
    fn palette_jp2(base: &[u8], channels: u8, color: u8) -> Vec<u8> {
        let mut palette = 256u16.to_be_bytes().to_vec();
        palette.push(channels);
        palette.extend(std::iter::repeat_n(7, channels as usize));
        for n in 0..=255u8 {
            palette.extend(std::iter::repeat_n(n, channels as usize));
        }
        let mut data = Vec::new();
        for (kind, body) in jp2_boxes(base) {
            let body = if kind == b"jp2h" {
                let mut header = Vec::new();
                for (kind, body) in jp2_boxes(body) {
                    if kind == b"colr" {
                        header.extend_from_slice(&jp2_box(b"colr", &[1, 0, 0, 0, 0, 0, color]));
                    } else {
                        header.extend_from_slice(&jp2_box(kind.try_into().unwrap(), body));
                    }
                }
                header.extend_from_slice(&jp2_box(b"pclr", &palette));
                header
            } else {
                body.to_vec()
            };
            data.extend_from_slice(&jp2_box(kind.try_into().unwrap(), &body));
        }
        data
    }
    #[test]
    fn fix_r1_jpx_rejects_component_mismatch_before_decode() {
        // Only one-component palette expansion is supported; a three-component
        // codestream claiming a one-channel palette must still be rejected.
        let data = palette_jp2(include_bytes!("testdata/rgb8.jp2"), 1, 17);
        assert_eq!(jpx_siz(&data).unwrap().components, 3);
        let parsed = hayro_jpeg2000::Image::new(&data, &Default::default()).unwrap();
        assert_eq!(
            parsed.color_space().num_channels() + u8::from(parsed.has_alpha()),
            1
        );
        JPX_DECODE_CALLS.with(|n| n.set(0));
        assert!(
            decode_jpx(&data, None).is_err(),
            "unchecked component count was decoded"
        );
        assert_eq!(JPX_DECODE_CALLS.with(|n| n.get()), 0);
    }
    #[test]
    fn fix_r2_jpx_odd_scan_whole_page_is_an_image() {
        // Synthetic 4961x7017 half-black PGM, OpenJPEG 2.5 opj_compress -n 6.
        // Both axes are odd, so a ceil target used to select level zero.
        let data = include_bytes!("testdata/gray-odd.jp2");
        let decoded = decode_jpx(data, None).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2481, 3509));
        drop(decoded);
        let out = view(
            fixture(image_stream(
                Some("JPXDecode"),
                4961,
                7017,
                8,
                data.to_vec(),
            ))
            .0,
        );
        assert_eq!((out.source_width, out.source_height), (4961, 7017));
        assert_eq!((out.width, out.height), (1448, 2048));
        probe(&out, false);
    }
    #[test]
    fn fix_r2_jpx_palette_is_an_image() {
        for (channels, color, space, inverted) in
            [(3, 16, "DeviceRGB", false), (4, 12, "DeviceCMYK", true)]
        {
            let data = palette_jp2(include_bytes!("testdata/gray8.jp2"), channels, color);
            // Even when a smaller view is requested, palette indices must be
            // decoded at full SIZ resolution before expanding their colors.
            let decoded = decode_jpx_with_side(&data, None, 16).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (64, 8));
            let mut stream = image_stream(Some("JPXDecode"), 64, 8, 8, data);
            stream.dict.set("ColorSpace", space);
            let out = view(fixture(stream).0);
            assert_eq!((out.width, out.height), (64, 8));
            probe(&out, inverted);
        }
    }
    fn annotated_scan(state: bool) -> (Document, ObjectId, ObjectId, ObjectId, ObjectId) {
        let (mut doc, page, _, content) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
        let image = doc.add_object(image_stream(None, 2, 2, 8, vec![37; 4]));
        let appearance = doc.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),2.into(),2.into()],"Resources"=>dictionary!{"XObject"=>dictionary!{"StampImage"=>image}}}, b"2 0 0 2 0 0 cm /StampImage Do".to_vec()));
        let off = doc.add_object(Stream::new(
            dictionary! {},
            b"unused-off-appearance".to_vec(),
        ));
        let normal = if state {
            dictionary! {"On"=>appearance,"Off"=>off}.into()
        } else {
            Object::Reference(appearance)
        };
        let action = doc.add_object(dictionary!{"S"=>"JavaScript","JS"=>Object::string_literal("synthetic-annotation-action")});
        let annotation = doc.add_object(dictionary!{"Type"=>"Annot","Subtype"=>"Stamp","Rect"=>vec![10.into(),1.into(),12.into(),3.into()],"F"=>4,"AS"=>"On","Border"=>vec![0.into(),0.into(),1.into()],"BS"=>dictionary!{"W"=>1,"S"=>"S"},"C"=>vec![1.into(),0.into(),0.into()],"CA"=>0.5,"BM"=>"Multiply","AP"=>dictionary!{"N"=>normal,"R"=>off,"D"=>off},"A"=>action,"AA"=>dictionary!{"E"=>action},"Parent"=>action,"Popup"=>action,"P"=>page,"IRT"=>action,"OC"=>action,"Contents"=>Object::string_literal("synthetic-annotation-contents"),"T"=>Object::string_literal("author"),"FT"=>"Sig","V"=>action,"Kids"=>vec![Object::Reference(action)]});
        let annots = doc.add_object(vec![Object::Reference(annotation)]);
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Annots", annots);
        let root = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
        doc.get_object_mut(root)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "AcroForm",
                dictionary! {"Fields"=>vec![Object::Reference(annotation)]},
            );
        (doc, page, annotation, content, image)
    }
    #[test]
    fn fix_r4_visible_stamp_and_state_appearance_survive_fallback() {
        for state in [false, true] {
            let (doc, _, _, _, image) = annotated_scan(state);
            let out = assert_fallback(doc);
            let page = out
                .get_dictionary(*out.get_pages().values().next().unwrap())
                .unwrap();
            let annots = deref(&out, page.get(b"Annots").unwrap())
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(annots.len(), 1);
            let annot = deref(&out, &annots[0]).unwrap().as_dict().unwrap();
            for key in [
                b"A".as_slice(),
                b"AA",
                b"Popup",
                b"Parent",
                b"P",
                b"IRT",
                b"OC",
                b"Contents",
                b"T",
                b"FT",
                b"V",
                b"Kids",
            ] {
                assert!(!annot.has(key), "{key:?}");
            }
            for key in [
                b"Type".as_slice(),
                b"Subtype",
                b"Rect",
                b"F",
                b"AS",
                b"Border",
                b"BS",
                b"C",
                b"CA",
                b"BM",
            ] {
                assert!(annot.has(key));
            }
            assert_eq!(annot.len(), 11);
            let ap = annot.get(b"AP").unwrap().as_dict().unwrap();
            assert_eq!(ap.len(), 1);
            let mut normal = deref(&out, ap.get(b"N").unwrap()).unwrap();
            if state {
                let states = normal.as_dict().unwrap();
                assert_eq!(states.len(), 1);
                normal = deref(&out, states.get(b"On").unwrap()).unwrap();
            }
            assert!(
                normal
                    .as_stream()
                    .unwrap()
                    .content
                    .ends_with(b"/StampImage Do")
            );
            assert_eq!(
                out.get_object(image).unwrap().as_stream().unwrap().content,
                vec![37; 4]
            );
            let catalog = out
                .get_dictionary(out.trailer.get(b"Root").unwrap().as_reference().unwrap())
                .unwrap();
            assert!(!catalog.has(b"AcroForm"));
            assert_eq!(catalog.len(), 2);
            let serialized = bytes(out);
            for removed in [
                "synthetic-annotation-action",
                "synthetic-annotation-contents",
                "unused-off-appearance",
            ] {
                assert!(
                    !serialized
                        .windows(removed.len())
                        .any(|w| w == removed.as_bytes())
                );
            }
        }
    }
    #[test]
    fn fix_r4_invisible_or_appearance_less_annotations_do_not_block_image() {
        for kind in [
            "hidden",
            "noview",
            "popup",
            "link",
            "flat_rect",
            "missing_state",
        ] {
            let (mut doc, _, id, content, _) = annotated_scan(kind == "missing_state");
            let annot = doc.get_object_mut(id).unwrap().as_dict_mut().unwrap();
            match kind {
                "hidden" => annot.set("F", 2),
                "noview" => annot.set("F", 32),
                "popup" => annot.set("Subtype", "Popup"),
                "link" => {
                    annot.set("Subtype", "Link");
                    annot.remove(b"AP");
                }
                "flat_rect" => annot.set("Rect", vec![1.into(), 1.into(), 1.into(), 3.into()]),
                "missing_state" => annot.set("AS", "Missing"),
                _ => unreachable!(),
            }
            view(doc.clone());
            doc.get_object_mut(content)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .set_content(b"/Im Do /Im Do".to_vec());
            let out = assert_fallback(doc);
            assert!(
                !out.get_dictionary(*out.get_pages().values().next().unwrap())
                    .unwrap()
                    .has(b"Annots")
            );
        }
    }
    #[test]
    fn fix_r4_annotations_preserve_order_and_use_only_the_page_array() {
        let (mut doc, page, first, _, _) = annotated_scan(false);
        let mut other = doc.get_dictionary(first).unwrap().clone();
        other.set("Subtype", "Widget");
        other.set("Rect", vec![12.into(), 3.into(), 10.into(), 1.into()]); // Reversed coordinates still visible.
        let second = doc.add_object(other);
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "Annots",
                vec![Object::Reference(second), Object::Reference(first)],
            );
        let out = assert_fallback(doc.clone());
        let page_out = out
            .get_dictionary(*out.get_pages().values().next().unwrap())
            .unwrap();
        let annots = page_out.get(b"Annots").unwrap().as_array().unwrap();
        let subtypes: Vec<_> = annots
            .iter()
            .map(|a| {
                deref(&out, a)
                    .unwrap()
                    .as_dict()
                    .unwrap()
                    .get(b"Subtype")
                    .unwrap()
                    .as_name()
                    .unwrap()
            })
            .collect();
        assert_eq!(subtypes, [b"Widget".as_slice(), b"Stamp"]);
        let parent = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Parent")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_object_mut(parent)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Annots", vec![Object::Reference(first)]);
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .remove(b"Annots");
        view(doc);
    }
    fn scan_with_content(content: &[u8]) -> Document {
        let (mut doc, _, _, id) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
        doc.get_object_mut(id)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_content(content.to_vec());
        doc
    }
    #[test]
    fn fix_r4_clips_that_contain_the_scan_or_are_restored_allow_images() {
        for content in [
            "0 0 64 8 re W n 64 0 0 8 0 0 cm /Im Do",
            "0 0 64 8 re W* n 64 0 0 8 0 0 cm /Im Do",
            "q 0 0 32 8 re W n Q 64 0 0 8 0 0 cm /Im Do",
            "0 1 -1 0 64 0 cm 0 0 8 64 re W n 8 0 0 64 0 0 cm /Im Do",
            "-1 0 0 1 64 0 cm 0 0 64 8 re W n 64 0 0 8 0 0 cm /Im Do",
            "0.5 0 63 8 re W n 64 0 0 8 0 0 cm /Im Do",
            // The clip uses the CTM at re, not the later CTM at n.
            "0 0 64 8 re 64 0 0 8 0 0 cm W n /Im Do",
            "-100 -100 300 300 re W n 0.8660254 0.5 -0.5 0.8660254 0 0 cm 64 0 0 8 0 0 cm /Im Do",
        ] {
            let out = view(scan_with_content(content.as_bytes()));
            assert_eq!((out.width, out.height), (64, 8));
        }
    }
    #[test]
    fn fix_r4_cutting_or_inadmissible_clips_fall_back_before_decode() {
        for content in [
            "0 0 32 8 re W n 64 0 0 8 0 0 cm /Im Do",
            "0 0 m 64 0 l 0 8 l h W n 64 0 0 8 0 0 cm /Im Do",
            "0 0 64 8 re 0 0 64 8 re W n 64 0 0 8 0 0 cm /Im Do",
            "0.8660254 0.5 -0.5 0.8660254 0 0 cm 0 0 64 8 re W n 64 0 0 8 0 0 cm /Im Do",
            "W n 64 0 0 8 0 0 cm /Im Do",
            "0 0 64 re W n 64 0 0 8 0 0 cm /Im Do",
            "0 0 64 8 re W n 100 0 64 8 re W n 64 0 0 8 0 0 cm /Im Do",
            "0.51 0 63 8 re W n 64 0 0 8 0 0 cm /Im Do",
            "1 0 0 1 0 cm 64 0 0 8 0 0 cm /Im Do",
            "1 0 0 1 /bad 0 cm 64 0 0 8 0 0 cm /Im Do",
        ] {
            let mut doc = scan_with_content(content.as_bytes());
            let page = *doc.get_pages().values().next().unwrap();
            doc.get_object_mut(page)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("CropBox", vec![1.into(), 2.into(), 63.into(), 7.into()]);
            // Clipping must not affect the text walk or its result.
            let pdf = PdfDocument::load(&bytes(doc.clone()), &HashSet::new()).unwrap();
            assert!(!pdf.page_text(1).unwrap().content_too_large);
            let out = assert_fallback(doc);
            let page = out
                .get_dictionary(*out.get_pages().values().next().unwrap())
                .unwrap();
            assert_eq!(
                page.get(b"MediaBox").unwrap().as_array().unwrap(),
                &vec![0.into(), 0.into(), 64.into(), 8.into()]
            );
            assert_eq!(
                page.get(b"CropBox").unwrap().as_array().unwrap(),
                &vec![1.into(), 2.into(), 63.into(), 7.into()]
            );
        }
    }
    fn scan_in_form(content: &[u8], bbox: Vec<Object>, matrix: Option<Object>) -> Document {
        let (mut doc, page, image, id) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
        let mut dict = dictionary! {"Type"=>"XObject","Subtype"=>"Form","BBox"=>bbox,"Resources"=>dictionary!{"XObject"=>dictionary!{"Im"=>image}}};
        if let Some(matrix) = matrix {
            dict.set("Matrix", matrix);
        }
        let form = doc.add_object(Stream::new(dict, content.to_vec()));
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "Resources",
                dictionary! {"XObject"=>dictionary!{"Form"=>form}},
            );
        doc.get_object_mut(id)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_content(b"/Form Do".to_vec());
        doc
    }
    #[test]
    fn fix_r4_form_bbox_matrix_and_inner_clip_control_eligibility() {
        let full = || vec![0.into(), 0.into(), 64.into(), 8.into()];
        for (content, bbox, matrix) in [
            (
                b"0 0 32 8 re W n 64 0 0 8 0 0 cm /Im Do".as_slice(),
                full(),
                None,
            ),
            (
                b"64 0 0 8 0 0 cm /Im Do",
                vec![0.into(), 0.into(), 32.into(), 8.into()],
                None,
            ),
            (
                b"64 0 0 8 0 0 cm /Im Do",
                full(),
                Some(vec![1.into(), 0.into()].into()),
            ),
            (
                b"64 0 0 8 0 0 cm /Im Do",
                vec![0.into(), 0.into(), 64.into()],
                None,
            ),
            (
                b"64 0 0 8 0 0 cm /Im Do",
                full(),
                Some(
                    vec![
                        0.8660254.into(),
                        0.5.into(),
                        (-0.5).into(),
                        0.8660254.into(),
                        0.into(),
                        0.into(),
                    ]
                    .into(),
                ),
            ),
        ] {
            assert_fallback(scan_in_form(content, bbox, matrix));
        }
        let doc = scan_in_form(
            b"64 0 0 8 0 0 cm /Im Do",
            full(),
            Some(
                vec![
                    0.into(),
                    1.into(),
                    (-1).into(),
                    0.into(),
                    64.into(),
                    0.into(),
                ]
                .into(),
            ),
        );
        view(doc);
    }
    #[test]
    fn fix_r4_annotation_dereference_boundary_and_finite_rectangles() {
        fn chain(doc: &mut Document, mut value: Object, hops: usize) -> Object {
            for _ in 0..hops {
                value = Object::Reference(doc.add_object(value));
            }
            value
        }
        for hops in [128, 129] {
            let (mut doc, page, annotation, _, _) = annotated_scan(false);
            let value = doc.get_object(annotation).unwrap().clone();
            let annotation = chain(&mut doc, value, hops);
            let annots = chain(&mut doc, vec![annotation].into(), hops);
            doc.get_object_mut(page)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("Annots", annots);
            assert_eq!(
                visible_annotations(&doc, page).len(),
                usize::from(hops == 128)
            );
        }
        for rect in [
            vec![0.into(), 0.into(), f32::NAN.into(), 1.into()],
            vec![0.into(), 0.into(), f32::INFINITY.into(), 1.into()],
            vec![0.into(), 0.into(), 1.into(), 0.into()],
            vec![0.into(), 0.into(), Object::string_literal("1"), 1.into()],
        ] {
            let (mut doc, page, annotation, _, _) = annotated_scan(false);
            doc.get_object_mut(annotation)
                .unwrap()
                .as_dict_mut()
                .unwrap()
                .set("Rect", rect);
            assert!(visible_annotations(&doc, page).is_empty());
        }
        let (mut doc, page, annotation, _, _) = annotated_scan(false);
        doc.get_object_mut(annotation)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .remove(b"F");
        assert_eq!(visible_annotations(&doc, page).len(), 1); // Absent F = 0.
        doc.get_object_mut(annotation)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("F", 1);
        assert_eq!(visible_annotations(&doc, page).len(), 1); // Only Hidden/NoView exclude it.
    }
    #[test]
    fn fix_r4_form_ctm_composition_and_clip_state_do_not_leak() {
        let mut doc = scan_in_form(
            b"64 0 0 8 0 0 cm /Im Do",
            vec![0.into(), 0.into(), 64.into(), 8.into()],
            Some(vec![1.into(), 0.into(), 0.into(), 1.into(), 5.into(), 7.into()].into()),
        );
        let page = *doc.get_pages().values().next().unwrap();
        let content = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_object_mut(content)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_content(b"20 41 128 24 re W n 2 0 0 3 10 20 cm /Form Do".to_vec());
        view(doc);
        let (mut doc, page, image, content) = fixture(image_stream(None, 64, 8, 8, vec![255; 512]));
        let form = doc.add_object(Stream::new(dictionary!{"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),1.into(),1.into()]},b"0 0 0.25 1 re W n 10 0 0 10 0 0 cm".to_vec()));
        doc.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "Resources",
                dictionary! {"XObject"=>dictionary!{"Form"=>form,"Im"=>image}},
            );
        doc.get_object_mut(content)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_content(b"/Form Do 64 0 0 8 0 0 cm /Im Do".to_vec());
        view(doc);
    }
    #[test]
    fn fix_r4_nonfinite_ctm_falls_back_and_text_walk_is_unchanged() {
        let cm = "10000000000000000000000000000000000000.0 0 0 10000000000000000000000000000000000000.0 0 0 cm ";
        let content = format!(
            "BT /F1 12 Tf 3 Tr (OCR text) Tj ET {} /Im Do",
            cm.repeat(10)
        );
        let mut doc = scan_with_content(content.as_bytes());
        let page = *doc.get_pages().values().next().unwrap();
        doc.get_object_mut(page).unwrap().as_dict_mut().unwrap().set("Resources",dictionary!{"Font"=>dictionary!{"F1"=>dictionary!{"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica","Encoding"=>"WinAnsiEncoding"}}});
        let data = bytes(doc.clone());
        let pdf = PdfDocument::load(&data, &HashSet::new()).unwrap();
        let paint = pdf.page_paint(1).unwrap();
        assert!(paint.clipped);
        assert_eq!(paint.images.len(), 1);
        let text = pdf.page_text(1).unwrap();
        assert_eq!(text.text, "OCR text");
        assert_eq!(text.visible_chars, 0);
        assert!(text.invisible_chars > 0);
        assert!(!text.content_too_large);
        assert_fallback(doc);
    }
    fn assert_jbig2_rejected(data: &[u8], globals: Option<&[u8]>) {
        // RED stops at the prescan assertion, before any dangerous decode.
        assert!(
            jbig2_prescan(data, globals).is_err(),
            "JBIG2 header guard accepted the fixture"
        );
        CODEC_CALLS.with(|n| n.set(0));
        assert!(decode_jbig2(data, globals, false).is_err());
        assert_eq!(CODEC_CALLS.with(|n| n.get()), 0);
    }
    fn assert_jbig2_fallback(data: Vec<u8>, globals: Option<Vec<u8>>) {
        assert_jbig2_rejected(&data, globals.as_deref());
        let (mut doc, _, id, _) = fixture(image_stream(Some("JBIG2Decode"), 64, 8, 1, data));
        if let Some(globals) = globals {
            let mut globals = Stream::new(dictionary! {}, globals);
            globals.compress().unwrap();
            let globals_id = doc.add_object(globals);
            doc.get_object_mut(id)
                .unwrap()
                .as_stream_mut()
                .unwrap()
                .dict
                .set("DecodeParms", dictionary! {"JBIG2Globals"=>globals_id});
        }
        assert_fallback(doc);
    }
    fn symbol_header(exported: u32) -> Vec<u8> {
        let mut payload = 1u16.to_be_bytes().to_vec(); // Huffman, no AT fields.
        payload.extend_from_slice(&exported.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes()); // SDNUMNEWSYMS.
        payload
    }
    fn small_region() -> Vec<u8> {
        let mut payload = 1u32.to_be_bytes().repeat(2);
        payload.extend_from_slice(&[0; 30]);
        payload
    }
    #[test]
    fn fix_r3_jbig2_segment_type_allowlist_before_codec() {
        let allowed = [0, 6, 7, 38, 39, 42, 43, 48, 49, 50, 51, 52, 62];
        // Includes all formerly admitted intermediate/halftone types, tiny
        // type-53 tables, pattern dictionaries, palettes and reserved values.
        for kind in [16, 53, 36, 63]
            .into_iter()
            .chain(0..64)
            .filter(|kind| !allowed.contains(kind))
        {
            let payload = small_region();
            let mut data = page_info(64, 8);
            data.extend_from_slice(&segment(3, kind, &[], 1, &payload));
            assert_jbig2_fallback(data, None);
            let globals = segment(1, kind, &[], 0, &payload);
            assert_jbig2_fallback(jbig2_generic(), Some(globals));
        }
        for kind in allowed {
            let mut data = page_info(64, 8);
            if kind != 48 {
                let payload = match kind {
                    0 => symbol_header(0),
                    6 | 7 | 38 | 39 | 42 | 43 => small_region(),
                    _ => Vec::new(),
                };
                data.extend_from_slice(&segment(3, kind, &[], 1, &payload));
            }
            jbig2_prescan(&data, None).unwrap();
        }
    }
    #[test]
    fn fix_r3_jbig2_duplicate_references_before_codec() {
        for number in [3, 257, 65537] {
            let mut data = page_info(64, 8);
            data.extend_from_slice(&segment(number, 7, &[1, 1], 1, &small_region()));
            assert_jbig2_fallback(data, None);
        }
        // Long-form referred count and its retention bytes.
        let globals = segment(100, 0, &[1, 2, 3, 4, 1], 0, &symbol_header(0));
        assert_jbig2_fallback(jbig2_generic(), Some(globals));
    }
    #[test]
    fn fix_r3_jbig2_segment_count_is_shared_with_globals() {
        assert_eq!(JBIG2_MAX_SEGMENTS, 65536);
        let data = page_info(64, 8); // One segment in the page stream.
        let empty = segment(0, 52, &[], 0, &[]);
        let mut globals = empty.repeat(JBIG2_MAX_SEGMENTS as usize - 1);
        jbig2_prescan(&data, Some(&globals)).unwrap();
        globals.extend_from_slice(&empty);
        // Each stream is individually within the cap, but their sum is not.
        assert_jbig2_fallback(data, Some(globals));
    }
    #[test]
    fn fix_r3_jbig2_symbol_reference_total_is_shared_with_globals() {
        assert_eq!(JBIG2_MAX_SYMBOL_REFS, 1 << 20);
        let mut globals = segment(1, 0, &[], 0, &symbol_header(32768));
        globals.extend_from_slice(&segment(2, 0, &[], 0, &symbol_header(1)));
        // Type-0 consumers in globals charge half of the call's symbol refs.
        for number in 3..19 {
            globals.extend_from_slice(&segment(number, 0, &[1], 0, &symbol_header(0)));
        }
        let mut data = page_info(64, 8);
        // Both text region types charge the other half in the page stream.
        for number in 19..35 {
            data.extend_from_slice(&segment(
                number,
                if number % 2 == 0 { 6 } else { 7 },
                &[1],
                1,
                &small_region(),
            ));
        }
        jbig2_prescan(&data, Some(&globals)).unwrap(); // Exactly 2^20.
        data.extend_from_slice(&segment(35, 7, &[2], 1, &small_region()));
        assert_jbig2_fallback(data, Some(globals)); // Exactly 2^20 + 1.
    }
    #[test]
    fn fix_r3_jbig2_duplicate_dictionary_numbers_use_largest_export() {
        for exports in [[32769, 1], [1, 32769]] {
            let mut dictionaries = Vec::new();
            for exported in exports {
                dictionaries.extend_from_slice(&segment(1, 0, &[], 0, &symbol_header(exported)));
            }
            let mut data = page_info(64, 8);
            for number in 3..35 {
                data.extend_from_slice(&segment(number, 7, &[1], 1, &small_region()));
            }
            assert_jbig2_fallback(data.clone(), Some(dictionaries.clone()));
            // hayro sorts by segment number, so even a dictionary encoded after
            // its consumers is earlier for decoding and must be counted.
            data.extend_from_slice(&dictionaries);
            assert_jbig2_fallback(data, None);
        }
    }
    #[test]
    fn fix_r3_jbig2_symbol_header_flags_and_truncation() {
        // T.88 SDHUFF, SDREFAGG, SDTEMPLATE, SDRTEMPLATE and the resulting AT bytes.
        for (flags, at_bytes) in [
            (0u16, 8),
            (0x400, 2),
            (0x800, 2),
            (0xc00, 2),
            (1, 0),
            (3, 4),
            (2, 12),
            (0x402, 6),
            (0x1002, 8),
            (0x1402, 2),
            (0x1003, 0),
        ] {
            let mut payload = flags.to_be_bytes().to_vec();
            payload.extend(std::iter::repeat_n(0x7f, at_bytes));
            payload.extend_from_slice(&(JBIG2_MAX_SYMBOL_REFS as u32).to_be_bytes());
            payload.extend_from_slice(&0u32.to_be_bytes());
            let mut data = page_info(64, 8);
            data.extend_from_slice(&segment(3, 7, &[1], 1, &small_region()));
            // Header-only boundary: do not decode the synthetic dictionary.
            let globals = segment(1, 0, &[], 0, &payload);
            jbig2_prescan(&data, Some(&globals)).unwrap();
            for end in 0..payload.len() {
                let globals = segment(1, 0, &[], 0, &payload[..end]);
                assert_jbig2_rejected(&data, Some(&globals));
            }
            let offset = payload.len() - 8;
            payload[offset..offset + 4]
                .copy_from_slice(&((JBIG2_MAX_SYMBOL_REFS + 1) as u32).to_be_bytes());
            let globals = segment(1, 0, &[], 0, &payload);
            assert_jbig2_fallback(data, Some(globals));
        }
    }
}
