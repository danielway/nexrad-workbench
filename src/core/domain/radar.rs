//! Radar data structures for timeline representation.

use crate::core::tilt::TiltFilter;
use crate::data::{ScanCompleteness, ScanKey};
use serde::{Deserialize, Serialize};

/// A contiguous time range of radar data.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimeRange {
    /// Start timestamp (Unix seconds)
    pub start: f64,
    /// End timestamp (Unix seconds)
    pub end: f64,
}

impl TimeRange {
    /// Creates a new time range.
    pub(crate) fn new(start: f64, end: f64) -> Self {
        Self { start, end }
    }

    /// Returns the duration of this range in seconds.
    #[cfg(test)]
    pub(crate) fn duration(&self) -> f64 {
        self.end - self.start
    }

    /// Returns true if the given timestamp is within this range.
    pub(crate) fn contains(&self, ts: f64) -> bool {
        ts >= self.start && ts <= self.end
    }
}

/// A scan's time boundaries derived from adjacent file timestamps in a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScanBoundary {
    /// Start of this scan (Unix seconds).
    pub start: i64,
    /// End of this scan (next scan's start, or estimated for last scan; Unix seconds).
    pub end: i64,
}

/// A single radial (one azimuth direction at one elevation)
#[derive(Clone, Debug)]
pub(crate) struct Radial {
    /// Start timestamp (Unix seconds with sub-second precision)
    pub start_time: f64,
    /// Azimuth angle in degrees
    pub azimuth: f32,
}

/// A sweep (360-degree rotation at one elevation)
#[derive(Clone, Debug)]
pub(crate) struct Sweep {
    /// Start timestamp (Unix seconds with sub-second precision)
    pub start_time: f64,
    /// End timestamp
    pub end_time: f64,
    /// Elevation angle in degrees
    pub elevation: f32,
    /// Elevation number (index into the VCP elevation list)
    pub elevation_number: u8,
    /// Azimuth angle (degrees) of the chronologically first radial in this sweep.
    pub start_azimuth: f32,
    /// Individual radials in this sweep
    pub radials: Vec<Radial>,
    /// Product names (matching `SweepDataKey` product strings) that have a
    /// pre-computed sweep blob stored. Empty means "unknown" — typical for
    /// legacy index entries or placeholder sweeps — and callers should skip
    /// product-availability checks in that case.
    pub cached_products: Vec<String>,
}

impl Sweep {
    #[cfg(test)]
    pub(crate) fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }
}

/// Whether `sweep` has a cached blob for `product` (worker-string name).
/// Empty `cached_products` means "nothing stored / unknown" and is treated as
/// not-available — matching the render resolver
/// ([`crate::core::playback_manager::resolve_active_sweep_target`]) so the
/// macro frame list never lists a frame the canvas would blank.
/// The volume a live stream is collecting, as the timeline draws it: its
/// storage-key time and its projection (collected cuts at their real times,
/// the rest projected from the VCP).
#[derive(Clone, Copy)]
pub(crate) struct LiveVolume<'a> {
    pub key_secs: f64,
    pub projection: &'a crate::core::projection::ScanProjection,
}

/// `(elevation_number, end offset from the volume start)` for each cut in a
/// VCP plan that is a frame at `tilt` (every cut for Latest; each visit of
/// a held angle), estimated from the plan's azimuth rates.
fn vcp_cut_ends(
    vcp: &crate::data::ExtractedVcp,
    tilt: TiltFilter,
) -> impl Iterator<Item = (u8, f64)> {
    let durations = vcp
        .estimated_volume_duration()
        .map(|total| vcp.sweep_durations(total))
        .unwrap_or_default();
    let cuts = tilt.cuts(Some(vcp));
    let mut elapsed = 0.0;
    durations.into_iter().enumerate().filter_map(move |(i, d)| {
        elapsed += d;
        let elevation_number = (i + 1) as u8;
        cuts.contains(elevation_number)
            .then_some((elevation_number, elapsed))
    })
}

fn sweep_has_product(sweep: &Sweep, product: &str) -> bool {
    sweep.cached_products.iter().any(|p| p == product)
}

/// A complete volume scan (multiple sweeps at different elevations)
#[derive(Clone, Debug)]
pub(crate) struct Scan {
    /// Start timestamp (Unix seconds with sub-second precision).
    /// May be adjusted earlier than `key_timestamp` to encompass sweep data.
    pub start_time: f64,
    /// End timestamp
    pub end_time: f64,
    /// The nominal scan key timestamp (Unix seconds) before sweep adjustments.
    /// Matches the timestamp encoded in the scan storage key.
    pub key_timestamp: f64,
    /// Volume Coverage Pattern number (e.g., 215, 35, 212)
    pub vcp: u16,
    /// Full extracted VCP pattern with per-elevation metadata.
    pub vcp_pattern: Option<crate::data::ExtractedVcp>,
    /// Sweeps in this scan, ordered by elevation
    pub sweeps: Vec<Sweep>,
    /// Completeness state for this scan (from cache metadata).
    pub completeness: Option<ScanCompleteness>,
    /// Number of records present (from cache metadata).
    pub cached_sweep_count: Option<u32>,
    /// Expected number of records (from cache metadata).
    pub planned_sweep_count: Option<u32>,
}

impl Scan {
    /// The cache holds only some of this volume's sweeps (a fetch scoped
    /// to one tilt, or a live volume joined part way). Unknown counts as
    /// not partial.
    pub(crate) fn is_partial(&self) -> bool {
        matches!(
            self.completeness,
            Some(ScanCompleteness::PartialNoVcp | ScanCompleteness::PartialWithVcp)
        )
    }

    #[cfg(test)]
    pub(crate) fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }

    /// The scan's storage-key timestamp in milliseconds — the same value
    /// encoded in its `ScanKey`. Use this (not ad-hoc `* 1000.0` math) when
    /// matching against millisecond keys from IDB or the live stream.
    pub(crate) fn key_ms(&self) -> i64 {
        crate::data::UnixMillis::from_secs_f64(self.key_timestamp).0
    }

    /// Full VCP-projected volume end for timeline block rendering.
    ///
    /// A scan's stored sweeps can be sparse — a live stream that ended
    /// mid-volume, or a partial archive download — but the VCP plan defines
    /// the whole volume's duration. Returns the later of the real data end
    /// (`end_time`) and the VCP-estimated end so the timeline block reflects
    /// the entire scan instead of shrinking to the last downloaded sweep.
    ///
    /// Display-only: does NOT affect `end_time`, which stays anchored to real
    /// data for playback progress, scan-at-timestamp, and contiguous-range
    /// logic.
    pub(crate) fn display_end_time(&self) -> f64 {
        self.vcp_pattern
            .as_ref()
            .and_then(|v| v.estimated_volume_duration())
            .map(|dur| self.end_time.max(self.start_time + dur))
            .unwrap_or(self.end_time)
    }

    /// VCP-defined target ("commanded") angle for the given elevation number,
    /// looking through the extracted pattern first then the static VCP table.
    /// Returns `None` only when neither source has an entry — callers should
    /// fall back to `Sweep::elevation` (the per-sweep measured average from
    /// the antenna encoder) for display.
    pub(crate) fn target_elevation_angle(&self, elevation_number: u8) -> Option<f32> {
        let idx = elevation_number.saturating_sub(1) as usize;
        if let Some(ref pattern) = self.vcp_pattern {
            if let Some(elev) = pattern.elevations.get(idx) {
                return Some(elev.angle);
            }
        }
        if let Some(def) = crate::data::vcp::get_vcp_definition(self.vcp) {
            if let Some(elev) = def.elevations.get(idx) {
                return Some(elev.angle);
            }
        }
        None
    }

    /// Display angle for a sweep — VCP target if available, else the
    /// measured average. Use this everywhere we render an elevation cut to
    /// the user; it keeps every surface anchored to the commanded angle
    /// (the cut's identity) instead of the encoder's noisy reading.
    pub(crate) fn display_angle(&self, sweep: &Sweep) -> f32 {
        self.target_elevation_angle(sweep.elevation_number)
            .unwrap_or(sweep.elevation)
    }

    /// Find the sweep containing the given timestamp
    pub(crate) fn find_sweep_at_timestamp(&self, ts: f64) -> Option<(usize, &Sweep)> {
        self.sweeps
            .iter()
            .enumerate()
            .find(|(_, sweep)| ts >= sweep.start_time && ts <= sweep.end_time)
    }
}

/// Lightweight metadata for timeline display (avoids loading full scan data).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScanMetadata {
    /// Storage key identifying this scan
    pub key: ScanKey,
    /// Original file name from AWS
    pub file_name: String,
    /// File size in bytes
    pub file_size: u64,
    /// End timestamp of the scan in seconds (populated when scan is decoded)
    pub end_timestamp: Option<i64>,
    /// Full Volume Coverage Pattern extracted from scan data.
    pub vcp: Option<crate::data::ExtractedVcp>,
    /// Completeness state for this scan.
    pub completeness: Option<crate::data::ScanCompleteness>,
    /// Number of sweeps actually cached for this scan.
    pub cached_sweep_count: Option<u32>,
    /// Number of sweeps the VCP plans (`vcp.elevations.len()`).
    pub planned_sweep_count: Option<u32>,
    /// Cached sweep metadata, if any.
    pub sweeps: Option<Vec<crate::data::CachedSweep>>,
}

/// Collection of radar data for timeline display
#[derive(Clone, Debug, Default)]
pub(crate) struct RadarTimeline {
    /// All scans, ordered by start time
    pub scans: Vec<Scan>,
}

/// Maximum gap (in seconds) between consecutive scans to consider them part of
/// the same contiguous time range. Gaps larger than this create a new range.
/// Default: 15 minutes (scans are typically 5 minutes apart)
const MAX_CONTIGUOUS_GAP_SECS: f64 = 15.0 * 60.0;

