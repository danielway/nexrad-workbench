//! Live-volume anchor state machine.
//!
//! In-flight identity + timing for the current real-time volume: the
//! provisional (availability-derived) start estimate and its transition to
//! the confirmed (radial-parsed) start.

use crate::data::keys::ScanKey;

/// ACTUAL category: Unix-seconds volume collection start time, parsed from
/// the volume header's first-radial collection time. Authoritative — only
/// known once the worker has decoded at least one radial of the volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfirmedStart(pub f64);

/// AVAILABILITY-derived category: Unix-seconds volume start estimate
/// computed as `upload_date_time − median_availability_lag` from the Start
/// chunk's S3 upload time. Lands within ~1s of the eventual confirmed value;
/// used as the IDB key (chunks have to be written before any radial parse)
/// and as the UI's display value until [`ConfirmedStart`] arrives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProvisionalStart(pub f64);

/// In-flight identity + timing for the current live volume.
///
/// Replaces the parallel `current_scan_key: String` and
/// `current_volume_start: f64` fields that previously lived on
/// `LiveModeState`. Holding both timestamp variants in one place makes the
/// provisional → confirmed transition explicit — UI surfaces read
/// [`Self::best_start_secs`] (which swaps to confirmed atomically when the
/// worker reports the header time) and IDB code reads [`Self::scan_key`]
/// (always derived from the provisional value, since that's what was
/// written at first-chunk arrival).
///
/// `scan_key` doubles as the volume's stable identity for the lifetime of
/// the volume: it's built once when the Start chunk is processed and never
/// updated, so consumers can match on it without worrying about the
/// timestamp underneath shifting. A future revision may switch identity to
/// a synthetic `(site, NEXRAD volume_index)` pair without changing this
/// type's surface.
#[derive(Debug, Clone)]
pub struct LiveVolumeAnchor {
    pub scan_key: ScanKey,
    pub provisional: ProvisionalStart,
    pub confirmed: Option<ConfirmedStart>,
}

impl LiveVolumeAnchor {
    /// Construct an anchor for a freshly-arrived volume. The `scan_key`
    /// must already encode the provisional start (it's the IDB key the
    /// streaming loop has been using all along — see
    /// `realtime.rs::provisional_scan_start_secs`).
    pub fn new(scan_key: ScanKey, provisional: ProvisionalStart) -> Self {
        Self {
            scan_key,
            provisional,
            confirmed: None,
        }
    }

    /// Best-known volume start (Unix seconds). Confirmed when available,
    /// otherwise the provisional estimate. Every UI surface should use this
    /// for visual placement and timestamp comparisons.
    pub fn best_start_secs(&self) -> f64 {
        self.confirmed.map(|c| c.0).unwrap_or(self.provisional.0)
    }

    /// Record the confirmed (radial-parsed) start time. Idempotent.
    pub fn confirm(&mut self, confirmed: ConfirmedStart) {
        self.confirmed = Some(confirmed);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── LiveVolumeAnchor ─────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn live_anchor_new_uses_provisional_until_confirmed() {
        let scan = ScanKey::from_secs("KDMX", 1_700_000_000);
        let anchor = LiveVolumeAnchor::new(scan.clone(), ProvisionalStart(1_700_000_001.25));
        // No confirmed value yet -> best_start_secs is the provisional value.
        assert!(anchor.confirmed.is_none());
        assert!((anchor.best_start_secs() - 1_700_000_001.25).abs() < 1e-9);
        // scan_key is preserved verbatim.
        assert_eq!(anchor.scan_key, scan);
        assert!((anchor.provisional.0 - 1_700_000_001.25).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn live_anchor_confirm_swaps_best_start_and_is_idempotent() {
        let scan = ScanKey::from_secs("KDMX", 1_700_000_000);
        let mut anchor = LiveVolumeAnchor::new(scan, ProvisionalStart(1_700_000_001.25));

        anchor.confirm(ConfirmedStart(1_700_000_000.5));
        // Once confirmed, best_start_secs prefers the confirmed value.
        assert!((anchor.best_start_secs() - 1_700_000_000.5).abs() < 1e-9);
        assert_eq!(anchor.confirmed, Some(ConfirmedStart(1_700_000_000.5)));

        // Re-confirming overwrites (idempotent in shape; last write wins).
        anchor.confirm(ConfirmedStart(1_700_000_000.75));
        assert!((anchor.best_start_secs() - 1_700_000_000.75).abs() < 1e-9);
        // Provisional is untouched by confirm.
        assert!((anchor.provisional.0 - 1_700_000_001.25).abs() < 1e-9);
    }
}
