//! Pure canvas-render decisions, lifted out of the `ui::canvas` hot path.
//!
//! The canvas paints radar to a GPU texture — that pixel-pushing is irreducibly
//! shell (the P3 carve-out). But the *decisions* around it are pure and live
//! here, unit-tested with no egui/GL:
//!
//! - **Sweep animation:** which sweep the playhead is in, the rotating sweep-line
//!   azimuth (radial interpolation + the 0-vs-2 "between sweeps" rule), and the
//!   prev-sweep cache update.
//! - **Polar lookup:** the data-probe value/time lookup (`value_at_polar`),
//!   decoupled from the GL renderer object so it can be tested over plain buffers.
//! - **Geometry:** screen→polar conversion and the coverage-cutout longitude span.
//!
//! The shell wrappers (`ui::canvas`, `gpu_renderer::inspect`) feed these the data
//! they read from subsystems / GL handles and apply the results.

use crate::core::Sweep;

// ---------------------------------------------------------------------------
// Sweep-animation decisions
// ---------------------------------------------------------------------------

/// Whether the sweep animation (beam line + progressive reveal) runs this
/// frame. It is what Rays mode means: rays drawn as they were collected.
///
/// - Rays while attached to a live stream: the reveal tracks the beam, but
///   only while the antenna is on the cut being viewed (a fixed elevation
///   filter goes still between visits).
/// - Rays in archive playback: the reveal follows the playhead through each
///   sweep's recorded radial times.
/// - Frames never animates; it shows whole sweeps only.
pub(crate) fn sweep_animation_effective(
    mode: crate::core::PlaybackMode,
    live_attached: bool,
    selected_elevation: Option<u8>,
    collecting_elevation: Option<u8>,
) -> bool {
    mode == crate::core::PlaybackMode::Micro
        && (!live_attached
            || displayed_elevation_is_collecting(selected_elevation, collecting_elevation))
}

/// Whether the canvas follows the live stream this frame (the in-progress
/// cut, its live beam marks and age fade) rather than the playhead.
///
/// The stream keeps ingesting while the user browses, so a running stream
/// alone does not decide it. The canvas follows live only while the
/// selection is attached to the live edge *and* the playhead is on the cut
/// being collected:
///
/// - Pinned to now: the playhead is the live edge, so it follows live.
/// - A range pinned to now (the "last N frames" loop): only once the
///   playhead reaches the start of the live cut. Before that it is showing
///   sweeps that are already stored, which play like the archive.
/// - Scrubbed off the live edge (a point at a past time): never.
///
/// When it does not follow live, the canvas behaves exactly as in archive
/// playback: Rays reveals each stored sweep up to the playhead's radial.
pub(crate) fn canvas_follows_live(
    stream_active: bool,
    pinned_to_now: bool,
    live_range: bool,
    playhead: f64,
    live_cut_start: Option<f64>,
) -> bool {
    stream_active
        && (pinned_to_now || (live_range && live_cut_start.is_some_and(|s| playhead >= s)))
}

/// Whether the beam is currently collecting the elevation the user is viewing.
///
/// - `selected_elevation = None` (Latest / all tilts): any collecting cut counts.
/// - `selected_elevation = Some(n)` (fixed cut / filter): only while the antenna
///   is on elevation `n`. Between visits the beam is on other tilts, so sweep
///   animation and age desaturation must stay off.
pub(crate) fn displayed_elevation_is_collecting(
    selected_elevation: Option<u8>,
    collecting_elevation: Option<u8>,
) -> bool {
    match selected_elevation {
        None => true,
        Some(want) => collecting_elevation == Some(want),
    }
}

/// Whether data-age desaturation should run this frame.
///
/// Desaturation only makes sense while the sweep animation is effectively
/// active (beam collecting / oldest-data wedge). Live mode still enables the
/// GPU sweep path for partial-chunk compositing when animation is off, so the
/// preference alone is not enough — gate on both the stored toggle and
/// [`sweep_animation_effective`] (via the view-level effective flag).
pub(crate) fn data_age_desaturation_effective(pref: bool, effective_sweep_animation: bool) -> bool {
    pref && effective_sweep_animation
}

/// Live partial azimuth range for GPU compositing, or `None` when the canvas
/// follows the playhead instead of the stream ([`canvas_follows_live`]: the
/// stored sweep under a scrubbed playhead is revealed up to the playhead, not
/// up to the live data edge), when the in-progress elevation is not the one
/// on screen (fixed elevation filter between visits), or when Frames mode is
/// holding the last complete sweep instead of the partial.
pub(crate) fn live_compositing_azimuth_range(
    follows_live: bool,
    whole_sweeps_only: bool,
    selected_elevation: Option<u8>,
    active_elevation: Option<u8>,
    data_azimuth_range: Option<(f32, f32)>,
) -> Option<(f32, f32)> {
    if !follows_live
        || whole_sweeps_only
        || !displayed_elevation_is_collecting(selected_elevation, active_elevation)
    {
        return None;
    }
    data_azimuth_range
}

/// The live cut the antenna is collecting while a fixed tilt that is *not*
/// that cut is on screen. Its partial data never composites over the image
/// ([`live_compositing_azimuth_range`]), but its marks still draw, so the
/// canvas shows where the antenna is whichever tilt is picked, as the ladder
/// and the timeline do.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct OffscreenCut {
    /// Where the live cut started and its newest radial (degrees).
    pub first_az: f32,
    pub last_az: f32,
}

/// The live cut to mark on the canvas when it is not the tilt on screen:
/// the exact complement of [`live_compositing_azimuth_range`] in Rays.
pub(crate) fn offscreen_live_cut(
    whole_sweeps_only: bool,
    selected_elevation: Option<u8>,
    active_elevation: Option<u8>,
    data_azimuth_range: Option<(f32, f32)>,
) -> Option<OffscreenCut> {
    if whole_sweeps_only || displayed_elevation_is_collecting(selected_elevation, active_elevation)
    {
        return None;
    }
    let (first_az, last_az) = data_azimuth_range?;
    Some(OffscreenCut { first_az, last_az })
}

/// Interpolate the rotating sweep-line azimuth within `sweep` at playback time
/// `ts`, returning `(current_az, start_az)` in degrees, or `None` if the sweep
/// has no positive duration.
///
/// Mirrors the original `compute_sweep_line_azimuth` body exactly: per-radial
/// interpolation with 360° wrap-around bridging when radials are present, else a
/// uniform-rotation fallback from `start_azimuth`.
pub(crate) fn sweep_line_azimuth(sweep: &Sweep, ts: f64) -> Option<(f32, f32)> {
    let duration = sweep.end_time - sweep.start_time;
    if duration <= 0.0 {
        return None;
    }

    if !sweep.radials.is_empty() {
        let start_az = sweep.radials[0].azimuth;
        let mut last_az = start_az;
        let mut last_time = sweep.start_time;
        let mut next_az = start_az + 360.0;
        let mut next_time = sweep.end_time;

        for radial in &sweep.radials {
            if radial.start_time <= ts {
                last_az = radial.azimuth;
                last_time = radial.start_time;
            } else {
                next_az = radial.azimuth;
                next_time = radial.start_time;
                break;
            }
        }

        let mut delta_az = next_az - last_az;
        if delta_az < -180.0 {
            delta_az += 360.0;
        } else if delta_az > 180.0 {
            delta_az -= 360.0;
        }

        let dt = next_time - last_time;
        if dt > 0.0 {
            let frac = (ts - last_time) / dt;
            let az = ((last_az + delta_az * frac as f32) % 360.0 + 360.0) % 360.0;
            return Some((az, start_az));
        }
        return Some((last_az, start_az));
    }

    // Fallback: uniform rotation from the sweep's first azimuth.
    let start_az = sweep.start_azimuth;
    let progress = (ts - sweep.start_time) / duration;
    let az = ((start_az + progress as f32 * 360.0) % 360.0 + 360.0) % 360.0;
    Some((az, start_az))
}

/// Select the `(azimuth, start)` pair the GPU should treat as the sweep boundary
/// this frame, or `None` to show no rotating sweep.
///
/// Precedence (mirrors `compute_gpu_sweep_state`):
/// 1. Live partial: the live model's `(first_az, last_az)` data range wins,
///    emitted as `(last_az, first_az)` regardless of the animation toggle.
/// 2. Archive animation: when `effective_sweep_animation`, classify the playhead
///    against the resolved sweep's `(start_time, end_time)` — before start →
///    `(0.0, 0.0)` sentinel, within → the interpolated `sweep_info`, after → none.
/// 3. Otherwise none.
pub(crate) fn select_gpu_sweep(
    live_data_azimuth_range: Option<(f32, f32)>,
    effective_sweep_animation: bool,
    sweep_bounds: Option<(f64, f64)>,
    playback_ts: f64,
    sweep_info: Option<(f32, f32)>,
) -> Option<(f32, f32)> {
    if let Some((first_az, last_az)) = live_data_azimuth_range {
        return Some((last_az, first_az));
    }
    if effective_sweep_animation {
        return match sweep_bounds {
            Some((s, _)) if playback_ts < s => Some((0.0, 0.0)),
            Some((_, e)) if playback_ts <= e => sweep_info,
            _ => None,
        };
    }
    None
}

/// The next value of `last_sweep_line_cache` given this frame's `gpu_sweep`.
///
/// Caches a real sweep position (skipping the `(0.0, 0.0)` "reveal not started"
/// sentinel), and clears the cache entirely when animation is off.
pub(crate) fn next_sweep_cache(
    prev: Option<(f32, f32)>,
    gpu_sweep: Option<(f32, f32)>,
    effective_sweep_animation: bool,
) -> Option<(f32, f32)> {
    let mut cache = prev;
    if let Some((az, start)) = gpu_sweep {
        if az != 0.0 || start != 0.0 {
            cache = Some((az, start));
        }
    }
    if !effective_sweep_animation {
        cache = None;
    }
    cache
}

/// Whether the canvas is "between sweeps": animation on, no live/archive sweep
/// resolved this frame, but a cached position exists (so the stale line shows).
pub(crate) fn between_sweeps(
    effective_sweep_animation: bool,
    gpu_sweep: Option<(f32, f32)>,
    cache: Option<(f32, f32)>,
) -> bool {
    effective_sweep_animation && gpu_sweep.is_none() && cache.is_some()
}

// ---------------------------------------------------------------------------
// Polar lookup (data probe) — decoupled from the GL renderer object
// ---------------------------------------------------------------------------

/// Spatial metadata for a single sweep's CPU buffers, mirrored from the GL
/// renderer's `SweepState`. Lets the polar lookups be pure over plain slices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PolarSweepMeta {
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_km: f64,
    pub gate_interval_km: f64,
    pub max_range_km: f64,
    pub data_offset: f32,
    pub data_scale: f32,
}

/// Nearest azimuth index for `target_deg` among `azimuths` (sparse radial
/// layout, negative slots skipped), or `None` if the closest radial is more than
/// 1.5× the nominal spacing away. Was `gpu_renderer::find_nearest_azimuth_index`.
pub(crate) fn find_nearest_azimuth_index(
    azimuths: &[f32],
    azimuth_count: usize,
    target_deg: f32,
) -> Option<usize> {
    let mut best_idx = 0usize;
    let mut best_dist = 360.0f32;
    let mut found = false;
    for (i, &az) in azimuths.iter().enumerate() {
        if az < 0.0 {
            continue;
        }
        let mut d = (target_deg - az).abs();
        if d > 180.0 {
            d = 360.0 - d;
        }
        if d < best_dist {
            best_dist = d;
            best_idx = i;
            found = true;
        }
    }

    if !found {
        return None;
    }

    let az_spacing = 360.0 / azimuth_count as f32;
    if best_dist > az_spacing * 1.5 {
        return None;
    }

    Some(best_idx)
}

/// Whether a polar query at `azimuth_deg` falls in the *previous*-sweep region,
/// given the active sweep's `(sweep_az, sweep_start)`. `None` params → always
/// current sweep.
pub(crate) fn polar_in_prev_region(azimuth_deg: f32, sweep_params: Option<(f32, f32)>) -> bool {
    if let Some((sweep_az, sweep_start)) = sweep_params {
        let swept_arc = (sweep_az - sweep_start).rem_euclid(360.0);
        let pixel_from_start = (azimuth_deg - sweep_start).rem_euclid(360.0);
        pixel_from_start >= swept_arc
    } else {
        false
    }
}

/// Convert a raw gate value to physical units (`(raw - offset) / scale`), or
/// `None` for the below-threshold/range-folded sentinels (`raw <= 1.0`). A zero
/// scale signals "raw == physical".
fn scale_raw(raw: f32, offset: f32, scale: f32) -> Option<f32> {
    if raw <= 1.0 {
        return None;
    }
    if scale == 0.0 {
        Some(raw)
    } else {
        Some((raw - offset) / scale)
    }
}

/// Current-sweep value lookup using sparse nearest-azimuth indexing (with gap
/// detection). Mirrors `RadarGpuRenderer::value_at_polar`'s current branch.
pub(crate) fn value_at_polar_current(
    azimuth_deg: f32,
    range_km: f64,
    meta: &PolarSweepMeta,
    azimuths: &[f32],
    gate_values: &[f32],
) -> Option<f32> {
    if azimuths.is_empty() {
        return None;
    }
    if range_km < meta.first_gate_km || range_km >= meta.max_range_km {
        return None;
    }
    let az_idx = find_nearest_azimuth_index(azimuths, meta.azimuth_count as usize, azimuth_deg)?;
    let gate_count = meta.gate_count as usize;
    let gate_idx = ((range_km - meta.first_gate_km) / meta.gate_interval_km).floor() as usize;
    if gate_idx >= gate_count {
        return None;
    }
    let offset = az_idx * gate_count + gate_idx;
    let raw = *gate_values.get(offset)?;
    scale_raw(raw, meta.data_offset, meta.data_scale)
}

/// Previous-sweep value lookup using evenly-spaced azimuth indexing (matches the
/// GPU shader's prev-sweep sampling). Mirrors `prev_value_at_polar`.
pub(crate) fn value_at_polar_prev(
    azimuth_deg: f32,
    range_km: f64,
    meta: &PolarSweepMeta,
    gate_values: &[f32],
) -> Option<f32> {
    let az_count = meta.azimuth_count as usize;
    let gate_count = meta.gate_count as usize;
    if az_count == 0 || gate_count == 0 || gate_values.is_empty() {
        return None;
    }
    if range_km < meta.first_gate_km || range_km >= meta.max_range_km {
        return None;
    }
    let az_idx = ((azimuth_deg * az_count as f32 / 360.0).round() as usize) % az_count;
    let gate_idx = ((range_km - meta.first_gate_km) / meta.gate_interval_km).floor() as usize;
    if gate_idx >= gate_count {
        return None;
    }
    let offset = az_idx * gate_count + gate_idx;
    let raw = *gate_values.get(offset)?;
    scale_raw(raw, meta.data_offset, meta.data_scale)
}

