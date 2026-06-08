//! Computed, read-only snapshot of live radar state.
//!
//! `LiveRadarModel` is derived once per frame from `LiveModeState`, capturing
//! the current wall-clock `now` so all UI consumers see a consistent picture
//! of what sweeps/chunks are in the past, which sweep is being received, and
//! what the radar is doing right now.

use super::LiveModeState;
use crate::nexrad::projection::{ExtrapolationState, ScanProjection};

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

    /// Unified scan projection — the single source of truth for sweep timing,
    /// chunk positions, and volume progress. Sourced from the shared engine.
    pub position: Option<ScanProjection>,

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
    pub fn compute_model(&self, now_secs: f64, position: Option<ScanProjection>) -> LiveRadarModel {
        let active = self.is_active();
        if !active {
            return LiveRadarModel::default();
        }

        // The engine emits the container with `extrapolation: None`; fill it per
        // frame from the live last-radial + the current sweep's resolved rate.
        let mut position = position;
        if let Some(ref mut p) = position {
            if let (Some(az), Some(t)) = (self.last_radial_azimuth, self.last_radial_time_secs) {
                let rate = p
                    .in_progress_elevation
                    .and_then(|e| p.sweeps.iter().find(|s| s.elevation_number == e))
                    .map(|s| s.azimuth_rate_dps)
                    .filter(|&r| r > 0.0)
                    .unwrap_or(20.0);
                p.extrapolation = Some(ExtrapolationState {
                    last_radial_azimuth: az,
                    last_radial_time: t,
                    degrees_per_sec: rate,
                });
            }
        }
        let estimated_azimuth = position
            .as_ref()
            .and_then(|p| p.estimated_azimuth_at(now_secs));

        // VCP pattern + roster come from the engine's projection (the single
        // owner); empty in the cold window before the first projection.
        let volume = Some(LiveVolumeModel {
            vcp_pattern: position.as_ref().and_then(|p| p.vcp_pattern.clone()),
            roster: position
                .as_ref()
                .map(|p| p.roster.clone())
                .unwrap_or_default(),
        });

        // The in-progress sweep is read entirely from the engine projection (the
        // in-progress `SweepProjection` carries the per-chunk az + time spans);
        // only the decoder-specific azimuth fields stay on `LiveModeState`.
        let active_sweep = position.as_ref().and_then(|p| {
            let elev = p.in_progress_elevation?;
            let sweep = p.sweeps.iter().find(|s| s.elevation_number == elev);
            let chunk_time_spans: Vec<(f64, f64, u32)> = sweep
                .map(|s| {
                    s.chunks
                        .iter()
                        .map(|c| (c.start, c.end, c.radial_count))
                        .collect()
                })
                .unwrap_or_default();
            let chunks: Vec<LiveChunkBoundary> = sweep
                .map(|s| {
                    s.chunks
                        .iter()
                        .map(|c| LiveChunkBoundary {
                            first_az: c.first_azimuth,
                            last_az: c.last_azimuth,
                            radial_count: c.radial_count,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let chunks_expected = sweep.map(|s| s.chunks_in_sweep as u32).filter(|&c| c > 0);
            Some(LiveSweepModel {
                elevation_number: elev,
                radials_received: p.in_progress_radials.unwrap_or(0),
                data_azimuth_range: self.live_data_azimuth_range,
                sweep_start_azimuth: self.sweep_start_azimuth,
                chunks,
                chunks_expected,
                chunk_time_spans,
            })
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
