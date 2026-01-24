//! Data modules containing static datasets and record-based caching.
//!
//! ## Static Data
//! - `sites`: NEXRAD radar site definitions
//!
//! ## Record-Based Cache (v4 schema)
//!
//! The new caching system stores individual compressed records rather than
//! full scans, enabling:
//! - Efficient partial scan storage (realtime streaming, interrupted downloads)
//! - Time-based queries without loading full scans
//! - Deduplication across download/streaming sources
//!
//! ### Key Types
//! - `SiteId`: Radar site identifier (e.g., "KDMX")
//! - `ScanKey`: Identifies a volume scan (site + start time)
//! - `RecordKey`: Identifies a record within a scan
//! - `RecordBlob`: Compressed record data
//!
//! ### Storage Hierarchy
//! ```text
//! IndexedDB "nexrad-workbench" v4
//! ├── records_v4      - Raw bzip2-compressed record blobs
//! ├── record_index_v4 - Per-record metadata with time indexing
//! └── scan_index_v4   - Per-scan metadata with completeness tracking
//! ```
//!
//! ### Migration from v3
//! Old `nexrad-scans` and `scan-metadata` stores are preserved.
//! Migration happens lazily when old data is accessed.

pub mod sites;
pub mod keys;
pub mod record_cache;
pub mod facade;

#[cfg(target_arch = "wasm32")]
pub mod indexeddb_v4;

// Re-export static site data
pub use sites::{all_sites_sorted, get_site, NEXRAD_SITES};

// Re-export record cache types
pub use keys::*;
pub use record_cache::*;
pub use facade::*;
