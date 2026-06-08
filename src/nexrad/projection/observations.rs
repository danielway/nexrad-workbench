//! The engine-owned accumulator for one in-progress volume's observations.
//!
//! Before this module the live volume's observed state (received elevations,
//! VCP pattern, in-progress cut, per-chunk spans, completed-sweep metadata)
//! lived on `LiveModeState`; the worker pipeline wrote it there and then read it
//! back each ingest to repack a derived snapshot for the [`super::ProjectionEngine`].
//! That made the engine a copy-holder rather than the owner.
//!
//! `VolumeObservations` is that accumulator, owned by the engine. The worker
//! feeds it directly (`record_*`), the engine's projection build reads it, and
//! diagnostics read it — one home for the data, reset at each volume boundary.
//! The derivations the worker used to compute (the `received` bitmap, the
//! elevation roster, the expected volume duration, the VCP-weighted fallback
//! durations) move here as methods so the single owner produces them.

use crate::data::keys::ExtractedVcp;
use crate::data::CachedSweep;
use crate::state::VolumeElevationRoster;

/// Default expected volume duration (seconds) when neither a completed volume
/// nor a VCP estimate is available — mirrors the worker's old `unwrap_or(300.0)`.
const DEFAULT_VOLUME_DURATION_SECS: f64 = 300.0;

/// Accumulated observations for the in-progress volume. Reset to `default()` at
/// each volume boundary via [`super::ProjectionEngine::reset_volume_observations`].
#[derive(Debug, Default, Clone)]
pub struct VolumeObservations {
    /// Elevation numbers received so far (sorted), from chunk radial headers.
    pub elevations_received: Vec<u8>,
    /// Total elevations claimed by the current VCP (message type 5).
    pub expected_elevation_count: Option<u8>,
    /// VCP number of the in-progress volume.
    pub current_vcp_number: Option<u16>,
    /// Full extracted VCP pattern (for elevation-angle lookups + display).
    pub current_vcp_pattern: Option<ExtractedVcp>,
    /// Elevation currently being accumulated (partial sweep).
    pub current_in_progress_elevation: Option<u8>,
    /// Radials received so far for the in-progress elevation.
    pub current_in_progress_radials: Option<u32>,
    /// Per-chunk azimuth ranges for the in-progress elevation
    /// `(first_az, last_az, radial_count)`. Cleared on elevation change.
    pub current_elev_chunks: Vec<(f32, f32, u32)>,
    /// Per-elevation chunk time spans for the whole volume
    /// `(elevation, start_secs, end_secs, radial_count)`.
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
    /// Actual sweep metadata for completed elevations (real timestamps).
    pub completed_sweep_metas: Vec<CachedSweep>,
    /// Duration of the most recently completed volume (seconds), fed by the
    /// worker from `LiveModeState.last_completed_volume`. Drives `expected_dur_secs`.
    pub last_volume_duration_secs: Option<f64>,
}

impl VolumeObservations {
    /// Reset to the empty state for a new volume.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    // ── Feed (moved verbatim from `LiveModeState`) ──

    /// Record newly-received elevation cuts (deduped, kept sorted).
    pub fn record_elevations(&mut self, elevations: &[u8]) {
        for &e in elevations {
            if !self.elevations_received.contains(&e) {
                self.elevations_received.push(e);
            }
        }
        self.elevations_received.sort_unstable();
    }

    /// Record VCP info from an ingest result.
    pub fn record_vcp(&mut self, vcp: &ExtractedVcp) {
        self.current_vcp_number = Some(vcp.number);
        self.expected_elevation_count = Some(vcp.elevations.len() as u8);
        if !vcp.elevations.is_empty() {
            self.current_vcp_pattern = Some(vcp.clone());
        }
    }

    /// Record which elevation is being accumulated. Clears the per-chunk
    /// azimuth list on an elevation change and returns whether it changed so the
    /// caller can reset the decoder-side `sweep_start_azimuth` on `LiveModeState`.
    pub fn record_in_progress_elevation(
        &mut self,
        elevation: Option<u8>,
        radials: Option<u32>,
    ) -> bool {
        let changed = elevation != self.current_in_progress_elevation;
        if changed {
            self.current_elev_chunks.clear();
        }
        self.current_in_progress_elevation = elevation;
        self.current_in_progress_radials = radials;
        changed
    }

    /// Append a chunk's per-elevation time spans.
    pub fn record_chunk_elev_spans(&mut self, spans: &[(u8, f64, f64, u32)]) {
        self.chunk_elev_spans.extend_from_slice(spans);
    }

    /// Append a per-chunk azimuth range for the in-progress elevation.
    pub fn push_elev_chunk(&mut self, chunk: (f32, f32, u32)) {
        self.current_elev_chunks.push(chunk);
    }

    /// Replace the completed-sweep metadata (the worker returns the full list).
    pub fn update_sweep_metas(&mut self, metas: Vec<CachedSweep>) {
        self.completed_sweep_metas = metas;
    }

