//! The panel on a rail (one-instrument §8): View, Layers, Events, Settings.
//!
//! One right-side panel absorbs both old sidebars. The rail's 44 px of tab
//! icons always shows and keeps its counts, so a folded panel still says what
//! is on the map; a click opens a tab or folds the open one
//! ([`crate::core::panels::rail_click`]). View holds the product row and the
//! tilt ladder ([`super::ladder`]), Layers the map, live and analysis layers,
//! Events the saved events, Settings storage, data saving, time zone, the
//! keyboard map, developer mode and reset.
//!
//! The section functions are also the phone sheet's bodies
//! (`ui::mobile::settings_modal`) until the phone adopts the rail's tabs.

use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::core::ladder::VolumeLadder;
use crate::core::panels::{self, MpingRow, RailTab};
use crate::core::top_bar::{TopBarIntent, TopBarVm};
use crate::core::{Intent, InterpolationMode};
use crate::state::{format_bytes, AppState, StorageSettings};
use crate::ui::theme;
use eframe::egui::{self, RichText, ScrollArea};
use egui_phosphor::regular as icons;

/// The rail's width.
const RAIL_W: f32 = 44.0;

pub(super) struct RightPanelLayer;

impl Layer for RightPanelLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        40
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        let mping_reports = &ctx.diagnostics.mping.reports;
        let playback_secs = ctx.playback.state.playback_position();
        if ctx.chrome.right_sidebar_visible {
            egui::Panel::right("panel_body")
                .resizable(true)
                .default_size(300.0)
                .min_size(260.0)
                .max_size(440.0)
                .show(ctx.root_ui, |ui| {
                    // Hold the panel's width whichever tab is open.
                    ui.set_min_width(ui.available_width());
                    let tab = ctx.chrome.rail_tab;
                    title_row(ui, tab);
                    ScrollArea::vertical().show(ui, |ui| match tab {
                        RailTab::View => {
                            view_tab(ui, ctx.state, ctx.top_bar_vm, ctx.ladder_vm);
                        }
                        RailTab::Layers => {
                            render_layers_section(
                                ui,
                                ctx.state,
                                ctx.diagnostics,
                                ctx.playback,
                                ctx.derived,
                                ctx.chrome,
                            );
                            ui.add_space(8.0);
                            mping_legend(ui, ctx.state, mping_reports, playback_secs);
                            render_rendering_section(ui, ctx.state, ctx.playback, ctx.chrome);
                        }
                        RailTab::Events => {
                            render_events_section(
                                ui,
                                ctx.state,
                                ctx.live,
                                ctx.playback,
                                ctx.chrome,
                            );
                        }
                        RailTab::Settings => settings_tab(ui, ctx.state, ctx.chrome),
                    });
                });
        }
        rail(ctx.root_ui, ctx.state, ctx.chrome);
    }
}

// ── The rail ───────────────────────────────────────────────────────────────

fn tab_icon(tab: RailTab) -> &'static str {
    match tab {
        RailTab::View => icons::EYE,
        RailTab::Layers => icons::STACK,
        RailTab::Events => icons::BOOKMARK_SIMPLE,
        RailTab::Settings => icons::SLIDERS_HORIZONTAL,
    }
}

fn rail(root_ui: &mut egui::Ui, state: &AppState, chrome: &mut crate::subsystem::Chrome) {
    let site = &state.viz_state.site_id;
    let events_here = state
        .saved_events
        .events
        .iter()
        .filter(|e| &e.site_id == site)
        .count();
    let tabs = panels::rail_tabs(
        chrome.rail_tab,
        chrome.right_sidebar_visible,
        panels::layers_on(&state.layer_state.geo, state.viz_state.storm_cells_visible),
        events_here,
    );
    egui::Panel::right("panel_rail")
        .exact_size(RAIL_W)
        .resizable(false)
        .show(root_ui, |ui| {
            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                for t in &tabs {
                    if t.tab == RailTab::Settings {
                        // Settings sits apart, at the rail's foot.
                        let room = ui.available_height() - 44.0;
                        if room > 0.0 {
                            ui.add_space(room);
                        }
                    }
                    let resp = rail_cell(ui, tab_icon(t.tab), t.count, t.active)
                        .on_hover_text(t.tab.label());
                    if resp.clicked() {
                        let (tab, expanded) = panels::rail_click(
                            chrome.rail_tab,
                            chrome.right_sidebar_visible,
                            t.tab,
                        );
                        chrome.rail_tab = tab;
                        chrome.right_sidebar_visible = expanded;
                    }
                    ui.add_space(4.0);
                }
            });
        });
}

