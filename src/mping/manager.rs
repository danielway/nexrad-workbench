//! Owns the mPING fetch lifecycle.
//!
//! Polls when the active site, playback time (quantized to a minute), or
//! API key change. Does nothing while the layer is hidden or no key is
//! configured. Results are drained from the channel each frame.

use eframe::egui;

use super::api::{self, FetchParams};
use super::channel::{MpingChannel, MpingEvent};
use crate::data::get_site;
use crate::state::MpingState;

/// Half-window around the current playback position, in seconds.
const TIME_WINDOW_SECS: i64 = 30 * 60;

/// Radius around the active radar site, in meters.
const RADIUS_METERS: u32 = 300_000;

/// Per-frame inputs computed by the caller. Lets the manager stay
/// decoupled from `AppState`.
pub struct MpingTickInputs<'a> {
    /// Whether the mPING overlay layer is currently visible.
    pub layer_visible: bool,
    /// Whether data is "live" (within the recency window). When false,
    /// the manager skips polling.
    pub is_live: bool,
    /// Active radar site id (any case; manager uppercases for lookup).
    pub site_id: &'a str,
    /// Current playback position in Unix seconds.
    pub playback_secs: f64,
}

/// Inputs that determine whether a refetch is required.
#[derive(Clone, Debug, PartialEq)]
struct CacheKey {
    site_id: String,
    playback_minute: i64,
    api_key_fingerprint: u64,
}

pub struct MpingManager {
    channel: MpingChannel,
    fetch_in_flight: bool,
    last_fetched: Option<CacheKey>,
}

impl Default for MpingManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MpingManager {
    pub fn new() -> Self {
        Self {
            channel: MpingChannel::new(),
            fetch_in_flight: false,
            last_fetched: None,
        }
    }

    /// Called every frame. Drains any events, kicks off a new fetch when due.
    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        mping: &mut MpingState,
        inputs: MpingTickInputs<'_>,
    ) {
        let events = self.channel.drain();
        if !events.is_empty() {
            self.fetch_in_flight = false;
        }
        for event in events {
            self.apply_event(mping, event);
        }

        // Bail early if the layer is off, the user is viewing archive data far
        // behind wall-clock (the overlay is hidden then), or no key has been
        // configured.
        if !inputs.layer_visible || !inputs.is_live {
            return;
        }
        let api_key = match mping.api_key.as_deref() {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => return,
        };

        // Resolve the active site center.
        let site_id = inputs.site_id.to_uppercase();
        let site = match get_site(&site_id) {
            Some(s) => s,
            None => return,
        };

        // Quantize playback position to whole minutes for dedup so that
        // sub-second scrubbing doesn't trigger a fetch storm.
        let playback_minute = (inputs.playback_secs / 60.0) as i64;

        let key = CacheKey {
            site_id: site_id.clone(),
            playback_minute,
            api_key_fingerprint: hash_str(&api_key),
        };

        if self.fetch_in_flight {
            return;
        }
        if self.last_fetched.as_ref() == Some(&key) && mping.last_error.is_none() {
            return;
        }

        // Throttle retries after errors — wait at least 30 s before retrying
        // the same key/site/minute so a misconfigured key doesn't hammer the
        // server every frame.
        if mping.last_error.is_some() {
            let now_ms = js_sys::Date::now();
            if now_ms - mping.last_poll_ms < 30_000.0 {
                return;
            }
        }

        let center_secs_i64 = inputs.playback_secs as i64;
        let params = FetchParams {
            center_lon: site.lon,
            center_lat: site.lat,
            radius_m: RADIUS_METERS,
            min_obtime_ms: (center_secs_i64 - TIME_WINDOW_SECS) * 1000,
            max_obtime_ms: (center_secs_i64 + TIME_WINDOW_SECS) * 1000,
        };

        self.fetch_in_flight = true;
        mping.fetch_in_flight = true;
        mping.last_poll_ms = js_sys::Date::now();
        mping.window_min_ms = params.min_obtime_ms as f64;
        mping.window_max_ms = params.max_obtime_ms as f64;
        self.last_fetched = Some(key);
        api::spawn_fetch(ctx.clone(), self.channel.clone(), api_key, params);
    }

    fn apply_event(&mut self, mping: &mut MpingState, event: MpingEvent) {
        mping.fetch_in_flight = false;
        match event {
            MpingEvent::Updated {
                reports,
                total_count,
            } => {
                log::info!(
                    "mPING reports refreshed: {} loaded ({} total)",
                    reports.len(),
                    total_count
                );
                // If the previously-selected report id is no longer in the
                // refreshed list, drop the stale selection so the popover
                // doesn't reference a missing entry.
                if let Some(sel) = mping.selected_report_id {
                    if !reports.iter().any(|r| r.id == sel) {
                        mping.selected_report_id = None;
                    }
                }
                mping.reports = reports;
                mping.total_count = total_count;
                mping.last_error = None;
                mping.last_success_ms = js_sys::Date::now();
            }
            MpingEvent::Error(msg) => {
                log::warn!("mPING fetch failed: {}", msg);
                mping.last_error = Some(msg);
                // Drop any stale reports so the user isn't shown them as if
                // they were fresh.
                mping.reports.clear();
                mping.total_count = 0;
                mping.selected_report_id = None;
            }
        }
    }

    /// Force a refetch on the next tick — used after the user saves a new
    /// API key.
    pub fn invalidate(&mut self) {
        self.last_fetched = None;
    }
}

/// Cheap, stable hash of a string for dedup-key fingerprinting. We don't
/// need cryptographic strength; we just need the cache key to change when
/// the key changes.
fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}
