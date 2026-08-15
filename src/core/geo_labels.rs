//! Pure screen-space selection for geographic labels.

use crate::geo::{GeoLabelClass, GeoLayerVisibility, ProjectionFingerprint, ScreenBounds};

/// One retained label per this many logical square points at most.
pub(crate) const LABEL_AREA_PER_ENTRY: f32 = 12_000.0;

/// Padding around measured text bounds, covering the halo plus visual separation.
pub(crate) const LABEL_COLLISION_PADDING: f32 = 2.0;

/// Layer-toggle inputs that affect the global candidate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GeoLabelVisibilityKey {
    pub states: bool,
    pub counties: bool,
    pub cities: bool,
    pub highways: bool,
    pub lakes: bool,
    pub labels: bool,
}

impl From<&GeoLayerVisibility> for GeoLabelVisibilityKey {
    fn from(visibility: &GeoLayerVisibility) -> Self {
        Self {
            states: visibility.states,
            counties: visibility.counties,
            cities: visibility.cities,
            highways: visibility.highways,
            lakes: visibility.lakes,
            labels: visibility.labels,
        }
    }
}

/// Complete identity of a settled geographic-label placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GeoLabelPlacementKey {
    pub projection: ProjectionFingerprint,
    pub visibility: GeoLabelVisibilityKey,
    pub dark: bool,
}

/// Paint-independent metadata consumed by the pure selector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GeoLabelSelectionCandidate {
    pub class: GeoLabelClass,
    pub bounds: ScreenBounds,
    pub source_order: usize,
}

/// Whether the retained placement should be replaced this frame.
pub(crate) fn should_rebuild_labels(
    cached: Option<GeoLabelPlacementKey>,
    requested: GeoLabelPlacementKey,
    camera_settled: bool,
) -> bool {
    cached.is_none() || (cached != Some(requested) && camera_settled)
}

/// Select globally non-overlapping candidates under an area-derived count cap.
///
/// Returned indices address `candidates`. Priority is explicit; source order and
/// finally input order make every repeated selection deterministic.
pub(crate) fn select_geo_labels(
    candidates: &[GeoLabelSelectionCandidate],
    viewport: ScreenBounds,
) -> Vec<usize> {
    let Some(area) = viewport.area() else {
        return Vec::new();
    };
    let budget = ((area / LABEL_AREA_PER_ENTRY).floor() as usize).max(1);

    let mut ordered: Vec<usize> = (0..candidates.len()).collect();
    ordered.sort_by_key(|&index| {
        let candidate = &candidates[index];
        (
            priority_rank(candidate.class),
            candidate.source_order,
            index,
        )
    });

    let mut selected = Vec::with_capacity(budget.min(candidates.len()));
    let mut occupied = Vec::with_capacity(budget.min(candidates.len()));

    for index in ordered {
        let bounds = candidates[index].bounds;
        if !bounds.is_valid() || !viewport.contains(bounds) {
            continue;
        }

        let padded = bounds.expanded(LABEL_COLLISION_PADDING);
        if occupied
            .iter()
            .any(|existing: &ScreenBounds| padded.intersects(*existing))
        {
            continue;
        }

        selected.push(index);
        occupied.push(padded);
        if selected.len() == budget {
            break;
        }
    }

    selected
}

