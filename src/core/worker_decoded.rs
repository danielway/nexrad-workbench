//! Pure decode-outcome reducers — the decisions the shell's
//! `handle_decoded_outcome` / `handle_live_decoded_outcome` used to
//! interleave with the GPU upload block.
//!
//! The GPU upload itself (renderer lock, `update_data`, color table, storm
//! cell detection) is an effect and stays in the shell
//! (`crate::app::worker_results`); these reducers decide *whether* it runs
//! and what feeds it.
//!
//! Archived decode ([`reduce_decoded`]) is **one phase**: every decision is
//! known before the upload executes. The only GPU feedback — upload success —
//! gates the assignment of an already-decided value
//! ([`DecodedActions::displayed_on_upload`]), so no post-effect re-read is
//! needed. The shell assembles a read-only [`DecodedEnv`] snapshot plus
//! [`DecodedSlices`] over the sweep cache, calls the reducer, then executes
//! the returned [`DecodedActions`] in field order.
//!
//! Live decode is **two phases separated by the GPU upload**, preserving the
//! original log interleaving exactly ("Live decode:" → GPU upload logs →
//! "Live azimuth range:"): [`reduce_live_decoded`] decides the
//! attached-playhead effect set (env carries the GPU renderer's
//! `current_sweep_id`, shell-read, for the promote decision), and
//! [`reduce_live_decoded_azimuths`] applies the chronological azimuth
//! bookkeeping to [`LiveModeState`] after the upload.
//!
//! Pattern exemplars: [`crate::core::worker_ingest`] (Env/Slices/Actions
//! reducer, same worker-outcome family) and [`crate::core::render_loop`]
//! (two decision points separated by effects).

use crate::core::live_radar_model::LiveVolumeModel;
use crate::core::playback_manager::{
    resolve_desired_display, sweep_cache_key, CachedSweepData, DesiredDisplay, PlaybackManager,
};
use crate::core::{
    DecodeResult, DisplayedSweep, ElevationSelection, LiveModeState, RadarProduct, RadarTimeline,
    SweepIdentity,
};

// ---------------------------------------------------------------------------
// Archived decode (`WorkerOutcome::Decoded`)
// ---------------------------------------------------------------------------

/// Read-only frame context for one archived decode outcome, shell-assembled.
pub(crate) struct DecodedEnv<'a> {
    /// `state.dev_mode`.
    pub dev_mode: bool,
    /// `viz_state.site_id`.
    pub site_id: &'a str,
    /// `playback.state.playback_position()`.
    pub playback_position: f64,
    /// `viz_state.elevation_selection`.
    pub elevation_selection: &'a ElevationSelection,
    /// `viz_state.product`.
    pub product: RadarProduct,
    /// `crate::MAX_SCAN_AGE_SECS`.
    pub max_scan_age_secs: f64,
    /// `self.live_render_sources()` — the live collecting cut + anchor
    /// feeding [`resolve_desired_display`].
    pub live_cut: Option<(u8, i64)>,
    /// `state.effective_sweep_animation(&playback.state, streaming, collecting)`.
    pub sweep_animation: bool,
    /// `viz_state.storm_cells_visible`.
    pub storm_cells_visible: bool,
    /// `state.download_progress.in_flight_scans`, read BEFORE the shell
    /// executes the retain — the clear predicate is decided against it.
    pub in_flight_scans: &'a [(i64, i64)],
    /// `state.download_progress.pending_scans.is_empty()`.
    pub pending_scans_empty: bool,
}

/// Mutable borrows of the core state the reducer updates directly.
pub(crate) struct DecodedSlices<'a> {
    /// `render.playback_manager` — receives the cached sweep entry and the
    /// pending-prev-sweep clearing.
    pub playback_manager: &'a mut PlaybackManager,
}

/// Described effects the shell executes, in this field order.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct DecodedActions {
    /// dev-mode: `session_stats.record_render_time(x)`.
    pub record_render_time: Option<f64>,
    /// Run the main-slot GPU upload block — the result is the current scan.
    pub upload_to_gpu: bool,
    /// `set_current_sweep_id(Some(..))` inside the upload block.
    pub gpu_sweep_id: String,
    /// Re-run storm cell detection inside the upload block.
    pub run_storm_cells: bool,
    /// Assign to `viz_state.displayed` iff the shell's GPU upload succeeded.
    /// Always populated; only meaningful when `upload_to_gpu`.
    pub displayed_on_upload: Option<DisplayedSweep>,
    /// dev-mode: store `last_render_detail` (the shell adds the measured
    /// `gpu_upload_ms` to the result's worker-side timings).
    pub record_render_detail: bool,
    /// `download_progress.in_flight_scans.retain(|&(start, _)| start != x)`.
    pub remove_in_flight_scan_start: i64,
    /// After the retain, in-flight and pending are both empty →
    /// `download_progress.clear()`.
    pub clear_download_progress: bool,
    /// `update_overlay_from_sweep(start, end, display_angle)`.
    pub update_overlay: Option<(f64, f64, f32)>,
}

