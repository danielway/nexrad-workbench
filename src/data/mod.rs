//! Data modules containing static datasets and sweep-based caching.
//!
//! ## Static Data
//! - `sites`: NEXRAD radar site definitions
//!
//! ## Sweep Cache
//!
//! Stores pre-computed sweep data (GPU-ready) rather than raw records, enabling:
//! - Near-zero render latency (~5-10ms per sweep)
//! - Efficient elevation/product switching
//! - Time-based queries for timeline display
//!
//! ### Key Types
//! - `SiteId`: Radar site identifier (e.g., "KDMX")
//! - `ScanKey`: Identifies a volume scan (site + start time)
//! - `SweepDataKey`: Identifies a sweep (scan + elevation + product)
//! - `PrecomputedSweep`: GPU-ready sweep data (azimuths, gates, metadata)
//!
//! ### Storage Hierarchy
//! ```text
//! IndexedDB "nexrad-workbench"
//! ├── sweeps       - Pre-computed sweep blobs (binary, GPU-ready)
//! ├── scan_index   - Per-scan metadata with completeness tracking
//! └── scan_touches - Per-scan last-access timestamps (LRU eviction)
//! ```

pub mod blob_format;
pub mod facade;
pub mod indexeddb;
pub mod keys;
pub mod live_anchor;
pub mod quota;
pub mod sites;
pub mod vcp;
pub mod vcp_timing;

// Re-export static site data
pub use sites::{all_sites_sorted, get_site, nearest_site, NEXRAD_SITES};

// Re-export cache types
pub use blob_format::*;
pub use facade::*;
pub use keys::*;
pub use live_anchor::*;
pub use vcp_timing::*;
