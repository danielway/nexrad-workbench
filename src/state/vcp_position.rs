//! Unified VCP position model for consistent sweep/chunk positioning.
//!
//! `VcpPositionModel` provides a single, computed view of where every elevation
//! sweep sits in time within a volume scan. It works identically for live
//! in-progress volumes (constructed from `LiveModeState`) and archived completed
//! scans (constructed from `Scan`), so all UI consumers — timeline, left panel,
//! canvas, tooltips — see the same data without duplicating positioning logic.
//!
//! Migration note: consumers now read the engine's `ScanProjection`; this module
//! survives only as the parity oracle (`build_position_from_live`) + the
//! `SweepStatus`/`SweepTiming` enums the VCP-forecast diagnostics still use. It
//! is deleted in the final cleanup step.
#![allow(dead_code)]

use super::radar_data::Scan;
use super::LiveModeState;

// ── Core types ──────────────────────────────────────────────────────────

/// Computed position model for a single volume scan.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct VcpPositionModel {
    /// VCP number (e.g., 215, 35, 212). 0 if unknown.
    pub vcp_number: u16,
    /// Volume start time (Unix seconds).
    pub volume_start: f64,
    /// Observed or estimated volume end time.
    pub volume_end: f64,
    /// Whether the volume is fully complete.
    pub complete: bool,
    /// Scan key for identifying this volume in storage.
    pub scan_key: Option<String>,
    /// Per-elevation sweep positions, ordered by elevation index.
    pub sweeps: Vec<SweepPosition>,
    /// Extrapolation state for live azimuth estimation.
    pub extrapolation: Option<ExtrapolationState>,
    /// Projected next volume, present only when the next download target is
    /// expected to fall in the next volume (the active elevation filter has
    /// no remaining matches in the current volume). All sweeps inside have
    /// `status = Future` and `timing = Estimated`. The timeline renders this
    /// as a faded "ghost" of the next scan so the user can see where their
    /// target sweep lands relative to the current scan.
    pub next_volume_ghost: Option<Box<VcpPositionModel>>,
}

/// Position and state of a single elevation sweep within a volume.
#[derive(Clone, Debug)]
pub struct SweepPosition {
    /// Elevation number (1-based, from NEXRAD data).
    pub elevation_number: u8,
    /// Elevation angle in degrees.
    pub elevation_angle: f32,
    /// Best-known start time (Unix seconds).
    pub start: f64,
    /// Best-known end time (Unix seconds).
    pub end: f64,
    /// How this sweep's timing was determined.
    pub timing: SweepTiming,
    /// Completion status.
    pub status: SweepStatus,
    /// Chunks received for this sweep (live only; empty for archived).
    pub chunks: Vec<ChunkSpan>,
}

/// How a sweep's time bounds were derived.
#[derive(Clone, Debug, PartialEq)]
pub enum SweepTiming {
    /// Actual observed timestamps from radial collection.
    Observed,
    /// Estimated relative to a known completed sweep.
    Anchored,
    /// Purely estimated from VCP azimuth rates.
    Estimated,
}

/// Source-agnostic availability of a sweep, as the timeline understands it:
/// whether the data is already in hand, arriving now, or only forecast.
///
/// This is the vocabulary the [`crate::state::TimelineView`] exposes to the
/// UI so renderers draw by *availability* rather than by which source the
/// data came from. It is derived from [`SweepStatus`] via
/// [`SweepPosition::availability`]; `Complete` (persisted to IDB, whether
/// downloaded from the archive or collected live) maps to `Cached`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SweepAvailability {
    /// Persisted and renderable — downloaded from the archive or collected
    /// live and flushed to IDB.
    Cached,
    /// Actively being received right now.
    Collecting,
    /// Forecast to be collected; not present yet.
    Projected,
}

/// Completion status of a sweep.
#[derive(Clone, Debug, PartialEq)]
pub enum SweepStatus {
    /// All radials received, data persisted to IDB.
    Complete,
    /// Currently receiving chunks.
    InProgress {
        radials_received: u32,
        chunks_received: u32,
        chunks_expected: Option<u32>,
    },
    /// Not yet started.
    Future,
}

/// A single chunk's time and azimuth span within a sweep.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ChunkSpan {
    pub start: f64,
    pub end: f64,
    pub first_azimuth: f32,
    pub last_azimuth: f32,
    pub radial_count: u32,
}

/// State needed for extrapolating the current sweep line position.
#[derive(Clone, Debug)]
pub struct ExtrapolationState {
    pub last_radial_azimuth: f32,
    pub last_radial_time: f64,
    /// Degrees per second for the current sweep (360 / sweep_duration).
    pub degrees_per_sec: f64,
}

/// Aggregated projected timing for a single sweep, derived from chunk projections.
struct ProjectedSweepBounds {
    /// Earliest projected time among chunks in this sweep.
    min_time: f64,
    /// Latest projected time among chunks in this sweep.
    max_time: f64,
    /// Azimuth rotation rate from the VCP (degrees/second).
    azimuth_rate_dps: f64,
    /// Total number of chunks expected in this sweep.
    chunk_count: u32,
}

