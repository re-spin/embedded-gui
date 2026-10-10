//! Feasibility spike: embedded-gui (default-features off + `libm`) rendering
//! two CDJ-2000NXS screens into a 480x234 X1RGB555 surface.
//!
//! embedded-gui renders `Rgb565`; the surface converts to 555 once per draw
//! call and takes glyph masks and fills natively (`RawTarget` + `Direct`).
#![no_std]

mod fonts;

use core::{cell::Cell, mem::MaybeUninit};

use embedded_graphics_core::pixelcolor::{Rgb555, Rgb565, IntoStorage};
use embedded_gui::{
    Border, CustomCanvas, CustomWidget, Direct, EdgeInsets, Mask, MaskFormat, RawImage, RawTarget, Rgb555Surface, FontId, GuiContext, GuiError,
    LinearGradient, WidgetFlags, QEase, QTween, Rect, SevenSegmentDisplay, Style, VisualState, WidgetId, WidgetStyle, encode_7seg,
};

pub const W: u32 = 480;
pub const H: u32 = 234;

/// X1RGB555 surface: the crate's `Rgb555Surface` (565 -> 555 at the target,
/// one conversion per draw call) rendered through the `Direct` compositor.
pub fn fb(px: &mut [u16]) -> Rgb555Surface<'_> {
    let mut s = Rgb555Surface::new(px, W, H);
    #[cfg(target_os = "none")]
    {
        s.fill_row = firmware::fill16;
        s.index_row = firmware::index_row;
        s.move_px = firmware::move_px;
        s.smear_line = firmware::smear_line;
        s.mask1_rows = firmware::mask1_rows;
    }
    s
}

// ── styles ───────────────────────────────────────────────────────────────────

const fn rgb(hex: u32) -> Rgb565 {
    embedded_gui::colors::from_hex24(hex)
}
const BLACK: Rgb565 = rgb(0x000000);
const WHITE: Rgb565 = rgb(0xffffff);
const ROW: FontId = FontId::Bitmap(&fonts::ROW);
const SMALL: FontId = FontId::Bitmap(&fonts::SMALL);
const PANE: FontId = FontId::Bitmap(&fonts::PANE);

const fn st(bg: Option<Rgb565>, text: Rgb565, font: FontId, pad: EdgeInsets) -> Style {
    Style { background: bg, text, font, padding: pad, ..Style::new() }
}
const fn pad(left: i16, top: i16) -> EdgeInsets {
    EdgeInsets::new(top, 0, 0, left)
}
const fn row_style(i: usize, selected: bool) -> Style {
    if selected {
        st(Some(rgb(0xf0f0f0)), BLACK, ROW, pad(26, 3))
    } else {
        st(Some(if i % 2 == 0 { rgb(0x3c3c3c) } else { rgb(0x2c2c2c) }), WHITE, ROW, pad(26, 3))
    }
}
fn header(text: Rgb565) -> Style {
    Style {
        gradient: Some(LinearGradient::horizontal(rgb(0x1890d0), rgb(0x082040))),
        ..st(Some(rgb(0x1060a0)), text, ROW, pad(4, 1))
    }
}
fn boxed(color: Rgb565) -> Style {
    Style { border: Border { color, width: 1 }, ..st(Some(BLACK), color, SMALL, pad(2, 1)) }
}

// ── custom widgets built from the crate's stock pieces ───────────────────────

/// Seven-segment digits (`SevenSegmentDisplay::digit_segments` + `encode_7seg`);
/// the crate has the geometry but no widget render for it.
pub struct Seg {
    pub text: Cell<&'static str>,
    pub color: Rgb565,
    pub thick: u8,
}

impl CustomWidget for Seg {
    fn render(&self, c: &mut dyn CustomCanvas, r: Rect, _: &WidgetStyle, _: VisualState) -> Result<(), GuiError> {
        let (dw, t) = (r.h / 2, self.thick as u32);
        let mut x = r.x;
        for ch in self.text.get().chars() {
            match ch {
                ':' | '.' => {
                    let y = if ch == ':' { r.y + r.h as i32 / 3 } else { r.bottom() - t as i32 };
                    c.fill_rect(Rect::new(x, y, t, t), self.color)?;
                    if ch == ':' {
                        c.fill_rect(Rect::new(x, r.y + 2 * r.h as i32 / 3, t, t), self.color)?;
                    }
                    x += 2 * t as i32;
                }
                _ => {
                    let bits = encode_7seg(ch);
                    for (i, s) in SevenSegmentDisplay::digit_segments(Rect::new(x, r.y, dw, r.h), self.thick).iter().enumerate() {
                        if bits & (1 << i) != 0 {
                            c.fill_rect(*s, self.color)?;
                        }
                    }
                    x += (dw + t + 2) as i32;
                }
            }
        }
        Ok(())
    }
}

