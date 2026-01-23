//! Radar data structures for timeline representation.

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
    #[allow(dead_code)] // Part of TimeRange API
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
#[allow(dead_code)] // Fields are part of data model, used in generate_sample_data
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
#[allow(dead_code)] // Fields are part of data model, used in generate_sample_data
pub struct Sweep {
    /// Start timestamp (Unix seconds with sub-second precision)
    pub start_time: f64,
    /// End timestamp
    pub end_time: f64,
    /// Elevation angle in degrees
    pub elevation: f32,
    /// Individual radials in this sweep
    pub radials: Vec<Radial>,
}

impl Sweep {
    #[allow(dead_code)] // Part of data model API
    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }

    /// Interpolate azimuth for a timestamp within this sweep (smooth animation)
    /// Assumes the sweep rotates 360 degrees from start to end
    pub fn interpolate_azimuth(&self, ts: f64) -> Option<f32> {
        if ts < self.start_time || ts > self.end_time {
            return None;
        }
        let duration = self.end_time - self.start_time;
        if duration <= 0.0 {
            return Some(0.0);
        }
        let progress = (ts - self.start_time) / duration;
        Some((progress * 360.0) as f32)
    }
}

/// A complete volume scan (multiple sweeps at different elevations)
#[derive(Clone, Debug)]
#[allow(dead_code)] // vcp field is part of data model, used in generate_sample_data
pub struct Scan {
    /// Start timestamp (Unix seconds with sub-second precision)
    pub start_time: f64,
    /// End timestamp
    pub end_time: f64,
    /// Volume Coverage Pattern identifier (e.g., VCP 215)
    pub vcp: u16,
    /// Sweeps in this scan, ordered by elevation
    pub sweeps: Vec<Sweep>,
}

impl Scan {
    #[allow(dead_code)] // Part of data model API
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

    /// Find the scan containing the given timestamp
    pub fn find_scan_at_timestamp(&self, ts: f64) -> Option<&Scan> {
        self.scans
            .iter()
            .find(|scan| ts >= scan.start_time && ts <= scan.end_time)
    }

    /// Find the most recent scan at or before the given timestamp, within a time window.
    ///
    /// Returns the scan whose start_time is closest to (but not after) the timestamp,
    /// as long as it's within `max_age_secs` of the timestamp.
    pub fn find_recent_scan(&self, ts: f64, max_age_secs: f64) -> Option<&Scan> {
        // Find all scans that start at or before the timestamp
        let candidates: Vec<_> = self
            .scans
            .iter()
            .filter(|scan| scan.start_time <= ts)
            .collect();

        // Get the most recent one (last in the sorted list)
        let most_recent = candidates.last()?;

        // Check if it's within the time window
        let age = ts - most_recent.start_time;
        if age <= max_age_secs {
            Some(most_recent)
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

            for &elevation in elevations {
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
                    radials,
                });

                sweep_time = sweep_end + 0.5; // Small gap between sweeps
            }

            let scan_end = sweep_time;
            scans.push(Scan {
                start_time: scan_start,
                end_time: scan_end,
                vcp: 215,
                sweeps,
            });

            // Next scan starts after the interval
            current_time = scan_start + scan_interval;
        }

        Self { scans }
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
                let start_time = meta.key.timestamp as f64;
                let end_time = meta
                    .end_timestamp
                    .unwrap_or(meta.key.timestamp + DEFAULT_SCAN_DURATION_SECS)
                    as f64;

                Scan {
                    start_time,
                    end_time,
                    vcp: meta.vcp.unwrap_or(0),
                    sweeps: Vec::new(), // Loaded on-demand when scan is selected
                }
            })
            .collect();

        Self { scans }
    }
}
