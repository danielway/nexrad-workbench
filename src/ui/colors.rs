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

/// Top-level application mode indicator colors (theme-independent).
pub mod mode {
    use super::Color32;

    /// Idle - no data under cursor, not streaming.
    pub const IDLE: Color32 = Color32::from_rgb(100, 100, 100);
    /// Archive - data loaded and under cursor.
    pub const ARCHIVE: Color32 = Color32::from_rgb(100, 180, 255);
    /// Live - real-time streaming locked to now.
    pub const LIVE: Color32 = Color32::from_rgb(255, 80, 80);
}

/// Colors for radar sweep visualization (theme-independent).
pub mod radar {
    use super::Color32;

    /// Active sweep line.
    pub const SWEEP_LINE: Color32 = Color32::from_rgb(100, 255, 100);
    /// Sweep start boundary line (blue-purple, matches previous sweep arc).
    pub fn sweep_start_line() -> Color32 {
        Color32::from_rgba_unmultiplied(160, 160, 220, 180)
    }
    /// Stale sweep line (muted grey, shown between sweeps).
    pub fn sweep_line_stale() -> Color32 {
        Color32::from_rgba_unmultiplied(120, 120, 120, 140)
    }
    /// Stale sweep start line (muted grey-blue, shown between sweeps).
    pub fn sweep_start_line_stale() -> Color32 {
        Color32::from_rgba_unmultiplied(100, 100, 120, 100)
    }
}

/// Colors for timeline visualization.
pub mod timeline {
    use super::Color32;

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