// ── Construction ────────────────────────────────────────────────────────

impl VcpPositionModel {
    /// Build a position model from live streaming state.
    ///
    /// Centralizes the sweep-positioning cascade. Uses the priority:
    /// 1. Complete + CachedSweep → Observed timing
    /// 2. InProgress + chunk data → Anchored timing
    /// 3. Library projection (ChunkProjectionInfo) → Projected timing
    /// 4. Fallback: VCP-weighted proportional distribution → Estimated timing
    pub fn from_live(live: &LiveModeState, _now_secs: f64) -> Option<Self> {
        let vol_start = live.current_volume.as_ref().map(|a| a.best_start_secs())?;
        let roster = live.elevation_roster();
        let expected_count = roster.expected_count().unwrap_or(0);
        if expected_count == 0 {
            return None;
        }

        let vcp_number = live.current_vcp_number.unwrap_or(0);

        // ── Volume end time ───────────────────────────────────────────
        // COLLECTION time — right edge of the in-progress volume on the
        // timeline is when the radar finishes scanning, not when the final
        // chunk uploads.
        let expected_dur = live.last_volume_duration_secs().unwrap_or(300.0);
        let volume_end = live
            .plan
            .as_ref()
            .and_then(|p| p.current_volume_end_collection_secs)
            .unwrap_or(vol_start + expected_dur);

        // ── Build projected sweep bounds from library projections ──────
        // Sweep bounds are COLLECTION times (when the radar will physically
        // scan), so timeline placeholders line up with the in-progress
        // sweep's own collection-time axis.
        let projected_sweeps: Option<std::collections::BTreeMap<u8, ProjectedSweepBounds>> =
            live.plan.as_ref().map(|plan| {
                let mut map: std::collections::BTreeMap<u8, ProjectedSweepBounds> =
                    std::collections::BTreeMap::new();
                for chunk in &plan.current_volume_chunks {
                    if let Some(elev) = chunk.elevation_number {
                        let elev_u8 = elev as u8;
                        let entry = map.entry(elev_u8).or_insert(ProjectedSweepBounds {
                            min_time: f64::MAX,
                            max_time: f64::MIN,
                            azimuth_rate_dps: chunk.azimuth_rate_dps,
                            chunk_count: 0,
                        });
                        entry.chunk_count += 1;
                        if let Some(t) = chunk.forecast.as_ref().map(|f| f.collection_time_secs) {
                            entry.min_time = entry.min_time.min(t);
                            entry.max_time = entry.max_time.max(t);
                        }
                    }
                }
                map
            });

        // ── Fallback: VCP-weighted durations ──────────────────────────
        let fallback_durs = live.fallback_sweep_durations();
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

        // Lookup helpers for VCP elevation angles.
        let vcp_def = crate::state::get_vcp_definition(vcp_number);
        let elev_angle_for = |elev_num: u8| -> f32 {
            if let Some(ref vcp) = live.current_vcp_pattern {
                if let Some(e) = vcp.elevations.get(elev_num.saturating_sub(1) as usize) {
                    return e.angle;
                }
            }
            vcp_def
                .and_then(|d| d.elevations.get(elev_num.saturating_sub(1) as usize))
                .map(|e| e.angle)
                .unwrap_or(0.5 * elev_num as f32)
        };

        let mut sweeps = Vec::with_capacity(expected_count);

        for elev_idx in 0..expected_count {
            let elev_num = (elev_idx + 1) as u8;
            let is_complete = roster.is_received(elev_num);
            let is_in_progress =
                !is_complete && live.current_in_progress_elevation == Some(elev_num);
            let this_sweep_dur = weighted_durations[elev_idx];

            // Library projection for this sweep (if available).
            let proj_sweep = projected_sweeps.as_ref().and_then(|ps| ps.get(&elev_num));

            // ── Determine sweep time bounds ────────────────────────────

            let (sw_start, sw_end, timing) = if is_complete {
                // Priority 1: Completed sweep with actual CachedSweep timestamps.
                if let Some(meta) = live
                    .completed_sweep_metas
                    .iter()
                    .find(|m| m.elevation_number == elev_num)
                {
                    (meta.start, meta.end, SweepTiming::Observed)
                } else {
                    let offset = weighted_offsets[elev_idx];
                    (
                        vol_start + offset,
                        vol_start + offset + this_sweep_dur,
                        SweepTiming::Estimated,
                    )
                }
            } else {
                // Check for actual chunk data for this elevation.
                let chunk_min = live
                    .chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == elev_num)
                    .map(|&(_, s, _, _)| s)
                    .reduce(f64::min);
                let chunk_max = live
                    .chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == elev_num)
                    .map(|&(_, _, e, _)| e)
                    .reduce(f64::max);

                if let Some(cm) = chunk_min {
                    // Have actual chunk data: use it for start, project end.
                    let sw_end_actual = match chunk_max {
                        Some(cmax) => {
                            // Use projection end if available, otherwise estimate.
                            let proj_end = proj_sweep
                                .filter(|p| p.max_time > f64::MIN)
                                .map(|p| p.max_time);
                            match proj_end {
                                Some(pe) => cmax.max(pe),
                                None => cmax.max(cm + this_sweep_dur),
                            }
                        }
                        None => cm + this_sweep_dur,
                    };
                    (cm, sw_end_actual, SweepTiming::Anchored)
                } else if let Some(ps) = proj_sweep.filter(|p| p.min_time < f64::MAX) {
                    // Priority 3: Library projection with valid projected times.
                    let proj_start = ps.min_time;
                    let proj_end = if ps.max_time > f64::MIN {
                        ps.max_time
                    } else {
                        // Estimate sweep end from azimuth rate
                        let rate = ps.azimuth_rate_dps;
                        let dur = if rate > 0.0 {
                            360.0 / rate - 0.67
                        } else {
                            this_sweep_dur
                        };
                        proj_start + dur
                    };
                    (proj_start, proj_end, SweepTiming::Estimated)
                } else {
                    // Priority 4: Fallback — anchor from predecessor or VCP weights.
                    let anchor_end = live
                        .completed_sweep_metas
                        .iter()
                        .filter(|m| m.elevation_number < elev_num)
                        .max_by_key(|m| m.elevation_number)
                        .map(|m| m.end);

                    match anchor_end {
                        Some(ae) => {
                            let anchor_elev_num = live
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
                                        (weighted_durations[i] / remaining_weight_sum)
                                            * remaining_dur
                                    })
                                    .sum();
                                let start = ae + offset_from_anchor;
                                (start, start + this_sweep_dur, SweepTiming::Anchored)
                            } else {
                                (ae, ae + this_sweep_dur, SweepTiming::Anchored)
                            }
                        }
                        None => {
                            let offset = weighted_offsets[elev_idx];
                            (
                                vol_start + offset,
                                vol_start + offset + this_sweep_dur,
                                SweepTiming::Estimated,
                            )
                        }
                    }
                }
            };

            // ── Determine sweep status ─────────────────────────────────

            let status = if is_complete {
                SweepStatus::Complete
            } else if is_in_progress {
                let chunks_for_elev: Vec<&(u8, f64, f64, u32)> = live
                    .chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == elev_num)
                    .collect();

                let total_radials: u32 =
                    chunks_for_elev.iter().map(|&&(_, _, _, r)| r).sum::<u32>()
                        + live.current_in_progress_radials.unwrap_or(0);

                // Plan-derived chunk count. Without a plan we leave it
                // unknown — the timeline renders the sweep block without
                // chunk subdivisions until a projection lands.
                let chunks_expected = proj_sweep.map(|ps| ps.chunk_count);

                SweepStatus::InProgress {
                    radials_received: total_radials,
                    chunks_received: chunks_for_elev.len() as u32,
                    chunks_expected,
                }
            } else {
                SweepStatus::Future
            };

            // ── Collect chunk spans ────────────────────────────────────

            let chunks: Vec<ChunkSpan> = live
                .chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == elev_num)
                .zip(
                    live.current_elev_chunks
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

            sweeps.push(SweepPosition {
                elevation_number: elev_num,
                elevation_angle: elev_angle_for(elev_num),
                start: sw_start,
                end: sw_end,
                timing,
                status,
                chunks,
            });
        }

        // ── Extrapolation state ────────────────────────────────────────

        let extrapolation = match (live.last_radial_azimuth, live.last_radial_time_secs) {
            (Some(az), Some(t)) => {
                let current_elev_idx = live
                    .current_in_progress_elevation
                    .map(|e| e.saturating_sub(1) as usize)
                    .unwrap_or(0);

                // Prefer projection's azimuth rate, fall back to 360/sweep_dur.
                let degrees_per_sec = projected_sweeps
                    .as_ref()
                    .and_then(|ps| {
                        let elev_num = (current_elev_idx + 1) as u8;
                        ps.get(&elev_num).map(|p| p.azimuth_rate_dps)
                    })
                    .filter(|&r| r > 0.0)
                    .unwrap_or_else(|| {
                        let sweep_dur = weighted_durations
                            .get(current_elev_idx)
                            .copied()
                            .unwrap_or(expected_dur / expected_count as f64);
                        if sweep_dur > 0.0 {
                            360.0 / sweep_dur
                        } else {
                            20.0 // safe fallback
                        }
                    });

                Some(ExtrapolationState {
                    last_radial_azimuth: az,
                    last_radial_time: t,
                    degrees_per_sec,
                })
            }
            _ => None,
        };

        let next_volume_ghost = live
            .plan
            .as_ref()
            .and_then(|p| p.next_volume_chunks.as_deref())
            .and_then(|projs| Self::ghost_from_projections(projs, vcp_number, live))
            .map(Box::new);

        Some(VcpPositionModel {
            vcp_number,
            volume_start: vol_start,
            volume_end,
            complete: false,
            scan_key: live
                .current_volume
                .as_ref()
                .map(|a| a.scan_key.to_storage_key()),
            sweeps,
            extrapolation,
            next_volume_ghost,
        })
    }

    /// Build a faded "ghost" model for the projected next volume from the
    /// chained chunk projections. All sweeps are `Future` / `Estimated` —
    /// we have no observed radials for the next volume yet, just physics
    /// projections.
    fn ghost_from_projections(
        projections: &[crate::nexrad::ChunkProjectionInfo],
        vcp_number: u16,
        live: &LiveModeState,
    ) -> Option<VcpPositionModel> {
        if projections.is_empty() {
            return None;
        }

        // Group projected collection times by elevation number.
        let mut per_elev: std::collections::BTreeMap<u8, (f64, f64, u32, f64)> =
            std::collections::BTreeMap::new();
        let mut vol_start = f64::MAX;
        let mut vol_end = f64::MIN;
        for chunk in projections {
            let Some(t) = chunk.forecast.as_ref().map(|f| f.collection_time_secs) else {
                continue;
            };
            vol_start = vol_start.min(t);
            vol_end = vol_end.max(t);
            if let Some(e) = chunk.elevation_number {
                let entry = per_elev.entry(e as u8).or_insert((
                    f64::MAX,
                    f64::MIN,
                    0u32,
                    chunk.azimuth_rate_dps,
                ));
                entry.0 = entry.0.min(t);
                entry.1 = entry.1.max(t);
                entry.2 += 1;
                if entry.3 <= 0.0 {
                    entry.3 = chunk.azimuth_rate_dps;
                }
            }
        }

        if vol_start >= vol_end {
            return None;
        }

        let vcp_def = crate::state::get_vcp_definition(vcp_number);
        let elev_angle_for = |elev_num: u8| -> f32 {
            if let Some(ref vcp) = live.current_vcp_pattern {
                if let Some(e) = vcp.elevations.get(elev_num.saturating_sub(1) as usize) {
                    return e.angle;
                }
            }
            vcp_def
                .and_then(|d| d.elevations.get(elev_num.saturating_sub(1) as usize))
                .map(|e| e.angle)
                .unwrap_or(0.5 * elev_num as f32)
        };

        let mut sweeps: Vec<SweepPosition> = per_elev
            .into_iter()
            .map(|(elev_num, (min_t, max_t, _chunk_count, rate))| {
                // Estimate sweep end from azimuth rate when only one chunk
                // bracketed it (min == max), same as the current-volume
                // projected-bounds fallback.
                let end = if max_t > min_t {
                    max_t
                } else if rate > 0.0 {
                    min_t + (360.0 / rate - 0.67)
                } else {
                    min_t
                };
                SweepPosition {
                    elevation_number: elev_num,
                    elevation_angle: elev_angle_for(elev_num),
                    start: min_t,
                    end,
                    timing: SweepTiming::Estimated,
                    status: SweepStatus::Future,
                    chunks: Vec::new(),
                }
            })
            .collect();
        sweeps.sort_by_key(|s| s.elevation_number);

        Some(VcpPositionModel {
            vcp_number,
            volume_start: vol_start,
            volume_end: vol_end,
            complete: false,
            scan_key: None,
            sweeps,
            extrapolation: None,
            next_volume_ghost: None,
        })
    }

    /// Build a position model from an archived (completed) scan.
    pub fn from_scan(scan: &Scan) -> Self {
        let sweeps = scan
            .sweeps
            .iter()
            .map(|s| SweepPosition {
                elevation_number: s.elevation_number,
                elevation_angle: s.elevation,
                start: s.start_time,
                end: s.end_time,
                timing: SweepTiming::Observed,
                status: SweepStatus::Complete,
                chunks: Vec::new(),
            })
            .collect();

        VcpPositionModel {
            vcp_number: scan.vcp,
            volume_start: scan.start_time,
            volume_end: scan.end_time,
            complete: true,
            scan_key: None,
            sweeps,
            extrapolation: None,
            next_volume_ghost: None,
        }
    }
}

