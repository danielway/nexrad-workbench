//! Site selection modal overlay.
//!
//! Always presents a welcome/selection screen with three ways to pick a site:
//! browse the list, enter a zip code, or use browser geolocation. On first
//! visit (no preferred site saved), shows welcome verbiage; on subsequent
//! visits, shows a shorter "change site" heading instead.

use crate::data::{all_sites_sorted, get_site, nearest_site};
use crate::net::retry::{with_retry, Verdict, DEFAULT_POLICY};
use crate::state::AppState;
use eframe::egui::{self, Color32, RichText, Vec2};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};
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
pub(crate) enum LocationResult {
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
    /// Sender given to async callbacks. Clone freely.
    location_tx: UnboundedSender<LocationResult>,
    /// Receiver drained inside `render_site_modal` each frame.
    location_rx: UnboundedReceiver<LocationResult>,
    /// Whether this is the first visit (no preferred site yet).
    pub is_first_visit: bool,
}

impl SiteModalState {
    /// A clone-able sink for async callbacks (geolocation, zip lookup).
    pub fn location_sender(&self) -> UnboundedSender<LocationResult> {
        self.location_tx.clone()
    }

    /// Drain all location results that have arrived since the last call.
    pub fn drain_location_results(&mut self) -> Vec<LocationResult> {
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
            filter: String::new(),
            mode: SiteModalMode::Welcome,
            zip_input: String::new(),
            error_message: None,
            location_tx,
            location_rx,
            is_first_visit: true,
        }
    }
}

/// Pick a modal window width that fits the current viewport. Narrow devices
/// (phones) shrink the modal to leave a small gutter; wider viewports clamp
/// to `desktop` so the modal isn't absurdly wide on a big monitor.
fn responsive_width(ctx: &egui::Context, desktop: f32) -> f32 {
    let viewport_w = ctx.input(|i| i.viewport_rect()).width();
    (viewport_w - 16.0).min(desktop).max(240.0)
}

/// Open the site modal in `Pending` mode and start browser geolocation.
///
/// Used by the mobile bottom bar's location button to bypass the welcome
/// screen and go straight to "finding nearest site". The polling loop in
/// `render_site_modal` handles the result — success closes the modal after
/// applying the selection, failure drops back to the welcome screen with
/// the error visible.
pub fn trigger_geolocation(
    ctx: &egui::Context,
    _state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
    modal_state: &mut SiteModalState,
) {
    chrome.site_modal_open = true;
    modal_state.mode = SiteModalMode::Pending;
    modal_state.error_message = None;
    start_geolocation(modal_state.location_sender(), ctx.clone());
}

/// Apply a site selection to app state: update viz, center camera, refresh timeline.
pub(super) fn apply_site_selection(
    state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
    site_id: &str,
    lat: f64,
    lon: f64,
) {
    state.viz_state.site_id = site_id.to_string();
    state.viz_state.center_lat = lat;
    state.viz_state.center_lon = lon;
    state.viz_state.pan_offset = Vec2::ZERO;
    state.viz_state.camera.center_on(lat, lon);
    state.push_command(crate::state::AppCommand::RefreshTimeline {
        auto_position: true,
    });
    state.push_command(crate::state::AppCommand::RefreshAlerts);
    state.preferred_site = Some(site_id.to_string());
    chrome.site_modal_open = false;
}

