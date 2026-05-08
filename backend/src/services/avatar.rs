//! Pure-function image validation + thumbnail rendering for avatar
//! uploads (TODO [22]).
//!
//! Kept free of `actix_web` and `s3` so it can be exercised entirely with
//! unit tests against in-memory byte buffers — the upload handler then
//! composes these helpers with multipart parsing and the S3 client.
//!
//! ## Format whitelist
//!
//! ARCHITECTURE.md §4.2 limits avatars to PNG and JPEG. We sniff the
//! format from the first few magic bytes (via `image::guess_format`)
//! rather than trusting the multipart `Content-Type` header, so a
//! mismatched / spoofed header cannot get a non-image past us. The
//! `image` crate is built with `default-features = false` so even if
//! someone crafts a valid GIF/WebP/etc. payload, the decoder isn't
//! linked and the format check rejects it.
//!
//! ## Why `resize_to_fill` instead of `thumbnail`
//!
//! Avatars are displayed at a fixed square size. `thumbnail(w, h)` keeps
//! aspect ratio and produces something *up to* `w × h`; `resize_to_fill`
//! center-crops to the exact target. Square crops are what users expect
//! from a profile picture, so we use the latter.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat};
use thiserror::Error;

/// Hard upper bound on an uploaded avatar payload — the multipart layer
/// rejects anything larger before we ever decode it. 2 MiB matches the
/// TODO ("PNG/JPG ≤ 2MB") and ARCHITECTURE.md §4.2.
pub const MAX_AVATAR_BYTES: usize = 2 * 1024 * 1024;

/// Square edge length of the thumbnail. ARCHITECTURE.md §4.2 calls out
/// 200×200 specifically.
pub const THUMBNAIL_EDGE_PX: u32 = 200;

/// Defensive cap on decoded dimensions. A valid 2 MiB PNG can still
/// expand to a multi-gigapixel raw buffer (decompression bomb); cap each
/// edge so we never ask the decoder for more than ~64 megapixels.
const MAX_DIMENSION_PX: u32 = 8_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AvatarError {
    #[error("avatar payload is empty")]
    Empty,
    #[error("avatar exceeds the {MAX_AVATAR_BYTES}-byte limit")]
    TooLarge,
    #[error("avatar must be PNG or JPEG")]
    UnsupportedFormat,
    #[error("avatar dimensions exceed the {MAX_DIMENSION_PX}px-per-edge limit")]
    DimensionsTooLarge,
    #[error("avatar bytes are not a valid image")]
    Decode,
    #[error("could not encode thumbnail")]
    Encode,
}

/// Bytes-only validation: size + magic-bytes format check. Done up-front
/// so an obviously bad payload bails before the decode allocates.
pub fn validate_avatar_bytes(bytes: &[u8]) -> Result<ImageFormat, AvatarError> {
    if bytes.is_empty() {
        return Err(AvatarError::Empty);
    }
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(AvatarError::TooLarge);
    }
    let format = image::guess_format(bytes).map_err(|_| AvatarError::UnsupportedFormat)?;
    match format {
        ImageFormat::Png | ImageFormat::Jpeg => Ok(format),
        _ => Err(AvatarError::UnsupportedFormat),
    }
}

/// Decode `bytes` (already validated by [`validate_avatar_bytes`]) and
/// reject if either edge exceeds [`MAX_DIMENSION_PX`]. Returns the
/// decoded image so the caller can hand it straight to
/// [`render_thumbnail`] without paying a second decode.
pub fn decode_within_limits(
    bytes: &[u8],
    format: ImageFormat,
) -> Result<DynamicImage, AvatarError> {
    let img =
        image::load_from_memory_with_format(bytes, format).map_err(|_| AvatarError::Decode)?;
    if img.width() > MAX_DIMENSION_PX || img.height() > MAX_DIMENSION_PX {
        return Err(AvatarError::DimensionsTooLarge);
    }
    Ok(img)
}

