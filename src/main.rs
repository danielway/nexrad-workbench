#![warn(clippy::all)]

//! NEXRAD Workbench — a browser-based NEXRAD weather radar visualization tool.
//!
//! This is the application entry point. It initializes the eframe/egui app, sets up
//! the coordination managers (acquisition, render, streaming, persistence), and runs
//! the main update loop that polls channels, processes commands, and renders the UI.
//!
//! Heavy data operations run in a dedicated Web Worker (see `nexrad::decode_worker`
//! and `nexrad::worker_api`). The main thread is a thin UI shell that uploads
//! worker results to the GPU and paints the interface.

mod alerts;
mod app;
mod data;
mod geo;
mod mping;
mod net;
mod nexrad;
mod state;
mod subsystem;
mod ui;

use data::DataFacade;
use eframe::egui;
use state::AppState;

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// Maximum age (in seconds) for a scan to be considered relevant to the current
/// playback position. Scans older than this are not displayed when scrubbing.
/// 15 minutes covers a full VCP cycle with margin.
const MAX_SCAN_AGE_SECS: f64 = 15.0 * 60.0;

/// How far ahead (in real-time seconds) to prefetch the next sweep when
/// playback is active. Multiplied by the playback speed to get the lookahead
/// in timeline seconds. 0.5 s keeps the pipeline one decode ahead without
/// wasting bandwidth.
const PREFETCH_LOOKAHEAD_SECS: f64 = 0.5;

/// Fallback scan duration (in seconds) used when the true end timestamp of
/// a scan boundary is unknown. 300 s (5 minutes) is a conservative upper
/// bound for a single volume scan.
const FALLBACK_SCAN_DURATION_SECS: i64 = 300;

/// Maximum time difference (in seconds) between a cached scan's start_time
/// and an archive file's timestamp for them to be considered the same scan.
/// 60 s allows for minor clock drift and timestamp rounding.
const SCAN_CACHE_MATCH_TOLERANCE_SECS: i64 = 60;

fn main() {}

// Worker exports (worker_ingest, worker_render) are in nexrad::worker_api.

/// Entry point for the WASM application.
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub async fn start() {
    // Web Workers have no window — skip app initialization
    if web_sys::window().is_none() {
        return;
    }

    use eframe::wasm_bindgen::JsCast as _;

    // Redirect `log` messages to `console.log`:
    eframe::WebLogger::init(log::LevelFilter::Debug).ok();

    let web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .expect("No window")
            .document()
            .expect("No document");

        let canvas = document
            .get_element_by_id("app_canvas")
            .expect("Failed to find app_canvas")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("app_canvas was not a HtmlCanvasElement");

        let start_result = eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| Ok(Box::new(WorkbenchApp::new(cc)))),
            )
            .await;

        // Remove the loading text once the app has loaded:
        if let Some(loading_text) = document.get_element_by_id("loading_text") {
            match start_result {
                Ok(_) => {
                    loading_text.remove();
                }
                Err(e) => {
                    loading_text.set_inner_html(
                        "<p>The app has crashed. See the developer console for details.</p>",
                    );
                    panic!("Failed to start eframe: {e:?}");
                }
            }
        }
    });
}

/// All GPU renderers and their shared GL context, grouped for clarity.
pub struct GpuResources {
    /// GPU renderer for radar data (None if GL not available).
    pub gpu: Option<std::sync::Arc<std::sync::Mutex<nexrad::RadarGpuRenderer>>>,
    /// GL context for uploading data to GPU textures.
    pub gl: Option<std::sync::Arc<glow::Context>>,
    /// Globe sphere renderer (3D mode).
    pub globe: Option<std::sync::Arc<std::sync::Mutex<geo::GlobeRenderer>>>,
    /// Geographic line renderer for 3D globe.
    pub geo_line: Option<std::sync::Arc<std::sync::Mutex<geo::GeoLineRenderer>>>,
    /// Globe-mode radar renderer (projects radar data onto sphere).
    pub globe_radar: Option<std::sync::Arc<std::sync::Mutex<nexrad::GlobeRadarRenderer>>>,
    /// Volumetric ray-march renderer for 3D mode.
    pub volume_ray: Option<std::sync::Arc<std::sync::Mutex<nexrad::VolumeRayRenderer>>>,
}