    /// Set the most-recently-completed volume's duration (worker-fed).
    pub fn set_last_volume_duration_secs(&mut self, secs: Option<f64>) {
        self.last_volume_duration_secs = secs;
    }

    // ── Derivations (moved from the worker's repack) ──

    /// Combined expected-vs-received elevation roster.
    pub fn elevation_roster(&self) -> VolumeElevationRoster {
        VolumeElevationRoster::new(
            self.expected_elevation_count.map(|c| c as usize),
            self.elevations_received.clone(),
        )
    }

    /// `received[i]` — elevation `i + 1` is received — over the full roster.
    pub fn received_vec(&self) -> Vec<bool> {
        let roster = self.elevation_roster();
        let n = roster.expected_count().unwrap_or(0);
        (0..n).map(|i| roster.is_received((i + 1) as u8)).collect()
    }

    /// Full elevation roster size.
    pub fn expected_count(&self) -> usize {
        self.expected_elevation_count
            .map(|c| c as usize)
            .unwrap_or(0)
    }

    /// Expected in-progress volume duration: the last completed volume's span,
    /// else the VCP's own estimate, else a fixed default.
    pub fn expected_dur_secs(&self) -> f64 {
        self.last_volume_duration_secs
            .filter(|&d| d > 0.0 && d < 1200.0)
            .or_else(|| {
                self.current_vcp_pattern
                    .as_ref()
                    .and_then(|v| v.estimated_volume_duration())
            })
            .unwrap_or(DEFAULT_VOLUME_DURATION_SECS)
    }

    /// Per-elevation sweep durations from the VCP, returned only when no library
    /// projection is available (`plan_available == false`) — the library physics
    /// model is preferred when a plan exists. Empty when no VCP / a plan exists.
    pub fn fallback_sweep_durations(&self, plan_available: bool) -> Vec<f64> {
        if plan_available {
            return Vec::new();
        }
        let Some(vcp) = self.current_vcp_pattern.as_ref() else {
            return Vec::new();
        };
        if vcp.elevations.is_empty() {
            return Vec::new();
        }
        vcp.sweep_durations(self.expected_dur_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn roster_and_received_vec_track_recorded_elevations() {
        let mut obs = VolumeObservations::default();
        obs.expected_elevation_count = Some(3);
        obs.record_elevations(&[3, 1]);
        let roster = obs.elevation_roster();
        assert_eq!(roster.expected_count(), Some(3));
        assert_eq!(roster.received, vec![1, 3]);
        // received_vec is over the full 1..=expected roster.
        assert_eq!(obs.received_vec(), vec![true, false, true]);
        assert_eq!(obs.expected_count(), 3);
    }

    #[wasm_bindgen_test]
    fn record_in_progress_elevation_reports_and_clears_on_change() {
        let mut obs = VolumeObservations::default();
        obs.push_elev_chunk((0.0, 90.0, 50));
        // First set (None -> 1) is a change; clears the chunk list.
        assert!(obs.record_in_progress_elevation(Some(1), Some(50)));
        assert!(obs.current_elev_chunks.is_empty());
        obs.push_elev_chunk((0.0, 90.0, 50));
        // Same elevation -> not a change; chunk list preserved.
        assert!(!obs.record_in_progress_elevation(Some(1), Some(120)));
        assert_eq!(obs.current_elev_chunks.len(), 1);
        assert_eq!(obs.current_in_progress_radials, Some(120));
        // New elevation -> change; clears.
        assert!(obs.record_in_progress_elevation(Some(2), Some(0)));
        assert!(obs.current_elev_chunks.is_empty());
    }

    #[wasm_bindgen_test]
    fn expected_dur_prefers_completed_volume_then_default() {
        let mut obs = VolumeObservations::default();
        // No data -> default.
        assert_eq!(obs.expected_dur_secs(), DEFAULT_VOLUME_DURATION_SECS);
        // A plausible completed-volume duration wins.
        obs.set_last_volume_duration_secs(Some(280.0));
        assert_eq!(obs.expected_dur_secs(), 280.0);
        // Out-of-range durations are ignored (fall through to default here).
        obs.set_last_volume_duration_secs(Some(5000.0));
        assert_eq!(obs.expected_dur_secs(), DEFAULT_VOLUME_DURATION_SECS);
    }

    #[wasm_bindgen_test]
    fn fallback_durations_empty_when_plan_available_or_no_vcp() {
        let obs = VolumeObservations::default();
        // No VCP -> empty regardless.
        assert!(obs.fallback_sweep_durations(false).is_empty());
        assert!(obs.fallback_sweep_durations(true).is_empty());
    }

    #[wasm_bindgen_test]
    fn reset_clears_everything() {
        let mut obs = VolumeObservations::default();
        obs.record_elevations(&[1, 2]);
        obs.record_in_progress_elevation(Some(2), Some(10));
        obs.reset();
        assert!(obs.elevations_received.is_empty());
        assert!(obs.current_in_progress_elevation.is_none());
    }
}
