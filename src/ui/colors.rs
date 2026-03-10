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
    use crate::data::ScanCompleteness;

    pub fn background(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(10, 10, 14)
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
    /// Track separator line color.
    pub fn track_separator() -> Color32 {
        Color32::from_rgba_unmultiplied(100, 100, 130, 80)
    }
    /// Connector line from scan boundary into sweep track.
    pub fn connector() -> Color32 {
        Color32::from_rgba_unmultiplied(120, 120, 150, 60)
    }
    /// Estimated future scan boundary (dashed, very subtle).
    pub fn estimated_boundary() -> Color32 {
        Color32::from_rgba_unmultiplied(180, 200, 255, 50)
    }

    // ── Scan track colors (warm palette) ──────────────────────────────

    /// Base RGB for a VCP category. Warmer and more saturated than the old
    /// single-lane palette so scan blocks pop against the dark background.
    pub fn vcp_base_rgb(vcp: u16) -> (u8, u8, u8) {
        match vcp {
            // Precipitation modes — warm green
            215 => (55, 130, 75),
            212 => (60, 120, 80),
            // Clear air modes — warm blue
            31 | 32 | 35 => (55, 100, 155),
            // Severe weather modes — warm orange
            12 | 121 => (175, 100, 50),
            // Other known VCPs — teal
            _ if vcp > 0 => (60, 110, 110),
            // Unknown — gray
            _ => (80, 80, 80),
        }
    }

    /// Fill color for a scan block on the scan track.
    pub fn scan_fill(vcp: u16, completeness: Option<ScanCompleteness>) -> Color32 {
        let (r, g, b) = vcp_base_rgb(vcp);
        let alpha = match completeness {
            Some(ScanCompleteness::Complete) | None => 210u8,
            Some(ScanCompleteness::PartialWithVcp) => 170,
            Some(ScanCompleteness::PartialNoVcp) => 130,
            Some(ScanCompleteness::Missing) => 60,
        };
        Color32::from_rgba_unmultiplied(r, g, b, alpha)
    }

    /// Border color for a scan block.
    pub fn scan_border(vcp: u16, completeness: Option<ScanCompleteness>) -> Color32 {
        let (r, g, b) = vcp_base_rgb(vcp);
        let alpha = match completeness {
            Some(ScanCompleteness::Complete) | None => 180u8,
            Some(ScanCompleteness::PartialWithVcp) => 140,
            Some(ScanCompleteness::PartialNoVcp) => 100,
            Some(ScanCompleteness::Missing) => 50,
        };
        Color32::from_rgba_unmultiplied(
            (r as u16 * 6 / 10) as u8,
            (g as u16 * 6 / 10) as u8,
            (b as u16 * 6 / 10) as u8,
            alpha,
        )
    }

    /// Hatch line color for PartialWithVcp scans (diagonal stripes).
    pub fn scan_hatch(vcp: u16) -> Color32 {
        let (r, g, b) = vcp_base_rgb(vcp);
        Color32::from_rgba_unmultiplied(
            (r as u16 + 40).min(255) as u8,
            (g as u16 + 40).min(255) as u8,
            (b as u16 + 40).min(255) as u8,
            90,
        )
    }

    // ── Sweep track colors (cool palette) ─────────────────────────────

    /// Fill color for a sweep block. Maps elevation angle (0–20 deg)
    /// from deep indigo (low) to bright cyan (high).
    pub fn sweep_fill(elevation: f32, is_target: bool) -> Color32 {
        let t = (elevation / 20.0).clamp(0.0, 1.0);
        // Indigo → cyan gradient
        let r = (30.0 + t * 20.0) as u8; //  30– 50
        let g = (40.0 + t * 80.0) as u8; //  40–120
        let b = (90.0 + t * 70.0) as u8; //  90–160
        let alpha = if is_target { 220u8 } else { 120 };
        Color32::from_rgba_unmultiplied(r, g, b, alpha)
    }

    /// Border color for a sweep block.
    pub fn sweep_border(elevation: f32, is_active: bool) -> Color32 {
        if is_active {
            return ACTIVE_SWEEP;
        }
        let t = (elevation / 20.0).clamp(0.0, 1.0);
        let r = (20.0 + t * 15.0) as u8;
        let g = (30.0 + t * 60.0) as u8;
        let b = (70.0 + t * 50.0) as u8;
        Color32::from_rgba_unmultiplied(r, g, b, 100)
    }

    // ── Ghost / process state colors ──────────────────────────────────

    /// Ghost block for pending (queued) downloads — blue tint outline.
    pub fn ghost_pending_fill() -> Color32 {
        Color32::from_rgba_unmultiplied(100, 150, 255, 30)
    }
    pub fn ghost_pending_border() -> Color32 {
        Color32::from_rgba_unmultiplied(100, 150, 255, 55)
    }

    /// Ghost block for processing (ingesting after download) — amber.
    pub fn ghost_processing_border() -> Color32 {
        Color32::from_rgba_unmultiplied(200, 160, 60, 70)
    }

    /// Pending (expected but not yet received) sweep placeholder.
    pub fn rt_pending_sweep_border() -> Color32 {
        Color32::from_rgba_unmultiplied(80, 120, 180, 60)
    }

    // ── Shadow scan boundary colors ──────────────────────────────────

    /// Fill color for shadow scan boundaries from the archive index.
    /// Very subtle so they don't compete with real (downloaded) scan blocks.
    pub fn shadow_fill() -> Color32 {
        Color32::from_rgba_unmultiplied(120, 140, 180, 25)
    }

    /// Border color for shadow scan boundaries.
    pub fn shadow_border() -> Color32 {
        Color32::from_rgba_unmultiplied(120, 140, 180, 45)
    }
}

/// Colors for the map canvas.
pub mod canvas {
    use super::Color32;

    pub fn background(dark: bool) -> Color32 {
        if dark {
            Color32::BLACK
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
