//! Visualization state (canvas, zoom/pan, product selection).

use crate::core::{DisplayedSweep, ElevationSelection, RadarProduct, StormCellInfo};
use crate::geo::{Camera, Flat2DState, ViewMode};
use eframe::egui::Vec2;

/// Visualization state including view controls.
pub(crate) struct VizState {
    /// The camera state machine — the single source of truth for the view
    /// mode and all 2D/3D camera state. [`ViewMode`] is derived from the
    /// active variant via [`VizState::view_mode`] (no separate stored
    /// toggle). 2D pan/zoom live on the [`Flat2D`](crate::geo::Camera::Flat2D)
    /// variant; read/write via [`VizState::zoom`] / [`VizState::pan_offset`]
    /// / [`VizState::set_zoom`] / [`VizState::set_pan_offset`].
    pub camera: Camera,

    /// Selected radar product
    pub product: RadarProduct,

    /// Elevation selection (specific VCP cut or auto/latest mode)
    pub elevation_selection: ElevationSelection,

    /// Stored Fixed selection to restore when toggling off auto mode.
    pub last_fixed_selection: Option<(u8, f32)>,

    /// The 3D camera mode to return to when toggling 2D → 3D (the `T`
    /// shortcut and the Basic 3D pill). Preserves the historical "toggle
    /// last 2D/3D mode" behavior now that [`ViewMode`] is derived from the
    /// camera variant rather than stored independently. Updated whenever a
    /// 3D mode becomes active.
    pub last_3d_mode: crate::geo::CameraMode,

    /// Overlay info: radar site ID
    pub site_id: String,

    /// Overlay info: current elevation/sweep, e.g. "0.5°". Timezone-independent,
    /// so it stays a baked string; the displayed-frame *timestamp* is NOT baked
    /// here — it is formatted at render time from `displayed` (spec §11.4) so a
    /// local/UTC flip reformats it the same frame.
    pub elevation: String,

    /// Geographic center latitude (radar site location)
    pub center_lat: f64,

    /// Geographic center longitude (radar site location)
    pub center_lon: f64,

    /// Staleness of the most recent radial (sweep end) in seconds.
    /// Recomputed every frame from `displayed.end_time` against wall clock.
    pub data_staleness_secs: Option<f64>,

    /// Staleness of the oldest radial (sweep start) in seconds.
    /// Recomputed every frame from `displayed.start_time` against wall clock.
    pub data_staleness_start_secs: Option<f64>,

    /// Cached last sweep line position (azimuth, start_azimuth) for between-sweep display.
    pub last_sweep_line_cache: Option<(f32, f32)>,

    /// Whether 3D volumetric rendering is enabled (ray-marched volume).
    pub volume_3d_enabled: bool,

    /// Density cutoff for volume rendering (physical value, e.g. 5.0 dBZ).
    pub volume_density_cutoff: f32,

    /// Whether the data-probe tool is active (hover shows lat/lon and data
    /// value). Named "Data probe" to keep "inspector" for the scan inspector
    /// (the per-scan volume breakdown), which is an unrelated surface.
    pub data_probe_enabled: bool,

    /// Whether the distance measurement tool is active.
    pub distance_tool_active: bool,

    /// Distance measurement start point (lat, lon).
    pub distance_start: Option<(f64, f64)>,

    /// Distance measurement end point (lat, lon).
    pub distance_end: Option<(f64, f64)>,

    /// Whether storm cell detection overlay is visible.
    pub storm_cells_visible: bool,

    /// Minimum dBZ threshold for storm cell detection.
    pub storm_cell_threshold_dbz: f32,

    /// Cached storm cell detection results (centroid lat, lon, max dBZ, area km2).
    pub detected_storm_cells: Vec<StormCellInfo>,

    /// Last observed visible map bounds in 2D mode, as
    /// `(min_lon, min_lat, max_lon, max_lat)`. Updated each frame by the
    /// canvas renderer and consumed by top-bar / modal logic that needs
    /// to know what area the user is looking at without access to the
    /// canvas rect. `None` while in 3D globe mode.
    pub last_visible_bounds: Option<(f64, f64, f64, f64)>,

