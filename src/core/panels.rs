//! The panel on a rail (one-instrument §8): four tabs, View, Layers, Events
//! and Settings, in one right-side panel that collapses to its 44 px rail.
//!
//! The ladder itself is [`crate::core::ladder`]. This module holds the
//! rail's own decisions: which tab a click opens (or whether it folds the
//! panel), the counts the tab icons keep so a collapsed rail still says what
//! is on the map, the Layers tab's recency sentence, and the map toolbar's
//! tool toggles. All pure; the egui layer (`ui::right_panel`, `ui::canvas`)
//! draws them and emits the intents they name.

use crate::core::recency::{LiveOverlaysOff, RECENCY_SECS};
use crate::geo::GeoLayerVisibility;

// ───────────────────────────────────────────────────────────────────────────
// The rail
// ───────────────────────────────────────────────────────────────────────────

/// One tab of the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) enum RailTab {
    /// The product row and the tilt ladder (and, in 3D, volume rendering).
    #[default]
    View,
    /// Map, live and analysis layers.
    Layers,
    /// Saved events.
    Events,
    /// Storage, data saving, time zone, keyboard map, developer, reset.
    Settings,
}

impl RailTab {
    pub(crate) const ALL: [RailTab; 4] = [
        RailTab::View,
        RailTab::Layers,
        RailTab::Events,
        RailTab::Settings,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            RailTab::View => "View",
            RailTab::Layers => "Layers",
            RailTab::Events => "Events",
            RailTab::Settings => "Settings",
        }
    }

    /// The tab's keyboard hint in its title row, in words, with a hover that
    /// spells the keys out: `(short, hover)`.
    pub(crate) fn hint(self) -> Option<(&'static str, &'static str)> {
        match self {
            RailTab::View => Some((
                "keys: P product, E tilt",
                "P cycles the product; E steps down the tilt ladder and Shift+E up it",
            )),
            _ => None,
        }
    }
}

/// One rail icon, ready to draw.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RailTabVm {
    pub tab: RailTab,
    /// The icon is the open tab (drawn in the selected color).
    pub active: bool,
    /// The count badged on the icon: layers on the map, events for this site.
    pub count: Option<usize>,
}

/// The rail's icons. `active` is highlighted only while the panel is open.
pub(crate) fn rail_tabs(
    active: RailTab,
    expanded: bool,
    layers_on: usize,
    events_here: usize,
) -> Vec<RailTabVm> {
    RailTab::ALL
        .iter()
        .map(|&tab| RailTabVm {
            tab,
            active: expanded && tab == active,
            count: match tab {
                RailTab::Layers => Some(layers_on).filter(|&n| n > 0),
                RailTab::Events => Some(events_here).filter(|&n| n > 0),
                _ => None,
            },
        })
        .collect()
}

/// A click on a rail icon: open that tab, or fold the panel when it is the
/// open tab already. Returns `(tab, expanded)`.
pub(crate) fn rail_click(active: RailTab, expanded: bool, clicked: RailTab) -> (RailTab, bool) {
    if expanded && clicked == active {
        (active, false)
    } else {
        (clicked, true)
    }
}

/// How many layers draw on the map: the counted badge on the Layers icon.
pub(crate) fn layers_on(geo: &GeoLayerVisibility, storm_cells: bool) -> usize {
    [
        geo.states,
        geo.counties,
        geo.cities,
        geo.labels,
        geo.nexrad_sites,
        geo.gps_location,
        geo.alerts_warnings,
        geo.alerts_other,
        geo.national_mosaic,
        geo.mping,
        storm_cells,
    ]
    .iter()
    .filter(|&&on| on)
    .count()
}

// ───────────────────────────────────────────────────────────────────────────
// Layers
// ───────────────────────────────────────────────────────────────────────────

/// The Layers tab's note over the live rows while the live layers are off
/// (§7: one recency rule, said in words with a Return to live link instead
/// of greying a checked box): the playhead is more than [`RECENCY_SECS`]
/// behind now, all of a playing loop is, or a playing loop crosses that
/// edge, which holds them off for the whole loop even while the playhead is
/// recent. `None` while the live layers draw.
pub(crate) fn live_layers_note(off: Option<LiveOverlaysOff>) -> Option<String> {
    let rule = format!(
        "Warnings and the mosaic show current conditions, so they draw only within {} min of now.",
        (RECENCY_SECS / 60.0).round()
    );
    match off? {
        LiveOverlaysOff::Behind => Some(rule),
        LiveOverlaysOff::LoopReachesBack => Some(format!(
            "{rule} A loop that reaches further back keeps them off while it plays."
        )),
    }
}

/// The mPING row's state: drawn, waiting for a key ("Add a key"), or held
/// back by the recency rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MpingRow {
    /// Toggleable; reports draw when on.
    Ready,
    /// No API key yet: the row says "Add a key".
    NeedsKey,
}

