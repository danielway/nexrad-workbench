//! The one source of truth for chrome color and type ("One Instrument" §3, §4).
//!
//! **Color grammar.** Chrome is grey. Color is a claim about time. Three
//! accents each mean one thing on every surface, one chrome color means
//! *selected*, and nothing else in the interface is colored — the reflectivity
//! palette owns green, yellow, red and magenta, so the chrome gives them up.
//!
//! | Token family | Role |
//! |---|---|
//! | [`MAP`] [`PANEL`] [`RAISED`] [`HOVER`] | the four surfaces, from the dark canvas outward |
//! | [`TEXT`] [`TEXT_DIM`] [`TEXT_FAINT`] [`LINE`] | the four greys for text and dividers |
//! | [`SEEN`] | white accent — *what you see* |
//! | [`LOOP`] | amber accent — *what loops* (selection ranges) |
//! | [`NOW`] / [`NOW_IDLE`] | red accent — *now*, at two strengths |
//! | [`SELECTED`] | periwinkle — the one selected-control color, never a state |
//! | [`ON_DEVICE`] | slate — cached / on this device; a neutral fill, not an accent |
//!
//! Widgets ask for a role, never an RGB. Role helpers that derive alphas from
//! the base tokens live in the submodules ([`timeline`], [`canvas`]), and the
//! map marks in [`map`]. Hues in use today that the grammar retires are kept,
//! folded and named, in [`legacy`] until the PR that removes their use lands.
//! Data colors (mPING categories, storm-cell intensity,
//! the radar color tables) are not chrome and live in
//! [`crate::ui::data_colors`] / `crate::nexrad::render::color_table`.
//!
//! **Type.** IBM Plex Sans for prose and labels, IBM Plex Mono for every time,
//! number, ID and readout. Five sizes ([`CAPTION`], [`LABEL`], [`READOUT`],
//! [`BODY`], [`TITLE`]) are installed as egui's text styles by [`install`], so
//! stock widgets pick them up; [`readout`] / [`readout_font`] are the mono
//! readout helpers. The "one clock" formatting rule (24 h `HH:MM:SS`, `~` for
//! projected values, `m:ss` countdowns, "N min old" ages) belongs to
//! [`crate::ui::time_format`].
//!
//! The app is dark-only (`ThemeMode` has a single variant), so every token is
//! a constant.

use eframe::egui::{self, Color32, FontFamily, FontId, RichText, TextStyle};

// ── Surfaces ───────────────────────────────────────────────────────────────

/// Surface 0 — behind the radar image (the map canvas).
pub(crate) const MAP: Color32 = Color32::from_rgb(6, 8, 12);
/// Surface 1 — the top bar, side panels and the timeline track.
pub(crate) const PANEL: Color32 = Color32::from_rgb(12, 16, 22);
/// Surface 2 — raised: fields, chips, dialogs and scrims over the map.
pub(crate) const RAISED: Color32 = Color32::from_rgb(20, 26, 35);
/// Surface 3 — hover and pressed.
pub(crate) const HOVER: Color32 = Color32::from_rgb(28, 36, 48);

// ── Greys for text and dividers ────────────────────────────────────────────

/// Primary text — values and names.
pub(crate) const TEXT: Color32 = Color32::from_rgb(232, 237, 244);
/// Secondary text — labels.
pub(crate) const TEXT_DIM: Color32 = Color32::from_rgb(143, 154, 170);
/// Faint text — hints, units, idle states.
pub(crate) const TEXT_FAINT: Color32 = Color32::from_rgb(93, 103, 117);
/// The one hairline / divider grey (borders, minor ticks).
pub(crate) const LINE: Color32 = Color32::from_rgb(39, 48, 64);

// ── Accents ────────────────────────────────────────────────────────────────

