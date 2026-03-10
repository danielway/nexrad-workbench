//! Site selection modal overlay.
//!
//! On first visit (no preferred site saved), presents a welcome screen with
//! three ways to pick a site: browse the list, enter a zip code, or use
//! browser geolocation. On subsequent visits the modal opens directly to the
//! searchable site list.

use std::cell::RefCell;
use std::rc::Rc;

use crate::data::{all_sites_sorted, get_site, nearest_site};
use crate::state::AppState;
use eframe::egui::{self, Color32, RichText, Vec2};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// Which view the modal is currently showing.
#[derive(Default, Clone, PartialEq)]
pub enum SiteModalMode {
    /// First-visit welcome screen with three selection paths.
    #[default]
    Welcome,
    /// Searchable list of all NEXRAD sites.
    SiteList,
    /// Zip code entry form.
    ZipEntry,
    /// Waiting for an async location result (geolocation or zip lookup).
    Pending,
}

/// A location result delivered by an async operation (geolocation or zip).
pub enum LocationResult {
    /// Successfully resolved to a lat/lon.
    Success(f64, f64),
    /// The operation failed with an error message.
    Error(String),
}

/// Persistent state for the site modal.
pub struct SiteModalState {
    /// Search filter for the site list view.
    pub filter: String,
    /// Current modal view.
    pub mode: SiteModalMode,
    /// Zip code input string.
    pub zip_input: String,
    /// Error message to display (from geolocation or zip lookup).
    pub error_message: Option<String>,
    /// Shared queue for receiving async location results.
    pub location_results: Rc<RefCell<Vec<LocationResult>>>,
    /// Whether this is the first visit (no preferred site yet).
    pub is_first_visit: bool,
}

impl Default for SiteModalState {
    fn default() -> Self {
        let location_results = Rc::new(RefCell::new(Vec::new()));
        Self {
            filter: String::new(),
            mode: SiteModalMode::Welcome,
            zip_input: String::new(),
            error_message: None,
            location_results,
            is_first_visit: true,
        }
    }
}

/// Apply a site selection to app state: update viz, center camera, refresh timeline.
fn apply_site_selection(state: &mut AppState, site_id: &str, lat: f64, lon: f64) {
    state.viz_state.site_id = site_id.to_string();
    state.viz_state.center_lat = lat;
    state.viz_state.center_lon = lon;
    state.viz_state.pan_offset = Vec2::ZERO;
    state.viz_state.camera.center_on(lat, lon);
    state.push_command(crate::state::AppCommand::RefreshTimeline {
        auto_position: true,
    });
    state.preferred_site = Some(site_id.to_string());
    state.site_modal_open = false;
}

/// Start browser geolocation lookup.
fn start_geolocation(results: Rc<RefCell<Vec<LocationResult>>>, ctx: egui::Context) {
    let window = match web_sys::window() {
        Some(w) => w,
        None => {
            results
                .borrow_mut()
                .push(LocationResult::Error("No browser window".into()));
            return;
        }
    };

    let navigator = window.navigator();
    let geolocation = match navigator.geolocation() {
        Ok(g) => g,
        Err(_) => {
            results
                .borrow_mut()
                .push(LocationResult::Error("Geolocation not available".into()));
            return;
        }
    };

    let results_ok = results.clone();
    let ctx_ok = ctx.clone();
    let success_cb = Closure::once(move |position: JsValue| {
        let coords = js_sys::Reflect::get(&position, &"coords".into()).unwrap();
        let lat = js_sys::Reflect::get(&coords, &"latitude".into())
            .unwrap()
            .as_f64()
            .unwrap_or(0.0);
        let lon = js_sys::Reflect::get(&coords, &"longitude".into())
            .unwrap()
            .as_f64()
            .unwrap_or(0.0);
        results_ok
            .borrow_mut()
            .push(LocationResult::Success(lat, lon));
        ctx_ok.request_repaint();
    });

    let results_err = results.clone();
    let ctx_err = ctx;
    let error_cb = Closure::once(move |error: JsValue| {
        let msg = js_sys::Reflect::get(&error, &"message".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "Location access denied".into());
        results_err.borrow_mut().push(LocationResult::Error(msg));
        ctx_err.request_repaint();
    });

    let _ = geolocation.get_current_position_with_error_callback(
        success_cb.as_ref().unchecked_ref(),
        Some(error_cb.as_ref().unchecked_ref()),
    );

    // Prevent closures from being dropped (they need to live until the callback fires).
    success_cb.forget();
    error_cb.forget();
}

