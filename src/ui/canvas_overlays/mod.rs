//! Canvas overlay components drawn on top of the radar image.
//!
//! Two flavors of overlay coexist:
//!
//! 1. **Corner-chrome overlays** — color scale, compass, scale bar, the
//!    layer legends (the mosaic stamp with its palette, mPING categories)
//!    and the info box's fallback stamp, off by default. Each is small,
//!    self-contained, and depends only on [`OverlayContext`] state. These
//!    implement the [`Overlay`] trait: [`layout_chrome`] measures them and
//!    asks the core where each goes
//!    ([`crate::core::map_chrome::layout_map_chrome`] — the colour scale top
//!    right, the scale bar and legends in one bottom tray), and
//!    [`render_chrome_overlays`] paints each in its rectangle, in z-order.
//!
//! 2. **Geo / data-flow overlays** — sites, alerts, mPING reports,
//!    GPS location, national mosaic, radar GPU, sweep animation,
//!    storm cells, distance tool, data probe. These have either
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
mod frame_stamp;
mod globe;
mod gps_location;
mod layer_legends;
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
pub(crate) use sites::{render_nexrad_sites, SiteLabelRoom};
pub(crate) use sweep::{render_radar_sweep, render_site_sweep_marks, SiteScreen, SweepPaint};

use crate::core::map_chrome::{
    layout_map_chrome, ChromeInputs, ChromeLayout, ChromeSize, TrayItem,
};
use crate::geo::ScreenBounds;
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
pub(crate) struct OverlayContext<'a> {
    /// Full canvas rect for this frame.
    pub rect: Rect,
    /// Read-only app state.
    pub state: &'a AppState,
    /// The focus site: its id names the frame, its position anchors the
    /// scale bar.
    pub site: &'a crate::subsystem::SiteState,
    /// Live streaming subsystem (for the in-progress chunk indicator
    /// that the overlay info panel shows).
    pub live: &'a Live,
    /// Per-frame derived snapshot (e.g. `data_is_live` for mosaic stamp).
    pub derived: &'a Derived,
    /// The mPING reports on the map (for their layer legend).
    pub mping_reports: &'a [crate::mping::StormReport],
    /// The playhead (Unix seconds).
    pub playback_secs: f64,
}

/// Where a corner-chrome overlay sits: the core places the colour scale and
/// the tray items ([`crate::core::map_chrome::layout_map_chrome`]); a free
/// overlay places itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Free,
    ColorScale,
    Tray(TrayItem),
}

/// A corner-chrome overlay: one of the small self-contained surfaces
/// drawn after the radar image to provide context (legend, scale, etc.).
///
/// Implementors are zero-sized marker types; behavior + dependencies
/// live in the impl. Drawing is two-phase: every overlay is measured, the
/// core lays them all out at once, then each paints in its rectangle.
pub(crate) trait Overlay {
    /// Lower draws earlier (further back). Z-order is data, not
    /// order-of-call. Tray items enter the tray in this order.
    fn z_order(&self) -> i32;

    /// Whether this overlay should render this frame. Default: always.
    fn visible(&self, _ctx: &OverlayContext) -> bool {
        true
    }

    /// The overlay's slot and measured size this frame, or `None` when it
    /// has nothing to show. A free overlay's size is ignored.
    fn measure(&self, ui: &egui::Ui, ctx: &OverlayContext) -> Option<(Slot, ChromeSize)>;

    /// Paint the overlay into `rect`, the rectangle the layout gave it (the
    /// whole canvas for a free overlay).
    fn draw(&self, ui: &mut egui::Ui, ctx: &OverlayContext, rect: Rect);
}

/// Static registry. Array order must match ascending `z_order` — checked
/// once per build in debug. Adding an overlay is one new struct + one new
/// entry here, placed at its z-order position.
const OVERLAYS: &[&dyn Overlay] = &[
    &frame_stamp::FrameStampOverlay,     // z=10 (fallback; off)
    &color_scale::ColorScaleOverlay,     // z=20
    &compass::CompassOverlay,            // z=30 (3D tray, left end)
    &scale_bar::ScaleBarOverlay,         // z=40 (2D tray, left end)
    &layer_legends::MosaicLegendOverlay, // z=50 (tray, bottom-right corner)
    &layer_legends::MpingLegendOverlay,  // z=51 (tray, next in)
];

