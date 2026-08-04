//! localStorage persistence for the streaming loop: the per-site volume-number
//! hint used for a fast resume, and the rolling chunk-timing statistics that let
//! a new session start warm instead of cold-starting from pure physics.
//!
//! Encoding/decoding is split from the storage I/O so the JSON contract is
//! unit-testable without a browser.

use super::current_timestamp_f64;

fn volume_cache_key(site_id: &str) -> String {
    format!("nexrad_volume_{}", site_id)
}

/// Serialize a `(volume, cached-at seconds)` pair to the JSON form stored in
/// localStorage. Pure — split out for testing.
fn encode_volume_cache(volume: usize, cached_at_secs: f64) -> String {
    format!("{{\"v\":{},\"t\":{}}}", volume, cached_at_secs)
}

/// Parse the cached volume value. Returns `(volume, cached-at seconds)` for the
/// current JSON form. Legacy bare-number entries (older builds) carry no
/// timestamp, so they return `None` and are simply ignored — the next Start
/// chunk rewrites them in the new form. Pure — split out for testing.
fn decode_volume_cache(raw: &str) -> Option<(nexrad_data::aws::realtime::VolumeIndex, f64)> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let v = value.get("v")?.as_u64()? as usize;
    let t = value.get("t")?.as_f64()?;
    if (1..=999).contains(&v) {
        Some((nexrad_data::aws::realtime::VolumeIndex::new(v), t))
    } else {
        None
    }
}

/// Cache the latest volume number (with the current wall-clock time) in
/// localStorage for fast resume.
pub(super) fn cache_volume_number(site_id: &str, volume: nexrad_data::aws::realtime::VolumeIndex) {
    if let Some(window) = web_sys::window() {
        if let Ok(Some(storage)) = window.local_storage() {
            let payload = encode_volume_cache(volume.as_number(), current_timestamp_f64());
            let _ = storage.set_item(&volume_cache_key(site_id), &payload);
        }
    }
}

/// Read the cached volume hint for a site: the slot and its cached-at seconds.
/// Returns `None` when absent, malformed, or in the legacy timestamp-less form.
pub(super) fn get_cached_volume_hint(
    site_id: &str,
) -> Option<(nexrad_data::aws::realtime::VolumeIndex, f64)> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok()??;
    let raw = storage.get_item(&volume_cache_key(site_id)).ok()??;
    decode_volume_cache(&raw)
}

// ── Timing stats persistence ──────────────────────────────────────────────

fn timing_stats_key(site_id: &str) -> String {
    format!("nexrad_timing_stats_{}", site_id)
}

/// Persist the site's rolling chunk-timing statistics to localStorage so the
/// next session starts warm instead of cold-starting from pure physics.
pub(super) fn save_timing_stats(site_id: &str, stats: &crate::core::timing::ChunkTimingStats) {
    let Some(json) = stats.to_json() else {
        return;
    };
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(storage)) = window.local_storage() else {
        return;
    };
    let _ = storage.set_item(&timing_stats_key(site_id), &json);
}

/// Read a previously-persisted timing stats snapshot for the site, if any.
pub(super) fn load_cached_timing_stats(
    site_id: &str,
) -> Option<crate::core::timing::ChunkTimingStats> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok()??;
    let raw = storage.get_item(&timing_stats_key(site_id)).ok()??;
    crate::core::timing::ChunkTimingStats::from_json(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn volume_cache_round_trips_through_json() {
        let encoded = encode_volume_cache(347, 1_700_000_000.5);
        let (vol, t) = decode_volume_cache(&encoded).expect("decodes own output");
        assert_eq!(vol.as_number(), 347);
        assert_eq!(t, 1_700_000_000.5);
    }

    #[wasm_bindgen_test]
    fn legacy_bare_number_is_ignored() {
        // Older builds wrote a bare decimal (no timestamp). It must decode to
        // None so the fast path is skipped until the entry is rewritten.
        assert!(decode_volume_cache("347").is_none());
        assert!(decode_volume_cache("VolumeIndex(347)").is_none());
    }

    #[wasm_bindgen_test]
    fn out_of_range_volume_rejected() {
        assert!(decode_volume_cache(&encode_volume_cache(0, 1.0)).is_none());
        assert!(decode_volume_cache(&encode_volume_cache(1000, 1.0)).is_none());
        assert!(decode_volume_cache("garbage").is_none());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── localStorage key derivation (pure formatting) ─────────────────────

    #[wasm_bindgen_test]
    fn volume_cache_key_is_site_namespaced() {
        assert_eq!(volume_cache_key("KTLX"), "nexrad_volume_KTLX");
        assert_eq!(volume_cache_key("KFWS"), "nexrad_volume_KFWS");
        // Distinct sites must not collide.
        assert!(volume_cache_key("KTLX") != volume_cache_key("KFWS"));
    }

    #[wasm_bindgen_test]
    fn timing_stats_key_is_site_namespaced() {
        assert_eq!(timing_stats_key("KTLX"), "nexrad_timing_stats_KTLX");
        assert_eq!(timing_stats_key("KOUN"), "nexrad_timing_stats_KOUN");
        // The two key families never collide for the same site.
        assert!(timing_stats_key("KTLX") != volume_cache_key("KTLX"));
    }

    // ── decode_volume_cache: gaps the existing tests leave open ────────────

    #[wasm_bindgen_test]
    fn decode_volume_cache_accepts_range_endpoints() {
        // The inclusive range is 1..=999; both endpoints decode.
        match decode_volume_cache(&encode_volume_cache(1, 10.0)) {
            Some((vol, t)) => {
                assert_eq!(vol.as_number(), 1);
                assert_eq!(t, 10.0);
            }
            None => panic!("v=1 should be accepted"),
        }
        match decode_volume_cache(&encode_volume_cache(999, 20.0)) {
            Some((vol, t)) => {
                assert_eq!(vol.as_number(), 999);
                assert_eq!(t, 20.0);
            }
            None => panic!("v=999 should be accepted"),
        }
    }

    #[wasm_bindgen_test]
    fn decode_volume_cache_requires_timestamp_field() {
        // Valid volume but missing the `t` field → None (incomplete entry).
        assert!(decode_volume_cache("{\"v\":42}").is_none());
    }

    #[wasm_bindgen_test]
    fn decode_volume_cache_requires_volume_field() {
        // Timestamp present but no `v` → None.
        assert!(decode_volume_cache("{\"t\":1700000000.0}").is_none());
    }

    #[wasm_bindgen_test]
    fn decode_volume_cache_rejects_wrong_typed_fields() {
        // `v` as a string is not a u64 → None.
        assert!(decode_volume_cache("{\"v\":\"42\",\"t\":1.0}").is_none());
        // `t` as a non-number → None.
        assert!(decode_volume_cache("{\"v\":42,\"t\":\"soon\"}").is_none());
        // A JSON array (no object fields) → None.
        assert!(decode_volume_cache("[42,1.0]").is_none());
    }
}
