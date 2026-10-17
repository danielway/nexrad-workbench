//! The selection chip: one control in the transport row that names what plays
//! (a point or a range, fixed or attached to now) and opens the menu that sets
//! it. Pure view-model + intent mapping; the egui shell only draws it.
//!
//! The chip reads back the same words the menu offers, so every way of
//! choosing lands on one of four selections:
//!
//! | Selection        | Chip                         |
//! |------------------|------------------------------|
//! | live point       | `LIVE`                       |
//! | fixed point      | `At 14:02:10 · 2:17 behind`  |
//! | fixed range      | `⟲ 13:10 – 14:40`            |
//! | range to now     | `⟲ Last 6 frames → now`      |
//!
//! The menu is the one place for everything about the selection: going live,
//! presets, the loop style, the range's own times, saving it as an event, and
//! stopping (the loop, or the stream). The loop style rides on the chip when
//! it is not plain Loop (see [`crate::core::session::SessionVm::loop_style`]).

use crate::core::transport::LiveStopPlacement;
use crate::core::{Intent, LoopBasis, LoopMode, LoopPreset, PlaybackState};

/// What the chip shows, as projected by [`crate::core::session::SessionVm`]
/// (which also owns its words). Timestamps are left unformatted: the shell
/// owns the zone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ChipLabel {
    /// Pinned to the live edge.
    Live,
    /// A fixed point; `behind_secs` is the lag while a stream runs.
    At { ts: f64, behind_secs: Option<f64> },
    /// A fixed range.
    Range { start: f64, end: f64 },
    /// A range attached to now, sliding at its basis.
    RangeToNow { basis: LoopBasis },
}

impl ChipLabel {
    /// Hover text for the chip.
    pub(crate) fn hover(&self) -> &'static str {
        match self {
            ChipLabel::Live => "Showing the newest frame. Click to choose a loop.",
            ChipLabel::At { .. } => "Playing from a point. Click to choose a loop or go live.",
            ChipLabel::Range { .. } => "Looping this range. Click to change it.",
            ChipLabel::RangeToNow { .. } => {
                "Looping the newest frames; the range slides forward as data arrives."
            }
        }
    }
}

/// The chip menu's view of the stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ChipEnv {
    /// A live stream is running (attached or in the background).
    pub stream_active: bool,
    /// A touch device: the menu's hints never mention dragging or keys.
    pub touch: bool,
}

/// One entry in the chip's menu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ChipAction {
    /// Go to (or start) the live point.
    Live,
    /// Stop looping and stay at the playhead as a fixed point.
    StopLooping,
    /// Stop the stream.
    StopStream,
    /// Loop the last N frames / minutes, attached to now.
    Preset(LoopPreset),
    /// Loop what's in view (a copy of the visible span; it pins when it
    /// reaches now, by the one "ends at now" rule).
    LoopView { start: f64, end: f64 },
    /// Choose how the range loops.
    LoopStyle(LoopMode),
    /// Retime the range from the menu's range editor.
    EditRange { start: f64, end: f64 },
    /// Open the event dialog for the range.
    SaveAsEvent,
    /// Open the calendar sheet (phone only).
    GoToDay,
}

impl ChipAction {
    /// The key that does the same thing, shown beside the entry.
    pub(crate) fn key_hint(&self) -> Option<&'static str> {
        match self {
            ChipAction::Live => Some("L"),
            ChipAction::StopStream => Some("Shift+L"),
            _ => None,
        }
    }
}

/// The menu, grouped by the shape of the selection each entry makes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChipMenu {
    /// Already on the live point (the Live entry reads as current).
    pub at_live: bool,
    /// "Stop looping · stay at <ts>": `Some(ts)` when a range is selected.
    pub stop_looping_at: Option<f64>,
    /// A stream is running, so it can be stopped.
    pub can_stop_stream: bool,
    /// The range-ending-now presets, the active one marked.
    pub presets: Vec<(LoopPreset, bool)>,
    /// The visible span (clamped to now) to offer as a loop, if it is long
    /// enough to loop.
    pub view_span: Option<(f64, f64)>,
    /// The loop style, offered while a range is selected.
    pub loop_style: Option<LoopMode>,
    /// A fixed range's times, for the range editor. A range pinned to now
    /// has no fixed end to edit.
    pub fixed_range: Option<(f64, f64)>,
    /// A range is selected, so it can be saved as an event.
    pub can_save_event: bool,
    /// Show the drag and key hints (not on a touch device).
    pub pointer_hints: bool,
    /// Offer "Go to a day…", the phone's calendar sheet, under the Go live
    /// group (a touch device only: the desktop has its overview lane).
    pub go_to_day: bool,
}

