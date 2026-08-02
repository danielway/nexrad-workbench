//! Pure per-frame playback-advance reducers — the decisions the shell's
//! `advance_playback` used to interleave with coordinator/GPU/worker calls.
//!
//! The frame is **two decision points separated by effects**, mirroring the
//! original inline control flow exactly:
//!
//! 1. [`reduce_advance_playback`] — macro frame-list rebuild + the scrub walk
//!    that keeps the coordinator's active scan in sync. The shell assembles a
//!    read-only [`AdvancePlaybackEnv`] snapshot plus mutable
//!    [`AdvancePlaybackSlices`] over the core state, calls the reducer, then
//!    executes the returned [`AdvancePlaybackActions`] in field order
//!    (active-scan sync → fresh-render → render request → volume request).
//! 2. [`decide_prefetch_and_caption`] — the near-boundary prefetch target and
//!    the canvas honesty caption. Runs on a **fresh** snapshot taken *after*
//!    step 1's effects executed, because the executed render request can blank
//!    `viz_state.displayed` (`clear_display_no_sweep`) and the active-scan
//!    sync moves `coordinator.scan_key()` — both of which the original code
//!    read after those calls.
//!
//! The reducers mutate only the core slices (playback/macro-frame state,
//! elevation selection, the scrub cache) and *describe* everything else.
//! Decision content they compose rather than re-implement:
//! [`crate::core::render::decide_prefetch_next_elevation`] (prefetch window +
//! target), [`crate::core::playback_manager::build_elevation_list`] (VCP
//! elevation roster), and [`crate::core::canvas::derive_canvas_caption`]
//! (honesty caption).
//!
//! Pattern exemplar: [`crate::core::worker_ingest`] (Env/Slices/Actions reducer
//! executed by the shell in field order).

use crate::core::canvas::{derive_canvas_caption, CanvasCaption};
use crate::core::{
    DisplayedSweep, ElevationSelection, MacroFrameInputs, PlaybackMode, PlaybackState,
    RadarProduct, RadarTimeline, RebuildCause, SweepIdentity,
};
use crate::data::ScanKey;

/// Per-frame inputs the scrub-detection cache compares against.
///
/// The reducer skips the O(scans) timeline search when none of these have
/// changed since the last frame; that's the whole point of the cache. The
/// active scan timestamp catches ingest-driven scan changes that happen
/// without playback movement.
#[derive(Default)]
pub(crate) struct ScrubCache {
    pub last_playback_ts: Option<f64>,
    pub last_elevation_selection: Option<ElevationSelection>,
    pub last_scan_count: usize,
    /// Active scan timestamp (sub-second Unix seconds) from
    /// `RenderCoordinator::scan_key`.
    pub last_active_scan_ts: Option<f64>,
}

/// Read-only frame context for the macro-rebuild + scrub decision,
/// shell-assembled at frame start.
pub(crate) struct AdvancePlaybackEnv<'a> {
    /// `live.mode_state.is_active()`.
    pub live_active: bool,
    /// `render.coordinator.has_worker()`.
    pub has_worker: bool,
    /// `viz_state.site_id`.
    pub site_id: &'a str,
    /// `viz_state.product`.
    pub product: RadarProduct,
    /// `viz_state.volume_3d_enabled`.
    pub volume_3d_enabled: bool,
    /// `render.coordinator.scan_key()` at frame start.
    pub coordinator_scan_key: Option<&'a ScanKey>,
    /// `crate::MAX_SCAN_AGE_SECS`.
    pub max_scan_age_secs: f64,
}

/// Mutable borrows of the core state the reducer updates directly.
pub(crate) struct AdvancePlaybackSlices<'a> {
    pub playback: &'a mut PlaybackState,
    pub elevation_selection: &'a mut ElevationSelection,
    pub scrub_cache: &'a mut ScrubCache,
}

/// Coordinator active-scan sync decided by the scrub walk.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ActiveScanSync {
    /// `self.set_active_scan(scan_key, elevations, scan_ts)` — full elevation
    /// list known.
    Set {
        scan_key: ScanKey,
        elevations: Vec<u8>,
        scan_ts: f64,
    },
    /// `self.advance_active_scan_chunk(scan_key, &[], scan_ts)` — scan with no
    /// sweeps yet (key only; the elevation list stays as-is).
    AdvanceChunkEmpty { scan_key: ScanKey, scan_ts: f64 },
    /// `self.clear_active_scan()` — the playhead left every cached scan.
    Clear,
}

/// Described effects of the macro-rebuild + scrub decision, executed by the
/// shell in this field order.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct AdvancePlaybackActions {
    /// Active-scan sync on the render coordinator.
    pub active_scan: Option<ActiveScanSync>,
    /// `render.coordinator.force_fresh_render()`.
    pub force_fresh_render: bool,
    /// Shell calls `request_worker_render()` (the unified display resolver).
    pub request_render: bool,
    /// Shell calls `request_worker_render_volume()`.
    pub request_volume_render: bool,
}

