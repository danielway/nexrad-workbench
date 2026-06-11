//! Playback controls state per PRODUCT.md specification.
//!
//! Implements a dual-time model separating playback position from wall-clock time,
//! with timeline bounds enforcement and zoom-based feature restrictions.

/// Zoom boundary between macro (scan blocks) and micro (individual sweeps) modes,
/// in pixels per second.
pub const MICRO_ZOOM_THRESHOLD: f64 = 1.0;

/// Playback mode derived from timeline zoom level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaybackMode {
    /// Frame-stepping between matching sweeps (zoomed out, < 1.0 px/sec)
    Macro,
    /// Continuous time-based playback (zoomed in, >= 1.0 px/sec)
    Micro,
}

/// Inputs the macro `sweep_frames` list is built from. Compared against the
/// previous build's inputs to decide whether a rebuild is needed.
#[derive(PartialEq, Clone, Default)]
pub struct MacroFrameInputs {
    pub elevation: super::viz::ElevationSelection,
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
    /// Whether the previous frame was in macro mode (for transition detection).
    pub was_macro: bool,
}

impl Default for MacroPlaybackState {
    fn default() -> Self {
        Self {
            sweep_frames: Vec::new(),
            current_frame_index: 0,
            frame_accumulator: 0.0,
            built_from: MacroFrameInputs::default(),
            last_seen_position: 0.0,
            was_macro: false,
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
        } else if self.built_from.bounds != inputs.bounds
            || self.built_from.scan_count != inputs.scan_count
        {
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
#[derive(Default, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
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
    pub fn label(&self) -> &'static str {
        match self {
            PlaybackSpeed::Realtime => "1x (real)",
            PlaybackSpeed::RealtimeDouble => "2x (real)",
            PlaybackSpeed::FifteenToOne => "15s/s",
            PlaybackSpeed::ThirtyToOne => "30s/s",
            PlaybackSpeed::Quarter => "1 min/s",
            PlaybackSpeed::Half => "2 min/s",
            PlaybackSpeed::Normal => "5 min/s",
            PlaybackSpeed::Double => "10 min/s",
            PlaybackSpeed::Quadruple => "20 min/s",
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

    /// Timeline view position - absolute timestamp of left edge (Unix seconds)
    pub timeline_view_start: f64,

    /// Start of user's timeline selection (Unix seconds), if selecting
    pub selection_start: Option<f64>,

    /// End of user's timeline selection (Unix seconds), if selecting
    pub selection_end: Option<f64>,

    /// Whether a drag selection is currently in progress
    pub selection_in_progress: bool,

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
            timeline_view_start: now - view_width_secs / 2.0,
            selection_start: None,
            selection_end: None,
            selection_in_progress: false,
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
        self.time_model.mode = PlayheadMode::PinnedToNow;
        self.time_model.playback_position = now;
    }

    /// LIVE-* → ARCHIVE (`Free`). Clears the lookback loop window if one
    /// was active; a user's shift-drag selection bounds (only settable in
    /// `Free`) are untouched. `freeze` picks the landing position.
    /// Idempotent when already `Free`.
    pub fn exit_live(&mut self, freeze: FreezeAt) {
        if self.time_model.mode == PlayheadMode::LookbackLoop {
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
    pub fn enter_lookback(&mut self, seed: Option<f64>) {
        debug_assert!(
            self.time_model.mode == PlayheadMode::PinnedToNow,
            "lookback starts from pinned live, not {:?}",
            self.time_model.mode
        );
        self.time_model.mode = PlayheadMode::LookbackLoop;
        self.time_model.loop_mode = LoopMode::Loop;
        if let Some(ts) = seed {
            self.time_model.playback_position = ts;
        }
        self.playing = true;
        self.macro_playback.current_frame_index = 0;
        self.macro_playback.frame_accumulator = 0.0;
    }

    /// LIVE-LOOKBACK → LIVE-NOW (pause during replay): drop the loop window
    /// and re-pin to `now`. The stream is untouched.
    pub fn exit_lookback_to_now(&mut self, now: f64) {
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

    /// Check if playback is allowed at current zoom level.
    /// Playback requires at least 0.1 px/sec (~3 hours visible in 1000px).
    pub fn is_playback_allowed(&self) -> bool {
        self.timeline_zoom >= 0.1
    }

    /// Derive the current playback mode from timeline zoom level.
    pub fn playback_mode(&self) -> PlaybackMode {
        if self.timeline_zoom < MICRO_ZOOM_THRESHOLD {
            PlaybackMode::Macro
        } else {
            PlaybackMode::Micro
        }
    }

    /// Playback mode for advance dispatch + speed UI. Lookback always
    /// frame-steps (Macro) regardless of zoom so it snaps between the recent
    /// sweeps as frames; otherwise this is the zoom-derived
    /// [`Self::playback_mode`].
    pub fn effective_playback_mode(&self) -> PlaybackMode {
        if self.time_model.is_lookback() {
            PlaybackMode::Macro
        } else {
            self.playback_mode()
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
        match (self.selection_start, self.selection_end) {
            (Some(a), Some(b)) => {
                let start = a.min(b);
                let end = a.max(b);
                // Only return if selection has meaningful width (> 1 second)
                if (end - start).abs() > 1.0 {
                    Some((start, end))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Clear the current selection.
    pub fn clear_selection(&mut self) {
        self.selection_start = None;
        self.selection_end = None;
        self.selection_in_progress = false;
        self.time_model.clear_bounds();
    }

    /// Apply selection as playback bounds.
    pub fn apply_selection_as_bounds(&mut self) {
        if let Some((start, end)) = self.selection_range() {
            self.time_model.set_bounds_from_selection(start, end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn enter_lookback_sets_loop_seed_and_resets_macro_cursor() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        // Pretend a prior macro session left a non-zero cursor.
        ps.macro_playback.current_frame_index = 7;
        ps.macro_playback.frame_accumulator = 0.4;
        ps.enter_lookback(Some(940.0));
        assert!(ps.time_model.loop_mode == LoopMode::Loop);
        assert!(ps.time_model.is_lookback());
        assert!(ps.playing);
        assert_eq!(ps.playback_position(), 940.0);
        assert_eq!(ps.macro_playback.current_frame_index, 0);
        assert_eq!(ps.macro_playback.frame_accumulator, 0.0);
    }

    #[wasm_bindgen_test]
    fn effective_mode_is_macro_iff_lookback() {
        let mut ps = PlaybackState::default();
        ps.timeline_zoom = 2.0; // micro zoom (live)
        assert!(ps.effective_playback_mode() == PlaybackMode::Micro);
        ps.enter_pinned_live(1000.0);
        ps.enter_lookback(None);
        assert!(ps.effective_playback_mode() == PlaybackMode::Macro);
    }

    #[wasm_bindgen_test]
    fn exit_lookback_to_now_clears_window_and_repins() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_lookback(Some(940.0));
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
        ps.enter_lookback(Some(940.0));
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
}
