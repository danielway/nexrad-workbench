//! Shell for the overview's storm activity: reads a site's stored day
//! summaries and its published index once, fetches the index's month files
//! and lists the days the overview shows that none of them covers (nearest
//! the middle first, a few at a time, once the overview stops moving), and
//! stores what comes back. Every decision is in
//! [`crate::core::storm_activity`]; this only runs it.

use crate::core::storm_activity::{day_start_of, needs_scans, ActivityWindow};
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
            acq.activity_channel.load(
                ctx.clone(),
                acq.coordinator.download_channel.stats(),
                acq.coordinator.facade().clone(),
                site.clone(),
            );
            return;
        }

        // Zoomed out the days size the bars; zoomed in their volumes do. The
        // phone strip at 3 H or DAY asks for its window instead of the
        // overview's.
        let ov = self.playback.state.overview;
        let phone_day = if self.chrome.device_layout.is_phone() {
            self.chrome.phone_strip_view.and_then(|(window, lane_px)| {
                crate::core::phone_strip::activity_window(
                    self.chrome.phone_strip_zoom,
                    window,
                    lane_px,
                )
            })
        } else {
            None
        };
        let (start, end, lane_px) =
            phone_day.unwrap_or((ov.start, ov.end(), self.playback.state.timeline_width_px));
        if lane_px <= 0.0 {
            return;
        }
        // Zoomed in to volumes, days the index covers still need listing.
        let need_scans = needs_scans(lane_px, end - start);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (&site.0, day_start_of(start), day_start_of(end), need_scans).hash(&mut hasher);
        if !acq
            .activity_settle
            .poll(hasher.finish(), now_ms, ACTIVITY_SETTLE_MS)
        {
            return;
        }

        let window = ActivityWindow {
            start,
            end,
            era_start: crate::core::NEXRAD_ARCHIVE_START_SECS,
            now_secs: self.state.frame_now.secs(),
            need_scans,
        };
        let plan = acq
            .activity_fetcher
            .plan(&self.timeline.activity, &site, &window, now_ms);
        let stats = acq.coordinator.download_channel.stats();
        for month in plan.months {
            acq.activity_fetcher.start_month(&site, month);
            acq.activity_channel
                .fetch_month(ctx.clone(), stats.clone(), site.clone(), month);
        }
        for day_start in plan.list {
            acq.activity_fetcher.start(&site, day_start);
            acq.activity_channel
                .list_day(ctx.clone(), stats.clone(), site.clone(), day_start);
        }
    }

    fn drain_storm_activity(&mut self, now_ms: f64) {
        let mut listed = Vec::new();
        while let Some(message) = self.acquisition.activity_channel.try_recv() {
            match message {
                ActivityMessage::Loaded {
                    site,
                    days,
                    index_days,
                    month_days,
                } => {
                    let stored = days.len();
                    self.timeline.activity.insert_many(days);
                    let indexed = self.timeline.activity.insert_index(index_days);
                    self.timeline.activity.set_month_days(&site, month_days);
                    log::debug!(
                        "Read {} stored and {} indexed activity days for {}",
                        stored,
                        indexed,
                        site
                    );
                    self.acquisition.activity_fetcher.finish_load(&site);
                }
                ActivityMessage::Month {
                    site,
                    month,
                    result,
                } => {
                    self.acquisition.activity_fetcher.finish_month(
                        &site,
                        month,
                        result.is_ok(),
                        now_ms,
                    );
                    if let Ok(days) = result {
                        // Stored like listed days, so the month isn't
                        // fetched again for them.
                        listed.extend(self.timeline.activity.insert_month(days));
                    }
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
