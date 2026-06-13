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
    /// Active-frame ring — one of the three permitted accents (spec §6.2).
    pub const ACTIVE_SWEEP: Color32 = Color32::from_rgb(255, 255, 100);
    /// Previous active-frame ring during the cell-to-cell blend. A FAINTER
    /// shade of the SAME accent hue (not a second accent color) so the accent
    /// budget holds — it just trails the active ring for one animation.
    /// Premultiplied (const-constructible) equivalent of the active yellow at
    /// ~43% alpha: rgb scaled by 110/255.
    pub const PREV_ACTIVE_SWEEP: Color32 = Color32::from_rgba_premultiplied(110, 110, 43, 110);
    /// The now-line / "GO LIVE" cap when NOT streaming — a calm, muted red
    /// that reads as an invitation. Red is reserved exclusively for the
    /// now/live concept (see [`LIVE_ACTIVE`] for the streaming state).
    pub const NOW_IDLE: Color32 = Color32::from_rgb(200, 95, 95);
    /// Live status color for the now-line / "LIVE" cap when streaming —
    /// the bright, active end of the same red.
    pub const LIVE_ACTIVE: Color32 = Color32::from_rgb(255, 80, 80);
    /// Selection range boundary label color — neutral (the loop band is no
    /// longer a saturated-blue accent; it reads as a translucent neutral wash).
    pub const SELECTION_LABEL: Color32 = Color32::from_rgb(180, 188, 205);
    /// Selection/loop range (shift+drag) translucent fill — neutral, so it
    /// doesn't spend one of the three accent slots (spec §6.2).
    pub fn selection_fill() -> Color32 {
        Color32::from_rgba_unmultiplied(180, 188, 210, 34)
    }
    /// Selection/loop range boundary lines — neutral.
    pub fn selection_edge() -> Color32 {
        Color32::from_rgba_unmultiplied(180, 188, 210, 170)
    }
    /// Text drawn inside scan/sweep blocks. Theme-independent because it
    /// sits on the blocks' own fill colors, not the panel background.
    pub fn block_label() -> Color32 {
        Color32::from_rgba_unmultiplied(225, 232, 248, 200)
    }
    /// De-emphasized in-block text (ghost/projected blocks).
    pub fn block_label_weak() -> Color32 {
        Color32::from_rgba_unmultiplied(225, 232, 248, 110)
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

    /// Tooltip status-word tint for on-device data — a brighter, readable
    /// take on the cached steel blue.
    pub fn status_cached() -> Color32 {
        Color32::from_rgb(125, 170, 220)
    }

    /// Tooltip status-word tint for cloud-available data.
    pub fn status_available() -> Color32 {
        Color32::from_rgb(150, 165, 200)
    }

    // ── Frames-first cell palette (spec §6.2 / §6.3) ───────────────────
    //
    // The strip's primary cells are frames of the selected product + tilt.
    // Per the accent budget (≤3 accents at once: playhead, live edge, active
    // ring), every cell state is conveyed by FILL + SHAPE/TEXTURE, never by a
    // unique hue — one neutral steel tone carries them all and the strip reads
    // in grayscale. Accents (active ring, failure tick) are layered on top by
    // the renderer using ACTIVE_SWEEP / acquisition::FAILED.

    /// Subtle bounding box around a scan container.
    pub fn container_border(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgba_unmultiplied(120, 130, 150, 90)
        } else {
            Color32::from_rgba_unmultiplied(90, 100, 120, 90)
        }
    }

    /// Faint neutral sub-texture: the thin vertical sweep-boundary lines of the
    /// full volume drawn inside a container (NOT colored per elevation).
    pub fn sub_texture(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgba_unmultiplied(120, 130, 150, 45)
        } else {
            Color32::from_rgba_unmultiplied(90, 100, 120, 45)
        }
    }

    /// Solid fill for a downloaded (Cached) frame cell — the neutral steel
    /// tone at full presence.
    pub fn cell_cached(dark: bool) -> Color32 {
        let (r, g, b) = if dark { CACHED_RGB } else { (70, 110, 160) };
        Color32::from_rgba_unmultiplied(r, g, b, 230)
    }

    /// Hollow-outline border for an Available (server-only) frame cell.
    pub fn cell_available_border(dark: bool) -> Color32 {
        available_border(dark)
    }

    /// Faint wash inside an Available frame cell (kept low so the outline, not
    /// the fill, carries the meaning).
    pub fn cell_available_fill(dark: bool) -> Color32 {
        available_fill(dark)
    }

    /// In-flight fill (archive pulse / live chunk slots) — same steel hue as
    /// cached, at reduced alpha so it reads as "filling in".
    pub fn cell_inflight(dark: bool) -> Color32 {
        let (r, g, b) = if dark { CACHED_RGB } else { (70, 110, 160) };
        Color32::from_rgba_unmultiplied(r, g, b, 150)
    }

    /// Border around an in-flight cell.
    pub fn cell_inflight_border(dark: bool) -> Color32 {
        let (r, g, b) = if dark { CACHED_RGB } else { (70, 110, 160) };
        Color32::from_rgba_unmultiplied(r, g, b, 180)
    }

    /// Hatch stroke for a Queued frame cell (faint, neutral — distinct from the
    /// dashed Available outline by texture, readable in grayscale).
    pub fn cell_queued_hatch(dark: bool) -> Color32 {
        let (r, g, b) = if dark { AVAILABLE_RGB } else { (90, 110, 150) };
        Color32::from_rgba_unmultiplied(r, g, b, 95)
    }

    /// Dashed-ghost border for a Projected (future) frame cell.
    pub fn cell_projected_border(dark: bool) -> Color32 {
        let (r, g, b) = if dark { AVAILABLE_RGB } else { (90, 110, 150) };
        Color32::from_rgba_unmultiplied(r, g, b, 110)
    }

    /// Faint fill for a Projected cell.
    pub fn cell_projected_fill(dark: bool) -> Color32 {
        let (r, g, b) = if dark { AVAILABLE_RGB } else { (90, 110, 150) };
        Color32::from_rgba_unmultiplied(r, g, b, 24)
    }

    /// Countdown text on the nearest projected ghost ("0.5° in ~40s").
    pub fn cell_countdown_label(dark: bool) -> Color32 {
        if dark {
            Color32::from_rgba_unmultiplied(200, 210, 230, 220)
        } else {
            Color32::from_rgba_unmultiplied(60, 75, 100, 230)
        }
    }

    // The former indigo→cyan per-elevation sweep gradient and the realtime
    // chunk/ghost palette were removed with the two-lane strip: frame cells are
    // one tilt now and read in a single neutral steel tone (see the frames-first
    // cell palette above), so no per-elevation hue and no separate live colors.

    // ── Saved event overlay colors ────────────────────────────────────
    //
    // Saved events are NEUTRAL with a distinct SHAPE (a bookmark tick on the
    // tick rail), not an amber fill — the accent budget reserves color for the
    // playhead / live edge / active ring. The name labels disambiguate.

    const EVENT_RGB: (u8, u8, u8) = (190, 198, 215);

    /// Faint neutral fill for a saved event overlay band.
    pub fn event_fill() -> Color32 {
        let (r, g, b) = EVENT_RGB;
        Color32::from_rgba_unmultiplied(r, g, b, 22)
    }

    /// Border/line + bookmark-tick color for a saved event overlay.
    pub fn event_border() -> Color32 {
        let (r, g, b) = EVENT_RGB;
        Color32::from_rgba_unmultiplied(r, g, b, 150)
    }

    /// Label color for a saved event name.
    pub fn event_label() -> Color32 {
        let (r, g, b) = EVENT_RGB;
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
