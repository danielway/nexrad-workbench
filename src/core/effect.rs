//! Effects — described side effects the core returns for the shell to perform.
//!
//! A decision function in the core is `(state, intent) -> (next state, effects)`:
//! it mutates in-memory state and returns a *description* of any I/O to do. The
//! shell (the eframe `update` loop) executes these. This keeps the deciding
//! logic pure and unit-testable — a test asserts the returned `Effect`s without
//! performing them.
//!
//! This enum carries the **simple, cross-cutting** effects that share one
//! executor (URL/history, localStorage, geolocation, …). Heavy per-decision
//! effects that carry large payloads or live handles — GPU texture uploads,
//! worker render dispatch — keep using their own local action enums
//! ([`crate::state::playback_manager::PrevSweepAction`] is the prototype): that
//! is the same "effect as data" idiom at a granularity that suits a buffer or a
//! `postMessage`, rather than forcing every effect through one type.
//!
//! Variants are added per migration phase as decisions are extracted; the enum
//! is `#[non_exhaustive]`-in-spirit and grows.

use crate::state::url_state::ViewState;
use crate::state::UserPreferences;

/// Result of an async location operation (browser geolocation or zip-code
/// geocoding) — the response vocabulary of [`Effect::StartGeolocation`].
pub enum LocationResult {
    /// Successfully resolved to a lat/lon.
    Success(f64, f64),
    /// The operation failed with an error message.
    Error(String),
}

/// A fully-described URL-bar push (`history.replaceState`). The core builds this
/// from the current view; the shell calls [`crate::state::url_state::push_to_url`].
#[derive(Debug, Clone, PartialEq)]
pub struct UrlPush {
    pub site: String,
    pub time: f64,
    pub product: String,
    pub lat: f64,
    pub lon: f64,
    pub view: ViewState,
    pub dev: bool,
}

/// A side effect the core asks the shell to perform.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Push the current view state to the browser URL bar (throttled upstream).
    PushUrl(UrlPush),
    /// Persist user preferences to localStorage.
    SavePreferences(Box<UserPreferences>),
    /// Begin a one-shot browser geolocation lookup for the "My Location" overlay.
    /// The shell supplies the result sink (a `GpsState` channel) and `egui`
    /// context, so the effect itself carries no payload.
    StartGeolocation,
}
