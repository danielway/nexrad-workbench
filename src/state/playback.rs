//! Playback controls state per PRODUCT.md specification.
//!
//! Implements a dual-time model separating playback position from wall-clock time,
//! with timeline bounds enforcement and zoom-based feature restrictions.

/// Nominal zoom boundary between macro (scan blocks) and micro (individual
/// sweeps) tiers, in pixels per second. The tier state machine adds
/// hysteresis around this value (see [`tier`]); this constant survives only
/// as the seam used for deterministic tier seeding (boot / URL restore) and
/// as the return-to-live floor's reference point.
pub const MICRO_ZOOM_THRESHOLD: f64 = 1.0;

/// Minimum / maximum timeline zoom (pixels per second). Every zoom mutation
/// routes through [`PlaybackState::set_timeline_zoom`], which clamps here.
///
/// The minimum is the *hard floor* that bounds even a very wide strip; the
/// effective per-frame floor is tighter and width-aware (see
/// [`PlaybackState::min_zoom_for_width`] / [`MAX_VIEW_SPAN_SECS`]). The old
/// `0.000001` floor allowed a single linear strip stretched across ~30 years —
/// the "label soup" year-wide zoom the Archive calendar tier replaces (spec
/// §6.4 DECIDED, §15 cut #4). It is now tightened so the widest view is a
/// readable multi-month calendar span, not decades.
pub const TIMELINE_ZOOM_MIN: f64 = 0.00001;
pub const TIMELINE_ZOOM_MAX: f64 = 1000.0;

/// Widest visible span (seconds) any timeline view may show — the ceiling the
/// width-aware min-zoom enforces. The linear Micro/Macro strip stops being the
/// renderer past the Archive-enter span (~60 h); beyond that the Archive
/// **calendar** tier renders day cells over this same zoom scalar, so this
/// ceiling sizes the calendar's widest reach (~a quarter of day cells) rather
/// than the deprecated year-wide strip. Tuned so a multi-month heatmap stays
/// legible while old URLs with absurd (near-zero) zooms clamp sanely into range.
pub const MAX_VIEW_SPAN_SECS: f64 = 100.0 * 86_400.0;

/// Tunable tier thresholds. The timeline's zoom level maps to one of three
/// behavioral+visual tiers; transitions carry hysteresis (distinct enter/exit
/// thresholds) so a zoom hovering at a boundary never flickers. Values are
/// the alignment-pass decisions (see docs/north_star_alignment.md §2); tune
/// here, in one place.
pub mod tier {
    /// Enter Micro (from Macro) when zoom rises to/above this (px/sec).
    pub const MICRO_ENTER_ZOOM: f64 = 1.15;
    /// Exit Micro (to Macro) when zoom falls to/below this (px/sec).
    pub const MICRO_EXIT_ZOOM: f64 = 0.87;
    /// Enter Archive (from Macro) when the visible span exceeds this (seconds).
    pub const ARCHIVE_ENTER_SPAN_SECS: f64 = 60.0 * 3600.0;
    /// Exit Archive (to Macro) when the visible span falls below this (seconds).
    pub const ARCHIVE_EXIT_SPAN_SECS: f64 = 48.0 * 3600.0;
    /// Nominal Archive boundary used only for hysteresis-free seeding
    /// (boot / URL restore), midway between the enter/exit spans.
    pub const ARCHIVE_NOMINAL_SPAN_SECS: f64 = 54.0 * 3600.0;
}

/// The single stored timeline tier. Replaces the two previously-uncoupled
/// per-frame derivations (the behavioral [`PlaybackMode`] and the renderer's
/// detail level). Transitions are owned by [`PlaybackState::set_timeline_zoom`]
/// / the per-frame reconcile, which apply hysteresis; nothing else writes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TimelineTier {
    /// Zoomed in (minutes–hours visible): realtime-multiple playback, frame
    /// cells / sweep detail on the strip.
    #[default]
    Micro,
    /// Zoomed out (hours–days visible): equidistant frame (fps) playback.
    Macro,
    /// Far out (multi-day span): a navigator only — no playback.
    Archive,
}

/// Playback mode derived from the stored timeline tier. The behavioral split
/// the rest of the app reads: Micro tier → continuous realtime-multiple
/// advance; Macro/Archive tier → equidistant frame stepping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaybackMode {
    /// Frame-stepping between matching sweeps (Macro/Archive tier).
    Macro,
    /// Continuous time-based playback (Micro tier).
    Micro,
}

/// Fallback median frame spacing (seconds) for cadence conversion when the
/// macro frame list is too short to derive one — a typical NEXRAD volume
/// interval.
pub const FALLBACK_FRAME_SPACING_SECS: f64 = 300.0;

/// Map a tier (plus the lookback override) to the behavioral playback mode.
/// Free function so it's callable from `apply_tier` before `&mut self`
/// borrows settle, and shared by `playback_mode`/`effective_playback_mode`.
/// Lookback always frame-steps regardless of tier.
fn mode_of_tier(tier: TimelineTier, is_lookback: bool) -> PlaybackMode {
    if is_lookback {
        return PlaybackMode::Macro;
    }
    match tier {
        TimelineTier::Micro => PlaybackMode::Micro,
        TimelineTier::Macro | TimelineTier::Archive => PlaybackMode::Macro,
    }
}

/// The renderer's per-frame visual detail, mapped from the stored tier (with
/// a Macro sub-detail kept for Phase 1; Phase 2 reworks Macro rendering).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetailLevel {
    /// Far out: solid coverage fill only.
    Coverage,
    /// Volume-scan blocks.
    Volumes,
    /// Tilt (sweep) blocks within volume scans.
    Tilts,
}

/// Inputs the macro `sweep_frames` list is built from. Compared against the
/// previous build's inputs to decide whether a rebuild is needed.
#[derive(PartialEq, Clone, Default)]
pub struct MacroFrameInputs {
    pub elevation: super::viz::ElevationSelection,
    /// Selected product (worker-string). A frame is a sweep matching the
    /// product AND tilt, so the list must rebuild when the product changes —
    /// otherwise a stale list survives a product switch.
    pub product: String,
    pub bounds: Option<(f64, f64)>,
    pub scan_count: usize,
}

/// Why the macro frame list needs rebuilding. The elevation case is
/// distinguished because it additionally snaps the playback position to the
/// resolved frame (see `render_loop`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RebuildCause {
    ElevationChanged,
    /// Bounds or scan count changed (streaming, selection edits) — rebuild
    /// without teleporting the cursor.
    WindowChanged,
}

/// State for macro (frame-stepping) playback.
pub struct MacroPlaybackState {
    /// Sorted sweep end-times matching the user's elevation filter.
    pub sweep_frames: Vec<f64>,
    /// Current index into sweep_frames.
    pub current_frame_index: usize,
    /// Fractional frame accumulator for sub-frame advancement.
    pub frame_accumulator: f64,
    /// Inputs the current `sweep_frames` list was built from (dirty check).
    built_from: MacroFrameInputs,
    /// Last known playback position, used to detect manual seeks.
    pub last_seen_position: f64,
}

impl Default for MacroPlaybackState {
    fn default() -> Self {
        Self {
            sweep_frames: Vec::new(),
            current_frame_index: 0,
            frame_accumulator: 0.0,
            built_from: MacroFrameInputs::default(),
            last_seen_position: 0.0,
        }
    }
}

impl MacroPlaybackState {
    /// `None` = the frame list is current; `Some(cause)` = the owner must
    /// rebuild. An elevation change wins over a simultaneous window change
    /// because it carries the snap-to-frame side effect.
    pub fn rebuild_cause(&self, inputs: &MacroFrameInputs) -> Option<RebuildCause> {
        if self.built_from.elevation != inputs.elevation {
            Some(RebuildCause::ElevationChanged)
        } else if self.built_from.product != inputs.product
            || self.built_from.bounds != inputs.bounds
            || self.built_from.scan_count != inputs.scan_count
        {
            // A product switch re-filters the frame list (frame = product +
            // tilt) but keeps the cursor where it is — no snap side-effect, so
            // it's a window-class change.
            Some(RebuildCause::WindowChanged)
        } else {
            None
        }
    }

    /// Install a freshly built frame list and remember the inputs it was
    /// built from.
    pub fn store_rebuilt(&mut self, inputs: MacroFrameInputs, frames: Vec<f64>) {
        self.sweep_frames = frames;
        self.built_from = inputs;
    }
}

/// Playback speed multiplier options.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum PlaybackSpeed {
    /// Real-time: 1 second of timeline = 1 second of real time
    Realtime,
    /// 2x real-time: 2 seconds of timeline per 1 second of real time
    RealtimeDouble,
    /// 15 seconds of timeline per 1 second of real time
    FifteenToOne,
    /// 30 seconds of timeline per 1 second of real time
    ThirtyToOne,
    Quarter,
    Half,
    #[default]
    Normal,
    Double,
    Quadruple,
}