// ── Pure derivation (the extracted cascade — the parity oracle) ─────────

/// Explicit inputs for [`build_position_from_live`], gathered off
/// `LiveModeState` by the thin [`VcpPositionModel::from_live`] adapter. Naming
/// the inputs explicitly makes the cascade a pure, testable function and the
/// parity oracle the engine's `build_sweeps` is checked against.
#[allow(dead_code)] // Consumed by the Step-3 parity test + migration.
pub(crate) struct FromLiveInputs<'a> {
    pub vol_start: f64,
    pub expected_count: usize,
    /// `received[elev_idx]` — whether elevation `elev_idx + 1` is fully received.
    pub received: &'a [bool],
    pub vcp_number: u16,
    pub vcp_pattern: Option<&'a crate::data::keys::ExtractedVcp>,
    pub expected_dur: f64,
    pub current_volume_end_collection_secs: Option<f64>,
    pub current_volume_chunks: &'a [crate::nexrad::ChunkProjectionInfo],
    pub next_volume_chunks: Option<&'a [crate::nexrad::ChunkProjectionInfo]>,
    pub completed_sweep_metas: &'a [crate::data::CachedSweep],
    pub chunk_elev_spans: &'a [(u8, f64, f64, u32)],
    pub current_elev_chunks: &'a [(f32, f32, u32)],
    pub in_progress_elevation: Option<u8>,
    pub in_progress_radials: Option<u32>,
    pub fallback_sweep_durations: &'a [f64],
    pub last_radial_azimuth: Option<f32>,
    pub last_radial_time_secs: Option<f64>,
    pub scan_key: Option<String>,
}