/// White — *what you see*: the playhead line and triangle, the playhead
/// clock, the overview's view box.
pub(crate) const SEEN: Color32 = Color32::from_rgb(244, 247, 251);
/// Amber — *what loops*: the range bar, handles, wash and the selection chip.
pub(crate) const LOOP: Color32 = Color32::from_rgb(230, 162, 60);
/// [`LOOP`] lightened for text set on a dark chip.
pub(crate) const LOOP_TEXT: Color32 = Color32::from_rgb(243, 207, 134);
/// [`LOOP`] darkened for a chip's border.
pub(crate) const LOOP_DEEP: Color32 = Color32::from_rgb(122, 90, 28);
/// Red — *now*, full strength: the stream is running (now line, cap, the dot
/// on a pinned handle, the overview's now dot).
pub(crate) const NOW: Color32 = Color32::from_rgb(255, 95, 95);
/// Red — *now*, low strength: not streaming (the idle now line, "GO LIVE").
pub(crate) const NOW_IDLE: Color32 = Color32::from_rgb(200, 95, 95);

/// Blue — *weather*: the phone strip's storm-activity bars and the calendar
/// sheet's day shade (how much echo a day held, from the nightly
/// storm-activity index). Its strength is the level; the hue is not a status
/// color, so it never competes with now or a loop.
pub(crate) const STORM: Color32 = Color32::from_rgb(79, 127, 217);

// ── Selected control and on-device ─────────────────────────────────────────

/// Periwinkle — the one *selected control* color (pressed product, chosen
/// ladder row, active rail tab). In no radar palette. Never for a state.
#[allow(dead_code)] // Adopted by the controls in later One Instrument PRs.
pub(crate) const SELECTED: Color32 = Color32::from_rgb(138, 162, 255);
/// Slate — *on device*: cached cells, received cuts, held days. A neutral,
/// not an accent: it is the fill channel.
pub(crate) const ON_DEVICE: Color32 = Color32::from_rgb(70, 96, 127);

