//! Data source mode and related state structures.

// Fields and methods are defined for future integration
#![allow(dead_code)]

/// The active data source mode, controlling which UI is shown in the left panel.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum DataSourceMode {
    #[default]
    UploadFile,
    ArchiveBrowser,
    RealtimeStream,
}

impl DataSourceMode {
    pub fn label(&self) -> &'static str {
        match self {
            DataSourceMode::UploadFile => "Upload File",
            DataSourceMode::ArchiveBrowser => "Archive Browser",
            DataSourceMode::RealtimeStream => "Realtime Stream",
        }
    }
}

/// State for the file upload mode.
#[derive(Default)]
pub struct UploadState {
    /// Name of the selected file, if any
    pub file_name: Option<String>,

    /// Size of the selected file in bytes
    pub file_size: Option<u64>,

    /// Raw file data (placeholder for future use)
    pub file_data: Option<Vec<u8>>,
}

/// State for the AWS archive browser mode.
#[derive(Default)]
pub struct ArchiveState {
    /// Radar site identifier (e.g., "KTLX")
    pub site_id: String,

    /// Date string for archive query (e.g., "2024-05-20")
    pub date_string: String,

    /// List of available times from the archive (placeholder)
    pub available_times: Vec<String>,

    /// Index of the selected time in the list
    pub selected_time_index: Option<usize>,

    /// Whether an archive query is in progress
    pub loading: bool,
}

/// State for the realtime stream mode.
#[derive(Default)]
pub struct RealtimeState {
    /// Selected radar site for streaming
    pub site_id: String,

    /// Whether currently connected to the stream
    pub connected: bool,

    /// Connection status message
    pub status: String,
}

impl RealtimeState {
    pub fn new() -> Self {
        Self {
            status: "Disconnected".to_string(),
            ..Default::default()
        }
    }
}
