//! Legends for the colored layers, on scrims in the bottom tray from its
//! right end inward (one-instrument §7, "legends for every colored layer";
//! placement is core::map_chrome's):
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

use crate::core::map_chrome::{ChromeSize, TrayItem};
use crate::ui::data_colors;
use crate::ui::theme;
use crate::ui::time_format::format_mosaic_stamp;
use eframe::egui::{self, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

pub(super) struct MosaicLegendOverlay;
pub(super) struct MpingLegendOverlay;

const PAD: f32 = 6.0;

fn mosaic_stamp(ctx: &super::OverlayContext) -> Option<String> {
    let state = ctx.state;
    if !(state.layer_state.geo.national_mosaic && ctx.derived.data_is_live) {
        return None;
    }
    format_mosaic_stamp(state.national_mosaic.image_time(), state.use_local_time)
}

fn mping_entries(
    ctx: &super::OverlayContext,
) -> Option<Vec<(crate::mping::ReportCategory, &'static str)>> {
    let state = ctx.state;
    if !(state.layer_state.geo.mping && state.viz_state.view_mode() == crate::geo::ViewMode::Flat2D)
    {
        return None;
    }
    let legend = crate::core::canvas::mping_legend(ctx.mping_reports, ctx.playback_secs);
    (!legend.is_empty()).then_some(legend)
}

fn size(v: Vec2) -> ChromeSize {
    ChromeSize { w: v.x, h: v.y }
}

impl super::Overlay for MosaicLegendOverlay {
    fn z_order(&self) -> i32 {
        50
    }

    fn measure(
        &self,
        ui: &egui::Ui,
        ctx: &super::OverlayContext,
    ) -> Option<(super::Slot, ChromeSize)> {
        let parts = MosaicParts::new(ui.painter(), mosaic_stamp(ctx)?);
        Some((
            super::Slot::Tray(TrayItem::MosaicLegend),
            size(parts.size()),
        ))
    }

    fn draw(&self, ui: &mut egui::Ui, ctx: &super::OverlayContext, rect: Rect) {
        if let Some(stamp) = mosaic_stamp(ctx) {
            let painter = ui.painter();
            MosaicParts::new(painter, stamp).paint(painter, rect);
        }
    }
}

impl super::Overlay for MpingLegendOverlay {
    fn z_order(&self) -> i32 {
        51
    }

    fn measure(
        &self,
        ui: &egui::Ui,
        ctx: &super::OverlayContext,
    ) -> Option<(super::Slot, ChromeSize)> {
        let parts = MpingParts::new(ui.painter(), &mping_entries(ctx)?);
        Some((super::Slot::Tray(TrayItem::MpingLegend), size(parts.size())))
    }

    fn draw(&self, ui: &mut egui::Ui, ctx: &super::OverlayContext, rect: Rect) {
        if let Some(legend) = mping_entries(ctx) {
            let painter = ui.painter();
            MpingParts::new(painter, &legend).paint(painter, rect);
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

/// The mosaic stamp with its palette strip under it, laid out.
struct MosaicParts {
    title: std::sync::Arc<egui::Galley>,
    lo: std::sync::Arc<egui::Galley>,
    hi: std::sync::Arc<egui::Galley>,
}

const SWATCH: Vec2 = Vec2::new(8.0, 8.0);

impl MosaicParts {
    fn new(painter: &Painter, stamp: String) -> Self {
        let font = egui::FontId::monospace(theme::CAPTION);
        let title = painter.layout_no_wrap(stamp, font.clone(), theme::TEXT_DIM);
        let (lo, hi) = crate::core::canvas::mosaic_legend_ends();
        let lo = painter.layout_no_wrap(lo, font.clone(), theme::TEXT_FAINT);
        let hi = painter.layout_no_wrap(hi, font, theme::TEXT_FAINT);
        Self { title, lo, hi }
    }

    fn row_h(&self) -> f32 {
        self.lo.size().y.max(SWATCH.y)
    }

    fn size(&self) -> Vec2 {
        let strip_w = SWATCH.x * crate::core::canvas::MOSAIC_LEGEND_DBZ.len() as f32;
        let width = self
            .title
            .size()
            .x
            .max(self.lo.size().x + 4.0 + strip_w + 4.0 + self.hi.size().x)
            + PAD * 2.0;
        let height = self.title.size().y + 3.0 + self.row_h() + PAD * 2.0;
        Vec2::new(width, height)
    }

    /// Paint the scrim and its contents in `r`, the tray's slot.
    fn paint(self, painter: &Painter, r: Rect) {
        scrim(painter, r);
        let row_h = self.row_h();
        let title_h = self.title.size().y;
        painter.galley(r.min + Vec2::new(PAD, PAD), self.title, theme::TEXT_DIM);
        let y = r.min.y + PAD + title_h + 3.0;
        let mut x = r.min.x + PAD;
        let lo_w = self.lo.size().x;
        painter.galley(Pos2::new(x, y), self.lo, theme::TEXT_FAINT);
        x += lo_w + 4.0;
        for dbz in crate::core::canvas::MOSAIC_LEGEND_DBZ {
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(x, y + (row_h - SWATCH.y) / 2.0), SWATCH),
                0.0,
                data_colors::mosaic_reflectivity(dbz),
            );
            x += SWATCH.x;
        }
        painter.galley(Pos2::new(x + 4.0, y), self.hi, theme::TEXT_FAINT);
    }
}

/// The mPING categories drawn at the playhead, one row each, laid out.
struct MpingParts {
    title: std::sync::Arc<egui::Galley>,
    rows: Vec<(crate::mping::ReportCategory, std::sync::Arc<egui::Galley>)>,
}

const DOT: f32 = 4.5;

impl MpingParts {
    fn new(painter: &Painter, legend: &[(crate::mping::ReportCategory, &'static str)]) -> Self {
        let font = egui::FontId::proportional(theme::CAPTION);
        let title =
            painter.layout_no_wrap("mPING reports".to_string(), font.clone(), theme::TEXT_DIM);
        let rows = legend
            .iter()
            .map(|(cat, label)| {
                (
                    *cat,
                    painter.layout_no_wrap(label.to_string(), font.clone(), theme::TEXT),
                )
            })
            .collect();
        Self { title, rows }
    }

    fn row_h(&self) -> f32 {
        self.rows.iter().map(|r| r.1.size().y).fold(0.0, f32::max)
    }

    fn size(&self) -> Vec2 {
        let width = self
            .rows
            .iter()
            .map(|r| r.1.size().x + DOT * 2.0 + 6.0)
            .fold(self.title.size().x, f32::max)
            + PAD * 2.0;
        let height = self.title.size().y + 3.0 + self.row_h() * self.rows.len() as f32 + PAD * 2.0;
        Vec2::new(width, height)
    }

    /// Paint the scrim and its contents in `r`, the tray's slot.
    fn paint(self, painter: &Painter, r: Rect) {
        scrim(painter, r);
        let row_h = self.row_h();
        let title_h = self.title.size().y;
        painter.galley(r.min + Vec2::new(PAD, PAD), self.title, theme::TEXT_DIM);
        let mut y = r.min.y + PAD + title_h + 3.0;
        for (cat, galley) in self.rows {
            let c = Pos2::new(r.min.x + PAD + DOT, y + row_h / 2.0);
            painter.circle_filled(c, DOT, data_colors::mping_fill(cat));
            painter.circle_stroke(c, DOT, Stroke::new(1.0_f32, theme::map::HALO));
            painter.galley(Pos2::new(c.x + DOT + 6.0, y), galley, theme::TEXT);
            y += row_h;
        }
    }
}
