//! Data source state structures.

// Fields and methods are defined for future integration
#![allow(dead_code)]

/// State for file upload.
#[derive(Default)]
pub struct UploadState {
    /// Name of the selected file, if any
    pub file_name: Option<String>,

    /// Size of the selected file in bytes
    pub file_size: Option<u64>,

    /// Raw file data (placeholder for future use)
    pub file_data: Option<Vec<u8>>,

    /// Whether a file pick operation is in progress
    pub loading: bool,
}