impl RadarTimeline {
    /// Get contiguous time ranges covered by this data.
    ///
    /// Returns multiple ranges when there are large gaps between scans
    /// (e.g., data from different days or sessions). Consecutive scans
    /// within ~15 minutes of each other are grouped into the same range.
    pub(crate) fn time_ranges(&self) -> Vec<TimeRange> {
        if self.scans.is_empty() {
            return Vec::new();
        }

        let mut ranges = Vec::new();
        let mut range_start = self.scans[0].start_time;
        let mut range_end = self.scans[0].end_time;

        for scan in self.scans.iter().skip(1) {
            let gap = scan.start_time - range_end;

            if gap > MAX_CONTIGUOUS_GAP_SECS {
                // Gap too large - save current range and start a new one
                ranges.push(TimeRange::new(range_start, range_end));
                range_start = scan.start_time;
            }

            range_end = scan.end_time;
        }

        // Don't forget the last range
        ranges.push(TimeRange::new(range_start, range_end));

        ranges
    }

    /// Find the scan containing the given timestamp.
    ///
    /// Uses binary search on the sorted scan list for O(log n) lookup.
    pub(crate) fn find_scan_at_timestamp(&self, ts: f64) -> Option<&Scan> {
        // partition_point returns the first index where start_time > ts,
        // so the candidate scan is the one just before that.
        let idx = self.scans.partition_point(|s| s.start_time <= ts);
        let scan = self.scans.get(idx.wrapping_sub(1))?;
        (ts <= scan.end_time).then_some(scan)
    }

    /// Find the most recent scan at or before the given timestamp, within a time window.
    ///
    /// Returns the scan whose start_time is closest to (but not after) the timestamp,
    /// as long as it's within `max_age_secs` of the timestamp.
    /// Uses binary search on the sorted scan list for O(log n) lookup.
    pub(crate) fn find_recent_scan(&self, ts: f64, max_age_secs: f64) -> Option<&Scan> {
        let idx = self.scans.partition_point(|s| s.start_time <= ts);
        let most_recent = self.scans.get(idx.wrapping_sub(1))?;
        (ts - most_recent.start_time <= max_age_secs).then_some(most_recent)
    }

    /// Find the scan immediately before the one containing `ts`, within a time window.
    ///
    /// Returns `None` if `ts` is before or within the first scan, or if the
    /// previous scan is older than `max_age_secs` from `ts`.
    pub(crate) fn find_previous_scan(&self, ts: f64, max_age_secs: f64) -> Option<&Scan> {
        let idx = self.scans.partition_point(|s| s.start_time <= ts);
        // idx-1 is the scan containing ts; idx-2 is the one before it
        if idx >= 2 {
            let scan = self.scans.get(idx - 2)?;
            (ts - scan.start_time <= max_age_secs).then_some(scan)
        } else {
            None
        }
    }

