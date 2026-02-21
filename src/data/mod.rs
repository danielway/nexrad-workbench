//! Data modules containing static datasets and record-based caching.
//!
//! ## Static Data
//! - `sites`: NEXRAD radar site definitions
//!
//! ## Record Cache
//!
//! Stores individual compressed records rather than full scans, enabling:
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
//! IndexedDB "nexrad-workbench"
//! ├── records      - Raw bzip2-compressed record blobs
//! ├── record_index - Per-record metadata with time indexing
//! └── scan_index   - Per-scan metadata with completeness tracking
//! ```

pub mod facade;
pub mod keys;
pub mod record_cache;
pub mod sites;

pub mod indexeddb;

// Re-export static site data
pub use sites::{all_sites_sorted, get_site, NEXRAD_SITES};

// Re-export record cache types
pub use facade::*;
pub use keys::*;
pub use record_cache::*;
