use crate::{
    geometry::Rect,
    render::RenderCtx,
    style::{VisualState, lerp_style},
    widget::WidgetId,
};
use embedded_graphics_core::pixelcolor::Rgb565;

use super::*;

impl<'a, const NODES: usize, const EVENTS: usize, const DIRTY: usize>
    GuiContext<'a, NODES, EVENTS, DIRTY>
{
    pub fn render<D>(&self, target: &mut D) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
    {
        let mut ctx = RenderCtx::new(target, self.viewport);
        ctx.set_quality(self.render_quality);
        self.render_into(&mut ctx, 0, 0, 255)
    }

    /// Like [`GuiContext::render`], but every widget's translucency is composited
    /// with true per-pixel alpha blending instead of ordered dithering. Requires
    /// a readback-capable target ([`PixelRead`](crate::PixelRead)), e.g. a
    /// [`Framebuffer`](crate::Framebuffer) used as a software back buffer.
    pub fn render_composited<D>(&self, target: &mut D) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>
            + crate::render::PixelRead,
    {
        let mut ctx = RenderCtx::compositing(target, self.viewport);
        ctx.set_quality(self.render_quality);
        self.render_into::<D, crate::render::Blend>(&mut ctx, 0, 0, 255)
    }

    pub fn render_dirty<D>(&self, target: &mut D) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
    {
        self.render_dirty_with::<D, crate::render::Dither>(target)
    }

    /// [`GuiContext::render`] with compositor `C`, e.g.
    /// [`Direct`](crate::render::Direct) for a [`RawTarget`](crate::render::RawTarget).
    pub fn render_with<D, C>(&self, target: &mut D) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
        C: crate::render::Compositor<D>,
    {
        let mut ctx = RenderCtx::<D, C>::with_compositor(target, self.viewport, None);
        ctx.set_quality(self.render_quality);
        self.render_into(&mut ctx, 0, 0, 255)
    }

    /// [`GuiContext::render_dirty`] with compositor `C`.
    pub fn render_dirty_with<D, C>(&self, target: &mut D) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
        C: crate::render::Compositor<D>,
    {
        let slice = self.dirty.as_slice();
        let pass = |target: &mut D, dirty: Rect| {
            let mut ctx = RenderCtx::<D, C>::with_compositor(target, self.viewport, Some(dirty));
            ctx.set_quality(self.render_quality);
            self.render_into(&mut ctx, 0, 0, 255)
        };
        match slice {
            [] => return Ok(()),
            [one] => return pass(target, *one),
            _ => {}
        }

        if let Some(bound) = self.dirty.bounding_rect() {
            let area_bound = bound.w as u64 * bound.h as u64;
            let sum_area: u64 = slice.iter().map(|r| r.w as u64 * r.h as u64).sum();
            // If bounding rect area is close to sum of individual areas (overlap or cluster),
            // render bounding rect in 1 pass to avoid multiple tree sweeps
            if area_bound <= sum_area + (sum_area / 2) {
                return pass(target, bound);
            }
        }

        for dirty in slice {
            pass(target, *dirty)?;
        }
        Ok(())
    }

    /// Scrolls the pixels of `rect` already on `target` by `(dx, dy)` and
    /// damages only the band that scrolled in, so the next `render_dirty`
    /// draws just that strip. Everything drawn in `rect` must move with it:
    /// damage any overlay on top yourself. Damage still pending inside `rect`
    /// moves along. If the target can't copy, all of `rect` is damaged.
    pub fn scroll_rect<D: crate::render::RawTarget>(
        &mut self,
        target: &mut D,
        rect: Rect,
        dx: i32,
        dy: i32,
    ) -> Result<(), GuiError> {
        let r = rect.intersection(self.viewport);
        if r.is_empty() || (dx == 0 && dy == 0) {
            return Ok(());
        }
        let shift = |s: Rect| Rect::new(s.x + dx, s.y + dy, s.w, s.h);
        let kept = r.intersection(shift(r));
        if kept.is_empty()
            || !target.copy_rect(Rect::new(kept.x - dx, kept.y - dy, kept.w, kept.h), dx, dy)
        {
            return self.damage(r);
        }
        // Pending damage was not drawn yet, so its stale pixels moved too.
        let pending =
            heapless::Vec::<Rect, DIRTY>::from_slice(self.dirty.as_slice()).unwrap_or_default();
        for d in &pending {
            self.damage(shift(d.intersection(r)).intersection(r))?;
        }
        let (w, h) = (dx.unsigned_abs(), dy.unsigned_abs());
        let x = if dx > 0 { r.x } else { r.right() - w as i32 };
        let y = if dy > 0 { r.y } else { r.bottom() - h as i32 };
        self.damage(Rect::new(r.x, y, r.w, h))?;
        self.damage(Rect::new(x, r.y, w, r.h))
    }

    #[cfg(feature = "scroll-view")]
    /// [`Self::set_scroll_offset`] that scrolls the view's pixels already on
    /// `target` (see [`Self::scroll_rect`]) instead of repainting the view, so
    /// only the rows that scrolled in (and the thumb) are drawn. The view's
    /// content must be opaque and not overlapped from outside it.
    pub fn scroll_view_to<D: crate::render::RawTarget>(
        &mut self,
        target: &mut D,
        id: WidgetId,
        offset_y: i32,
    ) -> Result<(), GuiError> {
        let node = self.node(id).ok_or(GuiError::NotFound)?;
        let crate::widgets::WidgetKind::ScrollView {
            offset_y: old,
            content_h,
        } = node.kind
        else {
            return Err(GuiError::NotFound);
        };
        let thumb = !node
            .flags
            .contains(crate::widget::WidgetFlags::NO_SCROLLBAR);
        let inner = crate::block::Block::styled(node.style.resolve(VisualState::Normal));
        let mut state = crate::state::ScrollState::new(old, content_h);
        state.set_offset(offset_y);
        let new = state.offset_y;
        if new == old {
            return Ok(());
        }
        let (rect, clip) = self.placement(id).ok_or(GuiError::NotFound)?;
        if let Some(crate::widgets::WidgetKind::ScrollView { offset_y: v, .. }) =
            self.node_mut(id).map(|n| &mut n.kind)
        {
            *v = new;
        }
        // Children clip to the whole view, so all of it scrolls; the border
        // and padding ring around the content stays, and is redrawn.
        let inner = inner.inner(rect);
        self.scroll_rect(target, rect.intersection(clip), 0, old - new)?;
        let ring = [
            Rect::new(rect.x, rect.y, rect.w, (inner.y - rect.y).max(0) as u32),
            Rect::new(
                rect.x,
                inner.bottom(),
                rect.w,
                (rect.bottom() - inner.bottom()).max(0) as u32,
            ),
            Rect::new(rect.x, rect.y, (inner.x - rect.x).max(0) as u32, rect.h),
            Rect::new(
                inner.right(),
                rect.y,
                (rect.right() - inner.right()).max(0) as u32,
                rect.h,
            ),
        ];
        for r in ring {
            self.damage(r.intersection(clip))?;
        }
        if thumb && content_h > rect.h {
            self.damage(Rect::new(inner.right() - 3, inner.y, 2, inner.h).intersection(clip))?;
        }
        Ok(())
    }

    /// Adds damage; if the tracker is full, falls back to the whole viewport.
    fn damage(&mut self, r: Rect) -> Result<(), GuiError> {
        if self.dirty.add(r).is_err() {
            self.dirty.mark_all(self.viewport)?;
        }
        Ok(())
    }

    /// Renders dirty regions to a hardware display controller implementing [`crate::render::WindowedDrawTarget`].
    /// Sets the physical controller's column/row window bounds (`set_window`) before rendering,
    /// enabling hardware SPI/DMA transfers exclusively to the dirty sub-window.
    pub fn render_dirty_windowed<D>(&self, target: &mut D) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>
            + crate::render::WindowedDrawTarget,
    {
        let slice = self.dirty.as_slice();
        if slice.is_empty() {
            return Ok(());
        }

        for dirty in slice {
            let eg_rect = embedded_graphics_core::primitives::Rectangle::new(
                embedded_graphics_core::geometry::Point::new(dirty.x, dirty.y),
                embedded_graphics_core::geometry::Size::new(dirty.w, dirty.h),
            );
            target.set_window(&eg_rect)?;
            let mut ctx = RenderCtx::with_dirty(target, self.viewport, *dirty);
            ctx.set_quality(self.render_quality);
            self.render_into(&mut ctx, 0, 0, 255)?;
        }
        Ok(())
    }

    /// Renders dirty regions into a small caller-provided scratch pixel buffer, then
    /// contiguously flushes each region to the display target.
    ///
    /// This eliminates visual flicker and dramatically speeds up partial updates on
    /// SPI/I2C displays without requiring a full RAM framebuffer.
    pub fn render_dirty_buffered<D>(
        &self,
        target: &mut D,
        scratch_buffer: &mut [Rgb565],
    ) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
    {
        let slice = self.dirty.as_slice();
        if slice.is_empty() || scratch_buffer.is_empty() {
            return Ok(());
        }

        for &dirty in slice {
            if dirty.is_empty() {
                continue;
            }
            let total_pixels = (dirty.w as usize).saturating_mul(dirty.h as usize);
            if total_pixels <= scratch_buffer.len() {
                // Entire dirty rect fits in the scratch buffer
                let mut fb =
                    crate::framebuffer::FramebufferSlice::new(scratch_buffer, dirty.w, dirty.h);
                let bg = self.theme.panel.background.unwrap_or(Rgb565::new(0, 0, 0));
                fb.clear_color(bg);
                let mut ctx = RenderCtx::new(&mut fb, Rect::new(0, 0, dirty.w, dirty.h));
                ctx.set_quality(self.render_quality);
                let _ = self.render_into(&mut ctx, -dirty.x, -dirty.y, 255);

                let area = embedded_graphics_core::primitives::Rectangle::new(
                    embedded_graphics_core::geometry::Point::new(dirty.x, dirty.y),
                    embedded_graphics_core::geometry::Size::new(dirty.w, dirty.h),
                );
                target.fill_contiguous(&area, scratch_buffer[..total_pixels].iter().copied())?;
            } else if scratch_buffer.len() >= dirty.w as usize {
                // Buffer can fit at least one row: render in vertical bands
                let band_height = (scratch_buffer.len() / dirty.w as usize) as u32;
                let mut y = dirty.y;
                let bottom = dirty.bottom();
                while y < bottom {
                    let h = band_height.min((bottom - y) as u32);
                    let band_pixels = (dirty.w * h) as usize;
                    let mut fb =
                        crate::framebuffer::FramebufferSlice::new(scratch_buffer, dirty.w, h);
                    let bg = self.theme.panel.background.unwrap_or(Rgb565::new(0, 0, 0));
                    fb.clear_color(bg);
                    let mut ctx = RenderCtx::new(&mut fb, Rect::new(0, 0, dirty.w, h));
                    ctx.set_quality(self.render_quality);
                    let _ = self.render_into(&mut ctx, -dirty.x, -y, 255);

                    let area = embedded_graphics_core::primitives::Rectangle::new(
                        embedded_graphics_core::geometry::Point::new(dirty.x, y),
                        embedded_graphics_core::geometry::Size::new(dirty.w, h),
                    );
                    target.fill_contiguous(&area, scratch_buffer[..band_pixels].iter().copied())?;
                    y += h as i32;
                }
            } else {
                // Micro-scratch buffer (smaller than 1 scanline): chunk sub-row rectangles
                let chunk_w = scratch_buffer.len() as u32;
                let mut y = dirty.y;
                let bottom = dirty.bottom();
                while y < bottom {
                    let mut x = dirty.x;
                    let right = dirty.right();
                    while x < right {
                        let w = chunk_w.min((right - x) as u32);
                        let chunk_pixels = w as usize;
                        let mut fb =
                            crate::framebuffer::FramebufferSlice::new(scratch_buffer, w, 1);
                        let bg = self.theme.panel.background.unwrap_or(Rgb565::new(0, 0, 0));
                        fb.clear_color(bg);
                        let mut ctx = RenderCtx::new(&mut fb, Rect::new(0, 0, w, 1));
                        ctx.set_quality(self.render_quality);
                        let _ = self.render_into(&mut ctx, -x, -y, 255);

                        let area = embedded_graphics_core::primitives::Rectangle::new(
                            embedded_graphics_core::geometry::Point::new(x, y),
                            embedded_graphics_core::geometry::Size::new(w, 1),
                        );
                        target.fill_contiguous(
                            &area,
                            scratch_buffer[..chunk_pixels].iter().copied(),
                        )?;
                        x += w as i32;
                    }
                    y += 1;
                }
            }
        }
        Ok(())
    }

    /// Renders only the dirty regions, using occlusion-aware damage tracking
    /// (after <https://bensimms.moe/reverse-engineering-scooter/>, footnote
    /// 7) instead of a flat dirty-rect list: a widget subtree that neither
    /// intersects the accumulated dirty regions nor anything already
    /// guaranteed to be freshly repainted is skipped entirely without being
    /// visited, and a container whose children fully, opaquely cover its
    /// dirty area is skipped from repainting its own background — see
    /// [`crate::damage::DamageTree`] for the exact rules.
    ///
    /// Falls back to [`Self::render_dirty`] (always correct, no occlusion
    /// pruning) if the internal quadtree's fixed capacity is exhausted by a
    /// pathological number of simultaneously-disjoint dirty regions in a
    /// single frame, rather than risk silently dropping one.
    pub fn render_dirty_occlusion<D>(&mut self, target: &mut D) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
    {
        const DEPTH: u8 = 3;
        const Q_NODES: usize = crate::quadtree::node_count_for_depth(DEPTH);
        const PER_NODE: usize = 4;

        let slice = self.dirty.as_slice();
        if slice.is_empty() {
            return Ok(());
        }

        let mut damage: crate::damage::DamageTree<Q_NODES, PER_NODE> =
            crate::damage::DamageTree::new(self.viewport, DEPTH);
        for &rect in slice {
            if damage.mark_dirty(rect).is_err() {
                return self.render_dirty(target);
            }
        }

        let mut ctx = RenderCtx::new(target, self.viewport);
        ctx.set_quality(self.render_quality);

        let root_ids: heapless::Vec<WidgetId, NODES> = self
            .widgets
            .iter()
            .filter(|node| node.parent.is_none())
            .map(|node| node.id)
            .collect();
        for root in root_ids {
            self.walk_occlusion(&mut ctx, &mut damage, root)?;
        }
        Ok(())
    }

    /// Post-order occlusion walk for one widget subtree: recurses into
    /// children first, then repaints `id` itself only if damage remains
    /// after accounting for whatever its children just covered. Returns
    /// whether anything in this subtree was repainted (currently unused by
    /// the caller, but useful for instrumentation/testing).
    fn walk_occlusion<D>(
        &self,
        ctx: &mut RenderCtx<'_, D, crate::render::Dither>,
        damage: &mut crate::damage::DamageTree<{ crate::quadtree::node_count_for_depth(3) }, 4>,
        id: WidgetId,
    ) -> Result<bool, D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
    {
        let Some(bounds) = self.absolute_rect(id) else {
            return Ok(false);
        };
        if bounds.is_empty() || !damage.overlaps_before(bounds) {
            return Ok(false);
        }

        let child_ids: heapless::Vec<WidgetId, NODES> = self
            .widgets
            .iter()
            .filter(|node| node.parent == Some(id))
            .map(|node| node.id)
            .collect();
        let mut child_painted = false;
        for child in child_ids {
            child_painted |= self.walk_occlusion(ctx, damage, child)?;
        }

        if !damage.overlaps_after(bounds) {
            return Ok(child_painted);
        }

        if let Some(is_opaque) = self.paint_single_node(ctx, id, bounds)? {
            if is_opaque {
                let _ = damage.mark_overdrawn(bounds);
            }
            return Ok(true);
        }
        Ok(child_painted)
    }

    /// Paints one node in isolation (visibility/clip-checked, style-resolved
    /// exactly like [`Self::render_into`]'s per-node body), returning
    /// `Some(is_opaque_cover)` if it was actually painted — `is_opaque_cover`
    /// is true when the resolved style fully, opaquely fills `bounds` (solid
    /// background, full opacity, square corners), the precondition for
    /// marking it overdrawn.
    fn paint_single_node<D>(
        &self,
        ctx: &mut RenderCtx<'_, D, crate::render::Dither>,
        id: WidgetId,
        bounds: Rect,
    ) -> Result<Option<bool>, D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
    {
        let (Some(node), Some((_, base_clip))) = (self.node(id), self.placement(id)) else {
            return Ok(None);
        };
        if bounds.intersection(base_clip).is_empty() {
            return Ok(None);
        }
        let old_clip = ctx.clip();
        ctx.set_clip(old_clip.intersection(base_clip));

        let state = if self.pressed.is_some_and(|pressed| pressed.id == id) {
            VisualState::Pressed
        } else if Some(id) == self.focus {
            VisualState::Focused
        } else if !self.effective_enabled(id) {
            VisualState::Disabled
        } else {
            VisualState::Normal
        };

        let owned = self.effective_style(node, state, 255);
        let style = owned.as_ref().unwrap_or(&node.style);
        let active_style = style.resolve(state);
        node.render_at_styled(ctx, bounds, state, style)?;
        ctx.set_clip(old_clip);

        let is_opaque_cover = active_style.background.is_some()
            && active_style.opacity == 255
            && active_style.corner_radius == 0;
        Ok(Some(is_opaque_cover))
    }

    pub fn render_with_offset<D>(
        &self,
        target: &mut D,
        offset_x: i32,
        offset_y: i32,
    ) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
    {
        self.render_with_offset_and_opacity(target, offset_x, offset_y, 255)
    }

    pub fn render_with_offset_and_opacity<D>(
        &self,
        target: &mut D,
        offset_x: i32,
        offset_y: i32,
        opacity: u8,
    ) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
    {
        let mut ctx = RenderCtx::new(target, self.viewport);
        ctx.set_quality(self.render_quality);
        self.render_into(&mut ctx, offset_x, offset_y, opacity)
    }

    pub fn render_with_offset_opacity_and_clip<D>(
        &self,
        target: &mut D,
        offset_x: i32,
        offset_y: i32,
        opacity: u8,
        clip: Rect,
    ) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
    {
        let mut ctx = RenderCtx::new(target, self.viewport);
        ctx.set_quality(self.render_quality);
        let old_clip = ctx.clip();
        ctx.set_clip(old_clip.intersection(clip));
        self.render_into(&mut ctx, offset_x, offset_y, opacity)
    }

    /// The style `node` renders with in `state`: `None` means its own (borrowed,
    /// no copy); a style is built only when a class, a state transition or the
    /// pass `opacity` changes it.
    fn effective_style(
        &self,
        node: &crate::widgets::WidgetNode<'a>,
        state: VisualState,
        opacity: u8,
    ) -> Option<crate::style::WidgetStyle> {
        let class_style = node.style_class.and_then(|class| {
            self.class_styles
                .iter()
                .find(|(id, _)| *id == class)
                .map(|(_, style)| style)
        });
        let transition = self.state_transition_progress(node.id);
        if class_style.is_none() && transition.is_none() && opacity == 255 {
            return None;
        }
        let resolve_state_style = |vs: VisualState| class_style.unwrap_or(&node.style).resolve(vs);
        let active_style = match transition {
            Some((from, to, t)) => {
                lerp_style(resolve_state_style(from), resolve_state_style(to), t)
            }
            None => resolve_state_style(state),
        };
        let mut style = node.style.with_state_override(state, active_style);
        if opacity < 255 {
            let apply = |v: u8| -> u8 { ((v as u16 * opacity as u16) / 255) as u8 };
            style.normal.opacity = apply(style.normal.opacity);
            style.focused.opacity = apply(style.focused.opacity);
            style.pressed.opacity = apply(style.pressed.opacity);
            style.disabled.opacity = apply(style.disabled.opacity);
        }
        Some(style)
    }

    pub(crate) fn render_into<D, C>(
        &self,
        ctx: &mut RenderCtx<'_, D, C>,
        offset_x: i32,
        offset_y: i32,
        opacity: u8,
    ) -> Result<(), D::Error>
    where
        D: embedded_graphics_core::draw_target::DrawTarget<Color = Rgb565>,
        C: crate::render::Compositor<D>,
    {
        for node in &self.widgets {
            let Some((base_rect, base_clip)) = self.placement(node.id) else {
                continue;
            };
            let rect = Rect::new(
                base_rect.x + offset_x,
                base_rect.y + offset_y,
                base_rect.w,
                base_rect.h,
            );
            let clip = Rect::new(
                base_clip.x + offset_x,
                base_clip.y + offset_y,
                base_clip.w,
                base_clip.h,
            );
            let visible = rect.intersection(clip);
            // Skip nodes outside this pass's damage before any style/text setup.
            if visible.is_empty()
                || ctx
                    .dirty_rect()
                    .is_some_and(|d| visible.intersection(d).is_empty())
            {
                continue;
            }
            let old_clip = ctx.clip();
            ctx.set_clip(old_clip.intersection(clip));
            let state = if self.pressed.is_some_and(|pressed| pressed.id == node.id) {
                VisualState::Pressed
            } else if Some(node.id) == self.focus {
                VisualState::Focused
            } else if !self.effective_enabled(node.id) {
                VisualState::Disabled
            } else {
                VisualState::Normal
            };
            let owned = self.effective_style(node, state, opacity);
            node.render_at_styled(ctx, rect, state, owned.as_ref().unwrap_or(&node.style))?;
            ctx.set_clip(old_clip);
        }
        Ok(())
    }
}