    /// Collect all sweep end-times at the held tilt (every visit of its
    /// angle, see [`crate::core::tilt`]) AND holding a cached blob for
    /// `product` (the worker-string product name).
    ///
    /// A frame (spec §4) is a sweep matching the *selected product + tilt* —
    /// something the canvas can render. Filtering by product here keeps macro
    /// stepping/looping from dwelling on sweeps the resolver would blank.
    /// `cached_products` follows the resolver's convention: a sweep counts
    /// only when it lists `product`; an empty list ("nothing stored / unknown")
    /// is excluded.
    ///
    /// Returns a sorted, deduplicated `Vec<f64>` of `end_time` values. If
    /// `bounds` is provided, only sweeps within that time range are included.
    pub(crate) fn matching_sweep_end_times(
        &self,
        tilt: TiltFilter,
        product: &str,
        bounds: Option<(f64, f64)>,
    ) -> Vec<f64> {
        let mut times: Vec<f64> = Vec::new();
        for scan in &self.scans {
            if let Some((start, end)) = bounds {
                if scan.end_time < start || scan.start_time > end {
                    continue;
                }
            }
            let cuts = tilt.cuts(scan.vcp_pattern.as_ref());
            for sweep in &scan.sweeps {
                if cuts.contains(sweep.elevation_number) && sweep_has_product(sweep, product) {
                    if let Some((start, end)) = bounds {
                        if sweep.end_time < start || sweep.end_time > end {
                            continue;
                        }
                    }
                    times.push(sweep.end_time);
                }
            }
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        times.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        times
    }

    /// Collect all sweep end-times (regardless of elevation) holding a cached
    /// blob for `product`. Used by Latest/auto mode where every matching sweep
    /// is a frame. See [`Self::matching_sweep_end_times`] for the
    /// product-filter contract.
    pub(crate) fn all_sweep_end_times(
        &self,
        product: &str,
        bounds: Option<(f64, f64)>,
    ) -> Vec<f64> {
        let mut times: Vec<f64> = Vec::new();
        for scan in &self.scans {
            if let Some((start, end)) = bounds {
                if scan.end_time < start || scan.start_time > end {
                    continue;
                }
            }
            for sweep in &scan.sweeps {
                if !sweep_has_product(sweep, product) {
                    continue;
                }
                if let Some((start, end)) = bounds {
                    if sweep.end_time < start || sweep.end_time > end {
                        continue;
                    }
                }
                times.push(sweep.end_time);
            }
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        times.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        times
    }

    /// Collect one frame time per scan — the scan's `end_time` — for scans
    /// holding at least one cached sweep of `product`. The volumetric (3D)
    /// playback frame source: the volume render is keyed per scan, so
    /// stepping per sweep would move the playhead/captions without changing
    /// the picture. `end_time` is the instant the whole volume exists, which
    /// is what the 3D composite renders. The elevation filter is irrelevant
    /// to the volumetric pass (it composites all cuts).
    ///
    /// Returns a sorted, deduplicated `Vec<f64>`, bounds-clipped like the
    /// per-sweep sources.
    pub(crate) fn scan_end_frame_times(
        &self,
        product: &str,
        bounds: Option<(f64, f64)>,
    ) -> Vec<f64> {
        let mut times: Vec<f64> = Vec::new();
        for scan in &self.scans {
            if let Some((start, end)) = bounds {
                if scan.end_time < start || scan.start_time > end {
                    continue;
                }
            }
            if !scan.sweeps.iter().any(|s| sweep_has_product(s, product)) {
                continue;
            }
            if let Some((start, end)) = bounds {
                if scan.end_time < start || scan.end_time > end {
                    continue;
                }
            }
            times.push(scan.end_time);
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        times.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        times
    }

    /// The `(start, end)` span covering the last `n` frames at or before
    /// `now`, for the given elevation selection: the window a "Last N frames
    /// → now" range covers. It counts every frame the timeline knows about,
    /// not just the downloaded ones (see [`Self::known_frame_ends`]), so the
    /// range's extent is settled before backfill fetches the volumes it needs.
    ///
    /// Returns `None` until `n` frames are known at/<= `now` (no listing and
    /// too little cached), and the caller falls back to the nominal span.
    pub(crate) fn lookback_window(
        &self,
        shadows: &[ScanBoundary],
        live: Option<LiveVolume<'_>>,
        elevation_selection: &crate::core::ElevationSelection,
        product: &str,
        now: f64,
        n: usize,
    ) -> Option<(f64, f64)> {
        let frames = self.known_frame_ends(shadows, live, elevation_selection.into(), product);
        // `frames` is sorted ascending; keep those at/<= now (small slack so a
        // just-completed frame whose end_time rounds a hair past `now` counts).
        let cutoff = now + 0.5;
        let usable: Vec<f64> = frames.into_iter().filter(|&t| t <= cutoff).collect();
        let n = n.max(1);
        if usable.len() < n {
            return None;
        }
        let start_idx = usable.len() - n;
        Some((usable[start_idx], *usable.last().unwrap()))
    }

    /// End times of every frame the timeline knows about, ascending: the
    /// cached frames (the same ones stepping uses), plus estimated frames for
    /// each listed volume that isn't cached yet.
    ///
    /// A listed volume's sweeps aren't known until it downloads, so its frames
    /// are placed by a template (see [`Self::frame_offsets_template`]). With
    /// no template, each listed volume counts as one frame at its end. `tilt`
    /// is the selected tilt: Latest counts every sweep as a frame, a held
    /// angle every visit of it.
    pub(crate) fn known_frame_ends(
        &self,
        shadows: &[ScanBoundary],
        live: Option<LiveVolume<'_>>,
        tilt: TiltFilter,
        product: &str,
    ) -> Vec<f64> {
        let is_frame = |scan: &Scan, sweep: &Sweep| {
            tilt.matches(scan.vcp_pattern.as_ref(), sweep.elevation_number)
                && sweep_has_product(sweep, product)
        };
        let mut times: Vec<f64> = self
            .scans
            .iter()
            .flat_map(|scan| scan.sweeps.iter().map(move |sw| (scan, sw)))
            .filter(|(scan, sw)| is_frame(scan, sw))
            .map(|(_, sw)| sw.end_time)
            .collect();

        let tolerance = crate::core::timeline_view::SCAN_JOIN_TOLERANCE_SECS as f64;
        let has_cut = |scan: Option<&Scan>, n: u8| {
            scan.is_some_and(|s| s.sweeps.iter().any(|sw| sw.elevation_number == n))
        };

        // The volume being streamed: every cut the timeline draws for it
        // counts, at the time it draws it, including cuts collected before
        // the stream joined (never received) and ones still to come (past
        // `now`, so they drop out of a lookback).
        if let Some(lv) = live {
            let cached = self
                .scans
                .iter()
                .find(|s| (s.key_timestamp - lv.key_secs).abs() <= tolerance);
            let cuts = tilt.cuts(lv.projection.vcp_pattern.as_ref());
            times.extend(
                lv.projection
                    .sweeps
                    .iter()
                    .filter(|sp| cuts.contains(sp.elevation_number))
                    .filter(|sp| !has_cut(cached, sp.elevation_number))
                    .map(|sp| sp.collection_end_secs),
            );
        }

        // Any other cached volume can be missing cuts that exist too (a
        // fixed-tilt download holds only that tilt). Its VCP plan still knows
        // them, so place each missing cut by the plan.
        for scan in &self.scans {
            let is_live =
                live.is_some_and(|lv| (scan.key_timestamp - lv.key_secs).abs() <= tolerance);
            if is_live || scan.completeness == Some(ScanCompleteness::Complete) {
                continue;
            }
            let Some(vcp) = scan.vcp_pattern.as_ref() else {
                continue;
            };
            times.extend(
                vcp_cut_ends(vcp, tilt)
                    .filter(|(n, _)| !has_cut(Some(scan), *n))
                    .map(|(_, off)| scan.key_timestamp + off),
            );
        }

        let offsets = self.frame_offsets_template(tilt, &is_frame);
        let cached = |start: f64| {
            self.scans
                .iter()
                .any(|s| (s.key_timestamp - start).abs() <= tolerance)
        };
        for b in shadows {
            let (start, end) = (b.start as f64, b.end as f64);
            if cached(start) {
                continue;
            }
            if offsets.is_empty() {
                times.push(end);
            } else {
                times.extend(offsets.iter().map(|off| start + off).filter(|&t| t <= end));
            }
        }

        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        times.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        times
    }

    /// Where a volume's frames end, in seconds from its start, for placing
    /// the frames of listed volumes that aren't downloaded. In order of
    /// preference: the frames of the newest complete cached volume; the VCP
    /// plan of the newest cached volume that has one (a volume joined
    /// mid-stream holds only its last few sweeps, but its plan has them all);
    /// the frames of the newest cached volume. Empty when nothing is cached.
    fn frame_offsets_template(
        &self,
        tilt: TiltFilter,
        is_frame: &dyn Fn(&Scan, &Sweep) -> bool,
    ) -> Vec<f64> {
        let any_frame = |scan: &&Scan| scan.sweeps.iter().any(|sw| is_frame(scan, sw));
        let cached_offsets = |scan: &Scan| -> Vec<f64> {
            scan.sweeps
                .iter()
                .filter(|sw| is_frame(scan, sw))
                .map(|sw| (sw.end_time - scan.key_timestamp).max(0.0))
                .collect()
        };
        let newest = self.scans.iter().rev();
        if let Some(scan) = newest
            .clone()
            .filter(|s| s.completeness == Some(ScanCompleteness::Complete))
            .find(any_frame)
        {
            return cached_offsets(scan);
        }
        if let Some(vcp) = newest.clone().find_map(|s| s.vcp_pattern.as_ref()) {
            let offsets: Vec<f64> = vcp_cut_ends(vcp, tilt).map(|(_, off)| off).collect();
            if !offsets.is_empty() {
                return offsets;
            }
        }
        newest
            .clone()
            .find(any_frame)
            .map(cached_offsets)
            .unwrap_or_default()
    }

    /// Find the end time of the next sweep at the held tilt after `ts`.
    pub(crate) fn next_matching_sweep_end(&self, ts: f64, tilt: TiltFilter) -> Option<f64> {
        for scan in &self.scans {
            let cuts = tilt.cuts(scan.vcp_pattern.as_ref());
            for sweep in &scan.sweeps {
                if cuts.contains(sweep.elevation_number) && sweep.end_time > ts + 0.5 {
                    return Some(sweep.end_time);
                }
            }
        }
        None
    }

    /// Find the end time of the previous sweep at the held tilt before `ts`.
    pub(crate) fn prev_matching_sweep_end(&self, ts: f64, tilt: TiltFilter) -> Option<f64> {
        let mut best: Option<f64> = None;
        for scan in self.scans.iter().rev() {
            let cuts = tilt.cuts(scan.vcp_pattern.as_ref());
            for sweep in scan.sweeps.iter().rev() {
                if cuts.contains(sweep.elevation_number) && sweep.end_time < ts - 0.5 {
                    match best {
                        None => best = Some(sweep.end_time),
                        Some(b) if sweep.end_time > b => best = Some(sweep.end_time),
                        _ => {}
                    }
                }
            }
            if let Some(b) = best {
                if scan.end_time < b {
                    break;
                }
            }
        }
        best
    }

    /// Find the end time of the next sweep of any elevation after `ts`.
    pub(crate) fn next_any_sweep_end(&self, ts: f64) -> Option<f64> {
        for scan in &self.scans {
            for sweep in &scan.sweeps {
                if sweep.end_time > ts + 0.5 {
                    return Some(sweep.end_time);
                }
            }
        }
        None
    }

    /// Find the end time of the previous sweep of any elevation before `ts`.
    pub(crate) fn prev_any_sweep_end(&self, ts: f64) -> Option<f64> {
        let mut best: Option<f64> = None;
        for scan in self.scans.iter().rev() {
            for sweep in scan.sweeps.iter().rev() {
                if sweep.end_time < ts - 0.5 {
                    match best {
                        None => best = Some(sweep.end_time),
                        Some(b) if sweep.end_time > b => best = Some(sweep.end_time),
                        _ => {}
                    }
                }
            }
            if let Some(b) = best {
                if scan.end_time < b {
                    break;
                }
            }
        }
        best
    }

    /// Index of the scan that `ts` resolves into: the last scan whose
    /// `start_time <= ts`. Matches the render resolver's "most recent scan at or
    /// before the playhead" (see
    /// [`crate::core::playback_manager::resolve_active_sweep_target`]). Returns
    /// `None` when `ts` is before every scan.
    fn scan_index_at(&self, ts: f64) -> Option<usize> {
        let idx = self.scans.partition_point(|s| s.start_time <= ts);
        (idx > 0).then(|| idx - 1)
    }

    /// A renderable timestamp to land on inside `scan` when scan-stepping with a
    /// `Fixed` tilt. Prefers the scan's earliest sweep at the held tilt (its
    /// first matching frame); if the scan lacks that
    /// tilt, falls back to the scan's earliest sweep end; with no sweeps at all,
    /// the scan's start. Keeps a scan step landing on the whole volume even when
    /// the selected tilt is absent.
    fn scan_landing_at_tilt(scan: &Scan, tilt: TiltFilter) -> f64 {
        let cuts = tilt.cuts(scan.vcp_pattern.as_ref());
        scan.sweeps
            .iter()
            .filter(|s| cuts.contains(s.elevation_number))
            .map(|s| s.end_time)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .or_else(|| {
                scan.sweeps
                    .iter()
                    .map(|s| s.end_time)
                    .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            })
            .unwrap_or(scan.start_time)
    }

    /// A renderable timestamp to land on inside `scan` when scan-stepping in
    /// `Latest` mode: the scan's earliest sweep end (any tilt), else its start.
    fn scan_landing_any(scan: &Scan) -> f64 {
        scan.sweeps
            .iter()
            .map(|s| s.end_time)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(scan.start_time)
    }

    /// Scan step forward (spec §12 Shift+→): a landing timestamp inside the
    /// *next whole volume scan* after the one `ts` resolves into, matching
    /// the held tilt. When `ts` is before all scans, lands in the first
    /// scan. `None` when there is no later scan.
    pub(crate) fn next_scan_matching_sweep_end(&self, ts: f64, tilt: TiltFilter) -> Option<f64> {
        let next_idx = match self.scan_index_at(ts) {
            Some(i) => i + 1,
            None => 0,
        };
        let scan = self.scans.get(next_idx)?;
        Some(Self::scan_landing_at_tilt(scan, tilt))
    }

    /// Scan step backward (spec §12 Shift+←): a landing timestamp inside the
    /// *previous whole volume scan* before the one `ts` resolves into, matching
    /// the held tilt. `None` when there is no earlier scan.
    pub(crate) fn prev_scan_matching_sweep_end(&self, ts: f64, tilt: TiltFilter) -> Option<f64> {
        let cur = self.scan_index_at(ts)?;
        let prev = cur.checked_sub(1)?;
        let scan = self.scans.get(prev)?;
        Some(Self::scan_landing_at_tilt(scan, tilt))
    }

    /// Scan step forward in `Latest` mode (any tilt). See
    /// [`Self::next_scan_matching_sweep_end`].
    pub(crate) fn next_scan_any_sweep_end(&self, ts: f64) -> Option<f64> {
        let next_idx = match self.scan_index_at(ts) {
            Some(i) => i + 1,
            None => 0,
        };
        let scan = self.scans.get(next_idx)?;
        Some(Self::scan_landing_any(scan))
    }

    /// Scan step backward in `Latest` mode (any tilt). See
    /// [`Self::prev_scan_matching_sweep_end`].
    pub(crate) fn prev_scan_any_sweep_end(&self, ts: f64) -> Option<f64> {
        let cur = self.scan_index_at(ts)?;
        let prev = cur.checked_sub(1)?;
        let scan = self.scans.get(prev)?;
        Some(Self::scan_landing_any(scan))
    }

    /// Find scans that overlap with the given time range, by real-data
    /// extent (`end_time`). The display counterpart is
    /// [`Self::scans_in_visual_range`]; keep this for callers that need the
    /// scan's true cached-data bounds rather than the projected block.
    #[allow(dead_code)] // Doc above: retained real-data-extent counterpart to the visual-range query.
    pub(crate) fn scans_in_range(&self, start: f64, end: f64) -> impl Iterator<Item = &Scan> {
        self.scans
            .iter()
            .filter(move |scan| scan.end_time >= start && scan.start_time <= end)
    }

    // Display-extent culling and the clamped right edge live in
    // `TimelineView::visual_scans_in_range`, which also has the archive shadow
    // boundaries needed to clamp sparse scans against the next *known* (not just
    // downloaded) volume. See `state::timeline_view::clamped_display_end`.

    /// Builds a timeline from cached scan metadata.
    ///
    /// This is the fast path for loading the timeline from IndexedDB -
    /// it only uses lightweight metadata, not full scan data.
    /// Sweeps are left empty and loaded on-demand when a scan is selected.
    pub(crate) fn from_metadata(metadata_list: Vec<ScanMetadata>) -> Self {
        // Default scan duration estimate (5 minutes) when end_timestamp is unknown
        const DEFAULT_SCAN_DURATION_SECS: i64 = 300;

        let scans = metadata_list
            .into_iter()
            .map(|meta| {
                let ts_secs = meta.key.scan_start.as_secs();
                let start_time = ts_secs as f64;
                let end_time =
                    meta.end_timestamp
                        .unwrap_or(ts_secs + DEFAULT_SCAN_DURATION_SECS) as f64;

                // Convert persisted sweep metadata to timeline Sweep structs
                let sweeps: Vec<Sweep> = meta
                    .sweeps
                    .unwrap_or_default()
                    .into_iter()
                    .map(|sm| Sweep {
                        start_time: sm.start,
                        end_time: sm.end,
                        elevation: sm.elevation,
                        elevation_number: sm.elevation_number,
                        start_azimuth: sm.start_azimuth,
                        radials: Vec::new(),
                        cached_products: sm.cached_products,
                    })
                    .collect();

                let vcp_number = meta.vcp.as_ref().map(|v| v.number).unwrap_or(0);

                // Adjust scan bounds to encompass all sweep times.
                // Sweep times come from actual radial collection timestamps, which
                // can precede the nominal scan key timestamp or extend past the
                // computed end. Ensure the scan fully contains its sweeps.
                let sweep_min: Option<f64> = sweeps.iter().map(|s| s.start_time).reduce(f64::min);
                let sweep_max: Option<f64> = sweeps.iter().map(|s| s.end_time).reduce(f64::max);
                let start_time = match sweep_min {
                    Some(sm) if sm < start_time => sm,
                    _ => start_time,
                };
                let end_time = match sweep_max {
                    Some(sm) if sm > end_time => sm,
                    _ => end_time,
                };

                Scan {
                    start_time,
                    end_time,
                    key_timestamp: ts_secs as f64,
                    vcp: vcp_number,
                    vcp_pattern: meta.vcp,
                    sweeps,
                    completeness: meta.completeness,
                    cached_sweep_count: meta.cached_sweep_count,
                    planned_sweep_count: meta.planned_sweep_count,
                }
            })
            .collect();

        Self { scans }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Helper to create a minimal Scan for testing (no sweeps).
    fn scan(start: f64, end: f64) -> Scan {
        Scan {
            start_time: start,
            end_time: end,
            key_timestamp: start,
            vcp: 215,
            vcp_pattern: None,
            sweeps: Vec::new(),
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    /// Helper to create a Scan with sweeps.
    fn scan_with_sweeps(start: f64, end: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: start,
            end_time: end,
            key_timestamp: start,
            vcp: 215,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    /// Test sweep with the default product ("reflectivity") cached, so it
    /// counts as a frame under product filtering. Use [`sweep_products`] to
    /// vary the cached set.
    fn sweep(start: f64, end: f64, elevation: f32, elev_num: u8) -> Sweep {
        sweep_products(start, end, elevation, elev_num, &["reflectivity"])
    }

    fn sweep_products(
        start: f64,
        end: f64,
        elevation: f32,
        elev_num: u8,
        products: &[&str],
    ) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: products.iter().map(|p| p.to_string()).collect(),
        }
    }

    // --- lookback_window tests ---

    use crate::core::ElevationSelection;

    fn fixed(elev_num: u8) -> ElevationSelection {
        ElevationSelection::Fixed {
            elevation_number: elev_num,
            angle: 0.5,
        }
    }

    /// Timeline of `count` scans, each with a single elevation-1 sweep ending
    /// at 100, 200, 300, ... so frames are easy to reason about.
    fn elev1_timeline(count: usize) -> RadarTimeline {
        let scans = (1..=count)
            .map(|i| {
                let end = (i as f64) * 100.0;
                scan_with_sweeps(end - 50.0, end, vec![sweep(end - 50.0, end, 0.5, 1)])
            })
            .collect();
        RadarTimeline { scans }
    }

    fn shadow(start: i64, end: i64) -> ScanBoundary {
        ScanBoundary { start, end }
    }

    /// One cached volume at 1000..1300 with a 0.5° sweep ending at +30 and a
    /// 1.5° sweep ending at +60 (the template for listed volumes).
    fn one_cached_volume() -> RadarTimeline {
        let mut scan = scan_with_sweeps(
            1000.0,
            1300.0,
            vec![sweep(1000.0, 1030.0, 0.5, 1), sweep(1030.0, 1060.0, 1.5, 2)],
        );
        scan.completeness = Some(ScanCompleteness::Complete);
        RadarTimeline { scans: vec![scan] }
    }

    #[wasm_bindgen_test]
    fn known_frames_place_listed_volumes_by_the_cached_template() {
        let tl = one_cached_volume();
        // Two listed volumes before the cached one, plus the cached one's own
        // listing entry (covered, so not counted twice).
        let shadows = [shadow(400, 700), shadow(700, 1000), shadow(1000, 1300)];
        assert_eq!(
            tl.known_frame_ends(&shadows, None, TiltFilter::number(1), "reflectivity"),
            vec![430.0, 730.0, 1030.0]
        );
        // Latest/auto counts every sweep, listed volumes included.
        assert_eq!(
            tl.known_frame_ends(&shadows, None, TiltFilter::Every, "reflectivity"),
            vec![430.0, 460.0, 730.0, 760.0, 1030.0, 1060.0]
        );
    }

    #[wasm_bindgen_test]
    fn known_frames_use_the_vcp_plan_of_a_volume_joined_mid_stream() {
        use crate::data::{ExtractedVcp, ExtractedVcpElevation};
        let cut = |angle: f32| ExtractedVcpElevation {
            angle,
            waveform: "CS".to_string(),
            prf_number: 1,
            is_sails: false,
            is_mrle: false,
            is_base_tilt: false,
            azimuth_rate: Some(12.0), // 30 s per cut
        };
        // Streaming joined at the third cut: only it is cached, but the plan
        // knows all three, both for the listed volume before it and for the
        // two cuts it missed.
        let mut scan = scan_with_sweeps(1000.0, 1090.0, vec![sweep(1060.0, 1090.0, 2.4, 3)]);
        scan.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: vec![cut(0.5), cut(1.5), cut(2.4)],
        });
        let tl = RadarTimeline { scans: vec![scan] };
        let shadows = [shadow(700, 1000)];
        assert_eq!(
            tl.known_frame_ends(&shadows, None, TiltFilter::Every, "reflectivity"),
            vec![730.0, 760.0, 790.0, 1030.0, 1060.0, 1090.0]
        );
        assert_eq!(
            tl.known_frame_ends(&shadows, None, TiltFilter::number(1), "reflectivity"),
            vec![730.0, 1030.0]
        );
        // The last 3 frames are the live volume's own cuts, not a reach back
        // across them into the previous volume.
        assert_eq!(
            tl.lookback_window(
                &shadows,
                None,
                &ElevationSelection::Latest,
                "reflectivity",
                1100.0,
                3
            ),
            Some((1030.0, 1090.0))
        );
    }

    #[wasm_bindgen_test]
    fn known_frames_take_the_live_volume_as_the_timeline_draws_it() {
        use crate::core::projection::{
            ProjectionScanRole, ScanProjection, SweepProjection, SweepProjectionStatus,
            SweepTimingProvenance,
        };
        let cut = |elevation_number: u8, start: f64, end: f64| SweepProjection {
            elevation_number,
            elevation_angle: 0.5,
            scan_role: ProjectionScanRole::CurrentInProgress,
            status: SweepProjectionStatus::FutureExpected,
            timing: SweepTimingProvenance::Estimated,
            collection_start_secs: start,
            collection_end_secs: end,
            chunks_in_sweep: 0,
            chunks_received: 0,
            radials_received: 0,
            azimuth_rate_dps: 12.0,
            chunks: Vec::new(),
        };
        let projection = ScanProjection {
            vcp_number: 212,
            vcp_pattern: None,
            roster: crate::core::VolumeElevationRoster::default(),
            in_progress_elevation: None,
            in_progress_radials: None,
            volume_start: 1000.0,
            volume_end: 1120.0,
            sweeps: vec![
                cut(1, 1000.0, 1025.0),
                cut(2, 1025.0, 1050.0),
                cut(3, 1050.0, 1090.0),
                cut(4, 1090.0, 1120.0),
            ],
            extrapolation: None,
            next_scan_ghost: None,
        };
        // Joined at the third cut: only it is cached. The first two were
        // collected before the stream joined, the fourth is still to come.
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1090.0,
                vec![sweep(1050.0, 1090.0, 2.4, 3)],
            )],
        };
        let live = Some(LiveVolume {
            key_secs: 1000.0,
            projection: &projection,
        });
        assert_eq!(
            tl.known_frame_ends(&[], live, TiltFilter::Every, "reflectivity"),
            vec![1025.0, 1050.0, 1090.0, 1120.0]
        );
        // At 1100 the last 3 frames are this volume's first three cuts.
        assert_eq!(
            tl.lookback_window(
                &[],
                live,
                &ElevationSelection::Latest,
                "reflectivity",
                1100.0,
                3
            ),
            Some((1025.0, 1090.0))
        );
    }

