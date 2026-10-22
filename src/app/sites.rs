//! The site set's shell: carry out the decisions of [`crate::core::sites`]
//! on the list of sites.
//!
//! Every handler asks the core what the change is (or why it is refused),
//! applies it with [`crate::subsystem::Sites::apply`], and then does the I/O
//! the change implies: a site that joins gets its stream while the session is
//! live (its anchor fetch and listings come from the per-site pumps on the
//! next frame), a site that leaves stops its stream and releases its GPU
//! objects, and a new focus moves the map centre onto it and becomes the
//! preferred site.

use crate::core::sites::{self as sites_core, Change, Select};
use crate::data::SiteId;
use crate::{nexrad, subsystem, WorkbenchApp};
use eframe::egui;

impl WorkbenchApp {
    /// A fresh element for `id`, its stream counting into the shared
    /// download stats.
    fn new_site(&self, id: &str, lat: f64, lon: f64) -> subsystem::SiteState {
        let mut site = subsystem::SiteState::new(id, lat, lon);
        site.live = subsystem::SiteLive::new(nexrad::RealtimeChannel::with_stats(
            self.acquisition.coordinator.download_stats(),
        ));
        site
    }

    /// Stop what is tied to sites that left the list: their streams, their
    /// GPU objects and their queued scans.
    fn retire_sites(&mut self, gone: Vec<subsystem::SiteState>) {
        for mut site in gone {
            log::info!("Site {} leaves the map", site.id.0);
            site.live.channel.stop();
            if let Some(gl) = self.gpu.gl.as_ref() {
                site.gpu.release(gl);
            }
            // Its queued scans go with it; a download in flight finishes and
            // its result is dropped for want of a site.
            let dropped = self
                .acquisition
                .coordinator
                .download_queue
                .drop_site_pending(&site.id);
            for item in dropped {
                if let Some(op_id) = item.operation_id {
                    self.acquisition.state.cancel_operation(op_id);
                }
            }
        }
    }

    /// The focus moved to another element: the map recentres on it, its
    /// tracks start over, and the preferences remember it.
    fn focus_moved(&mut self) {
        let focus = self.sites.focus();
        let (id, lat, lon) = (focus.id.0.clone(), focus.lat, focus.lon);
        let elevation = focus.displayed.as_ref().map(|d| d.elevation_deg);
        self.persistence.adopt_site(&id);
        self.state.preferred_site = Some(id);
        self.state.viz_state.centre_map_on(lat, lon);
        self.state.viz_state.storm_tracks.reset();
        // Letters restart with the tracks: let snapped endpoints go.
        self.state.viz_state.measurement.release_tracks();
        if let Some(deg) = elevation {
            self.state.viz_state.set_elevation_label(deg);
        }
    }

    /// `Intent::AddSite`: show `site_id` beside the sites on the map.
    pub(crate) fn add_site(&mut self, ctx: &egui::Context, site_id: &str, lat: f64, lon: f64) {
        let cap = sites_core::cap(self.chrome.device_layout);
        let change = match sites_core::add(&self.sites.ids(), &SiteId::new(site_id), cap) {
            Ok(change) => change,
            Err(refusal) => {
                log::debug!("Not adding {site_id}: {}", refusal.words());
                return;
            }
        };
        let site = self.new_site(site_id, lat, lon);
        self.sites.apply(change, Some(site));
        crate::core::site_picker::push_recent(&mut self.state.recent_sites, site_id);
        // The new site's scans on the device.
        self.state
            .push_command(crate::core::Intent::RefreshTimeline {
                auto_position: false,
            });
        if self.live.mode_state.is_active() {
            let index = self.sites.list.len() - 1;
            self.start_site_stream(ctx, index);
        }
    }

    /// `Intent::RemoveSite`: take `site_id` off the map.
    pub(crate) fn remove_site(&mut self, site_id: &str) {
        let before = self.sites.focus().id.clone();
        let change =
            match sites_core::remove(&self.sites.ids(), self.sites.focus, &SiteId::new(site_id)) {
                Ok(change) => change,
                Err(refusal) => {
                    log::debug!("Not removing {site_id}: {}", refusal.words());
                    return;
                }
            };
        let gone = self.sites.apply(change, None);
        self.retire_sites(gone);
        if self.sites.focus().id != before {
            self.focus_moved();
        }
    }

