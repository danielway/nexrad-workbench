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

mod data;
mod geo;
mod nexrad;
mod state;
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

use nexrad::download_queue::{QueueAction, QueueItem};
use nexrad::RenderRequest;
use state::playback_manager::{sweep_cache_key, CachedSweepData, PlaybackManager, PrevSweepAction};
use state::MAX_RECENT_NETWORK_REQUESTS;

/// Main application state and logic.
pub struct WorkbenchApp {
    /// Application state containing all sub-states
    state: AppState,

    /// Geographic layer data for map overlays
    geo_layers: geo::GeoLayerSet,

    /// All GPU renderers and their GL context.
    gpu: GpuResources,

    /// Render coordinator: owns the decode worker, scan key, elevations, and render dedup.
    render: nexrad::RenderCoordinator,

    /// Download pipeline: channels, queue, archive index, current scan.
    acquisition: nexrad::AcquisitionCoordinator,

    /// Live streaming and backfill lifecycle manager.
    streaming: nexrad::StreamingManager,

    /// URL state, preferences, and site change detection.
    persistence: nexrad::PersistenceManager,

    /// Transient state for the site selection modal.
    site_modal_state: ui::SiteModalState,

    /// Transient state for the event create/edit modal.
    event_modal_state: ui::EventModalState,

    /// Service worker network monitor (None if SW not available).
    network_monitor: Option<nexrad::NetworkMonitor>,

    /// Sweep cache and previous-sweep resolution for sweep animation.
    playback_manager: PlaybackManager,

    /// Cache of the inputs that drive `advance_playback`'s scrub-detection
    /// pass so we can skip the O(scans) timeline search on idle frames
    /// where the playback position, elevation selection, and scan count
    /// have not changed.
    scrub_cache: ScrubCache,
}

#[derive(Default)]
struct ScrubCache {
    last_playback_ts: Option<f64>,
    last_elevation_selection: Option<state::ElevationSelection>,
    last_scan_count: usize,
    last_displayed_scan_ts: Option<i64>,
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

        let mut state = AppState::new();

