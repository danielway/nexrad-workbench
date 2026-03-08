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

/// Radar rendering mode.
#[derive(Default, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RenderMode {
    /// Fixed elevation - shows complete sweep at a specific tilt.
    #[default]
    FixedTilt,

    /// Most recent sweep regardless of elevation.
    MostRecent,
}

impl RenderMode {
    pub fn label(&self) -> &'static str {
        match self {
            RenderMode::FixedTilt => "Fixed Tilt",
            RenderMode::MostRecent => "Most Recent",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            RenderMode::FixedTilt => "Shows complete sweep at selected elevation",
            RenderMode::MostRecent => "Shows most recent sweep regardless of elevation",
        }
    }

    pub fn all() -> &'static [RenderMode] {
        &[RenderMode::FixedTilt, RenderMode::MostRecent]
    }
}

/// Interpolation mode for radar rendering.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
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
    /// Whether Gaussian smoothing is enabled.
    pub smoothing_enabled: bool,
    /// Smoothing kernel radius in samples (1.0..5.0).
    pub smoothing_radius: f32,
    /// Whether despeckle filtering is enabled.
    pub despeckle_enabled: bool,
    /// Minimum valid neighbors to keep a pixel (1..6).
    pub despeckle_threshold: u32,
    /// Global opacity for radar data (0.0..1.0).
    pub opacity: f32,
    /// Whether edge softening is enabled (smooth alpha falloff at echo boundaries).
    pub edge_softening: bool,
}

impl Default for RenderProcessing {
    fn default() -> Self {
        Self {
            interpolation: InterpolationMode::Nearest,
            smoothing_enabled: false,
            smoothing_radius: 2.0,
            despeckle_enabled: false,
            despeckle_threshold: 3,
            opacity: 1.0,
            edge_softening: false,
        }
    }
}

/// Map view mode.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Classic flat equirectangular map.
    Flat2D,
    /// 3D globe.
    #[default]
    Globe3D,
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

    /// Render mode (fixed-tilt vs most-recent)
    pub render_mode: RenderMode,

    /// Target elevation for fixed-tilt mode (degrees)
    pub target_elevation: f32,

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

    /// Staleness of the currently displayed data in seconds (for fixed-tilt mode).
    pub data_staleness_secs: Option<f64>,

    /// End timestamp (Unix seconds) of the currently rendered sweep.
    /// Used to recompute `data_staleness_secs` every frame so the age counter ticks.
    pub rendered_sweep_end_secs: Option<f64>,

    /// Whether 3D volumetric rendering is enabled (ray-marched volume).
    pub volume_3d_enabled: bool,

    /// Density cutoff for volume rendering (physical value, e.g. 5.0 dBZ).
    pub volume_density_cutoff: f32,
}

impl Default for VizState {
    fn default() -> Self {
        Self {
            view_mode: ViewMode::default(),
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            camera: GlobeCamera::centered_on(41.7312, -93.7229),
            product: RadarProduct::default(),
            render_mode: RenderMode::default(),
            target_elevation: 0.5,
            site_id: "KDMX".to_string(),
            timestamp: "--:--:-- UTC".to_string(),
            elevation: "-- deg".to_string(),
            center_lat: 41.7312,
            center_lon: -93.7229,
            data_staleness_secs: None,
            rendered_sweep_end_secs: None,
            volume_3d_enabled: false,
            volume_density_cutoff: 5.0,
        }
    }
}
