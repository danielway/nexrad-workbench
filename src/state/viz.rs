//! Visualization state (canvas, zoom/pan, product/palette selection).

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
}

impl Default for VizState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            product: RadarProduct::default(),
            palette: ColorPalette::default(),
            site_id: "KDMX".to_string(),
            timestamp: "--:--:-- UTC".to_string(),
            elevation: "-- deg".to_string(),
            // Default to KDMX - Des Moines, Iowa
            center_lat: 41.7312,
            center_lon: -93.7229,
        }
    }
}