pub(crate) fn mping_row(has_key: bool) -> MpingRow {
    if has_key {
        MpingRow::Ready
    } else {
        MpingRow::NeedsKey
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The map toolbar
// ───────────────────────────────────────────────────────────────────────────

/// A tool in the map's corner toolbar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MapTool {
    /// Hover the radar for the value under the cursor.
    Probe,
    /// Click two points to measure the distance between them.
    Distance,
}

/// What a tool toggle leaves behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct ToolState {
    pub probe: bool,
    pub distance: bool,
    /// Drop the measured points (the distance tool was switched off).
    pub clear_points: bool,
}

/// Toggle one tool. The two are independent; switching the distance tool off
/// clears its measurement.
pub(crate) fn toggle_map_tool(probe: bool, distance: bool, tool: MapTool) -> ToolState {
    match tool {
        MapTool::Probe => ToolState {
            probe: !probe,
            distance,
            clear_points: false,
        },
        MapTool::Distance => ToolState {
            probe,
            distance: !distance,
            clear_points: distance,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn a_click_opens_a_tab_or_folds_the_open_one() {
        assert_eq!(
            rail_click(RailTab::View, true, RailTab::Layers),
            (RailTab::Layers, true)
        );
        assert_eq!(
            rail_click(RailTab::View, true, RailTab::View),
            (RailTab::View, false)
        );
        // A folded rail opens on whichever icon was clicked, the open one too.
        assert_eq!(
            rail_click(RailTab::View, false, RailTab::View),
            (RailTab::View, true)
        );
        assert_eq!(
            rail_click(RailTab::View, false, RailTab::Settings),
            (RailTab::Settings, true)
        );
    }

    #[wasm_bindgen_test]
    fn the_rail_lists_four_tabs_and_keeps_its_counts_when_folded() {
        let open = rail_tabs(RailTab::Events, true, 5, 2);
        let tabs: Vec<_> = open.iter().map(|t| t.tab.label()).collect();
        assert_eq!(tabs, ["View", "Layers", "Events", "Settings"]);
        assert!(open[2].active && !open[0].active);
        assert_eq!(open[1].count, Some(5));
        assert_eq!(open[2].count, Some(2));
        assert_eq!(open[0].count, None);

        let folded = rail_tabs(RailTab::Events, false, 5, 0);
        assert!(folded.iter().all(|t| !t.active));
        assert_eq!(folded[1].count, Some(5));
        assert_eq!(folded[2].count, None, "no badge for zero events");
    }

    #[wasm_bindgen_test]
    fn the_view_hint_names_its_keys_in_words() {
        let (short, hover) = RailTab::View.hint().unwrap();
        assert_eq!(short, "keys: P product, E tilt");
        assert!(hover.contains("Shift+E"));
        assert!(RailTab::Layers.hint().is_none());
    }

    #[wasm_bindgen_test]
    fn layers_on_counts_every_drawn_layer() {
        let mut geo = GeoLayerVisibility::default();
        let base = layers_on(&geo, false);
        geo.mping = true;
        assert_eq!(layers_on(&geo, false), base + 1);
        assert_eq!(layers_on(&geo, true), base + 2);
    }

    #[wasm_bindgen_test]
    fn the_recency_sentence_appears_only_away_from_now() {
        assert_eq!(live_layers_note(None), None);
        let s = live_layers_note(Some(LiveOverlaysOff::Behind)).unwrap();
        assert_eq!(
            s,
            "Warnings and the mosaic show current conditions, so they draw only within 15 min of now."
        );
        assert_eq!(mping_row(false), MpingRow::NeedsKey);
        assert_eq!(mping_row(true), MpingRow::Ready);
    }

    #[wasm_bindgen_test]
    fn a_loop_that_reaches_back_says_why_a_recent_playhead_has_no_live_layers() {
        let s = live_layers_note(Some(LiveOverlaysOff::LoopReachesBack)).unwrap();
        assert_eq!(
            s,
            "Warnings and the mosaic show current conditions, so they draw only within 15 min of now. A loop that reaches further back keeps them off while it plays."
        );
    }

    #[wasm_bindgen_test]
    fn tools_toggle_independently_and_distance_off_clears_points() {
        assert_eq!(
            toggle_map_tool(false, true, MapTool::Probe),
            ToolState {
                probe: true,
                distance: true,
                clear_points: false
            }
        );
        assert_eq!(
            toggle_map_tool(false, true, MapTool::Distance),
            ToolState {
                probe: false,
                distance: false,
                clear_points: true
            }
        );
        assert_eq!(
            toggle_map_tool(false, false, MapTool::Distance),
            ToolState {
                probe: false,
                distance: true,
                clear_points: false
            }
        );
    }
}
