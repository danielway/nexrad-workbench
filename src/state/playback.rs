//! Playback controls state.

// Fields and methods are defined for future integration
#![allow(dead_code)]

/// Playback speed multiplier options.
#[derive(Default, Clone, Copy, PartialEq)]
pub enum PlaybackSpeed {
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
            PlaybackSpeed::Quarter => "1 min/s",
            PlaybackSpeed::Half => "2 min/s",
            PlaybackSpeed::Normal => "5 min/s",
            PlaybackSpeed::Double => "10 min/s",
            PlaybackSpeed::Quadruple => "20 min/s",
        }
    }

    pub fn all() -> &'static [PlaybackSpeed] {
        &[
            PlaybackSpeed::Quarter,
            PlaybackSpeed::Half,
            PlaybackSpeed::Normal,
            PlaybackSpeed::Double,
            PlaybackSpeed::Quadruple,
        ]
    }

    pub fn multiplier(&self) -> f32 {
        match self {
            PlaybackSpeed::Quarter => 0.25,
            PlaybackSpeed::Half => 0.5,
            PlaybackSpeed::Normal => 1.0,
            PlaybackSpeed::Double => 2.0,
            PlaybackSpeed::Quadruple => 4.0,
        }
    }

    /// Returns how many seconds of timeline time pass per real second.
    /// Based on the label (e.g., "5 min/s" = 300 seconds per real second).
    pub fn timeline_seconds_per_real_second(&self) -> f64 {
        match self {
            PlaybackSpeed::Quarter => 60.0,     // 1 min/s
            PlaybackSpeed::Half => 120.0,       // 2 min/s
            PlaybackSpeed::Normal => 300.0,     // 5 min/s
            PlaybackSpeed::Double => 600.0,     // 10 min/s
            PlaybackSpeed::Quadruple => 1200.0, // 20 min/s
        }
    }
}

/// Playback mode options.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackMode {
    /// Radial-accurate playback (renders each radial as received)
    #[default]
    RadialAccurate,
    /// Frame step playback (jumps between complete frames)
    FrameStep,
}

impl PlaybackMode {
    pub fn label(&self) -> &'static str {
        match self {
            PlaybackMode::RadialAccurate => "Radial-accurate",
            PlaybackMode::FrameStep => "Frame step",
        }
    }
}

/// State for playback controls.
#[derive(Default)]
pub struct PlaybackState {
    /// Whether playback is currently active
    pub playing: bool,

    /// Current frame index in the timeline
    pub current_frame: usize,

    /// Total number of frames available
    pub total_frames: usize,

    /// Current playback speed
    pub speed: PlaybackSpeed,

    /// Current playback mode
    pub mode: PlaybackMode,

    /// Timeline zoom level (pixels per second)
    pub timeline_zoom: f64,

    /// Timeline view position - absolute timestamp of left edge (Unix seconds, with sub-second precision)
    pub timeline_view_start: f64,

    /// User-selected timestamp for seeking/downloading (Unix seconds with sub-second precision)
    /// This is where the user clicked, independent of loaded data
    pub selected_timestamp: Option<f64>,

    /// Start timestamp of loaded data (Unix seconds), if any
    pub data_start_timestamp: Option<i64>,

    /// End timestamp of loaded data (Unix seconds), if any
    pub data_end_timestamp: Option<i64>,
}

impl PlaybackState {
    pub fn new() -> Self {
        Self::new_at_time(1714521600.0) // 2024-05-01 00:00:00 UTC
    }

    pub fn new_at_time(now: f64) -> Self {
        // Start zoomed to show about 1 hour, centered on "now"
        let zoom = 0.15; // ~0.15 px/sec means ~1.8 hours visible in 1000px
        let view_width_secs = 1000.0 / zoom; // Approximate visible width

        Self {
            total_frames: 0,
            timeline_zoom: zoom,
            timeline_view_start: now - view_width_secs / 2.0, // Center on "now"
            selected_timestamp: Some(now),                    // Start with "now" selected
            data_start_timestamp: None,
            data_end_timestamp: None,
            ..Default::default()
        }
    }

    pub fn toggle_playback(&mut self) {
        self.playing = !self.playing;
    }

    pub fn frame_label(&self) -> String {
        format!("{} / {}", self.current_frame, self.total_frames)
    }

    /// Get the duration of loaded data in seconds
    pub fn data_duration_secs(&self) -> f64 {
        match (self.data_start_timestamp, self.data_end_timestamp) {
            (Some(start), Some(end)) => (end - start) as f64,
            _ => 0.0,
        }
    }

    /// Check if we have any loaded data
    pub fn has_data(&self) -> bool {
        self.data_start_timestamp.is_some() && self.total_frames > 0
    }

    /// Get the timestamp for the current frame (if data is loaded)
    pub fn current_timestamp(&self) -> Option<i64> {
        let start = self.data_start_timestamp?;
        let duration = self.data_duration_secs();
        if self.total_frames == 0 || duration <= 0.0 {
            return Some(start);
        }
        let position = self.current_frame as f64 / self.total_frames as f64;
        Some(start + (position * duration) as i64)
    }

    /// Convert a timestamp to a frame index (clamped to valid range)
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
                .clamp(0.0, (self.total_frames - 1) as f64) as usize,
        )
    }
}