/// Start zip code geocoding via the Zippopotam.us API.
fn start_zip_lookup(zip: &str, results: Rc<RefCell<Vec<LocationResult>>>, ctx: egui::Context) {
    let url = format!("https://api.zippopotam.us/us/{}", zip);
    let results = results.clone();

    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let window = web_sys::window().ok_or("No browser window")?;
            let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(&url))
                .await
                .map_err(|_| "Network error looking up zip code".to_string())?;
            let resp: web_sys::Response = resp_value
                .dyn_into()
                .map_err(|_| "Invalid response".to_string())?;

            if !resp.ok() {
                return Err("Zip code not found".to_string());
            }

            let json = wasm_bindgen_futures::JsFuture::from(
                resp.json()
                    .map_err(|_| "Failed to parse response".to_string())?,
            )
            .await
            .map_err(|_| "Failed to read response body".to_string())?;

            // Zippopotam response: { "places": [{ "latitude": "...", "longitude": "..." }] }
            let places = js_sys::Reflect::get(&json, &"places".into())
                .map_err(|_| "Invalid response format".to_string())?;
            let first = js_sys::Reflect::get_u32(&places, 0)
                .map_err(|_| "No location data for zip code".to_string())?;

            let lat_str = js_sys::Reflect::get(&first, &"latitude".into())
                .map_err(|_| "Missing latitude".to_string())?
                .as_string()
                .ok_or("Invalid latitude")?;
            let lon_str = js_sys::Reflect::get(&first, &"longitude".into())
                .map_err(|_| "Missing longitude".to_string())?
                .as_string()
                .ok_or("Invalid longitude")?;

            let lat: f64 = lat_str
                .parse()
                .map_err(|_| "Invalid latitude value".to_string())?;
            let lon: f64 = lon_str
                .parse()
                .map_err(|_| "Invalid longitude value".to_string())?;

            Ok((lat, lon))
        }
        .await;

        match result {
            Ok((lat, lon)) => {
                results.borrow_mut().push(LocationResult::Success(lat, lon));
            }
            Err(e) => {
                results.borrow_mut().push(LocationResult::Error(e));
            }
        }
        ctx.request_repaint();
    });
}

/// Render the site selection modal if open.
///
/// Returns `true` if a site was selected (so the caller can trigger acquisition).
pub fn render_site_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    modal_state: &mut SiteModalState,
) -> bool {
    if !state.site_modal_open {
        return false;
    }

    // Poll for async location results
    let results: Vec<_> = modal_state
        .location_results
        .borrow_mut()
        .drain(..)
        .collect();
    for result in results {
        match result {
            LocationResult::Success(lat, lon) => {
                if let Some(site) = nearest_site(lat, lon) {
                    apply_site_selection(state, site.id, site.lat, site.lon);
                    modal_state.mode = if modal_state.is_first_visit {
                        SiteModalMode::Welcome
                    } else {
                        SiteModalMode::SiteList
                    };
                    modal_state.filter.clear();
                    modal_state.zip_input.clear();
                    modal_state.error_message = None;
                    return true;
                } else {
                    modal_state.error_message = Some("Could not find a nearby site".into());
                    modal_state.mode = if modal_state.is_first_visit {
                        SiteModalMode::Welcome
                    } else {
                        SiteModalMode::SiteList
                    };
                }
            }
            LocationResult::Error(msg) => {
                modal_state.error_message = Some(msg);
                modal_state.mode = if modal_state.is_first_visit {
                    SiteModalMode::Welcome
                } else {
                    SiteModalMode::SiteList
                };
            }
        }
    }

    // Escape to go back or close
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        match modal_state.mode {
            SiteModalMode::Welcome => {
                // Only allow closing if we already have a site
                if get_site(&state.viz_state.site_id).is_some() && !modal_state.is_first_visit {
                    state.site_modal_open = false;
                    return false;
                }
            }
            SiteModalMode::SiteList if modal_state.is_first_visit => {
                modal_state.mode = SiteModalMode::Welcome;
                modal_state.filter.clear();
            }
            SiteModalMode::ZipEntry if modal_state.is_first_visit => {
                modal_state.mode = SiteModalMode::Welcome;
                modal_state.zip_input.clear();
                modal_state.error_message = None;
            }
            _ => {
                state.site_modal_open = false;
                return false;
            }
        }
    }

    let mut selected = false;

    // Semi-transparent backdrop
    egui::Area::new(egui::Id::new("site_modal_backdrop"))
        .fixed_pos(egui::Pos2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let screen_rect = ctx.input(|i| i.viewport_rect());
            let (response, painter) = ui.allocate_painter(screen_rect.size(), egui::Sense::click());
            painter.rect_filled(
                screen_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 160),
            );
            // Click backdrop to close (only if not first visit)
            if response.clicked() && !modal_state.is_first_visit {
                if get_site(&state.viz_state.site_id).is_some() {
                    state.site_modal_open = false;
                }
            }
        });

    match modal_state.mode {
        SiteModalMode::Welcome => {
            selected = render_welcome_screen(ctx, state, modal_state);
        }
        SiteModalMode::SiteList => {
            selected = render_site_list(ctx, state, modal_state);
        }
        SiteModalMode::ZipEntry => {
            selected = render_zip_entry(ctx, state, modal_state);
        }
        SiteModalMode::Pending => {
            render_pending_screen(ctx);
        }
    }

    selected
}

