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
        scan_key: crate::data::ScanKey,
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
    /// or `None` if no previous sweep exists. `scan_key_ts` is fractional
    /// Unix seconds (preserving sub-second precision from the IDB key)
    /// rather than truncated `i64` — the comparison sites in main.rs read
    /// it against `displayed_scan_timestamp` which is also `f64`.
    pub fn find_prev_sweep(
        timeline: &RadarTimeline,
        playback_ts: f64,
        displayed_elev: u8,
        is_auto: bool,
        max_scan_age: f64,
    ) -> Option<(f64, u8, f32, f64, f64)> {
        let current_scan = timeline.find_recent_scan(playback_ts, max_scan_age)?;

        let sweep_to_info = |scan: &Scan, s: &Sweep| {
            (
                scan.key_timestamp,
                s.elevation_number,
                scan.display_angle(s),
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
                    .map(|s| sweep_to_info(ps, s))
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
                    Some(sweep_to_info(current_scan, prev))
                }
                _ => {
                    // First sweep in scan (or not found) — previous scan's last sweep
                    let prev_scan = timeline.find_previous_scan(playback_ts, max_scan_age);
                    prev_scan.and_then(|ps| ps.sweeps.last().map(|s| sweep_to_info(ps, s)))
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
        prev_scan_key: &crate::data::ScanKey,
        prev_elev_num: u8,
        current_gpu_prev_id: Option<&str>,
        product: &str,
    ) -> PrevSweepAction {
        // The composite cache id is still string-shaped (it embeds elev +
        // product); serialize the typed key once at the top.
        let prev_scan_key_storage = prev_scan_key.to_storage_key();

        // Fast path: if the identity hasn't changed, nothing to do
        let new_identity = (
            prev_scan_key_storage.clone(),
            prev_elev_num,
            product.to_string(),
        );
        if self.cached_prev_identity.as_ref() == Some(&new_identity) {
            let desired_prev_id = sweep_cache_key(&prev_scan_key_storage, prev_elev_num, product);
            if current_gpu_prev_id == Some(desired_prev_id.as_str()) {
                return PrevSweepAction::AlreadyLoaded;
            }
        }
        self.cached_prev_identity = Some(new_identity);

        let desired_prev_id = sweep_cache_key(&prev_scan_key_storage, prev_elev_num, product);

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
            scan_key: prev_scan_key.clone(),
            elevation_number: prev_elev_num,
            product: product.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions (elevation helpers)
// ---------------------------------------------------------------------------

/// Resolve the user's intent against the timeline into a fully-qualified
/// sweep identity, or `None` if the requested sweep is not available.
///
/// **No fuzzy fallback**: in `Fixed` mode, only an exact `elevation_number`
/// match qualifies; if the selected elevation has no started sweep in the
/// resolved scan, this returns `None`. Callers must blank the canvas
/// rather than fall back to a different elevation.
///
/// In `Latest` mode, the most recent started sweep at or before the
/// playback position is selected (any elevation).
///
/// Product availability: the sweep's `cached_products` must list the
/// requested product. Empty `cached_products` is treated as "nothing
/// available" — current ingest never writes such an entry, and old IDB
/// data that still contains them is intentionally rejected here rather
/// than driving a worker render that's guaranteed to fail.
pub(crate) fn resolve_active_sweep_target(
    site_id: &str,
    playback_position: f64,
    elevation_selection: &crate::state::ElevationSelection,
    product: crate::state::RadarProduct,
    timeline: &RadarTimeline,
    max_scan_age_secs: f64,
) -> Option<crate::state::SweepIdentity> {
    let scan = timeline.find_recent_scan(playback_position, max_scan_age_secs)?;
    let product_str = product.to_worker_string();

    let sweep = match elevation_selection {
        crate::state::ElevationSelection::Fixed {
            elevation_number, ..
        } => scan
            .sweeps
            .iter()
            .filter(|s| s.elevation_number == *elevation_number)
            .filter(|s| s.start_time <= playback_position)
            .max_by(|a, b| {
                a.start_time
                    .partial_cmp(&b.start_time)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })?,
        crate::state::ElevationSelection::Latest => scan
            .sweeps
            .iter()
            .filter(|s| s.start_time <= playback_position)
            .max_by(|a, b| {
                a.start_time
                    .partial_cmp(&b.start_time)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })?,
    };

    // Require an exact product match. Empty `cached_products` is treated as
    // "nothing stored for this sweep" — reject rather than optimistically
    // ask the worker for a blob the index never claimed.
    if !sweep.cached_products.iter().any(|p| p == product_str) {
        return None;
    }

    Some(crate::state::SweepIdentity::new(
        crate::data::ScanKey::from_secs_f64(site_id, scan.key_timestamp),
        sweep.elevation_number,
        product_str,
    ))
}

/// What the canvas should display this frame.
///
/// Mode-agnostic: live and archive are *sources* feeding this decision, not
/// mutually-exclusive owners of the canvas. The canvas only ever shows one
/// sweep, so the live↔cache merge that [`crate::state::timeline_view`] does
/// per-volume collapses here to a single precedence choice — the same rule as
/// [`crate::state::timeline_view::merge_cached_into_live`]: the live session's
/// in-progress cut wins for *its own* elevation; cached data answers for every
/// other (elevation, position) the user can select.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DesiredDisplay {
    /// Render the in-progress live partial for the actively-collecting cut.
    /// Produced by the chunk-ingest → `render_live` → `LiveDecoded` path.
    LivePartial { elevation_number: u8 },
    /// Render a finalized/cached sweep blob (worker decode → `Decoded` upload).
    Cached(crate::state::SweepIdentity),
    /// Nothing matches the user's intent. Callers must not blindly blank: a
    /// valid live partial may legitimately hold the canvas (see the caller's
    /// `Blank` handling).
    Blank,
}

/// Resolve what the canvas should display, merging the live in-progress
/// accumulator with the cached timeline.
///
/// Precedence (mirrors `merge_cached_into_live`):
/// 1. If the live session is actively collecting cut `E` in volume `V`, and
///    the user's intent points at *that same cut* (the resolved scan is `V`
///    and the selected elevation is `E`, or selection is `Latest`), the live
///    partial wins → [`DesiredDisplay::LivePartial`].
/// 2. Otherwise the cache answers: [`resolve_active_sweep_target`] →
///    [`DesiredDisplay::Cached`] when a sweep matches, else
///    [`DesiredDisplay::Blank`].
///
/// `live_cut` is `Some((elevation, anchor_key_ms))` only while streaming and
/// only when the live volume is fully known — pass
/// `LiveModeState::current_in_progress_elevation` zipped with the anchor's
/// `scan_key.scan_start.0` (rounded millis, matching how `timeline_view`
/// identifies the live volume). `None` collapses the resolver to the cache
/// path.
pub(crate) fn resolve_desired_display(
    site_id: &str,
    playback_position: f64,
    elevation_selection: &crate::state::ElevationSelection,
    product: crate::state::RadarProduct,
    timeline: &RadarTimeline,
    max_scan_age_secs: f64,
    live_cut: Option<(u8, i64)>,
) -> DesiredDisplay {
    // Does the user's intent point at the cut the live session is actively
    // collecting? Only then does the live partial win — anything else reads
    // from cache, which is what makes a completed cut visible during live.
    if let Some((elev, anchor_ms)) = live_cut {
        if let Some(scan) = timeline.find_recent_scan(playback_position, max_scan_age_secs) {
            let scan_ms = scan.key_ms();
            let intent_is_live_cut = scan_ms == anchor_ms
                && match elevation_selection {
                    crate::state::ElevationSelection::Fixed {
                        elevation_number, ..
                    } => *elevation_number == elev,
                    // The collecting cut *is* the latest, so `Latest` always
                    // points at it while the resolved scan is the live volume.
                    crate::state::ElevationSelection::Latest => true,
                };
            if intent_is_live_cut {
                return DesiredDisplay::LivePartial {
                    elevation_number: elev,
                };
            }
        }
    }

    match resolve_active_sweep_target(
        site_id,
        playback_position,
        elevation_selection,
        product,
        timeline,
        max_scan_age_secs,
    ) {
        Some(identity) => DesiredDisplay::Cached(identity),
        None => DesiredDisplay::Blank,
    }
}

/// Build the elevation list from a scan's VCP data (extracted, static, or sweep-based).
///
/// The displayed `angle` is the VCP target (commanded) angle — the cut's
/// identity (e.g. "0.5°") — not the encoder's measured average, which
/// wobbles a few hundredths of a degree per sweep. We only fall back to
/// the measured value when no VCP info is available at all.
pub(crate) fn build_elevation_list(scan: &Scan) -> Vec<crate::state::ElevationListEntry> {
    let products_for = |elev_num: u8| -> Vec<String> {
        scan.sweeps
            .iter()
            .find(|s| s.elevation_number == elev_num)
            .map(|s| s.cached_products.clone())
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
                        cached_products: products_for(elevation_number),
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
                    cached_products: products_for(elevation_number),
                }
            })
            .collect();
    }

    // 3. Fall back to sweep metadata (no VCP available)
    scan.sweeps
        .iter()
        .map(|s| crate::state::ElevationListEntry {
            elevation_number: s.elevation_number,
            angle: s.elevation,
            waveform: String::new(),
            is_sails: false,
            is_mrle: false,
            cached_products: s.cached_products.clone(),
        })
        .collect()
}

