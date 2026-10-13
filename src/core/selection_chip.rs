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

use crate::core::transport::LiveStopPlacement;
use crate::core::{Intent, LoopBasis, LoopPreset, PlaybackState, Selection};

/// What the chip shows. Timestamps are left unformatted: the shell owns the
/// clock format (UTC / local).
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

/// The chip's view of the stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ChipEnv {
    /// A live stream is running (attached or in the background).
    pub stream_active: bool,
    /// Seconds the playhead is behind now while detached from a stream.
    pub lag_secs: Option<f64>,
}

/// Resolve the chip label from the selection.
pub(crate) fn chip_label(playback: &PlaybackState, env: ChipEnv) -> ChipLabel {
    match playback.selection {
        Selection::Point { live: true } => ChipLabel::Live,
        Selection::Point { live: false } => ChipLabel::At {
            ts: playback.playback_position(),
            behind_secs: if env.stream_active {
                env.lag_secs
            } else {
                None
            },
        },
        Selection::Range(r) if r.live => ChipLabel::RangeToNow { basis: r.basis },
        Selection::Range(r) => match r.bounds {
            Some((start, end)) => ChipLabel::Range { start, end },
            // A fixed range always has bounds; fall back to the point.
            None => ChipLabel::At {
                ts: playback.playback_position(),
                behind_secs: None,
            },
        },
    }
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
    /// Loop what's in view, once (a fixed copy of the visible span).
    LoopView { start: f64, end: f64 },
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
    /// The visible span (clamped to now) to offer as a fixed loop, if it is
    /// long enough to loop.
    pub view_span: Option<(f64, f64)>,
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
        ChipAction::LoopView { start, end } => Intent::SelectRange {
            start,
            end,
            to_live: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const STREAMING: ChipEnv = ChipEnv {
        stream_active: true,
        lag_secs: Some(137.0),
    };
    const NO_STREAM: ChipEnv = ChipEnv {
        stream_active: false,
        lag_secs: None,
    };

    #[wasm_bindgen_test]
    fn label_names_each_of_the_four_selections() {
        let mut ps = PlaybackState::default();
        ps.enter_pinned_live(1000.0);
        assert_eq!(chip_label(&ps, STREAMING), ChipLabel::Live);

        assert!(ps.select_range(700.0, 1000.0, true));
        assert_eq!(
            chip_label(&ps, STREAMING),
            ChipLabel::RangeToNow {
                basis: LoopBasis::Duration(300.0)
            }
        );

        assert!(ps.select_range(100.0, 400.0, false));
        assert_eq!(
            chip_label(&ps, STREAMING),
            ChipLabel::Range {
                start: 100.0,
                end: 400.0
            }
        );

        ps.clear_selection();
        assert_eq!(
            chip_label(&ps, STREAMING),
            ChipLabel::At {
                ts: ps.playback_position(),
                behind_secs: Some(137.0)
            }
        );
    }

    #[wasm_bindgen_test]
    fn point_shows_no_lag_without_a_stream() {
        let mut ps = PlaybackState::default();
        ps.time_model.playback_position = 500.0;
        assert_eq!(
            chip_label(&ps, NO_STREAM),
            ChipLabel::At {
                ts: 500.0,
                behind_secs: None
            }
        );
    }

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
}
