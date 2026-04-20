//! Playback sweep cache and previous-sweep resolution logic.
//!
//! Extracted from `main.rs` to reduce the size of `WorkbenchApp` and group
//! sweep-cache / sweep-animation helpers in one place.

use std::collections::HashMap;

use crate::state::radar_data::{RadarTimeline, Scan, Sweep};

// ---------------------------------------------------------------------------
// Cached sweep data
// ---------------------------------------------------------------------------

/// Cached decoded sweep data for stateless sweep animation.
///
/// Stores a small number of recent decode results so the renderer can load
/// any two sweeps (current + previous) without depending on decode arrival order.
#[allow(dead_code)] // Fields read when loading from cache into GPU
pub(crate) struct CachedSweepData {
    pub gate_values: Vec<f32>,
    pub azimuths: Vec<f32>,
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
    pub max_range_km: f64,
    pub offset: f32,
    pub scale: f32,
    pub azimuth_spacing_deg: f32,
    pub radial_times: Vec<f64>,
    pub sweep_start_secs: f64,
    pub sweep_end_secs: f64,
    pub product: String,
}

/// Build a sweep cache key from scan key and elevation number.
pub(crate) fn sweep_cache_key(scan_key: &str, elevation_number: u8, product: &str) -> String {
    format!("{}|{}|{}", scan_key, elevation_number, product)
}

// ---------------------------------------------------------------------------
// LRU sweep cache
// ---------------------------------------------------------------------------

/// LRU cache of decoded sweep data. Entries are evicted when the cache exceeds
/// `max_entries`. Keys are "SCAN_KEY|ELEV_NUM".
struct SweepDataCache {
    entries: HashMap<String, CachedSweepData>,
    insertion_order: Vec<String>,
    max_entries: usize,
}

impl SweepDataCache {
    fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            insertion_order: Vec::new(),
            max_entries,
        }
    }

    fn insert(&mut self, key: String, data: CachedSweepData) {
        if self.entries.contains_key(&key) {
            // Move to end of insertion order
            self.insertion_order.retain(|k| k != &key);
        } else if self.entries.len() >= self.max_entries {
            // Evict oldest
            if let Some(oldest) = self.insertion_order.first().cloned() {
                self.entries.remove(&oldest);
                self.insertion_order.remove(0);
            }
        }
        self.entries.insert(key.clone(), data);
        self.insertion_order.push(key);
    }

    fn get(&self, key: &str) -> Option<&CachedSweepData> {
        self.entries.get(key)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.insertion_order.clear();
    }
}

// ---------------------------------------------------------------------------
// PrevSweepAction
// ---------------------------------------------------------------------------

/// Action to take for the previous-sweep GPU texture.
#[allow(dead_code)]
pub(crate) enum PrevSweepAction {
    /// Previous sweep data is already loaded in GPU — no action needed.
    AlreadyLoaded,
    /// Load from cache into GPU.
    UploadFromCache(String),
    /// Request a decode from the worker.
    FetchFromWorker {
        scan_key: String,
        elevation_number: u8,
        product: String,
    },
    /// Clear the previous sweep (no suitable prev exists).
    Clear,
}

// ---------------------------------------------------------------------------
// PlaybackManager
// ---------------------------------------------------------------------------

/// Manages the sweep data cache and previous-sweep resolution for sweep
/// animation. Lives on `WorkbenchApp` and replaces the old `sweep_cache`
/// and `pending_prev_sweep_key` fields.
pub(crate) struct PlaybackManager {
    sweep_cache: SweepDataCache,
    pending_prev_sweep_key: Option<String>,
    /// Cached identity of the last resolved previous sweep
    /// (scan_key, elev_num, product). If unchanged between frames,
    /// `resolve_prev_sweep` can skip work. Includes product so a product
    /// change invalidates the cache and re-resolves the prev texture.
    cached_prev_identity: Option<(String, u8, String)>,
}

impl PlaybackManager {
    pub fn new() -> Self {
        Self {
            sweep_cache: SweepDataCache::new(4),
            pending_prev_sweep_key: None,
            cached_prev_identity: None,
        }
    }

    /// Insert decoded sweep data into the cache.
    pub fn cache_sweep(&mut self, key: String, data: CachedSweepData) {
        self.sweep_cache.insert(key, data);
    }

