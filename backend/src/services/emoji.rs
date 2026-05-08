//! Pure-function image validation + thumbnail rendering for emoji uploads
//! (TODO [35]).
//!
//! Mirrors the structure of [`crate::services::avatar`] — the two are
//! deliberately separate modules rather than a parameterised one because
//! the TODOs and ARCHITECTURE.md treat the two upload paths as distinct
//! features (different limits, different thumbnail sizes, different
//! buckets) that are likely to evolve independently.
//!
//! ## Limits, copied from the spec
//!
//! - 500 KB max payload (vs. 2 MiB for avatars — emojis are inline in
//!   the chat view, so we want them small).
//! - PNG / JPEG only, sniffed from magic bytes (the multipart
//!   Content-Type header is *not* trusted).
//! - 100×100 thumbnail (vs. 200×200 for avatars).

use std::io::Cursor;

use image::{DynamicImage, ImageFormat};
use thiserror::Error;

/// Hard upper bound on an emoji payload — TODO [35] / ARCHITECTURE.md
/// §4.5: "PNG/JPG ≤ 500KB". Multipart layer rejects oversized bodies
/// before decode.
pub const MAX_EMOJI_BYTES: usize = 500 * 1024;

/// Square edge length of the emoji thumbnail. TODO [35] calls out 100×100.
pub const EMOJI_THUMBNAIL_EDGE_PX: u32 = 100;

/// Defensive cap on decoded dimensions. Same posture as the avatar
/// service — guards against decompression bombs that fit under the byte
/// cap but expand to gigapixel raw buffers.
const MAX_DIMENSION_PX: u32 = 4_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EmojiError {
    #[error("emoji payload is empty")]
    Empty,
    #[error("emoji exceeds the {MAX_EMOJI_BYTES}-byte limit")]
    TooLarge,
    #[error("emoji must be PNG or JPEG")]
    UnsupportedFormat,
    #[error("emoji dimensions exceed the {MAX_DIMENSION_PX}px-per-edge limit")]
    DimensionsTooLarge,
    #[error("emoji bytes are not a valid image")]
    Decode,
    #[error("could not encode thumbnail")]
    Encode,
}

pub fn validate_emoji_bytes(bytes: &[u8]) -> Result<ImageFormat, EmojiError> {
    if bytes.is_empty() {
        return Err(EmojiError::Empty);
    }
    if bytes.len() > MAX_EMOJI_BYTES {
        return Err(EmojiError::TooLarge);
    }
    let format = image::guess_format(bytes).map_err(|_| EmojiError::UnsupportedFormat)?;
    match format {
        ImageFormat::Png | ImageFormat::Jpeg => Ok(format),
        _ => Err(EmojiError::UnsupportedFormat),
    }
}

pub fn decode_within_limits(
    bytes: &[u8],
    format: ImageFormat,
) -> Result<DynamicImage, EmojiError> {
    let img =
        image::load_from_memory_with_format(bytes, format).map_err(|_| EmojiError::Decode)?;
    if img.width() > MAX_DIMENSION_PX || img.height() > MAX_DIMENSION_PX {
        return Err(EmojiError::DimensionsTooLarge);
    }
    Ok(img)
}

pub fn render_thumbnail(img: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, EmojiError> {
    // Keep aspect ratio for emojis (vs. centre-crop for avatars). A
    // tall narrow custom emoji shouldn't be square-cropped — the
    // thumbnail is decorative, not a profile picture.
    let thumb = img.thumbnail(EMOJI_THUMBNAIL_EDGE_PX, EMOJI_THUMBNAIL_EDGE_PX);
    let mut out = Vec::with_capacity(8 * 1024);
    thumb
        .write_to(&mut Cursor::new(&mut out), format)
        .map_err(|_| EmojiError::Encode)?;
    Ok(out)
}

pub fn extension_for(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Png => "png",
        _ => "bin",
    }
}

pub fn content_type_for(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn synth_image(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let mut img = RgbImage::new(width, height);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([(x % 256) as u8, (y % 256) as u8, 96]);
        }
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), format)
            .expect("encode");
        out
    }

    #[test]
    fn validate_rejects_empty() {
        assert_eq!(validate_emoji_bytes(&[]).unwrap_err(), EmojiError::Empty);
    }

    #[test]
    fn validate_rejects_oversized() {
        let bytes = vec![0u8; MAX_EMOJI_BYTES + 1];
        assert_eq!(
            validate_emoji_bytes(&bytes).unwrap_err(),
            EmojiError::TooLarge
        );
    }

    #[test]
    fn validate_accepts_png() {
        let bytes = synth_image(8, 8, ImageFormat::Png);
        assert_eq!(validate_emoji_bytes(&bytes).unwrap(), ImageFormat::Png);
    }

    #[test]
    fn validate_accepts_jpeg() {
        let bytes = synth_image(8, 8, ImageFormat::Jpeg);
        assert_eq!(validate_emoji_bytes(&bytes).unwrap(), ImageFormat::Jpeg);
    }

    #[test]
    fn validate_rejects_gif_header() {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&[0u8; 100]);
        assert_eq!(
            validate_emoji_bytes(&bytes).unwrap_err(),
            EmojiError::UnsupportedFormat
        );
    }

    #[test]
    fn decode_rejects_image_above_dimension_limit() {
        let bytes = synth_image(MAX_DIMENSION_PX + 1, 1, ImageFormat::Png);
        assert_eq!(
            decode_within_limits(&bytes, ImageFormat::Png).unwrap_err(),
            EmojiError::DimensionsTooLarge
        );
    }

    /// Aspect-preserving thumbnail — a 400×100 source must produce a
    /// 100×25 thumb, not a square crop. Distinguishes us from the avatar
    /// pipeline which deliberately squares its output.
    #[test]
    fn thumbnail_preserves_aspect_ratio() {
        let bytes = synth_image(400, 100, ImageFormat::Png);
        let img = decode_within_limits(&bytes, ImageFormat::Png).unwrap();
        let thumb_bytes = render_thumbnail(&img, ImageFormat::Png).unwrap();
        let thumb = image::load_from_memory_with_format(&thumb_bytes, ImageFormat::Png)
            .expect("decodes");
        assert_eq!(thumb.width(), EMOJI_THUMBNAIL_EDGE_PX);
        // 400:100 = 4:1 aspect → 100:25 thumb.
        assert_eq!(thumb.height(), 25);
    }

    #[test]
    fn extension_mapping_is_pinned() {
        assert_eq!(extension_for(ImageFormat::Jpeg), "jpg");
        assert_eq!(extension_for(ImageFormat::Png), "png");
    }

    #[test]
    fn content_type_mapping_is_pinned() {
        assert_eq!(content_type_for(ImageFormat::Jpeg), "image/jpeg");
        assert_eq!(content_type_for(ImageFormat::Png), "image/png");
    }

    /// TODO [35] / ARCHITECTURE.md: 500 KB. Pin so a refactor can't
    /// silently widen the limit.
    #[test]
    fn max_emoji_bytes_is_500_kib() {
        assert_eq!(MAX_EMOJI_BYTES, 500 * 1024);
    }

    #[test]
    fn thumbnail_edge_is_100() {
        assert_eq!(EMOJI_THUMBNAIL_EDGE_PX, 100);
    }
}