    #[wasm_bindgen_test]
    fn known_frames_without_a_template_count_one_per_listed_volume() {
        let tl = RadarTimeline::default();
        let shadows = [shadow(400, 700), shadow(700, 1000)];
        assert_eq!(
            tl.known_frame_ends(&shadows, None, TiltFilter::number(1), "reflectivity"),
            vec![700.0, 1000.0]
        );
    }

    #[wasm_bindgen_test]
    fn lookback_window_spans_listed_frames_before_they_download() {
        // Only the newest volume is cached, but the listing knows the earlier
        // ones, so "last 4 frames" in auto mode reaches back 2 volumes by
        // their sweeps, not 4 whole volumes.
        let tl = one_cached_volume();
        let shadows = [shadow(100, 400), shadow(400, 700), shadow(700, 1000)];
        let w = tl
            .lookback_window(
                &shadows,
                None,
                &ElevationSelection::Latest,
                "reflectivity",
                2000.0,
                4,
            )
            .unwrap();
        assert_eq!(w, (730.0, 1060.0));
        // At a fixed tilt, one frame per volume.
        let w = tl
            .lookback_window(&shadows, None, &fixed(1), "reflectivity", 2000.0, 3)
            .unwrap();
        assert_eq!(w, (430.0, 1030.0));
    }