/// Apply one archived decode outcome to the sweep cache and describe the
/// effects. Pure: in-memory mutation of the slices only (plus `log::debug!`).
pub(crate) fn reduce_decoded(
    env: DecodedEnv<'_>,
    slices: DecodedSlices<'_>,
    timeline: &RadarTimeline,
    result: &DecodeResult,
) -> DecodedActions {
    let DecodedSlices { playback_manager } = slices;
    let mut actions = DecodedActions::default();

    log::debug!(
        "Decode complete: {}x{} (az x gates), {} radials, product={}, {:.0}ms",
        result.azimuth_count,
        result.gate_count,
        result.radial_count,
        result.product,
        result.total_ms,
    );

    if env.dev_mode {
        actions.record_render_time = Some(result.total_ms);
    }

    // Cache decoded data for stateless sweep animation
    let result_sweep_id = sweep_cache_key(
        &result.context.scan_key.to_storage_key(),
        result.context.elevation_number,
        &result.product,
    );
    playback_manager.cache_sweep(
        result_sweep_id.clone(),
        CachedSweepData {
            gate_values: result.gate_values.clone(),
            azimuths: result.azimuths.clone(),
            azimuth_count: result.azimuth_count,
            gate_count: result.gate_count,
            first_gate_range_km: result.first_gate_range_km,
            gate_interval_km: result.gate_interval_km,
            max_range_km: result.max_range_km,
            offset: result.offset,
            scale: result.scale,
            azimuth_spacing_deg: result.azimuth_spacing_deg,
            radial_times: result.radial_times.clone(),
            product: result.product.clone(),
        },
    );

    // Upload decoded data to GPU renderer — but only if this
    // result is for the currently displayed scan. Background
    // prev-sweep decodes are cached but not uploaded here;
    // sync_prev_sweep_texture picks them up next frame.
    // Only upload to the primary GPU texture if this result
    // matches what advance_playback intended: same scan key AND
    // same elevation number. Without the elevation check, SAILS
    // VCPs (duplicate 0.5° at elev 1 and 2) cause oscillation
    // where prefetch/sync requests fight the main render path.
    //
    // Re-run the resolver against current playback state and compare
    // identities: the result is for the main slot iff its identity
    // exactly matches what the resolver would request right now.
    // Stale results from rapid clicks or in-flight prefetches fail
    // this check and stay cached (for prev-sweep upload) without
    // clobbering the main GPU texture.
    let result_identity = SweepIdentity::new(
        result.context.scan_key.clone(),
        result.context.elevation_number,
        result.product.clone(),
    );
    // The result is for the main slot iff the unified resolver would
    // currently ask for exactly this cached sweep. When live is collecting
    // this cut the resolver returns `LivePartial`, so a cached `Decoded`
    // for it is *not* current and stays cached for prev-sweep use — it
    // never clobbers the live partial. This precedence replaces the old
    // `skip_gpu_upload = is_active()` mode flag: completed cached cuts now
    // upload during live, the actively-collecting cut does not.
    let desired = resolve_desired_display(
        env.site_id,
        env.playback_position,
        env.elevation_selection,
        env.product,
        timeline,
        env.max_scan_age_secs,
        env.live_cut,
    );
    let is_current_scan = desired == DesiredDisplay::Cached(result_identity.clone());
    if env.sweep_animation && !is_current_scan {
        log::debug!("[sweep-anim] cached bg decode: {}", result_sweep_id);
        // Clear pending tracker so sync_prev_sweep_texture can load from cache
        if playback_manager.pending_prev_sweep_key() == Some(&result_sweep_id) {
            playback_manager.set_pending_prev_sweep_key(None);
        }
    }
    actions.upload_to_gpu = is_current_scan;
    actions.gpu_sweep_id = result_sweep_id;
    actions.run_storm_cells = env.storm_cells_visible;

    // Capture the new on-GPU main-slot sweep identity. The
    // *previous-slot* identity is owned by `sync_prev_sweep_texture`
    // (archive) or `handle_live_decoded_outcome`'s promote branch
    // (live) — those represent what's in the prev-sweep GPU texture,
    // which is the time-ordered prior sweep, NOT the prior main upload.
    // Don't conflate the two: scrubbing across non-adjacent times
    // would otherwise mis-mark the timeline previous border.
    // Display angle for the cut: prefer the VCP target ("commanded")
    // angle so labels read 0.5° rather than 0.44° (the encoder
    // average wobbles a few hundredths of a degree per spin).
    let display_angle = timeline
        .find_scan_at_timestamp(result.context.scan_key.scan_start.as_secs_f64())
        .and_then(|scan| scan.target_elevation_angle(result.context.elevation_number))
        .unwrap_or(result.mean_elevation);

    actions.displayed_on_upload = Some(DisplayedSweep {
        identity: result_identity,
        start_time: result.sweep_start_secs,
        end_time: result.sweep_end_secs,
        elevation_deg: display_angle,
    });

    actions.record_render_detail = env.dev_mode;

    // Remove this scan from in-flight ghost tracking. The queue is
    // keyed by archive-derived i64 seconds; truncate the result's
    // scan-start (millis) at the same boundary.
    let result_scan_start_i64 = result.context.scan_key.scan_start.as_secs();
    actions.remove_in_flight_scan_start = result_scan_start_i64;
    // If no more in-flight or pending, fully clear progress. Decided
    // against the pre-retain snapshot: the retain leaves nothing iff
    // every entry carried this result's scan start.
    actions.clear_download_progress = env.pending_scans_empty
        && env
            .in_flight_scans
            .iter()
            .all(|&(start, _)| start == result_scan_start_i64);

    // Refine canvas overlay with precise decoded data
    if result.sweep_start_secs > 0.0 {
        actions.update_overlay = Some((
            result.sweep_start_secs,
            result.sweep_end_secs,
            display_angle,
        ));
    }

    actions
}