    /// Playback-position cursor ("the needle") and current-time readout.
    /// A neutral, high-contrast tone deliberately distinct from the red
    /// now/live family so the "where I'm looking" marker never reads as
    /// "now". Theme-aware: near-white on dark, near-black on light.
    pub fn selection(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgb(235, 240, 250)
        } else {
            Color32::from_rgb(40, 45, 60)
        }
    }
    /// Active sweep highlight (theme-independent).
    pub const ACTIVE_SWEEP: Color32 = Color32::from_rgb(255, 255, 100);
    /// Previous active sweep highlight during sweep animation (theme-independent).
    pub const PREV_ACTIVE_SWEEP: Color32 = Color32::from_rgb(160, 200, 255);
    /// The now-line / "GO LIVE" cap when NOT streaming — a calm, muted red
    /// that reads as an invitation. Red is reserved exclusively for the
    /// now/live concept (see [`LIVE_ACTIVE`] for the streaming state).
    pub const NOW_IDLE: Color32 = Color32::from_rgb(200, 95, 95);
    /// Live status color for the now-line / "LIVE" cap when streaming —
    /// the bright, active end of the same red.
    pub const LIVE_ACTIVE: Color32 = Color32::from_rgb(255, 80, 80);
    /// Selection range boundary label color.
    pub const SELECTION_LABEL: Color32 = Color32::from_rgb(140, 180, 255);
    /// Text drawn inside scan/sweep blocks. Theme-independent because it
    /// sits on the blocks' own fill colors, not the panel background.
    pub fn block_label() -> Color32 {
        Color32::from_rgba_unmultiplied(225, 232, 248, 200)
    }
    /// De-emphasized in-block text (ghost/projected blocks).
    pub fn block_label_weak() -> Color32 {
        Color32::from_rgba_unmultiplied(225, 232, 248, 110)
    }
    /// Track separator line color.
    pub fn track_separator() -> Color32 {
        Color32::from_rgba_unmultiplied(100, 100, 130, 80)
    }
    /// Lane-name header text (VOLUMES / TILTS).
    pub fn track_header(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgba_unmultiplied(150, 155, 175, 130)
        } else {
            Color32::from_rgba_unmultiplied(70, 75, 95, 150)
        }
    }
    /// Backdrop chip behind lane-name headers so they stay readable on
    /// top of block content.
    pub fn track_header_backdrop(dark: bool) -> Color32 {
        let c = background(dark);
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 180)
    }
    /// Connector line from scan boundary into sweep track.
    pub fn connector() -> Color32 {
        Color32::from_rgba_unmultiplied(120, 120, 150, 60)
    }
    /// Estimated future scan boundary (dashed).
    pub fn estimated_boundary() -> Color32 {
        Color32::from_rgba_unmultiplied(180, 200, 255, 90)
    }

    // ── Scan track colors: cached vs available ────────────────────────
    //
    // The scan track's color answers exactly one question — the data's
    // relationship to the user. Solid steel blue = on this device;
    // hollow slate = listed in the cloud archive but not downloaded.
    // VCP identity and exact sweep counts are text, not color.

    /// Base RGB for data that is on the device (steel blue).
    const CACHED_RGB: (u8, u8, u8) = (88, 130, 178);
    /// Base RGB for archive data not yet downloaded (desaturated slate).
    const AVAILABLE_RGB: (u8, u8, u8) = (130, 150, 185);

    /// Fill for an on-device scan block. Partial scans (some sweeps still
    /// missing) keep the same hue at reduced alpha — two tiers only; the
    /// exact count is carried by the block label and tooltip.
    pub fn cached_fill(dark: bool, partial: bool) -> Color32 {
        let (r, g, b) = if dark { CACHED_RGB } else { (70, 110, 160) };
        let alpha = if partial { 120 } else { 235 };
        Color32::from_rgba_unmultiplied(r, g, b, alpha)
    }

    /// Border for an on-device scan block. Full alpha even on partial
    /// blocks so the block's extent stays crisp.
    pub fn cached_border(dark: bool, _partial: bool) -> Color32 {
        if dark {
            Color32::from_rgb(53, 78, 107)
        } else {
            Color32::from_rgb(45, 70, 105)
        }
    }

    /// Faint interior wash for an available-but-not-downloaded block.
    pub fn available_fill(dark: bool) -> Color32 {
        let (r, g, b) = if dark { AVAILABLE_RGB } else { (90, 110, 150) };
        Color32::from_rgba_unmultiplied(r, g, b, 36)
    }

    /// Dashed border for an available-but-not-downloaded block.
    pub fn available_border(dark: bool) -> Color32 {
        let (r, g, b) = if dark { AVAILABLE_RGB } else { (90, 110, 150) };
        Color32::from_rgba_unmultiplied(r, g, b, 120)
    }

    /// Cloud glyph drawn inside available blocks.
    pub fn available_glyph(dark: bool) -> Color32 {
        let (r, g, b) = if dark { AVAILABLE_RGB } else { (90, 110, 150) };
        Color32::from_rgba_unmultiplied(r, g, b, 110)
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

    // ── Realtime (live volume) overlay colors ─────────────────────────
    //
    // Download-ghost colors live in [`super::acquisition`] so the timeline
    // ghosts and the acquisition drawer share one palette.

    /// Elapsed portion of the live in-progress volume — the same steel
    /// blue as a cached block at reduced alpha, so the block visually
    /// "becomes" a cached block as it fills in.
    pub fn live_elapsed_fill() -> Color32 {
        let (r, g, b) = CACHED_RGB;
        Color32::from_rgba_unmultiplied(r, g, b, 160)
    }
    pub fn live_elapsed_border() -> Color32 {
        let (r, g, b) = CACHED_RGB;
        Color32::from_rgba_unmultiplied(r, g, b, 180)
    }

    /// Projected remainder of the live in-progress volume.
    pub fn live_projected_fill() -> Color32 {
        let (r, g, b) = CACHED_RGB;
        Color32::from_rgba_unmultiplied(r, g, b, 55)
    }
    pub fn live_projected_border() -> Color32 {
        let (r, g, b) = CACHED_RGB;
        Color32::from_rgba_unmultiplied(r, g, b, 90)
    }

    /// Projected next-volume ghost — available-style slate, since the
    /// volume doesn't exist yet.
    pub fn next_volume_fill() -> Color32 {
        let (r, g, b) = AVAILABLE_RGB;
        Color32::from_rgba_unmultiplied(r, g, b, 28)
    }
    pub fn next_volume_border() -> Color32 {
        let (r, g, b) = AVAILABLE_RGB;
        Color32::from_rgba_unmultiplied(r, g, b, 70)
    }

    /// Received chunk slot inside a downloading sweep.
    pub fn rt_chunk_fill() -> Color32 {
        Color32::from_rgba_unmultiplied(80, 170, 230, 70)
    }
    pub fn rt_chunk_border() -> Color32 {
        Color32::from_rgba_unmultiplied(100, 180, 255, 90)
    }
    /// Dashed border of a still-accumulating (partial) chunk slot.
    pub fn rt_chunk_partial_border() -> Color32 {
        Color32::from_rgba_unmultiplied(100, 180, 255, 70)
    }
    /// Dashed border around an entire downloading sweep block.
    pub fn rt_downloading_sweep_border() -> Color32 {
        Color32::from_rgba_unmultiplied(60, 140, 200, 100)
    }

    /// Pending (expected but not yet received) sweep placeholder.
    pub fn rt_pending_sweep_border() -> Color32 {
        Color32::from_rgba_unmultiplied(80, 120, 180, 100)
    }

    /// Dotted border for the "next chunk" placeholder block.
    pub fn rt_next_chunk_border() -> Color32 {
        Color32::from_rgba_unmultiplied(140, 200, 255, 140)
    }

    /// Very faint fill for the "next chunk" placeholder block.
    pub fn rt_next_chunk_fill() -> Color32 {
        Color32::from_rgba_unmultiplied(100, 180, 255, 20)
    }

    /// Countdown label color for the "next chunk" placeholder.
    pub fn rt_next_chunk_label() -> Color32 {
        Color32::from_rgba_unmultiplied(160, 220, 255, 220)
    }

    // ── Saved event overlay colors ────────────────────────────────────

    const EVENT_PALETTE: &[(u8, u8, u8)] = &[
        (255, 200, 80),
        (120, 220, 160),
        (160, 180, 255),
        (255, 150, 150),
        (200, 160, 255),
        (255, 180, 120),
    ];

    /// Semi-transparent fill for a saved event overlay.
    pub fn event_fill(index: usize) -> Color32 {
        let (r, g, b) = EVENT_PALETTE[index % EVENT_PALETTE.len()];
        Color32::from_rgba_unmultiplied(r, g, b, 30)
    }

    /// Border/line color for a saved event overlay.
    pub fn event_border(index: usize) -> Color32 {
        let (r, g, b) = EVENT_PALETTE[index % EVENT_PALETTE.len()];
        Color32::from_rgba_unmultiplied(r, g, b, 160)
    }

    /// Label color for a saved event name.
    pub fn event_label(index: usize) -> Color32 {
        let (r, g, b) = EVENT_PALETTE[index % EVENT_PALETTE.len()];
        Color32::from_rgb(r, g, b)
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

/// Colors for acquisition queue operation statuses (theme-independent).
pub mod acquisition {
    use super::Color32;

    /// Queued operation — muted blue.
    pub const QUEUED: Color32 = Color32::from_rgb(120, 160, 200);
    /// Active operation — bright blue.
    pub const ACTIVE: Color32 = Color32::from_rgb(100, 180, 255);
    /// Completed operation — green.
    pub const COMPLETED: Color32 = Color32::from_rgb(100, 200, 100);
    /// Failed operation — red.
    pub const FAILED: Color32 = Color32::from_rgb(255, 100, 100);
    /// Cancelled operation — gray.
    pub const CANCELLED: Color32 = Color32::from_rgb(120, 120, 120);
}

/// Colors for mPING storm-report markers (theme-independent).
pub mod mping {
    use super::Color32;
    use crate::mping::ReportCategory;

    /// Stroke (and label) color for any marker — chosen for legibility
    /// against both bright reflectivity fills and the dark canvas
    /// background.
    pub const STROKE: Color32 = Color32::from_rgb(20, 20, 25);

    /// Fill color for a report's marker, by category.
    pub fn fill(category: ReportCategory) -> Color32 {
        match category {
            ReportCategory::RainSnow => Color32::from_rgb(120, 200, 255),
            ReportCategory::Hail => Color32::from_rgb(255, 230, 80),
            ReportCategory::WindDamage => Color32::from_rgb(255, 160, 60),
            ReportCategory::Tornado => Color32::from_rgb(255, 80, 80),
            ReportCategory::Flood => Color32::from_rgb(80, 120, 220),
            ReportCategory::ReducedVisibility => Color32::from_rgb(180, 180, 180),
            ReportCategory::Other => Color32::from_rgb(220, 220, 220),
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