// ponytail: the GUI is one task; the Cells are only touched from it.
unsafe impl Sync for Seg {}

/// Waveform stand-in: one 1-px bar per column from a fixed table.
pub struct Wave {
    pub color: Rgb565,
}

/// A scrolling detail waveform: column `x` shows sample `x + offset`, and
/// only columns inside the canvas clip are drawn.
pub struct ScrollWave {
    pub offset: Cell<i32>,
    pub color: Rgb565,
}
// ponytail: single-threaded bench; the Cell is only touched between renders.
unsafe impl Sync for ScrollWave {}

impl CustomWidget for ScrollWave {
    fn render(&self, c: &mut dyn CustomCanvas, r: Rect, _: &WidgetStyle, _: VisualState) -> Result<(), GuiError> {
        let clip = c.clip().intersection(r);
        let half = (r.h / 2) as i32;
        let mid = r.y + half;
        for x in clip.x..clip.right() {
            let n = (x - r.x + self.offset.get()) as u32;
            // Sample hash scaled to 0..half without a divide.
            let amp = ((n.wrapping_mul(2_654_435_761) >> 24) * half as u32 >> 8) as i32;
            c.fill_rect(Rect::new(x, mid - amp, 1, 2 * amp as u32 + 1), self.color)?;
        }
        Ok(())
    }
}
pub static DETAIL: ScrollWave = ScrollWave { offset: Cell::new(0), color: rgb(0x40a8e8) };

impl CustomWidget for Wave {
    fn render(&self, c: &mut dyn CustomCanvas, r: Rect, _: &WidgetStyle, _: VisualState) -> Result<(), GuiError> {
        // Integer only and no per-column division (BF531 has no divider): a
        // reciprocal per render, exact for these 8-bit values and half < 256.
        let half = r.h / 2;
        if half == 0 || half > 255 {
            return Ok(());
        }
        let inv = (1u32 << 16).div_ceil(half);
        let (mut seed, mut phase) = (0x1234_5678u32, 0);
        let mid = r.y + half as i32;
        for x in 0..r.w as i32 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = seed >> 24;
            let rem = v - ((v * inv) >> 16) * half; // v % half
            let amp = if phase < 3 { half - 1 } else { (rem * 0x5556) >> 16 }; // rem / 3
            phase = if phase == 21 { 0 } else { phase + 1 };
            c.fill_rect(Rect::new(r.x + x, mid - amp as i32, 1, 2 * amp + 1), self.color)?;
        }
        Ok(())
    }
}

/// Raw images, all through the target's blit hook: ARGB1555, indexed8 under
/// two palettes (theme recolour), tinted A4/A8 masks and an A1 mask.
pub struct Icons;

const IW: usize = 24;
/// Squared distance from the icon centre, in half pixels.
const fn d2(i: usize) -> usize {
    let (x, y) = ((i % IW) * 2 + 1, (i / IW) * 2 + 1);
    let (dx, dy) = (x.abs_diff(IW), y.abs_diff(IW));
    dx * dx + dy * dy
}
static ARGB: [u16; IW * IW] = {
    let mut a = [0u16; IW * IW];
    let mut i = 0;
    while i < a.len() {
        if d2(i) < 22 * 22 {
            a[i] = 0x8000 | ((i % IW) as u16 & 31) << 10 | ((i / IW) as u16 & 31) << 5 | 0x0f;
        }
        i += 1;
    }
    a
};
static IDX: [u8; IW * IW] = {
    let mut a = [0u8; IW * IW];
    let mut i = 0;
    while i < a.len() {
        a[i] = if d2(i) < 10 * 10 { 3 } else if d2(i) < 16 * 16 { 2 } else if d2(i) < 22 * 22 { 1 } else { 0 };
        i += 1;
    }
    a
};
static DAY: [u16; 4] = [0, 0x7fff, 0x03e0, 0x7c00];
static NIGHT: [u16; 4] = [0, 0x2108, 0x0010, 0x4000];
static A8: [u8; IW * IW] = {
    let mut a = [0u8; IW * IW];
    let mut i = 0;
    while i < a.len() {
        let d = d2(i);
        a[i] = if d >= 24 * 24 { 0 } else { (255 - d * 255 / (24 * 24)) as u8 };
        i += 1;
    }
    a
};
static A4: [u8; IW * IW / 2] = {
    let mut a = [0u8; IW * IW / 2];
    let mut i = 0;
    while i < a.len() {
        a[i] = (A8[2 * i] >> 4) << 4 | A8[2 * i + 1] >> 4;
        i += 1;
    }
    a
};
static A1: [u8; IW * IW / 8] = {
    let mut a = [0u8; IW * IW / 8];
    let mut i = 0;
    while i < IW * IW {
        if A8[i] > 96 && (i / IW + i % IW) % 3 != 0 {
            a[i / 8] |= 0x80 >> (i % 8);
        }
        i += 1;
    }
    a
};

