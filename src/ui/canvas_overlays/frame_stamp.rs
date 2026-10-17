//! The info box's fallback: a one-line frame stamp top-left, on a scrim.
//!
//! The info box is out (one-instrument §7): the frame's site, product, tilt,
//! time and age live in the top bar readout and age pill, and the caption
//! speaks whenever the frame and the playhead disagree. This overlay paints
//! only when [`crate::core::canvas::CANVAS_FALLBACKS`] turns the stamp back
//! on; the words are the core's [`crate::core::canvas::frame_stamp`].

use crate::ui::theme;
use eframe::egui::{self, Pos2, Stroke, StrokeKind, Vec2};

pub(super) struct FrameStampOverlay;

impl super::Overlay for FrameStampOverlay {
    fn z_order(&self) -> i32 {
        10
    }

    fn visible(&self, _ctx: &super::OverlayContext) -> bool {
        crate::core::canvas::CANVAS_FALLBACKS.frame_stamp
    }

    fn draw(&self, ui: &mut egui::Ui, ctx: &super::OverlayContext) -> Option<egui::Rect> {
        let state = ctx.state;
        let stamp = crate::core::canvas::frame_stamp(
            crate::core::canvas::CANVAS_FALLBACKS,
            &state.viz_state.site_id,
            state.viz_state.product,
            ctx.live.session.frame.as_ref(),
            state
                .viz_state
                .previous_displayed
                .as_ref()
                .map(|p| (p.start_time + p.end_time) / 2.0),
            ctx.derived.effective_sweep_animation,
            &crate::ui::time_format::ZoneClock {
                use_local: state.use_local_time,
            },
        )?;
        let painter = ui.painter();
        let font = theme::readout_font();
        let line = painter.layout_no_wrap(stamp.line, font.clone(), theme::SEEN);
        let previous = stamp
            .previous
            .map(|p| painter.layout_no_wrap(p, font, theme::TEXT_DIM));
        let pad = Vec2::new(8.0, 4.0);
        let width = line
            .size()
            .x
            .max(previous.as_ref().map_or(0.0, |p| p.size().x));
        let height = line.size().y + previous.as_ref().map_or(0.0, |p| p.size().y + 2.0);
        let origin = ctx.rect.left_top() + Vec2::new(10.0, 10.0);
        let scrim = egui::Rect::from_min_size(origin, Vec2::new(width, height) + pad * 2.0);
        painter.rect(
            scrim,
            4.0,
            theme::map::scrim(),
            Stroke::new(1.0_f32, theme::map::scrim_edge()),
            StrokeKind::Inside,
        );
        let mut at: Pos2 = origin + pad;
        let line_h = line.size().y;
        painter.galley(at, line, theme::SEEN);
        if let Some(p) = previous {
            at.y += line_h + 2.0;
            painter.galley(at, p, theme::TEXT_DIM);
        }
        None
    }
}
