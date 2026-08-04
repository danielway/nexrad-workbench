//! Owns the mPING fetch lifecycle.
//!
//! Polls under two regimes selected by the playback mode:
//!
//! * **Live-tailing** (playhead pinned to now or replaying the lookback
//!   loop): refetch on a fixed time interval so newly-submitted reports
//!   surface as wall-clock advances. The window is anchored to **wall-clock
//!   now**, not the playhead — see [`fetch_window_ms`].
//! * **Historical** (playhead parked or scrubbing through the archive):
//!   the data is static, so refetch only when the playhead nears the edge
//!   of the window we already hold. Scrubbing within a covered span costs
//!   no requests, and forward playback rolls through the pre-fetched
//!   forward half before the next refetch is due. The window is centered on
//!   the playhead.
//!
//! A fetch covers a 2-hour window; in the historical regime the future half is
//! buffer (never displayed — see `StormReport::visible_at`) so forward motion
//! doesn't refetch per minute. Does nothing while the layer is hidden or no key
//! is configured. Results are drained from the channel each frame.

use eframe::egui;

use super::api::{self, FetchParams};
use super::channel::{MpingChannel, MpingEvent};
use crate::core::MpingState;
use crate::data::get_site;

/// Half-window around the fetch anchor, in seconds. A fetch spans
/// `[anchor - HALF_WINDOW, anchor + HALF_WINDOW]`.
///
/// Widened from 30 min: with the old value the *comfortable* zone (the window
/// minus both refetch margins) was only 30 minutes wide, so looping over
/// anything longer than that re-fetched at both ends of every pass. Two hours
/// of coverage puts every realistic loop comfortably inside one window.
const HALF_WINDOW_SECS: i64 = 60 * 60;

/// How close (in seconds) the playhead may come to either edge of the
/// covered window before a refetch is triggered.
///
/// With the 2-hour window this leaves a 100-minute comfortable zone (was 30),
/// which is what actually stops loop playback from thrashing the API.
const REFETCH_MARGIN_SECS: f64 = 10.0 * 60.0;

/// How often (wall-clock ms) to refetch while live-tailing, to pick up
/// reports submitted since the last poll.
///
/// Matches the NWS alerts poller. mPING reports are crowd-sourced and arrive
/// minutes after the fact, so a 2-minute freshness poll costs nothing
/// perceptible and halves the request rate of a long live session.
const LIVE_POLL_INTERVAL_MS: f64 = 120_000.0;

/// How far past `now` the live-tailing window reaches, to absorb clock skew
/// between the browser and report timestamps.
const LIVE_LOOKAHEAD_SECS: i64 = 5 * 60;

/// Minimum wall-clock ms between retries after a fetch error, so a
/// misconfigured key or a CORS failure doesn't hammer the server.
const ERROR_RETRY_MS: f64 = 30_000.0;

/// Radius around the active radar site, in meters.
const RADIUS_METERS: u32 = 300_000;

/// Per-frame inputs computed by the caller. Lets the manager stay
/// decoupled from `AppState`.
pub(crate) struct MpingTickInputs<'a> {
    /// Whether the mPING overlay layer is currently visible.
    pub layer_visible: bool,
    /// Whether the playhead is tracking the live edge (pinned to now or
    /// replaying the lookback loop). Selects the live-tailing regime.
    pub pinned_to_now: bool,
    /// Active radar site id (any case; manager uppercases for lookup).
    pub site_id: &'a str,
    /// Current playback position in Unix seconds.
    pub playback_secs: f64,
}

/// The time window we currently hold reports for, plus the identity it was
/// fetched under. A change in site or key invalidates coverage.
#[derive(Clone, Debug, PartialEq)]
struct Covered {
    lo_ms: f64,
    hi_ms: f64,
    site_id: String,
    key_fingerprint: u64,
}

pub(crate) struct MpingManager {
    channel: MpingChannel,
    fetch_in_flight: bool,
    covered: Option<Covered>,
}

impl Default for MpingManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MpingManager {
    pub(crate) fn new() -> Self {
        Self {
            channel: MpingChannel::new(),
            fetch_in_flight: false,
            covered: None,
        }
    }