/// One rail tab: a fixed-size cell with the icon centred and any count as a
/// small badge at the icon's top-right corner, so every tab is the same height
/// and a count never wraps onto a row of its own.
fn rail_cell(ui: &mut egui::Ui, icon: &str, count: Option<usize>, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 32.0), egui::Sense::click());
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, 4.0, theme::HOVER);
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, theme::alpha(theme::HOVER, 120));
    }
    let color = if active {
        theme::SELECTED
    } else if resp.hovered() {
        theme::TEXT
    } else {
        theme::TEXT_DIM
    };
    let icon_rect = painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(theme::BODY),
        color,
    );
    if let Some(n) = count {
        painter.text(
            egui::pos2(icon_rect.right() - 1.0, icon_rect.top() + 1.0),
            egui::Align2::LEFT_CENTER,
            n.to_string(),
            egui::FontId::monospace(theme::CAPTION - 1.0),
            color,
        );
    }
    resp
}

fn title_row(ui: &mut egui::Ui, tab: RailTab) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(tab.label()).strong().color(theme::TEXT));
        if let Some((hint, hover)) = tab.hint() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(hint)
                        .size(theme::CAPTION)
                        .color(theme::TEXT_FAINT),
                )
                .on_hover_text(hover);
            });
        }
    });
    ui.add_space(4.0);
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(
        RichText::new(title)
            .size(theme::CAPTION)
            .color(theme::TEXT_FAINT),
    );
}

// ── View ───────────────────────────────────────────────────────────────────

fn view_tab(ui: &mut egui::Ui, state: &mut AppState, top_bar: &TopBarVm, ladder: &VolumeLadder) {
    section(ui, "Product");
    product_row(ui, state, top_bar);
    ui.label(
        RichText::new(&top_bar.product.name)
            .size(theme::CAPTION)
            .color(theme::TEXT_DIM),
    );
    section(ui, "Tilt");
    super::ladder::header(ui, ladder);
    ui.add_space(4.0);
    super::ladder::ladder(ui, ladder, state);
    render_volume_section(ui, state);
}

/// The segmented product row: one button per product, the chosen one in the
/// selected color.
fn product_row(ui: &mut egui::Ui, state: &mut AppState, top_bar: &TopBarVm) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for row in &top_bar.product.rows {
            let text = RichText::new(row.code)
                .size(theme::CAPTION)
                .monospace()
                .color(if row.selected {
                    theme::SELECTED
                } else {
                    theme::TEXT_DIM
                });
            if ui
                .add(egui::Button::new(text).selected(row.selected))
                .on_hover_text(&row.label)
                .clicked()
                && !row.selected
            {
                state.push_command(Intent::TopBar(TopBarIntent::SetProduct(row.product)));
            }
        }
    });
}

/// The phone sheet's product body: the product row and the one tilt list as
/// a list (the phone has no ladder yet).
pub(super) fn render_product_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
) {
    let product = state.viz_state.product;
    ui.horizontal_wrapped(|ui| {
        for p in crate::core::RadarProduct::all() {
            if ui.selectable_label(*p == product, p.short_code()).clicked() && *p != product {
                state.push_command(Intent::TopBar(TopBarIntent::SetProduct(*p)));
            }
        }
    });
    ui.separator();
    let entries = state.current_elevation_list(
        &playback.state,
        &timeline.scans,
        live.radar_model
            .volume
            .as_ref()
            .and_then(|v| v.vcp_pattern.as_ref()),
    );
    let rows =
        crate::core::ladder::tilt_rows(&state.viz_state.elevation_selection, &entries, product);
    for row in rows {
        let resp = ui.horizontal(|ui| {
            let r = ui.selectable_label(row.selected, &row.label);
            if let Some(note) = &row.note {
                ui.label(
                    RichText::new(note)
                        .size(theme::CAPTION)
                        .color(theme::TEXT_FAINT),
                );
            }
            r
        });
        if resp.inner.on_hover_text(&row.hover).clicked() {
            state.push_command(Intent::TopBar(TopBarIntent::SetTilt(row.pick)));
        }
    }
}

