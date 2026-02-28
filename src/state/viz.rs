//! Visualization state (canvas, zoom/pan, product/palette selection).

use eframe::egui::Vec2;

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
        ]
    }
}

// ============================================================================
// Render Mode System
// ============================================================================

/// Radar rendering mode per PRODUCT.md specification.
///
/// Determines how radar data is selected and displayed on the canvas.
#[derive(Default, Clone, Copy, PartialEq)]
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
#[derive(Default, Clone, Copy, PartialEq, Eq)]
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