    #[wasm_bindgen_test]
    fn lookback_window_takes_last_n_before_now() {
        let tl = elev1_timeline(10); // frames at 100..1000
        let w = tl
            .lookback_window(&[], None, &fixed(1), "reflectivity", 1000.0, 5)
            .unwrap();
        assert_eq!(w, (600.0, 1000.0)); // last 5: 600,700,800,900,1000
    }

    #[wasm_bindgen_test]
    fn lookback_window_excludes_future_frames() {
        let tl = elev1_timeline(10); // frames at 100..1000
                                     // now=550 → only frames <= 550 (100..500) are usable; last 3 = 300..500
        let w = tl
            .lookback_window(&[], None, &fixed(1), "reflectivity", 550.0, 3)
            .unwrap();
        assert_eq!(w, (300.0, 500.0));
    }

    #[wasm_bindgen_test]
    fn lookback_window_is_none_with_fewer_than_n() {
        let tl = elev1_timeline(3); // frames at 100,200,300
        assert_eq!(
            tl.lookback_window(&[], None, &fixed(1), "reflectivity", 10_000.0, 5),
            None
        );
        // Exactly n frames is enough.
        let w = tl
            .lookback_window(&[], None, &fixed(1), "reflectivity", 10_000.0, 3)
            .unwrap();
        assert_eq!(w, (100.0, 300.0));
    }

    #[wasm_bindgen_test]
    fn lookback_window_single_frame_is_none() {
        let tl = elev1_timeline(1); // single frame at 100
        assert_eq!(
            tl.lookback_window(&[], None, &fixed(1), "reflectivity", 10_000.0, 5),
            None
        );
    }

    #[wasm_bindgen_test]
    fn lookback_window_none_when_no_frames_before_now() {
        let tl = elev1_timeline(3); // earliest frame at 100
        assert!(tl
            .lookback_window(&[], None, &fixed(1), "reflectivity", 10.0, 5)
            .is_none());
    }

    #[wasm_bindgen_test]
    fn lookback_window_latest_uses_all_elevations() {
        // Two elevations per scan; Latest counts every sweep as a frame.
        let scans = vec![
            scan_with_sweeps(
                50.0,
                100.0,
                vec![sweep(50.0, 80.0, 0.5, 1), sweep(80.0, 100.0, 1.5, 2)],
            ),
            scan_with_sweeps(
                150.0,
                200.0,
                vec![sweep(150.0, 180.0, 0.5, 1), sweep(180.0, 200.0, 1.5, 2)],
            ),
        ];
        let tl = RadarTimeline { scans };
        // 4 frames: 80,100,180,200 → last 3 = 100..200
        let w = tl
            .lookback_window(
                &[],
                None,
                &ElevationSelection::Latest,
                "reflectivity",
                10_000.0,
                3,
            )
            .unwrap();
        assert_eq!(w, (100.0, 200.0));
    }

    // --- TimeRange tests ---

    #[wasm_bindgen_test]
    fn time_range_duration() {
        let r = TimeRange::new(100.0, 400.0);
        assert_eq!(r.duration(), 300.0);
    }

    #[wasm_bindgen_test]
    fn time_range_contains() {
        let r = TimeRange::new(100.0, 200.0);
        assert!(r.contains(100.0)); // start inclusive
        assert!(r.contains(150.0));
        assert!(r.contains(200.0)); // end inclusive
        assert!(!r.contains(99.9));
        assert!(!r.contains(200.1));
    }

    // --- Scan tests ---

    #[wasm_bindgen_test]
    fn scan_find_sweep_at_timestamp() {
        let s = scan_with_sweeps(
            1000.0,
            1030.0,
            vec![
                sweep(1000.0, 1010.0, 0.5, 1),
                sweep(1010.0, 1020.0, 0.9, 2),
                sweep(1020.0, 1030.0, 1.3, 3),
            ],
        );
        let (idx, sw) = s.find_sweep_at_timestamp(1005.0).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(sw.elevation_number, 1);

        let (idx, sw) = s.find_sweep_at_timestamp(1015.0).unwrap();
        assert_eq!(idx, 1);
        assert_eq!(sw.elevation_number, 2);

        assert!(s.find_sweep_at_timestamp(999.0).is_none());
    }

    // --- RadarTimeline tests ---

    #[wasm_bindgen_test]
    fn time_ranges_empty() {
        let tl = RadarTimeline { scans: vec![] };
        assert!(tl.time_ranges().is_empty());
    }