    /// Get cached sweep data.
    pub fn get_cached_sweep(&self, key: &str) -> Option<&CachedSweepData> {
        self.sweep_cache.get(key)
    }

    /// Clear the sweep cache and invalidate prev-sweep resolution cache.
    pub fn clear_cache(&mut self) {
        self.sweep_cache.clear();
        self.cached_prev_identity = None;
    }

    /// Get the pending prev sweep key.
    pub fn pending_prev_sweep_key(&self) -> Option<&str> {
        self.pending_prev_sweep_key.as_deref()
    }

    /// Set the pending prev sweep key.
    pub fn set_pending_prev_sweep_key(&mut self, key: Option<String>) {
        self.pending_prev_sweep_key = key;
    }

    /// Determine what the previous sweep should be for sweep animation.
    ///
    /// Returns the prev sweep identity `(scan_key_ts, elev_num, elev_deg, start, end)`
    /// or `None` if no previous sweep exists.
    pub fn find_prev_sweep(
        timeline: &RadarTimeline,
        playback_ts: f64,
        displayed_elev: u8,
        is_auto: bool,
        max_scan_age: f64,
    ) -> Option<(i64, u8, f32, f64, f64)> {
        let current_scan = timeline.find_recent_scan(playback_ts, max_scan_age)?;

        let sweep_to_info = |scan_key_ts: f64, s: &Sweep| {
            (
                scan_key_ts as i64,
                s.elevation_number,
                s.elevation,
                s.start_time,
                s.end_time,
            )
        };

        if !is_auto {
            // Fixed: same elevation from the previous scan
            let prev_scan = timeline.find_previous_scan(playback_ts, max_scan_age);
            prev_scan.and_then(|ps| {
                ps.sweeps
                    .iter()
                    .find(|s| s.elevation_number == displayed_elev)
                    .map(|s| sweep_to_info(ps.key_timestamp, s))
            })
        } else {
            // Latest: previous sweep in time order within the same scan
            let sweep_idx = current_scan
                .sweeps
                .iter()
                .position(|s| s.elevation_number == displayed_elev);
            match sweep_idx {
                Some(idx) if idx > 0 => {
                    let prev = &current_scan.sweeps[idx - 1];
                    Some(sweep_to_info(current_scan.key_timestamp, prev))
                }
                _ => {
                    // First sweep in scan (or not found) — previous scan's last sweep
                    let prev_scan = timeline.find_previous_scan(playback_ts, max_scan_age);
                    prev_scan
                        .and_then(|ps| ps.sweeps.last().map(|s| sweep_to_info(ps.key_timestamp, s)))
                }
            }
        }
    }

    /// Invalidate the cached prev-sweep identity (call on scan or elevation change).
    #[allow(dead_code)]
    pub fn invalidate_prev_cache(&mut self) {
        self.cached_prev_identity = None;
    }

