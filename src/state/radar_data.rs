//! Radar data structures for timeline representation.

/// A single radial (one azimuth direction at one elevation)
#[derive(Clone, Debug)]
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
    /// Individual radials in this sweep
    pub radials: Vec<Radial>,
}

impl Sweep {
    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }
}

/// A complete volume scan (multiple sweeps at different elevations)
#[derive(Clone, Debug)]
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
    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }
}

/// Collection of radar data for timeline display
#[derive(Clone, Debug, Default)]
pub struct RadarTimeline {
    /// All scans, ordered by start time
    pub scans: Vec<Scan>,
}

impl RadarTimeline {
    pub fn new() -> Self {
        Self { scans: Vec::new() }
    }

    /// Get the time range covered by this data
    pub fn time_range(&self) -> Option<(f64, f64)> {
        if self.scans.is_empty() {
            return None;
        }
        let start = self.scans.first().unwrap().start_time;
        let end = self.scans.last().unwrap().end_time;
        Some((start, end))
    }

    /// Generate sample data for testing/demo purposes
    /// Creates scans for the specified duration ending at `end_time`
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
}
