//! Circular buffer for decoded NEXRAD volumes.
//!
//! Keeps 2-3 decoded volumes in memory for dynamic sweep rendering
//! across scan boundaries.

use ::nexrad::prelude::Volume;
use std::collections::VecDeque;

/// Default capacity for the volume ring buffer.
const DEFAULT_CAPACITY: usize = 3;

/// Circular buffer keeping decoded volumes in memory.
///
/// Volumes are stored with their timestamps and kept in chronological order.
/// When the buffer is full, the oldest volume is evicted on insert.
pub struct VolumeRing {
    /// (timestamp_ms, Volume) pairs ordered by timestamp
    volumes: VecDeque<(i64, Volume)>,
    /// Maximum number of volumes to keep
    capacity: usize,
}

impl Default for VolumeRing {
    fn default() -> Self {
        Self::new()
    }
}

impl VolumeRing {
    /// Creates a new VolumeRing with default capacity (3 volumes).
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Creates a new VolumeRing with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            volumes: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Insert a volume at the given timestamp.
    ///
    /// Maintains chronological order and evicts oldest if at capacity.
    /// If a volume with the same timestamp already exists, it is replaced.
    pub fn insert(&mut self, timestamp_ms: i64, volume: Volume) {
        // Check for existing volume at this timestamp
        if let Some(pos) = self.volumes.iter().position(|(ts, _)| *ts == timestamp_ms) {
            self.volumes[pos] = (timestamp_ms, volume);
            return;
        }

        // Find insertion position to maintain sorted order
        let insert_pos = self
            .volumes
            .iter()
            .position(|(ts, _)| *ts > timestamp_ms)
            .unwrap_or(self.volumes.len());

        // Evict oldest if at capacity
        if self.volumes.len() >= self.capacity {
            // If inserting at front (older than all), don't insert
            if insert_pos == 0 && self.volumes.len() >= self.capacity {
                log::debug!(
                    "VolumeRing: skipping insert of old volume at {} (oldest is {})",
                    timestamp_ms,
                    self.volumes.front().map(|(ts, _)| *ts).unwrap_or(0)
                );
                return;
            }
            // Remove the oldest (front)
            self.volumes.pop_front();
            // Adjust insert position if we removed from before it
            let insert_pos = insert_pos.saturating_sub(1);
            self.volumes.insert(insert_pos, (timestamp_ms, volume));
        } else {
            self.volumes.insert(insert_pos, (timestamp_ms, volume));
        }

        log::debug!(
            "VolumeRing: inserted volume at {}, now have {} volumes",
            timestamp_ms,
            self.volumes.len()
        );
    }

    /// Get the volume at the specified timestamp, if it exists.
    #[allow(dead_code)]
    pub fn get(&self, timestamp_ms: i64) -> Option<&Volume> {
        self.volumes
            .iter()
            .find(|(ts, _)| *ts == timestamp_ms)
            .map(|(_, v)| v)
    }

    /// Get the most recent volume (highest timestamp).
    #[allow(dead_code)]
    pub fn most_recent(&self) -> Option<&Volume> {
        self.volumes.back().map(|(_, v)| v)
    }

    /// Get the most recent volume's timestamp.
    #[allow(dead_code)]
    pub fn most_recent_timestamp(&self) -> Option<i64> {
        self.volumes.back().map(|(ts, _)| *ts)
    }

    /// Returns an iterator over all volumes in chronological order (oldest first).
    pub fn volumes(&self) -> impl Iterator<Item = (i64, &Volume)> {
        self.volumes.iter().map(|(ts, v)| (*ts, v))
    }

    /// Returns the number of volumes currently stored.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.volumes.len()
    }

    /// Returns true if no volumes are stored.
    pub fn is_empty(&self) -> bool {
        self.volumes.is_empty()
    }

    /// Clear all stored volumes.
    pub fn clear(&mut self) {
        log::debug!("VolumeRing: clearing {} volumes", self.volumes.len());
        self.volumes.clear();
    }

    /// Returns all timestamps in the ring (oldest to newest).
    #[allow(dead_code)]
    pub fn timestamps(&self) -> Vec<i64> {
        self.volumes.iter().map(|(ts, _)| *ts).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests would require mock Volume objects
    // For now, the implementation is tested through integration
}