use state::MAX_RECENT_NETWORK_REQUESTS;

/// Main application state and logic.
pub struct WorkbenchApp {
    /// Application state containing all sub-states
    state: AppState,

    /// Geographic layer data for map overlays
    geo_layers: geo::GeoLayerSet,

    /// All GPU renderers and their GL context.
    gpu: GpuResources,

    /// Render subsystem: worker pool + scan/elevation tracking +
    /// sweep-animation cache.
    render: subsystem::Render,

    /// Acquisition subsystem: owns the download pipeline (channels, queue,
    /// archive index, data facade) and the per-operation tracking state
    /// (queue/operation log/drawer state) that UI panels read.
    acquisition: subsystem::Acquisition,

    /// Live subsystem: streaming channel + mode state + per-frame
    /// derived models (live radar model, top-level app mode).
    live: subsystem::Live,

    /// Timeline subsystem: scan inventory + shadow scan boundaries
    /// derived from the archive listing.
    timeline: subsystem::Timeline,

    /// Playback subsystem: cursor position, speed, mode, animation,
    /// realtime lock.
    playback: subsystem::Playback,

    /// Chrome subsystem: UI shell visibility flags + modal-open
    /// booleans (sidebars, help overlay, modals, mobile settings sheet).
    chrome: subsystem::Chrome,

    /// URL state, preferences, and site change detection.
    persistence: nexrad::PersistenceManager,

    /// Transient state for the modal UI overlays (site picker, event
    /// editor, mPING settings). Aggregated here so the three modals share
    /// one ownership and threading rule instead of three scattered fields.
    /// They live outside `AppState` so they don't need `Default + Clone`
    /// (one owns an `Rc<RefCell<>>` shared with async callbacks) and so
    /// the transient input doesn't survive a reload.
    modals: ui::ModalStates,

    /// Diagnostics subsystem: observability + peripheral telemetry
    /// overlays (NWS alerts, mPING storm reports, GPS location,
    /// service-worker network monitor).
    diagnostics: subsystem::Diagnostics,

    /// Last `AppMode` pushed to the favicon. `None` until the first frame so
    /// the initial mode is always sent. See `sync_favicon_to_mode`.
    last_favicon_mode: Option<state::AppMode>,
}

// Embed shapefile data at compile time
static STATES_SHP: &[u8] =
    include_bytes!("../assets/vectors/cb_2023_us_state_20m/cb_2023_us_state_20m.shp");
static STATES_DBF: &[u8] =
    include_bytes!("../assets/vectors/cb_2023_us_state_20m/cb_2023_us_state_20m.dbf");
static COUNTIES_SHP: &[u8] =
    include_bytes!("../assets/vectors/cb_2023_us_county_20m/cb_2023_us_county_20m.shp");
static COUNTIES_DBF: &[u8] =
    include_bytes!("../assets/vectors/cb_2023_us_county_20m/cb_2023_us_county_20m.dbf");

