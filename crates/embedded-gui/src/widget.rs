use embedded_graphics_core::{draw_target::DrawTarget, pixelcolor::Rgb565};

use crate::{geometry::Rect, render::RenderCtx};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WidgetFlags(u16);

impl WidgetFlags {
    pub const HIDDEN: Self = Self(1 << 0);
    pub const DISABLED: Self = Self(1 << 1);
    pub const CLICKABLE: Self = Self(1 << 2);
    pub const SCROLLABLE: Self = Self(1 << 3);
    pub const FOCUSABLE: Self = Self(1 << 4);
    pub const CLIP_CHILDREN: Self = Self(1 << 5);
    pub const EVENT_BUBBLE: Self = Self(1 << 6);
    /// A Label's text ends at the last glyph that fits whole
    /// ([`TextOverflow::WholeGlyphs`](crate::render::TextOverflow::WholeGlyphs)).
    pub const WHOLE_GLYPHS: Self = Self(1 << 7);
    /// A ScrollView draws no scroll thumb.
    pub const NO_SCROLLBAR: Self = Self(1 << 8);
    /// The widget paints every pixel of its rect opaquely, so a render pass
    /// whose damage it covers starts at it and skips what it hides (the
    /// parent's background, earlier siblings).
    pub const OPAQUE: Self = Self(1 << 9);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    pub fn insert(&mut self, flag: Self) {
        self.0 |= flag.0;
    }

    pub fn remove(&mut self, flag: Self) {
        self.0 &= !flag.0;
    }

    pub fn set(&mut self, flag: Self, enabled: bool) {
        if enabled {
            self.insert(flag);
        } else {
            self.remove(flag);
        }
    }
}

impl core::ops::BitOr for WidgetFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for WidgetFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WidgetId(pub u16);

impl WidgetId {
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u16 {
        self.0
    }
}