/// A waveform-like 400x90 indexed8 picture, ~70% transparent (index 0).
const BW: usize = 400;
const BH: usize = 90;
static BIG: [u8; BW * BH] = {
    let mut a = [0u8; BW * BH];
    let mut i = 0;
    while i < a.len() {
        let (x, y) = (i % BW, i / BW);
        let h = (x * 7) % 28;
        if y.abs_diff(BH / 2) < h {
            a[i] = 1 + (y % 3) as u8;
        }
        i += 1;
    }
    a
};

/// `BIG` with 255 as the transparent index (the firmware's keyed asm row).
static BIG255: [u8; BW * BH] = {
    let mut a = BIG;
    let mut i = 0;
    while i < a.len() {
        if a[i] == 0 {
            a[i] = 255;
        }
        i += 1;
    }
    a
};

/// Blits a 400x90 indexed8 picture with transparent index `t`.
pub struct BigIndexed {
    pub px: &'static [u8; BW * BH],
    pub t: u8,
}
static BIG_T0: BigIndexed = BigIndexed { px: &BIG, t: 0 };
static BIG_T255: BigIndexed = BigIndexed { px: &BIG255, t: 255 };

impl CustomWidget for BigIndexed {
    fn render(&self, c: &mut dyn CustomCanvas, r: Rect, _: &WidgetStyle, _: VisualState) -> Result<(), GuiError> {
        let img = RawImage::Indexed8 { w: BW as u32, h: BH as u32, stride: BW, px: self.px, palette: &DAY, transparent: Some(self.t) };
        c.draw_raw_image(r.x, r.y, &img, BLACK)
    }
}

/// A glyph-like A4 mask: mostly empty, a solid core, a one-pixel AA edge.
static GLYPH4: [u8; IW * IW / 2] = {
    let mut a = [0u8; IW * IW / 2];
    let mut i = 0;
    while i < IW * IW {
        let d = d2(i);
        let v: u8 = if d < 14 * 14 { 15 } else if d < 18 * 18 { ((18 * 18 - d) * 15 / (18 * 18 - 14 * 14)) as u8 } else { 0 };
        a[i / 2] |= if i % 2 == 0 { v << 4 } else { v };
        i += 1;
    }
    a
};

/// 20 glyph-like A4 masks in a row of two lines (text-heavy cost).
pub struct A4Text;

impl CustomWidget for A4Text {
    fn render(&self, c: &mut dyn CustomCanvas, r: Rect, _: &WidgetStyle, _: VisualState) -> Result<(), GuiError> {
        let (w, h) = (IW as u32, IW as u32);
        let m = RawImage::Mask(Mask { w, h, stride: IW / 2, format: MaskFormat::A4, data: &GLYPH4 });
        for i in 0..20 {
            c.draw_raw_image(r.x + 22 * (i % 10), r.y + 26 * (i / 10), &m, rgb(0x202020))?;
        }
        Ok(())
    }
}

impl CustomWidget for Icons {
    fn render(&self, c: &mut dyn CustomCanvas, r: Rect, _: &WidgetStyle, _: VisualState) -> Result<(), GuiError> {
        let (w, h) = (IW as u32, IW as u32);
        let mask = |format, data, stride| RawImage::Mask(Mask { w, h, stride, format, data });
        let imgs = [
            (RawImage::Argb1555 { w, h, stride: w as usize, px: &ARGB }, BLACK),
            (RawImage::Indexed8 { w, h, stride: w as usize, px: &IDX, palette: &DAY, transparent: Some(0) }, BLACK),
            (RawImage::Indexed8 { w, h, stride: w as usize, px: &IDX, palette: &NIGHT, transparent: Some(0) }, BLACK),
            (mask(MaskFormat::A4, &A4[..], IW / 2), rgb(0xffa000)),
            (mask(MaskFormat::A8, &A8[..], IW), rgb(0x40c0ff)),
            (mask(MaskFormat::A1, &A1[..], IW / 8), WHITE),
        ];
        for (i, (img, tint)) in imgs.iter().enumerate() {
            c.draw_raw_image(r.x + 28 * i as i32, r.y, img, *tint)?;
        }
        Ok(())
    }
}

/// A 16x16 A1 glyph (a ring with a stem, ~40% ink), stride 2.
static GLYPH1: [u8; 32] = {
    let mut a = [0u8; 32];
    let mut i = 0;
    while i < 256 {
        let (x, y) = ((i % 16) as i32 - 7, (i / 16) as i32 - 7);
        let d = x * x + y * y;
        if (d >= 16 && d < 42) || (x >= 4 && x < 7) {
            a[i / 8] |= 0x80 >> (i % 8);
        }
        i += 1;
    }
    a
};