fn render_volume_section(ui: &mut egui::Ui, state: &mut AppState) {
    use crate::geo::ViewMode;
    if !matches!(state.viz_state.view_mode(), ViewMode::Globe3D) {
        return;
    }
    section(ui, "Volume rendering");
    ui.checkbox(&mut state.viz_state.volume_3d_enabled, "Draw every tilt")
        .on_hover_text("Ray-march through all elevation sweeps as a volumetric cloud");
    if state.viz_state.volume_3d_enabled {
        // two-way binding: the egui widget owns this value while the user edits it.
        ui.add(
            egui::Slider::new(&mut state.viz_state.volume_density_cutoff, 0.0..=30.0)
                .text("Cutoff")
                .step_by(1.0),
        )
        .on_hover_text("Minimum physical value to render (e.g. dBZ for reflectivity)");
    }
}

// ── Layers ─────────────────────────────────────────────────────────────────

/// One map-overlay checkbox.
///
/// The panel never writes `layer_state` itself: the widget gets a per-frame
/// local binding seeded from the current value (egui needs an `&mut bool` to
/// draw the tick), and any change is emitted as
/// [`crate::core::Intent::SetGeoLayer`] for the shell to apply.
fn layer_checkbox(
    ui: &mut egui::Ui,
    state: &mut AppState,
    layer: crate::core::GeoLayer,
    label: &str,
) -> egui::Response {
    let mut on = layer.get(&state.layer_state.geo);
    let resp = ui.checkbox(&mut on, label);
    if resp.changed() {
        state.push_command(Intent::SetGeoLayer(layer, on));
    }
    resp
}

/// The Map and Live groups. Nothing here hides while it still works: live
/// rows stay checkable away from now, and one sentence says why they do not
/// draw.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_layers_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    diagnostics: &crate::subsystem::Diagnostics,
    _playback: &crate::subsystem::Playback,
    derived: &crate::subsystem::Derived,
    _chrome: &mut crate::subsystem::Chrome,
) {
    use crate::core::GeoLayer;

    section(ui, "Map");
    layer_checkbox(ui, state, GeoLayer::States, "States");
    layer_checkbox(ui, state, GeoLayer::Counties, "Counties");
    layer_checkbox(ui, state, GeoLayer::Cities, "Cities");
    layer_checkbox(ui, state, GeoLayer::Labels, "Labels");
    layer_checkbox(ui, state, GeoLayer::NexradSites, "Radar sites")
        .on_hover_text("Other NEXRAD sites; click one on the map to switch to it");

    // "My Location" is the one layer whose toggle carries a second intent:
    // the enable/disable side effects (clear coords, start the one-shot
    // geolocation) are decided by the diagnostics reducer.
    let mut gps_on = GeoLayer::GpsLocation.get(&state.layer_state.geo);
    let gps_resp = ui.checkbox(&mut gps_on, "My location").on_hover_text(
        "Show your device's location as a dot on the map \
         (one-shot lookup; requires browser permission)",
    );
    if gps_resp.changed() {
        state.push_command(Intent::SetGeoLayer(GeoLayer::GpsLocation, gps_on));
        state.push_command(Intent::Diagnostics(if gps_on {
            crate::core::diagnostics::DiagnosticsIntent::EnableGps
        } else {
            crate::core::diagnostics::DiagnosticsIntent::DisableGps
        }));
    }
    if let Some(err) = diagnostics.gps.error.clone() {
        ui.label(
            RichText::new(format!("{} {err}", icons::WARNING))
                .size(theme::CAPTION)
                .color(theme::TEXT_DIM),
        );
    }

    section(ui, "Live");
    if let Some(note) = panels::live_layers_note(derived.data_is_live) {
        ui.label(
            RichText::new(note)
                .size(theme::CAPTION)
                .color(theme::TEXT_DIM),
        );
        if ui
            .link(RichText::new("Return to live").size(theme::CAPTION))
            .clicked()
        {
            state.push_command(Intent::ReturnToLive);
        }
    }
    layer_checkbox(ui, state, GeoLayer::AlertsWarnings, "Warnings").on_hover_text(
        "NWS warnings (tornado, severe thunderstorm, flood, …). Click an area for details.",
    );
    layer_checkbox(ui, state, GeoLayer::AlertsOther, "Watches & advisories")
        .on_hover_text("NWS watches, advisories, and statements. Click an area for details.");
    layer_checkbox(ui, state, GeoLayer::NationalMosaic, "National mosaic").on_hover_text(
        "The CONUS base-reflectivity composite (Iowa State Mesonet, ~2 min refresh); \
         its palette is on its stamp",
    );
    ui.horizontal(|ui| {
        match panels::mping_row(diagnostics.mping.api_key.is_some()) {
            MpingRow::Ready => {
                layer_checkbox(ui, state, GeoLayer::Mping, "Storm reports (mPING)").on_hover_text(
                    "Crowd-sourced mPING storm reports near the radar \
                     (\u{00B1}30 min of the playback time, ~300 km radius)",
                );
            }
            MpingRow::NeedsKey => {
                ui.label(RichText::new("Storm reports (mPING)").color(theme::TEXT_DIM));
                if ui
                    .link(RichText::new("Add a key").size(theme::CAPTION))
                    .clicked()
                {
                    state.push_command(Intent::Diagnostics(
                        crate::core::diagnostics::DiagnosticsIntent::OpenMpingSettings,
                    ));
                }
            }
        }
        if ui
            .small_button(icons::GEAR)
            .on_hover_text("mPING settings (API key)")
            .clicked()
        {
            state.push_command(Intent::Diagnostics(
                crate::core::diagnostics::DiagnosticsIntent::OpenMpingSettings,
            ));
        }
    });
}