/// Build an elevation list from a live VCP pattern (no completed scan
/// yet). `cached_products` is left empty; the right panel treats
/// that as "unknown — allow" so all products are selectable until a
/// completed sweep narrows it down.
pub(crate) fn build_elevation_list_from_vcp(
    vcp: &crate::data::keys::ExtractedVcp,
) -> Vec<crate::state::ElevationListEntry> {
    vcp.elevations
        .iter()
        .enumerate()
        .map(|(i, e)| crate::state::ElevationListEntry {
            elevation_number: (i + 1) as u8,
            angle: e.angle,
            waveform: e.waveform.clone(),
            is_sails: e.is_sails,
            is_mrle: e.is_mrle,
            cached_products: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::radar_data::{Radial, Scan, Sweep};
    use crate::state::{ElevationSelection, RadarProduct};
    use wasm_bindgen_test::wasm_bindgen_test;

    const MAX_AGE: f64 = 15.0 * 60.0;

    fn sweep_with(
        start: f64,
        end: f64,
        elev: f32,
        elev_num: u8,
        cached_products: Vec<&str>,
    ) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: elev,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::<Radial>::new(),
            cached_products: cached_products.into_iter().map(String::from).collect(),
        }
    }

    fn scan_with(start: f64, end: f64, key_ts: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: start,
            end_time: end,
            key_timestamp: key_ts,
            vcp: 215,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn timeline_with(scans: Vec<Scan>) -> RadarTimeline {
        RadarTimeline { scans }
    }

    fn fixed(elev_num: u8) -> ElevationSelection {
        ElevationSelection::Fixed {
            elevation_number: elev_num,
            angle: 0.5,
        }
    }

    #[wasm_bindgen_test]
    fn resolve_returns_exact_match_when_elevation_present() {
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1040.0,
            1000.0,
            vec![
                sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"]),
                sweep_with(1010.0, 1020.0, 0.9, 2, vec!["reflectivity"]),
            ],
        )]);
        let id = resolve_active_sweep_target(
            "KDMX",
            1015.0,
            &fixed(2),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
        )
        .expect("expected Some");
        assert_eq!(id.elevation_number, 2);
        assert_eq!(id.product, "reflectivity");
        assert_eq!(id.site_id(), "KDMX");
        assert!((id.scan_timestamp_secs() - 1000.0).abs() < 1e-3);
    }

    #[wasm_bindgen_test]
    fn resolve_returns_none_when_elevation_missing() {
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1020.0,
            1000.0,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        )]);
        let id = resolve_active_sweep_target(
            "KDMX",
            1015.0,
            &fixed(5),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
        );
        assert!(id.is_none(), "elevation 5 not in scan: should be None");
    }

    #[wasm_bindgen_test]
    fn resolve_returns_none_when_product_unavailable() {
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1020.0,
            1000.0,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        )]);
        let id = resolve_active_sweep_target(
            "KDMX",
            1005.0,
            &fixed(1),
            RadarProduct::Velocity,
            &tl,
            MAX_AGE,
        );
        assert!(
            id.is_none(),
            "velocity not in cached_products: should be None"
        );
    }

    #[wasm_bindgen_test]
    fn resolve_rejects_sweep_with_empty_cached_products() {
        // Empty `cached_products` means "no blobs stored for this sweep" —
        // resolver must NOT optimistically ask the worker for one. Prior
        // behaviour treated empty as "unknown — allow", which drove an
        // infinite worker-render retry loop on phantom legacy entries.
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1020.0,
            1000.0,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec![])],
        )]);
        let id = resolve_active_sweep_target(
            "KDMX",
            1005.0,
            &fixed(1),
            RadarProduct::Velocity,
            &tl,
            MAX_AGE,
        );
        assert!(
            id.is_none(),
            "empty cached_products must be rejected, not silently allowed",
        );
    }

    #[wasm_bindgen_test]
    fn resolve_returns_none_before_sweep_starts() {
        // Sweep 1 starts at 1000; playback at 999.999 — before any started sweep.
        let tl = timeline_with(vec![scan_with(
            999.0,
            1020.0,
            999.0,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        )]);
        let id = resolve_active_sweep_target(
            "KDMX",
            999.999,
            &fixed(1),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
        );
        assert!(id.is_none(), "no sweep started yet: should be None");
    }

    #[wasm_bindgen_test]
    fn resolve_latest_picks_most_recent_started_sweep() {
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1040.0,
            1000.0,
            vec![
                sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"]),
                sweep_with(1010.0, 1020.0, 0.9, 2, vec!["reflectivity"]),
                sweep_with(1020.0, 1030.0, 1.3, 3, vec!["reflectivity"]),
            ],
        )]);
        let id = resolve_active_sweep_target(
            "KDMX",
            1015.0,
            &ElevationSelection::Latest,
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
        )
        .expect("expected Some");
        // Sweep 2 (elev_num=2) is the most recent that has started by ts=1015.
        assert_eq!(id.elevation_number, 2);
    }

    #[wasm_bindgen_test]
    fn resolve_returns_none_when_no_scan_in_window() {
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1020.0,
            1000.0,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        )]);
        // Playback at 5000 — far beyond MAX_AGE from scan start at 1000.
        let id = resolve_active_sweep_target(
            "KDMX",
            5000.0,
            &fixed(1),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
        );
        assert!(id.is_none());
    }

    #[wasm_bindgen_test]
    fn resolve_picks_most_recent_when_elevation_repeats_in_scan() {
        // SAILS-style: two sweeps at the same elevation_number? Not possible —
        // elevation_number is unique per scan. But a low-level rescan at a
        // *different* elevation_number with the same angle is the SAILS case.
        // Here we verify Fixed mode picks the most recent matching number.
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1040.0,
            1000.0,
            vec![
                sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"]),
                sweep_with(1010.0, 1020.0, 0.9, 2, vec!["reflectivity"]),
                sweep_with(1020.0, 1030.0, 0.5, 3, vec!["reflectivity"]),
            ],
        )]);
        // Selecting elev_num=1 — only one sweep matches; it must be returned.
        let id = resolve_active_sweep_target(
            "KDMX",
            1025.0,
            &fixed(1),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
        )
        .expect("expected Some");
        assert_eq!(id.elevation_number, 1);
        // Selecting elev_num=3 (the SAILS rescan at the same angle) — distinct number.
        let id = resolve_active_sweep_target(
            "KDMX",
            1025.0,
            &fixed(3),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
        )
        .expect("expected Some");
        assert_eq!(id.elevation_number, 3);
    }

    #[wasm_bindgen_test]
    fn resolve_uses_scan_key_timestamp_not_start_time() {
        // key_timestamp is the canonical scan identity (matches IDB key);
        // the resolver must encode that, not the (possibly-adjusted) start_time.
        let tl = timeline_with(vec![scan_with(
            999.5, // start_time adjusted earlier than key
            1020.0,
            1000.123, // sub-second key timestamp
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        )]);
        let id = resolve_active_sweep_target(
            "KDMX",
            1005.0,
            &fixed(1),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
        )
        .expect("expected Some");
        // ScanKey round-trips through UnixMillis (ms precision).
        assert!((id.scan_timestamp_secs() - 1000.123).abs() < 1e-3);
    }

    // ── resolve_desired_display (live↔cache merge precedence) ───────────────

    /// A single cached scan keyed at 1000s with elev 1 cached as reflectivity.
    /// Anchor key-millis for that volume is 1_000_000.
    fn live_scenario_timeline() -> RadarTimeline {
        timeline_with(vec![scan_with(
            1000.0,
            1040.0,
            1000.0,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        )])
    }

    #[wasm_bindgen_test]
    fn live_cut_wins_for_its_own_elevation() {
        // Live is collecting elev 2 in the live volume; user selects elev 2.
        let tl = live_scenario_timeline();
        let d = resolve_desired_display(
            "KDMX",
            1005.0,
            &fixed(2),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
            Some((2, 1_000_000)),
        );
        assert_eq!(
            d,
            DesiredDisplay::LivePartial {
                elevation_number: 2
            }
        );
    }

    #[wasm_bindgen_test]
    fn cached_wins_for_completed_cut_during_live() {
        // The reload bug: live collecting elev 2, user viewing completed elev 1.
        // The completed cut must resolve to its cached blob, not blank.
        let tl = live_scenario_timeline();
        let d = resolve_desired_display(
            "KDMX",
            1005.0,
            &fixed(1),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
            Some((2, 1_000_000)),
        );
        match d {
            DesiredDisplay::Cached(id) => assert_eq!(id.elevation_number, 1),
            other => panic!("expected Cached(elev 1), got {:?}", other),
        }
    }

    #[wasm_bindgen_test]
    fn latest_selects_live_partial_during_live() {
        // `Latest` always points at the actively-collecting cut while the
        // resolved scan is the live volume.
        let tl = live_scenario_timeline();
        let d = resolve_desired_display(
            "KDMX",
            1005.0,
            &ElevationSelection::Latest,
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
            Some((3, 1_000_000)),
        );
        assert_eq!(
            d,
            DesiredDisplay::LivePartial {
                elevation_number: 3
            }
        );
    }

    #[wasm_bindgen_test]
    fn cached_when_not_live() {
        let tl = live_scenario_timeline();
        let d = resolve_desired_display(
            "KDMX",
            1005.0,
            &fixed(1),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
            None,
        );
        assert!(matches!(d, DesiredDisplay::Cached(_)));
    }

    #[wasm_bindgen_test]
    fn blank_when_no_live_and_no_cache() {
        let tl = timeline_with(vec![]);
        let d = resolve_desired_display(
            "KDMX",
            1005.0,
            &fixed(1),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
            None,
        );
        assert_eq!(d, DesiredDisplay::Blank);
    }

    #[wasm_bindgen_test]
    fn live_cut_requires_matching_volume() {
        // Live is collecting elev 1, but its anchor volume (keyed at 2000s)
        // differs from the scan the user is parked on (1000s). The partial
        // must NOT paint onto the wrong volume — fall through to the cache.
        let tl = live_scenario_timeline();
        let d = resolve_desired_display(
            "KDMX",
            1005.0,
            &fixed(1),
            RadarProduct::Reflectivity,
            &tl,
            MAX_AGE,
            Some((1, 2_000_000)),
        );
        match d {
            DesiredDisplay::Cached(id) => assert_eq!(id.elevation_number, 1),
            other => panic!("expected Cached(elev 1), got {:?}", other),
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};
    use crate::data::ScanKey;
    use crate::state::radar_data::{Radial, Scan, Sweep};
    use wasm_bindgen_test::wasm_bindgen_test;

    const MAX_AGE: f64 = 15.0 * 60.0;

    // ── builders (re-declared; can't reach sibling module's private helpers) ──

    fn sweep_with(
        start: f64,
        end: f64,
        elev: f32,
        elev_num: u8,
        cached_products: Vec<&str>,
    ) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: elev,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::<Radial>::new(),
            cached_products: cached_products.into_iter().map(String::from).collect(),
        }
    }

    fn scan_with(start: f64, end: f64, key_ts: f64, vcp: u16, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: start,
            end_time: end,
            key_timestamp: key_ts,
            vcp,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn timeline_with(scans: Vec<Scan>) -> RadarTimeline {
        RadarTimeline { scans }
    }

    fn dummy_sweep_data(product: &str) -> CachedSweepData {
        CachedSweepData {
            gate_values: Vec::new(),
            azimuths: Vec::new(),
            azimuth_count: 0,
            gate_count: 0,
            first_gate_range_km: 0.0,
            gate_interval_km: 0.25,
            max_range_km: 460.0,
            offset: 0.0,
            scale: 1.0,
            azimuth_spacing_deg: 1.0,
            radial_times: Vec::new(),
            sweep_start_secs: 0.0,
            sweep_end_secs: 0.0,
            product: product.to_string(),
        }
    }

    fn extracted_elev(
        angle: f32,
        waveform: &str,
        is_sails: bool,
        is_mrle: bool,
    ) -> ExtractedVcpElevation {
        ExtractedVcpElevation {
            angle,
            waveform: waveform.to_string(),
            prf_number: 1,
            is_sails,
            is_mrle,
            is_base_tilt: false,
            azimuth_rate: None,
        }
    }

    // ── sweep_cache_key formatting ──────────────────────────────────────────

    #[wasm_bindgen_test]
    fn sweep_cache_key_concatenates_with_pipes() {
        // Pure string formatting: "SCAN|ELEV|PRODUCT".
        assert_eq!(
            sweep_cache_key("KDMX|1700000000000", 3, "reflectivity"),
            "KDMX|1700000000000|3|reflectivity"
        );
        // Elevation 0 and empty product still produce the trailing-pipe shape.
        assert_eq!(sweep_cache_key("S|0", 0, ""), "S|0|0|");
    }

    // ── LRU SweepDataCache (via PlaybackManager) ────────────────────────────

    #[wasm_bindgen_test]
    fn cache_roundtrips_inserted_entry() {
        let mut pm = PlaybackManager::new();
        pm.cache_sweep("k1".to_string(), dummy_sweep_data("reflectivity"));
        let got = pm.get_cached_sweep("k1").expect("inserted entry present");
        assert_eq!(got.product, "reflectivity");
        assert!(pm.get_cached_sweep("missing").is_none());
    }

    #[wasm_bindgen_test]
    fn cache_evicts_oldest_past_capacity() {
        // Capacity is 4 (PlaybackManager::new). Insert 5 distinct keys;
        // the first ("k0") must be evicted, the rest retained.
        let mut pm = PlaybackManager::new();
        for i in 0..5 {
            pm.cache_sweep(format!("k{i}"), dummy_sweep_data("p"));
        }
        assert!(
            pm.get_cached_sweep("k0").is_none(),
            "oldest must be evicted"
        );
        for i in 1..5 {
            assert!(
                pm.get_cached_sweep(&format!("k{i}")).is_some(),
                "k{i} retained"
            );
        }
    }

    #[wasm_bindgen_test]
    fn cache_reinsert_refreshes_recency_changing_eviction() {
        // Fill to capacity, re-touch the oldest so it moves to the tail, then
        // overflow by one. The *new* oldest (k1) is evicted, not k0.
        let mut pm = PlaybackManager::new();
        for i in 0..4 {
            pm.cache_sweep(format!("k{i}"), dummy_sweep_data("p"));
        }
        // Re-insert k0 — moves it to most-recent, updates its value.
        pm.cache_sweep("k0".to_string(), dummy_sweep_data("velocity"));
        assert_eq!(pm.get_cached_sweep("k0").unwrap().product, "velocity");
        // Now insert a 5th distinct key → evict the current oldest = k1.
        pm.cache_sweep("k4".to_string(), dummy_sweep_data("p"));
        assert!(
            pm.get_cached_sweep("k1").is_none(),
            "k1 is now oldest, evicted"
        );
        assert!(
            pm.get_cached_sweep("k0").is_some(),
            "k0 refreshed, retained"
        );
        assert!(pm.get_cached_sweep("k4").is_some());
    }

    #[wasm_bindgen_test]
    fn clear_cache_empties_entries() {
        let mut pm = PlaybackManager::new();
        pm.cache_sweep("k1".to_string(), dummy_sweep_data("p"));
        pm.clear_cache();
        assert!(pm.get_cached_sweep("k1").is_none());
    }

    // ── pending_prev_sweep_key get/set ──────────────────────────────────────

    #[wasm_bindgen_test]
    fn pending_prev_key_get_set_roundtrip() {
        let mut pm = PlaybackManager::new();
        assert_eq!(pm.pending_prev_sweep_key(), None);
        pm.set_pending_prev_sweep_key(Some("abc".to_string()));
        assert_eq!(pm.pending_prev_sweep_key(), Some("abc"));
        pm.set_pending_prev_sweep_key(None);
        assert_eq!(pm.pending_prev_sweep_key(), None);
    }

    // ── find_prev_sweep ─────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn find_prev_returns_none_when_no_current_scan() {
        // No scan within window → None for both modes.
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1020.0,
            1000.0,
            215,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        )]);
        // Playback far beyond MAX_AGE.
        assert!(PlaybackManager::find_prev_sweep(&tl, 9999.0, 1, true, MAX_AGE).is_none());
        assert!(PlaybackManager::find_prev_sweep(&tl, 9999.0, 1, false, MAX_AGE).is_none());
    }

    #[wasm_bindgen_test]
    fn find_prev_auto_returns_previous_sweep_in_same_scan() {
        // Latest mode, displayed elev 2 sits at idx 1 (>0) → prev is sweep idx 0
        // (elev_num 1). display_angle falls to static VCP 215 → 0.5°.
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1040.0,
            1000.0,
            215,
            vec![
                sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"]),
                sweep_with(1010.0, 1020.0, 0.9, 2, vec!["reflectivity"]),
            ],
        )]);
        let (key_ts, elev_num, elev_deg, start, end) =
            PlaybackManager::find_prev_sweep(&tl, 1015.0, 2, true, MAX_AGE)
                .expect("prev sweep within same scan");
        assert!((key_ts - 1000.0).abs() < 1e-9);
        assert_eq!(elev_num, 1);
        assert!(
            (elev_deg - 0.5).abs() < 1e-4,
            "VCP-215 target angle for elev 1"
        );
        assert!((start - 1000.0).abs() < 1e-9);
        assert!((end - 1010.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn find_prev_auto_first_sweep_falls_back_to_previous_scans_last() {
        // Latest mode, displayed elev 1 is the FIRST sweep (idx 0) of the
        // current scan → fall back to the previous scan's last sweep.
        let prev_scan = scan_with(
            1000.0,
            1040.0,
            1000.0,
            215,
            vec![
                sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"]),
                sweep_with(1010.0, 1020.0, 0.9, 2, vec!["reflectivity"]),
            ],
        );
        let cur_scan = scan_with(
            2000.0,
            2040.0,
            2000.0,
            215,
            vec![sweep_with(2000.0, 2010.0, 0.5, 1, vec!["reflectivity"])],
        );
        let tl = timeline_with(vec![prev_scan, cur_scan]);
        // Generous max_age so the ~1000s gap to the prior scan isn't age-gated
        // (age gating is covered by its own test); this checks the fallback path.
        let (key_ts, elev_num, _deg, start, end) =
            PlaybackManager::find_prev_sweep(&tl, 2005.0, 1, true, 5_000.0)
                .expect("falls back to prior scan's last sweep");
        // Prev scan's LAST sweep = elev_num 2, 1010..1020.
        assert!((key_ts - 1000.0).abs() < 1e-9);
        assert_eq!(elev_num, 2);
        assert!((start - 1010.0).abs() < 1e-9);
        assert!((end - 1020.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn find_prev_auto_first_sweep_none_when_no_previous_scan() {
        // Latest mode, single scan, displayed elev is the first sweep →
        // no previous scan exists → None.
        let tl = timeline_with(vec![scan_with(
            1000.0,
            1040.0,
            1000.0,
            215,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        )]);
        assert!(PlaybackManager::find_prev_sweep(&tl, 1005.0, 1, true, MAX_AGE).is_none());
    }

    #[wasm_bindgen_test]
    fn find_prev_fixed_picks_same_elevation_from_previous_scan() {
        // Fixed mode: same elevation_number from the immediately-previous scan.
        let prev_scan = scan_with(
            1000.0,
            1040.0,
            1000.0,
            215,
            vec![
                sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"]),
                sweep_with(1010.0, 1020.0, 0.9, 2, vec!["reflectivity"]),
            ],
        );
        let cur_scan = scan_with(
            2000.0,
            2040.0,
            2000.0,
            215,
            vec![sweep_with(2000.0, 2010.0, 0.5, 1, vec!["reflectivity"])],
        );
        let tl = timeline_with(vec![prev_scan, cur_scan]);
        // Generous max_age so the ~1000s gap to the prior scan isn't age-gated.
        let (key_ts, elev_num, _deg, start, end) =
            PlaybackManager::find_prev_sweep(&tl, 2005.0, 2, false, 5_000.0)
                .expect("prev scan has elev 2");
        assert!((key_ts - 1000.0).abs() < 1e-9);
        assert_eq!(elev_num, 2);
        assert!((start - 1010.0).abs() < 1e-9);
        assert!((end - 1020.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn find_prev_fixed_none_when_previous_scan_lacks_elevation() {
        // Fixed mode: prev scan exists but has no matching elevation_number.
        let prev_scan = scan_with(
            1000.0,
            1040.0,
            1000.0,
            215,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        );
        let cur_scan = scan_with(
            2000.0,
            2040.0,
            2000.0,
            215,
            vec![sweep_with(2000.0, 2010.0, 0.5, 1, vec!["reflectivity"])],
        );
        let tl = timeline_with(vec![prev_scan, cur_scan]);
        // Requesting elev 7 which the prev scan does not have.
        assert!(PlaybackManager::find_prev_sweep(&tl, 2005.0, 7, false, MAX_AGE).is_none());
    }

    #[wasm_bindgen_test]
    fn find_prev_auto_elevation_not_found_uses_previous_scan_last() {
        // Latest mode: displayed elev not present in current scan at all →
        // position() is None → fall back to previous scan's last sweep.
        let prev_scan = scan_with(
            1000.0,
            1040.0,
            1000.0,
            215,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        );
        let cur_scan = scan_with(
            2000.0,
            2040.0,
            2000.0,
            215,
            vec![sweep_with(2000.0, 2010.0, 0.5, 1, vec!["reflectivity"])],
        );
        let tl = timeline_with(vec![prev_scan, cur_scan]);
        // elev 9 absent from current scan → prev scan's last sweep (elev 1).
        // Generous max_age so the ~1000s gap to the prior scan isn't age-gated.
        let (key_ts, elev_num, _deg, _s, _e) =
            PlaybackManager::find_prev_sweep(&tl, 2005.0, 9, true, 5_000.0)
                .expect("falls back to prior scan last sweep");
        assert!((key_ts - 1000.0).abs() < 1e-9);
        assert_eq!(elev_num, 1);
    }

    // ── resolve_prev_sweep (PrevSweepAction state machine) ───────────────────

    #[wasm_bindgen_test]
    fn resolve_prev_already_loaded_when_gpu_matches() {
        let mut pm = PlaybackManager::new();
        let key = ScanKey::from_secs("KDMX", 1700);
        let desired = sweep_cache_key(&key.to_storage_key(), 2, "reflectivity");
        let action = pm.resolve_prev_sweep(&key, 2, Some(desired.as_str()), "reflectivity");
        assert!(matches!(action, PrevSweepAction::AlreadyLoaded));
    }

    #[wasm_bindgen_test]
    fn resolve_prev_upload_from_cache_when_present() {
        let mut pm = PlaybackManager::new();
        let key = ScanKey::from_secs("KDMX", 1700);
        let desired = sweep_cache_key(&key.to_storage_key(), 2, "reflectivity");
        pm.cache_sweep(desired.clone(), dummy_sweep_data("reflectivity"));
        // GPU has something else loaded (None) → upload from cache.
        let action = pm.resolve_prev_sweep(&key, 2, None, "reflectivity");
        match action {
            PrevSweepAction::UploadFromCache(k) => assert_eq!(k, desired),
            other => panic!("expected UploadFromCache, got {:?}", debug_action(&other)),
        }
        // Upload path clears any pending key.
        assert_eq!(pm.pending_prev_sweep_key(), None);
    }

    #[wasm_bindgen_test]
    fn resolve_prev_fetch_from_worker_when_not_cached() {
        let mut pm = PlaybackManager::new();
        let key = ScanKey::from_secs("KDMX", 1700);
        let action = pm.resolve_prev_sweep(&key, 4, None, "velocity");
        match action {
            PrevSweepAction::FetchFromWorker {
                elevation_number,
                product,
                ..
            } => {
                assert_eq!(elevation_number, 4);
                assert_eq!(product, "velocity");
            }
            other => panic!("expected FetchFromWorker, got {:?}", debug_action(&other)),
        }
        // The fetch marks the desired key as pending (in-flight).
        let expected = sweep_cache_key(&key.to_storage_key(), 4, "velocity");
        assert_eq!(pm.pending_prev_sweep_key(), Some(expected.as_str()));
    }

    #[wasm_bindgen_test]
    fn resolve_prev_in_flight_returns_already_loaded() {
        let mut pm = PlaybackManager::new();
        let key = ScanKey::from_secs("KDMX", 1700);
        // First call → FetchFromWorker, sets pending.
        let _ = pm.resolve_prev_sweep(&key, 4, None, "velocity");
        // Second call with a different identity to bypass the identity
        // fast-path, then back — but simplest: invalidate identity and repeat
        // the same desired key. The pending key already equals desired, so the
        // resolver short-circuits to AlreadyLoaded (already in flight).
        pm.invalidate_prev_cache();
        let action = pm.resolve_prev_sweep(&key, 4, None, "velocity");
        assert!(
            matches!(action, PrevSweepAction::AlreadyLoaded),
            "request already in flight should report AlreadyLoaded"
        );
    }

    #[wasm_bindgen_test]
    fn resolve_prev_product_change_reresolves() {
        // Same scan + elevation, different product → identity differs, so the
        // cached fast-path does not apply and a fresh fetch is requested.
        let mut pm = PlaybackManager::new();
        let key = ScanKey::from_secs("KDMX", 1700);
        let _ = pm.resolve_prev_sweep(&key, 2, None, "reflectivity");
        let action = pm.resolve_prev_sweep(&key, 2, None, "velocity");
        match action {
            PrevSweepAction::FetchFromWorker { product, .. } => {
                assert_eq!(product, "velocity")
            }
            other => panic!(
                "expected FetchFromWorker for new product, got {:?}",
                debug_action(&other)
            ),
        }
    }

    // Local debug helper — PrevSweepAction has no Debug derive.
    fn debug_action(a: &PrevSweepAction) -> &'static str {
        match a {
            PrevSweepAction::AlreadyLoaded => "AlreadyLoaded",
            PrevSweepAction::UploadFromCache(_) => "UploadFromCache",
            PrevSweepAction::FetchFromWorker { .. } => "FetchFromWorker",
            PrevSweepAction::Clear => "Clear",
        }
    }

    // ── build_elevation_list ────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn build_elevation_list_prefers_extracted_pattern() {
        let mut scan = scan_with(
            1000.0,
            1040.0,
            1000.0,
            215,
            vec![
                sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"]),
                sweep_with(1010.0, 1020.0, 0.9, 2, vec!["velocity"]),
            ],
        );
        scan.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: vec![
                extracted_elev(0.48, "CS", false, false),
                extracted_elev(0.91, "CDW", true, false),
            ],
        });
        let list = build_elevation_list(&scan);
        assert_eq!(list.len(), 2);
        // Elevation numbers are 1-based index.
        assert_eq!(list[0].elevation_number, 1);
        assert_eq!(list[1].elevation_number, 2);
        // Angle comes from the extracted pattern, not the sweep measured value.
        assert!((list[0].angle - 0.48).abs() < 1e-4);
        assert!((list[1].angle - 0.91).abs() < 1e-4);
        assert_eq!(list[1].waveform, "CDW");
        assert!(list[1].is_sails);
        // products_for matches by elevation_number against the sweeps.
        assert_eq!(list[0].cached_products, vec!["reflectivity".to_string()]);
        assert_eq!(list[1].cached_products, vec!["velocity".to_string()]);
    }

    #[wasm_bindgen_test]
    fn build_elevation_list_falls_back_to_static_vcp() {
        // No extracted pattern, but vcp=215 has a static definition (14 cuts).
        let scan = scan_with(
            1000.0,
            1040.0,
            1000.0,
            215,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        );
        let list = build_elevation_list(&scan);
        // VCP 215 static table has many elevations (>= the 1 stored sweep).
        assert!(list.len() >= 14, "VCP-215 static table has 14+ cuts");
        assert_eq!(list[0].elevation_number, 1);
        // First static cut is 0.5° CS.
        assert!((list[0].angle - 0.5).abs() < 1e-4);
        assert_eq!(list[0].waveform, "CS");
        // Static fallback never marks SAILS/MRLE.
        assert!(!list[0].is_sails);
        assert!(!list[0].is_mrle);
        // products_for found the stored elev-1 sweep's product.
        assert_eq!(list[0].cached_products, vec!["reflectivity".to_string()]);
        // Elev 2 has no stored sweep → empty products.
        assert!(list[1].cached_products.is_empty());
    }

    #[wasm_bindgen_test]
    fn build_elevation_list_falls_back_to_sweep_metadata_for_unknown_vcp() {
        // Unknown VCP (0) → no static def → use sweep metadata directly.
        let scan = scan_with(
            1000.0,
            1040.0,
            1000.0,
            0,
            vec![
                sweep_with(1000.0, 1010.0, 0.53, 1, vec!["reflectivity"]),
                sweep_with(1010.0, 1020.0, 0.91, 2, vec!["velocity", "reflectivity"]),
            ],
        );
        let list = build_elevation_list(&scan);
        assert_eq!(list.len(), 2);
        // Angle is the per-sweep measured elevation, waveform empty.
        assert!((list[0].angle - 0.53).abs() < 1e-4);
        assert!((list[1].angle - 0.91).abs() < 1e-4);
        assert_eq!(list[0].waveform, "");
        assert_eq!(list[0].elevation_number, 1);
        assert_eq!(list[1].elevation_number, 2);
        assert_eq!(list[1].cached_products.len(), 2);
        assert!(!list[0].is_sails && !list[0].is_mrle);
    }

    #[wasm_bindgen_test]
    fn build_elevation_list_empty_pattern_skips_to_next_source() {
        // An extracted pattern with NO elevations must not win; falls through
        // to the static VCP 215 table.
        let mut scan = scan_with(
            1000.0,
            1040.0,
            1000.0,
            215,
            vec![sweep_with(1000.0, 1010.0, 0.5, 1, vec!["reflectivity"])],
        );
        scan.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: Vec::new(),
        });
        let list = build_elevation_list(&scan);
        assert!(
            list.len() >= 14,
            "empty pattern ignored → static VCP-215 used"
        );
        assert!((list[0].angle - 0.5).abs() < 1e-4);
    }

    // ── build_elevation_list_from_vcp ───────────────────────────────────────

    #[wasm_bindgen_test]
    fn build_elevation_list_from_vcp_maps_indices_and_empties_products() {
        let vcp = ExtractedVcp {
            number: 35,
            elevations: vec![
                extracted_elev(0.5, "CS", false, false),
                extracted_elev(1.5, "CDW", true, true),
                extracted_elev(2.4, "CDWO", false, true),
            ],
        };
        let list = build_elevation_list_from_vcp(&vcp);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].elevation_number, 1);
        assert_eq!(list[2].elevation_number, 3);
        assert!((list[1].angle - 1.5).abs() < 1e-4);
        assert_eq!(list[1].waveform, "CDW");
        assert!(list[1].is_sails);
        assert!(list[1].is_mrle);
        assert!(list[2].is_mrle && !list[2].is_sails);
        // cached_products is always empty for the live-VCP path.
        assert!(list.iter().all(|e| e.cached_products.is_empty()));
    }

    #[wasm_bindgen_test]
    fn build_elevation_list_from_vcp_empty_yields_empty() {
        let vcp = ExtractedVcp {
            number: 0,
            elevations: Vec::new(),
        };
        assert!(build_elevation_list_from_vcp(&vcp).is_empty());
    }
}
