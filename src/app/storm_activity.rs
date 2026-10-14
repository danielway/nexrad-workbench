//! Shell for the overview's storm-activity bars: reads a site's stored day
//! summaries once, lists the days the zoomed-out overview shows (nearest
//! the middle first, a few at a time, once the overview stops moving), and
//! stores what comes back. Every decision is in
//! [`crate::core::storm_activity`]; this only runs it.

use crate::core::overview::{detail_for, OverviewDetail};
use crate::core::storm_activity::{day_start_of, ActivityWindow};
use crate::data::SiteId;
use crate::nexrad::acquisition::activity_listing::ActivityMessage;
use eframe::egui;
use std::hash::{Hash, Hasher};

/// How long the overview must hold still before its days are listed (ms).
const ACTIVITY_SETTLE_MS: f64 = 400.0;

impl crate::WorkbenchApp {
    /// Apply finished activity requests, then start the next ones.
    pub(crate) fn pump_storm_activity(&mut self, ctx: &egui::Context) {
        let now_ms = js_sys::Date::now();
        self.drain_storm_activity(now_ms);

        let site = SiteId::new(self.state.viz_state.site_id.clone());
        if site.0.is_empty() {
            return;
        }
        let acq = &mut self.acquisition;
        if acq.activity_fetcher.begin_load(&site) {
            acq.activity_channel
                .load(ctx.clone(), acq.coordinator.facade().clone(), site.clone());
            return;
        }

        // Only the zoomed-out overview draws activity.
        let ov = self.playback.state.overview;
        let width = self.playback.state.timeline_width_px;
        if width <= 0.0 || !matches!(detail_for(width / ov.span), OverviewDetail::Bars(_)) {
            return;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (&site.0, day_start_of(ov.start), day_start_of(ov.end())).hash(&mut hasher);
        if !acq
            .activity_settle
            .poll(hasher.finish(), now_ms, ACTIVITY_SETTLE_MS)
        {
            return;
        }

        let window = ActivityWindow {
            start: ov.start,
            end: ov.end(),
            era_start: crate::core::NEXRAD_ARCHIVE_START_SECS,
            now_secs: self.state.frame_now.secs(),
        };
        let days = acq
            .activity_fetcher
            .plan(&self.timeline.activity, &site, &window, now_ms);
        let stats = acq.coordinator.download_channel.stats();
        for day_start in days {
            acq.activity_fetcher.start(&site, day_start);
            acq.activity_channel
                .list_day(ctx.clone(), stats.clone(), site.clone(), day_start);
        }
    }

    fn drain_storm_activity(&mut self, now_ms: f64) {
        let mut listed = Vec::new();
        while let Some(message) = self.acquisition.activity_channel.try_recv() {
            match message {
                ActivityMessage::Loaded { site, days } => {
                    log::debug!("Read {} stored activity days for {}", days.len(), site);
                    self.timeline.activity.insert_many(days);
                    self.acquisition.activity_fetcher.finish_load(&site);
                }
                ActivityMessage::Listed {
                    site,
                    day_start,
                    result,
                } => {
                    let ok = result.is_ok();
                    self.acquisition
                        .activity_fetcher
                        .finish(&site, day_start, ok, now_ms);
                    match result {
                        Ok(day) => listed.push(day),
                        Err(e) => {
                            log::debug!("Activity listing {} {} failed: {}", site, day_start, e)
                        }
                    }
                }
            }
        }
        if !listed.is_empty() {
            self.timeline.activity.insert_many(listed.clone());
            let store = self.acquisition.coordinator.facade().clone();
            self.acquisition.activity_channel.save(store, listed);
        }
    }
}
