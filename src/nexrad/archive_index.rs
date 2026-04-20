//! Cache for NEXRAD archive file listings.
//!
//! Stores archive file metadata by site/date to avoid repeated AWS listing requests.
//! Listings for today's date are not cached since new files may still be added.

use chrono::NaiveDate;
use std::collections::HashMap;

/// Metadata for a single archive file (lightweight, no actual data).
#[derive(Debug, Clone)]
pub struct ArchiveFileMeta {
    /// File name (e.g., "KDMX20240501_000000_V06")
    pub name: String,
    /// File size in bytes (may be 0 if not available from listing).
    #[allow(dead_code)] // Populated from listing but not yet displayed in UI
    pub size: u64,
    /// Timestamp extracted from filename (Unix seconds)
    pub timestamp: i64,
}

impl ArchiveFileMeta {
    /// Parse timestamp from NEXRAD filename format: SITE_YYYYMMDD_HHMMSS_V0X
    pub fn parse_timestamp_from_name(name: &str, date: &NaiveDate) -> Option<i64> {
        // Format: KDMX20240501_120000_V06
        // The timestamp part is after the site ID (4 chars) and date (8 chars)
        if name.len() < 19 {
            return None;
        }

        // Extract HHMMSS from position 13-19 (after SITE + YYYYMMDD + _)
        let time_part = name.get(13..19)?;
        let hour: u32 = time_part.get(0..2)?.parse().ok()?;
        let minute: u32 = time_part.get(2..4)?.parse().ok()?;
        let second: u32 = time_part.get(4..6)?.parse().ok()?;

        let datetime = date.and_hms_opt(hour, minute, second)?;
        Some(datetime.and_utc().timestamp())
    }
}

/// Key for archive index entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArchiveIndexKey {
    pub site_id: String,
    pub date: NaiveDate,
}

impl ArchiveIndexKey {
    pub fn new(site_id: impl Into<String>, date: NaiveDate) -> Self {
        Self {
            site_id: site_id.into(),
            date,
        }
    }
}

/// Cached archive listing for a site/date.
#[derive(Debug, Clone)]
pub struct ArchiveListing {
    /// Files available in the archive, sorted by timestamp
    pub files: Vec<ArchiveFileMeta>,
    /// When this listing was fetched (for potential TTL)
    #[allow(dead_code)]
    pub fetched_at: f64,
}

/// A scan's time boundaries derived from adjacent file timestamps in a listing.
#[derive(Debug, Clone, Copy)]
pub struct ScanBoundary {
    /// Start of this scan (Unix seconds).
    pub start: i64,
    /// End of this scan (next scan's start, or estimated for last scan; Unix seconds).
    pub end: i64,
}

impl ArchiveListing {
    /// Compute scan time boundaries from adjacent file start times.
    ///
    /// Each scan starts at its own timestamp and ends at the next scan's
    /// timestamp. The last scan's duration is estimated from the average
    /// interval, or 300s if there's only one file.
    pub fn scan_boundaries(&self) -> Vec<ScanBoundary> {
        let n = self.files.len();
        if n == 0 {
            return Vec::new();
        }
        let mut boundaries = Vec::with_capacity(n);
        for i in 0..n {
            let start = self.files[i].timestamp;
            let end = if i + 1 < n {
                self.files[i + 1].timestamp
            } else if n > 1 {
                let total_span = self.files[n - 1].timestamp - self.files[0].timestamp;
                let avg_interval = total_span / (n as i64 - 1);
                start + avg_interval
            } else {
                start + 300
            };
            boundaries.push(ScanBoundary { start, end });
        }
        boundaries
    }

    /// Find all scans whose time span `[start, end)` intersects `[range_start, range_end]`.
    pub fn scans_intersecting(
        &self,
        range_start: i64,
        range_end: i64,
    ) -> Vec<(&ArchiveFileMeta, ScanBoundary)> {
        let boundaries = self.scan_boundaries();
        self.files
            .iter()
            .zip(boundaries.iter())
            .filter(|(_, b)| b.start < range_end && b.end > range_start)
            .map(|(file, b)| (file, *b))
            .collect()
    }

    /// Find the single scan containing the given timestamp.
    pub fn find_scan_containing(&self, timestamp: i64) -> Option<(&ArchiveFileMeta, ScanBoundary)> {
        let boundaries = self.scan_boundaries();
        self.files
            .iter()
            .zip(boundaries.iter())
            .find(|(_, b)| timestamp >= b.start && timestamp < b.end)
            .map(|(file, b)| (file, *b))
    }