/// mPING categories on its layer row (§7: legends for every colored layer).
fn mping_legend(
    ui: &mut egui::Ui,
    state: &AppState,
    reports: &[crate::mping::StormReport],
    playback_secs: f64,
) {
    if !state.layer_state.geo.mping {
        return;
    }
    let legend = crate::core::canvas::mping_legend(reports, playback_secs);
    if legend.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.add_space(22.0);
        for (cat, label) in legend {
            let (r, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(r.center(), 4.0, crate::ui::data_colors::mping_fill(cat));
            ui.label(
                RichText::new(label)
                    .size(theme::CAPTION)
                    .color(theme::TEXT_DIM),
            );
        }
    });
}

/// The Analysis group: storm cells with their threshold, fade older rays,
/// smooth, opacity.
pub(super) fn render_rendering_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    playback: &crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
) {
    let in_macro = playback.state.playback_mode() == crate::core::PlaybackMode::Macro;
    section(ui, "Analysis");

    ui.checkbox(&mut state.viz_state.storm_cells_visible, "Storm cells")
        .on_hover_text("Detect and outline storm cells on the radar");
    if state.viz_state.storm_cells_visible {
        ui.indent("storm_cell_indent", |ui| {
            // two-way binding: the egui widget owns this value while the user edits it.
            let mut threshold = state.viz_state.storm_cell_threshold_dbz;
            if ui
                .add(
                    egui::Slider::new(&mut threshold, 20.0..=60.0)
                        .text("Min dBZ")
                        .step_by(1.0),
                )
                .changed()
            {
                state.viz_state.storm_cell_threshold_dbz = threshold;
                // Clear cached results so detection re-runs with new threshold
                state.viz_state.detected_storm_cells.clear();
            }
        });
    }

    let proc = &mut state.render_processing;
    // Fading only applies to Rays' sweep reveal; Frames shows whole sweeps.
    ui.add_enabled_ui(!in_macro, |ui| {
        ui.checkbox(&mut proc.data_age_desaturation, "Fade older rays")
            .on_hover_text(if in_macro {
                "Applies in Rays; Frames shows whole sweeps"
            } else {
                "Desaturate the oldest data behind the sweep line"
            });
    });

    let mut smooth = proc.interpolation == InterpolationMode::Bilinear;
    if ui
        .checkbox(&mut smooth, "Smooth")
        .on_hover_text("Interpolate between gates and radials instead of drawing raw bins")
        .changed()
    {
        proc.interpolation = if smooth {
            InterpolationMode::Bilinear
        } else {
            InterpolationMode::Nearest
        };
    }

    // two-way binding: the egui widget owns this value while the user edits it.
    let mut opacity_pct = proc.opacity * 100.0;
    if ui
        .add(
            egui::Slider::new(&mut opacity_pct, 0.0..=100.0)
                .text("Opacity")
                .suffix("%")
                .step_by(1.0),
        )
        .changed()
    {
        proc.opacity = opacity_pct / 100.0;
    }
}

