//! Active filter on the realtime chunk stream.
//!
//! Lifted out of [`super::realtime`] so the projection layer
//! ([`super::streaming_state`], [`super::streaming_plan`]) can depend on
//! it without creating a cycle through `realtime`. The
//! `From<&crate::state::ElevationSelection>` translation stays in
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
pub enum StreamingFilter {
    #[default]
    All,
    Elevation(u8),
}

impl StreamingFilter {
    /// Whether the filter accepts a chunk for the given elevation number.
    /// `None` (Start chunk) is always accepted.
    pub fn accepts(self, elevation_number: Option<usize>) -> bool {
        match (self, elevation_number) {
            (StreamingFilter::All, _) => true,
            (StreamingFilter::Elevation(_), None) => true,
            (StreamingFilter::Elevation(target), Some(elev)) => elev as u8 == target,
        }
    }
}