/// `c` with its alpha replaced (unmultiplied), for washes over another surface.
pub(crate) fn alpha(c: Color32, a: u8) -> Color32 {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// The favicon's color: the one red while pinned to a running stream, the
/// faint tone otherwise (shell-side mapping; the core stays color-free).
pub(crate) fn favicon_color(live_now: bool) -> Color32 {
    if live_now {
        NOW
    } else {
        TEXT_FAINT
    }
}

/// Retired hues — in use today, outside the grammar. Each is folded from the
/// near-duplicates it replaced and goes away with the PR named on it.
pub(crate) mod legacy {
    use super::Color32;

    /// The old "active" blue. The panel no longer uses it (the ladder and the
    /// rail PR); what is left is busy work in the activity chip and sheet and
    /// the timeline tooltips, the site and VCP forecast dialogs (dialogs PR)
    /// and the favicon's archive color (links, preferences and modes PR).
    /// Selected controls are [`super::SELECTED`]; busy work becomes words.
    pub(crate) const ACTIVE_BLUE: Color32 = Color32::from_rgb(100, 180, 255);
    /// Success / "on device" / completed green. Becomes nothing in chrome.
    pub(crate) const SUCCESS: Color32 = Color32::from_rgb(100, 200, 100);
    /// Failure and form-error red. Becomes the ⚠ glyph in text color.
    pub(crate) const ERROR: Color32 = Color32::from_rgb(255, 100, 100);
    /// Stalled / degraded / caution orange. Stalled becomes a hollow red dot
    /// plus the word.
    pub(crate) const WARN: Color32 = Color32::from_rgb(255, 170, 50);
    /// Badge yellow-amber (SAILS/MRLE, cache warning, 3D pill, notes). SAILS
    /// keeps a tiny [`super::LOOP`] tick; the rest lose color.
    pub(crate) const CAUTION: Color32 = Color32::from_rgb(230, 190, 80);
}

/// Dialogs and popovers (§12): one frame, one primary, one destructive.
/// Chrome is grey, so the primary is a filled neutral and the destructive
/// button is outlined in text color — never red, which means *now*.
pub(crate) mod dialog {
    use super::{alpha, Color32, HOVER, LINE, MAP, PANEL, RAISED, TEXT};

    /// The dialog / popover surface.
    pub(crate) const SURFACE: Color32 = RAISED;
    /// The frame's hairline.
    pub(crate) const EDGE: Color32 = LINE;
    /// Corner radius of the frame.
    pub(crate) const RADIUS: u8 = 6;
    /// The dim over the map behind a dialog (one strength for all).
    pub(crate) fn backdrop() -> Color32 {
        alpha(MAP, 150)
    }
    /// The primary button's fill.
    pub(crate) const PRIMARY_FILL: Color32 = TEXT;
    /// The primary button's label.
    pub(crate) const PRIMARY_TEXT: Color32 = PANEL;
    /// A secondary button's fill.
    pub(crate) const SECONDARY_FILL: Color32 = HOVER;
    /// The destructive button's outline and label.
    pub(crate) const DESTRUCTIVE: Color32 = TEXT;
}

/// Map marks drawn over the radar (sites, location, rings, compass).
pub(crate) mod map {
    use super::{alpha, Color32, LINE, TEXT, TEXT_DIM, TEXT_FAINT};

    /// Other (non-current) NEXRAD site marker.
    pub(crate) const SITE: Color32 = Color32::from_rgb(255, 180, 80);
    /// Other site marker stroke.
    pub(crate) const SITE_STROKE: Color32 = Color32::from_rgb(180, 120, 40);
    /// The current site marker and its label.
    pub(crate) const SITE_CURRENT: Color32 = Color32::from_rgb(50, 200, 255);
    /// Current site marker stroke.
    pub(crate) const SITE_CURRENT_STROKE: Color32 = Color32::from_rgb(30, 150, 200);
    /// The user's GPS location dot.
    pub(crate) const GPS: Color32 = Color32::from_rgb(33, 150, 243);
    /// Dark stroke that keeps a mark legible on bright reflectivity.
    pub(crate) const HALO: Color32 = Color32::from_rgb(20, 20, 25);
    /// White ring around a map mark (location dot, measure end points,
    /// storm reports), legible on any echo.
    pub(crate) const OUTLINE: Color32 = Color32::WHITE;
    /// The red "N" on the 3D compass.
    pub(crate) const NORTH: Color32 = Color32::from_rgb(255, 80, 80);
    /// The measure tool's line, end points and readout border.
    pub(crate) const TOOL: Color32 = Color32::from_rgb(255, 100, 100);

    /// Minor range ring.
    pub(crate) fn ring() -> Color32 {
        alpha(LINE, 120)
    }
    /// Major (labeled) range ring.
    pub(crate) fn ring_major() -> Color32 {
        alpha(TEXT_FAINT, 150)
    }
    /// Radial spokes.
    pub(crate) fn radial() -> Color32 {
        alpha(LINE, 80)
    }
    /// Radar-site center marker fill.
    pub(crate) const CENTER_MARKER: Color32 = TEXT_DIM;
    /// Radar-site center marker stroke.
    pub(crate) const CENTER_MARKER_STROKE: Color32 = TEXT_FAINT;
    /// A label printed on the map (site IDs, readouts).
    pub(crate) const LABEL: Color32 = TEXT;
    /// The translucent surface under anything that sits over the map (the
    /// legend, the scale bar, the caption, stamps), so nothing prints
    /// straight over map labels.
    pub(crate) fn scrim() -> Color32 {
        alpha(super::PANEL, 210)
    }
    /// The scrim's hairline edge.
    pub(crate) fn scrim_edge() -> Color32 {
        alpha(LINE, 200)
    }
}

/// Radar sweep-position marks on the canvas (§7): white is what you see,
/// red is now.
pub(crate) mod radar {
    use super::{alpha, Color32, SEEN, TEXT, TEXT_FAINT};

    /// The data edge: the newest radial drawn.
    pub(crate) const SWEEP_LINE: Color32 = SEEN;
    /// Where the new sweep started: a faint white.
    pub(crate) fn sweep_start_line() -> Color32 {
        alpha(SEEN, 90)
    }
    /// A received chunk's boundary inside the live cut.
    pub(crate) fn chunk_line() -> Color32 {
        alpha(TEXT, 60)
    }
    /// The edge ring's arc for the new sweep.
    pub(crate) fn edge_ring_new() -> Color32 {
        alpha(SEEN, 190)
    }
    /// The edge ring's arc for the previous sweep (and the held ring).
    pub(crate) fn edge_ring_old() -> Color32 {
        alpha(TEXT_FAINT, 150)
    }
    /// Stale sweep line (shown between sweeps).
    pub(crate) fn sweep_line_stale() -> Color32 {
        alpha(TEXT_FAINT, 140)
    }
    /// Stale sweep start line (shown between sweeps).
    pub(crate) fn sweep_start_line_stale() -> Color32 {
        alpha(TEXT_FAINT, 100)
    }
}

/// Acquisition queue operation statuses.
pub(crate) mod acquisition {
    use super::{legacy, Color32, TEXT_DIM, TEXT_FAINT};

    /// Queued operation — waiting, so grey.
    pub(crate) const QUEUED: Color32 = TEXT_DIM;
    /// Active operation.
    pub(crate) const ACTIVE: Color32 = legacy::ACTIVE_BLUE;
    /// Completed operation.
    pub(crate) const COMPLETED: Color32 = legacy::SUCCESS;
    /// Failed operation.
    pub(crate) const FAILED: Color32 = legacy::ERROR;
    /// Cancelled operation.
    pub(crate) const CANCELLED: Color32 = TEXT_FAINT;
}

/// Timeline roles, derived from the base tokens.
pub(crate) mod timeline {
    use super::{
        alpha, Color32, LINE, LOOP, NOW, ON_DEVICE, PANEL, RAISED, SEEN, TEXT, TEXT_DIM, TEXT_FAINT,
    };

    /// The track's background.
    pub(crate) const BACKGROUND: Color32 = PANEL;
    /// The track's outline.
    pub(crate) const BORDER: Color32 = LINE;
    /// Major tick.
    pub(crate) const TICK_MAJOR: Color32 = TEXT_FAINT;
    /// Minor tick.
    pub(crate) const TICK_MINOR: Color32 = LINE;
    /// Tick label.
    pub(crate) const TICK_LABEL: Color32 = TEXT_DIM;
    /// The playhead ("the needle") and the playhead clock — *what you see*,
    /// never to be read as "now".
    pub(crate) const PLAYHEAD: Color32 = SEEN;
    /// The ring on the frame that is on the GPU — white, *what you see*,
    /// the same mark the inspector uses for "currently displayed".
    pub(crate) const ACTIVE_SWEEP: Color32 = SEEN;
    /// The previous active-frame ring during the cell-to-cell blend: a fainter
    /// shade of the same white so it trails the active ring for one animation.
    pub(crate) fn prev_active_sweep() -> Color32 {
        alpha(SEEN, 110)
    }
    /// The edge arrow for an off-screen playhead.
    pub(crate) const PLAYHEAD_EDGE: Color32 = SEEN;
    /// The ⚠ on a failed download — text color, never red (red is now).
    pub(crate) const FAILURE_GLYPH: Color32 = TEXT;
    /// The ⚠'s hover state.
    pub(crate) const FAILURE_GLYPH_HOVER: Color32 = SEEN;
    /// A pinned range's handle: the red dot that sits on the now line.
    pub(crate) const PINNED_HANDLE: Color32 = NOW;
    /// The now line / cap when not streaming.
    pub(crate) const NOW_IDLE: Color32 = super::NOW_IDLE;
    /// The now line / cap while streaming.
    pub(crate) const LIVE_ACTIVE: Color32 = NOW;
    /// A range boundary label.
    pub(crate) const SELECTION_LABEL: Color32 = TEXT_DIM;
    /// A selected range.
    pub(crate) const RANGE: Color32 = LOOP;
    /// A selected range's wash over the frames.
    pub(crate) fn selection_fill() -> Color32 {
        alpha(LOOP, 40)
    }
    /// A selected range's boundary lines over the frames.
    pub(crate) fn selection_edge() -> Color32 {
        alpha(LOOP, 190)
    }
    /// The selection strip's empty background, a step off the track's.
    pub(crate) const SELECTION_STRIP_BG: Color32 = RAISED;
    /// The overview's view box outline.
    pub(crate) fn view_box_edge() -> Color32 {
        SEEN.gamma_multiply(0.85)
    }
    /// The overview's view box wash, and the funnel joining it to the strip.
    pub(crate) fn view_box_fill() -> Color32 {
        SEEN.gamma_multiply(0.10)
    }
    /// The part of the overview past now, where nothing can be yet.
    pub(crate) fn future_wash() -> Color32 {
        SEEN.gamma_multiply(0.03)
    }
    /// Text drawn inside scan/frame blocks.
    pub(crate) fn block_label() -> Color32 {
        alpha(TEXT, 200)
    }
    /// De-emphasized in-block text (ghost/projected blocks).
    pub(crate) fn block_label_weak() -> Color32 {
        alpha(TEXT, 110)
    }
    /// Backdrop for a label chip drawn over frame cells.
    pub(crate) fn block_label_backdrop() -> Color32 {
        alpha(PANEL, 170)
    }

    // ── On device vs listed in the archive ──────────────────────────────
    //
    // The track's color answers one question — the data's relationship to
    // the user. Solid slate = on this device; hollow grey = listed in the
    // archive but not downloaded. VCP identity and counts are text.

    /// Fill for an on-device scan block; partial scans keep the hue at
    /// reduced alpha (two tiers only).
    pub(crate) fn cached_fill(partial: bool) -> Color32 {
        alpha(ON_DEVICE, if partial { 120 } else { 235 })
    }
    /// Faint interior wash for an archive-only block.
    pub(crate) fn available_fill() -> Color32 {
        alpha(TEXT_DIM, 36)
    }
    /// An overview bar's storm-activity height — one color, so nothing
    /// competes with the height (held time is the underline).
    pub(crate) fn activity_fill() -> Color32 {
        alpha(TEXT_DIM, 120)
    }
    /// The phone strip's storm-activity bars at its zoomed-out stops: the
    /// storm blue, so a bar's height reads as weather at a glance on the
    /// small strip.
    pub(crate) fn storm_fill() -> Color32 {
        alpha(super::STORM, 217)
    }
    /// The stub of an overview bar whose days are still being listed.
    pub(crate) fn activity_pending() -> Color32 {
        alpha(TEXT_DIM, 45)
    }
    /// The overview's held underline — slate, *on device*.
    pub(crate) const HELD: Color32 = ON_DEVICE;
    /// A saved event's pennant and underline on the overview — amber, an
    /// event loops under its name.
    pub(crate) const PENNANT: Color32 = LOOP;
    /// A known outage's tick on the overview — neutral, since red is now.
    pub(crate) const OUTAGE: Color32 = TEXT_DIM;
    /// Dashed border for an archive-only block.
    pub(crate) fn available_border() -> Color32 {
        alpha(TEXT_DIM, 120)
    }
    /// Tooltip status word for on-device data.
    pub(crate) const STATUS_CACHED: Color32 = TEXT;
    /// Tooltip status word for archive-only data.
    pub(crate) const STATUS_AVAILABLE: Color32 = TEXT_DIM;

    // ── Frame cells ─────────────────────────────────────────────────────
    //
    // Every cell state is fill + shape/texture, never a unique hue; accents
    // (active ring, failure tick) are layered on top by the renderer.

    /// Bounding box around a scan container.
    pub(crate) fn container_border() -> Color32 {
        alpha(TEXT_DIM, 90)
    }
    /// The sweep-boundary sub-texture inside a container.
    pub(crate) fn sub_texture() -> Color32 {
        alpha(TEXT_DIM, 45)
    }
    /// A downloaded (cached) frame cell.
    pub(crate) fn cell_cached() -> Color32 {
        alpha(ON_DEVICE, 230)
    }
    /// Hollow outline for an archive-only frame cell.
    pub(crate) fn cell_available_border() -> Color32 {
        available_border()
    }
    /// Faint wash inside an archive-only frame cell.
    pub(crate) fn cell_available_fill() -> Color32 {
        available_fill()
    }
    /// In-flight fill — the on-device hue at reduced alpha, "filling in".
    pub(crate) fn cell_inflight() -> Color32 {
        alpha(ON_DEVICE, 150)
    }
    /// Border around an in-flight cell.
    pub(crate) fn cell_inflight_border() -> Color32 {
        alpha(ON_DEVICE, 180)
    }
    /// Hatch for a queued frame cell.
    pub(crate) fn cell_queued_hatch() -> Color32 {
        alpha(TEXT_DIM, 95)
    }
    /// Dashed-ghost border for a projected (future) cell.
    pub(crate) fn cell_projected_border() -> Color32 {
        alpha(TEXT_DIM, 110)
    }
    /// Faint fill for a projected cell.
    pub(crate) fn cell_projected_fill() -> Color32 {
        alpha(TEXT_DIM, 24)
    }
    /// A projected cell's `~0.5°` and the next-volume ghost's `~VCP 212`.
    pub(crate) fn cell_projected_label() -> Color32 {
        alpha(TEXT, 120)
    }
    /// Dashed border of the next-volume ghost container.
    pub(crate) fn container_projected_border() -> Color32 {
        alpha(TEXT_DIM, 80)
    }
    /// Countdown text on the nearest projected ghost ("~0.5° in ~0:40").
    pub(crate) fn cell_countdown_label() -> Color32 {
        alpha(TEXT, 220)
    }

    // ── Saved events: neutral, told apart by shape ──────────────────────

    /// Faint fill for a saved event band.
    pub(crate) fn event_fill() -> Color32 {
        alpha(TEXT, 22)
    }
    /// Border / bookmark tick for a saved event.
    pub(crate) fn event_border() -> Color32 {
        alpha(TEXT, 150)
    }
    /// A saved event's name.
    pub(crate) const EVENT_LABEL: Color32 = TEXT;
}

// ── Type ───────────────────────────────────────────────────────────────────

/// Caption — hints, units, small print (egui `Small`).
pub(crate) const CAPTION: f32 = 11.0;
/// Label — chrome text: labels, buttons, list rows (egui `Body`, `Button`).
pub(crate) const LABEL: f32 = 12.5;
/// Readout — mono numbers that tick or line up: clocks, angles, values,
/// sizes (egui `Monospace`).
pub(crate) const READOUT: f32 = 13.0;
/// Body — prose in dialogs and sheets ([`body_style`]).
pub(crate) const BODY: f32 = 14.0;
/// Title — dialog and section titles (egui `Heading`).
pub(crate) const TITLE: f32 = 17.0;

/// The named egui text style carrying [`BODY`] prose.
pub(crate) fn body_style() -> TextStyle {
    TextStyle::Name("body".into())
}

/// A mono readout font at [`READOUT`] size.
#[allow(dead_code)] // Adopted by readouts in later One Instrument PRs.
pub(crate) fn readout_font() -> FontId {
    FontId::monospace(READOUT)
}

/// Mono readout text — times, numbers, IDs, dBZ values.
#[allow(dead_code)] // Adopted by readouts in later One Instrument PRs.
pub(crate) fn readout(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).monospace()
}