// ---------------------------------------------------------------------------
// Live decode (`WorkerOutcome::LiveDecoded`) — phase 1: display decisions
// ---------------------------------------------------------------------------

/// Read-only frame context for one live decode outcome, shell-assembled.
pub(crate) struct LiveDecodedEnv<'a> {
    /// `!self.live.is_detached(&self.playback.state)`.
    pub playhead_attached: bool,
    /// `live.radar_model.volume` — VCP target-angle lookup.
    pub live_volume: Option<&'a LiveVolumeModel>,
    /// The GPU renderer's `current_sweep_id()`, shell-read under the lock.
    /// Feeds the promote decision — GPU state enters as env, the decision
    /// lives here.
    pub gpu_current_sweep_id: Option<String>,
    /// `viz_state.storm_cells_visible`.
    pub storm_cells_visible: bool,
}

/// Described effects the shell executes, in this field order. When the
/// playhead is detached every field is suppressed (upload false, no
/// displayed roll, no overlay, no storm rerun) — only the azimuth
/// bookkeeping ([`reduce_live_decoded_azimuths`]) still runs.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct LiveDecodedActions {
    /// Run the GPU upload block (playhead attached).
    pub upload_to_gpu: bool,
    /// Inside the block: `promote_current_to_previous` BEFORE `update_data`,
    /// and roll `displayed` into `previous_displayed` after the upload.
    pub promote_prev_texture: bool,
    /// `set_current_sweep_id(Some(..))` inside the block — `"live|{elev}"`.
    pub gpu_sweep_id: String,
    /// New on-GPU identity. `promote_prev_texture` picks the roll semantics:
    /// promote → `displayed.replace(new)` with the prior moved into
    /// `previous_displayed`; else overwrite `displayed` in place.
    pub new_displayed: Option<DisplayedSweep>,
    /// Re-run storm cell detection inside the block.
    pub run_storm_cells: bool,
    /// `update_overlay_from_sweep(start, end, display_angle)`.
    pub update_overlay: Option<(f64, f64, f32)>,
}