/// Rebuild the macro frame list when dirty and keep the coordinator's active
/// scan in sync while scrubbing. Pure: in-memory mutation of the slices only.
pub(crate) fn reduce_advance_playback(
    env: AdvancePlaybackEnv<'_>,
    slices: AdvancePlaybackSlices<'_>,
    timeline: &RadarTimeline,
) -> AdvancePlaybackActions {
    let AdvancePlaybackSlices {
        playback,
        elevation_selection,
        scrub_cache,
    } = slices;
    let mut actions = AdvancePlaybackActions::default();

    // Live mode no longer skips playback-driven renders: the unified
    // resolver (`request_worker_render`) runs in both modes so a cached
    // cut paints while streaming. Live still owns *acquisition* (the
    // archive "acquiring" hint never applies) and the active-scan tracking
    // + prefetch-next-sweep path stay archive-only.
    let live_active = env.live_active;

    // Rebuild macro frame list when dirty (elevation selection, bounds, or
    // scan count changed). Uses the *effective* mode so the list is also
    // built during a lookback replay (which frame-steps regardless of zoom).
    if playback.effective_playback_mode() == PlaybackMode::Macro {
        let product = env.product.to_worker_string();
        let inputs = MacroFrameInputs {
            elevation: elevation_selection.clone(),
            product,
            // No explicit loop/selection falls back to the visible window, not
            // the whole cache — see `macro_frame_bounds`.
            bounds: crate::core::macro_frame_bounds(
                playback.time_model.playback_bounds,
                playback.timeline_view_start,
                playback.view_width_secs(),
                playback.playback_position(),
            ),
            scan_count: timeline.scans.len(),
        };

        if let Some(cause) = playback.macro_playback.rebuild_cause(&inputs) {
            let frames = match &inputs.elevation {
                ElevationSelection::Fixed {
                    elevation_number, ..
                } => timeline.matching_sweep_end_times_by_number(
                    *elevation_number,
                    product,
                    inputs.bounds,
                ),
                ElevationSelection::Latest => timeline.all_sweep_end_times(product, inputs.bounds),
            };
            playback.macro_playback.store_rebuilt(inputs, frames);
            playback.sync_macro_frame_index();
            // When the elevation filter changes, snap playback_position
            // to the resolved frame so the canvas resolver picks a sweep
            // at the new elevation. Frames are sweep end-times and a
            // higher elevation's sweep starts after the previous one
            // ends — without snapping, the resolver's
            // `start_time <= playback_position` filter rejects every
            // sweep at the new elevation in the current scan, blanking
            // the canvas. Skip on bounds/scan_count changes so
            // streaming and selection edits don't teleport the cursor.
            if cause == RebuildCause::ElevationChanged {
                playback.snap_playback_to_macro_frame();
            }
        }

        // Detect manual seek: if playback position changed externally
        // (user clicked timeline, jog, etc.) re-sync frame index.
        let pos = playback.playback_position();
        let last_pos = playback.macro_playback.last_seen_position;
        if (pos - last_pos).abs() > 0.5 {
            playback.sync_macro_frame_index();
            playback.macro_playback.frame_accumulator = 0.0;
        }
        playback.macro_playback.last_seen_position = pos;
    }

    // Auto-load scan when scrubbing: find the most recent scan within 15 minutes.
    // In the worker architecture, this sends a render request directly —
    // the worker reads records from IDB, decodes the target elevation, and renders.
    //
    // In FixedTilt mode, we also detect intra-scan sweep changes: a scan may
    // contain multiple sweeps at the target elevation (e.g. VCP 215 has 0.5°
    // at both elevation_number 1 and 3). As playback advances past a new
    // sweep's start_time, we re-render with that sweep's elevation_number.
    {
        let playback_ts = playback.playback_position();

        // Skip the timeline walk when nothing that feeds the scrub
        // decision has moved since last frame. The O(scans) search
        // below used to run every frame even while paused; this lets
        // the idle case cost only a few comparisons.
        let scan_count = timeline.scans.len();
        let elev_sel = &*elevation_selection;
        let active_ts = env.coordinator_scan_key.map(|k| k.scan_start.as_secs_f64());
        let scrub_cache_hit = scrub_cache.last_playback_ts == Some(playback_ts)
            && scrub_cache.last_scan_count == scan_count
            && scrub_cache.last_active_scan_ts == active_ts
            && scrub_cache
                .last_elevation_selection
                .as_ref()
                .is_some_and(|cached| cached == elev_sel);

        if !scrub_cache_hit {
            scrub_cache.last_playback_ts = Some(playback_ts);
            scrub_cache.last_scan_count = scan_count;
            scrub_cache.last_active_scan_ts = active_ts;
            scrub_cache.last_elevation_selection = Some(elev_sel.clone());
        }

        if !scrub_cache_hit {
            // Identify the scan covering the playback position. The
            // resolver in `request_worker_render` then decides which
            // sweep within it to actually fetch — this reducer's
            // job is just to keep `RenderCoordinator.current_scan_key`
            // (and the elevation list / VCP-resolution) in sync.
            let scrub_action = timeline
                .find_recent_scan(playback_ts, env.max_scan_age_secs)
                .map(|scan| {
                    let scan_ts: f64 = scan.key_timestamp;
                    let mut elev_nums: Vec<u8> =
                        scan.sweeps.iter().map(|s| s.elevation_number).collect();
                    elev_nums.sort_unstable();
                    elev_nums.dedup();
                    let roster = crate::core::playback_manager::build_elevation_roster(scan);
                    (scan_ts, elev_nums, roster)
                });

            match scrub_action {
                Some((scan_ts, elev_nums, (elev_list, roster_source))) => {
                    if env.has_worker {
                        let scan_key = ScanKey::from_secs_f64(env.site_id, scan_ts);
                        let scan_changed = active_ts != Some(scan_ts);
                        // The live ingest path owns active-scan tracking
                        // while streaming; only archive playback mutates it
                        // here (and `force_fresh_render` would fight live
                        // dedup). The unified `request_worker_render` below
                        // still runs in both modes.
                        if scan_changed && !live_active {
                            // Active scan moved — refresh the cache snapshot
                            // with the key the coordinator will hold after
                            // the sync executes.
                            scrub_cache.last_active_scan_ts =
                                Some(scan_key.scan_start.as_secs_f64());
                            actions.active_scan = Some(if !elev_nums.is_empty() {
                                ActiveScanSync::Set {
                                    scan_key,
                                    elevations: elev_nums,
                                    scan_ts,
                                }
                            } else {
                                ActiveScanSync::AdvanceChunkEmpty { scan_key, scan_ts }
                            });
                            // Re-resolve the user's elevation selection only
                            // against a trusted (complete-VCP) roster — a
                            // partial cached-sweeps roster must never rewrite
                            // durable intent (it would snap e.g. elevation 7
                            // to 0.5° while a scan is still ingesting).
                            if let Some((elevation_number, angle)) = elevation_selection
                                .resolved_against_roster(&elev_list, roster_source)
                            {
                                *elevation_selection = ElevationSelection::Fixed {
                                    elevation_number,
                                    angle,
                                };
                            }
                            actions.force_fresh_render = true;
                        }
                        actions.request_render = true;
                        if env.volume_3d_enabled && !live_active {
                            actions.request_volume_render = true;
                        }
                    }
                }
                None => {
                    // The playhead drifted into an undownloaded region or
                    // gap. Per spec §11.2 (alignment §3) we DON'T blank on
                    // age — keep showing the most recent frame and surface
                    // the discrepancy via the canvas caption (the follow-up
                    // decision). Blanking stays correct only for
                    // site/product/elevation changes and cache wipes,
                    // which clear `displayed` on their own paths.
                    //
                    // Still drop the stale active-scan key so the resolver
                    // and prefetch don't keep targeting a scan the playhead
                    // has left — without re-clearing the GPU frame.
                    if active_ts.is_some() && !live_active {
                        actions.active_scan = Some(ActiveScanSync::Clear);
                    }
                }
            }
        }
    }

    actions
}

