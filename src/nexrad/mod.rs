//! NEXRAD data integration module.
//!
//! This module provides functionality for:
//! - Downloading archival NEXRAD data from AWS
//! - Caching downloaded data in IndexedDB
//! - Custom rendering of NEXRAD sweeps using egui's Painter API

mod cache;
mod download;
mod render;
mod types;

pub use cache::NexradCache;
pub use download::DownloadChannel;
pub use render::{decode_sweep_from_data, render_sweep, DecodedSweep, ReflectivityPalette};
pub use types::{CachedScan, DownloadResult};
