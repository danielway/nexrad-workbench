//! The set of sweeps we have cached locally, possibly sparse.
//!
//! Keyed by `(scan_start_secs rounded to whole seconds, elevation_number)` —
//! matching the whole-second `ScanKey` identity. Coverage within a scan may have
//! gaps (e.g. elevations 1, 4, 5, 9), so this is a set of present cuts, not a
//! dense array. Drives the `CollectedByUs` status and supplies observed spans
//! for the display view.

use crate::data::CachedSweep;
use std::collections::BTreeMap;

/// Round a scan-start time (Unix seconds) to the whole-second key used to group
/// cached cuts by scan — mirrors the `ScanKey` whole-second identity.
fn scan_key(scan_start_secs: f64) -> i64 {
    scan_start_secs.round() as i64
}

/// Locally-cached sweeps, keyed by `(scan, elevation)`; sparse within a scan.
#[derive(Clone, Debug, Default)]
pub struct CachedSweepSet {
    /// Observed (start, end) collection span per cached cut.
    spans: BTreeMap<(i64, u8), (f64, f64)>,
}

impl CachedSweepSet {
    /// Replace the cached cuts recorded for one scan. Cuts of other scans are
    /// untouched, so sparse per-scan coverage accumulates independently.
    pub fn set_for_scan(&mut self, scan_start_secs: f64, sweeps: &[CachedSweep]) {
        let key = scan_key(scan_start_secs);
        self.spans.retain(|(s, _), _| *s != key);
        for sweep in sweeps {
            self.spans
                .insert((key, sweep.elevation_number), (sweep.start, sweep.end));
        }
    }

    /// Whether a specific `(scan, elevation)` cut is cached locally.
    pub fn has(&self, scan_start_secs: f64, elevation_number: u8) -> bool {
        self.spans
            .contains_key(&(scan_key(scan_start_secs), elevation_number))
    }

    /// Observed (start, end) collection span for a cached cut, if present.
    #[allow(dead_code)] // Exercised by tests; utility accessor with no prod caller.
    pub fn span(&self, scan_start_secs: f64, elevation_number: u8) -> Option<(f64, f64)> {
        self.spans
            .get(&(scan_key(scan_start_secs), elevation_number))
            .copied()
    }

    /// Cached cuts for one scan, as `(elevation_number, start, end)`, ascending
    /// by elevation.
    #[allow(dead_code)] // Utility accessor; no caller yet.
    pub fn cuts_for_scan(&self, scan_start_secs: f64) -> Vec<(u8, f64, f64)> {
        let key = scan_key(scan_start_secs);
        self.spans
            .iter()
            .filter(|((s, _), _)| *s == key)
            .map(|((_, elev), (start, end))| (*elev, *start, *end))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep(elev: u8, start: f64, end: f64) -> CachedSweep {
        CachedSweep {
            start,
            end,
            elevation: elev as f32,
            elevation_number: elev,
            start_azimuth: 0.0,
            cached_products: vec![],
        }
    }

    #[wasm_bindgen_test]
    fn sparse_coverage_has_and_span() {
        let mut set = CachedSweepSet::default();
        // Sparse: elevations 1, 4, 5, 9 cached for one scan.
        set.set_for_scan(
            1000.0,
            &[
                sweep(1, 1000.0, 1010.0),
                sweep(4, 1030.0, 1040.0),
                sweep(5, 1040.0, 1050.0),
                sweep(9, 1090.0, 1100.0),
            ],
        );
        assert!(set.has(1000.0, 1));
        assert!(!set.has(1000.0, 2));
        assert!(set.has(1000.4, 4)); // sub-second scan start rounds to same key
        assert_eq!(set.span(1000.0, 5), Some((1040.0, 1050.0)));
        assert_eq!(set.cuts_for_scan(1000.0).len(), 4);
    }

    #[wasm_bindgen_test]
    fn set_for_scan_replaces_only_that_scan() {
        let mut set = CachedSweepSet::default();
        set.set_for_scan(1000.0, &[sweep(1, 1000.0, 1010.0)]);
        set.set_for_scan(2000.0, &[sweep(2, 2000.0, 2010.0)]);
        // Re-setting scan 1000 leaves scan 2000 intact.
        set.set_for_scan(1000.0, &[sweep(3, 1000.0, 1010.0)]);
        assert!(!set.has(1000.0, 1)); // replaced
        assert!(set.has(1000.0, 3));
        assert!(set.has(2000.0, 2)); // untouched
    }
}