/// Apply parsed URL parameters to the freshly bootstrapped state +
/// playback + chrome subsystems. Extracted from `WorkbenchApp::new`
/// to keep the constructor focused on wiring up subsystems rather
/// than restoring view state.
fn apply_url_params(
    url_params: &state::url_state::UrlParams,
    state: &mut AppState,
    playback: &mut subsystem::Playback,
    chrome: &mut subsystem::Chrome,
) {
    state.dev_mode = url_params.dev;
    if let Some(advanced) = url_params.ui_advanced {
        state.advanced_mode = advanced;
    }
    if let Some(ref site) = url_params.site {
        state.viz_state.site_id = site.to_uppercase();
        if let Some(site_info) = data::sites::get_site(site) {
            state.viz_state.center_lat = site_info.lat;
            state.viz_state.center_lon = site_info.lon;
            state
                .viz_state
                .camera
                .center_on(site_info.lat, site_info.lon);
        }
        state.push_command(state::AppCommand::RefreshTimeline {
            auto_position: false,
        });
    }
    if let Some(lat) = url_params.lat {
        state.viz_state.center_lat = lat;
    }
    if let Some(lon) = url_params.lon {
        state.viz_state.center_lon = lon;
    }
    // Sync camera with potentially overridden lat/lon
    state
        .viz_state
        .camera
        .center_on(state.viz_state.center_lat, state.viz_state.center_lon);

    // Apply view state (zoom levels) before centering so the zoom is correct
    if let Some(mz) = url_params.view.mz {
        state.viz_state.zoom = mz;
    }
    if let Some(tz) = url_params.view.tz {
        playback.state.timeline_zoom = tz;
    }

    // Restore 3D view mode and camera parameters from URL
    let v = &url_params.view;
    if let Some(vm) = v.vm {
        state.viz_state.view_mode = match vm {
            0 => state::ViewMode::Flat2D,
            _ => state::ViewMode::Globe3D,
        };
    }
    if let Some(cm) = v.cm {
        state.viz_state.camera.mode = match cm {
            1 => state::CameraMode::SiteOrbit,
            2 => state::CameraMode::FreeLook,
            _ => state::CameraMode::PlanetOrbit,
        };
    }
    if let Some(cd) = v.cd {
        state.viz_state.camera.distance = cd;
    }
    if let Some(clat) = v.clat {
        state.viz_state.camera.center_lat = clat;
    }
    if let Some(clon) = v.clon {
        state.viz_state.camera.center_lon = clon;
    }
    if let Some(ct) = v.ct {
        state.viz_state.camera.tilt = ct;
    }
    if let Some(cr) = v.cr {
        state.viz_state.camera.rotation = cr;
    }
    if let Some(ob) = v.ob {
        state.viz_state.camera.orbit_bearing = ob;
    }
    if let Some(oe) = v.oe {
        state.viz_state.camera.orbit_elevation = oe;
    }
    if let Some(fp) = v.fp {
        state.viz_state.camera.free_pos = glam::Vec3::new(fp[0], fp[1], fp[2]);
    }
    if let Some(fy) = v.fy {
        state.viz_state.camera.free_yaw = fy;
    }
    if let Some(fpt) = v.fpt {
        state.viz_state.camera.free_pitch = fpt;
    }
    if let Some(fs) = v.fs {
        state.viz_state.camera.free_speed = fs;
    }
    if let Some(v3d) = v.v3d {
        state.viz_state.volume_3d_enabled = v3d;
    }
    if let Some(vdc) = v.vdc {
        state.viz_state.volume_density_cutoff = vdc;
    }

    // If the URL indicates real-time mode was active, re-enter live on boot.
    // Queued behind the initial RefreshTimeline so the timeline populates first.
    if url_params.view.rt == Some(true) {
        state.push_command(state::AppCommand::StartLive);
    }
    if let Some(ref product_code) = url_params.product {
        if let Some(product) = state::RadarProduct::from_short_code(product_code) {
            state.viz_state.product = product;
        }
    }
    if let Some(time) = url_params.time {
        playback.state.set_playback_position(time);
        // Center view on the restored position. timeline_width_px may
        // still be the default 1000px since we haven't rendered yet, but
        // it will be accurate on subsequent centers.
        playback.state.center_view_on(time);
    }

    // First-launch detection: if no site specified in the URL, check for a
    // saved preferred site. If one exists, apply it silently. Otherwise open
    // the first-visit modal so the user can choose a site.
    if url_params.site.is_none() {
        if let Some(ref preferred) = state.preferred_site {
            if let Some(site) = crate::data::get_site(preferred) {
                state.viz_state.site_id = site.id.to_string();
                state.viz_state.center_lat = site.lat;
                state.viz_state.center_lon = site.lon;
                state.viz_state.camera.center_on(site.lat, site.lon);
                // Not a first visit — modal starts in SiteList mode if reopened
            }
        } else {
            chrome.site_modal_open = true;
        }
    }
}