// ---------------------------------------------------------------------------
// Follow-up decision: prefetch + canvas caption
// ---------------------------------------------------------------------------

/// Read-only context for the follow-up decision. Assembled **after** the
/// [`AdvancePlaybackActions`] executed: `displayed` and `coordinator_scan_key`
/// must be re-read at that point — the executed render request can blank
/// `displayed`, and the active-scan sync moves the coordinator key.
pub(crate) struct FrameFollowupEnv<'a> {
    /// `live.mode_state.is_active()`.
    pub live_active: bool,
    /// `render.coordinator.has_worker()`.
    pub has_worker: bool,
    /// `viz_state.product`.
    pub product: RadarProduct,
    /// `viz_state.displayed`, re-read post-effects.
    pub displayed: Option<&'a DisplayedSweep>,
    /// `render.coordinator.scan_key()`, re-read post-effects.
    pub coordinator_scan_key: Option<&'a ScanKey>,
    /// `state.download_progress` ghost ranges `(start, end)` in Unix seconds:
    /// active, in-flight, and pending downloads. Chained for the
    /// "Acquiring…" coverage test.
    pub active_download_ranges: &'a [(i64, i64)],
    pub in_flight_download_ranges: &'a [(i64, i64)],
    pub pending_download_ranges: &'a [(i64, i64)],
    /// `crate::MAX_SCAN_AGE_SECS`.
    pub max_scan_age_secs: f64,
    /// `crate::PREFETCH_LOOKAHEAD_SECS` (real seconds; scaled by playback
    /// speed inside the decision).
    pub prefetch_lookahead_secs: f64,
}

/// A near-boundary prefetch of the upcoming sweep.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PrefetchSweep {
    /// Stored in the dedup cache (`set_last_render`) and dispatched via
    /// `render_direct`.
    pub identity: SweepIdentity,
    /// Seconds until the current sweep ends — the "{:.1}s ahead" in the
    /// prefetch debug log.
    pub lead_secs: f64,
}

/// Described effects of the follow-up decision, executed by the shell in this
/// field order.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct FrameFollowup {
    /// Prefetch log + `set_last_render` + `render_direct`.
    pub prefetch: Option<PrefetchSweep>,
    /// Assign `viz_state.canvas_caption` (recomputed every frame).
    pub canvas_caption: CanvasCaption,
}

/// Whether an archive fetch covering `playback_secs` is in flight or being
/// ingested — drives the canvas "Acquiring…" hint when the position has no
/// cached scan yet. Checks the download-progress ghost ranges, which mirror
/// the active and just-completed-but-still-ingesting downloads.
pub(crate) fn position_is_being_acquired(
    playback_secs: f64,
    active: &[(i64, i64)],
    in_flight: &[(i64, i64)],
    pending: &[(i64, i64)],
) -> bool {
    let pos = playback_secs as i64;
    active
        .iter()
        .chain(in_flight.iter())
        .chain(pending.iter())
        .any(|&(start, end)| pos >= start && pos <= end)
}