/// Start browser geolocation lookup.
///
/// `results` is an unbounded mpsc sender that the success/error callbacks
/// push their outcome into. The caller is responsible for draining the
/// corresponding receiver each frame.
pub(crate) fn start_geolocation(
    results: futures_channel::mpsc::UnboundedSender<LocationResult>,
    ctx: egui::Context,
) {
    let window = match web_sys::window() {
        Some(w) => w,
        None => {
            let _ = results.unbounded_send(LocationResult::Error("No browser window".into()));
            return;
        }
    };

    let navigator = window.navigator();
    let geolocation = match navigator.geolocation() {
        Ok(g) => g,
        Err(_) => {
            let _ =
                results.unbounded_send(LocationResult::Error("Geolocation not available".into()));
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
        let _ = results_ok.unbounded_send(LocationResult::Success(lat, lon));
        ctx_ok.request_repaint();
    });

    let results_err = results;
    let ctx_err = ctx;
    let error_cb = Closure::once(move |error: JsValue| {
        let msg = js_sys::Reflect::get(&error, &"message".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "Location access denied".into());
        let _ = results_err.unbounded_send(LocationResult::Error(msg));
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
fn start_zip_lookup(zip: &str, results: UnboundedSender<LocationResult>, ctx: egui::Context) {
    let url = format!("https://api.zippopotam.us/us/{}", zip);

    wasm_bindgen_futures::spawn_local(async move {
        let result: Result<(f64, f64), String> =
            with_retry(&DEFAULT_POLICY, "zip_lookup", |_attempt| {
                let url = url.clone();
                async move { zip_lookup_attempt(&url).await }
            })
            .await
            .map_err(|msg| {
                // Zippopotam returns 404 for invalid zips; surface a friendlier
                // message than the raw HTTP status.
                if msg.contains("HTTP 404") {
                    "Zip code not found".to_string()
                } else {
                    msg
                }
            });

        let payload = match result {
            Ok((lat, lon)) => LocationResult::Success(lat, lon),
            Err(e) => LocationResult::Error(e),
        };
        let _ = results.unbounded_send(payload);
        ctx.request_repaint();
    });
}

/// One attempt against the Zippopotam.us API. Network errors and 5xx are
/// retryable; 404 (invalid zip) and parse failures are terminal.
async fn zip_lookup_attempt(url: &str) -> Verdict<(f64, f64)> {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return Verdict::Terminal("No browser window".into()),
    };

    let resp_value = match wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await {
        Ok(v) => v,
        Err(_) => return Verdict::Retry { after: None },
    };
    let resp: web_sys::Response = match resp_value.dyn_into() {
        Ok(r) => r,
        Err(_) => return Verdict::Terminal("Invalid response".into()),
    };

    let status = resp.status();
    if status == 408 || status == 429 || (500..=599).contains(&status) {
        return Verdict::Retry { after: None };
    }
    if !resp.ok() {
        return Verdict::Terminal(format!("HTTP {}", status));
    }

    let json_promise = match resp.json() {
        Ok(p) => p,
        Err(_) => return Verdict::Terminal("Failed to parse response".into()),
    };
    let json = match wasm_bindgen_futures::JsFuture::from(json_promise).await {
        Ok(v) => v,
        Err(_) => return Verdict::Retry { after: None },
    };

    // Zippopotam response: { "places": [{ "latitude": "...", "longitude": "..." }] }
    let places = match js_sys::Reflect::get(&json, &"places".into()) {
        Ok(p) => p,
        Err(_) => return Verdict::Terminal("Invalid response format".into()),
    };
    let first = match js_sys::Reflect::get_u32(&places, 0) {
        Ok(f) => f,
        Err(_) => return Verdict::Terminal("No location data for zip code".into()),
    };

    let lat_str = match js_sys::Reflect::get(&first, &"latitude".into()) {
        Ok(v) => match v.as_string() {
            Some(s) => s,
            None => return Verdict::Terminal("Invalid latitude".into()),
        },
        Err(_) => return Verdict::Terminal("Missing latitude".into()),
    };
    let lon_str = match js_sys::Reflect::get(&first, &"longitude".into()) {
        Ok(v) => match v.as_string() {
            Some(s) => s,
            None => return Verdict::Terminal("Invalid longitude".into()),
        },
        Err(_) => return Verdict::Terminal("Missing longitude".into()),
    };

    let lat: f64 = match lat_str.parse() {
        Ok(v) => v,
        Err(_) => return Verdict::Terminal("Invalid latitude value".into()),
    };
    let lon: f64 = match lon_str.parse() {
        Ok(v) => v,
        Err(_) => return Verdict::Terminal("Invalid longitude value".into()),
    };

    Verdict::Ok((lat, lon))
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
        draw_site_modal(ctx.ctx, ctx.state, ctx.chrome, &mut ctx.modals.site);
    }
}

/// Returns `true` if a site was selected (so the caller can trigger acquisition).
///
/// Currently the caller ignores the return value — the site-selection flow
/// dispatches `AppCommand::ChangeSite` via `apply_site_selection` instead.
/// Kept for the standalone-call-site path used by tests.
fn draw_site_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
    modal_state: &mut SiteModalState,
) -> bool {
    // Poll for async location results
    for result in modal_state.drain_location_results() {
        match result {
            LocationResult::Success(lat, lon) => {
                if let Some(site) = nearest_site(lat, lon) {
                    apply_site_selection(state, chrome, site.id, site.lat, site.lon);
                    modal_state.mode = SiteModalMode::Welcome;
                    modal_state.filter.clear();
                    modal_state.zip_input.clear();
                    modal_state.error_message = None;
                    return true;
                } else {
                    modal_state.error_message = Some("Could not find a nearby site".into());
                    modal_state.mode = SiteModalMode::Welcome;
                }
            }
            LocationResult::Error(msg) => {
                modal_state.error_message = Some(msg);
                modal_state.mode = SiteModalMode::Welcome;
            }
        }
    }

    // Escape to go back or close
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        match modal_state.mode {
            SiteModalMode::Welcome => {
                // Only allow closing if we already have a site
                if get_site(&state.viz_state.site_id).is_some() && !modal_state.is_first_visit {
                    chrome.site_modal_open = false;
                    return false;
                }
            }
            SiteModalMode::SiteList => {
                modal_state.mode = SiteModalMode::Welcome;
                modal_state.filter.clear();
            }
            SiteModalMode::ZipEntry => {
                modal_state.mode = SiteModalMode::Welcome;
                modal_state.zip_input.clear();
                modal_state.error_message = None;
            }
            _ => {
                chrome.site_modal_open = false;
                return false;
            }
        }
    }

    let mut selected = false;

    // Semi-transparent backdrop
    egui::Area::new(egui::Id::new("site_modal_backdrop"))
        .fixed_pos(egui::Pos2::ZERO)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let screen_rect = ctx.input(|i| i.viewport_rect());
            let (response, painter) = ui.allocate_painter(screen_rect.size(), egui::Sense::click());
            painter.rect_filled(
                screen_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 160),
            );
            // Click backdrop to close (only if not first visit)
            if response.clicked()
                && !modal_state.is_first_visit
                && get_site(&state.viz_state.site_id).is_some()
            {
                chrome.site_modal_open = false;
            }
        });

    match modal_state.mode {
        SiteModalMode::Welcome => {
            selected = render_welcome_screen(ctx, state, chrome, modal_state);
        }
        SiteModalMode::SiteList => {
            selected = render_site_list(ctx, state, chrome, modal_state);
        }
        SiteModalMode::ZipEntry => {
            selected = render_zip_entry(ctx, state, chrome, modal_state);
        }
        SiteModalMode::Pending => {
            render_pending_screen(ctx);
        }
    }

    selected
}