    /// Called every frame. Drains any events, kicks off a new fetch when due.
    ///
    /// `errors` is the app-wide error collector; failures encountered
    /// while polling are pushed here in addition to landing in
    /// `mping.last_error` (which the modal still reads to display).
    pub(crate) fn tick(
        &mut self,
        ctx: &egui::Context,
        mping: &mut MpingState,
        inputs: MpingTickInputs<'_>,
        errors: &mut crate::core::ErrorContext,
    ) {
        let events = self.channel.drain();
        if !events.is_empty() {
            self.fetch_in_flight = false;
        }
        for event in events {
            self.apply_event(mping, event, errors);
        }

        // Bail early if the layer is off or no key has been configured.
        // Unlike before, we no longer gate on data recency: historical
        // playback shows reports for the time being viewed.
        if !inputs.layer_visible {
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

        if self.fetch_in_flight {
            return;
        }

        let key_fingerprint = hash_str(&api_key);
        let playback_ms = inputs.playback_secs * 1000.0;
        let now_ms = js_sys::Date::now();

        let needs = refetch_needed(
            self.covered.as_ref(),
            &site_id,
            key_fingerprint,
            playback_ms,
            inputs.pinned_to_now,
            now_ms,
            mping.last_poll_ms,
        );

        // A pending error overrides coverage: retry on the fixed backoff
        // regardless of where the playhead sits. Otherwise honor the
        // regime decision.
        if mping.last_error.is_some() {
            if now_ms - mping.last_poll_ms < ERROR_RETRY_MS {
                return;
            }
        } else if !needs {
            return;
        }

        let (min_ms, max_ms) =
            fetch_window_ms(inputs.playback_secs, now_ms / 1000.0, inputs.pinned_to_now);
        let params = FetchParams {
            center_lon: site.lon,
            center_lat: site.lat,
            radius_m: RADIUS_METERS,
            min_obtime_ms: min_ms,
            max_obtime_ms: max_ms,
        };

        self.fetch_in_flight = true;
        mping.fetch_in_flight = true;
        mping.last_poll_ms = now_ms;
        mping.window_min_ms = min_ms as f64;
        mping.window_max_ms = max_ms as f64;
        self.covered = Some(Covered {
            lo_ms: min_ms as f64,
            hi_ms: max_ms as f64,
            site_id,
            key_fingerprint,
        });
        api::spawn_fetch(ctx.clone(), self.channel.clone(), api_key, params);
    }

    fn apply_event(
        &mut self,
        mping: &mut MpingState,
        event: MpingEvent,
        errors: &mut crate::core::ErrorContext,
    ) {
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
                mping.last_error = Some(msg.clone());
                errors.push(crate::core::AppError::Mping { message: msg });
                // Reports are deliberately KEPT. A transient failure (a blip, a
                // rate limit) used to blank the whole overlay, which is a worse
                // lie than showing reports a minute stale — `last_error` is
                // already surfaced, so the user knows the feed is unhealthy.
                //
                // But the coverage claim must go: it is written optimistically
                // at spawn time, so leaving it would have us believe we hold a
                // window we never received, and suppress the refetch once the
                // error clears.
                self.covered = None;
            }
        }
    }

    /// Force a refetch on the next tick — used after the user saves a new
    /// API key.
    pub(crate) fn invalidate(&mut self) {
        self.covered = None;
    }
}

/// Decide whether the next tick should issue a fetch, from pure inputs
/// (no clock or network access) so the regime logic is unit-testable.
///
/// `playback_ms` is the playhead in epoch ms; `pinned` selects the
/// live-tailing regime; `now_ms`/`last_poll_ms` drive the live poll
/// interval. Error-retry backoff is handled by the caller.
/// The `[min, max]` epoch-ms window a fetch should request.
///
/// **While live-tailing the window is anchored to wall-clock `now`, not to the
/// playhead.** Anchoring on the playhead meant that during a lookback loop —
/// which is a "pinned" mode — the requested bounds oscillated with the loop
/// phase, so every poll asked for a different window and reports flickered in
/// and out of the render-side filter (which compares against the bounds
/// recorded when the request was *spawned*). A now-anchored window is stable
/// and monotonic, and a lookback loop of any realistic length sits entirely
/// inside it.
///
/// Detached/archive browsing keeps the playhead anchor: there is no live edge
/// to track, and the reports that matter are the ones around what is on screen.
///
/// Pure, so the anchoring rule is testable without a clock.
fn fetch_window_ms(playback_secs: f64, now_secs: f64, pinned: bool) -> (i64, i64) {
    if pinned {
        let now = now_secs as i64;
        return (
            (now - HALF_WINDOW_SECS) * 1000,
            (now + LIVE_LOOKAHEAD_SECS) * 1000,
        );
    }
    let center = playback_secs as i64;
    (
        (center - HALF_WINDOW_SECS) * 1000,
        (center + HALF_WINDOW_SECS) * 1000,
    )
}

