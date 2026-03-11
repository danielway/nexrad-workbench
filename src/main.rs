#![warn(clippy::all)]

//! NEXRAD Workbench - A web-based radar data visualization tool.
//!
//! This application provides an interface for loading, viewing, and analyzing
//! NEXRAD weather radar data. It supports AWS archive browsing and realtime
//! streaming (when implemented).

mod data;
mod geo;
mod nexrad;
mod state;
mod ui;

use data::DataFacade;
use eframe::egui;
use state::AppState;

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
pub struct Renderers {
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
    /// Previous render parameters for change detection (scan_key, elev_num, product, render_mode).
    pub last_render_params: Option<(String, u8, String, crate::state::RenderMode)>,
    /// Previous volume render parameters for change detection (scan_key, product).
    pub last_volume_render_params: Option<(String, String)>,
}

/// Main application state and logic.
pub struct WorkbenchApp {
    /// Application state containing all sub-states
    state: AppState,

    /// Geographic layer data for map overlays
    geo_layers: geo::GeoLayerSet,

    /// All GPU renderers and their GL context.
    renderers: Renderers,

    /// Record-based data facade
    data_facade: DataFacade,

    /// Channel for async NEXRAD download operations
    download_channel: nexrad::DownloadChannel,

    /// Channel for async cache metadata loading
    cache_load_channel: nexrad::CacheLoadChannel,

    /// Cache for archive file listings (by site/date)
    archive_index: nexrad::ArchiveIndex,

    /// Currently loaded NEXRAD scan
    current_scan: Option<nexrad::CachedScan>,

    /// Queue of files to download for selection download feature.
    /// Each entry is (date, file_name, scan_start, scan_end).
    selection_download_queue: Vec<(chrono::NaiveDate, String, i64, i64)>,

    /// Map from scan start timestamp to computed end timestamp, populated when
    /// building the download queue so in-flight tracking can look up boundaries.
    scan_end_times: std::collections::HashMap<i64, i64>,

    /// Previous site ID to detect site changes (for clearing volume ring)
    previous_site_id: String,

    /// Channel for real-time streaming
    realtime_channel: nexrad::RealtimeChannel,

    /// Web Worker for offloading expensive NEXRAD operations.
    /// None if the worker failed to initialize.
    decode_worker: Option<nexrad::DecodeWorker>,

    /// Scan key of the currently displayed scan (data storage format "SITE|TIMESTAMP_MS").
    /// Used to send render requests to the worker.
    current_render_scan_key: Option<String>,

    /// Available elevation numbers for the current scan (from ingest).
    available_elevation_numbers: Vec<u8>,

    /// Monotonic instant of last URL push (for throttling to ~1/sec).
    last_url_push: web_time::Instant,

    /// Last-saved user preferences snapshot (for change detection).
    last_saved_preferences: state::UserPreferences,

    /// Transient state for the site selection modal.
    site_modal_state: ui::SiteModalState,

    /// Transient state for the event create/edit modal.
    event_modal_state: ui::EventModalState,
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

