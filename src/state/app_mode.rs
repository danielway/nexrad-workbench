//! Top-level application mode — derived state summarizing what the app is
//! currently doing. Recomputed once per frame from `live_mode_state` and
//! `radar_timeline`; never set directly.

use crate::ui::colors::mode;
use eframe::egui::Color32;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppMode {
    /// No data under the playback cursor and not streaming.
    #[default]
    Idle,
    /// Cursor is on a loaded scan; historical playback.
    Archive,
    /// Real-time streaming is active (lock acquired or in progress).
    Live,
}

impl AppMode {
    pub fn label(self) -> &'static str {
        match self {
            AppMode::Idle => "IDLE",
            AppMode::Archive => "ARCHIVE",
            AppMode::Live => "LIVE",
        }
    }

    pub fn color(self) -> Color32 {
        match self {
            AppMode::Idle => mode::IDLE,
            AppMode::Archive => mode::ARCHIVE,
            AppMode::Live => mode::LIVE,
        }
    }
}
