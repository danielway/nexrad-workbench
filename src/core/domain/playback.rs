//! Playback controls state per PRODUCT.md specification.
//!
//! Implements a dual-time model separating playback position from wall-clock time,
//! with timeline bounds enforcement and zoom-based feature restrictions.

/// Nominal zoom boundary between macro (scan blocks) and micro (individual
/// sweeps) tiers, in pixels per second. The tier state machine adds
/// hysteresis around this value (see [`tier`]); this constant survives only
/// as the seam used for deterministic tier seeding (boot / URL restore) and
/// as the return-to-live floor's reference point.
pub(crate) const MICRO_ZOOM_THRESHOLD: f64 = 1.0;

/// Minimum / maximum timeline zoom (pixels per second). Every zoom mutation
/// routes through [`PlaybackState::set_timeline_zoom`], which clamps here.
///
/// The minimum is the *hard floor* that bounds even a very wide strip; the
/// effective per-frame floor is tighter and width-aware (see
/// [`PlaybackState::min_zoom_for_width`] / [`MAX_VIEW_SPAN_SECS`]). It exists
/// only as a backstop against a degenerate `width_px`, and must therefore sit
/// *below* the width-aware floor on even the narrowest plausible strip —
/// otherwise it, not [`MAX_VIEW_SPAN_SECS`], silently decides how far the
/// timeline can zoom out.
pub(crate) const TIMELINE_ZOOM_MIN: f64 = 0.0000001;
pub(crate) const TIMELINE_ZOOM_MAX: f64 = 1000.0;

/// Widest span (seconds) the strip may show: three days, the ceiling the
/// width-aware min-zoom enforces. Further out is the overview's job: it
/// zooms out to the whole archive era (see [`crate::core::overview`]), so the
/// strip only ever draws scans and sweeps (round-4 design).
pub(crate) const MAX_VIEW_SPAN_SECS: f64 = 3.0 * 86_400.0;

/// Tunable tier thresholds. The timeline's zoom level maps to one of two
/// visual tiers; transitions carry hysteresis (distinct enter/exit
/// thresholds) so a zoom hovering at a boundary never flickers. Values are
/// the alignment-pass decisions (see docs/PRODUCT.md, Alignment §2);
/// tune here, in one place.
pub(crate) mod tier {
    /// Enter Micro (from Macro) when zoom rises to/above this (px/sec).
    pub(crate) const MICRO_ENTER_ZOOM: f64 = 1.15;
    /// Exit Micro (to Macro) when zoom falls to/below this (px/sec).
    pub(crate) const MICRO_EXIT_ZOOM: f64 = 0.87;
}

/// Bucket sizes the overview aggregates coverage at when it is zoomed out
/// past single scans, coarsest last.
///
/// Reaching the whole NEXRAD era on one lane means the *bars* get coarser as
/// the span grows, not that the lane gets denser — otherwise the widest view
/// is a shimmer of sub-pixel ticks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum BucketGranularity {
    /// UTC days.
    #[default]
    Day,
    /// UTC weeks starting Monday.
    Week,
    /// Real calendar months (28-31 days).
    Month,
    /// Calendar quarters (Jan/Apr/Jul/Oct). The terminal rung: at the widest
    /// zoom this keeps cells ~8px instead of the ~3px a month rung would give.
    Quarter,
}

/// Nominal seconds per bucket at each granularity, for threshold math only —
/// the real boundaries are calendar-aligned and variable width.
pub(crate) mod bucket_span {
    pub(crate) const DAY_SECS: f64 = 86_400.0;
    pub(crate) const WEEK_SECS: f64 = 7.0 * DAY_SECS;
    /// Mean Gregorian month.
    pub(crate) const MONTH_SECS: f64 = 2_629_746.0;
    pub(crate) const QUARTER_SECS: f64 = 3.0 * MONTH_SECS;
}

impl BucketGranularity {
    /// Nominal span of one bucket, for pixel-width estimates.
    pub(crate) fn nominal_secs(self) -> f64 {
        match self {
            BucketGranularity::Day => bucket_span::DAY_SECS,
            BucketGranularity::Week => bucket_span::WEEK_SECS,
            BucketGranularity::Month => bucket_span::MONTH_SECS,
            BucketGranularity::Quarter => bucket_span::QUARTER_SECS,
        }
    }
}

/// The single stored timeline tier: the strip's detail level. It only changes
/// what the strip draws; the playback mode is the separate [`PlaybackMode`]
/// choice. Transitions are owned by [`PlaybackState::set_timeline_zoom`] / the
/// per-frame reconcile, which apply hysteresis; nothing else writes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum TimelineTier {
    /// Zoomed in (minutes–hours visible): frame cells / sweep detail on the
    /// strip.
    #[default]
    Micro,
    /// Zoomed out (hours–days visible): scan blocks.
    Macro,
}

/// How playback advances: the user's explicit choice, stored on
/// [`PlaybackState::play_mode`] and independent of the timeline zoom.
/// Shown to the user as **Frames** (Macro) and **Rays** (Micro).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) enum PlaybackMode {
    /// Frames: step between complete matching sweeps at a chosen fps.
    #[default]
    Macro,
    /// Rays: continuous time at a realtime multiple, showing data as the
    /// radar collected it.
    Micro,
}

impl PlaybackMode {
    /// User-facing name of the mode.
    pub(crate) fn label(self) -> &'static str {
        match self {
            PlaybackMode::Macro => "Frames",
            PlaybackMode::Micro => "Rays",
        }
    }
}

/// Fallback median frame spacing (seconds) for cadence conversion when the
/// macro frame list is too short to derive one — a typical NEXRAD volume
/// interval.
pub(crate) const FALLBACK_FRAME_SPACING_SECS: f64 = 300.0;

/// Inputs the macro `sweep_frames` list is built from. Compared against the
/// previous build's inputs to decide whether a rebuild is needed.
#[derive(PartialEq, Clone, Default)]
pub(crate) struct MacroFrameInputs {
    pub elevation: crate::core::ElevationSelection,
    /// Selected product (worker-string). A frame is a sweep matching the
    /// product AND tilt, so the list must rebuild when the product changes —
    /// otherwise a stale list survives a product switch. Borrowed `&'static str`
    /// (from `RadarProduct::to_worker_string`) so the per-frame dirty check
    /// doesn't heap-allocate during macro/lookback playback.
    pub product: &'static str,
    pub bounds: Option<(f64, f64)>,
    pub scan_count: usize,
    /// Whether the volumetric (3D) render path is active. In 3D a frame is a
    /// whole scan (the volume render is keyed per scan), so flipping this
    /// must rebuild the list — without this term a stale per-sweep list
    /// survives the 3D toggle.
    pub volume_3d: bool,
}

/// How finely the fallback view-derived macro bounds are quantized: the visible
/// span divided by this, floored at [`MIN_VIEW_BOUNDS_QUANTUM_SECS`]. Without
/// quantization every pixel of pan changes the bounds, and the bounds
/// participate in the frame-list dirty check — so the whole list (an O(scans)
/// walk) would rebuild on every frame of a pan.
const VIEW_BOUNDS_QUANTUM_DIVISOR: f64 = 16.0;

/// Floor for the view-bounds quantum, so a deeply zoomed-in view still doesn't
/// rebuild more often than once per minute of timeline travel.
const MIN_VIEW_BOUNDS_QUANTUM_SECS: f64 = 60.0;

/// Time bounds the macro frame list is built from.
///
/// An explicit loop/selection always wins. Otherwise the list falls back to the
/// **visible window** rather than the whole cache: with no bounds at all every
/// cached sweep in the session is a frame, so pressing play with the playhead
/// parked somewhere specific marched off to whatever else happened to be in the
/// cache — the "playback jumps to crazy points in the cache/history other than
/// what is visible" report.
///
/// Two adjustments make the fallback safe:
/// - the window is quantized *outward* (see [`VIEW_BOUNDS_QUANTUM_DIVISOR`]),
///   so panning doesn't rebuild the list every frame and the result always
///   contains the whole visible span;
/// - `playhead` is always included, so limiting the frames can never strand the
///   cursor outside its own frame list and cause the very teleport this fixes.
pub(crate) fn macro_frame_bounds(
    explicit: Option<(f64, f64)>,
    view_start: f64,
    view_span: f64,
    playhead: f64,
) -> Option<(f64, f64)> {
    if explicit.is_some() {
        return explicit;
    }
    if !view_span.is_finite() || view_span <= 0.0 || !view_start.is_finite() {
        return None;
    }
    let quantum = (view_span / VIEW_BOUNDS_QUANTUM_DIVISOR).max(MIN_VIEW_BOUNDS_QUANTUM_SECS);
    let mut lo = (view_start / quantum).floor() * quantum;
    let mut hi = ((view_start + view_span) / quantum).ceil() * quantum;
    if playhead.is_finite() {
        lo = lo.min((playhead / quantum).floor() * quantum);
        hi = hi.max((playhead / quantum).ceil() * quantum);
    }
    Some((lo, hi))
}

/// Why the macro frame list needs rebuilding. The elevation case is
/// distinguished because it additionally snaps the playback position to the
/// resolved frame (see `render_loop`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RebuildCause {
    ElevationChanged,
    /// Bounds or scan count changed (streaming, selection edits) — rebuild
    /// without teleporting the cursor.
    WindowChanged,
}

/// State for macro (frame-stepping) playback.
pub(crate) struct MacroPlaybackState {
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
    pub(crate) fn rebuild_cause(&self, inputs: &MacroFrameInputs) -> Option<RebuildCause> {
        if self.built_from.elevation != inputs.elevation {
            Some(RebuildCause::ElevationChanged)
        } else if self.built_from.product != inputs.product
            || self.built_from.bounds != inputs.bounds
            || self.built_from.scan_count != inputs.scan_count
            || self.built_from.volume_3d != inputs.volume_3d
        {
            // A product switch re-filters the frame list (frame = product +
            // tilt) but keeps the cursor where it is — no snap side-effect, so
            // it's a window-class change. A 3D toggle swaps the frame
            // granularity (sweep ↔ scan) the same way: rebuild, re-land the
            // cursor on the nearest frame, no teleport.
            Some(RebuildCause::WindowChanged)
        } else {
            None
        }
    }

    /// Install a freshly built frame list and remember the inputs it was
    /// built from.
    pub(crate) fn store_rebuilt(&mut self, inputs: MacroFrameInputs, frames: Vec<f64>) {
        self.sweep_frames = frames;
        self.built_from = inputs;
    }
}

