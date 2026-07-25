//! Per-sweep status derivation and the sweep-projection builder.
//!
//! Turns the projector's per-chunk forecasts plus the engine's other inputs
//! (cached sweeps, known-available inventory, in-progress elevation) into the
//! per-sweep [`SweepProjection`] list the unified `Projection` emits — for the
//! current in-progress scan and the projected next scan, on both the collection
//! and availability axes.

use super::cached_sweeps::CachedSweepSet;
use super::inventory::{ChunkCoord, KnownChunkInventory};
use super::{
    ChunkSpan, ProjectionScanRole, SweepProjection, SweepProjectionStatus, SweepTimingProvenance,
};
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
    if published_in_inventory(inventory, volume, last_seq_of_sweep) {
        SweepProjectionStatus::AvailableNotCollected
    } else {
        SweepProjectionStatus::FutureExpected
    }
}

/// Whether a sweep's final chunk (or a later sequence / the End chunk) is known
/// to be published in S3 per the inventory — the presence rule shared by the
/// status derivation and the streaming probe's early-fire.
pub fn published_in_inventory(
    inventory: &KnownChunkInventory,
    volume: VolumeIndex,
    last_seq_of_sweep: usize,
) -> bool {
    inventory.has_end(volume)
        || inventory
            .newest_seq_in(volume)
            .is_some_and(|s| s >= last_seq_of_sweep)
        || inventory.contains(ChunkCoord {
            volume,
            sequence: last_seq_of_sweep,
        })
}

/// Inputs to [`build_sweeps`], bundled to keep the signature manageable.
pub struct SweepBuildCtx<'a> {
    /// Every chunk of the current volume (past chunks carry `projected: None`).
    pub current_chunks: &'a [ChunkProjectionInfo],
    /// Every chunk of the next volume, when the projection extends into it.
    pub next_chunks: Option<&'a [ChunkProjectionInfo]>,
    /// Whole-second scan-start of the current volume (cache + status key).
    pub current_scan_start_secs: f64,
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
    // ── Current-scan cascade inputs (feed `cascade_current_sweeps`) ──
    /// Full elevation roster size for the current volume.
    pub expected_count: usize,
    /// `received[elev_idx]` — elevation `elev_idx + 1` fully received.
    pub received: &'a [bool],
    pub vcp_number: u16,
    pub vcp_pattern: Option<&'a crate::data::keys::ExtractedVcp>,
    pub vol_start_secs: f64,
    pub expected_dur_secs: f64,
    pub completed_sweep_metas: &'a [crate::data::CachedSweep],
    pub chunk_elev_spans: &'a [(u8, f64, f64, u32)],
    pub current_elev_chunks: &'a [(f32, f32, u32)],
    pub in_progress_radials: Option<u32>,
    pub fallback_sweep_durations: &'a [f64],
}

