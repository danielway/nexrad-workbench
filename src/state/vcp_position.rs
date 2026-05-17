//! Unified VCP position model for consistent sweep/chunk positioning.
//!
//! `VcpPositionModel` provides a single, computed view of where every elevation
//! sweep sits in time within a volume scan. It works identically for live
//! in-progress volumes (constructed from `LiveModeState`) and archived completed
//! scans (constructed from `Scan`), so all UI consumers — timeline, left panel,
//! canvas, tooltips — see the same data without duplicating positioning logic.

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
}