    /// What is actually on the GPU main-slot texture right now. Set only
    /// after a successful `update_data()` in `handle_decoded_outcome` /
    /// `handle_live_decoded_outcome`; cleared when the canvas blanks.
    /// Single source of truth for the timeline active border, canvas
    /// overlay text, and staleness counters.
    pub displayed: Option<DisplayedSweep>,

    /// What is on the GPU prev-sweep texture (the under-layer for sweep
    /// animation). Written by `sync_prev_sweep_texture` in archive mode
    /// and by the `should_promote` branch in
    /// `handle_live_decoded_outcome` for live mode — both reflect what
    /// the prev-sweep slot actually holds, NOT the prior main upload.
    /// Drives the timeline secondary border and the prev-sweep overlay
    /// panel.
    pub previous_displayed: Option<DisplayedSweep>,

    /// What honesty caption the canvas should show this frame (spec §11.2).
    /// Recomputed each frame in `advance_playback`; replaces the old
    /// `acquiring` bool. Either the centered "Acquiring data…" hint on a blank
    /// canvas, or a "showing X · fetching/​no-data Y" discrepancy caption when a
    /// stale frame is held while the playhead has drifted past it.
    pub canvas_caption: CanvasCaption,
}

/// Honesty caption for the canvas (spec §11.2 / alignment §3 — caption only).
///
/// The canvas keeps showing the most recent successfully displayed frame when
/// the playhead drifts into an undownloaded region or gap (it never blanks
/// merely because the playhead moved away in time). This enum tells the overlay
/// which caption, if any, to render to keep the time honest.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) enum CanvasCaption {
    /// No caption — the displayed frame covers the playhead, or live owns the
    /// canvas.
    #[default]
    None,
    /// Blank canvas with a reactive fetch in flight: show "Acquiring data…" so
    /// an empty view reads as loading, not broken.
    Acquiring,
    /// A stale frame is held while the playhead sits past it. `showing` is the
    /// displayed frame's representative (midpoint) time; `target` is the
    /// playhead time. `fetching` distinguishes "fetching Y…" (a download covers
    /// the playhead) from "no data at Y" (nothing is, or is coming).
    Discrepancy {
        showing: f64,
        target: f64,
        fetching: bool,
    },
}

/// Pure derivation of the canvas honesty caption (spec §11.2). Kept separate
/// from `advance_playback` so the decision is unit-testable.
///
/// - `attached`: the playhead is tethered to the live edge (pinned/lookback) or
///   a live stream owns the canvas — then live's partial path owns the caption,
///   so we emit `None`.
/// - `displayed`: the on-screen frame's `(start, end, midpoint)`, if any.
/// - `playhead`: the playback position (seconds).
/// - `scan_covers_playhead`: whether a cached scan exists at-or-before the
///   playhead within the recency window (i.e. the resolver could pick a frame
///   covering it). When true there's no discrepancy — the resolver/render path
///   is repainting; when false the held frame is stale relative to the playhead.
/// - `fetch_covers_playhead`: whether a download/ingest covers the playhead.
pub(crate) fn derive_canvas_caption(
    attached: bool,
    displayed: Option<(f64, f64, f64)>,
    playhead: f64,
    scan_covers_playhead: bool,
    fetch_covers_playhead: bool,
) -> CanvasCaption {
    // Live (or any attached) state: the live partial path owns the canvas.
    if attached {
        return CanvasCaption::None;
    }
    match displayed {
        // A frame is held. If no scan covers the playhead, the held frame is
        // stale relative to where the playhead sits — surface the discrepancy
        // (the canvas keeps showing it rather than blanking).
        Some((_start, _end, midpoint)) => {
            if scan_covers_playhead {
                CanvasCaption::None
            } else {
                CanvasCaption::Discrepancy {
                    showing: midpoint,
                    target: playhead,
                    fetching: fetch_covers_playhead,
                }
            }
        }
        // Blank canvas: the legacy "Acquiring…" hint when a fetch covers the
        // playhead, else nothing.
        None => {
            if fetch_covers_playhead {
                CanvasCaption::Acquiring
            } else {
                CanvasCaption::None
            }
        }
    }
}

