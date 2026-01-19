//! Centralized color constants for the UI.
//!
//! This module provides consistent colors across all UI panels.

use eframe::egui::Color32;

/// General UI colors for labels and values.
pub mod ui {
    use super::Color32;

    /// Muted gray for stat labels.
    pub const LABEL: Color32 = Color32::from_rgb(100, 100, 100);
    /// Slightly brighter for stat values.
    pub const VALUE: Color32 = Color32::from_rgb(160, 160, 160);
    /// Emphasized color for active states.
    pub const ACTIVE: Color32 = Color32::from_rgb(100, 180, 255);
    /// Dim text color.
    #[allow(dead_code)] // Available for future UI elements
    pub const DIM: Color32 = Color32::from_rgb(120, 120, 130);
    /// Success/positive indicator.
    pub const SUCCESS: Color32 = Color32::from_rgb(100, 200, 100);
}

/// Colors for live streaming mode indicators.
pub mod live {
    use super::Color32;

    /// Orange - acquiring lock/connecting.
    pub const ACQUIRING: Color32 = Color32::from_rgb(255, 180, 50);
    /// Red - actively streaming.
    pub const STREAMING: Color32 = Color32::from_rgb(255, 80, 80);
    /// Blue - waiting for next chunk.
    pub const WAITING: Color32 = Color32::from_rgb(100, 180, 255);
}

/// Colors for radar sweep visualization.
pub mod radar {
    use super::Color32;

    /// Active sweep line.
    pub const SWEEP_LINE: Color32 = Color32::from_rgb(100, 255, 100);
    /// Current indicator in lists.
    #[allow(dead_code)] // Available for left panel radar state display
    pub const CURRENT_INDICATOR: Color32 = Color32::from_rgb(100, 255, 100);
    /// Dimmed version of current indicator.
    #[allow(dead_code)] // Available for left panel radar state display
    pub const CURRENT_INDICATOR_DIM: Color32 = Color32::from_rgb(80, 200, 80);
}

/// Colors for timeline visualization.
pub mod timeline {
    use super::Color32;

    /// Background color for timeline.
    pub const BACKGROUND: Color32 = Color32::from_rgb(30, 30, 40);
    /// Border color for timeline.
    pub const BORDER: Color32 = Color32::from_rgb(60, 60, 80);
    /// Major tick mark color.
    pub const TICK_MAJOR: Color32 = Color32::from_rgb(120, 120, 140);
    /// Minor tick mark color.
    pub const TICK_MINOR: Color32 = Color32::from_rgb(60, 60, 80);
    /// Tick label text color.
    pub const TICK_LABEL: Color32 = Color32::from_rgb(140, 140, 160);
    /// Selection marker color.
    pub const SELECTION: Color32 = Color32::from_rgb(255, 100, 100);

    /// Scan block fill color.
    pub const SCAN_FILL: Color32 = Color32::from_rgb(60, 120, 80);
    /// Scan block border color.
    pub const SCAN_BORDER: Color32 = Color32::from_rgb(80, 160, 100);
}

/// Colors for the map canvas.
pub mod canvas {
    use super::Color32;

    /// Background color.
    pub const BACKGROUND: Color32 = Color32::from_rgb(20, 20, 35);
    /// Center marker (radar site).
    pub const CENTER_MARKER: Color32 = Color32::from_rgb(180, 180, 200);
    /// Center marker stroke.
    pub const CENTER_MARKER_STROKE: Color32 = Color32::from_rgb(100, 100, 120);

    /// Range ring color (minor rings) - requires alpha, use function.
    pub fn ring() -> Color32 {
        Color32::from_rgba_unmultiplied(60, 80, 60, 120)
    }

    /// Range ring color (major rings) - requires alpha, use function.
    pub fn ring_major() -> Color32 {
        Color32::from_rgba_unmultiplied(80, 100, 80, 150)
    }

    /// Radial line color - requires alpha, use function.
    pub fn radial() -> Color32 {
        Color32::from_rgba_unmultiplied(50, 70, 50, 80)
    }

    /// Cardinal direction label color - requires alpha, use function.
    pub fn cardinal_label() -> Color32 {
        Color32::from_rgba_unmultiplied(120, 140, 120, 200)
    }
}

/// Colors for NWS alert severity badges.
pub mod alerts {
    use super::Color32;

    /// Warning level (red).
    pub const WARNING: Color32 = Color32::from_rgb(255, 80, 80);
    /// Watch level (orange).
    pub const WATCH: Color32 = Color32::from_rgb(255, 180, 50);
    /// Advisory level (yellow).
    pub const ADVISORY: Color32 = Color32::from_rgb(200, 200, 100);
    /// Statement level (blue-gray).
    pub const STATEMENT: Color32 = Color32::from_rgb(140, 140, 180);
}

/// Colors for NEXRAD site markers.
pub mod sites {
    use super::Color32;

    /// Orange for other (non-current) sites.
    pub const OTHER: Color32 = Color32::from_rgb(255, 180, 80);
    /// Orange stroke for other sites.
    pub const OTHER_STROKE: Color32 = Color32::from_rgb(180, 120, 40);
    /// Cyan for current site.
    pub const CURRENT: Color32 = Color32::from_rgb(50, 200, 255);
    /// Cyan stroke for current site.
    pub const CURRENT_STROKE: Color32 = Color32::from_rgb(30, 150, 200);
    /// Label color for other sites.
    pub const LABEL: Color32 = Color32::from_rgb(220, 220, 240);
    /// Label color for current site.
    pub const CURRENT_LABEL: Color32 = Color32::from_rgb(50, 200, 255);
}