pub trait StatefulWidget<State> {
    fn render_stateful<D>(
        &self,
        area: Rect,
        state: &mut State,
        ctx: &mut RenderCtx<'_, D>,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Rgb565>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventPhase {
    Capture,
    Target,
    Bubble,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventPolicy {
    Continue,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventContext {
    pub target: WidgetId,
    pub current: WidgetId,
    pub phase: EventPhase,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusGroupId(pub u8);

impl FocusGroupId {
    pub const ROOT: Self = Self(0);

    pub const fn new(raw: u8) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StyleClassId(pub u8);

impl StyleClassId {
    pub const NONE: Self = Self(0);

    pub const fn new(raw: u8) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuContract {
    pub wrap_navigation: bool,
    pub select_opens_dropdown: bool,
    pub back_closes_dropdown: bool,
    pub back_closes_notification_sheet: bool,
    pub select_toggles_feed_expanded: bool,
}

impl Default for MenuContract {
    fn default() -> Self {
        Self {
            wrap_navigation: true,
            select_opens_dropdown: true,
            back_closes_dropdown: true,
            back_closes_notification_sheet: true,
            select_toggles_feed_expanded: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyKey {
    Value,
    Min,
    Max,
    Text,
    Progress,
    Selected,
    State,
    Offset,
    Open,
    Expanded,
    Custom(u16),
}

#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue<'a> {
    Float(f32),
    Int(i32),
    Usize(usize),
    Bool(bool),
    Str(&'a str),
    Char(char),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyError {
    NotFound,
    ReadOnly,
    TypeMismatch,
}

pub trait Widget {
    fn render_widget_bounds(&self, _bounds: Rect, _style: &crate::style::Style) {}

    fn handle_widget_event(
        &mut self,
        _event: &crate::input::UiEvent,
        _ctx: &mut EventContext,
    ) -> EventPolicy {
        EventPolicy::Continue
    }

    fn get_property(&self, _key: PropertyKey) -> Option<PropertyValue<'_>> {
        None
    }

    fn set_property<'a>(
        &mut self,
        _key: PropertyKey,
        _val: PropertyValue<'a>,
    ) -> Result<(), PropertyError> {
        Err(PropertyError::NotFound)
    }
}

/// Abstract drawing canvas supplied to custom widgets during render.
pub trait CustomCanvas {
    fn fill_rect(
        &mut self,
        rect: Rect,
        color: embedded_graphics_core::pixelcolor::Rgb565,
    ) -> Result<(), crate::GuiError>;
    fn fill_rounded_rect(
        &mut self,
        rect: Rect,
        radius: u8,
        color: embedded_graphics_core::pixelcolor::Rgb565,
    ) -> Result<(), crate::GuiError>;
    fn draw_rect(
        &mut self,
        rect: Rect,
        color: embedded_graphics_core::pixelcolor::Rgb565,
    ) -> Result<(), crate::GuiError>;
    fn draw_line(
        &mut self,
        p1: (i32, i32),
        p2: (i32, i32),
        color: embedded_graphics_core::pixelcolor::Rgb565,
    ) -> Result<(), crate::GuiError>;
    fn draw_pixel(
        &mut self,
        x: i32,
        y: i32,
        color: embedded_graphics_core::pixelcolor::Rgb565,
    ) -> Result<(), crate::GuiError>;
    fn draw_text(
        &mut self,
        text: &str,
        x: i32,
        y: i32,
        color: embedded_graphics_core::pixelcolor::Rgb565,
        font: crate::font::FontId,
    ) -> Result<(), crate::GuiError>;
    /// The area drawing can reach in this pass: the clip (already narrowed to
    /// the widget's rect) ∩ the damage. A widget can skip work outside it.
    /// Canvases that don't track it report a rect covering everything.
    fn clip(&self) -> Rect {
        Rect::new(-(1 << 29), -(1 << 29), 1 << 30, 1 << 30)
    }
    /// `text` laid out in `rect` like a Label: alignment, wrapping, width-fitted
    /// ellipsis, clipped to `rect`. Canvases without layout draw it at the
    /// rect's top-left.
    fn draw_text_in(
        &mut self,
        rect: Rect,
        text: &str,
        style: crate::render::TextStyle,
    ) -> Result<(), crate::GuiError> {
        self.draw_text(text, rect.x, rect.y, style.color, style.font)
    }
    /// Raw image (see [`RawImage`](crate::render::RawImage)) at `(x, y)`;
    /// `tint` colours a mask. Canvases without raw support draw nothing.
    fn draw_raw_image(
        &mut self,
        _x: i32,
        _y: i32,
        _img: &crate::render::RawImage<'_>,
        _tint: embedded_graphics_core::pixelcolor::Rgb565,
    ) -> Result<(), crate::GuiError> {
        Ok(())
    }
    /// Masks (glyphs) with their top-left corners, all in `tint`: one call
    /// for a run of text, so clip and colour are worked out once.
    fn draw_masks(
        &mut self,
        masks: &[(i32, i32, crate::render::Mask<'_>)],
        tint: embedded_graphics_core::pixelcolor::Rgb565,
    ) -> Result<(), crate::GuiError> {
        for (x, y, m) in masks {
            self.draw_raw_image(*x, *y, &crate::render::RawImage::Mask(*m), tint)?;
        }
        Ok(())
    }
}

/// A safe `#![no_std]` extension point allowing user code to define
/// custom drawing and input handling for widgets.
pub trait CustomWidget {
    /// Draw the custom widget onto the provided canvas within `rect`.
    fn render(
        &self,
        canvas: &mut dyn CustomCanvas,
        rect: Rect,
        style: &crate::style::WidgetStyle,
        state: crate::style::VisualState,
    ) -> Result<(), crate::GuiError>;

    /// Handle UI events directed to this widget.
    fn handle_event(&self, _event: &crate::input::UiEvent, _ctx: &mut EventContext) -> EventPolicy {
        EventPolicy::Continue
    }

    /// Whether this custom widget can receive input focus.
    fn focusable(&self) -> bool {
        false
    }
}

/// A copyable fat-pointer wrapper around a `CustomWidget` trait object.
#[derive(Clone, Copy)]
pub struct CustomWidgetRef<'a> {
    pub widget: &'a (dyn CustomWidget + 'a),
}

impl<'a> CustomWidgetRef<'a> {
    pub const fn new(widget: &'a (dyn CustomWidget + 'a)) -> Self {
        Self { widget }
    }
}

impl<'a> PartialEq for CustomWidgetRef<'a> {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::addr_eq(
            self.widget as *const dyn CustomWidget as *const (),
            other.widget as *const dyn CustomWidget as *const (),
        )
    }
}

impl<'a> core::fmt::Debug for CustomWidgetRef<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CustomWidgetRef")
            .field(
                "addr",
                &(self.widget as *const dyn CustomWidget as *const ()),
            )
            .finish()
    }
}
