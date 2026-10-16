//! Downsize oversized local art for delivery (v2
//! `CoverDeliveryThumbnailer`).
//!
//! Folder images and embedded pictures can be many megapixels; a 250-pixel
//! grid tile should not ship them whole. A picture larger than the asked
//! size is decoded, scaled to fit inside it (aspect kept), and encoded in
//! its own format: JPEG at quality 85, PNG and WebP lossless. Pictures
//! already small enough, of another format, or over 40 megapixels (v2
//! `MAX_DELIVERY_IMAGE_PIXELS`, a decompression-bomb guard) are left as
//! they are. Cover Art Archive art never comes here: the archive renders
//! its own thumbnails. Callers run this on the blocking pool.

use std::io::Cursor;

use image::{ImageFormat, ImageReader, imageops::FilterType};

/// Largest picture decoded for resizing (v2 `MAX_DELIVERY_IMAGE_PIXELS`).
const MAX_PIXELS: u64 = 40_000_000;
/// JPEG quality for renditions.
const JPEG_QUALITY: u8 = 85;

/// A smaller rendition fitting inside `max` by `max` pixels, with its MIME
/// type, or `None` when the original should be served as it is.
pub fn shrink_to_fit(bytes: &[u8], max: u32) -> Option<(Vec<u8>, &'static str)> {
    if max == 0 {
        return None;
    }
    let format = image::guess_format(bytes).ok()?;
    let content_type = match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
        ImageFormat::WebP => "image/webp",
        _ => return None,
    };
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .ok()?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS || width.max(height) <= max {
        return None;
    }
    let decoded = match ImageReader::with_format(Cursor::new(bytes), format).decode() {
        Ok(decoded) => decoded,
        Err(error) => {
            tracing::debug!(%error, "local art did not decode; serving it unresized");
            return None;
        }
    };
    let scaled = decoded.resize(max, max, FilterType::Lanczos3);
    let mut out = Vec::new();
    let encoded = match format {
        ImageFormat::Jpeg => {
            let encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
            scaled.to_rgb8().write_with_encoder(encoder)
        }
        _ => scaled
            .to_rgba8()
            .write_to(&mut Cursor::new(&mut out), format),
    };
    match encoded {
        Ok(()) => Some((out, content_type)),
        Err(error) => {
            tracing::debug!(%error, "local art rendition did not encode; serving it unresized");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 10, 10, 255]));
        let mut out = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn large_art_shrinks_to_fit_and_small_art_stays() {
        let (shrunk, content_type) = shrink_to_fit(&png(1000, 600), 250).unwrap();
        assert_eq!(content_type, "image/png");
        let decoded = image::load_from_memory(&shrunk).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (250, 150));
        assert!(shrink_to_fit(&png(200, 200), 250).is_none());
    }
}