impl PlaybackSpeed {
    /// Micro-mode label: realtime multiple in `×` notation (spec §9). The
    /// multiple is `timeline_seconds_per_real_second()` — 1 timeline-second per
    /// real second is 1×, 1200 is 1200×, etc.
    pub fn label(&self) -> &'static str {
        match self {
            PlaybackSpeed::Realtime => "1×",
            PlaybackSpeed::RealtimeDouble => "2×",
            PlaybackSpeed::FifteenToOne => "15×",
            PlaybackSpeed::ThirtyToOne => "30×",
            PlaybackSpeed::Quarter => "60×",
            PlaybackSpeed::Half => "120×",
            PlaybackSpeed::Normal => "300×",
            PlaybackSpeed::Double => "600×",
            PlaybackSpeed::Quadruple => "1200×",
        }
    }

    pub fn all() -> &'static [PlaybackSpeed] {
        &[
            PlaybackSpeed::Realtime,
            PlaybackSpeed::RealtimeDouble,
            PlaybackSpeed::FifteenToOne,
            PlaybackSpeed::ThirtyToOne,
            PlaybackSpeed::Quarter,
            PlaybackSpeed::Half,
            PlaybackSpeed::Normal,
            PlaybackSpeed::Double,
            PlaybackSpeed::Quadruple,
        ]
    }

    /// Returns the frames-per-second for macro mode, or None if this speed
    /// is not available in macro mode (the real-time / sub-minute speeds).
    pub fn macro_frames_per_second(&self) -> Option<f64> {
        match self {
            PlaybackSpeed::Realtime
            | PlaybackSpeed::RealtimeDouble
            | PlaybackSpeed::FifteenToOne
            | PlaybackSpeed::ThirtyToOne => None,
            PlaybackSpeed::Quarter => Some(1.0),
            PlaybackSpeed::Half => Some(2.0),
            PlaybackSpeed::Normal => Some(5.0),
            PlaybackSpeed::Double => Some(10.0),
            PlaybackSpeed::Quadruple => Some(15.0),
        }
    }

    /// Label for macro mode display (fps-based).
    pub fn macro_label(&self) -> &'static str {
        match self {
            PlaybackSpeed::Realtime => "1x (real)",
            PlaybackSpeed::RealtimeDouble => "2x (real)",
            PlaybackSpeed::FifteenToOne => "15s/s",
            PlaybackSpeed::ThirtyToOne => "30s/s",
            PlaybackSpeed::Quarter => "1 fps",
            PlaybackSpeed::Half => "2 fps",
            PlaybackSpeed::Normal => "5 fps",
            PlaybackSpeed::Double => "10 fps",
            PlaybackSpeed::Quadruple => "15 fps",
        }
    }

    /// Speeds available in macro mode (Quarter through Quadruple).
    pub fn macro_speeds() -> &'static [PlaybackSpeed] {
        &[
            PlaybackSpeed::Quarter,
            PlaybackSpeed::Half,
            PlaybackSpeed::Normal,
            PlaybackSpeed::Double,
            PlaybackSpeed::Quadruple,
        ]
    }

    /// Curated tap-cycle for the compact mobile speed button (spec §13 "speed"
    /// control). A short, mode-appropriate ladder rather than the full combo:
    /// Micro walks the realtime-multiple rungs people actually use; Macro walks
    /// the fps rungs. Wraps at the end. Returns the next speed after `self`,
    /// snapping to the first rung if `self` isn't on the ladder.
    pub fn mobile_cycle(self, mode: PlaybackMode) -> PlaybackSpeed {
        let ladder: &[PlaybackSpeed] = match mode {
            PlaybackMode::Micro => &[
                PlaybackSpeed::Realtime,
                PlaybackSpeed::FifteenToOne,
                PlaybackSpeed::Quarter,
                PlaybackSpeed::Normal,
                PlaybackSpeed::Quadruple,
            ],
            PlaybackMode::Macro => Self::macro_speeds(),
        };
        match ladder.iter().position(|s| *s == self) {
            Some(i) => ladder[(i + 1) % ladder.len()],
            None => ladder[0],
        }
    }

    /// Returns how many seconds of timeline time pass per real second.
    pub fn timeline_seconds_per_real_second(&self) -> f64 {
        match self {
            PlaybackSpeed::Realtime => 1.0,
            PlaybackSpeed::RealtimeDouble => 2.0,
            PlaybackSpeed::FifteenToOne => 15.0,
            PlaybackSpeed::ThirtyToOne => 30.0,
            PlaybackSpeed::Quarter => 60.0,
            PlaybackSpeed::Half => 120.0,
            PlaybackSpeed::Normal => 300.0,
            PlaybackSpeed::Double => 600.0,
            PlaybackSpeed::Quadruple => 1200.0,
        }
    }

    /// The macro-capable variant whose fps is nearest `target_fps` (log-scale
    /// nearness, so doublings read symmetrically). Used by cadence
    /// preservation entering Macro. Defaults to the slowest macro speed when
    /// the target is non-positive.
    pub fn nearest_macro_fps(target_fps: f64) -> PlaybackSpeed {
        Self::nearest_by_log(target_fps, Self::macro_speeds(), |s| {
            s.macro_frames_per_second()
        })
    }

    /// The variant whose timeline multiple is nearest `target_multiple`
    /// (log-scale nearness). Used by cadence preservation entering Micro;
    /// considers all variants since every one is a valid micro multiple.
    pub fn nearest_micro_multiple(target_multiple: f64) -> PlaybackSpeed {
        Self::nearest_by_log(target_multiple, Self::all(), |s| {
            Some(s.timeline_seconds_per_real_second())
        })
    }

    /// Pick the candidate whose `value(candidate)` is closest to `target` on a
    /// log scale. Candidates whose value function returns `None` or a
    /// non-positive value are skipped; falls back to the first candidate.
    fn nearest_by_log(
        target: f64,
        candidates: &'static [PlaybackSpeed],
        value: impl Fn(&PlaybackSpeed) -> Option<f64>,
    ) -> PlaybackSpeed {
        let target = target.max(f64::MIN_POSITIVE);
        let log_target = target.ln();
        candidates
            .iter()
            .filter_map(|s| value(s).filter(|v| *v > 0.0).map(|v| (s, v)))
            .min_by(|(_, a), (_, b)| {
                (a.ln() - log_target)
                    .abs()
                    .partial_cmp(&(b.ln() - log_target).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(s, _)| *s)
            .unwrap_or_else(|| candidates.first().copied().unwrap_or_default())
    }
}

/// Format a "behind live" lag (seconds) for the LIVE button readout (spec §7,
/// e.g. "2:14 behind"). `m:ss` under an hour, `h:mm:ss` at/above one hour.
/// Negative or sub-second lags clamp to `0:00`.
pub fn format_lag(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Default loop-window size when a pinned/preset loop is created without an
/// explicit choice (alignment §7: "last 6 frames"). Replaces the old
/// compile-time `LOOKBACK_FRAMES = 5` so the window size is user-driven state.
pub const DEFAULT_LOOP_FRAMES: u32 = 6;

/// How a loop window's extent is measured. Frame-count windows ("last 6
/// frames") are preferred in Micro since scan spacing varies; duration windows
/// ("last 30 min") are the alternative offered in presets (spec §8).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LoopBasis {
    /// The last `n` matching frames (sweeps of the selected product + tilt).
    FrameCount(u32),
    /// The last `secs` seconds of timeline time.
    Duration(f64),
}

impl Default for LoopBasis {
    fn default() -> Self {
        LoopBasis::FrameCount(DEFAULT_LOOP_FRAMES)
    }
}

impl LoopBasis {
    /// A short menu label for the basis (e.g. "6 frames", "30 min").
    pub fn label(&self) -> String {
        match self {
            LoopBasis::FrameCount(n) => format!("{n} frames"),
            LoopBasis::Duration(secs) => {
                let mins = secs / 60.0;
                if mins >= 60.0 {
                    format!("{:.0} h", mins / 60.0)
                } else {
                    format!("{mins:.0} min")
                }
            }
        }
    }

    /// Conservative fallback span (seconds) the *pinned* sliding window covers
    /// before any matching frame is cached — so `tick_live` can seed bounds
    /// that bound the macro frame list to recent data rather than all history.
    /// Frame-count bases assume the typical volume interval per frame (plus a
    /// volume of slack); duration bases use their own span.
    pub fn fallback_span_secs(&self) -> f64 {
        match self {
            LoopBasis::FrameCount(n) => (*n as f64 + 1.0) * FALLBACK_FRAME_SPACING_SECS,
            LoopBasis::Duration(secs) => *secs,
        }
    }
}

/// First-class loop-window model (spec §8). Lives on [`PlaybackState`], NOT
/// inside [`TimeSelection`] (which `tick_live` overwrites wholesale every frame
/// while replaying) — the per-frame slide reads this to size the sliding
/// window, so it must survive that clobber.
///
/// `pinned` distinguishes the two loop flavors the presets create:
/// - **pinned** (sliding): the window's later edge tracks the live edge as new
///   sweeps arrive (the "loop the last N while still streaming" gesture).
/// - **fixed** (custom range): a static range that does not follow now.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LoopWindow {
    /// How the window's extent is measured (frame-count or duration).
    pub basis: LoopBasis,
    /// Whether the window slides forward with the live edge (`true`) or is a
    /// fixed range (`false`).
    pub pinned: bool,
}

/// A loop preset the user can pick from the transport row / mobile settings
/// (spec §8 "creation order: presets first"). The app turns each into a
/// concrete [`LoopWindow`] + the right playhead transition (alignment #7:
/// menu offers 4/6/10 frames, 30 min / 1 h, and "pin to live").
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LoopPreset {
    /// Enter the pinned sliding loop at the default frame count.
    PinToLive,
    /// A loop of the last `n` matching frames.
    LastFrames(u32),
    /// A duration-basis loop covering the last `secs` seconds.
    LastDuration(f64),
}

impl LoopPreset {
    /// The presets offered in the menu, in display order.
    pub fn menu() -> &'static [LoopPreset] {
        &[
            LoopPreset::PinToLive,
            LoopPreset::LastFrames(4),
            LoopPreset::LastFrames(6),
            LoopPreset::LastFrames(10),
            LoopPreset::LastDuration(30.0 * 60.0),
            LoopPreset::LastDuration(60.0 * 60.0),
        ]
    }

    /// Menu label.
    pub fn label(&self) -> String {
        match self {
            LoopPreset::PinToLive => "Pin to live".to_string(),
            LoopPreset::LastFrames(n) => format!("Last {n} frames"),
            LoopPreset::LastDuration(secs) => {
                let mins = secs / 60.0;
                if mins >= 60.0 {
                    format!("Last {:.0} h", mins / 60.0)
                } else {
                    format!("Last {mins:.0} min")
                }
            }
        }
    }

    /// The loop-window basis this preset resolves to.
    pub fn basis(&self) -> LoopBasis {
        match self {
            LoopPreset::PinToLive => LoopBasis::FrameCount(DEFAULT_LOOP_FRAMES),
            LoopPreset::LastFrames(n) => LoopBasis::FrameCount(*n),
            LoopPreset::LastDuration(secs) => LoopBasis::Duration(*secs),
        }
    }
}

/// Loop behavior when playback bounds are set.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode {
    /// Play forward, jump to start when reaching end
    #[default]
    Loop,
    /// Play forward then backward (ping-pong)
    PingPong,
    /// Stop at end
    Once,
}

impl LoopMode {
    pub fn label(&self) -> &'static str {
        match self {
            LoopMode::Loop => "Loop",
            LoopMode::PingPong => "Ping-Pong",
            LoopMode::Once => "Once",
        }
    }

    pub fn all() -> &'static [LoopMode] {
        &[LoopMode::Loop, LoopMode::PingPong, LoopMode::Once]
    }
}

/// Playback direction for ping-pong mode.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackDirection {
    #[default]
    Forward,
    Backward,
}

/// Playhead position semantics. Replaces the old `locked_to_realtime` +
/// `lookback_active` flag pair with one explicit mode; all transitions go
/// through the named methods on [`PlaybackState`], which own the
/// invariants (bounds ownership, position snapping, macro-cursor resets).
///
/// The live *stream session* is a separate bit (`live.mode_state`): the
/// stream keeps running across `PinnedToNow` ↔ `LookbackLoop`, and the
/// `playing` flag keeps its ordinary meaning in `Free`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PlayheadMode {
    /// Archive: free seeking; `playing` drives `advance()`.
    #[default]
    Free,
    /// Live: the per-frame tick pins the position to wall-clock via
    /// [`PlaybackState::pin_tick`]. Position writes require a transition
    /// out of this mode first.
    PinnedToNow,
    /// Live replay: bounds are owned by the tick's sliding lookback
    /// window; always macro frame-stepping; `playing` is true.
    LookbackLoop,
}

/// Where the playhead lands when leaving live ([`PlaybackState::exit_live`]).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FreezeAt {
    /// Snap to the given wall-clock now (predictable stop on the live edge).
    Now(f64),
    /// Keep the current position (seek/jog paths set it themselves).
    Keep,
}

