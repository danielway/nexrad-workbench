//! Visualization state (canvas, zoom/pan, product/palette selection, processing options).

use eframe::egui::Vec2;

// ============================================================================
// Processing Options
// ============================================================================

/// State for radar data processing options.
#[derive(Default)]
pub struct ProcessingState {
    /// Enable spatial smoothing.
    pub smoothing_enabled: bool,
    /// Smoothing strength (0.0 - 1.0).
    pub smoothing_strength: f32,
    /// Enable velocity dealiasing.
    pub dealiasing_enabled: bool,
    /// Dealiasing aggressiveness (0.0 - 1.0).
    pub dealiasing_strength: f32,
}

impl ProcessingState {
    #[allow(dead_code)] // Convenience constructor with non-default values
    pub fn new() -> Self {
        Self {
            smoothing_strength: 0.5,
            dealiasing_strength: 0.5,
            ..Default::default()
        }
    }
}

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
    /// Data may become stale as radar continues scanning other elevations.
    #[default]
    FixedTilt,

    /// Most recent data - blends data from multiple sweeps to show
    /// the most temporally immediate data at each azimuth.
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

/// Strategy for handling sweep transitions in MostRecent mode.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum BlendStrategy {
    /// Continuously wipe older data as sweep progresses
    #[default]
    ContinuousWipe,
    /// Clear display at sweep end, start fresh
    ClearOnSweepEnd,
}

impl BlendStrategy {
    pub fn label(&self) -> &'static str {
        match self {
            BlendStrategy::ContinuousWipe => "Continuous Wipe",
            BlendStrategy::ClearOnSweepEnd => "Clear on Sweep End",
        }
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

    /// Blend strategy for most-recent mode
    pub blend_strategy: BlendStrategy,

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
            render_mode: RenderMode::default(),
            target_elevation: 0.5, // Default lowest tilt
            blend_strategy: BlendStrategy::default(),
            site_id: "KDMX".to_string(),
            timestamp: "--:--:-- UTC".to_string(),
            elevation: "-- deg".to_string(),
            // Default to KDMX - Des Moines, Iowa
            center_lat: 41.7312,
            center_lon: -93.7229,
        }
    }
}
