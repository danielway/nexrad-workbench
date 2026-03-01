//! Visualization state (canvas, zoom/pan, product/palette selection).

use eframe::egui::Vec2;
use nexrad_render::{Interpolation, Product};

// ============================================================================
// Product and Palette Selection
// ============================================================================

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

    /// String identifier used by the worker render protocol.
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

    /// Convert to the nexrad-render Product type.
    pub fn to_render_product(self) -> Product {
        match self {
            RadarProduct::Reflectivity => Product::Reflectivity,
            RadarProduct::Velocity => Product::Velocity,
            RadarProduct::SpectrumWidth => Product::SpectrumWidth,
            RadarProduct::DifferentialReflectivity => Product::DifferentialReflectivity,
            RadarProduct::CorrelationCoefficient => Product::CorrelationCoefficient,
            RadarProduct::DifferentialPhase => Product::DifferentialPhase,
            RadarProduct::ClutterFilterPower => Product::ClutterFilterPower,
        }
    }
}

// ============================================================================
// Render Mode System
// ============================================================================

/// Radar rendering mode per PRODUCT.md specification.
///
/// Determines how radar data is selected and displayed on the canvas.
#[derive(Default, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RenderMode {
    /// Fixed elevation - shows complete sweep at a specific tilt.
    #[default]
    FixedTilt,

    /// Most recent data - shows the most recent eligible value at each
    /// azimuth/range regardless of elevation.
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
            RenderMode::MostRecent => "Shows most recent data across all tilts",
        }
    }

    pub fn all() -> &'static [RenderMode] {
        &[RenderMode::FixedTilt, RenderMode::MostRecent]
    }
}

// ============================================================================
// Color Palettes
// ============================================================================

/// Available color palettes for rendering.
#[derive(Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ColorPalette {
    #[default]
    Standard,
    Enhanced,
    ColorBlindSafe,
    Monochrome,
}

impl ColorPalette {
    pub fn label(&self) -> &'static str {
        match self {
            ColorPalette::Standard => "Standard",
            ColorPalette::Enhanced => "Enhanced",
            ColorPalette::ColorBlindSafe => "Color-blind Safe",
            ColorPalette::Monochrome => "Monochrome",
        }
    }

    pub fn all() -> &'static [ColorPalette] {
        &[
            ColorPalette::Standard,
            ColorPalette::Enhanced,
            ColorPalette::ColorBlindSafe,
            ColorPalette::Monochrome,
        ]
    }
}

// ============================================================================
// Processing Configuration
// ============================================================================

/// Smoothing algorithm selection.
#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SmoothingMode {
    /// No smoothing applied
    #[default]
    None,
    /// Median filter (removes spikes while preserving edges)
    Median,
    /// Gaussian smoothing (overall blur)
    Gaussian,
}

impl SmoothingMode {
    pub fn label(self) -> &'static str {
        match self {
            SmoothingMode::None => "None",
            SmoothingMode::Median => "Median",
            SmoothingMode::Gaussian => "Gaussian",
        }
    }

    pub fn all() -> &'static [SmoothingMode] {
        &[
            SmoothingMode::None,
            SmoothingMode::Median,
            SmoothingMode::Gaussian,
        ]
    }
}

/// Processing pipeline configuration applied before rendering.
#[derive(Clone, Copy)]
pub struct ProcessingConfig {
    /// Whether processing is enabled at all
    pub enabled: bool,
    /// Minimum value threshold (gates below are masked). Product-dependent units.
    pub threshold_min: Option<f32>,
    /// Maximum value threshold (gates above are masked).
    pub threshold_max: Option<f32>,
    /// Smoothing algorithm
    pub smoothing: SmoothingMode,
    /// Smoothing kernel size (for median: odd integer 3-9; for gaussian: sigma 0.5-5.0)
    pub smoothing_strength: u8,
}

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_min: None,
            threshold_max: None,
            smoothing: SmoothingMode::None,
            smoothing_strength: 3,
        }
    }
}

impl ProcessingConfig {
    /// Compute a hash for cache key discrimination.
    pub fn cache_hash(self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.enabled.hash(&mut hasher);
        if self.enabled {
            self.threshold_min.map(|v| v.to_bits()).hash(&mut hasher);
            self.threshold_max.map(|v| v.to_bits()).hash(&mut hasher);
            self.smoothing.hash(&mut hasher);
            self.smoothing_strength.hash(&mut hasher);
        }
        hasher.finish()
    }
}

// ============================================================================
// Interpolation Mode
// ============================================================================

/// Available interpolation modes for radar rendering.
#[derive(Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InterpolationMode {
    /// Nearest-neighbor sampling (fastest, produces blocky output)
    #[default]
    Nearest,
    /// Bilinear interpolation (smoother, anti-aliased output)
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

    /// String identifier used by the worker render protocol.
    pub fn to_worker_string(self) -> &'static str {
        match self {
            InterpolationMode::Nearest => "nearest",
            InterpolationMode::Bilinear => "bilinear",
        }
    }

    /// Convert to the nexrad-render Interpolation type.
    pub fn to_render_interpolation(self) -> Interpolation {
        match self {
            InterpolationMode::Nearest => Interpolation::Nearest,
            InterpolationMode::Bilinear => Interpolation::Bilinear,
        }
    }
}

/// Visualization state including view controls.
pub struct VizState {
    /// Current zoom level (1.0 = 100%)
    pub zoom: f32,

    /// Current pan offset from center
    pub pan_offset: Vec2,

    /// Selected radar product
    pub product: RadarProduct,

    /// Selected color palette
    pub palette: ColorPalette,

    /// Render mode (fixed-tilt vs most-recent)
    pub render_mode: RenderMode,

    /// Interpolation mode for rendering
    pub interpolation: InterpolationMode,

    /// Data processing pipeline settings
    pub processing: ProcessingConfig,

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
}

impl Default for VizState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            product: RadarProduct::default(),
            palette: ColorPalette::default(),
            render_mode: RenderMode::default(),
            interpolation: InterpolationMode::default(),
            processing: ProcessingConfig::default(),
            target_elevation: 0.5,
            site_id: "KDMX".to_string(),
            timestamp: "--:--:-- UTC".to_string(),
            elevation: "-- deg".to_string(),
            center_lat: 41.7312,
            center_lon: -93.7229,
            data_staleness_secs: None,
        }
    }
}