/// A user time selection on the timeline — ONE concept serving loop/playback
/// bounds, the live replay window, and event ranges. Replaces the old
/// `selection_start`/`selection_end`/`selection_in_progress` field triple.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TimeSelection {
    /// First-placed endpoint (drag origin). Unordered relative to `b`.
    pub a: f64,
    /// Second endpoint — the one a drag moves.
    pub b: f64,
    /// True while a shift-drag is still moving `b`.
    pub in_progress: bool,
    /// The selection's later edge tracks the live edge: while streaming, the
    /// per-frame live tick slides it forward as time passes, so a loop over
    /// "the recent past up to now" keeps following now. The lookback replay
    /// is exactly an anchored selection being played.
    pub anchored_to_live: bool,
}

impl TimeSelection {
    /// A plain static selection between two timestamps.
    pub fn between(a: f64, b: f64) -> Self {
        Self {
            a,
            b,
            in_progress: false,
            anchored_to_live: false,
        }
    }

    /// Normalized `(start, end)`, or `None` until the selection has
    /// meaningful width (> 1 second).
    pub fn range(&self) -> Option<(f64, f64)> {
        let (start, end) = (self.a.min(self.b), self.a.max(self.b));
        ((end - start).abs() > 1.0).then_some((start, end))
    }

    /// Whether a timestamp falls inside the (normalized) selection.
    pub fn contains(&self, ts: f64) -> bool {
        self.range().is_some_and(|(s, e)| ts >= s && ts <= e)
    }

    /// Slide the later edge to `end` (live-anchored follow). The earlier
    /// edge keeps its place so the window's extent grows/slides naturally.
    pub fn slide_end_to(&mut self, end: f64) {
        if self.a <= self.b {
            self.b = end;
        } else {
            self.a = end;
        }
    }
}

/// Time model per PRODUCT.md specification.
///
/// Separates playback position (the moment in radar time being displayed)
/// from wall-clock time (current real-world time).
#[derive(Clone)]
pub struct TimeModel {
    /// Playback position - the moment in radar time being displayed.
    /// This is independent of wall-clock time during archive playback.
    /// Unix seconds with sub-second precision.
    pub playback_position: f64,

    /// Playhead mode (archive / pinned-live / lookback replay).
    pub mode: PlayheadMode,

    /// Playback range constraints (from selection or real-time window).
    /// When set, playback position is constrained to (start, end).
    pub playback_bounds: Option<(f64, f64)>,

    /// Loop behavior when bounds are set.
    pub loop_mode: LoopMode,

    /// Current playback direction (for ping-pong mode).
    pub direction: PlaybackDirection,
}

impl Default for TimeModel {
    fn default() -> Self {
        Self {
            playback_position: Self::wall_clock_time(),
            mode: PlayheadMode::Free,
            playback_bounds: None,
            loop_mode: LoopMode::Loop,
            direction: PlaybackDirection::Forward,
        }
    }
}

impl TimeModel {
    /// Get current wall-clock time as Unix seconds.
    pub fn wall_clock_time() -> f64 {
        js_sys::Date::now() / 1000.0
    }

    /// Create a new time model at the given position.
    pub fn at_position(position: f64) -> Self {
        Self {
            playback_position: position,
            ..Default::default()
        }
    }

    /// Whether the playhead is pinned to "now" (LIVE-NOW).
    pub fn is_pinned(&self) -> bool {
        self.mode == PlayheadMode::PinnedToNow
    }

    /// Whether the playhead is replaying the lookback window (LIVE-LOOKBACK).
    pub fn is_lookback(&self) -> bool {
        self.mode == PlayheadMode::LookbackLoop
    }

    /// Advance playback position by delta time, respecting bounds and loop mode.
    pub fn advance(&mut self, delta_secs: f64, speed: PlaybackSpeed) {
        if self.is_pinned() {
            // LIVE-NOW: the position is owned by the per-frame pin_tick;
            // nothing to advance.
            return;
        }

        let advance_amount = delta_secs * speed.timeline_seconds_per_real_second();

        let effective_advance = match self.direction {
            PlaybackDirection::Forward => advance_amount,
            PlaybackDirection::Backward => -advance_amount,
        };

        let new_position = self.playback_position + effective_advance;

        // Apply bounds if set
        if let Some((start, end)) = self.playback_bounds {
            self.playback_position = self.apply_bounds(new_position, start, end);
        } else {
            self.playback_position = new_position;
        }
    }

    /// Apply bounds with loop behavior.
    fn apply_bounds(&mut self, position: f64, start: f64, end: f64) -> f64 {
        if position >= end {
            match self.loop_mode {
                LoopMode::Loop => start + (position - end) % (end - start),
                LoopMode::PingPong => {
                    self.direction = PlaybackDirection::Backward;
                    end - (position - end).min(end - start)
                }
                LoopMode::Once => end,
            }
        } else if position <= start {
            match self.loop_mode {
                LoopMode::Loop => end - (start - position) % (end - start),
                LoopMode::PingPong => {
                    self.direction = PlaybackDirection::Forward;
                    start + (start - position).min(end - start)
                }
                LoopMode::Once => start,
            }
        } else {
            position
        }
    }

    /// Set bounds from a selection range.
    pub fn set_bounds_from_selection(&mut self, start: f64, end: f64) {
        let (s, e) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.playback_bounds = Some((s, e));
        // Reset direction when bounds change
        self.direction = PlaybackDirection::Forward;
        // Ensure playback position is within bounds
        self.playback_position = self.playback_position.clamp(s, e);
    }

    /// Clear playback bounds.
    pub fn clear_bounds(&mut self) {
        self.playback_bounds = None;
        self.direction = PlaybackDirection::Forward;
    }

    /// Update playback bounds *without* resetting direction or clamping the
    /// position. Used by the sliding lookback window: as new frames complete,
    /// the window end follows the latest frame while the loop keeps its
    /// progression — `apply_bounds` re-contains the position on the next
    /// advance. Contrast [`set_bounds_from_selection`], which resets a fresh
    /// selection.
    pub fn set_bounds_preserving(&mut self, start: f64, end: f64) {
        let (s, e) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.playback_bounds = Some((s, e));
    }
}

/// State for playback controls.
pub struct PlaybackState {
    /// Whether playback is currently active
    pub playing: bool,

    /// Time model (playback position, bounds, loop mode)
    pub time_model: TimeModel,

    /// Current playback speed
    pub speed: PlaybackSpeed,

    /// Timeline zoom level (pixels per second)
    pub timeline_zoom: f64,

    /// The single stored timeline tier (see [`TimelineTier`]). Seeded from
    /// zoom+width at construction/URL restore and thereafter advanced with
    /// hysteresis by [`Self::set_timeline_zoom`] / [`Self::reconcile_tier`].
    /// Every behavioral+visual tier read derives from this — never from raw
    /// `timeline_zoom`.
    pub timeline_tier: TimelineTier,

    /// Timeline view position - absolute timestamp of left edge (Unix seconds)
    pub timeline_view_start: f64,

    /// The user's timeline selection, if any (see [`TimeSelection`]).
    pub selection: Option<TimeSelection>,

    /// The active loop window's basis + pinned-ness (see [`LoopWindow`]), if a
    /// loop exists. `Some` whenever a loop is active — a fixed range (alongside
    /// `playback_bounds`) or a pinned replay (whose bounds `tick_live` owns).
    /// Kept in step with the selection/bounds by the selection/preset/handle
    /// APIs. Sized state that `tick_live` reads to slide the pinned window — it
    /// must NOT live in the selection, which the live tick rewrites every frame.
    pub loop_window: Option<LoopWindow>,

    /// Wrap-point incorporation buffer (spec §8): while *playing* a pinned
    /// loop, the freshly resolved target window is parked here instead of being
    /// applied immediately. It is committed to `playback_bounds` only when the
    /// playhead crosses the loop's wrap point, so the visible band and frame set
    /// stay fixed between wraps and newly arrived frames "enter at the wrap,
    /// never mid-cycle". Cleared (and bypassed) while not playing — a
    /// paused/idle pinned window may track now continuously. See
    /// [`Self::commit_pinned_window`] and the wrap branch of
    /// [`Self::step_macro_frame_internal`].
    pub pending_loop_window: Option<(f64, f64)>,

    /// Actual pixel width of the timeline widget (set by render_timeline each frame).
    /// Used for accurate view centering calculations outside the render function.
    pub timeline_width_px: f64,

    /// State for macro (frame-stepping) playback mode.
    pub macro_playback: MacroPlaybackState,
}

impl Default for PlaybackState {
    fn default() -> Self {
        let now = TimeModel::wall_clock_time();
        let zoom = 0.15; // ~0.15 px/sec means ~1.8 hours visible in 1000px
        let view_width_secs = 1000.0 / zoom;

        Self {
            playing: false,
            time_model: TimeModel::at_position(now),
            speed: PlaybackSpeed::default(),
            timeline_zoom: zoom,
            timeline_tier: Self::seed_tier(zoom, 1000.0),
            timeline_view_start: now - view_width_secs / 2.0,
            selection: None,
            loop_window: None,
            pending_loop_window: None,
            timeline_width_px: 1000.0,
            macro_playback: MacroPlaybackState::default(),
        }
    }
}

impl PlaybackState {
    pub fn new_at_time(now: f64) -> Self {
        let zoom = 0.15;
        let view_width_secs = 1000.0 / zoom;

        Self {
            time_model: TimeModel::at_position(now),
            timeline_view_start: now - view_width_secs / 2.0,
            ..Default::default()
        }
    }

    /// Get the current playback position (convenience accessor).
    pub fn playback_position(&self) -> f64 {
        self.time_model.playback_position
    }

    /// Seek: write the playback position. Position writes while live must
    /// go through a transition out of `PinnedToNow`/`LookbackLoop` first
    /// (`exit_live`) — every UI seek path does, and this assert keeps it
    /// that way.
    pub fn set_playback_position(&mut self, position: f64) {
        debug_assert!(
            self.time_model.mode == PlayheadMode::Free,
            "seek while {:?} — exit live first",
            self.time_model.mode
        );
        self.time_model.playback_position = position;
    }

    // ------------------------------------------------------------------
    // Playhead mode transitions — the ONLY mutation points for
    // `TimeModel::mode`. Each owns its invariants so call sites can't
    // half-apply a mode switch.
    // ------------------------------------------------------------------

    /// → LIVE-NOW. Pins the playhead to `now`; the per-frame live tick
    /// keeps it there via [`Self::pin_tick`]. Drops any bounds (live owns
    /// its own constraints).
    pub fn enter_pinned_live(&mut self, now: f64) {
        self.time_model.playback_bounds = None;
        self.loop_window = None;
        self.pending_loop_window = None;
        self.time_model.mode = PlayheadMode::PinnedToNow;
        self.time_model.playback_position = now;
    }

