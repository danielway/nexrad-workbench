#![warn(clippy::all)]

//! NEXRAD Workbench - A web-based radar data visualization tool.
//!
//! This application provides an interface for loading, viewing, and analyzing
//! NEXRAD weather radar data. It supports local file upload, AWS archive browsing,
//! and realtime streaming (when implemented).

mod data;
mod file_ops;
mod geo;
mod nexrad;
mod state;
mod storage;
mod ui;

use eframe::egui;
use file_ops::FilePickerChannel;
// Use explicit crate path to avoid conflict with local nexrad module
use ::nexrad::prelude::{load, Volume};
use state::AppState;
use storage::{CachedFile, StorageConfig};

#[cfg(target_arch = "wasm32")]
use storage::IndexedDbStore;

// Native entry point
#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    env_logger::init();

    let native_options = eframe::NativeOptions::default();

    eframe::run_native(
        "NEXRAD Workbench",
        native_options,
        Box::new(|cc| Ok(Box::new(WorkbenchApp::new(cc)))),
    )
}

// WASM entry point - main is not called on wasm32
#[cfg(target_arch = "wasm32")]
fn main() {}

/// Entry point for the WASM application.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub async fn start() {
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

/// Main application state and logic.
pub struct WorkbenchApp {
    /// Application state containing all sub-states
    state: AppState,

    /// Channel for async file picker operations
    file_picker: FilePickerChannel,

    /// File cache storage (IndexedDB on WASM)
    #[cfg(target_arch = "wasm32")]
    file_cache: IndexedDbStore,

    /// Geographic layer data for map overlays
    geo_layers: geo::GeoLayerSet,

    /// NEXRAD scan cache for AWS downloads
    nexrad_cache: nexrad::NexradCache,

    /// Channel for async NEXRAD download operations
    download_channel: nexrad::DownloadChannel,

    /// Channel for async cache metadata loading
    cache_load_channel: nexrad::CacheLoadChannel,

    /// Cache for archive file listings (by site/date)
    archive_index: nexrad::ArchiveIndex,

    /// Currently loaded NEXRAD scan
    current_scan: Option<nexrad::CachedScan>,

    /// Full decoded volume for texture-based rendering
    decoded_volume: Option<Volume>,

    /// Texture cache for rendered radar imagery
    radar_texture_cache: nexrad::RadarTextureCache,

    /// Queue of files to download for selection download feature.
    /// Each entry is (date, file_name, timestamp).
    selection_download_queue: Vec<(chrono::NaiveDate, String, i64)>,
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

        log::info!(
            "Loaded geo layers: {} states, {} counties",
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
        );

        let state = AppState::new();
        let nexrad_cache = nexrad::NexradCache::new();
        let cache_load_channel = nexrad::CacheLoadChannel::new();

        // Run migration to create metadata for any existing cached scans
        cache_load_channel.run_migration(cc.egui_ctx.clone(), nexrad_cache.clone());