/// Build the menu from the selection, the stream, and the visible span.
pub(crate) fn chip_menu(
    playback: &PlaybackState,
    env: ChipEnv,
    view: (f64, f64),
    now: f64,
) -> ChipMenu {
    let live_basis = playback
        .selection
        .range()
        .filter(|r| r.live)
        .map(|r| r.basis);
    let (view_start, view_end) = (view.0, view.1.min(now));
    ChipMenu {
        at_live: playback.is_pinned(),
        stop_looping_at: playback
            .selection
            .range()
            .map(|_| playback.playback_position()),
        can_stop_stream: env.stream_active,
        presets: LoopPreset::menu()
            .iter()
            .map(|p| (*p, live_basis == Some(p.basis())))
            .collect(),
        view_span: (view_end - view_start > 1.0).then_some((view_start, view_end)),
        loop_style: playback
            .selection
            .range()
            .map(|_| playback.time_model.loop_mode),
        fixed_range: playback
            .selection
            .range()
            .filter(|r| !r.live)
            .and_then(|r| r.bounds),
        can_save_event: playback.playback_bounds().is_some(),
        pointer_hints: !env.touch,
        go_to_day: env.touch,
    }
}

/// The intent a menu entry emits. `stop_placement` is where stopping the
/// stream leaves the playhead (the desktop and mobile surfaces differ).
pub(crate) fn intent_for(
    action: ChipAction,
    env: ChipEnv,
    stop_placement: LiveStopPlacement,
) -> Intent {
    match action {
        ChipAction::Live if env.stream_active => Intent::ReturnToLive,
        ChipAction::Live => Intent::GoLive,
        ChipAction::StopLooping => Intent::StopLooping,
        ChipAction::StopStream => Intent::StopLive(stop_placement),
        ChipAction::Preset(p) => Intent::ApplyLoopPreset(p),
        ChipAction::LoopView { start, end } | ChipAction::EditRange { start, end } => {
            Intent::SelectRange {
                start,
                end,
                to_live: false,
            }
        }
        ChipAction::LoopStyle(style) => Intent::SetLoopStyle(style),
        ChipAction::SaveAsEvent => Intent::SaveSelectionAsEvent,
        ChipAction::GoToDay => Intent::OpenGoToDay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const STREAMING: ChipEnv = ChipEnv {
        stream_active: true,
        touch: false,
    };
    const NO_STREAM: ChipEnv = ChipEnv {
        stream_active: false,
        touch: false,
    };

    #[wasm_bindgen_test]
    fn menu_marks_the_active_preset_and_offers_stop_looping_on_a_range() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        let menu = chip_menu(&ps, STREAMING, (0.0, 2000.0), 1000.0);
        assert!(menu.at_live);
        assert_eq!(menu.stop_looping_at, None);
        assert!(menu.can_stop_stream);
        assert!(menu.presets.iter().all(|(_, on)| !on));
        // The view is clamped to now.
        assert_eq!(menu.view_span, Some((0.0, 1000.0)));

        ps.enter_live_range(Some(900.0), LoopBasis::FrameCount(6));
        let menu = chip_menu(&ps, STREAMING, (0.0, 2000.0), 1000.0);
        assert!(!menu.at_live);
        assert_eq!(menu.stop_looping_at, Some(900.0));
        let on: Vec<_> = menu.presets.iter().filter(|(_, on)| *on).collect();
        assert_eq!(on, vec![&(LoopPreset::LastFrames(6), true)]);
    }

    #[wasm_bindgen_test]
    fn menu_hides_an_empty_view_span() {
        let ps = PlaybackState::default();
        // A view entirely in the future has nothing to loop.
        let menu = chip_menu(&ps, NO_STREAM, (2000.0, 3000.0), 1000.0);
        assert_eq!(menu.view_span, None);
        assert!(!menu.can_stop_stream);
    }

    #[wasm_bindgen_test]
    fn actions_map_to_intents() {
        let placement = LiveStopPlacement::LiveEdge;
        assert_eq!(
            intent_for(ChipAction::Live, STREAMING, placement),
            Intent::ReturnToLive
        );
        assert_eq!(
            intent_for(ChipAction::Live, NO_STREAM, placement),
            Intent::GoLive
        );
        assert_eq!(
            intent_for(ChipAction::StopLooping, STREAMING, placement),
            Intent::StopLooping
        );
        assert_eq!(
            intent_for(
                ChipAction::StopStream,
                STREAMING,
                LiveStopPlacement::InPlace
            ),
            Intent::StopLive(LiveStopPlacement::InPlace)
        );
        assert_eq!(
            intent_for(
                ChipAction::Preset(LoopPreset::LastFrames(4)),
                STREAMING,
                placement
            ),
            Intent::ApplyLoopPreset(LoopPreset::LastFrames(4))
        );
        assert_eq!(
            intent_for(
                ChipAction::LoopView {
                    start: 1.0,
                    end: 9.0
                },
                STREAMING,
                placement
            ),
            Intent::SelectRange {
                start: 1.0,
                end: 9.0,
                to_live: false
            }
        );
    }

    #[wasm_bindgen_test]
    fn a_fixed_range_offers_its_style_times_and_save() {
        let mut ps = PlaybackState::default();
        assert!(ps.select_range(100.0, 400.0, false));
        ps.time_model.loop_mode = LoopMode::PingPong;
        let menu = chip_menu(&ps, NO_STREAM, (0.0, 2000.0), 1000.0);
        assert_eq!(menu.loop_style, Some(LoopMode::PingPong));
        assert_eq!(menu.fixed_range, Some((100.0, 400.0)));
        assert!(menu.can_save_event);
        assert!(menu.pointer_hints);
    }

    #[wasm_bindgen_test]
    fn a_point_offers_no_range_entries() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        let menu = chip_menu(&ps, STREAMING, (0.0, 2000.0), 1000.0);
        assert_eq!(menu.loop_style, None);
        assert_eq!(menu.fixed_range, None);
        assert!(!menu.can_save_event);
    }

    #[wasm_bindgen_test]
    fn a_range_to_now_keeps_its_style_but_has_no_fixed_times() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        ps.enter_live_range(Some(900.0), LoopBasis::FrameCount(6));
        let menu = chip_menu(&ps, STREAMING, (0.0, 2000.0), 1000.0);
        assert_eq!(menu.loop_style, Some(LoopMode::Loop));
        assert_eq!(menu.fixed_range, None);
        // No bounds until the live tick resolves the window.
        assert!(!menu.can_save_event);
    }

    #[wasm_bindgen_test]
    fn the_phone_menu_never_mentions_dragging() {
        let ps = PlaybackState::default();
        let touch = ChipEnv {
            stream_active: false,
            touch: true,
        };
        assert!(!chip_menu(&ps, touch, (0.0, 2000.0), 1000.0).pointer_hints);
    }

    #[wasm_bindgen_test]
    fn only_the_phone_menu_offers_go_to_a_day() {
        let ps = PlaybackState::default();
        let touch = ChipEnv {
            stream_active: false,
            touch: true,
        };
        assert!(chip_menu(&ps, touch, (0.0, 2000.0), 1000.0).go_to_day);
        assert!(!chip_menu(&ps, NO_STREAM, (0.0, 2000.0), 1000.0).go_to_day);
        assert_eq!(
            intent_for(ChipAction::GoToDay, touch, LiveStopPlacement::LiveEdge),
            Intent::OpenGoToDay
        );
        assert_eq!(ChipAction::GoToDay.key_hint(), None);
    }

    #[wasm_bindgen_test]
    fn range_entries_map_to_intents_and_keys() {
        let placement = LiveStopPlacement::LiveEdge;
        assert_eq!(
            intent_for(ChipAction::LoopStyle(LoopMode::Once), NO_STREAM, placement),
            Intent::SetLoopStyle(LoopMode::Once)
        );
        assert_eq!(
            intent_for(ChipAction::SaveAsEvent, NO_STREAM, placement),
            Intent::SaveSelectionAsEvent
        );
        assert_eq!(
            intent_for(
                ChipAction::EditRange {
                    start: 5.0,
                    end: 50.0
                },
                NO_STREAM,
                placement
            ),
            Intent::SelectRange {
                start: 5.0,
                end: 50.0,
                to_live: false
            }
        );
        assert_eq!(ChipAction::Live.key_hint(), Some("L"));
        assert_eq!(ChipAction::StopStream.key_hint(), Some("Shift+L"));
        assert_eq!(ChipAction::SaveAsEvent.key_hint(), None);
    }
}
