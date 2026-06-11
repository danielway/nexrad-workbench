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

/// Pure app-mode derivation, recomputed once per frame.
///
/// `streaming` is the stream-session bit (`LiveModeState::is_active`);
/// `playhead_live` says the playhead is attached to the live edge
/// (pinned-to-now or replaying the lookback window). The two are
/// independent: a running stream with a detached playhead (the user
/// scrubbed away while ingestion continues) is ARCHIVE/IDLE territory —
/// the canvas shows what's under the cursor, not the stream.
pub fn derive_app_mode(streaming: bool, playhead_live: bool, has_scan_at_cursor: bool) -> AppMode {
    if streaming && playhead_live {
        AppMode::Live
    } else if has_scan_at_cursor {
        AppMode::Archive
    } else {
        AppMode::Idle
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn derive_app_mode_truth_table() {
        // LIVE requires both the stream session AND a live-attached playhead.
        assert_eq!(derive_app_mode(true, true, false), AppMode::Live);
        assert_eq!(derive_app_mode(true, true, true), AppMode::Live);
        // Detached while streaming: the cursor decides (archive browsing
        // with the stream ingesting in the background).
        assert_eq!(derive_app_mode(true, false, true), AppMode::Archive);
        assert_eq!(derive_app_mode(true, false, false), AppMode::Idle);
        // Not streaming: playhead_live is impossible in practice, but the
        // derivation still ignores it without a stream.
        assert_eq!(derive_app_mode(false, false, true), AppMode::Archive);
        assert_eq!(derive_app_mode(false, false, false), AppMode::Idle);
    }
}
