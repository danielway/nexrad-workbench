//! Application state management.
//!
//! This module contains all state structures used throughout the application.
//! State is organized into logical groupings that correspond to different
//! areas of functionality.

mod data_source;
mod layer;
mod playback;
mod processing;
mod viz;

pub use data_source::UploadState;
pub use layer::{GeoLayerVisibility, LayerState};
pub use playback::{PlaybackMode, PlaybackSpeed, PlaybackState};
pub use processing::ProcessingState;
pub use viz::{ColorPalette, RadarProduct, VizState};

/// Root application state containing all sub-states.
#[derive(Default)]
pub struct AppState {
    /// State for file upload
    pub upload_state: UploadState,

    /// Playback controls state
    pub playback_state: PlaybackState,

    /// Visualization state (canvas, zoom/pan, product selection)
    pub viz_state: VizState,

    /// Layer visibility toggles
    pub layer_state: LayerState,

    /// Processing options
    pub processing_state: ProcessingState,

    /// Application status message displayed in top bar
    pub status_message: String,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            playback_state: PlaybackState::new(),
            status_message: "Ready".to_string(),
            ..Default::default()
        }
    }
}
