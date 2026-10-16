//! The site picker: one popover under the top bar's site control (One
//! Instrument answers, "Choosing a site").
//!
//! One search field that stays put takes a site ID, a city, a state or a
//! zip; under it, Near you (from the browser's location, with distances),
//! Recent, and All sites grouped by state, TDWR sites tagged. The current
//! site is marked, not excluded, so a first visit can keep the default. S
//! opens it, typing filters it, Enter takes the first match. "Pick on map"
//! turns the Sites layer on until a site is clicked.
//!
//! The rows, the ranking and what a location result does are the core's
//! ([`crate::core::site_picker`]); this file draws them and emits intents.

use crate::core::dialogs::{DialogId, DialogIntent};
use crate::core::site_picker::{
    on_location, site_picker_vm, EnterAction, GroupKind, NearYou, SitePickerInputs, SiteRole,
    SiteRow, SiteSet,
};
use crate::core::{Intent, LocationResult};
use crate::state::AppState;
use crate::ui::dialog::{self, Popover, Width};
use crate::ui::theme;
use eframe::egui::{self, Pos2, Rect, RichText, Vec2};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};

/// Where the top bar's site control was drawn this frame (egui memory key);
/// the picker anchors under it.
pub(crate) fn anchor_id() -> egui::Id {
    egui::Id::new("site_picker_anchor")
}

/// Transient state for the site picker.
pub(crate) struct SiteModalState {
    /// The search field's text.
    pub query: String,
    /// The Near you group.
    pub near: NearYou,
    /// The zip an in-flight lookup is for (it names the group).
    pending_zip: Option<String>,
    /// Pick the nearest site when the fix arrives (the phone's location
    /// button); the picker's own link only lists the nearest.
    auto_pick: bool,
    /// The opening (stack generation) the field was last reset for.
    opened_for: Option<u64>,
    /// Sender given to async callbacks. Clone freely.
    location_tx: UnboundedSender<LocationResult>,
    /// Receiver drained by the picker each frame it is open.
    location_rx: UnboundedReceiver<LocationResult>,
}

impl SiteModalState {
    /// A clone-able sink for async callbacks (geolocation, zip lookup).
    pub(crate) fn location_sender(&self) -> UnboundedSender<LocationResult> {
        self.location_tx.clone()
    }

    /// A lookup started: Near you shows its spinner. `zip` names the zip
    /// being looked up; `auto_pick` picks the nearest site on arrival.
    pub(crate) fn begin_pending(&mut self, zip: Option<String>, auto_pick: bool) {
        self.near = NearYou::Locating;
        self.pending_zip = zip;
        self.auto_pick = auto_pick;
    }

    /// A lookup could not start (validation): say why in Near you.
    pub(crate) fn show_error(&mut self, message: String) {
        self.near = NearYou::Failed(message);
    }

    fn drain_location_results(&mut self) -> Vec<LocationResult> {
        let mut out = Vec::new();
        while let Ok(r) = self.location_rx.try_recv() {
            out.push(r);
        }
        out
    }
}

impl Default for SiteModalState {
    fn default() -> Self {
        let (location_tx, location_rx) = futures_channel::mpsc::unbounded();
        Self {
            query: String::new(),
            near: NearYou::Idle,
            pending_zip: None,
            auto_pick: false,
            opened_for: None,
            location_tx,
            location_rx,
        }
    }
}

pub(super) struct SiteModalLayer;

impl super::layout::Layer for SiteModalLayer {
    fn kind(&self) -> super::layout::LayerKind {
        super::layout::LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        10
    }
    fn visible(&self, ctx: &super::layout::LayoutCtx) -> bool {
        ctx.chrome.site_modal_open
    }
    fn render(&self, ctx: &mut super::layout::LayoutCtx) {
        draw_site_picker(ctx.ctx, ctx.state, ctx.chrome, &mut ctx.modals.site);
    }
}

fn select(state: &mut AppState, row: &SiteRow) {
    state.push_command(Intent::SelectSite {
        site_id: row.id.to_string(),
        lat: row.lat,
        lon: row.lon,
    });
}