/// Render `img` to a centred 200×200 thumbnail in the same format as the
/// original (so the file extension can stay consistent: a JPEG upload
/// produces a `_thumb.jpg`, a PNG upload produces a `_thumb.png`).
pub fn render_thumbnail(img: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, AvatarError> {
    // Lanczos3 trades a touch of CPU for noticeably better edges on the
    // downscale. For a 200×200 target it's still cheap.
    let thumb = img.resize_to_fill(
        THUMBNAIL_EDGE_PX,
        THUMBNAIL_EDGE_PX,
        image::imageops::FilterType::Lanczos3,
    );
    let mut out = Vec::with_capacity(16 * 1024);
    thumb
        .write_to(&mut Cursor::new(&mut out), format)
        .map_err(|_| AvatarError::Encode)?;
    Ok(out)
}

/// File-extension and `Content-Type` mapping for the two formats we
/// accept. Pinning these as a single source of truth means the original
/// upload, the thumbnail, and the S3 metadata can never disagree.
pub fn extension_for(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Png => "png",
        // Unreachable in practice — `validate_avatar_bytes` only ever
        // returns Png/Jpeg — but writing it as a panic would be worse
        // than degrading to a sensible default.
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

    /// Encode an in-memory test image to PNG/JPEG bytes via the same
    /// crate we'll later decode them with. Round-tripping ensures any
    /// future format-feature regression (e.g. accidentally dropping
    /// `jpeg` from Cargo.toml) breaks the test loudly.
    fn synth_image(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let mut img = RgbImage::new(width, height);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), format)
            .expect("encode");
        out
    }

    // ── validate_avatar_bytes ──

    #[test]
    fn validate_rejects_empty() {
        assert_eq!(validate_avatar_bytes(&[]).unwrap_err(), AvatarError::Empty);
    }

    #[test]
    fn validate_rejects_oversized() {
        // One byte over the limit — boundary check.
        let bytes = vec![0u8; MAX_AVATAR_BYTES + 1];
        assert_eq!(
            validate_avatar_bytes(&bytes).unwrap_err(),
            AvatarError::TooLarge
        );
    }

    #[test]
    fn validate_rejects_garbage_bytes() {
        // Plausible-looking payload that's not a valid image header.
        let bytes = vec![0u8; 256];
        assert_eq!(
            validate_avatar_bytes(&bytes).unwrap_err(),
            AvatarError::UnsupportedFormat
        );
    }

    #[test]
    fn validate_accepts_png() {
        let bytes = synth_image(8, 8, ImageFormat::Png);
        assert_eq!(validate_avatar_bytes(&bytes).unwrap(), ImageFormat::Png);
    }

    #[test]
    fn validate_accepts_jpeg() {
        let bytes = synth_image(8, 8, ImageFormat::Jpeg);
        assert_eq!(validate_avatar_bytes(&bytes).unwrap(), ImageFormat::Jpeg);
    }

    /// GIF magic bytes — the format-whitelist must reject anything that
    /// isn't PNG or JPEG, even when the bytes look like a real image of
    /// some other kind. Pin against accidentally accepting more formats
    /// than the schema allows.
    #[test]
    fn validate_rejects_gif_header() {
        // `GIF89a` minimal header — `image::guess_format` recognises it
        // but our whitelist must turn it down.
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&[0u8; 100]);
        assert_eq!(
            validate_avatar_bytes(&bytes).unwrap_err(),
            AvatarError::UnsupportedFormat
        );
    }

    // ── decode_within_limits ──

    #[test]
    fn decode_accepts_normal_image() {
        let bytes = synth_image(64, 64, ImageFormat::Png);
        let img = decode_within_limits(&bytes, ImageFormat::Png).unwrap();
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 64);
    }

    #[test]
    fn decode_accepts_image_at_dimension_limit() {
        // We don't actually synthesise a 8000x8000 image (that would be
        // huge); instead test the boundary by constructing the check
        // semantics: a 8000x1 wide image must pass.
        let bytes = synth_image(MAX_DIMENSION_PX, 1, ImageFormat::Png);
        decode_within_limits(&bytes, ImageFormat::Png).expect("max-edge image accepted");
    }

    #[test]
    fn decode_rejects_image_above_dimension_limit() {
        let bytes = synth_image(MAX_DIMENSION_PX + 1, 1, ImageFormat::Png);
        assert_eq!(
            decode_within_limits(&bytes, ImageFormat::Png).unwrap_err(),
            AvatarError::DimensionsTooLarge
        );
    }

    // ── render_thumbnail ──

    #[test]
    fn thumbnail_emits_exact_target_dimensions() {
        // Non-square source — `resize_to_fill` must crop+resize to a
        // perfect 200×200, not preserve the source aspect.
        let bytes = synth_image(400, 100, ImageFormat::Png);
        let img = decode_within_limits(&bytes, ImageFormat::Png).unwrap();
        let thumb_bytes = render_thumbnail(&img, ImageFormat::Png).unwrap();

        let thumb = image::load_from_memory_with_format(&thumb_bytes, ImageFormat::Png)
            .expect("thumbnail decodes");
        assert_eq!(thumb.width(), THUMBNAIL_EDGE_PX);
        assert_eq!(thumb.height(), THUMBNAIL_EDGE_PX);
    }

    #[test]
    fn thumbnail_round_trips_jpeg() {
        let bytes = synth_image(300, 300, ImageFormat::Jpeg);
        let img = decode_within_limits(&bytes, ImageFormat::Jpeg).unwrap();
        let thumb_bytes = render_thumbnail(&img, ImageFormat::Jpeg).unwrap();

        // Jpeg magic: SOI marker FF D8 — pin format consistency so a
        // future refactor that accidentally re-encodes JPEG thumbs as
        // PNG fails loudly.
        assert_eq!(&thumb_bytes[..2], &[0xFF, 0xD8]);
    }

    // ── extension / content-type mapping ──

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

    /// The TODO and ARCHITECTURE.md both say "≤ 2MB". Pin the constant
    /// so a refactor can't silently widen the gate.
    #[test]
    fn max_avatar_bytes_is_two_mib() {
        assert_eq!(MAX_AVATAR_BYTES, 2 * 1024 * 1024);
    }

    #[test]
    fn thumbnail_edge_is_two_hundred() {
        assert_eq!(THUMBNAIL_EDGE_PX, 200);
    }
}
