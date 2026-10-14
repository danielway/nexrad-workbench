//! The stored storm-activity summary of one radar site's UTC day.
//!
//! Built from that day's archive listing: how many volume files the radar
//! produced and their mean compressed size. Bigger volumes hold more echo, so
//! the mean tracks how stormy the day was (see `core::storm_activity` for how
//! it becomes a bar height). One small record per site-day, kept in its own
//! IndexedDB store so a day is listed once, not on every visit.

use crate::data::keys::SiteId;
use serde::{Deserialize, Serialize};

/// One site-day's archive listing, reduced to what the overview's activity
/// bars need.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayActivity {
    pub site: SiteId,
    /// UTC midnight that starts the day (Unix seconds).
    pub day_start: i64,
    /// Volume files listed for the day. 0 means the radar has no data that
    /// day (an outage), which is worth showing.
    pub volumes: u32,
    /// Mean compressed size of those volumes, in bytes.
    pub mean_bytes: u64,
    /// The volumes' format suffix (`"V06"`, `"V03"`; empty for the
    /// unsuffixed pre-2008 files). Sizes are only comparable within one
    /// format, since super-resolution and dual-pol each made files bigger.
    pub format: String,
    /// When the listing was fetched (Unix ms), to tell a finished day from
    /// one that was still filling in.
    pub fetched_at_ms: i64,
}

impl DayActivity {
    /// The record's IndexedDB key, `"SITE|day_start"`. Shares the
    /// `"SITE|"` prefix scheme, so [`SiteId::idb_prefix_bounds`] reads one
    /// site's days.
    pub fn storage_key(&self) -> String {
        format!("{}|{}", self.site.0, self.day_start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn storage_key_falls_inside_the_site_prefix_range() {
        let day = DayActivity {
            site: SiteId::new("KTBW"),
            day_start: 1_728_432_000,
            volumes: 216,
            mean_bytes: 21_900_000,
            format: "V06".into(),
            fetched_at_ms: 0,
        };
        let key = day.storage_key();
        assert_eq!(key, "KTBW|1728432000");
        let (lower, upper) = day.site.idb_prefix_bounds();
        assert!(key >= lower && key < upper);
        let (other_lower, other_upper) = SiteId::new("KTB").idb_prefix_bounds();
        assert!(!(key >= other_lower && key < other_upper));
    }
}
