//! Visualization state (canvas, zoom/pan, product selection).

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
    /// Whether despeckle filtering is enabled.
    pub despeckle_enabled: bool,
    /// Minimum valid neighbors to keep a pixel (1..8).
    pub despeckle_threshold: u32,
    /// Global opacity for radar data (0.0..1.0).
    pub opacity: f32,
    /// Whether sweep animation is enabled (progressive radial reveal during playback).
    pub sweep_animation: bool,
    /// Whether data age indicator is shown (desaturates oldest data behind sweep line).
    pub data_age_indicator: bool,
}

impl Default for RenderProcessing {
    fn default() -> Self {
        Self {
            interpolation: InterpolationMode::Nearest,
            despeckle_enabled: false,
            despeckle_threshold: 3,
            opacity: 1.0,
            sweep_animation: false,
            data_age_indicator: true,
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
    /// Centroid latitude.
    pub lat: f64,
    /// Centroid longitude.
    pub lon: f64,
    /// Maximum reflectivity (dBZ).
    pub max_dbz: f32,
    /// Cell area in km^2.
    pub area_km2: f32,
    /// Bounding box (min_lat, min_lon, max_lat, max_lon).
    pub bounds: (f64, f64, f64, f64),
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

    /// Cached elevation list from the current VCP, for the UI list.
    pub cached_vcp_elevations: Vec<ElevationListEntry>,

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
    pub data_staleness_secs: Option<f64>,

    /// Staleness of the oldest radial (sweep start) in seconds.
    pub data_staleness_start_secs: Option<f64>,

    /// Start timestamp (Unix seconds) of the currently rendered sweep.
    /// Used to recompute `data_staleness_start_secs` every frame.
    pub rendered_sweep_start_secs: Option<f64>,

    /// End timestamp (Unix seconds) of the currently rendered sweep.
    /// Used to recompute `data_staleness_secs` every frame so the age counter ticks.
    pub rendered_sweep_end_secs: Option<f64>,

    /// Previous sweep info for overlay display during sweep animation.
    /// Contains (elevation_deg, start_time_secs, end_time_secs).
    pub prev_sweep_overlay: Option<(f32, f64, f64)>,

    /// Scan timestamp of the previous sweep (for timeline secondary highlight).
    pub prev_sweep_scan_timestamp: Option<i64>,

    /// Elevation number of the previous sweep (for timeline secondary highlight).
    pub prev_sweep_elevation_number: Option<u8>,

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

    /// Timestamp of the currently displayed scan (seconds since epoch).
    pub displayed_scan_timestamp: Option<i64>,

    /// Elevation number of the currently displayed sweep.
    pub displayed_sweep_elevation_number: Option<u8>,

    /// Last observed visible map bounds in 2D mode, as
    /// `(min_lon, min_lat, max_lon, max_lat)`. Updated each frame by the
    /// canvas renderer and consumed by top-bar / modal logic that needs
    /// to know what area the user is looking at without access to the
    /// canvas rect. `None` while in 3D globe mode.
    pub last_visible_bounds: Option<(f64, f64, f64, f64)>,
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
            cached_vcp_elevations: Vec::new(),
            last_fixed_selection: None,
            site_id: "KDMX".to_string(),
            timestamp: "--:--:-- UTC".to_string(),
            elevation: "-- deg".to_string(),
            center_lat: 41.7312,
            center_lon: -93.7229,
            data_staleness_secs: None,
            data_staleness_start_secs: None,
            rendered_sweep_start_secs: None,
            rendered_sweep_end_secs: None,
            prev_sweep_overlay: None,
            prev_sweep_scan_timestamp: None,
            prev_sweep_elevation_number: None,
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
            displayed_scan_timestamp: None,
            displayed_sweep_elevation_number: None,
            last_visible_bounds: None,
        }
    }
}

impl VizState {
    /// Update the canvas overlay text with sweep timing and elevation info.
    pub fn update_overlay(
        &mut self,
        start: f64,
        end: f64,
        elevation_deg: f32,
        use_local_time: bool,
    ) {
        self.elevation = format!("{:.1}\u{00B0}", elevation_deg);

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

        // Store sweep start/end times so staleness can be recomputed each frame
        self.rendered_sweep_start_secs = Some(start);
        self.rendered_sweep_end_secs = Some(end);
        // Staleness is recomputed per-frame in update(); seed it here for immediate display
        let now = js_sys::Date::now() / 1000.0;
        let staleness_end = now - end;
        let staleness_start = now - start;
        self.data_staleness_secs = if staleness_end >= 0.0 {
            Some(staleness_end)
        } else {
            None
        };
        self.data_staleness_start_secs = if staleness_start >= 0.0 {
            Some(staleness_start)
        } else {
            None
        };
    }
}
