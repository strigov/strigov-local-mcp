use crate::{
    documents::{ImageFormat, Source},
    filesystem::{Result, error, io_error},
};
use image::{
    DynamicImage, GrayImage, ImageDecoder, ImageReader, RgbImage, RgbaImage, metadata::Orientation,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};

pub(crate) const IMAGE_SOURCE_LIMIT: u64 = 128 * 1024 * 1024;
pub(crate) const IMAGE_DECODE_ALLOC: u64 = 256 * 1024 * 1024;
pub(crate) const IMAGE_MAX_SIDE: u32 = 2048;
// Ф1: min(4 MiB, 1992232, max(1 MiB, 1992232 * 3 / 4)).
pub(crate) const IMAGE_OUTPUT_LIMIT: usize = 1_494_174;
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
pub(crate) fn parse_region(args: &Value) -> Result<Option<Region>> {
    let Some(value) = args.get("region") else {
        return Ok(None);
    };
    let invalid = || {
        error(
            "INVALID_REGION",
            "region must contain x,y,width,height fractions within the oriented image",
        )
    };
    let obj = value.as_object().ok_or_else(invalid)?;
    if obj.len() != 4 {
        return Err(invalid());
    }
    let n = |key| {
        obj.get(key)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .ok_or_else(invalid)
    };
    let r = Region {
        x: n("x")?,
        y: n("y")?,
        width: n("width")?,
        height: n("height")?,
    };
    if r.x < 0.
        || r.y < 0.
        || r.x > 1.
        || r.y > 1.
        || r.width <= 0.
        || r.height <= 0.
        || r.width > 1.
        || r.height > 1.
        || r.x + r.width > 1. + 1e-9
        || r.y + r.height > 1. + 1e-9
    {
        return Err(invalid());
    }
    Ok(Some(r))
}
#[derive(Debug)]
pub(crate) struct ViewImage {
    pub mime: &'static str,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub page: Option<u32>,
    pub pages: Option<u32>,
    pub passthrough: bool,
    pub region: Option<Region>,
}
fn invalid(_: impl std::fmt::Debug) -> crate::filesystem::FsError {
    error("DOCUMENT_INVALID", "Invalid or over-limit image")
}
fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(IMAGE_DECODE_ALLOC);
    limits
}
fn check_source(source: &mut Source) -> Result<u64> {
    let size = source.seek(SeekFrom::End(0)).map_err(io_error)?;
    source.rewind().map_err(io_error)?;
    if size > IMAGE_SOURCE_LIMIT {
        return Err(error("FILE_TOO_LARGE", "Image exceeds 128 MiB"));
    }
    Ok(size)
}
fn tiff_decoder(source: &mut Source) -> Result<tiff::decoder::Decoder<&mut Source>> {
    source.rewind().map_err(io_error)?;
    let mut limits = tiff::decoder::Limits::default();
    limits.decoding_buffer_size = IMAGE_DECODE_ALLOC as usize;
    tiff::decoder::Decoder::new(source)
        .map(|d| d.with_limits(limits))
        .map_err(invalid)
}
fn tiff_pages(source: &mut Source) -> Result<u32> {
    let mut d = tiff_decoder(source)?;
    let mut count = 1u32;
    while d.more_images() {
        d.next_image().map_err(invalid)?;
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid("too many TIFF pages"))?;
    }
    Ok(count)
}
pub(crate) fn info(source: &mut Source, format: ImageFormat) -> Result<Value> {
    check_source(source)?;
    let pages = if format == ImageFormat::Tiff {
        tiff_pages(source)?
    } else {
        1
    };
    source.rewind().map_err(io_error)?;
    let mut reader = ImageReader::new(BufReader::new(source))
        .with_guessed_format()
        .map_err(invalid)?;
    reader.limits(limits());
    let mut decoder = reader.into_decoder().map_err(invalid)?;
    let (w, h) = decoder.dimensions();
    let orientation = decoder.orientation().map_err(invalid)?;
    let (width, height) = if orientation.to_exif() >= 5 {
        (h, w)
    } else {
        (w, h)
    };
    Ok(json!({"width":width,"height":height,"pages":pages}))
}
pub(crate) fn view_file(
    source: &mut Source,
    format: ImageFormat,
    page: Option<u32>,
    region: Option<Region>,
) -> Result<ViewImage> {
    let size = check_source(source)?;
    let pages = if format == ImageFormat::Tiff {
        tiff_pages(source)?
    } else {
        1
    };
    let selected = page.unwrap_or(1);
    if selected == 0 || selected > pages {
        return Err(error(
            "PAGE_OUT_OF_RANGE",
            "Page is outside the image page range",
        ));
    }
    let mut view = if format == ImageFormat::Tiff && selected > 1 {
        let mut d = tiff_decoder(source)?;
        d.seek_to_image((selected - 1) as usize).map_err(invalid)?;
        let (w, h) = d.dimensions().map_err(invalid)?;
        let color = d.colortype().map_err(invalid)?;
        let channels = match color {
            tiff::ColorType::Gray(1 | 8) => 1,
            tiff::ColorType::RGB(8) => 3,
            tiff::ColorType::RGBA(8) => 4,
            _ => {
                return Err(error(
                    "UNSUPPORTED_FORMAT",
                    "TIFF page must be Gray 1/8-bit or RGB/RGBA 8-bit",
                ));
            }
        };
        if w == 0 || h == 0 || u64::from(w) * u64::from(h) > IMAGE_DECODE_ALLOC / channels {
            return Err(invalid("TIFF pixels"));
        }
        let orientation = Orientation::from_exif(
            d.get_tag_u32(tiff::tags::Tag::Orientation)
                .unwrap_or(1)
                .try_into()
                .unwrap_or(1),
        )
        .unwrap_or(Orientation::NoTransforms);
        let tiff::decoder::DecodingResult::U8(mut data) = d.read_image().map_err(invalid)? else {
            return Err(invalid("TIFF samples"));
        };
        if color == tiff::ColorType::Gray(1) {
            let row = w.div_ceil(8) as usize;
            if data.len() != row * h as usize {
                return Err(invalid("TIFF packed length"));
            }
            data = (0..h as usize)
                .flat_map(|y| (0..w as usize).map(move |x| (y, x)))
                .map(|(y, x)| {
                    if data[y * row + x / 8] & (128 >> (x % 8)) == 0 {
                        0
                    } else {
                        255
                    }
                })
                .collect();
        }
        let mut image = match channels {
            1 => DynamicImage::ImageLuma8(
                GrayImage::from_raw(w, h, data).ok_or_else(|| invalid("TIFF length"))?,
            ),
            3 => DynamicImage::ImageRgb8(
                RgbImage::from_raw(w, h, data).ok_or_else(|| invalid("TIFF length"))?,
            ),
            _ => DynamicImage::ImageRgba8(
                RgbaImage::from_raw(w, h, data).ok_or_else(|| invalid("TIFF length"))?,
            ),
        };
        image.apply_orientation(orientation);
        view_pixels(image, color == tiff::ColorType::Gray(1), 0, region)?
    } else {
        source.rewind().map_err(io_error)?;
        let mut reader = ImageReader::new(BufReader::new(&mut *source))
            .with_guessed_format()
            .map_err(invalid)?;
        reader.limits(limits());
        let actual = reader.format();
        let mut decoder = reader.into_decoder().map_err(invalid)?;
        let (w, h) = decoder.dimensions();
        if decoder.total_bytes() > IMAGE_DECODE_ALLOC {
            return Err(invalid("image pixels"));
        }
        let orientation = decoder.orientation().map_err(invalid)?;
        let lossless = matches!(
            format,
            ImageFormat::Png | ImageFormat::Gif | ImageFormat::Bmp
        ) || decoder.original_color_type() == image::ExtendedColorType::L1;
        if region.is_none()
            && orientation == Orientation::NoTransforms
            && w <= IMAGE_MAX_SIDE
            && h <= IMAGE_MAX_SIDE
            && size <= IMAGE_OUTPUT_LIMIT as u64
            && matches!(
                (format, actual),
                (ImageFormat::Png, Some(image::ImageFormat::Png))
                    | (ImageFormat::Jpeg, Some(image::ImageFormat::Jpeg))
            )
        {
            drop(decoder);
            source.rewind().map_err(io_error)?;
            let mut data = Vec::with_capacity(size as usize + 1);
            source
                .take(size + 1)
                .read_to_end(&mut data)
                .map_err(io_error)?;
            if data.len() as u64 > size {
                return Err(error("FILE_TOO_LARGE", "Image grew while reading"));
            }
            ViewImage {
                mime: if format == ImageFormat::Png {
                    "image/png"
                } else {
                    "image/jpeg"
                },
                data,
                width: w,
                height: h,
                source_width: w,
                source_height: h,
                page: None,
                pages: None,
                passthrough: true,
                region: None,
            }
        } else {
            let mut image = DynamicImage::from_decoder(decoder).map_err(invalid)?;
            image.apply_orientation(orientation);
            view_pixels(image, lossless, 0, region)?
        }
    };
    if format == ImageFormat::Tiff || page.is_some() {
        view.page = Some(selected);
        view.pages = Some(pages);
    }
    Ok(view)
}
pub(crate) fn view_pixels(
    mut image: DynamicImage,
    lossless: bool,
    rotate_degrees: u16,
    region: Option<Region>,
) -> Result<ViewImage> {
    image = match rotate_degrees {
        90 => image.rotate90(),
        180 => image.rotate180(),
        270 => image.rotate270(),
        _ => image,
    };
    let (source_width, source_height) = (image.width(), image.height());
    if let Some(r) = region {
        let x =
            ((r.x * f64::from(source_width)).floor() as u32).min(source_width.saturating_sub(1));
        let y =
            ((r.y * f64::from(source_height)).floor() as u32).min(source_height.saturating_sub(1));
        let right = (((r.x + r.width) * f64::from(source_width)).ceil() as u32)
            .min(source_width)
            .max(x + 1);
        let bottom = (((r.y + r.height) * f64::from(source_height)).ceil() as u32)
            .min(source_height)
            .max(y + 1);
        image = image.crop_imm(x, y, right - x, bottom - y);
    }
    if image.width() > IMAGE_MAX_SIDE || image.height() > IMAGE_MAX_SIDE {
        image = image.thumbnail(IMAGE_MAX_SIDE, IMAGE_MAX_SIDE);
    }
    let (mime, data, width, height) = encode(image, lossless, IMAGE_OUTPUT_LIMIT)?;
    Ok(ViewImage {
        mime,
        data,
        width,
        height,
        source_width,
        source_height,
        page: None,
        pages: None,
        passthrough: false,
        region,
    })
}
fn encode(
    mut image: DynamicImage,
    lossless: bool,
    limit: usize,
) -> Result<(&'static str, Vec<u8>, u32, u32)> {
    if lossless {
        let mut data = Cursor::new(Vec::new());
        image
            .write_to(&mut data, image::ImageFormat::Png)
            .map_err(invalid)?;
        if data.get_ref().len() <= limit {
            return Ok((
                "image/png",
                data.into_inner(),
                image.width(),
                image.height(),
            ));
        }
    }
    // JPEG has no alpha: flatten every pixel onto white before encoding.
    if image.color().has_alpha() {
        let rgba = image.to_rgba8();
        image = DynamicImage::ImageRgb8(RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
            let p = rgba.get_pixel(x, y);
            let a = u16::from(p[3]);
            image::Rgb(
                [0, 1, 2].map(|i| ((u16::from(p[i]) * a + 255 * (255 - a) + 127) / 255) as u8),
            )
        }));
    }
    let mut rgb = image.to_rgb8();
    for attempt in 0..3 {
        if attempt == 2 {
            rgb = image::imageops::thumbnail(
                &rgb,
                (rgb.width() / 2).max(1),
                (rgb.height() / 2).max(1),
            );
        }
        let mut data = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut data,
            if attempt == 0 { 85 } else { 70 },
        )
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(invalid)?;
        if data.len() <= limit {
            return Ok(("image/jpeg", data, rgb.width(), rgb.height()));
        }
    }
    Err(error(
        "OUTPUT_TOO_LARGE",
        "Image cannot fit the calibrated response limit",
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, GrayImage, Luma};
    #[test]
    fn oriented_crop_and_downscale_preserve_the_requested_area() {
        let image = GrayImage::from_fn(4000, 1000, |x, _| Luma([if x < 2000 { 0 } else { 255 }]));
        let whole = view_pixels(DynamicImage::ImageLuma8(image.clone()), true, 0, None).unwrap();
        assert_eq!((whole.width, whole.height), (2048, 512));
        let crop = view_pixels(
            DynamicImage::ImageLuma8(image),
            true,
            90,
            Some(Region {
                x: 0.,
                y: 0.5,
                width: 1.,
                height: 0.5,
            }),
        )
        .unwrap();
        assert_eq!((crop.source_width, crop.source_height), (1000, 4000));
        assert_eq!((crop.width, crop.height), (1000, 2000));
        let pixels = image::load_from_memory(&crop.data).unwrap().to_luma8();
        assert!(pixels.get_pixel(500, 1000)[0] > 240);
    }
    #[test]
    fn region_rejects_invalid_and_non_numeric_bounds() {
        for region in [
            serde_json::json!({"x":0.8,"y":0,"width":0.3,"height":1}),
            serde_json::json!({"x":0,"y":0,"width":0,"height":1}),
            serde_json::json!({"x":"0","y":0,"width":1,"height":1}),
        ] {
            assert_eq!(
                parse_region(&serde_json::json!({"region":region}))
                    .unwrap_err()
                    .code,
                "INVALID_REGION"
            );
        }
    }
    fn source(image: &DynamicImage, format: image::ImageFormat) -> Source {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        Source::Memory(Cursor::new(bytes.into_inner()))
    }
    #[test]
    fn jpeg_exif_orientation_is_applied_before_region() {
        let image = DynamicImage::ImageLuma8(GrayImage::from_fn(64, 32, |x, _| {
            Luma([if x < 32 { 0 } else { 255 }])
        }));
        let Source::Memory(bytes) = source(&image, image::ImageFormat::Jpeg) else {
            unreachable!()
        };
        // APP1 Exif: little-endian TIFF, one Orientation SHORT entry, value 6 (90 CW).
        let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let jpeg = bytes.into_inner();
        let mut oriented = jpeg[..2].to_vec();
        oriented.extend_from_slice(&[255, 225]);
        oriented.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        oriented.extend_from_slice(exif);
        oriented.extend_from_slice(&jpeg[2..]);
        let mut input = Source::Memory(Cursor::new(oriented));
        let view = view_file(
            &mut input,
            ImageFormat::Jpeg,
            None,
            Some(Region {
                x: 0.,
                y: 0.5,
                width: 1.,
                height: 0.5,
            }),
        )
        .unwrap();
        assert_eq!(
            (
                view.source_width,
                view.source_height,
                view.width,
                view.height
            ),
            (32, 64, 32, 32)
        );
        assert!(!view.passthrough);
        assert!(
            image::load_from_memory(&view.data)
                .unwrap()
                .to_luma8()
                .get_pixel(16, 16)[0]
                > 240
        );
    }
    #[test]
    fn passthrough_formats_encoding_and_output_ceiling() {
        let image = DynamicImage::ImageLuma8(GrayImage::from_pixel(32, 16, Luma([30])));
        for (fmt, kind) in [
            (image::ImageFormat::Png, ImageFormat::Png),
            (image::ImageFormat::Jpeg, ImageFormat::Jpeg),
        ] {
            let mut input = source(&image, fmt);
            let original = match &input {
                Source::Memory(b) => b.get_ref().clone(),
                _ => unreachable!(),
            };
            let view = view_file(&mut input, kind, None, None).unwrap();
            assert!(view.passthrough);
            assert_eq!(view.data, original);
            assert_eq!(
                view_file(&mut input, kind, Some(2), None).unwrap_err().code,
                "PAGE_OUT_OF_RANGE"
            );
        }
        assert_eq!(
            view_pixels(image.clone(), true, 0, None).unwrap().mime,
            "image/png"
        );
        assert_eq!(
            view_pixels(image.clone(), false, 0, None).unwrap().mime,
            "image/jpeg"
        );
        assert_eq!(
            encode(image.clone(), true, 1).unwrap_err().code,
            "OUTPUT_TOO_LARGE"
        );
        // High-frequency deterministic pixels force PNG -> JPEG -> lower quality/size.
        let noise = DynamicImage::ImageRgb8(RgbImage::from_fn(128, 128, |x, y| {
            image::Rgb([
                (x * 37 + y * 13) as u8,
                (x * 17 + y * 41) as u8,
                (x * 53 + y * 19) as u8,
            ])
        }));
        let (mime, data, w, h) = encode(noise, true, 4000).unwrap();
        assert_eq!(mime, "image/jpeg");
        assert!(data.len() <= 4000);
        assert_eq!((w, h), (64, 64));
        let transparent =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 0])));
        let view = view_pixels(transparent, false, 0, None).unwrap();
        assert!(
            image::load_from_memory(&view.data)
                .unwrap()
                .to_luma8()
                .get_pixel(4, 4)[0]
                > 240
        );
    }
    #[test]
    fn multipage_tiff_has_count_and_selected_pixels() {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut encoder = tiff::encoder::TiffEncoder::new(&mut bytes).unwrap();
            for value in [0u8, 100, 255] {
                encoder
                    .write_image::<tiff::encoder::colortype::Gray8>(16, 8, &[value; 128])
                    .unwrap();
            }
        }
        let mut input = Source::Memory(Cursor::new(bytes.into_inner()));
        let info = info(&mut input, ImageFormat::Tiff).unwrap();
        assert_eq!(info, json!({"width":16,"height":8,"pages":3}));
        for (page, value) in [(1, 0), (2, 100), (3, 255)] {
            let view = view_file(&mut input, ImageFormat::Tiff, Some(page), None).unwrap();
            assert_eq!((view.page, view.pages), (Some(page), Some(3)));
            assert!(
                (i16::from(
                    image::load_from_memory(&view.data)
                        .unwrap()
                        .to_luma8()
                        .get_pixel(8, 4)[0]
                ) - value)
                    .abs()
                    < 5
            );
        }
        assert_eq!(
            view_file(&mut input, ImageFormat::Tiff, Some(4), None)
                .unwrap_err()
                .code,
            "PAGE_OUT_OF_RANGE"
        );
    }
    #[test]
    fn decoder_rejects_forged_giant_png_without_pixels() {
        let Source::Memory(bytes) = source(&DynamicImage::new_luma8(1, 1), image::ImageFormat::Png)
        else {
            unreachable!()
        };
        let mut png = bytes.into_inner();
        png[16..20].copy_from_slice(&100000u32.to_be_bytes());
        png[20..24].copy_from_slice(&100000u32.to_be_bytes());
        let crc = png[12..29].iter().fold(u32::MAX, |mut c, b| {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = (c >> 1) ^ (0xedb88320u32 & (0u32.wrapping_sub(c & 1)));
            }
            c
        }) ^ u32::MAX;
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        let mut input = Source::Memory(Cursor::new(png));
        assert_eq!(
            view_file(&mut input, ImageFormat::Png, None, None)
                .unwrap_err()
                .code,
            "DOCUMENT_INVALID"
        );
    }
    #[test]
    fn bilevel_ccitt_tiff_first_and_later_pages_have_the_same_orientation_and_tones() {
        let mut fax = fax::encoder::Encoder::new(fax::VecWriter::new());
        for _ in 0..8 {
            fax.encode_line(
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
        let encoded = fax.finish().unwrap().finish();
        // Three minimal little-endian TIFF IFDs, CCITT Group4, WhiteIsZero.
        // The derived fixture is synthetic; no TIFF is committed.
        let ifd_size = 2 + 10 * 12 + 4;
        let data_start = 8 + 3 * ifd_size;
        let mut data = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        for page in 0..3 {
            data.extend_from_slice(&10u16.to_le_bytes());
            for (tag, kind, value) in [
                (256u16, 4u16, 64u32),
                (257, 4, 8),
                (258, 3, 1),
                (259, 3, 4),
                (262, 3, 0),
                (273, 4, (data_start + page * encoded.len()) as u32),
                (277, 3, 1),
                (278, 4, 8),
                (279, 4, encoded.len() as u32),
                (284, 3, 1),
            ] {
                data.extend_from_slice(&tag.to_le_bytes());
                data.extend_from_slice(&kind.to_le_bytes());
                data.extend_from_slice(&1u32.to_le_bytes());
                data.extend_from_slice(&value.to_le_bytes());
            }
            data.extend_from_slice(
                &(if page == 2 {
                    0
                } else {
                    8 + (page as u32 + 1) * ifd_size as u32
                })
                .to_le_bytes(),
            );
        }
        for _ in 0..3 {
            data.extend_from_slice(&encoded);
        }
        let mut input = Source::Memory(Cursor::new(data));
        for page in 1..=3 {
            let view = view_file(&mut input, ImageFormat::Tiff, Some(page), None).unwrap();
            assert_eq!((view.width, view.height, view.pages), (64, 8, Some(3)));
            assert_eq!(view.mime, "image/png");
            let pixels = image::load_from_memory(&view.data).unwrap().to_luma8();
            assert!(pixels.get_pixel(16, 4)[0] < 35 && pixels.get_pixel(48, 4)[0] > 220);
        }
    }
}