fn draw_site_picker(
    ctx: &egui::Context,
    state: &mut AppState,
    chrome: &crate::subsystem::Chrome,
    picker: &mut SiteModalState,
) {
    // Location results (geolocation or zip) → the core decides.
    for result in picker.drain_location_results() {
        let out = on_location(result, picker.pending_zip.take(), picker.auto_pick);
        picker.near = out.near;
        picker.auto_pick = false;
        if let Some((id, lat, lon)) = out.pick {
            state.push_command(Intent::SelectSite {
                site_id: id.to_string(),
                lat,
                lon,
            });
        }
        if let Some((lat, lon)) = out.show_location {
            state.push_command(Intent::SetGeoLayer(
                crate::core::GeoLayer::GpsLocation,
                true,
            ));
            state.push_command(Intent::Diagnostics(
                crate::core::diagnostics::DiagnosticsIntent::GpsResolved(lat, lon),
            ));
        }
    }

    // A fresh opening starts with an empty field, focused.
    let generation = chrome.dialogs.generation(DialogId::SitePicker);
    let fresh = generation.is_some() && picker.opened_for != generation;
    if fresh {
        picker.opened_for = generation;
        picker.query.clear();
    }

    let required = chrome.site_required;
    let anchor = ctx
        .data(|d| d.get_temp::<Rect>(anchor_id()))
        .unwrap_or_else(|| Rect::from_min_size(Pos2::new(12.0, 40.0), Vec2::ZERO));
    let sites = SiteSet::single(&state.viz_state.site_id);
    let (title, subtitle) = if required {
        (
            "Welcome to NEXRAD Workbench",
            Some("Pick a radar site to get started"),
        )
    } else {
        ("Radar site", None)
    };

    let mut popover = Popover::new(DialogId::SitePicker, anchor)
        .title(title)
        .width(Width::Regular)
        .dismissible(!required);
    if let Some(sub) = subtitle {
        popover = popover.subtitle(sub);
    }
    let out = popover.show(ctx, |ui, room| {
        let field = ui.add(
            // two-way binding: the egui widget owns this value while the
            // user edits it.
            egui::TextEdit::singleline(&mut picker.query)
                .id(egui::Id::new("site_picker_search"))
                .hint_text("Site ID, city, state or zip")
                .desired_width(f32::INFINITY),
        );
        if fresh {
            field.request_focus();
        }
        // Enter takes the first match from the field, or with nothing focused
        // (after Escape left the field on a first visit).
        let nothing_focused = ui.ctx().memory(|m| m.focused().is_none());
        let enter = (field.lost_focus() || nothing_focused)
            && ui.input(|i| i.key_pressed(egui::Key::Enter));

        let vm = site_picker_vm(&SitePickerInputs {
            query: &picker.query,
            sites: &sites,
            recent: &state.recent_sites,
            near: &picker.near,
        });
        if enter {
            match &vm.enter {
                EnterAction::Pick(row) => select(state, row),
                EnterAction::LookUpZip(zip) => state.push_command(Intent::SubmitZip(zip.clone())),
                EnterAction::None => {}
            }
        }

        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt("site_picker_scroll")
            .max_height((room - 90.0).max(120.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for group in &vm.groups {
                    ui.add_space(6.0);
                    dialog::group_heading(ui, &group.title);
                    if group.kind == GroupKind::NearYou && group.rows.is_empty() {
                        near_you_actions(ui, state, &vm);
                    }
                    for row in &group.rows {
                        if site_row(ui, row) {
                            select(state, row);
                        }
                    }
                }
                if vm.match_count == 0 && vm.zip_query.is_none() {
                    ui.label(RichText::new("No site matches").color(theme::TEXT_DIM));
                }
            });

        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(format!(
                            "{} Pick on map",
                            egui_phosphor::regular::MAP_TRIFOLD
                        ))
                        .color(theme::TEXT),
                    )
                    .frame(false),
                )
                .on_hover_text("Show every site on the map and click one")
                .clicked()
            {
                state.push_command(Intent::Dialog(DialogIntent::PickSiteOnMap));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} sites", vm.match_count))
                        .size(theme::CAPTION)
                        .color(theme::TEXT_FAINT),
                );
            });
        });
    });
    if out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::SitePicker)));
    }
}

/// The Near you group before it has rows: the location link, the spinner
/// while locating, the zip lookup for a zip query, and the last error.
fn near_you_actions(
    ui: &mut egui::Ui,
    state: &mut AppState,
    vm: &crate::core::site_picker::SitePickerVm,
) {
    if vm.locating {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Finding your location\u{2026}").color(theme::TEXT_DIM));
        });
        return;
    }
    if let Some(zip) = &vm.zip_query {
        if ui
            .link(format!(
                "{} Find sites near {zip}",
                egui_phosphor::regular::MAP_PIN
            ))
            .clicked()
        {
            state.push_command(Intent::SubmitZip(zip.clone()));
        }
    } else if ui
        .link(format!(
            "{} Use my location",
            egui_phosphor::regular::CROSSHAIR
        ))
        .on_hover_text("List the sites nearest you (asks the browser for your location)")
        .clicked()
    {
        state.push_command(Intent::LocateMeForSite);
    }
    if let Some(err) = &vm.location_error {
        ui.label(
            RichText::new(format!("{} {err}", egui_phosphor::regular::WARNING))
                .size(theme::CAPTION)
                .color(theme::TEXT),
        );
    }
}

/// One site row: the primary's filled circle, the ID, the place, a TDWR tag
/// and the distance. Returns whether it was clicked.
fn site_row(ui: &mut egui::Ui, row: &SiteRow) -> bool {
    let primary = row.role == SiteRole::Primary;
    let mut job = egui::text::LayoutJob::default();
    let style = ui.style().clone();
    let mut push = |text: RichText| {
        text.append_to(
            &mut job,
            &style,
            egui::FontSelection::Default,
            egui::Align::Center,
        )
    };
    // The filled circle marks the primary; other rows keep its space.
    let mark = if primary {
        theme::SELECTED
    } else {
        egui::Color32::TRANSPARENT
    };
    push(RichText::new(format!("{} ", egui_phosphor::regular::RECORD)).color(mark));
    push(RichText::new(row.id).monospace().color(theme::TEXT));
    push(RichText::new(format!("  {}", row.place)).color(theme::TEXT_DIM));
    if row.tdwr {
        push(
            RichText::new("  TDWR")
                .size(theme::CAPTION)
                .color(theme::TEXT_FAINT),
        );
    }
    if let Some(d) = &row.distance {
        push(
            RichText::new(format!("  {d}"))
                .monospace()
                .size(theme::CAPTION)
                .color(theme::TEXT_FAINT),
        );
    }
    let hover = if primary {
        "On the map now"
    } else if row.tdwr {
        "Terminal Doppler Weather Radar (airport site)"
    } else {
        "Show this site"
    };
    // Full-width, left-aligned rows.
    let response = ui
        .with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
            ui.add(egui::Button::selectable(primary, job).min_size(Vec2::new(0.0, 22.0)))
        })
        .inner;
    response.on_hover_text(hover).clicked()
}