/// The sweep-positioning cascade (Observed > Anchored > Projected > Estimated),
/// as a pure function of explicit inputs. Behavior is identical to the previous
/// inline `from_live` body. Step 3 of the migration moves this logic into the
/// engine's `build_sweeps`; this fn is the parity oracle until then.
#[allow(dead_code)] // Oracle for the Step-3 parity test; consumed there.
pub(crate) fn build_position_from_live(inp: &FromLiveInputs) -> Option<VcpPositionModel> {
    if inp.expected_count == 0 {
        return None;
    }
    let volume_end = inp
        .current_volume_end_collection_secs
        .unwrap_or(inp.vol_start + inp.expected_dur);

    // Delegate the per-sweep bounds cascade to the shared engine core so the
    // oracle and `build_sweeps` can't diverge (parity by construction).
    let cascade = crate::nexrad::projection::cascade_current_sweeps(
        &crate::nexrad::projection::CascadeInputs {
            vol_start: inp.vol_start,
            expected_count: inp.expected_count,
            received: inp.received,
            vcp_number: inp.vcp_number,
            vcp_pattern: inp.vcp_pattern,
            expected_dur: inp.expected_dur,
            current_volume_chunks: inp.current_volume_chunks,
            completed_sweep_metas: inp.completed_sweep_metas,
            chunk_elev_spans: inp.chunk_elev_spans,
            current_elev_chunks: inp.current_elev_chunks,
            in_progress_elevation: inp.in_progress_elevation,
            in_progress_radials: inp.in_progress_radials,
            fallback_sweep_durations: inp.fallback_sweep_durations,
        },
    );

    let sweeps: Vec<SweepPosition> = cascade
        .iter()
        .map(|b| SweepPosition {
            elevation_number: b.elevation_number,
            elevation_angle: b.elevation_angle,
            start: b.start,
            end: b.end,
            timing: map_provenance(b.timing),
            status: if b.is_complete {
                SweepStatus::Complete
            } else if b.is_in_progress {
                SweepStatus::InProgress {
                    radials_received: b.radials_received,
                    chunks_received: b.chunks_received,
                    chunks_expected: b.chunks_expected,
                }
            } else {
                SweepStatus::Future
            },
            chunks: b
                .chunks
                .iter()
                .map(|c| ChunkSpan {
                    start: c.start,
                    end: c.end,
                    first_azimuth: c.first_azimuth,
                    last_azimuth: c.last_azimuth,
                    radial_count: c.radial_count,
                })
                .collect(),
        })
        .collect();

    let extrapolation = match (inp.last_radial_azimuth, inp.last_radial_time_secs) {
        (Some(az), Some(t)) => {
            let current_elev_idx = inp
                .in_progress_elevation
                .map(|e| e.saturating_sub(1) as usize)
                .unwrap_or(0);
            let degrees_per_sec = cascade
                .get(current_elev_idx)
                .map(|b| b.azimuth_rate_dps)
                .unwrap_or(20.0);
            Some(ExtrapolationState {
                last_radial_azimuth: az,
                last_radial_time: t,
                degrees_per_sec,
            })
        }
        _ => None,
    };

    let next_volume_ghost = inp
        .next_volume_chunks
        .and_then(|projs| build_ghost(projs, inp.vcp_number, inp.vcp_pattern))
        .map(Box::new);

    Some(VcpPositionModel {
        vcp_number: inp.vcp_number,
        volume_start: inp.vol_start,
        volume_end,
        complete: false,
        scan_key: inp.scan_key.clone(),
        sweeps,
        extrapolation,
        next_volume_ghost,
    })
}