/// `n` A1 16x16 glyphs, one `draw_raw_image` each, in rows of 20 (respin-ui's
/// text path), clipped on three sides.
pub struct A1Text(pub i32);
static A1TEXT_40: A1Text = A1Text(40);
static A1TEXT_0: A1Text = A1Text(0);

impl CustomWidget for A1Text {
    fn render(&self, c: &mut dyn CustomCanvas, r: Rect, _: &WidgetStyle, _: VisualState) -> Result<(), GuiError> {
        let m = RawImage::Mask(Mask { w: 16, h: 16, stride: 2, format: MaskFormat::A1, data: &GLYPH1 });
        // Running pen, no `/ 20`: codegen_gcc calls ___divsi3 even for a
        // constant. The first row and column and the last column are cut by
        // the widget rect (columns skipped on the left, rows on top).
        let (mut x, mut y) = (r.x - 5, r.y - 3);
        for _ in 0..self.0 {
            c.draw_raw_image(x, y, &m, WHITE)?;
            x += 12;
            if x >= r.x + 235 {
                (x, y) = (r.x - 5, y + 18);
            }
        }
        Ok(())
    }
}

// ── screens ──────────────────────────────────────────────────────────────────

/// ~1.2 KB per node slot (eight per-node side tables): keep it static, not on the stack.
pub type Gui = GuiContext<'static, 32, 8, 8>;
pub const ROWS: usize = 8;
const ROW_H: u32 = 24;
const LIST_W: u32 = 300;
static ARTISTS: [&str; ROWS] =
    ["Aura Vortex", "Iglesias, Crazy Fluke", "Neelix", "Nhato", "Ozgur Can", "Paul van Dyk", "Ricky Ryan", "Sasha"];
static TRACKS: [&str; ROWS] =
    ["Reactivation", "Obey", "Spacebird", "Stromae", "Moments", "For an Angel", "Lost in Time", "Xpander"];

/// Browse list: header + 8 rows + right pane + footer. Returns the row ids.
pub fn browse(g: &mut Gui) -> Result<[WidgetId; ROWS], GuiError> {
    g.add_panel(Rect::new(0, 0, W, H), st(Some(BLACK), WHITE, SMALL, pad(0, 0)))?;
    g.add_label(Rect::new(0, 0, W, 20), "[ARTIST]", header(WHITE))?;
    let mut rows = [WidgetId(0); ROWS];
    for (i, row) in rows.iter_mut().enumerate() {
        let y = 20 + (i as u32 * ROW_H) as i32;
        *row = g.add_label(Rect::new(0, y, LIST_W, ROW_H), ARTISTS[i], row_style(i, i == 0))?;
    }
    g.add_panel(Rect::new(LIST_W as i32, 20, W - LIST_W, ROWS as u32 * ROW_H), st(Some(rgb(0xe8e8e8)), BLACK, PANE, pad(0, 0)))?;
    for (i, t) in TRACKS.iter().enumerate() {
        let y = 20 + (i as u32 * ROW_H) as i32;
        g.add_label(Rect::new(LIST_W as i32 + 4, y, W - LIST_W - 8, ROW_H - 1), t, st(Some(rgb(0xe8e8e8)), BLACK, PANE, pad(18, 5)))?;
    }
    g.add_label(Rect::new(0, 212, W, 22), "4 / 128   ARTIST", st(Some(rgb(0x101010)), rgb(0xa0a0a0), SMALL, pad(8, 6)))?;
    Ok(rows)
}

pub static TRACK: Seg = Seg { text: Cell::new("05"), color: WHITE, thick: 3 };
pub static TIME: Seg = Seg { text: Cell::new("06:57"), color: WHITE, thick: 3 };
pub static FRAMES: Seg = Seg { text: Cell::new("59.0"), color: WHITE, thick: 3 };
pub static TEMPO: Seg = Seg { text: Cell::new("0.00"), color: WHITE, thick: 2 };
pub static BPM: Seg = Seg { text: Cell::new("126.0"), color: WHITE, thick: 2 };
pub static PLAYER_NO: Seg = Seg { text: Cell::new("1"), color: rgb(0x909090), thick: 3 };
pub static WAVE: Wave = Wave { color: rgb(0x40a8e8) };
pub static OVERVIEW: Wave = Wave { color: rgb(0x7088b0) };

