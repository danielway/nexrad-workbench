#![warn(clippy::all)]
#![warn(unreachable_pub)]
// Test fixtures routinely build a default and then set the two or three fields
// the case is about. Struct-update syntax reads worse there — the point of the
// fixture is *which fields this test changes* — and the lint's perf rationale
// doesn't apply to a test. Production code is still held to it.
#![cfg_attr(test, allow(clippy::field_reassign_with_default))]

//! NEXRAD Workbench — a browser-based NEXRAD weather radar visualization tool.
//!
//! This is the application entry point. It initializes the eframe/egui app, sets up
//! the coordination managers (acquisition, render, streaming, persistence), and runs
//! the main update loop that polls channels, processes commands, and renders the UI.
//!
//! Heavy data operations run in a dedicated Web Worker (see `nexrad::decode::decode_worker`
//! and `nexrad::decode::worker_api`). The main thread is a thin UI shell that uploads
//! worker results to the GPU and paints the interface.

mod alerts;
mod app;
mod core;
#[allow(unreachable_pub)] // pub surface is the lib facade for tests/idb.rs
mod data;
mod geo;
mod mping;
mod net;
mod nexrad;
mod state;
mod subsystem;
mod ui;

use data::MainThreadStore;
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

/// Reactive (implicit) data-acquisition tuning. These bound *archive*
/// prefetch — fetching scans as a side effect of navigation (PRODUCT.md §5).
///
/// `PREFETCH_DEBOUNCE_MS`: the view (playback position, filter) must be stable
/// this long before a prefetch fires, so transient scrub/zoom positions don't
/// trigger downloads. Collapses to zero during playback.
const PREFETCH_DEBOUNCE_MS: f64 = 300.0;
/// Real-time seconds of lead to keep buffered during playback; multiplied by
/// the playback speed so fast playback fetches proportionally further ahead
/// (floored at one scan — `FALLBACK_SCAN_DURATION_SECS` — so playback never
/// waits on a cold fetch at a scan boundary). While paused there is no lead
/// at all: only the scan under the playhead is fetched.
const PREFETCH_PLAY_LEAD_SECS: f64 = 4.0;

/// Fallback scan duration (in seconds) used when the true end timestamp of
/// a scan boundary is unknown. 300 s (5 minutes) is a conservative upper
/// bound for a single volume scan.
const FALLBACK_SCAN_DURATION_SECS: i64 = 300;

/// Timeline zoom (px/sec) live mode floors to so the strip shows individual
/// sweeps and chunks. Comfortably above the Micro-enter tier threshold so
/// going/returning live lands the tier in Micro.
const LIVE_DEFAULT_ZOOM: f64 = 2.0;

/// Maximum time difference (in seconds) between a cached scan's start_time
/// and an archive file's timestamp for them to be considered the same scan.
/// Aliases the timeline's single scan-start join tolerance so there is exactly
/// one number governing "same scan" decisions across acquisition and the strip.
const SCAN_CACHE_MATCH_TOLERANCE_SECS: i64 = crate::core::SCAN_JOIN_TOLERANCE_SECS;

/// A finalized timeline selection at or under this span downloads its scans
/// immediately; a longer span first asks for confirmation (the bulk download
/// could be large). 6 hours ≈ 70+ volumes — a sensible "are you sure" line.
const SELECTION_BULK_CONFIRM_SECS: f64 = 6.0 * 3600.0;

/// Hard backstop for the selection-fetch pump: if a date's listing never
/// arrives within this window, the pump disarms rather than staying armed
/// forever. Guarantees termination regardless of network outcome.
const SELECTION_FETCH_DEADLINE_SECS: f64 = 30.0;

/// Approximate compressed bytes per volume scan, used only to estimate a
/// selection's download size. The S3 listing exposes no file sizes, so this is
/// a rough, tunable constant rather than a measured value. Defined in
/// [`core::domain::ops`] and re-exported here for the existing
/// `crate::AVG_SCAN_BYTES` call sites.
use core::AVG_SCAN_BYTES;

/// How long a live stream keeps ingesting after the playhead detaches (the
/// user scrubbed away to browse) before it auto-stops. Bounds background S3
/// chunk polling while still making "return to live" instant for any
/// realistic browsing detour. The `pause_stream_while_reviewing` preference
/// stops immediately instead; this is the safety backstop for the default-off
/// case (alignment §5: raised from 15 to 60 min).
const LIVE_DETACHED_STOP_SECS: f64 = 60.0 * 60.0;

fn main() {}

