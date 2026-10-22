//! Pure chunk-ingest reducer — the decisions the shell's
//! `handle_chunk_ingested_outcome` used to interleave with effect execution.
//!
//! The shell (`crate::app::worker_results`) reads `had_elevations`, runs
//! `advance_active_scan_chunk` (render-coordinator sync stays in the shell),
//! assembles a read-only [`ChunkIngestEnv`] snapshot plus mutable
//! [`ChunkIngestSlices`] over the core state, calls [`reduce_chunk_ingested`],
//! then executes the returned [`ChunkIngestActions`] in field order. The
//! reducer mutates only the core slices (live-mode state, projection-engine
//! observations, elevation selection, playback position) and *describes*
//! everything else — channel records, the live render dispatch, GPU texture
//! promotion, status text, intents, and the render-request flags.
//!
//! Pattern exemplars: [`crate::core::persist`] (reducer returning a decision
//! struct) and [`crate::core::diagnostics`] (reduce over `&mut` slices).

use crate::core::projection::{Projection, ProjectionEngine};
use crate::core::{
    build_elevation_list_from_vcp, ChunkIngestResult, ElevationSelection, Intent, LiveModeState,
    LiveVolumeState, PlaybackState, RosterSource, SiteStreamEvent,
};

/// Read-only frame context for one chunk-ingest outcome, shell-assembled.
/// `had_elevations` is read *before* the shell's `advance_active_scan_chunk`;
/// `available_elevations` reflects the coordinator *after* it.
pub(crate) struct ChunkIngestEnv<'a> {
    pub is_live: bool,
    pub site_id: &'a str,
    /// The chunk's site is the focus, whose volume ends move the session
    /// phase ([`crate::core::live_mode::session_phase_after`]).
    pub is_focus: bool,
    /// `viz_state.product.to_worker_string()`.
    pub product_worker_string: &'a str,
    /// `state.frame_now.secs()`.
    pub now_secs: f64,
    /// Coordinator elevations non-empty BEFORE `advance_active_scan_chunk`.
    pub had_elevations: bool,
    /// Coordinator elevations AFTER `advance_active_scan_chunk` — status
    /// strings and log lines print the list and its length.
    pub available_elevations: &'a [u8],
    /// The site's `live.frame_projection` (for the enriched chunk-position log).
    pub frame_projection: Option<&'a Projection>,
    pub volume_3d_active: bool,
}

/// Mutable borrows of the core state the reducer updates directly.
pub(crate) struct ChunkIngestSlices<'a> {
    /// The session: its phase moves when the volume ends.
    pub live_mode: &'a mut LiveModeState,
    /// The chunk's site: its volume in progress and its projection engine.
    pub volume: &'a mut LiveVolumeState,
    pub engine: &'a mut ProjectionEngine,
    pub elevation_selection: &'a mut ElevationSelection,
    pub playback: &'a mut PlaybackState,
}

/// Described effects the shell executes, in this field order.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct ChunkIngestActions {
    /// The site's `live.channel.record_chunk_collection_end_secs(x)`.
    pub record_chunk_collection_end_secs: Option<f64>,
    /// The site's `live.channel.record_availability_lag_secs(x)`.
    pub record_availability_lag_secs: Option<f64>,
    /// `render.coordinator.render_live(site, elevation, product)`, posted
    /// once per site per drain after [`SiteLiveRenderCoalescer`] folds the
    /// drain's acks.
    pub render_live: Option<(u8, String)>,
    /// GPU `promote_current_to_previous` under the renderer lock.
    pub promote_prev_texture: bool,
    /// `state.push_command(..)` pushes (RefreshTimeline / CheckEviction).
    pub intents: Vec<Intent>,
    /// `state.session_stats.pipeline.mark_processing_done()`.
    pub mark_processing_done: bool,
    /// `render.force_fresh_render()` on the site's track.
    pub force_fresh_render: bool,
    /// Shell calls `request_worker_render()` (the unified display resolver,
    /// which reads the stored sweep blob for a completed cut). Archive sets
    /// it on the first elevation and at volume end; live sets it when the
    /// acknowledgement says the elevation being collected just completed.
    pub request_render: bool,
    /// Shell calls `request_worker_render_volume()`.
    pub request_volume_render: bool,
}

/// The elevation a live partial-sweep render should draw after a chunk
/// acknowledgement, or `None` when there is nothing in memory to draw.
///
/// The worker keeps radials in memory only until their elevation completes:
/// the completing chunk flushes the sweep to IndexedDB and drops its radials,
/// but `current_elevation` keeps naming it. A render request then would find
/// no radials and come back as a worker error, although nothing is wrong (the
/// sweep is stored). So the request needs both an elevation and a non-zero
/// radial count for that elevation; the completing chunk instead asks for the
/// stored sweep (see [`completed_current_elevation`] and [`live_cut`]).
pub(crate) fn live_render_target(
    current_elevation: Option<u8>,
    current_elevation_radials: Option<u32>,
) -> Option<u8> {
    match (current_elevation, current_elevation_radials) {
        (Some(elevation), Some(radials)) if radials > 0 => Some(elevation),
        _ => None,
    }
}

/// The cut the live partial owns on the canvas, as `(elevation, anchor_ms)`
/// for [`crate::core::playback_manager::resolve_desired_display`] and the
/// `Decoded` upload gate, or `None` when the accumulator holds nothing to
/// draw for it.
///
/// Gated like [`live_render_target`]: once the completing chunk has flushed
/// an elevation to IndexedDB, no partial of it will ever be drawn again, so
/// the elevation must stop claiming the canvas. The resolver then falls
/// through to the cache and paints the stored sweep. Without this, the
/// completing chunk of the selected tilt never reached the canvas: the view
/// kept the last partial (or stayed empty if none was drawn) until the next
/// volume, because the resolver kept answering "live partial" for a cut no
/// live render would fill.
pub(crate) fn live_cut(
    current_elevation: Option<u8>,
    current_elevation_radials: Option<u32>,
    anchor_ms: Option<i64>,
) -> Option<(u8, i64)> {
    live_render_target(current_elevation, current_elevation_radials).zip(anchor_ms)
}