/// Playback speed multiplier options.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum PlaybackSpeed {
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
    pub(crate) fn label(&self) -> &'static str {
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

    pub(crate) fn all() -> &'static [PlaybackSpeed] {
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
    pub(crate) fn macro_frames_per_second(&self) -> Option<f64> {
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
    pub(crate) fn macro_label(&self) -> &'static str {
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
    pub(crate) fn macro_speeds() -> &'static [PlaybackSpeed] {
        &[
            PlaybackSpeed::Quarter,
            PlaybackSpeed::Half,
            PlaybackSpeed::Normal,
            PlaybackSpeed::Double,
            PlaybackSpeed::Quadruple,
        ]
    }

    /// Returns how many seconds of timeline time pass per real second.
    pub(crate) fn timeline_seconds_per_real_second(&self) -> f64 {
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
    /// preservation switching to Frames. Defaults to the slowest macro speed when
    /// the target is non-positive.
    pub(crate) fn nearest_macro_fps(target_fps: f64) -> PlaybackSpeed {
        Self::nearest_by_log(target_fps, Self::macro_speeds(), |s| {
            s.macro_frames_per_second()
        })
    }

    /// The variant whose timeline multiple is nearest `target_multiple`
    /// (log-scale nearness). Used by cadence preservation switching to Rays;
    /// considers all variants since every one is a valid micro multiple.
    pub(crate) fn nearest_micro_multiple(target_multiple: f64) -> PlaybackSpeed {
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
pub(crate) fn format_lag(secs: f64) -> String {
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
pub(crate) const DEFAULT_LOOP_FRAMES: u32 = 6;

/// How a loop window's extent is measured. Frame-count windows ("last 6
/// frames") are preferred in Micro since scan spacing varies; duration windows
/// ("last 30 min") are the alternative offered in presets (spec §8).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum LoopBasis {
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
    pub(crate) fn label(&self) -> String {
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

    /// Conservative fallback span (seconds) a range to now covers while too
    /// few frames are known (no archive listing yet and too little cached), so
    /// `tick_live` can seed bounds
    /// that bound the macro frame list to recent data rather than all history.
    /// Frame-count bases assume the typical volume interval per frame (plus a
    /// volume of slack); duration bases use their own span.
    pub(crate) fn fallback_span_secs(&self) -> f64 {
        match self {
            LoopBasis::FrameCount(n) => (*n as f64 + 1.0) * FALLBACK_FRAME_SPACING_SECS,
            LoopBasis::Duration(secs) => *secs,
        }
    }
}

/// The user's selection on the timeline (spec §8): a **point** or a
/// **range**, each either at a fixed time or attached to the live edge. One
/// value replaces the old playhead mode + playback bounds + loop window +
/// live-anchored selection, which had to be kept in step by hand.
///
/// | Selection | Was |
/// |---|---|
/// | `Point { live: false }` | archive, free seeking |
/// | `Point { live: true }` | LIVE-NOW (pinned to now) |
/// | `Range { live: false }` | a fixed loop / a range drawn on the strip |
/// | `Range { live: true }` | the lookback replay, or a selection anchored to live |
///
/// The playhead position lives in [`TimeModel::playback_position`]; for a
/// live point the per-frame live tick keeps it on now, and for a live range
/// the tick slides the window (sized by its [`LoopBasis`]) forward as new
/// sweeps arrive. Change it only through the named transitions on
/// [`PlaybackState`], which own the invariants.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Selection {
    /// A single moment: the playhead.
    Point {
        /// Pinned to the live edge.
        live: bool,
    },
    /// A span the playhead plays through, looping.
    Range(RangeSelection),
}

impl Default for Selection {
    fn default() -> Self {
        Selection::Point { live: false }
    }
}

/// The range half of [`Selection`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct RangeSelection {
    /// Ordered playback bounds. `None` only for a live range before the live
    /// tick has resolved its first window.
    pub bounds: Option<(f64, f64)>,
    /// The later edge follows the live edge (a "last 6 frames" loop).
    pub live: bool,
    /// How the window is sized when it slides: last N frames or a duration.
    /// For a range drawn by hand this is its span.
    pub basis: LoopBasis,
}

impl Selection {
    /// Attached to the live edge (a live point or a live range).
    pub(crate) fn is_live(&self) -> bool {
        match self {
            Selection::Point { live } => *live,
            Selection::Range(r) => r.live,
        }
    }

    /// The range, if this is one.
    pub(crate) fn range(&self) -> Option<&RangeSelection> {
        match self {
            Selection::Range(r) => Some(r),
            Selection::Point { .. } => None,
        }
    }
}

/// A range ending now that the user can pick from the selection chip's menu
/// (spec §8 "creation order: presets first"). The app turns each into a
/// range attached to the live edge (alignment #7: 4/6/10 frames,
/// 30 min / 1 h).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum LoopPreset {
    /// A loop of the last `n` matching frames.
    LastFrames(u32),
    /// A duration-basis loop covering the last `secs` seconds.
    LastDuration(f64),
}

impl LoopPreset {
    /// The presets offered in the menu, in display order.
    pub(crate) fn menu() -> &'static [LoopPreset] {
        &[
            LoopPreset::LastFrames(4),
            LoopPreset::LastFrames(6),
            LoopPreset::LastFrames(10),
            LoopPreset::LastDuration(30.0 * 60.0),
            LoopPreset::LastDuration(60.0 * 60.0),
        ]
    }

    /// Menu label.
    pub(crate) fn label(&self) -> String {
        match self {
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
    pub(crate) fn basis(&self) -> LoopBasis {
        match self {
            LoopPreset::LastFrames(n) => LoopBasis::FrameCount(*n),
            LoopPreset::LastDuration(secs) => LoopBasis::Duration(*secs),
        }
    }
}

/// Loop behavior when playback bounds are set (the loop style, chosen from
/// the selection chip's menu). A standing preference: it survives across
/// ranges, fixed or pinned to now.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum LoopMode {
    /// Play forward, jump to start when reaching end
    #[default]
    Loop,
    /// Play forward then backward (ping-pong)
    PingPong,
    /// Stop at end
    Once,
}

impl LoopMode {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            LoopMode::Loop => "Loop",
            LoopMode::PingPong => "Ping-pong",
            LoopMode::Once => "Once",
        }
    }

    pub(crate) fn all() -> &'static [LoopMode] {
        &[LoopMode::Loop, LoopMode::PingPong, LoopMode::Once]
    }
}

/// Playback direction for ping-pong mode.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackDirection {
    #[default]
    Forward,
    Backward,
}

/// Where the playhead lands when leaving live ([`PlaybackState::exit_live`]).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum FreezeAt {
    /// Snap to the given wall-clock now (predictable stop on the live edge).
    Now(f64),
    /// Keep the current position (seek/jog paths set it themselves).
    Keep,
}

/// A range selection being drawn by a drag on the timeline. It becomes a
/// [`Selection::Range`] when the drag ends.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct TimeSelection {
    /// First-placed endpoint (drag origin). Unordered relative to `b`.
    pub a: f64,
    /// Second endpoint — the one a drag moves.
    pub b: f64,
    /// True while a strip drag is still moving `b`.
    pub in_progress: bool,
}

impl TimeSelection {
    /// Normalized `(start, end)`, or `None` until the selection has
    /// meaningful width (> 1 second).
    pub(crate) fn range(&self) -> Option<(f64, f64)> {
        let (start, end) = (self.a.min(self.b), self.a.max(self.b));
        ((end - start).abs() > 1.0).then_some((start, end))
    }
}

/// Time model per PRODUCT.md specification.
///
/// Separates playback position (the moment in radar time being displayed)
/// from wall-clock time (current real-world time).
#[derive(Clone)]
pub(crate) struct TimeModel {
    /// Playback position - the moment in radar time being displayed.
    /// This is independent of wall-clock time during archive playback.
    /// Unix seconds with sub-second precision.
    pub playback_position: f64,

    /// Loop behavior when bounds are set.
    pub loop_mode: LoopMode,

    /// Current playback direction (for ping-pong mode).
    pub direction: PlaybackDirection,
}