impl Default for VizState {
    fn default() -> Self {
        Self {
            camera: Camera::centered_on(41.7312, -93.7229),
            product: RadarProduct::default(),
            elevation_selection: ElevationSelection::default(),
            last_fixed_selection: None,
            last_3d_mode: crate::geo::CameraMode::default(),
            site_id: "KDMX".to_string(),
            elevation: "-- deg".to_string(),
            center_lat: 41.7312,
            center_lon: -93.7229,
            data_staleness_secs: None,
            data_staleness_start_secs: None,
            last_sweep_line_cache: None,
            volume_3d_enabled: false,
            volume_density_cutoff: 5.0,
            data_probe_enabled: false,
            distance_tool_active: false,
            distance_start: None,
            distance_end: None,
            storm_cells_visible: false,
            storm_cell_threshold_dbz: 35.0,
            detected_storm_cells: Vec::new(),
            last_visible_bounds: None,
            displayed: None,
            previous_displayed: None,
            canvas_caption: CanvasCaption::None,
        }
    }
}

impl VizState {
    /// The active [`ViewMode`], derived from the camera variant.
    pub(crate) fn view_mode(&self) -> ViewMode {
        self.camera.view_mode()
    }

    /// Whether the flat 2D view is active.
    pub(crate) fn is_2d(&self) -> bool {
        self.camera.is_2d()
    }

    /// Current 2D zoom level (1.0 = 100%). Falls back to the default zoom in
    /// 3D modes (the 2D pan/zoom is only meaningful in the Flat2D variant).
    pub(crate) fn zoom(&self) -> f32 {
        self.camera.flat_2d().map(|s| s.zoom).unwrap_or(1.0)
    }

    /// Current 2D pan offset. `ZERO` in 3D modes.
    pub(crate) fn pan_offset(&self) -> Vec2 {
        self.camera
            .flat_2d()
            .map(|s| s.pan_offset)
            .unwrap_or(Vec2::ZERO)
    }

    /// Set the 2D zoom level. No-op in 3D modes.
    pub(crate) fn set_zoom(&mut self, zoom: f32) {
        if let Some(s) = self.camera.flat_2d_mut() {
            s.zoom = zoom;
        }
    }

    /// Set the 2D pan offset. No-op in 3D modes.
    pub(crate) fn set_pan_offset(&mut self, pan_offset: Vec2) {
        if let Some(s) = self.camera.flat_2d_mut() {
            s.pan_offset = pan_offset;
        }
    }

    /// Mutable access to the 2D pan offset, if the flat view is active.
    /// Used by the WASD pan path that increments each axis.
    pub(crate) fn flat_pan_mut(&mut self) -> Option<&mut Vec2> {
        self.camera.flat_2d_mut().map(|s| &mut s.pan_offset)
    }

    /// Switch the camera into the given 3D mode and remember it as the
    /// mode to return to when toggling 2D → 3D.
    pub(crate) fn switch_camera_mode(&mut self, mode: crate::geo::CameraMode) {
        self.last_3d_mode = mode;
        self.camera.switch_to_3d(mode);
    }

    /// Switch the camera to the flat 2D view (default pan/zoom).
    pub(crate) fn switch_to_2d(&mut self) {
        self.camera.switch_to_flat_2d(Flat2DState::default());
    }

    /// Toggle between the flat 2D view and the last-used 3D mode. Mirrors
    /// the historical `T` shortcut: 2D → the remembered 3D mode, any 3D
    /// mode → 2D.
    pub(crate) fn toggle_2d_3d(&mut self) {
        if self.camera.is_2d() {
            self.switch_camera_mode(self.last_3d_mode);
        } else {
            self.switch_to_2d();
        }
    }

