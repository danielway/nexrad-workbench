//! Active filter on the realtime chunk stream.
//!
//! The pure elevation-cut filter vocabulary shared by the streaming loop
//! ([`crate::nexrad::live::realtime`]), the per-volume plan
//! ([`super::streaming_plan`]), and the timing mapper
//! ([`super::timing::ElevationChunkMapper`] consumers).

use crate::core::tilt::{CutSet, TiltFilter};
use crate::core::ElevationSelection;
use crate::data::ExtractedVcp;

/// Active filter on the realtime chunk stream.
///
/// `All` (default) downloads every chunk in the volume — equivalent to
/// `ElevationSelection::Latest` because the renderer chooses whichever
/// elevation completed most recently. `Cuts(set)` restricts the loop to
/// the Start chunk plus chunks belonging to the cuts in `set` (every visit
/// of a held angle, SAILS/MRLE repeats included); the loop uses the VCP's
/// `ElevationChunkMapper` and the physics-based timing model to wait
/// through chunks that don't match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StreamingFilter {
    #[default]
    All,
    Cuts(CutSet),
}

impl StreamingFilter {
    /// Just one cut.
    pub(crate) fn elevation(elevation_number: u8) -> Self {
        StreamingFilter::Cuts(CutSet::only(elevation_number))
    }

    /// The filter for the user's tilt, resolved against the live volume's
    /// VCP pattern: every cut at the held angle. Between volumes the pattern
    /// is briefly unknown; a filter already resolved for the held cut is
    /// kept then (`previous`) rather than narrowed to the one cut and
    /// widened again a moment later.
    pub(crate) fn for_selection(
        selection: &ElevationSelection,
        plan: Option<&ExtractedVcp>,
        previous: StreamingFilter,
    ) -> Self {
        let tilt = TiltFilter::from(selection);
        let Some(anchor) = tilt.anchor() else {
            return StreamingFilter::All;
        };
        if plan.is_none() {
            if let StreamingFilter::Cuts(set) = previous {
                if set.contains(anchor) {
                    return previous;
                }
            }
        }
        StreamingFilter::Cuts(tilt.cuts(plan))
    }

    /// Whether the filter accepts a chunk for the given elevation number.
    /// `None` (Start chunk) is always accepted.
    pub(crate) fn accepts(self, elevation_number: Option<usize>) -> bool {
        match (self, elevation_number) {
            (StreamingFilter::All, _) => true,
            (StreamingFilter::Cuts(_), None) => true,
            (StreamingFilter::Cuts(set), Some(elev)) => {
                u8::try_from(elev).is_ok_and(|n| set.contains(n))
            }
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::core::tilt::fixtures::{vcp_212_sails_x3, vcp_35};
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn default_is_all() {
        assert_eq!(StreamingFilter::default(), StreamingFilter::All);
    }

    #[wasm_bindgen_test]
    fn all_accepts_start_chunk() {
        assert!(StreamingFilter::All.accepts(None));
    }

    #[wasm_bindgen_test]
    fn all_accepts_any_elevation() {
        assert!(StreamingFilter::All.accepts(Some(0)));
        assert!(StreamingFilter::All.accepts(Some(1)));
        assert!(StreamingFilter::All.accepts(Some(15)));
        assert!(StreamingFilter::All.accepts(Some(255)));
    }

    #[wasm_bindgen_test]
    fn elevation_always_accepts_start_chunk() {
        // Start chunk (None) is always accepted regardless of target.
        assert!(StreamingFilter::elevation(1).accepts(None));
        assert!(StreamingFilter::elevation(7).accepts(None));
    }

    #[wasm_bindgen_test]
    fn elevation_accepts_matching() {
        assert!(StreamingFilter::elevation(3).accepts(Some(3)));
        assert!(StreamingFilter::elevation(1).accepts(Some(1)));
        assert!(StreamingFilter::elevation(20).accepts(Some(20)));
    }

    #[wasm_bindgen_test]
    fn elevation_rejects_non_matching() {
        assert!(!StreamingFilter::elevation(3).accepts(Some(2)));
        assert!(!StreamingFilter::elevation(3).accepts(Some(4)));
        assert!(!StreamingFilter::elevation(1).accepts(Some(0)));
        assert!(!StreamingFilter::elevation(7).accepts(Some(1)));
        // A number past a u8 never wraps onto a held cut.
        assert!(!StreamingFilter::elevation(1).accepts(Some(257)));
    }

    #[wasm_bindgen_test]
    fn equality_and_clone_copy() {
        let f = StreamingFilter::elevation(5);
        let g = f; // Copy
        assert_eq!(f, g);
        assert_ne!(StreamingFilter::elevation(5), StreamingFilter::elevation(6));
        assert_ne!(StreamingFilter::All, StreamingFilter::elevation(1));
    }

    fn held_05() -> ElevationSelection {
        ElevationSelection::Fixed {
            elevation_number: 1,
            angle: 0.5,
        }
    }

    #[wasm_bindgen_test]
    fn a_held_angle_streams_every_sails_visit() {
        let v = vcp_212_sails_x3();
        let f = StreamingFilter::for_selection(&held_05(), Some(&v), StreamingFilter::All);
        for n in [1, 9, 14, 19] {
            assert!(f.accepts(Some(n)), "cut {n}");
        }
        for n in [2, 3, 10, 13, 20] {
            assert!(!f.accepts(Some(n)), "cut {n}");
        }
        let f35 = StreamingFilter::for_selection(&held_05(), Some(&vcp_35()), StreamingFilter::All);
        assert_eq!(f35, StreamingFilter::elevation(1));
    }

    #[wasm_bindgen_test]
    fn latest_streams_everything() {
        let v = vcp_212_sails_x3();
        assert_eq!(
            StreamingFilter::for_selection(
                &ElevationSelection::Latest,
                Some(&v),
                StreamingFilter::All
            ),
            StreamingFilter::All
        );
    }

    #[wasm_bindgen_test]
    fn between_volumes_the_resolved_filter_holds() {
        let v = vcp_212_sails_x3();
        let resolved = StreamingFilter::for_selection(&held_05(), Some(&v), StreamingFilter::All);
        // The pattern is wiped at the volume boundary: keep the set.
        assert_eq!(
            StreamingFilter::for_selection(&held_05(), None, resolved),
            resolved
        );
        // A different held cut is not kept from the old set.
        let held_09 = ElevationSelection::Fixed {
            elevation_number: 3,
            angle: 0.9,
        };
        assert_eq!(
            StreamingFilter::for_selection(&held_09, None, resolved),
            StreamingFilter::elevation(3)
        );
    }
}