/// The elevation this acknowledgement finished while it was the one being
/// collected: the worker still names it, holds no radials for it, and lists
/// it as completed (its sweep is stored). The live frame then comes from the
/// stored sweep instead of the accumulator.
pub(crate) fn completed_current_elevation(result: &ChunkIngestResult) -> Option<u8> {
    let elevation = result.current_elevation?;
    let flushed =
        live_render_target(result.current_elevation, result.current_elevation_radials).is_none();
    (flushed && result.elevations_completed.contains(&elevation)).then_some(elevation)
}

/// Folds the live render requests of one worker-result drain into at most one.
///
/// Chunk ingests are not serialized on their acknowledgements: when several
/// chunks reach the worker together (the backfill burst on joining mid-sweep,
/// or any catch-up), it ingests them back to back and the main thread drains
/// several acknowledgements in one frame. By then the worker's accumulator
/// reflects the last of them, so only the last live target is worth drawing,
/// and none if a later acknowledgement in the drain completed (stored) that
/// elevation: its radials are gone, and the completing acknowledgement
/// already asked for the stored sweep.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LiveRenderCoalescer {
    pending: Option<(u8, String)>,
}

impl LiveRenderCoalescer {
    /// Observe one acknowledgement, in drain order: the reducer's
    /// `render_live` for it and the elevations it completed.
    pub(crate) fn observe(
        &mut self,
        render_live: Option<(u8, String)>,
        elevations_completed: &[u8],
    ) {
        if let Some((elevation, _)) = &self.pending {
            if elevations_completed.contains(elevation) {
                log::debug!(
                    "render_live elev={} dropped: a later ack in this drain stored it",
                    elevation
                );
                self.pending = None;
            }
        }
        if render_live.is_some() {
            if let Some((elevation, _)) = &self.pending {
                log::debug!(
                    "render_live elev={} replaced by a later ack in this drain",
                    elevation
                );
            }
            self.pending = render_live;
        }
    }

    /// The one live render to post for the drain, if any.
    pub(crate) fn finish(self) -> Option<(u8, String)> {
        self.pending
    }
}

/// [`LiveRenderCoalescer`] per streaming site: each site's worker
/// accumulator is its own, so one site's acknowledgements never replace or
/// drop another's live render. Sites keep the order of their first
/// acknowledgement in the drain.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SiteLiveRenderCoalescer {
    by_site: Vec<(crate::data::SiteId, LiveRenderCoalescer)>,
}

impl SiteLiveRenderCoalescer {
    /// Observe one acknowledgement from `site`'s stream, in drain order.
    pub(crate) fn observe(
        &mut self,
        site: &crate::data::SiteId,
        render_live: Option<(u8, String)>,
        elevations_completed: &[u8],
    ) {
        let index = match self.by_site.iter().position(|(id, _)| id == site) {
            Some(index) => index,
            None => {
                self.by_site
                    .push((site.clone(), LiveRenderCoalescer::default()));
                self.by_site.len() - 1
            }
        };
        self.by_site[index]
            .1
            .observe(render_live, elevations_completed);
    }

    /// The live renders to post for the drain: at most one per site.
    pub(crate) fn finish(self) -> Vec<(crate::data::SiteId, u8, String)> {
        self.by_site
            .into_iter()
            .filter_map(|(site, coalescer)| {
                coalescer
                    .finish()
                    .map(|(elevation, product)| (site, elevation, product))
            })
            .collect()
    }
}