/// Player-like panel: title, waveform, big digits, labels, boxes. Returns the
/// frames-digits id.
pub fn player(g: &mut Gui) -> Result<WidgetId, GuiError> {
    let small = |c| st(None, c, SMALL, pad(0, 0));
    g.add_panel(Rect::new(0, 0, W, H), st(Some(BLACK), WHITE, SMALL, pad(0, 0)))?;
    g.add_label(Rect::new(0, 0, W, 20), "Obey (Iglesias Extended Mix)", header(WHITE))?;
    g.add_label(Rect::new(400, 2, 60, 16), "Dbm", st(None, WHITE, ROW, pad(0, 0)))?;
    g.add_custom_widget(Rect::new(40, 50, 400, 90), &WAVE, small(WHITE))?;
    g.add_label(Rect::new(2, 148, 36, 38), "PLAYER", boxed(rgb(0x909090)))?;
    g.add_custom_widget(Rect::new(14, 162, 20, 22), &PLAYER_NO, small(WHITE))?;
    g.add_label(Rect::new(48, 147, 40, 10), "TRACK", small(WHITE))?;
    g.add_custom_widget(Rect::new(48, 158, 40, 26), &TRACK, small(WHITE))?;
    g.add_label(Rect::new(93, 162, 40, 10), "REMAIN", small(WHITE))?;
    g.add_label(Rect::new(216, 148, 60, 10), "QUANTIZE", small(rgb(0xe02020)))?;
    g.add_custom_widget(Rect::new(140, 158, 140, 26), &TIME, small(WHITE))?;
    let frames = g.add_custom_widget(Rect::new(250, 158, 70, 26), &FRAMES, small(WHITE))?;
    g.add_label(Rect::new(325, 148, 40, 10), "TEMPO", small(WHITE))?;
    g.add_custom_widget(Rect::new(348, 164, 60, 20), &TEMPO, small(WHITE))?;
    g.add_label(Rect::new(413, 147, 64, 40), "", boxed(WHITE))?;
    g.add_custom_widget(Rect::new(418, 152, 56, 20), &BPM, small(WHITE))?;
    g.add_label(Rect::new(452, 176, 22, 10), "BPM", small(WHITE))?;
    g.add_custom_widget(Rect::new(40, 196, 400, 26), &OVERVIEW, small(WHITE))?;
    for (i, (t, bg)) in [("+-16", 0x404040), ("+-10", 0xe00000), ("+-6", 0x104010)].iter().enumerate() {
        g.add_label(Rect::new(444, 200 + 11 * i as i32, 34, 10), t, st(Some(rgb(*bg)), WHITE, SMALL, pad(4, 0)))?;
    }
    Ok(frames)
}

// ── scenario ─────────────────────────────────────────────────────────────────

/// What a mark covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Mark {
    /// building a screen (GuiContext::new + add_*)
    Build = 0,
    /// one full-frame render (the frame is dumped)
    Frame = 1,
    /// a render_dirty pass (the frame is dumped)
    Dirty = 2,
    /// a reference step with no embedded-gui involvement
    Ref = 3,
}