/// Aggregated timing for one elevation's forecast-bearing chunks.
struct Agg {
    collection_start: f64,
    collection_end: f64,
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
        let Some(f) = c.projected.as_ref() else {
            continue;
        };
        aggs.entry(elev)
            .and_modify(|a| {
                a.collection_start = a.collection_start.min(f.collection_time_secs);
                a.collection_end = a.collection_end.max(f.collection_time_secs);
                a.min_seq = a.min_seq.min(c.sequence);
            })
            .or_insert(Agg {
                collection_start: f.collection_time_secs,
                collection_end: f.collection_time_secs,
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

/// Inputs to the current-scan bounds cascade. Borrowed slices so both the
/// engine (`build_sweeps`) and the `build_position_from_live` oracle can call
/// the SAME cascade — guaranteeing the per-sweep bounds can't diverge.
pub struct CascadeInputs<'a> {
    pub vol_start: f64,
    pub expected_count: usize,
    /// `received[elev_idx]` — elevation `elev_idx + 1` is fully received.
    pub received: &'a [bool],
    pub vcp_number: u16,
    pub vcp_pattern: Option<&'a crate::data::keys::ExtractedVcp>,
    pub expected_dur: f64,
    pub current_volume_chunks: &'a [ChunkProjectionInfo],
    pub completed_sweep_metas: &'a [crate::data::CachedSweep],
    pub chunk_elev_spans: &'a [(u8, f64, f64, u32)],
    pub current_elev_chunks: &'a [(f32, f32, u32)],
    pub in_progress_elevation: Option<u8>,
    pub in_progress_radials: Option<u32>,
    pub fallback_sweep_durations: &'a [f64],
}

/// One sweep's derived bounds + provenance + in-progress detail — the shared
/// output of the cascade, mapped by each caller to its own render type.
pub struct SweepBounds {
    pub elevation_number: u8,
    pub elevation_angle: f32,
    pub start: f64,
    pub end: f64,
    pub timing: SweepTimingProvenance,
    /// Bound-derivation flags (received roster / in-progress). Orthogonal to
    /// the acquisition `status`; asserted by the cascade golden test, not read
    /// in production.
    #[allow(dead_code)]
    pub is_complete: bool,
    #[allow(dead_code)]
    pub is_in_progress: bool,
    pub radials_received: u32,
    pub chunks_received: u32,
    pub chunks_expected: Option<u32>,
    /// Resolved azimuth rate (projected if >0, else 360/dur, else 20) — used
    /// for the sweep-line extrapolation and per-sweep display.
    pub azimuth_rate_dps: f64,
    pub chunks: Vec<ChunkSpan>,
}

/// The sweep-positioning cascade (Observed > Anchored > Projected > Estimated)
/// for the current scan, as a pure function. Centralized here so the engine and
/// the legacy `from_live` oracle share one implementation. Faithful transcription
/// of the previous `from_live` body.
pub fn cascade_current_sweeps(inp: &CascadeInputs) -> Vec<SweepBounds> {
    let vol_start = inp.vol_start;
    let expected_count = inp.expected_count;
    let expected_dur = inp.expected_dur;

    // Projected sweep bounds from the library projections (COLLECTION times).
    let mut projected_sweeps: std::collections::BTreeMap<u8, (f64, f64, f64, u32)> =
        std::collections::BTreeMap::new();
    for chunk in inp.current_volume_chunks {
        if let Some(elev) = chunk.elevation_number {
            let entry = projected_sweeps.entry(elev as u8).or_insert((
                f64::MAX,
                f64::MIN,
                chunk.azimuth_rate_dps,
                0,
            ));
            entry.3 += 1;
            if let Some(t) = chunk.projected.as_ref().map(|f| f.collection_time_secs) {
                entry.0 = entry.0.min(t);
                entry.1 = entry.1.max(t);
            }
        }
    }

    let fallback_durs = inp.fallback_sweep_durations;
    let weighted_durations: Vec<f64> = if !fallback_durs.is_empty() {
        let total_weight: f64 = fallback_durs.iter().sum();
        if total_weight > 0.0 {
            fallback_durs
                .iter()
                .map(|d| (d / total_weight) * expected_dur)
                .collect()
        } else {
            vec![expected_dur / expected_count as f64; expected_count]
        }
    } else {
        vec![expected_dur / expected_count as f64; expected_count]
    };
    let weighted_offsets: Vec<f64> = {
        let mut offsets = Vec::with_capacity(expected_count);
        let mut cum = 0.0;
        for dur in &weighted_durations {
            offsets.push(cum);
            cum += dur;
        }
        offsets
    };

    let vcp_def = crate::data::vcp::get_vcp_definition(inp.vcp_number);
    let elev_angle_for = |elev_num: u8| -> f32 {
        if let Some(vcp) = inp.vcp_pattern {
            if let Some(e) = vcp.elevations.get(elev_num.saturating_sub(1) as usize) {
                return e.angle;
            }
        }
        vcp_def
            .and_then(|d| d.elevations.get(elev_num.saturating_sub(1) as usize))
            .map(|e| e.angle)
            .unwrap_or(0.5 * elev_num as f32)
    };

    let mut out = Vec::with_capacity(expected_count);
    for elev_idx in 0..expected_count {
        let elev_num = (elev_idx + 1) as u8;
        let is_complete = inp.received.get(elev_idx).copied().unwrap_or(false);
        let is_in_progress = !is_complete && inp.in_progress_elevation == Some(elev_num);
        let this_sweep_dur = weighted_durations[elev_idx];
        // (min_time, max_time, azimuth_rate, chunk_count)
        let proj = projected_sweeps.get(&elev_num).copied();

        let (start, end, timing) = if is_complete {
            if let Some(meta) = inp
                .completed_sweep_metas
                .iter()
                .find(|m| m.elevation_number == elev_num)
            {
                (meta.start, meta.end, SweepTimingProvenance::Observed)
            } else {
                let offset = weighted_offsets[elev_idx];
                (
                    vol_start + offset,
                    vol_start + offset + this_sweep_dur,
                    SweepTimingProvenance::Estimated,
                )
            }
        } else {
            let chunk_min = inp
                .chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == elev_num)
                .map(|&(_, s, _, _)| s)
                .reduce(f64::min);
            let chunk_max = inp
                .chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == elev_num)
                .map(|&(_, _, e, _)| e)
                .reduce(f64::max);
            if let Some(cm) = chunk_min {
                let sw_end_actual = match chunk_max {
                    Some(cmax) => {
                        let proj_end = proj.filter(|p| p.1 > f64::MIN).map(|p| p.1);
                        match proj_end {
                            Some(pe) => cmax.max(pe),
                            None => cmax.max(cm + this_sweep_dur),
                        }
                    }
                    None => cm + this_sweep_dur,
                };
                (cm, sw_end_actual, SweepTimingProvenance::Anchored)
            } else if let Some(p) = proj.filter(|p| p.0 < f64::MAX) {
                let proj_start = p.0;
                let proj_end = if p.1 > f64::MIN {
                    p.1
                } else {
                    let rate = p.2;
                    let dur = if rate > 0.0 {
                        360.0 / rate - 0.67
                    } else {
                        this_sweep_dur
                    };
                    proj_start + dur
                };
                (proj_start, proj_end, SweepTimingProvenance::Projected)
            } else {
                let anchor_end = inp
                    .completed_sweep_metas
                    .iter()
                    .filter(|m| m.elevation_number < elev_num)
                    .max_by_key(|m| m.elevation_number)
                    .map(|m| m.end);
                match anchor_end {
                    Some(ae) => {
                        let anchor_elev_num = inp
                            .completed_sweep_metas
                            .iter()
                            .filter(|m| m.elevation_number < elev_num)
                            .max_by_key(|m| m.elevation_number)
                            .map(|m| m.elevation_number)
                            .unwrap_or(0);
                        let anchor_idx = anchor_elev_num as usize;
                        let remaining_dur = (vol_start + expected_dur) - ae;
                        let remaining_weight_sum: f64 = (anchor_idx..expected_count)
                            .map(|i| weighted_durations[i])
                            .sum();
                        if remaining_weight_sum > 0.0 {
                            let offset_from_anchor: f64 = (anchor_idx..elev_idx)
                                .map(|i| {
                                    (weighted_durations[i] / remaining_weight_sum) * remaining_dur
                                })
                                .sum();
                            let start = ae + offset_from_anchor;
                            (
                                start,
                                start + this_sweep_dur,
                                SweepTimingProvenance::Anchored,
                            )
                        } else {
                            (ae, ae + this_sweep_dur, SweepTimingProvenance::Anchored)
                        }
                    }
                    None => {
                        let offset = weighted_offsets[elev_idx];
                        (
                            vol_start + offset,
                            vol_start + offset + this_sweep_dur,
                            SweepTimingProvenance::Estimated,
                        )
                    }
                }
            }
        };

        let chunks_for_elev: Vec<&(u8, f64, f64, u32)> = inp
            .chunk_elev_spans
            .iter()
            .filter(|&&(e, _, _, _)| e == elev_num)
            .collect();
        let radials_received: u32 = if is_in_progress {
            chunks_for_elev.iter().map(|&&(_, _, _, r)| r).sum::<u32>()
                + inp.in_progress_radials.unwrap_or(0)
        } else {
            0
        };
        let chunks_received = if is_in_progress {
            chunks_for_elev.len() as u32
        } else {
            0
        };
        let chunks_expected = proj.map(|p| p.3);

        let chunks: Vec<ChunkSpan> = inp
            .chunk_elev_spans
            .iter()
            .filter(|&&(e, _, _, _)| e == elev_num)
            .zip(
                inp.current_elev_chunks
                    .iter()
                    .chain(std::iter::repeat(&(0.0f32, 0.0f32, 0u32))),
            )
            .map(
                |(&(_, start, end, radial_count), &(first_az, last_az, _))| ChunkSpan {
                    start,
                    end,
                    first_azimuth: first_az,
                    last_azimuth: last_az,
                    radial_count,
                },
            )
            .collect();

        // Resolved azimuth rate for extrapolation + per-sweep display: projected
        // rate when positive, else 360/duration, else a safe fallback. Matches
        // the previous from_live extrapolation resolution.
        let azimuth_rate_dps = proj.map(|p| p.2).filter(|&r| r > 0.0).unwrap_or_else(|| {
            if this_sweep_dur > 0.0 {
                360.0 / this_sweep_dur
            } else {
                20.0
            }
        });

        out.push(SweepBounds {
            elevation_number: elev_num,
            elevation_angle: elev_angle_for(elev_num),
            start,
            end,
            timing,
            is_complete,
            is_in_progress,
            radials_received,
            chunks_received,
            chunks_expected,
            azimuth_rate_dps,
            chunks,
        });
    }
    out
}

