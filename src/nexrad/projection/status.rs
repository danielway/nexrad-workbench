//! Per-sweep status derivation and the sweep-projection builder.
//!
//! Turns the projector's per-chunk forecasts plus the engine's other inputs
//! (cached sweeps, known-available inventory, in-progress elevation) into the
//! per-sweep [`SweepProjection`] list the unified `Projection` emits — for the
//! current in-progress scan and the projected next scan, on both the collection
//! and availability axes.

use super::cached_sweeps::CachedSweepSet;
use super::inventory::{ChunkCoord, KnownChunkInventory};
use super::{ProjectionScanRole, SweepProjection, SweepProjectionStatus, SweepTimingProvenance};
use crate::nexrad::ChunkProjectionInfo;
use nexrad_data::aws::realtime::VolumeIndex;
use std::collections::HashMap;

/// Status of one sweep, given what we have cached, what S3 has published, and
/// what's currently being received. Pure.
///
/// Precedence: cached locally → currently receiving → published-but-not-ours →
/// purely future.
#[allow(clippy::too_many_arguments)]
pub fn derive_sweep_status(
    scan_start_secs: f64,
    elevation_number: u8,
    volume: VolumeIndex,
    last_seq_of_sweep: usize,
    cached: &CachedSweepSet,
    inventory: &KnownChunkInventory,
    in_progress_elevation: Option<u8>,
) -> SweepProjectionStatus {
    if cached.has(scan_start_secs, elevation_number) {
        return SweepProjectionStatus::CollectedByUs;
    }
    if in_progress_elevation == Some(elevation_number) {
        return SweepProjectionStatus::InProgress;
    }
    // Published if the sweep's final chunk (or a later sequence) is known, or
    // the volume's End chunk has appeared. Same presence rule the streaming
    // probe uses for early-fire.
    let published = inventory.has_end(volume)
        || inventory
            .newest_seq_in(volume)
            .is_some_and(|s| s >= last_seq_of_sweep)
        || inventory.contains(ChunkCoord {
            volume,
            sequence: last_seq_of_sweep,
        });
    if published {
        SweepProjectionStatus::AvailableNotCollected
    } else {
        SweepProjectionStatus::FutureExpected
    }
}

/// Inputs to [`build_sweeps`], bundled to keep the signature manageable.
pub struct SweepBuildCtx<'a> {
    /// Every chunk of the current volume (past chunks carry `forecast: None`).
    pub current_chunks: &'a [ChunkProjectionInfo],
    /// Every chunk of the next volume, when the projection extends into it.
    pub next_chunks: Option<&'a [ChunkProjectionInfo]>,
    /// Whole-second scan-start of the current volume (cache + status key).
    pub current_scan_start_secs: f64,
    /// Whole-second scan-start of the next volume, when known.
    pub next_scan_start_secs: Option<f64>,
    pub current_volume: VolumeIndex,
    pub next_volume: VolumeIndex,
    pub cached: &'a CachedSweepSet,
    pub inventory: &'a KnownChunkInventory,
    /// Elevation currently being received (drives `InProgress`); current scan
    /// only.
    pub in_progress_elevation: Option<u8>,
    /// Authoritative next-scan start (e.g. archive `ScanBoundary.end`); when
    /// present, next-scan sweep times are shifted so the scan begins here.
    pub next_scan_boundary_start_secs: Option<f64>,
}

/// Aggregated timing for one elevation's forecast-bearing chunks.
struct Agg {
    collection_start: f64,
    collection_end: f64,
    available_at: f64,
    chunks_in_sweep: usize,
    azimuth_rate: f64,
    min_seq: usize,
}

/// Group a volume's chunks into per-elevation forecast aggregates, ordered by
/// first appearance (sequence). Only chunks carrying a forecast contribute to
/// timing; `last_seq_of` covers every chunk so status sees the true final
/// sequence even for partially-collected sweeps.
fn group_sweeps(chunks: &[ChunkProjectionInfo]) -> Vec<(u8, Agg)> {
    let mut aggs: HashMap<u8, Agg> = HashMap::new();
    for c in chunks {
        let Some(elev) = c.elevation_number else {
            continue;
        };
        let elev = elev as u8;
        let Some(f) = c.forecast.as_ref() else {
            continue;
        };
        aggs.entry(elev)
            .and_modify(|a| {
                a.collection_start = a.collection_start.min(f.collection_time_secs);
                a.collection_end = a.collection_end.max(f.collection_time_secs);
                a.available_at = a.available_at.max(f.available_at_secs);
                a.min_seq = a.min_seq.min(c.sequence);
            })
            .or_insert(Agg {
                collection_start: f.collection_time_secs,
                collection_end: f.collection_time_secs,
                available_at: f.available_at_secs,
                chunks_in_sweep: c.chunks_in_sweep,
                azimuth_rate: c.azimuth_rate_dps,
                min_seq: c.sequence,
            });
    }
    let mut out: Vec<(u8, Agg)> = aggs.into_iter().collect();
    out.sort_by_key(|(_, a)| a.min_seq);
    out
}

