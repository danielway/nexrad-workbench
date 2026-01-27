//! Dynamic sweep builder for radar rendering.
//!
//! Builds a sweep by selecting the most appropriate radial at each azimuth
//! position based on playback timestamp and target elevation.

use super::VolumeRing;
use ::nexrad::prelude::{Radial, Volume};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Tolerance for elevation angle matching (degrees).
const ELEVATION_TOLERANCE: f32 = 0.5;

/// Dynamic sweep built from radials across multiple volumes.
///
/// Selects one radial per azimuth position based on:
/// - Elevation angle matching the target (+/- tolerance)
/// - Collection timestamp at or before the playback time
/// - Preferring the most recent radial when multiple match
pub struct RenderSweep<'a> {
    /// Radials indexed by azimuth number (integer degrees * 10 for 0.1 degree precision)
    radials: HashMap<u16, RadialEntry<'a>>,
    /// Target elevation angle in degrees
    target_elevation: f32,
    /// Playback timestamp in milliseconds
    playback_timestamp_ms: i64,
    /// Optional azimuth range constraint (start, end) in degrees
    azimuth_range: Option<(f32, f32)>,
}

/// Entry for a radial with its metadata for selection.
struct RadialEntry<'a> {
    radial: &'a Radial,
    timestamp_ms: i64,
}

impl<'a> RenderSweep<'a> {
    /// Create a new RenderSweep for the given target elevation and playback time.
    pub fn new(target_elevation: f32, playback_timestamp_ms: i64) -> Self {
        Self::new_with_range(target_elevation, playback_timestamp_ms, None)
    }

    /// Create a new RenderSweep with optional azimuth range constraint.
    pub fn new_with_range(
        target_elevation: f32,
        playback_timestamp_ms: i64,
        azimuth_range: Option<(f32, f32)>,
    ) -> Self {
        Self {
            radials: HashMap::new(),
            target_elevation,
            playback_timestamp_ms,
            azimuth_range,
        }
    }

    /// Build a RenderSweep from a VolumeRing.
    ///
    /// Iterates through all volumes (oldest to newest) and selects the best
    /// radial at each azimuth position for the target elevation.
    pub fn from_volume_ring(
        ring: &'a VolumeRing,
        target_elevation: f32,
        playback_timestamp_ms: i64,
    ) -> Self {
        Self::from_volume_ring_animated(ring, target_elevation, playback_timestamp_ms, None)
    }

    /// Build a RenderSweep from a VolumeRing with optional azimuth range constraint.
    ///
    /// When `azimuth_range` is provided, only radials within that range are included.
    /// This is used for animated sweep display where only part of the sweep is visible.
    ///
    /// # Arguments
    /// * `ring` - The VolumeRing containing radar volumes
    /// * `target_elevation` - Target elevation angle in degrees
    /// * `playback_timestamp_ms` - Playback timestamp in milliseconds
    /// * `azimuth_range` - Optional (start, end) azimuth range in degrees (0-360)
    pub fn from_volume_ring_animated(
        ring: &'a VolumeRing,
        target_elevation: f32,
        playback_timestamp_ms: i64,
        azimuth_range: Option<(f32, f32)>,
    ) -> Self {
        let mut sweep = Self::new_with_range(target_elevation, playback_timestamp_ms, azimuth_range);

        // Process volumes from oldest to newest so newer radials replace older ones
        for (_volume_ts, volume) in ring.volumes() {
            sweep.consider_volume(volume);
        }

        sweep
    }

    /// Consider all radials in a volume for inclusion in the sweep.
    pub fn consider_volume(&mut self, volume: &'a Volume) {
        for sweep in volume.sweeps() {
            // Check if this sweep's elevation matches our target
            let sweep_elevation = sweep
                .radials()
                .first()
                .map(|r| r.elevation_angle_degrees())
                .unwrap_or(0.0);

            if (sweep_elevation - self.target_elevation).abs() > ELEVATION_TOLERANCE {
                continue;
            }

            for radial in sweep.radials() {
                self.consider_radial(radial);
            }
        }
    }