/// Apply one worker chunk-ingest outcome to the core state and describe the
/// effects. Pure: in-memory mutation of the slices only (plus `log::debug!`).
pub(crate) fn reduce_chunk_ingested(
    env: ChunkIngestEnv<'_>,
    slices: ChunkIngestSlices<'_>,
    result: &ChunkIngestResult,
) -> ChunkIngestActions {
    let ChunkIngestSlices {
        live_mode,
        volume,
        engine,
        elevation_selection,
        playback,
    } = slices;
    let mut actions = ChunkIngestActions::default();
    let is_live = env.is_live;
    let source = "Realtime";
    let mut render_stored_sweep = false;

    // Build enriched log with projection-derived chunk positioning.
    let chunk_vol_index = result.context.chunk_index + 1; // 1-based for display
    let elev_nums: Vec<u8> = result
        .chunk_elev_spans
        .iter()
        .map(|&(e, _, _, _)| e)
        .collect();
    let total_azimuths: u32 = result
        .chunk_elev_spans
        .iter()
        .map(|&(_, _, _, count)| count)
        .sum();

    // Azimuth angle range from the chunk's azimuth data
    let az_range_str = if let Some(&(_, first_az, last_az)) = result.chunk_elev_az_ranges.first() {
        format!("{:.1}°–{:.1}°", first_az, last_az)
    } else {
        "n/a".to_string()
    };

    // Look up chunk-in-sweep and remaining from projection metadata.
    // chunk_index is 0-based where 0 = Start chunk (sequence 1), so
    // chunk_vol_index (= chunk_index + 1) already equals the 1-based sequence.
    let sequence = chunk_vol_index as usize;
    let (chunk_in_sweep_str, remaining_str) = env
        .frame_projection
        .and_then(|p| {
            p.current_volume_chunks()
                .iter()
                .find(|c| c.sequence == sequence)
        })
        .map(|c| {
            let in_sweep = format!("{}/{}", c.chunk_index_in_sweep + 1, c.chunks_in_sweep);
            let remaining = c.chunks_in_sweep.saturating_sub(c.chunk_index_in_sweep + 1);
            (in_sweep, format!("{}", remaining))
        })
        .unwrap_or_else(|| ("?/?".to_string(), "?".to_string()));

    log::debug!(
        "{}: chunk ingested scan={} vol_chunk={} sweep_chunk={} remaining_in_sweep={} \
         elevs={:?} azimuths={} az_range={} \
         elevs_completed={:?} sweeps_stored={} is_end={} vcp={:?} {:.1}ms",
        source,
        result.scan_key,
        chunk_vol_index,
        chunk_in_sweep_str,
        remaining_str,
        elev_nums,
        total_azimuths,
        az_range_str,
        result.elevations_completed,
        result.sweeps_stored,
        result.is_end,
        result.vcp.as_ref().map(|v| v.number),
        result.total_ms,
    );

    // Only update the site's live volume when actually in live mode
    if is_live {
        // Adopt the live volume anchor — provisional from the streaming
        // loop's IDB key, confirmed (when present) from the radial-parsed
        // header time. `set_or_confirm_volume` handles same-volume vs.
        // new-volume internally and runs `try_capture_forecast` on the
        // transition that first makes start time + VCP pattern both
        // known.
        let scan_key =
            crate::data::ScanKey::from_secs_f64(env.site_id, result.context.timestamp_secs);
        volume.set_or_confirm_volume(
            scan_key,
            result.context.timestamp_secs,
            result.volume_header_time_secs,
        );

        if !result.chunk_elev_spans.is_empty() {
            engine
                .observations_mut()
                .record_chunk_elev_spans(&result.chunk_elev_spans);
        }

        // Feed the shared projection engine the cached-sweep + in-progress
        // inputs: which cuts we have locally (CollectedByUs / omit from the
        // acquisition view) and which elevation is being received now
        // (InProgress).
        let scan_start_secs = result
            .volume_header_time_secs
            .unwrap_or(result.context.timestamp_secs);
        // Completed-volume duration so the engine can size the expected
        // in-progress duration (it falls back to the VCP estimate / default).
        let last_dur = volume
            .last_completed_volume
            .as_ref()
            .map(|r| r.volume_end_secs - r.volume_start_secs);
        engine.set_current_scan_start_secs(scan_start_secs);
        engine
            .observations_mut()
            .set_last_volume_duration_secs(last_dur);
        // Reads the engine's own completed metas — the prior ingest's,
        // since `update_sweep_metas` runs later this ingest.
        engine.set_cached_sweeps_for_scan(scan_start_secs);
        // Same source as the old `LiveModeState.current_in_progress_elevation`
        // so the projection and the live model agree on the collecting cut.
        engine.set_in_progress_elevation(scan_start_secs, result.current_elevation);

        // Push the most recent chunk's collection-end time down to the
        // streaming loop so the next projection anchors on the current
        // chunk's actual collection time (not the volume's start time).
        // Without this, forward-chunk projections come out as
        // volume_start + small_offset, landing in the past once the
        // volume is past its first chunk.
        if let Some(chunk_max_secs) = result.chunk_max_time_secs {
            actions.record_chunk_collection_end_secs = Some(chunk_max_secs);
        }

        // Record the empirical availability lag (S3 upload − ACTUAL
        // chunk collection time) into the projector's stats bucket.
        // Uses the chunk's latest-radial time (when the radar finished
        // this chunk) paired with the most recent arrival stat's
        // Last-Modified header.
        if let Some(chunk_max_secs) = result.chunk_max_time_secs {
            // Lag requires both a parsed collection time AND the chunk's
            // S3 Last-Modified header. Stamp collection time unconditionally
            // and lag only when both are finite.
            let s3_at = volume
                .chunk_arrivals
                .last()
                .and_then(|a| a.s3_last_modified_at);
            let lag_secs = s3_at
                .map(|s3| s3 - chunk_max_secs)
                .filter(|v| v.is_finite());
            if let Some(lag) = lag_secs {
                actions.record_availability_lag_secs = Some(lag);
            }
            // Back-fill onto the most recent arrival so the diagnostics
            // modal can compute per-chunk collection-space intervals
            // and (when available) per-chunk availability lag.
            volume.attach_collection_data_to_last_arrival(
                chunk_max_secs,
                lag_secs.map(|lag| (lag * 1000.0) as i64),
            );
        }
        if !result.elevations_completed.is_empty() {
            engine
                .observations_mut()
                .record_elevations(&result.elevations_completed);
        }
        if let Some(ref vcp) = result.vcp {
            engine.observations_mut().record_vcp(vcp);
            // Re-resolve the user's selected elevation against the new VCP.
            // The incoming pattern is a complete roster, and the resolver
            // only rewrites when the selected cut genuinely has no home in
            // it (a real VCP transition) — so this is safe to run on every
            // volume's Start chunk, even though `reset_volume_observations`
            // wipes the previous pattern between volumes.
            let entries = build_elevation_list_from_vcp(vcp);
            if let Some((elevation_number, angle)) =
                elevation_selection.resolved_against_roster(&entries, RosterSource::FullVcp)
            {
                *elevation_selection = ElevationSelection::Fixed {
                    elevation_number,
                    angle,
                };
            }
        }

        let elev_changed = engine.observations_mut().record_in_progress_elevation(
            result.current_elevation,
            result.current_elevation_radials,
        );
        if elev_changed {
            // The per-chunk az list (engine) and the decoder-side sweep
            // start azimuth (live) reset together on an elevation change.
            volume.on_in_progress_elevation_changed();
        }

        // Record per-chunk azimuth ranges for the current elevation
        if let Some(cur_elev) = result.current_elevation {
            for &(elev, first_az, last_az) in &result.chunk_elev_az_ranges {
                if elev == cur_elev {
                    let radial_count = result
                        .chunk_elev_spans
                        .iter()
                        .find(|&&(e, _, _, _)| e == elev)
                        .map(|&(_, _, _, c)| c)
                        .unwrap_or(0);
                    engine
                        .observations_mut()
                        .push_elev_chunk((first_az, last_az, radial_count));
                }
            }
        }

        if !result.sweeps.is_empty() {
            engine
                .observations_mut()
                .update_sweep_metas(result.sweeps.clone());
        }

        volume.record_last_radial(result.last_radial_azimuth, result.last_radial_time_secs);

        // ── Log: sweep storage ────────────────────────────────────
        if !result.elevations_completed.is_empty() {
            for &completed_elev in &result.elevations_completed {
                if let Some(meta) = result
                    .sweeps
                    .iter()
                    .find(|s| s.elevation_number == completed_elev)
                {
                    log::debug!(
                        "{}: sweep stored elev={} angle={:.1}° start_az={:.1}° \
                         time={:.1}–{:.1}s dur={:.2}s products={} vol_chunk={}",
                        source,
                        completed_elev,
                        meta.elevation,
                        meta.start_azimuth,
                        meta.start,
                        meta.end,
                        meta.end - meta.start,
                        result.sweeps_stored,
                        chunk_vol_index,
                    );
                } else {
                    log::debug!(
                        "{}: sweep stored elev={} (no CachedSweep) products={} vol_chunk={}",
                        source,
                        completed_elev,
                        result.sweeps_stored,
                        chunk_vol_index,
                    );
                }
            }
        }

        // ── Log + dispatch: live partial-sweep render ─────────────
        // Render whatever elevation is currently being accumulated — the
        // user expects to see live progress regardless of which elevation
        // was previously displayed — but only while the accumulator still
        // holds radials for it (see `live_render_target`). The chunk that
        // completes that elevation asks for the stored sweep instead.
        if !result.is_end {
            if let Some(target_elev) =
                live_render_target(result.current_elevation, result.current_elevation_radials)
            {
                let product = env.product_worker_string.to_string();

                // Summarize what the accumulator holds for this elevation
                let accum_radials = result.current_elevation_radials.unwrap_or(0);
                let (accum_chunks, accum_az_range) = {
                    let obs = engine.observations();
                    let chunks = obs
                        .chunk_elev_spans
                        .iter()
                        .filter(|&&(e, _, _, _)| e == target_elev)
                        .count();
                    let az = obs
                        .current_elev_chunks
                        .iter()
                        .fold((f32::MAX, f32::MIN), |(lo, hi), &(first_az, last_az, _)| {
                            (lo.min(first_az), hi.max(last_az))
                        });
                    (chunks, az)
                };
                let az_str = if accum_az_range.0 < f32::MAX {
                    format!("{:.1}°–{:.1}°", accum_az_range.0, accum_az_range.1)
                } else {
                    "n/a".to_string()
                };

                log::debug!(
                    "{}: render_live dispatched elev={} product={} accum_radials={} \
                     accum_chunks={} accum_az={} vol_chunk={}",
                    source,
                    target_elev,
                    product,
                    accum_radials,
                    accum_chunks,
                    az_str,
                    chunk_vol_index,
                );

                actions.render_live = Some((target_elev, product));
            } else if let Some(stored_elev) = completed_current_elevation(result) {
                // The chunk that finished the cut being collected: its
                // radials are in IndexedDB now, not in the accumulator, so
                // draw the stored sweep through the normal render path.
                log::debug!(
                    "{}: elev={} completed and stored, rendering stored sweep vol_chunk={}",
                    source,
                    stored_elev,
                    chunk_vol_index,
                );
                render_stored_sweep = true;
            }
        }
    }

    // Refresh timeline when new elevations are written to cache
    if !result.elevations_completed.is_empty() {
        log::debug!(
            "{}: {} new elevation(s) cached, refreshing timeline (total available: {:?})",
            source,
            result.elevations_completed.len(),
            env.available_elevations,
        );
        actions.intents.push(Intent::RefreshTimeline {
            auto_position: !is_live,
        });
    }

    if result.is_end {
        if is_live {
            actions.promote_prev_texture = true;
            let now = env.now_secs;
            // Seal the diagnostics record from the engine's observations,
            // then reset them for the next volume (seal-before-reset).
            volume.handle_volume_complete(now, engine.observations());
            engine.reset_volume_observations();
            live_mode.apply_site_event(SiteStreamEvent::VolumeComplete, env.is_focus, now);
        } else {
            // Archive volume-end: jump the playhead to now. Applied directly
            // to the playback slice — the shell has nothing to execute.
            playback.set_playback_position(env.now_secs);
        }

        log::debug!(
            "{}: volume complete — {} elevations, triggering render",
            source,
            env.available_elevations.len()
        );
        actions.intents.push(Intent::RefreshTimeline {
            auto_position: !is_live,
        });
        actions.intents.push(Intent::CheckEviction);
        actions.mark_processing_done = true;

        actions.force_fresh_render = true;
        if !is_live {
            actions.request_render = true;
            if env.volume_3d_active {
                actions.request_volume_render = true;
            }
        }
    } else if !env.had_elevations && !env.available_elevations.is_empty() {
        log::debug!(
            "{}: first elevation available, triggering initial render",
            source
        );
        actions.force_fresh_render = true;
        if !is_live {
            actions.request_render = true;
            if env.volume_3d_active {
                actions.request_volume_render = true;
            }
        }
    }

    if render_stored_sweep {
        actions.force_fresh_render = true;
        actions.request_render = true;
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ChunkArrivalStat, ChunkIngestContext, StreamingPlan};
    use crate::data::{ExtractedVcp, ExtractedVcpElevation, ScanKey};
    use wasm_bindgen_test::wasm_bindgen_test;

    const NOW: f64 = 2_000.0;

    fn extracted_elev(angle: f32) -> ExtractedVcpElevation {
        ExtractedVcpElevation {
            angle,
            waveform: "CS".to_string(),
            prf_number: 1,
            is_sails: false,
            is_mrle: false,
            is_base_tilt: false,
            azimuth_rate: Some(20.0),
        }
    }

    fn vcp(angles: &[f32]) -> ExtractedVcp {
        ExtractedVcp {
            number: 215,
            elevations: angles.iter().map(|&a| extracted_elev(a)).collect(),
        }
    }

    fn base_result(ts: f64) -> ChunkIngestResult {
        ChunkIngestResult {
            context: ChunkIngestContext {
                scan_key: ScanKey::from_secs_f64("KDMX", ts),
                timestamp_secs: ts,
                chunk_index: 0,
            },
            scan_key: ScanKey::from_secs_f64("KDMX", ts),
            elevations_completed: Vec::new(),
            sweeps_stored: 0,
            is_end: false,
            sweeps: Vec::new(),
            vcp: None,
            total_ms: 12.0,
            current_elevation: None,
            current_elevation_radials: None,
            last_radial_azimuth: None,
            last_radial_time_secs: None,
            volume_header_time_secs: None,
            chunk_min_time_secs: None,
            chunk_max_time_secs: None,
            chunk_elev_spans: Vec::new(),
            chunk_elev_az_ranges: Vec::new(),
        }
    }

    fn env<'a>(is_live: bool, had_elevations: bool, available: &'a [u8]) -> ChunkIngestEnv<'a> {
        ChunkIngestEnv {
            is_live,
            site_id: "KDMX",
            is_focus: true,
            product_worker_string: "reflectivity",
            now_secs: NOW,
            had_elevations,
            available_elevations: available,
            frame_projection: None,
            volume_3d_active: false,
        }
    }

    /// Default core state bundle the reducer mutates.
    struct Fx {
        session: LiveModeState,
        live: LiveVolumeState,
        engine: ProjectionEngine,
        sel: ElevationSelection,
        playback: PlaybackState,
    }

    impl Fx {
        fn new() -> Self {
            Self {
                session: LiveModeState::default(),
                live: LiveVolumeState::default(),
                engine: ProjectionEngine::new(),
                sel: ElevationSelection::default(),
                playback: PlaybackState::default(),
            }
        }

        fn run(
            &mut self,
            env: ChunkIngestEnv<'_>,
            result: &ChunkIngestResult,
        ) -> ChunkIngestActions {
            reduce_chunk_ingested(
                env,
                ChunkIngestSlices {
                    live_mode: &mut self.session,
                    volume: &mut self.live,
                    engine: &mut self.engine,
                    elevation_selection: &mut self.sel,
                    playback: &mut self.playback,
                },
                result,
            )
        }
    }

    // (1) Archive chunk with completed elevations: timeline refresh with
    // auto-position, and no live-side mutations or live render dispatch.
    #[wasm_bindgen_test]
    fn archive_completed_elevations_refresh_timeline_only() {
        let mut fx = Fx::new();
        let mut r = base_result(1_000.0);
        r.elevations_completed = vec![1];
        r.sweeps_stored = 1;
        r.current_elevation = Some(2); // live-only input; must be ignored

        let a = fx.run(env(false, true, &[1, 2]), &r);

        assert_eq!(
            a.intents,
            vec![Intent::RefreshTimeline {
                auto_position: true
            }]
        );
        assert!(a.render_live.is_none());
        assert!(!a.promote_prev_texture);
        assert!(!a.mark_processing_done);
        assert!(!a.force_fresh_render && !a.request_render && !a.request_volume_render);
        assert!(a.record_chunk_collection_end_secs.is_none());
        // No live mutations happened.
        assert!(fx.live.current_volume.is_none());
        assert!(fx.engine.observations().elevations_received.is_empty());
        assert!(fx
            .engine
            .observations()
            .current_in_progress_elevation
            .is_none());
    }

    // (2) Live mid-volume chunk: volume anchor adopted, engine observations
    // updated, live render dispatched with the env product, no promote.
    #[wasm_bindgen_test]
    fn live_mid_volume_chunk_adopts_anchor_and_dispatches_render_live() {
        let mut fx = Fx::new();
        let mut r = base_result(1_000.0);
        r.current_elevation = Some(1);
        r.current_elevation_radials = Some(120);
        r.chunk_elev_spans = vec![(1, 1_000.0, 1_010.0, 120)];
        r.chunk_elev_az_ranges = vec![(1, 0.0, 120.0)];
        r.volume_header_time_secs = Some(1_000.5);

        let a = fx.run(env(true, false, &[]), &r);

        assert_eq!(a.render_live, Some((1, "reflectivity".to_string())));
        assert!(!a.promote_prev_texture);
        assert!(a.intents.is_empty());
        assert!(!a.force_fresh_render);
        // Anchor adopted with the confirmed header time winning.
        let anchor = fx.live.current_volume.as_ref().expect("anchor adopted");
        assert_eq!(anchor.scan_key, ScanKey::from_secs_f64("KDMX", 1_000.0));
        assert_eq!(anchor.best_start_secs(), 1_000.5);
        // Engine observations updated: in-progress cut + spans + az chunk.
        let obs = fx.engine.observations();
        assert_eq!(obs.current_in_progress_elevation, Some(1));
        assert_eq!(obs.current_in_progress_radials, Some(120));
        assert_eq!(obs.chunk_elev_spans, vec![(1, 1_000.0, 1_010.0, 120)]);
        assert_eq!(obs.current_elev_chunks, vec![(0.0, 120.0, 120)]);
    }

    /// Partial ack: the accumulator holds `radials` of `elev`.
    fn partial(elev: u8, radials: u32) -> ChunkIngestResult {
        let mut r = base_result(1_000.0);
        r.current_elevation = Some(elev);
        r.current_elevation_radials = Some(radials);
        r.chunk_elev_spans = vec![(elev, 1_000.0, 1_010.0, 120)];
        r
    }

    /// The ack for the chunk that completes `elev`: the worker still names
    /// it, holds none of its radials, and lists it as completed and stored.
    fn completing(elev: u8) -> ChunkIngestResult {
        let mut r = base_result(1_000.0);
        r.current_elevation = Some(elev);
        r.current_elevation_radials = Some(0);
        r.elevations_completed = vec![elev];
        r.sweeps_stored = 4;
        r.chunk_elev_spans = vec![(elev, 1_060.0, 1_071.0, 120)];
        r
    }

    /// The live cut the shell's `live_render_sources` derives from the
    /// engine observations after the reducer ran (anchor of the 1000 s scan).
    fn observed_live_cut(fx: &Fx) -> Option<(u8, i64)> {
        let obs = fx.engine.observations();
        live_cut(
            obs.current_in_progress_elevation,
            obs.current_in_progress_radials,
            Some(1_000_000),
        )
    }

    /// The scan at 1000 s with elevation 1 stored (what the timeline holds
    /// once the refresh after the completing chunk lands).
    fn timeline_with_stored_elev1() -> crate::core::RadarTimeline {
        crate::core::RadarTimeline {
            scans: vec![crate::core::Scan {
                start_time: 1_000.0,
                end_time: 1_300.0,
                key_timestamp: 1_000.0,
                vcp: 35,
                vcp_pattern: None,
                sweeps: vec![crate::core::Sweep {
                    start_time: 1_000.0,
                    end_time: 1_071.0,
                    elevation: 0.5,
                    elevation_number: 1,
                    start_azimuth: 0.0,
                    radials: Vec::new(),
                    cached_products: vec!["reflectivity".to_string()],
                }],
                completeness: None,
                cached_sweep_count: None,
                planned_sweep_count: None,
            }],
        }
    }

    /// What the canvas resolver answers in Rays mode (partials shown) for the
    /// fixed 0.5° tilt at the live edge, given the observed live cut.
    fn desired_at_live_edge(
        fx: &Fx,
        timeline: &crate::core::RadarTimeline,
    ) -> crate::core::playback_manager::DesiredDisplay {
        crate::core::playback_manager::resolve_desired_display(
            "KDMX",
            1_075.0,
            &ElevationSelection::Fixed {
                elevation_number: 1,
                angle: 0.5,
            },
            crate::core::RadarProduct::Reflectivity,
            timeline,
            15.0 * 60.0,
            observed_live_cut(fx),
            false,
        )
    }

    // Decision table for the cut being collected, chunk by chunk.
    //
    // | ack                                   | render_live | stored render |
    // |---------------------------------------|-------------|---------------|
    // | partial (radials of elev in memory)   | elev        | no            |
    // | completing (elev flushed and stored)  | none        | yes           |
    // | next elevation's first chunk          | next elev   | no            |
    #[wasm_bindgen_test]
    fn live_cut_decision_table_partial_completing_next() {
        let mut fx = Fx::new();

        let a = fx.run(env(true, false, &[]), &partial(1, 600));
        assert_eq!(a.render_live, Some((1, "reflectivity".to_string())));
        assert!(!a.request_render);
        assert_eq!(observed_live_cut(&fx), Some((1, 1_000_000)));

        let a = fx.run(env(true, false, &[1]), &completing(1));
        assert_eq!(a.render_live, None);
        assert!(a.request_render, "stored sweep goes through request_render");
        assert!(a.force_fresh_render);
        assert_eq!(observed_live_cut(&fx), None);

        let a = fx.run(env(true, true, &[1]), &partial(2, 120));
        assert_eq!(a.render_live, Some((2, "reflectivity".to_string())));
        assert!(!a.request_render);
        assert_eq!(observed_live_cut(&fx), Some((2, 1_000_000)));
    }

    // The completing chunk of an elevation flushes its radials to IndexedDB:
    // the ack still names the elevation but holds none of its radials. No
    // live render is requested (it would only come back as a worker error);
    // the stored sweep is rendered instead, and the timeline refresh that
    // makes it resolvable is pushed as usual.
    #[wasm_bindgen_test]
    fn live_elevation_completing_chunk_renders_stored_sweep_not_live() {
        let mut fx = Fx::new();
        let a = fx.run(env(true, false, &[1]), &completing(1));

        assert_eq!(a.render_live, None);
        assert!(a.request_render);
        assert!(a.force_fresh_render);
        assert!(!a.request_volume_render);
        assert_eq!(
            a.intents,
            vec![Intent::RefreshTimeline {
                auto_position: false
            }]
        );
        // The engine still records which cut the stream is on; only the live
        // cut (what owns the canvas) lets go of it.
        assert_eq!(
            fx.engine.observations().current_in_progress_elevation,
            Some(1)
        );
        assert_eq!(completed_current_elevation(&completing(1)), Some(1));
    }

    // Joining live on the chunk that completes the selected tilt: with the
    // sweep stored and in the timeline, the resolver paints it from cache
    // instead of waiting on a live partial that will never come (the
    // empty-view bug). While the cut is partial, the live partial wins.
    #[wasm_bindgen_test]
    fn completed_live_cut_resolves_to_stored_sweep() {
        use crate::core::playback_manager::DesiredDisplay;
        let tl = timeline_with_stored_elev1();

        let mut fx = Fx::new();
        fx.run(env(true, false, &[]), &partial(1, 600));
        assert_eq!(
            desired_at_live_edge(&fx, &tl),
            DesiredDisplay::LivePartial {
                elevation_number: 1
            }
        );

        fx.run(env(true, false, &[1]), &completing(1));
        match desired_at_live_edge(&fx, &tl) {
            DesiredDisplay::Cached(identity) => {
                assert_eq!(identity.scan_key, ScanKey::from_secs_f64("KDMX", 1_000.0));
                assert_eq!(identity.elevation_number, 1);
                assert_eq!(identity.product, "reflectivity");
            }
            other => panic!("expected the stored sweep, got {other:?}"),
        }
    }

    // Only a flushed *and* completed current elevation counts: a partial
    // ack, a completion of some other cut, or no elevation at all does not.
    #[wasm_bindgen_test]
    fn completed_current_elevation_needs_flushed_and_completed() {
        assert_eq!(completed_current_elevation(&partial(1, 600)), None);
        let mut other = completing(1);
        other.current_elevation = Some(2);
        other.current_elevation_radials = Some(120);
        assert_eq!(completed_current_elevation(&other), None);
        let mut not_listed = completing(1);
        not_listed.elevations_completed.clear();
        assert_eq!(completed_current_elevation(&not_listed), None);
        assert_eq!(completed_current_elevation(&base_result(1_000.0)), None);
        assert_eq!(live_cut(Some(1), Some(0), Some(1_000_000)), None);
        assert_eq!(live_cut(Some(1), Some(120), None), None);
        assert_eq!(live_cut(Some(1), Some(120), Some(7)), Some((1, 7)));
    }

    // The next elevation's first chunk resumes live rendering.
    #[wasm_bindgen_test]
    fn live_render_resumes_on_next_elevation_after_completion() {
        let mut fx = Fx::new();
        assert_eq!(
            fx.run(env(true, false, &[1]), &completing(1)).render_live,
            None
        );
        let a = fx.run(env(true, true, &[1]), &partial(2, 120));
        assert_eq!(a.render_live, Some((2, "reflectivity".to_string())));
    }

    /// Run each ack through the reducer in drain order and coalesce the
    /// live renders the way the shell does; also report whether any ack
    /// asked for the stored sweep.
    fn drain(fx: &mut Fx, acks: &[ChunkIngestResult]) -> (Option<(u8, String)>, bool) {
        let mut coalescer = LiveRenderCoalescer::default();
        let mut stored = false;
        for ack in acks {
            let a = fx.run(env(true, false, &[1]), ack);
            stored |= a.request_render;
            coalescer.observe(a.render_live, &ack.elevations_completed);
        }
        (coalescer.finish(), stored)
    }

    fn refl(elev: u8) -> Option<(u8, String)> {
        Some((elev, "reflectivity".to_string()))
    }

    #[wasm_bindgen_test]
    fn coalesce_single_partial_ack_keeps_its_render() {
        let mut fx = Fx::new();
        assert_eq!(drain(&mut fx, &[partial(1, 120)]), (refl(1), false));
    }

    #[wasm_bindgen_test]
    fn coalesce_two_partials_of_one_elevation_render_once() {
        let mut fx = Fx::new();
        assert_eq!(
            drain(&mut fx, &[partial(1, 120), partial(1, 240)]),
            (refl(1), false)
        );
    }

    // The burst that used to raise "No radials for elevation 1 in
    // accumulator": by the time render_live(1) reached the worker, the
    // completing chunk had already flushed elevation 1.
    #[wasm_bindgen_test]
    fn coalesce_partial_then_completing_drops_live_and_keeps_stored_render() {
        let mut fx = Fx::new();
        assert_eq!(
            drain(&mut fx, &[partial(1, 600), partial(1, 680), completing(1)]),
            (None, true)
        );
    }

    #[wasm_bindgen_test]
    fn coalesce_partials_across_elevations_render_the_last() {
        let mut fx = Fx::new();
        assert_eq!(
            drain(&mut fx, &[partial(1, 600), partial(2, 120)]),
            (refl(2), false)
        );
        // Completing 1 then starting 2 in one drain: stored render for 1,
        // live render for 2.
        let mut fx = Fx::new();
        assert_eq!(
            drain(&mut fx, &[partial(1, 600), completing(1), partial(2, 120)]),
            (refl(2), true)
        );
    }

    type LiveTarget = Option<(u8, String)>;

    fn coalesce(drain: &[(LiveTarget, &[u8])]) -> LiveTarget {
        let mut coalescer = LiveRenderCoalescer::default();
        for (render_live, completed) in drain {
            coalescer.observe(render_live.clone(), completed);
        }
        coalescer.finish()
    }

    #[wasm_bindgen_test]
    fn coalesce_folds_render_and_completion_pairs() {
        assert_eq!(coalesce(&[]), None);
        assert_eq!(coalesce(&[(refl(1), &[][..]), (None, &[1][..])]), None);
        // A completion of some other elevation leaves the pending one.
        assert_eq!(coalesce(&[(refl(2), &[][..]), (None, &[1][..])]), refl(2));
        // A target set in the same ack that completes an older one survives.
        assert_eq!(
            coalesce(&[(refl(1), &[][..]), (refl(2), &[1][..])]),
            refl(2)
        );
    }

    #[wasm_bindgen_test]
    fn each_site_coalesces_its_own_live_renders() {
        let ktlx = crate::data::SiteId::new("KTLX");
        let kinx = crate::data::SiteId::new("KINX");
        let mut coalescer = SiteLiveRenderCoalescer::default();
        coalescer.observe(&ktlx, refl(1), &[]);
        coalescer.observe(&kinx, refl(3), &[]);
        // KINX completing elevation 1 leaves KTLX's pending render alone.
        coalescer.observe(&kinx, None, &[1]);
        coalescer.observe(&ktlx, refl(2), &[]);
        assert_eq!(
            coalescer.finish(),
            vec![
                (ktlx, 2, "reflectivity".to_string()),
                (kinx, 3, "reflectivity".to_string()),
            ]
        );
    }

    #[wasm_bindgen_test]
    fn a_site_whose_render_was_stored_posts_none() {
        let ktlx = crate::data::SiteId::new("KTLX");
        let mut coalescer = SiteLiveRenderCoalescer::default();
        coalescer.observe(&ktlx, refl(1), &[]);
        coalescer.observe(&ktlx, None, &[1]);
        assert!(coalescer.finish().is_empty());
    }

    #[wasm_bindgen_test]
    fn live_render_target_needs_an_elevation_with_radials_in_memory() {
        assert_eq!(live_render_target(Some(3), Some(240)), Some(3));
        assert_eq!(live_render_target(Some(3), Some(1)), Some(3));
        // Flushed (completed) elevation, or nothing counted yet.
        assert_eq!(live_render_target(Some(3), Some(0)), None);
        assert_eq!(live_render_target(Some(3), None), None);
        // Start chunk of a mid-volume join: no radials, no elevation.
        assert_eq!(live_render_target(None, Some(0)), None);
        assert_eq!(live_render_target(None, None), None);
    }

    // (3) Live volume end: promote, seal-before-reset, both
    // RefreshTimeline pushes + CheckEviction, mark done, fresh render but NO
    // worker render request (live renders come from the stream).
    #[wasm_bindgen_test]
    fn live_volume_end_seals_resets_and_promotes_without_render_request() {
        let mut fx = Fx::new();
        let playback_before = fx.playback.playback_position();
        // Mid-volume chunk establishes the anchor + VCP pattern.
        let mut first = base_result(1_000.0);
        first.current_elevation = Some(1);
        first.vcp = Some(vcp(&[0.5, 1.5, 3.0]));
        let _ = fx.run(env(true, false, &[]), &first);
        // Plan capture normally happens in Live::refresh; install directly.
        fx.live.volume_start_plan = Some(StreamingPlan::with_next_target_key_for_test(None));

        let mut end = base_result(1_000.0);
        end.is_end = true;
        end.elevations_completed = vec![2];
        let a = fx.run(env(true, true, &[1, 2]), &end);

        assert!(a.promote_prev_texture);
        assert_eq!(
            a.intents,
            vec![
                Intent::RefreshTimeline {
                    auto_position: false
                },
                Intent::RefreshTimeline {
                    auto_position: false
                },
                Intent::CheckEviction,
            ]
        );
        assert!(a.mark_processing_done);
        assert!(a.force_fresh_render);
        assert!(!a.request_render);
        assert!(!a.request_volume_render);
        // Live volume-end never moves the playhead.
        assert_eq!(fx.playback.playback_position(), playback_before);
        // Sealed on live_mode (seal-before-reset) at env.now_secs...
        let rec = fx.live.last_completed_volume.as_ref().expect("sealed");
        assert_eq!(rec.volume_end_secs, NOW);
        assert_eq!(rec.volume_start_secs, 1_000.0);
        assert!(fx.live.current_volume.is_none());
        // The focus site's volume end moves the session phase.
        assert_eq!(fx.session.phase, crate::core::LivePhase::Streaming);
        assert_eq!(fx.session.phase_started_at, Some(NOW));
        // ...and the engine observations were reset for the next volume.
        assert!(fx.engine.observations().current_vcp_pattern.is_none());
        assert!(fx.engine.observations().elevations_received.is_empty());
    }

    // (4) Archive volume end: playback jumps to now (applied by the reducer),
    // worker render requested, 3D volume render mirrors the toggle.
    #[wasm_bindgen_test]
    fn archive_volume_end_sets_playback_and_requests_render() {
        let mut fx = Fx::new();
        let mut end = base_result(1_000.0);
        end.is_end = true;

        let a = fx.run(env(false, true, &[1]), &end);

        // The reducer applies the playback jump to the slice directly.
        assert_eq!(fx.playback.playback_position(), NOW);
        assert!(a.force_fresh_render);
        assert!(a.request_render);
        assert!(!a.request_volume_render, "3D disabled → no volume render");
        assert!(a.mark_processing_done);
        assert!(!a.promote_prev_texture);
        assert_eq!(
            a.intents,
            vec![
                Intent::RefreshTimeline {
                    auto_position: true
                },
                Intent::CheckEviction,
            ]
        );

        // With the 3D toggle on, the volume render request mirrors it.
        let mut fx2 = Fx::new();
        let mut e = env(false, true, &[1]);
        e.volume_3d_active = true;
        let a2 = fx2.run(e, &end);
        assert!(a2.request_render);
        assert!(a2.request_volume_render);
    }

    // (5) First-elevation transition (archive, not end): initial render fires.
    #[wasm_bindgen_test]
    fn first_elevation_triggers_initial_render() {
        let mut fx = Fx::new();
        let mut r = base_result(1_000.0);
        r.elevations_completed = vec![1];

        let a = fx.run(env(false, false, &[1]), &r);

        assert!(a.force_fresh_render);
        assert!(a.request_render);
        assert!(!a.request_volume_render);
        assert!(!a.mark_processing_done);
        assert_eq!(
            a.intents,
            vec![Intent::RefreshTimeline {
                auto_position: true
            }]
        );
    }

    // (6) A VCP arrival remaps a fixed selection whose cut has no home in the
    // new pattern — and is idempotent, so the per-volume Start-chunk arrivals
    // (after `reset_volume_observations` wiped the previous pattern) never
    // churn a selection that's already valid.
    #[wasm_bindgen_test]
    fn vcp_arrival_remaps_homeless_selection_and_is_idempotent() {
        let mut fx = Fx::new();
        fx.sel = ElevationSelection::Fixed {
            elevation_number: 9,
            angle: 1.4,
        };
        let mut r = base_result(1_000.0);
        r.vcp = Some(vcp(&[0.5, 1.5, 3.0]));

        let _ = fx.run(env(true, false, &[]), &r);

        // Closest angle to 1.4 in the new VCP is 1.5 → entry #2.
        assert_eq!(fx.sel.elevation_number(), Some(2));
        assert!((fx.sel.angle() - 1.5).abs() < 1e-6);

        // Same VCP on the next volume's Start chunk → the now-valid selection
        // is untouched (no per-volume flicker).
        let _ = fx.run(env(true, false, &[]), &r);
        assert_eq!(fx.sel.elevation_number(), Some(2));

        // A selection that already lives in the pattern is never rewritten.
        fx.sel = ElevationSelection::Fixed {
            elevation_number: 3,
            angle: 3.0,
        };
        let _ = fx.run(env(true, false, &[]), &r);
        assert_eq!(
            fx.sel.elevation_number(),
            Some(3),
            "valid selection must survive repeated VCP arrivals"
        );
    }

    // (7) chunk_max_time_secs: collection end always recorded; lag only when
    // an arrival carries the S3 Last-Modified stamp, and it back-fills.
    #[wasm_bindgen_test]
    fn collection_end_always_recorded_lag_only_with_s3_stamp() {
        // No arrival stat → collection end recorded, no lag, attach no-op.
        let mut fx = Fx::new();
        let mut r = base_result(1_000.0);
        r.chunk_max_time_secs = Some(1_000.0);
        let a = fx.run(env(true, false, &[]), &r);
        assert_eq!(a.record_chunk_collection_end_secs, Some(1_000.0));
        assert_eq!(a.record_availability_lag_secs, None);
        assert!(fx.live.chunk_arrivals.is_empty());

        // Arrival with S3 Last-Modified → lag recorded and back-filled.
        let mut fx2 = Fx::new();
        let mut stat = ChunkArrivalStat::minimal_for_test(1, 1_001.0);
        stat.s3_last_modified_at = Some(1_002.5);
        fx2.live.record_chunk_arrival(stat);
        let a2 = fx2.run(env(true, false, &[]), &r);
        assert_eq!(a2.record_chunk_collection_end_secs, Some(1_000.0));
        assert_eq!(a2.record_availability_lag_secs, Some(2.5));
        let last = fx2.live.chunk_arrivals.last().unwrap();
        assert_eq!(last.collection_time_secs, Some(1_000.0));
        assert_eq!(last.availability_lag_ms, Some(2_500));
    }
}
