//! Target-side fast paths: colour resolved once per draw call, glyph masks
//! written straight to the target's native pixels.
//!
//! The render path stays `Rgb565`. A target that implements [`RawTarget`] and
//! is rendered with the [`Direct`] compositor gets 1-bit glyph masks without a
//! per-span trip through `DrawTarget`. Every other target keeps
//! the [`Compositor`] defaults, which fall back to `fill_solid`.

use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Point, Size},
    pixelcolor::{IntoStorage, Rgb555, Rgb565, raw::RawU16},
    primitives::Rectangle,
};

use super::compositor::{BlendMode, Compositor, Dither};
use crate::geometry::Rect;

/// Coverage format of a [`Mask`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskFormat {
    /// 1 bit per pixel, MSB first.
    A1,
    /// 4 bits per pixel, high nibble first; 15 = opaque.
    A4,
    /// 8 bits per pixel; 255 = opaque.
    A8,
}

/// A coverage mask (glyph or single-colour icon), drawn in one colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mask<'a> {
    pub w: u32,
    pub h: u32,
    /// Bytes per row.
    pub stride: usize,
    pub format: MaskFormat,
    pub data: &'a [u8],
}

impl Mask<'_> {
    /// Coverage 0..=255 at `(col, row)`; 0 outside the data.
    #[inline]
    pub fn alpha(&self, col: u32, row: u32) -> u8 {
        let base = row as usize * self.stride;
        match self.format {
            MaskFormat::A1 => match self.data.get(base + (col >> 3) as usize) {
                Some(b) if b & (0x80 >> (col & 7)) != 0 => 255,
                _ => 0,
            },
            MaskFormat::A4 => self
                .data
                .get(base + (col >> 1) as usize)
                .map_or(0, |b| ((b >> (4 - 4 * (col & 1))) & 15) * 17),
            MaskFormat::A8 => self.data.get(base + col as usize).copied().unwrap_or(0),
        }
    }
}

/// A target with native-pixel hooks. `clip` arguments are already limited to
/// the pass (clip ∩ dirty) but not to the target: implementations bound-check
/// once per call.
pub trait RawTarget: DrawTarget<Color = Rgb565> {
    /// Native pixel for `color` (resolved once per draw call).
    fn native(&self, color: Rgb565) -> u32;

    /// Solid fill.
    fn fill_native(&mut self, rect: Rect, px: u32);

    /// Set bits of an A1 mask whose top-left is `(x, y)`, limited to `clip`.
    fn mask1(&mut self, x: i32, y: i32, mask: &Mask<'_>, clip: Rect, px: u32) {
        mask1_spans(x, y, mask, clip, |r| self.fill_native(r, px));
    }
}

/// Compositor for [`RawTarget`]s: fills, glyph masks and raw images go
/// straight to the target. Single pixels are plotted like [`Dither`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Direct;

impl<D: RawTarget> Compositor<D> for Direct {
    #[inline]
    fn plot(
        target: &mut D,
        x: i32,
        y: i32,
        color: Rgb565,
        opacity: u8,
        blend: BlendMode,
        backdrop: Rgb565,
    ) -> Result<(), D::Error> {
        <Dither as Compositor<D>>::plot(target, x, y, color, opacity, blend, backdrop)
    }

    #[inline]
    fn ink(target: &D, color: Rgb565) -> u32 {
        target.native(color)
    }

    #[inline]
    fn fill(target: &mut D, rect: Rect, ink: u32) -> Result<(), D::Error> {
        target.fill_native(rect, ink);
        Ok(())
    }

    #[inline]
    fn mask(
        target: &mut D,
        x: i32,
        y: i32,
        mask: &Mask<'_>,
        clip: Rect,
        ink: u32,
    ) -> Result<(), D::Error> {
        target.mask1(x, y, mask, clip, ink);
        Ok(())
    }
}

