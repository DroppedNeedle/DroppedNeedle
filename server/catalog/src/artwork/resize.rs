//! Downsize oversized local art for delivery, and draw an artist's mosaic
//! of covers.
//!
//! Folder images and embedded pictures can be many megapixels; a 250-pixel
//! grid tile should not ship them whole. A picture larger than the asked
//! size is decoded, scaled to fit inside it (aspect kept), and encoded as
//! JPEG at quality 85. Only a PNG or WebP with real transparency (a pixel
//! whose alpha is below full) keeps its own format, losslessly, so its
//! see-through parts survive. Pictures already small enough, of another
//! format, or over 40 megapixels (a decompression-bomb guard) are left as
//! they are. Cover Art Archive art comes here only to make a 250 or 375
//! from its 500 thumbnail. A mosaic decodes its covers under the same
//! limits and is always JPEG. Callers run this on the blocking pool.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, ImageReader, RgbImage, imageops::FilterType};

/// Largest picture decoded for resizing.
const MAX_PIXELS: u64 = 40_000_000;
/// JPEG quality for renditions.
const JPEG_QUALITY: u8 = 85;

/// Why a picture is not decoded.
#[derive(Debug)]
pub enum Undecodable {
    /// Not JPEG, PNG or WebP.
    Format,
    /// Over the 40 megapixel decode limit.
    TooLarge,
    /// No pixels on one edge.
    Empty,
    /// The decoder refused it.
    Broken(image::ImageError),
}

impl std::fmt::Display for Undecodable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format => f.write_str("not JPEG, PNG or WebP"),
            Self::TooLarge => f.write_str("over the decode pixel limit"),
            Self::Empty => f.write_str("has no pixels"),
            Self::Broken(error) => write!(f, "{error}"),
        }
    }
}

/// The picture's format, MIME type and dimensions, read from its header.
fn probe(bytes: &[u8]) -> Result<(ImageFormat, &'static str, u32, u32), Undecodable> {
    let format = image::guess_format(bytes).map_err(|_| Undecodable::Format)?;
    let content_type = match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
        ImageFormat::WebP => "image/webp",
        _ => return Err(Undecodable::Format),
    };
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(Undecodable::Broken)?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Undecodable::TooLarge);
    }
    Ok((format, content_type, width, height))
}

fn decode(bytes: &[u8], format: ImageFormat) -> Result<DynamicImage, Undecodable> {
    ImageReader::with_format(Cursor::new(bytes), format)
        .decode()
        .map_err(Undecodable::Broken)
}

/// Whether any pixel is less than fully opaque. A format with no alpha
/// channel never is; an alpha channel left at full is not transparency.
fn has_transparency(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageLumaA8(pixels) => pixels.pixels().any(|pixel| pixel.0[1] < u8::MAX),
        DynamicImage::ImageRgba8(pixels) => pixels.pixels().any(|pixel| pixel.0[3] < u8::MAX),
        DynamicImage::ImageLumaA16(pixels) => pixels.pixels().any(|pixel| pixel.0[1] < u16::MAX),
        DynamicImage::ImageRgba16(pixels) => pixels.pixels().any(|pixel| pixel.0[3] < u16::MAX),
        DynamicImage::ImageRgba32F(pixels) => pixels.pixels().any(|pixel| pixel.0[3] < 1.0),
        other => other.color().has_alpha(),
    }
}

fn encode_jpeg(image: &DynamicImage) -> Result<Vec<u8>, image::ImageError> {
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    image.to_rgb8().write_with_encoder(encoder)?;
    Ok(out)
}

/// A smaller rendition fitting inside `max` by `max` pixels, with its MIME
/// type (JPEG unless the picture has real transparency), or `None` when the
/// original should be served as it is.
pub fn shrink_to_fit(bytes: &[u8], max: u32) -> Option<(Vec<u8>, &'static str)> {
    if max == 0 {
        return None;
    }
    let (format, content_type, width, height) = probe(bytes).ok()?;
    if width.max(height) <= max {
        return None;
    }
    let decoded = match decode(bytes, format) {
        Ok(decoded) => decoded,
        Err(error) => {
            tracing::debug!(%error, "local art did not decode; serving it unresized");
            return None;
        }
    };
    let transparent = format != ImageFormat::Jpeg && has_transparency(&decoded);
    let scaled = decoded.resize(max, max, FilterType::Lanczos3);
    let encoded = if transparent {
        let mut out = Vec::new();
        scaled
            .to_rgba8()
            .write_to(&mut Cursor::new(&mut out), format)
            .map(|()| (out, content_type))
    } else {
        encode_jpeg(&scaled).map(|out| (out, "image/jpeg"))
    };
    match encoded {
        Ok(rendition) => Some(rendition),
        Err(error) => {
            tracing::debug!(%error, "local art rendition did not encode; serving it unresized");
            None
        }
    }
}

/// One cover cut to a `side` by `side` square (centre kept) for a mosaic.
/// The square is cut in source pixels before scaling, so a long sliver of
/// a picture never grows into a huge intermediate.
pub fn tile(bytes: &[u8], side: u32) -> Result<RgbImage, Undecodable> {
    let (format, ..) = probe(bytes)?;
    let decoded = decode(bytes, format)?;
    let (width, height) = (decoded.width(), decoded.height());
    let edge = width.min(height);
    if edge == 0 {
        return Err(Undecodable::Empty);
    }
    Ok(decoded
        .crop_imm((width - edge) / 2, (height - edge) / 2, edge, edge)
        .resize_exact(side, side, FilterType::Lanczos3)
        .to_rgb8())
}

/// Square tiles of `side` pixels drawn as one JPEG twice as wide: four or
/// more as a 2x2 of the first four, two or three as the first two
/// crosswise (A B / B A). `None` for fewer than two tiles or a failed
/// encode.
pub fn mosaic(tiles: &[RgbImage], side: u32) -> Option<Vec<u8>> {
    let order: &[usize] = match tiles.len() {
        0 | 1 => return None,
        2 | 3 => &[0, 1, 1, 0],
        _ => &[0, 1, 2, 3],
    };
    let mut canvas = RgbImage::new(side * 2, side * 2);
    for (place, &index) in (0u32..).zip(order) {
        let (x, y) = ((place % 2) * side, (place / 2) * side);
        image::imageops::replace(&mut canvas, &tiles[index], i64::from(x), i64::from(y));
    }
    match encode_jpeg(&DynamicImage::ImageRgb8(canvas)) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            tracing::warn!(%error, "artist mosaic did not encode");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(image: DynamicImage) -> Vec<u8> {
        let mut out = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn an_opaque_png_shrinks_to_a_jpeg_and_a_see_through_one_stays_png() {
        let opaque = png(DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            600,
            600,
            image::Rgba([200, 30, 30, 255]),
        )));
        let (bytes, content_type) = shrink_to_fit(&opaque, 250).unwrap();
        assert_eq!(content_type, "image/jpeg");
        assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Jpeg);

        let mut clear = image::RgbaImage::from_pixel(600, 600, image::Rgba([200, 30, 30, 255]));
        clear.put_pixel(10, 10, image::Rgba([0, 0, 0, 0]));
        let (bytes, content_type) =
            shrink_to_fit(&png(DynamicImage::ImageRgba8(clear)), 250).unwrap();
        assert_eq!(content_type, "image/png");
        let shrunk = image::load_from_memory_with_format(&bytes, ImageFormat::Png).unwrap();
        assert!(
            shrunk.to_rgba8().pixels().any(|pixel| pixel.0[3] < u8::MAX),
            "the see-through pixel survives"
        );
    }
}
