//! Canvas overlay components drawn on top of the radar image.
//!
//! Two flavors of overlay coexist:
//!
//! 1. **Corner-chrome overlays** — color scale, overlay info, compass,
//!    scale bar. Each is small, self-contained, and depends only on
//!    [`OverlayContext`] state. These implement the [`Overlay`] trait
//!    and are dispatched through the [`render_chrome_overlays`] loop in
//!    z-order, with a `visible` predicate that gates on view mode and
//!    user preferences.
//!
//! 2. **Geo / data-flow overlays** — sites, alerts, mPING reports,
//!    GPS location, national mosaic, radar GPU, sweep animation,
//!    storm cells, distance tool, inspector. These have either
//!    interleaved pass dependencies (geo layers' Lines/Labels passes
//!    bracket the radar image) or mid-render computed inputs
//!    (`gpu_sweep`, `radar_cutout`, `sweep_line_info`). They stay as
//!    explicit function calls in [`super::canvas::render_canvas_with_geo`]
//!    so the data-flow stays visible and the z-order between them
//!    (radar under alerts under labels under sites…) is clear from
//!    reading the canvas top-to-bottom.

mod alerts;
mod color_scale;
mod compass;
mod globe;
mod gps_location;
mod info;
mod mping;
mod national_mosaic;
mod scale_bar;
mod sites;
mod sweep;

pub(crate) use alerts::{render_alerts, AlertRenderPhase};
pub(crate) use globe::draw_globe;
pub(crate) use gps_location::render_gps_location;
pub(crate) use mping::{render_mping_detail, render_mping_reports};
pub(crate) use national_mosaic::{draw_national_mosaic, RadarCutout};
pub(crate) use sites::render_nexrad_sites;
pub(crate) use sweep::render_radar_sweep;

use crate::state::AppState;
use crate::subsystem::{Derived, Live};
use eframe::egui::{self, Rect};

/// Per-frame state every corner-chrome overlay can read.
///
/// Built once in [`render_chrome_overlays`] from the canvas's own
/// inputs; passed by `&` to each overlay's `visible` and `draw`. The
/// fields are intentionally narrow — overlays that need more (e.g.
/// the camera for the compass) reach through `state` or get a typed
/// borrow when constructed.
pub struct OverlayContext<'a> {
    /// Full canvas rect for this frame.
    pub rect: Rect,
    /// Read-only app state.
    pub state: &'a AppState,
    /// Live streaming subsystem (for the in-progress chunk indicator
    /// that the overlay info panel shows).
    pub live: &'a Live,
    /// Per-frame derived snapshot. Currently unused by the chrome
    /// overlays themselves, but available so future predicates (e.g.
    /// hide the color scale while data is stale) can gate on it
    /// without changing the signature.
    #[allow(dead_code)]
    pub derived: &'a Derived,
}

/// A corner-chrome overlay: one of the small self-contained surfaces
/// drawn after the radar image to provide context (legend, scale, etc.).
///
/// Implementors are zero-sized marker types; behavior + dependencies
/// live in the impl.
pub trait Overlay {
    /// Lower draws earlier (further back). Z-order is data, not
    /// order-of-call.
    fn z_order(&self) -> i32;

    /// Whether this overlay should render this frame. Default: always.
    fn visible(&self, _ctx: &OverlayContext) -> bool {
        true
    }

    /// Paint the overlay onto `ui`. `ctx` is also passed so impls can
    /// access state without taking it twice.
    fn draw(&self, ui: &mut egui::Ui, ctx: &OverlayContext);
}

/// Dispatch all corner-chrome overlays in z-order. Called once per
/// frame from [`super::canvas::render_canvas_with_geo`] after the
/// data-flow overlays (radar, alerts, sites, etc.) have drawn.
pub(crate) fn render_chrome_overlays(ui: &mut egui::Ui, ctx: &OverlayContext) {
    // Static registry. Array order must match ascending `z_order` —
    // checked once per build in debug. Adding an overlay is one new
    // struct + one new entry here, placed at its z-order position.
    let overlays: &[&dyn Overlay] = &[
        &info::OverlayInfo,              // z=10
        &color_scale::ColorScaleOverlay, // z=20
        &compass::CompassOverlay,        // z=30
        &scale_bar::ScaleBarOverlay,     // z=40
    ];

    debug_assert!(
        overlays
            .windows(2)
            .all(|w| w[0].z_order() <= w[1].z_order()),
        "OVERLAYS array must be sorted by z_order ascending",
    );

    for overlay in overlays {
        if overlay.visible(ctx) {
            overlay.draw(ui, ctx);
        }
    }
}