/// Decide the live-decode display effects. Pure and read-only (plus
/// `log::debug!`); the azimuth bookkeeping is phase 2 so the shell's GPU
/// upload logs land between the two, exactly as the inline handler did.
pub(crate) fn reduce_live_decoded(
    env: LiveDecodedEnv<'_>,
    result: &DecodeResult,
) -> LiveDecodedActions {
    let mut actions = LiveDecodedActions::default();

    log::debug!(
        "Live decode: {}x{}, {} radials, {}, {:.0}ms",
        result.azimuth_count,
        result.gate_count,
        result.radial_count,
        result.product,
        result.total_ms,
    );

    // While the playhead is detached (browsing the archive with the
    // stream ingesting in the background), the canvas belongs to the
    // scrubbed position — skip the GPU upload, `displayed` mutation,
    // and overlay refresh. The azimuth bookkeeping (phase 2) still runs
    // so sweep compositing is correct the instant the user re-pins.
    let playhead_attached = env.playhead_attached;

    // VCP target angle for the cut (mirrors archived path).
    let display_angle = env
        .live_volume
        .and_then(|v| v.target_elevation_angle(result.context.elevation_number))
        .unwrap_or(result.mean_elevation);

    // Build a live sweep ID so we can detect elevation transitions
    let live_elev = result.context.elevation_number;
    let live_sweep_id = format!("live|{}", live_elev);

    if playhead_attached {
        // If the current texture has data from a different sweep
        // (complete or different live elevation), promote it to
        // previous so it becomes the background for compositing
        // partial data. The promote flag also rolls `displayed` into
        // `previous_displayed` (post-update_data in the shell), which
        // is the canonical source for overlay/timeline prev info.
        let should_promote = env
            .gpu_current_sweep_id
            .as_deref()
            .is_some_and(|id| id != live_sweep_id);

        actions.upload_to_gpu = true;
        actions.promote_prev_texture = should_promote;

        // Capture the live on-GPU identity. `should_promote` flags
        // an elevation transition within the live volume — only at
        // those boundaries is the prior `displayed` semantically a
        // *different* sweep, so we only roll it into
        // `previous_displayed` then. Repeated partial-sweep uploads
        // for the *same* elevation overwrite `displayed` in place.
        actions.new_displayed = Some(DisplayedSweep {
            identity: SweepIdentity::new(
                result.context.scan_key.clone(),
                result.context.elevation_number,
                result.product.clone(),
            ),
            start_time: result.sweep_start_secs,
            end_time: result.sweep_end_secs,
            elevation_deg: display_angle,
        });

        // Re-run storm cell detection on the freshly-uploaded live
        // sweep so the overlay tracks the incoming chunks rather
        // than freezing until the user toggles the feature.
        actions.run_storm_cells = env.storm_cells_visible;
    }
    actions.gpu_sweep_id = live_sweep_id;

    // Update overlay staleness so the age counter reflects
    // the most recently received live data.
    if playhead_attached && result.sweep_end_secs > 0.0 {
        actions.update_overlay = Some((
            result.sweep_start_secs,
            result.sweep_end_secs,
            display_angle,
        ));
    }

    actions
}

// ---------------------------------------------------------------------------
// Live decode — phase 2: azimuth bookkeeping
// ---------------------------------------------------------------------------