impl WorkbenchApp {
    /// Creates a new WorkbenchApp instance.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Initialize Phosphor icon font
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);

        let mut geo_layers = geo::GeoLayerSet::new();

        // Load embedded geographic data
        if let Err(e) = geo_layers.load_layer_from_shapefile(
            geo::GeoLayerType::States,
            STATES_SHP,
            Some(STATES_DBF),
        ) {
            log::error!("Failed to load states layer: {}", e);
        }

        if let Err(e) = geo_layers.load_layer_from_shapefile(
            geo::GeoLayerType::Counties,
            COUNTIES_SHP,
            Some(COUNTIES_DBF),
        ) {
            log::error!("Failed to load counties layer: {}", e);
        }

        // Load built-in cities layer
        geo_layers.set_layer(geo::cities::build_cities_layer());

        log::debug!(
            "Loaded geo layers: {} states, {} counties, {} cities",
            geo_layers
                .states
                .as_ref()
                .map(|l| l.features.len())
                .unwrap_or(0),
            geo_layers
                .counties
                .as_ref()
                .map(|l| l.features.len())
                .unwrap_or(0),
            geo_layers
                .cities
                .as_ref()
                .map(|l| l.features.len())
                .unwrap_or(0),
        );

        let state::AppStateBootstrap {
            mut state,
            playback: bootstrapped_playback,
            mping_api_key: loaded_mping_api_key,
        } = AppState::bootstrap();
        let mut playback = subsystem::Playback {
            state: bootstrapped_playback,
        };
        let mut chrome = subsystem::Chrome::new();

        let url_params = state::url_state::parse_from_url();
        apply_url_params(&url_params, &mut state, &mut playback, &mut chrome);

        let initial_site_id = state.viz_state.site_id.clone();
        let data_facade = DataFacade::new();
        let acquisition = subsystem::Acquisition::new(data_facade.clone());
        let realtime_channel =
            nexrad::RealtimeChannel::with_stats(acquisition.coordinator.download_stats());

        // Open the record cache database
        {
            let facade = data_facade.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = facade.open().await {
                    log::error!("Failed to open record cache: {}", e);
                }
            });
        }

        let initial_prefs = state::UserPreferences::from_app_state(
            &state,
            &playback.state,
            loaded_mping_api_key.clone(),
        );
        let has_preferred_site = state.preferred_site.is_some();

        // Create decode worker pool (offloads heavy NEXRAD work to parallel
        // Web Workers so the download → decompress → store pipeline can fan
        // out across cores instead of serializing through a single worker).
        let pool_size = nexrad::default_pool_size();
        let decode_worker = match nexrad::WorkerPool::new(cc.egui_ctx.clone(), pool_size) {
            Ok(pool) => Some(pool),
            Err(e) => {
                log::warn!("Failed to create decode worker pool: {}", e);
                state.worker_init_error =
                    Some(format!("Decode worker failed to initialize: {}", e));
                None
            }
        };

        // Create GPU renderer for radar visualization
        let gpu_renderer_gl = cc.gl.clone();
        let gpu_renderer = cc
            .gl
            .as_ref()
            .and_then(|gl| match nexrad::RadarGpuRenderer::new(gl) {
                Ok(renderer) => Some(std::sync::Arc::new(std::sync::Mutex::new(renderer))),
                Err(e) => {
                    log::error!("Failed to create GPU radar renderer: {}", e);
                    None
                }
            });

        // Create globe and geo-line renderers for 3D mode
        let globe_renderer = cc.gl.as_ref().map(|gl| {
            let r = geo::GlobeRenderer::new(gl);
            std::sync::Arc::new(std::sync::Mutex::new(r))
        });
        let geo_line_renderer = cc.gl.as_ref().map(|gl| {
            let mut r = geo::GeoLineRenderer::new(gl);
            // Upload all static geo layers now
            let layers_vec: Vec<&geo::GeoLayer> = [
                geo_layers.states.as_ref(),
                geo_layers.counties.as_ref(),
                geo_layers.highways.as_ref(),
                geo_layers.lakes.as_ref(),
            ]
            .into_iter()
            .flatten()
            .collect();
            let owned: Vec<geo::GeoLayer> = layers_vec.into_iter().cloned().collect();
            r.upload_layers(gl, &owned);
            std::sync::Arc::new(std::sync::Mutex::new(r))
        });
        let globe_radar_renderer = cc.gl.as_ref().map(|gl| {
            let r = nexrad::GlobeRadarRenderer::new(gl);
            std::sync::Arc::new(std::sync::Mutex::new(r))
        });
        let volume_ray_renderer = cc.gl.as_ref().map(|gl| {
            let r = nexrad::VolumeRayRenderer::new(gl);
            std::sync::Arc::new(std::sync::Mutex::new(r))
        });

        let mut app = Self {
            state,
            geo_layers,
            gpu: GpuResources {
                gpu: gpu_renderer,
                gl: gpu_renderer_gl,
                globe: globe_renderer,
                geo_line: geo_line_renderer,
                globe_radar: globe_radar_renderer,
                volume_ray: volume_ray_renderer,
            },
            render: subsystem::Render::new(nexrad::RenderCoordinator::new(decode_worker)),
            acquisition,
            live: subsystem::Live::new(realtime_channel),
            timeline: subsystem::Timeline::default(),
            playback,
            chrome,
            persistence: nexrad::PersistenceManager::new(initial_site_id, initial_prefs),
            modals: ui::ModalStates::new(has_preferred_site),
            diagnostics: {
                let mut diag = subsystem::Diagnostics::new();
                // Apply the persisted mPING API key (loaded from prefs at
                // AppState construction; mPING state lives on the
                // subsystem so it can't be applied inside AppState::new).
                diag.mping.api_key = loaded_mping_api_key;
                diag
            },
            last_favicon_mode: None,
        };

        // Check cross-origin isolation status on startup
        app.state.cross_origin_isolated = nexrad::is_cross_origin_isolated();
        if !app.state.cross_origin_isolated {
            log::warn!("Not cross-origin isolated: SharedArrayBuffer unavailable");
        }

        // Attach the service-worker network monitor only in dev mode. When
        // toggled on later, `update_network_stats` will lazily attach it.
        if app.state.dev_mode {
            app.diagnostics.network_monitor = nexrad::NetworkMonitor::new();
        }

        app
    }

    /// Start live mode streaming for the current site.
    fn update_network_stats(&mut self) {
        // Update session stats from live network statistics
        let network_stats = self.acquisition.coordinator.download_channel.stats();
        self.state
            .session_stats
            .update_from_network_stats(&network_stats);

        // Service worker metrics are only collected in dev mode. Lazily
        // attach the listener the first time dev mode becomes active.
        if !self.state.dev_mode {
            return;
        }
        if self.diagnostics.network_monitor.is_none() {
            self.diagnostics.network_monitor = nexrad::NetworkMonitor::new();
        }

        // Drain service worker network metrics into app state
        if let Some(ref monitor) = self.diagnostics.network_monitor {
            self.state.network_aggregate = monitor.aggregate();
            let mut pending = monitor.take_pending();
            if !pending.is_empty() {
                // Correlate each new request exactly once, then append to
                // the app-level ring. The previous implementation
                // re-cloned and re-correlated the entire ring every frame
                // regardless of whether anything had changed.
                for req in pending.iter_mut() {
                    req.operation_id = self.acquisition.state.correlate_network_request(&req.url);
                }
                let ring = &mut self.state.recent_network_requests;
                ring.reserve(pending.len());
                for req in pending {
                    if ring.len() >= MAX_RECENT_NETWORK_REQUESTS {
                        ring.pop_front();
                    }
                    ring.push_back(req);
                }
            }
        }
    }

    /// Push current app state to the URL bar and save user preferences (throttled).
    fn persist_url_state(&mut self) {
        self.persistence.persist_if_due(
            &self.state,
            &self.playback.state,
            self.diagnostics.mping.api_key.clone(),
            self.live.mode_state.is_active(),
        );
    }

    /// Push the current `AppMode`'s color to the browser favicon via the
    /// `setFaviconColor` JS hook in `index.html`. No-op when the mode hasn't
    /// changed since the last push.
    fn sync_favicon_to_mode(&mut self) {
        use wasm_bindgen::prelude::*;
        #[wasm_bindgen]
        extern "C" {
            #[wasm_bindgen(js_namespace = window, js_name = setFaviconColor, catch)]
            fn js_set_favicon_color(hex: &str) -> Result<(), JsValue>;
        }

        let mode = self.live.app_mode;
        if self.last_favicon_mode == Some(mode) {
            return;
        }
        let c = mode.color();
        let hex = format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
        let _ = js_set_favicon_color(&hex);

        // Prefix the document title so backgrounded tabs surface the mode.
        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
            let prefix = match mode {
                state::AppMode::Idle => "",
                state::AppMode::Archive => "[ARCHIVE] ",
                state::AppMode::Live => "[LIVE] ",
            };
            document.set_title(&format!("{}NEXRAD Workbench", prefix));
        }

        self.last_favicon_mode = Some(mode);
    }
}

