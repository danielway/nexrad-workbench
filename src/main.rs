#![warn(clippy::all)]

//! NEXRAD Workbench - A web-based radar data visualization tool.
//!
//! This application provides an interface for loading, viewing, and analyzing
//! NEXRAD weather radar data. It supports local file upload, AWS archive browsing,
//! and realtime streaming (when implemented).

mod data;
mod file_ops;
mod geo;
mod state;
mod storage;
mod ui;

use eframe::egui;
use file_ops::FilePickerChannel;
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
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
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

        Self {
            state: AppState::new(),
            file_picker: FilePickerChannel::new(),
            #[cfg(target_arch = "wasm32")]
            file_cache: IndexedDbStore::new(StorageConfig::new("nexrad-workbench", "file-cache")),
            geo_layers,
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
}

impl eframe::App for WorkbenchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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

        // Render UI panels in the correct order for egui layout
        // Side and top/bottom panels must be rendered before CentralPanel
        ui::render_top_bar(ctx, &mut self.state);
        ui::render_bottom_panel(ctx, &mut self.state);
        ui::render_left_panel(ctx, &mut self.state, &self.file_picker);
        ui::render_right_panel(ctx, &mut self.state);
        ui::render_canvas_with_geo(ctx, &mut self.state, Some(&self.geo_layers));
    }
}