    /// LIVE-* → ARCHIVE (`Free`). Clears the lookback replay's anchored
    /// selection + window if one was active; a user's static shift-drag
    /// selection (only settable in `Free`) is untouched. `freeze` picks the
    /// landing position. Idempotent when already `Free`.
    pub fn exit_live(&mut self, freeze: FreezeAt) {
        if self.time_model.mode == PlayheadMode::LookbackLoop {
            self.clear_anchored_selection();
            self.loop_window = None;
            self.pending_loop_window = None;
            self.time_model.clear_bounds();
        }
        self.time_model.mode = PlayheadMode::Free;
        if let FreezeAt::Now(now) = freeze {
            self.time_model.playback_position = now;
        }
    }

    /// LIVE-NOW → LIVE-LOOKBACK: discretely step + loop the recent matching
    /// frames. The window (`playback_bounds`) is owned by `tick_live`, which
    /// `render_loop` turns into the macro `sweep_frames` list that
    /// `advance_macro` steps over — lookback is macro frame-stepping forced
    /// on regardless of zoom (see [`Self::effective_playback_mode`]).
    /// `seed` places the playhead (oldest cached frame) so the first pass
    /// runs oldest→newest.
    ///
    /// `basis` records the pinned window's measure (frame-count / duration) so
    /// `tick_live` and the backfill pump can size the sliding window; it is
    /// stored as a pinned [`LoopWindow`].
    pub fn enter_lookback(&mut self, seed: Option<f64>, basis: LoopBasis) {
        debug_assert!(
            self.time_model.mode == PlayheadMode::PinnedToNow,
            "lookback starts from pinned live, not {:?}",
            self.time_model.mode
        );
        self.time_model.mode = PlayheadMode::LookbackLoop;
        self.time_model.loop_mode = LoopMode::Loop;
        self.loop_window = Some(LoopWindow {
            basis,
            pinned: true,
        });
        if let Some(ts) = seed {
            self.time_model.playback_position = ts;
        }
        self.playing = true;
        self.macro_playback.current_frame_index = 0;
        self.macro_playback.frame_accumulator = 0.0;
    }

    /// LIVE-LOOKBACK → LIVE-NOW (pause during replay): drop the loop window
    /// (its anchored selection included) and re-pin to `now`. The stream is
    /// untouched.
    pub fn exit_lookback_to_now(&mut self, now: f64) {
        self.clear_anchored_selection();
        self.loop_window = None;
        self.pending_loop_window = None;
        self.time_model.clear_bounds();
        self.time_model.mode = PlayheadMode::PinnedToNow;
        self.time_model.playback_position = now;
    }

    /// Per-frame position pin while LIVE-NOW (called from the live tick,
    /// independent of the `playing` flag).
    pub fn pin_tick(&mut self, now: f64) {
        debug_assert!(
            self.time_model.mode == PlayheadMode::PinnedToNow,
            "pin_tick outside PinnedToNow ({:?})",
            self.time_model.mode
        );
        self.time_model.playback_position = now;
    }

    /// Visible time width in seconds, using the real timeline widget width.
    pub fn view_width_secs(&self) -> f64 {
        if self.timeline_zoom > 0.0 {
            self.timeline_width_px / self.timeline_zoom
        } else {
            0.0
        }
    }

    /// Center the timeline view on a given timestamp.
    pub fn center_view_on(&mut self, ts: f64) {
        self.timeline_view_start = ts - self.view_width_secs() / 2.0;
    }

    // ------------------------------------------------------------------
    // Timeline tier state machine. `timeline_tier` is the single source of
    // truth for both the behavioral split (Micro/Macro playback) and the
    // renderer's detail level. The raw `timeline_zoom` scalar feeds it but
    // is never read directly for tier decisions — that would lose the
    // hysteresis memory the field carries.
    // ------------------------------------------------------------------

    /// Visible time span for a given zoom + widget width, in seconds. The
    /// Archive boundary is span-based (a wide window of history) rather than
    /// pure zoom, so it stays meaningful as the strip widens.
    fn visible_span_secs(zoom: f64, width_px: f64) -> f64 {
        if zoom > 0.0 {
            width_px / zoom
        } else {
            f64::INFINITY
        }
    }

    /// Deterministically classify zoom+width into a tier with NO hysteresis
    /// memory, using the nominal boundaries. For boot / URL restore, where
    /// there is no prior tier to bias the decision.
    pub fn seed_tier(zoom: f64, width_px: f64) -> TimelineTier {
        if Self::visible_span_secs(zoom, width_px) > tier::ARCHIVE_NOMINAL_SPAN_SECS {
            TimelineTier::Archive
        } else if zoom >= MICRO_ZOOM_THRESHOLD {
            TimelineTier::Micro
        } else {
            TimelineTier::Macro
        }
    }

    /// Reseed the tier from the current zoom+width without hysteresis. Used at
    /// boot / URL restore once `timeline_zoom` is set.
    pub fn seed_tier_from_state(&mut self) {
        self.timeline_tier = Self::seed_tier(self.timeline_zoom, self.timeline_width_px);
    }

    /// Compute the tier a zoom+width should land in, given the current tier
    /// (so enter/exit thresholds differ — hysteresis). Pure: does not mutate.
    fn next_tier(&self, zoom: f64, width_px: f64) -> TimelineTier {
        let span = Self::visible_span_secs(zoom, width_px);
        match self.timeline_tier {
            TimelineTier::Micro => {
                // Leave Micro only once zoom drops to the (lower) exit floor.
                if zoom <= tier::MICRO_EXIT_ZOOM {
                    if span > tier::ARCHIVE_ENTER_SPAN_SECS {
                        TimelineTier::Archive
                    } else {
                        TimelineTier::Macro
                    }
                } else {
                    TimelineTier::Micro
                }
            }
            TimelineTier::Macro => {
                if zoom >= tier::MICRO_ENTER_ZOOM {
                    TimelineTier::Micro
                } else if span > tier::ARCHIVE_ENTER_SPAN_SECS {
                    TimelineTier::Archive
                } else {
                    TimelineTier::Macro
                }
            }
            TimelineTier::Archive => {
                // Leave Archive only once the span shrinks past the (lower)
                // exit span; then re-evaluate Micro vs Macro by zoom.
                if span < tier::ARCHIVE_EXIT_SPAN_SECS {
                    if zoom >= tier::MICRO_ENTER_ZOOM {
                        TimelineTier::Micro
                    } else {
                        TimelineTier::Macro
                    }
                } else {
                    TimelineTier::Archive
                }
            }
        }
    }

    /// Apply a tier change, running the side effects a transition owns:
    /// preserve playback cadence across a Micro↔Macro flip (spec §9) and reset
    /// the sub-frame accumulator on a behavioral flip. No-op when the tier is
    /// unchanged. Runs regardless of `playing`, so paused (and mobile)
    /// transitions get cadence preservation too.
    fn apply_tier(&mut self, next: TimelineTier, median_frame_spacing: f64) {
        let prev = self.timeline_tier;
        if prev == next {
            return;
        }
        let prev_mode = mode_of_tier(prev, self.time_model.is_lookback());
        self.timeline_tier = next;
        let new_mode = mode_of_tier(next, self.time_model.is_lookback());

        if prev_mode != new_mode {
            self.preserve_cadence_across_snap(prev_mode, new_mode, median_frame_spacing);
            // Reset the sub-frame accumulator on any behavioral flip so the
            // next advance starts clean.
            self.macro_playback.frame_accumulator = 0.0;
        }
    }

    /// The width-aware minimum zoom (px/sec): the smallest zoom whose visible
    /// span does not exceed [`MAX_VIEW_SPAN_SECS`] at `width_px`, floored at the
    /// hard [`TIMELINE_ZOOM_MIN`]. This is what stops the strip zooming out into
    /// the deprecated year-wide view: past the Archive-enter span the calendar
    /// tier renders, and this floor keeps even the calendar's widest reach to a
    /// readable multi-month span (spec §6.4 DECIDED). Width-aware so the ceiling
    /// holds whether the strip is a phone sliver or a wide desktop.
    pub fn min_zoom_for_width(width_px: f64) -> f64 {
        let w = width_px.max(1.0);
        (w / MAX_VIEW_SPAN_SECS).max(TIMELINE_ZOOM_MIN)
    }

    /// The single zoom-mutation path. Clamps, stores the zoom, and advances
    /// the tier with hysteresis (running the cadence-preservation side effect
    /// on a behavioral flip). `width_px` is the current strip width; pass
    /// `timeline_width_px` when unknown. `median_frame_spacing` feeds cadence
    /// conversion (see [`Self::median_frame_spacing`]).
    ///
    /// The minimum is width-aware ([`Self::min_zoom_for_width`]) so the widest
    /// view stays a readable calendar span instead of the deprecated year-wide
    /// strip.
    pub fn set_timeline_zoom(&mut self, zoom: f64, width_px: f64, median_frame_spacing: f64) {
        let min = Self::min_zoom_for_width(width_px);
        self.timeline_zoom = zoom.clamp(min, TIMELINE_ZOOM_MAX);
        let next = self.next_tier(self.timeline_zoom, width_px);
        self.apply_tier(next, median_frame_spacing);
    }

    /// Per-frame reconcile: the strip width can change (responsive layout)
    /// even when zoom is untouched, which moves the Archive span boundary.
    /// Re-evaluate the tier against the current width with hysteresis. Cheap
    /// and idempotent when nothing moved.
    pub fn reconcile_tier(&mut self, width_px: f64, median_frame_spacing: f64) {
        let next = self.next_tier(self.timeline_zoom, width_px);
        self.apply_tier(next, median_frame_spacing);
    }

    /// Whether a zoom change (at the current width) would transition *out of*
    /// the Micro tier — i.e. the hysteresis-aware "zoomed out far enough to
    /// detach" gesture. Pure; lets the interaction layer decide to detach
    /// before committing the zoom.
    pub fn zoom_would_exit_micro(&self, new_zoom: f64, width_px: f64) -> bool {
        self.timeline_tier == TimelineTier::Micro
            && self.next_tier(new_zoom, width_px) != TimelineTier::Micro
    }

    /// Check if playback is allowed at the current tier. The Archive tier is a
    /// navigator only (spec §6.4); every other tier permits playback.
    pub fn is_playback_allowed(&self) -> bool {
        self.timeline_tier != TimelineTier::Archive
    }

    /// Derive the current playback mode from the stored tier.
    pub fn playback_mode(&self) -> PlaybackMode {
        mode_of_tier(self.timeline_tier, false)
    }

    /// Playback mode for advance dispatch + speed UI. Lookback always
    /// frame-steps (Macro) regardless of tier so it snaps between the recent
    /// sweeps as frames; otherwise this is the tier-derived
    /// [`Self::playback_mode`].
    pub fn effective_playback_mode(&self) -> PlaybackMode {
        mode_of_tier(self.timeline_tier, self.time_model.is_lookback())
    }