impl eframe::App for WorkbenchApp {
    // Frame orchestration. The sequence below is load-bearing; reordering
    // steps can cause one-frame lag, dropped renders, or UI reading stale
    // state. Stages are grouped as:
    //
    //   PER-FRAME SETUP    1
    //   INTAKE             2..=5      drain user/worker/network inputs
    //   BACKGROUND TICKS   6..=8      independent periodic work
    //   COMPUTE            9..=13     advance playback, request next render
    //   FRAME SNAPSHOT     14..=18    materialize state UI will read
    //   RENDER             19..=21    egui panels, canvas, overlays
    //
    // Invariants worth preserving:
    //   - (2) dispatch_commands must precede (4) pump_download_queue because
    //     it returns the CommandOutcome the pump consumes. The pump waits
    //     until AFTER (3) handle_worker_results so newly-decoded sweeps are
    //     visible before download decisions are made.
    //   - (3) handle_worker_results applies decoded-sweep state that
    //     (10) sync_prev_sweep_texture and (11) request_render_if_needed
    //     read this same frame — running results first avoids one-frame lag.
    //   - (9) advance_playback must precede (11) request_render_if_needed so
    //     a new playback position can trigger a render in the same frame.
    //   - (14) refresh_live_model and (15) refresh_mobile_mode produce the
    //     per-frame snapshots UI panels (19) read — they must run before
    //     panel render.
    //   - (16) gps drain must precede UI render so the "My Location"
    //     checkbox reflects the geolocation callback on the same frame.
    //   - (19) side/top/bottom panels must render before the CentralPanel
    //     (canvas in step 20); this is an egui layout requirement.
    //   - (21) modal overlays render last so they layer above the canvas.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. PER-FRAME SETUP: theme, staleness, site-change cleanup.
        self.apply_frame_setup(ctx);