/// Map the shared cascade's provenance to the legacy `SweepTiming` (collapsing
/// `Projected`→`Estimated` to preserve the old labeling the golden froze).
fn map_provenance(p: crate::nexrad::projection::SweepTimingProvenance) -> SweepTiming {
    use crate::nexrad::projection::SweepTimingProvenance as P;
    match p {
        P::Observed => SweepTiming::Observed,
        P::Anchored => SweepTiming::Anchored,
        P::Projected | P::Estimated => SweepTiming::Estimated,
    }
}

/// Faded "ghost" model for the projected next volume — all `Future`/`Estimated`.
/// Pure extraction of the previous `ghost_from_projections`.
#[allow(dead_code)] // Consumed via `build_position_from_live` in the migration.
pub(crate) fn build_ghost(
    projections: &[crate::nexrad::ChunkProjectionInfo],
    vcp_number: u16,
    vcp_pattern: Option<&crate::data::keys::ExtractedVcp>,
) -> Option<VcpPositionModel> {
    if projections.is_empty() {
        return None;
    }

    let mut per_elev: std::collections::BTreeMap<u8, (f64, f64, u32, f64)> =
        std::collections::BTreeMap::new();
    let mut vol_start = f64::MAX;
    let mut vol_end = f64::MIN;
    for chunk in projections {
        let Some(t) = chunk.forecast.as_ref().map(|f| f.collection_time_secs) else {
            continue;
        };
        vol_start = vol_start.min(t);
        vol_end = vol_end.max(t);
        if let Some(e) = chunk.elevation_number {
            let entry = per_elev.entry(e as u8).or_insert((
                f64::MAX,
                f64::MIN,
                0u32,
                chunk.azimuth_rate_dps,
            ));
            entry.0 = entry.0.min(t);
            entry.1 = entry.1.max(t);
            entry.2 += 1;
            if entry.3 <= 0.0 {
                entry.3 = chunk.azimuth_rate_dps;
            }
        }
    }

    if vol_start >= vol_end {
        return None;
    }

    let vcp_def = crate::state::get_vcp_definition(vcp_number);
    let elev_angle_for = |elev_num: u8| -> f32 {
        if let Some(vcp) = vcp_pattern {
            if let Some(e) = vcp.elevations.get(elev_num.saturating_sub(1) as usize) {
                return e.angle;
            }
        }
        vcp_def
            .and_then(|d| d.elevations.get(elev_num.saturating_sub(1) as usize))
            .map(|e| e.angle)
            .unwrap_or(0.5 * elev_num as f32)
    };

    let mut sweeps: Vec<SweepPosition> = per_elev
        .into_iter()
        .map(|(elev_num, (min_t, max_t, _chunk_count, rate))| {
            let end = if max_t > min_t {
                max_t
            } else if rate > 0.0 {
                min_t + (360.0 / rate - 0.67)
            } else {
                min_t
            };
            SweepPosition {
                elevation_number: elev_num,
                elevation_angle: elev_angle_for(elev_num),
                start: min_t,
                end,
                timing: SweepTiming::Estimated,
                status: SweepStatus::Future,
                chunks: Vec::new(),
            }
        })
        .collect();
    sweeps.sort_by_key(|s| s.elevation_number);

    Some(VcpPositionModel {
        vcp_number,
        volume_start: vol_start,
        volume_end: vol_end,
        complete: false,
        scan_key: None,
        sweeps,
        extrapolation: None,
        next_volume_ghost: None,
    })
}

