//! Application state management.
//!
//! This module contains all state structures used throughout the application.
//! State is organized into logical groupings that correspond to different
//! areas of functionality.

mod data_source;
mod layer;
mod playback;
mod processing;
pub mod radar_data;
mod stats;
pub mod vcp;
mod viz;

pub use data_source::UploadState;
pub use layer::{GeoLayerVisibility, LayerState};
pub use playback::{PlaybackSpeed, PlaybackState};
pub use processing::ProcessingState;
pub use radar_data::RadarTimeline;
pub use stats::SessionStats;
pub use vcp::get_vcp_definition;
pub use viz::{ColorPalette, RadarProduct, VizState};

/// Root application state containing all sub-states.
#[derive(Default)]
pub struct AppState {
    /// State for file upload
    pub upload_state: UploadState,

    /// Playback controls state
    pub playback_state: PlaybackState,

    /// Radar timeline data (scans, sweeps, radials)
    pub radar_timeline: RadarTimeline,

    /// Visualization state (canvas, zoom/pan, product selection)
    pub viz_state: VizState,

    /// Layer visibility toggles
    pub layer_state: LayerState,

    /// Processing options
    pub processing_state: ProcessingState,

    /// Application status message displayed in top bar
    pub status_message: String,

    /// Session and performance statistics
    pub session_stats: SessionStats,
}

impl AppState {
    pub fn new() -> Self {
        // Use a fixed "now" for demo (2024-05-01 12:00:00 UTC)
        let now = 1714564800.0_f64;

        // Generate sample radar data for the last 3 hours
        let radar_timeline = RadarTimeline::generate_sample_data(now, 3.0);

        // Set up playback state centered on "now"
        let mut playback_state = PlaybackState::new_at_time(now);

        // Update data range from the generated timeline
        if let Some((start, end)) = radar_timeline.time_range() {
            playback_state.data_start_timestamp = Some(start as i64);
            playback_state.data_end_timestamp = Some(end as i64);
        }

        Self {
            playback_state,
            radar_timeline,
            status_message: "Ready".to_string(),
            session_stats: SessionStats::with_dummy_data(),
            ..Default::default()
        }
    }
}
