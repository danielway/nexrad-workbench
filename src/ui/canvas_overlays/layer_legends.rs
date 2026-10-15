//! Legends for the colored layers, bottom-right on scrims (one-instrument
//! §7, "legends for every colored layer"):
//!
//! - **National mosaic** — its stamp (the MRMS product valid time, from the
//!   WMS `time` dimension) carries the mosaic's own palette, which is not
//!   the radar's.
//! - **mPING** — the categories of the reports drawn at the playhead.
//!
//! Alert colors are keyed in the alerts chip's list, beside each event. The
//! "Alerts checked" stamp moved there too. The entries are the core's
//! ([`crate::core::canvas::mping_legend`],
//! [`crate::core::canvas::MOSAIC_LEGEND_DBZ`]); this file only paints.

use crate::ui::data_colors;
use crate::ui::theme;
use crate::ui::time_format::format_mosaic_stamp;
use eframe::egui::{self, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

pub(super) struct LayerLegendsOverlay;

const MARGIN: f32 = 12.0;
const PAD: f32 = 6.0;
const GAP: f32 = 6.0;

impl super::Overlay for LayerLegendsOverlay {
    fn z_order(&self) -> i32 {
        50
    }

    fn draw(&self, ui: &mut egui::Ui, ctx: &super::OverlayContext) {
        let painter = ui.painter();
        let state = ctx.state;
        let right = ctx.rect.right() - MARGIN;
        let mut bottom = ctx.rect.bottom() - MARGIN;

        if state.layer_state.geo.national_mosaic && ctx.derived.data_is_live {
            if let Some(stamp) =
                format_mosaic_stamp(state.national_mosaic.image_time(), state.use_local_time)
            {
                bottom = draw_mosaic(painter, right, bottom, stamp) - GAP;
            }
        }

        if state.layer_state.geo.mping
            && state.viz_state.view_mode() == crate::geo::ViewMode::Flat2D
        {
            let legend = crate::core::canvas::mping_legend(ctx.mping_reports, ctx.playback_secs);
            if !legend.is_empty() {
                draw_mping(painter, right, bottom, &legend);
            }
        }
    }
}

fn scrim(painter: &Painter, r: Rect) {
    painter.rect(
        r,
        4.0,
        theme::map::scrim(),
        Stroke::new(1.0_f32, theme::map::scrim_edge()),
        StrokeKind::Inside,
    );
}

/// The mosaic stamp with its palette strip under it. Returns the scrim's top.
fn draw_mosaic(painter: &Painter, right: f32, bottom: f32, stamp: String) -> f32 {
    let font = egui::FontId::monospace(theme::CAPTION);
    let title = painter.layout_no_wrap(stamp, font.clone(), theme::TEXT_DIM);
    let (lo, hi) = crate::core::canvas::mosaic_legend_ends();
    let lo = painter.layout_no_wrap(lo, font.clone(), theme::TEXT_FAINT);
    let hi = painter.layout_no_wrap(hi, font, theme::TEXT_FAINT);
    let swatch = Vec2::new(8.0, 8.0);
    let steps = crate::core::canvas::MOSAIC_LEGEND_DBZ;
    let strip_w = swatch.x * steps.len() as f32;
    let row_h = lo.size().y.max(swatch.y);
    let width = title
        .size()
        .x
        .max(lo.size().x + 4.0 + strip_w + 4.0 + hi.size().x)
        + PAD * 2.0;
    let height = title.size().y + 3.0 + row_h + PAD * 2.0;
    let r = Rect::from_min_size(
        Pos2::new(right - width, bottom - height),
        Vec2::new(width, height),
    );
    scrim(painter, r);
    let title_h = title.size().y;
    painter.galley(r.min + Vec2::new(PAD, PAD), title, theme::TEXT_DIM);
    let y = r.min.y + PAD + title_h + 3.0;
    let mut x = r.min.x + PAD;
    let lo_w = lo.size().x;
    painter.galley(Pos2::new(x, y), lo, theme::TEXT_FAINT);
    x += lo_w + 4.0;
    for dbz in steps {
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(x, y + (row_h - swatch.y) / 2.0), swatch),
            0.0,
            data_colors::mosaic_reflectivity(dbz),
        );
        x += swatch.x;
    }
    painter.galley(Pos2::new(x + 4.0, y), hi, theme::TEXT_FAINT);
    r.min.y
}

/// The mPING categories drawn at the playhead, one row each.
fn draw_mping(
    painter: &Painter,
    right: f32,
    bottom: f32,
    legend: &[(crate::mping::ReportCategory, &'static str)],
) {
    let font = egui::FontId::proportional(theme::CAPTION);
    let title = painter.layout_no_wrap("mPING reports".to_string(), font.clone(), theme::TEXT_DIM);
    let rows: Vec<_> = legend
        .iter()
        .map(|(cat, label)| {
            (
                *cat,
                painter.layout_no_wrap(label.to_string(), font.clone(), theme::TEXT),
            )
        })
        .collect();
    let row_h = rows.iter().map(|r| r.1.size().y).fold(0.0, f32::max);
    let dot = 4.5f32;
    let width = rows
        .iter()
        .map(|r| r.1.size().x + dot * 2.0 + 6.0)
        .fold(title.size().x, f32::max)
        + PAD * 2.0;
    let height = title.size().y + 3.0 + row_h * rows.len() as f32 + PAD * 2.0;
    let r = Rect::from_min_size(
        Pos2::new(right - width, bottom - height),
        Vec2::new(width, height),
    );
    scrim(painter, r);
    let title_h = title.size().y;
    painter.galley(r.min + Vec2::new(PAD, PAD), title, theme::TEXT_DIM);
    let mut y = r.min.y + PAD + title_h + 3.0;
    for (cat, galley) in rows {
        let c = Pos2::new(r.min.x + PAD + dot, y + row_h / 2.0);
        painter.circle_filled(c, dot, data_colors::mping_fill(cat));
        painter.circle_stroke(c, dot, Stroke::new(1.0_f32, theme::map::HALO));
        painter.galley(Pos2::new(c.x + dot + 6.0, y), galley, theme::TEXT);
        y += row_h;
    }
}