    /// The renderer's visual detail for this frame, derived from the stored
    /// tier (not raw zoom). Micro → Tilts (frames-first cells); Macro →
    /// Volumes (uniform ticks, with the renderer deciding tick-vs-coverage by
    /// density); Archive → Coverage. The former `MACRO_VOLUMES_ZOOM` sub-detail
    /// stopgap is gone — the Macro track now merges to coverage by tick
    /// density, not a zoom threshold.
    pub fn detail_level(&self) -> DetailLevel {
        match self.timeline_tier {
            TimelineTier::Micro => DetailLevel::Tilts,
            TimelineTier::Macro => DetailLevel::Volumes,
            TimelineTier::Archive => DetailLevel::Coverage,
        }
    }

    /// Median frame spacing (seconds) of the current macro frame list, for
    /// cadence conversion. Derived from consecutive `sweep_frames` deltas when
    /// ≥2 frames exist; otherwise the typical volume interval. Tiny lists are
    /// fine — the median of one delta is that delta.
    pub fn median_frame_spacing(&self) -> f64 {
        let frames = &self.macro_playback.sweep_frames;
        if frames.len() < 2 {
            return FALLBACK_FRAME_SPACING_SECS;
        }
        let mut deltas: Vec<f64> = frames.windows(2).map(|w| w[1] - w[0]).collect();
        deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = deltas.len() / 2;
        let median = if deltas.len().is_multiple_of(2) {
            (deltas[mid - 1] + deltas[mid]) / 2.0
        } else {
            deltas[mid]
        };
        if median > 0.0 {
            median
        } else {
            FALLBACK_FRAME_SPACING_SECS
        }
    }

    /// Preserve perceived playback rhythm across a Micro↔Macro snap (spec §9).
    /// Converts the current effective frame cadence to the nearest valid speed
    /// in the new mode. Runs regardless of `playing` (paused transitions and
    /// mobile get it too). The shared [`PlaybackSpeed`] enum is kept intact —
    /// only the selected variant changes.
    fn preserve_cadence_across_snap(
        &mut self,
        from: PlaybackMode,
        to: PlaybackMode,
        frame_spacing: f64,
    ) {
        let spacing = if frame_spacing > 0.0 {
            frame_spacing
        } else {
            FALLBACK_FRAME_SPACING_SECS
        };
        match (from, to) {
            (PlaybackMode::Micro, PlaybackMode::Macro) => {
                // Micro multiple ÷ frame spacing = effective frames per second.
                let target_fps = self.speed.timeline_seconds_per_real_second() / spacing;
                self.speed = PlaybackSpeed::nearest_macro_fps(target_fps);
            }
            (PlaybackMode::Macro, PlaybackMode::Micro) => {
                // Current fps × frame spacing = target timeline multiple.
                let fps = self.speed.macro_frames_per_second().unwrap_or(5.0);
                let target_multiple = fps * spacing;
                self.speed = PlaybackSpeed::nearest_micro_multiple(target_multiple);
            }
            _ => {}
        }
    }

    /// Advance playback by delta time (micro/continuous mode).
    pub fn advance(&mut self, delta_secs: f64) {
        if self.playing {
            self.time_model.advance(delta_secs, self.speed);
        }
    }

    /// Advance playback in macro mode: step through frames at constant fps.
    pub fn advance_macro(&mut self, delta_secs: f64) {
        if !self.playing {
            return;
        }
        let frames = &self.macro_playback.sweep_frames;
        if frames.is_empty() {
            return;
        }

        let fps = self.speed.macro_frames_per_second().unwrap_or(5.0);
        self.macro_playback.frame_accumulator += delta_secs * fps;

        while self.macro_playback.frame_accumulator >= 1.0 {
            self.macro_playback.frame_accumulator -= 1.0;
            let delta = match self.time_model.direction {
                PlaybackDirection::Forward => 1,
                PlaybackDirection::Backward => -1,
            };
            let stepped = self.step_macro_frame_internal(delta);
            if !stepped {
                break;
            }
        }
    }

    /// Step the macro frame index by `delta` (+1 = forward, -1 = backward).
    /// Snaps playback_position to the frame's timestamp.
    pub fn step_macro_frame(&mut self, delta: isize) {
        let frames = &self.macro_playback.sweep_frames;
        if frames.is_empty() {
            return;
        }
        self.step_macro_frame_internal(delta);
    }

    /// Internal frame step, returns false if playback should stop (Once mode at boundary).
    fn step_macro_frame_internal(&mut self, delta: isize) -> bool {
        let len = self.macro_playback.sweep_frames.len();
        if len == 0 {
            return false;
        }
        let idx = self.macro_playback.current_frame_index;
        let new_idx = idx as isize + delta;

        if new_idx >= len as isize {
            // Past end
            match self.time_model.loop_mode {
                LoopMode::Loop => {
                    // Wrap point: flush any pinned window parked while playing so
                    // newly arrived frames enter the loop here, never mid-cycle
                    // (spec §8). No-op when nothing is parked / not a pinned loop.
                    self.apply_pending_window_at_wrap();
                    self.macro_playback.current_frame_index = 0;
                }
                LoopMode::PingPong => {
                    self.time_model.direction = PlaybackDirection::Backward;
                    self.macro_playback.current_frame_index = len.saturating_sub(1);
                }
                LoopMode::Once => {
                    self.macro_playback.current_frame_index = len - 1;
                    self.playing = false;
                    self.snap_playback_to_macro_frame();
                    return false;
                }
            }
        } else if new_idx < 0 {
            // Before start
            match self.time_model.loop_mode {
                LoopMode::Loop => {
                    self.macro_playback.current_frame_index = len.saturating_sub(1);
                }
                LoopMode::PingPong => {
                    self.time_model.direction = PlaybackDirection::Forward;
                    self.macro_playback.current_frame_index = 0;
                }
                LoopMode::Once => {
                    self.macro_playback.current_frame_index = 0;
                    self.playing = false;
                    self.snap_playback_to_macro_frame();
                    return false;
                }
            }
        } else {
            self.macro_playback.current_frame_index = new_idx as usize;
        }

        self.snap_playback_to_macro_frame();
        true
    }

    /// Snap playback position to the current macro frame's timestamp.
    pub fn snap_playback_to_macro_frame(&mut self) {
        if let Some(&ts) = self
            .macro_playback
            .sweep_frames
            .get(self.macro_playback.current_frame_index)
        {
            self.time_model.playback_position = ts;
        }
    }

    /// Sync the macro frame index to the nearest frame matching the current playback position.
    pub fn sync_macro_frame_index(&mut self) {
        let frames = &self.macro_playback.sweep_frames;
        if frames.is_empty() {
            self.macro_playback.current_frame_index = 0;
            return;
        }
        let pos = self.time_model.playback_position;
        // Binary search for the closest frame
        let idx = frames.partition_point(|&t| t < pos);
        let best = if idx >= frames.len() {
            frames.len() - 1
        } else if idx == 0 {
            0
        } else {
            // Compare distance to idx-1 and idx
            if (frames[idx] - pos).abs() < (frames[idx - 1] - pos).abs() {
                idx
            } else {
                idx - 1
            }
        };
        self.macro_playback.current_frame_index = best;
    }

    /// Get the normalized selection range (start <= end), if any.
    pub fn selection_range(&self) -> Option<(f64, f64)> {
        self.selection.as_ref().and_then(|s| s.range())
    }

    /// Whether a shift-drag selection is currently being drawn.
    pub fn selection_in_progress(&self) -> bool {
        self.selection.as_ref().is_some_and(|s| s.in_progress)
    }

    /// Whether a timestamp falls inside the current selection.
    pub fn selection_contains(&self, ts: f64) -> bool {
        self.selection.as_ref().is_some_and(|s| s.contains(ts))
    }

    /// Replace the selection with a static range (event jump, shift+click).
    pub fn set_selection(&mut self, a: f64, b: f64) {
        self.selection = Some(TimeSelection::between(a, b));
    }

    /// Start a shift-drag selection at `ts` (both endpoints collapsed).
    pub fn begin_selection_drag(&mut self, ts: f64) {
        self.selection = Some(TimeSelection {
            a: ts,
            b: ts,
            in_progress: true,
            anchored_to_live: false,
        });
    }

    /// Move the dragged endpoint while a shift-drag is in progress.
    pub fn update_selection_drag(&mut self, ts: f64) {
        if let Some(sel) = self.selection.as_mut() {
            if sel.in_progress {
                sel.b = ts;
            }
        }
    }

    /// Finish a shift-drag. Returns true when the settled selection has a
    /// meaningful range (callers then apply it as bounds / anchor it).
    pub fn end_selection_drag(&mut self) -> bool {
        match self.selection.as_mut() {
            Some(sel) if sel.in_progress => {
                sel.in_progress = false;
                sel.range().is_some()
            }
            _ => false,
        }
    }

    /// Anchor the selection's later edge to the live edge (it slides
    /// forward with now while streaming). Mirrors the pin into the loop window
    /// so the band reads as pinned.
    pub fn anchor_selection_to_live(&mut self) {
        if let Some(sel) = self.selection.as_mut() {
            sel.anchored_to_live = true;
        }
        if let Some(w) = self.loop_window.as_mut() {
            w.pinned = true;
        }
    }

    /// Un-anchor the selection (a dragged right handle moved off the live
    /// edge): the loop becomes a fixed range. Mirrors the un-pin into the loop
    /// window. The selection keeps its current bounds.
    pub fn unanchor_selection_from_live(&mut self) {
        if let Some(sel) = self.selection.as_mut() {
            sel.anchored_to_live = false;
        }
        if let Some(w) = self.loop_window.as_mut() {
            w.pinned = false;
        }
    }

    /// Per-frame follow for a live-anchored selection: slide its later edge
    /// to `end` and keep playback bounds in step (without resetting the
    /// loop's direction or clamping the position).
    pub fn slide_anchored_selection(&mut self, end: f64) {
        let Some(sel) = self.selection.as_mut() else {
            return;
        };
        if !sel.anchored_to_live || sel.in_progress {
            return;
        }
        sel.slide_end_to(end);
        if self.time_model.playback_bounds.is_some() {
            if let Some((s, e)) = sel.range() {
                self.time_model.set_bounds_preserving(s, e);
            }
        }
    }

    /// Drop a live-anchored selection (stream stopped / re-pinned to now).
    /// A static user selection is left untouched.
    pub fn clear_anchored_selection(&mut self) {
        if self.selection.is_some_and(|s| s.anchored_to_live) {
            self.clear_selection();
        }
    }

