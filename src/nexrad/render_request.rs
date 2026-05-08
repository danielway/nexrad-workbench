//! Render request parameter types.
//!
//! Single-elevation requests are identified by [`SweepIdentity`] (defined in
//! `state::viz`) so the dedup cache shares one type with the on-GPU
//! `displayed` slot and the resolver. Volume requests keep their own type
//! because they span elevations.

use crate::data::ScanKey;

/// Parameters for a volume (all-elevations) render request.
#[derive(Clone, PartialEq)]
pub struct VolumeRenderRequest {
    pub scan_key: ScanKey,
    pub product: String,
}
