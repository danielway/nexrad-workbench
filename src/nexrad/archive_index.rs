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
    /// File size in bytes (may be 0 if not available from listing)
    #[allow(dead_code)]
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

impl ArchiveListing {
    /// Find the file containing or closest to the given timestamp.
    #[allow(dead_code)] // Utility method for future use
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
    #[allow(dead_code)] // Utility method for future use
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

    /// Clear all cached listings.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.listings.clear();
    }
}

/// Get current timestamp in seconds.
pub fn current_timestamp_secs() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() / 1000.0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}