fn priority_rank(class: GeoLabelClass) -> u8 {
    match class {
        GeoLabelClass::State => 0,
        GeoLabelClass::CityMajor => 1,
        GeoLabelClass::CityMedium => 2,
        GeoLabelClass::CitySmall => 3,
        GeoLabelClass::Highway => 4,
        GeoLabelClass::Lake => 5,
        GeoLabelClass::County => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn bounds(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> ScreenBounds {
        ScreenBounds {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    fn candidate(
        class: GeoLabelClass,
        source_order: usize,
        bounds: ScreenBounds,
    ) -> GeoLabelSelectionCandidate {
        GeoLabelSelectionCandidate {
            class,
            bounds,
            source_order,
        }
    }

    fn visibility() -> GeoLabelVisibilityKey {
        GeoLabelVisibilityKey {
            states: true,
            counties: true,
            cities: true,
            highways: false,
            lakes: false,
            labels: true,
        }
    }

    fn placement_key(
        projection: ProjectionFingerprint,
        visibility: GeoLabelVisibilityKey,
        dark: bool,
    ) -> GeoLabelPlacementKey {
        GeoLabelPlacementKey {
            projection,
            visibility,
            dark,
        }
    }

    #[wasm_bindgen_test]
    fn empty_candidates_and_zero_viewport_select_nothing() {
        assert!(select_geo_labels(&[], bounds(0.0, 0.0, 100.0, 100.0)).is_empty());
        let candidates = vec![candidate(
            GeoLabelClass::State,
            0,
            bounds(0.0, 0.0, 10.0, 10.0),
        )];
        assert!(select_geo_labels(&candidates, bounds(0.0, 0.0, 0.0, 100.0)).is_empty());
    }

    #[wasm_bindgen_test]
    fn conflicts_resolve_in_required_priority_order() {
        let overlap = bounds(10.0, 10.0, 30.0, 20.0);
        let candidates = vec![
            candidate(GeoLabelClass::County, 0, overlap),
            candidate(GeoLabelClass::Lake, 0, overlap),
            candidate(GeoLabelClass::Highway, 0, overlap),
            candidate(GeoLabelClass::CitySmall, 0, overlap),
            candidate(GeoLabelClass::CityMedium, 0, overlap),
            candidate(GeoLabelClass::CityMajor, 0, overlap),
            candidate(GeoLabelClass::State, 0, overlap),
        ];

        assert_eq!(
            select_geo_labels(&candidates, bounds(0.0, 0.0, 400.0, 400.0)),
            vec![6]
        );
    }

    #[wasm_bindgen_test]
    fn equal_priority_uses_stable_source_order() {
        let overlap = bounds(10.0, 10.0, 30.0, 20.0);
        let candidates = vec![
            candidate(GeoLabelClass::CityMajor, 9, overlap),
            candidate(GeoLabelClass::CityMajor, 2, overlap),
        ];
        let viewport = bounds(0.0, 0.0, 400.0, 400.0);

        assert_eq!(select_geo_labels(&candidates, viewport), vec![1]);
        assert_eq!(select_geo_labels(&candidates, viewport), vec![1]);
    }

    #[wasm_bindgen_test]
    fn collision_padding_separates_nearby_labels() {
        let candidates = vec![
            candidate(GeoLabelClass::State, 0, bounds(0.0, 0.0, 10.0, 10.0)),
            candidate(GeoLabelClass::State, 1, bounds(13.0, 0.0, 23.0, 10.0)),
            candidate(GeoLabelClass::State, 2, bounds(15.0, 0.0, 25.0, 10.0)),
        ];

        assert_eq!(
            select_geo_labels(&candidates, bounds(0.0, 0.0, 400.0, 400.0)),
            vec![0, 2]
        );
    }

    #[wasm_bindgen_test]
    fn area_budget_caps_non_overlapping_labels() {
        let candidates: Vec<_> = (0..10)
            .map(|i| {
                candidate(
                    GeoLabelClass::State,
                    i,
                    bounds(i as f32 * 20.0, 0.0, i as f32 * 20.0 + 10.0, 10.0),
                )
            })
            .collect();
        let viewport = bounds(0.0, 0.0, 390.0, LABEL_AREA_PER_ENTRY * 3.9 / 390.0);

        assert_eq!(select_geo_labels(&candidates, viewport).len(), 3);
    }

    #[wasm_bindgen_test]
    fn invalid_and_offscreen_candidates_are_rejected() {
        let candidates = vec![
            candidate(GeoLabelClass::State, 0, bounds(f32::NAN, 0.0, 1.0, 1.0)),
            candidate(GeoLabelClass::State, 1, bounds(200.0, 200.0, 210.0, 210.0)),
            candidate(GeoLabelClass::State, 2, bounds(95.0, 10.0, 105.0, 20.0)),
        ];

        assert!(select_geo_labels(&candidates, bounds(0.0, 0.0, 100.0, 100.0)).is_empty());
    }

    #[wasm_bindgen_test]
    fn rebuild_waits_for_a_changed_placement_to_settle() {
        let original = placement_key(ProjectionFingerprint::test_value(1), visibility(), true);
        let changed = placement_key(ProjectionFingerprint::test_value(2), visibility(), true);

        assert!(should_rebuild_labels(None, original, false));
        assert!(!should_rebuild_labels(Some(original), original, true));
        assert!(!should_rebuild_labels(Some(original), changed, false));
        assert!(should_rebuild_labels(Some(original), changed, true));
    }

    #[wasm_bindgen_test]
    fn viewport_theme_and_visibility_invalidate_placement() {
        let base_fingerprint = ProjectionFingerprint::test_value(1);
        let base = placement_key(base_fingerprint, visibility(), true);
        let changed_viewport =
            placement_key(ProjectionFingerprint::test_value(2), visibility(), true);
        let theme = placement_key(base_fingerprint, visibility(), false);
        let mut toggled_visibility = visibility();
        toggled_visibility.counties = false;
        let toggled = placement_key(base_fingerprint, toggled_visibility, true);

        assert_ne!(base, changed_viewport);
        assert_ne!(base, theme);
        assert_ne!(base, toggled);
    }
}