// ── Events ─────────────────────────────────────────────────────────────────

/// Saved events for this site, then other sites under a toggle. Saving lives
/// in the selection chip's menu, where the selection is.
pub(super) fn render_events_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    let current_site = state.viz_state.site_id.clone();
    let site_events: Vec<_> = state
        .saved_events
        .events
        .iter()
        .filter(|e| e.site_id == current_site)
        .cloned()
        .collect();
    let other_count = state.saved_events.events.len() - site_events.len();

    for event in &site_events {
        event_row(ui, state, live, playback, chrome, event, false);
    }

    if other_count > 0 {
        ui.add_space(4.0);
        egui::CollapsingHeader::new(
            RichText::new(format!("{} events on other sites", other_count))
                .size(theme::CAPTION)
                .color(theme::TEXT_DIM),
        )
        .id_salt("other_site_events")
        .default_open(false)
        .show(ui, |ui| {
            let other_events: Vec<_> = state
                .saved_events
                .events
                .iter()
                .filter(|e| e.site_id != current_site)
                .cloned()
                .collect();
            for event in &other_events {
                event_row(ui, state, live, playback, chrome, event, true);
            }
        });
    }

    if site_events.is_empty() && other_count == 0 {
        ui.label(
            RichText::new(
                "No saved events. Select a range on the timeline and choose \
                 \u{201C}Save as event\u{201D} in its menu.",
            )
            .size(theme::CAPTION)
            .color(theme::TEXT_FAINT),
        );
    }
}

fn event_row(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
    event: &crate::state::SavedEvent,
    show_site: bool,
) {
    ui.horizontal(|ui| {
        // Events wear the amber loop mark, as on the timeline and overview.
        ui.label(RichText::new(icons::REPEAT).color(theme::LOOP));
        let name = if show_site {
            format!("{} ({})", event.name, event.site_id)
        } else {
            event.name.clone()
        };
        ui.label(RichText::new(name).color(theme::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(icons::PENCIL_SIMPLE)
                .on_hover_text("Edit")
                .clicked()
            {
                chrome.event_modal_open = true;
                chrome.event_modal_editing_id = Some(event.id);
            }
            if ui
                .small_button(icons::NAVIGATION_ARROW)
                .on_hover_text("Go to this event and loop it")
                .clicked()
            {
                navigate_to_event(state, live, playback, event);
            }
        });
    });
    let start_label = format_event_time(event.start_time, state.use_local_time);
    let end_label = format_event_time(event.end_time, state.use_local_time);
    ui.label(
        theme::readout(format!("{start_label} \u{2013} {end_label}"))
            .size(theme::CAPTION)
            .color(theme::TEXT_DIM),
    );
    ui.add_space(2.0);
}

/// Navigate to a saved event: switch site if needed, loop its span, center timeline.
fn navigate_to_event(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    event: &crate::state::SavedEvent,
) {
    use crate::data::get_site;

    if event.site_id != state.viz_state.site_id {
        if let Some(site) = get_site(&event.site_id) {
            state.viz_state.site_id = site.id.to_string();
            state.viz_state.center_lat = site.lat;
            state.viz_state.center_lon = site.lon;
            state.viz_state.set_pan_offset(egui::Vec2::ZERO);
            state.viz_state.camera.center_on(site.lat, site.lon);
            state.push_command(Intent::RefreshTimeline {
                auto_position: false,
            });
        }
    }

    // Loop the event's span (a fixed range: the playhead leaves live).
    live.select_range(
        &mut playback.state,
        event.start_time,
        event.end_time,
        false,
        state.frame_now.secs(),
        state.pause_stream_while_reviewing,
    );

    let mid = (event.start_time + event.end_time) / 2.0;
    playback.state.center_view_on(mid);
}

/// Format a timestamp for display in the events list.
fn format_event_time(ts: f64, use_local: bool) -> String {
    let p = super::time_format::parts(ts, use_local);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        p.year, p.month, p.day, p.hour, p.minute
    )
}

// ── Settings ───────────────────────────────────────────────────────────────