    /// Consider a single radial for inclusion in the sweep.
    ///
    /// The radial is included if:
    /// 1. Its elevation matches the target (+/- tolerance)
    /// 2. Its timestamp is at or before the playback time
    /// 3. It's newer than any existing radial at this azimuth
    /// 4. Its azimuth is within the optional range constraint
    pub fn consider_radial(&mut self, radial: &'a Radial) {
        // Check elevation match
        let elevation = radial.elevation_angle_degrees();
        if (elevation - self.target_elevation).abs() > ELEVATION_TOLERANCE {
            return;
        }

        // Check timestamp is not in the future
        let timestamp_ms = radial.collection_timestamp();
        if timestamp_ms > self.playback_timestamp_ms {
            return;
        }

        // Check azimuth range constraint if set
        if let Some((start_az, end_az)) = self.azimuth_range {
            let az = radial.azimuth_angle_degrees();
            // Handle wrap-around case (e.g., range from 350° to 10°)
            let in_range = if start_az <= end_az {
                az >= start_az && az <= end_az
            } else {
                az >= start_az || az <= end_az
            };
            if !in_range {
                return;
            }
        }

        // Get azimuth key (using azimuth_number for precision)
        let azimuth_key = radial.azimuth_number();

        // Check if we should replace existing radial (if any)
        let should_insert = match self.radials.get(&azimuth_key) {
            Some(existing) => timestamp_ms > existing.timestamp_ms,
            None => true,
        };

        if should_insert {
            self.radials.insert(
                azimuth_key,
                RadialEntry {
                    radial,
                    timestamp_ms,
                },
            );
        }
    }

    /// Get all radials in the sweep, sorted by azimuth.
    pub fn radials(&self) -> Vec<&'a Radial> {
        let mut entries: Vec<_> = self.radials.iter().collect();
        entries.sort_by_key(|(azimuth_key, _)| *azimuth_key);
        entries.into_iter().map(|(_, entry)| entry.radial).collect()
    }

    /// Get the number of radials in the sweep.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.radials.len()
    }

    /// Returns true if the sweep has no radials.
    pub fn is_empty(&self) -> bool {
        self.radials.is_empty()
    }

    /// Find the most recent radial in the sweep (for indicator display).
    pub fn most_recent_radial(&self) -> Option<&'a Radial> {
        self.radials
            .values()
            .max_by_key(|entry| entry.timestamp_ms)
            .map(|entry| entry.radial)
    }

    /// Calculate data staleness relative to a reference time.
    ///
    /// Returns how many seconds old the newest radial is compared to the
    /// reference time (typically wall-clock time). Useful for showing
    /// staleness indicators in fixed-tilt mode.
    pub fn staleness_seconds(&self, reference_time_ms: i64) -> Option<f64> {
        self.most_recent_radial()
            .map(|r| (reference_time_ms - r.collection_timestamp()) as f64 / 1000.0)
    }

    /// Generate a cache signature for this sweep.
    ///
    /// The signature is a hash of all included radial timestamps, allowing
    /// the texture cache to detect when the sweep contents have changed.
    pub fn cache_signature(&self) -> u64 {
        let mut hasher = DefaultHasher::new();

        // Hash the target elevation (quantized to 0.1 degree)
        let elev_quantized = (self.target_elevation * 10.0).round() as i32;
        elev_quantized.hash(&mut hasher);

        // Hash all radial timestamps in azimuth order for consistency
        let mut entries: Vec<_> = self.radials.iter().collect();
        entries.sort_by_key(|(azimuth_key, _)| *azimuth_key);

        for (azimuth_key, entry) in entries {
            azimuth_key.hash(&mut hasher);
            entry.timestamp_ms.hash(&mut hasher);
        }

        hasher.finish()
    }

    /// Get the target elevation for this sweep.
    #[allow(dead_code)]
    pub fn target_elevation(&self) -> f32 {
        self.target_elevation
    }

    /// Get the playback timestamp for this sweep.
    #[allow(dead_code)]
    pub fn playback_timestamp_ms(&self) -> i64 {
        self.playback_timestamp_ms
    }
}
