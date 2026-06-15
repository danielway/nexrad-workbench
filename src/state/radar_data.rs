//! Radar data structures for timeline representation.

use crate::data::ScanCompleteness;
use crate::nexrad::ScanMetadata;

/// A contiguous time range of radar data.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeRange {
    /// Start timestamp (Unix seconds)
    pub start: f64,
    /// End timestamp (Unix seconds)
    pub end: f64,
}

impl TimeRange {
    /// Creates a new time range.
    pub fn new(start: f64, end: f64) -> Self {
        Self { start, end }
    }

    /// Returns the duration of this range in seconds.
    #[allow(dead_code)]
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }

    /// Returns true if the given timestamp is within this range.
    pub fn contains(&self, ts: f64) -> bool {
        ts >= self.start && ts <= self.end
    }
}

/// A single radial (one azimuth direction at one elevation)
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Radial {
    /// Start timestamp (Unix seconds with sub-second precision)
    pub start_time: f64,
    /// Duration in seconds
    pub duration: f64,
    /// Azimuth angle in degrees
    pub azimuth: f32,
}

/// A sweep (360-degree rotation at one elevation)
#[derive(Clone, Debug)]
pub struct Sweep {
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
    #[allow(dead_code)]
    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }
}

/// Whether `sweep` has a cached blob for `product` (worker-string name).
/// Empty `cached_products` means "nothing stored / unknown" and is treated as
/// not-available — matching the render resolver
/// ([`crate::state::playback_manager::resolve_active_sweep_target`]) so the
/// macro frame list never lists a frame the canvas would blank.
fn sweep_has_product(sweep: &Sweep, product: &str) -> bool {
    sweep.cached_products.iter().any(|p| p == product)
}

