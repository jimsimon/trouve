//! Generic attachment presentation helpers for Slint clients.
//!
//! The `.slint` components consume plain media data; they know nothing about
//! trouve protocol types. Rust-side decoding is deliberately limited so a
//! small compressed attachment cannot allocate unbounded pixel or GIF-frame
//! memory.

use std::io::{BufReader, Cursor};

use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageReader};

slint::include_modules!();

/// Path to the crate's `.slint` sources for embedding in another scene.
pub const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui");

const MAX_DIMENSION: u32 = 8_192;
const MAX_PIXELS: u64 = 24_000_000;
const MAX_VIEW_WIDTH: u32 = 2_048;
const MAX_VIEW_HEIGHT: u32 = 2_048;
const THUMB_WIDTH: u32 = 160;
const THUMB_HEIGHT: u32 = 100;
const MAX_GIF_FRAMES: usize = 120;
const MAX_GIF_DECODED_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Gif,
    Video,
    File,
}

impl MediaKind {
    /// Stable integer mirrored by `MediaItem.kind` in `media-view.slint`.
    pub const fn as_i32(self) -> i32 {
        match self {
            Self::Image => 0,
            Self::Gif => 1,
            Self::Video => 2,
            Self::File => 3,
        }
    }

    pub const fn is_viewable(self) -> bool {
        matches!(self, Self::Image | Self::Gif)
    }
}

/// Classify from the server-recorded MIME type, with the filename only used
/// to distinguish GIFs whose MIME was reduced to a generic image type.
pub fn media_kind(name: &str, mime: &str) -> MediaKind {
    let mime = mime.to_ascii_lowercase();
    if mime == "image/gif" || name.to_ascii_lowercase().ends_with(".gif") {
        MediaKind::Gif
    } else if mime.starts_with("image/") {
        MediaKind::Image
    } else if mime.starts_with("video/") {
        MediaKind::Video
    } else {
        MediaKind::File
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedFrame {
    pub pixels: DecodedPixels,
    pub delay_ms: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedMedia {
    pub thumbnail: DecodedPixels,
    pub frames: Vec<DecodedFrame>,
    /// False when a GIF was reduced to its first frame by a safety limit.
    pub animated: bool,
}

/// Plain host-side mirror of `MediaItem`. Applications can keep this in
/// their own view models and convert it to generated Slint structs at the
/// UI-thread boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaItemData {
    pub key: String,
    pub name: String,
    pub meta: String,
    pub kind: MediaKind,
    pub decoded: Option<DecodedMedia>,
    pub loading: bool,
    pub failed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("unsupported or malformed image: {0}")]
    Image(#[from] image::ImageError),
    #[error("image dimensions {width}×{height} exceed the preview limit")]
    Dimensions { width: u32, height: u32 },
    #[error("image contains no frames")]
    NoFrames,
}

/// Decode a still image or GIF into a thumbnail and bounded viewer frames.
pub fn decode(bytes: &[u8], kind: MediaKind) -> Result<DecodedMedia, DecodeError> {
    match kind {
        MediaKind::Gif => decode_gif(bytes),
        MediaKind::Image => decode_still(bytes),
        MediaKind::Video | MediaKind::File => Err(DecodeError::NoFrames),
    }
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), DecodeError> {
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(DecodeError::Dimensions { width, height });
    }
    Ok(())
}

fn decode_still(bytes: &[u8]) -> Result<DecodedMedia, DecodeError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(image::ImageError::IoError)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_GIF_DECODED_BYTES as u64);
    reader.limits(limits);
    let decoded = reader.decode()?;
    validate_dimensions(decoded.width(), decoded.height())?;
    let thumbnail = to_pixels(bounded(decoded.clone(), THUMB_WIDTH, THUMB_HEIGHT));
    let full = to_pixels(bounded(decoded, MAX_VIEW_WIDTH, MAX_VIEW_HEIGHT));
    Ok(DecodedMedia {
        thumbnail,
        frames: vec![DecodedFrame {
            pixels: full,
            delay_ms: 0,
        }],
        animated: false,
    })
}

