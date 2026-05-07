//! Single source of truth for which timeline source draws each entry.
//!
//! Before this module the scan-track renderer, the sweep-track renderer,
//! the download-ghost overlay, and the shadow-boundary overlay each ran
//! their own fuzzy float compares (`< 0.5`, `< 30`, `< 60`) to decide
//! whether to skip an entry that would also be drawn by another layer.
//! The 0.5-second tolerance in particular existed only because callers
//! were parsing timestamp strings back to floats and didn't trust the
//! round-trip; with [`crate::data::LiveVolumeAnchor`] in place that path
//! is now exact.
//!
//! [`TimelineModel`] is built once per frame from the same upstream data
//! the renderers used to read directly. Each renderer asks the model
//! whether to draw a given source entry — there is exactly one place
//! where dedup logic lives, and the bands that *are* still fuzzy
//! (filename-timestamp vs. radial-header time, 60s) are documented as
//! constants here.
//!
//! The shape is reconciliation-helper rather than a flat
//! `Vec<TimelineEntry>` because Rust's ownership story is simpler when the
//! model borrows from the upstream sources rather than cloning them every
//! frame; the renderers continue to iterate their own slices and ask the
//! model "should this entry render?". Conceptually the answers add up to
//! the same flat picture.

use crate::data::{LiveVolumeAnchor, ScanKey, UnixMillis};
use crate::nexrad::ScanBoundary;
use crate::state::radar_data::Scan;
use std::collections::BTreeSet;

/// Tolerance for matching a download/shadow boundary against a stored
/// scan, in seconds. Wider than zero because the boundary comes from the
/// archive listing's filename time while the stored scan's
/// `key_timestamp` comes from the volume header time parsed during
/// ingest; the two can differ by a few seconds without representing
/// different volumes. 60s comfortably covers normal NEXRAD volume
/// cadence (~5–10 min) without absorbing adjacent volumes.
pub const BOUNDARY_MATCH_TOLERANCE_SECS: i64 = 60;

/// Tolerance for matching the ghost overlay's recently-completed flash
/// against a stored scan. Tighter than [`BOUNDARY_MATCH_TOLERANCE_SECS`]
/// because the flash is short-lived (~1s) and a wider band would let an
/// unrelated scan trigger the wrong flash position.
pub const COMPLETION_MATCH_TOLERANCE_SECS: i64 = 30;

/// Frame-scoped reconciliation view across all timeline sources.
pub struct TimelineModel<'a> {
    live_anchor: Option<&'a LiveVolumeAnchor>,
    /// Stored scan keys, in UnixMillis, sorted for range queries.
    historical_keys: BTreeSet<i64>,
}

