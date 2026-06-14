//! Visualization state (canvas, zoom/pan, product selection).

use crate::data::ScanKey;
use crate::geo::{Camera, Flat2DState};
use eframe::egui::Vec2;

/// Available radar products for display.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum RadarProduct {
    #[default]
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    CorrelationCoefficient,
    DifferentialPhase,
    ClutterFilterPower,
}

impl RadarProduct {
    pub fn label(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "Reflectivity",
            RadarProduct::Velocity => "Velocity",
            RadarProduct::SpectrumWidth => "Spectrum Width",
            RadarProduct::DifferentialReflectivity => "Differential Reflectivity",
            RadarProduct::CorrelationCoefficient => "Correlation Coefficient",
            RadarProduct::DifferentialPhase => "Differential Phase",
            RadarProduct::ClutterFilterPower => "Clutter Filter Power",
        }
    }

    /// Unit string for display (e.g., "dBZ", "m/s").
    pub fn unit(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "dBZ",
            RadarProduct::Velocity => "m/s",
            RadarProduct::SpectrumWidth => "m/s",
            RadarProduct::DifferentialReflectivity => "dB",
            RadarProduct::CorrelationCoefficient => "",
            RadarProduct::DifferentialPhase => "\u{00B0}/km",
            RadarProduct::ClutterFilterPower => "dB",
        }
    }

    /// Short code for URL parameters.
    pub fn short_code(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "REF",
            RadarProduct::Velocity => "VEL",
            RadarProduct::SpectrumWidth => "SW",
            RadarProduct::DifferentialReflectivity => "ZDR",
            RadarProduct::CorrelationCoefficient => "CC",
            RadarProduct::DifferentialPhase => "KDP",
            RadarProduct::ClutterFilterPower => "CFP",
        }
    }

    /// Parse from a short code string.
    pub fn from_short_code(code: &str) -> Option<Self> {
        match code {
            "REF" => Some(RadarProduct::Reflectivity),
            "VEL" => Some(RadarProduct::Velocity),
            "SW" => Some(RadarProduct::SpectrumWidth),
            "ZDR" => Some(RadarProduct::DifferentialReflectivity),
            "CC" => Some(RadarProduct::CorrelationCoefficient),
            "KDP" => Some(RadarProduct::DifferentialPhase),
            "CFP" => Some(RadarProduct::ClutterFilterPower),
            _ => None,
        }
    }

    pub fn all() -> &'static [RadarProduct] {
        &[
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialPhase,
            RadarProduct::ClutterFilterPower,
        ]
    }

    /// String identifier used by the worker protocol.
    pub fn to_worker_string(self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "reflectivity",
            RadarProduct::Velocity => "velocity",
            RadarProduct::SpectrumWidth => "spectrum_width",
            RadarProduct::DifferentialReflectivity => "differential_reflectivity",
            RadarProduct::CorrelationCoefficient => "correlation_coefficient",
            RadarProduct::DifferentialPhase => "differential_phase",
            RadarProduct::ClutterFilterPower => "reflectivity", // fallback
        }
    }
}

/// Fully-qualified identity of a single sweep — site, scan, elevation, product.
///
/// The single canonical "which sweep" identifier shared by the render
/// coordinator's dedup cache, the on-GPU `displayed` slot, and the
/// resolver that maps user intent to a concrete render target. By
/// construction, two `SweepIdentity` values compare equal iff they
/// reference the same on-disk sweep blob in IndexedDB.
///
/// `product` is the worker-string form (matches `SweepDataKey` and
/// `RadarProduct::to_worker_string`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SweepIdentity {
    pub scan_key: ScanKey,
    pub elevation_number: u8,
    pub product: String,
}

/// What is currently on the GPU canvas.
///
/// Populated only after a successful `update_data()` call in
/// `handle_decoded_outcome` / `handle_live_decoded_outcome`. Consumed by
/// the timeline (active border), canvas overlay (timestamp, elevation
/// text), and staleness counters.
///
/// This is the "displayed" half of the intent-vs-displayed split: user
/// intent lives in `elevation_selection`, the resolver produces a
/// target, the worker fulfils it, and only then does this slot move.
/// Reading this field is therefore safe to interpret as "what the user
/// is actually looking at right now."
#[derive(Clone, PartialEq, Debug)]
pub struct DisplayedSweep {
    pub identity: SweepIdentity,
    /// Sweep start time (Unix seconds, sub-second precision).
    pub start_time: f64,
    /// Sweep end time (Unix seconds, sub-second precision).
    pub end_time: f64,
    /// Physical elevation angle in degrees, for overlay text.
    pub elevation_deg: f32,
}