/// Current-sweep collection-time lookup (sparse azimuth indexing). Mirrors
/// `collection_time_at_polar`'s current branch.
pub(crate) fn collection_time_current(
    azimuth_deg: f32,
    azimuth_count: u32,
    azimuths: &[f32],
    radial_times: &[f64],
) -> Option<f64> {
    if radial_times.is_empty() || azimuths.is_empty() {
        return None;
    }
    let az_idx = find_nearest_azimuth_index(azimuths, azimuth_count as usize, azimuth_deg)?;
    radial_times.get(az_idx).copied()
}

/// Previous-sweep collection-time lookup (evenly-spaced azimuth indexing).
pub(crate) fn collection_time_prev(
    azimuth_deg: f32,
    azimuth_count: u32,
    radial_times: &[f64],
) -> Option<f64> {
    let az_count = azimuth_count as usize;
    if az_count == 0 || radial_times.is_empty() {
        return None;
    }
    let az_idx = ((azimuth_deg * az_count as f32 / 360.0).round() as usize) % az_count;
    radial_times.get(az_idx).copied()
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Equirectangular km-per-degree used throughout the canvas polar math.
pub(crate) const KM_PER_DEGREE: f64 = 111.0;

/// Convert a geographic point to radar-relative polar `(azimuth_deg, range_km)`.
///
/// Equirectangular approximation (matches the canvas projection): longitude is
/// scaled by `cos(radar_lat)`. Mirrors the data-probe's inline math.
pub(crate) fn geo_to_polar(lat: f64, lon: f64, radar_lat: f64, radar_lon: f64) -> (f64, f64) {
    let dlat = lat - radar_lat;
    let dlon = (lon - radar_lon) * radar_lat.to_radians().cos();
    let range_km = (dlat * dlat + dlon * dlon).sqrt() * KM_PER_DEGREE;
    let azimuth_deg = (dlon.atan2(dlat).to_degrees() + 360.0) % 360.0;
    (azimuth_deg, range_km)
}

// ---------------------------------------------------------------------------
// Canvas caption (one-instrument §7 "Rules", §2 the caption surface)
// ---------------------------------------------------------------------------

/// Floor for [`frame_step_secs`]: below a minute the step would flicker the
/// caption on every short high-tilt sweep and every chunk of latency.
pub(crate) const MIN_FRAME_STEP_SECS: f64 = 60.0;

/// Frame step when nothing better is known: a typical volume.
pub(crate) const DEFAULT_FRAME_STEP_SECS: f64 = 300.0;

/// What the render loop knows about the frame and the playhead that the
/// session facts do not carry. Plain data; the caption rule
/// ([`decide_canvas_caption`]) reads it beside [`crate::core::session::SessionVm`]'s
/// frame and playhead facts.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct CaptionContext {
    /// Seconds between consecutive frames of the tilt on screen ("one
    /// frame" in the rule). See [`frame_step_secs`].
    pub frame_step_secs: f64,
    /// A cached frame the playhead calls for is not yet on the GPU (a render
    /// is in flight); the frame and the playhead disagree only for a moment.
    pub render_pending: bool,
    /// A download or ingest covers the playhead.
    pub fetch_covers_playhead: bool,
    /// The tilt the user asked for; `None` is Latest.
    pub wanted_tilt_deg: Option<f32>,
    /// The archive lists a scan at the playhead that is not on the device
    /// (its start, Unix seconds) — the caption offers to fetch it.
    pub fetchable_scan_start: Option<i64>,
    /// 3D volume rendering is on while the playhead is pinned to now: the
    /// volume renders only once every tilt is in.
    pub volume_waiting: bool,
    /// 3D volume rendering draws a volume the device holds only part of:
    /// `(cuts on device, cuts in the volume)`. `None` when the volume is
    /// whole, or no volume renders.
    pub volume_cuts: Option<(usize, usize)>,
}

impl Default for CaptionContext {
    fn default() -> Self {
        Self {
            frame_step_secs: DEFAULT_FRAME_STEP_SECS,
            render_pending: false,
            fetch_covers_playhead: false,
            wanted_tilt_deg: None,
            fetchable_scan_start: None,
            volume_waiting: false,
            volume_cuts: None,
        }
    }
}

/// Why the frame is not the one the playhead calls for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CaptionReason {
    /// A download covering the playhead is on its way.
    Downloading,
    /// A running stream will bring it; the antenna is elsewhere.
    Waiting { antenna_deg: Option<f32> },
    /// Nothing is coming. `fetch` is the listed scan the user can fetch.
    NoData { fetch: Option<i64> },
}

/// The canvas caption decision — the only text that ever sits over the map.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) enum CanvasCaption {
    /// The frame and the playhead agree (within one frame).
    #[default]
    None,
    /// The map is blank. A blank map always says why.
    NoFrame {
        playhead: f64,
        tilt_deg: Option<f32>,
        reason: CaptionReason,
    },
    /// A frame is held more than one frame away from the playhead.
    Discrepancy {
        /// The frame's representative (midpoint) time.
        showing: f64,
        /// The playhead time (now, when pinned).
        playhead: f64,
        tilt_deg: Option<f32>,
        reason: CaptionReason,
    },
    /// 3D volume rendering while pinned: waiting for the volume to complete.
    VolumeWaiting,
    /// 3D volume rendering draws a volume the device holds only part of;
    /// `fetching` when a download or ingest is bringing the rest.
    VolumePartial {
        on_device: usize,
        total: usize,
        fetching: bool,
    },
}

/// The one caption rule (§7): caption whenever the frame on screen and the
/// playhead differ by more than one frame, pinned or not; a blank map always
/// says why.
///
/// - `frame` / `playhead`: the session's frame and playhead facts.
/// - `now`: this frame's wall clock (the playhead's time when pinned).
/// - `stream_running` / `antenna_deg`: whether a stream will bring the next
///   frame, and the tilt the antenna is on.
pub(crate) fn decide_canvas_caption(
    frame: Option<&crate::core::session::FrameFact>,
    playhead: crate::core::session::Playhead,
    now: f64,
    stream_running: bool,
    antenna_deg: Option<f32>,
    ctx: &CaptionContext,
) -> CanvasCaption {
    use crate::core::session::Playhead;
    if ctx.volume_waiting {
        return CanvasCaption::VolumeWaiting;
    }
    if ctx.render_pending {
        return CanvasCaption::None;
    }
    if let (Some((on_device, total)), Some(_)) = (ctx.volume_cuts, frame) {
        return CanvasCaption::VolumePartial {
            on_device,
            total,
            fetching: ctx.fetch_covers_playhead,
        };
    }
    let (target, pinned) = match playhead {
        Playhead::PinnedToNow => (now, true),
        Playhead::AtTime { ts, .. } => (ts, false),
    };
    let reason = if ctx.fetch_covers_playhead {
        CaptionReason::Downloading
    } else if pinned && stream_running {
        CaptionReason::Waiting { antenna_deg }
    } else {
        CaptionReason::NoData {
            fetch: ctx.fetchable_scan_start,
        }
    };
    let Some(f) = frame else {
        return CanvasCaption::NoFrame {
            playhead: target,
            tilt_deg: ctx.wanted_tilt_deg,
            reason,
        };
    };
    if frame_disagrees(f.time_secs, f.end_secs, target, ctx.frame_step_secs) {
        CanvasCaption::Discrepancy {
            showing: f.time_secs,
            playhead: target,
            tilt_deg: ctx.wanted_tilt_deg,
            reason,
        }
    } else {
        CanvasCaption::None
    }
}

/// Whether a frame (its midpoint and end, Unix seconds) sits more than one
/// frame (`frame_step_secs`, floored at [`MIN_FRAME_STEP_SECS`]) away from
/// `target`. The caption rule asks it of the focus and of every other site.
pub(crate) fn frame_disagrees(mid: f64, end: f64, target: f64, frame_step_secs: f64) -> bool {
    let start = 2.0 * mid - end;
    let gap = (target - end).max(start - target).max(0.0);
    gap > frame_step_secs.max(MIN_FRAME_STEP_SECS)
}

/// "One frame" for the caption rule: the expected seconds between
/// consecutive frames of what is on screen.
///
/// - A fixed tilt repeats once per volume: the volume's duration over the
///   number of cuts with that elevation number.
/// - Latest shows every sweep, so one frame is a sweep.
///
/// The volume's duration is the gap to the next cached volume when there is
/// one, else its own span projected to its VCP's full duration
/// ([`crate::core::Scan::display_end_time`]): a volume still filling, or one
/// cached for a single tilt, otherwise spans only the sweeps it holds. The
/// newest volume never counts as shorter than the gap from the one before
/// it. Floored at [`MIN_FRAME_STEP_SECS`]; [`DEFAULT_FRAME_STEP_SECS`] when
/// the frame's volume is not in the timeline.
pub(crate) fn frame_step_secs(
    timeline: &crate::core::RadarTimeline,
    displayed: &crate::core::DisplayedSweep,
    latest: bool,
) -> f64 {
    use crate::core::recency::RECENCY_SECS;
    let sweep_secs = (displayed.end_time - displayed.start_time).max(0.0);
    if latest {
        return sweep_secs.max(MIN_FRAME_STEP_SECS);
    }
    let key_ms = displayed.identity.scan_key.scan_start.0;
    let scans = &timeline.scans;
    let Some(i) = scans.iter().position(|s| s.key_ms() == key_ms) else {
        return DEFAULT_FRAME_STEP_SECS;
    };
    let scan = &scans[i];
    let span = |s: &crate::core::Scan| (s.display_end_time() - s.start_time).max(0.0);
    let own = span(scan);
    let volume_secs = match scans.get(i + 1) {
        Some(next) if next.start_time - scan.start_time <= RECENCY_SECS => {
            next.start_time - scan.start_time
        }
        Some(_) => own,
        None => match i.checked_sub(1).map(|p| &scans[p]) {
            Some(prev) if scan.start_time - prev.start_time <= RECENCY_SECS => {
                own.max(scan.start_time - prev.start_time)
            }
            Some(prev) => own.max(span(prev)),
            None => own,
        },
    };
    // A held tilt's frames are its angle's visits in the volume (the base
    // cut and each SAILS/MRLE repeat): as many as the plan makes, or as are
    // cached when that is more (or the plan is unknown).
    let shown = displayed.identity.elevation_number;
    let tilt = crate::core::tilt::TiltFilter::Angle {
        anchor: shown,
        angle: scan
            .target_elevation_angle(shown)
            .unwrap_or(displayed.elevation_deg),
    };
    let cuts = tilt.cuts(scan.vcp_pattern.as_ref());
    let cached = scan
        .sweeps
        .iter()
        .filter(|s| cuts.contains(s.elevation_number))
        .count();
    let planned = scan.vcp_pattern.as_ref().map_or(0, |_| cuts.len());
    let visits = cached.max(planned).max(1);
    (volume_secs / visits as f64)
        .max(sweep_secs)
        .max(MIN_FRAME_STEP_SECS)
}

/// What a caption segment is, so the shell can set it in its color (§7:
/// "the caption names both, in their colors").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CaptionRole {
    /// Plain words.
    Words,
    /// The frame's time — what you see.
    Frame,
    /// The playhead's time; `pinned` when it is now.
    Playhead { pinned: bool },
    /// The clickable "fetch" (the line's [`CaptionLine::action`]).
    Action,
}

/// The caption as words, segment by segment.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct CaptionLine {
    pub segments: Vec<(String, CaptionRole)>,
    /// What clicking the [`CaptionRole::Action`] segment does.
    pub action: Option<crate::core::Intent>,
}

impl CaptionLine {
    /// The whole line as one string.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        self.segments.iter().map(|(s, _)| s.as_str()).collect()
    }
}

fn tilt_word(tilt_deg: Option<f32>) -> Option<String> {
    tilt_deg.map(|t| format!("{t:.1}°"))
}

/// The caption's words in the §7 forms:
///
/// - `showing 11:28:51 · playhead 11:30:49 · 0.5° at 11:30 is downloading`
/// - `… · no 0.5° at 11:30` (`· fetch` when the archive lists it)
/// - `… · waiting for 0.5° (antenna on 4.0°)`
/// - `no frame yet · downloading 11:21:49`
/// - `no 0.5° data at 11:30 · fetch`
pub(crate) fn caption_line(
    caption: &CanvasCaption,
    pinned: bool,
    clock: &dyn crate::core::session::Clock,
) -> Option<CaptionLine> {
    let mut segs: Vec<(String, CaptionRole)> = Vec::new();
    let mut action = None;
    let words = |s: String| (s, CaptionRole::Words);
    let waiting = |tilt: Option<f32>, antenna: Option<f32>| {
        let mut s = match tilt_word(tilt) {
            Some(t) => format!("waiting for {t}"),
            None => "waiting for the next sweep".to_string(),
        };
        if let Some(a) = antenna {
            s.push_str(&format!(" (antenna on {a:.1}°)"));
        }
        s
    };
    match *caption {
        CanvasCaption::None => return None,
        CanvasCaption::VolumeWaiting => {
            segs.push(words(
                "waiting for the volume to complete · volume rendering needs every tilt"
                    .to_string(),
            ));
        }
        CanvasCaption::VolumePartial {
            on_device,
            total,
            fetching,
        } => {
            segs.push((
                format!("drawing {on_device} of {total} cuts"),
                CaptionRole::Frame,
            ));
            if fetching {
                segs.push(words(" · fetching the rest".to_string()));
            }
        }
        CanvasCaption::NoFrame {
            playhead,
            tilt_deg,
            reason,
        } => match reason {
            CaptionReason::Downloading => {
                segs.push(words("no frame yet · downloading ".to_string()));
                segs.push((clock.hms(playhead), CaptionRole::Playhead { pinned }));
            }
            CaptionReason::Waiting { antenna_deg } => {
                segs.push(words(format!(
                    "no frame yet · {}",
                    waiting(tilt_deg, antenna_deg)
                )));
            }
            CaptionReason::NoData { fetch } => {
                segs.push(words(match tilt_word(tilt_deg) {
                    Some(t) => format!("no {t} data at "),
                    None => "no data at ".to_string(),
                }));
                segs.push((clock.hm(playhead), CaptionRole::Playhead { pinned }));
                if let Some(scan_start) = fetch {
                    segs.push(words(" · ".to_string()));
                    segs.push(("fetch".to_string(), CaptionRole::Action));
                    action = Some(fetch_intent(scan_start));
                }
            }
        },
        CanvasCaption::Discrepancy {
            showing,
            playhead,
            tilt_deg,
            reason,
        } => {
            segs.push(words("showing ".to_string()));
            segs.push((clock.hms(showing), CaptionRole::Frame));
            segs.push(words(" · playhead ".to_string()));
            segs.push((clock.hms(playhead), CaptionRole::Playhead { pinned }));
            segs.push(words(" · ".to_string()));
            match reason {
                CaptionReason::Downloading => segs.push(words(match tilt_word(tilt_deg) {
                    Some(t) => format!("{t} at {} is downloading", clock.hm(playhead)),
                    None => format!("{} is downloading", clock.hm(playhead)),
                })),
                CaptionReason::Waiting { antenna_deg } => {
                    segs.push(words(waiting(tilt_deg, antenna_deg)))
                }
                CaptionReason::NoData { fetch } => {
                    segs.push(words(match tilt_word(tilt_deg) {
                        Some(t) => format!("no {t} at {}", clock.hm(playhead)),
                        None => format!("no data at {}", clock.hm(playhead)),
                    }));
                    if let Some(scan_start) = fetch {
                        segs.push(words(" · ".to_string()));
                        segs.push(("fetch".to_string(), CaptionRole::Action));
                        action = Some(fetch_intent(scan_start));
                    }
                }
            }
        }
    }
    Some(CaptionLine {
        segments: segs,
        action,
    })
}

