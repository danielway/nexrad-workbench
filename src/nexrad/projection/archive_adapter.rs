//! Archive/cached scan → unified projection adapter.
//!
//! Cached and archived scans render ACTUALS, not projections — but consumers
//! read one universal type. This maps a cached [`Scan`] into a [`ScanProjection`]
//! where every cut is `CollectedByUs` / `Observed`, replacing the old
//! `VcpPositionModel::from_scan`.

use super::{
    ProjectionScanRole, ScanProjection, SweepProjection, SweepProjectionStatus,
    SweepTimingProvenance,
};
use crate::state::radar_data::Scan;

/// Build a [`ScanProjection`] from a cached/archived [`Scan`] — all sweeps
/// observed and collected-by-us, no projection/ghost/extrapolation.
pub fn scan_to_projection(scan: &Scan) -> ScanProjection {
    let sweeps = scan
        .sweeps
        .iter()
        .map(|s| SweepProjection {
            elevation_number: s.elevation_number,
            elevation_angle: s.elevation,
            scan_role: ProjectionScanRole::CurrentInProgress,
            status: SweepProjectionStatus::CollectedByUs,
            timing: SweepTimingProvenance::Observed,
            collection_start_secs: s.start_time,
            collection_end_secs: s.end_time,
            chunks_in_sweep: 0,
            chunks_received: 0,
            radials_received: 0,
            azimuth_rate_dps: 0.0,
            chunks: Vec::new(),
        })
        .collect();

    ScanProjection {
        vcp_number: scan.vcp,
        vcp_pattern: scan.vcp_pattern.clone(),
        roster: crate::state::VolumeElevationRoster::new(
            Some(scan.sweeps.len()),
            scan.sweeps.iter().map(|s| s.elevation_number).collect(),
        ),
        in_progress_elevation: None,
        in_progress_radials: None,
        volume_start: scan.start_time,
        volume_end: scan.end_time,
        sweeps,
        extrapolation: None,
        next_scan_ghost: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::radar_data::{Scan, Sweep};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep(elev: u8, start: f64, end: f64) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: elev as f32 * 0.5,
            elevation_number: elev,
            start_azimuth: 0.0,
            radials: vec![],
            cached_products: vec![],
        }
    }

    #[wasm_bindgen_test]
    fn maps_cached_scan_to_observed_collected_sweeps() {
        let scan = Scan {
            start_time: 1000.0,
            end_time: 1300.0,
            key_timestamp: 1000.0,
            vcp: 215,
            vcp_pattern: None,
            sweeps: vec![sweep(1, 1000.0, 1010.0), sweep(2, 1020.0, 1030.0)],
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        };
        let p = scan_to_projection(&scan);
        assert_eq!(p.vcp_number, 215);
        assert_eq!(p.volume_start, 1000.0);
        assert_eq!(p.volume_end, 1300.0);
        assert!(p.next_scan_ghost.is_none());
        assert!(p.extrapolation.is_none());
        assert_eq!(p.sweeps.len(), 2);
        for s in &p.sweeps {
            assert_eq!(s.status, SweepProjectionStatus::CollectedByUs);
            assert_eq!(s.timing, SweepTimingProvenance::Observed);
            assert!(s.is_complete() && s.is_observed());
        }
        assert_eq!(p.sweeps[1].collection_start_secs, 1020.0);
        assert_eq!(p.sweeps[1].collection_end_secs, 1030.0);
        assert_eq!(p.sweeps[1].elevation_angle, 1.0);
    }
}
