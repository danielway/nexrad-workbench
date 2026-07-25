//! Cache for NEXRAD archive file listings.
//!
//! Stores archive file metadata by site/date to avoid repeated AWS listing requests.
//! Listings for today's date are not cached since new files may still be added.

use chrono::NaiveDate;
use std::collections::HashMap;

/// Metadata for a single archive file (lightweight, no actual data).
#[derive(Debug, Clone)]
pub(crate) struct ArchiveFileMeta {
    /// File name (e.g., "KDMX20240501_000000_V06")
    pub name: String,
    /// Timestamp extracted from filename (Unix seconds)
    pub timestamp: i64,
}

impl ArchiveFileMeta {
    /// Parse timestamp from NEXRAD filename format: SITE_YYYYMMDD_HHMMSS_V0X
    pub(crate) fn parse_timestamp_from_name(name: &str, date: &NaiveDate) -> Option<i64> {
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
pub(crate) struct ArchiveIndexKey {
    pub site_id: String,
    pub date: NaiveDate,
}

impl ArchiveIndexKey {
    pub(crate) fn new(site_id: impl Into<String>, date: NaiveDate) -> Self {
        Self {
            site_id: site_id.into(),
            date,
        }
    }
}

/// Cached archive listing for a site/date.
#[derive(Debug, Clone)]
pub(crate) struct ArchiveListing {
    /// Files available in the archive, sorted by timestamp
    pub files: Vec<ArchiveFileMeta>,
    /// When this listing was fetched (for potential TTL)
    pub fetched_at: f64,
}

/// A scan's time boundaries derived from adjacent file timestamps in a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScanBoundary {
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
    pub(crate) fn scan_boundaries(&self) -> Vec<ScanBoundary> {
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
    pub(crate) fn scans_intersecting(
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

    /// The most recent scan that starts at or before `timestamp`.
    ///
    /// This is the scan a playback cursor renders even when it sits in the
    /// dead-time after a scan's last sweep or in a gap before the next scan —
    /// matching `find_recent_scan`'s "most recent started" semantics on the
    /// render side. `find_scan_containing` only matches when the cursor is
    /// *within* a scan's span; this also covers the after-the-end case.
    pub(crate) fn scan_at_or_before(
        &self,
        timestamp: i64,
    ) -> Option<(&ArchiveFileMeta, ScanBoundary)> {
        let boundaries = self.scan_boundaries();
        self.files
            .iter()
            .zip(boundaries.iter())
            .filter(|(_, b)| b.start <= timestamp)
            .max_by_key(|(_, b)| b.start)
            .map(|(file, b)| (file, *b))
    }
}

/// How long today's listing stays fresh (seconds). The archive only grows at
/// the live edge, so non-today listings never expire; today's is re-listed on
/// this cadence so the shadow track keeps growing near "now".
pub(crate) const TODAY_LISTING_TTL_SECS: f64 = 120.0;

/// In-memory cache for archive listings.
///
/// Caches all listings for the current session. Today's listings are stored
/// in memory but may become stale as new files are added to the archive.
#[derive(Default)]
pub(crate) struct ArchiveIndex {
    listings: HashMap<ArchiveIndexKey, ArchiveListing>,
}

impl ArchiveIndex {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Check if we have a cached listing for this site/date.
    pub(crate) fn get(&self, site_id: &str, date: &NaiveDate) -> Option<&ArchiveListing> {
        let key = ArchiveIndexKey::new(site_id, *date);
        self.listings.get(&key)
    }

    /// Whether a *fresh* listing exists: any cached listing for a past date,
    /// or one younger than [`TODAY_LISTING_TTL_SECS`] when `date == today`.
    /// Stale-but-present listings are still served by [`Self::get`] (better
    /// stale shadows than none); this only tells the listing pump whether a
    /// refresh is worthwhile.
    pub(crate) fn has_fresh(
        &self,
        site_id: &str,
        date: &NaiveDate,
        now_secs: f64,
        today: NaiveDate,
    ) -> bool {
        match self.get(site_id, date) {
            None => false,
            Some(listing) => {
                *date != today || now_secs - listing.fetched_at < TODAY_LISTING_TTL_SECS
            }
        }
    }

    /// Store a listing in the cache.
    ///
    /// Today's listings are cached in memory for the current session.
    /// They may become stale as new files are added, but avoid repeated
    /// API calls during the same download operation.
    pub(crate) fn insert(&mut self, site_id: &str, date: NaiveDate, listing: ArchiveListing) {
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

    /// Collect scan boundaries from all cached listings for a given site.
    ///
    /// Returns boundaries sorted by start time with duplicates removed.
    pub(crate) fn all_boundaries_for_site(&self, site_id: &str) -> Vec<ScanBoundary> {
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
pub(crate) fn current_timestamp_secs() -> f64 {
    js_sys::Date::now() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn file(name: &str, timestamp: i64) -> ArchiveFileMeta {
        ArchiveFileMeta {
            name: name.to_string(),
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

    // --- scan_at_or_before (wasm_bindgen_test so it actually executes under
    // the node harness, unlike the plain #[test] cases above) ---

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn scan_at_or_before_within_scan() {
        let l = listing(vec![file("a", 1000), file("b", 1300)]);
        // Cursor inside scan a's span → a.
        assert_eq!(l.scan_at_or_before(1150).unwrap().0.name, "a");
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn scan_at_or_before_in_dead_time_picks_most_recent() {
        let l = listing(vec![file("a", 1000), file("b", 1300)]);
        // Cursor at/after b's start (even past its computed end) → b, the most
        // recent scan that has started — matching the render-side staleness rule.
        assert_eq!(l.scan_at_or_before(1500).unwrap().0.name, "b");
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn scan_at_or_before_none_before_first() {
        let l = listing(vec![file("a", 1000), file("b", 1300)]);
        assert!(l.scan_at_or_before(500).is_none());
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

    /// Half-open boundary inclusivity: the filter is `b.start < range_end &&
    /// b.end > range_start`. A range whose `end` equals a scan's `start` excludes
    /// that scan; a range whose `start` equals a scan's `end` excludes the
    /// earlier scan.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn scans_intersecting_half_open_boundaries() {
        // Boundaries: a [1000,1300), b [1300,1600), c [1600,1900).
        let l = listing(vec![file("a", 1000), file("b", 1300), file("c", 1600)]);

        // range_end == b.start (1300): b is excluded (1300 < 1300 is false); only
        // a intersects.
        let r = l.scans_intersecting(1100, 1300);
        let names: Vec<&str> = r.iter().map(|(f, _)| f.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);

        // range_start == a.end (1300 == b.start, but a.end is also 1300): a is
        // excluded (a.end 1300 > 1300 is false); b is the first match.
        let r = l.scans_intersecting(1300, 1500);
        let names: Vec<&str> = r.iter().map(|(f, _)| f.name.as_str()).collect();
        assert_eq!(names, vec!["b"]);

        // A range fully inside a single scan returns just that scan.
        let r = l.scans_intersecting(1350, 1400);
        let names: Vec<&str> = r.iter().map(|(f, _)| f.name.as_str()).collect();
        assert_eq!(names, vec!["b"]);
    }

    /// The single-file zero-margin case: one file → boundary [start, start+300).
    /// A range touching the synthetic end is excluded; a range inside is matched.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn scans_intersecting_single_file_estimated_end() {
        let l = listing(vec![file("a", 1000)]); // [1000, 1300)
        assert_eq!(l.scans_intersecting(1000, 1300).len(), 1); // overlaps start
        assert_eq!(l.scans_intersecting(1300, 1400).len(), 0); // range_start==end
        assert_eq!(l.scans_intersecting(500, 900).len(), 0); // entirely before
    }

    // --- all_boundaries_for_site ---

    /// Merges every cached listing for one site, sorts ascending by start,
    /// dedups identical (start,end) pairs, and excludes other sites' listings.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn all_boundaries_merges_sorts_dedups_and_filters_site() {
        let mut idx = ArchiveIndex::new();
        let day1 = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let day2 = NaiveDate::from_ymd_opt(2024, 5, 2).unwrap();

        // Two KDMX listings on different dates, inserted out of start order.
        // Listing on day2 starts later (3000+) than day1 (1000+).
        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", day2),
            listing(vec![file("c", 3000), file("d", 3300)]),
        );
        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", day1),
            // 'a' shares its (start,end) with 'b' below to exercise dedup:
            // both [1000,1300).
            listing(vec![file("a", 1000), file("b", 1300)]),
        );
        // A second day1-equivalent listing under a DIFFERENT key would overwrite
        // in the map; instead add an overlapping duplicate within KDMX by reusing
        // the same boundaries via another date that yields the same (start,end).
        // Simpler: another site's listing must be excluded entirely.
        idx.listings.insert(
            ArchiveIndexKey::new("KABR", day1),
            listing(vec![file("x", 5000), file("y", 5300)]),
        );

        let b = idx.all_boundaries_for_site("KDMX");
        // KDMX boundaries only (KABR's 5000/5300 excluded), sorted ascending.
        let starts: Vec<i64> = b.iter().map(|sb| sb.start).collect();
        assert_eq!(starts, vec![1000, 1300, 3000, 3300]);
        // None of KABR's boundaries leaked in.
        assert!(b.iter().all(|sb| sb.start != 5000 && sb.start != 5300));
    }

    /// Identical (start,end) boundaries appearing across listings collapse to a
    /// single entry after the ascending sort + dedup.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn all_boundaries_dedups_identical_pairs() {
        let mut idx = ArchiveIndex::new();
        let day1 = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let day2 = NaiveDate::from_ymd_opt(2024, 5, 2).unwrap();
        // Two listings that each yield the exact same two-file boundaries
        // [1000,1300) and [1300,1600).
        let same = || listing(vec![file("a", 1000), file("b", 1300), file("c", 1600)]);
        idx.listings
            .insert(ArchiveIndexKey::new("KDMX", day1), same());
        idx.listings
            .insert(ArchiveIndexKey::new("KDMX", day2), same());

        let b = idx.all_boundaries_for_site("KDMX");
        // Without dedup there would be 6 entries; identical (start,end) pairs
        // collapse → 3 unique boundaries.
        let pairs: Vec<(i64, i64)> = b.iter().map(|sb| (sb.start, sb.end)).collect();
        assert_eq!(pairs, vec![(1000, 1300), (1300, 1600), (1600, 1900)]);
    }

    // --- ArchiveIndex ---

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn has_fresh_ttl_only_applies_to_today() {
        let mut idx = ArchiveIndex::new();
        let today = NaiveDate::from_ymd_opt(2026, 6, 11).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let now = 10_000.0;

        // Nothing cached → never fresh.
        assert!(!idx.has_fresh("KDMX", &yesterday, now, today));

        // A past date is fresh forever once cached, regardless of age.
        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", yesterday),
            ArchiveListing {
                files: vec![file("a", 1000)],
                fetched_at: 0.0,
            },
        );
        assert!(idx.has_fresh("KDMX", &yesterday, now, today));

        // Today's listing expires after the TTL.
        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", today),
            ArchiveListing {
                files: vec![file("b", 2000)],
                fetched_at: now - TODAY_LISTING_TTL_SECS - 1.0,
            },
        );
        assert!(!idx.has_fresh("KDMX", &today, now, today));
        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", today),
            ArchiveListing {
                files: vec![file("b", 2000)],
                fetched_at: now - 1.0,
            },
        );
        assert!(idx.has_fresh("KDMX", &today, now, today));
    }

    #[test]
    fn archive_index_get() {
        let mut idx = ArchiveIndex::new();
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        assert!(idx.get("KDMX", &date).is_none());

        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", date),
            listing(vec![file("a", 1000)]),
        );

        assert!(idx.get("KDMX", &date).is_some());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use chrono::NaiveDate;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn file(name: &str, timestamp: i64) -> ArchiveFileMeta {
        ArchiveFileMeta {
            name: name.to_string(),
            timestamp,
        }
    }

    fn listing(files: Vec<ArchiveFileMeta>) -> ArchiveListing {
        ArchiveListing {
            files,
            fetched_at: 0.0,
        }
    }

    // --- parse_timestamp_from_name: untested error/edge branches ---

    /// Minute 60 is out of range for `and_hms_opt`, so the chrono construction
    /// returns None even though the substrings parse as integers.
    #[wasm_bindgen_test]
    fn parse_timestamp_invalid_minute() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        assert!(
            ArchiveFileMeta::parse_timestamp_from_name("KDMX20240501_126000_V06", &date).is_none()
        );
    }

    /// Second 60 is likewise rejected by `and_hms_opt` (no leap-second slot here).
    #[wasm_bindgen_test]
    fn parse_timestamp_invalid_second() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        assert!(
            ArchiveFileMeta::parse_timestamp_from_name("KDMX20240501_120060_V06", &date).is_none()
        );
    }