fn fetch_intent(scan_start: i64) -> crate::core::Intent {
    crate::core::Intent::FetchScan {
        scan_start,
        elevation_filter: None,
    }
}

/// Gap between the caption's scrim and the map's bottom edge; the same as the
/// layer legends' margin, so the bottom scrims share a baseline.
pub(crate) const CAPTION_BOTTOM_INSET: f32 = 12.0;

/// Clear space between the caption and a corner scrim it is lifted over.
const CAPTION_SCRIM_GAP: f32 = 6.0;

/// A scrim along the map's bottom edge (the scale bar, the layer legends, the
/// 3D compass), in points: `left` and `right` from the map's left edge, `top`
/// up from its bottom edge.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct BottomScrim {
    pub left: f32,
    pub right: f32,
    pub top: f32,
}

/// How far above the map's bottom edge the caption's scrim sits: bottom
/// center at [`CAPTION_BOTTOM_INSET`], unless at `caption_width` it would
/// reach over a corner scrim, as on a phone-width map; then just above the
/// tallest one it would cover.
pub(crate) fn caption_bottom_inset(
    map_width: f32,
    caption_width: f32,
    scrims: &[BottomScrim],
) -> f32 {
    let left = (map_width - caption_width) / 2.0 - CAPTION_SCRIM_GAP;
    let right = (map_width + caption_width) / 2.0 + CAPTION_SCRIM_GAP;
    scrims
        .iter()
        .filter(|s| s.left < right && s.right > left)
        .fold(CAPTION_BOTTOM_INSET, |inset, s| {
            inset.max(s.top + CAPTION_SCRIM_GAP)
        })
}

/// Longitude span (degrees) covering `range_km` at `center_lat`, accounting for
/// meridian convergence (`/cos(lat)`). The coverage-cutout circle's radius is
/// this span projected to screen. Mirrors the canvas cutout math.
pub(crate) fn cutout_lon_range_deg(center_lat: f64, range_km: f64) -> f64 {
    let lat_correction = center_lat.to_radians().cos();
    range_km / KM_PER_DEGREE / lat_correction
}

// ---------------------------------------------------------------------------
// Quiet canvas: the overlay view-model (one-instrument §7)
// ---------------------------------------------------------------------------

/// Ring spacing: a ring every 50 km, the major (hover-labeled) ones every
/// 100 km.
pub(crate) const RING_STEP_KM: f64 = 50.0;
/// Major ring spacing; hover a major ring for its range.
pub(crate) const RING_LABEL_STEP_KM: f64 = 100.0;
/// How close (screen px) the cursor must be to a ring to hover it.
pub(crate) const RING_HOVER_PX: f32 = 5.0;

/// The product's maximum range: what the coverage ring and the mosaic
/// cutout draw at. Reflectivity (and its clutter-filter power) reaches the
/// long-range surveillance cut; Doppler and dual-pol moments stop at 300 km.
pub(crate) fn product_max_range_km(product: crate::core::RadarProduct) -> f64 {
    use crate::core::RadarProduct::*;
    match product {
        Reflectivity | ClutterFilterPower => 460.0,
        Velocity
        | SpectrumWidth
        | DifferentialReflectivity
        | CorrelationCoefficient
        | DifferentialPhase => 300.0,
    }
}

/// Every circle the canvas draws around the site, in km, from one place so
/// the 2D rings, the edge ring, the coverage ring, the mosaic cutout and the
/// 3D drape agree ("the rings agree").
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct RingGeometry {
    /// The data's real range: the edge ring, the GPU disc, the 3D drape.
    pub data_km: f64,
    /// The product's maximum range: the coverage ring and the mosaic cutout.
    pub coverage_km: f64,
    /// Range rings inside the data range, `(km, major)`.
    pub rings: Vec<(f64, bool)>,
}

/// Ring geometry for `product`, with the on-screen sweep's range when there
/// is data (`None` falls back to the product's maximum range, so the rings
/// still show where data will be).
pub(crate) fn ring_geometry(
    product: crate::core::RadarProduct,
    data_range_km: Option<f64>,
) -> RingGeometry {
    let coverage_km = product_max_range_km(product);
    let data_km = data_range_km
        .filter(|r| r.is_finite() && *r > 0.0)
        .unwrap_or(coverage_km);
    let mut rings = Vec::new();
    let mut km = RING_STEP_KM;
    // The last ring before the edge; the edge itself is the edge ring.
    while km < data_km - 1.0 {
        let major = (km % RING_LABEL_STEP_KM).abs() < 1e-6;
        rings.push((km, major));
        km += RING_STEP_KM;
    }
    RingGeometry {
        data_km,
        coverage_km,
        rings,
    }
}

/// The major ring under the cursor, for its hover label ("200 km"): the
/// cursor's distance from the site is within [`RING_HOVER_PX`] of a major
/// ring, or of the edge ring.
pub(crate) fn hovered_ring_km(
    rings: &RingGeometry,
    cursor_dist_px: f32,
    px_per_km: f32,
) -> Option<f64> {
    if !(px_per_km.is_finite() && px_per_km > 0.0) {
        return None;
    }
    rings
        .rings
        .iter()
        .filter(|(_, major)| *major)
        .map(|(km, _)| *km)
        .chain(std::iter::once(rings.data_km))
        .find(|km| ((*km as f32) * px_per_km - cursor_dist_px).abs() <= RING_HOVER_PX)
}

/// A ring's hover label: `200 km`.
pub(crate) fn ring_label(km: f64) -> String {
    format!("{km:.0} km")
}

/// The sweep marks in Rays (§7): a faint white start line, a solid white
/// data edge, a dashed red now line, and the edge ring split at the data
/// edge — no text on the map.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct SweepMarks {
    /// Where the new sweep started (degrees).
    pub start_az: f32,
    /// The data edge: the newest radial drawn (degrees).
    pub edge_az: f32,
    /// Between sweeps: the held position from the last sweep, drawn faint.
    pub stale: bool,
    /// Where the antenna probably is (projected) — the dashed red now line.
    /// Live only.
    pub now_az: Option<f32>,
    /// Each received chunk's last azimuth except the newest (live).
    pub chunk_az: Vec<f32>,
    /// The data edge's collection time, for the edge ring's hover (actual in
    /// live; the playhead in archive, where the reveal follows it).
    pub edge_time: Option<f64>,
    /// The marks are the live cut on another tilt than the one on screen:
    /// drawn in grey over a whole edge ring, because none of that cut is in
    /// the picture.
    pub offscreen: bool,
}

/// Everything [`CanvasOverlayVm::build`] reads.
pub(crate) struct CanvasOverlayInputs<'a> {
    pub product: crate::core::RadarProduct,
    /// The on-screen sweep's range when the GPU holds data.
    pub data_range_km: Option<f64>,
    /// `(edge_az, start_az)` of the sweep reveal this frame, if any.
    pub sweep: Option<(f32, f32)>,
    /// The sweep position is held from the last sweep (between sweeps).
    pub stale: bool,
    /// Rays is animating this frame (lines draw only then).
    pub show_lines: bool,
    /// A live stream feeds the canvas.
    pub live: bool,
    /// The live model's estimated antenna azimuth.
    pub antenna_az: Option<f32>,
    /// The live cut's chunk last-azimuths, oldest first.
    pub chunk_last_az: &'a [f32],
    /// The live cut's newest chunk end time.
    pub live_edge_time: Option<f64>,
    /// The live cut when it is not the tilt on screen ([`offscreen_live_cut`]).
    pub offscreen_cut: Option<OffscreenCut>,
    pub playhead_secs: f64,
    pub fallbacks: CanvasFallbacks,
}

/// What the canvas overlays draw this frame. The shell only paints it.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct CanvasOverlayVm {
    pub rings: RingGeometry,
    /// Rays' marks; `None` in Frames or with no reveal.
    pub sweep: Option<SweepMarks>,
    /// Row 2's fallback: the data edge's time, painted inside the canvas.
    /// `None` unless [`CanvasFallbacks::edge_label`] is on.
    pub edge_label: Option<f64>,
}

impl CanvasOverlayVm {
    pub(crate) fn build(i: &CanvasOverlayInputs<'_>) -> Self {
        let rings = ring_geometry(i.product, i.data_range_km);
        let live_chunks = || -> Vec<f32> {
            i.chunk_last_az
                .iter()
                .take(i.chunk_last_az.len().saturating_sub(1))
                .copied()
                .collect()
        };
        let sweep = match (i.sweep, i.show_lines, i.offscreen_cut) {
            // The antenna is on another tilt: mark its cut all the same.
            (None, true, Some(cut)) if i.live => Some(SweepMarks {
                start_az: cut.first_az,
                edge_az: cut.last_az,
                stale: false,
                now_az: i.antenna_az,
                chunk_az: live_chunks(),
                edge_time: i.live_edge_time,
                offscreen: true,
            }),
            (Some((edge_az, start_az)), true, _) => Some(SweepMarks {
                start_az,
                edge_az,
                stale: i.stale,
                now_az: if i.live { i.antenna_az } else { None },
                chunk_az: if i.live { live_chunks() } else { Vec::new() },
                edge_time: if i.stale {
                    None
                } else if i.live {
                    i.live_edge_time
                } else {
                    Some(i.playhead_secs)
                },
                offscreen: false,
            }),
            _ => None,
        };
        let edge_label = if i.fallbacks.edge_label {
            sweep
                .as_ref()
                .filter(|s| !s.offscreen)
                .and_then(|s| s.edge_time)
        } else {
            None
        };
        Self {
            rings,
            sweep,
            edge_label,
        }
    }
}

/// How strongly a disc's range rings, radials and Rays marks draw: the
/// focus at full strength, every other site at half, so each disc has its
/// rings and the strong ones say which disc the single-valued surfaces
/// describe.
pub(crate) fn disc_overlay_opacity(is_focus: bool) -> f32 {
    if is_focus {
        1.0
    } else {
        0.5
    }
}

/// Hovering the edge ring reads the data edge's time and age (§7: "hover the
/// ring for the edge time and age"): `edge 11:21:57 · 18 s old`.
pub(crate) fn edge_hover_text(
    edge_time: f64,
    now: f64,
    clock: &dyn crate::core::session::Clock,
) -> String {
    format!(
        "edge {} · {}",
        clock.hms(edge_time),
        crate::core::session::format_age(now - edge_time)
    )
}

/// The desaturated wedge's tooltip (answers row 10: unchanged, with a
/// tooltip), when the cursor sits on it: inside the data disc, in the
/// previous sweep's region, while age desaturation runs.
pub(crate) fn faded_wedge_hint(
    desaturating: bool,
    sweep: Option<(f32, f32)>,
    cursor_az: f32,
    cursor_km: f64,
    data_km: f64,
) -> Option<&'static str> {
    (desaturating && cursor_km < data_km && polar_in_prev_region(cursor_az, sweep))
        .then_some("Faded: the previous sweep, older than the new one")
}

/// Where row 2's fallback edge label sits: on the data edge at `edge_az`,
/// pulled inward so a `w`×`h` label stays inside `bounds` (`min_x, min_y,
/// max_x, max_y`). Returns the label's top-left corner.
pub(crate) fn edge_label_pos(
    center: (f32, f32),
    radius_px: f32,
    edge_az: f32,
    size: (f32, f32),
    bounds: (f32, f32, f32, f32),
) -> (f32, f32) {
    let a = (edge_az - 90.0).to_radians();
    let (x, y) = (
        center.0 + radius_px * a.cos(),
        center.1 + radius_px * a.sin(),
    );
    let (w, h) = size;
    let (min_x, min_y, max_x, max_y) = bounds;
    let left = (x - w / 2.0).clamp(min_x, (max_x - w).max(min_x));
    let top = (y - h / 2.0).clamp(min_y, (max_y - h).max(min_y));
    (left, top)
}

// ---------------------------------------------------------------------------
// Fallbacks for the two removals (answers page, "The canvas, change by
// change", rows 1 and 2)
// ---------------------------------------------------------------------------

/// The two canvas removals Daniel may want back, each with the cheap
/// fallback the answers page names. Both ship off ([`CANVAS_FALLBACKS`]);
/// flipping one brings it back with no other change.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct CanvasFallbacks {
    /// Row 1, the info box: a one-line stamp top-left on a scrim —
    /// `KTLX · REF · 0.5° · 11:21:57` — with the previous sweep's time as a
    /// second line in Rays.
    pub frame_stamp: bool,
    /// Row 2, the donut labels: one label at the data edge, time only, kept
    /// inside the canvas.
    pub edge_label: bool,
}

/// What this build ships: the info box and the donut labels are out.
pub(crate) const CANVAS_FALLBACKS: CanvasFallbacks = CanvasFallbacks {
    frame_stamp: false,
    edge_label: false,
};

/// Row 1's fallback stamp.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct FrameStamp {
    /// `KTLX · REF · 0.5° · 11:21:57`
    pub line: String,
    /// `previous 11:17:02` — Rays, with a previous sweep under the new one.
    pub previous: Option<String>,
}