        log::info!(
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
                auto_position: true,
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
        let cache_load_channel = nexrad::CacheLoadChannel::new();
        let download_channel = nexrad::DownloadChannel::new();
        let realtime_channel = nexrad::RealtimeChannel::with_stats(download_channel.stats());

        // Open the record cache database
        {
            let facade = data_facade.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = facade.open().await {
                    log::error!("Failed to open record cache: {}", e);
                } else {
                    log::info!("Opened record cache database");
                }
            });
        }

        let initial_prefs = state::UserPreferences::from_app_state(&state);
        let has_preferred_site = state.preferred_site.is_some();

        // Create decode worker (offloads nexrad::load() to a Web Worker)
        let decode_worker = match nexrad::DecodeWorker::new(cc.egui_ctx.clone()) {
            Ok(w) => {
                log::info!("Decode worker created successfully");
                Some(w)
            }
            Err(e) => {
                log::warn!("Failed to create decode worker, using main thread: {}", e);
                None
            }
        };

        // Create GPU renderer for radar visualization
        let gpu_renderer_gl = cc.gl.clone();
        let gpu_renderer = cc
            .gl
            .as_ref()
            .and_then(|gl| match nexrad::RadarGpuRenderer::new(gl) {
                Ok(renderer) => {
                    log::info!("GPU radar renderer created");
                    Some(std::sync::Arc::new(std::sync::Mutex::new(renderer)))
                }
                Err(e) => {
                    log::error!("Failed to create GPU radar renderer: {}", e);
                    None
                }
            });

        // Create globe and geo-line renderers for 3D mode
        let globe_renderer = cc.gl.as_ref().map(|gl| {
            let r = geo::GlobeRenderer::new(gl);
            log::info!("Globe renderer created");
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
            log::info!("Geo line renderer created");
            std::sync::Arc::new(std::sync::Mutex::new(r))
        });
        let globe_radar_renderer = cc.gl.as_ref().map(|gl| {
            let r = nexrad::GlobeRadarRenderer::new(gl);
            log::info!("Globe radar renderer created");
            std::sync::Arc::new(std::sync::Mutex::new(r))
        });
        let volume_ray_renderer = cc.gl.as_ref().map(|gl| {
            let r = nexrad::VolumeRayRenderer::new(gl);
            log::info!("Volume ray renderer created");
            std::sync::Arc::new(std::sync::Mutex::new(r))
        });

        Self {
            state,
            geo_layers,
            renderers: Renderers {
                gpu: gpu_renderer,
                gl: gpu_renderer_gl,
                globe: globe_renderer,
                geo_line: geo_line_renderer,
                globe_radar: globe_radar_renderer,
                volume_ray: volume_ray_renderer,
                last_render_params: None,
                last_volume_render_params: None,
            },
            data_facade,
            download_channel,
            cache_load_channel,
            archive_index: nexrad::ArchiveIndex::new(),
            current_scan: None,
            selection_download_queue: Vec::new(),
            scan_end_times: std::collections::HashMap::new(),
            previous_site_id: initial_site_id,
            realtime_channel,
            decode_worker,
            current_render_scan_key: None,
            available_elevation_numbers: Vec::new(),
            last_url_push: web_time::Instant::now(),
            last_saved_preferences: initial_prefs,
            site_modal_state: {
                let mut sms = ui::SiteModalState::default();
                // If the user already has a preferred site, they're not a first-time
                // visitor, so the modal should open directly to the site list.
                if has_preferred_site {
                    sms.is_first_visit = false;
                    sms.mode = ui::SiteModalMode::SiteList;
                }
                sms
            },
            event_modal_state: ui::EventModalState::default(),
        }
    }

    /// Process selection download: download scans in the selected time range serially.
    ///
    /// `download_type` is `None` when pumping the existing queue (no new command),
    /// `Some(true)` for a position-download, or `Some(false)` for a range-selection download.
    fn process_selection_download(&mut self, ctx: &egui::Context, download_type: Option<bool>) {
        let site_id = self.state.viz_state.site_id.clone();

        // If we have items in the queue, try to download the next one
        if !self.selection_download_queue.is_empty() {
            // Check if current download is still in progress
            let (_, _, timestamp, _) = &self.selection_download_queue[0];
            if self
                .download_channel
                .is_download_pending(&site_id, *timestamp)
            {
                // Still downloading, wait
                return;
            }

            // Previous download finished, remove it from queue
            let _ = self.selection_download_queue.remove(0);

            // Start the next download if there are more items
            if !self.selection_download_queue.is_empty() {
                let (next_date, next_name, next_ts, next_end) = &self.selection_download_queue[0];
                self.state.status_message = format!(
                    "Downloading {} ({} remaining)",
                    next_name,
                    self.selection_download_queue.len()
                );
                // Update download progress for next file
                self.state.download_progress.active_scan = Some((*next_ts, *next_end));
                self.state.download_progress.phase = crate::state::DownloadPhase::Downloading;
                self.state.download_progress.batch_completed += 1;
                self.download_channel.download_file(
                    ctx.clone(),
                    site_id.clone(),
                    *next_date,
                    next_name.clone(),
                    *next_ts,
                    self.data_facade.clone(),
                );
            } else {
                // Download queue drained, but in-flight processing may continue.
                self.state.download_selection_in_progress = false;
                self.state.download_progress.pending_scans.clear();
                self.state.download_progress.active_scan = None;
                self.state.download_progress.phase = crate::state::DownloadPhase::Done;
                // Full clear only if no in-flight scans remain.
                if self.state.download_progress.in_flight_scans.is_empty() {
                    self.state.download_progress.clear();
                }
                self.state.status_message = "Selection download complete".to_string();
                log::info!("Selection download complete");
            }
            return;
        }

        // No queue — check if a new download command was issued
        let is_position_download = match download_type {
            Some(true) => true,
            Some(false) => false,
            None => return, // Just pumping queue, nothing to build
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

        log::info!(
            "Building download queue for selection: {} to {} ({} to {})",
            sel_start_i64,
            sel_end_i64,
            start_date,
            end_date
        );

        // Collect all files whose scan boundaries intersect the selection
        let mut files_to_download = Vec::new();
        let mut current_date = start_date;

        while current_date <= end_date {
            if let Some(listing) = self.archive_index.get(&site_id, &current_date) {
                if is_position_download {
                    // Single-position: find the exact scan containing the playback position
                    if let Some((file, boundary)) = listing.find_scan_containing(sel_start_i64) {
                        let is_cached = self
                            .state
                            .radar_timeline
                            .scans
                            .iter()
                            .any(|s| (s.start_time as i64 - file.timestamp).abs() < 60);
                        if !is_cached {
                            files_to_download.push((
                                current_date,
                                file.name.clone(),
                                boundary.start,
                                boundary.end,
                            ));
                        }
                    } else {
                        // No scan covers this time in the cached listing.
                        // The listing may be stale (e.g. archives created after
                        // it was cached), so invalidate and re-fetch.
                        log::info!(
                            "No scan at {} in cached listing for {}/{}; re-fetching",
                            sel_start_i64,
                            site_id,
                            current_date
                        );
                        self.archive_index.remove(&site_id, &current_date);
                        if !self
                            .download_channel
                            .is_listing_pending(&site_id, &current_date)
                        {
                            self.download_channel.fetch_listing(
                                ctx.clone(),
                                site_id.clone(),
                                current_date,
                            );
                        }
                        self.state
                            .push_command(state::AppCommand::DownloadAtPosition);
                        self.state.status_message =
                            format!("Re-fetching archive listing for {}...", current_date);
                        return;
                    }
                } else {
                    // Range selection: find all scans that intersect [sel_start, sel_end]
                    for (file, boundary) in listing.scans_intersecting(sel_start_i64, sel_end_i64) {
                        let is_cached = self
                            .state
                            .radar_timeline
                            .scans
                            .iter()
                            .any(|s| (s.start_time as i64 - file.timestamp).abs() < 60);
                        if !is_cached {
                            files_to_download.push((
                                current_date,
                                file.name.clone(),
                                boundary.start,
                                boundary.end,
                            ));
                        }
                    }
                }
            } else {
                // Need to fetch the listing first
                if !self
                    .download_channel
                    .is_listing_pending(&site_id, &current_date)
                {
                    log::info!("Fetching listing for {}/{}", site_id, current_date);
                    self.download_channel
                        .fetch_listing(ctx.clone(), site_id.clone(), current_date);
                }
                // Re-trigger once listing arrives — preserve the download type
                if is_position_download {
                    self.state
                        .push_command(state::AppCommand::DownloadAtPosition);
                } else {
                    self.state
                        .push_command(state::AppCommand::DownloadSelection);
                }
                self.state.status_message =
                    format!("Fetching archive listing for {}...", current_date);
                return;
            }

            current_date += chrono::Duration::days(1);
        }

        if files_to_download.is_empty() {
            self.state.status_message = "No new scans to download in selection".to_string();
            log::info!("No new scans to download in selection");
            return;
        }

        // Sort by start timestamp
        files_to_download.sort_by_key(|(_, _, start, _)| *start);

        log::info!(
            "Queued {} files for download in selection",
            files_to_download.len()
        );

        // Start downloading
        self.state.download_selection_in_progress = true;
        self.selection_download_queue = files_to_download;

        // Build scan_end_times lookup for in-flight tracking
        self.scan_end_times.clear();
        for (_, _, start, end) in &self.selection_download_queue {
            self.scan_end_times.insert(*start, *end);
        }

        // Populate download progress for timeline ghosts and pipeline display
        {
            let progress = &mut self.state.download_progress;
            progress.pending_scans = self
                .selection_download_queue
                .iter()
                .map(|(_, _, start, end)| (*start, *end))
                .collect();
            progress.batch_total = self.selection_download_queue.len() as u32;
            progress.batch_completed = 0;
            progress.phase = crate::state::DownloadPhase::Downloading;
            let first = &self.selection_download_queue[0];
            progress.active_scan = Some((first.2, first.3));
        }

        // Kick off first download
        let (date, file_name, timestamp, _) = &self.selection_download_queue[0];
        self.state.status_message = format!(
            "Downloading {} ({} total)",
            file_name,
            self.selection_download_queue.len()
        );
        self.download_channel.download_file(
            ctx.clone(),
            site_id,
            *date,
            file_name.clone(),
            *timestamp,
            self.data_facade.clone(),
        );
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

        // Start the realtime channel with DataFacade for record storage
        self.realtime_channel
            .start(ctx.clone(), site_id, self.data_facade.clone());
    }

    /// Fetch the latest available archive scan for the current site.
    ///
    /// Fetches today's (and yesterday's) archive listing to find the most recent
    /// scan, then downloads it. This gives users immediate data after site selection
    /// without starting real-time streaming.
    fn fetch_latest_scan(&mut self, ctx: &egui::Context) {
        let site_id = self.state.viz_state.site_id.clone();
        log::info!("Fetching latest scan for site: {}", site_id);

        self.state.status_message = "Fetching latest data...".to_string();

        // Position playback at current time so DownloadAtPosition finds the latest scan
        let now = js_sys::Date::now() / 1000.0;
        self.state.playback_state.set_playback_position(now);
        self.state.playback_state.center_view_on(now);

        // Fetch listings for today and yesterday (in case we're near midnight UTC
        // or today has no data yet). DownloadAtPosition will fire once the listing
        // arrives and pick the scan closest to the current playback position.
        let today = chrono::Utc::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);

        // Fetch yesterday's listing first (fallback)
        if self.archive_index.get(&site_id, &yesterday).is_none()
            && !self
                .download_channel
                .is_listing_pending(&site_id, &yesterday)
        {
            self.download_channel
                .fetch_listing(ctx.clone(), site_id.clone(), yesterday);
        }

        // Fetch today's listing
        if self.archive_index.get(&site_id, &today).is_none()
            && !self.download_channel.is_listing_pending(&site_id, &today)
        {
            self.download_channel
                .fetch_listing(ctx.clone(), site_id.clone(), today);
        }

        // Queue a DownloadAtPosition to fire once listings are available.
        self.state
            .push_command(state::AppCommand::DownloadAtPosition);
    }

    /// Find the best elevation number for the current target_elevation.
    ///
    /// If sweep metadata with angles is available, picks the number whose angle
    /// is closest to target_elevation. Otherwise falls back to the lowest available number.
    fn best_elevation_number(&self) -> u8 {
        // First try to match by angle using timeline sweep metadata
        let target = self.state.viz_state.target_elevation;
        if let Some(scan) = self
            .state
            .radar_timeline
            .find_recent_scan(self.state.playback_state.playback_position(), 15.0 * 60.0)
        {
            if !scan.sweeps.is_empty() {
                // Find sweep whose angle is closest to target
                if let Some(best) = scan.sweeps.iter().min_by(|a, b| {
                    (a.elevation - target)
                        .abs()
                        .partial_cmp(&(b.elevation - target).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    return best.elevation_number;
                }
            }
        }

        // Fallback: use lowest available elevation number
        self.available_elevation_numbers
            .first()
            .copied()
            .unwrap_or(1)
    }

    /// Pick the closest available elevation to the requested one.
    fn best_available_elevation(&self, requested: u8) -> u8 {
        self.available_elevation_numbers
            .iter()
            .copied()
            .min_by_key(|&e| (e as i16 - requested as i16).unsigned_abs())
            .unwrap_or(requested)
    }

    /// Find the best elevation number for a scan given the playback position.
    ///
    /// In FixedTilt mode, finds the most recent sweep at the target elevation
    /// whose start_time <= playback_ts. A scan may contain multiple sweeps at the
    /// same elevation (e.g. VCP 215 has 0.5° at elevation_number 1 and 3).
    fn best_elevation_at_playback(
        &self,
        scan: &crate::state::radar_data::Scan,
        playback_ts: f64,
    ) -> u8 {
        let target = self.state.viz_state.target_elevation;

        // Filter sweeps matching target elevation (within 0.15° tolerance)
        // then filter to those that have started (start_time <= playback_ts)
        // pick the one with the latest start_time (most recent instance)
        let matching = scan
            .sweeps
            .iter()
            .filter(|s| (s.elevation - target).abs() < 0.15)
            .filter(|s| s.start_time <= playback_ts)
            .max_by(|a, b| {
                a.start_time
                    .partial_cmp(&b.start_time)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        if let Some(sweep) = matching {
            return sweep.elevation_number;
        }

        // No matching sweep started yet — fall back to best_elevation_number()
        self.best_elevation_number()
    }

    /// Find the most recent sweep (any elevation) at or before the playback position.
    ///
    /// Used by MostRecent render mode to always show the latest available data
    /// regardless of elevation.
    fn most_recent_sweep_elevation(
        &self,
        scan: &crate::state::radar_data::Scan,
        playback_ts: f64,
    ) -> u8 {
        scan.sweeps
            .iter()
            .filter(|s| s.start_time <= playback_ts)
            .max_by(|a, b| {
                a.start_time
                    .partial_cmp(&b.start_time)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|s| s.elevation_number)
            .unwrap_or_else(|| self.best_elevation_number())
    }

    /// Update the canvas overlay text with sweep timing and elevation info.
    fn update_overlay_from_sweep(&mut self, start: f64, end: f64, elevation_deg: f32) {
        self.state.viz_state.elevation = format!("{:.1}\u{00B0}", elevation_deg);

        // Format midpoint timestamp with full date and time
        let mid_ms = ((start + end) / 2.0) * 1000.0;
        let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(mid_ms));
        if self.state.use_local_time {
            self.state.viz_state.timestamp = format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                date.get_full_year(),
                date.get_month() + 1, // JS months are 0-indexed
                date.get_date(),
                date.get_hours(),
                date.get_minutes(),
                date.get_seconds()
            );
        } else {
            self.state.viz_state.timestamp = format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
                date.get_utc_full_year(),
                date.get_utc_month() + 1, // JS months are 0-indexed
                date.get_utc_date(),
                date.get_utc_hours(),
                date.get_utc_minutes(),
                date.get_utc_seconds()
            );
        }

        // Store sweep end time so staleness can be recomputed each frame
        self.state.viz_state.rendered_sweep_end_secs = Some(end);
        // Staleness is recomputed per-frame in update(); seed it here for immediate display
        let now = js_sys::Date::now() / 1000.0;
        let staleness = now - end;
        self.state.viz_state.data_staleness_secs = if staleness >= 0.0 {
            Some(staleness)
        } else {
            None
        };
    }

    /// Send a decode request to the worker for the current scan + settings.
    /// Estimate the actual volume start time by back-calculating from a chunk's
    /// data time and which elevation it contains.
    ///
    /// If we join mid-volume at elevation N, the data time represents the time
    /// of that elevation's radials, not the volume start. We subtract the
    /// estimated time for the preceding N-1 elevations.
    fn estimate_volume_start(
        chunk_data_time: f64,
        current_elevation: Option<u8>,
        expected_elevation_count: Option<u8>,
        last_volume_duration_secs: Option<f64>,
    ) -> f64 {
        let elev = match current_elevation {
            Some(e) if e > 1 => e,
            _ => return chunk_data_time, // Elevation 1 or unknown — data IS the start
        };
        let count = expected_elevation_count.unwrap_or(0) as f64;
        let dur = last_volume_duration_secs.unwrap_or(300.0);
        if count <= 0.0 || dur <= 0.0 {
            return chunk_data_time;
        }
        let sweep_dur = dur / count;
        // Elevation numbers are 1-based; subtract time for preceding elevations
        chunk_data_time - (elev as f64 - 1.0) * sweep_dur
    }

    /// Send a render request to the worker for the current scan/elevation/product.
    ///
    /// Skips the request if the parameters haven't changed since the last render.
    fn request_worker_render(&mut self) {
        let Some(ref scan_key) = self.current_render_scan_key else {
            return;
        };
        if self.decode_worker.is_none() {
            return;
        }

        let mut elevation_number = self
            .state
            .displayed_sweep_elevation_number
            .unwrap_or_else(|| self.best_elevation_number());

        // During real-time streaming, best_elevation_number() may return an
        // elevation from a previous completed scan that doesn't exist yet in the
        // in-progress scan. Constrain to what's actually available.
        if self.state.live_mode_state.is_active()
            && !self.available_elevation_numbers.is_empty()
            && !self.available_elevation_numbers.contains(&elevation_number)
        {
            elevation_number = self.best_available_elevation(elevation_number);
        }
        let product = self.state.viz_state.product.to_worker_string().to_string();

        let params = (
            scan_key.clone(),
            elevation_number,
            product.clone(),
            self.state.viz_state.render_mode,
        );

        // Skip if same as last request
        if self.renderers.last_render_params.as_ref() == Some(&params) {
            return;
        }

        log::info!(
            "Requesting worker decode: {} elev={} product={}",
            scan_key,
            elevation_number,
            product,
        );

        let scan_key = scan_key.clone();
        self.renderers.last_render_params = Some(params);
        if !self.state.session_stats.pipeline.processing {
            self.state.session_stats.pipeline.processing = true;
        }
        self.decode_worker
            .as_mut()
            .unwrap()
            .render(scan_key, elevation_number, product);
    }

    /// Request volume render (all elevations for ray marching).
    fn request_worker_render_volume(&mut self) {
        let Some(ref scan_key) = self.current_render_scan_key else {
            log::debug!("Volume render skipped: no scan key");
            return;
        };
        if self.decode_worker.is_none() {
            log::debug!("Volume render skipped: no worker");
            return;
        }
        if self.available_elevation_numbers.is_empty() {
            log::warn!("Volume render skipped: no elevation numbers available");
            return;
        }

        let product = self.state.viz_state.product.to_worker_string().to_string();
        let params = (scan_key.clone(), product.clone());

        if self.renderers.last_volume_render_params.as_ref() == Some(&params) {
            return; // Already requested with same params
        }

        log::info!(
            "Requesting volume render: {} product={} elevations={:?}",
            scan_key,
            product,
            self.available_elevation_numbers,
        );

        let scan_key = scan_key.clone();
        let elev_nums = self.available_elevation_numbers.clone();
        self.renderers.last_volume_render_params = Some(params);

        self.decode_worker
            .as_mut()
            .unwrap()
            .render_volume(scan_key, product, elev_nums);
    }

    /// Stop live mode streaming.
    #[allow(dead_code)] // Called from UI when user stops live mode
    fn stop_live_mode(&mut self, reason: state::LiveExitReason) {
        log::info!("Stopping live mode: {:?}", reason);

        self.state.live_mode_state.stop(reason);
        self.state.playback_state.time_model.disable_realtime_lock();
        self.realtime_channel.stop();

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
                log::info!("Realtime streaming started for site: {}", site_id);
                self.state.live_mode_state.handle_streaming_started(now);
                self.state.status_message = format!("Live: connected to {}", site_id);
            }
            nexrad::RealtimeResult::ChunkReceived {
                chunks_in_volume,
                time_until_next,
                is_volume_end,
                fetch_latency_ms,
            } => {
                self.state
                    .session_stats
                    .record_fetch_latency(fetch_latency_ms);
                log::info!(
                    "Realtime status: chunks_in_volume={} is_end={} latency={:.0}ms next_in={:?}",
                    chunks_in_volume,
                    is_volume_end,
                    fetch_latency_ms,
                    time_until_next,
                );
                self.state.live_mode_state.handle_realtime_chunk(
                    chunks_in_volume,
                    time_until_next,
                    is_volume_end,
                    now,
                );
            }
            nexrad::RealtimeResult::ChunkData {
                data,
                chunk_index,
                is_start,
                is_end,
                timestamp,
            } => {
                log::info!(
                    "Realtime chunk received: index={} is_start={} is_end={} size={} bytes ts={}",
                    chunk_index,
                    is_start,
                    is_end,
                    data.len(),
                    timestamp,
                );

                if is_start {
                    self.state.status_message = "Live: receiving new volume...".to_string();
                    log::info!("Realtime: new volume started, forwarding start chunk to worker");
                }

                // Forward chunk to worker for incremental ingest
                let site_id = self.state.viz_state.site_id.clone();
                let file_name = format!("live_{}_{}.nexrad", site_id, timestamp);
                if let Some(ref mut worker) = self.decode_worker {
                    if is_start {
                        self.state.session_stats.pipeline.processing = true;
                    }
                    log::info!(
                        "Realtime: forwarding chunk {} to worker for ingest (site={}, ts={})",
                        chunk_index,
                        site_id,
                        timestamp
                    );
                    worker.ingest_chunk(
                        data,
                        site_id,
                        timestamp,
                        chunk_index,
                        is_start,
                        is_end,
                        file_name,
                    );
                }
            }
            nexrad::RealtimeResult::Error(msg) => {
                log::error!("Realtime streaming error: {}", msg);
                self.state.live_mode_state.set_error(msg.clone());
                self.state.status_message = format!("Live error: {}", msg);
            }
        }
    }
}