        // 2-5. INTAKE: drain user commands, worker responses, the
        // download queue, and realtime streaming results.
        let command_outcome = self.dispatch_commands(ctx);
        self.handle_worker_results(ctx);
        self.pump_download_queue(ctx, &command_outcome);
        self.handle_streaming_results(ctx);
        // 6-8. BACKGROUND TICKS: independent periodic work.
        // Take an early Derived snapshot so subsystem ticks consume the
        // same `data_is_live` value the panels will (and recomputation
        // is centralised).
        let early_derived = subsystem::Derived::for_frame(&self.state, &self.playback);
        self.state.national_mosaic.poll_tick(
            ctx,
            self.state.layer_state.geo.national_mosaic && early_derived.data_is_live,
        );
        // NWS alerts and mPING storm reports — polled if due.
        let diagnostics_inputs = subsystem::diagnostics::DiagnosticsInputs {
            is_live: early_derived.data_is_live,
            mping_layer_visible: self.state.layer_state.geo.mping,
            site_id: &self.state.viz_state.site_id,
            playback_secs: self.playback.state.playback_position(),
        };
        self.diagnostics
            .tick(ctx, diagnostics_inputs, &mut self.state.errors);

        // 9-13. COMPUTE: advance playback, sync GPU state, decide whether
        // to issue the next render, then capture network stats and persist.
        self.advance_playback();
        self.sync_prev_sweep_texture();
        self.request_render_if_needed();
        self.update_network_stats();
        self.persist_url_state();