/// Store the chronological azimuth range for sweep compositing.
/// Must use chronological first/last (from radial timestamps), NOT
/// sorted min/max. Once a sweep wraps past 0°, the sorted range
/// spans ~360° and the shader thinks the entire circle has current
/// data, hiding the previous sweep.
///
/// Runs attached or detached, AFTER the shell's GPU upload — the
/// "Live azimuth range" log follows the GPU upload logs, matching the
/// original inline order.
pub(crate) fn reduce_live_decoded_azimuths(live_mode: &mut LiveModeState, result: &DecodeResult) {
    if !result.azimuths.is_empty() {
        // Chronological first = sweep start azimuth (set once per sweep).
        // Chronological last = most recent radial's azimuth from the live state.
        if live_mode.sweep_start_azimuth.is_none() {
            // First live decode for this sweep: use the earliest radial
            // by collection time as the sweep start.
            let first_az = if !result.radial_times.is_empty() {
                let min_time_idx = result
                    .radial_times
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                result.azimuths[min_time_idx]
            } else {
                result.azimuths[0]
            };
            live_mode.sweep_start_azimuth = Some(first_az);
        }

        // The trailing edge of received data: latest radial by collection time.
        let last_az = if !result.radial_times.is_empty() {
            let max_time_idx = result
                .radial_times
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                .map(|(i, _)| i)
                .unwrap_or(result.azimuths.len() - 1);
            result.azimuths[max_time_idx]
        } else {
            *result.azimuths.last().unwrap()
        };

        let first_az = live_mode.sweep_start_azimuth.unwrap_or(0.0);
        log::debug!(
            "Live azimuth range: chrono_first={:.1} chrono_last={:.1} count={}",
            first_az,
            last_az,
            result.azimuths.len(),
        );
        live_mode.live_data_azimuth_range = Some((first_az, last_az));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{RadarTimeline, RenderContext, Scan, Sweep, VolumeElevationRoster};
    use crate::data::{ExtractedVcp, ExtractedVcpElevation, ScanKey};
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

    /// One-scan timeline: elev 1 (1000–1010) + elev 2 (1010–1020), both
    /// with reflectivity blobs. VCP 215 → static-table target angles.
    fn timeline() -> RadarTimeline {
        RadarTimeline {
            scans: vec![scan(
                1000.0,
                vec![
                    sweep(1, 1000.0, 1010.0, vec!["reflectivity"]),
                    sweep(2, 1010.0, 1020.0, vec!["reflectivity"]),
                ],
            )],
        }
    }

    fn decode_result(scan_ts: f64, elev: u8, product: &str) -> DecodeResult {
        DecodeResult {
            context: RenderContext {
                scan_key: ScanKey::from_secs_f64("KDMX", scan_ts),
                elevation_number: elev,
            },
            azimuths: vec![0.0, 90.0, 180.0],
            gate_values: vec![10.0; 6],
            azimuth_count: 3,
            gate_count: 2,
            first_gate_range_km: 2.0,
            gate_interval_km: 0.25,
            max_range_km: 460.0,
            product: product.to_string(),
            radial_count: 3,
            fetch_ms: 1.0,
            deser_ms: 2.0,
            marshal_ms: 3.0,
            total_ms: 42.0,
            scale: 2.0,
            offset: 66.0,
            mean_elevation: 0.44,
            sweep_start_secs: scan_ts,
            sweep_end_secs: scan_ts + 10.0,
            radial_times: vec![scan_ts, scan_ts + 1.0, scan_ts + 2.0],
            azimuth_spacing_deg: 0.5,
        }
    }

    fn env<'a>(
        sel: &'a ElevationSelection,
        playback_position: f64,
        in_flight: &'a [(i64, i64)],
    ) -> DecodedEnv<'a> {
        DecodedEnv {
            dev_mode: false,
            site_id: "KDMX",
            playback_position,
            elevation_selection: sel,
            product: RadarProduct::Reflectivity,
            max_scan_age_secs: MAX_AGE,
            live_cut: None,
            sweep_animation: false,
            storm_cells_visible: false,
            in_flight_scans: in_flight,
            pending_scans_empty: true,
        }
    }

    fn fixed(elevation_number: u8) -> ElevationSelection {
        ElevationSelection::Fixed {
            elevation_number,
            angle: 0.5,
        }
    }

    fn run(
        pm: &mut PlaybackManager,
        env: DecodedEnv<'_>,
        tl: &RadarTimeline,
        result: &DecodeResult,
    ) -> DecodedActions {
        reduce_decoded(
            env,
            DecodedSlices {
                playback_manager: pm,
            },
            tl,
            result,
        )
    }

    // ── archived decode ─────────────────────────────────────────────────────

    // (1) Result matches the resolver's current target: upload, sweep id,
    // displayed identity with the VCP target angle (0.5°, not the 0.44°
    // encoder mean), overlay refresh, and the sweep cached.
    #[wasm_bindgen_test]
    fn decoded_current_identity_uploads_and_sets_displayed() {
        let tl = timeline();
        let sel = fixed(1);
        let mut pm = PlaybackManager::new();
        let r = decode_result(1000.0, 1, "reflectivity");

        let a = run(&mut pm, env(&sel, 1005.0, &[]), &tl, &r);

        assert!(a.upload_to_gpu);
        let key = ScanKey::from_secs_f64("KDMX", 1000.0);
        let expected_id = sweep_cache_key(&key.to_storage_key(), 1, "reflectivity");
        assert_eq!(a.gpu_sweep_id, expected_id);
        let d = a.displayed_on_upload.expect("displayed value decided");
        assert_eq!(d.identity, SweepIdentity::new(key, 1, "reflectivity"));
        assert_eq!(d.start_time, 1000.0);
        assert_eq!(d.end_time, 1010.0);
        assert_eq!(d.elevation_deg, 0.5, "VCP 215 target angle, not the mean");
        assert_eq!(a.update_overlay, Some((1000.0, 1010.0, 0.5)));
        assert!(pm.get_cached_sweep(&expected_id).is_some());
        // Non-dev frame: no timing effects.
        assert_eq!(a.record_render_time, None);
        assert!(!a.record_render_detail);
        assert!(!a.run_storm_cells);
    }

    // (2) SAILS-style duplicate elevation: the resolver targets elev 1 at
    // this position, so a result for elev 2 in the SAME scan is not current —
    // cached only, no upload (the oscillation guard the comments describe).
    #[wasm_bindgen_test]
    fn decoded_sails_elevation_mismatch_stays_cached() {
        let tl = timeline();
        let sel = fixed(1);
        let mut pm = PlaybackManager::new();
        let r = decode_result(1000.0, 2, "reflectivity");

        let a = run(&mut pm, env(&sel, 1005.0, &[]), &tl, &r);

        assert!(!a.upload_to_gpu);
        let key = ScanKey::from_secs_f64("KDMX", 1000.0);
        let id = sweep_cache_key(&key.to_storage_key(), 2, "reflectivity");
        assert!(
            pm.get_cached_sweep(&id).is_some(),
            "stale result stays cached"
        );
    }

    // (3) Sweep-animation bookkeeping on a non-current result: the pending
    // prev-sweep tracker clears iff it points at this result; a current
    // result never touches it, nor does a frame with animation off.
    #[wasm_bindgen_test]
    fn decoded_pending_prev_clears_only_for_bg_decode_with_animation() {
        let tl = timeline();
        let sel = fixed(1);
        let key = ScanKey::from_secs_f64("KDMX", 1000.0);
        let bg_id = sweep_cache_key(&key.to_storage_key(), 2, "reflectivity");

        // Animation on + non-current result matching the tracker → cleared.
        let mut pm = PlaybackManager::new();
        pm.set_pending_prev_sweep_key(Some(bg_id.clone()));
        let mut e = env(&sel, 1005.0, &[]);
        e.sweep_animation = true;
        let _ = run(&mut pm, e, &tl, &decode_result(1000.0, 2, "reflectivity"));
        assert_eq!(pm.pending_prev_sweep_key(), None);

        // Animation off → tracker untouched.
        let mut pm = PlaybackManager::new();
        pm.set_pending_prev_sweep_key(Some(bg_id.clone()));
        let _ = run(
            &mut pm,
            env(&sel, 1005.0, &[]),
            &tl,
            &decode_result(1000.0, 2, "reflectivity"),
        );
        assert_eq!(pm.pending_prev_sweep_key(), Some(bg_id.as_str()));

        // Current result → tracker untouched even when it matches.
        let cur_id = sweep_cache_key(&key.to_storage_key(), 1, "reflectivity");
        let mut pm = PlaybackManager::new();
        pm.set_pending_prev_sweep_key(Some(cur_id.clone()));
        let mut e = env(&sel, 1005.0, &[]);
        e.sweep_animation = true;
        let a = run(&mut pm, e, &tl, &decode_result(1000.0, 1, "reflectivity"));
        assert!(a.upload_to_gpu);
        assert_eq!(pm.pending_prev_sweep_key(), Some(cur_id.as_str()));
    }

    // (4) Live precedence: while live is collecting this exact cut the
    // resolver answers LivePartial, so the cached decode is NOT current —
    // it must never clobber the live partial on the GPU.
    #[wasm_bindgen_test]
    fn decoded_live_partial_precedence_blocks_upload() {
        let tl = timeline();
        let sel = fixed(1);
        let mut pm = PlaybackManager::new();
        let r = decode_result(1000.0, 1, "reflectivity");

        // Anchor ms of the 1000s scan = 1_000_000; live collecting elev 1.
        let mut e = env(&sel, 1005.0, &[]);
        e.live_cut = Some((1, 1_000_000));
        let a = run(&mut pm, e, &tl, &r);

        assert!(!a.upload_to_gpu, "live partial owns the collecting cut");
        let key = ScanKey::from_secs_f64("KDMX", 1000.0);
        let id = sweep_cache_key(&key.to_storage_key(), 1, "reflectivity");
        assert!(pm.get_cached_sweep(&id).is_some());

        // A different collecting cut (elev 2): the cached elev-1 result IS
        // current and uploads during live.
        let mut pm = PlaybackManager::new();
        let mut e = env(&sel, 1005.0, &[]);
        e.live_cut = Some((2, 1_000_000));
        let a = run(&mut pm, e, &tl, &r);
        assert!(a.upload_to_gpu, "completed cached cuts upload during live");
    }

    // (5) Display-angle fallback: no scan at the result's timestamp (or no
    // VCP entry) → the encoder mean; dev-mode gates the timing effects.
    #[wasm_bindgen_test]
    fn decoded_display_angle_falls_back_to_mean_and_dev_gates_timing() {
        let empty = RadarTimeline { scans: Vec::new() };
        let sel = fixed(1);
        let mut pm = PlaybackManager::new();
        let r = decode_result(1000.0, 1, "reflectivity");

        let mut e = env(&sel, 1005.0, &[]);
        e.dev_mode = true;
        e.storm_cells_visible = true;
        let a = run(&mut pm, e, &empty, &r);

        assert!(!a.upload_to_gpu, "no scan → resolver Blank → not current");
        let d = a.displayed_on_upload.expect("still decided");
        assert_eq!(d.elevation_deg, 0.44, "falls back to mean_elevation");
        assert_eq!(a.update_overlay, Some((1000.0, 1010.0, 0.44)));
        assert_eq!(a.record_render_time, Some(42.0));
        assert!(a.record_render_detail);
        assert!(a.run_storm_cells);

        // Unknown VCP number and no pattern → same fallback via the scan path.
        let mut s = scan(1000.0, vec![sweep(1, 1000.0, 1010.0, vec!["reflectivity"])]);
        s.vcp = 0;
        let tl = RadarTimeline { scans: vec![s] };
        let a = run(&mut pm, env(&sel, 1005.0, &[]), &tl, &r);
        assert_eq!(a.update_overlay, Some((1000.0, 1010.0, 0.44)));
    }

    // (6) Ghost retention: the retain key is the truncated scan start; the
    // clear predicate fires only when the retain empties in-flight AND
    // pending is already empty.
    #[wasm_bindgen_test]
    fn decoded_ghost_clear_predicate() {
        let tl = timeline();
        let sel = fixed(1);
        let r = decode_result(1000.0, 1, "reflectivity");

        // All in-flight entries belong to this scan → clear.
        let mut pm = PlaybackManager::new();
        let in_flight = [(1000_i64, 1300_i64), (1000, 1290)];
        let a = run(&mut pm, env(&sel, 1005.0, &in_flight), &tl, &r);
        assert_eq!(a.remove_in_flight_scan_start, 1000);
        assert!(a.clear_download_progress);

        // A foreign in-flight entry survives the retain → no clear.
        let in_flight = [(1000_i64, 1300_i64), (2000, 2300)];
        let a = run(&mut pm, env(&sel, 1005.0, &in_flight), &tl, &r);
        assert!(!a.clear_download_progress);

        // Pending scans present → no clear even when in-flight empties.
        let in_flight = [(1000_i64, 1300_i64)];
        let mut e = env(&sel, 1005.0, &in_flight);
        e.pending_scans_empty = false;
        let a = run(&mut pm, e, &tl, &r);
        assert!(!a.clear_download_progress);

        // Nothing tracked at all → clear (matches the inline empty&&empty).
        let a = run(&mut pm, env(&sel, 1005.0, &[]), &tl, &r);
        assert!(a.clear_download_progress);
    }

    // (7) Overlay refresh is gated on a positive sweep start (streamed
    // partials can carry 0) — and it fires even for non-current results.
    #[wasm_bindgen_test]
    fn decoded_overlay_gate_on_sweep_start() {
        let tl = timeline();
        let sel = fixed(1);
        let mut pm = PlaybackManager::new();
        let mut r = decode_result(1000.0, 2, "reflectivity");

        let a = run(&mut pm, env(&sel, 1005.0, &[]), &tl, &r);
        assert!(!a.upload_to_gpu);
        assert!(
            a.update_overlay.is_some(),
            "overlay refresh is not gated on upload"
        );

        r.sweep_start_secs = 0.0;
        let a = run(&mut pm, env(&sel, 1005.0, &[]), &tl, &r);
        assert_eq!(a.update_overlay, None);
    }

    // ── live decode: phase 1 ────────────────────────────────────────────────

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

    fn live_volume(angles: &[f32]) -> LiveVolumeModel {
        LiveVolumeModel {
            vcp_pattern: Some(ExtractedVcp {
                number: 215,
                elevations: angles.iter().map(|&a| extracted_elev(a)).collect(),
            }),
            roster: VolumeElevationRoster::default(),
        }
    }

    fn live_env<'a>(
        playhead_attached: bool,
        live_volume: Option<&'a LiveVolumeModel>,
        gpu_current_sweep_id: Option<&str>,
    ) -> LiveDecodedEnv<'a> {
        LiveDecodedEnv {
            playhead_attached,
            live_volume,
            gpu_current_sweep_id: gpu_current_sweep_id.map(str::to_string),
            storm_cells_visible: false,
        }
    }

    // (8) Detached playhead suppresses every display effect; only the live
    // sweep id is still derived (it feeds nothing while detached).
    #[wasm_bindgen_test]
    fn live_detached_suppresses_display_effects() {
        let vol = live_volume(&[0.5, 1.5]);
        let r = decode_result(1000.0, 1, "reflectivity");
        let mut e = live_env(false, Some(&vol), Some("KDMX|1|1|reflectivity"));
        e.storm_cells_visible = true;

        let a = reduce_live_decoded(e, &r);

        assert!(!a.upload_to_gpu);
        assert!(!a.promote_prev_texture);
        assert_eq!(a.new_displayed, None);
        assert!(!a.run_storm_cells);
        assert_eq!(a.update_overlay, None);
        assert_eq!(a.gpu_sweep_id, "live|1");
    }

    // (9) Attached first upload (blank GPU): no promote, displayed decided
    // with the VCP target angle, overlay refresh with the same angle.
    #[wasm_bindgen_test]
    fn live_attached_first_upload_no_promote() {
        let vol = live_volume(&[0.5, 1.5]);
        let r = decode_result(1000.0, 2, "reflectivity");

        let a = reduce_live_decoded(live_env(true, Some(&vol), None), &r);

        assert!(a.upload_to_gpu);
        assert!(!a.promote_prev_texture, "blank GPU: nothing to promote");
        assert_eq!(a.gpu_sweep_id, "live|2");
        let d = a.new_displayed.expect("displayed decided");
        assert_eq!(
            d.identity,
            SweepIdentity::new(ScanKey::from_secs_f64("KDMX", 1000.0), 2, "reflectivity")
        );
        assert_eq!(d.elevation_deg, 1.5, "VCP target for elev 2");
        assert_eq!(a.update_overlay, Some((1000.0, 1010.0, 1.5)));
    }

    // (10) Promote decision: a different sweep on the GPU (prior live
    // elevation or a completed cached cut) promotes; the same live id
    // overwrites in place.
    #[wasm_bindgen_test]
    fn live_promote_on_sweep_transition_only() {
        let r = decode_result(1000.0, 2, "reflectivity");

        // Prior live elevation → promote.
        let a = reduce_live_decoded(live_env(true, None, Some("live|1")), &r);
        assert!(a.promote_prev_texture);

        // Completed cached cut on the GPU → promote.
        let a = reduce_live_decoded(
            live_env(true, None, Some("KDMX|1000000|2|reflectivity")),
            &r,
        );
        assert!(a.promote_prev_texture);

        // Same live sweep → in-place overwrite.
        let a = reduce_live_decoded(live_env(true, None, Some("live|2")), &r);
        assert!(!a.promote_prev_texture);
        assert!(a.upload_to_gpu);
    }

    // (11) Display-angle fallbacks (live variant): no volume model, or a
    // pattern without the requested cut → the encoder mean. Storm rerun
    // mirrors the visibility toggle while attached.
    #[wasm_bindgen_test]
    fn live_display_angle_fallback_and_storm_gate() {
        // No volume model at all.
        let r = decode_result(1000.0, 1, "reflectivity");
        let mut e = live_env(true, None, None);
        e.storm_cells_visible = true;
        let a = reduce_live_decoded(e, &r);
        assert_eq!(a.new_displayed.unwrap().elevation_deg, 0.44);
        assert!(a.run_storm_cells);

        // Pattern too short for elev 3 → mean fallback.
        let vol = live_volume(&[0.5, 1.5]);
        let r3 = decode_result(1000.0, 3, "reflectivity");
        let a = reduce_live_decoded(live_env(true, Some(&vol), None), &r3);
        assert_eq!(a.update_overlay, Some((1000.0, 1010.0, 0.44)));
        assert!(!a.run_storm_cells, "storm rerun follows the toggle");
    }

    // (12) Overlay gate (live variant): gated on sweep END time — a
    // partial with end 0 refreshes nothing even while attached.
    #[wasm_bindgen_test]
    fn live_overlay_gate_on_sweep_end() {
        let mut r = decode_result(1000.0, 1, "reflectivity");
        r.sweep_end_secs = 0.0;
        let a = reduce_live_decoded(live_env(true, None, None), &r);
        assert!(a.upload_to_gpu);
        assert_eq!(a.update_overlay, None);
    }

    // ── live decode: phase 2 (azimuth bookkeeping) ──────────────────────────

    // (13) Chronological first/last from radial times — NOT sorted min/max:
    // the earliest-by-time radial seeds the sweep start (once), the
    // latest-by-time radial is the trailing edge.
    #[wasm_bindgen_test]
    fn live_azimuths_chronological_first_last() {
        let mut live = LiveModeState::default();
        let mut r = decode_result(1000.0, 1, "reflectivity");
        r.azimuths = vec![10.0, 350.0, 20.0];
        r.radial_times = vec![5.0, 3.0, 9.0];

        reduce_live_decoded_azimuths(&mut live, &r);

        assert_eq!(live.sweep_start_azimuth, Some(350.0), "earliest by time");
        assert_eq!(live.live_data_azimuth_range, Some((350.0, 20.0)));

        // A later decode keeps the sweep start and advances the trailing edge.
        let mut r2 = decode_result(1000.0, 1, "reflectivity");
        r2.azimuths = vec![40.0, 60.0];
        r2.radial_times = vec![11.0, 12.0];
        reduce_live_decoded_azimuths(&mut live, &r2);
        assert_eq!(live.sweep_start_azimuth, Some(350.0), "set once per sweep");
        assert_eq!(live.live_data_azimuth_range, Some((350.0, 60.0)));
    }

    // (14) Positional fallbacks without radial times, and the empty-azimuth
    // no-op.
    #[wasm_bindgen_test]
    fn live_azimuths_fallbacks_and_empty_noop() {
        // No radial times → first/last by position.
        let mut live = LiveModeState::default();
        let mut r = decode_result(1000.0, 1, "reflectivity");
        r.azimuths = vec![30.0, 40.0];
        r.radial_times = Vec::new();
        reduce_live_decoded_azimuths(&mut live, &r);
        assert_eq!(live.sweep_start_azimuth, Some(30.0));
        assert_eq!(live.live_data_azimuth_range, Some((30.0, 40.0)));

        // Empty azimuths → nothing changes.
        let mut live = LiveModeState::default();
        let mut r = decode_result(1000.0, 1, "reflectivity");
        r.azimuths = Vec::new();
        r.radial_times = Vec::new();
        reduce_live_decoded_azimuths(&mut live, &r);
        assert_eq!(live.sweep_start_azimuth, None);
        assert_eq!(live.live_data_azimuth_range, None);
    }
}