/// Calls `fill` with each run of set bits of an A1 mask inside `clip`.
pub fn mask1_spans(x: i32, y: i32, mask: &Mask<'_>, clip: Rect, mut fill: impl FnMut(Rect)) {
    let clip = clip.intersection(Rect::new(x, y, mask.w, mask.h));
    for py in clip.y..clip.bottom() {
        let row = (py - y) as u32;
        let mut run: Option<i32> = None;
        for px in clip.x..=clip.right() {
            let on = px < clip.right() && mask.alpha((px - x) as u32, row) != 0;
            match (on, run) {
                (true, None) => run = Some(px),
                (false, Some(s)) => {
                    fill(Rect::new(s, py, (px - s) as u32, 1));
                    run = None;
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn rectangle(r: Rect) -> Rectangle {
    Rectangle::new(Point::new(r.x, r.y), Size::new(r.w, r.h))
}

/// Native-pixel `Rgb565` for the default compositor's `ink` token.
#[inline]
pub(crate) fn rgb565_of(ink: u32) -> Rgb565 {
    Rgb565::from(RawU16::new(ink as u16))
}

/// An X1RGB555 surface (one `u16` per pixel, bit 15 ignored) that renders
/// the `Rgb565` path with the 565 → 555 conversion at the target. Row fills
/// go through `fill_row`, so firmware can plug in its hardware-loop fill.
pub struct Rgb555Surface<'a> {
    pub px: &'a mut [u16],
    pub w: u32,
    pub h: u32,
    /// Fills a row slice with one pixel value.
    pub fill_row: fn(&mut [u16], u16),
}

impl<'a> Rgb555Surface<'a> {
    pub fn new(px: &'a mut [u16], w: u32, h: u32) -> Self {
        Self {
            px,
            w,
            h,
            fill_row: |row, v| row.fill(v),
        }
    }

    #[inline]
    fn bounded(&self, r: Rect) -> Rect {
        r.intersection(Rect::new(0, 0, self.w, self.h))
    }

    /// Row `y`, columns `x0..x1` (in bounds by construction; empty if not).
    #[inline]
    fn row(&mut self, y: i32, x0: i32, x1: i32) -> &mut [u16] {
        let base = y as usize * self.w as usize;
        self.px
            .get_mut(base + x0 as usize..base + x1 as usize)
            .unwrap_or_default()
    }
}

#[inline]
fn to555(c: Rgb565) -> u16 {
    Rgb555::from(c).into_storage()
}

impl OriginDimensions for Rgb555Surface<'_> {
    fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }
}

impl DrawTarget for Rgb555Surface<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            if (p.x as u32) < self.w && (p.y as u32) < self.h {
                if let Some(px) = self
                    .px
                    .get_mut(p.y as usize * self.w as usize + p.x as usize)
                {
                    *px = to555(c);
                }
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Rgb565) -> Result<(), Self::Error> {
        let r = Rect::new(
            area.top_left.x,
            area.top_left.y,
            area.size.width,
            area.size.height,
        );
        self.fill_native(r, to555(color) as u32);
        Ok(())
    }
}

impl RawTarget for Rgb555Surface<'_> {
    #[inline]
    fn native(&self, color: Rgb565) -> u32 {
        to555(color) as u32
    }

    fn fill_native(&mut self, rect: Rect, px: u32) {
        let r = self.bounded(rect);
        let fill = self.fill_row;
        for y in r.y..r.bottom() {
            fill(self.row(y, r.x, r.right()), px as u16);
        }
    }

    fn mask1(&mut self, x: i32, y: i32, mask: &Mask<'_>, clip: Rect, px: u32) {
        let c = self.bounded(clip.intersection(Rect::new(x, y, mask.w, mask.h)));
        if c.is_empty() {
            return;
        }
        // Up to 32 px per row: the row's bits go in one word, MSB = column 0.
        if mask.w > 32 || mask.format != MaskFormat::A1 {
            return mask1_spans(x, y, mask, c, |r| self.fill_native(r, px));
        }
        // Columns outside the clip are masked off, so a row ends at its last set bit.
        let (skip, nbytes) = ((c.x - x) as u32, mask.w.div_ceil(8) as usize);
        let keep = u32::MAX << (32 - c.w); // 1 <= c.w <= 32
        for py in c.y..c.bottom() {
            let at = (py - y) as usize * mask.stride;
            let Some(bits) = mask.data.get(at..at + nbytes) else {
                return;
            };
            let mut word = bits
                .iter()
                .enumerate()
                .fold(0u32, |w, (i, b)| w | (*b as u32) << (24 - 8 * i));
            word = (word << skip) & keep;
            if word == 0 {
                continue;
            }
            for p in self.row(py, c.x, c.right()) {
                if (word as i32) < 0 {
                    *p = px as u16;
                }
                word <<= 1;
                if word == 0 {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{font::BitmapFont, render::RenderCtx};
    use std::vec;

    // 10x3 glyphs, 2 bytes per row: 'A' = stripes, 'B' = solid.
    static GLYPHS: [u8; 12] = [
        0b1010_1010,
        0b1100_0000,
        0b0111_1111,
        0b0100_0000,
        0,
        0,
        0xff,
        0xc0,
        0xff,
        0xc0,
        0xff,
        0xc0,
    ];
    static FONT: BitmapFont = BitmapFont {
        width: 10,
        height: 3,
        advance: 11,
        line_height: 4,
        first_char: b'A',
        bytes_per_row: 2,
        glyphs: &GLYPHS,
    };

    fn draw(direct: bool, clip: Rect) -> vec::Vec<u16> {
        let mut px = vec![0u16; 24 * 6];
        let mut s = Rgb555Surface::new(&mut px, 24, 6);
        let color = Rgb565::new(20, 40, 10);
        if direct {
            let mut ctx =
                RenderCtx::<_, Direct>::with_compositor(&mut s, Rect::new(0, 0, 24, 6), Some(clip));
            ctx.draw_text_with_font(-3, 1, "AB", color, &FONT).unwrap();
        } else {
            let mut ctx = RenderCtx::with_dirty(&mut s, Rect::new(0, 0, 24, 6), clip);
            ctx.draw_text_with_font(-3, 1, "AB", color, &FONT).unwrap();
        }
        px
    }

    #[test]
    fn direct_glyphs_match_fill_solid_path() {
        for clip in [
            Rect::new(0, 0, 24, 6),
            Rect::new(1, 2, 7, 3),
            Rect::new(5, 0, 13, 2),
            Rect::new(9, 1, 1, 1),
        ] {
            assert_eq!(draw(true, clip), draw(false, clip), "clip {clip:?}");
        }
        assert!(draw(true, Rect::new(0, 0, 24, 6)).iter().any(|p| *p != 0));
    }
}