/// Resolve an elevation angle from the live VCP pattern, else the static VCP
/// definition, else a 0.5°-per-cut fallback.
fn elev_angle(
    vcp_pattern: Option<&crate::data::keys::ExtractedVcp>,
    vcp_number: u16,
    elev_num: u8,
) -> f32 {
    if let Some(vcp) = vcp_pattern {
        if let Some(e) = vcp.elevations.get(elev_num.saturating_sub(1) as usize) {
            return e.angle;
        }
    }
    crate::data::vcp::get_vcp_definition(vcp_number)
        .and_then(|d| d.elevations.get(elev_num.saturating_sub(1) as usize))
        .map(|e| e.angle)
        .unwrap_or(0.5 * elev_num as f32)
}

/// Build the per-sweep projection for the current + next scan.
pub fn build_sweeps(ctx: &SweepBuildCtx) -> Vec<SweepProjection> {
    let mut sweeps = Vec::new();

    // ── Current scan — full cascade over every expected elevation ──
    let current_last_seq = last_seq_by_elev(ctx.current_chunks);
    let cascade = cascade_current_sweeps(&CascadeInputs {
        vol_start: ctx.vol_start_secs,
        expected_count: ctx.expected_count,
        received: ctx.received,
        vcp_number: ctx.vcp_number,
        vcp_pattern: ctx.vcp_pattern,
        expected_dur: ctx.expected_dur_secs,
        current_volume_chunks: ctx.current_chunks,
        completed_sweep_metas: ctx.completed_sweep_metas,
        chunk_elev_spans: ctx.chunk_elev_spans,
        current_elev_chunks: ctx.current_elev_chunks,
        in_progress_elevation: ctx.in_progress_elevation,
        in_progress_radials: ctx.in_progress_radials,
        fallback_sweep_durations: ctx.fallback_sweep_durations,
    });
    for b in cascade {
        let last_seq = current_last_seq
            .get(&b.elevation_number)
            .copied()
            .unwrap_or(0);
        // Acquisition/display status from the engine's cached set (sparse cuts
        // we actually have) + inventory + in-progress. Orthogonal to the
        // cascade's `is_complete`/`is_in_progress` *bound-derivation* flags.
        let status = derive_sweep_status(
            ctx.current_scan_start_secs,
            b.elevation_number,
            ctx.current_volume,
            last_seq,
            ctx.cached,
            ctx.inventory,
            ctx.in_progress_elevation,
        );
        sweeps.push(SweepProjection {
            elevation_number: b.elevation_number,
            elevation_angle: b.elevation_angle,
            scan_role: ProjectionScanRole::CurrentInProgress,
            status,
            timing: b.timing,
            collection_start_secs: b.start,
            collection_end_secs: b.end,
            chunks_in_sweep: b.chunks_expected.unwrap_or(0) as usize,
            chunks_received: b.chunks_received,
            radials_received: b.radials_received,
            azimuth_rate_dps: b.azimuth_rate_dps,
            chunks: b.chunks,
        });
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
        let next_scan_start = projected_start.map(|p| p + delta).unwrap_or(0.0);
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
                elevation_angle: elev_angle(ctx.vcp_pattern, ctx.vcp_number, elev),
                scan_role: ProjectionScanRole::NextScan,
                status,
                timing: SweepTimingProvenance::Projected,
                collection_start_secs: agg.collection_start + delta,
                collection_end_secs: agg.collection_end + delta,
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
    use crate::nexrad::realtime::ChunkProjectedTimes;
    use crate::nexrad::timing::{IntervalCase, PhysicsBreakdown, SchedulerPath};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn vol(n: usize) -> VolumeIndex {
        VolumeIndex::new(n)
    }

    fn forecast(collection: f64, available: f64) -> ChunkProjectedTimes {
        ChunkProjectedTimes {
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

    fn chunk(
        seq: usize,
        elev: Option<usize>,
        fc: Option<ChunkProjectedTimes>,
    ) -> ChunkProjectionInfo {
        ChunkProjectionInfo {
            sequence: seq,
            elevation_number: elev,
            azimuth_rate_dps: 20.0,
            chunk_index_in_sweep: 0,
            chunks_in_sweep: 3,
            projected: fc,
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
        let received = [true, false, false];
        let metas = vec![crate::data::CachedSweep {
            start: 1000.0,
            end: 1010.0,
            elevation: 1.0,
            elevation_number: 1,
            start_azimuth: 0.0,
            cached_products: vec![],
        }];
        let durs = [100.0, 100.0, 100.0];
        let ctx = SweepBuildCtx {
            current_chunks: &current,
            next_chunks: Some(&next),
            current_scan_start_secs: 1000.0,
            current_volume: vol(1),
            next_volume: vol(2),
            cached: &cached,
            inventory: &inv,
            in_progress_elevation: Some(2),
            next_scan_boundary_start_secs: None,
            expected_count: 3,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            vol_start_secs: 1000.0,
            expected_dur_secs: 300.0,
            completed_sweep_metas: &metas,
            chunk_elev_spans: &[],
            current_elev_chunks: &[],
            in_progress_radials: None,
            fallback_sweep_durations: &durs,
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
        let received = [true, false];
        let metas = vec![crate::data::CachedSweep {
            start: 1000.0,
            end: 1010.0,
            elevation: 1.0,
            elevation_number: 1,
            start_azimuth: 0.0,
            cached_products: vec![],
        }];
        let durs = [100.0, 100.0];
        let ctx = SweepBuildCtx {
            current_chunks: &current,
            next_chunks: None,
            current_scan_start_secs: 1000.0,
            current_volume: vol(1),
            next_volume: vol(2),
            cached: &cached,
            inventory: &inv,
            in_progress_elevation: None,
            next_scan_boundary_start_secs: None,
            expected_count: 2,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            vol_start_secs: 1000.0,
            expected_dur_secs: 300.0,
            completed_sweep_metas: &metas,
            chunk_elev_spans: &[],
            current_elev_chunks: &[],
            in_progress_radials: None,
            fallback_sweep_durations: &durs,
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
        let received = [false];
        let durs = [100.0];
        let ctx = SweepBuildCtx {
            current_chunks: &current,
            next_chunks: Some(&next),
            current_scan_start_secs: 1000.0,
            current_volume: vol(1),
            next_volume: vol(2),
            cached: &cached,
            inventory: &inv,
            in_progress_elevation: None,
            // Authoritative next-scan start is 1090, projection said 1100 → −10s.
            next_scan_boundary_start_secs: Some(1090.0),
            expected_count: 1,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            vol_start_secs: 1000.0,
            expected_dur_secs: 300.0,
            completed_sweep_metas: &[],
            chunk_elev_spans: &[],
            current_elev_chunks: &[],
            in_progress_radials: None,
            fallback_sweep_durations: &durs,
        };
        let sweeps = build_sweeps(&ctx);
        let next_sweep = sweeps
            .iter()
            .find(|s| s.scan_role == ProjectionScanRole::NextScan)
            .unwrap();
        assert_eq!(next_sweep.collection_start_secs, 1090.0);
    }

    /// Golden freeze of the current-scan cascade (formerly validated against the
    /// from_live oracle): elev 1 cached→Observed, elev 2 in-progress with chunk
    /// data→Anchored, elev 3 future→Projected.
    #[wasm_bindgen_test]
    fn cascade_freezes_observed_anchored_projected() {
        let current_chunks = vec![chunk(7, Some(3), Some(forecast(1100.0, 1105.0)))];
        let metas = vec![crate::data::CachedSweep {
            start: 1000.0,
            end: 1010.0,
            elevation: 0.5,
            elevation_number: 1,
            start_azimuth: 0.0,
            cached_products: vec![],
        }];
        let spans = vec![(2u8, 1020.0, 1025.0, 50u32)];
        let received = [true, false, false];
        let durs = [100.0, 100.0, 100.0];
        let out = cascade_current_sweeps(&CascadeInputs {
            vol_start: 1000.0,
            expected_count: 3,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            expected_dur: 300.0,
            current_volume_chunks: &current_chunks,
            completed_sweep_metas: &metas,
            chunk_elev_spans: &spans,
            current_elev_chunks: &[],
            in_progress_elevation: Some(2),
            in_progress_radials: Some(7),
            fallback_sweep_durations: &durs,
        });
        assert_eq!(out.len(), 3);
        assert_eq!((out[0].start, out[0].end), (1000.0, 1010.0));
        assert_eq!(out[0].timing, SweepTimingProvenance::Observed);
        assert!(out[0].is_complete);
        assert_eq!(out[1].start, 1020.0);
        assert_eq!(out[1].end, 1025.0_f64.max(1120.0));
        assert_eq!(out[1].timing, SweepTimingProvenance::Anchored);
        assert!(out[1].is_in_progress);
        assert_eq!(out[2].start, 1100.0);
        assert_eq!(out[2].timing, SweepTimingProvenance::Projected);
    }

    // ── anchor-interpolation branch (no chunk data, no projection) ──────────

    /// Prior-meta-only: a not-yet-collected forecast cut with no chunk data and
    /// no library projection interpolates its start from the most recent
    /// completed sweep's end, weighted across the remaining cuts → Anchored,
    /// within the volume bounds. Hand-computed for 3 cuts, 100s each.
    #[wasm_bindgen_test]
    fn cascade_anchor_interpolates_from_prior_meta() {
        // elev 1 complete (anchor at end=1010); elev 2 & 3 forecast-only.
        let metas = vec![crate::data::CachedSweep {
            start: 1000.0,
            end: 1010.0,
            elevation: 0.5,
            elevation_number: 1,
            start_azimuth: 0.0,
            cached_products: vec![],
        }];
        let received = [true, false, false];
        let durs = [100.0, 100.0, 100.0];
        let out = cascade_current_sweeps(&CascadeInputs {
            vol_start: 1000.0,
            expected_count: 3,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            expected_dur: 300.0,
            current_volume_chunks: &[], // no projection
            completed_sweep_metas: &metas,
            chunk_elev_spans: &[], // no chunk data
            current_elev_chunks: &[],
            in_progress_elevation: None,
            in_progress_radials: None,
            fallback_sweep_durations: &durs,
        });
        assert_eq!(out.len(), 3);
        // elev 1: observed from meta.
        assert_eq!((out[0].start, out[0].end), (1000.0, 1010.0));
        assert_eq!(out[0].timing, SweepTimingProvenance::Observed);

        // elev 2: anchor end 1010, offset 0 (first remaining cut) → 1010..1110.
        assert_eq!(out[1].timing, SweepTimingProvenance::Anchored);
        assert!((out[1].start - 1010.0).abs() < 1e-9, "got {}", out[1].start);
        assert!((out[1].end - 1110.0).abs() < 1e-9, "got {}", out[1].end);

        // elev 3: remaining_dur = 1300-1010 = 290, remaining_weight = 200,
        // offset = (100/200)*290 = 145 → start 1155, end 1255.
        assert_eq!(out[2].timing, SweepTimingProvenance::Anchored);
        assert!((out[2].start - 1155.0).abs() < 1e-9, "got {}", out[2].start);
        assert!((out[2].end - 1255.0).abs() < 1e-9, "got {}", out[2].end);

        // All Anchored starts stay inside the volume bounds.
        for b in &out {
            assert!(b.start >= 1000.0 && b.end <= 1300.0 + 1e-9);
        }
    }

    /// No prior meta and no chunk/projection data → the cut can't be anchored;
    /// it falls back to a purely VCP-weighted Estimated bound from `vol_start`.
    #[wasm_bindgen_test]
    fn cascade_no_prior_meta_yields_estimated() {
        let received = [false, false];
        let durs = [100.0, 100.0];
        let out = cascade_current_sweeps(&CascadeInputs {
            vol_start: 1000.0,
            expected_count: 2,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            expected_dur: 200.0,
            current_volume_chunks: &[],
            completed_sweep_metas: &[], // no anchor available
            chunk_elev_spans: &[],
            current_elev_chunks: &[],
            in_progress_elevation: None,
            in_progress_radials: None,
            fallback_sweep_durations: &durs,
        });
        assert_eq!(out.len(), 2);
        // weighted_durations = [100,100]; offsets = [0,100].
        assert_eq!(out[0].timing, SweepTimingProvenance::Estimated);
        assert!((out[0].start - 1000.0).abs() < 1e-9);
        assert!((out[0].end - 1100.0).abs() < 1e-9);
        assert_eq!(out[1].timing, SweepTimingProvenance::Estimated);
        assert!((out[1].start - 1100.0).abs() < 1e-9);
        assert!((out[1].end - 1200.0).abs() < 1e-9);
    }

    /// Zero remaining weight (degenerate: expected_dur 0 with no fallback
    /// durations → all weighted durations 0): the anchor-interpolation collapses
    /// to the anchor end itself, still Anchored.
    #[wasm_bindgen_test]
    fn cascade_zero_remaining_weight_falls_back_to_anchor_end() {
        let metas = vec![crate::data::CachedSweep {
            start: 1000.0,
            end: 1010.0,
            elevation: 0.5,
            elevation_number: 1,
            start_azimuth: 0.0,
            cached_products: vec![],
        }];
        let received = [true, false];
        let out = cascade_current_sweeps(&CascadeInputs {
            vol_start: 1000.0,
            expected_count: 2,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            expected_dur: 0.0, // → weighted_durations all 0
            current_volume_chunks: &[],
            completed_sweep_metas: &metas,
            chunk_elev_spans: &[],
            current_elev_chunks: &[],
            in_progress_elevation: None,
            in_progress_radials: None,
            fallback_sweep_durations: &[], // empty → even distribution of 0
        });
        // elev 2: remaining_weight_sum == 0 → (ae, ae + 0) = (1010, 1010).
        assert_eq!(out[1].timing, SweepTimingProvenance::Anchored);
        assert!((out[1].start - 1010.0).abs() < 1e-9, "got {}", out[1].start);
        assert!((out[1].end - 1010.0).abs() < 1e-9, "got {}", out[1].end);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::nexrad::realtime::ChunkProjectedTimes;
    use crate::nexrad::timing::{IntervalCase, PhysicsBreakdown, SchedulerPath};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn vol(n: usize) -> VolumeIndex {
        VolumeIndex::new(n)
    }

    fn forecast(collection: f64, available: f64) -> ChunkProjectedTimes {
        ChunkProjectedTimes {
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

    fn chunk(
        seq: usize,
        elev: Option<usize>,
        fc: Option<ChunkProjectedTimes>,
    ) -> ChunkProjectionInfo {
        ChunkProjectionInfo {
            sequence: seq,
            elevation_number: elev,
            azimuth_rate_dps: 20.0,
            chunk_index_in_sweep: 0,
            chunks_in_sweep: 3,
            projected: fc,
        }
    }

    fn observe_chunk(
        inv: &mut KnownChunkInventory,
        volume: usize,
        sequence: usize,
        upload: f64,
        ty: nexrad_data::aws::realtime::ChunkType,
    ) {
        inv.observe(super::super::inventory::KnownChunk {
            coord: ChunkCoord {
                volume: vol(volume),
                sequence,
            },
            upload_secs: upload,
            chunk_type: ty,
        });
    }

    // ── published_in_inventory: each of the three independent OR branches ──

    #[wasm_bindgen_test]
    fn published_false_on_empty_inventory() {
        let inv = KnownChunkInventory::default();
        assert!(!published_in_inventory(&inv, vol(1), 5));
    }

    #[wasm_bindgen_test]
    fn published_true_via_has_end_even_with_low_sequences() {
        let mut inv = KnownChunkInventory::default();
        // End chunk observed at a low sequence; last_seq_of_sweep is far higher.
        observe_chunk(
            &mut inv,
            1,
            2,
            10.0,
            nexrad_data::aws::realtime::ChunkType::End,
        );
        // has_end short-circuits to true regardless of the requested last seq.
        assert!(published_in_inventory(&inv, vol(1), 999));
    }

    #[wasm_bindgen_test]
    fn published_true_via_newest_seq_gte_last_seq() {
        let mut inv = KnownChunkInventory::default();
        // Newest known seq in vol 1 is 7 (no End).
        observe_chunk(
            &mut inv,
            1,
            7,
            10.0,
            nexrad_data::aws::realtime::ChunkType::Intermediate,
        );
        // newest_seq (7) >= last_seq (5) → published.
        assert!(published_in_inventory(&inv, vol(1), 5));
        // Boundary: newest_seq (7) >= last_seq (7) → published.
        assert!(published_in_inventory(&inv, vol(1), 7));
        // newest_seq (7) < last_seq (8); seq 8 not present → not published.
        assert!(!published_in_inventory(&inv, vol(1), 8));
    }

    #[wasm_bindgen_test]
    fn published_true_via_contains_exact_sequence() {
        let mut inv = KnownChunkInventory::default();
        // Observe a gap: seq 3 and seq 9 known.
        observe_chunk(
            &mut inv,
            1,
            3,
            10.0,
            nexrad_data::aws::realtime::ChunkType::Intermediate,
        );
        observe_chunk(
            &mut inv,
            1,
            9,
            20.0,
            nexrad_data::aws::realtime::ChunkType::Intermediate,
        );
        // Target seq 3 is present exactly (and newest 9 >= 3 anyway).
        assert!(published_in_inventory(&inv, vol(1), 3));
        // A different volume has nothing.
        assert!(!published_in_inventory(&inv, vol(2), 3));
    }

    // ── derive_sweep_status: AvailableNotCollected via the has_end branch ──

    #[wasm_bindgen_test]
    fn derive_status_available_via_end_chunk() {
        let cached = CachedSweepSet::default();
        let mut inv = KnownChunkInventory::default();
        // End chunk observed for vol 1 → any not-cached, not-in-progress sweep
        // is AvailableNotCollected regardless of last_seq.
        observe_chunk(
            &mut inv,
            1,
            4,
            10.0,
            nexrad_data::aws::realtime::ChunkType::End,
        );
        assert_eq!(
            derive_sweep_status(1000.0, 6, vol(1), 100, &cached, &inv, None),
            SweepProjectionStatus::AvailableNotCollected
        );
    }

    #[wasm_bindgen_test]
    fn derive_status_in_progress_beats_available() {
        // Even when published, the in-progress elevation wins over Available
        // (precedence: cached > in-progress > available > future).
        let cached = CachedSweepSet::default();
        let mut inv = KnownChunkInventory::default();
        observe_chunk(
            &mut inv,
            1,
            10,
            10.0,
            nexrad_data::aws::realtime::ChunkType::End,
        );
        assert_eq!(
            derive_sweep_status(1000.0, 3, vol(1), 5, &cached, &inv, Some(3)),
            SweepProjectionStatus::InProgress
        );
    }

    // ── cascade_current_sweeps: 0.5°-per-cut elevation-angle fallback ──

    #[wasm_bindgen_test]
    fn cascade_elev_angle_falls_back_to_half_degree_per_cut() {
        // vcp_number 0 + no vcp_pattern → elev_angle_for uses 0.5 * elev_num.
        let received = [false, false, false];
        let durs = [100.0, 100.0, 100.0];
        let out = cascade_current_sweeps(&CascadeInputs {
            vol_start: 1000.0,
            expected_count: 3,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            expected_dur: 300.0,
            current_volume_chunks: &[],
            completed_sweep_metas: &[],
            chunk_elev_spans: &[],
            current_elev_chunks: &[],
            in_progress_elevation: None,
            in_progress_radials: None,
            fallback_sweep_durations: &durs,
        });
        assert_eq!(out.len(), 3);
        assert!((out[0].elevation_angle - 0.5).abs() < 1e-6);
        assert!((out[1].elevation_angle - 1.0).abs() < 1e-6);
        assert!((out[2].elevation_angle - 1.5).abs() < 1e-6);
    }

    // ── cascade_current_sweeps: azimuth_rate fallback to 360/duration ──

    #[wasm_bindgen_test]
    fn cascade_azimuth_rate_falls_back_to_360_over_duration() {
        // No projection (so no projected azimuth rate), duration 100s per cut →
        // azimuth_rate_dps resolves to 360/100 = 3.6.
        let received = [false];
        let durs = [100.0];
        let out = cascade_current_sweeps(&CascadeInputs {
            vol_start: 1000.0,
            expected_count: 1,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            expected_dur: 100.0,
            current_volume_chunks: &[],
            completed_sweep_metas: &[],
            chunk_elev_spans: &[],
            current_elev_chunks: &[],
            in_progress_elevation: None,
            in_progress_radials: None,
            fallback_sweep_durations: &durs,
        });
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].azimuth_rate_dps - 3.6).abs() < 1e-9,
            "got {}",
            out[0].azimuth_rate_dps
        );
    }

    // ── build_sweeps: next scan with no authoritative boundary (delta == 0) ──

    #[wasm_bindgen_test]
    fn build_sweeps_next_scan_unshifted_when_no_boundary() {
        let current = vec![chunk(2, Some(1), Some(forecast(1020.0, 1025.0)))];
        // Next volume has two elevations with distinct forecast spans.
        let next = vec![
            chunk(2, Some(1), Some(forecast(1100.0, 1105.0))),
            chunk(3, Some(1), Some(forecast(1108.0, 1112.0))),
            chunk(4, Some(2), Some(forecast(1130.0, 1135.0))),
        ];
        let cached = CachedSweepSet::default();
        let inv = KnownChunkInventory::default();
        let received = [false];
        let durs = [100.0];
        let ctx = SweepBuildCtx {
            current_chunks: &current,
            next_chunks: Some(&next),
            current_scan_start_secs: 1000.0,
            current_volume: vol(1),
            next_volume: vol(2),
            cached: &cached,
            inventory: &inv,
            in_progress_elevation: None,
            next_scan_boundary_start_secs: None, // delta = 0
            expected_count: 1,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            vol_start_secs: 1000.0,
            expected_dur_secs: 300.0,
            completed_sweep_metas: &[],
            chunk_elev_spans: &[],
            current_elev_chunks: &[],
            in_progress_radials: None,
            fallback_sweep_durations: &durs,
        };
        let sweeps = build_sweeps(&ctx);
        let next_sweeps: Vec<_> = sweeps
            .iter()
            .filter(|s| s.scan_role == ProjectionScanRole::NextScan)
            .collect();
        // Two distinct next-scan elevations.
        assert_eq!(next_sweeps.len(), 2);
        let e1 = next_sweeps
            .iter()
            .find(|s| s.elevation_number == 1)
            .unwrap();
        // Elev 1 spans the min/max of its two forecast chunks (1100..1108),
        // unshifted because there is no boundary.
        assert!((e1.collection_start_secs - 1100.0).abs() < 1e-9);
        assert!((e1.collection_end_secs - 1108.0).abs() < 1e-9);
        assert_eq!(e1.timing, SweepTimingProvenance::Projected);
        let e2 = next_sweeps
            .iter()
            .find(|s| s.elevation_number == 2)
            .unwrap();
        assert!((e2.collection_start_secs - 1130.0).abs() < 1e-9);
        // Next-scan sweeps never report received chunks/radials.
        assert_eq!(e2.chunks_received, 0);
        assert_eq!(e2.radials_received, 0);
    }
}