        // 14-17. FRAME SNAPSHOT: materialize the per-frame state UI reads.
        // Live::refresh captures a consistent `now` for every consumer.
        self.live.refresh(subsystem::live::LiveRefreshInputs {
            radar_timeline: &self.timeline.scans,
            playback: &self.playback.state,
        });
        self.state.refresh_mobile_mode(ctx);

        // Drain GPS-overlay async results before panels render so the
        // "My Location" checkbox sees coords/error on the same frame the
        // geolocation callback fires.
        for r in self.diagnostics.gps.drain_results() {
            match r {
                ui::LocationResult::Success(lat, lon) => {
                    self.diagnostics.gps.coords = Some((lat, lon));
                    self.diagnostics.gps.error = None;
                }
                ui::LocationResult::Error(msg) => {
                    self.diagnostics.gps.error = Some(msg);
                    self.diagnostics.gps.coords = None;
                    self.state.layer_state.geo.gps_location = false;
                }
            }
        }

        // 18. Recolor the favicon if the AppMode changed this frame.
        self.sync_favicon_to_mode();

        // Per-frame chrome animations that should tick in both layouts.
        // Hoisted out of `render_bottom_panel` so the mobile path doesn't
        // have to call it as a no-op side-effect carrier.
        let dt = ctx.input(|i| i.stable_dt);
        self.live.mode_state.update_pulse(dt);

        // Re-materialise the per-frame Derived snapshot here so panel
        // renders see live-mode pulse + the freshest playback position.
        // (An earlier copy was already taken before the diagnostics tick
        // so `data_is_live` flows into that consumer.)
        let derived = subsystem::Derived::for_frame(&self.state, &self.playback);

        // 19. Consume any deferred geolocation request raised by the
        // mobile action bar. Handled before layout dispatch because the
        // site-modal state lives outside AppState and the handler needs
        // unique access to both `chrome` and `modals.site`.
        if self.state.is_mobile && self.chrome.mobile_geolocate_requested {
            self.chrome.mobile_geolocate_requested = false;
            ui::trigger_geolocation(
                ctx,
                &mut self.state,
                &mut self.chrome,
                &mut self.modals.site,
            );
        }

        // 20. RENDER (layout tree): chrome panels + all modals dispatched
        // through the declarative `Layer` registry. The desktop and
        // mobile layouts pick the chrome panel set; the modal set is
        // shared. Visibility predicates absorb per-panel and per-modal
        // visibility guards that previously lived in each function body.
        let is_mobile = self.state.is_mobile;
        let mut layout_ctx = ui::LayoutCtx {
            ctx,
            state: &mut self.state,
            timeline: &self.timeline,
            live: &mut self.live,
            playback: &mut self.playback,
            acquisition: &mut self.acquisition,
            chrome: &mut self.chrome,
            diagnostics: &mut self.diagnostics,
            derived: &derived,
            modals: &mut self.modals,
        };
        ui::render_layout(is_mobile, &mut layout_ctx);

        // 21. RENDER (canvas): GPU-based radar rendering in the CentralPanel.
        ui::render_canvas_with_geo(
            ctx,
            &mut self.state,
            &self.timeline,
            &self.live,
            &mut self.playback,
            &mut self.chrome,
            &mut self.diagnostics,
            &derived,
            Some(&self.geo_layers),
            &self.gpu,
        );

        // Keyboard shortcuts (after canvas so shortcuts can reflect hover/focus).
        ui::handle_shortcuts(
            ctx,
            &mut self.state,
            &mut self.live,
            &self.timeline,
            &mut self.playback,
            &mut self.chrome,
        );
    }
}

// `cargo test` compiles a single test binary from this bin crate that
// includes every `#[cfg(test)] mod tests` block across `src/`.
// `#[wasm_bindgen_test]` defaults to executing tests in node (no browser,
// no real IndexedDB) — fast enough to run in the pre-commit hook.
//
// Functional IDB tests that need a real browser will live in a separate
// `tests/idb.rs` integration crate (deferred) with
// `wasm_bindgen_test_configure!(run_in_browser)`.