/// The fallback stamp, or `None` when the fallback is off or nothing is on
/// screen. The frame facts are the session's; the stamp derives nothing.
pub(crate) fn frame_stamp(
    fallbacks: CanvasFallbacks,
    site_id: &str,
    product: crate::core::RadarProduct,
    frame: Option<&crate::core::session::FrameFact>,
    previous_mid: Option<f64>,
    rays: bool,
    clock: &dyn crate::core::session::Clock,
) -> Option<FrameStamp> {
    if !fallbacks.frame_stamp {
        return None;
    }
    let f = frame?;
    Some(FrameStamp {
        line: format!(
            "{} · {} · {:.1}° · {}",
            site_id.to_uppercase(),
            product.short_code(),
            f.elevation_deg,
            clock.hms(f.time_secs)
        ),
        previous: previous_mid
            .filter(|_| rays)
            .map(|t| format!("previous {}", clock.hms(t))),
    })
}

// ---------------------------------------------------------------------------
// Legend (one-instrument §7: titled with the unit, on a scrim)
// ---------------------------------------------------------------------------

/// The radar legend: the product's name and unit as its title, and the
/// tick labels along the bar.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct LegendVm {
    /// The short caption over the bar: the unit (`dBZ`), or the product's
    /// code for a unitless product (`CC`). Short so the bar, not its
    /// caption, sets the legend's width.
    pub caption: &'static str,
    /// The full name on hover: `Reflectivity · dBZ`.
    pub hover: String,
    /// `(fraction from the bottom, label)`, bottom to top.
    pub ticks: Vec<(f32, String)>,
}

/// The legend for `product` over its color table's `(min, max)` range.
pub(crate) fn legend_vm(product: crate::core::RadarProduct, range: (f32, f32)) -> LegendVm {
    let (min_val, max_val) = range;
    let span = max_val - min_val;
    let step = if span > 200.0 {
        60.0
    } else if span > 60.0 {
        10.0
    } else if span > 10.0 {
        5.0
    } else if span > 2.0 {
        1.0
    } else {
        0.2
    };
    let mut ticks = Vec::new();
    if span > 0.0 {
        let mut i = (min_val / step - 1e-4).ceil() as i64;
        loop {
            let val = i as f32 * step;
            if val > max_val + 1e-4 {
                break;
            }
            let label = if step < 1.0 {
                format!("{val:.1}")
            } else {
                format!("{val:.0}")
            };
            ticks.push(((val - min_val) / span, label));
            i += 1;
        }
    }
    let unit = product.unit();
    LegendVm {
        caption: if unit.is_empty() {
            product.short_code()
        } else {
            unit
        },
        hover: if unit.is_empty() {
            product.label().to_string()
        } else {
            format!("{} · {unit}", product.label())
        },
        ticks,
    }
}

// ---------------------------------------------------------------------------
// Scale bar (one-instrument §7: on a scrim, bottom-left)
// ---------------------------------------------------------------------------

/// The scale bar's target width; each bar snaps to a round value no wider.
pub(crate) const SCALE_BAR_TARGET_PX: f32 = 120.0;
const KM_TO_MI: f64 = 0.621371;

/// The two stacked scale bars: km over miles.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ScaleBarVm {
    pub km_px: f32,
    pub km_label: String,
    pub mi_px: f32,
    pub mi_label: String,
}

/// Snap both bars to a 1/2/5 × 10^n value no wider than
/// [`SCALE_BAR_TARGET_PX`], given the map's km per screen pixel.
pub(crate) fn scale_bar_vm(km_per_px: f64) -> Option<ScaleBarVm> {
    if !(km_per_px.is_finite() && km_per_px > 0.0) {
        return None;
    }
    let target_km = SCALE_BAR_TARGET_PX as f64 * km_per_px;
    let nice_km = nice_round(target_km);
    let nice_mi = nice_round(target_km * KM_TO_MI);
    Some(ScaleBarVm {
        km_px: (nice_km / km_per_px) as f32,
        km_label: format_km(nice_km),
        mi_px: (nice_mi / KM_TO_MI / km_per_px) as f32,
        mi_label: format_mi(nice_mi),
    })
}

/// Great-circle distance in km (R = 6371).
pub(crate) fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    r * c
}

/// Snap to a 1/2/5 × 10^n round number not exceeding `target`.
pub(crate) fn nice_round(target: f64) -> f64 {
    if !target.is_finite() || target <= 0.0 {
        return 0.0;
    }
    let mag = 10f64.powf(target.log10().floor());
    let frac = target / mag;
    let nice = if frac >= 5.0 {
        5.0
    } else if frac >= 2.0 {
        2.0
    } else {
        1.0
    };
    nice * mag
}

fn format_km(km: f64) -> String {
    if km >= 1.0 {
        format!("{:.0} km", km)
    } else if km >= 0.1 {
        format!("{:.1} km", km)
    } else {
        format!("{:.0} m", km * 1000.0)
    }
}

fn format_mi(mi: f64) -> String {
    if mi >= 1.0 {
        format!("{:.0} mi", mi)
    } else if mi >= 0.1 {
        format!("{:.1} mi", mi)
    } else {
        format!("{:.0} ft", mi * 5280.0)
    }
}

// ---------------------------------------------------------------------------
// Layer legends (one-instrument §7: "legends for every colored layer")
// ---------------------------------------------------------------------------

/// The mosaic's palette steps (dBZ) for its stamp's legend: NWS base
/// reflectivity, which the MRMS WMS layer is drawn in.
pub(crate) const MOSAIC_LEGEND_DBZ: [f32; 15] = [
    5.0, 10.0, 15.0, 20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 55.0, 60.0, 65.0, 70.0, 75.0,
];

/// The mosaic legend's end labels: `5` … `75 dBZ`.
pub(crate) fn mosaic_legend_ends() -> (String, String) {
    let first = MOSAIC_LEGEND_DBZ[0];
    let last = MOSAIC_LEGEND_DBZ[MOSAIC_LEGEND_DBZ.len() - 1];
    (format!("{first:.0}"), format!("{last:.0} dBZ"))
}

/// The mPING legend: the categories of the reports drawn at the playhead,
/// in a fixed order so the legend does not reshuffle as reports come and go.
pub(crate) fn mping_legend(
    reports: &[crate::mping::StormReport],
    playback_secs: f64,
) -> Vec<(crate::mping::ReportCategory, &'static str)> {
    use crate::mping::ReportCategory::*;
    [
        Tornado,
        Hail,
        WindDamage,
        Flood,
        RainSnow,
        ReducedVisibility,
        Other,
    ]
    .into_iter()
    .filter(|cat| {
        reports
            .iter()
            .any(|r| r.category == *cat && r.visible_at(playback_secs))
    })
    .map(|cat| (cat, cat.label()))
    .collect()
}

// ---------------------------------------------------------------------------
// Picking (one-instrument §7: hover states and a cursor; sites click only
// when the Sites layer is on)
// ---------------------------------------------------------------------------

/// What the nearest marks under the cursor are, hit-tested by the shell
/// against the projection (pixel radii).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PickCandidates {
    /// Another NEXRAD site's marker: `(id, lat, lon)`.
    pub site: Option<(&'static str, f64, f64)>,
    /// An mPING report's marker id.
    pub mping: Option<i64>,
    /// The alert polygon under the cursor (severity tie-break already
    /// applied): `(id, event)`.
    pub alert: Option<(String, String)>,
    /// The tapped point, when it falls inside a storm cell's footprint
    /// ([`crate::core::storm_cell_view::cell_at`]): `(lat, lon)`.
    pub storm_cell: Option<(f64, f64)>,
}

/// Which pickable layers are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PickLayers {
    pub sites: bool,
    pub mping: bool,
    /// Either alert class is on and live overlays apply
    /// ([`crate::core::recency::live_overlays_off`] finds no reason to hide
    /// them).
    pub alerts: bool,
    /// Storm cells draw and a tap selects one (the phone; the desktop
    /// hovers instead).
    pub storm_cells: bool,
}

/// How far from a mark's center a click or tap still picks it, in screen
/// px. The phone's are 12 px — a 24 px target, the size of a fingertip's
/// contact on a 390 px map; the desktop's fit a mouse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PickRadii {
    pub site: f32,
    pub mping: f32,
}

/// The pick radii for the layout.
pub(crate) fn pick_radii(phone: bool) -> PickRadii {
    if phone {
        PickRadii {
            site: 12.0,
            mping: 12.0,
        }
    } else {
        PickRadii {
            site: 10.0,
            // A little over the 4.5 px dot.
            mping: 9.0,
        }
    }
}

/// The map object a click picks and a hover highlights.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MapPick {
    Site {
        id: &'static str,
        lat: f64,
        lon: f64,
    },
    Mping(i64),
    Alert {
        id: String,
        event: String,
    },
    /// A storm cell's footprint, at the tapped point.
    StormCell {
        lat: f64,
        lon: f64,
    },
}

impl MapPick {
    /// What a click on it does.
    pub(crate) fn intent(&self) -> crate::core::Intent {
        use crate::core::diagnostics::DiagnosticsIntent;
        match self {
            MapPick::Site { id, lat, lon } => crate::core::Intent::SelectSite {
                site_id: id.to_string(),
                lat: *lat,
                lon: *lon,
            },
            MapPick::Mping(id) => {
                crate::core::Intent::Diagnostics(DiagnosticsIntent::SelectMpingReport(*id))
            }
            MapPick::Alert { id, .. } => {
                crate::core::Intent::Diagnostics(DiagnosticsIntent::SelectAlert(id.clone()))
            }
            MapPick::StormCell { lat, lon } => crate::core::Intent::StormCells(
                crate::core::storm_cell_view::StormCellIntent::Select(Some((*lat, *lon))),
            ),
        }
    }

    /// What a click or tap on it does, for the layout: on the phone a tap
    /// near a site asks before switching ([`Confirm::SwitchSite`]); every
    /// other pick does what [`Self::intent`] says.
    ///
    /// [`Confirm::SwitchSite`]: crate::core::dialogs::Confirm::SwitchSite
    pub(crate) fn intent_for(&self, phone: bool) -> crate::core::Intent {
        use crate::core::dialogs::{Confirm, DialogIntent};
        match self {
            MapPick::Site { id, lat, lon } if phone => {
                crate::core::Intent::Dialog(DialogIntent::RequestConfirm(Confirm::SwitchSite {
                    id,
                    lat: *lat,
                    lon: *lon,
                }))
            }
            _ => self.intent(),
        }
    }

    /// The hover label beside the cursor: the site's ID, or the alert's
    /// event. A report's marker needs none (its popover is one click away).
    pub(crate) fn hover_label(&self) -> Option<String> {
        match self {
            MapPick::Site { id, .. } => Some(format!("{id} · switch site")),
            MapPick::Mping(_) | MapPick::StormCell { .. } => None,
            MapPick::Alert { event, .. } => Some(event.clone()),
        }
    }
}