/// Decide the near-boundary prefetch and the canvas honesty caption. Pure and
/// read-only (no slices — nothing here mutates core state).
pub(crate) fn decide_prefetch_and_caption(
    env: FrameFollowupEnv<'_>,
    playback: &PlaybackState,
    timeline: &RadarTimeline,
) -> FrameFollowup {
    let mut followup = FrameFollowup::default();

    // Pre-render next sweep: when playing and near the end of the current sweep,
    // preemptively send a render request for the upcoming sweep so the result
    // is ready when the boundary is crossed, reducing perceived stutter.
    // Skip in macro mode — frame jumps are instant and the frame list handles sequencing.
    if playback.playing
        && !env.live_active
        && env.has_worker
        && playback.playback_mode() == PlaybackMode::Micro
    {
        let playback_ts = playback.playback_position();
        let speed = playback.speed.timeline_seconds_per_real_second();
        let prefetch_lookahead = env.prefetch_lookahead_secs * speed;

        if let Some(scan) = timeline.find_scan_at_timestamp(playback_ts) {
            if let Some((sweep_idx, sweep)) = scan.find_sweep_at_timestamp(playback_ts) {
                let sweep_end = sweep.end_time;
                let cur_elev = env.displayed.map(|d| d.identity.elevation_number);
                // Only the last-sweep-in-scan case consults the next scan;
                // mirror that so we don't do an extra timeline walk.
                let future_scan = if sweep_idx + 1 < scan.sweeps.len() {
                    None
                } else {
                    timeline.find_scan_at_timestamp(playback_ts + prefetch_lookahead)
                };
                let next_elev = crate::core::render::decide_prefetch_next_elevation(
                    scan,
                    sweep_idx,
                    sweep_end,
                    playback_ts,
                    prefetch_lookahead,
                    future_scan,
                    cur_elev,
                );

                if let Some(next_en) = next_elev {
                    if let Some(scan_key) = env.coordinator_scan_key {
                        let product = env.product.to_worker_string().to_string();
                        followup.prefetch = Some(PrefetchSweep {
                            identity: SweepIdentity::new(scan_key.clone(), next_en, product),
                            lead_secs: sweep_end - playback_ts,
                        });
                    }
                }
            }
        }
    }

    // Canvas honesty caption (spec §11.2). Derived here where the displayed
    // identity, playhead, and download-progress ranges are all in scope. The
    // live partial path owns the canvas while the playhead is attached
    // (pinned/lookback), so the caption is suppressed there — but a detached
    // background stream resolves the cached frame at the cursor, so the
    // caption applies just like ordinary archive browsing.
    let pos = playback.playback_position();
    let attached = playback.time_model.is_pinned() || playback.time_model.is_lookback();
    let displayed = env
        .displayed
        .map(|d| (d.start_time, d.end_time, (d.start_time + d.end_time) / 2.0));
    let scan_covers_playhead = timeline
        .find_recent_scan(pos, env.max_scan_age_secs)
        .is_some();
    let fetch_covers_playhead = position_is_being_acquired(
        pos,
        env.active_download_ranges,
        env.in_flight_download_ranges,
        env.pending_download_ranges,
    );
    followup.canvas_caption = derive_canvas_caption(
        attached,
        displayed,
        pos,
        scan_covers_playhead,
        fetch_covers_playhead,
    );

    followup
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::CanvasCaption;
    use crate::core::{RadarTimeline, Scan, Sweep, TimelineTier};
    use crate::data::{ExtractedVcp, ExtractedVcpElevation};
    use wasm_bindgen_test::wasm_bindgen_test;

    const MAX_AGE: f64 = 15.0 * 60.0;

    // ── builders ────────────────────────────────────────────────────────────

    fn sweep(elev_num: u8, start: f64, end: f64, products: Vec<&str>) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: elev_num as f32 * 0.5,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: products.into_iter().map(String::from).collect(),
        }
    }

    fn scan(key_ts: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: key_ts,
            end_time: key_ts + 300.0,
            key_timestamp: key_ts,
            vcp: 215,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    /// Two-scan archive timeline: elev 1 + elev 2 in each volume, both
    /// carrying reflectivity blobs.
    fn two_scan_timeline() -> RadarTimeline {
        RadarTimeline {
            scans: vec![
                scan(
                    1000.0,
                    vec![
                        sweep(1, 1000.0, 1010.0, vec!["reflectivity"]),
                        sweep(2, 1010.0, 1020.0, vec!["reflectivity"]),
                    ],
                ),
                scan(
                    2000.0,
                    vec![
                        sweep(1, 2000.0, 2010.0, vec!["reflectivity"]),
                        sweep(2, 2010.0, 2020.0, vec!["reflectivity"]),
                    ],
                ),
            ],
        }
    }

    fn displayed_sweep(elev: u8, start: f64, end: f64) -> DisplayedSweep {
        DisplayedSweep {
            identity: SweepIdentity::new(
                ScanKey::from_secs_f64("KDMX", start),
                elev,
                "reflectivity",
            ),
            start_time: start,
            end_time: end,
            elevation_deg: 0.5,
        }
    }

    fn base_env<'a>() -> AdvancePlaybackEnv<'a> {
        AdvancePlaybackEnv {
            live_active: false,
            has_worker: true,
            site_id: "KDMX",
            product: RadarProduct::Reflectivity,
            volume_3d_enabled: false,
            coordinator_scan_key: None,
            max_scan_age_secs: MAX_AGE,
        }
    }

    fn base_followup_env<'a>() -> FrameFollowupEnv<'a> {
        FrameFollowupEnv {
            live_active: false,
            has_worker: true,
            product: RadarProduct::Reflectivity,
            displayed: None,
            coordinator_scan_key: None,
            active_download_ranges: &[],
            in_flight_download_ranges: &[],
            pending_download_ranges: &[],
            max_scan_age_secs: MAX_AGE,
            prefetch_lookahead_secs: 0.5,
        }
    }

    /// Core-state bundle the reducer mutates.
    struct Fx {
        playback: PlaybackState,
        selection: ElevationSelection,
        scrub: ScrubCache,
    }

    impl Fx {
        fn at(pos: f64, tier: TimelineTier) -> Self {
            let mut playback = PlaybackState::default();
            playback.set_playback_position(pos);
            playback.timeline_tier = tier;
            // The default view sits at wall-clock now, decades away from these
            // fixtures' epoch-relative timestamps. Center it on the playhead so
            // the macro frame list's visible-window fallback
            // (`macro_frame_bounds`) sees a realistic view, as it always does
            // in the app.
            playback.center_view_on(pos);
            Self {
                playback,
                selection: ElevationSelection::default(),
                scrub: ScrubCache::default(),
            }
        }

        fn run(
            &mut self,
            env: AdvancePlaybackEnv<'_>,
            timeline: &RadarTimeline,
        ) -> AdvancePlaybackActions {
            reduce_advance_playback(
                env,
                AdvancePlaybackSlices {
                    playback: &mut self.playback,
                    elevation_selection: &mut self.selection,
                    scrub_cache: &mut self.scrub,
                },
                timeline,
            )
        }

        fn followup(&self, env: FrameFollowupEnv<'_>, timeline: &RadarTimeline) -> FrameFollowup {
            decide_prefetch_and_caption(env, &self.playback, timeline)
        }
    }

    // ── macro frame-list rebuild ────────────────────────────────────────────

    // (1) First macro-tier frame with a fresh state rebuilds the frame list
    // (product differs from the default inputs → window-class change), syncs
    // the frame index to the playhead, and does NOT snap the position.
    #[wasm_bindgen_test]
    fn macro_rebuild_stores_frames_and_syncs_index() {
        let tl = two_scan_timeline();
        let mut fx = Fx::at(2005.0, TimelineTier::Macro);

        let a = fx.run(base_env(), &tl);

        // Fixed elev 1 (default selection) → frames are elev-1 end times.
        assert_eq!(
            fx.playback.macro_playback.sweep_frames,
            vec![1010.0, 2010.0]
        );
        assert_eq!(fx.playback.macro_playback.current_frame_index, 1);
        assert_eq!(fx.playback.macro_playback.last_seen_position, 2005.0);
        // Window-class rebuild: the cursor must NOT teleport.
        assert_eq!(fx.playback.playback_position(), 2005.0);
        // The scrub walk still ran (fresh scan under the playhead).
        assert!(a.request_render);
    }

    // (2) An elevation-selection change is the one rebuild cause that snaps
    // the playback position to the resolved frame.
    #[wasm_bindgen_test]
    fn macro_elevation_change_snaps_playback_to_frame() {
        let tl = two_scan_timeline();
        let mut fx = Fx::at(2005.0, TimelineTier::Macro);
        let _ = fx.run(base_env(), &tl); // establish built_from (elev 1)

        fx.selection = ElevationSelection::Fixed {
            elevation_number: 2,
            angle: 0.9,
        };
        let _ = fx.run(base_env(), &tl);

        // Frames now follow elev 2; index resolves to 2020 and the position
        // snaps onto it.
        assert_eq!(
            fx.playback.macro_playback.sweep_frames,
            vec![1020.0, 2020.0]
        );
        assert_eq!(fx.playback.macro_playback.current_frame_index, 1);
        assert_eq!(fx.playback.playback_position(), 2020.0);
    }

    // (3) A scan-count change rebuilds without snapping (streaming must not
    // teleport the cursor).
    #[wasm_bindgen_test]
    fn macro_scan_count_change_rebuilds_without_snap() {
        let tl = two_scan_timeline();
        let mut fx = Fx::at(2005.0, TimelineTier::Macro);
        let _ = fx.run(base_env(), &tl);

        let mut grown = two_scan_timeline();
        grown.scans.push(scan(
            3000.0,
            vec![sweep(1, 3000.0, 3010.0, vec!["reflectivity"])],
        ));
        let _ = fx.run(base_env(), &grown);

        assert_eq!(
            fx.playback.macro_playback.sweep_frames,
            vec![1010.0, 2010.0, 3010.0]
        );
        assert_eq!(fx.playback.playback_position(), 2005.0, "no snap");
    }

    // (4) Manual seek (position moved externally by >0.5s) re-syncs the frame
    // index and zeroes the sub-frame accumulator.
    #[wasm_bindgen_test]
    fn macro_manual_seek_resyncs_index_and_resets_accumulator() {
        let tl = two_scan_timeline();
        let mut fx = Fx::at(2005.0, TimelineTier::Macro);
        let _ = fx.run(base_env(), &tl);
        assert_eq!(fx.playback.macro_playback.current_frame_index, 1);

        fx.playback.set_playback_position(1010.0); // user clicked the timeline
        fx.playback.macro_playback.frame_accumulator = 0.7;
        let _ = fx.run(base_env(), &tl);

        assert_eq!(fx.playback.macro_playback.current_frame_index, 0);
        assert_eq!(fx.playback.macro_playback.frame_accumulator, 0.0);
        assert_eq!(fx.playback.macro_playback.last_seen_position, 1010.0);
    }

    // (5) Micro tier skips the macro block entirely.
    #[wasm_bindgen_test]
    fn micro_tier_skips_macro_rebuild() {
        let tl = two_scan_timeline();
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);
        let _ = fx.run(base_env(), &tl);

        assert!(fx.playback.macro_playback.sweep_frames.is_empty());
        assert_eq!(fx.playback.macro_playback.last_seen_position, 0.0);
    }

    // ── scrub walk / active-scan sync ───────────────────────────────────────

    // (6) When nothing feeding the scrub decision moved, the frame is a
    // cache hit: no actions at all (the default Actions).
    #[wasm_bindgen_test]
    fn scrub_cache_hit_produces_no_actions() {
        let tl = two_scan_timeline();
        let key = ScanKey::from_secs_f64("KDMX", 2000.0);
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);
        fx.scrub.last_playback_ts = Some(2005.0);
        fx.scrub.last_scan_count = 2;
        fx.scrub.last_active_scan_ts = Some(2000.0);
        fx.scrub.last_elevation_selection = Some(fx.selection.clone());

        let mut env = base_env();
        env.coordinator_scan_key = Some(&key);
        let a = fx.run(env, &tl);

        assert_eq!(a, AdvancePlaybackActions::default());
    }

    // (7) Scan under the playhead differs from the active scan (archive):
    // Set with sorted+deduped elevations, fresh-render, render request, and
    // the cache snapshot pre-refreshed to the new key.
    #[wasm_bindgen_test]
    fn scrub_new_scan_sets_active_scan_and_requests_render() {
        // Sweeps deliberately out of order + duplicated elevation number.
        let tl = RadarTimeline {
            scans: vec![scan(
                2000.0,
                vec![
                    sweep(2, 2010.0, 2020.0, vec!["reflectivity"]),
                    sweep(1, 2000.0, 2010.0, vec!["reflectivity"]),
                    sweep(2, 2020.0, 2030.0, vec!["reflectivity"]),
                ],
            )],
        };
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);
        fx.selection = ElevationSelection::Latest; // roster resolution no-op

        let a = fx.run(base_env(), &tl);

        assert_eq!(
            a.active_scan,
            Some(ActiveScanSync::Set {
                scan_key: ScanKey::from_secs_f64("KDMX", 2000.0),
                elevations: vec![1, 2],
                scan_ts: 2000.0,
            })
        );
        assert!(a.force_fresh_render);
        assert!(a.request_render);
        assert!(!a.request_volume_render);
        // Cache snapshot reflects the post-sync coordinator key.
        assert_eq!(fx.scrub.last_active_scan_ts, Some(2000.0));
        assert_eq!(fx.scrub.last_playback_ts, Some(2005.0));
        assert_eq!(fx.scrub.last_scan_count, 1);
    }

    // (8) On scan change the fixed elevation selection re-resolves against
    // the new scan's VCP list; the scrub cache keeps the PRE-resolve
    // selection (exactly what the old inline order produced).
    #[wasm_bindgen_test]
    fn scrub_scan_change_resolves_selection_for_vcp() {
        let mut s = scan(2000.0, vec![sweep(1, 2000.0, 2010.0, vec!["reflectivity"])]);
        s.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: [0.5f32, 1.5, 3.0]
                .iter()
                .map(|&angle| ExtractedVcpElevation {
                    angle,
                    waveform: "CS".to_string(),
                    prf_number: 1,
                    is_sails: false,
                    is_mrle: false,
                    is_base_tilt: false,
                    azimuth_rate: None,
                })
                .collect(),
        });
        let tl = RadarTimeline { scans: vec![s] };

        let mut fx = Fx::at(2005.0, TimelineTier::Micro);
        let stale = ElevationSelection::Fixed {
            elevation_number: 9,
            angle: 1.4,
        };
        fx.selection = stale.clone();

        let _ = fx.run(base_env(), &tl);

        // Closest angle to 1.4 in the new VCP is 1.5 → entry #2.
        assert_eq!(fx.selection.elevation_number(), Some(2));
        assert!((fx.selection.angle() - 1.5).abs() < 1e-6);
        // Cache holds the selection as it was when the walk started.
        // (ElevationSelection has no Debug derive — compare via PartialEq.)
        assert!(fx.scrub.last_elevation_selection == Some(stale));
    }

    // (8b) A scan with no VCP metadata yields a cached-sweeps-only roster,
    // which must never rewrite the user's fixed selection — the QA regression
    // where elevation 7 snapped to 0.5° on every scan boundary mid-ingest.
    #[wasm_bindgen_test]
    fn scrub_scan_change_keeps_selection_on_untrusted_roster() {
        // Only the 0.5° cut is cached and there's no vcp_pattern / known VCP
        // (vcp = 0 mirrors `RadarTimeline::from_metadata` when extraction
        // hasn't landed) → the roster degenerates to cached sweeps only.
        let mut s = scan(2000.0, vec![sweep(1, 2000.0, 2010.0, vec!["reflectivity"])]);
        s.vcp = 0;
        let tl = RadarTimeline { scans: vec![s] };
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);
        fx.selection = ElevationSelection::Fixed {
            elevation_number: 7,
            angle: 6.4,
        };

        let a = fx.run(base_env(), &tl);

        // The scan sync itself still happens…
        assert!(a.force_fresh_render);
        // …but the selection survives untouched.
        assert_eq!(fx.selection.elevation_number(), Some(7));
        assert!((fx.selection.angle() - 6.4).abs() < 1e-6);
    }

    // (9) Same scan already active: no sync, no fresh render — just the
    // unified render request.
    #[wasm_bindgen_test]
    fn scrub_same_scan_only_requests_render() {
        let tl = two_scan_timeline();
        let key = ScanKey::from_secs_f64("KDMX", 2000.0);
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);

        let mut env = base_env();
        env.coordinator_scan_key = Some(&key);
        let a = fx.run(env, &tl);

        assert!(a.active_scan.is_none());
        assert!(!a.force_fresh_render);
        assert!(a.request_render);
    }

    // (10) Scan with no sweeps yet: key-only sync (AdvanceChunkEmpty), the
    // elevation list stays as-is.
    #[wasm_bindgen_test]
    fn scrub_sweepless_scan_advances_chunk_key_only() {
        let tl = RadarTimeline {
            scans: vec![scan(2000.0, Vec::new())],
        };
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);
        fx.selection = ElevationSelection::Latest;

        let a = fx.run(base_env(), &tl);

        assert_eq!(
            a.active_scan,
            Some(ActiveScanSync::AdvanceChunkEmpty {
                scan_key: ScanKey::from_secs_f64("KDMX", 2000.0),
                scan_ts: 2000.0,
            })
        );
        assert!(a.force_fresh_render);
        assert!(a.request_render);
    }

    // (11) Live streaming owns active-scan tracking: archive-only mutations
    // are suppressed but the unified render request still fires; the volume
    // (3D) render stays archive-only.
    #[wasm_bindgen_test]
    fn scrub_live_mode_suppresses_active_scan_and_volume() {
        let tl = two_scan_timeline();
        let key = ScanKey::from_secs_f64("KDMX", 1000.0); // differs from 2000
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);

        let mut env = base_env();
        env.live_active = true;
        env.volume_3d_enabled = true;
        env.coordinator_scan_key = Some(&key);
        let a = fx.run(env, &tl);

        assert!(a.active_scan.is_none());
        assert!(!a.force_fresh_render);
        assert!(a.request_render);
        assert!(
            !a.request_volume_render,
            "3D volume renders are archive-only"
        );
    }

    // (12) No worker: the walk decides nothing (but the cache still updates
    // so the idle frame stays cheap).
    #[wasm_bindgen_test]
    fn scrub_without_worker_decides_nothing() {
        let tl = two_scan_timeline();
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);

        let mut env = base_env();
        env.has_worker = false;
        let a = fx.run(env, &tl);

        assert!(a.active_scan.is_none());
        assert!(!a.request_render && !a.force_fresh_render && !a.request_volume_render);
        assert_eq!(fx.scrub.last_playback_ts, Some(2005.0));
    }

    // (13) Playhead in a gap: drop the stale active key (archive), never
    // during live, and not when there is no active key to drop.
    #[wasm_bindgen_test]
    fn scrub_gap_clears_active_scan_archive_only() {
        let empty = RadarTimeline { scans: Vec::new() };
        let key = ScanKey::from_secs_f64("KDMX", 1000.0);

        // Archive with an active key → Clear.
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);
        let mut env = base_env();
        env.coordinator_scan_key = Some(&key);
        let a = fx.run(env, &empty);
        assert_eq!(a.active_scan, Some(ActiveScanSync::Clear));
        assert!(!a.request_render);

        // Live keeps its key.
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);
        let mut env = base_env();
        env.live_active = true;
        env.coordinator_scan_key = Some(&key);
        let a = fx.run(env, &empty);
        assert!(a.active_scan.is_none());

        // No key → nothing to clear.
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);
        let a = fx.run(base_env(), &empty);
        assert!(a.active_scan.is_none());
    }

    // (14) The 3D volume render request mirrors the toggle on the archive
    // scrub path (independent of whether the scan changed).
    #[wasm_bindgen_test]
    fn scrub_volume_render_mirrors_3d_toggle() {
        let tl = two_scan_timeline();
        let key = ScanKey::from_secs_f64("KDMX", 2000.0);
        let mut fx = Fx::at(2005.0, TimelineTier::Micro);

        let mut env = base_env();
        env.volume_3d_enabled = true;
        env.coordinator_scan_key = Some(&key);
        let a = fx.run(env, &tl);

        assert!(a.active_scan.is_none(), "same scan: no sync needed");
        assert!(a.request_render);
        assert!(a.request_volume_render);
    }

    // ── prefetch (follow-up decision) ───────────────────────────────────────

    /// Playing micro-mode fixture parked 0.2s before the elev-1 sweep ends.
    fn playing_fx() -> Fx {
        let mut fx = Fx::at(1009.8, TimelineTier::Micro);
        fx.playback.playing = true;
        fx.playback.speed = crate::core::PlaybackSpeed::Realtime;
        fx
    }

    // (15) Near the sweep boundary while playing: prefetch the next sweep in
    // the scan under the current coordinator key.
    #[wasm_bindgen_test]
    fn prefetch_near_boundary_targets_next_sweep() {
        let tl = two_scan_timeline();
        let key = ScanKey::from_secs_f64("KDMX", 1000.0);
        let displayed = displayed_sweep(1, 1000.0, 1010.0);
        let fx = playing_fx();

        let mut env = base_followup_env();
        env.coordinator_scan_key = Some(&key);
        env.displayed = Some(&displayed);
        let f = fx.followup(env, &tl);

        let p = f.prefetch.expect("prefetch decided");
        assert_eq!(p.identity.scan_key, key);
        assert_eq!(p.identity.elevation_number, 2);
        assert_eq!(p.identity.product, "reflectivity");
        assert!((p.lead_secs - 0.2).abs() < 1e-9);
        // In-window playhead with a covering scan → no caption.
        assert_eq!(f.canvas_caption, CanvasCaption::None);
    }

    // (16) Every gate suppresses the prefetch: paused, live, macro mode,
    // no worker, no coordinator key, and next == displayed.
    #[wasm_bindgen_test]
    fn prefetch_gates_suppress() {
        let tl = two_scan_timeline();
        let key = ScanKey::from_secs_f64("KDMX", 1000.0);
        let displayed = displayed_sweep(1, 1000.0, 1010.0);

        // Paused.
        let mut fx = playing_fx();
        fx.playback.playing = false;
        let mut env = base_followup_env();
        env.coordinator_scan_key = Some(&key);
        env.displayed = Some(&displayed);
        assert!(fx.followup(env, &tl).prefetch.is_none());

        // Live.
        let fx = playing_fx();
        let mut env = base_followup_env();
        env.live_active = true;
        env.coordinator_scan_key = Some(&key);
        env.displayed = Some(&displayed);
        assert!(fx.followup(env, &tl).prefetch.is_none());

        // Macro mode.
        let mut fx = playing_fx();
        fx.playback.timeline_tier = TimelineTier::Macro;
        let mut env = base_followup_env();
        env.coordinator_scan_key = Some(&key);
        env.displayed = Some(&displayed);
        assert!(fx.followup(env, &tl).prefetch.is_none());

        // No worker.
        let fx = playing_fx();
        let mut env = base_followup_env();
        env.has_worker = false;
        env.coordinator_scan_key = Some(&key);
        env.displayed = Some(&displayed);
        assert!(fx.followup(env, &tl).prefetch.is_none());

        // No coordinator scan key (e.g. it was cleared this frame).
        let fx = playing_fx();
        let mut env = base_followup_env();
        env.displayed = Some(&displayed);
        assert!(fx.followup(env, &tl).prefetch.is_none());

        // Next sweep already displayed (elev 2) → churn guard.
        let displayed2 = displayed_sweep(2, 1010.0, 1020.0);
        let fx = playing_fx();
        let mut env = base_followup_env();
        env.coordinator_scan_key = Some(&key);
        env.displayed = Some(&displayed2);
        assert!(fx.followup(env, &tl).prefetch.is_none());
    }

    // (17) Composition across the two decision points: the scrub walk syncs
    // onto a new scan; the follow-up (fed the post-sync key, as the shell
    // re-reads it) prefetches within THAT scan — never the stale one.
    #[wasm_bindgen_test]
    fn prefetch_composes_with_post_sync_scan_key() {
        let tl = two_scan_timeline();
        let stale_key = ScanKey::from_secs_f64("KDMX", 500.0);
        let displayed = displayed_sweep(1, 1000.0, 1010.0);
        let mut fx = playing_fx();
        fx.selection = ElevationSelection::Latest;

        let mut env = base_env();
        env.coordinator_scan_key = Some(&stale_key);
        let a = fx.run(env, &tl);

        // The scrub walk re-synced onto the 1000s scan…
        let new_key = ScanKey::from_secs_f64("KDMX", 1000.0);
        assert!(matches!(
            a.active_scan,
            Some(ActiveScanSync::Set { ref scan_key, .. }) if *scan_key == new_key
        ));

        // …the shell executes that sync, then re-reads the coordinator key
        // for the follow-up, so the prefetch targets the new scan.
        let mut env = base_followup_env();
        env.coordinator_scan_key = Some(&new_key);
        env.displayed = Some(&displayed);
        let f = fx.followup(env, &tl);
        let p = f.prefetch.expect("prefetch decided");
        assert_eq!(p.identity.scan_key, new_key);
        assert_eq!(p.identity.elevation_number, 2);
    }

    // ── canvas caption (follow-up decision) ─────────────────────────────────

    // (18) Held frame + playhead drifted into a gap → discrepancy caption;
    // `fetching` reflects the download ghost ranges.
    #[wasm_bindgen_test]
    fn caption_discrepancy_with_and_without_fetch() {
        let empty = RadarTimeline { scans: Vec::new() };
        let displayed = displayed_sweep(1, 100.0, 200.0);
        let fx = Fx::at(700.0, TimelineTier::Micro);

        let mut env = base_followup_env();
        env.displayed = Some(&displayed);
        env.in_flight_download_ranges = &[(650, 750)];
        let f = fx.followup(env, &empty);
        assert_eq!(
            f.canvas_caption,
            CanvasCaption::Discrepancy {
                showing: 150.0,
                target: 700.0,
                fetching: true,
            }
        );

        let mut env = base_followup_env();
        env.displayed = Some(&displayed);
        let f = fx.followup(env, &empty);
        assert_eq!(
            f.canvas_caption,
            CanvasCaption::Discrepancy {
                showing: 150.0,
                target: 700.0,
                fetching: false,
            }
        );
    }

    // (19) Blank canvas + a pending fetch covering the playhead → the
    // "Acquiring…" hint; covered playhead → no caption at all.
    #[wasm_bindgen_test]
    fn caption_acquiring_and_covered_cases() {
        let empty = RadarTimeline { scans: Vec::new() };
        let fx = Fx::at(700.0, TimelineTier::Micro);
        let mut env = base_followup_env();
        env.pending_download_ranges = &[(600, 800)];
        let f = fx.followup(env, &empty);
        assert_eq!(f.canvas_caption, CanvasCaption::Acquiring);

        // A scan covering the playhead → None even with a held frame.
        let tl = two_scan_timeline();
        let displayed = displayed_sweep(1, 2000.0, 2010.0);
        let fx = Fx::at(2005.0, TimelineTier::Micro);
        let mut env = base_followup_env();
        env.displayed = Some(&displayed);
        let f = fx.followup(env, &tl);
        assert_eq!(f.canvas_caption, CanvasCaption::None);
    }

    // (20) Attached playhead (pinned live) suppresses the caption even with
    // a drifted held frame and no covering scan.
    #[wasm_bindgen_test]
    fn caption_suppressed_while_pinned_live() {
        let empty = RadarTimeline { scans: Vec::new() };
        let displayed = displayed_sweep(1, 100.0, 200.0);
        let mut fx = Fx::at(700.0, TimelineTier::Micro);
        fx.playback.enter_pinned_live(700.0);

        let mut env = base_followup_env();
        env.live_active = true;
        env.displayed = Some(&displayed);
        let f = fx.followup(env, &empty);
        assert_eq!(f.canvas_caption, CanvasCaption::None);
    }

    // (21) The acquisition-coverage predicate chains all three ghost-range
    // lists, includes both boundaries, and truncates fractional seconds.
    #[wasm_bindgen_test]
    fn position_acquired_chains_ranges_inclusive() {
        assert!(position_is_being_acquired(700.0, &[(700, 800)], &[], &[]));
        assert!(position_is_being_acquired(800.0, &[], &[(700, 800)], &[]));
        assert!(position_is_being_acquired(750.0, &[], &[], &[(700, 800)]));
        // 700.9 truncates to 700 — still inside [700, 800].
        assert!(position_is_being_acquired(700.9, &[], &[], &[(700, 800)]));
        // 699.9 truncates to 699 — outside.
        assert!(!position_is_being_acquired(699.9, &[(700, 800)], &[], &[]));
        assert!(!position_is_being_acquired(700.0, &[], &[], &[]));
    }
}
