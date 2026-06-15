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

    /// Orange - acquiring lock/connecting. Part of the live palette; the
    /// transport-row badge moved to a stateful LIVE button, so this is reserved
    /// for the queue sheet / connection detail next phase.
    #[allow(dead_code)]
    pub const ACQUIRING: Color32 = Color32::from_rgb(255, 180, 50);
    /// Red - actively streaming.
    pub const STREAMING: Color32 = Color32::from_rgb(255, 80, 80);
    /// Blue - waiting for next chunk. Reserved alongside [`ACQUIRING`] for the
    /// next-phase queue sheet / countdown chrome.
    #[allow(dead_code)]
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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── ui:: theme branches + theme-independent constants ──────────────

    #[wasm_bindgen_test]
    fn ui_label_value_theme_branches() {
        // Opaque (from_rgb) colors store their bytes verbatim.
        let dl = ui::label(true);
        assert!(dl.to_array() == [100, 100, 100, 255]);
        let ll = ui::label(false);
        assert!(ll.to_array() == [120, 120, 120, 255]);

        let dv = ui::value(true);
        assert!(dv.to_array() == [160, 160, 160, 255]);
        let lv = ui::value(false);
        assert!(lv.to_array() == [60, 60, 60, 255]);

        // Dark vs light must differ for every theme-aware fn here.
        assert!(ui::label(true).to_array() != ui::label(false).to_array());
        assert!(ui::value(true).to_array() != ui::value(false).to_array());
    }

    #[wasm_bindgen_test]
    fn ui_const_accents() {
        assert!(ui::ACTIVE.to_array() == [100, 180, 255, 255]);
        assert!(ui::SUCCESS.to_array() == [100, 200, 100, 255]);
    }

    // ── live / mode share the same "now/live" red ──────────────────────

    #[wasm_bindgen_test]
    fn live_and_mode_red_family() {
        let red = [255u8, 80, 80, 255];
        assert!(live::STREAMING.to_array() == red);
        assert!(mode::LIVE.to_array() == red);
        assert!(timeline::LIVE_ACTIVE.to_array() == red);

        // ACQUIRING orange and WAITING blue are distinct reserved tones.
        assert!(live::ACQUIRING.to_array() == [255, 180, 50, 255]);
        assert!(live::WAITING.to_array() == [100, 180, 255, 255]);
    }

    #[wasm_bindgen_test]
    fn mode_distinct_states() {
        assert!(mode::IDLE.to_array() == [100, 100, 100, 255]);
        assert!(mode::ARCHIVE.to_array() == [100, 180, 255, 255]);
        // All three mode tones differ.
        assert!(mode::IDLE.to_array() != mode::ARCHIVE.to_array());
        assert!(mode::ARCHIVE.to_array() != mode::LIVE.to_array());
        assert!(mode::IDLE.to_array() != mode::LIVE.to_array());
    }

    // ── radar sweep lines: alpha encodes "active vs stale" ─────────────

    #[wasm_bindgen_test]
    fn radar_stale_lines_are_more_transparent() {
        // from_rgba_unmultiplied stores alpha verbatim; assert via .a().
        assert!(radar::SWEEP_LINE.to_array() == [100, 255, 100, 255]);
        // Active start line is more opaque than its stale variant.
        assert!(radar::sweep_start_line().a() == 180);
        assert!(radar::sweep_start_line_stale().a() == 100);
        assert!(radar::sweep_start_line().a() > radar::sweep_start_line_stale().a());
        // Stale sweep line alpha.
        assert!(radar::sweep_line_stale().a() == 140);
    }

    // ── timeline theme branches ────────────────────────────────────────

    #[wasm_bindgen_test]
    fn timeline_background_and_border_themes() {
        assert!(timeline::background(true).to_array() == [10, 10, 14, 255]);
        assert!(timeline::background(false).to_array() == [230, 230, 235, 255]);
        assert!(timeline::border(true).to_array() == [60, 60, 80, 255]);
        assert!(timeline::border(false).to_array() == [180, 180, 195, 255]);
        // Dark background is much darker than light background.
        assert!(timeline::background(true).r() < timeline::background(false).r());
    }

    #[wasm_bindgen_test]
    fn timeline_tick_hierarchy() {
        // Major tick is brighter (higher luminance proxy) than minor on dark.
        let major = timeline::tick_major(true);
        let minor = timeline::tick_minor(true);
        assert!(major.to_array() == [120, 120, 140, 255]);
        assert!(minor.to_array() == [60, 60, 80, 255]);
        assert!(major.r() > minor.r());
        // Theme inversion: light-mode ticks differ from dark-mode.
        assert!(timeline::tick_major(true).to_array() != timeline::tick_major(false).to_array());
        assert!(timeline::tick_label(true).to_array() == [140, 140, 160, 255]);
        assert!(timeline::tick_label(false).to_array() == [60, 60, 80, 255]);
    }

    #[wasm_bindgen_test]
    fn timeline_selection_is_near_white_on_dark() {
        let dark = timeline::selection(true);
        let light = timeline::selection(false);
        assert!(dark.to_array() == [235, 240, 250, 255]);
        assert!(light.to_array() == [40, 45, 60, 255]);
        // Near-white on dark, near-black on light.
        assert!(dark.r() > 200);
        assert!(light.r() < 100);
    }

    #[wasm_bindgen_test]
    fn timeline_active_sweep_accents() {
        // Active ring is the yellow accent.
        assert!(timeline::ACTIVE_SWEEP.to_array() == [255, 255, 100, 255]);
        // PREV is a const-premultiplied fainter shade of the SAME hue:
        // rgb scaled by 110/255 at alpha 110 -> stored verbatim.
        assert!(timeline::PREV_ACTIVE_SWEEP.to_array() == [110, 110, 43, 110]);
        // Trailing ring is more transparent than the active ring.
        assert!(timeline::PREV_ACTIVE_SWEEP.a() < timeline::ACTIVE_SWEEP.a());
    }

    #[wasm_bindgen_test]
    fn timeline_now_idle_vs_active_red() {
        // Idle "GO LIVE" cap is a muted red; streaming is the bright red.
        assert!(timeline::NOW_IDLE.to_array() == [200, 95, 95, 255]);
        assert!(timeline::LIVE_ACTIVE.to_array() == [255, 80, 80, 255]);
        // Active red is brighter (more saturated red) than idle.
        assert!(timeline::LIVE_ACTIVE.r() > timeline::NOW_IDLE.r());
    }

    // ── timeline cached_fill: two-tier alpha (partial vs full) ─────────

    #[wasm_bindgen_test]
    fn timeline_cached_fill_alpha_tiers() {
        // Full (not partial) -> alpha 235; partial -> alpha 120. Alpha stored
        // verbatim by from_rgba_unmultiplied.
        assert!(cached_fill_a(true, false) == 235);
        assert!(cached_fill_a(true, true) == 120);
        assert!(cached_fill_a(false, false) == 235);
        assert!(cached_fill_a(false, true) == 120);
        // Partial is always more transparent than full, both themes.
        assert!(cached_fill_a(true, true) < cached_fill_a(true, false));
        assert!(cached_fill_a(false, true) < cached_fill_a(false, false));
    }

    // Helper mirroring the production alpha selection, kept private to the test
    // module (asserts the real fn's stored alpha via .a()).
    fn cached_fill_a(dark: bool, partial: bool) -> u8 {
        timeline::cached_fill(dark, partial).a()
    }

    #[wasm_bindgen_test]
    fn timeline_available_fill_and_border_alpha() {
        // available_fill is a faint wash (alpha 36); border is stronger (120).
        assert!(timeline::available_fill(true).a() == 36);
        assert!(timeline::available_border(true).a() == 120);
        assert!(timeline::available_fill(true).a() < timeline::available_border(true).a());
        // cell_* aliases delegate to the same functions -> identical bytes.
        assert!(
            timeline::cell_available_border(true).to_array()
                == timeline::available_border(true).to_array()
        );
        assert!(
            timeline::cell_available_fill(false).to_array()
                == timeline::available_fill(false).to_array()
        );
    }

    #[wasm_bindgen_test]
    fn timeline_cell_inflight_ordering() {
        // In-flight reads as "filling in": more opaque than projected/available
        // washes but less than a solid cached cell.
        assert!(timeline::cell_cached(true).a() == 230);
        assert!(timeline::cell_inflight(true).a() == 150);
        assert!(timeline::cell_inflight_border(true).a() == 180);
        assert!(timeline::cell_projected_fill(true).a() == 24);
        assert!(timeline::cell_inflight(true).a() < timeline::cell_cached(true).a());
        assert!(timeline::cell_inflight(true).a() > timeline::cell_projected_fill(true).a());
        // Border is more opaque than the in-flight interior fill.
        assert!(timeline::cell_inflight_border(true).a() > timeline::cell_inflight(true).a());
    }

    #[wasm_bindgen_test]
    fn timeline_status_word_tints() {
        // Opaque tooltip status tints.
        assert!(timeline::status_cached().to_array() == [125, 170, 220, 255]);
        assert!(timeline::status_available().to_array() == [150, 165, 200, 255]);
        assert!(timeline::status_cached().to_array() != timeline::status_available().to_array());
    }

    #[wasm_bindgen_test]
    fn timeline_event_palette_shares_neutral_hue() {
        // event_fill / event_border / event_label all use EVENT_RGB at
        // different alphas; fill is the faintest, label is opaque.
        assert!(timeline::event_fill().a() == 22);
        assert!(timeline::event_border().a() == 150);
        assert!(timeline::event_label().a() == 255);
        assert!(timeline::event_fill().a() < timeline::event_border().a());
        // Opaque label carries the base EVENT_RGB verbatim.
        assert!(timeline::event_label().to_array() == [190, 198, 215, 255]);
    }

    #[wasm_bindgen_test]
    fn timeline_block_label_weak_is_dimmer() {
        assert!(timeline::block_label().a() == 200);
        assert!(timeline::block_label_weak().a() == 110);
        assert!(timeline::block_label_weak().a() < timeline::block_label().a());
    }

    // ── canvas: BLACK background on dark, light gray on light ───────────

    #[wasm_bindgen_test]
    fn canvas_background_and_marker_themes() {
        assert!(canvas::background(true).to_array() == [0, 0, 0, 255]);
        assert!(canvas::background(false).to_array() == [235, 235, 240, 255]);
        // Center marker is brighter on dark than light (it sits on a dark bg).
        assert!(canvas::center_marker(true).to_array() == [180, 180, 200, 255]);
        assert!(canvas::center_marker(false).to_array() == [80, 80, 100, 255]);
        assert!(canvas::center_marker(true).r() > canvas::center_marker(false).r());
        // Translucent rings keep alpha verbatim.
        assert!(canvas::ring(true).a() == 120);
        assert!(canvas::ring_major(true).a() == 150);
        assert!(canvas::radial(true).a() == 80);
    }

    // ── acquisition status table ───────────────────────────────────────

    #[wasm_bindgen_test]
    fn acquisition_status_colors_distinct() {
        assert!(acquisition::QUEUED.to_array() == [120, 160, 200, 255]);
        assert!(acquisition::ACTIVE.to_array() == [100, 180, 255, 255]);
        assert!(acquisition::COMPLETED.to_array() == [100, 200, 100, 255]);
        assert!(acquisition::FAILED.to_array() == [255, 100, 100, 255]);
        assert!(acquisition::CANCELLED.to_array() == [120, 120, 120, 255]);
        // FAILED is the red-dominant tone; COMPLETED is green-dominant.
        assert!(acquisition::FAILED.r() > acquisition::FAILED.g());
        assert!(acquisition::COMPLETED.g() > acquisition::COMPLETED.r());
    }

    // ── mping::fill category table + STROKE ────────────────────────────

    #[wasm_bindgen_test]
    fn mping_fill_category_table() {
        use crate::mping::ReportCategory;
        assert!(mping::fill(ReportCategory::RainSnow).to_array() == [120, 200, 255, 255]);
        assert!(mping::fill(ReportCategory::Hail).to_array() == [255, 230, 80, 255]);
        assert!(mping::fill(ReportCategory::WindDamage).to_array() == [255, 160, 60, 255]);
        assert!(mping::fill(ReportCategory::Tornado).to_array() == [255, 80, 80, 255]);
        assert!(mping::fill(ReportCategory::Flood).to_array() == [80, 120, 220, 255]);
        assert!(mping::fill(ReportCategory::ReducedVisibility).to_array() == [180, 180, 180, 255]);
        assert!(mping::fill(ReportCategory::Other).to_array() == [220, 220, 220, 255]);
        // Tornado reuses the now/live red; stroke is near-black for legibility.
        assert!(
            mping::fill(ReportCategory::Tornado).to_array() == mping::STROKE.to_opaque_red_check()
        );
    }

    // Tiny extension trait to keep the assertion above readable without an
    // extra `use`; just re-reads STROKE as a red tuple is wrong, so instead we
    // verify STROKE directly here.
    trait StrokeCheck {
        fn to_opaque_red_check(self) -> [u8; 4];
    }
    impl StrokeCheck for Color32 {
        fn to_opaque_red_check(self) -> [u8; 4] {
            // STROKE is NOT red; return the known tornado red so the equality
            // above is a true positive only when fill(Tornado) matches it.
            [255, 80, 80, 255]
        }
    }

    #[wasm_bindgen_test]
    fn mping_stroke_is_near_black() {
        assert!(mping::STROKE.to_array() == [20, 20, 25, 255]);
        // Dark enough to read against bright fills.
        assert!(mping::STROKE.r() < 30);
    }

    // ── sites: current (cyan) vs other (orange) ────────────────────────

    #[wasm_bindgen_test]
    fn sites_current_vs_other() {
        assert!(sites::OTHER.to_array() == [255, 180, 80, 255]);
        assert!(sites::OTHER_STROKE.to_array() == [180, 120, 40, 255]);
        assert!(sites::CURRENT.to_array() == [50, 200, 255, 255]);
        assert!(sites::CURRENT_STROKE.to_array() == [30, 150, 200, 255]);
        // Current label matches the current marker hue exactly.
        assert!(sites::CURRENT_LABEL.to_array() == sites::CURRENT.to_array());
        assert!(sites::LABEL.to_array() == [220, 220, 240, 255]);
        // Other is orange (red>blue); current is cyan (blue>red).
        assert!(sites::OTHER.r() > sites::OTHER.b());
        assert!(sites::CURRENT.b() > sites::CURRENT.r());
    }
}
