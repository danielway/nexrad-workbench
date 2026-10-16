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
/// The one recency rule ([`core::recency::RECENCY_SECS`]).
const MAX_SCAN_AGE_SECS: f64 = core::recency::RECENCY_SECS;

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
const LIVE_DEFAULT_ZOOM: f64 = crate::core::timeline_interaction::LIVE_ZOOM;

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

    /// Last live-now flag pushed to the favicon. `None` until the first frame
    /// so the initial state is always sent. See `sync_favicon`.
    last_favicon_live: Option<bool>,
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

/// Open the session from the link (one-instrument §14): apply what the link
/// says is on screen to the freshly bootstrapped state + playback + chrome,
/// following the pure [`core::link::BootPlan`]. Extracted from
/// `WorkbenchApp::new` to keep the constructor focused on wiring up
/// subsystems rather than restoring view state.
fn apply_link(
    link: &core::link::LinkState,
    prefs: &core::Preferences,
    state: &mut AppState,
    playback: &mut subsystem::Playback,
    chrome: &mut subsystem::Chrome,
) {
    use core::link::{BootStart, LinkTilt};

    let plan = core::link::decide_boot(link, state.preferred_site.as_deref(), |id| {
        data::get_site(id).is_some()
    });

    // The link's developer flag wins for this session without being saved.
    if let Some(dev) = link.dev {
        state.dev_mode = dev;
    }

    match plan.site.as_deref().and_then(data::get_site) {
        Some(site) => {
            state.viz_state.site_id = site.id.to_string();
            state.viz_state.center_lat = site.lat;
            state.viz_state.center_lon = site.lon;
            if state.recent_sites.is_empty() {
                state.recent_sites = vec![site.id.to_string()];
            }
            if plan.site_from_link {
                state.push_command(crate::core::Intent::RefreshTimeline {
                    auto_position: false,
                });
            }
        }
        // True first visit: the site picker opens and cannot be dismissed
        // until a site is chosen (any pick clears `site_required`); the
        // stream starts once one is.
        None => {
            chrome.site_modal_open = true;
            chrome.site_required = true;
        }
    }
    if let Some(lat) = link.lat {
        state.viz_state.center_lat = lat;
    }
    if let Some(lon) = link.lon {
        state.viz_state.center_lon = lon;
    }
    state
        .viz_state
        .camera
        .center_on(state.viz_state.center_lat, state.viz_state.center_lon);

    let pinned = plan.start.is_pinned();

    // Apply view state (zoom levels) before centering so the zoom is correct
    if let Some(mz) = link.view.mz {
        state.viz_state.set_zoom(mz);
    }
    {
        // Boot zoom: the live zoom when the session boots pinned to now (so
        // going live never jumps the strip), else the link's or the macro
        // default. Clamp into the *width-aware* range so an old link with an
        // absurdly small (year-wide) zoom lands at the widest readable
        // calendar span instead of decades out (spec §6.4 DECIDED). Width isn't
        // measured yet at boot, so use the seeded `timeline_width_px`; the
        // per-frame reconcile corrects the tier once the real width is known.
        let zoom = crate::core::timeline_interaction::boot_zoom(link.view.tz, pinned);
        let min = crate::core::PlaybackState::min_zoom_for_width(playback.state.timeline_width_px);
        playback.state.timeline_zoom = zoom.clamp(min, crate::core::TIMELINE_ZOOM_MAX);
        // Seed the tier deterministically from the restored zoom+width (no
        // hysteresis memory at boot). The per-frame reconcile corrects it once
        // the real strip width is measured.
        playback.state.seed_tier_from_state();
        if pinned {
            let now = playback.state.playback_position();
            playback.state.center_view_on(now);
        }
    }

    // Restore 3D view mode and camera parameters from the link. The camera is
    // the single source of truth for the view mode, so a globe link
    // reconstructs the orbit camera from the saved snapshot; a 2D link leaves
    // the camera in its (already-centered) Flat2D state.
    let v = &link.view;
    if v.vm.is_some_and(|vm| vm != 0) {
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

    if let Some(product) = link
        .product
        .as_deref()
        .and_then(crate::core::RadarProduct::from_short_code)
    {
        state.viz_state.product = product;
    }
    match link.tilt {
        Some(LinkTilt::Latest) => {
            state.viz_state.elevation_selection = crate::core::ElevationSelection::Latest;
        }
        // Re-resolved to the volume's elevation number when its VCP arrives.
        Some(LinkTilt::Angle(angle)) => {
            state.viz_state.elevation_selection = crate::core::ElevationSelection::Fixed {
                elevation_number: 1,
                angle,
            };
        }
        None => {}
    }
    // The link says which mode plays; the preferences say how fast.
    if let Some(mode) = link.play_mode {
        playback.state.speed = prefs.speed_for(mode);
        playback.state.restore_play_mode(mode);
    }

    match plan.start {
        BootStart::At(t) => {
            playback.state.set_playback_position(t);
            playback.state.center_view_on(t);
        }
        BootStart::Range {
            start,
            end,
            playhead,
        } => {
            playback.state.set_playback_position(playhead);
            playback.state.select_range(start, end, false);
            playback.state.center_view_on((start + end) / 2.0);
        }
        // Session start pinned to now (spec §7 DECIDED). With a site known,
        // tether now — queued behind the initial RefreshTimeline so the
        // timeline populates first. On a first visit, tether once the user
        // picks a site.
        BootStart::PinnedNow | BootStart::PinnedRange(_) if plan.site.is_none() => {
            state.start_live_on_site_select = true;
        }
        BootStart::PinnedNow => state.push_command(crate::core::Intent::StartLive),
        BootStart::PinnedRange(preset) => {
            state.push_command(crate::core::Intent::ApplyLoopPreset(preset));
        }
    }
}

impl WorkbenchApp {
    /// Creates a new WorkbenchApp instance.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Fonts (IBM Plex + Phosphor icons) and the five text sizes.
        ui::theme::install(&cc.egui_ctx);

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
            prefs: loaded_prefs,
        } = AppState::bootstrap();
        let mut playback = subsystem::Playback {
            state: bootstrapped_playback,
        };
        let mut chrome = subsystem::Chrome::new();
        chrome.rail_tab = loaded_prefs.panel_tab;
        chrome.right_sidebar_visible = loaded_prefs.panel_open;

        let link = state::url_state::read_link();
        apply_link(&link, &loaded_prefs, &mut state, &mut playback, &mut chrome);

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

        // Track what is on disk, so anything the boot changed (a first
        // recent site, say) is saved at the first persist.
        let initial_prefs = loaded_prefs.clone();

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
            modals: ui::ModalStates::new(),
            diagnostics: {
                let mut diag = subsystem::Diagnostics::new();
                // Apply the persisted mPING API key (loaded from prefs at
                // AppState construction; mPING state lives on the
                // subsystem so it can't be applied inside AppState::new).
                diag.mping.api_key = loaded_mping_api_key;
                diag
            },
            last_favicon_live: None,
        };

        // Check cross-origin isolation status on startup
        app.state.cross_origin_isolated = subsystem::network_monitor::is_cross_origin_isolated();
        if !app.state.cross_origin_isolated {
            log::warn!("Not cross-origin isolated: SharedArrayBuffer unavailable");
        }

        // Attach the service-worker network monitor for every user: it is the
        // high-fidelity source behind the activity surface's throughput and
        // recent-request readouts, not a dev-only diagnostic. `new()` still
        // returns `None` when there is no service-worker container to listen
        // on, which is a real runtime condition, not a mode.
        app.diagnostics.network_monitor = subsystem::NetworkMonitor::new();

        app
    }

    /// Feed the rolling throughput window from the cumulative byte counter.
    ///
    /// This is the fallback source, used when the service worker produced no
    /// metrics this frame (no SW registered yet, or an unsupported browser).
    /// It diffs `NetworkStats::bytes_transferred()` frame to frame, so it needs
    /// no service worker and no changes to the async download path, but it is
    /// frame-paced and carries no per-request detail.
    ///
    /// `already_sampled` suppresses the delta so the same bytes aren't counted
    /// twice when both sources are live. The counter is still advanced, so the
    /// fallback stays correctly rebased for any later frame that needs it.
    ///
    /// Pruning runs unconditionally — an idle window must decay back to "no
    /// rate" rather than freezing on the last value it saw.
    fn sample_throughput(&mut self, already_sampled: bool) {
        let now_ms = self.state.frame_now.millis();
        let stats = &mut self.state.session_stats;
        let total = stats.session_transferred_bytes;
        if !already_sampled {
            if let Some(sample) =
                core::throughput_delta_sample(stats.last_total_bytes, total, now_ms)
            {
                stats.throughput.push(sample, now_ms);
            }
        }
        stats.last_total_bytes = total;
        stats.throughput.prune(now_ms);
    }

    /// Assemble this frame's activity view-model.
    ///
    /// Pure gathering: every decision (which counter is authoritative for
    /// which stage, how stages are deduplicated, when a worker load is stale)
    /// lives in [`core::activity`] and is tested headlessly there.
    fn build_activity_vm(&self) -> core::activity::ActivityVm {
        let now_ms = self.state.frame_now.millis();
        let stats = &self.state.session_stats;
        core::activity::ActivityVm::build(core::activity::ActivityInputs {
            now_ms,
            operations: &self.acquisition.state.operations,
            queue_paused: self.acquisition.state.is_paused(),
            ledger: self.acquisition.request_ledger.summarize(now_ms),
            awaiting_scan_starts: &self.acquisition.request_ledger.awaiting_scan_starts(now_ms),
            worker: stats.worker_load,
            last_worker_outcome_ms: stats.last_worker_outcome_ms,
            gpu_render_in_flight: stats.pipeline.rendering,
            throughput: &stats.throughput,
            http_in_flight: stats.active_request_count,
            session_requests: stats.session_request_count,
            session_bytes: stats.session_transferred_bytes,
            sw_aggregate: &self.state.network_aggregate,
            recent_requests: &self.state.recent_network_requests,
            cache_size_bytes: stats.cache_size_bytes,
            stream: self.live.session.stream,
            sheet_open: self.chrome.activity_sheet_open,
            dev: self
                .state
                .dev_mode
                .then_some(core::activity::ActivityDevInputs {
                    fps: stats.avg_fps,
                    cross_origin_isolated: self.state.cross_origin_isolated,
                    avg_fetch_ms: stats.median_chunk_latency_ms,
                    avg_processing_ms: stats.median_processing_time_ms,
                    avg_render_ms: stats.avg_render_time_ms,
                    vcp_forecast_available: self.live.mode_state.volume_start_plan.is_some()
                        || self.live.mode_state.last_completed_volume.is_some(),
                    ingest_phases: stats.last_ingest_detail.as_ref().map(|d| {
                        [
                            ("Split", d.split_ms),
                            ("Decompress", d.decompress_ms),
                            ("Decode", d.decode_ms),
                            ("Extract", d.extract_ms),
                            ("Store (IDB)", d.store_ms),
                            ("Index", d.index_ms),
                        ]
                    }),
                    render_phases: stats.last_render_detail.as_ref().map(|d| {
                        [
                            ("IDB fetch", d.fetch_ms),
                            ("Deserialize", d.deser_ms),
                            ("Marshal", d.marshal_ms),
                            ("GPU upload", d.gpu_upload_ms),
                        ]
                    }),
                    chunk_latency: self
                        .acquisition
                        .state
                        .latency_summary()
                        .map(|l| (l.avg_fetch_ms, l.p95_fetch_ms, l.avg_e2e_ms)),
                }),
        })
    }

    /// Assemble this frame's top-bar view-model. Pure gathering: the words,
    /// rows and alert-chip state are decided in [`core::top_bar`].
    fn build_top_bar_vm(
        &self,
        diagnostics_vm: &core::diagnostics::DiagnosticsVm,
        derived: &subsystem::Derived,
    ) -> core::top_bar::TopBarVm {
        let now = self.state.frame_now.secs();
        let use_local = self.state.use_local_time;
        let elevations = self.state.current_elevation_list(
            &self.playback.state,
            &self.timeline.scans,
            self.live
                .radar_model
                .volume
                .as_ref()
                .and_then(|v| v.vcp_pattern.as_ref()),
        );
        let zone = ui::time_format::zone_label(now, use_local);
        core::top_bar::TopBarVm::build(
            core::top_bar::TopBarInputs {
                session: &self.live.session,
                now_secs: now,
                site_id: &self.state.viz_state.site_id,
                product: self.state.viz_state.product,
                elevation_selection: &self.state.viz_state.elevation_selection,
                elevations: &elevations,
                zone: &zone,
                use_local_time: use_local,
                view_mode: self.state.viz_state.view_mode(),
                alerts: &self.diagnostics.alerts,
                visible_alerts: &diagnostics_vm.visible_alerts,
                map_bounds_known: derived.visible_bounds.is_some(),
                worker_init_error: self.state.worker_init_error.as_deref(),
                version: env!("NEXRAD_VERSION_FULL"),
            },
            &ui::time_format::ZoneClock { use_local },
        )
    }

    /// Assemble this frame's tilt ladder. Pure gathering: which volume and
    /// clock, and every row, bar, move and word, are decided in
    /// [`core::ladder`].
    fn build_ladder_vm(&self) -> core::ladder::VolumeLadder {
        let now = self.state.frame_now.secs();
        let playhead = self.playback.state.playback_position();
        let live_vcp = self
            .live
            .radar_model
            .volume
            .as_ref()
            .and_then(|v| v.vcp_pattern.as_ref());
        let entries =
            self.state
                .current_elevation_list(&self.playback.state, &self.timeline.scans, live_vcp);
        let view = core::TimelineView::build(
            &self.timeline.scans,
            &self.timeline.shadow_scan_boundaries,
            Some(&self.live.mode_state),
            self.live.radar_model.position.as_ref(),
        );
        let pinned = self.live.session.playhead == core::session::Playhead::PinnedToNow;
        let (volume, clock) = core::ladder::ladder_volume(&view, pinned, now, playhead);
        let viz = &self.state.viz_state;
        core::ladder::VolumeLadder::build(
            core::ladder::LadderInputs {
                entries: &entries,
                selection: &viz.elevation_selection,
                product: viz.product,
                volume: volume.as_ref(),
                clock,
                antenna_azimuth: self.live.radar_model.estimated_azimuth,
                on_screen: viz.displayed.as_ref().map(|d| core::ladder::OnScreenCut {
                    elevation_number: d.identity.elevation_number,
                    start_secs: d.start_time,
                }),
            },
            &ui::time_format::ZoneClock {
                use_local: self.state.use_local_time,
            },
        )
    }

    /// Refresh every per-frame acquisition telemetry input.
    ///
    /// All of it feeds the activity surface, so none of it is dev-gated. The
    /// one thing that stays conditional is URL→operation correlation, which is
    /// O(operations) per request and only affects grouped display inside a
    /// collapsed disclosure — see below.
    fn capture_activity_telemetry(&mut self) {
        // Update session stats from live network statistics
        let network_stats = self.acquisition.coordinator.download_channel.stats();
        self.state
            .session_stats
            .update_from_network_stats(&network_stats);
        self.state.session_stats.worker_load = self.render.coordinator.worker_load();

        // Drain service-worker metrics. These carry real per-request byte
        // counts and timestamps, so they are the preferred throughput source;
        // `sample_throughput` falls back to the cumulative counter only when
        // this yields nothing.
        let mut sampled_from_monitor = false;
        if let Some(ref monitor) = self.diagnostics.network_monitor {
            self.state.network_aggregate = monitor.aggregate();
            let mut pending = monitor.take_pending();
            if !pending.is_empty() {
                let now_ms = self.state.frame_now.millis();
                // Correlation is only read by the grouped network list behind
                // the activity sheet's Details disclosure. It is a linear scan
                // of the (up to 200) retained operations per request, so we
                // skip it entirely while nothing is looking.
                let correlate = self.chrome.activity_sheet_open;
                for req in pending.iter_mut() {
                    req.operation_id = if correlate {
                        self.acquisition.state.correlate_network_request(&req.url)
                    } else {
                        None
                    };
                    self.state.session_stats.throughput.push(
                        core::ThroughputSample {
                            at_ms: req.timestamp_ms,
                            bytes: req.bytes,
                        },
                        now_ms,
                    );
                }
                sampled_from_monitor = true;
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

        self.sample_throughput(sampled_from_monitor);
    }

    /// Push current app state to the URL bar and save user preferences (throttled).
    fn persist_url_state(&mut self, ctx: &egui::Context) {
        // The link carries what is on screen (core::link); a pinned selection
        // is written as `sel=`, so a reload re-enters live only when the
        // playhead was attached.
        //
        // The decision (throttle gate + prefs change-detection) is pure and
        // clock-injected: we pass this frame's wall clock and execute the
        // returned effects through the shell's effect runtime.
        let effects = self.persistence.persist_if_due(
            self.state.frame_now.secs(),
            &self.state,
            &self.playback.state,
            state::PanelPrefs {
                tab: self.chrome.rail_tab,
                open: self.chrome.right_sidebar_visible,
            },
            self.diagnostics.mping.api_key.clone(),
        );
        self.apply_effects(ctx, effects);
    }

    /// Push the live-now color to the browser favicon via the
    /// `setFaviconColor` JS hook in `index.html`. No-op when it hasn't
    /// changed since the last push.
    fn sync_favicon(&mut self) {
        use wasm_bindgen::prelude::*;
        #[wasm_bindgen]
        extern "C" {
            #[wasm_bindgen(js_namespace = window, js_name = setFaviconColor, catch)]
            fn js_set_favicon_color(hex: &str) -> Result<(), JsValue>;
        }

        let live_now = self.live.session.live_now;
        if self.last_favicon_live == Some(live_now) {
            return;
        }
        let c = ui::theme::favicon_color(live_now);
        let hex = format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
        let _ = js_set_favicon_color(&hex);

        // Prefix the document title so a backgrounded tab says it is live.
        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
            let prefix = if live_now { "[LIVE] " } else { "" };
            document.set_title(&format!("{}NEXRAD Workbench", prefix));
        }

        self.last_favicon_live = Some(live_now);
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx_owned = ui.ctx().clone();
        let ctx = &ctx_owned;
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
            self.live.radar_model.collecting_elevation(),
        );
        self.state.national_mosaic.poll_tick(
            ctx,
            self.state.layer_state.geo.national_mosaic && early_derived.data_is_live,
        );
        // NWS alerts and mPING storm reports — polled if due.
        let diagnostics_inputs = subsystem::diagnostics::DiagnosticsInputs {
            is_live: early_derived.data_is_live,
            mping_layer_visible: self.state.layer_state.geo.mping,
            mping_pinned_to_now: self.playback.state.is_pinned()
                || self.playback.state.is_live_range(),
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
            let cadence_ms = if self.playback.state.is_pinned() {
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
            self.playback.state.reconcile_tier(width);
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
        // Storm activity for the zoomed-out overview's bars: one listing
        // per day it shows, stored so each day is listed once.
        self.pump_storm_activity(ctx);
        // Selection = the fetch: resolve any range the user just selected
        // (arm the bulk fetch directly, or open the confirm modal if the span
        // is large), then pump the armed target's scans into the queue.
        self.resolve_selection_fetch_gate();
        self.pump_selection_fetch(ctx);
        self.sync_prev_sweep_texture();
        self.request_render_if_needed();
        self.capture_activity_telemetry();
        self.persist_url_state(ctx);

        // 14-17. FRAME SNAPSHOT: materialize the per-frame state UI reads.
        // Live::refresh derives everything from this frame's shared `now`.
        self.live.refresh(subsystem::live::LiveRefreshInputs {
            playback: &self.playback.state,
            archive_boundaries: &self.timeline.shadow_scan_boundaries,
            now: self.state.frame_now,
            elevation_selection: &self.state.viz_state.elevation_selection,
            displayed: self.state.viz_state.displayed.as_ref(),
            caption_context: self.state.viz_state.caption_context,
        });
        self.state.refresh_mobile_mode(ctx);

        // Drain GPS-overlay async results before panels render so the
        // "My Location" checkbox sees coords/error on the same frame the
        // geolocation callback fires. Each result is applied through the pure
        // diagnostics reducer (same path as user intents) rather than mutated
        // inline — so the success/auto-off-on-failure rules stay testable.
        for r in self.diagnostics.gps.drain_results() {
            let intent = match r {
                core::LocationResult::Success { lat, lon, .. } => {
                    core::diagnostics::DiagnosticsIntent::GpsResolved(lat, lon)
                }
                core::LocationResult::Error(msg) => {
                    core::diagnostics::DiagnosticsIntent::GpsFailed(msg)
                }
            };
            self.handle_diagnostics_intent(ctx, intent);
        }

        // 18. Recolor the favicon if live-now changed this frame.
        self.sync_favicon();

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
            self.live.radar_model.collecting_elevation(),
        );

        // 19. Consume any deferred geolocation request raised by the
        // mobile action bar. Handled before layout dispatch so the modal
        // opens pending in the same frame the button was pressed.
        if self.state.is_mobile && self.chrome.mobile_geolocate_requested {
            self.chrome.mobile_geolocate_requested = false;
            self.begin_site_geolocation(ctx, true);
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

        // Activity view-model: the one reconciled answer to "what is the app
        // doing?", built once from this frame's inputs so the chip, the sheet
        // and the mobile bar all read the same numbers.
        let activity_vm = self.build_activity_vm();

        // Toasts: move this frame's legacy status line and fresh errors into
        // the queue, then advance it on the frame clock (core::toast).
        self.state
            .toasts
            .absorb_status(&mut self.state.status_message);
        self.state.toasts.absorb_errors(&self.state.errors);
        self.state.toasts.tick(self.state.frame_now.millis());

        // Top-bar view-model: what is on screen, in the bar's words.
        let top_bar_vm = self.build_top_bar_vm(&diagnostics_vm, &derived);
        let ladder_vm = self.build_ladder_vm();

        // Dialog stack (one-instrument §12): bring the open-dialog order in
        // step with this frame's flags before anything renders, so the
        // layers, the shortcut gate and Escape all read the same stack.
        app::dialogs::sync_dialogs(&mut self.chrome, &self.diagnostics);

        let is_mobile = self.state.is_mobile;
        let mut layout_ctx = ui::LayoutCtx {
            ctx,
            root_ui: ui,
            state: &mut self.state,
            timeline: &self.timeline,
            live: &mut self.live,
            playback: &mut self.playback,
            acquisition: &mut self.acquisition,
            chrome: &mut self.chrome,
            diagnostics: &self.diagnostics,
            derived: &derived,
            diagnostics_vm: &diagnostics_vm,
            activity_vm: &activity_vm,
            top_bar_vm: &top_bar_vm,
            ladder_vm: &ladder_vm,
            modals: &mut self.modals,
        };
        ui::render_layout(is_mobile, &mut layout_ctx);

        // Restack the dialog windows when the stack changed, then route an
        // Escape no widget claimed (the time field claims it while editing)
        // to the top dialog.
        if self.chrome.dialogs.take_changed() {
            ui::dialog::restack(ctx, &self.chrome.dialogs);
        }
        if !self.chrome.dialogs.is_empty()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.state
                .push_command(core::Intent::Dialog(core::dialogs::DialogIntent::Escape));
            ctx.request_repaint();
        }

        // 21. RENDER (canvas): GPU-based radar rendering in the CentralPanel.
        ui::render_canvas_with_geo(
            ui,
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
