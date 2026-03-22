//! Computed, read-only snapshot of live radar state.
//!
//! `LiveRadarModel` is derived once per frame from `LiveModeState`, capturing
//! the current wall-clock `now` so all UI consumers see a consistent picture
//! of what sweeps/chunks are in the past, which sweep is being received, and
//! what the radar is doing right now.

use super::LiveModeState;

/// Computed snapshot of live radar state for consistent UI consumption.
///
/// Derived once per frame from [`LiveModeState`]. All fields reflect the same
/// `now` timestamp, eliminating inconsistencies between components that would
/// otherwise independently call `js_sys::Date::now()`.
#[derive(Clone, Debug, Default)]
pub struct LiveRadarModel {
    /// Whether live streaming is active (not Idle, not Error).
    pub active: bool,

    /// Extrapolated radar azimuth at snapshot time (degrees, 0-360).
    pub estimated_azimuth: Option<f32>,

    /// Volume-level progress (present when streaming has started a volume).
    pub volume: Option<LiveVolumeModel>,

    /// Active sweep being received (present when an elevation is in progress).
    pub active_sweep: Option<LiveSweepModel>,
}

/// Volume-level state for the in-progress scan.
#[derive(Clone, Debug)]
pub struct LiveVolumeModel {
    /// Scan key ("SITE|TIMESTAMP_MS") for the in-progress volume.
    pub scan_key: Option<String>,

    /// VCP pattern for elevation angle lookups.
    pub vcp_pattern: Option<crate::data::keys::ExtractedVcp>,

    /// Elevation numbers that have completed in this volume.
    pub elevations_complete: Vec<u8>,

    /// Expected total elevation count from the VCP.
    pub elevations_expected: Option<u8>,
}

/// Active sweep state: the elevation currently being collected.
#[derive(Clone, Debug)]
pub struct LiveSweepModel {
    /// Elevation number being collected.
    pub elevation_number: u8,

    /// Radials received so far for this elevation.
    pub radials_received: u32,

    /// Azimuth range of actual received data (first_az, last_az).
    pub data_azimuth_range: Option<(f32, f32)>,

    /// Starting azimuth of the sweep (first radial).
    pub sweep_start_azimuth: Option<f32>,

    /// Per-chunk azimuth boundaries within this sweep.
    pub chunks: Vec<LiveChunkBoundary>,

    /// Expected total chunks for this sweep (from VCP timing).
    pub chunks_expected: Option<u32>,

    /// Per-chunk time spans for the current elevation (start_secs, end_secs, radial_count).
    /// Pre-filtered from `LiveModeState::chunk_elev_spans` for the active elevation.
    pub chunk_time_spans: Vec<(f64, f64, u32)>,
}

/// A single chunk's azimuth boundary within a sweep.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct LiveChunkBoundary {
    pub first_az: f32,
    pub last_az: f32,
    pub radial_count: u32,
}

impl LiveModeState {
    /// Compute a consistent, read-only model of the current live radar state.
    ///
    /// Call once per frame at the start of the UI rendering pass, then pass the
    /// result to all consumers so they see the same `now` timestamp.
    pub fn compute_model(&self, now_secs: f64) -> LiveRadarModel {
        let active = self.is_active();
        if !active {
            return LiveRadarModel::default();
        }

        let estimated_azimuth = self.estimated_azimuth(now_secs);

        let volume = Some(LiveVolumeModel {
            scan_key: self.current_scan_key.clone(),
            vcp_pattern: self.current_vcp_pattern.clone(),
            elevations_complete: self.elevations_received.clone(),
            elevations_expected: self.expected_elevation_count,
        });

        let active_sweep = self.current_in_progress_elevation.map(|elev| {
            let current_elev = elev;
            let chunk_time_spans: Vec<(f64, f64, u32)> = self
                .chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == current_elev)
                .map(|&(_, start, end, radials)| (start, end, radials))
                .collect();

            LiveSweepModel {
                elevation_number: elev,
                radials_received: self.current_in_progress_radials.unwrap_or(0),
                data_azimuth_range: self.live_data_azimuth_range,
                sweep_start_azimuth: self.sweep_start_azimuth,
                chunks: self
                    .current_elev_chunks
                    .iter()
                    .map(|&(first, last, count)| LiveChunkBoundary {
                        first_az: first,
                        last_az: last,
                        radial_count: count,
                    })
                    .collect(),
                chunks_expected: self.expected_chunks_for_current_sweep(),
                chunk_time_spans,
            }
        });

        LiveRadarModel {
            active,
            estimated_azimuth,
            volume,
            active_sweep,
        }
    }
}