impl eframe::App for WorkbenchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
        if let Some(sweep_end) = self.state.viz_state.rendered_sweep_end_secs {
            let now = js_sys::Date::now() / 1000.0;
            let staleness = now - sweep_end;
            self.state.viz_state.data_staleness_secs = if staleness >= 0.0 {
                Some(staleness)
            } else {
                None
            };
        }

        // Run storm cell detection on demand when toggled on with existing data
        if self.state.storm_cells_visible && self.state.detected_storm_cells.is_empty() {
            if let Some(ref renderer) = self.renderers.gpu {
                if let Ok(r) = renderer.lock() {
                    if r.has_data() {
                        self.state.detected_storm_cells = r.detect_storm_cells(
                            self.state.viz_state.center_lat,
                            self.state.viz_state.center_lon,
                            self.state.storm_cell_threshold_dbz,
                        );
                    }
                }
            }
        }
        // Clear cached cells when toggle is off
        if !self.state.storm_cells_visible && !self.state.detected_storm_cells.is_empty() {
            self.state.detected_storm_cells.clear();
        }

        // Detect site changes and clear volume ring
        if self.state.viz_state.site_id != self.previous_site_id {
            log::info!(
                "Site changed from {} to {}",
                self.previous_site_id,
                self.state.viz_state.site_id
            );
            if let Some(ref renderer) = self.renderers.gpu {
                if let Ok(mut r) = renderer.lock() {
                    r.clear_data();
                }
            }
            self.state.displayed_scan_timestamp = None;
            self.state.displayed_sweep_elevation_number = None;
            self.previous_site_id = self.state.viz_state.site_id.clone();
            // Clear shadow boundaries from previous site; new listings will repopulate.
            self.state.shadow_scan_boundaries.clear();
        }

        // Drain and dispatch commands from the queue.
        let commands = self.state.drain_commands();
        let mut do_download_selection = false;
        let mut do_download_at_position = false;
        for cmd in commands {
            match cmd {
                state::AppCommand::ClearCache => {
                    if !self.cache_load_channel.is_loading() {
                        self.cache_load_channel
                            .clear_cache(ctx.clone(), self.data_facade.clone());
                    } else {
                        // Re-enqueue if channel is busy
                        self.state.push_command(state::AppCommand::ClearCache);
                    }
                }
                state::AppCommand::WipeAll => {
                    let facade = self.data_facade.clone();
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
                    if !self.cache_load_channel.is_loading() {
                        self.cache_load_channel.load_site_timeline(
                            ctx.clone(),
                            self.data_facade.clone(),
                            self.state.viz_state.site_id.clone(),
                        );
                    } else {
                        self.state.push_command(state::AppCommand::RefreshTimeline {
                            auto_position: false,
                        });
                    }
                }
                state::AppCommand::CheckEviction => {
                    let facade = self.data_facade.clone();
                    let quota = self.state.storage_settings.quota_bytes;
                    let target = self.state.storage_settings.eviction_target_bytes;
                    let ctx_clone = ctx.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match facade.check_and_evict(quota, target).await {
                            Ok((evicted, count)) => {
                                if evicted {
                                    log::info!("Eviction complete: removed {} scans", count);
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
                state::AppCommand::FetchLatest => {
                    self.fetch_latest_scan(ctx);
                }
                state::AppCommand::DownloadSelection => {
                    do_download_selection = true;
                }
                state::AppCommand::DownloadAtPosition => {
                    do_download_at_position = true;
                }
            }
        }

        // Check for completed cache load operations
        if let Some(result) = self.cache_load_channel.try_recv() {
            match result {
                nexrad::CacheLoadResult::Success {
                    site_id,
                    metadata,
                    total_cache_size,
                } => {
                    log::info!(
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

                        log::info!("Timeline has {} contiguous range(s)", ranges.len());
                    }
                }
                nexrad::CacheLoadResult::Error(msg) => {
                    log::error!("Cache load failed: {}", msg);
                }
            }
        }

        // Check for completed Web Worker operations
        if let Some(ref mut worker) = self.decode_worker {
            for outcome in worker.try_recv() {
                match outcome {
                    nexrad::WorkerOutcome::Ingested(result) => {
                        // Processing stays active through decode — don't mark done yet.
                        // Transition to decoding phase. Don't remove the ghost
                        // yet — it stays visible until the timeline refreshes
                        // and a real scan block replaces it (the ghost renderer's
                        // overlap check handles the visual transition).
                        self.state.download_progress.phase = crate::state::DownloadPhase::Decoding;
                        log::info!(
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
                        self.state.session_stats.last_ingest_detail =
                            Some(crate::state::IngestTimingDetail {
                                split_ms: result.split_ms,
                                decompress_ms: result.decompress_ms,
                                decode_ms: result.decode_ms,
                                extract_ms: result.extract_ms,
                                store_ms: result.store_ms,
                                index_ms: result.index_ms,
                            });

                        // Track the scan for render requests
                        self.current_render_scan_key = Some(result.scan_key.clone());
                        self.available_elevation_numbers = result.elevation_numbers;
                        self.state.displayed_scan_timestamp = Some(result.context.timestamp_secs);
                        self.state.displayed_sweep_elevation_number = None;
                        // Refresh timeline to include the new scan (sweeps
                        // were persisted to IDB during ingest and will be
                        // loaded by from_metadata on the next refresh).
                        self.state.push_command(state::AppCommand::RefreshTimeline {
                            auto_position: false,
                        });

                        // Request eviction check
                        self.state.push_command(state::AppCommand::CheckEviction);

                        // Clear last render params to force a fresh render
                        self.renderers.last_render_params = None;
                        self.renderers.last_volume_render_params = None;

                        // Trigger render for the ingested scan
                        self.request_worker_render();
                        if self.state.viz_state.volume_3d_enabled {
                            self.request_worker_render_volume();
                        }
                    }
                    nexrad::WorkerOutcome::ChunkIngested(result) => {
                        log::info!(
                            "Chunk ingested: scan={} elevations_completed={:?} sweeps_stored={} is_end={} vcp={:?} available_elevs={:?} {:.1}ms",
                            result.scan_key,
                            result.elevations_completed,
                            result.sweeps_stored,
                            result.is_end,
                            result.vcp.as_ref().map(|v| v.number),
                            self.available_elevation_numbers,
                            result.total_ms,
                        );

                        // Update scan key and available elevations
                        self.current_render_scan_key = Some(result.scan_key.clone());
                        self.state.live_mode_state.current_scan_key = Some(result.scan_key.clone());
                        let had_elevations = !self.available_elevation_numbers.is_empty();
                        for elev in &result.elevations_completed {
                            if !self.available_elevation_numbers.contains(elev) {
                                self.available_elevation_numbers.push(*elev);
                                self.available_elevation_numbers.sort_unstable();
                            }
                        }

                        // Update displayed timestamp
                        self.state.displayed_scan_timestamp = Some(result.context.timestamp_secs);

                        // Record per-elevation chunk time spans for timeline visualization
                        if !result.chunk_elev_spans.is_empty() {
                            self.state
                                .live_mode_state
                                .record_chunk_elev_spans(&result.chunk_elev_spans);
                        }

                        // Update live mode partial scan tracking — always set volume
                        // start on first chunk so the timeline block appears immediately,
                        // even before a full sweep/elevation completes.
                        //
                        // Set the volume start time. Prefer the authoritative volume
                        // header time from the Archive II header. Fall back to
                        // back-calculating from chunk data time + elevation number.
                        if self.state.live_mode_state.current_volume_start.is_none() {
                            let vol_start =
                                if let Some(header_time) = result.volume_header_time_secs {
                                    header_time
                                } else {
                                    let chunk_data_time = result
                                        .chunk_min_time_secs
                                        .unwrap_or(result.context.timestamp_secs as f64);
                                    Self::estimate_volume_start(
                                        chunk_data_time,
                                        result.current_elevation,
                                        self.state.live_mode_state.expected_elevation_count,
                                        self.state.live_mode_state.last_volume_duration_secs,
                                    )
                                };
                            self.state.live_mode_state.current_volume_start = Some(vol_start);
                        } else if let Some(header_time) = result.volume_header_time_secs {
                            // If we already have a volume start but now get the
                            // authoritative header time, prefer the header.
                            self.state.live_mode_state.current_volume_start = Some(header_time);
                        }
                        if !result.elevations_completed.is_empty() {
                            // Use the already-estimated volume start for consistency
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
                            self.state
                                .live_mode_state
                                .record_vcp(vcp.number, vcp.elevations.len() as u8);
                        }

                        // Track in-progress elevation for partial sweep visualization
                        self.state.live_mode_state.record_in_progress_elevation(
                            result.current_elevation,
                            result.current_elevation_radials,
                        );

                        // Store actual sweep timing metadata for accurate timeline positioning
                        if !result.sweeps.is_empty() {
                            self.state
                                .live_mode_state
                                .update_sweep_metas(result.sweeps.clone());
                        }

                        // Track last radial position for sweep line extrapolation
                        self.state.live_mode_state.record_last_radial(
                            result.last_radial_azimuth,
                            result.last_radial_time_secs,
                        );

                        // Refresh timeline when new elevations are written to cache
                        if !result.elevations_completed.is_empty() {
                            log::info!(
                                "Realtime: {} new elevation(s) cached, refreshing timeline (total available: {:?})",
                                result.elevations_completed.len(),
                                self.available_elevation_numbers,
                            );
                            self.state.push_command(state::AppCommand::RefreshTimeline {
                                auto_position: false,
                            });

                            // Update status to show progress
                            self.state.status_message = format!(
                                "Live: {} elevation(s) cached",
                                self.available_elevation_numbers.len()
                            );
                        }

                        if result.is_end {
                            // Volume complete: trigger render, check eviction
                            let now = js_sys::Date::now() / 1000.0;
                            self.state.live_mode_state.handle_volume_complete(now);
                            log::info!(
                                "Realtime: volume complete — {} elevations, triggering render",
                                self.available_elevation_numbers.len()
                            );
                            self.state.status_message = format!(
                                "Live: volume complete ({} elevations)",
                                self.available_elevation_numbers.len()
                            );
                            self.state.push_command(state::AppCommand::RefreshTimeline {
                                auto_position: false,
                            });
                            self.state.push_command(state::AppCommand::CheckEviction);
                            self.state.session_stats.pipeline.mark_processing_done();

                            // Clear last render params to force a fresh render
                            self.state.displayed_sweep_elevation_number = None;
                            self.renderers.last_render_params = None;
                            self.renderers.last_volume_render_params = None;
                            self.request_worker_render();
                            if self.state.viz_state.volume_3d_enabled {
                                self.request_worker_render_volume();
                            }
                        } else if !had_elevations && !self.available_elevation_numbers.is_empty() {
                            // First elevation arrived — we can render something now
                            log::info!(
                                "Realtime: first elevation available, triggering initial render"
                            );
                            self.renderers.last_render_params = None;
                            self.renderers.last_volume_render_params = None;
                            self.request_worker_render();
                            if self.state.viz_state.volume_3d_enabled {
                                self.request_worker_render_volume();
                            }
                        }
                    }
                    nexrad::WorkerOutcome::Decoded(result) => {
                        // Processing complete → transition to rendering.
                        self.state.session_stats.pipeline.mark_processing_done();
                        self.state.session_stats.pipeline.rendering = true;

                        log::info!(
                            "Decode complete: {}x{} (az x gates), {} radials, product={}, {:.0}ms",
                            result.azimuth_count,
                            result.gate_count,
                            result.radial_count,
                            result.product,
                            result.total_ms,
                        );

                        self.state.session_stats.record_render_time(result.total_ms);

                        // Upload decoded data to GPU renderer
                        let t_gpu = web_time::Instant::now();
                        if let (Some(ref renderer), Some(ref gl)) =
                            (&self.renderers.gpu, &self.renderers.gl)
                        {
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
                                );
                                r.update_color_table(gl, &result.product);

                                // Run storm cell detection if enabled
                                if self.state.storm_cells_visible {
                                    self.state.detected_storm_cells = r.detect_storm_cells(
                                        self.state.viz_state.center_lat,
                                        self.state.viz_state.center_lon,
                                        self.state.storm_cell_threshold_dbz,
                                    );
                                }
                            }
                        }
                        let gpu_upload_ms = t_gpu.elapsed().as_secs_f64() * 1000.0;

                        // Store detailed render timing for the detail modal.
                        self.state.session_stats.last_render_detail =
                            Some(crate::state::RenderTimingDetail {
                                fetch_ms: result.fetch_ms,
                                deser_ms: result.deser_ms,
                                marshal_ms: result.marshal_ms,
                                gpu_upload_ms,
                            });

                        // GPU upload complete.
                        self.state.session_stats.pipeline.mark_render_done();

                        // Remove this scan from in-flight ghost tracking.
                        if let Some(displayed_ts) = self.state.displayed_scan_timestamp {
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
                    nexrad::WorkerOutcome::VolumeDecoded(volume_data) => {
                        log::info!(
                            "Volume decode complete: {} sweeps, {:.1}KB, product={}, {:.0}ms",
                            volume_data.sweeps.len(),
                            volume_data.buffer.len() as f64 / 1024.0,
                            volume_data.product,
                            volume_data.total_ms,
                        );

                        // Upload to volume ray renderer
                        if let (Some(ref renderer), Some(ref gl)) =
                            (&self.renderers.volume_ray, &self.renderers.gl)
                        {
                            if let Ok(mut r) = renderer.lock() {
                                r.update_volume(
                                    gl,
                                    &volume_data.buffer,
                                    &volume_data.sweeps,
                                    self.state.viz_state.center_lat,
                                    self.state.viz_state.center_lon,
                                );
                            }
                        }

                        // Update LUT for the volume product
                        if let (Some(ref renderer), Some(ref gl)) =
                            (&self.renderers.gpu, &self.renderers.gl)
                        {
                            if let Ok(mut r) = renderer.lock() {
                                r.update_color_table(gl, &volume_data.product);
                            }
                        }
                    }
                    nexrad::WorkerOutcome::WorkerError { id, message } => {
                        log::error!("Worker error (request {}): {}", id, message);
                        self.state.status_message = format!("Worker error: {}", message);

                        // Clean up ghost and progress for the failed scan.
                        if let Some(displayed_ts) = self.state.displayed_scan_timestamp {
                            self.state
                                .download_progress
                                .in_flight_scans
                                .retain(|&(start, _)| start != displayed_ts);
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
                }
            }
        }

        // Check for completed NEXRAD download operations
        if let Some(result) = self.download_channel.try_recv() {
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
                    .scan_end_times
                    .get(&scan_ts)
                    .copied()
                    .unwrap_or(scan_ts + 300);
                self.state
                    .download_progress
                    .in_flight_scans
                    .push((scan_ts, scan_end));

                // Track which scan is being processed so error cleanup
                // can remove the correct ghost.
                self.state.displayed_scan_timestamp = Some(scan_ts);

                if is_cache_hit {
                    self.state.status_message = format!("Loaded from cache: {}", scan.file_name);

                    // Cache hit: skip ingest, go straight to decode.
                    // Ghost stays until timeline refresh shows the real scan.
                    self.state.download_progress.phase = crate::state::DownloadPhase::Decoding;

                    // Cache hit: records already in IDB. Send render request directly.
                    self.current_render_scan_key = Some(scan.key.to_storage_key());
                    self.state.displayed_sweep_elevation_number = None;

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
                            self.available_elevation_numbers = elev_nums;
                        }
                    }

                    self.renderers.last_render_params = None; // Force fresh render
                    self.renderers.last_volume_render_params = None;
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
                    if let Some(ref mut worker) = self.decode_worker {
                        self.state.session_stats.pipeline.processing = true;
                        worker.ingest(
                            scan.data.clone(),
                            scan.key.site.0.clone(),
                            scan.key.scan_start.as_secs(),
                            scan.file_name.clone(),
                            fetch_latency,
                        );
                    }
                }

                self.current_scan = Some(scan.clone());

                // Refresh timeline to show the new/loaded scan
                self.state.push_command(state::AppCommand::RefreshTimeline {
                    auto_position: false,
                });
            }

            if let nexrad::DownloadResult::Error(msg) = &result {
                self.state.status_message = format!("Download failed: {}", msg);
                log::error!("Download failed: {}", msg);
                // Clear download progress on error (batch will continue via queue)
                if self.selection_download_queue.is_empty() {
                    self.state.download_progress.clear();
                }
            }
        }

        // Check for completed archive listing operations
        if let Some(result) = self.download_channel.try_recv_listing() {
            match result {
                nexrad::ListingResult::Success {
                    site_id,
                    date,
                    listing,
                } => {
                    log::info!(
                        "Archive listing received: {} files for {}/{}",
                        listing.files.len(),
                        site_id,
                        date
                    );
                    self.archive_index.insert(&site_id, date, listing);

                    // Rebuild shadow scan boundaries for the timeline
                    if site_id == self.state.viz_state.site_id {
                        self.state.shadow_scan_boundaries =
                            self.archive_index.all_boundaries_for_site(&site_id);
                    }
                }
                nexrad::ListingResult::Error(msg) => {
                    log::error!("Listing request failed: {}", msg);
                }
            }
        }

        // Process selection download queue
        {
            let download_type = if do_download_at_position {
                Some(true)
            } else if do_download_selection {
                Some(false)
            } else {
                None // Just pumping existing queue, or nothing to do
            };
            if do_download_selection
                || do_download_at_position
                || !self.selection_download_queue.is_empty()
            {
                self.process_selection_download(ctx, download_type);
            }
        }

        // Handle realtime streaming results
        while let Some(result) = self.realtime_channel.try_recv() {
            self.handle_realtime_result(result, ctx);
        }

        // Stop realtime channel if live mode was stopped by UI
        if !self.state.live_mode_state.is_active() && self.realtime_channel.is_active() {
            log::info!("Stopping realtime channel (live mode ended)");
            self.realtime_channel.stop();
        }

        // Update live mode countdown from realtime channel
        if self.state.live_mode_state.is_active() {
            if let Some(duration) = self.realtime_channel.time_until_next() {
                let now = js_sys::Date::now() / 1000.0;

                self.state.live_mode_state.next_chunk_expected_at =
                    Some(now + duration.as_secs_f64());
            }
        }

        // Auto-load scan when scrubbing: find the most recent scan within 15 minutes.
        // In the worker architecture, this sends a render request directly —
        // the worker reads records from IDB, decodes the target elevation, and renders.
        //
        // In FixedTilt mode, we also detect intra-scan sweep changes: a scan may
        // contain multiple sweeps at the target elevation (e.g. VCP 215 has 0.5°
        // at both elevation_number 1 and 3). As playback advances past a new
        // sweep's start_time, we re-render with that sweep's elevation_number.
        const MAX_SCAN_AGE_SECS: f64 = 15.0 * 60.0;
        {
            let playback_ts = self.state.playback_state.playback_position();

            // Extract scrub decision data from the immutable borrow of radar_timeline
            let scrub_action = self
                .state
                .radar_timeline
                .find_recent_scan(playback_ts, MAX_SCAN_AGE_SECS)
                .map(|scan| {
                    let scan_ts = scan.key_timestamp as i64;
                    let target_elev_num = match self.state.viz_state.render_mode {
                        crate::state::RenderMode::FixedTilt => {
                            self.best_elevation_at_playback(scan, playback_ts)
                        }
                        crate::state::RenderMode::MostRecent => {
                            self.most_recent_sweep_elevation(scan, playback_ts)
                        }
                    };

                    let needs_new_scan = match self.state.displayed_scan_timestamp {
                        Some(displayed) => displayed != scan_ts,
                        None => true,
                    };
                    let needs_new_sweep = !needs_new_scan
                        && self.state.displayed_sweep_elevation_number != Some(target_elev_num);

                    // Capture overlay data from the matching sweep
                    let sweep_overlay = scan
                        .sweeps
                        .iter()
                        .find(|s| s.elevation_number == target_elev_num)
                        .map(|s| (s.start_time, s.end_time, s.elevation));

                    // Extract all elevation numbers for volume rendering
                    let mut elev_nums: Vec<u8> =
                        scan.sweeps.iter().map(|s| s.elevation_number).collect();
                    elev_nums.sort_unstable();
                    elev_nums.dedup();

                    (
                        scan_ts,
                        target_elev_num,
                        needs_new_scan,
                        needs_new_sweep,
                        sweep_overlay,
                        elev_nums,
                    )
                });

            if let Some((
                scan_ts,
                target_elev_num,
                needs_new_scan,
                needs_new_sweep,
                sweep_overlay,
                elev_nums,
            )) = scrub_action
            {
                if (needs_new_scan || needs_new_sweep) && self.decode_worker.is_some() {
                    if needs_new_scan {
                        log::debug!(
                            "Scrubbing: new scan at {} elev={} (playback at {})",
                            scan_ts,
                            target_elev_num,
                            playback_ts as i64
                        );
                    } else {
                        log::debug!(
                            "Scrubbing: new sweep elev_num={} within scan {} (playback at {})",
                            target_elev_num,
                            scan_ts,
                            playback_ts as i64
                        );
                    }

                    // Update canvas overlay from sweep metadata
                    if let Some((start, end, elev)) = sweep_overlay {
                        self.update_overlay_from_sweep(start, end, elev);
                    }

                    // Build scan key in data storage format: "SITE|TIMESTAMP_MS"
                    let scan_key = data::ScanKey::from_secs(&self.state.viz_state.site_id, scan_ts);
                    self.current_render_scan_key = Some(scan_key.to_storage_key());
                    self.state.displayed_scan_timestamp = Some(scan_ts);
                    self.state.displayed_sweep_elevation_number = Some(target_elev_num);
                    if !elev_nums.is_empty() {
                        self.available_elevation_numbers = elev_nums;
                    }
                    self.renderers.last_render_params = None; // Force fresh render
                    self.renderers.last_volume_render_params = None;
                    self.request_worker_render();
                    if self.state.viz_state.volume_3d_enabled {
                        self.request_worker_render_volume();
                    }
                }
            } else if self.state.displayed_scan_timestamp.is_some() {
                // No scan found within range — clear stale render
                log::debug!(
                    "No scan within {}s of playback at {}, clearing render",
                    MAX_SCAN_AGE_SECS,
                    playback_ts as i64
                );
                if let Some(ref renderer) = self.renderers.gpu {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_data();
                    }
                }
                self.state.displayed_scan_timestamp = None;
                self.state.displayed_sweep_elevation_number = None;
                self.current_render_scan_key = None;
                self.renderers.last_render_params = None;
                self.state.viz_state.data_staleness_secs = None;
                self.state.viz_state.rendered_sweep_end_secs = None;
                self.state.viz_state.timestamp = "--:--:-- UTC".to_string();
                self.state.viz_state.elevation = "-- deg".to_string();
            }
        }

        // Pre-render next sweep: when playing and near the end of the current sweep,
        // preemptively send a render request for the upcoming sweep so the result
        // is ready when the boundary is crossed, reducing perceived stutter.
        #[allow(clippy::unnecessary_unwrap)]
        if self.state.playback_state.playing && self.decode_worker.is_some() {
            let playback_ts = self.state.playback_state.playback_position();
            let speed = self
                .state
                .playback_state
                .speed
                .timeline_seconds_per_real_second();
            // Prefetch threshold: 0.5 real seconds * playback speed = timeline seconds ahead
            let prefetch_lookahead = 0.5 * speed;

            if let Some(scan) = self
                .state
                .radar_timeline
                .find_scan_at_timestamp(playback_ts)
            {
                if let Some((sweep_idx, sweep)) = scan.find_sweep_at_timestamp(playback_ts) {
                    let time_to_end = sweep.end_time - playback_ts;
                    if time_to_end > 0.0 && time_to_end < prefetch_lookahead {
                        // We're near the end of this sweep — figure out the next one
                        let next_elev_num = if sweep_idx + 1 < scan.sweeps.len() {
                            // Next sweep in same scan
                            Some(scan.sweeps[sweep_idx + 1].elevation_number)
                        } else {
                            // End of scan — next scan's first sweep at target elevation
                            let future_ts = playback_ts + prefetch_lookahead;
                            self.state
                                .radar_timeline
                                .find_scan_at_timestamp(future_ts)
                                .and_then(|next_scan| {
                                    next_scan.sweeps.first().map(|s| s.elevation_number)
                                })
                        };

                        if let Some(next_en) = next_elev_num {
                            // Only prefetch if it differs from what we're currently showing
                            if self.state.displayed_sweep_elevation_number != Some(next_en) {
                                if let Some(ref scan_key) = self.current_render_scan_key {
                                    let product =
                                        self.state.viz_state.product.to_worker_string().to_string();
                                    let prefetch_params = (
                                        scan_key.clone(),
                                        next_en,
                                        product.clone(),
                                        self.state.viz_state.render_mode,
                                    );
                                    if self.renderers.last_render_params.as_ref()
                                        != Some(&prefetch_params)
                                    {
                                        log::debug!(
                                            "Prefetching next sweep: elev_num={} ({:.1}s ahead)",
                                            next_en,
                                            time_to_end,
                                        );
                                        self.renderers.last_render_params = Some(prefetch_params);
                                        self.decode_worker.as_mut().unwrap().render(
                                            scan_key.clone(),
                                            next_en,
                                            product,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Detect elevation/product changes and trigger worker re-render.
        // If the user changes these settings and we have a current scan, we need
        // a new render from the worker.
        if self.current_render_scan_key.is_some() && self.decode_worker.is_some() {
            if self.state.viz_state.volume_3d_enabled
                && self.state.viz_state.view_mode == state::ViewMode::Globe3D
            {
                self.request_worker_render_volume();
            }
            self.request_worker_render();
        }

        // Update session stats from live network statistics
        let network_stats = self.download_channel.stats();
        self.state
            .session_stats
            .update_from_network_stats(&network_stats);

        // Push current state to URL (throttled to once per second)
        {
            let now = web_time::Instant::now();
            if now.duration_since(self.last_url_push).as_secs_f64() >= 1.0 {
                self.last_url_push = now;
                let cam = &self.state.viz_state.camera;
                let view = state::url_state::ViewState {
                    mz: Some(self.state.viz_state.zoom),
                    tz: Some(self.state.playback_state.timeline_zoom),
                    vm: Some(match self.state.viz_state.view_mode {
                        state::ViewMode::Flat2D => 0,
                        state::ViewMode::Globe3D => 1,
                    }),
                    cm: Some(match cam.mode {
                        state::CameraMode::PlanetOrbit => 0,
                        state::CameraMode::SiteOrbit => 1,
                        state::CameraMode::FreeLook => 2,
                    }),
                    cd: Some(cam.distance),
                    clat: Some(cam.center_lat),
                    clon: Some(cam.center_lon),
                    ct: Some(cam.tilt),
                    cr: Some(cam.rotation),
                    ob: Some(cam.orbit_bearing),
                    oe: Some(cam.orbit_elevation),
                    fp: Some([cam.free_pos.x, cam.free_pos.y, cam.free_pos.z]),
                    fy: Some(cam.free_yaw),
                    fpt: Some(cam.free_pitch),
                    fs: Some(cam.free_speed),
                    v3d: Some(self.state.viz_state.volume_3d_enabled),
                    vdc: Some(self.state.viz_state.volume_density_cutoff),
                };
                state::url_state::push_to_url(
                    &self.state.viz_state.site_id,
                    self.state.playback_state.playback_position(),
                    self.state.viz_state.product.short_code(),
                    self.state.viz_state.center_lat,
                    self.state.viz_state.center_lon,
                    &view,
                );

                // Save user preferences if changed (piggyback on URL throttle)
                let current_prefs = state::UserPreferences::from_app_state(&self.state);
                if current_prefs != self.last_saved_preferences {
                    current_prefs.save();
                    self.last_saved_preferences = current_prefs;
                }
            }
        }

        // Render UI panels in the correct order for egui layout
        // Side and top/bottom panels must be rendered before CentralPanel
        ui::render_top_bar(ctx, &mut self.state);
        ui::render_bottom_panel(ctx, &mut self.state);
        ui::render_left_panel(ctx, &mut self.state);
        ui::render_right_panel(ctx, &mut self.state);

        // Render canvas with GPU-based radar rendering
        ui::render_canvas_with_geo(
            ctx,
            &mut self.state,
            Some(&self.geo_layers),
            self.renderers.gpu.as_ref(),
            self.renderers.globe.as_ref(),
            self.renderers.geo_line.as_ref(),
            self.renderers.globe_radar.as_ref(),
            self.renderers.volume_ray.as_ref(),
        );

        // Process keyboard shortcuts
        ui::handle_shortcuts(ctx, &mut self.state);

        // Render overlays (on top of everything)
        ui::render_site_modal(ctx, &mut self.state, &mut self.site_modal_state);
        ui::render_shortcuts_help(ctx, &mut self.state);
        ui::render_wipe_modal(ctx, &mut self.state);
        ui::render_stats_modal(ctx, &mut self.state);
        ui::render_event_modal(ctx, &mut self.state, &mut self.event_modal_state);
    }
}