    /// Non-numeric characters in the HHMMSS window fail the `.parse().ok()?`.
    #[wasm_bindgen_test]
    fn parse_timestamp_non_numeric_time() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        assert!(
            ArchiveFileMeta::parse_timestamp_from_name("KDMX20240501_12ab00_V06", &date).is_none()
        );
    }

    /// Exactly 18 chars: `name.len() < 19` is true → early None. (One short of
    /// the minimum the existing "short" test never pins precisely.)
    #[wasm_bindgen_test]
    fn parse_timestamp_length_boundary_18_is_none() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        // 18 chars: "KDMX20240501_12000" (site8 + _ + 5 time digits = 18)
        let name = "KDMX20240501_12000";
        assert_eq!(name.len(), 18);
        assert!(ArchiveFileMeta::parse_timestamp_from_name(name, &date).is_none());
    }

    /// Exactly 19 chars passes the length gate and the slice 13..19 reads the
    /// full HHMMSS even with no trailing `_V06`. "KDMX20240501_010203" → 01:02:03.
    #[wasm_bindgen_test]
    fn parse_timestamp_length_boundary_19_parses() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let name = "KDMX20240501_010203";
        assert_eq!(name.len(), 19);
        let ts = ArchiveFileMeta::parse_timestamp_from_name(name, &date).unwrap();
        let expected = date.and_hms_opt(1, 2, 3).unwrap().and_utc().timestamp();
        assert_eq!(ts, expected);
    }

    /// The hour/minute/second window is at fixed offset 13..19; a longer site
    /// prefix is NOT special-cased, so the bytes there are interpreted as time.
    /// Using a pre-epoch date yields a negative Unix timestamp.
    #[wasm_bindgen_test]
    fn parse_timestamp_pre_epoch_negative() {
        let date = NaiveDate::from_ymd_opt(1969, 12, 31).unwrap();
        let ts =
            ArchiveFileMeta::parse_timestamp_from_name("KDMX19691231_235959_V06", &date).unwrap();
        // One second before the epoch.
        assert_eq!(ts, -1);
    }

    // --- ArchiveIndexKey equality / hashing ---

    /// `new` accepts any Into<String>; keys are equal iff both site and date
    /// match. Used as the HashMap key, so this equality is load-bearing.
    #[wasm_bindgen_test]
    fn index_key_equality() {
        let d1 = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2024, 5, 2).unwrap();
        let a = ArchiveIndexKey::new("KDMX", d1);
        let b = ArchiveIndexKey::new(String::from("KDMX"), d1);
        let c = ArchiveIndexKey::new("KDMX", d2);
        let e = ArchiveIndexKey::new("KABR", d1);
        assert_eq!(a, b);
        assert_ne!(a, c); // different date
        assert_ne!(a, e); // different site
    }

    // --- scan_boundaries: average-interval branch with non-uniform gaps ---

    /// With >2 files and unequal gaps, the LAST scan's end uses the AVERAGE
    /// interval, not the final gap. Files at 1000, 1100, 1900 → total span 900
    /// over 2 intervals → avg 450 → last end = 1900 + 450 = 2350.
    #[wasm_bindgen_test]
    fn scan_boundaries_last_uses_average_not_final_gap() {
        let l = listing(vec![file("a", 1000), file("b", 1100), file("c", 1900)]);
        let b = l.scan_boundaries();
        assert_eq!(b.len(), 3);
        assert_eq!((b[0].start, b[0].end), (1000, 1100));
        assert_eq!((b[1].start, b[1].end), (1100, 1900));
        // avg = (1900 - 1000) / 2 = 450
        assert_eq!((b[2].start, b[2].end), (1900, 2350));
    }

    /// Two-file case: average interval equals the single gap, so the last end
    /// is one more interval out. 2000, 2500 → gap 500 → last end 3000.
    #[wasm_bindgen_test]
    fn scan_boundaries_two_files_average_equals_gap() {
        let l = listing(vec![file("a", 2000), file("b", 2500)]);
        let b = l.scan_boundaries();
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].start, b[0].end), (2000, 2500));
        assert_eq!((b[1].start, b[1].end), (2500, 3000));
    }

    // --- scans_intersecting / scan_at_or_before: empty + extra branches ---

    #[wasm_bindgen_test]
    fn scans_intersecting_empty_listing() {
        let l = listing(vec![]);
        assert!(l.scans_intersecting(0, 10_000).is_empty());
    }

    #[wasm_bindgen_test]
    fn scan_at_or_before_empty_listing_is_none() {
        let l = listing(vec![]);
        assert!(l.scan_at_or_before(1000).is_none());
    }

    /// Filter is `b.start <= timestamp` (inclusive); a cursor exactly on a
    /// scan's start selects that scan, not the prior one.
    #[wasm_bindgen_test]
    fn scan_at_or_before_exact_start_is_inclusive() {
        let l = listing(vec![file("a", 1000), file("b", 1300)]);
        assert_eq!(l.scan_at_or_before(1300).unwrap().0.name, "b");
        assert_eq!(l.scan_at_or_before(1000).unwrap().0.name, "a");
    }

    /// Well past every start → the latest-started scan (max_by_key on start).
    #[wasm_bindgen_test]
    fn scan_at_or_before_past_all_picks_last() {
        let l = listing(vec![file("a", 1000), file("b", 1300), file("c", 1600)]);
        assert_eq!(l.scan_at_or_before(999_999).unwrap().0.name, "c");
    }

    // --- has_fresh: exact-TTL boundary + missing-site path ---

    /// The freshness test is strict `<`: at exactly the TTL the listing is NOT
    /// fresh (the `has_fresh_ttl_only_applies_to_today` case only probes
    /// TTL-1 and TTL+1, never the exact edge).
    #[wasm_bindgen_test]
    fn has_fresh_exact_ttl_edge_is_not_fresh() {
        let mut idx = ArchiveIndex::new();
        let today = NaiveDate::from_ymd_opt(2026, 6, 11).unwrap();
        let now = 10_000.0;
        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", today),
            ArchiveListing {
                files: vec![file("b", 2000)],
                fetched_at: now - TODAY_LISTING_TTL_SECS, // age == TTL exactly
            },
        );
        // now - fetched_at == TTL, and TTL < TTL is false → stale.
        assert!(!idx.has_fresh("KDMX", &today, now, today));
    }

    /// A cached listing for one site does not make a different site "fresh"
    /// (key includes site_id → cache miss → false).
    #[wasm_bindgen_test]
    fn has_fresh_other_site_is_false() {
        let mut idx = ArchiveIndex::new();
        let day = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 6, 11).unwrap();
        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", day),
            listing(vec![file("a", 1000)]),
        );
        assert!(!idx.has_fresh("KABR", &day, 10_000.0, today));
    }

    // --- ArchiveIndex::get / all_boundaries_for_site: miss paths ---

    /// `get` keys on both site and date: a matching site but wrong date misses,
    /// and a matching date but wrong site misses.
    #[wasm_bindgen_test]
    fn get_misses_on_wrong_date_or_site() {
        let mut idx = ArchiveIndex::new();
        let day = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let other_day = NaiveDate::from_ymd_opt(2024, 5, 2).unwrap();
        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", day),
            listing(vec![file("a", 1000)]),
        );
        assert!(idx.get("KDMX", &day).is_some());
        assert!(idx.get("KDMX", &other_day).is_none()); // wrong date
        assert!(idx.get("KABR", &day).is_none()); // wrong site
    }

    /// No listings cached for the requested site → empty boundary set (covers
    /// both the empty-index and unknown-site filter outcomes).
    #[wasm_bindgen_test]
    fn all_boundaries_unknown_site_is_empty() {
        let mut idx = ArchiveIndex::new();
        // Empty index first.
        assert!(idx.all_boundaries_for_site("KDMX").is_empty());

        let day = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        idx.listings.insert(
            ArchiveIndexKey::new("KDMX", day),
            listing(vec![file("a", 1000), file("b", 1300)]),
        );
        // Querying a different site yields nothing.
        assert!(idx.all_boundaries_for_site("KABR").is_empty());
        // Sanity: the known site does return boundaries.
        assert_eq!(idx.all_boundaries_for_site("KDMX").len(), 2);
    }
}