/// Render the first-visit welcome screen with three selection paths.
fn render_welcome_screen(
    ctx: &egui::Context,
    state: &mut AppState,
    modal_state: &mut SiteModalState,
) -> bool {
    let selected = false;

    egui::Window::new("Welcome to NEXRAD Workbench")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(380.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label("Select a radar site to get started.");
            ui.add_space(12.0);

            // Show error if any
            if let Some(ref err) = modal_state.error_message {
                ui.colored_label(Color32::from_rgb(255, 120, 120), err);
                ui.add_space(8.0);
            }

            // Option 1: Use My Location
            ui.vertical_centered(|ui| {
                let btn = egui::Button::new(
                    RichText::new(format!(
                        "{} Use My Location",
                        egui_phosphor::regular::CROSSHAIR
                    ))
                    .size(15.0),
                )
                .min_size(Vec2::new(320.0, 36.0));

                if ui.add(btn).clicked() {
                    modal_state.error_message = None;
                    modal_state.mode = SiteModalMode::Pending;
                    start_geolocation(modal_state.location_results.clone(), ctx.clone());
                }
            });

            ui.add_space(6.0);

            // Option 2: Enter Zip Code
            ui.vertical_centered(|ui| {
                let btn = egui::Button::new(
                    RichText::new(format!(
                        "{} Enter Zip Code",
                        egui_phosphor::regular::MAP_PIN
                    ))
                    .size(15.0),
                )
                .min_size(Vec2::new(320.0, 36.0));

                if ui.add(btn).clicked() {
                    modal_state.error_message = None;
                    modal_state.mode = SiteModalMode::ZipEntry;
                }
            });

            ui.add_space(6.0);

            // Option 3: Browse NEXRAD Sites
            ui.vertical_centered(|ui| {
                let btn = egui::Button::new(
                    RichText::new(format!(
                        "{} Browse NEXRAD Sites",
                        egui_phosphor::regular::LIST
                    ))
                    .size(15.0),
                )
                .min_size(Vec2::new(320.0, 36.0));

                if ui.add(btn).clicked() {
                    modal_state.error_message = None;
                    modal_state.mode = SiteModalMode::SiteList;
                }
            });

            ui.add_space(8.0);

            // If reopening modal (not first visit), show a cancel option
            if !modal_state.is_first_visit {
                ui.separator();
                ui.add_space(4.0);
                ui.vertical_centered(|ui| {
                    if ui
                        .small_button(RichText::new("Cancel").color(Color32::GRAY))
                        .clicked()
                    {
                        state.site_modal_open = false;
                    }
                });
                ui.add_space(4.0);
            }
        });

    selected
}

