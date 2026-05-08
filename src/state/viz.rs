//! Visualization state (canvas, zoom/pan, product selection).

use crate::data::ScanKey;
use crate::geo::GlobeCamera;
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
#[derive(Default, Clone, Copy, PartialEq, Eq)]
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
    /// Active view mode (flat 2D or 3D globe).
    pub view_mode: ViewMode,

    /// Current zoom level (1.0 = 100%) — used in Flat2D mode.
    pub zoom: f32,

    /// Current pan offset from center — used in Flat2D mode.
    pub pan_offset: Vec2,

    /// Orbital camera for Globe3D mode.
    pub camera: GlobeCamera,

    /// Selected radar product
    pub product: RadarProduct,

    /// Elevation selection (specific VCP cut or auto/latest mode)
    pub elevation_selection: ElevationSelection,

    /// Stored Fixed selection to restore when toggling off auto mode.
    pub last_fixed_selection: Option<(u8, f32)>,

    /// Overlay info: radar site ID
    pub site_id: String,

    /// Overlay info: current timestamp
    pub timestamp: String,

    /// Overlay info: current elevation/sweep
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

    /// Whether the inspector tool is active (hover shows lat/lon and data value).
    pub inspector_enabled: bool,

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
}

impl Default for VizState {
    fn default() -> Self {
        Self {
            view_mode: ViewMode::default(),
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            camera: GlobeCamera::centered_on(41.7312, -93.7229),
            product: RadarProduct::default(),
            elevation_selection: ElevationSelection::default(),
            last_fixed_selection: None,
            site_id: "KDMX".to_string(),
            timestamp: "--:--:-- UTC".to_string(),
            elevation: "-- deg".to_string(),
            center_lat: 41.7312,
            center_lon: -93.7229,
            data_staleness_secs: None,
            data_staleness_start_secs: None,
            last_sweep_line_cache: None,
            volume_3d_enabled: false,
            volume_density_cutoff: 5.0,
            inspector_enabled: false,
            distance_tool_active: false,
            distance_start: None,
            distance_end: None,
            storm_cells_visible: false,
            storm_cell_threshold_dbz: 35.0,
            detected_storm_cells: Vec::new(),
            last_visible_bounds: None,
            displayed: None,
            previous_displayed: None,
        }
    }
}

impl VizState {
    /// Update the canvas overlay text with sweep timing and elevation info.
    /// Sweep start/end times are stored on `displayed` (set by the decode
    /// handler); staleness is recomputed each frame from there.
    pub fn update_overlay(
        &mut self,
        start: f64,
        end: f64,
        elevation_deg: f32,
        use_local_time: bool,
    ) {
        self.elevation = format!("{:.2}\u{00B0}", elevation_deg);

        // Format midpoint timestamp with full date and time
        let mid_ms = ((start + end) / 2.0) * 1000.0;
        let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(mid_ms));
        if use_local_time {
            self.timestamp = format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
                date.get_full_year(),
                date.get_month() + 1,
                date.get_date(),
                date.get_hours(),
                date.get_minutes(),
                date.get_seconds(),
                date.get_milliseconds()
            );
        } else {
            self.timestamp = format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} UTC",
                date.get_utc_full_year(),
                date.get_utc_month() + 1,
                date.get_utc_date(),
                date.get_utc_hours(),
                date.get_utc_minutes(),
                date.get_utc_seconds(),
                date.get_utc_milliseconds()
            );
        }

        // Seed staleness for immediate display; the per-frame recompute
        // in `update()` keeps it ticking from `displayed`.
        let now = js_sys::Date::now() / 1000.0;
        let staleness_end = now - end;
        let staleness_start = now - start;
        self.data_staleness_secs = (staleness_end >= 0.0).then_some(staleness_end);
        self.data_staleness_start_secs = (staleness_start >= 0.0).then_some(staleness_start);
    }
}