/// This frame's chrome: which overlays are on, and the core's layout of
/// them in screen coordinates.
pub(crate) struct ChromeFrame {
    /// Screen-space layout (the core's canvas-local layout moved to the
    /// canvas's origin).
    pub layout: ChromeLayout,
    placed: Vec<(usize, Slot)>,
}

impl ChromeFrame {
    /// Every rectangle chrome covers, for map labels to keep clear of.
    pub(crate) fn keep_out(&self) -> Vec<ScreenBounds> {
        self.layout.keep_out()
    }
}

fn to_rect(b: ScreenBounds) -> Rect {
    Rect::from_min_max(egui::pos2(b.min_x, b.min_y), egui::pos2(b.max_x, b.max_y))
}

/// The canvas as label bounds.
pub(crate) fn viewport_bounds(rect: Rect) -> ScreenBounds {
    ScreenBounds {
        min_x: rect.min.x,
        min_y: rect.min.y,
        max_x: rect.max.x,
        max_y: rect.max.y,
    }
}

/// Measure every visible corner-chrome overlay and lay them out
/// ([`crate::core::map_chrome::layout_map_chrome`]). `toolbar` is the map
/// toolbar's size (2D), so the tray keeps clear of it. Called before the
/// map's labels paint, so they can keep clear of the chrome.
pub(crate) fn layout_chrome(
    ui: &egui::Ui,
    ctx: &OverlayContext,
    toolbar: Option<egui::Vec2>,
) -> ChromeFrame {
    debug_assert!(
        OVERLAYS
            .windows(2)
            .all(|w| w[0].z_order() <= w[1].z_order()),
        "OVERLAYS array must be sorted by z_order ascending",
    );

    let mut placed = Vec::new();
    let mut color_scale = None;
    let mut tray = Vec::new();
    for (index, overlay) in OVERLAYS.iter().enumerate() {
        if !overlay.visible(ctx) {
            continue;
        }
        let Some((slot, size)) = overlay.measure(ui, ctx) else {
            continue;
        };
        match slot {
            Slot::Free => {}
            Slot::ColorScale => color_scale = Some(size),
            Slot::Tray(item) => tray.push((item, size)),
        }
        placed.push((index, slot));
    }
    let layout = layout_map_chrome(&ChromeInputs {
        width: ctx.rect.width(),
        height: ctx.rect.height(),
        toolbar: toolbar.map(|s| ChromeSize { w: s.x, h: s.y }),
        color_scale,
        tray: &tray,
    })
    .offset(ctx.rect.left(), ctx.rect.top());
    ChromeFrame { layout, placed }
}

/// Paint the corner-chrome overlays in z-order, each in the rectangle the
/// layout gave it. Called once per frame from
/// [`super::canvas::render_canvas_with_geo`] after the data-flow overlays
/// (radar, alerts, sites, etc.) have drawn. Returns the tray's scrims, for
/// the canvas caption to keep clear of.
pub(crate) fn render_chrome_overlays(
    ui: &mut egui::Ui,
    ctx: &OverlayContext,
    frame: &ChromeFrame,
) -> Vec<crate::core::canvas::BottomScrim> {
    for &(index, slot) in &frame.placed {
        let rect = match slot {
            Slot::Free => Some(ctx.rect),
            Slot::ColorScale => frame.layout.color_scale.map(to_rect),
            Slot::Tray(item) => frame.layout.tray_rect(item).map(to_rect),
        };
        if let Some(rect) = rect {
            // A rectangle clipped to a narrow canvas clips its painting too.
            let clip = ui.clip_rect();
            ui.set_clip_rect(clip.intersect(rect.expand(1.0)));
            OVERLAYS[index].draw(ui, ctx, rect);
            ui.set_clip_rect(clip);
        }
    }

    frame
        .layout
        .tray
        .iter()
        .map(|&(_, r)| crate::core::canvas::BottomScrim {
            left: r.min_x - ctx.rect.left(),
            right: r.max_x - ctx.rect.left(),
            top: ctx.rect.bottom() - r.min_y,
        })
        .collect()
}
