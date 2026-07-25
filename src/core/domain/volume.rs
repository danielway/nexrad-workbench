//! `VolumeElevationRoster` — typed view of an in-progress live volume's
//! elevation list, combining the VCP-claimed expected count with the
//! observed-from-chunks received list.
//!
//! Before this module, two parallel fields on `LiveModeState` carried the
//! information separately: `expected_elevation_count: Option<u8>` (set by
//! `record_vcp` from the message-type-5 payload) and `elevations_received:
//! Vec<u8>` (appended by `record_elevations` from chunk radial headers).
//! Consumers had to read both and reconcile implicitly. Edge cases like
//! split-cut VCPs (some claimed elevations skipped) and observed-not-in-VCP
//! (rare radial-header drift) were papered over rather than visible.
//!
//! The roster surfaces these explicitly via `expected_but_not_received()`
//! and `received_but_not_expected()`, while keeping the simpler accessors
//! (`is_complete`, `is_received`, `status_label`) the common UI path needs.
//!
//! Note: `RenderCoordinator.available_elevations` is intentionally NOT
//! folded into the roster — it covers a different scope ("elevations the
//! GPU can render right now," combining live + archive sources). The
//! roster is observation-only and live-only.

/// Combined view of expected vs received elevations for the in-progress
/// live volume.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct VolumeElevationRoster {
    /// Total elevations claimed by the VCP message-type-5 payload.
    /// `None` until the VCP arrives. NEXRAD VCPs are 1..=count contiguous,
    /// so the expected elevation *numbers* are implicit.
    pub expected_count: Option<usize>,
    /// Elevation numbers observed in chunk radial headers, sorted ascending.
    /// May contain values outside `1..=expected_count` in rare radial-header
    /// drift scenarios; `received_but_not_expected()` surfaces those.
    pub received: Vec<u8>,
}

impl VolumeElevationRoster {
    pub(crate) fn new(expected_count: Option<usize>, received: Vec<u8>) -> Self {
        Self {
            expected_count,
            received,
        }
    }

    /// Number of expected elevations per the VCP, or `None` pre-VCP.
    pub(crate) fn expected_count(&self) -> Option<usize> {
        self.expected_count
    }

    /// Number of distinct elevations observed so far.
    #[allow(dead_code)] // Roster API (module docs); production callers iterate `received` directly.
    pub(crate) fn received_count(&self) -> usize {
        self.received.len()
    }

    /// `true` once the VCP is known and every claimed elevation has been
    /// received. Returns `false` while the VCP is unknown — callers can
    /// distinguish "incomplete because still streaming" from "incomplete
    /// because unknown" via `expected_count().is_some()`.
    #[allow(dead_code)] // Roster API (module docs); reserved for status-bar surfaces.
    pub(crate) fn is_complete(&self) -> bool {
        match self.expected_count {
            Some(n) => self.received.len() >= n,
            None => false,
        }
    }

    /// Whether the given elevation number has been received.
    pub(crate) fn is_received(&self, elev_num: u8) -> bool {
        self.received.contains(&elev_num)
    }

    /// Elevation numbers expected per the VCP but not yet received.
    /// Empty before the VCP arrives, or once the volume is complete.
    #[allow(dead_code)] // Roster API (module docs); reserved for diagnostics overlays.
    pub(crate) fn expected_but_not_received(&self) -> Vec<u8> {
        let Some(count) = self.expected_count else {
            return Vec::new();
        };
        (1..=count as u8)
            .filter(|n| !self.received.contains(n))
            .collect()
    }

    /// Elevation numbers received in chunk radial headers but outside the
    /// VCP's claimed range. Almost always empty; non-empty indicates
    /// either a split-cut VCP not described in the message or radial-header
    /// drift. Useful for diagnostics — surfacing this to a debug overlay
    /// would catch a class of bugs that's currently silent.
    #[allow(dead_code)] // Doc above: diagnostics surface for silent VCP/radial-header drift.
    pub(crate) fn received_but_not_expected(&self) -> Vec<u8> {
        let Some(count) = self.expected_count else {
            return Vec::new();
        };
        self.received
            .iter()
            .copied()
            .filter(|&n| (n as usize) == 0 || (n as usize) > count)
            .collect()
    }