    /// Determine what action to take for the previous sweep texture.
    ///
    /// `current_gpu_prev_id` is the sweep ID currently loaded in the GPU's
    /// previous-sweep slot (from `renderer.prev_sweep_id()`).
    pub fn resolve_prev_sweep(
        &mut self,
        prev_scan_key: &str,
        prev_elev_num: u8,
        current_gpu_prev_id: Option<&str>,
        product: &str,
    ) -> PrevSweepAction {
        // Fast path: if the identity hasn't changed, nothing to do
        let new_identity = (
            prev_scan_key.to_string(),
            prev_elev_num,
            product.to_string(),
        );
        if self.cached_prev_identity.as_ref() == Some(&new_identity) {
            let desired_prev_id = sweep_cache_key(prev_scan_key, prev_elev_num, product);
            if current_gpu_prev_id == Some(desired_prev_id.as_str()) {
                return PrevSweepAction::AlreadyLoaded;
            }
        }
        self.cached_prev_identity = Some(new_identity);

        let desired_prev_id = sweep_cache_key(prev_scan_key, prev_elev_num, product);

        // Check if the GPU already has the right data
        if current_gpu_prev_id == Some(desired_prev_id.as_str()) {
            return PrevSweepAction::AlreadyLoaded;
        }

        // Try to load from cache
        if self.sweep_cache.get(&desired_prev_id).is_some() {
            self.pending_prev_sweep_key = None;
            return PrevSweepAction::UploadFromCache(desired_prev_id);
        }

        // Not in cache — request a decode, but only if we haven't already requested this key
        if self.pending_prev_sweep_key.as_deref() == Some(&desired_prev_id) {
            return PrevSweepAction::AlreadyLoaded; // already in flight
        }

        self.pending_prev_sweep_key = Some(desired_prev_id);
        PrevSweepAction::FetchFromWorker {
            scan_key: prev_scan_key.to_string(),
            elevation_number: prev_elev_num,
            product: product.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions (elevation helpers)
// ---------------------------------------------------------------------------

/// Find the best elevation number for a scan given the playback position.
///
/// In Fixed mode, filters by exact elevation_number match (no angle tolerance),
/// then picks the most recent sweep that has started. This eliminates
/// SAILS/MRLE ambiguity where CS and CD sweeps share the same angle.
pub(crate) fn best_elevation_at_playback(
    elevation_selection: &crate::state::ElevationSelection,
    scan: &Scan,
    playback_ts: f64,
    available_elevations: &[u8],
) -> Option<u8> {
    match elevation_selection {
        crate::state::ElevationSelection::Fixed {
            elevation_number, ..
        } => {
            // Filter sweeps by exact elevation_number match
            // then filter to those that have started (start_time <= playback_ts)
            // pick the one with the latest start_time (most recent instance).
            // Returns None when the selected elevation has no sweep in this scan,
            // so callers can clear display rather than send a doomed render.
            scan.sweeps
                .iter()
                .filter(|s| s.elevation_number == *elevation_number)
                .filter(|s| s.start_time <= playback_ts)
                .max_by(|a, b| {
                    a.start_time
                        .partial_cmp(&b.start_time)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|s| s.elevation_number)
        }
        crate::state::ElevationSelection::Latest => Some(most_recent_sweep_elevation(
            scan,
            playback_ts,
            available_elevations.first().copied().unwrap_or(1),
        )),
    }
}

/// Find the most recent sweep (any elevation) at or before the playback position.
///
/// Used by MostRecent render mode to always show the latest available data
/// regardless of elevation.
pub(crate) fn most_recent_sweep_elevation(scan: &Scan, playback_ts: f64, fallback: u8) -> u8 {
    scan.sweeps
        .iter()
        .filter(|s| s.start_time <= playback_ts)
        .max_by(|a, b| {
            a.start_time
                .partial_cmp(&b.start_time)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|s| s.elevation_number)
        .unwrap_or(fallback)
}

/// Build the elevation list from a scan's VCP data (extracted, static, or sweep-based).
pub(crate) fn build_elevation_list(scan: &Scan) -> Vec<crate::state::ElevationListEntry> {
    let products_for = |elev_num: u8| -> Vec<String> {
        scan.sweeps
            .iter()
            .find(|s| s.elevation_number == elev_num)
            .map(|s| s.available_products.clone())
            .unwrap_or_default()
    };

    // 1. Prefer extracted VCP pattern (has waveform, SAILS, MRLE info)
    if let Some(ref pattern) = scan.vcp_pattern {
        if !pattern.elevations.is_empty() {
            return pattern
                .elevations
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let elevation_number = (i + 1) as u8;
                    crate::state::ElevationListEntry {
                        elevation_number,
                        angle: e.angle,
                        waveform: e.waveform.clone(),
                        is_sails: e.is_sails,
                        is_mrle: e.is_mrle,
                        available_products: products_for(elevation_number),
                    }
                })
                .collect();
        }
    }

    // 2. Fall back to static VCP definition
    if let Some(def) = crate::state::get_vcp_definition(scan.vcp) {
        return def
            .elevations
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let elevation_number = (i + 1) as u8;
                crate::state::ElevationListEntry {
                    elevation_number,
                    angle: e.angle,
                    waveform: e.waveform.to_string(),
                    is_sails: false,
                    is_mrle: false,
                    available_products: products_for(elevation_number),
                }
            })
            .collect();
    }

    // 3. Fall back to sweep metadata
    scan.sweeps
        .iter()
        .map(|s| crate::state::ElevationListEntry {
            elevation_number: s.elevation_number,
            angle: s.elevation,
            waveform: String::new(),
            is_sails: false,
            is_mrle: false,
            available_products: s.available_products.clone(),
        })
        .collect()
}
