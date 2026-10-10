//! Feasibility spike: embedded-gui (default-features off + `libm`) rendering
//! two CDJ-2000NXS screens into a 480x234 X1RGB555 surface.
//!
//! embedded-gui renders `Rgb565` only, so the surface is wrapped in the
//! crate's own `ColorConvertedDrawTarget` (565 -> 555 per call / per pixel).
#![no_std]

mod fonts;

use core::{cell::Cell, mem::MaybeUninit};

use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{Dimensions, OriginDimensions, Size},
    pixelcolor::{IntoStorage, Rgb555, Rgb565},
    primitives::Rectangle,
};
use embedded_gui::{
    Border, CustomCanvas, CustomWidget, DrawTargetColorExt, EdgeInsets, FontId, GuiContext, GuiError,
    LinearGradient, Rect, SevenSegmentDisplay, Style, VisualState, WidgetId, WidgetStyle, encode_7seg,
};

pub const W: u32 = 480;
pub const H: u32 = 234;

/// X1RGB555 surface. embedded-graphics `Rgb555` storage is that layout bit for
/// bit, so a pixel is one u16 store. Size is a parameter (800x480 later).
pub struct Fb<'a> {
    pub px: &'a mut [u16],
    pub w: u32,
    pub h: u32,
}

impl OriginDimensions for Fb<'_> {
    fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }
}

impl DrawTarget for Fb<'_> {
    type Color = Rgb555;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb555>>>(&mut self, pixels: I) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            if (p.x as u32) < self.w && (p.y as u32) < self.h {
                if let Some(px) = self.px.get_mut(p.y as usize * self.w as usize + p.x as usize) {
                    *px = c.into_storage();
                }
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Rgb555) -> Result<(), Self::Error> {
        let a = area.intersection(&self.bounding_box());
        let Some(br) = a.bottom_right() else { return Ok(()) };
        let (x0, x1, w) = (a.top_left.x as usize, br.x as usize + 1, self.w as usize);
        for y in a.top_left.y as usize..=br.y as usize {
            if let Some(row) = self.px.get_mut(y * w + x0..y * w + x1) {
                row.fill(color.into_storage());
            }
        }
        Ok(())
    }
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

impl CustomWidget for Wave {
    fn render(&self, c: &mut dyn CustomCanvas, r: Rect, _: &WidgetStyle, _: VisualState) -> Result<(), GuiError> {
        let mut seed = 0x1234_5678u32;
        let mid = r.y + r.h as i32 / 2;
        for x in 0..r.w as i32 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let amp = (seed >> 24) % (r.h / 2);
            let amp = if x % 22 < 3 { r.h / 2 - 1 } else { amp / 3 };
            c.fill_rect(Rect::new(r.x + x, mid - amp as i32, 1, 2 * amp + 1), self.color)?;
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
    let g = slot.write(Gui::new(view));
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

    // ponytail: the old context is just overwritten; it owns nothing to drop.
    let g = slot.write(Gui::new(view));
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

    // Cost attribution. Raw fill of the whole screen, our code only:
    Fb { px, w: W, h: H }.fill_solid(&Rectangle::new(Default::default(), Size::new(W, H)), Rgb555::new(1, 2, 3)).ok();
    mark(Mark::Ref, px);
    // One full-screen panel through embedded-gui:
    let g = slot.write(Gui::new(view));
    g.add_panel(view, st(Some(BLACK), WHITE, SMALL, pad(0, 0)))?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    // One 300x24 row label, 21 glyphs of the 9x18 font:
    let g = slot.write(Gui::new(view));
    g.add_label(Rect::new(0, 44, LIST_W, ROW_H), ARTISTS[1], row_style(1, false))?;
    mark(Mark::Build, px);
    full(g, px);
    mark(Mark::Ref, px);
    Ok(())
}

fn full(g: &mut Gui, px: &mut [u16]) {
    let mut fb = Fb { px, w: W, h: H };
    let _ = g.render(&mut fb.color_converted::<Rgb565>());
}

fn dirty(g: &mut Gui, px: &mut [u16]) {
    let mut fb = Fb { px, w: W, h: H };
    let _ = g.render_dirty(&mut fb.color_converted::<Rgb565>());
}

#[cfg(target_os = "none")]
mod firmware {
    unsafe extern "C" {
        fn spike_panic(file: *const u8, len: usize, line: u32) -> !;
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
