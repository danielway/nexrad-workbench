//! Playback controls state per PRODUCT.md specification.
//!
//! Implements a dual-time model separating playback position from wall-clock time,
//! with timeline bounds enforcement and zoom-based feature restrictions.

/// Playback speed multiplier options.
#[derive(Default, Clone, Copy, PartialEq)]
pub enum PlaybackSpeed {
    /// Real-time: 1 second of timeline = 1 second of real time
    Realtime,
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
            PlaybackSpeed::ThirtyToOne,
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

    /// Seek to a specific position, respecting bounds.
    pub fn seek_to(&mut self, position: f64) {
        if self.locked_to_realtime {
            return; // Can't seek in real-time mode
        }

        if let Some((start, end)) = self.playback_bounds {
            self.playback_position = position.clamp(start, end);
        } else {
            self.playback_position = position;
        }
    }
}

/// Timeline bounds per PRODUCT.md specification.
///
/// The timeline has hard bounds: right = now + epsilon, left = start of NEXRAD data.
/// Zoom level governs which operations are available.
#[derive(Clone)]
pub struct TimelineBounds {
    /// Absolute left bound (start of NEXRAD data: ~1991).
    pub hard_left: f64,

    /// Small buffer beyond "now" for right bound (seconds).
    pub right_epsilon: f64,

    /// Minimum zoom level (px/sec) for playback to be enabled.
    /// Below this, playback is disabled to avoid overwhelming data acquisition.
    pub playback_min_zoom: f64,

    /// Minimum zoom level for real-time mode to be available.
    pub realtime_min_zoom: f64,

    /// Maximum historical window for real-time mode (seconds).
    /// In real-time mode, left bound is constrained to now - this value.
    pub realtime_history_window: f64,
}

impl Default for TimelineBounds {
    fn default() -> Self {
        Self {
            // NEXRAD data started ~1991
            hard_left: 662688000.0, // 1991-01-01 00:00:00 UTC
            // 5 minute buffer beyond now
            right_epsilon: 300.0,
            // Playback requires at least 0.1 px/sec (~3 hours visible in 1000px)
            playback_min_zoom: 0.1,
            // Real-time requires at least 0.5 px/sec (~30 min visible in 1000px)
            realtime_min_zoom: 0.5,
            // Real-time mode shows up to 1 hour of history
            realtime_history_window: 3600.0,
        }
    }
}

impl TimelineBounds {
    /// Get the current right bound (now + epsilon).
    pub fn hard_right(&self) -> f64 {
        TimeModel::wall_clock_time() + self.right_epsilon
    }

    /// Check if playback is allowed at the given zoom level.
    pub fn is_playback_allowed(&self, zoom: f64) -> bool {
        zoom >= self.playback_min_zoom
    }

    /// Check if real-time mode is allowed at the given zoom level.
    pub fn is_realtime_allowed(&self, zoom: f64) -> bool {
        zoom >= self.realtime_min_zoom
    }

    /// Clamp a view position to valid bounds.
    pub fn clamp_view_start(&self, view_start: f64, view_width_secs: f64) -> f64 {
        let max_start = self.hard_right() - view_width_secs;
        view_start.clamp(self.hard_left, max_start.max(self.hard_left))
    }

    /// Clamp a playback position to valid bounds.
    pub fn clamp_playback_position(&self, position: f64) -> f64 {
        position.clamp(self.hard_left, self.hard_right())
    }

    /// Get the left bound for real-time mode (now - history window).
    pub fn realtime_left_bound(&self) -> f64 {
        TimeModel::wall_clock_time() - self.realtime_history_window
    }
}

/// State for playback controls.
pub struct PlaybackState {
    /// Whether playback is currently active
    pub playing: bool,

    /// Time model (playback position, bounds, loop mode)
    pub time_model: TimeModel,

    /// Timeline bounds (hard limits, zoom restrictions)
    pub bounds: TimelineBounds,

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
}

impl Default for PlaybackState {
    fn default() -> Self {
        let now = TimeModel::wall_clock_time();
        let zoom = 0.15; // ~0.15 px/sec means ~1.8 hours visible in 1000px
        let view_width_secs = 1000.0 / zoom;

        Self {
            playing: false,
            time_model: TimeModel::at_position(now),
            bounds: TimelineBounds::default(),
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
        }
    }
}

impl PlaybackState {
    pub fn new() -> Self {
        Self::default()
    }

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

    /// Get playback position as Option for compatibility with old code.
    /// Always returns Some in the new model.
    pub fn selected_timestamp(&self) -> Option<f64> {
        Some(self.time_model.playback_position)
    }

    /// Set playback position (convenience method).
    pub fn set_playback_position(&mut self, position: f64) {
        self.time_model.seek_to(position);
    }

    /// Toggle playback on/off.
    pub fn toggle_playback(&mut self) {
        // Only allow playback if zoom permits
        if self.bounds.is_playback_allowed(self.timeline_zoom) {
            self.playing = !self.playing;
        } else {
            self.playing = false;
        }
    }

    /// Check if playback is allowed at current zoom.
    pub fn is_playback_allowed(&self) -> bool {
        self.bounds.is_playback_allowed(self.timeline_zoom)
    }

    /// Check if real-time mode is allowed at current zoom.
    pub fn is_realtime_allowed(&self) -> bool {
        self.bounds.is_realtime_allowed(self.timeline_zoom)
    }

    /// Advance playback by delta time.
    pub fn advance(&mut self, delta_secs: f64) {
        if self.playing {
            self.time_model.advance(delta_secs, self.speed);
        }
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

    /// Get the visible time range at current zoom.
    pub fn visible_time_range(&self, view_width_pixels: f64) -> (f64, f64) {
        let duration = view_width_pixels / self.timeline_zoom;
        (self.timeline_view_start, self.timeline_view_start + duration)
    }

    /// Clamp view to bounds after pan/zoom.
    pub fn clamp_view_to_bounds(&mut self, view_width_pixels: f64) {
        let duration = view_width_pixels / self.timeline_zoom;
        self.timeline_view_start = self.bounds.clamp_view_start(self.timeline_view_start, duration);
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
