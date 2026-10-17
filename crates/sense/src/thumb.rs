//! Images shown in the chat, decoded ahead of time and scaled to the size
//! they're shown at. gpui decodes a plain image on its first frame and then
//! waits for a redraw; a transcript row may never get one, and a full-size
//! screenshot would sit in the GPU at full size for a 160-point thumbnail.

use std::sync::Arc;

use gpui_kit::{Pixels, RenderImage, Size, px, size};

/// The largest an image is shown, in points: in a message or the composer.
pub const MAX_SHOWN: Size<Pixels> = Size {
    width: px(240.),
    height: px(160.),
};

/// Pixels per point it's decoded at, for sharp thumbnails on Retina screens.
const DENSITY: u32 = 2;

#[derive(Clone)]
pub struct Thumb {
    pub image: Arc<RenderImage>,
    /// Its size on screen, in points, at most [`MAX_SHOWN`].
    pub shown: Size<Pixels>,
}

impl std::fmt::Debug for Thumb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumb").field("shown", &self.shown).finish()
    }
}

/// Decodes `bytes` (PNG, JPEG, GIF or WebP) into a thumbnail no larger than
/// [`MAX_SHOWN`]. Slow for large images: call it off the main thread.
pub fn thumbnail(bytes: &[u8]) -> Option<Thumb> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let max_w = f32::from(MAX_SHOWN.width) as u32 * DENSITY;
    let max_h = f32::from(MAX_SHOWN.height) as u32 * DENSITY;
    // Never scaled up: a small image stays its own size.
    let scaled = if decoded.width() > max_w || decoded.height() > max_h {
        decoded.thumbnail(max_w, max_h)
    } else {
        decoded
    };
    let mut pixels = scaled.into_rgba8();
    // gpui draws BGRA.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let shown = size(
        px(pixels.width() as f32 / DENSITY as f32),
        px(pixels.height() as f32 / DENSITY as f32),
    );
    Some(Thumb {
        image: Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])),
        shown,
    })
}

#[cfg(test)]
mod tests {
    use super::{MAX_SHOWN, thumbnail};
    use gpui_kit::px;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn a_screenshot_shrinks_to_fit_keeping_its_shape() {
        let thumb = thumbnail(&png(2000, 1300)).unwrap();
        assert_eq!(thumb.shown.width, MAX_SHOWN.width);
        // 2000×1300 at 240 wide is 156 tall.
        assert_eq!(thumb.shown.height, px(156.));
        // Decoded at twice that, and no more.
        let pixels = thumb.image.size(0);
        assert_eq!((pixels.width.0, pixels.height.0), (480, 312));
    }

    #[test]
    fn a_small_image_keeps_its_size() {
        let thumb = thumbnail(&png(64, 64)).unwrap();
        assert_eq!((thumb.shown.width, thumb.shown.height), (px(32.), px(32.)));
    }

    #[test]
    fn colors_come_out_in_the_order_gpui_draws() {
        let thumb = thumbnail(&png(4, 4)).unwrap();
        // Red, as BGRA: blue first.
        assert_eq!(&thumb.image.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn bytes_that_are_not_an_image_give_none() {
        assert!(thumbnail(b"not an image").is_none());
    }
}
