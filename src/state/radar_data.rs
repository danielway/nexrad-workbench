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
}

impl Sweep {
    #[allow(dead_code)]
    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }
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
    pub present_records: Option<u32>,
    /// Expected number of records (from cache metadata).
    pub expected_records: Option<u32>,
}

impl Scan {
    #[allow(dead_code)]
    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
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

    /// Find the scan immediately before the one containing `ts`.
    ///
    /// Returns `None` if `ts` is before or within the first scan.
    pub fn find_previous_scan(&self, ts: f64) -> Option<&Scan> {
        let idx = self.scans.partition_point(|s| s.start_time <= ts);
        // idx-1 is the scan containing ts; idx-2 is the one before it
        if idx >= 2 {
            self.scans.get(idx - 2)
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
                present_records: None,
                expected_records: None,
            });

            // Next scan starts after the interval
            current_time = scan_start + scan_interval;
        }

        Self { scans }
    }

    /// Find the nearest scan or sweep boundary to a given timestamp.
    /// Returns the snapped timestamp if a boundary is within `max_dist_secs`.
    pub fn snap_to_boundary(&self, ts: f64, max_dist_secs: f64) -> Option<f64> {
        let mut best: Option<f64> = None;
        let mut best_dist = max_dist_secs;

        for scan in &self.scans {
            // Check scan boundaries
            for &boundary in &[scan.start_time, scan.end_time] {
                let dist = (ts - boundary).abs();
                if dist < best_dist {
                    best_dist = dist;
                    best = Some(boundary);
                }
            }
            // Check sweep boundaries
            for sweep in &scan.sweeps {
                for &boundary in &[sweep.start_time, sweep.end_time] {
                    let dist = (ts - boundary).abs();
                    if dist < best_dist {
                        best_dist = dist;
                        best = Some(boundary);
                    }
                }
            }
        }

        best
    }

    /// Find the end time of the next matching sweep after the given timestamp.
    ///
    /// "Matching" means the sweep's elevation is within `elev_tolerance` of
    /// `target_elevation`. Returns the sweep's end_time so the cursor lands
    /// at the completion of that sweep.
    pub fn next_matching_sweep_end(
        &self,
        ts: f64,
        target_elevation: f32,
        elev_tolerance: f32,
    ) -> Option<f64> {
        for scan in &self.scans {
            for sweep in &scan.sweeps {
                if (sweep.elevation - target_elevation).abs() < elev_tolerance
                    && sweep.end_time > ts + 0.5
                {
                    return Some(sweep.end_time);
                }
            }
        }
        None
    }

    /// Find the end time of the previous matching sweep before the given timestamp.
    ///
    /// Searches backward through sweeps to find the most recent matching sweep
    /// whose end_time is before the current position.
    pub fn prev_matching_sweep_end(
        &self,
        ts: f64,
        target_elevation: f32,
        elev_tolerance: f32,
    ) -> Option<f64> {
        let mut best: Option<f64> = None;
        for scan in self.scans.iter().rev() {
            for sweep in scan.sweeps.iter().rev() {
                if (sweep.elevation - target_elevation).abs() < elev_tolerance
                    && sweep.end_time < ts - 0.5
                {
                    // First match scanning backward is the most recent
                    match best {
                        None => best = Some(sweep.end_time),
                        Some(b) if sweep.end_time > b => best = Some(sweep.end_time),
                        _ => {}
                    }
                }
            }
            // If we found one and this scan ends before what we found, no need to keep looking
            if let Some(b) = best {
                if scan.end_time < b {
                    break;
                }
            }
        }
        best
    }

    /// Find scans that overlap with the given time range
    pub fn scans_in_range(&self, start: f64, end: f64) -> impl Iterator<Item = &Scan> {
        self.scans
            .iter()
            .filter(move |scan| scan.end_time >= start && scan.start_time <= end)
    }

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
                    present_records: meta.present_records,
                    expected_records: meta.expected_records,
                }
            })
            .collect();

        Self { scans }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            present_records: None,
            expected_records: None,
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
            present_records: None,
            expected_records: None,
        }
    }

    fn sweep(start: f64, end: f64, elevation: f32, elev_num: u8) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::new(),
        }
    }

    // --- TimeRange tests ---

    #[test]
    fn time_range_duration() {
        let r = TimeRange::new(100.0, 400.0);
        assert_eq!(r.duration(), 300.0);
    }

    #[test]
    fn time_range_contains() {
        let r = TimeRange::new(100.0, 200.0);
        assert!(r.contains(100.0)); // start inclusive
        assert!(r.contains(150.0));
        assert!(r.contains(200.0)); // end inclusive
        assert!(!r.contains(99.9));
        assert!(!r.contains(200.1));
    }

    // --- Scan tests ---

    #[test]
    fn scan_progress_at_timestamp() {
        let s = scan(1000.0, 1100.0);
        assert_eq!(s.progress_at_timestamp(1000.0), Some(0.0));
        assert_eq!(s.progress_at_timestamp(1050.0), Some(0.5));
        assert_eq!(s.progress_at_timestamp(1100.0), Some(1.0));
        assert_eq!(s.progress_at_timestamp(999.0), None);
        assert_eq!(s.progress_at_timestamp(1101.0), None);
    }

    #[test]
    fn scan_progress_zero_duration() {
        let s = scan(1000.0, 1000.0);
        assert_eq!(s.progress_at_timestamp(1000.0), Some(0.0));
    }

    #[test]
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

    #[test]
    fn time_ranges_empty() {
        let tl = RadarTimeline { scans: vec![] };
        assert!(tl.time_ranges().is_empty());
    }

    #[test]
    fn time_ranges_single_scan() {
        let tl = RadarTimeline {
            scans: vec![scan(1000.0, 1300.0)],
        };
        let ranges = tl.time_ranges();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 1000.0);
        assert_eq!(ranges[0].end, 1300.0);
    }

    #[test]
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

    #[test]
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

    #[test]
    fn overall_time_range() {
        let tl = RadarTimeline {
            scans: vec![scan(1000.0, 1300.0), scan(5000.0, 5300.0)],
        };
        assert_eq!(tl.overall_time_range(), Some((1000.0, 5300.0)));
    }

    #[test]
    fn overall_time_range_empty() {
        let tl = RadarTimeline { scans: vec![] };
        assert_eq!(tl.overall_time_range(), None);
    }

    #[test]
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

    #[test]
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

    #[test]
    fn snap_to_boundary() {
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1030.0,
                vec![sweep(1000.0, 1010.0, 0.5, 1), sweep(1010.0, 1020.0, 0.9, 2)],
            )],
        };
        // Close to sweep boundary at 1010
        assert_eq!(tl.snap_to_boundary(1011.0, 5.0), Some(1010.0));
        // Too far from any boundary
        assert_eq!(tl.snap_to_boundary(1015.0, 2.0), None);
    }

    #[test]
    fn next_matching_sweep_end() {
        let tl = RadarTimeline {
            scans: vec![scan_with_sweeps(
                1000.0,
                1040.0,
                vec![
                    sweep(1000.0, 1010.0, 0.5, 1),
                    sweep(1010.0, 1020.0, 0.9, 2),
                    sweep(1020.0, 1030.0, 0.5, 3), // same elevation as first
                    sweep(1030.0, 1040.0, 0.9, 4),
                ],
            )],
        };
        // From ts=1005, next 0.5° sweep end is at 1030
        assert_eq!(tl.next_matching_sweep_end(1005.0, 0.5, 0.1), Some(1030.0));
        // From ts=1005, next 0.9° sweep end is at 1020
        assert_eq!(tl.next_matching_sweep_end(1005.0, 0.9, 0.1), Some(1020.0));
    }

    #[test]
    fn prev_matching_sweep_end() {
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
        // From ts=1035, prev 0.5° sweep end is at 1030
        assert_eq!(tl.prev_matching_sweep_end(1035.0, 0.5, 0.1), Some(1030.0));
        // From ts=1025, prev 0.9° sweep end is at 1020
        assert_eq!(tl.prev_matching_sweep_end(1025.0, 0.9, 0.1), Some(1020.0));
    }

    #[test]
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
}