/// Final (max) sequence per elevation across every chunk of a volume.
fn last_seq_by_elev(chunks: &[ChunkProjectionInfo]) -> HashMap<u8, usize> {
    let mut last: HashMap<u8, usize> = HashMap::new();
    for c in chunks {
        if let Some(elev) = c.elevation_number {
            let e = last.entry(elev as u8).or_insert(0);
            *e = (*e).max(c.sequence);
        }
    }
    last
}

/// Build the per-sweep projection for the current + next scan.
pub fn build_sweeps(ctx: &SweepBuildCtx) -> Vec<SweepProjection> {
    let mut sweeps = Vec::new();

    // ── Current scan ──
    let current_last_seq = last_seq_by_elev(ctx.current_chunks);
    let mut current_elevs_built: Vec<u8> = Vec::new();
    for (elev, agg) in group_sweeps(ctx.current_chunks) {
        let last_seq = current_last_seq.get(&elev).copied().unwrap_or(0);
        let status = derive_sweep_status(
            ctx.current_scan_start_secs,
            elev,
            ctx.current_volume,
            last_seq,
            ctx.cached,
            ctx.inventory,
            ctx.in_progress_elevation,
        );
        current_elevs_built.push(elev);
        sweeps.push(SweepProjection {
            elevation_number: elev,
            elevation_angle: 0.0,
            scan_role: ProjectionScanRole::CurrentInProgress,
            status,
            timing: SweepTimingProvenance::Projected,
            collection_start_secs: agg.collection_start,
            collection_end_secs: agg.collection_end,
            available_at_secs: agg.available_at,
            chunks_in_sweep: agg.chunks_in_sweep,
            chunks_received: 0,
            radials_received: 0,
            azimuth_rate_dps: agg.azimuth_rate,
            chunks: Vec::new(),
        });
    }
    // Append already-collected cuts that are behind the anchor (no forecast
    // chunks) so the display view is complete. These are `CollectedByUs`.
    for (elev, start, end) in ctx.cached.cuts_for_scan(ctx.current_scan_start_secs) {
        if !current_elevs_built.contains(&elev) {
            sweeps.push(SweepProjection {
                elevation_number: elev,
                elevation_angle: 0.0,
                scan_role: ProjectionScanRole::CurrentInProgress,
                status: SweepProjectionStatus::CollectedByUs,
                timing: SweepTimingProvenance::Observed,
                collection_start_secs: start,
                collection_end_secs: end,
                available_at_secs: end,
                chunks_in_sweep: 0,
                chunks_received: 0,
                radials_received: 0,
                azimuth_rate_dps: 0.0,
                chunks: Vec::new(),
            });
        }
    }

    // ── Next scan ──
    if let Some(next_chunks) = ctx.next_chunks {
        let next_groups = group_sweeps(next_chunks);
        let next_last_seq = last_seq_by_elev(next_chunks);
        // Clamp: shift all next-scan times so the scan begins at the
        // authoritative boundary, when one is supplied.
        let projected_start = next_groups.first().map(|(_, a)| a.collection_start);
        let delta = match (ctx.next_scan_boundary_start_secs, projected_start) {
            (Some(boundary), Some(proj)) => boundary - proj,
            _ => 0.0,
        };
        let next_scan_start = ctx
            .next_scan_start_secs
            .or(projected_start.map(|p| p + delta))
            .unwrap_or(0.0);
        for (elev, agg) in next_groups {
            let last_seq = next_last_seq.get(&elev).copied().unwrap_or(0);
            let status = derive_sweep_status(
                next_scan_start,
                elev,
                ctx.next_volume,
                last_seq,
                ctx.cached,
                ctx.inventory,
                None, // in-progress applies to the current scan only
            );
            sweeps.push(SweepProjection {
                elevation_number: elev,
                elevation_angle: 0.0,
                scan_role: ProjectionScanRole::NextScan,
                status,
                timing: SweepTimingProvenance::Projected,
                collection_start_secs: agg.collection_start + delta,
                collection_end_secs: agg.collection_end + delta,
                available_at_secs: agg.available_at + delta,
                chunks_in_sweep: agg.chunks_in_sweep,
                chunks_received: 0,
                radials_received: 0,
                azimuth_rate_dps: agg.azimuth_rate,
                chunks: Vec::new(),
            });
        }
    }

    sweeps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nexrad::realtime::ChunkForecast;
    use crate::nexrad::timing::{IntervalCase, PhysicsBreakdown, SchedulerPath};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn vol(n: usize) -> VolumeIndex {
        VolumeIndex::new(n)
    }

    fn forecast(collection: f64, available: f64) -> ChunkForecast {
        ChunkForecast {
            collection_time_secs: collection,
            available_at_secs: available,
            poll_at_secs: available + 1.0,
            physics_breakdown: PhysicsBreakdown {
                case: IntervalCase::IntraSweep,
                total_secs: 0.0,
                chunk_duration_secs: None,
                inter_sweep_gap_secs: None,
                waveform_penalty_secs: None,
            },
            stats_n: 0,
            scheduler_path: SchedulerPath::Physics,
            bucket: None,
        }
    }

    fn chunk(seq: usize, elev: Option<usize>, fc: Option<ChunkForecast>) -> ChunkProjectionInfo {
        ChunkProjectionInfo {
            sequence: seq,
            elevation_number: elev,
            azimuth_rate_dps: 20.0,
            chunk_index_in_sweep: 0,
            chunks_in_sweep: 3,
            forecast: fc,
        }
    }

    fn empty_inventory() -> KnownChunkInventory {
        KnownChunkInventory::default()
    }

    #[wasm_bindgen_test]
    fn status_precedence_cached_beats_everything() {
        let mut cached = CachedSweepSet::default();
        cached.set_for_scan(
            1000.0,
            &[crate::data::CachedSweep {
                start: 1000.0,
                end: 1010.0,
                elevation: 1.0,
                elevation_number: 1,
                start_azimuth: 0.0,
                cached_products: vec![],
            }],
        );
        let mut inv = empty_inventory();
        inv.observe(super::super::inventory::KnownChunk {
            coord: ChunkCoord {
                volume: vol(1),
                sequence: 3,
            },
            upload_secs: 100.0,
            chunk_type: nexrad_data::aws::realtime::ChunkType::Intermediate,
        });
        // Cached wins even though it's also "in progress" and published.
        let status = derive_sweep_status(1000.0, 1, vol(1), 3, &cached, &inv, Some(1));
        assert_eq!(status, SweepProjectionStatus::CollectedByUs);
    }

    #[wasm_bindgen_test]
    fn status_in_progress_then_available_then_future() {
        let cached = CachedSweepSet::default();
        let mut inv = empty_inventory();
        // Elevation 2's last chunk (seq 5) is published.
        inv.observe(super::super::inventory::KnownChunk {
            coord: ChunkCoord {
                volume: vol(1),
                sequence: 5,
            },
            upload_secs: 100.0,
            chunk_type: nexrad_data::aws::realtime::ChunkType::Intermediate,
        });
        // In progress.
        assert_eq!(
            derive_sweep_status(1000.0, 9, vol(1), 30, &cached, &inv, Some(9)),
            SweepProjectionStatus::InProgress
        );
        // Available (seq 5 known >= last_seq 5).
        assert_eq!(
            derive_sweep_status(1000.0, 2, vol(1), 5, &cached, &inv, None),
            SweepProjectionStatus::AvailableNotCollected
        );
        // Future (last_seq 30 not yet published, not in progress).
        assert_eq!(
            derive_sweep_status(1000.0, 7, vol(1), 30, &cached, &inv, None),
            SweepProjectionStatus::FutureExpected
        );
    }

    #[wasm_bindgen_test]
    fn build_sweeps_current_and_next_with_sparse_cached() {
        // Current volume: elevations 1 (cached, no forecast), 2 (future), 3 (future).
        let current = vec![
            chunk(1, None, None), // Start
            chunk(2, Some(2), Some(forecast(1020.0, 1025.0))),
            chunk(3, Some(2), Some(forecast(1021.0, 1026.0))),
            chunk(4, Some(3), Some(forecast(1030.0, 1035.0))),
        ];
        let next = vec![
            chunk(2, Some(1), Some(forecast(1100.0, 1105.0))),
            chunk(3, Some(2), Some(forecast(1110.0, 1115.0))),
        ];
        let mut cached = CachedSweepSet::default();
        cached.set_for_scan(
            1000.0,
            &[crate::data::CachedSweep {
                start: 1000.0,
                end: 1010.0,
                elevation: 1.0,
                elevation_number: 1,
                start_azimuth: 0.0,
                cached_products: vec![],
            }],
        );
        let inv = empty_inventory();
        let ctx = SweepBuildCtx {
            current_chunks: &current,
            next_chunks: Some(&next),
            current_scan_start_secs: 1000.0,
            next_scan_start_secs: Some(1100.0),
            current_volume: vol(1),
            next_volume: vol(2),
            cached: &cached,
            inventory: &inv,
            in_progress_elevation: Some(2),
            next_scan_boundary_start_secs: None,
        };
        let sweeps = build_sweeps(&ctx);

        // Current: elev 2 (in progress), elev 3 (future), elev 1 (cached, appended).
        let current_sweeps: Vec<_> = sweeps
            .iter()
            .filter(|s| s.scan_role == ProjectionScanRole::CurrentInProgress)
            .collect();
        assert_eq!(current_sweeps.len(), 3);
        let elev2 = current_sweeps
            .iter()
            .find(|s| s.elevation_number == 2)
            .unwrap();
        assert_eq!(elev2.status, SweepProjectionStatus::InProgress);
        assert_eq!(elev2.collection_start_secs, 1020.0);
        assert_eq!(elev2.collection_end_secs, 1021.0);
        let elev1 = current_sweeps
            .iter()
            .find(|s| s.elevation_number == 1)
            .unwrap();
        assert_eq!(elev1.status, SweepProjectionStatus::CollectedByUs);
        assert_eq!(elev1.collection_start_secs, 1000.0);

        // Next scan: two future sweeps.
        let next_sweeps: Vec<_> = sweeps
            .iter()
            .filter(|s| s.scan_role == ProjectionScanRole::NextScan)
            .collect();
        assert_eq!(next_sweeps.len(), 2);
        assert!(next_sweeps
            .iter()
            .all(|s| s.status == SweepProjectionStatus::FutureExpected));
    }

    #[wasm_bindgen_test]
    fn acquisition_excludes_cached_but_display_includes_it() {
        let current = vec![chunk(2, Some(2), Some(forecast(1020.0, 1025.0)))];
        let mut cached = CachedSweepSet::default();
        cached.set_for_scan(
            1000.0,
            &[crate::data::CachedSweep {
                start: 1000.0,
                end: 1010.0,
                elevation: 1.0,
                elevation_number: 1,
                start_azimuth: 0.0,
                cached_products: vec![],
            }],
        );
        let inv = empty_inventory();
        let ctx = SweepBuildCtx {
            current_chunks: &current,
            next_chunks: None,
            current_scan_start_secs: 1000.0,
            next_scan_start_secs: None,
            current_volume: vol(1),
            next_volume: vol(2),
            cached: &cached,
            inventory: &inv,
            in_progress_elevation: None,
            next_scan_boundary_start_secs: None,
        };
        let sweeps = build_sweeps(&ctx);
        // Display has both elev 1 (cached) and elev 2.
        assert_eq!(sweeps.len(), 2);
        // Acquisition filtering drops the cached one.
        let acq: Vec<_> = sweeps
            .iter()
            .filter(|s| s.status != SweepProjectionStatus::CollectedByUs)
            .collect();
        assert_eq!(acq.len(), 1);
        assert_eq!(acq[0].elevation_number, 2);
    }

    #[wasm_bindgen_test]
    fn next_scan_times_shift_to_boundary() {
        let current = vec![chunk(2, Some(1), Some(forecast(1020.0, 1025.0)))];
        let next = vec![chunk(2, Some(1), Some(forecast(1100.0, 1105.0)))];
        let cached = CachedSweepSet::default();
        let inv = empty_inventory();
        let ctx = SweepBuildCtx {
            current_chunks: &current,
            next_chunks: Some(&next),
            current_scan_start_secs: 1000.0,
            next_scan_start_secs: None,
            current_volume: vol(1),
            next_volume: vol(2),
            cached: &cached,
            inventory: &inv,
            in_progress_elevation: None,
            // Authoritative next-scan start is 1090, projection said 1100 → −10s.
            next_scan_boundary_start_secs: Some(1090.0),
        };
        let sweeps = build_sweeps(&ctx);
        let next_sweep = sweeps
            .iter()
            .find(|s| s.scan_role == ProjectionScanRole::NextScan)
            .unwrap();
        assert_eq!(next_sweep.collection_start_secs, 1090.0);
        assert_eq!(next_sweep.available_at_secs, 1095.0);
    }
}