// ── Query methods ───────────────────────────────────────────────────────

impl VcpPositionModel {
    /// Find the sweep that contains the given timestamp.
    pub fn sweep_at(&self, ts: f64) -> Option<&SweepPosition> {
        self.sweeps.iter().find(|s| ts >= s.start && ts <= s.end)
    }

    /// Estimate the sweep line azimuth at a given time.
    ///
    /// For live volumes: extrapolates from last known radial position.
    /// For archived volumes: interpolates within the sweep containing `ts`.
    pub fn estimated_azimuth_at(&self, ts: f64) -> Option<f32> {
        // Live extrapolation path.
        if let Some(ref ext) = self.extrapolation {
            let dt = ts - ext.last_radial_time;
            if !(0.0..=120.0).contains(&dt) {
                return None;
            }
            let estimated = ext.last_radial_azimuth as f64 + dt * ext.degrees_per_sec;
            return Some(((estimated % 360.0 + 360.0) % 360.0) as f32);
        }

        // Archived interpolation: linear within sweep.
        let sweep = self.sweep_at(ts)?;
        let duration = sweep.end - sweep.start;
        if duration <= 0.0 {
            return None;
        }
        let progress = (ts - sweep.start) / duration;
        Some((progress * 360.0 % 360.0) as f32)
    }

    /// Volume progress as 0.0..1.0 at the given timestamp.
    pub fn progress_at(&self, ts: f64) -> f32 {
        let duration = self.volume_end - self.volume_start;
        if duration <= 0.0 {
            return 0.0;
        }
        ((ts - self.volume_start) / duration).clamp(0.0, 1.0) as f32
    }

    /// Estimated elevation index (0-based) at the given timestamp.
    ///
    /// First tries to find a sweep that contains `ts` (between start and end).
    /// If none contains it (e.g., ts is in the gap between two sweeps, or
    /// projected start times lag behind actual collection due to S3 upload
    /// delay), returns the first sweep whose end is still in the future,
    /// or falls back to the last sweep whose end is in the past.
    pub fn elevation_index_at(&self, ts: f64) -> Option<usize> {
        // Exact containment
        for (i, s) in self.sweeps.iter().enumerate() {
            if ts >= s.start && ts <= s.end {
                return Some(i);
            }
        }
        // First sweep that hasn't ended yet (antenna is collecting it)
        for (i, s) in self.sweeps.iter().enumerate() {
            if ts < s.end {
                return Some(i);
            }
        }
        // All sweeps have ended — return the last one
        if !self.sweeps.is_empty() {
            Some(self.sweeps.len() - 1)
        } else {
            None
        }
    }

    /// Get the sweep time bounds for a given elevation number.
    #[allow(dead_code)]
    pub fn sweep_bounds(&self, elevation_number: u8) -> Option<(f64, f64)> {
        self.sweeps
            .iter()
            .find(|s| s.elevation_number == elevation_number)
            .map(|s| (s.start, s.end))
    }

