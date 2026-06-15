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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use crate::nexrad::projection::{
        ChunkSpan, ProjectionScanRole, SweepProjection, SweepProjectionStatus,
        SweepTimingProvenance,
    };
    use crate::state::live_mode::LivePhase;

    // ── construction helpers (mirror the projection/mod.rs test idioms) ──

    fn sweep(elev: u8, start: f64, end: f64) -> SweepProjection {
        SweepProjection {
            elevation_number: elev,
            elevation_angle: 0.5 * elev as f32,
            scan_role: ProjectionScanRole::CurrentInProgress,
            status: SweepProjectionStatus::FutureExpected,
            timing: SweepTimingProvenance::Estimated,
            collection_start_secs: start,
            collection_end_secs: end,
            chunks_in_sweep: 0,
            chunks_received: 0,
            radials_received: 0,
            azimuth_rate_dps: 20.0,
            chunks: Vec::new(),
        }
    }

    fn scan(sweeps: Vec<SweepProjection>, vol_start: f64, vol_end: f64) -> ScanProjection {
        ScanProjection {
            vcp_number: 0,
            vcp_pattern: None,
            roster: crate::state::VolumeElevationRoster::default(),
            in_progress_elevation: None,
            in_progress_radials: None,
            volume_start: vol_start,
            volume_end: vol_end,
            sweeps,
            extrapolation: None,
            next_scan_ghost: None,
        }
    }

    /// An active (Streaming) live-mode state. `compute_model` returns a default
    /// model for any non-active state, so most behavior needs an active one.
    fn active_state() -> LiveModeState {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::Streaming;
        s
    }

    fn vcp_one_elev(angle: f32) -> crate::data::keys::ExtractedVcp {
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};
        ExtractedVcp {
            number: 215,
            elevations: vec![ExtractedVcpElevation {
                angle,
                waveform: "CS".to_string(),
                prf_number: 1,
                is_sails: false,
                is_mrle: false,
                is_base_tilt: false,
                azimuth_rate: Some(20.0),
            }],
        }
    }

    // ── compute_model: inactive short-circuit ──

    #[wasm_bindgen_test]
    fn compute_model_inactive_returns_default() {
        // Idle phase → not active → model is the all-default snapshot,
        // independent of the position passed in.
        let s = LiveModeState::new(); // phase defaults to Idle
        assert!(!s.is_active());
        let model = s.compute_model(
            1005.0,
            Some(scan(vec![sweep(1, 1000.0, 1010.0)], 1000.0, 1010.0)),
        );
        assert!(!model.active);
        assert!(model.position.is_none());
        assert!(model.volume.is_none());
        assert!(model.active_sweep.is_none());
        assert!(model.estimated_azimuth.is_none());
        assert!(model.frame_now.sweep_index.is_none());
        assert!(model.frame_now.elevation_angle.is_none());
        assert!(model.frame_now.progress.is_none());
    }

    #[wasm_bindgen_test]
    fn compute_model_error_phase_is_inactive() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::Error;
        assert!(!s.is_active());
        let model = s.compute_model(1005.0, None);
        assert!(!model.active);
    }

    // ── compute_model: active with no position ──

    #[wasm_bindgen_test]
    fn compute_model_active_no_position() {
        // Active but the engine hasn't emitted a projection yet (cold window).
        let s = active_state();
        let model = s.compute_model(1005.0, None);
        assert!(model.active);
        assert!(model.position.is_none());
        // estimated_azimuth comes from position → None here.
        assert!(model.estimated_azimuth.is_none());
        // volume is always Some when active; vcp/roster empty without a position.
        let volume = model.volume.expect("active volume");
        assert!(volume.vcp_pattern.is_none());
        assert_eq!(
            volume.roster,
            crate::state::VolumeElevationRoster::default()
        );
        // No active sweep without a position.
        assert!(model.active_sweep.is_none());
        // frame_now all None without a position.
        assert!(model.frame_now.sweep_index.is_none());
        assert!(model.frame_now.elevation_angle.is_none());
        assert!(model.frame_now.progress.is_none());
    }

    // ── compute_model: frame_now derivations from position ──

    #[wasm_bindgen_test]
    fn compute_model_frame_now_derivations() {
        // Two sweeps; now=1025 lands inside sweep index 1 (elev 2).
        let s = active_state();
        let sp = scan(
            vec![sweep(1, 1000.0, 1010.0), sweep(2, 1020.0, 1030.0)],
            1000.0,
            1030.0,
        );
        let model = s.compute_model(1025.0, Some(sp));
        assert!(model.active);
        assert!(model.position.is_some());
        // elevation_index_at(1025) → containment in sweep index 1.
        assert_eq!(model.frame_now.sweep_index, Some(1));
        // elevation_angle for sweep(2) is 0.5*2 = 1.0.
        let angle = model.frame_now.elevation_angle.expect("angle");
        assert!((angle - 1.0).abs() < 1e-4, "got {angle}");
        // progress_at(1025) over [1000,1030] = 25/30.
        let progress = model.frame_now.progress.expect("progress");
        assert!((progress - (25.0 / 30.0)).abs() < 1e-4, "got {progress}");
    }

    // ── compute_model: extrapolation filled from last radial, estimated azimuth ──

    #[wasm_bindgen_test]
    fn compute_model_fills_extrapolation_and_estimated_azimuth() {
        // No in-progress sweep so the rate falls back to the 20.0 dps default;
        // last radial at az=350 @ t=1000, now=1001 → 350 + 20*1 = 370 → wraps 10.
        let mut s = active_state();
        s.last_radial_azimuth = Some(350.0);
        s.last_radial_time_secs = Some(1000.0);
        let sp = scan(vec![sweep(1, 1000.0, 1010.0)], 1000.0, 1010.0);
        let model = s.compute_model(1001.0, Some(sp));

        // The model filled extrapolation onto the returned position.
        let pos = model.position.as_ref().expect("position");
        let ext = pos.extrapolation.as_ref().expect("extrapolation filled");
        assert!((ext.last_radial_azimuth - 350.0).abs() < 1e-4);
        assert!((ext.last_radial_time - 1000.0).abs() < 1e-6);
        assert!((ext.degrees_per_sec - 20.0).abs() < 1e-6, "default rate");

        // estimated_azimuth uses the extrapolation: 370 wrapped → 10.
        let az = model.estimated_azimuth.expect("estimated azimuth");
        assert!((az - 10.0).abs() < 1e-3, "got {az}");
    }

    #[wasm_bindgen_test]
    fn compute_model_extrapolation_uses_in_progress_sweep_rate() {
        // The in-progress sweep's azimuth_rate_dps (30.0) overrides the 20.0
        // default: az=0 @ t=1000, now=1001 → 0 + 30 = 30.
        let mut s = active_state();
        s.last_radial_azimuth = Some(0.0);
        s.last_radial_time_secs = Some(1000.0);
        let mut sw = sweep(1, 1000.0, 1010.0);
        sw.azimuth_rate_dps = 30.0;
        let mut sp = scan(vec![sw], 1000.0, 1010.0);
        sp.in_progress_elevation = Some(1);
        let model = s.compute_model(1001.0, Some(sp));

        let pos = model.position.as_ref().unwrap();
        let ext = pos.extrapolation.as_ref().unwrap();
        assert!(
            (ext.degrees_per_sec - 30.0).abs() < 1e-6,
            "in-progress rate used"
        );
        let az = model.estimated_azimuth.unwrap();
        assert!((az - 30.0).abs() < 1e-3, "got {az}");
    }

    // ── compute_model: active sweep built from the in-progress sweep projection ──

    #[wasm_bindgen_test]
    fn compute_model_active_sweep_built_from_in_progress() {
        // in_progress_elevation=2 selects sweep(2); chunks/time-spans are
        // copied from that sweep's ChunkSpan list.
        let s = active_state();
        let mut sw = sweep(2, 1000.0, 1010.0);
        sw.chunks_in_sweep = 5;
        sw.chunks = vec![
            ChunkSpan {
                start: 1000.0,
                end: 1002.0,
                first_azimuth: 10.0,
                last_azimuth: 40.0,
                radial_count: 30,
            },
            ChunkSpan {
                start: 1002.0,
                end: 1004.0,
                first_azimuth: 40.0,
                last_azimuth: 70.0,
                radial_count: 31,
            },
        ];
        let mut sp = scan(vec![sweep(1, 990.0, 998.0), sw], 990.0, 1010.0);
        sp.in_progress_elevation = Some(2);
        sp.in_progress_radials = Some(61);

        let model = s.compute_model(1001.0, Some(sp));
        let active_sweep = model.active_sweep.expect("active sweep");
        assert_eq!(active_sweep.elevation_number, 2);
        assert_eq!(active_sweep.radials_received, 61);
        // chunks_expected = chunks_in_sweep (5) as u32, kept because > 0.
        assert_eq!(active_sweep.chunks_expected, Some(5));
        // Two chunk boundaries copied through.
        assert_eq!(active_sweep.chunks.len(), 2);
        assert!((active_sweep.chunks[0].first_az - 10.0).abs() < 1e-4);
        assert!((active_sweep.chunks[1].last_az - 70.0).abs() < 1e-4);
        assert_eq!(active_sweep.chunks[1].radial_count, 31);
        // chunk_time_spans = (start, end, radial_count) tuples.
        assert_eq!(active_sweep.chunk_time_spans.len(), 2);
        let (s0, e0, c0) = active_sweep.chunk_time_spans[0];
        assert!((s0 - 1000.0).abs() < 1e-6 && (e0 - 1002.0).abs() < 1e-6);
        assert_eq!(c0, 30);
    }

    #[wasm_bindgen_test]
    fn compute_model_no_active_sweep_without_in_progress_elevation() {
        // in_progress_elevation = None → active_sweep is None even with sweeps.
        let s = active_state();
        let sp = scan(vec![sweep(1, 1000.0, 1010.0)], 1000.0, 1010.0); // in_progress None
        let model = s.compute_model(1005.0, Some(sp));
        assert!(model.active_sweep.is_none());
        // The volume is still populated.
        assert!(model.volume.is_some());
    }

    // ── LiveVolumeModel::target_elevation_angle ──

    #[wasm_bindgen_test]
    fn target_elevation_angle_lookup_and_bounds() {
        // 1-based elevation_number indexes (n-1) into vcp.elevations.
        let vm = LiveVolumeModel {
            vcp_pattern: Some(vcp_one_elev(0.5)),
            roster: crate::state::VolumeElevationRoster::default(),
        };
        // Elevation 1 → index 0 → angle 0.5.
        let a = vm.target_elevation_angle(1).expect("angle for elev 1");
        assert!((a - 0.5).abs() < 1e-4, "got {a}");
        // Elevation 2 → index 1 → out of range → None.
        assert!(vm.target_elevation_angle(2).is_none());
        // Elevation 0 → saturating_sub keeps index 0 → angle 0.5 (no underflow).
        let a0 = vm
            .target_elevation_angle(0)
            .expect("elev 0 saturates to index 0");
        assert!((a0 - 0.5).abs() < 1e-4);

        // No VCP attached → always None.
        let vm_none = LiveVolumeModel {
            vcp_pattern: None,
            roster: crate::state::VolumeElevationRoster::default(),
        };
        assert!(vm_none.target_elevation_angle(1).is_none());
    }
}
