//! Target-side fast paths: colour resolved once per draw call, glyph masks and
//! raw images written straight to the target's native pixels.
//!
//! The render path stays `Rgb565`. A target that implements [`RawTarget`] and
//! is rendered with the [`Direct`] compositor gets solid fills, 1-bit glyph
//! masks and raw images (ARGB1555, indexed8, tinted A4/A8/A1 masks) without
//! a per-pixel or per-span trip through `DrawTarget`. Every other target keeps
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

/// A raw image in a target-native format, drawn only through
/// [`RawTarget::blit`] (never through `Rgb565` scratch or layer buffers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawImage<'a> {
    /// ARGB1555: opaque pixels (bit 15 set) are copied, transparent skipped.
    Argb1555 { w: u32, h: u32, px: &'a [u16] },
    /// One byte per pixel into `palette` (native pixels, so a theme can swap
    /// it); `transparent` is skipped.
    Indexed8 {
        w: u32,
        h: u32,
        px: &'a [u8],
        palette: &'a [u16],
        transparent: Option<u8>,
    },
    /// A coverage mask tinted with one colour, blended into the target.
    Mask(Mask<'a>),
}

impl RawImage<'_> {
    pub fn size(&self) -> (u32, u32) {
        match *self {
            Self::Argb1555 { w, h, .. } | Self::Indexed8 { w, h, .. } => (w, h),
            Self::Mask(m) => (m.w, m.h),
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

    /// `row` repeated on every line of `rect` (`row.len() == rect.w`).
    fn repeat_row(&mut self, rect: Rect, row: &[Rgb565]) {
        for (x, c) in (rect.x..rect.right()).zip(row) {
            let px = self.native(*c);
            self.fill_native(Rect::new(x, rect.y, 1, rect.h), px);
        }
    }

    /// Raw image at `(x, y)` limited to `clip`; `tint` is the native colour
    /// of a [`RawImage::Mask`].
    fn blit(&mut self, x: i32, y: i32, img: &RawImage<'_>, clip: Rect, tint: u32);
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
        if mask.format == MaskFormat::A1 {
            target.mask1(x, y, mask, clip, ink);
        } else {
            target.blit(x, y, &RawImage::Mask(*mask), clip, ink);
        }
        Ok(())
    }

    #[inline]
    fn repeat_row(target: &mut D, rect: Rect, row: &[Rgb565]) -> Result<(), D::Error> {
        target.repeat_row(rect, row);
        Ok(())
    }

    #[inline]
    fn blit(
        target: &mut D,
        x: i32,
        y: i32,
        img: &RawImage<'_>,
        clip: Rect,
        ink: u32,
    ) -> Result<(), D::Error> {
        target.blit(x, y, img, clip, ink);
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

/// Default (any `DrawTarget`) blit: decoded per pixel to `Rgb565`. Masks
/// below full coverage are dithered; ARGB1555 and palette entries are read
/// as X1RGB555.
pub(crate) fn blit_fallback<D: DrawTarget<Color = Rgb565>>(
    target: &mut D,
    x: i32,
    y: i32,
    img: &RawImage<'_>,
    clip: Rect,
    tint: Rgb565,
) -> Result<(), D::Error> {
    let (w, h) = img.size();
    let clip = clip.intersection(Rect::new(x, y, w, h));
    let from555 = |p: u16| Rgb565::from(Rgb555::from(RawU16::new(p & 0x7fff)));
    for py in clip.y..clip.bottom() {
        let row = (py - y) as u32;
        for px in clip.x..clip.right() {
            let col = (px - x) as u32;
            let i = (row * w + col) as usize;
            let c = match *img {
                RawImage::Argb1555 { px: p, .. } => {
                    p.get(i).filter(|p| *p & 0x8000 != 0).map(|p| from555(*p))
                }
                RawImage::Indexed8 {
                    px: p,
                    palette,
                    transparent,
                    ..
                } => p
                    .get(i)
                    .filter(|ix| Some(**ix) != transparent)
                    .and_then(|ix| palette.get(*ix as usize))
                    .map(|p| from555(*p)),
                RawImage::Mask(m) => {
                    let a = m.alpha(col, row);
                    (a != 0 && super::compositor::should_draw_at_opacity(px, py, a)).then_some(tint)
                }
            };
            if let Some(c) = c {
                target.draw_iter([Pixel(Point::new(px, py), c)])?;
            }
        }
    }
    Ok(())
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

/// `dst` 555 blended toward `src` 555 by coverage `a` (0..=255).
#[inline]
pub fn blend555(dst: u16, src: u16, a: u32) -> u16 {
    // Green moves to the high half so each channel has 5 bits of headroom for
    // a 5-bit alpha (33 levels, the channels' own precision).
    const SPREAD: u32 = 0x03e0_7c1f;
    let a = (a + 4) >> 3;
    let (d, s) = (dst as u32, src as u32);
    let (d, s) = ((d | d << 16) & SPREAD, (s | s << 16) & SPREAD);
    let m = ((d * (32 - a) + s * a) >> 5) & SPREAD;
    (m | m >> 16) as u16 & 0x7fff
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
        // ponytail: below 8 px a call to the row fill costs more than the stores.
        if r.w < 8 {
            for y in r.y..r.bottom() {
                self.row(y, r.x, r.right()).fill(px as u16);
            }
            return;
        }
        let fill = self.fill_row;
        for y in r.y..r.bottom() {
            fill(self.row(y, r.x, r.right()), px as u16);
        }
    }

    fn repeat_row(&mut self, rect: Rect, row: &[Rgb565]) {
        let r = self.bounded(rect);
        if r.is_empty() {
            return;
        }
        // Convert the first line in place, then copy it down.
        let skip = (r.x - rect.x) as usize;
        let (stride, x0, n) = (self.w as usize, r.x as usize, r.w as usize);
        let first = r.y as usize * stride + x0;
        for (d, c) in self
            .row(r.y, r.x, r.right())
            .iter_mut()
            .zip(row.iter().skip(skip))
        {
            *d = to555(*c);
        }
        for y in r.y + 1..r.bottom() {
            let at = y as usize * stride + x0;
            if at + n <= self.px.len() {
                self.px.copy_within(first..first + n, at);
            }
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

    fn blit(&mut self, x: i32, y: i32, img: &RawImage<'_>, clip: Rect, tint: u32) {
        let (w, h) = img.size();
        let c = self.bounded(clip.intersection(Rect::new(x, y, w, h)));
        if c.is_empty() {
            return;
        }
        let (sx, n) = ((c.x - x) as usize, c.w as usize);
        let rows = (c.y..c.bottom()).map(|py| (py, (py - y) as usize));
        match *img {
            RawImage::Argb1555 { px, .. } => {
                for (py, sy) in rows {
                    let at = sy * w as usize + sx;
                    let Some(src) = px.get(at..at + n) else {
                        return;
                    };
                    for (d, s) in self.row(py, c.x, c.right()).iter_mut().zip(src) {
                        // Branchless: all ones when opaque.
                        let keep = ((*s as i16) >> 15) as u16;
                        *d = (*d & !keep) | (*s & keep & 0x7fff);
                    }
                }
            }
            RawImage::Indexed8 {
                px,
                palette,
                transparent,
                ..
            } => {
                // A 256-entry table: no bounds check per pixel, and the
                // transparent index maps to "keep the destination".
                let mut lut = [0u16; 256];
                for (l, p) in lut.iter_mut().zip(palette) {
                    *l = *p & 0x7fff;
                }
                let t = transparent.map_or(256, |t| t as usize);
                for (py, sy) in rows {
                    let at = sy * w as usize + sx;
                    let Some(src) = px.get(at..at + n) else {
                        return;
                    };
                    for (d, s) in self.row(py, c.x, c.right()).iter_mut().zip(src) {
                        let keep = ((*s as usize != t) as u16).wrapping_neg();
                        *d = (*d & !keep) | (lut[*s as usize] & keep);
                    }
                }
            }
            RawImage::Mask(m) if m.format == MaskFormat::A1 => self.mask1(x, y, &m, c, tint),
            RawImage::Mask(m) => {
                let put = |d: &mut u16, a: u32| match a {
                    0 => {}
                    255 => *d = tint as u16,
                    a => *d = blend555(*d, tint as u16, a),
                };
                for (py, sy) in rows {
                    let line = sy * m.stride;
                    if m.format == MaskFormat::A8 {
                        let Some(src) = m.data.get(line + sx..line + sx + n) else {
                            return;
                        };
                        for (d, a) in self.row(py, c.x, c.right()).iter_mut().zip(src) {
                            put(d, *a as u32);
                        }
                    } else {
                        let Some(src) = m.data.get(line..line + m.stride) else {
                            return;
                        };
                        for (i, d) in self.row(py, c.x, c.right()).iter_mut().enumerate() {
                            let col = sx + i;
                            let b = src.get(col >> 1).copied().unwrap_or(0) as u32;
                            put(d, (b >> (4 - 4 * (col as u32 & 1)) & 15) * 17);
                        }
                    }
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
    fn direct_blits_match_fallback() {
        let argb: [u16; 12] = core::array::from_fn(|i| {
            if i % 3 == 0 {
                0
            } else {
                0x8000 | (i as u16 * 0x0421)
            }
        });
        let idx: [u8; 12] = core::array::from_fn(|i| (i % 4) as u8);
        let a1 = [0b1011_0000, 0b0110_0000, 0b1111_0000];
        let imgs = [
            RawImage::Argb1555 {
                w: 4,
                h: 3,
                px: &argb,
            },
            RawImage::Indexed8 {
                w: 4,
                h: 3,
                px: &idx,
                palette: &[0x7fff, 0x7c00, 0x03e0, 0x001f],
                transparent: Some(2),
            },
            RawImage::Mask(Mask {
                w: 4,
                h: 3,
                stride: 1,
                format: MaskFormat::A1,
                data: &a1,
            }),
        ];
        for img in imgs {
            for clip in [Rect::new(0, 0, 8, 5), Rect::new(3, 2, 2, 2)] {
                let mut out = [vec![0x1234u16; 40], vec![0x1234u16; 40]];
                for (i, px) in out.iter_mut().enumerate() {
                    let mut s = Rgb555Surface::new(px, 8, 5);
                    let tint = Rgb565::new(31, 0, 31);
                    if i == 0 {
                        RenderCtx::<_, Direct>::with_compositor(&mut s, clip, None)
                            .draw_raw_image(2, 1, &img, tint)
                            .unwrap();
                    } else {
                        RenderCtx::with_dirty(&mut s, clip, clip)
                            .draw_raw_image(2, 1, &img, tint)
                            .unwrap();
                    }
                }
                assert_eq!(out[0], out[1], "{img:?} {clip:?}");
            }
        }
    }

    #[test]
    fn blend555_channels() {
        let (white, red, blue) = (0x7fff, 0x7c00, 0x001f);
        assert_eq!(blend555(0, white, 0), 0);
        assert_eq!(blend555(0, white, 255), white);
        assert_eq!(blend555(red, blue, 128), 0x3c0f); // half of each, no bleed into green
        assert_eq!(blend555(blue, red, 255), red);
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