    /// Total number of elevations in this volume.
    #[allow(dead_code)]
    pub fn elevation_count(&self) -> usize {
        self.sweeps.len()
    }

    /// Count of completed sweeps.
    pub fn completed_count(&self) -> usize {
        self.sweeps
            .iter()
            .filter(|s| s.status == SweepStatus::Complete)
            .count()
    }
}

// ── SweepPosition helpers ───────────────────────────────────────────────

impl SweepPosition {
    /// Sweep duration in seconds.
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }

    /// Whether this sweep has observed (not estimated) timestamps.
    pub fn is_observed(&self) -> bool {
        self.timing == SweepTiming::Observed
    }

    /// Radial progress fraction (0.0..1.0). Only meaningful for InProgress.
    #[allow(dead_code)]
    pub fn radial_fraction(&self) -> f32 {
        match &self.status {
            SweepStatus::InProgress {
                radials_received, ..
            } => (*radials_received as f32 / 360.0).clamp(0.0, 1.0),
            SweepStatus::Complete => 1.0,
            SweepStatus::Future => 0.0,
        }
    }

    /// Whether this sweep is currently being received.
    pub fn is_in_progress(&self) -> bool {
        matches!(self.status, SweepStatus::InProgress { .. })
    }

    /// Whether this sweep is complete.
    pub fn is_complete(&self) -> bool {
        self.status == SweepStatus::Complete
    }

    /// Whether this sweep hasn't started yet.
    pub fn is_future(&self) -> bool {
        self.status == SweepStatus::Future
    }

    /// Source-agnostic availability for the timeline view.
    pub fn availability(&self) -> SweepAvailability {
        match self.status {
            SweepStatus::Complete => SweepAvailability::Cached,
            SweepStatus::InProgress { .. } => SweepAvailability::Collecting,
            SweepStatus::Future => SweepAvailability::Projected,
        }
    }
}