    /// Find the file containing or closest to the given timestamp.
    #[allow(dead_code)]
    pub fn find_file_at_timestamp(&self, timestamp: i64) -> Option<&ArchiveFileMeta> {
        if self.files.is_empty() {
            return None;
        }

        // Binary search for the closest file
        match self.files.binary_search_by_key(&timestamp, |f| f.timestamp) {
            Ok(idx) => Some(&self.files[idx]),
            Err(idx) => {
                // idx is where it would be inserted
                if idx == 0 {
                    Some(&self.files[0])
                } else if idx >= self.files.len() {
                    Some(&self.files[self.files.len() - 1])
                } else {
                    // Pick the closer one
                    let before = &self.files[idx - 1];
                    let after = &self.files[idx];
                    if (timestamp - before.timestamp).abs() <= (after.timestamp - timestamp).abs() {
                        Some(before)
                    } else {
                        Some(after)
                    }
                }
            }
        }
    }

    /// Find the file after the given timestamp.
    #[allow(dead_code)]
    pub fn find_next_file_after(&self, timestamp: i64) -> Option<&ArchiveFileMeta> {
        self.files.iter().find(|f| f.timestamp > timestamp)
    }

    /// Find the file by name.
    #[allow(dead_code)]
    pub fn find_file_by_name(&self, name: &str) -> Option<&ArchiveFileMeta> {
        self.files.iter().find(|f| f.name == name)
    }
}

/// In-memory cache for archive listings.
///
/// Caches all listings for the current session. Today's listings are stored
/// in memory but may become stale as new files are added to the archive.
#[derive(Default)]
pub struct ArchiveIndex {
    listings: HashMap<ArchiveIndexKey, ArchiveListing>,
}

impl ArchiveIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if we have a cached listing for this site/date.
    pub fn get(&self, site_id: &str, date: &NaiveDate) -> Option<&ArchiveListing> {
        let key = ArchiveIndexKey::new(site_id, *date);
        self.listings.get(&key)
    }

    /// Store a listing in the cache.
    ///
    /// Today's listings are cached in memory for the current session.
    /// They may become stale as new files are added, but avoid repeated
    /// API calls during the same download operation.
    pub fn insert(&mut self, site_id: &str, date: NaiveDate, listing: ArchiveListing) {
        let today = chrono::Utc::now().date_naive();
        let is_today = date == today;

        let key = ArchiveIndexKey::new(site_id, date);
        self.listings.insert(key, listing);

        if is_today {
            log::debug!(
                "Cached archive listing for today's date: {}/{} (may become stale)",
                site_id,
                date
            );
        } else {
            log::debug!("Cached archive listing for {}/{}", site_id, date);
        }
    }

    /// Check if a listing is cached (and valid) for this site/date.
    #[allow(dead_code)]
    pub fn has_listing(&self, site_id: &str, date: &NaiveDate) -> bool {
        self.get(site_id, date).is_some()
    }

    /// Remove a specific cached listing (e.g. to force a re-fetch).
    pub fn remove(&mut self, site_id: &str, date: &NaiveDate) {
        let key = ArchiveIndexKey::new(site_id, *date);
        self.listings.remove(&key);
    }

    /// Clear all cached listings.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.listings.clear();
    }

    /// Collect scan boundaries from all cached listings for a given site.
    ///
    /// Returns boundaries sorted by start time with duplicates removed.
    pub fn all_boundaries_for_site(&self, site_id: &str) -> Vec<ScanBoundary> {
        let mut boundaries: Vec<ScanBoundary> = self
            .listings
            .iter()
            .filter(|(key, _)| key.site_id == site_id)
            .flat_map(|(_, listing)| listing.scan_boundaries())
            .collect();
        boundaries.sort_by_key(|a| a.start);
        boundaries.dedup_by(|a, b| a.start == b.start && a.end == b.end);
        boundaries
    }
}