/// Render the searchable site list view.
fn render_site_list(
    ctx: &egui::Context,
    state: &mut AppState,
    modal_state: &mut SiteModalState,
) -> bool {
    let mut selected = false;

    let title = if modal_state.is_first_visit {
        "Select Radar Site"
    } else {
        "Select Radar Site"
    };

    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(420.0, 500.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // Back button for first-visit flow
            if modal_state.is_first_visit {
                if ui
                    .small_button(RichText::new(format!(
                        "{} Back",
                        egui_phosphor::regular::ARROW_LEFT
                    )))
                    .clicked()
                {
                    modal_state.mode = SiteModalMode::Welcome;
                    modal_state.filter.clear();
                    return;
                }
                ui.add_space(4.0);
            }

            // Search/filter input
            ui.horizontal(|ui| {
                ui.label("Search:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut modal_state.filter)
                        .hint_text("Site ID, name, or state...")
                        .desired_width(300.0),
                );
                // Auto-focus the search field
                if state.site_modal_open {
                    response.request_focus();
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            // Filter sites
            let filter_upper = modal_state.filter.to_uppercase();
            let sites = all_sites_sorted();
            let filtered: Vec<_> = if modal_state.filter.is_empty() {
                sites.clone()
            } else {
                sites
                    .into_iter()
                    .filter(|s| {
                        s.id.contains(&filter_upper)
                            || s.name.contains(&filter_upper)
                            || s.state
                                .map(|st| st.to_uppercase().contains(&filter_upper))
                                .unwrap_or(false)
                    })
                    .collect()
            };

            // Site count
            ui.label(
                RichText::new(format!("{} sites", filtered.len()))
                    .small()
                    .color(Color32::GRAY),
            );

            ui.add_space(4.0);

            // Scrollable site list
            egui::ScrollArea::vertical()
                .max_height(380.0)
                .show(ui, |ui| {
                    for site in &filtered {
                        let is_current = site.id == state.viz_state.site_id;
                        let label = site.display_label();

                        let text = if is_current {
                            RichText::new(format!("{} {}", label, egui_phosphor::regular::CHECK))
                                .color(Color32::from_rgb(100, 200, 255))
                        } else {
                            RichText::new(label)
                        };

                        if ui.selectable_label(is_current, text).clicked() && !is_current {
                            apply_site_selection(state, site.id, site.lat, site.lon);
                            modal_state.filter.clear();
                            modal_state.mode = if modal_state.is_first_visit {
                                SiteModalMode::Welcome
                            } else {
                                SiteModalMode::SiteList
                            };
                            modal_state.is_first_visit = false;
                            selected = true;
                        }
                    }
                });
        });

    selected
}

/// Render the zip code entry view.
fn render_zip_entry(
    ctx: &egui::Context,
    state: &mut AppState,
    modal_state: &mut SiteModalState,
) -> bool {
    egui::Window::new("Enter Zip Code")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(340.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // Back button
            if modal_state.is_first_visit {
                if ui
                    .small_button(RichText::new(format!(
                        "{} Back",
                        egui_phosphor::regular::ARROW_LEFT
                    )))
                    .clicked()
                {
                    modal_state.mode = SiteModalMode::Welcome;
                    modal_state.zip_input.clear();
                    modal_state.error_message = None;
                    return;
                }
                ui.add_space(4.0);
            }

            ui.label("Enter a US zip code to find the nearest radar site:");
            ui.add_space(8.0);

            // Show error if any
            if let Some(ref err) = modal_state.error_message {
                ui.colored_label(Color32::from_rgb(255, 120, 120), err);
                ui.add_space(4.0);
            }

            let mut submit = false;

            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut modal_state.zip_input)
                        .hint_text("e.g. 50309")
                        .desired_width(120.0),
                );
                response.request_focus();

                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }

                if ui.button("Find Site").clicked() {
                    submit = true;
                }
            });

            if submit {
                let zip = modal_state.zip_input.trim();
                if zip.len() == 5 && zip.chars().all(|c| c.is_ascii_digit()) {
                    modal_state.error_message = None;
                    modal_state.mode = SiteModalMode::Pending;
                    start_zip_lookup(zip, modal_state.location_results.clone(), ctx.clone());
                } else {
                    modal_state.error_message =
                        Some("Please enter a valid 5-digit zip code".into());
                }
            }

            ui.add_space(8.0);

            // Cancel for non-first-visit
            if !modal_state.is_first_visit {
                ui.separator();
                ui.add_space(4.0);
                ui.vertical_centered(|ui| {
                    if ui
                        .small_button(RichText::new("Cancel").color(Color32::GRAY))
                        .clicked()
                    {
                        state.site_modal_open = false;
                    }
                });
                ui.add_space(4.0);
            }
        });

    false
}

/// Render a "please wait" screen while async operation is in progress.
fn render_pending_screen(ctx: &egui::Context) {
    egui::Window::new("Finding Nearest Site...")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(300.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.add_space(8.0);
                ui.label("Determining your location...");
            });
            ui.add_space(12.0);
        });
}
