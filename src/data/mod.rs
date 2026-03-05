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
//! ├── sweeps     - Pre-computed sweep blobs (binary, GPU-ready)
//! └── scan_index - Per-scan metadata with completeness tracking
//! ```

pub(crate) mod facade;
pub(crate) mod indexeddb;
pub(crate) mod keys;
pub(crate) mod sites;

// Re-export static site data
pub use sites::{all_sites_sorted, get_site, NEXRAD_SITES};

// Re-export cache types
pub use facade::*;
pub use keys::*;