impl TimeModel {
    /// Advance playback position by delta time within `bounds` (if any),
    /// respecting the loop mode. Returns true at the loop's wrap point: a
    /// Loop jumping back to the start, a Ping-pong turning at either end, or
    /// a Once reaching its end — the moments a range pinned to now may take
    /// in the frames that arrived meanwhile.
    pub(crate) fn advance(
        &mut self,
        delta_secs: f64,
        speed: PlaybackSpeed,
        bounds: Option<(f64, f64)>,
    ) -> bool {
        let advance_amount = delta_secs * speed.timeline_seconds_per_real_second();

        let effective_advance = match self.direction {
            PlaybackDirection::Forward => advance_amount,
            PlaybackDirection::Backward => -advance_amount,
        };

        let new_position = self.playback_position + effective_advance;

        match bounds {
            Some((start, end)) => {
                let wrapped = match self.loop_mode {
                    LoopMode::Loop | LoopMode::Once => new_position >= end,
                    LoopMode::PingPong => new_position >= end || new_position <= start,
                };
                self.playback_position = self.apply_bounds(new_position, start, end);
                wrapped
            }
            None => {
                self.playback_position = new_position;
                false
            }
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
}

/// State for playback controls.
pub(crate) struct PlaybackState {
    /// Whether playback is currently active
    pub playing: bool,

    /// Time model (playback position, loop mode, direction)
    pub time_model: TimeModel,

    /// What is selected: a point or range, fixed or live (see
    /// [`Selection`]). Owns the playhead mode and the playback bounds.
    pub selection: Selection,

    /// Current playback speed
    pub speed: PlaybackSpeed,

    /// The Rays speed the user had chosen when going live locked `speed` to
    /// real time (one-instrument §14). Given back on leaving live, and it is
    /// what the preferences save, so the lock never overwrites the saved
    /// speed. `None` outside the lock.
    pub held_speed: Option<PlaybackSpeed>,

    /// The user's chosen playback mode (Frames or Rays). Never derived from
    /// zoom; changed only through [`Self::set_play_mode`], which keeps
    /// `speed` valid for the mode.
    pub play_mode: PlaybackMode,

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

    /// Whether the view may be nudged to keep "now" on screen while streaming
    /// (`WorkbenchApp::keep_now_on_screen`). A deliberate user pan clears it —
    /// otherwise the nudge fights the pan every frame and panning away from the
    /// live edge is impossible. Re-armed by [`Self::center_view_on`], which
    /// every "take me back to the data" path already routes through.
    pub view_follows_now: bool,

    /// The overview lane's window above the strip (see
    /// [`crate::core::overview`]).
    pub overview: crate::core::overview::Overview,

    /// A range being drawn by a drag, until it is released.
    pub drag: Option<TimeSelection>,

    /// Wrap-point incorporation buffer (spec §8): while *playing* a pinned
    /// loop, the freshly resolved target window is parked here instead of being
    /// applied immediately. It is committed to `playback_bounds` only when the
    /// playhead crosses the loop's wrap point, so the visible band and frame set
    /// stay fixed between wraps and newly arrived frames "enter at the wrap,
    /// never mid-cycle". Cleared (and bypassed) while not playing — a
    /// paused/idle pinned window may track now continuously. See
    /// [`Self::commit_pinned_window`] and the wrap branches of
    /// [`Self::step_macro_frame_internal`] and [`Self::advance`].
    pub pending_loop_window: Option<(f64, f64)>,

    /// Actual pixel width of the timeline widget (set by render_timeline each frame).
    /// Used for accurate view centering calculations outside the render function.
    pub timeline_width_px: f64,

    /// State for macro (frame-stepping) playback mode.
    pub macro_playback: MacroPlaybackState,
}

impl PlaybackState {
    /// Get the current playback position (convenience accessor).
    pub(crate) fn playback_position(&self) -> f64 {
        self.time_model.playback_position
    }

    /// Seek: write the playback position. Position writes while live must
    /// go through a transition off the live edge first (`exit_live`) — every
    /// UI seek path does, and this assert keeps it that way.
    pub(crate) fn set_playback_position(&mut self, position: f64) {
        debug_assert!(
            !self.selection.is_live(),
            "seek while {:?} — exit live first",
            self.selection
        );
        self.time_model.playback_position = position;
    }

    /// Pinned to the live edge (LIVE-NOW).
    pub(crate) fn is_pinned(&self) -> bool {
        self.selection == Selection::Point { live: true }
    }

    /// Playing a range attached to the live edge (the "last N frames" loop).
    pub(crate) fn is_live_range(&self) -> bool {
        matches!(self.selection, Selection::Range(r) if r.live)
    }

    /// The playback bounds the playhead is confined to, if a range is selected.
    pub(crate) fn playback_bounds(&self) -> Option<(f64, f64)> {
        self.selection.range().and_then(|r| r.bounds)
    }

    /// Select a fixed or live range between two timestamps and confine the
    /// playhead to it. A live range keeps its span as its sliding basis.
    /// Ignored (returns false) when the span is under a second.
    pub(crate) fn select_range(&mut self, a: f64, b: f64, live: bool) -> bool {
        let (start, end) = order_pair(a, b);
        if end - start <= 1.0 {
            return false;
        }
        self.drag = None;
        self.pending_loop_window = None;
        self.selection = Selection::Range(RangeSelection {
            bounds: Some((start, end)),
            live,
            basis: LoopBasis::Duration(end - start),
        });
        self.time_model.direction = PlaybackDirection::Forward;
        self.time_model.playback_position = self.time_model.playback_position.clamp(start, end);
        true
    }

    /// Place the playhead inside the selected range (clamped to its bounds).
    /// Unlike [`Self::set_playback_position`] this is allowed on a live
    /// range: the playhead loops within it rather than sitting on now.
    /// A no-op without range bounds.
    pub(crate) fn seek_within_range(&mut self, ts: f64) {
        if let Some((start, end)) = self.playback_bounds() {
            self.time_model.playback_position = ts.clamp(start, end);
        }
    }

    /// Move a range's bounds without resetting the loop's direction or
    /// clamping the position (the live window sliding, or a wrap flush).
    fn set_range_bounds_preserving(&mut self, start: f64, end: f64) {
        if let Selection::Range(r) = &mut self.selection {
            r.bounds = Some(order_pair(start, end));
        }
    }

    // ------------------------------------------------------------------
    // Playhead mode transitions — the ONLY mutation points for
    // `TimeModel::mode`. Each owns its invariants so call sites can't
    // half-apply a mode switch.
    // ------------------------------------------------------------------

    /// → live point (LIVE-NOW). Pins the playhead to `now`; the per-frame
    /// live tick keeps it there via [`Self::pin_tick`]. Drops any range (live
    /// owns its own constraints). In Rays, Realtime lock is the only
    /// meaningful speed while tethered to the live edge (spec §8.2) — force
    /// it so a prior large × multiple cannot remain selected in the transport
    /// readout. In Frames the fps is kept (see [`Self::live_speed`]).
    pub(crate) fn enter_pinned_live(&mut self, now: f64) {
        self.selection = Selection::Point { live: true };
        self.drag = None;
        self.pending_loop_window = None;
        self.time_model.direction = PlaybackDirection::Forward;
        self.time_model.playback_position = now;
        let live = self.live_speed();
        if self.play_mode == PlaybackMode::Micro && self.speed != live && self.held_speed.is_none()
        {
            self.held_speed = Some(self.speed);
        }
        self.speed = live;
    }

    /// The speed the user chose: the held one while Rays is locked to real
    /// time by live, else the current one. What the preferences save.
    pub(crate) fn chosen_speed(&self) -> PlaybackSpeed {
        self.held_speed.unwrap_or(self.speed)
    }

    /// Leave the live edge for the archive. A live point becomes a fixed
    /// point; a live range is dropped (it only means something attached to
    /// now). A fixed range is untouched. `freeze` picks the landing position.
    /// Idempotent when already off live.
    pub(crate) fn exit_live(&mut self, freeze: FreezeAt) {
        if let Some(held) = self.held_speed.take() {
            if self.play_mode == PlaybackMode::Micro {
                self.speed = held;
            }
        }
        if self.selection.is_live() {
            self.selection = Selection::Point { live: false };
            self.pending_loop_window = None;
            self.time_model.direction = PlaybackDirection::Forward;
        }
        if let FreezeAt::Now(now) = freeze {
            self.time_model.playback_position = now;
        }
    }

    /// Live point → live range: loop the recent frames, sliding with now.
    /// The window is resolved each frame by the live tick from `basis` (see
    /// [`Self::commit_pinned_window`]); `render_loop` turns it into the frame
    /// list in Frames, and Rays plays it continuously. `seed` places the
    /// playhead (oldest cached frame) so the first pass runs oldest→newest.
    pub(crate) fn enter_live_range(&mut self, seed: Option<f64>, basis: LoopBasis) {
        debug_assert!(
            self.is_pinned(),
            "a live range starts from the live point, not {:?}",
            self.selection
        );
        self.selection = Selection::Range(RangeSelection {
            bounds: None,
            live: true,
            basis,
        });
        // The loop style is the user's standing choice; only the direction
        // restarts. Like every new range, it starts paused: Play is the Play
        // button.
        self.time_model.direction = PlaybackDirection::Forward;
        if let Some(ts) = seed {
            self.time_model.playback_position = ts;
        }
        self.playing = false;
        self.macro_playback.current_frame_index = 0;
        self.macro_playback.frame_accumulator = 0.0;
    }

    /// Per-frame position pin while on the live point (called from the live
    /// tick, independent of the `playing` flag).
    pub(crate) fn pin_tick(&mut self, now: f64) {
        debug_assert!(
            self.is_pinned(),
            "pin_tick off the live point ({:?})",
            self.selection
        );
        self.time_model.playback_position = now;
    }

    /// Visible time width in seconds, using the real timeline widget width.
    pub(crate) fn view_width_secs(&self) -> f64 {
        if self.timeline_zoom > 0.0 {
            self.timeline_width_px / self.timeline_zoom
        } else {
            0.0
        }
    }

    /// Center the timeline view on a given timestamp.
    ///
    /// Also re-arms [`Self::view_follows_now`]: every deliberate "put me back
    /// where the data is" path (live attach, `CenterTimelineOnNow`, a fresh
    /// timeline load) routes through here, so this is the one seam where
    /// view-following is legitimately restored after a user pan turned it off.
    pub(crate) fn center_view_on(&mut self, ts: f64) {
        self.timeline_view_start = ts - self.view_width_secs() / 2.0;
        self.view_follows_now = true;
    }

    /// Show `[start, end]` on the strip: zoom to its width and pan to it (the
    /// overview's view box). If the zoom clamps, the edge named by
    /// `keep_end` stays put. The view follows now again when its right edge
    /// reaches now.
    pub(crate) fn show_view(&mut self, start: f64, end: f64, keep_end: bool, now: f64) {
        let wanted = end - start;
        if wanted.is_nan() || wanted <= 0.0 || self.timeline_width_px <= 0.0 {
            return;
        }
        let width = self.timeline_width_px;
        self.set_timeline_zoom(width / wanted, width);
        let span = self.view_width_secs();
        self.timeline_view_start = if keep_end { end - span } else { start };
        self.view_follows_now = self.timeline_view_start + span >= now;
    }

    // ------------------------------------------------------------------
    // Timeline tier state machine. `timeline_tier` is the single source of
    // truth for the renderer's detail level. It never changes playback: the
    // Frames/Rays mode is the user's separate choice (`play_mode`). The raw `timeline_zoom` scalar feeds it but
    // is never read directly for tier decisions — that would lose the
    // hysteresis memory the field carries.
    // ------------------------------------------------------------------

    /// Deterministically classify a zoom into a tier with NO hysteresis
    /// memory, using the nominal boundary. For boot / URL restore, where
    /// there is no prior tier to bias the decision.
    pub(crate) fn seed_tier(zoom: f64) -> TimelineTier {
        if zoom >= MICRO_ZOOM_THRESHOLD {
            TimelineTier::Micro
        } else {
            TimelineTier::Macro
        }
    }

    /// Reseed the tier from the current zoom without hysteresis. Used at
    /// boot / URL restore once `timeline_zoom` is set.
    pub(crate) fn seed_tier_from_state(&mut self) {
        self.timeline_tier = Self::seed_tier(self.timeline_zoom);
    }

    /// Compute the tier a zoom should land in, given the current tier (so
    /// enter/exit thresholds differ — hysteresis). Pure: does not mutate.
    fn next_tier(&self, zoom: f64) -> TimelineTier {
        match self.timeline_tier {
            // Leave Micro only once zoom drops to the (lower) exit floor.
            TimelineTier::Micro if zoom <= tier::MICRO_EXIT_ZOOM => TimelineTier::Macro,
            TimelineTier::Macro if zoom >= tier::MICRO_ENTER_ZOOM => TimelineTier::Micro,
            current => current,
        }
    }

    /// The width-aware minimum zoom (px/sec): how far out the strip may go,
    /// so the visible span never exceeds [`MAX_VIEW_SPAN_SECS`] at
    /// `width_px`. [`TIMELINE_ZOOM_MIN`] sits below it as a degenerate-width
    /// backstop.
    pub(crate) fn min_zoom_for_width(width_px: f64) -> f64 {
        let w = width_px.max(1.0);
        (w / MAX_VIEW_SPAN_SECS).max(TIMELINE_ZOOM_MIN)
    }

    /// The single zoom-mutation path. Clamps, stores the zoom, and advances
    /// the tier with hysteresis (running the cadence-preservation side effect
    /// on a behavioral flip). `width_px` is the current strip width; pass
    /// `timeline_width_px` when unknown. `median_frame_spacing` feeds cadence
    /// conversion (see [`Self::median_frame_spacing`]).
    ///
    /// The minimum is width-aware ([`Self::min_zoom_for_width`]): the strip
    /// shows at most three days.
    pub(crate) fn set_timeline_zoom(&mut self, zoom: f64, width_px: f64) {
        let min = Self::min_zoom_for_width(width_px);
        self.timeline_zoom = zoom.clamp(min, TIMELINE_ZOOM_MAX);
        self.timeline_tier = self.next_tier(self.timeline_zoom);
    }

    /// Per-frame reconcile: the strip width can change (responsive layout)
    /// even when zoom is untouched, which moves the zoom floor. Re-clamp the
    /// zoom and re-evaluate the tier with hysteresis. Cheap and idempotent
    /// when nothing moved.
    pub(crate) fn reconcile_tier(&mut self, width_px: f64) {
        // `min_zoom_for_width` is width-aware, so a zoom restored from a URL
        // was only ever clamped against the *seeded* width.
        let min = Self::min_zoom_for_width(width_px);
        self.timeline_zoom = self.timeline_zoom.clamp(min, TIMELINE_ZOOM_MAX);
        self.timeline_tier = self.next_tier(self.timeline_zoom);
    }

    /// The user's chosen playback mode (Frames or Rays).
    pub(crate) fn playback_mode(&self) -> PlaybackMode {
        self.play_mode
    }

    /// Switch between Frames and Rays. Converts the speed to the nearest one
    /// with the same perceived cadence in the new mode (spec §9) and resets the
    /// sub-frame accumulator. While pinned to live there is no cadence to
    /// carry (Rays is locked to 1×, spec §8.2), so each mode gets its live
    /// default instead. No-op when the mode is unchanged.
    pub(crate) fn set_play_mode(&mut self, mode: PlaybackMode) {
        let prev = self.play_mode;
        if prev == mode {
            return;
        }
        self.play_mode = mode;
        self.held_speed = None;
        if self.is_pinned() {
            self.speed = match mode {
                PlaybackMode::Micro => PlaybackSpeed::Realtime,
                PlaybackMode::Macro => PlaybackSpeed::default(),
            };
        } else {
            let spacing = self.median_frame_spacing();
            self.preserve_cadence_across_snap(prev, mode, spacing);
        }
        self.macro_playback.frame_accumulator = 0.0;
    }

    /// Restore a saved mode at boot, alongside an already-restored `speed`.
    /// Unlike [`Self::set_play_mode`] there is no cadence to carry over; the
    /// speed is only replaced when it can't be expressed in the mode (a
    /// Rays-only multiple in Frames).
    pub(crate) fn restore_play_mode(&mut self, mode: PlaybackMode) {
        self.play_mode = mode;
        if mode == PlaybackMode::Macro && self.speed.macro_frames_per_second().is_none() {
            self.speed = PlaybackSpeed::default();
        }
    }

    /// The speed the transport shows while pinned to live: 1× in Rays (the
    /// live edge moves at real time), or the current fps in Frames, falling
    /// back to the default when the current speed has no fps.
    fn live_speed(&self) -> PlaybackSpeed {
        match self.play_mode {
            PlaybackMode::Micro => PlaybackSpeed::Realtime,
            PlaybackMode::Macro if self.speed.macro_frames_per_second().is_some() => self.speed,
            PlaybackMode::Macro => PlaybackSpeed::default(),
        }
    }

    /// Median frame spacing (seconds) of the current macro frame list, for
    /// cadence conversion. Derived from consecutive `sweep_frames` deltas when
    /// ≥2 frames exist; otherwise the typical volume interval. Tiny lists are
    /// fine — the median of one delta is that delta.
    pub(crate) fn median_frame_spacing(&self) -> f64 {
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

    /// Preserve perceived playback rhythm across a Frames↔Rays switch (spec §9).
    /// Converts the current effective frame cadence to the nearest valid speed
    /// in the new mode. Runs regardless of `playing`. The shared [`PlaybackSpeed`] enum is kept intact —
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

    /// Advance playback by delta time (Rays / continuous). A live point's
    /// position is owned by the live tick, so nothing advances there. A
    /// forward wrap flushes a parked live-range window, the same as a frame
    /// wrap in Frames.
    pub(crate) fn advance(&mut self, delta_secs: f64) {
        if !self.playing || self.is_pinned() {
            return;
        }
        let bounds = self.playback_bounds();
        if self.time_model.advance(delta_secs, self.speed, bounds) {
            self.apply_pending_window_at_wrap();
        }
    }

    /// Advance playback in macro mode: step through frames at constant fps.
    pub(crate) fn advance_macro(&mut self, delta_secs: f64) {
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
    /// Snaps playback_position to the frame's timestamp. User steps go through
    /// `core::transport::reduce_step`; this drives the wrap tests.
    #[cfg(test)]
    pub(crate) fn step_macro_frame(&mut self, delta: isize) {
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
                    // A turnaround is this style's wrap point.
                    self.apply_pending_window_at_wrap();
                    self.time_model.direction = PlaybackDirection::Backward;
                    self.macro_playback.current_frame_index = len.saturating_sub(1);
                }
                LoopMode::Once if self.is_live_range() => {
                    // Once with a pinned end plays to now and holds there:
                    // take in what arrived, stay on the newest frame, and keep
                    // playing so the next frame to arrive plays too.
                    self.apply_pending_window_at_wrap();
                    self.macro_playback.current_frame_index = len - 1;
                    self.snap_playback_to_macro_frame();
                    return false;
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
                    self.apply_pending_window_at_wrap();
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
    pub(crate) fn snap_playback_to_macro_frame(&mut self) {
        if let Some(&ts) = self
            .macro_playback
            .sweep_frames
            .get(self.macro_playback.current_frame_index)
        {
            self.time_model.playback_position = ts;
        }
    }

    /// Sync the macro frame index to the nearest frame matching the current playback position.
    pub(crate) fn sync_macro_frame_index(&mut self) {
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

    /// The selected range (start <= end): a drag in progress, else the
    /// range selection's bounds.
    pub(crate) fn selection_range(&self) -> Option<(f64, f64)> {
        match self.drag {
            Some(d) if d.in_progress => d.range(),
            _ => self.playback_bounds(),
        }
    }

    /// Whether a range is being drawn on the selection strip.
    pub(crate) fn selection_in_progress(&self) -> bool {
        self.drag.is_some_and(|d| d.in_progress)
    }

    /// Whether a timestamp falls inside the current selection.
    #[cfg(test)]
    pub(crate) fn selection_contains(&self, ts: f64) -> bool {
        self.selection_range()
            .is_some_and(|(s, e)| ts >= s && ts <= e)
    }

    /// Start drawing a range anchored at `ts` (both endpoints collapsed). A
    /// handle drag anchors the opposite edge.
    pub(crate) fn begin_selection_drag(&mut self, ts: f64) {
        self.drag = Some(TimeSelection {
            a: ts,
            b: ts,
            in_progress: true,
        });
    }

    /// Move the dragged endpoint while a range is being drawn.
    pub(crate) fn update_selection_drag(&mut self, ts: f64) {
        if let Some(d) = self.drag.as_mut() {
            if d.in_progress {
                d.b = ts;
            }
        }
    }

    /// Finish drawing a range, returning the drawn range when it has meaningful
    /// width. The caller decides whether it becomes a fixed or live range
    /// ([`Self::select_range`]).
    pub(crate) fn end_selection_drag(&mut self) -> Option<(f64, f64)> {
        let drawn = self.drag.take().filter(|d| d.in_progress)?;
        drawn.range()
    }

    /// Per-frame slide for a live range: the live tick resolves the target
    /// window and this commits it (see [`Self::commit_pinned_window`]).
    pub(crate) fn slide_live_range(&mut self, target_start: f64, target_end: f64) {
        if !self.is_live_range() {
            return;
        }
        let (s, e) = self.commit_pinned_window(target_start, target_end);
        self.set_range_bounds_preserving(s, e);
        // Keep the playhead inside the window as it slides past it (a paused
        // range would otherwise leave the playhead behind).
        self.time_model.playback_position = self.time_model.playback_position.clamp(s, e);
    }

    /// Clear the range selection. A live range falls back to the live point
    /// (the loop ends, the playhead stays on now); a fixed range to a fixed
    /// point at the current position.
    pub(crate) fn clear_selection(&mut self) {
        self.drag = None;
        self.pending_loop_window = None;
        self.time_model.direction = PlaybackDirection::Forward;
        if let Selection::Range(r) = self.selection {
            self.selection = Selection::Point { live: r.live };
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
    pub(crate) fn commit_pinned_window(
        &mut self,
        target_start: f64,
        target_end: f64,
    ) -> (f64, f64) {
        let target = order_pair(target_start, target_end);
        if !self.playing {
            self.pending_loop_window = None;
            return target;
        }
        match self.playback_bounds() {
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
            self.set_range_bounds_preserving(s, e);
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
    fn enter_live_range_sets_loop_seed_and_resets_macro_cursor() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        // Pretend a prior macro session left a non-zero cursor.
        ps.macro_playback.current_frame_index = 7;
        ps.macro_playback.frame_accumulator = 0.4;
        ps.time_model.loop_mode = LoopMode::PingPong;
        ps.enter_live_range(Some(940.0), LoopBasis::FrameCount(10));
        // The loop style is kept, and the range starts paused.
        assert!(ps.time_model.loop_mode == LoopMode::PingPong);
        assert!(ps.is_live_range());
        assert!(!ps.playing);
        assert_eq!(ps.playback_position(), 940.0);
        assert_eq!(ps.macro_playback.current_frame_index, 0);
        assert_eq!(ps.macro_playback.frame_accumulator, 0.0);
        // The basis is recorded on the live range for tick_live to read.
        assert_eq!(
            ps.selection,
            Selection::Range(RangeSelection {
                bounds: None,
                live: true,
                basis: LoopBasis::FrameCount(10),
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
    fn live_range_keeps_the_chosen_mode() {
        // Rays is allowed on a live range: it plays the window continuously.
        let mut ps = PlaybackState::default();
        ps.set_play_mode(PlaybackMode::Micro);
        ps.enter_pinned_live(1000.0);
        ps.enter_live_range(None, LoopBasis::default());
        assert_eq!(ps.playback_mode(), PlaybackMode::Micro);
    }

    #[wasm_bindgen_test]
    fn the_strip_shows_at_most_three_days() {
        // The width-aware floor bounds the visible span to MAX_VIEW_SPAN_SECS;
        // further out is the overview's job.
        let width = 1200.0;
        let min = PlaybackState::min_zoom_for_width(width);
        assert!((width / min - 3.0 * 86_400.0).abs() < 1.0);

        // A zoom request below the floor clamps UP to it.
        let mut ps = PlaybackState::default();
        ps.set_timeline_zoom(1e-12, width);
        assert_eq!(ps.timeline_zoom, min);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
    }

    #[wasm_bindgen_test]
    fn the_hard_floor_never_becomes_the_binding_constraint() {
        // TIMELINE_ZOOM_MIN is a degenerate-width backstop, not the zoom-out
        // ceiling: on any plausible strip the width-aware floor binds.
        for width in [320.0_f64, 768.0, 1200.0, 3840.0] {
            let width_aware = width / MAX_VIEW_SPAN_SECS;
            assert!(width_aware > TIMELINE_ZOOM_MIN);
            assert_eq!(PlaybackState::min_zoom_for_width(width), width_aware);
        }
    }

    #[wasm_bindgen_test]
    fn reconcile_tier_reclamps_zoom_to_the_current_width_floor() {
        // A URL-restored zoom is clamped against the seeded width; the real
        // strip can be wider. Without a re-clamp the view silently exceeds the
        // ceiling.
        let mut ps = PlaybackState::default();
        ps.timeline_zoom = 1e-12;
        ps.timeline_width_px = 1200.0;
        ps.reconcile_tier(1200.0);
        assert_eq!(ps.timeline_zoom, PlaybackState::min_zoom_for_width(1200.0));
    }

    #[wasm_bindgen_test]
    fn min_zoom_is_width_aware() {
        // span = width / zoom, so holding the same span ceiling, a WIDER strip
        // needs a LARGER floor (more pixels per second).
        let narrow = PlaybackState::min_zoom_for_width(1200.0);
        let wide = PlaybackState::min_zoom_for_width(2400.0);
        assert!(wide > narrow);
        // Both hold the span ceiling.
        assert!((1200.0 / narrow - MAX_VIEW_SPAN_SECS).abs() < 1.0);
        assert!((2400.0 / wide - MAX_VIEW_SPAN_SECS).abs() < 1.0);
    }

    #[wasm_bindgen_test]
    fn enter_pinned_live_from_live_range_clears_window_and_repins() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_live_range(Some(940.0), LoopBasis::default());
        ps.playing = true;
        ps.slide_live_range(940.0, 1000.0); // tick's window
        ps.enter_pinned_live(1010.0);
        assert_eq!(ps.playback_bounds(), None);
        assert!(ps.is_pinned());
        assert_eq!(ps.playback_position(), 1010.0);
        assert!(ps.playing); // caller owns `playing`
    }

    #[wasm_bindgen_test]
    fn exit_live_keep_preserves_position_and_clears_lookback_window() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_live_range(Some(940.0), LoopBasis::default());
        ps.slide_live_range(940.0, 1000.0);
        ps.exit_live(FreezeAt::Keep);
        assert_eq!(ps.selection, Selection::Point { live: false });
        assert_eq!(ps.playback_bounds(), None);
        assert_eq!(ps.playback_position(), 940.0); // untouched
    }

    #[wasm_bindgen_test]
    fn exit_live_now_freezes_on_the_live_edge() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.pin_tick(1005.0);
        ps.exit_live(FreezeAt::Now(1006.0));
        assert_eq!(ps.selection, Selection::Point { live: false });
        assert_eq!(ps.playback_position(), 1006.0);
    }

    #[wasm_bindgen_test]
    fn exit_live_is_idempotent_and_keeps_user_selection_bounds() {
        let mut ps = PlaybackState::default();
        // A fixed range must survive an exit_live no-op.
        assert!(ps.select_range(10.0, 20.0, false));
        ps.exit_live(FreezeAt::Keep);
        assert_eq!(ps.playback_bounds(), Some((10.0, 20.0)));
        assert!(!ps.selection.is_live());
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
            elevation: crate::core::ElevationSelection::default(),
            product: "reflectivity",
            bounds: None,
            scan_count: 3,
            volume_3d: false,
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
            product: "velocity",
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

        // 3D toggle alone → window change (frame granularity swaps sweep↔scan;
        // rebuild without teleporting the cursor).
        let volume_3d_changed = MacroFrameInputs {
            volume_3d: true,
            ..base.clone()
        };
        assert_eq!(
            mp.rebuild_cause(&volume_3d_changed),
            Some(RebuildCause::WindowChanged)
        );

        // Elevation change → ElevationChanged, even when the window moved too
        // (elevation wins because it carries the snap-to-frame side effect).
        let elev_changed = MacroFrameInputs {
            elevation: crate::core::ElevationSelection::Fixed {
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
        assert_eq!(ps.end_selection_drag(), None);
        assert!(!ps.selection_in_progress());
        // A real drag yields a range; endpoints normalize either direction.
        ps.begin_selection_drag(500.0);
        ps.update_selection_drag(200.0);
        // While drawing, the selection shown is the drag's.
        assert_eq!(ps.selection_range(), Some((200.0, 500.0)));
        assert!(ps.selection_contains(300.0));
        assert!(!ps.selection_contains(600.0));
        // Ending the drag hands the range back; the caller selects it.
        assert_eq!(ps.end_selection_drag(), Some((200.0, 500.0)));
        assert_eq!(ps.selection_range(), None);
        assert!(ps.select_range(200.0, 500.0, false));
        assert_eq!(ps.selection_range(), Some((200.0, 500.0)));
    }

    #[wasm_bindgen_test]
    fn live_range_slides_and_fixed_range_does_not() {
        let mut ps = PlaybackState::default();
        assert!(ps.select_range(100.0, 400.0, false));
        // A fixed range ignores the live slide.
        ps.slide_live_range(200.0, 500.0);
        assert_eq!(ps.selection_range(), Some((100.0, 400.0)));

        // A live range follows now; while paused the window is taken at once
        // and the loop's direction is left alone.
        assert!(ps.select_range(100.0, 400.0, true));
        assert!(ps.is_live_range());
        ps.time_model.direction = PlaybackDirection::Backward;
        ps.time_model.playback_position = 120.0;
        ps.slide_live_range(250.0, 550.0);
        assert_eq!(ps.playback_bounds(), Some((250.0, 550.0)));
        // The playhead is carried along when the window slides past it.
        assert_eq!(ps.playback_position(), 250.0);
        assert!(ps.time_model.direction == PlaybackDirection::Backward);
    }

    #[wasm_bindgen_test]
    fn select_range_records_span_basis_and_clamps_position() {
        let mut ps = PlaybackState::default();
        ps.time_model.playback_position = 50.0;
        ps.time_model.direction = PlaybackDirection::Backward;
        assert!(ps.select_range(460.0, 100.0, false));
        assert_eq!(
            ps.selection,
            Selection::Range(RangeSelection {
                bounds: Some((100.0, 460.0)),
                live: false,
                basis: LoopBasis::Duration(360.0),
            })
        );
        assert_eq!(ps.playback_position(), 100.0);
        assert!(ps.time_model.direction == PlaybackDirection::Forward);
        // A sub-second span is refused and leaves the range alone.
        assert!(!ps.select_range(200.0, 200.5, true));
        assert_eq!(ps.playback_bounds(), Some((100.0, 460.0)));
    }

    #[wasm_bindgen_test]
    fn clearing_a_range_returns_to_its_point() {
        // A fixed range falls back to a fixed point at the current position.
        let mut ps = PlaybackState::default();
        assert!(ps.select_range(100.0, 400.0, false));
        ps.clear_selection();
        assert_eq!(ps.selection, Selection::Point { live: false });
        assert_eq!(ps.playback_bounds(), None);
        // A live range falls back to the live point.
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_live_range(Some(940.0), LoopBasis::FrameCount(4));
        ps.clear_selection();
        assert!(ps.is_pinned());
    }

    #[wasm_bindgen_test]
    fn exit_live_from_live_range_drops_the_range() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        assert!(ps.select_range(940.0, 1000.0, true));
        ps.exit_live(FreezeAt::Keep);
        assert_eq!(ps.selection, Selection::Point { live: false });
        assert_eq!(ps.playback_bounds(), None);
    }

    #[wasm_bindgen_test]
    fn seek_within_range_clamps_to_bounds() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        assert!(ps.select_range(900.0, 1000.0, true));
        ps.seek_within_range(950.0);
        assert_eq!(ps.playback_position(), 950.0);
        ps.seek_within_range(10.0);
        assert_eq!(ps.playback_position(), 900.0);
        // Without bounds it is a no-op.
        let mut ps = PlaybackState::default();
        ps.time_model.playback_position = 5.0;
        ps.seek_within_range(50.0);
        assert_eq!(ps.playback_position(), 5.0);
    }

    // ---------------------------------------------------------------
    // Timeline tier state machine
    // ---------------------------------------------------------------

    /// A 1000px-wide strip whose tier sits in Micro, as a clean starting point.
    fn micro_state() -> PlaybackState {
        let mut ps = PlaybackState::default();
        ps.set_timeline_zoom(2.0, 1000.0);
        assert_eq!(ps.timeline_tier, TimelineTier::Micro);
        ps.set_play_mode(PlaybackMode::Micro);
        ps
    }

    #[wasm_bindgen_test]
    fn tier_micro_macro_hysteresis_no_flicker_in_band() {
        // Inside the dead band (0.87 .. 1.15 px/s) the tier must hold whatever
        // it was: crossing the nominal 1.0 repeatedly never flips it.
        let mut ps = micro_state();
        for z in [1.10, 0.95, 1.05, 0.90, 1.14, 0.88] {
            ps.set_timeline_zoom(z, 1000.0);
            assert_eq!(ps.timeline_tier, TimelineTier::Micro, "zoom {z} held Micro");
        }
        // Drop below the exit floor → Macro; then bouncing in-band stays Macro.
        ps.set_timeline_zoom(0.80, 1000.0);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        for z in [0.90, 1.05, 1.14, 0.88] {
            ps.set_timeline_zoom(z, 1000.0);
            assert_eq!(ps.timeline_tier, TimelineTier::Macro, "zoom {z} held Macro");
        }
    }

    #[wasm_bindgen_test]
    fn tier_micro_enter_exit_thresholds() {
        let mut ps = PlaybackState::default();
        // Start in Macro (default zoom 0.15).
        ps.set_timeline_zoom(0.15, 1000.0);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        // Just below the enter threshold: still Macro.
        ps.set_timeline_zoom(1.14, 1000.0);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
        // At the enter threshold: Micro.
        ps.set_timeline_zoom(1.15, 1000.0);
        assert_eq!(ps.timeline_tier, TimelineTier::Micro);
        // Just above the exit floor: still Micro.
        ps.set_timeline_zoom(0.88, 1000.0);
        assert_eq!(ps.timeline_tier, TimelineTier::Micro);
        // At the exit floor: Macro.
        ps.set_timeline_zoom(0.87, 1000.0);
        assert_eq!(ps.timeline_tier, TimelineTier::Macro);
    }

    #[wasm_bindgen_test]
    fn tier_seeding_is_deterministic_from_zoom() {
        // Seeding uses the nominal boundary (no hysteresis memory): zoom >=
        // 1.0 → Micro; otherwise Macro.
        assert_eq!(PlaybackState::seed_tier(2.0), TimelineTier::Micro);
        assert_eq!(PlaybackState::seed_tier(1.0), TimelineTier::Micro);
        assert_eq!(PlaybackState::seed_tier(0.5), TimelineTier::Macro);
    }

    #[wasm_bindgen_test]
    fn zoom_out_keeps_pinned_live() {
        // Zoom is view-only: zooming all the way out (Micro → Macro) and back
        // must leave the playhead pinned to now.
        let mut ps = micro_state();
        ps.enter_pinned_live(5_000.0);
        for z in [0.5, 0.01, PlaybackState::min_zoom_for_width(1000.0), 2.0] {
            ps.set_timeline_zoom(z, 1000.0);
            assert!(ps.is_pinned(), "unpinned at zoom {z}");
        }
    }

    #[wasm_bindgen_test]
    fn zoom_out_keeps_lookback_loop() {
        // A lookback replay survives the same zoom sweep with its window intact.
        let mut ps = micro_state();
        ps.enter_pinned_live(5_000.0);
        ps.enter_live_range(Some(4_000.0), LoopBasis::FrameCount(DEFAULT_LOOP_FRAMES));
        let window = ps.selection;
        for z in [0.5, 0.01, PlaybackState::min_zoom_for_width(1000.0), 2.0] {
            ps.set_timeline_zoom(z, 1000.0);
            assert!(ps.is_live_range(), "left lookback at zoom {z}");
            assert_eq!(ps.selection, window);
        }
    }

    // ---------------------------------------------------------------
    // Cadence preservation across the snap (spec §9)
    // ---------------------------------------------------------------

    #[wasm_bindgen_test]
    fn cadence_micro_to_macro_picks_nearest_fps() {
        let mut ps = PlaybackState::default();
        ps.set_play_mode(PlaybackMode::Micro);
        // Frame list with 300s spacing (median = 300).
        ps.macro_playback.sweep_frames = vec![0.0, 300.0, 600.0, 900.0];
        // Micro speed Normal = 300x. Effective fps = 300 / 300 = 1.0 → Quarter (1 fps).
        ps.speed = PlaybackSpeed::Normal;
        ps.set_play_mode(PlaybackMode::Macro);
        assert_eq!(ps.speed, PlaybackSpeed::Quarter);

        // Back to Micro from a 5 fps macro speed: 5 * 300 = 1500x → nearest
        // micro multiple is Quadruple (1200x), closer on log scale than 600x.
        ps.speed = PlaybackSpeed::Normal; // 5 fps in macro
        ps.set_play_mode(PlaybackMode::Micro);
        assert_eq!(ps.speed, PlaybackSpeed::Quadruple);
    }

    #[wasm_bindgen_test]
    fn cadence_macro_to_micro_with_fast_macro_speed() {
        let mut ps = PlaybackState::default();
        assert_eq!(ps.playback_mode(), PlaybackMode::Macro);
        // 60s frame spacing.
        ps.macro_playback.sweep_frames = vec![0.0, 60.0, 120.0, 180.0];
        // Quadruple macro = 15 fps. 15 * 60 = 900x. On a log scale 900 sits
        // closer to 1200x (Quadruple) than 600x (Double).
        ps.speed = PlaybackSpeed::Quadruple;
        ps.set_play_mode(PlaybackMode::Micro);
        assert_eq!(ps.speed, PlaybackSpeed::Quadruple);
    }

    #[wasm_bindgen_test]
    fn zoom_never_changes_mode_or_speed() {
        // The tier is the strip's detail level only: sweeping the zoom from
        // Micro through Archive and back leaves the mode and speed alone.
        for mode in [PlaybackMode::Macro, PlaybackMode::Micro] {
            let mut ps = PlaybackState::default();
            ps.set_play_mode(mode);
            ps.macro_playback.sweep_frames = vec![0.0, 300.0, 600.0];
            let speed = ps.speed;
            for z in [2.0, 0.5, PlaybackState::min_zoom_for_width(1000.0), 2.0] {
                ps.set_timeline_zoom(z, 1000.0);
                assert_eq!(ps.playback_mode(), mode, "mode changed at zoom {z}");
                assert_eq!(ps.speed, speed, "speed changed at zoom {z}");
            }
        }
    }

    #[wasm_bindgen_test]
    fn restore_play_mode_keeps_a_valid_speed() {
        let mut ps = PlaybackState::default();
        ps.speed = PlaybackSpeed::Realtime; // a Rays-only multiple
        ps.restore_play_mode(PlaybackMode::Micro);
        assert_eq!(ps.speed, PlaybackSpeed::Realtime);
        ps.restore_play_mode(PlaybackMode::Macro);
        assert!(ps.speed.macro_frames_per_second().is_some());
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
    }

    #[wasm_bindgen_test]
    fn loop_preset_basis_and_menu() {
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
    fn exiting_live_drops_the_live_range() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_live_range(Some(940.0), LoopBasis::FrameCount(4));
        ps.enter_pinned_live(1010.0);
        assert!(ps.is_pinned());
        assert!(ps.selection.range().is_none());
        // And the harder exit path.
        ps.enter_live_range(Some(940.0), LoopBasis::FrameCount(4));
        ps.exit_live(FreezeAt::Keep);
        assert_eq!(ps.selection, Selection::Point { live: false });
    }

    // ---------------------------------------------------------------
    // Wrap-point incorporation (spec §8)
    // ---------------------------------------------------------------

    /// A playing live range with no bounds resolved yet (a range starts
    /// paused; Play is pressed).
    fn live_range_state() -> PlaybackState {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_live_range(None, LoopBasis::default());
        ps.playing = true;
        ps
    }

    #[wasm_bindgen_test]
    fn commit_pinned_window_tracks_now_continuously_while_paused() {
        // Not playing: the window follows now every call, nothing is parked.
        let mut ps = live_range_state();
        ps.playing = false;
        let w = ps.commit_pinned_window(100.0, 200.0);
        assert_eq!(w, (100.0, 200.0));
        ps.set_range_bounds_preserving(100.0, 200.0);
        // A later target is taken immediately (continuous tracking).
        let w = ps.commit_pinned_window(150.0, 260.0);
        assert_eq!(w, (150.0, 260.0));
        assert!(ps.pending_loop_window.is_none());
    }

    #[wasm_bindgen_test]
    fn commit_pinned_window_defers_new_frames_until_wrap_while_playing() {
        let mut ps = live_range_state();
        // First commit while playing takes effect immediately (no prior bounds).
        let w = ps.commit_pinned_window(100.0, 400.0);
        assert_eq!(w, (100.0, 400.0));
        ps.set_range_bounds_preserving(w.0, w.1);

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
            ps.playback_bounds(),
            Some((160.0, 460.0)),
            "parked window committed at the wrap"
        );
        assert!(ps.pending_loop_window.is_none());
    }

    /// A playing pinned range with (100, 400) committed and (160, 460)
    /// parked, on its last frame.
    fn parked_range(style: LoopMode) -> PlaybackState {
        let mut ps = live_range_state();
        ps.time_model.loop_mode = style;
        let w = ps.commit_pinned_window(100.0, 400.0);
        ps.set_range_bounds_preserving(w.0, w.1);
        ps.commit_pinned_window(160.0, 460.0);
        ps.macro_playback.sweep_frames = vec![100.0, 250.0, 400.0];
        ps.macro_playback.current_frame_index = 2;
        ps
    }

    #[wasm_bindgen_test]
    fn new_frames_join_a_ping_pong_at_its_turn() {
        let mut ps = parked_range(LoopMode::PingPong);
        ps.step_macro_frame(1);
        assert!(ps.time_model.direction == PlaybackDirection::Backward);
        assert_eq!(ps.playback_bounds(), Some((160.0, 460.0)));
        assert!(ps.pending_loop_window.is_none());
    }

    #[wasm_bindgen_test]
    fn once_with_a_pinned_end_plays_to_now_and_holds() {
        let mut ps = parked_range(LoopMode::Once);
        ps.step_macro_frame(1);
        assert_eq!(ps.playback_bounds(), Some((160.0, 460.0)));
        assert_eq!(ps.macro_playback.current_frame_index, 2);
        assert!(ps.playing, "holds at now, ready for the next frame");

        // A fixed range played Once stops at its end.
        let mut ps = PlaybackState::default();
        assert!(ps.select_range(100.0, 400.0, false));
        ps.time_model.loop_mode = LoopMode::Once;
        ps.playing = true;
        ps.macro_playback.sweep_frames = vec![100.0, 250.0, 400.0];
        ps.macro_playback.current_frame_index = 2;
        ps.step_macro_frame(1);
        assert!(!ps.playing);
    }

    #[wasm_bindgen_test]
    fn rays_flush_a_parked_window_at_every_style_s_wrap() {
        for style in [LoopMode::Loop, LoopMode::PingPong, LoopMode::Once] {
            let mut ps = parked_range(style);
            ps.time_model.playback_position = 399.0;
            ps.speed = PlaybackSpeed::Realtime;
            ps.advance(5.0);
            assert_eq!(
                ps.playback_bounds(),
                Some((160.0, 460.0)),
                "{style:?} takes in new frames at its wrap"
            );
        }
    }

    #[wasm_bindgen_test]
    fn commit_pinned_window_no_park_when_target_unchanged() {
        let mut ps = live_range_state();
        ps.set_range_bounds_preserving(100.0, 400.0);
        // Same target as committed → nothing parked.
        let held = ps.commit_pinned_window(100.0, 400.0);
        assert_eq!(held, (100.0, 400.0));
        assert!(ps.pending_loop_window.is_none());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ---------------------------------------------------------------
    // macro_frame_bounds: the visible-window fallback
    // ---------------------------------------------------------------

    #[wasm_bindgen_test]
    fn explicit_bounds_always_win() {
        // A loop/selection is the user's stated intent; the view never
        // overrides it.
        let out = macro_frame_bounds(Some((10.0, 20.0)), 1_000_000.0, 3600.0, 1_000_000.0);
        assert_eq!(out, Some((10.0, 20.0)));
    }

    #[wasm_bindgen_test]
    fn without_a_selection_the_bounds_follow_the_visible_window() {
        // The audit case: previously `None`, which made every cached sweep in
        // the session a playback frame.
        let view_start = 1_000_000.0;
        let span = 3600.0;
        let (lo, hi) = macro_frame_bounds(None, view_start, span, view_start + span / 2.0).unwrap();
        // Quantized OUTWARD — never narrower than what is on screen.
        assert!(lo <= view_start);
        assert!(hi >= view_start + span);
    }

    #[wasm_bindgen_test]
    fn view_bounds_are_quantized_so_panning_does_not_rebuild_every_frame() {
        // Two view positions a few seconds apart must land on identical
        // bounds, otherwise the frame-list dirty check fires every frame of a pan.
        let span = 3600.0;
        let a = macro_frame_bounds(None, 1_000_000.0, span, 1_000_000.0);
        let b = macro_frame_bounds(None, 1_000_003.0, span, 1_000_000.0);
        assert_eq!(a, b);
        // …but a pan of a full quantum does move them.
        let quantum = (span / VIEW_BOUNDS_QUANTUM_DIVISOR).max(MIN_VIEW_BOUNDS_QUANTUM_SECS);
        let c = macro_frame_bounds(None, 1_000_000.0 + quantum * 2.0, span, 1_000_000.0);
        assert!(a != c);
    }

    #[wasm_bindgen_test]
    fn quantum_has_a_floor_at_deep_zoom() {
        // A 2-minute visible span would otherwise quantize at 7.5s and rebuild
        // constantly while panning.
        let span = 120.0;
        let (lo, hi) = macro_frame_bounds(None, 1_000_000.0, span, 1_000_000.0).unwrap();
        assert!(hi - lo >= MIN_VIEW_BOUNDS_QUANTUM_SECS);
    }

    #[wasm_bindgen_test]
    fn the_playhead_is_always_inside_the_derived_bounds() {
        // Limiting frames to the view must never strand the cursor outside its
        // own frame list — that would cause the exact teleport this fixes.
        let span = 3600.0;
        let view_start = 1_000_000.0;
        for playhead in [
            view_start - 50_000.0,
            view_start + 1800.0,
            view_start + 90_000.0,
        ] {
            let (lo, hi) = macro_frame_bounds(None, view_start, span, playhead).unwrap();
            assert!(lo <= playhead);
            assert!(hi >= playhead);
            // The visible window still stays covered as well.
            assert!(lo <= view_start);
            assert!(hi >= view_start + span);
        }
    }

    #[wasm_bindgen_test]
    fn degenerate_view_falls_back_to_unbounded() {
        // Before the first layout pass there is no view to derive from; the
        // old whole-cache behavior is the safe answer rather than an empty
        // frame list.
        assert_eq!(
            macro_frame_bounds(None, 1_000_000.0, 0.0, 1_000_000.0),
            None
        );
        assert_eq!(
            macro_frame_bounds(None, 1_000_000.0, -1.0, 1_000_000.0),
            None
        );
        assert_eq!(
            macro_frame_bounds(None, f64::NAN, 3600.0, 1_000_000.0),
            None
        );
    }

    // ---------------------------------------------------------------
    // PlaybackSpeed: labels, multipliers, fps mapping, enum tables
    // ---------------------------------------------------------------

    #[wasm_bindgen_test]
    fn speed_micro_labels_are_times_notation() {
        assert_eq!(PlaybackSpeed::Realtime.label(), "1×");
        assert_eq!(PlaybackSpeed::RealtimeDouble.label(), "2×");
        assert_eq!(PlaybackSpeed::FifteenToOne.label(), "15×");
        assert_eq!(PlaybackSpeed::ThirtyToOne.label(), "30×");
        assert_eq!(PlaybackSpeed::Quarter.label(), "60×");
        assert_eq!(PlaybackSpeed::Half.label(), "120×");
        assert_eq!(PlaybackSpeed::Normal.label(), "300×");
        assert_eq!(PlaybackSpeed::Double.label(), "600×");
        assert_eq!(PlaybackSpeed::Quadruple.label(), "1200×");
    }

    #[wasm_bindgen_test]
    fn speed_macro_labels_are_fps_strings() {
        assert_eq!(PlaybackSpeed::Realtime.macro_label(), "1x (real)");
        assert_eq!(PlaybackSpeed::RealtimeDouble.macro_label(), "2x (real)");
        assert_eq!(PlaybackSpeed::FifteenToOne.macro_label(), "15s/s");
        assert_eq!(PlaybackSpeed::ThirtyToOne.macro_label(), "30s/s");
        assert_eq!(PlaybackSpeed::Quarter.macro_label(), "1 fps");
        assert_eq!(PlaybackSpeed::Half.macro_label(), "2 fps");
        assert_eq!(PlaybackSpeed::Normal.macro_label(), "5 fps");
        assert_eq!(PlaybackSpeed::Double.macro_label(), "10 fps");
        assert_eq!(PlaybackSpeed::Quadruple.macro_label(), "15 fps");
    }

    #[wasm_bindgen_test]
    fn speed_macro_fps_none_for_realtime_rungs_some_otherwise() {
        // The sub-minute / realtime rungs have no macro fps.
        assert_eq!(PlaybackSpeed::Realtime.macro_frames_per_second(), None);
        assert_eq!(
            PlaybackSpeed::RealtimeDouble.macro_frames_per_second(),
            None
        );
        assert_eq!(PlaybackSpeed::FifteenToOne.macro_frames_per_second(), None);
        assert_eq!(PlaybackSpeed::ThirtyToOne.macro_frames_per_second(), None);
        // The macro-capable rungs map to concrete fps.
        assert_eq!(PlaybackSpeed::Quarter.macro_frames_per_second(), Some(1.0));
        assert_eq!(PlaybackSpeed::Half.macro_frames_per_second(), Some(2.0));
        assert_eq!(PlaybackSpeed::Normal.macro_frames_per_second(), Some(5.0));
        assert_eq!(PlaybackSpeed::Double.macro_frames_per_second(), Some(10.0));
        assert_eq!(
            PlaybackSpeed::Quadruple.macro_frames_per_second(),
            Some(15.0)
        );
    }

    #[wasm_bindgen_test]
    fn speed_timeline_multiples_match_labels() {
        let eps = 1e-9;
        assert!((PlaybackSpeed::Realtime.timeline_seconds_per_real_second() - 1.0).abs() < eps);
        assert!(
            (PlaybackSpeed::RealtimeDouble.timeline_seconds_per_real_second() - 2.0).abs() < eps
        );
        assert!(
            (PlaybackSpeed::FifteenToOne.timeline_seconds_per_real_second() - 15.0).abs() < eps
        );
        assert!((PlaybackSpeed::ThirtyToOne.timeline_seconds_per_real_second() - 30.0).abs() < eps);
        assert!((PlaybackSpeed::Quarter.timeline_seconds_per_real_second() - 60.0).abs() < eps);
        assert!((PlaybackSpeed::Half.timeline_seconds_per_real_second() - 120.0).abs() < eps);
        assert!((PlaybackSpeed::Normal.timeline_seconds_per_real_second() - 300.0).abs() < eps);
        assert!((PlaybackSpeed::Double.timeline_seconds_per_real_second() - 600.0).abs() < eps);
        assert!((PlaybackSpeed::Quadruple.timeline_seconds_per_real_second() - 1200.0).abs() < eps);
    }

    #[wasm_bindgen_test]
    fn speed_tables_have_expected_membership() {
        // `all()` lists every variant; `macro_speeds()` is the Quarter..Quadruple
        // tail. Use lower bounds + representative membership, not fragile lengths.
        let all = PlaybackSpeed::all();
        assert_eq!(all.len(), 9);
        assert!(all.contains(&PlaybackSpeed::Realtime));
        assert!(all.contains(&PlaybackSpeed::Quadruple));
        let macros = PlaybackSpeed::macro_speeds();
        assert_eq!(macros.len(), 5);
        // Every macro speed must have a concrete fps.
        assert!(macros.iter().all(|s| s.macro_frames_per_second().is_some()));
        assert!(!macros.contains(&PlaybackSpeed::Realtime));
        assert_eq!(macros[0], PlaybackSpeed::Quarter);
        assert_eq!(macros[macros.len() - 1], PlaybackSpeed::Quadruple);
    }

    #[wasm_bindgen_test]
    fn nearest_macro_fps_defaults_slowest_for_nonpositive() {
        // Non-positive target → slowest macro speed (Quarter, 1 fps).
        assert_eq!(
            PlaybackSpeed::nearest_macro_fps(0.0),
            PlaybackSpeed::Quarter
        );
        assert_eq!(
            PlaybackSpeed::nearest_macro_fps(-100.0),
            PlaybackSpeed::Quarter
        );
        // Exact match lands on that rung.
        assert_eq!(
            PlaybackSpeed::nearest_macro_fps(10.0),
            PlaybackSpeed::Double
        );
        // Between 5 and 10 fps on a log scale: sqrt(50) ≈ 7.07 is the midpoint.
        // 8 fps sits closer to 10 (Double) than 5 (Normal).
        assert_eq!(PlaybackSpeed::nearest_macro_fps(8.0), PlaybackSpeed::Double);
    }

    #[wasm_bindgen_test]
    fn nearest_micro_multiple_matches_exact_and_picks_log_nearest() {
        // Exact multiples land on their rung.
        assert_eq!(
            PlaybackSpeed::nearest_micro_multiple(300.0),
            PlaybackSpeed::Normal
        );
        assert_eq!(
            PlaybackSpeed::nearest_micro_multiple(1.0),
            PlaybackSpeed::Realtime
        );
        // Above the top rung clamps to the fastest (1200×).
        assert_eq!(
            PlaybackSpeed::nearest_micro_multiple(100000.0),
            PlaybackSpeed::Quadruple
        );
        // 900 sits between 600 and 1200; on a log scale sqrt(600*1200)≈848 is the
        // midpoint, so 900 is closer to 1200 (Quadruple).
        assert_eq!(
            PlaybackSpeed::nearest_micro_multiple(900.0),
            PlaybackSpeed::Quadruple
        );
    }

    // ---------------------------------------------------------------
    // LoopMode / LoopBasis / LoopPreset labels & tables
    // ---------------------------------------------------------------

    #[wasm_bindgen_test]
    fn loop_mode_labels_and_table() {
        assert_eq!(LoopMode::Loop.label(), "Loop");
        assert_eq!(LoopMode::PingPong.label(), "Ping-pong");
        assert_eq!(LoopMode::Once.label(), "Once");
        let all = LoopMode::all();
        assert_eq!(all.len(), 3);
        // LoopMode has no Debug derive, so compare via PartialEq.
        assert!(all[0] == LoopMode::Loop);
        assert!(all.contains(&LoopMode::PingPong));
        assert!(all.contains(&LoopMode::Once));
        // Default is Loop.
        assert!(LoopMode::default() == LoopMode::Loop);
    }

    #[wasm_bindgen_test]
    fn loop_basis_label_branches() {
        assert_eq!(LoopBasis::FrameCount(6).label(), "6 frames");
        assert_eq!(LoopBasis::FrameCount(1).label(), "1 frames");
        // Under an hour → "min".
        assert_eq!(LoopBasis::Duration(30.0 * 60.0).label(), "30 min");
        // Just under an hour stays minutes.
        assert_eq!(LoopBasis::Duration(59.0 * 60.0).label(), "59 min");
        // At/above an hour → "h".
        assert_eq!(LoopBasis::Duration(60.0 * 60.0).label(), "1 h");
        assert_eq!(LoopBasis::Duration(120.0 * 60.0).label(), "2 h");
    }

    #[wasm_bindgen_test]
    fn loop_preset_label_branches() {
        assert_eq!(LoopPreset::LastFrames(4).label(), "Last 4 frames");
        // Duration under an hour → min; at/above → h.
        assert_eq!(LoopPreset::LastDuration(30.0 * 60.0).label(), "Last 30 min");
        assert_eq!(LoopPreset::LastDuration(60.0 * 60.0).label(), "Last 1 h");
        assert_eq!(LoopPreset::LastDuration(90.0 * 60.0).label(), "Last 2 h");
    }

    // ---------------------------------------------------------------
    // TimeSelection (the drag being drawn): range None width gate
    // ---------------------------------------------------------------

    fn drag(a: f64, b: f64) -> TimeSelection {
        TimeSelection {
            a,
            b,
            in_progress: true,
        }
    }

    #[wasm_bindgen_test]
    fn time_selection_range_requires_meaningful_width() {
        // Sub-second / zero width → None (the > 1.0 gate).
        assert_eq!(drag(100.0, 100.5).range(), None);
        // Exactly at the 1.0 boundary is NOT meaningful ( > 1.0, not >= ).
        assert_eq!(drag(100.0, 101.0).range(), None);
        // Just over 1.0 is meaningful, and normalizes regardless of order.
        assert_eq!(drag(500.0, 200.0).range(), Some((200.0, 500.0)));
    }

    // ---------------------------------------------------------------
    // TimeModel::advance + apply_bounds: Loop / PingPong / Once at both ends
    // ---------------------------------------------------------------

    const B: Option<(f64, f64)> = Some((0.0, 100.0));

    #[wasm_bindgen_test]
    fn advance_loop_wraps_at_upper_bound() {
        let mut tm = TimeModel::at_position(90.0);
        tm.loop_mode = LoopMode::Loop;
        tm.playback_position = 90.0;
        // Realtime = 1 timeline-sec per real-sec; advance 20 → 110 wraps to 10.
        assert!(
            tm.advance(20.0, PlaybackSpeed::Realtime, B),
            "reports the wrap"
        );
        assert!((tm.playback_position - 10.0).abs() < 1e-9);
        assert!(tm.direction == PlaybackDirection::Forward);
    }

    #[wasm_bindgen_test]
    fn advance_loop_wraps_at_lower_bound_going_backward() {
        let mut tm = TimeModel::at_position(10.0);
        tm.loop_mode = LoopMode::Loop;
        tm.direction = PlaybackDirection::Backward;
        tm.playback_position = 10.0;
        // Backward 20 → -10 ≤ start; Loop wraps to end - 10 = 90.
        tm.advance(20.0, PlaybackSpeed::Realtime, B);
        assert!((tm.playback_position - 90.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn advance_pingpong_reflects_at_both_bounds() {
        let mut tm = TimeModel::at_position(90.0);
        tm.loop_mode = LoopMode::PingPong;
        tm.direction = PlaybackDirection::Forward;
        tm.playback_position = 90.0;
        // Forward overshoot by 10 → reflect to 90, flip to Backward.
        tm.advance(20.0, PlaybackSpeed::Realtime, B);
        assert!((tm.playback_position - 90.0).abs() < 1e-9);
        assert!(tm.direction == PlaybackDirection::Backward);
        // Now backward from 90 by 100 → undershoot start by 10 → reflect to 10,
        // flip to Forward.
        tm.playback_position = 90.0;
        tm.advance(100.0, PlaybackSpeed::Realtime, B);
        assert!((tm.playback_position - 10.0).abs() < 1e-9);
        assert!(tm.direction == PlaybackDirection::Forward);
    }

    #[wasm_bindgen_test]
    fn advance_once_clamps_at_each_bound() {
        // Once forward: clamp at end.
        let mut tm = TimeModel::at_position(90.0);
        tm.loop_mode = LoopMode::Once;
        tm.playback_position = 90.0;
        tm.advance(20.0, PlaybackSpeed::Realtime, B);
        assert!((tm.playback_position - 100.0).abs() < 1e-9);
        // Once backward: clamp at start.
        tm.direction = PlaybackDirection::Backward;
        tm.playback_position = 10.0;
        tm.advance(20.0, PlaybackSpeed::Realtime, B);
        assert!((tm.playback_position - 0.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn advance_unbounded_moves_freely_and_speed_scales() {
        let mut tm = TimeModel::at_position(1000.0);
        // 10 real seconds at Double (600×) = 6000 timeline seconds forward.
        tm.advance(10.0, PlaybackSpeed::Double, None);
        assert!((tm.playback_position - 7000.0).abs() < 1e-6);
        // Backward direction negates the advance.
        tm.direction = PlaybackDirection::Backward;
        tm.advance(10.0, PlaybackSpeed::Double, None);
        assert!((tm.playback_position - 1000.0).abs() < 1e-6);
    }

    // ---------------------------------------------------------------
    // PlaybackState: view math, macro frame stepping & sync, mode derivation
    // ---------------------------------------------------------------

    #[wasm_bindgen_test]
    fn show_view_zooms_and_pans_to_a_span() {
        let mut ps = PlaybackState::default();
        ps.timeline_width_px = 1000.0;
        ps.view_follows_now = false;
        let now = 1_000_000.0;
        ps.show_view(now - 10_000.0, now - 6_000.0, false, now);
        assert!((ps.view_width_secs() - 4_000.0).abs() < 1e-6);
        assert!((ps.timeline_view_start - (now - 10_000.0)).abs() < 1e-6);
        assert!(!ps.view_follows_now);
        // Reaching now follows now again.
        ps.show_view(now - 3_000.0, now, false, now);
        assert!(ps.view_follows_now);
        // A span past the zoom limit keeps the named edge: the view is as
        // narrow as the zoom allows, ending where asked.
        ps.show_view(now - 0.1, now, true, now);
        let span = ps.view_width_secs();
        assert!(span > 0.1);
        assert!((ps.timeline_view_start + span - now).abs() < 1e-6);
        // An empty or inverted span changes nothing.
        let before = (ps.timeline_view_start, ps.timeline_zoom);
        ps.show_view(now, now, false, now);
        ps.show_view(now, now - 5.0, false, now);
        assert_eq!((ps.timeline_view_start, ps.timeline_zoom), before);
    }

    #[wasm_bindgen_test]
    fn view_width_secs_and_center_view_on() {
        let mut ps = PlaybackState::default();
        ps.timeline_width_px = 1000.0;
        ps.timeline_zoom = 2.0;
        // span = width / zoom = 500.
        assert!((ps.view_width_secs() - 500.0).abs() < 1e-9);
        // Centering puts ts at the middle: view_start = ts - span/2.
        ps.center_view_on(1000.0);
        assert!((ps.timeline_view_start - (1000.0 - 250.0)).abs() < 1e-9);
        // Zero zoom guards against division → 0 width.
        ps.timeline_zoom = 0.0;
        assert_eq!(ps.view_width_secs(), 0.0);
    }

    #[wasm_bindgen_test]
    fn sync_macro_frame_index_picks_nearest_frame() {
        let mut ps = PlaybackState::default();
        ps.macro_playback.sweep_frames = vec![100.0, 200.0, 300.0];
        // Closer to the lower neighbor (240 → 200, since |200-240|<|300-240|).
        ps.time_model.playback_position = 240.0;
        ps.sync_macro_frame_index();
        assert_eq!(ps.macro_playback.current_frame_index, 1);
        // Closer to the upper neighbor (290 → 300).
        ps.time_model.playback_position = 290.0;
        ps.sync_macro_frame_index();
        assert_eq!(ps.macro_playback.current_frame_index, 2);
        // Before the first frame clamps to index 0.
        ps.time_model.playback_position = 50.0;
        ps.sync_macro_frame_index();
        assert_eq!(ps.macro_playback.current_frame_index, 0);
        // After the last frame clamps to the final index.
        ps.time_model.playback_position = 9999.0;
        ps.sync_macro_frame_index();
        assert_eq!(ps.macro_playback.current_frame_index, 2);
        // Empty list resets to 0.
        ps.macro_playback.sweep_frames.clear();
        ps.macro_playback.current_frame_index = 5;
        ps.sync_macro_frame_index();
        assert_eq!(ps.macro_playback.current_frame_index, 0);
    }

    #[wasm_bindgen_test]
    fn step_macro_frame_pingpong_and_once_at_boundaries() {
        // PingPong forward off the end flips to Backward and parks at last idx.
        let mut ps = PlaybackState::default();
        ps.macro_playback.sweep_frames = vec![10.0, 20.0, 30.0];
        ps.macro_playback.current_frame_index = 2;
        ps.time_model.loop_mode = LoopMode::PingPong;
        ps.step_macro_frame(1);
        assert_eq!(ps.macro_playback.current_frame_index, 2);
        assert!(ps.time_model.direction == PlaybackDirection::Backward);
        assert_eq!(ps.time_model.playback_position, 30.0); // snapped to frame

        // PingPong backward off the start flips to Forward and parks at index 0.
        ps.macro_playback.current_frame_index = 0;
        ps.time_model.direction = PlaybackDirection::Backward;
        ps.step_macro_frame(-1);
        assert_eq!(ps.macro_playback.current_frame_index, 0);
        assert!(ps.time_model.direction == PlaybackDirection::Forward);

        // Once forward at the end stops playback and snaps to the last frame.
        ps.time_model.loop_mode = LoopMode::Once;
        ps.macro_playback.current_frame_index = 2;
        ps.playing = true;
        ps.step_macro_frame(1);
        assert_eq!(ps.macro_playback.current_frame_index, 2);
        assert!(!ps.playing);
        assert_eq!(ps.time_model.playback_position, 30.0);
    }

    #[wasm_bindgen_test]
    fn step_and_advance_macro_are_inert_on_empty_frames() {
        let mut ps = PlaybackState::default();
        ps.macro_playback.sweep_frames.clear();
        ps.macro_playback.current_frame_index = 0;
        ps.time_model.playback_position = 555.0;
        ps.playing = true;
        // No frames: neither path moves the cursor or position.
        ps.step_macro_frame(1);
        ps.advance_macro(10.0);
        assert_eq!(ps.macro_playback.current_frame_index, 0);
        assert_eq!(ps.time_model.playback_position, 555.0);
    }

    #[wasm_bindgen_test]
    fn enter_pinned_live_drops_bounds_window_and_pending() {
        let mut ps = PlaybackState::default();
        assert!(ps.select_range(100.0, 200.0, false));
        ps.drag = Some(TimeSelection {
            a: 1.0,
            b: 50.0,
            in_progress: true,
        });
        ps.pending_loop_window = Some((1.0, 2.0));
        ps.enter_pinned_live(1234.0);
        assert_eq!(ps.selection, Selection::Point { live: true });
        assert!(ps.is_pinned());
        assert!(!ps.is_live_range());
        assert_eq!(ps.playback_bounds(), None);
        assert!(ps.drag.is_none());
        assert!(ps.pending_loop_window.is_none());
        assert_eq!(ps.playback_position(), 1234.0);
    }

    #[wasm_bindgen_test]
    fn enter_pinned_live_forces_realtime_speed_in_rays() {
        // Regression for #139: a large × multiple must not remain selected
        // once tethered in Rays.
        let mut ps = PlaybackState::default();
        ps.set_play_mode(PlaybackMode::Micro);
        ps.speed = PlaybackSpeed::Quadruple;
        ps.enter_pinned_live(1000.0);
        assert_eq!(ps.speed, PlaybackSpeed::Realtime);
        assert_eq!(ps.speed.label(), "1×");
    }

    #[wasm_bindgen_test]
    fn enter_pinned_live_keeps_fps_in_frames() {
        let mut ps = PlaybackState::default();
        ps.speed = PlaybackSpeed::Quadruple;
        ps.enter_pinned_live(1000.0);
        assert_eq!(ps.speed, PlaybackSpeed::Quadruple);
        // A Rays-only speed falls back to the default fps.
        let mut ps = PlaybackState::default();
        ps.speed = PlaybackSpeed::Realtime;
        ps.enter_pinned_live(1000.0);
        assert!(ps.speed.macro_frames_per_second().is_some());
    }

    #[wasm_bindgen_test]
    fn exit_lookback_to_now_restores_realtime_speed_in_rays() {
        let mut ps = PlaybackState::default();
        ps.set_play_mode(PlaybackMode::Micro);
        ps.enter_pinned_live(1000.0);
        ps.enter_live_range(Some(940.0), LoopBasis::FrameCount(6));
        // Lookback UI may leave a macro fps selection selected.
        ps.speed = PlaybackSpeed::Normal;
        ps.enter_pinned_live(1010.0);
        assert!(ps.is_pinned());
        assert_eq!(ps.speed, PlaybackSpeed::Realtime);
    }

    #[wasm_bindgen_test]
    fn switching_to_rays_while_pinned_shows_realtime() {
        // Frames 5 fps × 300s spacing would convert to 1200×, which would
        // misstate the speed of the live edge.
        let mut ps = PlaybackState::default();
        ps.speed = PlaybackSpeed::Normal; // 5 fps
        ps.macro_playback.sweep_frames = vec![0.0, 300.0, 600.0, 900.0];
        ps.enter_pinned_live(1000.0);
        ps.set_play_mode(PlaybackMode::Micro);
        assert_eq!(ps.speed, PlaybackSpeed::Realtime);
        assert_eq!(ps.speed.label(), "1×");
        // And back: 1× would convert to the slowest fps, so live Frames gets
        // its default instead.
        ps.set_play_mode(PlaybackMode::Macro);
        assert_eq!(ps.speed, PlaybackSpeed::default());
    }

    #[wasm_bindgen_test]
    fn commit_pinned_window_orders_reversed_target_args() {
        // order_pair (via commit) normalizes a reversed target while paused.
        let mut ps = PlaybackState::default();
        assert!(!ps.playing);
        let w = ps.commit_pinned_window(400.0, 100.0);
        assert_eq!(w, (100.0, 400.0));
        // While playing with no prior bounds the reversed target is taken
        // immediately, still ordered.
        ps.playing = true;
        assert_eq!(ps.playback_bounds(), None);
        let w2 = ps.commit_pinned_window(260.0, 150.0);
        assert_eq!(w2, (150.0, 260.0));
        assert!(ps.pending_loop_window.is_none());
    }
}