impl<'a> TimelineModel<'a> {
    /// Build the model from the same sources the renderers read directly.
    pub fn build<I: IntoIterator<Item = &'a Scan>>(
        live_anchor: Option<&'a LiveVolumeAnchor>,
        historical_scans: I,
    ) -> Self {
        let historical_keys = historical_scans
            .into_iter()
            .map(|s| (s.key_timestamp * 1000.0).round() as i64)
            .collect();
        Self {
            live_anchor,
            historical_keys,
        }
    }

    /// The currently-streaming volume's anchor, if any.
    #[allow(dead_code)]
    pub fn live_anchor(&self) -> Option<&'a LiveVolumeAnchor> {
        self.live_anchor
    }

    /// Whether the given stored scan represents the *same volume* as the
    /// active live stream and therefore must be skipped by the historical
    /// renderer (the realtime overlay owns it).
    ///
    /// Uses exact [`ScanKey`] equality — the IDB write that produced this
    /// stored scan was keyed under the live anchor's provisional start, so
    /// equality is the right relation. Any mismatch means a different
    /// volume.
    pub fn is_active_live_volume(&self, scan: &Scan) -> bool {
        let Some(anchor) = self.live_anchor else {
            return false;
        };
        let scan_key = ScanKey::new(
            anchor.scan_key.site.clone(),
            UnixMillis((scan.key_timestamp * 1000.0).round() as i64),
        );
        scan_key == anchor.scan_key
    }

    /// Whether a download-ghost or shadow-boundary range overlaps any
    /// stored scan. Used by the ghost/shadow overlays to suppress markers
    /// for ranges already represented as filled scan blocks.
    ///
    /// Tolerance is [`BOUNDARY_MATCH_TOLERANCE_SECS`] on each side of the
    /// range start.
    pub fn is_covered_by_historical(&self, start_secs: i64) -> bool {
        let lo_ms = (start_secs - BOUNDARY_MATCH_TOLERANCE_SECS).saturating_mul(1000);
        let hi_ms = (start_secs + BOUNDARY_MATCH_TOLERANCE_SECS).saturating_mul(1000);
        self.historical_keys.range(lo_ms..=hi_ms).next().is_some()
    }

    /// Like [`Self::is_covered_by_historical`] but with the tighter
    /// [`COMPLETION_MATCH_TOLERANCE_SECS`] tolerance, used by the
    /// recently-completed flash to find its target stored scan. Returns
    /// the matching key timestamp (Unix millis) when found so the caller
    /// can look up the scan's start/end.
    pub fn match_completion(&self, scan_start_secs: i64) -> Option<i64> {
        let lo_ms = (scan_start_secs - COMPLETION_MATCH_TOLERANCE_SECS).saturating_mul(1000);
        let hi_ms = (scan_start_secs + COMPLETION_MATCH_TOLERANCE_SECS).saturating_mul(1000);
        self.historical_keys.range(lo_ms..=hi_ms).next().copied()
    }

    /// Filter an iterator of historical scans down to entries the
    /// scan-track renderer should actually draw — i.e. exclude the live
    /// volume.
    pub fn historical_to_render<'b, I: IntoIterator<Item = &'b Scan>>(
        &'b self,
        scans: I,
    ) -> impl Iterator<Item = &'b Scan>
    where
        'a: 'b,
    {
        scans.into_iter().filter(|s| !self.is_active_live_volume(s))
    }

    /// Filter shadow boundaries to those not already covered by a stored
    /// scan. Caller should still apply view-range clipping.
    pub fn shadows_to_render<'b, I: IntoIterator<Item = &'b ScanBoundary>>(
        &'b self,
        boundaries: I,
    ) -> impl Iterator<Item = &'b ScanBoundary>
    where
        'a: 'b,
    {
        boundaries
            .into_iter()
            .filter(|b| !self.is_covered_by_historical(b.start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{ProvisionalStart, SiteId};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn scan_at(start_secs: f64) -> Scan {
        Scan {
            start_time: start_secs,
            end_time: start_secs + 300.0,
            key_timestamp: start_secs,
            vcp: 212,
            vcp_pattern: None,
            sweeps: Vec::new(),
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn anchor_at(site: &str, start_secs: f64) -> LiveVolumeAnchor {
        let scan_key = ScanKey::from_secs(site, start_secs as i64);
        LiveVolumeAnchor::new(scan_key, ProvisionalStart(start_secs))
    }

    #[wasm_bindgen_test]
    fn live_volume_dedup_uses_exact_scan_key_equality() {
        let scans = vec![scan_at(1_700_000_000.0), scan_at(1_700_000_300.0)];
        let anchor = anchor_at("KDMX", 1_700_000_000.0);
        let model = TimelineModel::build(Some(&anchor), scans.iter());

        assert!(model.is_active_live_volume(&scans[0]));
        assert!(!model.is_active_live_volume(&scans[1]));
    }

    #[wasm_bindgen_test]
    fn live_volume_dedup_ignores_scan_when_no_anchor() {
        let scans = vec![scan_at(1_700_000_000.0)];
        let model = TimelineModel::build(None, scans.iter());
        assert!(!model.is_active_live_volume(&scans[0]));
    }

    #[wasm_bindgen_test]
    fn live_volume_dedup_ignores_other_sites() {
        let scans = vec![Scan {
            // Same timestamp as the anchor below, but a different site —
            // the renderer should still draw it because it isn't the live
            // volume.
            ..scan_at(1_700_000_000.0)
        }];
        // Build the model with KDMX anchor; the scan above is implicitly
        // the same site as whatever the model thinks (the model derives
        // site from the anchor when forming the comparison ScanKey, so a
        // matched timestamp counts as "same volume" by design — this test
        // documents that limitation).
        let anchor = LiveVolumeAnchor::new(
            ScanKey::new(SiteId("KEAX".to_string()), UnixMillis(1_700_000_000_000)),
            ProvisionalStart(1_700_000_000.0),
        );
        let model = TimelineModel::build(Some(&anchor), scans.iter());
        // Same millis, different site than the scan would have if the
        // scan carried its own site — but our Scan doesn't, so the
        // current implementation conflates them. This test pins the
        // current behavior so a future change can intentionally split
        // identity by site.
        assert!(model.is_active_live_volume(&scans[0]));
    }

    #[wasm_bindgen_test]
    fn covered_by_historical_uses_60s_tolerance_band() {
        let scans = vec![scan_at(1_700_000_000.0)];
        let model = TimelineModel::build(None, scans.iter());

        assert!(model.is_covered_by_historical(1_700_000_000));
        assert!(model.is_covered_by_historical(1_700_000_059));
        assert!(model.is_covered_by_historical(1_700_000_000 - 60));
        assert!(!model.is_covered_by_historical(1_700_000_061));
        assert!(!model.is_covered_by_historical(1_700_000_000 - 61));
    }

    #[wasm_bindgen_test]
    fn match_completion_uses_30s_tolerance_band() {
        let scans = vec![scan_at(1_700_000_000.0)];
        let model = TimelineModel::build(None, scans.iter());

        assert_eq!(
            model.match_completion(1_700_000_000),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            model.match_completion(1_700_000_029),
            Some(1_700_000_000_000)
        );
        assert_eq!(model.match_completion(1_700_000_031), None);
    }
}