    /// Clear the current selection.
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.loop_window = None;
        self.pending_loop_window = None;
        self.time_model.clear_bounds();
    }

    /// Apply selection as playback bounds. Records a matching [`LoopWindow`]: a
    /// `Duration` basis equal to the selection's span, `pinned` mirroring the
    /// selection's live-anchoring (so a selection ending near now while
    /// streaming reads as a pinned loop). The anchored-selection slide path
    /// owns its own bounds tracking — the basis here is descriptive, not a
    /// re-derivation source for these selection-driven loops.
    pub fn apply_selection_as_bounds(&mut self) {
        if let Some((start, end)) = self.selection_range() {
            self.time_model.set_bounds_from_selection(start, end);
            let pinned = self.selection.is_some_and(|s| s.anchored_to_live);
            self.loop_window = Some(LoopWindow {
                basis: LoopBasis::Duration(end - start),
                pinned,
            });
        }
    }

    // ------------------------------------------------------------------
    // Wrap-point incorporation (spec §8). The pinned sliding loop must not
    // grow/shift mid-cycle: a frame that arrives while the loop is playing
    // joins the active set only when the playhead next wraps. The split is:
    // `commit_pinned_window` parks the resolved target while playing and
    // returns the still-committed bounds; the macro wrap branch flushes the
    // parked window via `apply_pending_window_at_wrap`.
    // ------------------------------------------------------------------

    /// Decide which window to apply this frame for a pinned loop, given the
    /// freshly resolved `target`. Returns the window the caller should set as
    /// `playback_bounds`.
    ///
    /// - **Not playing** (paused/idle pinned): track now continuously — return
    ///   the target and clear any parked window.
    /// - **Playing, no committed window yet**: commit the target immediately
    ///   (first frame of the loop has nothing to disrupt).
    /// - **Playing, target differs from committed**: park the target and keep
    ///   returning the committed window until the wrap flushes it.
    pub fn commit_pinned_window(&mut self, target_start: f64, target_end: f64) -> (f64, f64) {
        let target = order_pair(target_start, target_end);
        if !self.playing {
            self.pending_loop_window = None;
            return target;
        }
        match self.time_model.playback_bounds {
            None => {
                self.pending_loop_window = None;
                target
            }
            Some(current) => {
                if target != current {
                    self.pending_loop_window = Some(target);
                } else {
                    self.pending_loop_window = None;
                }
                current
            }
        }
    }

    /// Flush a parked pinned window into `playback_bounds` at the loop's wrap
    /// point. No-op when nothing is parked. The render loop rebuilds the macro
    /// frame list from the new bounds (a `WindowChanged` rebuild, which re-syncs
    /// to the nearest frame without teleporting the cursor).
    fn apply_pending_window_at_wrap(&mut self) {
        if let Some((s, e)) = self.pending_loop_window.take() {
            self.time_model.set_bounds_preserving(s, e);
        }
    }
}

