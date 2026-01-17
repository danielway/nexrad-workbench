#![warn(clippy::all)]

//! NEXRAD Workbench - A web-based radar data visualization tool.
//!
//! This application provides an interface for loading, viewing, and analyzing
//! NEXRAD weather radar data. It supports local file upload, AWS archive browsing,
//! and realtime streaming (when implemented).

mod renderer;
mod state;
mod ui;

use eframe::egui;
use state::AppState;

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

    /// Whether the placeholder texture has been initialized
    texture_initialized: bool,
}

impl WorkbenchApp {
    /// Creates a new WorkbenchApp instance.
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            state: AppState::new(),
            texture_initialized: false,
        }
    }
}

impl eframe::App for WorkbenchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Initialize the placeholder texture on first frame
        if !self.texture_initialized {
            self.state.viz_state.texture = Some(renderer::create_placeholder_texture(ctx));
            self.texture_initialized = true;
        }

        // Render UI panels in the correct order for egui layout
        // Side and top/bottom panels must be rendered before CentralPanel
        ui::render_top_bar(ctx, &mut self.state);
        ui::render_bottom_panel(ctx, &mut self.state);
        ui::render_left_panel(ctx, &mut self.state);
        ui::render_right_panel(ctx, &mut self.state);
        ui::render_canvas(ctx, &mut self.state);
    }
}