/// Steps, each followed by `mark`: browse build/full/one-row text/selection
/// move, then player build/full/clock digits, then a raw-fill reference.
/// `slot` is the context's static storage (it is too big for the stack).
pub fn run(px: &mut [u16], slot: &mut MaybeUninit<Gui>, mark: &mut impl FnMut(Mark, &[u16])) -> Result<(), GuiError> {
    let view = Rect::new(0, 0, W, H);
    let g = Gui::init_in_place(slot, view);
    let rows = browse(g)?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Frame, px);

    // One row's text changes (e.g. a name scrolling or updating).
    g.clear_dirty();
    g.set_text(rows[3], "Nhato (Live Edit)")?;
    dirty(g, px);
    mark(Mark::Dirty, px);

    // Selection moves 0 -> 1: two rows restyled.
    g.clear_dirty();
    g.set_widget_style(rows[0], row_style(0, false).into())?;
    g.set_widget_style(rows[1], row_style(1, true).into())?;
    dirty(g, px);
    mark(Mark::Dirty, px);

    // Integer tween: the selected row slides in from x=120 over 120 ms,
    // OutCubic, one render_dirty per 16 ms tick (8 frames, one mark).
    let tw = QTween::new(120, 0, 0, 120, QEase::OutCubic);
    let mut now = 0;
    loop {
        now += 16;
        g.clear_dirty();
        g.set_widget_rect(rows[1], Rect::new(tw.value(now), 20 + ROW_H as i32, LIST_W, ROW_H))?;
        dirty(g, px);
        if tw.done(now) {
            break;
        }
    }
    mark(Mark::Dirty, px);

    // ponytail: the old context is just overwritten; it owns nothing to drop.
    let g = Gui::init_in_place(slot, view);
    let frames = player(g)?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Frame, px);

    // Clock tick: the frames digits change (stock updates this at ~ 75 Hz).
    g.clear_dirty();
    FRAMES.text.set("58.9");
    g.mark_widget_dirty(frames)?;
    dirty(g, px);
    mark(Mark::Dirty, px);

    // Raw images appear over the waveform (one widget, six blits).
    g.clear_dirty();
    g.add_custom_widget(Rect::new(44, 60, 6 * 28, IW as u32), &Icons, st(None, WHITE, SMALL, pad(0, 0)))?;
    dirty(g, px);
    mark(Mark::Dirty, px);

    // Cost attribution. Raw fill of the whole screen, our code only:
    fb(px).fill_native(view, Rgb555::new(1, 2, 3).into_storage() as u32);
    mark(Mark::Ref, px);
    // One full-screen panel through embedded-gui:
    let g = Gui::init_in_place(slot, view);
    g.add_panel(view, st(Some(BLACK), WHITE, SMALL, pad(0, 0)))?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    // One 300x24 row label, 21 glyphs of the 9x18 font:
    let g = Gui::init_in_place(slot, view);
    g.add_label(Rect::new(0, 44, LIST_W, ROW_H), ARTISTS[1], row_style(1, false))?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    // The six raw images alone (no background): blit cost.
    let g = Gui::init_in_place(slot, view);
    g.add_custom_widget(Rect::new(44, 60, 6 * 28, IW as u32), &Icons, st(None, WHITE, SMALL, pad(0, 0)))?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    // 20 glyph-like A4 masks over a light pane (dark AA text on light).
    let g = Gui::init_in_place(slot, view);
    g.add_panel(Rect::new(40, 50, 240, 60), st(Some(rgb(0xe8e8e8)), BLACK, SMALL, pad(0, 0)))?;
    g.add_custom_widget(Rect::new(40, 50, 240, 60), &A4Text, st(None, WHITE, SMALL, pad(0, 0)))?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    // A 400x90 indexed8 picture, 70% transparent, alone: transparent index 0
    // (Rust row), then 255 (firmware keyed asm row on Blackfin).
    for big in [&BIG_T0, &BIG_T255] {
        let g = Gui::init_in_place(slot, view);
        g.add_custom_widget(Rect::new(40, 50, BW as u32, BH as u32), big, st(None, WHITE, SMALL, pad(0, 0)))?;
        mark(Mark::Build, px);
        full(g, px);
        mark(Mark::Ref, px);
    }
    // The waveform alone: 400 1-px column fill_rects through the canvas.
    let g = Gui::init_in_place(slot, view);
    g.add_custom_widget(Rect::new(40, 50, 400, 90), &WAVE, st(None, WHITE, SMALL, pad(0, 0)))?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    // Tree overhead: 32 widgets (root, 2 groups, 28 child labels, one 1x1
    // empty label). Damaging only the 1x1 label measures the per-pass walk.
    let g = Gui::init_in_place(slot, view);
    let root = g.add_panel(view, st(Some(BLACK), WHITE, SMALL, pad(0, 0)))?;
    for gx in [0, 240] {
        let group = g.add_panel(Rect::new(gx, 0, 240, H), st(None, WHITE, SMALL, pad(0, 0)))?;
        g.add_child(root, group)?;
        for i in 0..14 {
            let row = g.add_label(Rect::new(4, i * 16, 232, 16), TRACKS[i as usize % ROWS], st(None, WHITE, SMALL, pad(2, 2)))?;
            g.add_child(group, row)?;
        }
    }
    let dot = g.add_label(Rect::new(0, 0, 1, 1), "", st(None, WHITE, SMALL, pad(0, 0)))?;
    g.add_child(root, dot)?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    g.clear_dirty();
    g.mark_widget_dirty(dot)?;
    dirty(g, px);
    mark(Mark::Ref, px);
    // Detail waveform 480x90: full render, then a 3 px scroll through
    // scroll_rect (copy + redraw the 3 px strip).
    let g = Gui::init_in_place(slot, view);
    let detail = Rect::new(0, 60, W, 90);
    g.add_custom_widget(detail, &DETAIL, st(Some(BLACK), WHITE, SMALL, pad(0, 0)))?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    g.clear_dirty();
    DETAIL.offset.set(3);
    g.scroll_rect(&mut fb(px), detail, -3, 0)?;
    dirty(g, px);
    mark(Mark::Dirty, px);
    // Motion blur post-passes over the same 480x90, 8 taps.
    fb(px).smear_x(detail, 8);
    mark(Mark::Dirty, px);
    fb(px).smear_y(detail, 8);
    mark(Mark::Dirty, px);

    // Browse list as a ScrollView of 12 rows (no thumb), scrolled one row
    // by scroll_view_to: copy 7 rows, draw the one that scrolled in.
    let g = Gui::init_in_place(slot, view);
    let list = g.add_scroll_view(Rect::new(0, 20, LIST_W, ROWS as u32 * ROW_H), 0, 12 * ROW_H, st(Some(BLACK), WHITE, SMALL, pad(0, 0)))?;
    g.set_flag(list, WidgetFlags::NO_SCROLLBAR, true)?;
    for i in 0..12 {
        let row = g.add_label(Rect::new(0, (i * ROW_H) as i32, LIST_W, ROW_H), ARTISTS[i as usize % ROWS], row_style(i as usize, false))?;
        g.add_child(list, row)?;
    }
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    g.clear_dirty();
    g.scroll_view_to(&mut fb(px), list, ROW_H as i32)?;
    dirty(g, px);
    mark(Mark::Dirty, px);
    g.clear_dirty();
    g.set_scroll_offset(list, 2 * ROW_H as i32)?;
    dirty(g, px);
    mark(Mark::Dirty, px);

    // 40 A1 16x16 glyphs through draw_raw_image, then the same widget with
    // none: the difference / 40 is the cost per A1 glyph call.
    for n in [40, 0] {
        let g = Gui::init_in_place(slot, view);
        let w: &'static A1Text = if n == 0 { &A1TEXT_0 } else { &A1TEXT_40 };
        g.add_custom_widget(Rect::new(8, 50, 226, 36), w, st(None, WHITE, SMALL, pad(0, 0)))?;
        mark(Mark::Build, px);
        full(g, px);
        mark(if n == 0 { Mark::Ref } else { Mark::Frame }, px);
    }

    // The same label with no text: the difference / 21 is the cost per glyph.
    let g = Gui::init_in_place(slot, view);
    g.add_label(Rect::new(0, 44, LIST_W, ROW_H), "", row_style(1, false))?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    Ok(())
}