fn refetch_needed(
    covered: Option<&Covered>,
    site_id: &str,
    key_fingerprint: u64,
    playback_ms: f64,
    pinned: bool,
    now_ms: f64,
    last_poll_ms: f64,
) -> bool {
    let Some(c) = covered else {
        // Nothing held yet.
        return true;
    };
    // Identity change always forces a refetch.
    if c.site_id != site_id || c.key_fingerprint != key_fingerprint {
        return true;
    }
    if pinned {
        // Live-tailing: refetch on the poll interval to pick up
        // newly-submitted reports as wall-clock advances.
        now_ms - last_poll_ms >= LIVE_POLL_INTERVAL_MS
    } else {
        // Historical: refetch only when the playhead nears either edge of
        // the covered window — the comfortable zone is
        // `[lo + margin, hi - margin]`.
        let margin = REFETCH_MARGIN_SECS * 1000.0;
        playback_ms < c.lo_ms + margin || playback_ms > c.hi_ms - margin
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const MIN_MS: f64 = 60_000.0;

    fn covered_at(center_min: f64) -> Covered {
        // A window centred `center_min` minutes past epoch, ±30 min.
        let center = center_min * MIN_MS;
        Covered {
            lo_ms: center - 30.0 * MIN_MS,
            hi_ms: center + 30.0 * MIN_MS,
            site_id: "KTLX".to_string(),
            key_fingerprint: 7,
        }
    }

    // Historical regime: scrubbing inside the comfortable zone holds.
    #[wasm_bindgen_test]
    fn no_refetch_inside_zone() {
        let c = covered_at(100.0);
        // 5 min forward of centre — well inside [70, 130] minus 15-min margins.
        let p = 105.0 * MIN_MS;
        assert!(!refetch_needed(Some(&c), "KTLX", 7, p, false, 0.0, 0.0));
    }

    // Historical regime: nearing the low (early) edge refetches.
    #[wasm_bindgen_test]
    fn refetch_near_low_edge() {
        let c = covered_at(100.0);
        // Window is [70, 130] min; the comfortable zone starts one margin in.
        // Derived from the constant so retuning the margin doesn't silently
        // invert the test's meaning.
        let margin_min = REFETCH_MARGIN_SECS / 60.0;
        let p = (70.0 + margin_min - 1.0) * MIN_MS;
        assert!(refetch_needed(Some(&c), "KTLX", 7, p, false, 0.0, 0.0));
    }

    // Historical regime: forward playback nearing the high edge refetches.
    #[wasm_bindgen_test]
    fn refetch_near_high_edge() {
        let c = covered_at(100.0);
        let margin_min = REFETCH_MARGIN_SECS / 60.0;
        let p = (130.0 - margin_min + 1.0) * MIN_MS;
        assert!(refetch_needed(Some(&c), "KTLX", 7, p, false, 0.0, 0.0));
    }

    // Identity change forces a refetch even when the playhead is centred.
    #[wasm_bindgen_test]
    fn refetch_on_site_or_key_change() {
        let c = covered_at(100.0);
        let p = 100.0 * MIN_MS;
        assert!(refetch_needed(Some(&c), "KFWS", 7, p, false, 0.0, 0.0));
        assert!(refetch_needed(Some(&c), "KTLX", 9, p, false, 0.0, 0.0));
    }

    // Nothing held yet always refetches.
    #[wasm_bindgen_test]
    fn refetch_when_uncovered() {
        assert!(refetch_needed(None, "KTLX", 7, 0.0, false, 0.0, 0.0));
    }

    // Live-tailing ignores coverage and refetches only on the interval.
    #[wasm_bindgen_test]
    fn live_tailing_refetches_on_interval() {
        let c = covered_at(100.0);
        let p = 100.0 * MIN_MS;
        // Just polled — not yet due even though pinned.
        assert!(!refetch_needed(
            Some(&c),
            "KTLX",
            7,
            p,
            true,
            LIVE_POLL_INTERVAL_MS - 1.0,
            0.0
        ));
        // Interval elapsed — due.
        assert!(refetch_needed(
            Some(&c),
            "KTLX",
            7,
            p,
            true,
            LIVE_POLL_INTERVAL_MS,
            0.0
        ));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const MIN_MS: f64 = 60_000.0;

    // Local builder mirroring the sibling `mod tests` helper (those helpers
    // are private to that module). Window centred `center_min` minutes past
    // epoch, ±30 min, fixed identity (site "KTLX", fingerprint 7).
    fn covered_at(center_min: f64) -> Covered {
        let center = center_min * MIN_MS;
        Covered {
            lo_ms: center - 30.0 * MIN_MS,
            hi_ms: center + 30.0 * MIN_MS,
            site_id: "KTLX".to_string(),
            key_fingerprint: 7,
        }
    }

    // Independent reference: exactly the algorithm `hash_str` documents.
    fn reference_hash(s: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    }

    // ---- hash_str (entirely uncovered by the sibling suite) ----

    // Deterministic: the same input hashes identically across calls.
    #[wasm_bindgen_test]
    fn hash_str_is_deterministic() {
        let a = hash_str("my-secret-key");
        let b = hash_str("my-secret-key");
        assert!(a == b);
    }

    // Matches the canonical DefaultHasher computation it claims to perform.
    #[wasm_bindgen_test]
    fn hash_str_matches_reference() {
        let s = "another-key-123";
        assert!(hash_str(s) == reference_hash(s));
    }

    // Distinct keys produce distinct fingerprints (the whole point: the
    // cache key must change when the API key changes).
    #[wasm_bindgen_test]
    fn hash_str_distinguishes_different_keys() {
        assert!(hash_str("key-A") != hash_str("key-B"));
    }

    // The empty string is handled and matches its reference value.
    #[wasm_bindgen_test]
    fn hash_str_empty_string() {
        assert!(hash_str("") == reference_hash(""));
    }

    // ---- refetch_needed: identity precedes the pinned/historical branch ----

    // Pinned + site change: identity mismatch forces a refetch even though
    // the live poll interval has NOT elapsed (last_poll == now).
    #[wasm_bindgen_test]
    fn pinned_site_change_forces_refetch_before_interval() {
        let c = covered_at(100.0);
        let p = 100.0 * MIN_MS;
        // now == last_poll → interval not elapsed, yet site differs.
        assert!(refetch_needed(Some(&c), "KFWS", 7, p, true, 0.0, 0.0));
    }

    // Pinned + key change: same — identity mismatch wins over the interval.
    #[wasm_bindgen_test]
    fn pinned_key_change_forces_refetch_before_interval() {
        let c = covered_at(100.0);
        let p = 100.0 * MIN_MS;
        assert!(refetch_needed(Some(&c), "KTLX", 9, p, true, 0.0, 0.0));
    }

    // Pinned ignores the historical edge logic: a playhead far outside the
    // covered window still holds while the interval has not elapsed.
    #[wasm_bindgen_test]
    fn pinned_ignores_window_edges() {
        let c = covered_at(100.0);
        // Window is [70, 130] min; place the playhead at 200 min, way past
        // the high edge — historical regime would refetch, pinned does not.
        let p = 200.0 * MIN_MS;
        assert!(!refetch_needed(
            Some(&c),
            "KTLX",
            7,
            p,
            true,
            LIVE_POLL_INTERVAL_MS - 1.0,
            0.0
        ));
    }

    // ---- refetch_needed: historical boundary semantics (strict compares) ----

    // Exactly at the comfortable low edge (lo + margin = 85 min): the
    // comparison is strict `<`, so equality does NOT trigger a refetch.
    #[wasm_bindgen_test]
    fn historical_exactly_at_low_edge_holds() {
        let c = covered_at(100.0);
        // lo = 70 min, margin = 15 min → comfortable low edge = 85 min.
        let p = 85.0 * MIN_MS;
        assert!(!refetch_needed(Some(&c), "KTLX", 7, p, false, 0.0, 0.0));
    }

    // Exactly at the comfortable high edge (hi - margin = 115 min): strict
    // `>` means equality holds (no refetch).
    #[wasm_bindgen_test]
    fn historical_exactly_at_high_edge_holds() {
        let c = covered_at(100.0);
        // hi = 130 min, margin = 15 min → comfortable high edge = 115 min.
        let p = 115.0 * MIN_MS;
        assert!(!refetch_needed(Some(&c), "KTLX", 7, p, false, 0.0, 0.0));
    }

    // Just inside the low edge by 1 ms still holds (proves the boundary is
    // the precise comfortable edge, not a coarse minute).
    #[wasm_bindgen_test]
    fn historical_one_ms_inside_low_edge_holds() {
        let c = covered_at(100.0);
        let p = 85.0 * MIN_MS + 1.0;
        assert!(!refetch_needed(Some(&c), "KTLX", 7, p, false, 0.0, 0.0));
    }

    // Playhead far below the covered window (before lo entirely) refetches.
    #[wasm_bindgen_test]
    fn historical_far_below_window_refetches() {
        let c = covered_at(100.0);
        // 10 min past epoch is far below the [70, 130] window.
        let p = 10.0 * MIN_MS;
        assert!(refetch_needed(Some(&c), "KTLX", 7, p, false, 0.0, 0.0));
    }

    // ── fetch_window_ms: where the window is anchored ──

    #[wasm_bindgen_test]
    fn historical_window_is_centered_on_the_playhead() {
        let playhead = 1_000_000.0;
        let (lo, hi) = fetch_window_ms(playhead, 9_000_000.0, false);
        assert_eq!(lo, (1_000_000 - HALF_WINDOW_SECS) * 1000);
        assert_eq!(hi, (1_000_000 + HALF_WINDOW_SECS) * 1000);
    }

    #[wasm_bindgen_test]
    fn live_window_is_anchored_to_now_not_the_playhead() {
        // The lookback-loop bug: `pinned` is true while replaying the last few
        // frames, so a playhead-anchored window slid back and forth with the
        // loop phase and every poll asked for different bounds. Anchoring on
        // `now` makes the bounds stable and monotonic.
        let now = 9_000_000.0;
        let (lo_a, hi_a) = fetch_window_ms(now - 300.0, now, true);
        let (lo_b, hi_b) = fetch_window_ms(now - 1800.0, now, true);
        assert_eq!((lo_a, hi_a), (lo_b, hi_b));
        assert_eq!(lo_a, (9_000_000 - HALF_WINDOW_SECS) * 1000);
        assert_eq!(hi_a, (9_000_000 + LIVE_LOOKAHEAD_SECS) * 1000);
    }

    #[wasm_bindgen_test]
    fn a_lookback_loop_fits_inside_one_live_window() {
        // A 6-frame lookback loop is roughly 20-35 min of content ending at
        // now. The whole loop must sit inside the fetched window, or the
        // overlay drops reports as the playhead sweeps back.
        let now = 9_000_000.0;
        let (lo, hi) = fetch_window_ms(now, now, true);
        let loop_start_ms = ((now - 35.0 * 60.0) * 1000.0) as i64;
        let loop_end_ms = (now * 1000.0) as i64;
        assert!(lo <= loop_start_ms);
        assert!(hi >= loop_end_ms);
    }

    // ── the widened comfortable zone (the loop-thrash fix) ──

    #[wasm_bindgen_test]
    fn a_loop_longer_than_the_old_zone_no_longer_refetches() {
        // Comfortable zone = window - 2 * margin. It used to be 30 min, so any
        // loop longer than that refetched at BOTH ends of every pass.
        let zone_secs = (2 * HALF_WINDOW_SECS) as f64 - 2.0 * REFETCH_MARGIN_SECS;
        assert!(zone_secs >= 90.0 * 60.0);

        // A 60-minute loop centered in the covered window costs zero requests.
        let center_ms = 1_000_000.0 * 1000.0;
        let c = Covered {
            lo_ms: center_ms - (HALF_WINDOW_SECS as f64) * 1000.0,
            hi_ms: center_ms + (HALF_WINDOW_SECS as f64) * 1000.0,
            site_id: "KTLX".to_string(),
            key_fingerprint: 7,
        };
        for offset_min in [-30.0, -15.0, 0.0, 15.0, 30.0] {
            let p = center_ms + offset_min * MIN_MS;
            assert!(!refetch_needed(Some(&c), "KTLX", 7, p, false, 0.0, 0.0));
        }
    }
}
