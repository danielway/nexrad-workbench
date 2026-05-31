//! Computed, read-only snapshot of live radar state.
//!
//! `LiveRadarModel` is derived once per frame from `LiveModeState`, capturing
//! the current wall-clock `now` so all UI consumers see a consistent picture
//! of what sweeps/chunks are in the past, which sweep is being received, and
//! what the radar is doing right now.

use super::LiveModeState;
use super::VcpPositionModel;

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

    /// Unified VCP position model — the single source of truth for
    /// sweep timing, chunk positions, and volume progress.
    pub position: Option<VcpPositionModel>,

    /// Volume-level progress (present when streaming has started a volume).
    pub volume: Option<LiveVolumeModel>,

    /// Active sweep being received (present when an elevation is in progress).
    pub active_sweep: Option<LiveSweepModel>,

    /// Frame-snapshotted derivations from `position` at the same `now_secs`
    /// the model was built with. UI surfaces (e.g. the left panel) should
    /// read these instead of calling `position.estimated_azimuth_at(now)`
    /// with a fresh wall-clock read — that would drift by frame-render
    /// duration and break the per-frame consistency `compute_model` exists
    /// to provide.
    pub frame_now: FrameDerivedPosition,
}

/// Derived values that need to be evaluated at "now" for live mode but
/// must agree with the frame's canonical timestamp. Populated only when
/// `position` is `Some` and live mode is active; otherwise all fields
/// are `None`.
#[derive(Clone, Debug, Default)]
pub struct FrameDerivedPosition {
    /// 0-based index of the sweep currently being received.
    pub sweep_index: Option<usize>,
    /// Elevation angle (degrees) of the sweep currently being received.
    pub elevation_angle: Option<f32>,
    /// Volume-scan progress at frame `now` (0.0 to 1.0).
    pub progress: Option<f32>,
}

/// Volume-level state for the in-progress scan.
#[derive(Clone, Debug)]
pub struct LiveVolumeModel {
    /// VCP pattern for elevation angle lookups.
    pub vcp_pattern: Option<crate::data::keys::ExtractedVcp>,

    /// Combined expected/received view for the volume's elevations.
    /// Replaces parallel `elevations_complete` + `elevations_expected`
    /// fields so consumers read one place — see
    /// [`crate::state::VolumeElevationRoster`] for the divergence helpers.
    pub roster: crate::state::VolumeElevationRoster,
}

impl LiveVolumeModel {
    /// VCP-defined target angle for this elevation cut. Returns `None`
    /// if no VCP pattern is attached yet — callers fall back to the
    /// measured average from the decode result.
    pub fn target_elevation_angle(&self, elevation_number: u8) -> Option<f32> {
        self.vcp_pattern.as_ref().and_then(|vcp| {
            vcp.elevations
                .get(elevation_number.saturating_sub(1) as usize)
                .map(|e| e.angle)
        })
    }
}

/// Active sweep state: the elevation currently being collected.
#[derive(Clone, Debug)]
pub struct LiveSweepModel {
    /// Elevation number being collected.
    pub elevation_number: u8,

    /// Radials received so far for this elevation.
    #[allow(dead_code)]
    pub radials_received: u32,

    /// Azimuth range of actual received data (first_az, last_az).
    pub data_azimuth_range: Option<(f32, f32)>,

    /// Starting azimuth of the sweep (first radial).
    #[allow(dead_code)]
    pub sweep_start_azimuth: Option<f32>,

    /// Per-chunk azimuth boundaries within this sweep.
    pub chunks: Vec<LiveChunkBoundary>,

    /// Expected total chunks for this sweep (from VCP timing).
    #[allow(dead_code)]
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

        let position = VcpPositionModel::from_live(self, now_secs);
        let estimated_azimuth = position
            .as_ref()
            .and_then(|p| p.estimated_azimuth_at(now_secs));

        let volume = Some(LiveVolumeModel {
            vcp_pattern: self.current_vcp_pattern.clone(),
            roster: self.elevation_roster(),
        });

        let active_sweep = self.current_in_progress_elevation.map(|elev| {
            let current_elev = elev;
            let chunk_time_spans: Vec<(f64, f64, u32)> = self
                .chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == current_elev)
                .map(|&(_, start, end, radials)| (start, end, radials))
                .collect();

            // Derive chunks_expected from the position model if available.
            let chunks_expected = position.as_ref().and_then(|p| {
                p.sweeps
                    .iter()
                    .find(|s| s.elevation_number == elev)
                    .and_then(|s| match &s.status {
                        crate::state::SweepStatus::InProgress {
                            chunks_expected, ..
                        } => *chunks_expected,
                        _ => None,
                    })
            });

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
                chunks_expected,
                chunk_time_spans,
            }
        });

        let frame_now = FrameDerivedPosition {
            sweep_index: position
                .as_ref()
                .and_then(|p| p.elevation_index_at(now_secs)),
            elevation_angle: position.as_ref().and_then(|p| {
                p.elevation_index_at(now_secs)
                    .and_then(|idx| p.sweeps.get(idx).map(|s| s.elevation_angle))
            }),
            progress: position.as_ref().map(|p| p.progress_at(now_secs)),
        };

        LiveRadarModel {
            active,
            estimated_azimuth,
            position,
            volume,
            active_sweep,
            frame_now,
        }
    }
}
