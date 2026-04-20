//! Playback controls state per PRODUCT.md specification.
//!
//! Implements a dual-time model separating playback position from wall-clock time,
//! with timeline bounds enforcement and zoom-based feature restrictions.

/// Playback mode derived from timeline zoom level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaybackMode {
    /// Frame-stepping between matching sweeps (zoomed out, < 1.0 px/sec)
    Macro,
    /// Continuous time-based playback (zoomed in, >= 1.0 px/sec)
    Micro,
}

/// State for macro (frame-stepping) playback.
pub struct MacroPlaybackState {
    /// Sorted sweep end-times matching the user's elevation filter.
    pub sweep_frames: Vec<f64>,
    /// Current index into sweep_frames.
    pub current_frame_index: usize,
    /// Fractional frame accumulator for sub-frame advancement.
    pub frame_accumulator: f64,
    /// Cached filter params for dirty-checking.
    pub cached_elevation_selection: super::viz::ElevationSelection,
    pub cached_bounds: Option<(f64, f64)>,
    pub cached_scan_count: usize,
    /// Last known playback position, used to detect manual seeks.
    pub cached_playback_position: f64,
    /// Whether the previous frame was in macro mode (for transition detection).
    pub was_macro: bool,
}

impl Default for MacroPlaybackState {
    fn default() -> Self {
        Self {
            sweep_frames: Vec::new(),
            current_frame_index: 0,
            frame_accumulator: 0.0,
            cached_elevation_selection: super::viz::ElevationSelection::default(),
            cached_bounds: None,
            cached_scan_count: 0,
            cached_playback_position: 0.0,
            was_macro: false,
        }
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

    /// Whether playback is locked to wall-clock (real-time mode).
    /// When true, playback_position tracks wall_clock_time().
    pub locked_to_realtime: bool,

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
            locked_to_realtime: false,
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

    /// Advance playback position by delta time, respecting bounds and loop mode.
    pub fn advance(&mut self, delta_secs: f64, speed: PlaybackSpeed) {
        if self.locked_to_realtime {
            // Real-time mode: track wall clock
            self.playback_position = Self::wall_clock_time();
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

    /// Enable real-time lock (playback tracks wall clock).
    pub fn enable_realtime_lock(&mut self) {
        self.locked_to_realtime = true;
        self.playback_position = Self::wall_clock_time();
        self.playback_bounds = None; // Real-time mode has its own constraints
    }

    /// Disable real-time lock.
    pub fn disable_realtime_lock(&mut self) {
        self.locked_to_realtime = false;
    }

    /// Seek to a specific position.
    pub fn seek_to(&mut self, position: f64) {
        if self.locked_to_realtime {
            return; // Can't seek in real-time mode
        }

        self.playback_position = position;
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

    // Legacy fields maintained for compatibility during transition
    /// Start timestamp of loaded data (Unix seconds), if any
    pub data_start_timestamp: Option<i64>,

    /// End timestamp of loaded data (Unix seconds), if any
    pub data_end_timestamp: Option<i64>,

    /// Current frame index (legacy, used by some displays)
    pub current_frame: usize,

    /// Total frames (legacy, used by some displays)
    pub total_frames: usize,

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
            data_start_timestamp: None,
            data_end_timestamp: None,
            current_frame: 0,
            total_frames: 0,
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

    /// Set playback position (convenience method).
    pub fn set_playback_position(&mut self, position: f64) {
        self.time_model.seek_to(position);
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
        if self.timeline_zoom < 1.0 {
            PlaybackMode::Macro
        } else {
            PlaybackMode::Micro
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
    fn snap_playback_to_macro_frame(&mut self) {
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

    /// Get data duration in seconds.
    pub fn data_duration_secs(&self) -> f64 {
        match (self.data_start_timestamp, self.data_end_timestamp) {
            (Some(start), Some(end)) => (end - start) as f64,
            _ => 0.0,
        }
    }

    /// Convert timestamp to frame index (legacy compatibility).
    pub fn timestamp_to_frame(&self, timestamp: i64) -> Option<usize> {
        let start = self.data_start_timestamp?;
        let duration = self.data_duration_secs();
        if self.total_frames == 0 || duration <= 0.0 {
            return Some(0);
        }
        let position = (timestamp - start) as f64 / duration;
        Some(
            (position * self.total_frames as f64)
                .round()
                .clamp(0.0, (self.total_frames.saturating_sub(1)) as f64) as usize,
        )
    }
}