    /// Update the canvas overlay's elevation text and seed staleness from a
    /// freshly decoded sweep. The displayed-frame *timestamp* is no longer baked
    /// here: it is formatted at render time from `displayed` so a local/UTC flip
    /// reformats it the same frame (spec §11.4). Sweep start/end times live on
    /// `displayed` (set by the decode handler); staleness is recomputed each
    /// frame from there. `now_secs` is the frame clock (`AppState::frame_now`).
    pub(crate) fn update_overlay(
        &mut self,
        start: f64,
        end: f64,
        elevation_deg: f32,
        now_secs: f64,
    ) {
        self.elevation = format!("{:.1}\u{00B0}", elevation_deg);

        // Seed staleness for immediate display; the per-frame recompute
        // in `update()` keeps it ticking from `displayed`.
        let staleness_end = now_secs - end;
        let staleness_start = now_secs - start;
        self.data_staleness_secs = (staleness_end >= 0.0).then_some(staleness_end);
        self.data_staleness_start_secs = (staleness_start >= 0.0).then_some(staleness_start);
    }

    /// Representative (midpoint) collection time of the on-screen frame, in Unix
    /// seconds, or `None` when the canvas holds no frame. This is the raw time
    /// the primary readout and overlay format live each frame — never the
    /// playhead (the canvas-honesty invariant).
    pub(crate) fn displayed_midpoint_secs(&self) -> Option<f64> {
        self.displayed
            .as_ref()
            .map(|d| (d.start_time + d.end_time) / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // A displayed frame [100, 200], midpoint 150.
    fn frame() -> Option<(f64, f64, f64)> {
        Some((100.0, 200.0, 150.0))
    }

    #[wasm_bindgen_test]
    fn view_mode_and_zoom_pan_derive_from_camera() {
        let mut viz = VizState::default();
        assert_eq!(viz.view_mode(), ViewMode::Flat2D);
        assert!(viz.is_2d());
        // 2D pan/zoom round-trip through the accessors.
        viz.set_zoom(3.0);
        viz.set_pan_offset(Vec2::new(12.0, -4.0));
        assert!((viz.zoom() - 3.0).abs() < 1e-6);
        assert!((viz.pan_offset().x - 12.0).abs() < 1e-6);
        // Switching to a 3D mode makes view_mode derive to Globe3D; zoom/pan
        // fall back to the 2D defaults (only meaningful in Flat2D).
        viz.switch_camera_mode(crate::geo::CameraMode::SiteOrbit);
        assert_eq!(viz.view_mode(), ViewMode::Globe3D);
        assert!(!viz.is_2d());
        assert!((viz.zoom() - 1.0).abs() < 1e-6);
        assert_eq!(viz.pan_offset(), Vec2::ZERO);
    }

    #[wasm_bindgen_test]
    fn toggle_returns_to_last_3d_mode() {
        let mut viz = VizState::default();
        // Pick FreeLook as the last 3D mode, then drop to 2D.
        viz.switch_camera_mode(crate::geo::CameraMode::FreeLook);
        assert_eq!(
            viz.camera.camera_mode(),
            Some(crate::geo::CameraMode::FreeLook)
        );
        viz.switch_to_2d();
        assert!(viz.is_2d());
        // Toggling back enters the remembered FreeLook mode, not the default.
        viz.toggle_2d_3d();
        assert_eq!(
            viz.camera.camera_mode(),
            Some(crate::geo::CameraMode::FreeLook)
        );
        // Toggling again returns to 2D.
        viz.toggle_2d_3d();
        assert!(viz.is_2d());
    }

    #[wasm_bindgen_test]
    fn caption_suppressed_while_attached() {
        // Live owns the canvas while attached — never caption, even with a
        // drifted playhead and no covering scan.
        assert_eq!(
            derive_canvas_caption(true, frame(), 9999.0, false, false),
            CanvasCaption::None
        );
        // Also suppressed on a blank canvas while attached.
        assert_eq!(
            derive_canvas_caption(true, None, 9999.0, false, true),
            CanvasCaption::None
        );
    }

    #[wasm_bindgen_test]
    fn caption_none_when_scan_covers_playhead() {
        // A frame is held and a scan covers the playhead: the resolver/render
        // path is repainting, so no discrepancy.
        assert_eq!(
            derive_canvas_caption(false, frame(), 180.0, true, false),
            CanvasCaption::None
        );
    }

    #[wasm_bindgen_test]
    fn caption_discrepancy_fetching_vs_no_data() {
        // Held frame, playhead drifted past it, no covering scan, fetch in
        // flight → "fetching" discrepancy carrying the displayed midpoint and
        // the playhead.
        assert_eq!(
            derive_canvas_caption(false, frame(), 700.0, false, true),
            CanvasCaption::Discrepancy {
                showing: 150.0,
                target: 700.0,
                fetching: true,
            }
        );
        // Same but nothing is being fetched → "no data" discrepancy.
        assert_eq!(
            derive_canvas_caption(false, frame(), 700.0, false, false),
            CanvasCaption::Discrepancy {
                showing: 150.0,
                target: 700.0,
                fetching: false,
            }
        );
    }

    #[wasm_bindgen_test]
    fn caption_blank_canvas_acquiring_hint() {
        // No frame on screen + a fetch covering the playhead → the legacy
        // centered "Acquiring data…" hint.
        assert_eq!(
            derive_canvas_caption(false, None, 700.0, false, true),
            CanvasCaption::Acquiring
        );
        // No frame and nothing fetching → no caption (a plain empty canvas).
        assert_eq!(
            derive_canvas_caption(false, None, 700.0, false, false),
            CanvasCaption::None
        );
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use crate::core::SweepIdentity;
    use crate::data::keys::UnixMillis;
    use crate::data::ScanKey;

    fn scan_key(site: &str, ms: i64) -> ScanKey {
        ScanKey::new(site, UnixMillis(ms))
    }

    // ---- enum defaults -----------------------------------------------------

    #[wasm_bindgen_test]
    fn enum_defaults() {
        assert_eq!(ViewMode::default(), ViewMode::Flat2D);
        assert_eq!(CanvasCaption::default(), CanvasCaption::None);
    }

    // ---- VizState::update_overlay & displayed_midpoint_secs ---------------

    #[wasm_bindgen_test]
    fn update_overlay_formats_and_seeds_staleness() {
        let mut viz = VizState::default();
        // now=300, end=200 -> 100s stale; start=100 -> 200s stale.
        viz.update_overlay(100.0, 200.0, 0.5, 300.0);
        assert_eq!(viz.elevation, "0.5\u{00B0}");
        assert!((viz.data_staleness_secs.unwrap() - 100.0).abs() < 1e-6);
        assert!((viz.data_staleness_start_secs.unwrap() - 200.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn update_overlay_clamps_future_frames_to_none() {
        let mut viz = VizState::default();
        // now precedes both start and end -> negative staleness clamped to None.
        viz.update_overlay(500.0, 600.0, 12.34, 400.0);
        assert_eq!(viz.elevation, "12.3\u{00B0}");
        assert_eq!(viz.data_staleness_secs, None);
        assert_eq!(viz.data_staleness_start_secs, None);
    }

    #[wasm_bindgen_test]
    fn displayed_midpoint_secs_none_then_some() {
        let mut viz = VizState::default();
        assert_eq!(viz.displayed_midpoint_secs(), None);

        viz.displayed = Some(DisplayedSweep {
            identity: SweepIdentity::new(scan_key("KDMX", 1000), 1, "reflectivity"),
            start_time: 100.0,
            end_time: 250.0,
            elevation_deg: 0.5,
        });
        assert!((viz.displayed_midpoint_secs().unwrap() - 175.0).abs() < 1e-6);
    }

    // ---- VizState 2D/3D accessor no-ops ----------------------------------

    #[wasm_bindgen_test]
    fn set_zoom_pan_are_noops_in_3d() {
        let mut viz = VizState::default();
        viz.switch_camera_mode(crate::geo::CameraMode::SiteOrbit);
        assert!(!viz.is_2d());
        // No flat-2D state to write -> setters are no-ops, getters fall back.
        viz.set_zoom(5.0);
        viz.set_pan_offset(Vec2::new(9.0, 9.0));
        assert!((viz.zoom() - 1.0).abs() < 1e-6);
        assert_eq!(viz.pan_offset(), Vec2::ZERO);
        // flat_pan_mut yields None in 3D.
        assert!(viz.flat_pan_mut().is_none());
    }
}