fn full(g: &mut Gui, px: &mut [u16]) {
    let _ = g.render_with::<_, Direct>(&mut fb(px));
}

fn dirty(g: &mut Gui, px: &mut [u16]) {
    let _ = g.render_dirty_with::<_, Direct>(&mut fb(px));
}

#[cfg(target_os = "none")]
mod firmware {
    use embedded_gui::{Mask, MaskFormat, Rect};

    unsafe extern "C" {
        fn spike_panic(file: *const u8, len: usize, line: u32) -> !;
        /// cdj-2000nxs-mods rust/gui/presentation/asm/copy.S: 32-bit stores in a
        /// hardware loop, the firmware's row fill.
        fn cdj_gui_fill16(dst: *mut u16, value: u16, count: usize) -> *mut u16;
        /// copy.S: dst[i] = table[src[i]], two pixels an iteration; with key
        /// nonzero, index 255 leaves dst[i] as it is.
        /// bfin/smear.S: one line of the box smear.
        fn spike_smear_line(a: *const SmearArgs);
        /// bfin/mask1.S: an A1 mask block up to 16 px wide.
        fn spike_mask1_rows(a: *const Mask1Args);
        /// copy.S: memmove; backwards only when dst lies inside the source.
        fn cdj_gui_memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8;
        fn cdj_gui_index_row(dst: *mut u16, src: *const u8, n: usize, table: *const u16, key: i32);
    }

    /// The firmware's indexed row for opaque pictures and transparent index
    /// 255; any other transparent index takes the crate's row.
    pub fn index_row(row: &mut [u16], src: &[u8], lut: &[u16; 256], transparent: Option<u8>) {
        let key = match transparent {
            None => 0,
            Some(255) => 1,
            Some(_) => return embedded_gui::render::raw::indexed_row(row, src, lut, transparent),
        };
        let n = row.len().min(src.len());
        unsafe { cdj_gui_index_row(row.as_mut_ptr(), src.as_ptr(), n, lut.as_ptr(), key) };
    }

    /// What bfin/mask1.S takes (see its header).
    #[repr(C)]
    struct Mask1Args {
        dst: *mut u16,
        dstep: usize,
        src: *const u8,
        sstep: usize,
        shift: u32,
        keep: u32,
        w: u32,
        h: u32,
        ink: u32,
        src2: *const u8,
    }