fn decode_gif(bytes: &[u8]) -> Result<DecodedMedia, DecodeError> {
    let decoder = GifDecoder::new(BufReader::new(Cursor::new(bytes)))?;
    let (width, height) = decoder.dimensions();
    validate_dimensions(width, height)?;

    let mut frames = Vec::new();
    let mut decoded_bytes = 0usize;
    let mut limited = false;
    for frame in decoder.into_frames() {
        let frame = frame?;
        let (delay_num, delay_den) = frame.delay().numer_denom_ms();
        let delay_ms = delay_num
            .checked_div(delay_den)
            .unwrap_or(100)
            .clamp(20, 10_000) as i32;
        let buffer = frame.into_buffer();
        decoded_bytes = decoded_bytes.saturating_add(buffer.len());
        if frames.len() >= MAX_GIF_FRAMES || decoded_bytes > MAX_GIF_DECODED_BYTES {
            limited = true;
            break;
        }
        let frame = bounded(
            DynamicImage::ImageRgba8(buffer),
            MAX_VIEW_WIDTH,
            MAX_VIEW_HEIGHT,
        );
        frames.push(DecodedFrame {
            pixels: to_pixels(frame),
            delay_ms,
        });
    }
    let first = frames.first().ok_or(DecodeError::NoFrames)?.pixels.clone();
    // Re-sample the already-bounded first viewer frame rather than decoding
    // compressed input twice.
    let thumbnail = if first.width <= THUMB_WIDTH && first.height <= THUMB_HEIGHT {
        first.clone()
    } else {
        // Slint images are not directly resizable; decode just the first
        // compressed frame again through the static path for a small tile.
        gif_thumbnail(bytes).unwrap_or_else(|| first.clone())
    };
    if limited {
        frames.truncate(1);
    }
    let animated = frames.len() > 1;
    Ok(DecodedMedia {
        thumbnail,
        frames,
        animated,
    })
}

fn gif_thumbnail(bytes: &[u8]) -> Option<DecodedPixels> {
    let decoder = GifDecoder::new(BufReader::new(Cursor::new(bytes))).ok()?;
    let frame = decoder.into_frames().next()?.ok()?;
    Some(to_pixels(bounded(
        DynamicImage::ImageRgba8(frame.into_buffer()),
        THUMB_WIDTH,
        THUMB_HEIGHT,
    )))
}

fn bounded(image: DynamicImage, max_width: u32, max_height: u32) -> DynamicImage {
    if image.width() <= max_width && image.height() <= max_height {
        image
    } else {
        image.thumbnail(max_width, max_height)
    }
}

fn to_pixels(image: DynamicImage) -> DecodedPixels {
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    DecodedPixels {
        width,
        height,
        rgba: rgba.into_raw(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::{GifEncoder, Repeat};

    #[test]
    fn classifies_media_without_trusting_extensions_for_video() {
        assert_eq!(media_kind("shot.png", "image/png"), MediaKind::Image);
        assert_eq!(
            media_kind("loop.GIF", "application/octet-stream"),
            MediaKind::Gif
        );
        assert_eq!(media_kind("demo.mp4", "video/mp4"), MediaKind::Video);
        assert_eq!(
            media_kind("demo.mp4", "application/octet-stream"),
            MediaKind::File
        );
    }

    #[test]
    fn decodes_a_small_png() {
        let source = DynamicImage::new_rgba8(2, 3);
        let mut bytes = Cursor::new(Vec::new());
        source
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let decoded = decode(bytes.get_ref(), MediaKind::Image).unwrap();
        assert_eq!(decoded.frames.len(), 1);
        assert!(!decoded.animated);
        assert_eq!(decoded.frames[0].pixels.width, 2);
        assert_eq!(decoded.frames[0].pixels.height, 3);
    }

    #[test]
    fn decodes_animated_gif_frames_and_delays() {
        let bytes = gif_with_frames(2);
        let decoded = decode(&bytes, MediaKind::Gif).unwrap();
        assert!(decoded.animated);
        assert_eq!(decoded.frames.len(), 2);
        assert_eq!(decoded.frames[0].delay_ms, 40);
    }

    #[test]
    fn oversized_gif_animation_falls_back_to_first_frame() {
        let bytes = gif_with_frames(MAX_GIF_FRAMES + 1);
        let decoded = decode(&bytes, MediaKind::Gif).unwrap();
        assert!(!decoded.animated);
        assert_eq!(decoded.frames.len(), 1);
    }

    fn gif_with_frames(count: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            let frames = (0..count).map(|index| {
                let pixel = if index % 2 == 0 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                image::Frame::from_parts(
                    image::RgbaImage::from_pixel(1, 1, image::Rgba(pixel)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(40, 1),
                )
            });
            encoder.encode_frames(frames).unwrap();
        }
        bytes
    }
}
