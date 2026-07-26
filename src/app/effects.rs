//! The shell's effect runtime: executes the [`Effect`]s the core returns.
//!
//! Decision functions in `core` describe their side effects as data; this is the
//! single place those described effects are actually performed (URL/history,
//! localStorage, and — as later phases land — geolocation and friends). Keeping
//! execution here keeps the deciding logic pure and unit-testable, and gives
//! every effect one home instead of scattering `web_sys` calls through the
//! decision paths.

use crate::core::Effect;
use crate::WorkbenchApp;
use eframe::egui;

impl WorkbenchApp {
    /// Execute a batch of effects in order.
    pub(crate) fn apply_effects(&mut self, ctx: &egui::Context, effects: Vec<Effect>) {
        for effect in effects {
            self.apply_effect(ctx, effect);
        }
    }

    /// Execute a single effect. The match is exhaustive so a new `Effect`
    /// variant forces a decision about how the shell performs it.
    fn apply_effect(&mut self, ctx: &egui::Context, effect: Effect) {
        match effect {
            Effect::PushUrl(p) => {
                crate::state::url_state::push_to_url(
                    &p.site, p.time, &p.product, p.lat, p.lon, &p.view, p.dev,
                );
            }
            Effect::SavePreferences(prefs) => prefs.save(),
            Effect::StartGeolocation => {
                crate::app::geolocation::start_geolocation(
                    self.diagnostics.gps.result_sender(),
                    ctx.clone(),
                );
            }
            Effect::OpenUrl(url) => {
                if let Some(window) = web_sys::window() {
                    let _ = window.open_with_url_and_target(url, "_blank");
                }
            }
            Effect::LocateForSite => self.begin_site_geolocation(ctx),
            Effect::GeocodeZip(zip) => self.begin_site_zip_lookup(ctx, zip),
        }
    }
}
