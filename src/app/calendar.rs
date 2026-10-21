//! Shell for the calendar, "Go to a day" (the phone's sheet and the
//! desktop's popover): runs the core's
//! landing rule against the app's state. Every decision is in
//! [`crate::core::calendar`]; this assembles its inputs and applies what it
//! returns.

use super::command_dispatch::DispatchState;
use crate::core::calendar::{land_on_day, settle_landing, CalendarDay, LandingFacts};
use crate::core::transport::SeekGesture;
use crate::core::Intent;
use crate::data::SiteId;

impl DispatchState<'_> {
    /// [`Intent::GoToDay`]: a running stream stopped, the phone's 3 H stop
    /// or the desktop's strip showing the day, the playhead paused on the
    /// day's stormiest volume (or its midday while the listing is out), and
    /// the calendar closed.
    pub(crate) fn go_to_day(&mut self, day: CalendarDay, intent: &Intent) {
        let site = SiteId::new(self.state.viz_state.site_id.clone());
        let now = self.state.frame_now.secs();
        let listing = self.timeline.activity.listing(&site, day.start, day.end);
        let (ts, landing) = land_on_day(&site, day, listing.as_deref(), &self.timeline.scans, now);
        self.chrome.phone_strip_zoom =
            crate::core::phone_strip::strip_zoom_after(self.chrome.phone_strip_zoom, intent);
        if let Some(placement) =
            crate::core::calendar::landing_stops_stream(self.live.mode_state.is_active())
        {
            self.transport(|env, slices| {
                crate::core::transport::reduce_stop_live(env, slices, placement)
            });
        }
        self.transport(|env, slices| {
            crate::core::transport::reduce_seek(env, slices, ts, SeekGesture::Typed)
        });
        crate::core::calendar::frame_day(
            &mut self.playback.state,
            day,
            now,
            crate::core::NEXRAD_ARCHIVE_START_SECS,
        );
        self.playback.state.playing = false;
        self.chrome.go_to_day = None;
        self.chrome.go_to_day_landing = landing;
        super::dialogs::sync_dialogs(self.chrome, self.diagnostics);
    }
}

impl DispatchState<'_> {
    /// [`Intent::GoToTime`]: a running stream stopped in place, the playhead
    /// paused on the typed time (the strip brought to it), the overview
    /// framed around it, and the calendar closed.
    pub(crate) fn go_to_time(&mut self, ts: f64) {
        let now = self.state.frame_now.secs();
        if let Some(placement) =
            crate::core::calendar::landing_stops_stream(self.live.mode_state.is_active())
        {
            self.transport(|env, slices| {
                crate::core::transport::reduce_stop_live(env, slices, placement)
            });
        }
        self.transport(|env, slices| {
            crate::core::transport::reduce_seek(env, slices, ts, SeekGesture::Typed)
        });
        crate::core::calendar::frame_time(
            &mut self.playback.state,
            ts.min(now),
            now,
            crate::core::NEXRAD_ARCHIVE_START_SECS,
        );
        self.playback.state.playing = false;
        self.chrome.go_to_day = None;
        super::dialogs::sync_dialogs(self.chrome, self.diagnostics);
    }
}

impl crate::WorkbenchApp {
    /// Finish a calendar landing: a re-seek to the stormiest volume once the
    /// day's listing is in, and onto its first frame once it is held, unless
    /// the playhead was touched. The re-seek is applied here rather than
    /// queued as an intent, which would read as a touch and drop the landing
    /// still waiting for its volume.
    pub(crate) fn settle_go_to_day(&mut self) {
        let Some(landing) = self.chrome.go_to_day_landing.take() else {
            return;
        };
        let site = SiteId::new(self.state.viz_state.site_id.clone());
        let listing =
            self.timeline
                .activity
                .listing(&landing.site, landing.day.start, landing.day.end);
        let (pending, seek) = settle_landing(
            landing,
            &LandingFacts {
                site: &site,
                listing: listing.as_deref(),
                held: &self.timeline.scans,
                playhead: self.playback.state.playback_position(),
                playing: self.playback.state.playing,
                now: self.state.frame_now.secs(),
            },
        );
        self.chrome.go_to_day_landing = pending;
        if let Some(ts) = seek {
            let mut s = DispatchState {
                state: &mut self.state,
                playback: &mut self.playback,
                acquisition: &mut self.acquisition,
                live: &mut self.live,
                diagnostics: &mut self.diagnostics,
                chrome: &mut self.chrome,
                timeline: &self.timeline,
            };
            s.transport(|env, slices| {
                crate::core::transport::reduce_seek(env, slices, ts, SeekGesture::Typed)
            });
        }
    }
}
