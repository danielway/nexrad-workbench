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
            PlaybackSpeed::Quarter => "0.25x",
            PlaybackSpeed::Half => "0.5x",
            PlaybackSpeed::Normal => "1x",
            PlaybackSpeed::Double => "2x",
            PlaybackSpeed::Quadruple => "4x",
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

    /// Timeline zoom level (pixels per frame)
    pub timeline_zoom: f32,

    /// Timeline pan offset (in frames from start)
    pub timeline_pan: f32,
}

impl PlaybackState {
    pub fn new() -> Self {
        Self {
            total_frames: 100,  // Placeholder for UI demonstration
            timeline_zoom: 5.0, // Default: 5 pixels per frame
            ..Default::default()
        }
    }

    pub fn toggle_playback(&mut self) {
        self.playing = !self.playing;
    }

    pub fn frame_label(&self) -> String {
        format!("{} / {}", self.current_frame, self.total_frames)
    }
}