        // Apply URL parameters (site, time, lat/lon)
        let url_params = state::url_state::parse_from_url();
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
            state.playback_state.timeline_zoom = tz;
        }
        // Restore 3D view mode and camera parameters from URL
        {
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
            state.playback_state.set_playback_position(time);
            // Center view on the restored position. timeline_width_px may
            // still be the default 1000px since we haven't rendered yet, but
            // it will be accurate on subsequent centers.
            state.playback_state.center_view_on(time);
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
                state.site_modal_open = true;
            }
        }

        let initial_site_id = state.viz_state.site_id.clone();
        let data_facade = DataFacade::new();
        let acquisition = nexrad::AcquisitionCoordinator::new(data_facade.clone());
        let realtime_channel = nexrad::RealtimeChannel::with_stats(acquisition.download_stats());

        // Open the record cache database
        {
            let facade = data_facade.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = facade.open().await {
                    log::error!("Failed to open record cache: {}", e);
                }
            });
        }

        let initial_prefs = state::UserPreferences::from_app_state(&state);
        let has_preferred_site = state.preferred_site.is_some();

        // Create decode worker (offloads nexrad::load() to a Web Worker)
        let decode_worker = match nexrad::DecodeWorker::new(cc.egui_ctx.clone()) {
            Ok(w) => Some(w),
            Err(e) => {
                log::warn!("Failed to create decode worker: {}", e);
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
            render: nexrad::RenderCoordinator::new(decode_worker),
            acquisition,
            streaming: nexrad::StreamingManager::new(realtime_channel),
            persistence: nexrad::PersistenceManager::new(initial_site_id, initial_prefs),
            site_modal_state: {
                let mut sms = ui::SiteModalState::default();
                if has_preferred_site {
                    sms.is_first_visit = false;
                }
                sms
            },
            event_modal_state: ui::EventModalState::default(),
            network_monitor: nexrad::NetworkMonitor::new(),
            playback_manager: PlaybackManager::new(),
            scrub_cache: ScrubCache::default(),
        };

        // Check cross-origin isolation status on startup
        app.state.cross_origin_isolated = nexrad::is_cross_origin_isolated();
        if !app.state.cross_origin_isolated {
            log::warn!("Not cross-origin isolated: SharedArrayBuffer unavailable");
        }

        app
    }

    /// Process selection download: download scans in the selected time range serially.
    ///
    /// `download_type` is `None` when pumping the existing queue (no new command),
    /// `Some(true)` for a position-download, or `Some(false)` for a range-selection download.
    fn process_selection_download(&mut self, ctx: &egui::Context, download_type: Option<bool>) {
        let site_id = self.state.viz_state.site_id.clone();

        // If we have items in the queue, try to advance the state machine
        if self.acquisition.download_queue.has_work() {
            // If there is an Active item, check whether its download has finished
            if let Some(active) = self.acquisition.download_queue.active_item() {
                let still_pending = self
                    .acquisition
                    .download_channel
                    .is_download_pending(&site_id, active.scan_start);

                if still_pending {
                    // Still downloading, wait
                    return;
                }

                // Download finished — transition Active → Done
                let active_start = active.scan_start;
                self.acquisition
                    .download_queue
                    .mark_active_done(active_start);
            }

            // Advance the queue (handles pause check internally)
            let is_paused = self.state.acquisition.is_paused();
            match self.acquisition.download_queue.advance(is_paused) {
                QueueAction::StartDownload {
                    idx: _,
                    date,
                    file_name,
                    scan_start,
                    scan_end,
                    remaining,
                } => {
                    self.state.status_message =
                        format!("Downloading {} ({} remaining)", file_name, remaining);
                    self.state.download_progress.active_scan = Some((scan_start, scan_end));
                    self.state.download_progress.phase = crate::state::DownloadPhase::Downloading;
                    self.state.download_progress.batch_completed += 1;

                    // Mark next acquisition operation as active
                    if let Some(op_id) = self.state.acquisition.next_queued_id() {
                        self.state.acquisition.mark_active(op_id);
                        self.acquisition
                            .download_queue
                            .set_active_operation_id(Some(op_id));
                    }

                    self.acquisition.download_channel.download_file(
                        ctx.clone(),
                        site_id.clone(),
                        date,
                        file_name,
                        scan_start,
                        self.acquisition.facade().clone(),
                    );
                }
                QueueAction::Complete => {
                    self.state.download_selection_in_progress = false;
                    self.state.download_progress.pending_scans.clear();
                    self.state.download_progress.active_scan = None;
                    self.state.download_progress.phase = crate::state::DownloadPhase::Done;
                    // Full clear only if no in-flight scans remain.
                    if self.state.download_progress.in_flight_scans.is_empty() {
                        self.state.download_progress.clear();
                    }
                    self.state.status_message = "Selection download complete".to_string();
                    log::debug!("Selection download complete");
                }
                QueueAction::Paused | QueueAction::StillDownloading => {
                    return;
                }
            }
            return;
        }

        // No queue — check if a new download command was issued or a pending
        // download is being resumed after a listing arrived.
        let is_position_download = match download_type {
            Some(is_pos) => {
                // Fresh user action (not a pending resume) — reset pending state
                if self
                    .acquisition
                    .pending_download
                    .as_ref()
                    .is_none_or(|p| p.is_position != is_pos)
                {
                    self.acquisition.pending_download = None;
                }
                is_pos
            }
            None => return, // Just pumping queue, nothing to do
        };

        // Get the download range: either from selection or from current position.
        // For position download, we use a temporary wide window to determine which
        // date listings to fetch, then narrow to the exact scan below.
        let (sel_start, sel_end) = if is_position_download {
            let pos = self.state.playback_state.playback_position();
            (pos, pos)
        } else {
            match self.state.playback_state.selection_range() {
                Some(range) => range,
                None => {
                    log::warn!("Download selection requested but no valid selection");
                    return;
                }
            }
        };

        let sel_start_i64 = sel_start as i64;
        let sel_end_i64 = sel_end as i64;

        // Determine the date range for listing lookups
        let start_date = match chrono::DateTime::from_timestamp(sel_start_i64, 0) {
            Some(dt) => dt.date_naive(),
            None => return,
        };
        let end_date = match chrono::DateTime::from_timestamp(sel_end_i64, 0) {
            Some(dt) => dt.date_naive(),
            None => return,
        };

        log::debug!(
            "Building download queue for selection: {} to {} ({} to {})",
            sel_start_i64,
            sel_end_i64,
            start_date,
            end_date
        );

        // Collect all files whose scan boundaries intersect the selection
        let mut files_to_download: Vec<QueueItem> = Vec::new();
        let mut current_date = start_date;

        while current_date <= end_date {
            if let Some(listing) = self.acquisition.archive_index.get(&site_id, &current_date) {
                if is_position_download {
                    // Single-position: find the exact scan containing the playback position
                    if let Some((file, boundary)) = listing.find_scan_containing(sel_start_i64) {
                        let is_cached = self.state.radar_timeline.scans.iter().any(|s| {
                            (s.start_time as i64 - file.timestamp).abs()
                                < SCAN_CACHE_MATCH_TOLERANCE_SECS
                        });
                        if !is_cached {
                            files_to_download.push(QueueItem::new(
                                current_date,
                                file.name.clone(),
                                boundary.start,
                                boundary.end,
                            ));
                        }
                    } else {
                        // No scan covers this time in the cached listing.
                        // Check if we already re-fetched this date's listing.
                        let already_refetched = self
                            .acquisition
                            .pending_download
                            .as_ref()
                            .is_some_and(|p| p.refetched_dates.contains(&current_date));

                        if !already_refetched {
                            // The listing may be stale (e.g. archives created
                            // after it was cached), so invalidate and re-fetch
                            // once. Store intent so we resume when it arrives.
                            log::debug!(
                                "No scan at {} in cached listing for {}/{}; re-fetching",
                                sel_start_i64,
                                site_id,
                                current_date
                            );
                            let pending =
                                self.acquisition.pending_download.get_or_insert_with(|| {
                                    nexrad::acquisition_coordinator::PendingDownload {
                                        is_position: true,
                                        refetched_dates: std::collections::HashSet::new(),
                                    }
                                });
                            pending.refetched_dates.insert(current_date);
                            self.acquisition
                                .archive_index
                                .remove(&site_id, &current_date);
                            if !self
                                .acquisition
                                .download_channel
                                .is_listing_pending(&site_id, &current_date)
                            {
                                self.acquisition.download_channel.fetch_listing(
                                    ctx.clone(),
                                    site_id.clone(),
                                    current_date,
                                );
                            }
                            self.state.status_message =
                                format!("Re-fetching archive listing for {}...", current_date);
                            return;
                        }

                        // Already re-fetched — no scan here, skip.
                        log::debug!(
                            "No scan at {} in listing for {}/{} after re-fetch; skipping",
                            sel_start_i64,
                            site_id,
                            current_date
                        );
                    }
                } else {
                    // Range selection: find all scans that intersect [sel_start, sel_end]
                    for (file, boundary) in listing.scans_intersecting(sel_start_i64, sel_end_i64) {
                        let is_cached = self.state.radar_timeline.scans.iter().any(|s| {
                            (s.start_time as i64 - file.timestamp).abs()
                                < SCAN_CACHE_MATCH_TOLERANCE_SECS
                        });
                        if !is_cached {
                            files_to_download.push(QueueItem::new(
                                current_date,
                                file.name.clone(),
                                boundary.start,
                                boundary.end,
                            ));
                        }
                    }
                }
            } else {
                // Need to fetch the listing first. Store intent so we resume
                // when the listing arrives (via handle_listing_outcome).
                if !self
                    .acquisition
                    .download_channel
                    .is_listing_pending(&site_id, &current_date)
                {
                    log::debug!("Fetching listing for {}/{}", site_id, current_date);
                    self.acquisition.download_channel.fetch_listing(
                        ctx.clone(),
                        site_id.clone(),
                        current_date,
                    );
                }
                self.acquisition.pending_download.get_or_insert_with(|| {
                    nexrad::acquisition_coordinator::PendingDownload {
                        is_position: is_position_download,
                        refetched_dates: std::collections::HashSet::new(),
                    }
                });
                self.state.status_message =
                    format!("Fetching archive listing for {}...", current_date);
                return;
            }

            current_date += chrono::Duration::days(1);
        }

        // Queue building complete — clear pending state
        self.acquisition.pending_download = None;

        if files_to_download.is_empty() {
            self.state.status_message = "No new scans to download in selection".to_string();
            log::debug!("No new scans to download in selection");
            return;
        }

        // Sort by start timestamp
        files_to_download.sort_by_key(|item| item.scan_start);

        log::debug!(
            "Queued {} files for download in selection",
            files_to_download.len()
        );

        // Start downloading
        self.state.download_selection_in_progress = true;

        // Cancel any existing acquisition operations (selection change = cancel all + rebuild)
        self.state.acquisition.cancel_all();
        self.acquisition.download_queue.set_queue(files_to_download);

        // Create acquisition operations for each file in the queue
        for item in self.acquisition.download_queue.items() {
            self.state
                .acquisition
                .create_operation(state::OperationKind::ArchiveDownload {
                    site_id: site_id.clone(),
                    file_name: item.file_name.clone(),
                    scan_start: item.scan_start,
                    scan_end: item.scan_end,
                });
        }

        // Populate download progress for timeline ghosts and pipeline display
        {
            let progress = &mut self.state.download_progress;
            progress.pending_scans = self
                .acquisition
                .download_queue
                .items()
                .iter()
                .map(|item| (item.scan_start, item.scan_end))
                .collect();
            progress.batch_total = self.acquisition.download_queue.len() as u32;
            progress.batch_completed = 0;
            progress.phase = crate::state::DownloadPhase::Downloading;
            let first = &self.acquisition.download_queue.items()[0];
            progress.active_scan = Some((first.scan_start, first.scan_end));
        }

        // Kick off first download
        if let Some(QueueAction::StartDownload {
            idx: _,
            date,
            file_name,
            scan_start,
            scan_end: _,
            remaining,
        }) = self.acquisition.download_queue.start_first()
        {
            self.state.status_message = format!("Downloading {} ({} total)", file_name, remaining);

            // Mark the first acquisition operation as active
            if let Some(op_id) = self.state.acquisition.next_queued_id() {
                self.state.acquisition.mark_active(op_id);
                self.acquisition
                    .download_queue
                    .set_active_operation_id(Some(op_id));
            }

            self.acquisition.download_channel.download_file(
                ctx.clone(),
                site_id,
                date,
                file_name,
                scan_start,
                self.acquisition.facade().clone(),
            );
        }
    }

    /// Start live mode streaming for the current site.
    fn start_live_mode(&mut self, ctx: &egui::Context) {
        let site_id = self.state.viz_state.site_id.clone();
        log::info!("Starting live mode for site: {}", site_id);

        // Get current time
        let now = js_sys::Date::now() / 1000.0;

        // Initialize live mode state
        self.state.live_mode_state.start(now);
        self.state.playback_state.set_playback_position(now);
        self.state.playback_state.time_model.enable_realtime_lock();
        self.state.playback_state.playing = true;
        self.state.status_message = "Connecting to live stream...".to_string();

        self.streaming
            .start_live(ctx.clone(), site_id, self.acquisition.facade().clone());
    }

    /// Find the best elevation number for the current elevation selection.
    fn best_elevation_number(&self) -> u8 {
        match &self.state.viz_state.elevation_selection {
            crate::state::ElevationSelection::Fixed {
                elevation_number, ..
            } => *elevation_number,
            crate::state::ElevationSelection::Latest => {
                let playback_ts = self.state.playback_state.playback_position();
                if let Some(scan) = self
                    .state
                    .radar_timeline
                    .find_recent_scan(playback_ts, MAX_SCAN_AGE_SECS)
                {
                    return self.most_recent_sweep_elevation(scan, playback_ts);
                }
                self.render
                    .available_elevations()
                    .first()
                    .copied()
                    .unwrap_or(1)
            }
        }
    }

    /// Find the best elevation number for a scan given the playback position.
    /// Returns None when no sweep in the scan matches the user's fixed selection
    /// (so callers can clear display instead of issuing a doomed render).
    fn best_elevation_at_playback(
        &self,
        scan: &crate::state::radar_data::Scan,
        playback_ts: f64,
    ) -> Option<u8> {
        state::playback_manager::best_elevation_at_playback(
            &self.state.viz_state.elevation_selection,
            scan,
            playback_ts,
            self.render.available_elevations(),
        )
    }

    /// Find the most recent sweep (any elevation) at or before the playback position.
    fn most_recent_sweep_elevation(
        &self,
        scan: &crate::state::radar_data::Scan,
        playback_ts: f64,
    ) -> u8 {
        let fallback = self
            .render
            .available_elevations()
            .first()
            .copied()
            .unwrap_or(1);
        state::playback_manager::most_recent_sweep_elevation(scan, playback_ts, fallback)
    }

    /// Build the elevation list from a scan's VCP data.
    fn build_elevation_list(
        scan: &crate::state::radar_data::Scan,
    ) -> Vec<crate::state::ElevationListEntry> {
        state::playback_manager::build_elevation_list(scan)
    }

    /// Update the canvas overlay text with sweep timing and elevation info.
    fn update_overlay_from_sweep(&mut self, start: f64, end: f64, elevation_deg: f32) {
        self.state
            .viz_state
            .update_overlay(start, end, elevation_deg, self.state.use_local_time);
    }

    /// Send a render request to the worker for the current scan/elevation/product.
    fn request_worker_render(&mut self) {
        let mut elevation_number = self
            .state
            .viz_state
            .displayed_sweep_elevation_number
            .unwrap_or_else(|| self.best_elevation_number());

        // During real-time streaming, constrain to what's actually available.
        if self.state.live_mode_state.is_active()
            && !self.render.available_elevations().is_empty()
            && !self
                .render
                .available_elevations()
                .contains(&elevation_number)
        {
            elevation_number = self.render.best_available_elevation(elevation_number);
        }

        // Preemptive availability gate (archive/scrub path only — live-mode
        // scans may have more elevations than radar_timeline yet knows about).
        // If the displayed scan exists in radar_timeline but has no sweep at
        // this elevation, clear the canvas rather than issuing a request the
        // worker will reject with "No pre-computed sweep".
        if !self.state.live_mode_state.is_active() {
            if let Some(displayed_ts) = self.state.viz_state.displayed_scan_timestamp {
                if let Some(scan) = self
                    .state
                    .radar_timeline
                    .find_scan_at_timestamp(displayed_ts as f64)
                {
                    if !scan.sweeps.is_empty()
                        && !scan
                            .sweeps
                            .iter()
                            .any(|s| s.elevation_number == elevation_number)
                    {
                        self.clear_display_no_sweep();
                        return;
                    }
                }
            }
        }

        let product = self.state.viz_state.product.to_worker_string().to_string();
        let is_auto = self.state.viz_state.elevation_selection.is_auto();

        if self
            .render
            .request_render(elevation_number, &product, is_auto)
            && !self.state.session_stats.pipeline.processing
        {
            self.state.session_stats.pipeline.processing = true;
        }
    }

    /// Request volume render (all elevations for ray marching).
    fn request_worker_render_volume(&mut self) {
        let product = self.state.viz_state.product.to_worker_string().to_string();
        self.render.request_volume_render(&product);
    }

    /// Stop live mode streaming.
    #[allow(dead_code)] // Called from UI when user stops live mode
    fn stop_live_mode(&mut self, reason: state::LiveExitReason) {
        log::info!("Stopping live mode: {:?}", reason);

        self.state.live_mode_state.stop(reason);
        self.state.playback_state.time_model.disable_realtime_lock();
        self.streaming.stop_realtime();

        // Halt playback unless the user is actively scrubbing/jogging — those
        // paths set the new position themselves. Without this, we leave
        // playing=true at Realtime speed and position=wall-clock, so the
        // cursor keeps pace with "now" and mimics still being locked.
        if !matches!(
            reason,
            state::LiveExitReason::UserSeeked | state::LiveExitReason::UserJogged
        ) {
            self.state.playback_state.playing = false;
        }

        self.state.status_message = self
            .state
            .live_mode_state
            .last_exit_reason
            .map(|r| r.message().to_string())
            .unwrap_or_default();
    }

    /// Handle a realtime streaming result.
    fn handle_realtime_result(&mut self, result: nexrad::RealtimeResult, _ctx: &egui::Context) {
        // Get current time
        let now = js_sys::Date::now() / 1000.0;

        match result {
            nexrad::RealtimeResult::Started { site_id } => {
                log::debug!("Realtime streaming started for site: {}", site_id);
                self.state.live_mode_state.handle_streaming_started(now);
                self.state.status_message = format!("Live: connected to {}", site_id);
            }
            nexrad::RealtimeResult::ChunkReceived {
                chunks_in_volume,
                time_until_next,
                is_volume_end,
                fetch_latency_ms,
                projected_volume_end_secs,
                chunk_projections,
            } => {
                self.state
                    .session_stats
                    .record_fetch_latency(fetch_latency_ms);
                log::debug!(
                    "Realtime status: chunks_in_volume={} is_end={} latency={:.0}ms next_in={:?} proj_end={:?}",
                    chunks_in_volume,
                    is_volume_end,
                    fetch_latency_ms,
                    time_until_next,
                    projected_volume_end_secs,
                );
                self.state.live_mode_state.handle_realtime_chunk(
                    chunks_in_volume,
                    time_until_next,
                    is_volume_end,
                    now,
                    projected_volume_end_secs,
                    chunk_projections,
                );

                // Record chunk latency for the acquisition drawer
                self.state.acquisition.record_chunk_latency(
                    chunks_in_volume,
                    fetch_latency_ms,
                    None, // radial timestamps populated after ingest
                    None,
                );
            }
            nexrad::RealtimeResult::ChunkData {
                data,
                chunk_index,
                is_start,
                is_end,
                timestamp,
                skip_overlap_delete,
            } => {
                log::debug!(
                    "Realtime chunk received: index={} is_start={} is_end={} size={} bytes ts={}",
                    chunk_index,
                    is_start,
                    is_end,
                    data.len(),
                    timestamp,
                );

                // Track realtime chunk as an acquisition operation
                let rt_site_id = self.state.viz_state.site_id.clone();
                let op_id =
                    self.state
                        .acquisition
                        .create_operation(state::OperationKind::RealtimeChunk {
                            site_id: rt_site_id,
                            chunk_index,
                            is_start,
                            is_end,
                            scan_timestamp: timestamp,
                        });
                self.state.acquisition.mark_active(op_id);
                self.state
                    .acquisition
                    .mark_completed(op_id, data.len() as u64);

                if is_start {
                    self.state.status_message = "Live: receiving new volume...".to_string();
                    log::debug!("Realtime: new volume started, forwarding start chunk to worker");
                }

                // Forward chunk to worker for incremental ingest
                let site_id = self.state.viz_state.site_id.clone();
                let file_name = format!("live_{}_{}.nexrad", site_id, timestamp);
                if is_start {
                    self.state.session_stats.pipeline.processing = true;
                }

                // Look up whether this is the last chunk in its sweep from
                // the projection metadata. sequence = chunk_index + 1 (1-based).
                let sequence = (chunk_index + 1) as usize;
                let is_last_in_sweep = self
                    .state
                    .live_mode_state
                    .chunk_projections
                    .as_ref()
                    .and_then(|projs| projs.iter().find(|c| c.sequence == sequence))
                    .map(|c| c.chunk_index_in_sweep + 1 == c.chunks_in_sweep)
                    .unwrap_or(false);

                log::debug!(
                    "Realtime: forwarding chunk {} to worker for ingest (site={}, ts={}, last_in_sweep={})",
                    chunk_index,
                    site_id,
                    timestamp,
                    is_last_in_sweep,
                );
                self.render.ingest_chunk(
                    data,
                    site_id,
                    timestamp,
                    chunk_index,
                    is_start,
                    is_end,
                    file_name,
                    skip_overlap_delete,
                    is_last_in_sweep,
                );
            }
            nexrad::RealtimeResult::Error(msg) => {
                log::error!("Realtime streaming error: {}", msg);
                self.stop_live_mode(state::LiveExitReason::ConnectionError);
                // Preserve error message (stop_live_mode clears it)
                self.state.live_mode_state.error_message = Some(msg.clone());
                self.state.status_message = format!("Live error: {}", msg);

                // Track error as a failed acquisition operation
                let err_site_id = self.state.viz_state.site_id.clone();
                let op_id =
                    self.state
                        .acquisition
                        .create_operation(state::OperationKind::RealtimeChunk {
                            site_id: err_site_id,
                            chunk_index: 0,
                            is_start: false,
                            is_end: false,
                            scan_timestamp: 0,
                        });
                self.state.acquisition.mark_failed(op_id, msg);
            }
        }
    }

    /// Per-frame bookkeeping: record stats, apply theme, recompute staleness,
    /// update storm cells, and detect site changes.
    fn apply_frame_setup(&mut self, ctx: &egui::Context) {
        // Record frame time for FPS meter
        let dt = ctx.input(|i| i.stable_dt);
        self.state.session_stats.record_frame_time(dt);

        // Resolve theme and apply egui visuals
        self.state.is_dark = self.state.theme_mode.is_dark();
        if self.state.is_dark {
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = egui::Color32::BLACK;
            visuals.window_fill = egui::Color32::BLACK;
            visuals.extreme_bg_color = egui::Color32::BLACK;
            ctx.set_visuals(visuals);
        } else {
            ctx.set_visuals(egui::Visuals::light());
        }

        // Recompute data staleness every frame against wall-clock time.
        // This ensures archive data correctly shows its true age (days/years)
        // rather than a misleading "few minutes" relative to playback position.
        {
            let now = js_sys::Date::now() / 1000.0;
            if let Some(sweep_end) = self.state.viz_state.rendered_sweep_end_secs {
                let staleness = now - sweep_end;
                self.state.viz_state.data_staleness_secs = if staleness >= 0.0 {
                    Some(staleness)
                } else {
                    None
                };
            }
            if let Some(sweep_start) = self.state.viz_state.rendered_sweep_start_secs {
                let staleness = now - sweep_start;
                self.state.viz_state.data_staleness_start_secs = if staleness >= 0.0 {
                    Some(staleness)
                } else {
                    None
                };
            }
        }

        // Ensure continuous repainting for time-dependent UI elements (the "now"
        // marker on the timeline and the data-age indicators) even when the user
        // is idle and playback is stopped.  Repaint once per second which is
        // sufficient for these indicators while being easy on the CPU.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));

        // Run storm cell detection on demand when toggled on with existing data
        if self.state.viz_state.storm_cells_visible
            && self.state.viz_state.detected_storm_cells.is_empty()
        {
            if let Some(ref renderer) = self.gpu.gpu {
                if let Ok(r) = renderer.lock() {
                    if r.has_data() {
                        self.state.viz_state.detected_storm_cells = r.detect_storm_cells(
                            self.state.viz_state.center_lat,
                            self.state.viz_state.center_lon,
                            self.state.viz_state.storm_cell_threshold_dbz,
                        );
                    }
                }
            }
        }
        // Clear cached cells when toggle is off
        if !self.state.viz_state.storm_cells_visible
            && !self.state.viz_state.detected_storm_cells.is_empty()
        {
            self.state.viz_state.detected_storm_cells.clear();
        }

        // Detect site changes and clear volume ring
        if self
            .persistence
            .detect_site_change(&self.state.viz_state.site_id)
        {
            if let Some(ref renderer) = self.gpu.gpu {
                if let Ok(mut r) = renderer.lock() {
                    r.clear_data();
                }
            }
            self.playback_manager.clear_cache();
            self.render.clear_for_site_change();
            self.state.viz_state.displayed_scan_timestamp = None;
            self.state.viz_state.displayed_sweep_elevation_number = None;
            self.state.shadow_scan_boundaries.clear();
        }
    }

    /// Drain the command queue and execute each command.
    /// Returns flags for (download_selection, download_at_position, pump_queue).
    fn dispatch_commands(&mut self, ctx: &egui::Context) -> (bool, bool, bool) {
        let commands = self.state.drain_commands();
        let mut do_download_selection = false;
        let mut do_download_at_position = false;
        let mut do_pump_queue = false;
        for cmd in commands {
            match cmd {
                state::AppCommand::ClearCache => {
                    if !self.acquisition.cache_load_channel.is_loading() {
                        self.acquisition
                            .cache_load_channel
                            .clear_cache(ctx.clone(), self.acquisition.facade().clone());
                    } else {
                        // Re-enqueue if channel is busy
                        self.state.push_command(state::AppCommand::ClearCache);
                    }
                }
                state::AppCommand::WipeAll => {
                    let facade = self.acquisition.facade().clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        if let Err(e) = facade.clear_all().await {
                            log::error!("Failed to clear IndexedDB: {}", e);
                        }
                        if let Some(window) = web_sys::window() {
                            if let Ok(Some(storage)) = window.local_storage() {
                                let _ = storage.clear();
                            }
                            let _ = window.location().reload();
                        }
                    });
                }
                state::AppCommand::RefreshTimeline { auto_position } => {
                    if auto_position {
                        self.state.auto_position_on_timeline_load = true;
                    }
                    if !self.acquisition.cache_load_channel.is_loading() {
                        self.acquisition.cache_load_channel.load_site_timeline(
                            ctx.clone(),
                            self.acquisition.facade().clone(),
                            self.state.viz_state.site_id.clone(),
                        );
                    } else {
                        self.state.push_command(state::AppCommand::RefreshTimeline {
                            auto_position: false,
                        });
                    }
                }
                state::AppCommand::CheckEviction => {
                    let facade = self.acquisition.facade().clone();
                    let quota = self.state.storage_settings.quota_bytes;
                    let target = self.state.storage_settings.eviction_target_bytes;
                    let ctx_clone = ctx.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match facade.check_and_evict(quota, target).await {
                            Ok((evicted, count, quota_warning)) => {
                                if evicted {
                                    log::debug!("Eviction complete: removed {} scans", count);
                                }
                                if let Some(warning) = quota_warning {
                                    log::warn!("Quota warning: {}", warning);
                                }
                            }
                            Err(e) => {
                                log::error!("Eviction check failed: {}", e);
                            }
                        }
                        ctx_clone.request_repaint();
                    });
                }
                state::AppCommand::StartLive => {
                    self.start_live_mode(ctx);
                }
                state::AppCommand::DownloadSelection => {
                    do_download_selection = true;
                }
                state::AppCommand::DownloadAtPosition => {
                    do_download_at_position = true;
                }
                state::AppCommand::PauseQueue => {
                    self.state.acquisition.pause();
                }
                state::AppCommand::ResumeQueue => {
                    self.state.acquisition.resume();
                    do_pump_queue = true;
                }
                state::AppCommand::RetryFailed(op_id) => {
                    self.state.acquisition.retry_failed(op_id);
                    do_pump_queue = true;
                }
                state::AppCommand::SkipFailed(op_id) => {
                    self.state.acquisition.skip_failed(op_id);
                    do_pump_queue = true;
                }
                state::AppCommand::CancelOperation(op_id) => {
                    self.state.acquisition.cancel_operation(op_id);
                }
                state::AppCommand::ReorderOperation(op_id, delta) => {
                    self.state.acquisition.reorder_operation(op_id, delta);
                }
                state::AppCommand::RetryWorker => match self.render.create_worker(ctx.clone()) {
                    Ok(()) => {
                        self.state.worker_init_error = None;
                        self.state.set_status("Decode worker initialized");
                    }
                    Err(msg) => {
                        self.state.worker_init_error = Some(msg);
                    }
                },
            }
        }

        (
            do_download_selection,
            do_download_at_position,
            do_pump_queue,
        )
    }

    /// Process results from cache loads, web workers, downloads, and archive listings.
    fn handle_worker_results(&mut self, _ctx: &egui::Context) {
        if let Some(result) = self.acquisition.cache_load_channel.try_recv() {
            self.handle_cache_load_outcome(result);
        }

        for outcome in self.render.try_recv() {
            match outcome {
                nexrad::WorkerOutcome::Ingested(result) => {
                    self.handle_ingested_outcome(result);
                }
                nexrad::WorkerOutcome::ChunkIngested(result) => {
                    self.handle_chunk_ingested_outcome(result);
                }
                nexrad::WorkerOutcome::Decoded(result) => {
                    self.handle_decoded_outcome(result);
                }
                nexrad::WorkerOutcome::LiveDecoded(result) => {
                    self.handle_live_decoded_outcome(result);
                }
                nexrad::WorkerOutcome::VolumeDecoded(volume_data) => {
                    self.handle_volume_decoded_outcome(volume_data);
                }
                nexrad::WorkerOutcome::WorkerError {
                    id,
                    message,
                    failed_scan_timestamp_secs,
                } => {
                    self.handle_worker_error_outcome(id, message, failed_scan_timestamp_secs);
                }
            }
        }

        if let Some(result) = self.acquisition.download_channel.try_recv() {
            self.handle_download_outcome(result);
        }

        if let Some(result) = self.acquisition.download_channel.try_recv_listing() {
            self.handle_listing_outcome(result);
        }
    }

    fn handle_cache_load_outcome(&mut self, result: nexrad::CacheLoadResult) {
        match result {
            nexrad::CacheLoadResult::Success {
                site_id,
                metadata,
                total_cache_size,
            } => {
                log::debug!(
                    "Timeline loaded from cache: {} scan(s) for site {}",
                    metadata.len(),
                    site_id
                );

                // Update cache size in session stats
                self.state.session_stats.cache_size_bytes = total_cache_size;

                // Build timeline from metadata
                self.state.radar_timeline = state::RadarTimeline::from_metadata(metadata);

                // Get time ranges (may be non-contiguous)
                let ranges = self.state.radar_timeline.time_ranges();
                if !ranges.is_empty() {
                    // Set overall bounds from first to last
                    let start = ranges.first().unwrap().start;
                    let end = ranges.last().unwrap().end;
                    self.state.playback_state.data_start_timestamp = Some(start as i64);
                    self.state.playback_state.data_end_timestamp = Some(end as i64);

                    // Position playback at the end of the most recent range
                    let most_recent_end = ranges.last().unwrap().end;

                    // Only auto-position on initial load or site change,
                    // not when refreshing after a download.
                    if self.state.auto_position_on_timeline_load {
                        self.state.auto_position_on_timeline_load = false;
                        let ts = self.state.playback_state.playback_position();
                        let in_any_range = ranges.iter().any(|r| r.contains(ts));
                        if !in_any_range {
                            self.state
                                .playback_state
                                .set_playback_position(most_recent_end);
                            self.state.playback_state.center_view_on(most_recent_end);
                        }
                    }

                    log::debug!("Timeline has {} contiguous range(s)", ranges.len());
                }
            }
            nexrad::CacheLoadResult::Error(msg) => {
                log::error!("Cache load failed: {}", msg);
            }
        }
    }

    fn handle_ingested_outcome(&mut self, result: nexrad::IngestResult) {
        // Processing stays active through decode — don't mark done yet.
        // Transition to decoding phase. Don't remove the ghost
        // yet — it stays visible until the timeline refreshes
        // and a real scan block replaces it (the ghost renderer's
        // overlap check handles the visual transition).
        self.state.download_progress.phase = crate::state::DownloadPhase::Decoding;
        log::debug!(
            "Ingest complete: {} ({} records, {} elevations, {} sweeps, {:.0}ms, fetch: {:.0}ms)",
            result.scan_key,
            result.records_stored,
            result.elevation_numbers.len(),
            result.sweeps.len(),
            result.total_ms,
            result.context.fetch_latency_ms,
        );

        self.state
            .session_stats
            .record_fetch_latency(result.context.fetch_latency_ms);
        self.state
            .session_stats
            .record_processing_time(result.total_ms);

        // Store detailed ingest timing for the detail modal.
        self.state.session_stats.last_ingest_detail = Some(crate::state::IngestTimingDetail {
            split_ms: result.split_ms,
            decompress_ms: result.decompress_ms,
            decode_ms: result.decode_ms,
            extract_ms: result.extract_ms,
            store_ms: result.store_ms,
            index_ms: result.index_ms,
        });

        // Track the scan for render requests
        self.render
            .set_scan(result.scan_key.clone(), result.elevation_numbers);
        self.state.viz_state.displayed_scan_timestamp = Some(result.context.timestamp_secs);
        self.state.viz_state.displayed_sweep_elevation_number = None;
        // Refresh timeline to include the new scan
        self.state.push_command(state::AppCommand::RefreshTimeline {
            auto_position: false,
        });

        // Request eviction check
        self.state.push_command(state::AppCommand::CheckEviction);

        // Force a fresh render
        self.render.force_fresh_render();

        // Trigger render for the ingested scan
        self.request_worker_render();
        if self.state.viz_state.volume_3d_enabled {
            self.request_worker_render_volume();
        }
    }

    fn handle_chunk_ingested_outcome(&mut self, result: nexrad::ChunkIngestResult) {
        let is_live = self.state.live_mode_state.is_active();
        let source = "Realtime";

        // Build enriched log with projection-derived chunk positioning.
        let chunk_vol_index = result.context.chunk_index + 1; // 1-based for display
        let elev_nums: Vec<u8> = result
            .chunk_elev_spans
            .iter()
            .map(|&(e, _, _, _)| e)
            .collect();
        let total_azimuths: u32 = result
            .chunk_elev_spans
            .iter()
            .map(|&(_, _, _, count)| count)
            .sum();

        // Azimuth angle range from the chunk's azimuth data
        let az_range_str =
            if let Some(&(_, first_az, last_az)) = result.chunk_elev_az_ranges.first() {
                format!("{:.1}°–{:.1}°", first_az, last_az)
            } else {
                "n/a".to_string()
            };

        // Look up chunk-in-sweep and remaining from projection metadata.
        // chunk_index is 0-based where 0 = Start chunk (sequence 1), so
        // chunk_vol_index (= chunk_index + 1) already equals the 1-based sequence.
        let sequence = chunk_vol_index as usize;
        let (chunk_in_sweep_str, remaining_str) = self
            .state
            .live_mode_state
            .chunk_projections
            .as_ref()
            .and_then(|projs| {
                projs.iter().find(|c| c.sequence == sequence).map(|c| {
                    let in_sweep = format!("{}/{}", c.chunk_index_in_sweep + 1, c.chunks_in_sweep);
                    // Count remaining chunks in this sweep after this one
                    let remaining_in_sweep =
                        c.chunks_in_sweep.saturating_sub(c.chunk_index_in_sweep + 1);
                    (in_sweep, format!("{}", remaining_in_sweep))
                })
            })
            .unwrap_or_else(|| ("?/?".to_string(), "?".to_string()));

        log::debug!(
            "{}: chunk ingested scan={} vol_chunk={} sweep_chunk={} remaining_in_sweep={} \
             elevs={:?} azimuths={} az_range={} \
             elevs_completed={:?} sweeps_stored={} is_end={} vcp={:?} {:.1}ms",
            source,
            result.scan_key,
            chunk_vol_index,
            chunk_in_sweep_str,
            remaining_str,
            elev_nums,
            total_azimuths,
            az_range_str,
            result.elevations_completed,
            result.sweeps_stored,
            result.is_end,
            result.vcp.as_ref().map(|v| v.number),
            result.total_ms,
        );

        // Update scan key and available elevations
        self.render.set_scan_key(result.scan_key.clone());
        let had_elevations = !self.render.available_elevations().is_empty();
        self.render.add_elevations(&result.elevations_completed);

        // Update displayed timestamp
        self.state.viz_state.displayed_scan_timestamp = Some(result.context.timestamp_secs);

        // Only update live_mode_state when actually in live mode
        if is_live {
            self.state.live_mode_state.current_scan_key = Some(result.scan_key.clone());

            if !result.chunk_elev_spans.is_empty() {
                self.state
                    .live_mode_state
                    .record_chunk_elev_spans(&result.chunk_elev_spans);
            }

            // Set the volume start time from the authoritative
            // timestamp parsed directly from the NEXRAD message
            // header (the first radial of the volume scan).
            if let Some(header_time) = result.volume_header_time_secs {
                self.state.live_mode_state.current_volume_start = Some(header_time);
            }
            if !result.elevations_completed.is_empty() {
                let vol_start_ts = self
                    .state
                    .live_mode_state
                    .current_volume_start
                    .unwrap_or(result.context.timestamp_secs as f64);
                self.state
                    .live_mode_state
                    .record_elevations(&result.elevations_completed, vol_start_ts);
            }
            if let Some(ref vcp) = result.vcp {
                self.state.live_mode_state.record_vcp(vcp);
            }

            self.state.live_mode_state.record_in_progress_elevation(
                result.current_elevation,
                result.current_elevation_radials,
            );

            // Record per-chunk azimuth ranges for the current elevation
            if let Some(cur_elev) = result.current_elevation {
                for &(elev, first_az, last_az) in &result.chunk_elev_az_ranges {
                    if elev == cur_elev {
                        let radial_count = result
                            .chunk_elev_spans
                            .iter()
                            .find(|&&(e, _, _, _)| e == elev)
                            .map(|&(_, _, _, c)| c)
                            .unwrap_or(0);
                        self.state.live_mode_state.current_elev_chunks.push((
                            first_az,
                            last_az,
                            radial_count,
                        ));
                    }
                }
            }

            if !result.sweeps.is_empty() {
                self.state
                    .live_mode_state
                    .update_sweep_metas(result.sweeps.clone());
            }

            self.state
                .live_mode_state
                .record_last_radial(result.last_radial_azimuth, result.last_radial_time_secs);

            // ── Log: sweep storage ────────────────────────────────────
            if !result.elevations_completed.is_empty() {
                for &completed_elev in &result.elevations_completed {
                    if let Some(meta) = result
                        .sweeps
                        .iter()
                        .find(|s| s.elevation_number == completed_elev)
                    {
                        log::debug!(
                            "{}: sweep stored elev={} angle={:.1}° start_az={:.1}° \
                             time={:.1}–{:.1}s dur={:.2}s products={} vol_chunk={}",
                            source,
                            completed_elev,
                            meta.elevation,
                            meta.start_azimuth,
                            meta.start,
                            meta.end,
                            meta.end - meta.start,
                            result.sweeps_stored,
                            chunk_vol_index,
                        );
                    } else {
                        log::debug!(
                            "{}: sweep stored elev={} (no SweepMeta) products={} vol_chunk={}",
                            source,
                            completed_elev,
                            result.sweeps_stored,
                            chunk_vol_index,
                        );
                    }
                }
            }

            // ── Log + dispatch: live partial-sweep render ─────────────
            // Always render whatever elevation is currently being
            // accumulated — the user expects to see live progress
            // regardless of which elevation was previously displayed.
            if !result.is_end {
                if let Some(target_elev) = result.current_elevation {
                    let product = self.state.viz_state.product.to_worker_string().to_string();

                    // Summarize what the accumulator holds for this elevation
                    let accum_radials = result.current_elevation_radials.unwrap_or(0);
                    let accum_chunks: usize = self
                        .state
                        .live_mode_state
                        .chunk_elev_spans
                        .iter()
                        .filter(|&&(e, _, _, _)| e == target_elev)
                        .count();
                    let accum_az_range = self
                        .state
                        .live_mode_state
                        .current_elev_chunks
                        .iter()
                        .fold((f32::MAX, f32::MIN), |(lo, hi), &(first_az, last_az, _)| {
                            (lo.min(first_az), hi.max(last_az))
                        });
                    let az_str = if accum_az_range.0 < f32::MAX {
                        format!("{:.1}°–{:.1}°", accum_az_range.0, accum_az_range.1)
                    } else {
                        "n/a".to_string()
                    };

                    log::debug!(
                        "{}: render_live dispatched elev={} product={} accum_radials={} \
                         accum_chunks={} accum_az={} vol_chunk={}",
                        source,
                        target_elev,
                        product,
                        accum_radials,
                        accum_chunks,
                        az_str,
                        chunk_vol_index,
                    );

                    self.render.render_live(target_elev, product);
                }
            }
        }

        // Refresh timeline when new elevations are written to cache
        if !result.elevations_completed.is_empty() {
            log::debug!(
                "{}: {} new elevation(s) cached, refreshing timeline (total available: {:?})",
                source,
                result.elevations_completed.len(),
                self.render.available_elevations(),
            );
            self.state.push_command(state::AppCommand::RefreshTimeline {
                auto_position: !is_live,
            });

            if is_live {
                self.state.status_message = format!(
                    "Live: {} elevation(s) cached",
                    self.render.available_elevations().len()
                );
            }
        }

        if result.is_end {
            if is_live {
                if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
                    if let Ok(mut r) = renderer.lock() {
                        r.promote_current_to_previous(gl);
                    }
                }
                let now = js_sys::Date::now() / 1000.0;
                self.state.live_mode_state.handle_volume_complete(now);
                self.state.status_message = format!(
                    "Live: volume complete ({} elevations)",
                    self.render.available_elevations().len()
                );
            } else {
                let now = js_sys::Date::now() / 1000.0;
                self.state.playback_state.set_playback_position(now);
                self.state.playback_state.center_view_on(now);
            }

            log::debug!(
                "{}: volume complete — {} elevations, triggering render",
                source,
                self.render.available_elevations().len()
            );
            self.state.push_command(state::AppCommand::RefreshTimeline {
                auto_position: !is_live,
            });
            self.state.push_command(state::AppCommand::CheckEviction);
            self.state.session_stats.pipeline.mark_processing_done();

            self.state.viz_state.displayed_sweep_elevation_number = None;
            self.render.force_fresh_render();
            if !is_live {
                self.request_worker_render();
                if self.state.viz_state.volume_3d_enabled {
                    self.request_worker_render_volume();
                }
            }
        } else if !had_elevations && !self.render.available_elevations().is_empty() {
            log::debug!(
                "{}: first elevation available, triggering initial render",
                source
            );
            self.render.force_fresh_render();
            if !is_live {
                self.request_worker_render();
                if self.state.viz_state.volume_3d_enabled {
                    self.request_worker_render_volume();
                }
            }
        }
    }

    fn handle_decoded_outcome(&mut self, result: nexrad::DecodeResult) {
        // Processing complete → transition to rendering.
        self.state.session_stats.pipeline.mark_processing_done();
        self.state.session_stats.pipeline.rendering = true;

        log::debug!(
            "Decode complete: {}x{} (az x gates), {} radials, product={}, {:.0}ms",
            result.azimuth_count,
            result.gate_count,
            result.radial_count,
            result.product,
            result.total_ms,
        );

        self.state.session_stats.record_render_time(result.total_ms);

        // Cache decoded data for stateless sweep animation
        let result_sweep_id = sweep_cache_key(
            &result.context.scan_key,
            result.context.elevation_number,
            &result.product,
        );
        self.playback_manager.cache_sweep(
            result_sweep_id.clone(),
            CachedSweepData {
                gate_values: result.gate_values.clone(),
                azimuths: result.azimuths.clone(),
                azimuth_count: result.azimuth_count,
                gate_count: result.gate_count,
                first_gate_range_km: result.first_gate_range_km,
                gate_interval_km: result.gate_interval_km,
                max_range_km: result.max_range_km,
                offset: result.offset,
                scale: result.scale,
                azimuth_spacing_deg: result.azimuth_spacing_deg,
                radial_times: result.radial_times.clone(),
                sweep_start_secs: result.sweep_start_secs,
                sweep_end_secs: result.sweep_end_secs,
                product: result.product.clone(),
            },
        );

        // Upload decoded data to GPU renderer — but only if this
        // result is for the currently displayed scan. Background
        // prev-sweep decodes are cached but not uploaded here;
        // sync_prev_sweep_texture picks them up next frame.
        // Only upload to the primary GPU texture if this result
        // matches what advance_playback intended: same scan key AND
        // same elevation number. Without the elevation check, SAILS
        // VCPs (duplicate 0.5° at elev 1 and 2) cause oscillation
        // where prefetch/sync requests fight the main render path.
        let is_current_scan = self
            .render
            .scan_key()
            .is_some_and(|k| k == result.context.scan_key)
            && self
                .state
                .viz_state
                .displayed_sweep_elevation_number
                .is_some_and(|e| e == result.context.elevation_number);
        if self.state.effective_sweep_animation() && !is_current_scan {
            log::debug!("[sweep-anim] cached bg decode: {}", result_sweep_id);
            // Clear pending tracker so sync_prev_sweep_texture can load from cache
            if self.playback_manager.pending_prev_sweep_key() == Some(&result_sweep_id) {
                self.playback_manager.set_pending_prev_sweep_key(None);
            }
        }
        let t_gpu = web_time::Instant::now();
        // In live mode, LiveDecoded drives the GPU — skip Decoded
        // uploads so completed-elevation IDB renders don't overwrite
        // the current partial sweep.
        let skip_gpu_upload = self.state.live_mode_state.is_active();
        if is_current_scan && !skip_gpu_upload {
            if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
                if let Ok(mut r) = renderer.lock() {
                    r.update_data(
                        gl,
                        &result.azimuths,
                        &result.gate_values,
                        result.azimuth_count,
                        result.gate_count,
                        result.first_gate_range_km,
                        result.gate_interval_km,
                        result.max_range_km,
                        result.offset,
                        result.scale,
                        result.azimuth_spacing_deg,
                        &result.radial_times,
                    );
                    r.set_current_sweep_id(Some(result_sweep_id));
                    r.update_color_table(gl, &result.product);

                    // Run storm cell detection if enabled
                    if self.state.viz_state.storm_cells_visible {
                        self.state.viz_state.detected_storm_cells = r.detect_storm_cells(
                            self.state.viz_state.center_lat,
                            self.state.viz_state.center_lon,
                            self.state.viz_state.storm_cell_threshold_dbz,
                        );
                    }
                }
            }
        }
        let gpu_upload_ms = t_gpu.elapsed().as_secs_f64() * 1000.0;

        // Store detailed render timing for the detail modal.
        self.state.session_stats.last_render_detail = Some(crate::state::RenderTimingDetail {
            fetch_ms: result.fetch_ms,
            deser_ms: result.deser_ms,
            marshal_ms: result.marshal_ms,
            gpu_upload_ms,
        });

        // GPU upload complete.
        self.state.session_stats.pipeline.mark_render_done();

        // Remove this scan from in-flight ghost tracking.
        if let Some(displayed_ts) = self.state.viz_state.displayed_scan_timestamp {
            self.state
                .download_progress
                .in_flight_scans
                .retain(|&(start, _)| start != displayed_ts);
        }
        // If no more in-flight or pending, fully clear progress.
        if self.state.download_progress.in_flight_scans.is_empty()
            && self.state.download_progress.pending_scans.is_empty()
            && !self.state.download_selection_in_progress
        {
            self.state.download_progress.clear();
        }

        // Refine canvas overlay with precise decoded data
        if result.sweep_start_secs > 0.0 {
            self.update_overlay_from_sweep(
                result.sweep_start_secs,
                result.sweep_end_secs,
                result.mean_elevation,
            );
        }
    }

    fn handle_live_decoded_outcome(&mut self, result: nexrad::DecodeResult) {
        log::debug!(
            "Live decode: {}x{}, {} radials, {}, {:.0}ms",
            result.azimuth_count,
            result.gate_count,
            result.radial_count,
            result.product,
            result.total_ms,
        );

        if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
            if let Ok(mut r) = renderer.lock() {
                // Build a live sweep ID so we can detect elevation transitions
                let live_elev = result.context.elevation_number;
                let live_sweep_id = format!("live|{}", live_elev);

                // If the current texture has data from a different sweep
                // (complete or different live elevation), promote it to previous
                // so it becomes the background for compositing partial data.
                let should_promote = r.current_sweep_id().is_some_and(|id| id != live_sweep_id);
                if should_promote {
                    // Capture the current sweep's metadata before promoting it to
                    // "previous" — this drives the overlay info panel and donut labels.
                    let prev_elev_deg =
                        self.state.viz_state.rendered_sweep_end_secs.and_then(|_| {
                            self.state
                                .viz_state
                                .elevation
                                .trim_end_matches('\u{00B0}')
                                .parse::<f32>()
                                .ok()
                        });
                    let prev_elev_num = self.state.viz_state.displayed_sweep_elevation_number;
                    if let (Some(start), Some(end), Some(elev)) = (
                        self.state.viz_state.rendered_sweep_start_secs,
                        self.state.viz_state.rendered_sweep_end_secs,
                        prev_elev_deg,
                    ) {
                        self.state.viz_state.prev_sweep_overlay = Some((elev, start, end));
                        self.state.viz_state.prev_sweep_elevation_number = prev_elev_num;
                    }

                    r.promote_current_to_previous(gl);
                }

                r.update_data(
                    gl,
                    &result.azimuths,
                    &result.gate_values,
                    result.azimuth_count,
                    result.gate_count,
                    result.first_gate_range_km,
                    result.gate_interval_km,
                    result.max_range_km,
                    result.offset,
                    result.scale,
                    result.azimuth_spacing_deg,
                    &result.radial_times,
                );
                r.set_current_sweep_id(Some(live_sweep_id));
                r.update_color_table(gl, &result.product);
            }
        }

        // Update overlay staleness so the age counter reflects
        // the most recently received live data.
        if result.sweep_end_secs > 0.0 {
            self.update_overlay_from_sweep(
                result.sweep_start_secs,
                result.sweep_end_secs,
                result.mean_elevation,
            );
        }

        // Store the chronological azimuth range for sweep compositing.
        // Must use chronological first/last (from radial timestamps), NOT
        // sorted min/max. Once a sweep wraps past 0°, the sorted range
        // spans ~360° and the shader thinks the entire circle has current
        // data, hiding the previous sweep.
        if !result.azimuths.is_empty() {
            // Chronological first = sweep start azimuth (set once per sweep).
            // Chronological last = most recent radial's azimuth from the live state.
            if self.state.live_mode_state.sweep_start_azimuth.is_none() {
                // First live decode for this sweep: use the earliest radial
                // by collection time as the sweep start.
                let first_az = if !result.radial_times.is_empty() {
                    let min_time_idx = result
                        .radial_times
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    result.azimuths[min_time_idx]
                } else {
                    result.azimuths[0]
                };
                self.state.live_mode_state.sweep_start_azimuth = Some(first_az);
            }

            // The trailing edge of received data: latest radial by collection time.
            let last_az = if !result.radial_times.is_empty() {
                let max_time_idx = result
                    .radial_times
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                    .map(|(i, _)| i)
                    .unwrap_or(result.azimuths.len() - 1);
                result.azimuths[max_time_idx]
            } else {
                *result.azimuths.last().unwrap()
            };

            let first_az = self
                .state
                .live_mode_state
                .sweep_start_azimuth
                .unwrap_or(0.0);
            log::debug!(
                "Live azimuth range: chrono_first={:.1} chrono_last={:.1} count={}",
                first_az,
                last_az,
                result.azimuths.len(),
            );
            self.state.live_mode_state.live_data_azimuth_range = Some((first_az, last_az));
        }
    }

    fn handle_volume_decoded_outcome(&mut self, volume_data: nexrad::VolumeData) {
        log::debug!(
            "Volume decode complete: {} sweeps, {:.1}KB, product={}, {:.0}ms",
            volume_data.sweeps.len(),
            volume_data.buffer.len() as f64 / 1024.0,
            volume_data.product,
            volume_data.total_ms,
        );

        // Upload to volume ray renderer
        if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.volume_ray, &self.gpu.gl) {
            if let Ok(mut r) = renderer.lock() {
                r.update_volume(
                    gl,
                    &volume_data.buffer,
                    volume_data.word_size,
                    &volume_data.sweeps,
                    self.state.viz_state.center_lat,
                    self.state.viz_state.center_lon,
                );
            }
        }

        // Update LUT for the volume product
        if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
            if let Ok(mut r) = renderer.lock() {
                r.update_color_table(gl, &volume_data.product);
            }
        }
    }

    fn handle_worker_error_outcome(
        &mut self,
        id: u64,
        message: String,
        failed_scan_timestamp_secs: Option<i64>,
    ) {
        log::warn!("Worker error (request {}): {}", id, message);
        self.state.status_message = format!("Worker error: {}", message);

        // When the worker reports that the requested (elevation, product) has
        // no pre-computed sweep, clear the stale canvas so the user sees what
        // the timeline already knows — nothing matches their current filter.
        // Narrowed to this specific message so transient errors (worker
        // disconnect, IDB failure) keep the last-good view instead of blanking.
        if message.starts_with("No pre-computed sweep") {
            self.clear_display_no_sweep();
        }

        // Clean up the "processing" timeline ghost for the failed scan.
        // Prefer the scan attributed to the failing worker request so the
        // right ghost is removed even after the user scrolled away and
        // displayed_scan_timestamp now points elsewhere.
        let cleanup_ts =
            failed_scan_timestamp_secs.or(self.state.viz_state.displayed_scan_timestamp);
        if let Some(ts) = cleanup_ts {
            self.state
                .download_progress
                .in_flight_scans
                .retain(|&(start, _)| start != ts);
        }
        self.state.session_stats.pipeline.processing = false;
        self.state.session_stats.pipeline.rendering = false;
        if self.state.download_progress.in_flight_scans.is_empty()
            && self.state.download_progress.pending_scans.is_empty()
            && !self.state.download_selection_in_progress
        {
            self.state.download_progress.clear();
        }
    }

    fn handle_download_outcome(&mut self, result: nexrad::DownloadResult) {
        // Extract scan and timing info from result
        let (scan_opt, is_cache_hit) = match &result {
            nexrad::DownloadResult::Success {
                scan,
                fetch_latency_ms,
                decode_latency_ms,
            } => {
                self.state
                    .session_stats
                    .record_fetch_latency(*fetch_latency_ms);
                self.state
                    .session_stats
                    .record_processing_time(*decode_latency_ms);
                (Some(scan), false)
            }
            nexrad::DownloadResult::CacheHit(scan) => (Some(scan), true),
            _ => (None, false),
        };

        if let Some(scan) = scan_opt {
            let fetch_latency = match &result {
                nexrad::DownloadResult::Success {
                    fetch_latency_ms, ..
                } => *fetch_latency_ms,
                _ => 0.0,
            };

            // Move this scan's boundary to in-flight tracking (ghost stays
            // visible until processing completes in the Decoded handler).
            let scan_ts = scan.key.scan_start.as_secs();
            let scan_end = self
                .acquisition
                .download_queue
                .find_by_scan_start(scan_ts)
                .map(|item| item.scan_end)
                .unwrap_or(scan_ts + FALLBACK_SCAN_DURATION_SECS);
            self.state
                .download_progress
                .in_flight_scans
                .push((scan_ts, scan_end));

            // Track which scan is being processed so error cleanup
            // can remove the correct ghost.
            self.state.viz_state.displayed_scan_timestamp = Some(scan_ts);

            if is_cache_hit {
                self.state.status_message = format!("Loaded from cache: {}", scan.file_name);

                // Cache hit: skip ingest, go straight to decode.
                // Ghost stays until timeline refresh shows the real scan.
                self.state.download_progress.phase = crate::state::DownloadPhase::Decoding;

                // Cache hit: records already in IDB. Send render request directly.
                self.render.set_scan_key(scan.key.to_storage_key());
                self.state.viz_state.displayed_sweep_elevation_number = None;

                // Populate elevation numbers from timeline metadata if available
                if let Some(tl_scan) = self
                    .state
                    .radar_timeline
                    .find_recent_scan(scan_ts as f64, 1.0)
                {
                    let mut elev_nums: Vec<u8> =
                        tl_scan.sweeps.iter().map(|s| s.elevation_number).collect();
                    elev_nums.sort_unstable();
                    elev_nums.dedup();
                    if !elev_nums.is_empty() {
                        self.render.set_elevations(elev_nums);
                    }
                }

                self.render.force_fresh_render();
                self.request_worker_render();
                if self.state.viz_state.volume_3d_enabled {
                    self.request_worker_render_volume();
                }
            } else {
                self.state.status_message =
                    format!("Downloaded: {} ({} bytes)", scan.file_name, scan.data.len());

                // Transition to ingesting phase
                self.state.download_progress.phase = crate::state::DownloadPhase::Ingesting;

                // Fresh download: send raw bytes to worker for ingest.
                // Worker splits records, probes elevations, stores in IDB,
                // then returns metadata. We render on the Ingested callback.
                self.state.session_stats.pipeline.processing = true;
                self.render.ingest(
                    scan.data.clone(),
                    scan.key.site.0.clone(),
                    scan.key.scan_start.as_secs(),
                    scan.file_name.clone(),
                    fetch_latency,
                );
            }

            self.acquisition.current_scan = Some(scan.clone());

            // Refresh timeline to show the new/loaded scan
            self.state.push_command(state::AppCommand::RefreshTimeline {
                auto_position: false,
            });
        }

        // Mark acquisition operation completed on success
        if let Some(scan) = scan_opt {
            if let Some(op_id) = self.acquisition.download_queue.take_active_operation_id() {
                self.state
                    .acquisition
                    .mark_completed(op_id, scan.data.len() as u64);
            }
        }

        if let nexrad::DownloadResult::Error(msg) = &result {
            self.state.status_message = format!("Download failed: {}", msg);
            log::error!("Download failed: {}", msg);

            // Mark acquisition operation as failed and error-pause
            if let Some(op_id) = self.acquisition.download_queue.take_active_operation_id() {
                self.state.acquisition.mark_failed(op_id, msg.clone());
            }

            // Clear download progress on error if no more work remains
            if !self.acquisition.download_queue.has_work() {
                self.acquisition.download_queue.clear();
                self.state.download_progress.clear();
            }
        }
    }

    fn handle_listing_outcome(&mut self, result: nexrad::ListingResult) {
        match result {
            nexrad::ListingResult::Success {
                site_id,
                date,
                listing,
            } => {
                log::debug!(
                    "Archive listing received: {} files for {}/{}",
                    listing.files.len(),
                    site_id,
                    date
                );
                self.acquisition
                    .archive_index
                    .insert(&site_id, date, listing);

                // Rebuild shadow scan boundaries for the timeline
                if site_id == self.state.viz_state.site_id {
                    self.state.shadow_scan_boundaries = self
                        .acquisition
                        .archive_index
                        .all_boundaries_for_site(&site_id);
                }

                // Resume pending download now that the listing is available
                if let Some(pending) = &self.acquisition.pending_download {
                    if pending.is_position {
                        self.state
                            .push_command(state::AppCommand::DownloadAtPosition);
                    } else {
                        self.state
                            .push_command(state::AppCommand::DownloadSelection);
                    }
                }
            }
            nexrad::ListingResult::Error(msg) => {
                log::error!("Listing request failed: {}", msg);
                // Abandon pending download on listing failure
                if self.acquisition.pending_download.is_some() {
                    self.acquisition.pending_download = None;
                    self.state.status_message =
                        format!("Download cancelled: listing fetch failed ({})", msg);
                }
            }
        }
    }

    /// Kick off or continue selection/position downloads.
    fn pump_download_queue(
        &mut self,
        ctx: &egui::Context,
        do_download_selection: bool,
        do_download_at_position: bool,
        do_pump_queue: bool,
    ) {
        {
            let download_type = if do_download_at_position {
                Some(true)
            } else if do_download_selection {
                Some(false)
            } else {
                None // Just pumping existing queue, or nothing to do
            };
            let queue_has_work = self.acquisition.download_queue.has_work();
            if do_download_selection || do_download_at_position || do_pump_queue || queue_has_work {
                self.process_selection_download(ctx, download_type);
            }
        }
    }

    /// Drain the realtime channel and manage live-mode lifecycle.
    fn handle_streaming_results(&mut self, ctx: &egui::Context) {
        for event in self.streaming.poll() {
            match event {
                nexrad::StreamingEvent::Realtime(result) => {
                    self.handle_realtime_result(result, ctx);
                }
            }
        }

        // Stop realtime channel if live mode was stopped by UI
        if !self.state.live_mode_state.is_active() && self.streaming.is_realtime_active() {
            log::debug!("Stopping realtime channel (live mode ended)");
            self.streaming.stop_realtime();
        }

        // Update live mode countdown from realtime channel
        if self.state.live_mode_state.is_active() {
            if let Some(duration) = self.streaming.time_until_next() {
                let now = js_sys::Date::now() / 1000.0;
                self.state.live_mode_state.next_chunk_expected_at =
                    Some(now + duration.as_secs_f64());
            }
        }
    }

    /// Auto-load scans when scrubbing the timeline and prefetch upcoming sweeps.
    fn advance_playback(&mut self) {
        // Live mode drives rendering via ChunkIngested/LiveDecoded — skip playback-driven renders.
        if self.state.live_mode_state.is_active() {
            return;
        }
        // Rebuild macro frame list when dirty (elevation selection, bounds, or scan count changed)
        if self.state.playback_state.playback_mode() == crate::state::PlaybackMode::Macro {
            let mp = &self.state.playback_state.macro_playback;
            let elev_sel = self.state.viz_state.elevation_selection.clone();
            let bounds = self.state.playback_state.time_model.playback_bounds;
            let scan_count = self.state.radar_timeline.scans.len();

            let dirty = mp.cached_elevation_selection != elev_sel
                || mp.cached_bounds != bounds
                || mp.cached_scan_count != scan_count;

            if dirty {
                let frames = match &elev_sel {
                    crate::state::ElevationSelection::Fixed {
                        elevation_number, ..
                    } => self
                        .state
                        .radar_timeline
                        .matching_sweep_end_times_by_number(*elevation_number, bounds),
                    crate::state::ElevationSelection::Latest => {
                        self.state.radar_timeline.all_sweep_end_times(bounds)
                    }
                };
                self.state.playback_state.macro_playback.sweep_frames = frames;
                self.state
                    .playback_state
                    .macro_playback
                    .cached_elevation_selection = elev_sel;
                self.state.playback_state.macro_playback.cached_bounds = bounds;
                self.state.playback_state.macro_playback.cached_scan_count = scan_count;
                self.state.playback_state.sync_macro_frame_index();
            }

            // Detect manual seek: if playback position changed externally
            // (user clicked timeline, jog, etc.) re-sync frame index.
            let pos = self.state.playback_state.playback_position();
            let cached_pos = self
                .state
                .playback_state
                .macro_playback
                .cached_playback_position;
            if (pos - cached_pos).abs() > 0.5 {
                self.state.playback_state.sync_macro_frame_index();
                self.state.playback_state.macro_playback.frame_accumulator = 0.0;
            }
            self.state
                .playback_state
                .macro_playback
                .cached_playback_position = pos;
        }

        // Auto-load scan when scrubbing: find the most recent scan within 15 minutes.
        // In the worker architecture, this sends a render request directly —
        // the worker reads records from IDB, decodes the target elevation, and renders.
        //
        // In FixedTilt mode, we also detect intra-scan sweep changes: a scan may
        // contain multiple sweeps at the target elevation (e.g. VCP 215 has 0.5°
        // at both elevation_number 1 and 3). As playback advances past a new
        // sweep's start_time, we re-render with that sweep's elevation_number.
        // Uses module-level MAX_SCAN_AGE_SECS constant.
        {
            let playback_ts = self.state.playback_state.playback_position();

            // Skip the timeline walk when nothing that feeds the scrub
            // decision has moved since last frame. The O(scans) search
            // below used to run every frame even while paused; this lets
            // the idle case cost only a few comparisons.
            let scan_count = self.state.radar_timeline.scans.len();
            let elev_sel = &self.state.viz_state.elevation_selection;
            let displayed_ts = self.state.viz_state.displayed_scan_timestamp;
            let scrub_cache_hit = self.scrub_cache.last_playback_ts == Some(playback_ts)
                && self.scrub_cache.last_scan_count == scan_count
                && self.scrub_cache.last_displayed_scan_ts == displayed_ts
                && self
                    .scrub_cache
                    .last_elevation_selection
                    .as_ref()
                    .is_some_and(|cached| cached == elev_sel);

            if !scrub_cache_hit {
                self.scrub_cache.last_playback_ts = Some(playback_ts);
                self.scrub_cache.last_scan_count = scan_count;
                self.scrub_cache.last_displayed_scan_ts = displayed_ts;
                self.scrub_cache.last_elevation_selection = Some(elev_sel.clone());
            }

            if !scrub_cache_hit {
                // Extract scrub decision data from the immutable borrow of radar_timeline
                let scrub_action = self
                    .state
                    .radar_timeline
                    .find_recent_scan(playback_ts, MAX_SCAN_AGE_SECS)
                    .map(|scan| {
                        let scan_ts = scan.key_timestamp as i64;
                        let target_elev_num: Option<u8> =
                            match &self.state.viz_state.elevation_selection {
                                crate::state::ElevationSelection::Fixed { .. } => {
                                    self.best_elevation_at_playback(scan, playback_ts)
                                }
                                crate::state::ElevationSelection::Latest => {
                                    Some(self.most_recent_sweep_elevation(scan, playback_ts))
                                }
                            };

                        let needs_new_scan = match self.state.viz_state.displayed_scan_timestamp {
                            Some(displayed) => displayed != scan_ts,
                            None => true,
                        };
                        let needs_new_sweep = !needs_new_scan
                            && self.state.viz_state.displayed_sweep_elevation_number
                                != target_elev_num;

                        // Capture overlay data from the matching sweep (if any)
                        let sweep_overlay = target_elev_num.and_then(|num| {
                            scan.sweeps
                                .iter()
                                .find(|s| s.elevation_number == num)
                                .map(|s| (s.start_time, s.end_time, s.elevation))
                        });

                        // Extract all elevation numbers for volume rendering
                        let mut elev_nums: Vec<u8> =
                            scan.sweeps.iter().map(|s| s.elevation_number).collect();
                        elev_nums.sort_unstable();
                        elev_nums.dedup();

                        // Build elevation list for new scans
                        let new_elev_list = if needs_new_scan {
                            Some(Self::build_elevation_list(scan))
                        } else {
                            None
                        };

                        (
                            scan_ts,
                            target_elev_num,
                            needs_new_scan,
                            needs_new_sweep,
                            sweep_overlay,
                            elev_nums,
                            new_elev_list,
                        )
                    });

                if let Some((
                    scan_ts,
                    target_elev_num,
                    needs_new_scan,
                    needs_new_sweep,
                    sweep_overlay,
                    elev_nums,
                    new_elev_list,
                )) = scrub_action
                {
                    if (needs_new_scan || needs_new_sweep) && self.render.has_worker() {
                        let scan_key =
                            data::ScanKey::from_secs(&self.state.viz_state.site_id, scan_ts);
                        self.render.set_scan_key(scan_key.to_storage_key());
                        self.state.viz_state.displayed_scan_timestamp = Some(scan_ts);
                        if !elev_nums.is_empty() {
                            self.render.set_elevations(elev_nums);
                        }
                        if let Some(entries) = new_elev_list {
                            self.state.viz_state.cached_vcp_elevations = entries.clone();
                            self.state
                                .viz_state
                                .elevation_selection
                                .resolve_for_vcp(&entries);
                        }

                        match target_elev_num {
                            Some(num) => {
                                if let Some((start, end, elev)) = sweep_overlay {
                                    self.update_overlay_from_sweep(start, end, elev);
                                }
                                self.state.viz_state.displayed_sweep_elevation_number = Some(num);
                                self.render.force_fresh_render();
                                self.request_worker_render();
                                if self.state.viz_state.volume_3d_enabled {
                                    self.request_worker_render_volume();
                                }
                            }
                            None => {
                                // Scan exists but the selected fixed elevation has no sweep.
                                // Clear the stale sweep so the canvas matches the timeline.
                                self.clear_display_no_sweep();
                            }
                        }
                        // The side-effects above change displayed_scan_timestamp,
                        // so refresh the cache snapshot now to keep it in sync.
                        self.scrub_cache.last_displayed_scan_ts =
                            self.state.viz_state.displayed_scan_timestamp;
                    }
                } else if self.state.viz_state.displayed_scan_timestamp.is_some() {
                    self.clear_display_no_scan();
                }
            }
        }

        // Pre-render next sweep: when playing and near the end of the current sweep,
        // preemptively send a render request for the upcoming sweep so the result
        // is ready when the boundary is crossed, reducing perceived stutter.
        // Skip in macro mode — frame jumps are instant and the frame list handles sequencing.
        if self.state.playback_state.playing
            && self.render.has_worker()
            && self.state.playback_state.playback_mode() == crate::state::PlaybackMode::Micro
        {
            let playback_ts = self.state.playback_state.playback_position();
            let speed = self
                .state
                .playback_state
                .speed
                .timeline_seconds_per_real_second();
            let prefetch_lookahead = PREFETCH_LOOKAHEAD_SECS * speed;

            if let Some(scan) = self
                .state
                .radar_timeline
                .find_scan_at_timestamp(playback_ts)
            {
                if let Some((sweep_idx, sweep)) = scan.find_sweep_at_timestamp(playback_ts) {
                    let time_to_end = sweep.end_time - playback_ts;
                    if time_to_end > 0.0 && time_to_end < prefetch_lookahead {
                        let next_elev_num = if sweep_idx + 1 < scan.sweeps.len() {
                            Some(scan.sweeps[sweep_idx + 1].elevation_number)
                        } else {
                            let future_ts = playback_ts + prefetch_lookahead;
                            self.state
                                .radar_timeline
                                .find_scan_at_timestamp(future_ts)
                                .and_then(|next_scan| {
                                    next_scan.sweeps.first().map(|s| s.elevation_number)
                                })
                        };

                        if let Some(next_en) = next_elev_num {
                            if self.state.viz_state.displayed_sweep_elevation_number
                                != Some(next_en)
                            {
                                if let Some(scan_key) =
                                    self.render.scan_key().map(|s| s.to_string())
                                {
                                    let product =
                                        self.state.viz_state.product.to_worker_string().to_string();
                                    let prefetch_request = RenderRequest {
                                        scan_key: scan_key.clone(),
                                        elevation_number: next_en,
                                        product: product.clone(),
                                        is_auto: self.state.viz_state.elevation_selection.is_auto(),
                                    };
                                    log::debug!(
                                        "Prefetching next sweep: elev_num={} ({:.1}s ahead)",
                                        next_en,
                                        time_to_end,
                                    );
                                    self.render.set_last_render(prefetch_request);
                                    self.render.render_direct(scan_key, next_en, product);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Stateless sweep animation: ensure the previous-sweep GPU texture matches
    /// the sweep that *should* be the under-layer based on the current playback
    /// position, not based on what happened to be rendered before.
    ///
    /// The "previous sweep" is the one displayed just before the current one:
    /// within the same scan that's the preceding sweep in time order. Only look
    /// at the previous scan if the current sweep is the very first in its scan.
    fn sync_prev_sweep_texture(&mut self) {
        // In live mode, the previous sweep texture is managed by
        // promote_current_to_previous in the LiveDecoded handler —
        // don't let the timeline-based sync overwrite or clear it.
        if self.state.live_mode_state.is_active() {
            return;
        }

        if !self.state.effective_sweep_animation() {
            self.state.viz_state.prev_sweep_overlay = None;
            self.state.viz_state.prev_sweep_scan_timestamp = None;
            self.state.viz_state.prev_sweep_elevation_number = None;
            self.state.viz_state.last_sweep_line_cache = None;
            return;
        }

        let playback_ts = self.state.playback_state.playback_position();
        let displayed_elev = match self.state.viz_state.displayed_sweep_elevation_number {
            Some(e) => e,
            None => return,
        };

        let is_auto = self.state.viz_state.elevation_selection.is_auto();

        // Determine which sweep should be the previous-sweep under-layer.
        let prev_info = PlaybackManager::find_prev_sweep(
            &self.state.radar_timeline,
            playback_ts,
            displayed_elev,
            is_auto,
            MAX_SCAN_AGE_SECS,
        );

        let (prev_scan_key_ts, prev_elev_num, prev_elev_deg, prev_start, prev_end) = match prev_info
        {
            Some(info) => info,
            None => {
                self.state.viz_state.prev_sweep_overlay = None;
                self.state.viz_state.prev_sweep_scan_timestamp = None;
                self.state.viz_state.prev_sweep_elevation_number = None;
                // Clear GPU previous sweep so shader composites against black
                if let Some(ref renderer) = self.gpu.gpu {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
                return;
            }
        };

        // Store previous sweep metadata for canvas overlay and timeline highlight
        self.state.viz_state.prev_sweep_overlay = Some((prev_elev_deg, prev_start, prev_end));
        self.state.viz_state.prev_sweep_scan_timestamp = Some(prev_scan_key_ts);
        self.state.viz_state.prev_sweep_elevation_number = Some(prev_elev_num);

        let prev_scan_key =
            data::ScanKey::from_secs(&self.state.viz_state.site_id, prev_scan_key_ts)
                .to_storage_key();

        // Get current GPU prev sweep ID for comparison
        let current_gpu_prev_id = self.gpu.gpu.as_ref().and_then(|renderer| {
            renderer
                .lock()
                .ok()
                .and_then(|r| r.prev_sweep_id().map(String::from))
        });

        let product = self.state.viz_state.product.to_worker_string().to_string();
        let action = self.playback_manager.resolve_prev_sweep(
            &prev_scan_key,
            prev_elev_num,
            current_gpu_prev_id.as_deref(),
            &product,
        );

        match action {
            PrevSweepAction::AlreadyLoaded => {}
            PrevSweepAction::UploadFromCache(cache_key) => {
                // Clear stale previous sweep immediately
                if let Some(ref renderer) = self.gpu.gpu {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
                if let Some(cached) = self.playback_manager.get_cached_sweep(&cache_key) {
                    if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
                        if let Ok(mut r) = renderer.lock() {
                            r.update_previous_data(
                                gl,
                                &cached.azimuths,
                                &cached.gate_values,
                                cached.azimuth_count,
                                cached.gate_count,
                                cached.first_gate_range_km,
                                cached.gate_interval_km,
                                cached.max_range_km,
                                cached.offset,
                                cached.scale,
                                cached.azimuth_spacing_deg,
                                Some(cache_key),
                                &cached.radial_times,
                            );
                        }
                    }
                }
            }
            PrevSweepAction::FetchFromWorker {
                scan_key,
                elevation_number,
                product,
            } => {
                // Clear stale previous sweep immediately
                if let Some(ref renderer) = self.gpu.gpu {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
                self.render
                    .render_direct(scan_key, elevation_number, product);
            }
            PrevSweepAction::Clear => {
                if let Some(ref renderer) = self.gpu.gpu {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
            }
        }
    }

    /// Clear the on-canvas sweep and the overlay fields when the entire scan
    /// is gone (e.g. scrubbed off the timeline). Resets the scan key too.
    fn clear_display_no_scan(&mut self) {
        if let Some(ref renderer) = self.gpu.gpu {
            if let Ok(mut r) = renderer.lock() {
                r.clear_data();
            }
        }
        self.state.viz_state.displayed_scan_timestamp = None;
        self.state.viz_state.displayed_sweep_elevation_number = None;
        self.render.clear_scan_key();
        self.state.viz_state.data_staleness_secs = None;
        self.state.viz_state.rendered_sweep_end_secs = None;
        self.state.viz_state.timestamp = "--:--:-- UTC".to_string();
        self.state.viz_state.elevation = "-- deg".to_string();
        // clear_data() drops both GPU textures; match the prev-sweep metadata
        // so the timeline highlight and canvas overlay don't point at state
        // that no longer has backing pixels.
        self.state.viz_state.prev_sweep_scan_timestamp = None;
        self.state.viz_state.prev_sweep_elevation_number = None;
        self.state.viz_state.prev_sweep_overlay = None;
        self.scrub_cache.last_displayed_scan_ts = None;
    }

    /// Clear the on-canvas sweep when the selected (elevation, product) isn't
    /// available for the current scan, but the scan itself is still valid.
    /// Leaves the scan key intact so other elevations/products can still render.
    fn clear_display_no_sweep(&mut self) {
        if let Some(ref renderer) = self.gpu.gpu {
            if let Ok(mut r) = renderer.lock() {
                r.clear_data();
            }
        }
        self.state.viz_state.displayed_sweep_elevation_number = None;
        self.state.viz_state.data_staleness_secs = None;
        self.state.viz_state.rendered_sweep_end_secs = None;
        self.state.viz_state.timestamp = "--:--:-- UTC".to_string();
        self.state.viz_state.elevation = "-- deg".to_string();
        // clear_data() drops both GPU textures. sync_prev_sweep_texture
        // early-returns while displayed_sweep_elevation_number is None, so
        // clear prev metadata here to prevent a stale timeline/overlay hint.
        self.state.viz_state.prev_sweep_scan_timestamp = None;
        self.state.viz_state.prev_sweep_elevation_number = None;
        self.state.viz_state.prev_sweep_overlay = None;
        self.render.clear_last_render();
    }

    /// Re-render when the user changes elevation, product, or view mode.
    fn request_render_if_needed(&mut self) {
        // Live mode re-renders on the next ChunkIngested (~12s) — no IDB-based render needed.
        if self.state.live_mode_state.is_active() {
            return;
        }
        // Detect elevation/product changes and trigger worker re-render.
        // If the user changes these settings and we have a current scan, we need
        // a new render from the worker.
        if self.render.scan_key().is_some() && self.render.has_worker() {
            if self.state.viz_state.volume_3d_enabled
                && self.state.viz_state.view_mode == state::ViewMode::Globe3D
            {
                self.request_worker_render_volume();
            }
            self.request_worker_render();
        }
    }

    /// Sync network statistics from the download channel and service worker.
    fn update_network_stats(&mut self) {
        // Update session stats from live network statistics
        let network_stats = self.acquisition.download_channel.stats();
        self.state
            .session_stats
            .update_from_network_stats(&network_stats);

        // Drain service worker network metrics into app state
        if let Some(ref monitor) = self.network_monitor {
            self.state.network_aggregate = monitor.aggregate();
            let mut pending = monitor.take_pending();
            if !pending.is_empty() {
                // Correlate each new request exactly once, then append to
                // the app-level ring. The previous implementation
                // re-cloned and re-correlated the entire ring every frame
                // regardless of whether anything had changed.
                for req in pending.iter_mut() {
                    req.operation_id = self.state.acquisition.correlate_network_request(&req.url);
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
        self.persistence.persist_if_due(&self.state);
    }
}

impl eframe::App for WorkbenchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_frame_setup(ctx);
        let (dl_sel, dl_pos, pump) = self.dispatch_commands(ctx);
        self.handle_worker_results(ctx);
        self.pump_download_queue(ctx, dl_sel, dl_pos, pump);
        self.handle_streaming_results(ctx);
        self.advance_playback();
        self.sync_prev_sweep_texture();
        self.request_render_if_needed();
        self.update_network_stats();
        self.persist_url_state();

        // Compute the live radar model snapshot for this frame so all UI
        // consumers see consistent state from the same `now` timestamp.
        self.state.refresh_live_model();

        // Render UI panels in the correct order for egui layout
        // Side and top/bottom panels must be rendered before CentralPanel
        ui::render_top_bar(ctx, &mut self.state);
        ui::render_bottom_panel(ctx, &mut self.state);
        ui::render_left_panel(ctx, &mut self.state);
        ui::render_right_panel(ctx, &mut self.state);

        // Render canvas with GPU-based radar rendering
        ui::render_canvas_with_geo(ctx, &mut self.state, Some(&self.geo_layers), &self.gpu);

        // Process keyboard shortcuts
        ui::handle_shortcuts(ctx, &mut self.state);

        // Render overlays (on top of everything)
        ui::render_site_modal(ctx, &mut self.state, &mut self.site_modal_state);
        ui::render_shortcuts_help(ctx, &mut self.state);
        ui::render_wipe_modal(ctx, &mut self.state);
        ui::render_stats_modal(ctx, &mut self.state);
        ui::render_network_log(ctx, &mut self.state);
        ui::render_event_modal(ctx, &mut self.state, &mut self.event_modal_state);
    }
}