        Self {
            state,
            file_picker: FilePickerChannel::new(),
            #[cfg(target_arch = "wasm32")]
            file_cache: IndexedDbStore::new(StorageConfig::new("nexrad-workbench", "file-cache")),
            geo_layers,
            nexrad_cache,
            download_channel: nexrad::DownloadChannel::new(),
            cache_load_channel,
            archive_index: nexrad::ArchiveIndex::new(),
            current_scan: None,
            decoded_volume: None,
            radar_texture_cache: nexrad::RadarTextureCache::new(),
            selection_download_queue: Vec::new(),
        }
    }

    /// Loads geographic layer data from GeoJSON string.
    #[allow(dead_code)]
    pub fn load_geo_layer(
        &mut self,
        layer_type: geo::GeoLayerType,
        geojson_str: &str,
    ) -> Result<(), String> {
        self.geo_layers.load_layer(layer_type, geojson_str)
    }

    /// Process selection download: download scans in the selected time range serially.
    fn process_selection_download(&mut self, ctx: &egui::Context) {
        let site_id = self.state.viz_state.site_id.clone();

        // If we have items in the queue, try to download the next one
        if !self.selection_download_queue.is_empty() {
            // Check if current download is still in progress
            let (_, _, timestamp) = &self.selection_download_queue[0];
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
                let (next_date, next_name, next_ts) = &self.selection_download_queue[0];
                self.state.status_message = format!(
                    "Downloading {} ({} remaining)",
                    next_name,
                    self.selection_download_queue.len()
                );
                self.download_channel.download_file(
                    ctx.clone(),
                    site_id.clone(),
                    *next_date,
                    next_name.clone(),
                    *next_ts,
                    self.nexrad_cache.clone(),
                );
            } else {
                // All done
                self.state.download_selection_in_progress = false;
                self.state.status_message = "Selection download complete".to_string();
                log::info!("Selection download complete");
            }
            return;
        }

        // No queue - check if we should build one
        if !self.state.download_selection_requested {
            return;
        }
        self.state.download_selection_requested = false;

        // Get the selection range
        let Some((sel_start, sel_end)) = self.state.playback_state.selection_range() else {
            log::warn!("Download selection requested but no valid selection");
            return;
        };

        let sel_start_i64 = sel_start as i64;
        let sel_end_i64 = sel_end as i64;

        // Determine the date range
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

        // Collect all files in the range from cached listings
        let mut files_to_download = Vec::new();
        let mut current_date = start_date;

        while current_date <= end_date {
            // Check if we have the listing for this date
            if let Some(listing) = self.archive_index.get(&site_id, &current_date) {
                // Find all files in the selection range
                for file in &listing.files {
                    if file.timestamp >= sel_start_i64 && file.timestamp <= sel_end_i64 {
                        // Check if already cached
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
                                file.timestamp,
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
                // Re-trigger download selection once listing arrives
                self.state.download_selection_requested = true;
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

        // Sort by timestamp
        files_to_download.sort_by_key(|(_, _, ts)| *ts);

        log::info!(
            "Queued {} files for download in selection",
            files_to_download.len()
        );

        // Start downloading
        self.state.download_selection_in_progress = true;
        self.selection_download_queue = files_to_download;

        // Kick off first download
        let (date, file_name, timestamp) = &self.selection_download_queue[0];
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
            self.nexrad_cache.clone(),
        );
    }
}

impl eframe::App for WorkbenchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle cache clear request
        if self.state.clear_cache_requested && !self.cache_load_channel.is_loading() {
            self.state.clear_cache_requested = false;
            self.cache_load_channel
                .clear_cache(ctx.clone(), self.nexrad_cache.clone());
        }

        // Check if timeline needs to be refreshed from cache
        if self.state.timeline_needs_refresh && !self.cache_load_channel.is_loading() {
            self.state.timeline_needs_refresh = false;
            self.cache_load_channel.load_site_timeline(
                ctx.clone(),
                self.nexrad_cache.clone(),
                self.state.viz_state.site_id.clone(),
            );
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

                        if let Some(ts) = self.state.playback_state.selected_timestamp {
                            // If current position is not within any range, move to most recent
                            let in_any_range = ranges.iter().any(|r| r.contains(ts));
                            if !in_any_range {
                                self.state.playback_state.selected_timestamp =
                                    Some(most_recent_end);
                            }
                        } else {
                            // No timestamp selected, start at the most recent scan
                            self.state.playback_state.selected_timestamp = Some(most_recent_end);
                        }

                        log::info!("Timeline has {} contiguous range(s)", ranges.len());
                    }
                }
                nexrad::CacheLoadResult::Error(msg) => {
                    log::error!("Cache load failed: {}", msg);
                }
            }
        }

        // Check for completed file pick operations
        if let Some(result) = self.file_picker.try_recv() {
            self.state.upload_state.loading = false;
            match result {
                Some(file_result) => {
                    // Cache the file to IndexedDB (WASM only)
                    #[cfg(target_arch = "wasm32")]
                    {
                        use storage::KeyValueStore;
                        let cached =
                            CachedFile::new(file_result.file_name.clone(), &file_result.file_data);
                        let cache = self.file_cache.clone();
                        let file_name = file_result.file_name.clone();
                        wasm_bindgen_futures::spawn_local(async move {
                            match cache.put(&file_name, &cached).await {
                                Ok(()) => log::info!("Cached file: {}", file_name),
                                Err(e) => log::error!("Failed to cache file: {}", e),
                            }
                        });
                    }

                    self.state.upload_state.file_name = Some(file_result.file_name.clone());
                    self.state.upload_state.file_size = Some(file_result.file_size);
                    self.state.upload_state.file_data = Some(file_result.file_data);
                    self.state.status_message = format!("Loaded file: {}", file_result.file_name);
                }
                None => {
                    // User cancelled the file dialog
                    self.state.status_message = "File selection cancelled".to_string();
                }
            }
        }

        // Check for completed NEXRAD download operations
        if let Some(result) = self.download_channel.try_recv() {
            self.state.download_in_progress = false;
            match &result {
                nexrad::DownloadResult::Success(scan) | nexrad::DownloadResult::CacheHit(scan) => {
                    let is_cache_hit = matches!(result, nexrad::DownloadResult::CacheHit(_));
                    self.state.status_message = if is_cache_hit {
                        format!("Loaded from cache: {}", scan.file_name)
                    } else {
                        format!("Downloaded: {}", scan.file_name)
                    };

                    // Load the volume for texture-based rendering
                    match load(&scan.data) {
                        Ok(volume) => {
                            let sweep_count = volume.sweeps().len();
                            log::info!("Loaded volume with {} sweeps", sweep_count);
                            self.decoded_volume = Some(volume);
                            // Invalidate texture cache to trigger re-render
                            self.radar_texture_cache.invalidate();
                        }
                        Err(e) => {
                            log::error!("Failed to load NEXRAD volume: {}", e);
                            self.state.status_message = format!("Load error: {}", e);
                        }
                    }

                    self.current_scan = Some(scan.clone());

                    // Refresh timeline to show the new/loaded scan
                    self.state.timeline_needs_refresh = true;
                }
                nexrad::DownloadResult::Error(msg) => {
                    self.state.status_message = format!("Download failed: {}", msg);
                    log::error!("Download failed: {}", msg);
                }
                nexrad::DownloadResult::Progress(current, total) => {
                    self.state.status_message =
                        format!("Downloading: {} / {} bytes", current, total);
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
                }
                nexrad::ListingResult::Error(msg) => {
                    log::error!("Listing request failed: {}", msg);
                }
            }
        }

        // Process selection download queue
        if self.state.download_selection_requested || !self.selection_download_queue.is_empty() {
            self.process_selection_download(ctx);
        }

        // Update session stats from live network statistics
        let network_stats = self.download_channel.stats();
        self.state
            .session_stats
            .update_from_network_stats(&network_stats);

        // Render UI panels in the correct order for egui layout
        // Side and top/bottom panels must be rendered before CentralPanel
        ui::render_top_bar(ctx, &mut self.state);
        ui::render_bottom_panel(ctx, &mut self.state);
        ui::render_left_panel(
            ctx,
            &mut self.state,
            &self.file_picker,
            &self.download_channel,
            &self.nexrad_cache,
        );
        ui::render_right_panel(ctx, &mut self.state);

        // Render canvas with texture-based radar rendering
        ui::render_canvas_with_geo(
            ctx,
            &mut self.state,
            Some(&self.geo_layers),
            self.decoded_volume.as_ref(),
            &mut self.radar_texture_cache,
        );
    }
}