    /// Short label suitable for status text: "5 of 7" when the VCP is
    /// known, "5" when it isn't.
    #[allow(dead_code)] // Roster API (module docs); reserved for status-bar surfaces.
    pub(crate) fn status_label(&self) -> String {
        match self.expected_count {
            Some(n) => format!("{} of {}", self.received.len(), n),
            None => format!("{}", self.received.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn roster_complete_when_all_expected_received() {
        let r = VolumeElevationRoster::new(Some(3), vec![1, 2, 3]);
        assert!(r.is_complete());
        assert!(r.expected_but_not_received().is_empty());
        assert!(r.received_but_not_expected().is_empty());
        assert_eq!(r.status_label(), "3 of 3");
    }

    #[wasm_bindgen_test]
    fn roster_partial_pre_vcp_status_omits_denominator() {
        let r = VolumeElevationRoster::new(None, vec![1, 2]);
        assert!(!r.is_complete());
        assert_eq!(r.expected_but_not_received(), Vec::<u8>::new());
        assert_eq!(r.status_label(), "2");
    }

    #[wasm_bindgen_test]
    fn roster_split_cut_vcp_marks_skipped_as_expected_but_not_received() {
        // VCP claims 5 elevations; radar transmits only 1, 3, 5 (split cut).
        let r = VolumeElevationRoster::new(Some(5), vec![1, 3, 5]);
        assert!(!r.is_complete());
        assert_eq!(r.expected_but_not_received(), vec![2, 4]);
        assert!(r.received_but_not_expected().is_empty());
        assert_eq!(r.status_label(), "3 of 5");
    }

    #[wasm_bindgen_test]
    fn roster_received_but_not_expected_surfaces_radial_header_drift() {
        // VCP claims 3 elevations; a radial header reports elevation 6 anyway.
        let r = VolumeElevationRoster::new(Some(3), vec![1, 2, 3, 6]);
        assert_eq!(r.received_but_not_expected(), vec![6]);
        // The complete-check is permissive: it succeeds because all 1..=3 arrived.
        assert!(r.is_complete());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn new_round_trips_fields() {
        let r = VolumeElevationRoster::new(Some(4), vec![1, 2]);
        assert!(r.expected_count == Some(4));
        assert!(r.received == vec![1u8, 2u8]);
    }

    #[wasm_bindgen_test]
    fn default_is_empty_and_pre_vcp() {
        let r = VolumeElevationRoster::default();
        assert!(r.expected_count.is_none());
        assert!(r.received.is_empty());
        assert_eq!(r.received_count(), 0);
        assert!(!r.is_complete());
        assert_eq!(r.status_label(), "0");
        assert!(r.expected_but_not_received().is_empty());
        assert!(r.received_but_not_expected().is_empty());
    }

    #[wasm_bindgen_test]
    fn expected_count_accessor_reflects_value() {
        let known = VolumeElevationRoster::new(Some(7), vec![]);
        assert_eq!(known.expected_count(), Some(7));
        let unknown = VolumeElevationRoster::new(None, vec![1]);
        assert_eq!(unknown.expected_count(), None);
    }

    #[wasm_bindgen_test]
    fn received_count_counts_entries_not_distinctness() {
        // received_count() is just .len(); it does not dedup.
        let r = VolumeElevationRoster::new(Some(3), vec![1, 1, 2]);
        assert_eq!(r.received_count(), 3);
    }

    #[wasm_bindgen_test]
    fn is_received_true_for_present_false_for_absent() {
        let r = VolumeElevationRoster::new(Some(5), vec![2, 4]);
        assert!(r.is_received(2));
        assert!(r.is_received(4));
        assert!(!r.is_received(1));
        assert!(!r.is_received(3));
        assert!(!r.is_received(5));
    }

    #[wasm_bindgen_test]
    fn is_received_false_on_empty_roster() {
        let r = VolumeElevationRoster::new(None, vec![]);
        assert!(!r.is_received(1));
    }

    #[wasm_bindgen_test]
    fn is_complete_true_when_received_exceeds_expected() {
        // is_complete uses `>=`, so an over-count (e.g. drift adding an
        // out-of-range elevation) still reads as complete.
        let r = VolumeElevationRoster::new(Some(2), vec![1, 2, 9]);
        assert!(r.is_complete());
    }

    #[wasm_bindgen_test]
    fn is_complete_false_one_below_boundary() {
        let r = VolumeElevationRoster::new(Some(3), vec![1, 2]);
        assert!(!r.is_complete());
    }

    #[wasm_bindgen_test]
    fn expected_zero_count_is_trivially_complete() {
        // Some(0): the 1..=0 range is empty, so nothing is expected-but-missing
        // and an empty received list is already "complete" via 0 >= 0.
        let r = VolumeElevationRoster::new(Some(0), vec![]);
        assert!(r.is_complete());
        assert!(r.expected_but_not_received().is_empty());
        assert_eq!(r.status_label(), "0 of 0");
    }

    #[wasm_bindgen_test]
    fn expected_zero_count_flags_all_received_as_unexpected() {
        // With Some(0), every received elevation has n > count, so all are
        // surfaced as received-but-not-expected.
        let r = VolumeElevationRoster::new(Some(0), vec![1, 2]);
        assert_eq!(r.received_but_not_expected(), vec![1, 2]);
    }

    #[wasm_bindgen_test]
    fn received_but_not_expected_flags_zero_sentinel() {
        // Elevation number 0 is out-of-range (VCP elevations are 1..=count)
        // and the filter's `n == 0` branch must catch it.
        let r = VolumeElevationRoster::new(Some(3), vec![0, 1, 2, 3]);
        assert_eq!(r.received_but_not_expected(), vec![0]);
    }

    #[wasm_bindgen_test]
    fn received_but_not_expected_preserves_received_order_and_dups() {
        // The filter copies straight from `received` without sorting/dedup.
        let r = VolumeElevationRoster::new(Some(2), vec![5, 3, 5]);
        assert_eq!(r.received_but_not_expected(), vec![5, 3, 5]);
    }

    #[wasm_bindgen_test]
    fn expected_but_not_received_full_when_none_received() {
        let r = VolumeElevationRoster::new(Some(4), vec![]);
        assert_eq!(r.expected_but_not_received(), vec![1, 2, 3, 4]);
        assert!(!r.is_complete());
        assert_eq!(r.status_label(), "0 of 4");
    }

    #[wasm_bindgen_test]
    fn clone_and_partial_eq_hold() {
        let a = VolumeElevationRoster::new(Some(3), vec![1, 2]);
        let b = a.clone();
        assert!(a == b);
        let c = VolumeElevationRoster::new(Some(3), vec![1, 3]);
        assert!(a != c);
        let d = VolumeElevationRoster::new(None, vec![1, 2]);
        assert!(a != d);
    }
}