/// Order a pair so `.0 <= .1`.
fn order_pair(a: f64, b: f64) -> (f64, f64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn mobile_speed_cycle_wraps_within_mode() {
        // Micro ladder wraps from the fastest rung back to realtime.
        assert_eq!(
            PlaybackSpeed::Realtime.mobile_cycle(PlaybackMode::Micro),
            PlaybackSpeed::FifteenToOne
        );
        assert_eq!(
            PlaybackSpeed::Quadruple.mobile_cycle(PlaybackMode::Micro),
            PlaybackSpeed::Realtime
        );
        // A speed not on the micro ladder snaps to its first rung.
        assert_eq!(
            PlaybackSpeed::ThirtyToOne.mobile_cycle(PlaybackMode::Micro),
            PlaybackSpeed::Realtime
        );
        // Macro ladder is the fps rungs and also wraps.
        assert_eq!(
            PlaybackSpeed::Quadruple.mobile_cycle(PlaybackMode::Macro),
            PlaybackSpeed::Quarter
        );
        assert_eq!(
            PlaybackSpeed::Quarter.mobile_cycle(PlaybackMode::Macro),
            PlaybackSpeed::Half
        );
    }

    #[wasm_bindgen_test]
    fn enter_lookback_sets_loop_seed_and_resets_macro_cursor() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        // Pretend a prior macro session left a non-zero cursor.
        ps.macro_playback.current_frame_index = 7;
        ps.macro_playback.frame_accumulator = 0.4;
        ps.enter_lookback(Some(940.0), LoopBasis::FrameCount(10));
        assert!(ps.time_model.loop_mode == LoopMode::Loop);
        assert!(ps.time_model.is_lookback());
        assert!(ps.playing);
        assert_eq!(ps.playback_position(), 940.0);
        assert_eq!(ps.macro_playback.current_frame_index, 0);
        assert_eq!(ps.macro_playback.frame_accumulator, 0.0);
        // The basis is recorded as a pinned loop window for tick_live to read.
        assert_eq!(
            ps.loop_window,
            Some(LoopWindow {
                basis: LoopBasis::FrameCount(10),
                pinned: true,
            })
        );
    }

    #[wasm_bindgen_test]
    fn format_lag_minutes_and_hours() {
        // Negative / sub-second clamps to 0:00.
        assert_eq!(format_lag(-5.0), "0:00");
        assert_eq!(format_lag(0.4), "0:00");
        // Under an hour: m:ss.
        assert_eq!(format_lag(9.0), "0:09");
        assert_eq!(format_lag(134.0), "2:14"); // the spec's "2:14 behind"
        assert_eq!(format_lag(59.0 * 60.0 + 59.0), "59:59");
        // At/above an hour: h:mm:ss.
        assert_eq!(format_lag(3600.0), "1:00:00");
        assert_eq!(format_lag(3600.0 + 2.0 * 60.0 + 5.0), "1:02:05");
        assert_eq!(format_lag(11.0 * 3600.0 + 9.0 * 60.0 + 3.0), "11:09:03");
    }

    #[wasm_bindgen_test]
    fn effective_mode_is_macro_iff_lookback() {
        let mut ps = PlaybackState::default();
        // Land the tier in Micro via the mutation path (zoom alone no longer
        // drives the mode — the stored tier does).
        ps.set_timeline_zoom(2.0, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Micro);
        assert!(ps.effective_playback_mode() == PlaybackMode::Micro);
        ps.enter_pinned_live(1000.0);
        ps.enter_lookback(None, LoopBasis::default());
        assert!(ps.effective_playback_mode() == PlaybackMode::Macro);
    }

    #[wasm_bindgen_test]
    fn min_zoom_clamp_caps_widest_view_at_calendar_span() {
        // The width-aware floor bounds the visible span to MAX_VIEW_SPAN_SECS.
        let width = 1200.0;
        let min = PlaybackState::min_zoom_for_width(width);
        // At the floor, the span equals the ceiling (within float slop).
        let span_at_min = width / min;
        assert!(
            (span_at_min - MAX_VIEW_SPAN_SECS).abs() < 1.0,
            "span {span_at_min}"
        );
        // The widest linear view is far short of a year — the deprecated
        // year-wide strip is gone.
        assert!(span_at_min < 365.0 * 86_400.0);

        // A zoom request below the floor clamps UP to it, never to the old
        // ~30-year minimum.
        let mut ps = PlaybackState::default();
        ps.set_timeline_zoom(1e-9, width, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_zoom, min);
        // And the resulting view can't show beyond the ceiling.
        assert!(width / ps.timeline_zoom <= MAX_VIEW_SPAN_SECS + 1.0);
    }

    #[wasm_bindgen_test]
    fn min_zoom_is_width_aware() {
        // span = width / zoom, so holding the same span ceiling, a WIDER strip
        // needs a LARGER floor (more pixels per second).
        let narrow = PlaybackState::min_zoom_for_width(300.0);
        let wide = PlaybackState::min_zoom_for_width(2400.0);
        assert!(wide > narrow);
        // Both hold the span ceiling.
        assert!((300.0 / narrow - MAX_VIEW_SPAN_SECS).abs() < 1.0);
        assert!((2400.0 / wide - MAX_VIEW_SPAN_SECS).abs() < 1.0);
    }

    #[wasm_bindgen_test]
    fn exit_lookback_to_now_clears_window_and_repins() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_lookback(Some(940.0), LoopBasis::default());
        ps.time_model.set_bounds_preserving(940.0, 1000.0); // tick's window
        ps.exit_lookback_to_now(1010.0);
        assert_eq!(ps.time_model.playback_bounds, None);
        assert!(ps.time_model.is_pinned());
        assert_eq!(ps.playback_position(), 1010.0);
        assert!(ps.playing); // caller owns `playing`
    }

    #[wasm_bindgen_test]
    fn exit_live_keep_preserves_position_and_clears_lookback_window() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_lookback(Some(940.0), LoopBasis::default());
        ps.time_model.set_bounds_preserving(940.0, 1000.0);
        ps.exit_live(FreezeAt::Keep);
        assert_eq!(ps.time_model.mode, PlayheadMode::Free);
        assert_eq!(ps.time_model.playback_bounds, None);
        assert_eq!(ps.playback_position(), 940.0); // untouched
    }

    #[wasm_bindgen_test]
    fn exit_live_now_freezes_on_the_live_edge() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.pin_tick(1005.0);
        ps.exit_live(FreezeAt::Now(1006.0));
        assert_eq!(ps.time_model.mode, PlayheadMode::Free);
        assert_eq!(ps.playback_position(), 1006.0);
    }

    #[wasm_bindgen_test]
    fn exit_live_is_idempotent_and_keeps_user_selection_bounds() {
        let mut ps = PlaybackState::default();
        // A user selection's bounds (set in Free mode) must survive an
        // exit_live no-op.
        ps.time_model.set_bounds_from_selection(10.0, 20.0);
        ps.exit_live(FreezeAt::Keep);
        assert_eq!(ps.time_model.playback_bounds, Some((10.0, 20.0)));
        assert_eq!(ps.time_model.mode, PlayheadMode::Free);
    }

    #[wasm_bindgen_test]
    fn pinned_advance_is_inert_and_pin_tick_moves_position() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.playing = true;
        ps.advance(10.0); // micro advance must not move a pinned playhead
        assert_eq!(ps.playback_position(), 1000.0);
        ps.pin_tick(1000.5);
        assert_eq!(ps.playback_position(), 1000.5);
    }

    #[wasm_bindgen_test]
    fn advance_macro_steps_and_loops_over_frames() {
        // Lookback frame-steps via the macro stepper, looping over the
        // sweep_frames list length (not time bounds).
        let mut ps = PlaybackState::default();
        ps.macro_playback.sweep_frames = vec![100.0, 200.0, 300.0];
        ps.macro_playback.current_frame_index = 0;
        ps.time_model.loop_mode = LoopMode::Loop;
        ps.playing = true;
        ps.speed = PlaybackSpeed::Normal; // 5 fps macro

        // Step one frame: 1/5 s of real time advances exactly one frame.
        ps.advance_macro(1.0 / 5.0);
        assert_eq!(ps.macro_playback.current_frame_index, 1);
        assert_eq!(ps.time_model.playback_position, 200.0);

        // Two more frames overshoots the end and wraps back to index 0.
        ps.advance_macro(2.0 / 5.0);
        assert_eq!(ps.macro_playback.current_frame_index, 0);
        assert_eq!(ps.time_model.playback_position, 100.0);
    }

    #[wasm_bindgen_test]
    fn rebuild_cause_distinguishes_elevation_from_window_changes() {
        let mut mp = MacroPlaybackState::default();
        let base = MacroFrameInputs {
            elevation: crate::state::ElevationSelection::default(),
            product: "reflectivity".to_string(),
            bounds: None,
            scan_count: 3,
        };
        // Fresh state vs non-default inputs: scan count differs → window change.
        assert_eq!(mp.rebuild_cause(&base), Some(RebuildCause::WindowChanged));
        mp.store_rebuilt(base.clone(), vec![1.0, 2.0]);
        assert_eq!(mp.sweep_frames, vec![1.0, 2.0]);
        // Same inputs → clean.
        assert_eq!(mp.rebuild_cause(&base), None);

        // Bounds change alone → window change (no cursor snap).
        let bounds_changed = MacroFrameInputs {
            bounds: Some((10.0, 20.0)),
            ..base.clone()
        };
        assert_eq!(
            mp.rebuild_cause(&bounds_changed),
            Some(RebuildCause::WindowChanged)
        );

        // Product change alone → window change (re-filter, no cursor snap).
        let product_changed = MacroFrameInputs {
            product: "velocity".to_string(),
            ..base.clone()
        };
        assert_eq!(
            mp.rebuild_cause(&product_changed),
            Some(RebuildCause::WindowChanged)
        );

        // Scan count change alone → window change.
        let scans_changed = MacroFrameInputs {
            scan_count: 4,
            ..base.clone()
        };
        assert_eq!(
            mp.rebuild_cause(&scans_changed),
            Some(RebuildCause::WindowChanged)
        );

        // Elevation change → ElevationChanged, even when the window moved too
        // (elevation wins because it carries the snap-to-frame side effect).
        let elev_changed = MacroFrameInputs {
            elevation: crate::state::ElevationSelection::Fixed {
                elevation_number: 2,
                angle: 1.45,
            },
            scan_count: 9,
            ..base.clone()
        };
        assert_eq!(
            mp.rebuild_cause(&elev_changed),
            Some(RebuildCause::ElevationChanged)
        );
    }

    #[wasm_bindgen_test]
    fn selection_drag_lifecycle_and_contains() {
        let mut ps = PlaybackState::default();
        ps.begin_selection_drag(100.0);
        assert!(ps.selection_in_progress());
        // Sub-second drag: no meaningful range yet.
        ps.update_selection_drag(100.5);
        assert!(!ps.end_selection_drag());
        // A real drag yields a range; endpoints normalize either direction.
        ps.begin_selection_drag(500.0);
        ps.update_selection_drag(200.0);
        assert!(ps.end_selection_drag());
        assert_eq!(ps.selection_range(), Some((200.0, 500.0)));
        assert!(ps.selection_contains(300.0));
        assert!(!ps.selection_contains(600.0));
    }

    #[wasm_bindgen_test]
    fn anchored_selection_slides_and_clears_separately_from_static() {
        let mut ps = PlaybackState::default();
        ps.set_selection(100.0, 400.0);
        ps.apply_selection_as_bounds();
        // Not anchored: slide is a no-op.
        ps.slide_anchored_selection(500.0);
        assert_eq!(ps.selection_range(), Some((100.0, 400.0)));
        // clear_anchored_selection leaves a static selection alone.
        ps.clear_anchored_selection();
        assert!(ps.selection.is_some());

        // Anchored: the later edge follows, bounds slide without resetting
        // direction, and clear_anchored_selection removes it.
        ps.anchor_selection_to_live();
        ps.time_model.direction = PlaybackDirection::Backward;
        ps.slide_anchored_selection(550.0);
        assert_eq!(ps.selection_range(), Some((100.0, 550.0)));
        assert_eq!(ps.time_model.playback_bounds, Some((100.0, 550.0)));
        assert!(ps.time_model.direction == PlaybackDirection::Backward);
        ps.clear_anchored_selection();
        assert!(ps.selection.is_none());
        assert_eq!(ps.time_model.playback_bounds, None);
    }

    #[wasm_bindgen_test]
    fn exit_live_from_lookback_drops_anchored_selection() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_lookback(Some(940.0), LoopBasis::default());
        ps.selection = Some(TimeSelection {
            a: 940.0,
            b: 1000.0,
            in_progress: false,
            anchored_to_live: true,
        });
        ps.time_model.set_bounds_preserving(940.0, 1000.0);
        ps.exit_live(FreezeAt::Keep);
        assert!(ps.selection.is_none());
        assert_eq!(ps.time_model.playback_bounds, None);
    }

    #[wasm_bindgen_test]
    fn set_bounds_preserving_keeps_direction_and_position() {
        let mut tm = TimeModel::at_position(130.0);
        tm.set_bounds_from_selection(100.0, 160.0);
        tm.direction = PlaybackDirection::Backward;
        tm.playback_position = 130.0;
        // Sliding window update should not reset direction or clamp position.
        tm.set_bounds_preserving(120.0, 180.0);
        assert_eq!(tm.playback_bounds, Some((120.0, 180.0)));
        assert!(tm.direction == PlaybackDirection::Backward);
        assert_eq!(tm.playback_position, 130.0);
    }

    // ---------------------------------------------------------------
    // Timeline tier state machine
    // ---------------------------------------------------------------

    /// A 1000px-wide strip whose tier sits in Micro, as a clean starting point.
    fn micro_state() -> PlaybackState {
        let mut ps = PlaybackState::default();
        ps.set_timeline_zoom(2.0, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Micro);
        ps
    }

    #[wasm_bindgen_test]
    fn tier_micro_macro_hysteresis_no_flicker_in_band() {
        // Inside the dead band (0.87 .. 1.15 px/s) the tier must hold whatever
        // it was: crossing the nominal 1.0 repeatedly never flips it.
        let mut ps = micro_state();
        for z in [1.10, 0.95, 1.05, 0.90, 1.14, 0.88] {
            ps.set_timeline_zoom(z, 1000.0, FALLBACK_FRAME_SPACING_SECS);
            assert_eq!(ps.timeline_tier, TimelineTier::Micro, "zoom {z} held Micro");
        }
        // Drop below the exit floor → Macro; then bouncing in-band stays Macro.
        ps.set_timeline_zoom(0.80, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        for z in [0.90, 1.05, 1.14, 0.88] {
            ps.set_timeline_zoom(z, 1000.0, FALLBACK_FRAME_SPACING_SECS);
            assert_eq!(ps.timeline_tier, TimelineTier::Macro, "zoom {z} held Macro");
        }
    }

    #[wasm_bindgen_test]
    fn tier_micro_enter_exit_thresholds() {
        let mut ps = PlaybackState::default();
        // Start in Macro (default zoom 0.15).
        ps.set_timeline_zoom(0.15, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        // Just below the enter threshold: still Macro.
        ps.set_timeline_zoom(1.14, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        // At the enter threshold: Micro.
        ps.set_timeline_zoom(1.15, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Micro);
        // Just above the exit floor: still Micro.
        ps.set_timeline_zoom(0.88, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Micro);
        // At the exit floor: Macro.
        ps.set_timeline_zoom(0.87, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
    }

    #[wasm_bindgen_test]
    fn tier_archive_span_boundaries() {
        // Width 1000px: span (s) = width / zoom. Archive enter > 60h, exit < 48h.
        let mut ps = PlaybackState::default();
        // Start in Macro.
        ps.set_timeline_zoom(0.15, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);

        // Span just under 60h → still Macro. zoom = 1000 / (59*3600).
        let zoom_59h = 1000.0 / (59.0 * 3600.0);
        ps.set_timeline_zoom(zoom_59h, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);

        // Span just over 60h → Archive. zoom = 1000 / (61*3600).
        let zoom_61h = 1000.0 / (61.0 * 3600.0);
        ps.set_timeline_zoom(zoom_61h, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Archive);

        // Span between 48h and 60h → hysteresis holds Archive (50h).
        let zoom_50h = 1000.0 / (50.0 * 3600.0);
        ps.set_timeline_zoom(zoom_50h, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Archive);

        // Span under 48h → exits Archive (back to Macro by zoom). 47h.
        let zoom_47h = 1000.0 / (47.0 * 3600.0);
        ps.set_timeline_zoom(zoom_47h, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);

        // Archive is a navigator only — no playback there, allowed elsewhere.
        ps.set_timeline_zoom(zoom_61h, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert!(!ps.is_playback_allowed());
        ps.set_timeline_zoom(2.0, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert!(ps.is_playback_allowed());
    }

    #[wasm_bindgen_test]
    fn tier_seeding_is_deterministic_from_zoom_and_width() {
        // Seeding uses the nominal boundaries (no hysteresis memory): zoom >=
        // 1.0 → Micro; span > 54h → Archive; otherwise Macro.
        assert_eq!(PlaybackState::seed_tier(2.0, 1000.0), TimelineTier::Micro);
        assert_eq!(PlaybackState::seed_tier(1.0, 1000.0), TimelineTier::Micro);
        assert_eq!(PlaybackState::seed_tier(0.5, 1000.0), TimelineTier::Macro);
        // 54h nominal Archive boundary at width 1000.
        let zoom_55h = 1000.0 / (55.0 * 3600.0);
        assert_eq!(
            PlaybackState::seed_tier(zoom_55h, 1000.0),
            TimelineTier::Archive
        );
        let zoom_53h = 1000.0 / (53.0 * 3600.0);
        assert_eq!(
            PlaybackState::seed_tier(zoom_53h, 1000.0),
            TimelineTier::Macro
        );
        // Width matters: span = width / zoom, so at a fixed zoom a *wider*
        // strip shows a *larger* span. A zoom that's Macro (50h) at 1000px
        // tips into Archive once the strip is wide enough (50h × 1.2 = 60h).
        let z = 1000.0 / (50.0 * 3600.0);
        assert_eq!(PlaybackState::seed_tier(z, 1000.0), TimelineTier::Macro);
        assert_eq!(PlaybackState::seed_tier(z, 1200.0), TimelineTier::Archive);
    }

    #[wasm_bindgen_test]
    fn reconcile_tier_reacts_to_width_change() {
        // A zoom that's Macro at 1000px becomes Archive when the strip narrows
        // enough that the visible span grows past the enter threshold.
        let mut ps = PlaybackState::default();
        // zoom giving exactly 50h span at 1000px (Macro).
        let zoom = 1000.0 / (50.0 * 3600.0);
        ps.set_timeline_zoom(zoom, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        // Narrow to 1100px? span shrinks — stays Macro. Widen the *time* by
        // narrowing pixels: at 1300px width the span is 1300/zoom > 60h.
        ps.timeline_width_px = 1300.0;
        ps.reconcile_tier(1300.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Archive);
    }

    #[wasm_bindgen_test]
    fn zoom_would_exit_micro_is_hysteresis_aware() {
        let ps = micro_state();
        // A drop that stays above the exit floor does NOT exit Micro.
        assert!(!ps.zoom_would_exit_micro(0.90, 1000.0));
        // A drop to/below the exit floor exits Micro.
        assert!(ps.zoom_would_exit_micro(0.87, 1000.0));
        // From Macro it never reports a Micro exit.
        let mut macro_ps = PlaybackState::default();
        macro_ps.set_timeline_zoom(0.5, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert!(!macro_ps.zoom_would_exit_micro(0.2, 1000.0));
    }

    // ---------------------------------------------------------------
    // Cadence preservation across the snap (spec §9)
    // ---------------------------------------------------------------

    #[wasm_bindgen_test]
    fn cadence_micro_to_macro_picks_nearest_fps() {
        let mut ps = micro_state();
        // Frame list with 300s spacing (median = 300).
        ps.macro_playback.sweep_frames = vec![0.0, 300.0, 600.0, 900.0];
        // Micro speed Normal = 300x. Effective fps = 300 / 300 = 1.0 → Quarter (1 fps).
        ps.speed = PlaybackSpeed::Normal;
        ps.set_timeline_zoom(0.5, 1000.0, ps.median_frame_spacing());
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        assert_eq!(ps.speed, PlaybackSpeed::Quarter);

        // Back to Micro from a 5 fps macro speed: 5 * 300 = 1500x → nearest
        // micro multiple is Quadruple (1200x), closer on log scale than 600x.
        ps.speed = PlaybackSpeed::Normal; // 5 fps in macro
        ps.set_timeline_zoom(2.0, 1000.0, ps.median_frame_spacing());
        assert_eq!(ps.timeline_tier, TimelineTier::Micro);
        assert_eq!(ps.speed, PlaybackSpeed::Quadruple);
    }

    #[wasm_bindgen_test]
    fn cadence_macro_to_micro_with_fast_macro_speed() {
        let mut ps = PlaybackState::default();
        ps.set_timeline_zoom(0.5, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        // 60s frame spacing.
        ps.macro_playback.sweep_frames = vec![0.0, 60.0, 120.0, 180.0];
        // Quadruple macro = 15 fps. 15 * 60 = 900x. On a log scale 900 sits
        // closer to 1200x (Quadruple) than 600x (Double).
        ps.speed = PlaybackSpeed::Quadruple;
        ps.set_timeline_zoom(2.0, 1000.0, ps.median_frame_spacing());
        assert_eq!(ps.timeline_tier, TimelineTier::Micro);
        assert_eq!(ps.speed, PlaybackSpeed::Quadruple);
    }

    #[wasm_bindgen_test]
    fn cadence_conversion_runs_while_paused() {
        // Not playing: the transition still converts the speed (the tier
        // machine owns it, not the playing-gated advance dispatch).
        let mut ps = micro_state();
        assert!(!ps.playing);
        ps.speed = PlaybackSpeed::Realtime; // 1x micro
        ps.macro_playback.sweep_frames = vec![0.0, 300.0, 600.0];
        // 1 / 300 ≈ 0.0033 fps → clamps to slowest macro speed Quarter.
        ps.set_timeline_zoom(0.5, 1000.0, ps.median_frame_spacing());
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        assert_eq!(ps.speed, PlaybackSpeed::Quarter);
    }

    #[wasm_bindgen_test]
    fn median_frame_spacing_falls_back_when_too_few_frames() {
        let mut ps = PlaybackState::default();
        ps.macro_playback.sweep_frames = vec![];
        assert_eq!(ps.median_frame_spacing(), FALLBACK_FRAME_SPACING_SECS);
        ps.macro_playback.sweep_frames = vec![100.0];
        assert_eq!(ps.median_frame_spacing(), FALLBACK_FRAME_SPACING_SECS);
        // Uneven spacing → true median of the deltas (100, 200, 300 → 200).
        ps.macro_playback.sweep_frames = vec![0.0, 100.0, 300.0, 600.0];
        assert_eq!(ps.median_frame_spacing(), 200.0);
    }

    #[wasm_bindgen_test]
    fn detail_level_maps_from_tier() {
        let mut ps = PlaybackState::default();
        // Micro → Tilts (frames-first cells).
        ps.set_timeline_zoom(2.0, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.detail_level(), DetailLevel::Tilts);
        // Macro → Volumes regardless of the in-tier zoom; the Macro renderer
        // decides tick-vs-coverage by density now (the MACRO_VOLUMES_ZOOM
        // sub-detail stopgap was removed).
        ps.set_timeline_zoom(0.5, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.detail_level(), DetailLevel::Volumes);
        ps.set_timeline_zoom(0.1, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.detail_level(), DetailLevel::Volumes);
        // Archive → Coverage.
        let zoom_61h = 1000.0 / (61.0 * 3600.0);
        ps.set_timeline_zoom(zoom_61h, 1000.0, FALLBACK_FRAME_SPACING_SECS);
        assert_eq!(ps.timeline_tier, TimelineTier::Archive);
        assert_eq!(ps.detail_level(), DetailLevel::Coverage);
    }

    // ---------------------------------------------------------------
    // Loop-window state (spec §8)
    // ---------------------------------------------------------------

    #[wasm_bindgen_test]
    fn loop_basis_default_is_six_frames() {
        // Alignment #7: the default preset is the last 6 frames.
        assert_eq!(
            LoopBasis::default(),
            LoopBasis::FrameCount(DEFAULT_LOOP_FRAMES)
        );
        assert_eq!(DEFAULT_LOOP_FRAMES, 6);
        assert_eq!(LoopWindow::default().basis, LoopBasis::FrameCount(6));
        assert!(!LoopWindow::default().pinned);
    }

    #[wasm_bindgen_test]
    fn loop_preset_basis_and_menu() {
        assert_eq!(
            LoopPreset::PinToLive.basis(),
            LoopBasis::FrameCount(DEFAULT_LOOP_FRAMES)
        );
        assert_eq!(
            LoopPreset::LastFrames(10).basis(),
            LoopBasis::FrameCount(10)
        );
        assert_eq!(
            LoopPreset::LastDuration(1800.0).basis(),
            LoopBasis::Duration(1800.0)
        );
        // The menu offers the alignment #7 set.
        let labels: Vec<String> = LoopPreset::menu().iter().map(|p| p.label()).collect();
        assert!(labels.contains(&"Pin to live".to_string()));
        assert!(labels.contains(&"Last 4 frames".to_string()));
        assert!(labels.contains(&"Last 6 frames".to_string()));
        assert!(labels.contains(&"Last 10 frames".to_string()));
        assert!(labels.contains(&"Last 30 min".to_string()));
        assert!(labels.contains(&"Last 1 h".to_string()));
    }

    #[wasm_bindgen_test]
    fn loop_basis_fallback_span_frame_count_vs_duration() {
        // Frame-count: (n + 1) volumes of slack at the typical interval.
        assert_eq!(
            LoopBasis::FrameCount(6).fallback_span_secs(),
            7.0 * FALLBACK_FRAME_SPACING_SECS
        );
        // Duration: exactly the span.
        assert_eq!(LoopBasis::Duration(1800.0).fallback_span_secs(), 1800.0);
    }

    #[wasm_bindgen_test]
    fn apply_selection_records_fixed_loop_window() {
        let mut ps = PlaybackState::default();
        ps.set_selection(100.0, 460.0);
        ps.apply_selection_as_bounds();
        // A plain selection → a fixed (un-pinned) duration-basis loop window.
        let w = ps.loop_window.expect("loop window set");
        assert_eq!(w.basis, LoopBasis::Duration(360.0));
        assert!(!w.pinned);
        // Anchoring it mirrors into the window as pinned.
        ps.anchor_selection_to_live();
        assert!(ps.loop_window.unwrap().pinned);
        // Un-anchoring (handle dragged off live) reverts to fixed.
        ps.unanchor_selection_from_live();
        assert!(!ps.loop_window.unwrap().pinned);
    }

    #[wasm_bindgen_test]
    fn clearing_a_loop_drops_the_window() {
        let mut ps = PlaybackState::default();
        ps.set_selection(100.0, 400.0);
        ps.apply_selection_as_bounds();
        assert!(ps.loop_window.is_some());
        ps.clear_selection();
        assert!(ps.loop_window.is_none());
        assert_eq!(ps.time_model.playback_bounds, None);
    }

    #[wasm_bindgen_test]
    fn exiting_live_clears_pinned_loop_window() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_lookback(Some(940.0), LoopBasis::FrameCount(4));
        assert!(ps.loop_window.is_some());
        ps.exit_lookback_to_now(1010.0);
        assert!(ps.loop_window.is_none());
        // And the harder exit path.
        ps.enter_lookback(Some(940.0), LoopBasis::FrameCount(4));
        assert!(ps.loop_window.is_some());
        ps.exit_live(FreezeAt::Keep);
        assert!(ps.loop_window.is_none());
    }

    // ---------------------------------------------------------------
    // Wrap-point incorporation (spec §8)
    // ---------------------------------------------------------------

    #[wasm_bindgen_test]
    fn commit_pinned_window_tracks_now_continuously_while_paused() {
        // Not playing: the window follows now every call, nothing is parked.
        let mut ps = PlaybackState::default();
        assert!(!ps.playing);
        let w = ps.commit_pinned_window(100.0, 200.0);
        assert_eq!(w, (100.0, 200.0));
        ps.time_model.set_bounds_preserving(100.0, 200.0);
        // A later target is taken immediately (continuous tracking).
        let w = ps.commit_pinned_window(150.0, 260.0);
        assert_eq!(w, (150.0, 260.0));
        assert!(ps.pending_loop_window.is_none());
    }

    #[wasm_bindgen_test]
    fn commit_pinned_window_defers_new_frames_until_wrap_while_playing() {
        let mut ps = PlaybackState::default();
        ps.playing = true;
        // First commit while playing takes effect immediately (no prior bounds).
        let w = ps.commit_pinned_window(100.0, 400.0);
        assert_eq!(w, (100.0, 400.0));
        ps.time_model.set_bounds_preserving(w.0, w.1);

        // A frame arrives mid-cycle (window would slide to 160..460): it is
        // parked, NOT applied — the committed window stays fixed.
        let held = ps.commit_pinned_window(160.0, 460.0);
        assert_eq!(held, (100.0, 400.0), "committed window held between wraps");
        assert_eq!(ps.pending_loop_window, Some((160.0, 460.0)));

        // Set up a frame list spanning the committed window and step to the wrap.
        ps.macro_playback.sweep_frames = vec![100.0, 250.0, 400.0];
        ps.macro_playback.current_frame_index = 2; // last frame
        ps.time_model.loop_mode = LoopMode::Loop;
        // Stepping forward past the end wraps to index 0 and flushes the parked
        // window into bounds.
        ps.step_macro_frame(1);
        assert_eq!(ps.macro_playback.current_frame_index, 0);
        assert_eq!(
            ps.time_model.playback_bounds,
            Some((160.0, 460.0)),
            "parked window committed at the wrap"
        );
        assert!(ps.pending_loop_window.is_none());
    }

    #[wasm_bindgen_test]
    fn commit_pinned_window_no_park_when_target_unchanged() {
        let mut ps = PlaybackState::default();
        ps.playing = true;
        ps.time_model.set_bounds_preserving(100.0, 400.0);
        // Same target as committed → nothing parked.
        let held = ps.commit_pinned_window(100.0, 400.0);
        assert_eq!(held, (100.0, 400.0));
        assert!(ps.pending_loop_window.is_none());
    }
}