fn settings_tab(ui: &mut egui::Ui, state: &mut AppState, chrome: &mut crate::subsystem::Chrome) {
    render_storage_section(ui, state, chrome);

    section(ui, "Time zone");
    ui.horizontal(|ui| {
        for (label, local) in [("Local", true), ("UTC", false)] {
            if ui
                .selectable_label(state.use_local_time == local, label)
                .clicked()
                && state.use_local_time != local
            {
                state.push_command(Intent::TopBar(TopBarIntent::ToggleTimeZone));
            }
        }
    });

    section(ui, "Keyboard");
    if ui
        .button(format!("{}  Keyboard map", icons::KEYBOARD))
        .clicked()
    {
        state.push_command(Intent::TopBar(TopBarIntent::ToggleShortcutsHelp));
    }

    section(ui, "Developer");
    ui.checkbox(&mut state.dev_mode, "Developer mode")
        .on_hover_text(
            "Add deep diagnostics — per-phase timings, worker queue depth, \
             FPS, and cross-origin isolation — to the activity sheet's Details \
             section. Adds ?dev=true to the URL when enabled.",
        );

    ui.add_space(8.0);
    if ui
        .button("Reset settings")
        .on_hover_text("Restore default settings while keeping saved events and cached data")
        .clicked()
    {
        chrome.reset_settings_modal_open = true;
    }
}

/// Storage (cache size, quota, clear) and data saving. Also the phone
/// sheet's storage body.
pub(super) fn render_storage_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    _chrome: &mut crate::subsystem::Chrome,
) {
    section(ui, "Storage");
    let cache_size = state.session_stats.cache_size_bytes;
    let quota = state.storage_settings.quota_bytes;
    let usage = if quota > 0 {
        (cache_size as f64 / quota as f64).min(1.0)
    } else {
        0.0
    };
    ui.label(
        theme::readout(format!(
            "{} of {} ({:.0}%)",
            format_bytes(cache_size),
            format_bytes(quota),
            usage * 100.0
        ))
        .size(theme::CAPTION)
        .color(theme::TEXT_DIM),
    );
    let (response, painter) = ui.allocate_painter(
        egui::Vec2::new(ui.available_width(), 6.0),
        egui::Sense::hover(),
    );
    let rect = response.rect;
    painter.rect_filled(rect, 2.0, theme::LINE);
    painter.rect_filled(
        egui::Rect::from_min_size(
            rect.min,
            egui::Vec2::new(rect.width() * usage as f32, rect.height()),
        ),
        2.0,
        theme::ON_DEVICE,
    );

    ui.add_space(4.0);
    let min_quota_mb = (StorageSettings::min_quota() / (1024 * 1024)) as f32;
    let max_quota_mb = (StorageSettings::max_quota() / (1024 * 1024)) as f32;
    // two-way binding: the egui widget owns this value while the user edits it.
    let mut quota_mb = (state.storage_settings.quota_bytes / (1024 * 1024)) as f32;
    let slider = egui::Slider::new(&mut quota_mb, min_quota_mb..=max_quota_mb)
        .text("Quota")
        .suffix(" MB")
        .logarithmic(true)
        .clamping(egui::SliderClamping::Always);
    if ui.add(slider).changed() {
        state
            .storage_settings
            .set_quota((quota_mb as u64) * 1024 * 1024);
        state.storage_settings.save();
    }
    if ui
        .button("Clear cache")
        .on_hover_text("Delete all cached radar data on this device")
        .clicked()
    {
        state.push_command(Intent::ClearCache);
    }

    section(ui, "Data saving");
    let mut autofetch = state.autofetch_while_scrubbing;
    if ui
        .checkbox(&mut autofetch, "Auto-fetch while scrubbing")
        .on_hover_text(
            "Download scans automatically as you scrub and seek. Turn off to save data — \
             you can still fetch a selected range or a cut from the ladder.",
        )
        .changed()
    {
        state.push_command(Intent::SetAutofetchWhileScrubbing(autofetch));
    }
    let mut pause_stream = state.pause_stream_while_reviewing;
    if ui
        .checkbox(&mut pause_stream, "Pause live stream while reviewing")
        .on_hover_text(
            "When you scrub away from the live edge, stop the background live stream \
             instead of letting it keep downloading.",
        )
        .changed()
    {
        state.push_command(Intent::SetPauseStreamWhileReviewing(pause_stream));
    }
}