impl SweepIdentity {
    pub fn new(scan_key: ScanKey, elevation_number: u8, product: impl Into<String>) -> Self {
        Self {
            scan_key,
            elevation_number,
            product: product.into(),
        }
    }

    /// Sub-second Unix-seconds form of the scan key.
    ///
    /// Used by the timeline to compare against `Scan::key_timestamp` (also
    /// `f64` with sub-second precision); round-trips through `UnixMillis`.
    pub fn scan_timestamp_secs(&self) -> f64 {
        self.scan_key.scan_start.as_secs_f64()
    }

    #[allow(dead_code)] // Read by tests and reserved for cross-site checks.
    pub fn site_id(&self) -> &str {
        &self.scan_key.site.0
    }
}

/// User's elevation selection — by specific VCP cut or auto (latest) mode.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ElevationSelection {
    /// A specific VCP elevation number. The f32 is the angle at time of
    /// selection, used for resilience when VCP changes.
    Fixed { elevation_number: u8, angle: f32 },
    /// Auto: show the most recently completed sweep (any elevation).
    Latest,
}

impl Default for ElevationSelection {
    fn default() -> Self {
        ElevationSelection::Fixed {
            elevation_number: 1,
            angle: 0.5,
        }
    }
}

impl ElevationSelection {
    pub fn is_auto(&self) -> bool {
        matches!(self, ElevationSelection::Latest)
    }

    pub fn elevation_number(&self) -> Option<u8> {
        match self {
            ElevationSelection::Fixed {
                elevation_number, ..
            } => Some(*elevation_number),
            ElevationSelection::Latest => None,
        }
    }

    pub fn angle(&self) -> f32 {
        match self {
            ElevationSelection::Fixed { angle, .. } => *angle,
            ElevationSelection::Latest => 0.5,
        }
    }

    /// On VCP change, find the closest angle match and update elevation_number.
    pub fn resolve_for_vcp(&mut self, entries: &[ElevationListEntry]) {
        if let ElevationSelection::Fixed {
            angle,
            elevation_number,
        } = self
        {
            if let Some(best) = entries.iter().min_by(|a, b| {
                (a.angle - *angle)
                    .abs()
                    .partial_cmp(&(b.angle - *angle).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                *elevation_number = best.elevation_number;
                *angle = best.angle;
            }
        }
    }
}

/// One row in the elevation list UI.
#[derive(Clone, Debug)]
pub struct ElevationListEntry {
    pub elevation_number: u8,
    pub angle: f32,
    pub waveform: String,
    pub is_sails: bool,
    pub is_mrle: bool,
    /// Product names (matching `SweepDataKey` / worker strings) available at
    /// this elevation. Empty means "unknown" — skip product-availability checks.
    pub cached_products: Vec<String>,
}

/// Interpolation mode for radar rendering.
#[derive(Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InterpolationMode {
    /// Raw nearest-neighbor sampling (blocky, traditional).
    #[default]
    Nearest,
    /// Bilinear interpolation between adjacent gates and azimuths.
    Bilinear,
}

impl InterpolationMode {
    pub fn label(&self) -> &'static str {
        match self {
            InterpolationMode::Nearest => "Nearest",
            InterpolationMode::Bilinear => "Bilinear",
        }
    }

    pub fn all() -> &'static [InterpolationMode] {
        &[InterpolationMode::Nearest, InterpolationMode::Bilinear]
    }
}

/// GPU rendering processing options (shader uniforms).
#[derive(Clone)]
pub struct RenderProcessing {
    /// Interpolation mode (nearest vs bilinear).
    pub interpolation: InterpolationMode,
    /// Global opacity for radar data (0.0..1.0).
    pub opacity: f32,
    /// Whether sweep animation is enabled (progressive radial reveal during playback).
    pub sweep_animation: bool,
    /// Whether data age desaturation is shown (desaturates oldest data behind sweep line).
    pub data_age_desaturation: bool,
}