    /// The crate's mask1_rows in assembly for blocks up to 16 px wide (one
    /// or two mask bytes a row); anything else takes the crate's.
    pub fn mask1_rows(px: &mut [u16], at: usize, stride: usize, mask: &Mask<'_>, sub: Rect, ink: u16) {
        let fits = || {
            let (skip, top) = (u32::try_from(sub.x).ok()?, usize::try_from(sub.y).ok()?);
            let (w, h) = (sub.w as usize, sub.h as usize);
            let last = h.checked_sub(1)?;
            let src = top.checked_mul(mask.stride)?;
            let dst_end = last.checked_mul(stride)?.checked_add(at)?.checked_add(w)?;
            let two = usize::from(skip + sub.w > 8);
            let src_end = last.checked_mul(mask.stride)?.checked_add(src)?.checked_add(1 + two)?;
            (w != 0 && skip + sub.w <= 16 && mask.format == MaskFormat::A1 && dst_end <= px.len() && src_end <= mask.data.len())
                .then_some((skip, src, two))
        };
        let Some((skip, src, two)) = fits() else {
            return embedded_gui::render::raw::mask1_rows(px, at, stride, mask, sub, ink);
        };
        let a = Mask1Args {
            dst: unsafe { px.as_mut_ptr().add(at) },
            dstep: 2 * stride,
            src: unsafe { mask.data.as_ptr().add(src) },
            sstep: mask.stride,
            shift: 16 + skip,
            keep: u32::MAX << (32 - sub.w),
            w: sub.w,
            h: sub.h,
            ink: ink as u32,
            src2: unsafe { mask.data.as_ptr().add(src + two) },
        };
        unsafe { spike_mask1_rows(&a) };
    }

    /// What bfin/smear.S takes (see its header).
    #[repr(C)]
    struct SmearArgs {
        out: *mut u16,
        step: isize,
        ring: *const u32,
        ring_len: u32,
        neg_shift: i32,
        sum: u32,
        main: u32,
        tail: u32,
        end: u32,
        lead: *const u16,
    }

    /// The crate's smear_line with the per-pixel loops in assembly: this side
    /// checks the line once and fills the window's ring (taps entries).
    pub fn smear_line(px: &mut [u16], start: usize, step: usize, len: usize, taps: u32) {
        let shift = match taps {
            0 | 1 => return,
            2 | 3 => 1,
            4..=7 => 2,
            8..=15 => 3,
            _ => 4,
        };
        let (n, half) = (1usize << shift, 1usize << (shift - 1));
        let Some(last) = len.checked_sub(1) else { return };
        if len <= half || last.checked_mul(step).and_then(|o| o.checked_add(start)).is_none_or(|e| e >= px.len()) {
            return;
        }
        let spread = |p: u16| (p as u32 | (p as u32) << 16) & 0x03e0_7c1f;
        let at = |k: isize| px.get(start + (k.max(0) as usize).min(last) * step).copied().unwrap_or(0);
        // Slot i holds pixel i - half: it leaves the window at step i.
        let mut ring = [0u32; 16];
        let mut sum = 0;
        for (i, r) in ring.iter_mut().take(n).enumerate() {
            *r = spread(at(i as isize - half as isize));
            sum += *r;
        }
        let end = spread(at(last as isize));
        let out = unsafe { px.as_mut_ptr().add(start) };
        let a = SmearArgs {
            out,
            step: (2 * step) as isize,
            ring: ring.as_ptr(),
            ring_len: 4 * n as u32,
            neg_shift: -(shift as i32),
            sum,
            main: (len - half) as u32,
            tail: half as u32,
            end,
            lead: out.wrapping_add(half * step),
        };
        unsafe { spike_smear_line(&a) };
    }

    /// The crate's bounds check, then the firmware memmove (bytes).
    pub fn move_px(px: &mut [u16], src: usize, dst: usize, n: usize) {
        if src.max(dst).checked_add(n).is_some_and(|end| end <= px.len()) {
            unsafe { cdj_gui_memmove(px.as_mut_ptr().add(dst).cast(), px.as_ptr().add(src).cast(), 2 * n) };
        }
    }

    pub fn fill16(row: &mut [u16], v: u16) {
        unsafe { cdj_gui_fill16(row.as_mut_ptr(), v, row.len()) };
    }

    /// Reports file:line to the bench and exits the simulator (no fmt).
    #[panic_handler]
    fn panic(info: &core::panic::PanicInfo) -> ! {
        let (f, l) = info.location().map_or(("?", 0), |l| (l.file(), l.line()));
        unsafe { spike_panic(f.as_ptr(), f.len(), l) }
    }

    /// C entry for the simulator bench. Returns 0 on success.
    #[unsafe(no_mangle)]
    pub extern "C" fn spike_run(fb: *mut u16, mark: extern "C" fn(u32, *const u16)) -> u32 {
        let fb = unsafe { core::slice::from_raw_parts_mut(fb, (super::W * super::H) as usize) };
        static mut GUI: core::mem::MaybeUninit<super::Gui> = core::mem::MaybeUninit::uninit();
        let slot = unsafe { &mut *core::ptr::addr_of_mut!(GUI) };
        super::run(fb, slot, &mut |m, f| mark(m as u32, f.as_ptr())).is_err() as u32
    }
}
