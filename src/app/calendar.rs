//! Shell for the phone's calendar sheet, "Go to a day": runs the core's
//! landing rule against the app's state. Every decision is in
//! [`crate::core::calendar`]; this assembles its inputs and applies what it
//! returns.

use super::command_dispatch::DispatchState;
use crate::core::calendar::{land_on_day, settle_landing, CalendarDay, LandingFacts};
use crate::core::transport::SeekGesture;
use crate::core::Intent;
use crate::data::SiteId;

impl DispatchState<'_> {
    /// [`Intent::GoToDay`]: the 3 H stop, the playhead detached and paused on
    /// the day's stormiest volume (or its midday while the listing is out),
    /// and the sheet closed.
    pub(crate) fn go_to_day(&mut self, day: CalendarDay, intent: &Intent) {
        let site = SiteId::new(self.state.viz_state.site_id.clone());
        let now = self.state.frame_now.secs();
        let listing = self.timeline.activity.listing(&site, day.start, day.end);
        let (ts, landing) = land_on_day(&site, day, listing.as_deref(), &self.timeline.scans, now);
        self.chrome.phone_strip_zoom =
            crate::core::phone_strip::strip_zoom_after(self.chrome.phone_strip_zoom, intent);
        self.transport(|env, slices| {
            crate::core::transport::reduce_seek(env, slices, ts, SeekGesture::Typed)
        });
        self.playback.state.playing = false;
        self.chrome.go_to_day = None;
        self.chrome.go_to_day_landing = landing;
        super::dialogs::sync_dialogs(self.chrome, self.diagnostics);
    }
}

impl crate::WorkbenchApp {
    /// Finish a calendar landing once its day's listing has arrived: one
    /// re-seek to the stormiest volume, unless the playhead was touched.
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
            self.state.push_command(Intent::Seek {
                ts,
                gesture: SeekGesture::Typed,
            });
        }
    }
}