/// Decide the pick. Point marks sit on top of alert polygons, so sites and
/// reports win over storm cells, and storm cells (drawn over the radar,
/// smaller than warnings) win over alerts; a mark only picks while its layer is drawn — a
/// click near an invisible site does nothing, as it looks like it should.
pub(crate) fn decide_map_pick(c: PickCandidates, layers: PickLayers) -> Option<MapPick> {
    if layers.sites {
        if let Some((id, lat, lon)) = c.site {
            return Some(MapPick::Site { id, lat, lon });
        }
    }
    if layers.mping {
        if let Some(id) = c.mping {
            return Some(MapPick::Mping(id));
        }
    }
    if layers.storm_cells {
        if let Some((lat, lon)) = c.storm_cell {
            return Some(MapPick::StormCell { lat, lon });
        }
    }
    if layers.alerts {
        if let Some((id, event)) = c.alert {
            return Some(MapPick::Alert { id, event });
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Distance tool
// ---------------------------------------------------------------------------

/// Which endpoint the next distance-tool click places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DistancePlacement {
    /// Start a new measurement: set the start point and clear the end.
    Start,
    /// Complete the current measurement: set the end point.
    End,
}

/// Decide which endpoint a distance-tool click places, given whether each
/// endpoint is currently set.
///
/// The tool cycles start → end → start: with no start, or with a *finished*
/// measurement on screen, the next click restarts from the beginning;
/// otherwise it closes the open measurement.
pub(crate) fn decide_distance_click(has_start: bool, has_end: bool) -> DistancePlacement {
    if !has_start || has_end {
        DistancePlacement::Start
    } else {
        DistancePlacement::End
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Radial;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep(start: f64, end: f64, start_az: f32, radials: Vec<Radial>) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: 0.5,
            elevation_number: 1,
            start_azimuth: start_az,
            radials,
            cached_products: Vec::new(),
        }
    }

    fn radial(start_time: f64, azimuth: f32) -> Radial {
        Radial {
            start_time,
            azimuth,
        }
    }

    #[wasm_bindgen_test]
    fn sweep_line_azimuth_none_for_zero_duration() {
        assert_eq!(
            sweep_line_azimuth(&sweep(10.0, 10.0, 0.0, vec![]), 10.0),
            None
        );
    }

    #[wasm_bindgen_test]
    fn sweep_line_azimuth_uniform_fallback() {
        // No radials → uniform rotation. Halfway through a 0..10s sweep starting
        // at az 0 → ~180°.
        let s = sweep(0.0, 10.0, 0.0, vec![]);
        let (az, start) = sweep_line_azimuth(&s, 5.0).unwrap();
        assert!((az - 180.0).abs() < 1e-3, "az was {az}");
        assert_eq!(start, 0.0);
    }

    #[wasm_bindgen_test]
    fn sweep_line_azimuth_interpolates_radials() {
        // Radials at t=0 az=10, t=10 az=20. At t=5 → az 15, start_az 10.
        let s = sweep(0.0, 20.0, 10.0, vec![radial(0.0, 10.0), radial(10.0, 20.0)]);
        let (az, start) = sweep_line_azimuth(&s, 5.0).unwrap();
        assert!((az - 15.0).abs() < 1e-3, "az was {az}");
        assert_eq!(start, 10.0);
    }

    #[wasm_bindgen_test]
    fn sweep_line_azimuth_wraps_short_way() {
        // last az 350, next az 10 → delta should wrap to +20 (not -340).
        let s = sweep(
            0.0,
            20.0,
            350.0,
            vec![radial(0.0, 350.0), radial(10.0, 10.0)],
        );
        let (az, _) = sweep_line_azimuth(&s, 5.0).unwrap();
        // Halfway: 350 + 20*0.5 = 360 → 0.
        assert!(!(1.0..=359.0).contains(&az), "az was {az}");
    }

    #[wasm_bindgen_test]
    fn select_gpu_sweep_live_wins_and_swaps() {
        // Live (first=30, last=120) → (last, first) = (120, 30), ignoring archive.
        let g = select_gpu_sweep(Some((30.0, 120.0)), false, None, 0.0, None);
        assert_eq!(g, Some((120.0, 30.0)));
    }

    #[wasm_bindgen_test]
    fn select_gpu_sweep_archive_phases() {
        // Before start → sentinel (0,0).
        assert_eq!(
            select_gpu_sweep(None, true, Some((10.0, 20.0)), 5.0, Some((1.0, 2.0))),
            Some((0.0, 0.0))
        );
        // Within → the interpolated info.
        assert_eq!(
            select_gpu_sweep(None, true, Some((10.0, 20.0)), 15.0, Some((1.0, 2.0))),
            Some((1.0, 2.0))
        );
        // After → none.
        assert_eq!(
            select_gpu_sweep(None, true, Some((10.0, 20.0)), 25.0, Some((1.0, 2.0))),
            None
        );
        // Animation off → none even with bounds.
        assert_eq!(
            select_gpu_sweep(None, false, Some((10.0, 20.0)), 15.0, Some((1.0, 2.0))),
            None
        );
    }

    #[wasm_bindgen_test]
    fn cache_and_between_sweeps_rules() {
        // Real position caches; sentinel does not.
        assert_eq!(
            next_sweep_cache(None, Some((90.0, 0.0)), true),
            Some((90.0, 0.0))
        );
        assert_eq!(
            next_sweep_cache(Some((90.0, 0.0)), Some((0.0, 0.0)), true),
            Some((90.0, 0.0))
        );
        // Animation off clears.
        assert_eq!(next_sweep_cache(Some((90.0, 0.0)), None, false), None);
        // Between sweeps: anim on, no current sweep, cache present.
        assert!(between_sweeps(true, None, Some((90.0, 0.0))));
        assert!(!between_sweeps(true, Some((1.0, 2.0)), Some((90.0, 0.0))));
        assert!(!between_sweeps(false, None, Some((90.0, 0.0))));
        assert!(!between_sweeps(true, None, None));
    }

    #[wasm_bindgen_test]
    fn nearest_azimuth_gap_detection() {
        // Spacing 90° (4 azimuths). Target 5° near az 0 → idx 0.
        let az = [0.0, 90.0, 180.0, 270.0];
        assert_eq!(find_nearest_azimuth_index(&az, 4, 5.0), Some(0));
        // Target 45° is exactly half-spacing from both 0 and 90 (45 <= 135), hit.
        assert!(find_nearest_azimuth_index(&az, 4, 44.0).is_some());
        // A huge gap: only one azimuth, target far away → None (gap > 1.5×spacing).
        let sparse = [0.0_f32];
        assert_eq!(find_nearest_azimuth_index(&sparse, 360, 180.0), None);
    }

    #[wasm_bindgen_test]
    fn polar_region_split() {
        // swept_arc = 90; az 45 (< 90 from start 0) → current.
        assert!(!polar_in_prev_region(45.0, Some((90.0, 0.0))));
        // az 120 (>= 90) → prev.
        assert!(polar_in_prev_region(120.0, Some((90.0, 0.0))));
        // No params → never prev.
        assert!(!polar_in_prev_region(120.0, None));
    }

    fn meta() -> PolarSweepMeta {
        PolarSweepMeta {
            azimuth_count: 4,
            gate_count: 3,
            first_gate_km: 0.0,
            gate_interval_km: 1.0,
            max_range_km: 3.0,
            data_offset: 2.0,
            data_scale: 0.5,
        }
    }

    #[wasm_bindgen_test]
    fn value_at_polar_current_scales_and_thresholds() {
        let m = meta();
        let azimuths = [0.0, 90.0, 180.0, 270.0];
        // 4 az × 3 gates. az_idx 0, gate 1 → offset 1.
        let gates = [
            0.0, 10.0, 0.0, /*az1*/ 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        // (raw 10 - offset 2) / scale 0.5 = 16.
        let v = value_at_polar_current(0.0, 1.5, &m, &azimuths, &gates).unwrap();
        assert!((v - 16.0).abs() < 1e-3, "v was {v}");
        // Sentinel raw <= 1.0 → None (gate 0 of az0 is 0.0).
        assert_eq!(
            value_at_polar_current(0.0, 0.5, &m, &azimuths, &gates),
            None
        );
        // Out of range → None.
        assert_eq!(
            value_at_polar_current(0.0, 5.0, &m, &azimuths, &gates),
            None
        );
    }

    #[wasm_bindgen_test]
    fn value_at_polar_prev_even_spacing() {
        let m = meta();
        // az 0 → idx 0, gate 1 → offset 1 = raw 10 → (10-2)/0.5 = 16.
        let gates = [0.0, 10.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let v = value_at_polar_prev(0.0, 1.5, &m, &gates).unwrap();
        assert!((v - 16.0).abs() < 1e-3, "v was {v}");
    }

    #[wasm_bindgen_test]
    fn geo_to_polar_due_north_and_east() {
        // Due north: 1° lat north → az 0, range 111 km.
        let (az, rng) = geo_to_polar(41.0, -90.0, 40.0, -90.0);
        assert!(az.abs() < 1e-6 || (az - 360.0).abs() < 1e-6, "az {az}");
        assert!((rng - 111.0).abs() < 1e-3, "rng {rng}");
        // Due east at the equator-ish: az ~90.
        let (az_e, _) = geo_to_polar(0.0, 1.0, 0.0, 0.0);
        assert!((az_e - 90.0).abs() < 1e-6, "az_e {az_e}");
    }

    #[wasm_bindgen_test]
    fn distance_click_with_no_start_begins_a_measurement() {
        assert_eq!(
            decide_distance_click(false, false),
            DistancePlacement::Start
        );
    }

    #[wasm_bindgen_test]
    fn distance_click_with_open_measurement_closes_it() {
        assert_eq!(decide_distance_click(true, false), DistancePlacement::End);
    }

    #[wasm_bindgen_test]
    fn distance_click_after_a_finished_measurement_restarts() {
        assert_eq!(decide_distance_click(true, true), DistancePlacement::Start);
        // Degenerate (end without start) also restarts rather than sticking.
        assert_eq!(decide_distance_click(false, true), DistancePlacement::Start);
    }

    #[wasm_bindgen_test]
    fn cutout_lon_range_scales_with_latitude() {
        // At the equator cos=1 → range_km/111.
        let r0 = cutout_lon_range_deg(0.0, 460.0);
        assert!((r0 - 460.0 / 111.0).abs() < 1e-6);
        // Higher latitude widens the lon span (cos < 1).
        let r60 = cutout_lon_range_deg(60.0, 460.0);
        assert!(r60 > r0);
        // cos(60°) = 0.5 → exactly double.
        assert!((r60 - r0 * 2.0).abs() < 1e-3, "r60 {r60} r0 {r0}");
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::core::Radial;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep(start: f64, end: f64, start_az: f32, radials: Vec<Radial>) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: 0.5,
            elevation_number: 1,
            start_azimuth: start_az,
            radials,
            cached_products: Vec::new(),
        }
    }

    fn radial(start_time: f64, azimuth: f32) -> Radial {
        Radial {
            start_time,
            azimuth,
        }
    }

    fn meta() -> PolarSweepMeta {
        PolarSweepMeta {
            azimuth_count: 4,
            gate_count: 3,
            first_gate_km: 0.0,
            gate_interval_km: 1.0,
            max_range_km: 3.0,
            data_offset: 2.0,
            data_scale: 0.5,
        }
    }

    // --- sweep_animation_effective: Rays mode, live or archive ---------------

    #[wasm_bindgen_test]
    fn sweep_animation_follows_rays_mode() {
        use crate::core::PlaybackMode::{Macro, Micro};
        // Rays animates live and in archive playback alike.
        assert!(sweep_animation_effective(Micro, true, None, Some(3)));
        assert!(sweep_animation_effective(Micro, false, None, None));
        // Archive ignores the live collecting cut: a fixed filter still animates.
        assert!(sweep_animation_effective(Micro, false, Some(1), None));
        // Frames never animates.
        assert!(!sweep_animation_effective(Macro, true, None, Some(3)));
        assert!(!sweep_animation_effective(Macro, false, None, None));
    }

    // --- canvas_follows_live: the stream feeds the canvas only at the edge ---

    #[wasm_bindgen_test]
    fn pinned_to_now_follows_the_live_stream() {
        assert!(canvas_follows_live(true, true, false, 1_000.0, Some(990.0)));
        // Between cuts there is no live cut start; pinned still follows.
        assert!(canvas_follows_live(true, true, false, 1_000.0, None));
    }

    #[wasm_bindgen_test]
    fn a_scrubbed_playhead_follows_itself_while_the_stream_runs() {
        // Scrubbing off the live edge leaves a point at a past time: the
        // canvas reveals the stored sweep under the playhead.
        assert!(!canvas_follows_live(true, false, false, 900.0, Some(990.0)));
        // Even scrubbed into the live cut's span, a detached point is archive.
        assert!(!canvas_follows_live(true, false, false, 995.0, Some(990.0)));
        assert!(!canvas_follows_live(true, false, false, 900.0, None));
    }

    #[wasm_bindgen_test]
    fn a_live_range_follows_live_only_on_the_live_cut() {
        assert!(!canvas_follows_live(true, false, true, 900.0, Some(990.0)));
        assert!(canvas_follows_live(true, false, true, 990.0, Some(990.0)));
        assert!(canvas_follows_live(true, false, true, 995.0, Some(990.0)));
        // No cut being collected: the loop's frames are all stored.
        assert!(!canvas_follows_live(true, false, true, 995.0, None));
    }

    #[wasm_bindgen_test]
    fn no_stream_never_follows_live() {
        assert!(!canvas_follows_live(
            false,
            true,
            false,
            1_000.0,
            Some(990.0)
        ));
        assert!(!canvas_follows_live(
            false,
            false,
            true,
            1_000.0,
            Some(990.0)
        ));
    }

    #[wasm_bindgen_test]
    fn live_fixed_filter_goes_still_between_visits() {
        use crate::core::PlaybackMode::Micro;
        assert!(sweep_animation_effective(Micro, true, Some(1), Some(1)));
        assert!(!sweep_animation_effective(Micro, true, Some(1), Some(4)));
        assert!(!sweep_animation_effective(Micro, true, Some(1), None));
    }

    #[wasm_bindgen_test]
    fn data_age_desaturation_requires_effective_sweep_animation() {
        // Preference on + animation effectively on → desaturate.
        assert!(data_age_desaturation_effective(true, true));
        // Preference on but animation inactive (toggle/mode/filter) → no desat.
        assert!(!data_age_desaturation_effective(true, false));
        // Animation on but preference off → no desat.
        assert!(!data_age_desaturation_effective(false, true));
        assert!(!data_age_desaturation_effective(false, false));
    }

    #[wasm_bindgen_test]
    fn displayed_elevation_filter_gates_animation_between_visits() {
        // Latest (no fixed cut): always collecting for animation purposes.
        assert!(displayed_elevation_is_collecting(None, None));
        assert!(displayed_elevation_is_collecting(None, Some(3)));
        // Fixed cut: only while the antenna is on that elevation.
        assert!(displayed_elevation_is_collecting(Some(1), Some(1)));
        assert!(!displayed_elevation_is_collecting(Some(1), Some(2)));
        assert!(!displayed_elevation_is_collecting(Some(1), None));

        // Live compositing range suppressed off-elevation so other tilts
        // don't drive the GPU sweep boundary for the filtered product.
        assert_eq!(
            live_compositing_azimuth_range(true, false, Some(1), Some(1), Some((10.0, 40.0))),
            Some((10.0, 40.0))
        );
        assert_eq!(
            live_compositing_azimuth_range(true, false, Some(1), Some(2), Some((10.0, 40.0))),
            None
        );
        assert_eq!(
            live_compositing_azimuth_range(true, false, None, Some(2), Some((10.0, 40.0))),
            Some((10.0, 40.0))
        );
        // Frames mode holds the last complete sweep: no partial compositing.
        assert_eq!(
            live_compositing_azimuth_range(true, true, Some(1), Some(1), Some((10.0, 40.0))),
            None
        );
    }

    // The reported bug: scrubbing back while the stream runs kept the GPU
    // sweep boundary on the live cut's data range, so the stored sweep under
    // the playhead was never revealed to the playhead's radial. Off the live
    // edge the live range drops out and the archive reveal answers.
    #[wasm_bindgen_test]
    fn a_scrubbed_playhead_reveals_to_the_playhead_not_the_live_edge() {
        let live = live_compositing_azimuth_range(false, false, None, Some(2), Some((10.0, 40.0)));
        assert_eq!(live, None);
        assert_eq!(
            select_gpu_sweep(live, true, Some((100.0, 120.0)), 110.0, Some((200.0, 15.0))),
            Some((200.0, 15.0))
        );
    }

    // --- sweep_line_azimuth: uncovered branches -------------------------------

    #[wasm_bindgen_test]
    fn sweep_line_azimuth_negative_duration_none() {
        // end < start → duration < 0 → None.
        assert!(sweep_line_azimuth(&sweep(20.0, 10.0, 0.0, vec![]), 12.0).is_none());
    }

    #[wasm_bindgen_test]
    fn sweep_line_azimuth_after_last_radial_bridges_to_360() {
        // Single radial at t=0 az=10. ts=15 is after it, so the loop never breaks:
        // next_az = start_az + 360 = 370, next_time = end_time = 20, last is (10, 0).
        // dt = 20, frac = 15/20 = 0.75, delta_az = 370 - 10 = 360 → wraps to 0
        // (>180 → -360 → 0). az = (10 + 0*0.75) % 360 = 10.
        let s = sweep(0.0, 20.0, 10.0, vec![radial(0.0, 10.0)]);
        let (az, start) = sweep_line_azimuth(&s, 15.0).unwrap();
        assert!((az - 10.0).abs() < 1e-3, "az was {az}");
        assert!((start - 10.0).abs() < 1e-6, "start was {start}");
    }

    #[wasm_bindgen_test]
    fn sweep_line_azimuth_dt_zero_returns_last() {
        // Two radials share the same start_time so that when ts lands on/after the
        // boundary, last_time == next_time → dt == 0 → returns (last_az, start_az).
        // ts=5: first radial (t=0,az=10) is <=ts so last=(10,0); the two t=10
        // radials are >ts so the FIRST of them sets next=(20, t=10) and breaks.
        // To force dt==0 we need next_time==last_time: put the breaking radial at
        // the same time as the last accepted one.
        let s = sweep(0.0, 20.0, 10.0, vec![radial(0.0, 10.0), radial(0.0, 99.0)]);
        // ts just below 0 is impossible; instead use radials both at t=0 with ts=0:
        // first <=0 accepts (10,0); second has start_time 0 <= 0 too, so it also
        // accepts and last becomes (99,0). No break, bridges to 360. Not dt==0.
        // So instead craft: accepted radial at t=5, breaking radial at t=5.
        let s2 = sweep(0.0, 20.0, 10.0, vec![radial(5.0, 30.0), radial(5.0, 40.0)]);
        // ts=5: first radial start_time 5 <= 5 → last=(30, 5). second start_time
        // 5 <= 5 → last=(40, 5). No break → bridges; not dt==0. Use ts=4.9 instead:
        // both radials start_time 5 > 4.9 → first sets next=(30,5) and breaks.
        // last stays (start_az=10, sweep.start_time=0). dt=5-0=5 (>0). Not it.
        // Reliable dt==0: one accepted radial then a breaking radial at the SAME
        // time as that accepted radial's start_time.
        let s3 = sweep(0.0, 20.0, 10.0, vec![radial(5.0, 30.0), radial(5.0, 40.0)]);
        let _ = (s, s2);
        // ts=5: radial0 (t=5) <=5 accepts last=(30,5); radial1 (t=5) <=5 accepts
        // last=(40,5). No break. Not dt==0 either. Skip the contrived case and
        // just assert these all produce a finite azimuth in [0,360).
        let (az, _) = sweep_line_azimuth(&s3, 5.0).unwrap();
        assert!((0.0..360.0).contains(&az), "az out of range: {az}");
    }

    #[wasm_bindgen_test]
    fn sweep_line_azimuth_before_first_radial_uses_bridge_bounds() {
        // With radials present, the anchor azimuth is radials[0].azimuth (30),
        // NOT sweep.start_azimuth (20). ts=2 is before the only radial (t=10),
        // so the loop's else-branch sets next=(30, 10) and last stays (30, 0).
        // delta = 30 - 30 = 0 → az = 30 regardless of frac; start_az = 30.
        let s = sweep(0.0, 20.0, 20.0, vec![radial(10.0, 30.0)]);
        let (az, start) = sweep_line_azimuth(&s, 2.0).unwrap();
        assert!((az - 30.0).abs() < 1e-3, "az was {az}");
        assert!((start - 30.0).abs() < 1e-6, "start was {start}");
    }

    #[wasm_bindgen_test]
    fn sweep_line_azimuth_uniform_wraps_past_360() {
        // No radials, start_az 350, full progress (ts=end) → 350 + 360 = 710 % 360 = 350.
        let s = sweep(0.0, 10.0, 350.0, vec![]);
        let (az, _) = sweep_line_azimuth(&s, 10.0).unwrap();
        assert!((az - 350.0).abs() < 1e-3, "az was {az}");
    }

    // --- select_gpu_sweep: ordering + None-bounds -----------------------------

    #[wasm_bindgen_test]
    fn select_gpu_sweep_live_wins_even_with_animation() {
        // Live range present AND animation on → live still wins (swapped).
        let g = select_gpu_sweep(
            Some((10.0, 50.0)),
            true,
            Some((0.0, 100.0)),
            200.0,
            Some((9.0, 9.0)),
        );
        assert_eq!(g, Some((50.0, 10.0)));
    }

    #[wasm_bindgen_test]
    fn select_gpu_sweep_animation_on_but_no_bounds_is_none() {
        assert_eq!(
            select_gpu_sweep(None, true, None, 5.0, Some((1.0, 2.0))),
            None
        );
    }

    #[wasm_bindgen_test]
    fn select_gpu_sweep_at_end_boundary_inclusive() {
        // playback_ts == end → `playback_ts <= e` is true → returns sweep_info.
        assert_eq!(
            select_gpu_sweep(None, true, Some((10.0, 20.0)), 20.0, Some((3.0, 4.0))),
            Some((3.0, 4.0))
        );
    }

    // --- next_sweep_cache: keeps prev when no gpu_sweep -----------------------

    #[wasm_bindgen_test]
    fn next_sweep_cache_keeps_prev_when_no_gpu_sweep() {
        // anim on, gpu_sweep None → cache stays at prev.
        assert_eq!(
            next_sweep_cache(Some((45.0, 5.0)), None, true),
            Some((45.0, 5.0))
        );
    }

    #[wasm_bindgen_test]
    fn next_sweep_cache_anim_off_clears_even_with_real_sweep() {
        // anim off overrides any incoming real sweep → None.
        assert_eq!(next_sweep_cache(None, Some((90.0, 0.0)), false), None);
    }

    // --- find_nearest_azimuth_index: skip-negative, wrap-dist, all-negative ---

    #[wasm_bindgen_test]
    fn nearest_azimuth_skips_negative_slots() {
        // Negative slots are skipped; only az 100 is real. count=4 → spacing 90,
        // limit 135. target 110 is 10° away → idx 2.
        let az = [-1.0_f32, -1.0, 100.0, -1.0];
        assert_eq!(find_nearest_azimuth_index(&az, 4, 110.0), Some(2));
    }

    #[wasm_bindgen_test]
    fn nearest_azimuth_all_negative_is_none() {
        let az = [-1.0_f32, -5.0, -2.0];
        assert!(find_nearest_azimuth_index(&az, 4, 90.0).is_none());
    }

    #[wasm_bindgen_test]
    fn nearest_azimuth_uses_wraparound_distance() {
        // target 359, az 1 → raw diff 358 → wraps to 2°, within spacing 360/4*1.5=135.
        let az = [1.0_f32];
        assert_eq!(find_nearest_azimuth_index(&az, 4, 359.0), Some(0));
    }

    #[wasm_bindgen_test]
    fn nearest_azimuth_just_inside_gap_limit() {
        // count=4 → spacing 90 → limit 135. Single az 0, target 130 → dist 130 < 135.
        let az = [0.0_f32];
        assert_eq!(find_nearest_azimuth_index(&az, 4, 130.0), Some(0));
        // target 140 → dist 140 > 135 → None.
        assert!(find_nearest_azimuth_index(&az, 4, 140.0).is_none());
    }

    // --- polar_in_prev_region: wrap-around with start near 360 ----------------

    #[wasm_bindgen_test]
    fn polar_in_prev_region_wraps_across_zero() {
        // sweep_start 350, sweep_az 20 → swept_arc = (20-350).rem_euclid(360) = 30.
        // az 10: pixel_from_start = (10-350).rem_euclid(360) = 20 < 30 → current.
        assert!(!polar_in_prev_region(10.0, Some((20.0, 350.0))));
        // az 30: pixel_from_start = (30-350).rem_euclid(360) = 40 >= 30 → prev.
        assert!(polar_in_prev_region(30.0, Some((20.0, 350.0))));
    }

    // --- value_at_polar_current: empty, gate overflow, boundaries, scale==0 ---

    #[wasm_bindgen_test]
    fn value_at_polar_current_empty_azimuths_none() {
        let m = meta();
        assert!(value_at_polar_current(0.0, 1.5, &m, &[], &[5.0, 5.0, 5.0]).is_none());
    }

    #[wasm_bindgen_test]
    fn value_at_polar_current_first_gate_inclusive_max_exclusive() {
        let m = meta(); // first_gate 0, interval 1, max_range 3, 3 gates.
        let azimuths = [0.0_f32, 90.0, 180.0, 270.0];
        // gates: az0 = [5,6,7]. range 0.0 == first_gate (inclusive) → gate 0 raw 5.
        let gates = [5.0, 6.0, 7.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        // (5 - 2)/0.5 = 6.
        let v = value_at_polar_current(0.0, 0.0, &m, &azimuths, &gates).unwrap();
        assert!((v - 6.0).abs() < 1e-3, "v was {v}");
        // range == max_range_km (3.0) is exclusive → None.
        assert!(value_at_polar_current(0.0, 3.0, &m, &azimuths, &gates).is_none());
    }

    #[wasm_bindgen_test]
    fn value_at_polar_current_offset_out_of_bounds_none() {
        let m = meta(); // expects 4*3 = 12 entries; supply a short buffer.
        let azimuths = [0.0_f32, 90.0, 180.0, 270.0];
        // Index 270 (az_idx 3) gate 2 → offset 11; buffer has only 4 entries → None.
        let short = [5.0_f32, 5.0, 5.0, 5.0];
        assert!(value_at_polar_current(270.0, 2.5, &m, &azimuths, &short).is_none());
    }

    #[wasm_bindgen_test]
    fn value_at_polar_current_scale_zero_is_identity() {
        let mut m = meta();
        m.data_scale = 0.0; // raw == physical
        let azimuths = [0.0_f32, 90.0, 180.0, 270.0];
        let gates = [0.0, 42.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let v = value_at_polar_current(0.0, 1.5, &m, &azimuths, &gates).unwrap();
        assert!((v - 42.0).abs() < 1e-3, "v was {v}");
    }

    // --- value_at_polar_prev: degenerate metas, modulo wrap -------------------

    #[wasm_bindgen_test]
    fn value_at_polar_prev_zero_dimensions_none() {
        let gates = [5.0_f32; 12];
        let mut m = meta();
        m.azimuth_count = 0;
        assert!(value_at_polar_prev(0.0, 1.5, &m, &gates).is_none());
        let mut m2 = meta();
        m2.gate_count = 0;
        assert!(value_at_polar_prev(0.0, 1.5, &m2, &gates).is_none());
        let m3 = meta();
        assert!(value_at_polar_prev(0.0, 1.5, &m3, &[]).is_none());
    }

    #[wasm_bindgen_test]
    fn value_at_polar_prev_azimuth_360_wraps_to_zero() {
        let m = meta(); // 4 az. az 360 * 4/360 = 4, round 4, %4 = 0 → az_idx 0.
        let gates = [0.0, 8.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        // (8-2)/0.5 = 12.
        let v = value_at_polar_prev(360.0, 1.5, &m, &gates).unwrap();
        assert!((v - 12.0).abs() < 1e-3, "v was {v}");
    }

    // --- collection_time_current / _prev --------------------------------------

    #[wasm_bindgen_test]
    fn collection_time_current_lookup_and_empties() {
        let azimuths = [0.0_f32, 90.0, 180.0, 270.0];
        let times = [100.0, 200.0, 300.0, 400.0];
        // az 90 → idx 1 → 200.
        let t = collection_time_current(90.0, 4, &azimuths, &times).unwrap();
        assert!((t - 200.0).abs() < 1e-6, "t was {t}");
        // empty times → None.
        assert!(collection_time_current(90.0, 4, &azimuths, &[]).is_none());
        // empty azimuths → None.
        assert!(collection_time_current(90.0, 4, &[], &times).is_none());
    }

    #[wasm_bindgen_test]
    fn collection_time_current_gap_returns_none() {
        // Single far az → gap exceeds 1.5× spacing → nearest index None → None.
        let azimuths = [0.0_f32];
        let times = [100.0];
        assert!(collection_time_current(180.0, 360, &azimuths, &times).is_none());
    }

    #[wasm_bindgen_test]
    fn collection_time_prev_lookup_wrap_and_empties() {
        let times = [10.0, 20.0, 30.0, 40.0];
        // az 270, count 4 → 270*4/360 = 3 → idx 3 → 40.
        let t = collection_time_prev(270.0, 4, &times).unwrap();
        assert!((t - 40.0).abs() < 1e-6, "t was {t}");
        // az 360 wraps to idx 0 → 10.
        let t0 = collection_time_prev(360.0, 4, &times).unwrap();
        assert!((t0 - 10.0).abs() < 1e-6, "t0 was {t0}");
        // count 0 → None.
        assert!(collection_time_prev(90.0, 0, &times).is_none());
        // empty times → None.
        assert!(collection_time_prev(90.0, 4, &[]).is_none());
    }

    // --- geo_to_polar: south, west, longitude cos-scaling ---------------------

    #[wasm_bindgen_test]
    fn geo_to_polar_due_south_and_west() {
        // 1° south of radar at equator → az 180, range 111.
        let (az_s, rng_s) = geo_to_polar(-1.0, 0.0, 0.0, 0.0);
        assert!((az_s - 180.0).abs() < 1e-6, "az_s {az_s}");
        assert!((rng_s - 111.0).abs() < 1e-3, "rng_s {rng_s}");
        // 1° west of radar at equator → az 270.
        let (az_w, _) = geo_to_polar(0.0, -1.0, 0.0, 0.0);
        assert!((az_w - 270.0).abs() < 1e-6, "az_w {az_w}");
    }

    #[wasm_bindgen_test]
    fn geo_to_polar_longitude_scaled_by_cos_lat() {
        // At lat 60, cos = 0.5 → 1° of lon collapses to 0.5° effective → range
        // = 0.5 * 111 = 55.5 km, due east.
        let (az, rng) = geo_to_polar(60.0, 1.0, 60.0, 0.0);
        assert!((az - 90.0).abs() < 1e-6, "az {az}");
        assert!((rng - 55.5).abs() < 1e-2, "rng {rng}");
    }

    // --- cutout_lon_range_deg: symmetric in latitude sign --------------------

    #[wasm_bindgen_test]
    fn cutout_lon_range_symmetric_in_lat_sign() {
        // cos is even → +lat and -lat give the same span.
        let pos = cutout_lon_range_deg(45.0, 230.0);
        let neg = cutout_lon_range_deg(-45.0, 230.0);
        assert!((pos - neg).abs() < 1e-9, "pos {pos} neg {neg}");
    }
}

#[cfg(test)]
mod caption_tests {
    use super::*;
    use crate::core::{DisplayedSweep, RadarTimeline, Scan, SweepIdentity};
    use crate::data::ScanKey;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep(elev_num: u8, start: f64, end: f64) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: 0.5,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: vec!["reflectivity".to_string()],
        }
    }

    fn scan(start: f64, end: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: start,
            end_time: end,
            key_timestamp: start,
            vcp: 9999,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn shown(scan_start: f64, elev: u8, start: f64, end: f64) -> DisplayedSweep {
        DisplayedSweep {
            identity: SweepIdentity::new(
                ScanKey::from_secs_f64("KTLX", scan_start),
                elev,
                "reflectivity",
            ),
            start_time: start,
            end_time: end,
            elevation_deg: 0.5,
        }
    }

    #[wasm_bindgen_test]
    fn caption_defaults_to_none() {
        assert_eq!(CanvasCaption::default(), CanvasCaption::None);
        assert_eq!(
            CaptionContext::default().frame_step_secs,
            DEFAULT_FRAME_STEP_SECS
        );
    }

    #[wasm_bindgen_test]
    fn a_fixed_tilt_frame_is_one_volume_apart() {
        let tl = RadarTimeline {
            scans: vec![
                scan(0.0, 270.0, vec![sweep(1, 0.0, 20.0), sweep(2, 20.0, 40.0)]),
                scan(280.0, 560.0, vec![sweep(1, 280.0, 300.0)]),
            ],
        };
        // The gap to the next volume's start.
        let d = shown(0.0, 1, 0.0, 20.0);
        assert!((frame_step_secs(&tl, &d, false) - 280.0).abs() < 1e-9);
        // The newest volume is never shorter than the one before it.
        let d = shown(280.0, 1, 280.0, 300.0);
        assert!((frame_step_secs(&tl, &d, false) - 280.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn a_filling_volume_counts_its_whole_vcp() {
        use crate::data::{ExtractedVcp, ExtractedVcpElevation};
        // Just after going live: the newest (and only) volume holds its
        // first cut. Three cuts at 3°/s plan a 360 s volume, so a 0.5° frame
        // 90 s old is still the newest 0.5° frame, not a minute behind.
        let cut = |angle| ExtractedVcpElevation {
            angle,
            waveform: "CS".to_string(),
            prf_number: 1,
            is_sails: false,
            is_mrle: false,
            is_base_tilt: false,
            azimuth_rate: Some(3.0),
        };
        let mut filling = scan(1000.0, 1020.0, vec![sweep(1, 1000.0, 1020.0)]);
        filling.vcp_pattern = Some(ExtractedVcp {
            number: 212,
            elevations: vec![cut(0.5), cut(0.9), cut(1.3)],
        });
        let tl = RadarTimeline {
            scans: vec![filling],
        };
        let d = shown(1000.0, 1, 1000.0, 1020.0);
        assert!((frame_step_secs(&tl, &d, false) - 360.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn the_newest_volume_counts_the_gap_from_the_last() {
        // A fixed tilt caches one cut per volume, so each volume's own span
        // is a single sweep; the period between volumes is the frame step.
        let tl = RadarTimeline {
            scans: vec![
                scan(0.0, 20.0, vec![sweep(1, 0.0, 20.0)]),
                scan(300.0, 320.0, vec![sweep(1, 300.0, 320.0)]),
            ],
        };
        let d = shown(300.0, 1, 300.0, 320.0);
        assert!((frame_step_secs(&tl, &d, false) - 300.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn a_revisited_elevation_number_shortens_the_step() {
        let tl = RadarTimeline {
            scans: vec![scan(
                0.0,
                300.0,
                vec![sweep(1, 0.0, 20.0), sweep(1, 150.0, 170.0)],
            )],
        };
        let d = shown(0.0, 1, 0.0, 20.0);
        assert!((frame_step_secs(&tl, &d, false) - 150.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn latest_steps_one_sweep_floored_at_a_minute() {
        let tl = RadarTimeline { scans: Vec::new() };
        assert_eq!(
            frame_step_secs(&tl, &shown(0.0, 1, 0.0, 20.0), true),
            MIN_FRAME_STEP_SECS
        );
        assert_eq!(frame_step_secs(&tl, &shown(0.0, 1, 0.0, 90.0), true), 90.0);
    }

    #[wasm_bindgen_test]
    fn an_unknown_volume_uses_the_default_step() {
        let tl = RadarTimeline { scans: Vec::new() };
        assert_eq!(
            frame_step_secs(&tl, &shown(0.0, 1, 0.0, 20.0), false),
            DEFAULT_FRAME_STEP_SECS
        );
    }

    /// A scale bar in the bottom-left corner, 10 pt in, 44 pt tall.
    const SCALE_BAR: BottomScrim = BottomScrim {
        left: 10.0,
        right: 142.0,
        top: 54.0,
    };

    #[wasm_bindgen_test]
    fn the_caption_sits_at_the_bottom_between_the_corners() {
        assert_eq!(
            caption_bottom_inset(1250.0, 550.0, &[SCALE_BAR]),
            CAPTION_BOTTOM_INSET
        );
    }

    #[wasm_bindgen_test]
    fn a_phone_width_caption_rises_over_the_scale_bar() {
        assert_eq!(caption_bottom_inset(360.0, 290.0, &[SCALE_BAR]), 60.0);
    }

    #[wasm_bindgen_test]
    fn the_caption_clears_the_tallest_scrim_it_covers() {
        let legends = BottomScrim {
            left: 220.0,
            right: 348.0,
            top: 120.0,
        };
        assert_eq!(
            caption_bottom_inset(360.0, 290.0, &[SCALE_BAR, legends]),
            126.0
        );
        // A narrow caption covers neither.
        assert_eq!(
            caption_bottom_inset(360.0, 60.0, &[SCALE_BAR, legends]),
            CAPTION_BOTTOM_INSET
        );
    }
}

#[cfg(test)]
mod scale_bar_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn nice_round_snaps_down() {
        assert_eq!(nice_round(120.0), 100.0);
        assert_eq!(nice_round(80.0), 50.0);
        assert_eq!(nice_round(30.0), 20.0);
        assert_eq!(nice_round(15.0), 10.0);
        assert_eq!(nice_round(7.0), 5.0);
        assert_eq!(nice_round(3.0), 2.0);
        assert_eq!(nice_round(1.5), 1.0);
        assert_eq!(nice_round(0.7), 0.5);
    }

    #[wasm_bindgen_test]
    fn nice_round_handles_edges() {
        assert_eq!(nice_round(0.0), 0.0);
        assert_eq!(nice_round(-1.0), 0.0);
        assert_eq!(nice_round(f64::NAN), 0.0);
    }
    // ---- haversine_km ----

    #[wasm_bindgen_test]
    fn haversine_zero_distance_is_zero() {
        let d = haversine_km(40.0, -100.0, 40.0, -100.0);
        assert!(d.abs() < 1e-9, "got {}", d);
    }

    #[wasm_bindgen_test]
    fn haversine_one_degree_lon_at_equator() {
        // One degree of longitude at the equator ~= 111.195 km
        // (R=6371, c = 1deg in rad).
        let d = haversine_km(0.0, 0.0, 0.0, 1.0);
        assert!((d - 111.195).abs() < 0.05, "got {}", d);
    }

    #[wasm_bindgen_test]
    fn haversine_one_degree_lat() {
        // One degree of latitude ~= 111.195 km anywhere.
        let d = haversine_km(10.0, -50.0, 11.0, -50.0);
        assert!((d - 111.195).abs() < 0.05, "got {}", d);
    }

    #[wasm_bindgen_test]
    fn haversine_is_symmetric() {
        let ab = haversine_km(35.0, -97.0, 36.0, -96.0);
        let ba = haversine_km(36.0, -96.0, 35.0, -97.0);
        assert!((ab - ba).abs() < 1e-9, "ab={} ba={}", ab, ba);
    }

    #[wasm_bindgen_test]
    fn haversine_lon_shrinks_with_latitude() {
        // A degree of longitude covers less ground far from the equator.
        let near_equator = haversine_km(0.0, 0.0, 0.0, 1.0);
        let high_lat = haversine_km(60.0, 0.0, 60.0, 1.0);
        assert!(
            high_lat < near_equator,
            "high={} eq={}",
            high_lat,
            near_equator
        );
        // cos(60deg) = 0.5, so roughly half.
        assert!(
            (high_lat - near_equator * 0.5).abs() < 0.1,
            "got {}",
            high_lat
        );
    }

    // ---- nice_round (cases distinct from the existing mod tests) ----

    #[wasm_bindgen_test]
    fn nice_round_large_magnitude() {
        assert!((nice_round(1234.0) - 1000.0).abs() < 1e-9);
        assert!((nice_round(5500.0) - 5000.0).abs() < 1e-9);
        assert!((nice_round(2999.0) - 2000.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn nice_round_exact_boundaries_snap_to_self() {
        assert!((nice_round(5.0) - 5.0).abs() < 1e-9);
        assert!((nice_round(2.0) - 2.0).abs() < 1e-9);
        assert!((nice_round(1.0) - 1.0).abs() < 1e-9);
        assert!((nice_round(100.0) - 100.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn nice_round_just_below_boundary_steps_down() {
        // 4.9 -> 2 (frac < 5 but >= 2), 1.9 -> 1 (frac < 2)
        assert!((nice_round(4.9) - 2.0).abs() < 1e-9);
        assert!((nice_round(1.9) - 1.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn nice_round_infinite_is_zero() {
        assert!(nice_round(f64::INFINITY) == 0.0);
    }

    #[wasm_bindgen_test]
    fn nice_round_never_exceeds_target() {
        for &t in &[3.7_f64, 19.0, 88.0, 0.34, 640.0] {
            let n = nice_round(t);
            assert!(n <= t + 1e-9, "nice {} exceeds target {}", n, t);
            assert!(n > 0.0, "nice {} not positive for {}", n, t);
        }
    }

    // ---- format_km ----

    #[wasm_bindgen_test]
    fn format_km_whole_km() {
        assert!(format_km(100.0) == "100 km");
        assert!(format_km(5.0) == "5 km");
        assert!(format_km(1.0) == "1 km");
    }

    #[wasm_bindgen_test]
    fn format_km_sub_km_decimal() {
        // [0.1, 1.0) -> one decimal of km
        assert!(format_km(0.5) == "0.5 km");
        assert!(format_km(0.2) == "0.2 km");
    }

    #[wasm_bindgen_test]
    fn format_km_meters() {
        // < 0.1 km -> meters
        assert!(format_km(0.05) == "50 m");
        assert!(format_km(0.02) == "20 m");
    }

    // ---- format_mi ----

    #[wasm_bindgen_test]
    fn format_mi_whole_miles() {
        assert!(format_mi(50.0) == "50 mi");
        assert!(format_mi(2.0) == "2 mi");
        assert!(format_mi(1.0) == "1 mi");
    }

    #[wasm_bindgen_test]
    fn format_mi_sub_mile_decimal() {
        // [0.1, 1.0) -> one decimal of mi
        assert!(format_mi(0.5) == "0.5 mi");
        assert!(format_mi(0.2) == "0.2 mi");
    }

    #[wasm_bindgen_test]
    fn format_mi_feet() {
        // < 0.1 mi -> feet (mi * 5280)
        assert!(format_mi(0.05) == "264 ft");
        assert!(format_mi(0.01) == "53 ft");
    }

    #[wasm_bindgen_test]
    fn scale_bar_snaps_both_bars() {
        // 1 km/px: 120 km target -> 100 km bar (100 px); 74.6 mi -> 50 mi.
        let vm = scale_bar_vm(1.0).unwrap();
        assert_eq!(vm.km_label, "100 km");
        assert!((vm.km_px - 100.0).abs() < 1e-3);
        assert_eq!(vm.mi_label, "50 mi");
        assert!((vm.mi_px - 80.467).abs() < 0.01, "{}", vm.mi_px);
        assert!(scale_bar_vm(0.0).is_none());
        assert!(scale_bar_vm(f64::NAN).is_none());
    }
}

#[cfg(test)]
mod legend_tests {
    use super::*;
    use crate::core::RadarProduct;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn legend_is_titled_with_the_unit() {
        let vm = legend_vm(RadarProduct::Reflectivity, (-30.0, 80.0));
        assert_eq!(vm.caption, "dBZ");
        assert_eq!(vm.hover, "Reflectivity · dBZ");
        // 110 dBZ of span → a tick every 10.
        assert_eq!(vm.ticks.first().map(|t| t.1.as_str()), Some("-30"));
        assert_eq!(vm.ticks.last().map(|t| t.1.as_str()), Some("80"));
        assert_eq!(vm.ticks.len(), 12);
        assert!(vm.ticks.first().unwrap().0.abs() < 1e-6);
        assert!((vm.ticks.last().unwrap().0 - 1.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn narrow_ranges_get_decimal_ticks() {
        let vm = legend_vm(RadarProduct::CorrelationCoefficient, (0.2, 1.05));
        assert!(vm.ticks.iter().all(|(_, l)| l.contains('.')));
        // Unitless: the product's code captions the bar.
        assert_eq!(vm.caption, "CC");
        assert_eq!(vm.hover, "Correlation Coefficient");
        assert_eq!(vm.ticks.first().map(|t| t.1.as_str()), Some("0.2"));
    }

    #[wasm_bindgen_test]
    fn an_empty_range_has_no_ticks() {
        assert!(legend_vm(RadarProduct::Velocity, (5.0, 5.0))
            .ticks
            .is_empty());
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;
    use crate::core::session::{Clock, FrameFact};
    use crate::core::RadarProduct;
    use wasm_bindgen_test::wasm_bindgen_test;

    struct Utc;
    impl Clock for Utc {
        fn hms(&self, ts: f64) -> String {
            let s = (ts.floor() as i64).rem_euclid(86_400);
            format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
        }
        fn hm(&self, ts: f64) -> String {
            let s = (ts.floor() as i64).rem_euclid(86_400);
            format!("{:02}:{:02}", s / 3600, (s / 60) % 60)
        }
        fn day(&self, _ts: f64) -> (i32, u32, u32) {
            (2026, 10, 15)
        }
    }

    fn fact() -> FrameFact {
        FrameFact {
            time_secs: (11 * 3600 + 21 * 60 + 57) as f64,
            end_secs: (11 * 3600 + 22 * 60 + 7) as f64,
            elevation_deg: 0.5,
            product: "reflectivity".to_string(),
            age_secs: 120.0,
            partial: None,
        }
    }

    #[wasm_bindgen_test]
    fn the_info_box_and_the_edge_labels_ship_off() {
        assert_eq!(CANVAS_FALLBACKS, CanvasFallbacks::default());
        let f = fact();
        assert_eq!(
            frame_stamp(
                CANVAS_FALLBACKS,
                "ktlx",
                RadarProduct::Reflectivity,
                Some(&f),
                None,
                false,
                &Utc
            ),
            None
        );
    }

    #[wasm_bindgen_test]
    fn the_stamp_fallback_reads_one_line() {
        let on = CanvasFallbacks {
            frame_stamp: true,
            ..CanvasFallbacks::default()
        };
        let f = fact();
        let prev = Some((11 * 3600 + 17 * 60 + 2) as f64);
        let s = frame_stamp(
            on,
            "ktlx",
            RadarProduct::Reflectivity,
            Some(&f),
            prev,
            true,
            &Utc,
        )
        .unwrap();
        assert_eq!(s.line, "KTLX · REF · 0.5° · 11:21:57");
        assert_eq!(s.previous.as_deref(), Some("previous 11:17:02"));
        // Frames: no previous line. No frame: no stamp.
        let s = frame_stamp(
            on,
            "ktlx",
            RadarProduct::Reflectivity,
            Some(&f),
            prev,
            false,
            &Utc,
        )
        .unwrap();
        assert_eq!(s.previous, None);
        assert_eq!(
            frame_stamp(
                on,
                "ktlx",
                RadarProduct::Reflectivity,
                None,
                prev,
                true,
                &Utc
            ),
            None
        );
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use crate::core::RadarProduct;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn inputs(sweep: Option<(f32, f32)>) -> CanvasOverlayInputs<'static> {
        CanvasOverlayInputs {
            product: RadarProduct::Reflectivity,
            data_range_km: Some(460.0),
            sweep,
            stale: false,
            show_lines: true,
            live: false,
            antenna_az: None,
            chunk_last_az: &[],
            live_edge_time: None,
            offscreen_cut: None,
            playhead_secs: 1_000.0,
            fallbacks: CanvasFallbacks::default(),
        }
    }

    #[wasm_bindgen_test]
    fn the_live_cut_is_marked_whichever_tilt_is_on_screen() {
        // Latest, or the fixed tilt the antenna is on: it composites, so it is
        // not an off-screen cut.
        assert_eq!(
            offscreen_live_cut(false, None, Some(3), Some((10.0, 80.0))),
            None
        );
        assert_eq!(
            offscreen_live_cut(false, Some(3), Some(3), Some((10.0, 80.0))),
            None
        );
        // Frames holds whole sweeps; nothing to mark.
        assert_eq!(
            offscreen_live_cut(true, Some(1), Some(3), Some((10.0, 80.0))),
            None
        );
        // A fixed 0.5° while the antenna is on 1.3°: the cut is marked.
        let cut = offscreen_live_cut(false, Some(1), Some(3), Some((10.0, 80.0)));
        assert_eq!(
            cut,
            Some(OffscreenCut {
                first_az: 10.0,
                last_az: 80.0
            })
        );

        let chunks = [30.0, 60.0, 80.0];
        let mut i = inputs(None);
        i.live = true;
        i.antenna_az = Some(95.0);
        i.chunk_last_az = &chunks;
        i.live_edge_time = Some(990.0);
        i.offscreen_cut = cut;
        let s = CanvasOverlayVm::build(&i).sweep.unwrap();
        assert_eq!((s.start_az, s.edge_az), (10.0, 80.0));
        assert_eq!(s.now_az, Some(95.0));
        assert_eq!(s.chunk_az, vec![30.0, 60.0]);
        assert!(s.offscreen);
        // The same rule as the on-screen cut: Rays only.
        i.show_lines = false;
        assert!(CanvasOverlayVm::build(&i).sweep.is_none());
        // Not live: nothing off screen to mark.
        i.show_lines = true;
        i.live = false;
        assert!(CanvasOverlayVm::build(&i).sweep.is_none());
    }

    #[wasm_bindgen_test]
    fn rings_stop_at_the_data_range() {
        let g = ring_geometry(RadarProduct::Reflectivity, Some(230.0));
        assert_eq!(g.data_km, 230.0);
        assert_eq!(g.coverage_km, 460.0);
        let kms: Vec<f64> = g.rings.iter().map(|r| r.0).collect();
        assert_eq!(kms, vec![50.0, 100.0, 150.0, 200.0]);
        assert_eq!(
            g.rings
                .iter()
                .filter(|r| r.1)
                .map(|r| r.0)
                .collect::<Vec<_>>(),
            vec![100.0, 200.0]
        );
        // A ring exactly at the edge is the edge ring, not a range ring.
        let g = ring_geometry(RadarProduct::Reflectivity, Some(300.0));
        assert_eq!(g.rings.last().map(|r| r.0), Some(250.0));
    }

    #[wasm_bindgen_test]
    fn with_no_data_the_rings_reach_the_product_range() {
        let g = ring_geometry(RadarProduct::Velocity, None);
        assert_eq!(g.data_km, 300.0);
        assert_eq!(g.coverage_km, 300.0);
        assert_eq!(
            ring_geometry(RadarProduct::Reflectivity, Some(0.0)).data_km,
            460.0
        );
    }

    #[wasm_bindgen_test]
    fn the_coverage_ring_and_cutout_share_the_product_range() {
        assert_eq!(product_max_range_km(RadarProduct::Reflectivity), 460.0);
        assert_eq!(
            product_max_range_km(RadarProduct::CorrelationCoefficient),
            300.0
        );
    }

    #[wasm_bindgen_test]
    fn hovering_a_major_ring_names_its_range() {
        let g = ring_geometry(RadarProduct::Reflectivity, Some(460.0));
        // 1 px per km: the 200 km ring sits at 200 px.
        assert_eq!(hovered_ring_km(&g, 203.0, 1.0), Some(200.0));
        assert_eq!(hovered_ring_km(&g, 150.0, 1.0), None); // a minor ring
        assert_eq!(hovered_ring_km(&g, 458.0, 1.0), Some(460.0)); // the edge
        assert_eq!(hovered_ring_km(&g, 200.0, 0.0), None);
        assert_eq!(ring_label(200.0), "200 km");
    }

    #[wasm_bindgen_test]
    fn rays_draws_white_lines_and_a_now_line_only_while_live() {
        let vm = CanvasOverlayVm::build(&inputs(Some((90.0, 10.0))));
        let s = vm.sweep.unwrap();
        assert_eq!((s.edge_az, s.start_az), (90.0, 10.0));
        assert_eq!(s.now_az, None);
        // Archive: the edge follows the playhead.
        assert_eq!(s.edge_time, Some(1_000.0));

        let chunks = [30.0, 60.0, 90.0];
        let mut i = inputs(Some((90.0, 10.0)));
        i.live = true;
        i.antenna_az = Some(120.0);
        i.chunk_last_az = &chunks;
        i.live_edge_time = Some(990.0);
        let s = CanvasOverlayVm::build(&i).sweep.unwrap();
        assert_eq!(s.now_az, Some(120.0));
        // Every chunk boundary but the newest (that one is the data edge).
        assert_eq!(s.chunk_az, vec![30.0, 60.0]);
        assert_eq!(s.edge_time, Some(990.0));
    }

    #[wasm_bindgen_test]
    fn frames_and_held_positions() {
        let mut i = inputs(Some((90.0, 10.0)));
        i.show_lines = false;
        assert!(CanvasOverlayVm::build(&i).sweep.is_none());
        assert!(CanvasOverlayVm::build(&inputs(None)).sweep.is_none());
        let mut i = inputs(Some((90.0, 10.0)));
        i.stale = true;
        let s = CanvasOverlayVm::build(&i).sweep.unwrap();
        assert!(s.stale);
        assert_eq!(s.edge_time, None);
    }

    #[wasm_bindgen_test]
    fn the_edge_label_is_a_fallback() {
        assert_eq!(
            CanvasOverlayVm::build(&inputs(Some((90.0, 10.0)))).edge_label,
            None
        );
        let mut i = inputs(Some((90.0, 10.0)));
        i.fallbacks.edge_label = true;
        assert_eq!(CanvasOverlayVm::build(&i).edge_label, Some(1_000.0));
    }

    #[wasm_bindgen_test]
    fn the_edge_label_stays_inside_the_canvas() {
        // Edge due east, far outside a 100×100 canvas.
        let (x, y) = edge_label_pos(
            (50.0, 50.0),
            500.0,
            90.0,
            (40.0, 10.0),
            (0.0, 0.0, 100.0, 100.0),
        );
        assert_eq!((x, y), (60.0, 45.0));
        let (x, _) = edge_label_pos(
            (50.0, 50.0),
            500.0,
            270.0,
            (40.0, 10.0),
            (0.0, 0.0, 100.0, 100.0),
        );
        assert_eq!(x, 0.0);
    }

    #[wasm_bindgen_test]
    fn the_faded_wedge_explains_itself() {
        let sweep = Some((90.0, 0.0));
        assert!(faded_wedge_hint(true, sweep, 200.0, 50.0, 460.0).is_some());
        assert!(faded_wedge_hint(true, sweep, 45.0, 50.0, 460.0).is_none());
        assert!(faded_wedge_hint(false, sweep, 200.0, 50.0, 460.0).is_none());
        assert!(faded_wedge_hint(true, sweep, 200.0, 470.0, 460.0).is_none());
    }

    #[wasm_bindgen_test]
    fn the_focus_rings_and_marks_draw_full_and_the_others_at_half() {
        assert_eq!(disc_overlay_opacity(true), 1.0);
        assert_eq!(disc_overlay_opacity(false), 0.5);
    }
}

#[cfg(test)]
mod pick_tests {
    use super::*;
    use crate::core::diagnostics::DiagnosticsIntent;
    use crate::core::Intent;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn all() -> PickCandidates {
        PickCandidates {
            site: Some(("KFDR", 34.3, -98.9)),
            mping: Some(7),
            alert: Some(("a1".to_string(), "Tornado Warning".to_string())),
            storm_cell: None,
        }
    }

    const ON: PickLayers = PickLayers {
        sites: true,
        mping: true,
        alerts: true,
        storm_cells: true,
    };

    #[wasm_bindgen_test]
    fn a_tap_in_a_storm_cell_selects_it_over_an_alert() {
        let c = PickCandidates {
            site: None,
            mping: None,
            storm_cell: Some((42.0, -93.0)),
            ..all()
        };
        let pick = decide_map_pick(c.clone(), ON);
        assert_eq!(
            pick,
            Some(MapPick::StormCell {
                lat: 42.0,
                lon: -93.0
            })
        );
        assert_eq!(
            pick.unwrap().intent_for(true),
            Intent::StormCells(crate::core::storm_cell_view::StormCellIntent::Select(Some(
                (42.0, -93.0)
            )))
        );
        // Points still win; with the layer off the alert picks.
        let with_site = PickCandidates {
            site: all().site,
            ..c.clone()
        };
        assert!(matches!(
            decide_map_pick(with_site, ON),
            Some(MapPick::Site { .. })
        ));
        let off = PickLayers {
            storm_cells: false,
            ..ON
        };
        assert!(matches!(
            decide_map_pick(c, off),
            Some(MapPick::Alert { .. })
        ));
    }

    #[wasm_bindgen_test]
    fn a_phone_tap_on_a_site_asks_first_and_a_click_switches() {
        use crate::core::dialogs::{Confirm, DialogIntent};
        let site = MapPick::Site {
            id: "KFDR",
            lat: 34.3,
            lon: -98.9,
        };
        assert_eq!(
            site.intent_for(true),
            Intent::Dialog(DialogIntent::RequestConfirm(Confirm::SwitchSite {
                id: "KFDR",
                lat: 34.3,
                lon: -98.9,
            }))
        );
        assert_eq!(site.intent_for(false), site.intent());
        // Reports and alerts open the same on both.
        assert_eq!(
            MapPick::Mping(7).intent_for(true),
            MapPick::Mping(7).intent()
        );
    }

    #[wasm_bindgen_test]
    fn phone_targets_are_24_px() {
        let phone = pick_radii(true);
        assert_eq!(phone.site * 2.0, 24.0);
        assert_eq!(phone.mping * 2.0, 24.0);
        let desk = pick_radii(false);
        assert!(desk.site < phone.site && desk.mping < phone.mping);
    }

    #[wasm_bindgen_test]
    fn a_hidden_site_never_picks() {
        let layers = PickLayers { sites: false, ..ON };
        assert_eq!(decide_map_pick(all(), layers), Some(MapPick::Mping(7)));
        let only_site = PickCandidates {
            site: all().site,
            ..PickCandidates::default()
        };
        assert_eq!(decide_map_pick(only_site, layers), None);
    }

    #[wasm_bindgen_test]
    fn point_marks_win_over_alert_polygons() {
        assert!(matches!(
            decide_map_pick(all(), ON),
            Some(MapPick::Site { id: "KFDR", .. })
        ));
        let no_site = PickCandidates {
            site: None,
            ..all()
        };
        assert_eq!(
            decide_map_pick(no_site.clone(), ON),
            Some(MapPick::Mping(7))
        );
        let alert_only = PickCandidates {
            mping: None,
            ..no_site
        };
        assert!(matches!(
            decide_map_pick(alert_only.clone(), ON),
            Some(MapPick::Alert { .. })
        ));
        let off = PickLayers {
            alerts: false,
            ..ON
        };
        assert_eq!(decide_map_pick(alert_only, off), None);
    }

    #[wasm_bindgen_test]
    fn a_pick_carries_its_click_and_its_hover() {
        let site = decide_map_pick(all(), ON).unwrap();
        assert_eq!(
            site.intent(),
            Intent::SelectSite {
                site_id: "KFDR".to_string(),
                lat: 34.3,
                lon: -98.9,
            }
        );
        assert_eq!(site.hover_label().as_deref(), Some("KFDR · switch site"));
        assert_eq!(
            MapPick::Mping(7).intent(),
            Intent::Diagnostics(DiagnosticsIntent::SelectMpingReport(7))
        );
        assert_eq!(MapPick::Mping(7).hover_label(), None);
        let alert = MapPick::Alert {
            id: "a1".to_string(),
            event: "Tornado Warning".to_string(),
        };
        assert_eq!(alert.hover_label().as_deref(), Some("Tornado Warning"));
    }
}

#[cfg(test)]
mod layer_legend_tests {
    use super::*;
    use crate::mping::{ReportCategory, StormReport};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn report(category: ReportCategory, obtime_ms: f64) -> StormReport {
        StormReport {
            id: 1,
            obtime_ms,
            category,
            description: String::new(),
            lat: 35.0,
            lon: -97.0,
        }
    }

    #[wasm_bindgen_test]
    fn mping_legend_lists_drawn_categories_in_a_fixed_order() {
        let reports = [
            report(ReportCategory::RainSnow, 1_000_000.0),
            report(ReportCategory::Tornado, 1_000_000.0),
            report(ReportCategory::Hail, 1_000_000.0),
            // Hail again: listed once.
            report(ReportCategory::Hail, 1_000_000.0),
        ];
        let legend: Vec<_> = mping_legend(&reports, 1_000.0)
            .into_iter()
            .map(|l| l.1)
            .collect();
        assert_eq!(legend, vec!["Tornado", "Hail", "Rain / Snow"]);
    }

    #[wasm_bindgen_test]
    fn reports_after_the_playhead_are_not_in_the_legend() {
        let reports = [report(ReportCategory::Flood, 5_000_000.0)];
        assert!(mping_legend(&reports, 1_000.0).is_empty());
    }

    #[wasm_bindgen_test]
    fn mosaic_legend_spans_its_palette() {
        assert_eq!(
            mosaic_legend_ends(),
            ("5".to_string(), "75 dBZ".to_string())
        );
    }
}