#[cfg(test)]
mod golden {
    //! Golden oracle for the from_live cascade. Freezes the
    //! Observed/Anchored/Projected/Estimated behavior so the engine's
    //! `build_sweeps` (Step 3) can be parity-checked against it.
    use super::*;
    use crate::data::CachedSweep;
    use crate::nexrad::timing::{IntervalCase, PhysicsBreakdown, SchedulerPath};
    use crate::nexrad::{ChunkForecast, ChunkProjectionInfo};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn forecast(collection: f64) -> ChunkForecast {
        ChunkForecast {
            collection_time_secs: collection,
            available_at_secs: collection + 5.0,
            poll_at_secs: collection + 6.0,
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

    fn chunk(seq: usize, elev: usize, collection: f64) -> ChunkProjectionInfo {
        ChunkProjectionInfo {
            sequence: seq,
            elevation_number: Some(elev),
            azimuth_rate_dps: 18.0,
            chunk_index_in_sweep: 0,
            chunks_in_sweep: 3,
            forecast: Some(forecast(collection)),
        }
    }

    fn cached(elev: u8, start: f64, end: f64) -> CachedSweep {
        CachedSweep {
            start,
            end,
            elevation: 0.5 * elev as f32,
            elevation_number: elev,
            start_azimuth: 0.0,
            cached_products: vec![],
        }
    }

    /// 3 cuts: elev 1 cached (Observed/Complete), elev 2 in-progress with chunk
    /// data (Anchored/InProgress), elev 3 future via projection (Projected→
    /// Estimated/Future).
    #[wasm_bindgen_test]
    fn cascade_observed_anchored_projected() {
        let current_chunks = vec![chunk(7, 3, 1100.0)];
        let cached_metas = vec![cached(1, 1000.0, 1010.0)];
        let chunk_spans = vec![(2u8, 1020.0, 1025.0, 50u32)];
        let received = [true, false, false];
        let inp = FromLiveInputs {
            vol_start: 1000.0,
            expected_count: 3,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            expected_dur: 300.0,
            current_volume_end_collection_secs: None,
            current_volume_chunks: &current_chunks,
            next_volume_chunks: None,
            completed_sweep_metas: &cached_metas,
            chunk_elev_spans: &chunk_spans,
            current_elev_chunks: &[],
            in_progress_elevation: Some(2),
            in_progress_radials: Some(7),
            fallback_sweep_durations: &[100.0, 100.0, 100.0],
            last_radial_azimuth: None,
            last_radial_time_secs: None,
            scan_key: None,
        };
        let m = build_position_from_live(&inp).expect("builds");
        assert_eq!(m.sweeps.len(), 3);
        assert_eq!(m.volume_start, 1000.0);
        assert_eq!(m.volume_end, 1300.0); // vol_start + expected_dur (no plan end)

        // elev 1 — cached → Observed/Complete with meta times.
        let s1 = &m.sweeps[0];
        assert_eq!(s1.elevation_number, 1);
        assert_eq!(s1.timing, SweepTiming::Observed);
        assert_eq!(s1.status, SweepStatus::Complete);
        assert_eq!((s1.start, s1.end), (1000.0, 1010.0));
        assert_eq!(s1.elevation_angle, 0.5); // vcp 0, no pattern → 0.5 * elev

        // elev 2 — in-progress with chunk data → Anchored start at chunk_min,
        // end projected (no plan forecast for elev 2 → chunk_max + dur).
        let s2 = &m.sweeps[1];
        assert_eq!(s2.elevation_number, 2);
        assert_eq!(s2.timing, SweepTiming::Anchored);
        assert!(matches!(s2.status, SweepStatus::InProgress { .. }));
        assert_eq!(s2.start, 1020.0);
        assert_eq!(s2.end, 1025.0_f64.max(1020.0 + 100.0)); // cmax.max(cm+dur)

        // elev 3 — future, projected from the forecast → Estimated/Future.
        let s3 = &m.sweeps[2];
        assert_eq!(s3.elevation_number, 3);
        assert_eq!(s3.timing, SweepTiming::Estimated);
        assert_eq!(s3.status, SweepStatus::Future);
        assert_eq!(s3.start, 1100.0); // forecast min == max == 1100
    }

    /// Parity gate: the engine's `build_sweeps` current-scan output must match
    /// the `from_live` oracle for identical inputs — bounds (exactly, both go
    /// through `cascade_current_sweeps`), observed-ness, and the status mapping.
    #[wasm_bindgen_test]
    fn build_sweeps_matches_oracle_current_scan() {
        use crate::nexrad::projection::{
            build_sweeps, CachedSweepSet, KnownChunkInventory, SweepBuildCtx, SweepProjectionStatus,
        };
        use nexrad_data::aws::realtime::VolumeIndex;

        let current_chunks = vec![chunk(7, 3, 1100.0)];
        let cached_metas = vec![cached(1, 1000.0, 1010.0)];
        let chunk_spans = vec![(2u8, 1020.0, 1025.0, 50u32)];
        let received = [true, false, false];
        let durs = [100.0, 100.0, 100.0];

        let inp = FromLiveInputs {
            vol_start: 1000.0,
            expected_count: 3,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            expected_dur: 300.0,
            current_volume_end_collection_secs: None,
            current_volume_chunks: &current_chunks,
            next_volume_chunks: None,
            completed_sweep_metas: &cached_metas,
            chunk_elev_spans: &chunk_spans,
            current_elev_chunks: &[],
            in_progress_elevation: Some(2),
            in_progress_radials: Some(7),
            fallback_sweep_durations: &durs,
            last_radial_azimuth: None,
            last_radial_time_secs: None,
            scan_key: None,
        };
        let oracle = build_position_from_live(&inp).expect("oracle");

        // CachedSweepSet aligned with `received` so the engine's status matches.
        let mut cached_set = CachedSweepSet::default();
        cached_set.set_for_scan(1000.0, &cached_metas);
        let inv = KnownChunkInventory::default();
        let ctx = SweepBuildCtx {
            current_chunks: &current_chunks,
            next_chunks: None,
            current_scan_start_secs: 1000.0,
            next_scan_start_secs: None,
            current_volume: VolumeIndex::new(1),
            next_volume: VolumeIndex::new(2),
            cached: &cached_set,
            inventory: &inv,
            in_progress_elevation: Some(2),
            next_scan_boundary_start_secs: None,
            expected_count: 3,
            received: &received,
            vcp_number: 0,
            vcp_pattern: None,
            vol_start_secs: 1000.0,
            expected_dur_secs: 300.0,
            completed_sweep_metas: &cached_metas,
            chunk_elev_spans: &chunk_spans,
            current_elev_chunks: &[],
            in_progress_radials: Some(7),
            fallback_sweep_durations: &durs,
        };
        let bs = build_sweeps(&ctx);
        let current: Vec<_> = bs
            .iter()
            .filter(|s| {
                matches!(
                    s.scan_role,
                    crate::nexrad::projection::ProjectionScanRole::CurrentInProgress
                )
            })
            .collect();

        assert_eq!(current.len(), oracle.sweeps.len());
        for (o, b) in oracle.sweeps.iter().zip(current.iter()) {
            assert_eq!(o.elevation_number, b.elevation_number);
            assert_eq!(o.elevation_angle, b.elevation_angle);
            assert_eq!(o.start, b.collection_start_secs);
            assert_eq!(o.end, b.collection_end_secs);
            assert_eq!(o.is_observed(), b.is_observed());
            // Status mapping: Complete↔CollectedByUs, InProgress↔InProgress,
            // Future↔(FutureExpected|AvailableNotCollected).
            let mapped_ok = match o.status {
                SweepStatus::Complete => b.status == SweepProjectionStatus::CollectedByUs,
                SweepStatus::InProgress { .. } => b.status == SweepProjectionStatus::InProgress,
                SweepStatus::Future => matches!(
                    b.status,
                    SweepProjectionStatus::FutureExpected
                        | SweepProjectionStatus::AvailableNotCollected
                ),
            };
            assert!(mapped_ok, "status mismatch elev {}", o.elevation_number);
        }
    }
}