/// Render the selection method screen with three paths (location, zip, browse).
fn render_welcome_screen(
    ctx: &egui::Context,
    _state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
    modal_state: &mut SiteModalState,
) -> bool {
    let selected = false;

    let title = if modal_state.is_first_visit {
        "Welcome to NEXRAD Workbench"
    } else {
        "Change Radar Site"
    };

    let window_w = responsive_width(ctx, 380.0);
    let btn_w = (window_w - 36.0).max(200.0);

    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(window_w, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            if modal_state.is_first_visit {
                ui.label("Select a radar site to get started.");
            } else {
                ui.label("Select a new radar site.");
            }
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
                .min_size(Vec2::new(btn_w, 44.0));

                if ui.add(btn).clicked() {
                    modal_state.error_message = None;
                    modal_state.mode = SiteModalMode::Pending;
                    start_geolocation(modal_state.location_sender(), ctx.clone());
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
                .min_size(Vec2::new(btn_w, 44.0));

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
                .min_size(Vec2::new(btn_w, 44.0));

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
                        chrome.site_modal_open = false;
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
    chrome: &mut crate::subsystem::Chrome,
    modal_state: &mut SiteModalState,
) -> bool {
    let mut selected = false;

    let title = "Select Radar Site";

    let window_w = responsive_width(ctx, 420.0);
    let viewport_h = ctx.input(|i| i.viewport_rect()).height();
    let window_h = (viewport_h - 80.0).clamp(320.0, 500.0);
    let search_w = (window_w - 80.0).max(120.0);

    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(window_w, window_h))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // Back button to return to the selection method screen
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

            // Search/filter input
            ui.horizontal(|ui| {
                ui.label("Search:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut modal_state.filter)
                        .hint_text("Site ID, name, or state...")
                        .desired_width(search_w),
                );
                // Auto-focus the search field
                if chrome.site_modal_open {
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

            // Enter key selects the site when filter narrows to exactly one result
            let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter_pressed && filtered.len() == 1 {
                let site = &filtered[0];
                if site.id != state.viz_state.site_id {
                    apply_site_selection(state, chrome, site.id, site.lat, site.lon);
                    modal_state.filter.clear();
                    modal_state.mode = SiteModalMode::Welcome;
                    modal_state.is_first_visit = false;
                    selected = true;
                }
            }

            // Site count
            ui.label(
                RichText::new(format!("{} sites", filtered.len()))
                    .small()
                    .color(Color32::GRAY),
            );

            ui.add_space(4.0);

            // Scrollable site list — height scales with the modal.
            let list_h = (window_h - 120.0).max(200.0);
            egui::ScrollArea::vertical()
                .max_height(list_h)
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
                            apply_site_selection(state, chrome, site.id, site.lat, site.lon);
                            modal_state.filter.clear();
                            modal_state.mode = SiteModalMode::Welcome;
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
    _state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
    modal_state: &mut SiteModalState,
) -> bool {
    let window_w = responsive_width(ctx, 340.0);

    egui::Window::new("Enter Zip Code")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(window_w, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // Back button to return to the selection method screen
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

            ui.label("Enter a US zip code to find the nearest radar site:");
            ui.add_space(8.0);

            // Show error if any
            if let Some(ref err) = modal_state.error_message {
                ui.colored_label(Color32::from_rgb(255, 120, 120), err);
                ui.add_space(4.0);
            }

            let mut submit = false;

            // Enter key submits the form
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }

            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut modal_state.zip_input)
                        .hint_text("e.g. 50309")
                        .desired_width(120.0),
                );
                response.request_focus();

                if ui.button("Find Site").clicked() {
                    submit = true;
                }
            });

            if submit {
                let zip = modal_state.zip_input.trim();
                if zip.len() == 5 && zip.chars().all(|c| c.is_ascii_digit()) {
                    modal_state.error_message = None;
                    modal_state.mode = SiteModalMode::Pending;
                    start_zip_lookup(zip, modal_state.location_sender(), ctx.clone());
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
                        chrome.site_modal_open = false;
                    }
                });
                ui.add_space(4.0);
            }
        });

    false
}

/// Render a "please wait" screen while async operation is in progress.
fn render_pending_screen(ctx: &egui::Context) {
    let window_w = responsive_width(ctx, 300.0);
    egui::Window::new("Finding Nearest Site...")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(window_w, 0.0))
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
