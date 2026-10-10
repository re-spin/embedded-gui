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
    /// `stride` is pixels per row of `px` (`w`, or more for a padded sprite
    /// or a sub-rectangle of a sheet).
    Argb1555 {
        w: u32,
        h: u32,
        stride: usize,
        px: &'a [u16],
    },
    /// Opaque X1RGB555: rows are copied as they are, so keep bit 15 clear
    /// (the fallback ignores it). `stride` as for `Argb1555`.
    Rgb555 {
        w: u32,
        h: u32,
        stride: usize,
        px: &'a [u16],
    },
    /// One byte per pixel into `palette` (native pixels, so a theme can swap
    /// it); `transparent` is skipped. `stride` as for `Argb1555`. A palette
    /// of 256 or more entries is used as is, with no per-blit table copy, so
    /// its entries should have bit 15 clear; a shorter one is copied into a
    /// table with bit 15 cleared.
    Indexed8 {
        w: u32,
        h: u32,
        stride: usize,
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
            Self::Argb1555 { w, h, .. }
            | Self::Rgb555 { w, h, .. }
            | Self::Indexed8 { w, h, .. } => (w, h),
            Self::Mask(m) => (m.w, m.h),
        }
    }

    /// Source elements per row.
    pub fn stride(&self) -> usize {
        match *self {
            Self::Argb1555 { stride, .. }
            | Self::Rgb555 { stride, .. }
            | Self::Indexed8 { stride, .. } => stride,
            Self::Mask(m) => m.stride,
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

    /// Moves the pixels of `src` by `(dx, dy)` (overlap allowed), where both
    /// ends are on the target. `false` if the target can't, so the caller
    /// redraws instead.
    fn copy_rect(&mut self, src: Rect, dx: i32, dy: i32) -> bool {
        let _ = (src, dx, dy);
        false
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

/// Default [`Rgb555Surface::mask1_rows`].
pub fn mask1_rows(px: &mut [u16], at: usize, stride: usize, mask: &Mask<'_>, sub: Rect, ink: u16) {
    if sub.is_empty() || sub.x < 0 || sub.y < 0 || sub.w > 32 {
        return;
    }
    // Columns outside `sub` are masked off, so a row ends at its last set bit.
    let (skip, nbytes) = (sub.x as u32, mask.w.div_ceil(8).min(4) as usize);
    let keep = u32::MAX << (32 - sub.w); // 1 <= sub.w <= 32
    for (r, my) in (sub.y..sub.bottom()).enumerate() {
        let src = my as usize * mask.stride;
        let dst = at + r * stride;
        let (Some(bits), Some(row)) = (
            mask.data.get(src..src + nbytes),
            px.get_mut(dst..dst + sub.w as usize),
        ) else {
            return;
        };
        let word = bits
            .iter()
            .enumerate()
            .fold(0u32, |w, (i, b)| w | (*b as u32) << (24 - 8 * i));
        let mut word = word.checked_shl(skip).unwrap_or(0) & keep;
        for p in row {
            if word == 0 {
                break;
            }
            if (word as i32) < 0 {
                *p = ink;
            }
            word <<= 1;
        }
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
            let i = row as usize * img.stride() + col as usize;
            let c = match *img {
                RawImage::Argb1555 { px: p, .. } => {
                    p.get(i).filter(|p| *p & 0x8000 != 0).map(|p| from555(*p))
                }
                RawImage::Rgb555 { px: p, .. } => p.get(i).map(|p| from555(*p)),
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
    /// One indexed8 row: `dst[i] = lut[src[i]]`, leaving `dst[i]` where
    /// `src[i]` is the transparent index (default: [`indexed_row`]).
    pub index_row: fn(&mut [u16], &[u8], &[u16; 256], Option<u8>),
    /// memmove of `n` pixels from index `src` to `dst` (default: [`move_px`]).
    pub move_px: fn(&mut [u16], usize, usize, usize),
    /// Copies a same-length row into `dst`: [`RawImage::Rgb555`] rows
    /// (default: `copy_from_slice`).
    pub copy_px: fn(&mut [u16], &[u16]),
    /// One line of [`Self::smear_x`]/[`Self::smear_y`]: `(px, start, step,
    /// len, taps)` (default: [`smear_line`]).
    pub smear_line: fn(&mut [u16], usize, usize, usize, u32),
    /// The part `sub` (mask coordinates, inside the mask, at most 32 wide)
    /// of an A1 mask: `ink` where a bit is set, other pixels kept. `sub`'s
    /// top-left lands on `px[at]`, rows `stride` apart (default:
    /// [`mask1_rows`]).
    pub mask1_rows: fn(&mut [u16], usize, usize, &Mask<'_>, Rect, u16),
}

impl<'a> Rgb555Surface<'a> {
    pub fn new(px: &'a mut [u16], w: u32, h: u32) -> Self {
        Self {
            px,
            w,
            h,
            fill_row: |row, v| row.fill(v),
            index_row: indexed_row,
            move_px,
            copy_px: <[u16]>::copy_from_slice,
            smear_line,
            mask1_rows,
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

    /// Horizontal box blur inside `rect`, `taps` wide (rounded down to 2, 4,
    /// 8 or 16; below 2 does nothing), edges clamped. A motion-blur pass for
    /// horizontal slides: a running sum, one add and one subtract per pixel.
    pub fn smear_x(&mut self, rect: Rect, taps: u32) {
        let r = self.bounded(rect);
        let stride = self.w as usize;
        for y in r.y..r.bottom() {
            (self.smear_line)(
                self.px,
                y as usize * stride + r.x as usize,
                1,
                r.w as usize,
                taps,
            );
        }
    }

    /// [`Self::smear_x`] down the columns, for vertical scrolls and glides.
    pub fn smear_y(&mut self, rect: Rect, taps: u32) {
        let r = self.bounded(rect);
        let stride = self.w as usize;
        for x in r.x..r.right() {
            (self.smear_line)(
                self.px,
                r.y as usize * stride + x as usize,
                stride,
                r.h as usize,
                taps,
            );
        }
    }
}

/// Box blur of the `len` pixels at `start`, `start + step`, ... in place,
/// `taps` wide (rounded down to 2, 4, 8 or 16), edges repeated. A line no
/// longer than half the window is left as is.
/// The three 5-bit channels are summed side by side (see [`blend555`]) with
/// room for 32 pixels; a 16-entry ring keeps the originals the window still
/// needs after they are overwritten.
pub fn smear_line(px: &mut [u16], start: usize, step: usize, len: usize, taps: u32) {
    match taps {
        0 | 1 => {}
        2 | 3 => smear_n::<1>(px, start, step, len),
        4..=7 => smear_n::<2>(px, start, step, len),
        8..=15 => smear_n::<3>(px, start, step, len),
        _ => smear_n::<4>(px, start, step, len),
    }
}

/// [`smear_line`] over `1 << SHIFT` taps.
fn smear_n<const SHIFT: u32>(px: &mut [u16], start: usize, step: usize, len: usize) {
    const SPREAD: u32 = 0x03e0_7c1f;
    let spread = |p: u16| (p as u32 | (p as u32) << 16) & SPREAD;
    let shift = SHIFT;
    let Some(last) = len.checked_sub(1) else {
        return;
    };
    // One bounds check for the whole line, then pointer walks.
    if last
        .checked_mul(step)
        .and_then(|o| o.checked_add(start))
        .is_none_or(|end| end >= px.len())
    {
        return;
    }
    let half = 1usize << (shift - 1);
    if len <= half {
        // ponytail: a line no longer than half the window is left as is.
        return;
    }
    let pack = |sum: u32| {
        let m = (sum >> shift) & SPREAD;
        (m | m >> 16) as u16
    };
    // SAFETY: every pointer below is `base + start + k * step` with k <= last
    // (inside `px` by the check above), and only dereferenced then.
    unsafe {
        let p = px.as_mut_ptr().add(start);
        let at = |k: usize| *p.add(k.min(last) * step);
        let (first, end) = (spread(at(0)), spread(at(last)));
        // Window [k - half, k + half) with the edges repeated. Output k is
        // stored at k - half, whose original the window has just dropped, so
        // no copy of the line is needed; the line shifts back at the end.
        let mut sum = first * half as u32 + (0..half).map(|k| spread(at(k))).sum::<u32>();
        let mut head = [0u16; 8];
        // Only dereferenced inside the line (k + half < len), so wrapping.
        let mut lead = p.wrapping_add(2 * half * step);
        for (k, h) in head.iter_mut().take(half).enumerate() {
            *h = pack(sum);
            sum = sum + spread(at(k + half)) - first;
        }
        // k in half..len - half: lead and trail both inside the line.
        let mut trail = p;
        for _ in half..len - half {
            let t = spread(*trail);
            *trail = pack(sum);
            sum = sum + spread(*lead) - t;
            lead = lead.wrapping_add(step);
            trail = trail.add(step);
        }
        for _ in (len - half).max(half)..len {
            let t = spread(*trail);
            *trail = pack(sum);
            sum = sum + end - t;
            trail = trail.wrapping_add(step);
        }
        // Outputs half..len sit at 0..len - half: move them up, then the head.
        for k in (0..len - half).rev() {
            *p.add((k + half) * step) = *p.add(k * step);
        }
        for (k, h) in head.iter().take(half).enumerate() {
            *p.add(k * step) = *h;
        }
    }
}

#[inline]
fn to555(c: Rgb565) -> u16 {
    Rgb555::from(c).into_storage()
}

/// `px[dst..dst + n] = px[src..src + n]` with overlap allowed; does nothing
/// if either range is out of bounds.
pub fn move_px(px: &mut [u16], src: usize, dst: usize, n: usize) {
    if src
        .max(dst)
        .checked_add(n)
        .is_some_and(|end| end <= px.len())
    {
        // SAFETY: both ranges were just checked to lie inside `px`.
        unsafe { core::ptr::copy(px.as_ptr().add(src), px.as_mut_ptr().add(dst), n) }
    }
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

/// One indexed8 row through `lut`. With a transparent index, runs of it are
/// skipped four pixels at a time (one word compare). Kept out of `blit` so
/// the loop gets the registers.
#[inline(never)]
pub fn indexed_row(row: &mut [u16], src: &[u8], lut: &[u16; 256], transparent: Option<u8>) {
    let Some(t) = transparent else {
        for (d, s) in row.iter_mut().zip(src) {
            *d = lut[*s as usize];
        }
        return;
    };
    let one = |d: &mut u16, s: u8| {
        if s != t {
            *d = lut[s as usize];
        }
    };
    let n = row.len().min(src.len());
    // ponytail: short rows (24 px icons) don't repay the word loop's setup;
    // 32 is a guess between the bench's 24 px and 400 px cases, not tuned.
    if n < 32 {
        for (d, s) in row.iter_mut().zip(src) {
            let keep = ((*s != t) as u16).wrapping_neg();
            *d = (*d & !keep) | (lut[*s as usize] & keep);
        }
        return;
    }
    // Equal lengths, so the word-aligned middle and tail line up with the row.
    let (Some(row), Some(src)) = (row.get_mut(..n), src.get(..n)) else {
        return;
    };
    // Pixels up to the first 4-aligned source byte, then whole words (one
    // aligned load and compare per 4 px), then the tail. Explicit, because
    // align_to may legally return no middle at all (and does under codegen_gcc).
    let lead = ((src.as_ptr() as usize).wrapping_neg() & 3).min(n);
    let quads = (n - lead) / 4;
    let all_t = u32::from_ne_bytes([t; 4]);
    for (d, s) in row.iter_mut().zip(src).take(lead) {
        one(d, *s);
    }
    for q in 0..quads {
        let at = lead + 4 * q;
        // SAFETY: at + 4 <= n == src.len(), and src.as_ptr() + at is 4-aligned.
        let w = unsafe { src.as_ptr().add(at).cast::<u32>().read() };
        if w == all_t {
            continue;
        }
        if let Some([d0, d1, d2, d3]) = row.get_mut(at..at + 4) {
            let [s0, s1, s2, s3] = w.to_ne_bytes();
            one(d0, s0);
            one(d1, s1);
            one(d2, s2);
            one(d3, s3);
        }
    }
    let done = lead + 4 * quads;
    for (d, s) in row.iter_mut().zip(src).skip(done) {
        one(d, *s);
    }
}

/// One A4 mask row from column `sx` (high nibble first) tinted onto `row`.
/// A byte at a time: 0x00 skips two pixels, 0xFF stores two, anything else
/// blends its nibbles.
#[inline(never)]
fn a4_row(row: &mut [u16], src: &[u8], sx: usize, tint: u16) {
    let px = |d: &mut u16, a: u8| match a {
        0 => {}
        15 => *d = tint,
        a => *d = blend555(*d, tint, a as u32 * 17),
    };
    // An odd start column takes the low nibble of its byte alone. (Split by
    // value so the remainder is a plain reborrow: stable borrowck accepts it.)
    let row = match (sx & 1 == 1, row) {
        (true, [d, rest @ ..]) => {
            if let Some(b) = src.get(sx >> 1) {
                px(d, b & 15);
            }
            rest
        }
        (_, row) => row,
    };
    let bytes = src.get(sx.div_ceil(2)..).unwrap_or_default();
    let used = row.len() / 2;
    let mut pairs = row.chunks_exact_mut(2);
    for (d, b) in (&mut pairs).zip(bytes) {
        if let [d0, d1] = d {
            match *b {
                0x00 => {}
                0xff => {
                    *d0 = tint;
                    *d1 = tint;
                }
                b => {
                    px(d0, b >> 4);
                    px(d1, b & 15);
                }
            }
        }
    }
    // An odd pixel count ends on a high nibble.
    if let ([d], Some(b)) = (pairs.into_remainder(), bytes.get(used)) {
        px(d, b >> 4);
    }
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

    fn copy_rect(&mut self, src: Rect, dx: i32, dy: i32) -> bool {
        let s = self.bounded(src);
        let d = self.bounded(Rect::new(s.x + dx, s.y + dy, s.w, s.h));
        if d.is_empty() {
            return true;
        }
        let (stride, n, mv) = (self.w as usize, d.w as usize, self.move_px);
        let at = |x: i32, y: i32| y as usize * stride + x as usize;
        let px = &mut *self.px;
        let mut row = |y: i32| mv(px, at(d.x - dx, y - dy), at(d.x, y), n);
        // Moving down, go bottom-up so no source row is overwritten first.
        if dy > 0 {
            (d.y..d.bottom()).rev().for_each(&mut row);
        } else {
            (d.y..d.bottom()).for_each(&mut row);
        }
        true
    }

    fn fill_native(&mut self, rect: Rect, px: u32) {
        let r = self.bounded(rect);
        // ponytail: below 8 px a call to the row fill costs more than the stores.
        if r.w < 8 {
            // One bounds check for the area; each chunk starts at the rect's left edge.
            let (stride, n) = (self.w as usize, r.w as usize);
            let first = r.y as usize * stride + r.x as usize;
            let last = (r.bottom() as usize - 1) * stride + r.right() as usize;
            let Some(area) = self.px.get_mut(first..last) else {
                return;
            };
            let (h, ptr) = (r.h as usize, area.as_mut_ptr());
            for row in 0..h {
                for col in 0..n {
                    // SAFETY: area.len() == (h - 1) * stride + n, so every
                    // row * stride + col < area.len() (row < h, col < n).
                    unsafe { *ptr.add(row * stride + col) = px as u16 };
                }
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
            if let Some((head, tail)) = self.px.split_at_mut_checked(at) {
                if let (Some(src), Some(dst)) = (head.get(first..first + n), tail.get_mut(..n)) {
                    dst.copy_from_slice(src);
                }
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
        let at = c.y as usize * self.w as usize + c.x as usize;
        let sub = Rect::new(c.x - x, c.y - y, c.w, c.h);
        let mask1_rows = self.mask1_rows;
        mask1_rows(self.px, at, self.w as usize, mask, sub, px as u16);
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
                    let at = sy * img.stride() + sx;
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
            RawImage::Rgb555 { px, .. } => {
                for (py, sy) in rows {
                    let at = sy * img.stride() + sx;
                    let Some(src) = px.get(at..at + n) else {
                        return;
                    };
                    let copy_px = self.copy_px;
                    if let Some(d) = self.row(py, c.x, c.right()).get_mut(..n) {
                        copy_px(d, src);
                    }
                }
            }
            RawImage::Indexed8 {
                px,
                palette,
                transparent,
                ..
            } => {
                // A 256-entry table: no bounds check per pixel. A full
                // palette is the table itself, with no per-blit copy.
                let own;
                let lut: &[u16; 256] = match palette.first_chunk() {
                    Some(full) => full,
                    None => {
                        let mut t = [0u16; 256];
                        for (l, p) in t.iter_mut().zip(palette) {
                            *l = *p & 0x7fff;
                        }
                        own = t;
                        &own
                    }
                };
                for (py, sy) in rows {
                    let at = sy * img.stride() + sx;
                    let Some(src) = px.get(at..at + n) else {
                        return;
                    };
                    let index_row = self.index_row;
                    index_row(self.row(py, c.x, c.right()), src, &lut, transparent);
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
                        a4_row(self.row(py, c.x, c.right()), src, sx, tint as u16);
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
        let rgb = argb.map(|p| p & 0x7fff);
        let idx: [u8; 12] = core::array::from_fn(|i| (i % 4) as u8);
        let a1 = [0b1011_0000, 0b0110_0000, 0b1111_0000];
        let imgs = [
            // Columns 1..4 of 4-wide sheets: stride past the width.
            RawImage::Argb1555 {
                w: 3,
                h: 3,
                stride: 4,
                px: &argb[1..],
            },
            RawImage::Rgb555 {
                w: 3,
                h: 3,
                stride: 4,
                px: &rgb[1..],
            },
            RawImage::Indexed8 {
                w: 3,
                h: 3,
                stride: 4,
                px: &idx[1..],
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

    /// Proportional font: 'W' advances 6, others 2; kern(W, i) = -1; 'i' sits one
    /// row down. Every glyph is a 1x3 A1 bar.
    struct Prop;
    impl crate::font::Font for Prop {
        fn advance(&self) -> u32 {
            4
        }
        fn line_height(&self) -> u32 {
            4
        }
        fn draw_glyph(&self, _: char, _: &mut dyn FnMut(i32, i32)) {}
        fn glyph_mask(&self, _: char) -> Option<Mask<'static>> {
            Some(Mask {
                w: 1,
                h: 3,
                stride: 1,
                format: MaskFormat::A1,
                data: &[0x80; 3],
            })
        }
        fn glyph_advance(&self, ch: char) -> u32 {
            if ch == 'W' { 6 } else { 2 }
        }
        fn kern(&self, prev: char, next: char) -> i32 {
            if (prev, next) == ('W', 'i') { -1 } else { 0 }
        }
        fn glyph_offset(&self, ch: char) -> (i32, i32) {
            if ch == 'i' { (0, 1) } else { (0, 0) }
        }
    }
    static PROP: Prop = Prop;

    #[test]
    fn proportional_metrics_kerning_and_offsets() {
        use crate::font::FontId;
        let font = FontId::Dynamic(&PROP);
        let mut px = vec![0u16; 16 * 5];
        let mut s = Rgb555Surface::new(&mut px, 16, 5);
        let color = Rgb565::new(31, 63, 31);
        RenderCtx::<_, Direct>::with_compositor(&mut s, Rect::new(0, 0, 16, 5), None)
            .draw_text_with_font(0, 0, "Wi", color, font)
            .unwrap();
        let on: vec::Vec<(usize, usize)> = (0..16 * 5)
            .filter(|i| px[*i] != 0)
            .map(|i| (i % 16, i / 16))
            .collect();
        // 'W' at pen 0; 'i' at 0 + 6 - 1 = 5, one row down.
        assert_eq!(on, [(0, 0), (0, 1), (5, 1), (0, 2), (5, 2), (5, 3)]);
        type R<'a> = RenderCtx<'a, Rgb555Surface<'a>, Direct>;
        assert_eq!(R::text_metrics_with_font("Wi", font).width, 7);
        // Width-based wrapping: two W (12 px) fit in 13, the third wraps.
        let m =
            R::text_metrics_wrapped_with_font("WWW", 13, crate::render::TextWrap::Character, font);
        assert_eq!((m.width, m.height), (12, 4 + 1 + 4));
    }

    #[test]
    fn custom_widgets_clip_and_ellipsis_fits_width() {
        use crate::{
            font::FontId,
            render::{TextOverflow, TextStyle},
            widget::{CustomCanvas, CustomWidget},
        };
        // Overdraws its rect, then lays text out in a 17 px slot with an ellipsis.
        struct Row;
        impl CustomWidget for Row {
            fn render(
                &self,
                c: &mut dyn CustomCanvas,
                r: Rect,
                _: &crate::style::WidgetStyle,
                _: crate::style::VisualState,
            ) -> Result<(), crate::GuiError> {
                // The widget's own rect narrows the clip; the pass has no damage.
                assert_eq!(c.clip(), r);
                c.fill_rect(
                    Rect::new(r.x - 2, r.y + 4, r.w + 4, 1),
                    Rgb565::new(31, 0, 0),
                )?;
                let style = TextStyle::new(Rgb565::new(31, 63, 31))
                    .with_font(FontId::Dynamic(&PROP))
                    .with_overflow(TextOverflow::Ellipsis);
                c.draw_text_in(Rect::new(r.x, r.y, 17, 3), "WWWW", style)
            }
        }
        static ROW: Row = Row;
        let mut gui = crate::GuiContext::<'static, 4, 4, 4>::new(Rect::new(0, 0, 24, 6));
        gui.add_custom_widget(Rect::new(2, 0, 20, 5), &ROW, crate::style::Style::new())
            .unwrap();
        let mut px = vec![0u16; 24 * 6];
        gui.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 24, 6))
            .unwrap();
        // The overdrawn row stays inside x 2..22.
        let red: vec::Vec<usize> = (0..24).filter(|x| px[4 * 24 + x] != 0).collect();
        assert_eq!(red, (2..22).collect::<vec::Vec<_>>());
        // "WWWW" is 24 px; "..." is 6, so one W (6) fits the 11 px left: "W...".
        let text: vec::Vec<usize> = (0..24).filter(|x| px[*x] != 0).collect();
        assert_eq!(text, [2, 8, 10, 12]);
    }

    #[test]
    fn indexed_rows_match_fallback_at_every_alignment() {
        // 37 px rows with transparent runs, drawn at shifted clips so the word
        // loop starts at each source alignment.
        // Indices stay inside the palette (the fallback skips the others).
        let row = |n: usize| -> [u8; 37 * 3] {
            core::array::from_fn(|i| {
                if (i / 5) % 3 == 0 {
                    0
                } else {
                    (i * 7 % n) as u8
                }
            })
        };
        let (idx, wide) = (row(4), row(256));
        // Short (copied into a table) and full (used as is).
        let short = [0x7fff, 0x7c00, 0x03e0, 0x001f];
        let full: [u16; 256] = core::array::from_fn(|i| (i as u16).wrapping_mul(0x0421) & 0x7fff);
        for (transparent, palette, idx) in [
            (Some(0), &short[..], &idx),
            (None, &short[..], &idx),
            (Some(0), &full[..], &wide),
            (None, &full[..], &wide),
        ] {
            let img = RawImage::Indexed8 {
                w: 37,
                h: 3,
                stride: 37,
                px: idx,
                palette,
                transparent,
            };
            for x0 in 0..4 {
                let clip = Rect::new(x0, 0, 40 - x0 as u32, 3);
                let mut out = [vec![0x1234u16; 40 * 3], vec![0x1234u16; 40 * 3]];
                for (i, px) in out.iter_mut().enumerate() {
                    let mut s = Rgb555Surface::new(px, 40, 3);
                    let tint = Rgb565::new(0, 0, 0);
                    if i == 0 {
                        RenderCtx::<_, Direct>::with_compositor(&mut s, clip, None)
                            .draw_raw_image(1, 0, &img, tint)
                            .unwrap();
                    } else {
                        RenderCtx::with_dirty(&mut s, clip, clip)
                            .draw_raw_image(1, 0, &img, tint)
                            .unwrap();
                    }
                }
                assert_eq!(out[0], out[1], "{transparent:?} x0 {x0}");
            }
        }
    }

    #[test]
    fn a4_rows_match_per_pixel_reference() {
        // 21 px wide (odd), runs of 0x00/0xff and partials, clipped at every
        // left offset so rows start on both nibbles and all byte alignments.
        let data: [u8; 11 * 3] = core::array::from_fn(|i| match (i % 11, i % 7) {
            (10, _) => 0x8f, // the odd last pixel's byte: nibbles differ
            (_, 0..=3) => 0x00,
            (_, 4) => 0xff,
            (_, 5) => 0x8f,
            _ => 0x31,
        });
        let m = Mask {
            w: 21,
            h: 3,
            stride: 11,
            format: MaskFormat::A4,
            data: &data,
        };
        let tint = Rgb565::new(31, 0, 0);
        for x0 in 0..8 {
            let clip = Rect::new(x0, 0, 30 - x0 as u32, 3);
            let mut got = vec![0x1234u16; 30 * 3];
            let mut s = Rgb555Surface::new(&mut got, 30, 3);
            RenderCtx::<_, Direct>::with_compositor(&mut s, clip, None)
                .draw_raw_image(2, 0, &RawImage::Mask(m), tint)
                .unwrap();
            let mut want = vec![0x1234u16; 30 * 3];
            let t = to555(tint);
            for y in 0..3u32 {
                for x in 0..21u32 {
                    let (px, i) = (x as i32 + 2, (y * 30) as usize + x as usize + 2);
                    if px >= x0 {
                        want[i] = match m.alpha(x, y) {
                            0 => want[i],
                            255 => t,
                            a => blend555(want[i], t, a as u32),
                        };
                    }
                }
            }
            assert_eq!(got, want, "x0 {x0}");
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

    /// Fills 3 px past its rect on every side, so any missing clip shows.
    struct Solid;
    impl crate::widget::CustomWidget for Solid {
        fn render(
            &self,
            c: &mut dyn crate::widget::CustomCanvas,
            r: Rect,
            _: &crate::style::WidgetStyle,
            _: crate::style::VisualState,
        ) -> Result<(), crate::GuiError> {
            c.fill_rect(
                Rect::new(r.x - 3, r.y - 3, r.w + 6, r.h + 6),
                Rgb565::new(31, 0, 0),
            )
        }
    }
    static SOLID: Solid = Solid;
    const RED: u16 = 0x7c00;

    type Tree = crate::GuiContext<'static, 8, 4, 4>;

    /// A 40x20 black backdrop, so damage repaints stale pixels.
    fn tree() -> Tree {
        let mut g = Tree::new(Rect::new(0, 0, 40, 20));
        g.add_panel(
            Rect::new(0, 0, 40, 20),
            crate::style::Style::new().with_background(Some(Rgb565::new(0, 0, 0))),
        )
        .unwrap();
        g
    }

    fn red_rect(px: &[u16]) -> Option<Rect> {
        let pts: vec::Vec<(i32, i32)> = (0..20)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .filter(|(x, y)| px[(*y * 40 + *x) as usize] == RED)
            .collect();
        let (x0, y0) = pts
            .iter()
            .fold((99, 99), |a, p| (a.0.min(p.0), a.1.min(p.1)));
        let (x1, y1) = pts
            .iter()
            .fold((-1, -1), |a, p| (a.0.max(p.0), a.1.max(p.1)));
        // Solid, so the count must match the bounding box.
        let r = Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
        (!pts.is_empty() && pts.len() == (r.w * r.h) as usize).then_some(r)
    }

    #[test]
    fn custom_children_follow_parent_moves_with_damage() {
        let mut g = tree();
        let parent = g
            .add_panel(Rect::new(2, 2, 10, 10), crate::style::Style::new())
            .unwrap();
        let child = g
            .add_custom_widget(Rect::new(1, 1, 4, 4), &SOLID, crate::style::Style::new())
            .unwrap();
        g.add_child(parent, child).unwrap();
        let mut px = vec![0u16; 40 * 20];
        g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
            .unwrap();
        assert_eq!(red_rect(&px), Some(Rect::new(3, 3, 4, 4)));
        g.clear_dirty();
        g.set_widget_rect(parent, Rect::new(20, 5, 10, 10)).unwrap();
        g.render_dirty_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
            .unwrap();
        assert_eq!(red_rect(&px), Some(Rect::new(21, 6, 4, 4)));
    }

    #[test]
    fn clip_children_clips_custom_children() {
        let mut g = tree();
        // A child hanging off the parent's bottom-right corner.
        let parent = g
            .add_panel(Rect::new(2, 2, 10, 10), crate::style::Style::new())
            .unwrap();
        let child = g
            .add_custom_widget(Rect::new(6, 6, 8, 8), &SOLID, crate::style::Style::new())
            .unwrap();
        g.add_child(parent, child).unwrap();
        let mut px = vec![0u16; 40 * 20];
        g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
            .unwrap();
        assert_eq!(red_rect(&px), Some(Rect::new(8, 8, 4, 4)));
        // Without CLIP_CHILDREN the child keeps only its own rect.
        g.set_flag(parent, crate::widget::WidgetFlags::CLIP_CHILDREN, false)
            .unwrap();
        g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
            .unwrap();
        assert_eq!(red_rect(&px), Some(Rect::new(8, 8, 8, 8)));
        // Clipped again and moved wholly outside the parent: not drawn at all.
        g.set_flag(parent, crate::widget::WidgetFlags::CLIP_CHILDREN, true)
            .unwrap();
        g.set_widget_rect(child, Rect::new(20, 0, 4, 4)).unwrap();
        px.fill(0);
        g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
            .unwrap();
        assert_eq!(red_rect(&px), None);
    }

    #[cfg(feature = "scroll-view")]
    #[test]
    fn scroll_view_offsets_and_clips_custom_children() {
        let mut g = tree();
        let view = g
            .add_scroll_view(Rect::new(2, 2, 20, 10), 0, 40, crate::style::Style::new())
            .unwrap();
        let child = g
            .add_custom_widget(Rect::new(0, 4, 4, 4), &SOLID, crate::style::Style::new())
            .unwrap();
        g.add_child(view, child).unwrap();
        let mut px = vec![0u16; 40 * 20];
        g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
            .unwrap();
        assert_eq!(red_rect(&px), Some(Rect::new(2, 6, 4, 4)));
        // Scrolled 6 down: the child moves up 6 and is cut at the view top.
        g.clear_dirty();
        g.set_scroll_offset(view, 6).unwrap();
        g.render_dirty_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
            .unwrap();
        assert_eq!(red_rect(&px), Some(Rect::new(2, 2, 4, 2)));
    }

    #[test]
    fn scroll_rect_matches_full_redraw() {
        use core::cell::Cell;
        // Content scrolled by (ox, oy): each pixel encodes its content coordinate.
        struct Pan(Cell<(i32, i32)>);
        unsafe impl Sync for Pan {}
        impl crate::widget::CustomWidget for Pan {
            fn render(
                &self,
                c: &mut dyn crate::widget::CustomCanvas,
                r: Rect,
                _: &crate::style::WidgetStyle,
                _: crate::style::VisualState,
            ) -> Result<(), crate::GuiError> {
                let (ox, oy) = self.0.get();
                for y in r.y..r.bottom() {
                    for x in r.x..r.right() {
                        let (u, v) = ((x + ox) as u16, (y + oy) as u16);
                        c.fill_rect(
                            Rect::new(x, y, 1, 1),
                            Rgb565::new(u as u8 & 31, v as u8 & 63, (u ^ v) as u8 & 31),
                        )?;
                    }
                }
                Ok(())
            }
        }
        static PAN: Pan = Pan(Cell::new((0, 0)));
        let area = Rect::new(4, 3, 30, 12);
        let frame = |g: &Tree| {
            let mut px = vec![0u16; 40 * 20];
            g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
                .unwrap();
            px
        };
        for (dx, dy) in [(-3, 0), (3, 0), (0, -2), (0, 2), (-1, 1), (5, -4), (31, 0)] {
            PAN.0.set((0, 0));
            let mut g = tree();
            g.add_custom_widget(area, &PAN, crate::style::Style::new())
                .unwrap();
            let dot = g
                .add_label(Rect::new(10, 6, 3, 3), "", crate::style::Style::new())
                .unwrap();
            let mut px = frame(&g);
            // Pending damage whose pixels are stale: it must move with the scroll.
            g.clear_dirty();
            g.mark_widget_dirty(dot).unwrap();
            for y in 6..9 {
                px[y * 40 + 10..y * 40 + 13].fill(0x1234);
            }
            let mut s = Rgb555Surface::new(&mut px, 40, 20);
            g.scroll_rect(&mut s, area, dx, dy).unwrap();
            PAN.0.set((-dx, -dy));
            g.render_dirty_with::<_, Direct>(&mut s).unwrap();
            assert!(px == frame(&g), "dx {dx} dy {dy}");
        }
    }

    #[test]
    fn opaque_widget_hides_what_is_under_it() {
        use core::cell::Cell;
        // Counts its renders and fills its rect with one colour.
        struct Solid(Cell<u32>, Rgb565);
        unsafe impl Sync for Solid {}
        impl crate::widget::CustomWidget for Solid {
            fn render(
                &self,
                c: &mut dyn crate::widget::CustomCanvas,
                r: Rect,
                _: &crate::style::WidgetStyle,
                _: crate::style::VisualState,
            ) -> Result<(), crate::GuiError> {
                self.0.set(self.0.get() + 1);
                c.fill_rect(r, self.1)
            }
        }
        static BACK: Solid = Solid(Cell::new(0), Rgb565::new(31, 0, 0));
        static ROW: Solid = Solid(Cell::new(0), Rgb565::new(0, 63, 0));
        let mut g = tree();
        g.add_custom_widget(Rect::new(0, 0, 40, 20), &BACK, crate::style::Style::new())
            .unwrap();
        let row = g
            .add_custom_widget(Rect::new(4, 4, 30, 8), &ROW, crate::style::Style::new())
            .unwrap();
        g.set_flag(row, crate::widget::WidgetFlags::OPAQUE, true)
            .unwrap();
        let mut full = vec![0u16; 40 * 20];
        g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut full, 40, 20))
            .unwrap();
        // Damage inside the row: only the row paints, and the pixels match.
        for (damage, back_paints) in [
            (Rect::new(6, 5, 10, 3), 0),
            (Rect::new(4, 4, 30, 8), 0),
            (Rect::new(2, 5, 10, 3), 1),
        ] {
            let mut px = full.clone();
            px[5 * 40 + 6..5 * 40 + 12].fill(0x1234);
            px[5 * 40 + 2..5 * 40 + 4].fill(0x1234);
            BACK.0.set(0);
            ROW.0.set(0);
            g.clear_dirty();
            g.damage_rect(damage).unwrap();
            g.render_dirty_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
                .unwrap();
            assert_eq!((BACK.0.get(), ROW.0.get()), (back_paints, 1), "{damage:?}");
            if back_paints == 1 {
                assert!(px == full, "{damage:?}");
            } else {
                assert!(px[5 * 40 + 6..5 * 40 + 12] == full[5 * 40 + 6..5 * 40 + 12]);
            }
        }
    }

    #[test]
    fn damage_rect_repaints_only_that_area() {
        let red = crate::style::Style::new().with_background(Some(Rgb565::new(31, 0, 0)));
        let mut g = tree();
        g.add_label(Rect::new(2, 2, 20, 10), "", red).unwrap();
        g.add_label(Rect::new(26, 2, 10, 10), "", red).unwrap();
        let mut full = vec![0u16; 40 * 20];
        g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut full, 40, 20))
            .unwrap();
        g.clear_dirty();
        // Out of the viewport entirely: nothing to do.
        g.damage_rect(Rect::new(50, 0, 5, 5)).unwrap();
        assert!(g.dirty_regions().is_empty());
        let hit = Rect::new(8, 4, 6, 3);
        g.damage_rect(hit).unwrap();
        let mut px = vec![0x1234u16; 40 * 20];
        g.render_dirty_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
            .unwrap();
        for y in 0..20 {
            for x in 0..40 {
                let i = y * 40 + x;
                let want = if hit.contains(x as i32, y as i32) {
                    full[i]
                } else {
                    0x1234
                };
                assert_eq!(px[i], want, "({x}, {y})");
            }
        }
    }

    #[test]
    fn smear_matches_box_reference() {
        // Reference: per-channel box average over [k - n/2, k + n/2), clamped.
        fn reference(line: &[u16], n: usize) -> vec::Vec<u16> {
            let at = |i: isize| line[i.clamp(0, line.len() as isize - 1) as usize] as u32;
            (0..line.len() as isize)
                .map(|k| {
                    let h = n as isize / 2;
                    let ch = |sh: u32| {
                        ((k - h..k + h).map(|i| (at(i) >> sh) & 31).sum::<u32>() / n as u32) << sh
                    };
                    (ch(0) | ch(5) | ch(10)) as u16
                })
                .collect()
        }
        let (w, h) = (23usize, 19usize);
        let src: vec::Vec<u16> = (0..w * h)
            .map(|i| (i as u32).wrapping_mul(2_654_435_761) as u16 & 0x7fff)
            .collect();
        for (r, n) in [2, 4, 8, 16].into_iter().flat_map(|n| {
            [
                (Rect::new(2, 1, 19, 17), n),
                (Rect::new(5, 3, n / 2 + 1, n / 2 + 1), n),
                (Rect::new(1, 2, n + 1, 15), n),
            ]
        }) {
            let (rw, rh) = (r.w as usize, r.h as usize);
            let (mut a, mut b) = (src.clone(), src.clone());
            Rgb555Surface::new(&mut a, w as u32, h as u32).smear_x(r, n as u32);
            Rgb555Surface::new(&mut b, w as u32, h as u32).smear_y(r, n as u32);
            for y in 0..h {
                for x in 0..w {
                    let inside = r.contains(x as i32, y as i32);
                    let (rx, ry) = (r.x as usize, r.y as usize);
                    let (want_a, want_b) = if inside {
                        let row: vec::Vec<u16> = (rx..rx + rw).map(|x| src[y * w + x]).collect();
                        let col: vec::Vec<u16> = (ry..ry + rh).map(|y| src[y * w + x]).collect();
                        (
                            reference(&row, n as usize)[x - rx],
                            reference(&col, n as usize)[y - ry],
                        )
                    } else {
                        (src[y * w + x], src[y * w + x])
                    };
                    assert_eq!(a[y * w + x], want_a, "x n={n} ({x},{y})");
                    assert_eq!(b[y * w + x], want_b, "y n={n} ({x},{y})");
                }
            }
        }
        // taps < 2, an empty rect and lines up to half the window leave the
        // frame alone.
        let r = Rect::new(2, 1, 19, 17);
        let mut c = src.clone();
        let mut s = Rgb555Surface::new(&mut c, w as u32, h as u32);
        s.smear_x(r, 1);
        s.smear_y(Rect::new(30, 0, 5, 5), 8);
        s.smear_x(Rect::new(0, 0, 4, 4), 8);
        assert!(c == src);
    }

    #[test]
    fn whole_glyphs_drops_the_glyph_past_the_edge() {
        use crate::{font::FontId, widget::WidgetFlags};
        // 'W' is 6 px wide and lights only its first column: in 13 px, two
        // fit whole (12) and the third's first column (x 12) still shows.
        let lit = |whole: bool| {
            let mut g = crate::GuiContext::<'static, 4, 4, 4>::new(Rect::new(0, 0, 20, 5));
            let id = g
                .add_label(
                    Rect::new(0, 0, 13, 5),
                    "WWW",
                    crate::style::Style {
                        font: FontId::Dynamic(&PROP),
                        ..crate::style::Style::new()
                    },
                )
                .unwrap();
            g.set_flag(id, WidgetFlags::WHOLE_GLYPHS, whole).unwrap();
            let mut px = vec![0u16; 20 * 5];
            g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 20, 5))
                .unwrap();
            (0..20).filter(|x| px[*x] != 0).collect::<vec::Vec<_>>()
        };
        assert_eq!(lit(false), [0, 6, 12]);
        assert_eq!(lit(true), [0, 6]);
    }

    #[test]
    fn strided_blits_read_the_sub_rectangle() {
        // A 3x2 window at (1, 1) of a 5x4 sheet, against the same pixels packed.
        let sheet: [u8; 20] = core::array::from_fn(|i| i as u8);
        let packed = [6u8, 7, 8, 11, 12, 13];
        let pal: [u16; 20] = core::array::from_fn(|i| 0x0421 * i as u16);
        let argb: [u16; 20] = core::array::from_fn(|i| 0x8000 | 0x0421 * i as u16);
        let argb_packed: [u16; 6] = packed.map(|i| argb[i as usize]);
        let draw = |img: RawImage<'_>| {
            let mut px = vec![0u16; 6 * 4];
            Rgb555Surface::new(&mut px, 6, 4).blit(1, 1, &img, Rect::new(0, 0, 6, 4), 0);
            px
        };
        let ix = |px, stride| RawImage::Indexed8 {
            w: 3,
            h: 2,
            stride,
            px,
            palette: &pal,
            transparent: None,
        };
        assert_eq!(draw(ix(&sheet[6..], 5)), draw(ix(&packed, 3)));
        let ar = |px, stride| RawImage::Argb1555 {
            w: 3,
            h: 2,
            stride,
            px,
        };
        assert_eq!(draw(ar(&argb[6..], 5)), draw(ar(&argb_packed, 3)));
        assert!(
            draw(ar(&argb_packed, 3))
                .iter()
                .filter(|p| **p != 0)
                .count()
                == 6
        );
    }

    #[cfg(feature = "scroll-view")]
    #[test]
    fn scroll_view_to_matches_full_redraw() {
        use crate::{style::Style, widget::WidgetFlags};
        let build = |thumb: bool| {
            let mut g = tree();
            let mut view_style = Style::new().with_background(Some(Rgb565::new(0, 8, 0)));
            view_style.padding = crate::geometry::EdgeInsets::all(1);
            let view = g
                .add_scroll_view(Rect::new(4, 3, 22, 14), 0, 36, view_style)
                .unwrap();
            g.set_flag(view, WidgetFlags::NO_SCROLLBAR, !thumb).unwrap();
            for i in 0..6 {
                let bg = Rgb565::new(4 * i as u8, 60 - 9 * i as u8, 31 - 5 * i as u8);
                let row = g
                    .add_label(
                        Rect::new(1, 1 + 6 * i, 18, 6),
                        "",
                        Style::new().with_background(Some(bg)),
                    )
                    .unwrap();
                g.add_child(view, row).unwrap();
            }
            (g, view)
        };
        let frame = |g: &Tree| {
            let mut px = vec![0u16; 40 * 20];
            g.render_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
                .unwrap();
            px
        };
        for thumb in [false, true] {
            let (mut g, view) = build(thumb);
            let mut px = frame(&g);
            for to in [1, 4, 3, 20, 99, 0] {
                g.clear_dirty();
                g.scroll_view_to(&mut Rgb555Surface::new(&mut px, 40, 20), view, to)
                    .unwrap();
                g.render_dirty_with::<_, Direct>(&mut Rgb555Surface::new(&mut px, 40, 20))
                    .unwrap();
                assert!(px == frame(&g), "thumb {thumb} to {to}");
            }
        }
    }
}