    /// `Intent::SetFocus`: make `site_id` the focus.
    pub(crate) fn set_focus(&mut self, site_id: &str) {
        match sites_core::set_focus(&self.sites.ids(), self.sites.focus, &SiteId::new(site_id)) {
            Ok(change) => {
                self.sites.apply(change, None);
                self.focus_moved();
            }
            Err(refusal) => log::debug!("Not focusing {site_id}: {}", refusal.words()),
        }
    }

    /// `Intent::OnlyFocus`: keep only the focus on the map.
    pub(crate) fn only_focus(&mut self) {
        match sites_core::only_focus(&self.sites.ids(), self.sites.focus) {
            Ok(change) => {
                let gone = self.sites.apply(change, None);
                self.retire_sites(gone);
            }
            Err(refusal) => log::debug!("Not narrowing to the focus: {}", refusal.words()),
        }
    }

    /// Apply a site selection (`Intent::SelectSite`): the picked site
    /// becomes the only site on the map and the focus. Recentre the camera,
    /// remember the preference, close the modal, and queue the timeline and
    /// alerts refresh.
    ///
    /// Every site-picking surface (modal list, zip/geolocation result, canvas
    /// marker click) routes here through [`crate::core::Intent::SelectSite`].
    pub(crate) fn apply_site_selection(&mut self, site_id: &str, lat: f64, lon: f64) {
        // Every pick — list, search, zip, location or map — completes the
        // first visit and ends "Pick on map" (restoring the Sites layer).
        self.chrome.site_required = false;
        if let Some(was_on) = self.chrome.site_map_pick.take() {
            self.state.layer_state.geo.nexrad_sites = was_on;
        }
        crate::core::site_picker::push_recent(&mut self.state.recent_sites, site_id);
        self.state.preferred_site = Some(site_id.to_string());
        self.chrome.site_modal_open = false;

        let keep =
            match sites_core::select(&self.sites.ids(), self.sites.focus, &SiteId::new(site_id)) {
                Select::Unchanged => {
                    // Picking the current site is allowed (a first visit can keep
                    // the default site); nothing to retarget, but a deferred boot
                    // tether still starts.
                    if std::mem::take(&mut self.state.start_live_on_site_select) {
                        self.state.push_command(crate::core::Intent::StartLive);
                    }
                    return;
                }
                Select::Keep { index } => {
                    // The picked site is on the map already: it stays, alone,
                    // with its scans and frame.
                    let gone = self.sites.apply(Change::Only { keep: index }, None);
                    self.retire_sites(gone);
                    self.focus_moved();
                    return;
                }
                Select::Retarget { keep } => keep,
            };
        if self.sites.list.len() > 1 {
            let gone = self.sites.apply(Change::Only { keep }, None);
            self.retire_sites(gone);
            let id = self.sites.focus().id.0.clone();
            self.persistence.adopt_site(&id);
        }
        self.sites.focus_mut().retarget(site_id, lat, lon);
        self.state.viz_state.centre_map_on(lat, lon);
        self.state
            .push_command(crate::core::Intent::RefreshTimeline {
                auto_position: true,
            });
        self.state.push_command(crate::core::Intent::Diagnostics(
            crate::core::diagnostics::DiagnosticsIntent::RefreshAlerts,
        ));

        // Boot-tether deferred from a first visit (no site at launch): now that
        // a site exists, open tethered to live (spec §7). One-shot — consumed
        // here so later mid-session site re-selections don't auto-tether.
        if std::mem::take(&mut self.state.start_live_on_site_select) {
            self.state.push_command(crate::core::Intent::StartLive);
        }

        // Tear down the previous radar's stream/GPU/cache state now, in the
        // same step that retargeted the site — otherwise the frame between
        // here and the next `apply_frame_setup` would paint the old site's
        // sweep under the new site's projection.
        self.sync_to_active_site();
    }
}