impl Default for RenderProcessing {
    fn default() -> Self {
        Self {
            interpolation: InterpolationMode::Nearest,
            opacity: 1.0,
            sweep_animation: false,
            data_age_desaturation: true,
        }
    }
}

/// Map view mode.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    /// Classic flat equirectangular map.
    #[default]
    Flat2D,
    /// 3D globe.
    Globe3D,
}

/// Lightweight storm cell info for rendering on the canvas.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct StormCellInfo {
    /// Reflectivity-weighted centroid latitude.
    pub lat: f64,
    /// Reflectivity-weighted centroid longitude.
    pub lon: f64,
    /// Maximum reflectivity (dBZ) anywhere in the cell.
    pub max_dbz: f32,
    /// Mean reflectivity (dBZ) across the cell's gates.
    pub mean_dbz: f32,
    /// Cell footprint area in km².
    pub area_km2: f32,
    /// Bounding box (min_lat, min_lon, max_lat, max_lon).
    pub bounds: (f64, f64, f64, f64),
    /// Compass bearing (0° = N, clockwise) from radar to centroid.
    pub bearing_from_radar_deg: f32,
    /// Great-circle-approximate distance from radar to centroid, km.
    pub range_from_radar_km: f32,
    /// Orientation of the cell's major axis in compass degrees, folded
    /// into [0, 180) since an axis is undirected.
    pub orientation_deg: f32,
    /// √(λ_major / λ_minor) from the pixel-weighted covariance. 1.0 = round.
    pub elongation: f32,
    /// Number of gates comprising the cell. Useful for debugging / further
    /// filtering.
    pub gate_count: u32,
}

/// Visualization state including view controls.
pub struct VizState {
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
pub enum CanvasCaption {
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
pub fn derive_canvas_caption(
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
    pub fn view_mode(&self) -> ViewMode {
        self.camera.view_mode()
    }

    /// Whether the flat 2D view is active.
    pub fn is_2d(&self) -> bool {
        self.camera.is_2d()
    }

    /// Current 2D zoom level (1.0 = 100%). Falls back to the default zoom in
    /// 3D modes (the 2D pan/zoom is only meaningful in the Flat2D variant).
    pub fn zoom(&self) -> f32 {
        self.camera.flat_2d().map(|s| s.zoom).unwrap_or(1.0)
    }

    /// Current 2D pan offset. `ZERO` in 3D modes.
    pub fn pan_offset(&self) -> Vec2 {
        self.camera
            .flat_2d()
            .map(|s| s.pan_offset)
            .unwrap_or(Vec2::ZERO)
    }

    /// Set the 2D zoom level. No-op in 3D modes.
    pub fn set_zoom(&mut self, zoom: f32) {
        if let Some(s) = self.camera.flat_2d_mut() {
            s.zoom = zoom;
        }
    }

    /// Set the 2D pan offset. No-op in 3D modes.
    pub fn set_pan_offset(&mut self, pan_offset: Vec2) {
        if let Some(s) = self.camera.flat_2d_mut() {
            s.pan_offset = pan_offset;
        }
    }

    /// Mutable access to the 2D pan offset, if the flat view is active.
    /// Used by the WASD pan path that increments each axis.
    pub fn flat_pan_mut(&mut self) -> Option<&mut Vec2> {
        self.camera.flat_2d_mut().map(|s| &mut s.pan_offset)
    }

    /// Switch the camera into the given 3D mode and remember it as the
    /// mode to return to when toggling 2D → 3D.
    pub fn switch_camera_mode(&mut self, mode: crate::geo::CameraMode) {
        self.last_3d_mode = mode;
        self.camera.switch_to_3d(mode);
    }

    /// Switch the camera to the flat 2D view (default pan/zoom).
    pub fn switch_to_2d(&mut self) {
        self.camera.switch_to_flat_2d(Flat2DState::default());
    }

    /// Toggle between the flat 2D view and the last-used 3D mode. Mirrors
    /// the historical `T` shortcut: 2D → the remembered 3D mode, any 3D
    /// mode → 2D.
    pub fn toggle_2d_3d(&mut self) {
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
    pub fn update_overlay(&mut self, start: f64, end: f64, elevation_deg: f32, now_secs: f64) {
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
    pub fn displayed_midpoint_secs(&self) -> Option<f64> {
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