/// egui's text styles mapped onto the five sizes.
pub(crate) fn text_styles() -> std::collections::BTreeMap<TextStyle, FontId> {
    [
        (TextStyle::Small, FontId::proportional(CAPTION)),
        (TextStyle::Body, FontId::proportional(LABEL)),
        (TextStyle::Button, FontId::proportional(LABEL)),
        (TextStyle::Monospace, FontId::monospace(READOUT)),
        (body_style(), FontId::proportional(BODY)),
        (TextStyle::Heading, FontId::proportional(TITLE)),
    ]
    .into()
}

const PLEX_SANS: &str = "IBMPlexSans-Regular";
const PLEX_MONO: &str = "IBMPlexMono-Regular";

/// The app's font set: IBM Plex Sans (proportional) and IBM Plex Mono
/// (monospace) first, egui's bundled fonts kept behind them as glyph
/// fallbacks, and the Phosphor icon font.
pub(crate) fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        PLEX_SANS.to_owned(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf"))
            .into(),
    );
    fonts.font_data.insert(
        PLEX_MONO.to_owned(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf"))
            .into(),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, PLEX_SANS.to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, PLEX_MONO.to_owned());
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    fonts
}

/// Install the theme's fonts and text styles on the context. Called once at
/// startup; visuals are applied per theme in the app shell.
pub(crate) fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    ctx.all_styles_mut(|style| style.text_styles = text_styles());
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn accents_are_distinct() {
        let accents = [SEEN, LOOP, NOW, NOW_IDLE, SELECTED, ON_DEVICE];
        for (i, a) in accents.iter().enumerate() {
            for b in &accents[i + 1..] {
                assert!(a != b);
            }
        }
    }

    #[wasm_bindgen_test]
    fn grammar_values_match_the_design() {
        assert!(SELECTED.to_array() == [0x8a, 0xa2, 0xff, 255]);
        assert!(ON_DEVICE.to_array() == [0x46, 0x60, 0x7f, 255]);
        // Now streaming is the stronger of the two reds.
        assert!(NOW.r() > NOW_IDLE.r());
    }

    #[wasm_bindgen_test]
    fn surfaces_step_outward_from_the_map() {
        let s = [MAP, PANEL, RAISED, HOVER];
        for w in s.windows(2) {
            assert!(w[0].r() < w[1].r() && w[0].g() < w[1].g() && w[0].b() < w[1].b());
        }
    }

    #[wasm_bindgen_test]
    fn greys_rank_by_emphasis() {
        assert!(TEXT.r() > TEXT_DIM.r());
        assert!(TEXT_DIM.r() > TEXT_FAINT.r());
        assert!(TEXT_FAINT.r() > LINE.r());
    }

    #[wasm_bindgen_test]
    fn alpha_replaces_only_alpha() {
        let c = alpha(LOOP, 40);
        assert!(c == Color32::from_rgba_unmultiplied(230, 162, 60, 40));
        assert!(c.a() == 40);
        assert!(alpha(TEXT, 255) == TEXT);
    }

    #[wasm_bindgen_test]
    fn timeline_alpha_tiers() {
        assert!(timeline::cached_fill(true).a() < timeline::cached_fill(false).a());
        assert!(timeline::cell_inflight().a() < timeline::cell_cached().a());
        assert!(timeline::cell_inflight().a() > timeline::cell_projected_fill().a());
        assert!(timeline::cell_inflight_border().a() > timeline::cell_inflight().a());
        assert!(timeline::available_fill().a() < timeline::available_border().a());
        assert!(timeline::event_fill().a() < timeline::event_border().a());
        assert!(timeline::block_label_weak().a() < timeline::block_label().a());
        assert!(timeline::prev_active_sweep().a() < timeline::ACTIVE_SWEEP.a());
    }

    #[wasm_bindgen_test]
    fn the_timeline_speaks_the_accent_grammar() {
        // White is what you see, amber what loops, red now; on device is
        // slate; failure is text, not red.
        assert!(timeline::ACTIVE_SWEEP == SEEN);
        assert!(timeline::PLAYHEAD == SEEN);
        assert!(timeline::PLAYHEAD_EDGE == SEEN);
        assert!(timeline::RANGE == LOOP);
        assert!(timeline::PENNANT == LOOP);
        assert!(timeline::LIVE_ACTIVE == NOW);
        assert!(timeline::PINNED_HANDLE == NOW);
        assert!(timeline::HELD == ON_DEVICE);
        assert!(timeline::FAILURE_GLYPH == TEXT);
        for c in [timeline::FAILURE_GLYPH, timeline::OUTAGE] {
            assert!(c != NOW && c != NOW_IDLE);
        }
    }

    #[wasm_bindgen_test]
    fn favicon_is_red_only_when_live() {
        assert!(favicon_color(true) == NOW);
        assert!(favicon_color(false) != NOW);
    }

    #[wasm_bindgen_test]
    fn five_sizes_ascend() {
        let sizes = [CAPTION, LABEL, READOUT, BODY, TITLE];
        for w in sizes.windows(2) {
            assert!(w[0] < w[1]);
        }
    }

    #[wasm_bindgen_test]
    fn text_styles_map_onto_the_five_sizes() {
        let styles = text_styles();
        let size = |s: &TextStyle| styles.get(s).map(|f| f.size);
        assert!(size(&TextStyle::Small) == Some(CAPTION));
        assert!(size(&TextStyle::Body) == Some(LABEL));
        assert!(size(&TextStyle::Button) == Some(LABEL));
        assert!(size(&TextStyle::Monospace) == Some(READOUT));
        assert!(size(&body_style()) == Some(BODY));
        assert!(size(&TextStyle::Heading) == Some(TITLE));
        // Readouts are mono; everything else is proportional.
        for (style, font) in &styles {
            let mono = *style == TextStyle::Monospace;
            assert!((font.family == FontFamily::Monospace) == mono);
        }
    }

    #[wasm_bindgen_test]
    fn plex_leads_both_families() {
        let fonts = font_definitions();
        let first = |f: FontFamily| fonts.families.get(&f).and_then(|v| v.first().cloned());
        assert!(first(FontFamily::Proportional).as_deref() == Some(PLEX_SANS));
        assert!(first(FontFamily::Monospace).as_deref() == Some(PLEX_MONO));
        // egui's bundled fonts stay behind Plex as glyph fallbacks.
        assert!(fonts.families[&FontFamily::Proportional].len() > 1);
        assert!(fonts.families[&FontFamily::Monospace].len() > 1);
    }

    #[wasm_bindgen_test]
    fn installed_fonts_load_and_lay_out() {
        // A headless frame parses the bundled TTFs and lays text out in both
        // families; a bad font file would panic here.
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        {
            let style = ctx.global_style();
            assert!(style.text_styles[&TextStyle::Body].size == LABEL);
            let g =
                ctx.fonts_mut(|f| f.layout_no_wrap("12:34:56 ~0.5°".into(), readout_font(), TEXT));
            assert!(g.size().x > 0.0);
            let g = ctx.fonts_mut(|f| {
                f.layout_no_wrap("Reflectivity".into(), FontId::proportional(BODY), TEXT)
            });
            assert!(g.size().x > 0.0);
        }
        ctx.end_pass().textures_delta.clear();
    }

    #[wasm_bindgen_test]
    fn dialog_tokens_follow_the_grammar() {
        // Destructive is not red: red means now.
        assert!(dialog::DESTRUCTIVE != NOW && dialog::DESTRUCTIVE != NOW_IDLE);
        let [r, g, b, _] = dialog::DESTRUCTIVE.to_array();
        assert!(r.abs_diff(g) < 30 && g.abs_diff(b) < 30, "a grey");
        // The primary reads against its own label.
        assert!(dialog::PRIMARY_FILL.r() > dialog::PRIMARY_TEXT.r() + 150);
        assert!(dialog::backdrop().a() < 255);
    }

    #[wasm_bindgen_test]
    fn readout_font_is_mono_at_readout_size() {
        let f = readout_font();
        assert!(f.family == FontFamily::Monospace);
        assert!((f.size - READOUT).abs() < 1e-6);
    }
}