// Worker exports (worker_ingest, worker_render) are in nexrad::decode::worker_api.

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
    persistence: app::persistence_manager::PersistenceManager,

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
        state.push_command(crate::core::Intent::RefreshTimeline {
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
        state.viz_state.set_zoom(mz);
    }
    if let Some(tz) = url_params.view.tz {
        // Clamp restored zoom into the *width-aware* range so an old link with
        // an absurdly small (year-wide) zoom lands at the widest readable
        // calendar span instead of decades out (spec §6.4 DECIDED). Width isn't
        // measured yet at boot, so use the seeded `timeline_width_px`; the
        // per-frame reconcile corrects the tier once the real width is known.
        let min = crate::core::PlaybackState::min_zoom_for_width(playback.state.timeline_width_px);
        playback.state.timeline_zoom = tz.clamp(min, crate::core::TIMELINE_ZOOM_MAX);
        // Seed the tier deterministically from the restored zoom+width (no
        // hysteresis memory at boot). The per-frame reconcile corrects it once
        // the real strip width is measured.
        playback.state.seed_tier_from_state();
    }

    // Restore 3D view mode and camera parameters from URL. The camera is the
    // single source of truth for the view mode, so a globe link reconstructs
    // the orbit camera from the saved snapshot; a 2D link leaves the camera
    // in its (already-centered) Flat2D state.
    let v = &url_params.view;
    let wants_3d = v.vm.is_some_and(|vm| vm != 0);
    if wants_3d {
        // The pure legacy mapping in `restore_from_url_fields` handles both
        // new-format links and pre-overhaul per-mode (`cm`) links.
        state
            .viz_state
            .camera
            .restore_from_url_fields(&crate::geo::UrlOrbitFields {
                cm: v.cm,
                cd: v.cd,
                clat: v.clat,
                clon: v.clon,
                ct: v.ct,
                cr: v.cr,
                ob: v.ob,
                oe: v.oe,
            });
    }
    if let Some(v3d) = v.v3d {
        state.viz_state.volume_3d_enabled = v3d;
    }
    if let Some(vdc) = v.vdc {
        state.viz_state.volume_density_cutoff = vdc;
    }

    if let Some(ref product_code) = url_params.product {
        if let Some(product) = crate::core::RadarProduct::from_short_code(product_code) {
            state.viz_state.product = product;
        }
    }

    // A deep link carries an explicit playback time WITHOUT `rt=true`: honor it
    // as a detached archive view (the user shared "this moment"), and do NOT
    // auto-tether. With `rt=true`, the boot-live path below re-tethers and the
    // restored time is irrelevant (the playhead snaps to now).
    let explicit_deep_link = url_params.time.is_some() && url_params.view.rt != Some(true);
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

    // Session start: open tethered to live (spec §7 DECIDED, alignment §5),
    // unless the user followed an explicit detached deep link. With a site
    // already known (URL or preferred), tether now. On a true first visit (no
    // site, modal open), defer: tether once the user picks a site.
    if !explicit_deep_link {
        let site_known = url_params.site.is_some() || state.preferred_site.is_some();
        if site_known {
            // Queued behind the initial RefreshTimeline so the timeline
            // populates first (same as the legacy `rt=true` restore).
            state.push_command(crate::core::Intent::StartLive);
        } else {
            state.start_live_on_site_select = true;
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
        let data_facade = MainThreadStore::new();
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

        let initial_prefs = core::UserPreferences::from_app_state(
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
            persistence: app::persistence_manager::PersistenceManager::new(
                initial_site_id,
                initial_prefs,
            ),
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
        app.state.cross_origin_isolated = subsystem::network_monitor::is_cross_origin_isolated();
        if !app.state.cross_origin_isolated {
            log::warn!("Not cross-origin isolated: SharedArrayBuffer unavailable");
        }

        // Attach the service-worker network monitor only in dev mode. When
        // toggled on later, `update_network_stats` will lazily attach it.
        if app.state.dev_mode {
            app.diagnostics.network_monitor = subsystem::NetworkMonitor::new();
        }

        app
    }

    /// Feed the rolling throughput window from the cumulative byte counter.
    ///
    /// This is the always-available fallback source: it diffs
    /// `NetworkStats::bytes_transferred()` frame to frame, so it needs no
    /// service worker and no changes to the async download path. It is
    /// frame-paced, so it contributes at most one sample per frame.
    ///
    /// Pruning runs unconditionally — an idle window must decay back to "no
    /// rate" rather than freezing on the last value it saw.
    fn sample_throughput(&mut self) {
        let now_ms = self.state.frame_now.millis();
        let stats = &mut self.state.session_stats;
        let total = stats.session_transferred_bytes;
        if let Some(sample) = core::throughput_delta_sample(stats.last_total_bytes, total, now_ms) {
            stats.throughput.push(sample, now_ms);
        }
        stats.last_total_bytes = total;
        stats.throughput.prune(now_ms);
    }

    /// Start live mode streaming for the current site.
    fn update_network_stats(&mut self) {
        // Update session stats from live network statistics
        let network_stats = self.acquisition.coordinator.download_channel.stats();
        self.state
            .session_stats
            .update_from_network_stats(&network_stats);
        self.sample_throughput();

        // Service worker metrics are only collected in dev mode. Lazily
        // attach the listener the first time dev mode becomes active.
        if !self.state.dev_mode {
            return;
        }
        if self.diagnostics.network_monitor.is_none() {
            self.diagnostics.network_monitor = subsystem::NetworkMonitor::new();
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
    fn persist_url_state(&mut self, ctx: &egui::Context) {
        // Encode `rt=` (reload re-enters live) only while the playhead is
        // attached to the live edge — a detached background stream's "current
        // view" is the scrubbed archive position, which the URL captures.
        //
        // The decision (throttle gate + prefs change-detection) is pure and
        // clock-injected: we pass this frame's wall clock and execute the
        // returned effects through the shell's effect runtime.
        let effects = self.persistence.persist_if_due(
            self.state.frame_now.secs(),
            &self.state,
            &self.playback.state,
            self.diagnostics.mping.api_key.clone(),
            self.live.app_mode == state::AppMode::Live,
        );
        self.apply_effects(ctx, effects);
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
        let c = ui::colors::mode::color(mode);
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
        let early_derived = subsystem::Derived::for_frame(
            &self.state,
            &self.playback,
            self.live.mode_state.is_active(),
        );
        self.state.national_mosaic.poll_tick(
            ctx,
            self.state.layer_state.geo.national_mosaic && early_derived.data_is_live,
        );
        // NWS alerts and mPING storm reports — polled if due.
        let diagnostics_inputs = subsystem::diagnostics::DiagnosticsInputs {
            is_live: early_derived.data_is_live,
            mping_layer_visible: self.state.layer_state.geo.mping,
            mping_pinned_to_now: self.playback.state.time_model.is_pinned()
                || self.playback.state.time_model.is_lookback(),
            site_id: &self.state.viz_state.site_id,
            playback_secs: self.playback.state.playback_position(),
        };
        self.diagnostics
            .tick(ctx, diagnostics_inputs, &mut self.state.errors);

        // 9-13. COMPUTE: drive the live playhead, advance playback, sync GPU
        // state, decide whether to issue the next render, then capture network
        // stats and persist.
        //
        // `tick_live` pins the playhead to "now" (LIVE-NOW) or slides the
        // lookback window (LIVE-LOOKBACK) independent of `playing`, before the
        // render decision below reads the position. While pinned to now, the
        // live edge moves continuously, so request a repaint to keep it smooth
        // even when the user isn't interacting.
        self.tick_live();
        if self.live.mode_state.is_active() {
            // Pinned: the live edge moves every frame — repaint fast. Detached
            // (background stream while browsing): the now-line and in-progress
            // overlay still advance, just at a relaxed cadence.
            let cadence_ms = if self.playback.state.time_model.is_pinned() {
                100
            } else {
                1000
            };
            ctx.request_repaint_after(std::time::Duration::from_millis(cadence_ms));
        }
        // Reconcile the timeline tier against this frame's strip width (which
        // may have changed under responsive layout) before advance_playback
        // reads the playback mode. Hysteresis-aware and idempotent when
        // nothing moved. Width comes from last frame's measured strip; the
        // Archive span boundary depends on it.
        {
            let width = self.playback.state.timeline_width_px;
            let spacing = self.playback.state.median_frame_spacing();
            self.playback.state.reconcile_tier(width, spacing);
        }
        self.advance_playback();
        // 9.5. REACTIVE ACQUISITION: now that advance_playback has settled the
        // playback position, prefetch the archive scans that position (and a
        // bounded lookahead) needs — debounced so scrub/zoom transients don't
        // fetch. Enqueues into the shared download queue; the next frame's
        // pump_download_queue (step 4) dispatches it.
        self.pump_implicit_prefetch(ctx);
        // The live counterpart: while replaying a lookback, backfill the recent
        // archive volumes the loop needs (pump_implicit_prefetch is off during
        // live and only looks forward).
        self.pump_lookback_backfill(ctx);
        // Listing counterpart: keep archive listings (→ timeline shadows)
        // populated for whatever date range the user is looking at, so the
        // timeline itself is the browsing surface.
        self.pump_visible_listings(ctx);
        // Selection = the fetch: resolve any range the user just selected
        // (arm the bulk fetch directly, or open the confirm modal if the span
        // is large), then pump the armed target's scans into the queue.
        self.resolve_selection_fetch_gate();
        self.pump_selection_fetch(ctx);
        self.sync_prev_sweep_texture();
        self.request_render_if_needed();
        self.update_network_stats();
        self.persist_url_state(ctx);

        // 14-17. FRAME SNAPSHOT: materialize the per-frame state UI reads.
        // Live::refresh derives everything from this frame's shared `now`.
        self.live.refresh(subsystem::live::LiveRefreshInputs {
            radar_timeline: &self.timeline.scans,
            playback: &self.playback.state,
            archive_boundaries: &self.timeline.shadow_scan_boundaries,
            now: self.state.frame_now,
        });
        self.state.refresh_mobile_mode(ctx);

        // Drain GPS-overlay async results before panels render so the
        // "My Location" checkbox sees coords/error on the same frame the
        // geolocation callback fires. Each result is applied through the pure
        // diagnostics reducer (same path as user intents) rather than mutated
        // inline — so the success/auto-off-on-failure rules stay testable.
        for r in self.diagnostics.gps.drain_results() {
            let intent = match r {
                core::LocationResult::Success(lat, lon) => {
                    core::diagnostics::DiagnosticsIntent::GpsResolved(lat, lon)
                }
                core::LocationResult::Error(msg) => {
                    core::diagnostics::DiagnosticsIntent::GpsFailed(msg)
                }
            };
            self.handle_diagnostics_intent(ctx, intent);
        }

        // 18. Recolor the favicon if the AppMode changed this frame.
        self.sync_favicon_to_mode();

        // Per-frame chrome animations that should tick in both layouts.
        // Hoisted out of `BottomPanelLayer` so the mobile path doesn't
        // have to call it as a no-op side-effect carrier.
        let dt = ctx.input(|i| i.stable_dt);
        self.live.mode_state.update_pulse(dt);

        // Re-materialise the per-frame Derived snapshot here so panel
        // renders see live-mode pulse + the freshest playback position.
        // (An earlier copy was already taken before the diagnostics tick
        // so `data_is_live` flows into that consumer.)
        let derived = subsystem::Derived::for_frame(
            &self.state,
            &self.playback,
            self.live.mode_state.is_active(),
        );

        // 19. Consume any deferred geolocation request raised by the
        // mobile action bar. Handled before layout dispatch so the modal
        // opens pending in the same frame the button was pressed.
        if self.state.is_mobile && self.chrome.mobile_geolocate_requested {
            self.chrome.mobile_geolocate_requested = false;
            self.begin_site_geolocation(ctx);
        }

        // 19b. Resolve mobile chrome auto-hide for this frame (spec §13 phone:
        // "Canvas full-bleed; chrome auto-hides during playback, tap to
        // reveal"). Done once, before layout, so the top bar, bottom chrome,
        // and the canvas all read the same resolved `hidden` flag. A press
        // while the chrome was hidden last frame is a reveal tap (only the
        // canvas is on screen then): it bumps the idle timer and latches
        // `revealed_this_frame` so the canvas swallows that tap instead of
        // panning. Inert on desktop.
        if self.state.is_mobile {
            ui::resolve_mobile_auto_hide(
                ctx,
                &mut self.playback,
                &mut self.chrome,
                &self.diagnostics,
                self.modals.datetime.open,
            );
        } else {
            self.chrome.mobile_auto_hide.hidden = false;
            self.chrome.mobile_auto_hide.revealed_this_frame = false;
        }

        // 20. RENDER (layout tree): chrome panels + all modals dispatched
        // through the declarative `Layer` registry. The desktop and
        // mobile layouts pick the chrome panel set; the modal set is
        // shared. Visibility predicates absorb per-panel and per-modal
        // visibility guards that previously lived in each function body.
        // Diagnostics view-model: the read-only projection (severity-sorted
        // alerts in view) the chip + list modal render, built once from this
        // frame's bounds so neither recomputes `visible_in`.
        let diagnostics_vm = core::diagnostics::DiagnosticsVm::build(
            &self.diagnostics.alerts,
            derived.visible_bounds,
        );

        let is_mobile = self.state.is_mobile;
        let mut layout_ctx = ui::LayoutCtx {
            ctx,
            state: &mut self.state,
            timeline: &self.timeline,
            live: &mut self.live,
            playback: &mut self.playback,
            acquisition: &mut self.acquisition,
            chrome: &mut self.chrome,
            diagnostics: &self.diagnostics,
            derived: &derived,
            diagnostics_vm: &diagnostics_vm,
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
