/// Parameters for a single-elevation render request. Adding a field here
/// automatically breaks the `PartialEq` comparison, preventing silent omissions.
#[derive(Clone, PartialEq)]
pub struct RenderRequest {
    pub scan_key: String,
    pub elevation_number: u8,
    pub product: String,
    pub is_auto: bool,
}

/// Parameters for a volume (all-elevations) render request.
#[derive(Clone, PartialEq)]
pub struct VolumeRenderRequest {
    pub scan_key: String,
    pub product: String,
}