/// A complete volume scan (multiple sweeps at different elevations)
#[derive(Clone, Debug)]
pub struct Scan {
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
    pub vcp_pattern: Option<crate::data::keys::ExtractedVcp>,
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
    #[allow(dead_code)]
    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }

    /// The scan's storage-key timestamp in milliseconds — the same value
    /// encoded in its `ScanKey`. Use this (not ad-hoc `* 1000.0` math) when
    /// matching against millisecond keys from IDB or the live stream.
    pub fn key_ms(&self) -> i64 {
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
    pub fn display_end_time(&self) -> f64 {
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
    pub fn target_elevation_angle(&self, elevation_number: u8) -> Option<f32> {
        let idx = elevation_number.saturating_sub(1) as usize;
        if let Some(ref pattern) = self.vcp_pattern {
            if let Some(elev) = pattern.elevations.get(idx) {
                return Some(elev.angle);
            }
        }
        if let Some(def) = crate::state::get_vcp_definition(self.vcp) {
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
    pub fn display_angle(&self, sweep: &Sweep) -> f32 {
        self.target_elevation_angle(sweep.elevation_number)
            .unwrap_or(sweep.elevation)
    }

    /// Find the sweep containing the given timestamp
    pub fn find_sweep_at_timestamp(&self, ts: f64) -> Option<(usize, &Sweep)> {
        self.sweeps
            .iter()
            .enumerate()
            .find(|(_, sweep)| ts >= sweep.start_time && ts <= sweep.end_time)
    }

    /// Calculate scan progress as a percentage (0.0 to 1.0)
    pub fn progress_at_timestamp(&self, ts: f64) -> Option<f32> {
        if ts < self.start_time || ts > self.end_time {
            return None;
        }
        let duration = self.end_time - self.start_time;
        if duration <= 0.0 {
            return Some(0.0);
        }
        Some(((ts - self.start_time) / duration) as f32)
    }
}

/// Collection of radar data for timeline display
#[derive(Clone, Debug, Default)]
pub struct RadarTimeline {
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
    pub fn time_ranges(&self) -> Vec<TimeRange> {
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

    /// Get the overall time range covered by this data (min start to max end).
    ///
    /// This is a convenience method that returns the bounding box of all ranges.
    /// For checking if data exists in a specific period, use `time_ranges()` instead.
    #[allow(dead_code)]
    pub fn overall_time_range(&self) -> Option<(f64, f64)> {
        if self.scans.is_empty() {
            return None;
        }
        let start = self.scans.first().unwrap().start_time;
        let end = self.scans.last().unwrap().end_time;
        Some((start, end))
    }

    /// Find the scan containing the given timestamp.
    ///
    /// Uses binary search on the sorted scan list for O(log n) lookup.
    pub fn find_scan_at_timestamp(&self, ts: f64) -> Option<&Scan> {
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
    pub fn find_recent_scan(&self, ts: f64, max_age_secs: f64) -> Option<&Scan> {
        let idx = self.scans.partition_point(|s| s.start_time <= ts);
        let most_recent = self.scans.get(idx.wrapping_sub(1))?;
        (ts - most_recent.start_time <= max_age_secs).then_some(most_recent)
    }

    /// Find the scan immediately before the one containing `ts`, within a time window.
    ///
    /// Returns `None` if `ts` is before or within the first scan, or if the
    /// previous scan is older than `max_age_secs` from `ts`.
    pub fn find_previous_scan(&self, ts: f64, max_age_secs: f64) -> Option<&Scan> {
        let idx = self.scans.partition_point(|s| s.start_time <= ts);
        // idx-1 is the scan containing ts; idx-2 is the one before it
        if idx >= 2 {
            let scan = self.scans.get(idx - 2)?;
            (ts - scan.start_time <= max_age_secs).then_some(scan)
        } else {
            None
        }
    }

    /// Get the timestamp of a scan for identification purposes.
    /// Used to check if we need to load a different scan.
    #[allow(dead_code)] // Utility method
    pub fn scan_timestamp(scan: &Scan) -> i64 {
        scan.start_time as i64
    }

    /// Generate sample data for testing/demo purposes
    /// Creates scans for the specified duration ending at `end_time`
    #[allow(dead_code)] // Kept for testing/demo purposes
    pub fn generate_sample_data(end_time: f64, duration_hours: f64) -> Self {
        let mut scans = Vec::new();
        let start_time = end_time - duration_hours * 3600.0;

        // VCP 215 typical elevations (degrees)
        let elevations: &[f32] = &[
            0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0, 10.0, 12.5, 15.6, 19.5,
        ];

        let mut current_time = start_time;
        let scan_interval = 300.0; // ~5 minutes between scan starts

        while current_time < end_time {
            let scan_start = current_time;
            let mut sweeps = Vec::new();
            let mut sweep_time = scan_start;

            for (elev_idx, &elevation) in elevations.iter().enumerate() {
                let sweep_start = sweep_time;
                // Sweep duration varies slightly by elevation (higher = faster)
                let sweep_duration = 10.0 + (15.0 - elevation as f64).max(0.0) * 0.5;
                let sweep_end = sweep_start + sweep_duration;

                // Generate radials for this sweep (typically ~720 radials for 0.5 degree azimuth resolution)
                let num_radials = 720;
                let radial_duration = sweep_duration / num_radials as f64;
                let mut radials = Vec::new();

                for i in 0..num_radials {
                    let azimuth = (i as f32) * 0.5; // 0.5 degree resolution
                    radials.push(Radial {
                        start_time: sweep_start + (i as f64) * radial_duration,
                        duration: radial_duration,
                        azimuth,
                    });
                }

                sweeps.push(Sweep {
                    start_time: sweep_start,
                    end_time: sweep_end,
                    elevation,
                    elevation_number: (elev_idx + 1) as u8,
                    start_azimuth: radials.first().map(|r| r.azimuth).unwrap_or(0.0),
                    radials,
                    cached_products: Vec::new(),
                });

                sweep_time = sweep_end + 0.5; // Small gap between sweeps
            }

            let scan_end = sweep_time;
            scans.push(Scan {
                start_time: scan_start,
                end_time: scan_end,
                key_timestamp: scan_start,
                vcp: 215,
                vcp_pattern: None,
                sweeps,
                completeness: Some(ScanCompleteness::Complete),
                cached_sweep_count: None,
                planned_sweep_count: None,
            });

            // Next scan starts after the interval
            current_time = scan_start + scan_interval;
        }

        Self { scans }
    }

    /// Collect all sweep end-times matching the given elevation number AND
    /// holding a cached blob for `product` (the worker-string product name).
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
    pub fn matching_sweep_end_times_by_number(
        &self,
        elevation_number: u8,
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
                if sweep.elevation_number == elevation_number && sweep_has_product(sweep, product) {
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
    /// is a frame. See [`Self::matching_sweep_end_times_by_number`] for the
    /// product-filter contract.
    pub fn all_sweep_end_times(&self, product: &str, bounds: Option<(f64, f64)>) -> Vec<f64> {
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

    /// The `(start, end)` span covering the last `n` matching frames at or
    /// before `now`, for the given elevation selection — the "lookback" window
    /// the live Play button replays. Uses the same frame source as the macro
    /// frame builder ([`Self::matching_sweep_end_times_by_number`] /
    /// [`Self::all_sweep_end_times`]), so a frame here means the same thing it
    /// does for stepping.
    ///
    /// Returns `None` when no frames exist at/<= `now`. With fewer than `n`
    /// frames, spans all available. A single frame yields a zero-width span
    /// (`start == end`); callers must reject that before using it as loop
    /// bounds (looping divides by the span width).
    pub fn lookback_window(
        &self,
        elevation_selection: &crate::state::ElevationSelection,
        product: &str,
        now: f64,
        n: usize,
    ) -> Option<(f64, f64)> {
        let frames = match elevation_selection {
            crate::state::ElevationSelection::Fixed {
                elevation_number, ..
            } => self.matching_sweep_end_times_by_number(*elevation_number, product, None),
            crate::state::ElevationSelection::Latest => self.all_sweep_end_times(product, None),
        };
        // `frames` is sorted ascending; keep those at/<= now (small slack so a
        // just-completed frame whose end_time rounds a hair past `now` counts).
        let cutoff = now + 0.5;
        let usable: Vec<f64> = frames.into_iter().filter(|&t| t <= cutoff).collect();
        if usable.is_empty() {
            return None;
        }
        let start_idx = usable.len().saturating_sub(n.max(1));
        Some((usable[start_idx], *usable.last().unwrap()))
    }

    /// Find the end time of the next sweep matching `elevation_number` after `ts`.
    pub fn next_matching_sweep_end_by_number(&self, ts: f64, elevation_number: u8) -> Option<f64> {
        for scan in &self.scans {
            for sweep in &scan.sweeps {
                if sweep.elevation_number == elevation_number && sweep.end_time > ts + 0.5 {
                    return Some(sweep.end_time);
                }
            }
        }
        None
    }

    /// Find the end time of the previous sweep matching `elevation_number` before `ts`.
    pub fn prev_matching_sweep_end_by_number(&self, ts: f64, elevation_number: u8) -> Option<f64> {
        let mut best: Option<f64> = None;
        for scan in self.scans.iter().rev() {
            for sweep in scan.sweeps.iter().rev() {
                if sweep.elevation_number == elevation_number && sweep.end_time < ts - 0.5 {
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
    pub fn next_any_sweep_end(&self, ts: f64) -> Option<f64> {
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
    pub fn prev_any_sweep_end(&self, ts: f64) -> Option<f64> {
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
    /// [`crate::state::playback_manager::resolve_active_sweep_target`]). Returns
    /// `None` when `ts` is before every scan.
    fn scan_index_at(&self, ts: f64) -> Option<usize> {
        let idx = self.scans.partition_point(|s| s.start_time <= ts);
        (idx > 0).then(|| idx - 1)
    }

    /// A renderable timestamp to land on inside `scan` when scan-stepping with a
    /// `Fixed` tilt. Prefers the scan's earliest sweep matching
    /// `elevation_number` (its first matching frame); if the scan lacks that
    /// tilt, falls back to the scan's earliest sweep end; with no sweeps at all,
    /// the scan's start. Keeps a scan step landing on the whole volume even when
    /// the selected tilt is absent.
    fn scan_landing_by_number(scan: &Scan, elevation_number: u8) -> f64 {
        scan.sweeps
            .iter()
            .filter(|s| s.elevation_number == elevation_number)
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
    /// `elevation_number`. When `ts` is before all scans, lands in the first
    /// scan. `None` when there is no later scan.
    pub fn next_scan_matching_sweep_end_by_number(
        &self,
        ts: f64,
        elevation_number: u8,
    ) -> Option<f64> {
        let next_idx = match self.scan_index_at(ts) {
            Some(i) => i + 1,
            None => 0,
        };
        let scan = self.scans.get(next_idx)?;
        Some(Self::scan_landing_by_number(scan, elevation_number))
    }

    /// Scan step backward (spec §12 Shift+←): a landing timestamp inside the
    /// *previous whole volume scan* before the one `ts` resolves into, matching
    /// `elevation_number`. `None` when there is no earlier scan.
    pub fn prev_scan_matching_sweep_end_by_number(
        &self,
        ts: f64,
        elevation_number: u8,
    ) -> Option<f64> {
        let cur = self.scan_index_at(ts)?;
        let prev = cur.checked_sub(1)?;
        let scan = self.scans.get(prev)?;
        Some(Self::scan_landing_by_number(scan, elevation_number))
    }

    /// Scan step forward in `Latest` mode (any tilt). See
    /// [`Self::next_scan_matching_sweep_end_by_number`].
    pub fn next_scan_any_sweep_end(&self, ts: f64) -> Option<f64> {
        let next_idx = match self.scan_index_at(ts) {
            Some(i) => i + 1,
            None => 0,
        };
        let scan = self.scans.get(next_idx)?;
        Some(Self::scan_landing_any(scan))
    }

    /// Scan step backward in `Latest` mode (any tilt). See
    /// [`Self::prev_scan_matching_sweep_end_by_number`].
    pub fn prev_scan_any_sweep_end(&self, ts: f64) -> Option<f64> {
        let cur = self.scan_index_at(ts)?;
        let prev = cur.checked_sub(1)?;
        let scan = self.scans.get(prev)?;
        Some(Self::scan_landing_any(scan))
    }

    /// Find scans that overlap with the given time range, by real-data
    /// extent (`end_time`). The display counterpart is
    /// [`Self::scans_in_visual_range`]; keep this for callers that need the
    /// scan's true cached-data bounds rather than the projected block.
    #[allow(dead_code)]
    pub fn scans_in_range(&self, start: f64, end: f64) -> impl Iterator<Item = &Scan> {
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
    pub fn from_metadata(metadata_list: Vec<ScanMetadata>) -> Self {
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

    use crate::state::ElevationSelection;

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

    #[wasm_bindgen_test]
    fn lookback_window_takes_last_n_before_now() {
        let tl = elev1_timeline(10); // frames at 100..1000
        let w = tl
            .lookback_window(&fixed(1), "reflectivity", 1000.0, 5)
            .unwrap();
        assert_eq!(w, (600.0, 1000.0)); // last 5: 600,700,800,900,1000
    }

    #[wasm_bindgen_test]
    fn lookback_window_excludes_future_frames() {
        let tl = elev1_timeline(10); // frames at 100..1000
                                     // now=550 → only frames <= 550 (100..500) are usable; last 3 = 300..500
        let w = tl
            .lookback_window(&fixed(1), "reflectivity", 550.0, 3)
            .unwrap();
        assert_eq!(w, (300.0, 500.0));
    }

    #[wasm_bindgen_test]
    fn lookback_window_spans_all_when_fewer_than_n() {
        let tl = elev1_timeline(3); // frames at 100,200,300
        let w = tl
            .lookback_window(&fixed(1), "reflectivity", 10_000.0, 5)
            .unwrap();
        assert_eq!(w, (100.0, 300.0));
    }

    #[wasm_bindgen_test]
    fn lookback_window_single_frame_is_zero_width() {
        let tl = elev1_timeline(1); // single frame at 100
        let w = tl
            .lookback_window(&fixed(1), "reflectivity", 10_000.0, 5)
            .unwrap();
        assert_eq!(w, (100.0, 100.0)); // caller must reject zero width
    }

    #[wasm_bindgen_test]
    fn lookback_window_none_when_no_frames_before_now() {
        let tl = elev1_timeline(3); // earliest frame at 100
        assert!(tl
            .lookback_window(&fixed(1), "reflectivity", 10.0, 5)
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
            .lookback_window(&ElevationSelection::Latest, "reflectivity", 10_000.0, 3)
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
    fn scan_progress_at_timestamp() {
        let s = scan(1000.0, 1100.0);
        assert_eq!(s.progress_at_timestamp(1000.0), Some(0.0));
        assert_eq!(s.progress_at_timestamp(1050.0), Some(0.5));
        assert_eq!(s.progress_at_timestamp(1100.0), Some(1.0));
        assert_eq!(s.progress_at_timestamp(999.0), None);
        assert_eq!(s.progress_at_timestamp(1101.0), None);
    }

    #[wasm_bindgen_test]
    fn scan_progress_zero_duration() {
        let s = scan(1000.0, 1000.0);
        assert_eq!(s.progress_at_timestamp(1000.0), Some(0.0));
    }

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
    fn overall_time_range() {
        let tl = RadarTimeline {
            scans: vec![scan(1000.0, 1300.0), scan(5000.0, 5300.0)],
        };
        assert_eq!(tl.overall_time_range(), Some((1000.0, 5300.0)));
    }

    #[wasm_bindgen_test]
    fn overall_time_range_empty() {
        let tl = RadarTimeline { scans: vec![] };
        assert_eq!(tl.overall_time_range(), None);
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
            tl.next_matching_sweep_end_by_number(1005.0, 1),
            Some(1010.0)
        );
        // From ts=1005, next elev_num=3 sweep end is at 1030
        assert_eq!(
            tl.next_matching_sweep_end_by_number(1005.0, 3),
            Some(1030.0)
        );
        // From ts=1005, next elev_num=2 sweep end is at 1020
        assert_eq!(
            tl.next_matching_sweep_end_by_number(1005.0, 2),
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
            tl.prev_matching_sweep_end_by_number(1035.0, 3),
            Some(1030.0)
        );
        // From ts=1025, prev elev_num=2 sweep end is at 1020
        assert_eq!(
            tl.prev_matching_sweep_end_by_number(1025.0, 2),
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
            tl.next_scan_matching_sweep_end_by_number(1015.0, 1),
            Some(1310.0)
        );
        // Mid middle scan → third scan's elev-1 landing.
        assert_eq!(
            tl.next_scan_matching_sweep_end_by_number(1315.0, 1),
            Some(1610.0)
        );
        // From the last scan there is no later scan.
        assert_eq!(tl.next_scan_matching_sweep_end_by_number(1615.0, 1), None);

        // Backward: mid middle scan → first scan's elev-2 landing.
        assert_eq!(
            tl.prev_scan_matching_sweep_end_by_number(1315.0, 2),
            Some(1020.0)
        );
        // From the first scan there is no earlier scan.
        assert_eq!(tl.prev_scan_matching_sweep_end_by_number(1015.0, 1), None);
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
            tl.next_scan_matching_sweep_end_by_number(500.0, 1),
            Some(1010.0)
        );
        // ...and there is nothing earlier than it to step back to.
        assert_eq!(tl.prev_scan_matching_sweep_end_by_number(500.0, 1), None);
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
            tl.next_scan_matching_sweep_end_by_number(1005.0, 2),
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
        let times = tl.matching_sweep_end_times_by_number(1, "reflectivity", None);
        assert_eq!(times, vec![1010.0, 1310.0]);

        // All elev_num=2 sweeps, no bounds
        let times = tl.matching_sweep_end_times_by_number(2, "reflectivity", None);
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
        let times =
            tl.matching_sweep_end_times_by_number(1, "reflectivity", Some((1005.0, 1025.0)));
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
            tl.matching_sweep_end_times_by_number(1, "reflectivity", None),
            vec![1010.0, 1020.0]
        );
        // Velocity: only the second cut cached it.
        assert_eq!(
            tl.matching_sweep_end_times_by_number(1, "velocity", None),
            vec![1020.0]
        );
        // A product nobody cached: no frames.
        assert!(tl
            .matching_sweep_end_times_by_number(1, "spectrum_width", None)
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
        let times = tl.matching_sweep_end_times_by_number(1, "reflectivity", None);
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
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};

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

    use crate::state::ElevationSelection;

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
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};
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
        let sw = sweep(0.0, 10.0, 3.14, 7);
        assert!((s.display_angle(&sw) - 3.14).abs() < 1e-4);
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

    // --- RadarTimeline::scan_timestamp ---

    #[wasm_bindgen_test]
    fn scan_timestamp_truncates_start_time_to_i64() {
        let s = scan(1700.9, 1800.0);
        assert_eq!(RadarTimeline::scan_timestamp(&s), 1700);
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
        assert_eq!(tl.next_matching_sweep_end_by_number(1000.0, 2), None);
        // elevation_number 1 exists but its only sweep ends at/<= ts+0.5.
        assert_eq!(tl.next_matching_sweep_end_by_number(1010.0, 1), None);
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
        let times = tl.matching_sweep_end_times_by_number(1, "reflectivity", None);
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
        let times = tl.matching_sweep_end_times_by_number(1, "reflectivity", Some((900.0, 1100.0)));
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
            .lookback_window(&ElevationSelection::Latest, "reflectivity", 100.0, 5)
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
            .lookback_window(&fixed(1), "reflectivity", 10_000.0, 0)
            .unwrap();
        assert_eq!(w, (300.0, 300.0));
    }

    // --- display_end_time: real end wins when later than VCP projection ---

    #[wasm_bindgen_test]
    fn display_end_time_keeps_real_end_when_data_extends_past_vcp() {
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};
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
