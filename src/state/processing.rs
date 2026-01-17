//! Processing options state.

// Fields and methods are defined for future integration
#![allow(dead_code)]

/// State for radar data processing options.
#[derive(Default)]
pub struct ProcessingState {
    /// Enable spatial smoothing
    pub smoothing_enabled: bool,

    /// Smoothing strength (0.0 - 1.0)
    pub smoothing_strength: f32,

    /// Enable velocity dealiasing
    pub dealiasing_enabled: bool,

    /// Dealiasing aggressiveness (0.0 - 1.0)
    pub dealiasing_strength: f32,
}

impl ProcessingState {
    pub fn new() -> Self {
        Self {
            smoothing_strength: 0.5,
            dealiasing_strength: 0.5,
            ..Default::default()
        }
    }
}