/// Get current timestamp in seconds.
pub fn current_timestamp_secs() -> f64 {
    js_sys::Date::now() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn file(name: &str, timestamp: i64) -> ArchiveFileMeta {
        ArchiveFileMeta {
            name: name.to_string(),
            size: 0,
            timestamp,
        }
    }

    fn listing(files: Vec<ArchiveFileMeta>) -> ArchiveListing {
        ArchiveListing {
            files,
            fetched_at: 0.0,
        }
    }

    // --- parse_timestamp_from_name ---

    #[test]
    fn parse_timestamp_basic() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let ts = ArchiveFileMeta::parse_timestamp_from_name("KDMX20240501_120000_V06", &date);
        assert!(ts.is_some());
        let expected = date.and_hms_opt(12, 0, 0).unwrap().and_utc().timestamp();
        assert_eq!(ts.unwrap(), expected);
    }

    #[test]
    fn parse_timestamp_midnight() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let ts = ArchiveFileMeta::parse_timestamp_from_name("KDMX20240501_000000_V06", &date);
        let expected = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        assert_eq!(ts.unwrap(), expected);
    }

    #[test]
    fn parse_timestamp_too_short() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        assert!(ArchiveFileMeta::parse_timestamp_from_name("short", &date).is_none());
    }

    #[test]
    fn parse_timestamp_invalid_time() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        // 25 is not a valid hour
        assert!(
            ArchiveFileMeta::parse_timestamp_from_name("KDMX20240501_250000_V06", &date).is_none()
        );
    }

    // --- scan_boundaries ---

    #[test]
    fn scan_boundaries_empty() {
        let l = listing(vec![]);
        assert!(l.scan_boundaries().is_empty());
    }

    #[test]
    fn scan_boundaries_single_file() {
        let l = listing(vec![file("a", 1000)]);
        let b = l.scan_boundaries();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].start, 1000);
        assert_eq!(b[0].end, 1300); // 1000 + 300
    }

    #[test]
    fn scan_boundaries_multiple_files() {
        let l = listing(vec![file("a", 1000), file("b", 1300), file("c", 1600)]);
        let b = l.scan_boundaries();
        assert_eq!(b.len(), 3);
        // First two end at next scan's start
        assert_eq!(b[0].start, 1000);
        assert_eq!(b[0].end, 1300);
        assert_eq!(b[1].start, 1300);
        assert_eq!(b[1].end, 1600);
        // Last uses average interval (300s)
        assert_eq!(b[2].start, 1600);
        assert_eq!(b[2].end, 1900);
    }

    // --- find_scan_containing ---

    #[test]
    fn find_scan_containing_found() {
        let l = listing(vec![file("a", 1000), file("b", 1300)]);
        let result = l.find_scan_containing(1150);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0.name, "a");
    }

    #[test]
    fn find_scan_containing_not_found() {
        let l = listing(vec![file("a", 1000), file("b", 1300)]);
        assert!(l.find_scan_containing(500).is_none());
    }

    // --- scans_intersecting ---

    #[test]
    fn scans_intersecting_range() {
        let l = listing(vec![file("a", 1000), file("b", 1300), file("c", 1600)]);
        let result = l.scans_intersecting(1200, 1400);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0.name, "a");
        assert_eq!(result[1].0.name, "b");
    }

    // --- find_file_at_timestamp ---

    #[test]
    fn find_file_at_timestamp_exact() {
        let l = listing(vec![file("a", 1000), file("b", 1300)]);
        assert_eq!(l.find_file_at_timestamp(1000).unwrap().name, "a");
        assert_eq!(l.find_file_at_timestamp(1300).unwrap().name, "b");
    }

    #[test]
    fn find_file_at_timestamp_between() {
        let l = listing(vec![file("a", 1000), file("b", 1300)]);
        // 1100 is closer to 1000
        assert_eq!(l.find_file_at_timestamp(1100).unwrap().name, "a");
        // 1200 is closer to 1300
        assert_eq!(l.find_file_at_timestamp(1200).unwrap().name, "b");
    }

    #[test]
    fn find_file_at_timestamp_empty() {
        let l = listing(vec![]);
        assert!(l.find_file_at_timestamp(1000).is_none());
    }

    // --- ArchiveIndex ---

    #[test]
    fn archive_index_get_and_remove() {
        let mut idx = ArchiveIndex::new();
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        assert!(idx.get("KDMX", &date).is_none());

        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", date),
            listing(vec![file("a", 1000)]),
        );

        assert!(idx.get("KDMX", &date).is_some());
        assert!(idx.has_listing("KDMX", &date));

        idx.remove("KDMX", &date);
        assert!(idx.get("KDMX", &date).is_none());
    }
}
