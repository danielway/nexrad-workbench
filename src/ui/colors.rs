//! Centralized color constants for the UI.
//!
//! Colors that vary between dark and light themes provide `for_theme(dark: bool)`
//! functions. Theme-independent colors (live indicators, site markers, etc.)
//! remain as constants.

use eframe::egui::Color32;

/// General UI colors for labels and values.
pub mod ui {
    use super::Color32;

    pub fn label(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(100, 100, 100)
        } else {
            Color32::from_rgb(120, 120, 120)
        }
    }

    pub fn value(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(160, 160, 160)
        } else {
            Color32::from_rgb(60, 60, 60)
        }
    }

    /// Emphasized color for active states (theme-independent).
    pub const ACTIVE: Color32 = Color32::from_rgb(100, 180, 255);
    /// Success/positive indicator (theme-independent).
    pub const SUCCESS: Color32 = Color32::from_rgb(100, 200, 100);
}

/// Colors for live streaming mode indicators (theme-independent).
pub mod live {
    use super::Color32;

    /// Orange - acquiring lock/connecting.
    pub const ACQUIRING: Color32 = Color32::from_rgb(255, 180, 50);
    /// Red - actively streaming.
    pub const STREAMING: Color32 = Color32::from_rgb(255, 80, 80);
    /// Blue - waiting for next chunk.
    pub const WAITING: Color32 = Color32::from_rgb(100, 180, 255);
}

/// Colors for radar sweep visualization (theme-independent).
pub mod radar {
    use super::Color32;

    /// Active sweep line.
    pub const SWEEP_LINE: Color32 = Color32::from_rgb(100, 255, 100);
}

/// Colors for timeline visualization.
pub mod timeline {
    use super::Color32;

    pub fn background(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(30, 30, 40)
        } else {
            Color32::from_rgb(230, 230, 235)
        }
    }

    pub fn border(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(60, 60, 80)
        } else {
            Color32::from_rgb(180, 180, 195)
        }
    }

    pub fn tick_major(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(120, 120, 140)
        } else {
            Color32::from_rgb(80, 80, 100)
        }
    }

    pub fn tick_minor(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(60, 60, 80)
        } else {
            Color32::from_rgb(170, 170, 185)
        }
    }

    pub fn tick_label(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(140, 140, 160)
        } else {
            Color32::from_rgb(60, 60, 80)
        }
    }

    /// Selection marker color (theme-independent).
    pub const SELECTION: Color32 = Color32::from_rgb(255, 100, 100);
    /// Active sweep highlight (theme-independent).
    pub const ACTIVE_SWEEP: Color32 = Color32::from_rgb(255, 255, 100);
    /// "Now" marker (current wall-clock time).
    pub const NOW_MARKER: Color32 = Color32::from_rgb(180, 200, 255);
    /// Selection range boundary label color.
    pub const SELECTION_LABEL: Color32 = Color32::from_rgb(140, 180, 255);
}

/// Colors for the map canvas.
pub mod canvas {
    use super::Color32;

    pub fn background(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(20, 20, 35)
        } else {
            Color32::from_rgb(235, 235, 240)
        }
    }

    pub fn center_marker(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(180, 180, 200)
        } else {
            Color32::from_rgb(80, 80, 100)
        }
    }

    pub fn center_marker_stroke(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(100, 100, 120)
        } else {
            Color32::from_rgb(60, 60, 80)
        }
    }

    pub fn ring(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgba_unmultiplied(60, 80, 60, 120)
        } else {
            Color32::from_rgba_unmultiplied(100, 120, 100, 100)
        }
    }

    pub fn ring_major(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgba_unmultiplied(80, 100, 80, 150)
        } else {
            Color32::from_rgba_unmultiplied(80, 100, 80, 130)
        }
    }

    pub fn radial(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgba_unmultiplied(50, 70, 50, 80)
        } else {
            Color32::from_rgba_unmultiplied(100, 120, 100, 60)
        }
    }

    pub fn cardinal_label(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgba_unmultiplied(120, 140, 120, 200)
        } else {
            Color32::from_rgba_unmultiplied(60, 80, 60, 200)
        }
    }
}

/// Colors for NEXRAD site markers (theme-independent).
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
