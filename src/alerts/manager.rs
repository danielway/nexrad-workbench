//! Owns the NWS alert fetch lifecycle and drains results into app state.
//!
//! Polls every `POLL_INTERVAL_MS`. The first tick after construction triggers
//! an immediate fetch. Results flow through an `AlertsChannel` and are applied
//! to `AlertsState` in-place.

use eframe::egui;

use super::api;
use super::channel::{AlertsChannel, AlertsEvent};
use crate::state::AlertsState;

/// Poll interval in wall-clock milliseconds. NWS recommends <= 1 req/min;
/// 2 minutes is comfortable and still plenty fresh for warnings.
const POLL_INTERVAL_MS: f64 = 120_000.0;

/// Retry interval after a failed fetch — shorter than the regular interval
/// so transient errors recover quickly.
const RETRY_INTERVAL_MS: f64 = 30_000.0;

pub struct AlertsManager {
    channel: AlertsChannel,
    /// True while a fetch future is in flight and we're waiting on it.
    fetch_in_flight: bool,
}

impl Default for AlertsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AlertsManager {
    pub fn new() -> Self {
        Self {
            channel: AlertsChannel::new(),
            fetch_in_flight: false,
        }
    }

    /// Called every frame. Drains any events, kicks off a new fetch when due.
    ///
    /// `is_live` should reflect [`crate::state::recency::data_is_live`] —
    /// when the user is scrubbed far behind wall-clock the alerts overlay
    /// is hidden so we skip the poll. Computed by the caller so this
    /// manager doesn't need to reach back into `AppState`.
    ///
    /// `errors` is the app-wide error collector; failures encountered
    /// while polling are pushed here in addition to landing in
    /// `alerts.last_error` (which the chip still reads for its color).
    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        alerts: &mut AlertsState,
        is_live: bool,
        errors: &mut crate::state::ErrorContext,
    ) {
        // Drain events produced by any in-flight fetch.
        let events = self.channel.drain();
        if !events.is_empty() {
            self.fetch_in_flight = false;
        }
        for event in events {
            self.apply_event(alerts, event, errors);
        }

        // Drop expired alerts proactively so the UI never shows stale items.
        let now = js_sys::Date::now() / 1000.0;
        if !alerts.alerts.is_empty() {
            alerts.alerts.retain(|a| !a.is_expired(now));
        }

        // While viewing archive data far behind wall-clock, the alerts overlay
        // is hidden — don't burn requests fetching warnings the user can't see.
        if !is_live {
            return;
        }

        // Was a manual refresh requested?
        let manual_refresh = std::mem::take(&mut alerts.refresh_requested);

        // Due to poll?
        let now_ms = js_sys::Date::now();
        let elapsed = now_ms - alerts.last_poll_ms;
        let due = alerts.last_poll_ms <= 0.0
            || (alerts.last_error.is_some() && elapsed >= RETRY_INTERVAL_MS)
            || elapsed >= POLL_INTERVAL_MS;

        if !self.fetch_in_flight && (manual_refresh || due) {
            self.start_fetch(ctx, alerts);
        }
    }

    fn start_fetch(&mut self, ctx: &egui::Context, alerts: &mut AlertsState) {
        self.fetch_in_flight = true;
        alerts.fetch_in_flight = true;
        alerts.last_poll_ms = js_sys::Date::now();
        let etag = alerts.last_etag.clone();
        api::spawn_fetch(ctx.clone(), self.channel.clone(), etag);
    }

    fn apply_event(
        &mut self,
        alerts: &mut AlertsState,
        event: AlertsEvent,
        errors: &mut crate::state::ErrorContext,
    ) {
        alerts.fetch_in_flight = false;
        match event {
            AlertsEvent::Updated {
                alerts: new_alerts,
                etag,
            } => {
                alerts.alerts = new_alerts;
                alerts.last_etag = etag;
                alerts.last_error = None;
                alerts.last_success_ms = js_sys::Date::now();
                log::info!("NWS alerts refreshed: {} active", alerts.alerts.len());
            }
            AlertsEvent::NotModified => {
                alerts.last_error = None;
                alerts.last_success_ms = js_sys::Date::now();
                log::debug!("NWS alerts: 304 Not Modified");
            }
            AlertsEvent::Error(msg) => {
                log::warn!("NWS alerts fetch failed: {}", msg);
                alerts.last_error = Some(msg.clone());
                errors.push(crate::state::AppError::Alerts { message: msg });
            }
        }
    }
}
