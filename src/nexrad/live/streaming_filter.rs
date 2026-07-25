//! Active filter on the realtime chunk stream.
//!
//! Lifted out of [`super::realtime`] so the projection layer
//! ([`super::streaming_state`], [`super::streaming_plan`]) can depend on
//! it without creating a cycle through `realtime`. The
//! `From<&crate::core::ElevationSelection>` translation stays in
//! `realtime.rs` since it depends on the UI-side selection type.

/// Active filter on the realtime chunk stream.
///
/// `All` (default) downloads every chunk in the volume — equivalent to
/// `ElevationSelection::Latest` because the renderer chooses whichever
/// elevation completed most recently. `Elevation(n)` restricts the loop to
/// the Start chunk plus chunks belonging to elevation `n`; the loop uses the
/// VCP's `ElevationChunkMapper` and the physics-based timing model to wait
/// through chunks that don't match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StreamingFilter {
    #[default]
    All,
    Elevation(u8),
}

impl StreamingFilter {
    /// Whether the filter accepts a chunk for the given elevation number.
    /// `None` (Start chunk) is always accepted.
    pub(crate) fn accepts(self, elevation_number: Option<usize>) -> bool {
        match (self, elevation_number) {
            (StreamingFilter::All, _) => true,
            (StreamingFilter::Elevation(_), None) => true,
            (StreamingFilter::Elevation(target), Some(elev)) => elev as u8 == target,
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
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
        assert!(StreamingFilter::Elevation(1).accepts(None));
        assert!(StreamingFilter::Elevation(7).accepts(None));
        assert!(StreamingFilter::Elevation(0).accepts(None));
    }

    #[wasm_bindgen_test]
    fn elevation_accepts_matching() {
        assert!(StreamingFilter::Elevation(3).accepts(Some(3)));
        assert!(StreamingFilter::Elevation(1).accepts(Some(1)));
        assert!(StreamingFilter::Elevation(20).accepts(Some(20)));
    }

    #[wasm_bindgen_test]
    fn elevation_rejects_non_matching() {
        assert!(!StreamingFilter::Elevation(3).accepts(Some(2)));
        assert!(!StreamingFilter::Elevation(3).accepts(Some(4)));
        assert!(!StreamingFilter::Elevation(1).accepts(Some(0)));
        assert!(!StreamingFilter::Elevation(7).accepts(Some(1)));
    }

    #[wasm_bindgen_test]
    fn elevation_usize_to_u8_wraps_at_256() {
        // `elev as u8` truncates: 256 wraps to 0, so Elevation(0) matches it.
        assert!(StreamingFilter::Elevation(0).accepts(Some(256)));
        // and Elevation(1) matches a usize 257 (257 as u8 == 1).
        assert!(StreamingFilter::Elevation(1).accepts(Some(257)));
        // A target that the truncated value does not equal is rejected.
        assert!(!StreamingFilter::Elevation(2).accepts(Some(256)));
    }

    #[wasm_bindgen_test]
    fn equality_and_clone_copy() {
        let f = StreamingFilter::Elevation(5);
        let g = f; // Copy
        assert_eq!(f, g);
        assert_eq!(f.clone(), StreamingFilter::Elevation(5));
        assert_ne!(StreamingFilter::Elevation(5), StreamingFilter::Elevation(6));
        assert_ne!(StreamingFilter::All, StreamingFilter::Elevation(0));
    }

    #[wasm_bindgen_test]
    fn debug_format_non_empty() {
        // Derived Debug should render the variant name.
        let s = format!("{:?}", StreamingFilter::Elevation(2));
        assert!(s.contains("Elevation"));
        assert!(s.contains('2'));
        let a = format!("{:?}", StreamingFilter::All);
        assert!(a.contains("All"));
    }
}