    #[wasm_bindgen_test]
    fn time_ranges_single_scan() {
        let tl = RadarTimeline {
            scans: vec![scan(1000.0, 1300.0)],
        };
        let ranges = tl.time_ranges();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 1000.0);
        assert_eq!(ranges[0].end, 1300.0);
    }

    #[wasm_bindgen_test]
    fn time_ranges_contiguous_scans() {
        // Scans 5 minutes apart — should be one range
        let tl = RadarTimeline {
            scans: vec![
                scan(1000.0, 1300.0),
                scan(1300.0, 1600.0),
                scan(1600.0, 1900.0),
            ],
        };
        let ranges = tl.time_ranges();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 1000.0);
        assert_eq!(ranges[0].end, 1900.0);
    }

    #[wasm_bindgen_test]
    fn time_ranges_with_gap() {
        // Two groups separated by more than MAX_CONTIGUOUS_GAP_SECS (15 min = 900s)
        let tl = RadarTimeline {
            scans: vec![
                scan(1000.0, 1300.0),
                scan(1300.0, 1600.0),
                // gap of 1000s > 900s
                scan(2600.0, 2900.0),
            ],
        };
        let ranges = tl.time_ranges();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].start, 1000.0);
        assert_eq!(ranges[0].end, 1600.0);
        assert_eq!(ranges[1].start, 2600.0);
        assert_eq!(ranges[1].end, 2900.0);
    }

    #[wasm_bindgen_test]
    fn find_scan_at_timestamp() {
        let tl = RadarTimeline {
            scans: vec![scan(1000.0, 1300.0), scan(1300.0, 1600.0)],
        };
        let s = tl.find_scan_at_timestamp(1150.0).unwrap();
        assert_eq!(s.start_time, 1000.0);

        let s = tl.find_scan_at_timestamp(1400.0).unwrap();
        assert_eq!(s.start_time, 1300.0);

        assert!(tl.find_scan_at_timestamp(999.0).is_none());
        assert!(tl.find_scan_at_timestamp(1601.0).is_none());
    }

    #[wasm_bindgen_test]
    fn find_recent_scan() {
        let tl = RadarTimeline {
            scans: vec![scan(1000.0, 1300.0), scan(1300.0, 1600.0)],
        };
        // Timestamp after last scan, within 600s window
        let s = tl.find_recent_scan(1700.0, 600.0).unwrap();
        assert_eq!(s.start_time, 1300.0);

        // Too old
        assert!(tl.find_recent_scan(2500.0, 600.0).is_none());
    }

    #[wasm_bindgen_test]
    fn next_matching_sweep_end_by_number() {
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1040.0,
                vec![
                    sweep(1000.0, 1010.0, 0.5, 1),
                    sweep(1010.0, 1020.0, 0.9, 2),
                    sweep(1020.0, 1030.0, 0.5, 3), // same angle, different number
                    sweep(1030.0, 1040.0, 0.9, 4),
                ],
            )],
        };
        // From ts=1005, next elev_num=1 sweep end is at 1010 (already past) — none
        // Actually 1010 > 1005 + 0.5 = 1005.5, so 1010 qualifies
        assert_eq!(
            tl.next_matching_sweep_end(1005.0, TiltFilter::number(1)),
            Some(1010.0)
        );
        // From ts=1005, next elev_num=3 sweep end is at 1030
        assert_eq!(
            tl.next_matching_sweep_end(1005.0, TiltFilter::number(3)),
            Some(1030.0)
        );
        // From ts=1005, next elev_num=2 sweep end is at 1020
        assert_eq!(
            tl.next_matching_sweep_end(1005.0, TiltFilter::number(2)),
            Some(1020.0)
        );
    }

    #[wasm_bindgen_test]
    fn prev_matching_sweep_end_by_number() {
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1040.0,
                vec![
                    sweep(1000.0, 1010.0, 0.5, 1),
                    sweep(1010.0, 1020.0, 0.9, 2),
                    sweep(1020.0, 1030.0, 0.5, 3),
                    sweep(1030.0, 1040.0, 0.9, 4),
                ],
            )],
        };
        // From ts=1035, prev elev_num=3 sweep end is at 1030
        assert_eq!(
            tl.prev_matching_sweep_end(1035.0, TiltFilter::number(3)),
            Some(1030.0)
        );
        // From ts=1025, prev elev_num=2 sweep end is at 1020
        assert_eq!(
            tl.prev_matching_sweep_end(1025.0, TiltFilter::number(2)),
            Some(1020.0)
        );
    }

    #[wasm_bindgen_test]
    fn scan_step_by_number_lands_in_adjacent_volume() {
        // Three volume scans, each with an elev-1 and elev-2 sweep.
        let tl = RadarTimeline {
            scans: vec![
                scan_with_sweeps(
                    1000.0,
                    1040.0,
                    vec![sweep(1000.0, 1010.0, 0.5, 1), sweep(1010.0, 1020.0, 0.9, 2)],
                ),
                scan_with_sweeps(
                    1300.0,
                    1340.0,
                    vec![sweep(1300.0, 1310.0, 0.5, 1), sweep(1310.0, 1320.0, 0.9, 2)],
                ),
                scan_with_sweeps(
                    1600.0,
                    1640.0,
                    vec![sweep(1600.0, 1610.0, 0.5, 1), sweep(1610.0, 1620.0, 0.9, 2)],
                ),
            ],
        };

        // Mid first scan → next scan's elev-1 landing (earliest matching sweep end).
        assert_eq!(
            tl.next_scan_matching_sweep_end(1015.0, TiltFilter::number(1)),
            Some(1310.0)
        );
        // Mid middle scan → third scan's elev-1 landing.
        assert_eq!(
            tl.next_scan_matching_sweep_end(1315.0, TiltFilter::number(1)),
            Some(1610.0)
        );
        // From the last scan there is no later scan.
        assert_eq!(
            tl.next_scan_matching_sweep_end(1615.0, TiltFilter::number(1)),
            None
        );

        // Backward: mid middle scan → first scan's elev-2 landing.
        assert_eq!(
            tl.prev_scan_matching_sweep_end(1315.0, TiltFilter::number(2)),
            Some(1020.0)
        );
        // From the first scan there is no earlier scan.
        assert_eq!(
            tl.prev_scan_matching_sweep_end(1015.0, TiltFilter::number(1)),
            None
        );
    }

    #[wasm_bindgen_test]
    fn scan_step_before_all_scans_lands_in_first() {
        let tl = RadarTimeline {
            scans: vec![
                scan_with_sweeps(1000.0, 1040.0, vec![sweep(1000.0, 1010.0, 0.5, 1)]),
                scan_with_sweeps(1300.0, 1340.0, vec![sweep(1300.0, 1310.0, 0.5, 1)]),
            ],
        };
        // A position before every scan steps forward into the first scan.
        assert_eq!(
            tl.next_scan_matching_sweep_end(500.0, TiltFilter::number(1)),
            Some(1010.0)
        );
        // ...and there is nothing earlier than it to step back to.
        assert_eq!(
            tl.prev_scan_matching_sweep_end(500.0, TiltFilter::number(1)),
            None
        );
    }

    #[wasm_bindgen_test]
    fn scan_step_missing_tilt_falls_back_to_earliest_sweep() {
        // The next scan lacks elev-2 entirely; the landing falls back to its
        // earliest sweep end so the step still lands on the whole volume.
        let tl = RadarTimeline {
            scans: vec![
                scan_with_sweeps(1000.0, 1020.0, vec![sweep(1000.0, 1010.0, 0.5, 1)]),
                scan_with_sweeps(1300.0, 1320.0, vec![sweep(1300.0, 1312.0, 0.5, 1)]),
            ],
        };
        assert_eq!(
            tl.next_scan_matching_sweep_end(1005.0, TiltFilter::number(2)),
            Some(1312.0)
        );
    }

    #[wasm_bindgen_test]
    fn scan_step_any_uses_earliest_sweep_of_adjacent_volume() {
        let tl = RadarTimeline {
            scans: vec![
                scan_with_sweeps(
                    1000.0,
                    1040.0,
                    vec![sweep(1000.0, 1010.0, 0.5, 1), sweep(1010.0, 1020.0, 0.9, 2)],
                ),
                scan_with_sweeps(
                    1300.0,
                    1340.0,
                    vec![sweep(1300.0, 1310.0, 0.5, 1), sweep(1310.0, 1320.0, 0.9, 2)],
                ),
            ],
        };
        // Latest mode: next scan's earliest sweep end.
        assert_eq!(tl.next_scan_any_sweep_end(1015.0), Some(1310.0));
        // Backward from the second scan: first scan's earliest sweep end.
        assert_eq!(tl.prev_scan_any_sweep_end(1315.0), Some(1010.0));
    }

    #[wasm_bindgen_test]
    fn matching_sweep_end_times_by_number_basic() {
        let tl = RadarTimeline {
            scans: vec![
                scan_with_sweeps(
                    1000.0,
                    1040.0,
                    vec![
                        sweep(1000.0, 1010.0, 0.5, 1),
                        sweep(1010.0, 1020.0, 0.9, 2),
                        sweep(1020.0, 1030.0, 0.5, 3),
                        sweep(1030.0, 1040.0, 0.9, 4),
                    ],
                ),
                scan_with_sweeps(
                    1300.0,
                    1340.0,
                    vec![sweep(1300.0, 1310.0, 0.5, 1), sweep(1310.0, 1320.0, 0.9, 2)],
                ),
            ],
        };
        // All elev_num=1 sweeps, no bounds
        let times = tl.matching_sweep_end_times(TiltFilter::number(1), "reflectivity", None);
        assert_eq!(times, vec![1010.0, 1310.0]);

        // All elev_num=2 sweeps, no bounds
        let times = tl.matching_sweep_end_times(TiltFilter::number(2), "reflectivity", None);
        assert_eq!(times, vec![1020.0, 1320.0]);
    }

    #[wasm_bindgen_test]
    fn matching_sweep_end_times_by_number_with_bounds() {
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1040.0,
                vec![
                    sweep(1000.0, 1010.0, 0.5, 1),
                    sweep(1010.0, 1020.0, 0.9, 2),
                    sweep(1020.0, 1030.0, 0.5, 3),
                    sweep(1030.0, 1040.0, 0.9, 4),
                ],
            )],
        };
        // elev_num=1 sweeps within bounds [1005, 1025]
        let times = tl.matching_sweep_end_times(
            TiltFilter::number(1),
            "reflectivity",
            Some((1005.0, 1025.0)),
        );
        assert_eq!(times, vec![1010.0]);
    }

    #[wasm_bindgen_test]
    fn all_sweep_end_times_basic() {
        let tl = RadarTimeline {
            scans: vec![
                scan_with_sweeps(
                    1000.0,
                    1040.0,
                    vec![
                        sweep(1000.0, 1010.0, 0.5, 1),
                        sweep(1010.0, 1020.0, 0.9, 2),
                        sweep(1020.0, 1030.0, 0.5, 3),
                        sweep(1030.0, 1040.0, 0.9, 4),
                    ],
                ),
                scan_with_sweeps(
                    1300.0,
                    1320.0,
                    vec![sweep(1300.0, 1310.0, 0.5, 1), sweep(1310.0, 1320.0, 0.9, 2)],
                ),
            ],
        };
        // All sweeps (Latest mode): every sweep is a frame
        let times = tl.all_sweep_end_times("reflectivity", None);
        assert_eq!(times, vec![1010.0, 1020.0, 1030.0, 1040.0, 1310.0, 1320.0]);
    }

    #[wasm_bindgen_test]
    fn frame_list_filters_by_product() {
        // A frame is a sweep matching the selected product AND tilt. Sweeps
        // missing the requested product (or with an empty/unknown set) are not
        // frames — the canvas would blank on them.
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1040.0,
                vec![
                    // elev 1: only reflectivity cached.
                    sweep_products(1000.0, 1010.0, 0.5, 1, &["reflectivity"]),
                    // elev 1 (second cut): both reflectivity and velocity.
                    sweep_products(1010.0, 1020.0, 0.5, 1, &["reflectivity", "velocity"]),
                    // elev 1 (third cut): unknown/empty — never a frame.
                    sweep_products(1020.0, 1030.0, 0.5, 1, &[]),
                ],
            )],
        };

        // Reflectivity: the two cuts that cached it.
        assert_eq!(
            tl.matching_sweep_end_times(TiltFilter::number(1), "reflectivity", None),
            vec![1010.0, 1020.0]
        );
        // Velocity: only the second cut cached it.
        assert_eq!(
            tl.matching_sweep_end_times(TiltFilter::number(1), "velocity", None),
            vec![1020.0]
        );
        // A product nobody cached: no frames.
        assert!(tl
            .matching_sweep_end_times(TiltFilter::number(1), "spectrum_width", None)
            .is_empty());

        // Latest mode honours the same filter.
        assert_eq!(tl.all_sweep_end_times("velocity", None), vec![1020.0]);
        // Empty cached_products is excluded even in Latest mode.
        assert_eq!(
            tl.all_sweep_end_times("reflectivity", None),
            vec![1010.0, 1020.0]
        );
    }

    #[wasm_bindgen_test]
    fn matching_sweep_end_times_empty() {
        let tl = RadarTimeline { scans: vec![] };
        let times = tl.matching_sweep_end_times(TiltFilter::number(1), "reflectivity", None);
        assert!(times.is_empty());
    }

    #[wasm_bindgen_test]
    fn scans_in_range() {
        let tl = RadarTimeline {
            scans: vec![
                scan(1000.0, 1300.0),
                scan(1300.0, 1600.0),
                scan(1600.0, 1900.0),
            ],
        };
        let result: Vec<_> = tl.scans_in_range(1200.0, 1500.0).collect();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].start_time, 1000.0);
        assert_eq!(result[1].start_time, 1300.0);
    }

    #[wasm_bindgen_test]
    fn display_end_time_projects_sparse_scan_to_full_vcp_volume() {
        use crate::data::{ExtractedVcp, ExtractedVcpElevation};

        // A sparse scan whose cached sweeps ended early: real data spans only
        // [1000, 1010], but the VCP plan projects a 120s volume so the drawn
        // block reaches to 1120. (360° / 3°/s = 120s; 3.0 is exact in f32.)
        // Clamping against neighbors/archive boundaries lives in
        // `TimelineView::visual_scans_in_range`; this only covers the per-scan
        // projection the view uses as its no-archive fallback.
        let mut sparse = scan(1000.0, 1010.0);
        sparse.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: vec![ExtractedVcpElevation {
                angle: 0.5,
                waveform: "CS".to_string(),
                prf_number: 1,
                is_sails: false,
                is_mrle: false,
                is_base_tilt: false,
                azimuth_rate: Some(3.0),
            }],
        });
        assert_eq!(sparse.display_end_time(), 1120.0);

        // With no VCP pattern it falls back to the real-data end_time.
        assert_eq!(scan(1000.0, 1010.0).display_end_time(), 1010.0);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use crate::core::ElevationSelection;

    // --- local builders (mirror the sibling `mod tests` private helpers) ---

    fn scan(start: f64, end: f64) -> Scan {
        Scan {
            start_time: start,
            end_time: end,
            key_timestamp: start,
            vcp: 215,
            vcp_pattern: None,
            sweeps: Vec::new(),
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn scan_with_sweeps(start: f64, end: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: start,
            end_time: end,
            key_timestamp: start,
            vcp: 215,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn sweep(start: f64, end: f64, elevation: f32, elev_num: u8) -> Sweep {
        sweep_products(start, end, elevation, elev_num, &["reflectivity"])
    }

    fn sweep_products(
        start: f64,
        end: f64,
        elevation: f32,
        elev_num: u8,
        products: &[&str],
    ) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: products.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn fixed(elev_num: u8) -> ElevationSelection {
        ElevationSelection::Fixed {
            elevation_number: elev_num,
            angle: 0.5,
        }
    }

    // --- scan_end_frame_times (volumetric 3D frame source) ---

    #[wasm_bindgen_test]
    fn scan_end_frame_times_one_frame_per_product_bearing_scan() {
        let tl = RadarTimeline {
            scans: vec![
                // Three sweeps → still ONE frame, at the scan's end_time.
                scan_with_sweeps(
                    1000.0,
                    1300.0,
                    vec![
                        sweep(1000.0, 1010.0, 0.5, 1),
                        sweep(1010.0, 1020.0, 1.5, 2),
                        sweep(1020.0, 1030.0, 2.4, 3),
                    ],
                ),
                // Sweeps exist but none carry the product → excluded.
                scan_with_sweeps(
                    1300.0,
                    1600.0,
                    vec![sweep_products(1300.0, 1310.0, 0.5, 1, &["velocity"])],
                ),
                // No sweeps at all → excluded.
                scan(1600.0, 1900.0),
                scan_with_sweeps(1900.0, 2200.0, vec![sweep(1900.0, 1910.0, 0.5, 1)]),
            ],
        };
        assert_eq!(
            tl.scan_end_frame_times("reflectivity", None),
            vec![1300.0, 2200.0]
        );
    }

    #[wasm_bindgen_test]
    fn scan_end_frame_times_clips_to_bounds() {
        let tl = RadarTimeline {
            scans: vec![
                scan_with_sweeps(1000.0, 1300.0, vec![sweep(1000.0, 1010.0, 0.5, 1)]),
                scan_with_sweeps(1300.0, 1600.0, vec![sweep(1300.0, 1310.0, 0.5, 1)]),
            ],
        };
        // Bounds admit only the first scan's end_time.
        assert_eq!(
            tl.scan_end_frame_times("reflectivity", Some((900.0, 1400.0))),
            vec![1300.0]
        );
        assert!(tl
            .scan_end_frame_times("reflectivity", Some((0.0, 500.0)))
            .is_empty());
    }

    // --- Sweep::duration / Scan::duration ---

    #[wasm_bindgen_test]
    fn sweep_duration_is_end_minus_start() {
        let sw = sweep(1000.0, 1012.5, 0.5, 1);
        assert!((sw.duration() - 12.5).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn scan_duration_is_end_minus_start() {
        let s = scan(2000.0, 2300.0);
        assert!((s.duration() - 300.0).abs() < 1e-9);
    }

    // --- Scan::key_ms ---

    #[wasm_bindgen_test]
    fn scan_key_ms_converts_seconds_to_millis() {
        // key_timestamp == start_time in the builder; 1700000000.789s -> ms (rounded).
        let mut s = scan(1000.0, 1100.0);
        s.key_timestamp = 1_700_000_000.789;
        assert_eq!(s.key_ms(), 1_700_000_000_789);
    }

    #[wasm_bindgen_test]
    fn scan_key_ms_whole_second() {
        let mut s = scan(0.0, 10.0);
        s.key_timestamp = 1500.0;
        assert_eq!(s.key_ms(), 1_500_000);
    }

    // --- Scan::target_elevation_angle (static VCP table fallback) ---

    #[wasm_bindgen_test]
    fn target_elevation_angle_uses_static_vcp_table_when_no_pattern() {
        // vcp 215, no extracted pattern -> falls through to the static table.
        // elevation_number 1 maps to index 0 (angle 0.5); number 5 -> index 4 (2.4).
        let s = scan(0.0, 10.0); // vcp 215, vcp_pattern None
        assert!((s.target_elevation_angle(1).unwrap() - 0.5).abs() < 1e-4);
        assert!((s.target_elevation_angle(5).unwrap() - 2.4).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn target_elevation_angle_number_zero_saturates_to_first() {
        // elevation_number 0 saturating_sub(1) -> idx 0.
        let s = scan(0.0, 10.0);
        assert!((s.target_elevation_angle(0).unwrap() - 0.5).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn target_elevation_angle_out_of_range_is_none() {
        // VCP 215 has 14 elevations; number 99 -> idx 98 is out of range, and
        // there is no extracted pattern, so neither source has an entry.
        let s = scan(0.0, 10.0);
        assert_eq!(s.target_elevation_angle(99), None);
    }

    #[wasm_bindgen_test]
    fn target_elevation_angle_unknown_vcp_is_none() {
        // A VCP number with no static definition and no pattern -> None.
        let mut s = scan(0.0, 10.0);
        s.vcp = 9999;
        assert_eq!(s.target_elevation_angle(1), None);
    }

    #[wasm_bindgen_test]
    fn target_elevation_angle_prefers_extracted_pattern() {
        use crate::data::{ExtractedVcp, ExtractedVcpElevation};
        let mut s = scan(0.0, 10.0);
        s.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: vec![
                ExtractedVcpElevation {
                    angle: 7.25,
                    waveform: "CS".to_string(),
                    prf_number: 1,
                    is_sails: false,
                    is_mrle: false,
                    is_base_tilt: false,
                    azimuth_rate: None,
                },
                ExtractedVcpElevation {
                    angle: 8.5,
                    waveform: "CS".to_string(),
                    prf_number: 1,
                    is_sails: false,
                    is_mrle: false,
                    is_base_tilt: false,
                    azimuth_rate: None,
                },
            ],
        });
        // Pattern wins over the static 215 table (0.5/0.9).
        assert!((s.target_elevation_angle(1).unwrap() - 7.25).abs() < 1e-4);
        assert!((s.target_elevation_angle(2).unwrap() - 8.5).abs() < 1e-4);
        // Number past the pattern (idx 2) falls back to the static table: idx 2 = 1.3.
        assert!((s.target_elevation_angle(3).unwrap() - 1.3).abs() < 1e-4);
    }

    // --- Scan::display_angle ---

    #[wasm_bindgen_test]
    fn display_angle_uses_vcp_target_when_available() {
        // sweep measured average is noisy (0.61); display angle anchors to the
        // commanded 0.5 from the static VCP 215 table for elevation_number 1.
        let s = scan(0.0, 10.0);
        let sw = sweep(0.0, 10.0, 0.61, 1);
        assert!((s.display_angle(&sw) - 0.5).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn display_angle_falls_back_to_measured_when_no_target() {
        // Unknown VCP, no pattern -> no target -> use the sweep's measured angle.
        let mut s = scan(0.0, 10.0);
        s.vcp = 9999;
        let sw = sweep(0.0, 10.0, 3.25, 7);
        assert!((s.display_angle(&sw) - 3.25).abs() < 1e-4);
    }

    // --- RadarTimeline::find_recent_scan (None branch when before all scans) ---

    #[wasm_bindgen_test]
    fn find_recent_scan_none_before_all_scans() {
        let tl = RadarTimeline {
            scans: vec![scan(1000.0, 1300.0), scan(1300.0, 1600.0)],
        };
        // ts before the first scan -> partition_point == 0 -> wrapping_sub None.
        assert!(tl.find_recent_scan(500.0, 10_000.0).is_none());
    }

    // --- RadarTimeline::find_previous_scan (entirely untested) ---

    #[wasm_bindgen_test]
    fn find_previous_scan_returns_scan_two_before() {
        let tl = RadarTimeline {
            scans: vec![
                scan(1000.0, 1300.0),
                scan(1300.0, 1600.0),
                scan(1600.0, 1900.0),
            ],
        };
        // ts inside the third scan (idx 2) -> idx==3 -> idx-2==1 is the previous scan.
        let prev = tl.find_previous_scan(1700.0, 10_000.0).unwrap();
        assert_eq!(prev.start_time, 1300.0);
    }

    #[wasm_bindgen_test]
    fn find_previous_scan_none_inside_first_scan() {
        let tl = RadarTimeline {
            scans: vec![scan(1000.0, 1300.0), scan(1300.0, 1600.0)],
        };
        // ts inside the first scan -> idx==1 (<2) -> None.
        assert!(tl.find_previous_scan(1100.0, 10_000.0).is_none());
    }

    #[wasm_bindgen_test]
    fn find_previous_scan_none_when_too_old() {
        let tl = RadarTimeline {
            scans: vec![
                scan(1000.0, 1300.0),
                scan(2000.0, 2300.0),
                scan(2400.0, 2700.0),
            ],
        };
        // ts in scan 3; previous (idx 1) starts at 2000; ts-2000 = 600 > 100 window.
        assert!(tl.find_previous_scan(2600.0, 100.0).is_none());
        // Wider window admits it.
        let prev = tl.find_previous_scan(2600.0, 10_000.0).unwrap();
        assert_eq!(prev.start_time, 2000.0);
    }

    // --- find_scan_at_timestamp edge: empty + before-all ---

    #[wasm_bindgen_test]
    fn find_scan_at_timestamp_empty_and_before_all() {
        let empty = RadarTimeline { scans: vec![] };
        assert!(empty.find_scan_at_timestamp(1000.0).is_none());

        let tl = RadarTimeline {
            scans: vec![scan(1000.0, 1300.0)],
        };
        // ts before any scan -> partition_point 0 -> wrapping_sub underflow -> None.
        assert!(tl.find_scan_at_timestamp(500.0).is_none());
        // ts inside the gap after the scan's end -> None (end check fails).
        assert!(tl.find_scan_at_timestamp(1400.0).is_none());
    }

    // --- next_any_sweep_end / prev_any_sweep_end (untested) ---

    #[wasm_bindgen_test]
    fn next_any_sweep_end_returns_first_future_sweep() {
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1040.0,
                vec![
                    sweep(1000.0, 1010.0, 0.5, 1),
                    sweep(1010.0, 1020.0, 0.9, 2),
                    sweep(1020.0, 1030.0, 1.3, 3),
                ],
            )],
        };
        // From ts=1012: 1010 is not > 1012.5, 1020 is -> 1020.
        assert_eq!(tl.next_any_sweep_end(1012.0), Some(1020.0));
        // Past the last sweep -> None.
        assert_eq!(tl.next_any_sweep_end(1030.0), None);
    }

    #[wasm_bindgen_test]
    fn prev_any_sweep_end_returns_latest_past_sweep() {
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1040.0,
                vec![
                    sweep(1000.0, 1010.0, 0.5, 1),
                    sweep(1010.0, 1020.0, 0.9, 2),
                    sweep(1020.0, 1030.0, 1.3, 3),
                ],
            )],
        };
        // From ts=1025: sweeps ending < 1024.5 are 1010, 1020 -> latest 1020.
        assert_eq!(tl.prev_any_sweep_end(1025.0), Some(1020.0));
        // Before all sweeps -> None.
        assert_eq!(tl.prev_any_sweep_end(1005.0), None);
    }

    // --- next_matching_sweep_end_by_number None when no later match ---

    #[wasm_bindgen_test]
    fn next_matching_sweep_end_by_number_none_when_absent() {
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1020.0,
                vec![sweep(1000.0, 1010.0, 0.5, 1)],
            )],
        };
        // No elevation_number 2 exists at all.
        assert_eq!(
            tl.next_matching_sweep_end(1000.0, TiltFilter::number(2)),
            None
        );
        // elevation_number 1 exists but its only sweep ends at/<= ts+0.5.
        assert_eq!(
            tl.next_matching_sweep_end(1010.0, TiltFilter::number(1)),
            None
        );
    }

    // --- matching_sweep_end_times: dedup near-equal end times ---

    #[wasm_bindgen_test]
    fn matching_sweep_end_times_dedups_near_equal() {
        // Two scans with an elev-1 sweep ending at ~the same time (within 0.01).
        let tl = RadarTimeline {
            scans: vec![
                scan_with_sweeps(1000.0, 1010.0, vec![sweep(1000.0, 1010.0, 0.5, 1)]),
                scan_with_sweeps(1010.0, 1020.0, vec![sweep(1010.0, 1010.005, 0.5, 1)]),
            ],
        };
        let times = tl.matching_sweep_end_times(TiltFilter::number(1), "reflectivity", None);
        assert_eq!(times.len(), 1);
        assert!((times[0] - 1010.0).abs() < 0.01);
    }

    // --- matching_sweep_end_times: scan-level bounds cull whole scan ---

    #[wasm_bindgen_test]
    fn matching_sweep_end_times_culls_scan_outside_bounds() {
        let tl = RadarTimeline {
            scans: vec![
                scan_with_sweeps(1000.0, 1010.0, vec![sweep(1000.0, 1010.0, 0.5, 1)]),
                scan_with_sweeps(5000.0, 5010.0, vec![sweep(5000.0, 5010.0, 0.5, 1)]),
            ],
        };
        // Bounds only cover the first scan; the second scan's start_time > end.
        let times = tl.matching_sweep_end_times(
            TiltFilter::number(1),
            "reflectivity",
            Some((900.0, 1100.0)),
        );
        assert_eq!(times, vec![1010.0]);
    }

    // --- lookback_window: Latest mode none when no frames before now ---

    #[wasm_bindgen_test]
    fn lookback_window_latest_none_before_now() {
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1010.0,
                vec![sweep(1000.0, 1010.0, 0.5, 1)],
            )],
        };
        assert!(tl
            .lookback_window(
                &[],
                None,
                &ElevationSelection::Latest,
                "reflectivity",
                100.0,
                5
            )
            .is_none());
    }

    #[wasm_bindgen_test]
    fn lookback_window_n_zero_treated_as_one() {
        // frames at 100,200,300; n=0 -> n.max(1)=1 -> only the last frame.
        let scans = (1..=3)
            .map(|i| {
                let end = (i as f64) * 100.0;
                scan_with_sweeps(end - 50.0, end, vec![sweep(end - 50.0, end, 0.5, 1)])
            })
            .collect();
        let tl = RadarTimeline { scans };
        let w = tl
            .lookback_window(&[], None, &fixed(1), "reflectivity", 10_000.0, 0)
            .unwrap();
        assert_eq!(w, (300.0, 300.0));
    }

    // --- display_end_time: real end wins when later than VCP projection ---

    #[wasm_bindgen_test]
    fn display_end_time_keeps_real_end_when_data_extends_past_vcp() {
        use crate::data::{ExtractedVcp, ExtractedVcpElevation};
        // VCP projects a 120s volume (360/3) -> projected end 1120. But the real
        // data already extends to 1500, so the max keeps 1500.
        let mut s = scan(1000.0, 1500.0);
        s.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: vec![ExtractedVcpElevation {
                angle: 0.5,
                waveform: "CS".to_string(),
                prf_number: 1,
                is_sails: false,
                is_mrle: false,
                is_base_tilt: false,
                azimuth_rate: Some(3.0),
            }],
        });
        assert!((s.display_end_time() - 1500.0).abs() < 1e-6);
    }

    // --- from_metadata ---

    fn meta(
        site: &str,
        scan_start_secs: i64,
        end_timestamp: Option<i64>,
        sweeps: Option<Vec<crate::data::CachedSweep>>,
    ) -> ScanMetadata {
        ScanMetadata {
            key: crate::data::keys::ScanKey::from_secs(site, scan_start_secs),
            file_name: "f".to_string(),
            file_size: 0,
            end_timestamp,
            vcp: None,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
            sweeps,
        }
    }

    fn cached_sweep(start: f64, end: f64, elev: f32, num: u8) -> crate::data::CachedSweep {
        crate::data::CachedSweep {
            start,
            end,
            elevation: elev,
            elevation_number: num,
            start_azimuth: 12.0,
            cached_products: vec!["reflectivity".to_string()],
        }
    }

    #[wasm_bindgen_test]
    fn from_metadata_default_duration_when_end_unknown() {
        // No end_timestamp, no sweeps -> end defaults to start + 300s.
        let tl = RadarTimeline::from_metadata(vec![meta("KDMX", 1000, None, None)]);
        assert_eq!(tl.scans.len(), 1);
        let s = &tl.scans[0];
        assert!((s.start_time - 1000.0).abs() < 1e-6);
        assert!((s.end_time - 1300.0).abs() < 1e-6);
        assert!((s.key_timestamp - 1000.0).abs() < 1e-6);
        assert_eq!(s.vcp, 0); // no vcp -> 0
        assert!(s.sweeps.is_empty());
    }

    #[wasm_bindgen_test]
    fn from_metadata_expands_bounds_to_contain_sweeps() {
        // Sweep starts before the nominal scan start and ends after the
        // declared end_timestamp -> both bounds expand to cover the sweep.
        let sweeps = Some(vec![cached_sweep(990.0, 1320.0, 0.5, 1)]);
        let tl = RadarTimeline::from_metadata(vec![meta("KDMX", 1000, Some(1100), sweeps)]);
        let s = &tl.scans[0];
        // start_time pulled earlier to the sweep start.
        assert!((s.start_time - 990.0).abs() < 1e-6);
        // end_time pushed later to the sweep end (past declared 1100).
        assert!((s.end_time - 1320.0).abs() < 1e-6);
        // key_timestamp stays anchored to the nominal scan start.
        assert!((s.key_timestamp - 1000.0).abs() < 1e-6);
        // Sweep fields carried over; radials left empty for the metadata path.
        assert_eq!(s.sweeps.len(), 1);
        assert_eq!(s.sweeps[0].elevation_number, 1);
        assert!((s.sweeps[0].start_azimuth - 12.0).abs() < 1e-6);
        assert!(s.sweeps[0].radials.is_empty());
        assert_eq!(
            s.sweeps[0].cached_products,
            vec!["reflectivity".to_string()]
        );
    }

    #[wasm_bindgen_test]
    fn from_metadata_keeps_declared_bounds_when_sweeps_within() {
        // Sweep fully inside [1000, 1100] -> bounds unchanged.
        let sweeps = Some(vec![cached_sweep(1010.0, 1090.0, 0.5, 1)]);
        let tl = RadarTimeline::from_metadata(vec![meta("KDMX", 1000, Some(1100), sweeps)]);
        let s = &tl.scans[0];
        assert!((s.start_time - 1000.0).abs() < 1e-6);
        assert!((s.end_time - 1100.0).abs() < 1e-6);
    }
}
