#![warn(clippy::all)]

use eframe::egui;

// Native entry point
#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    env_logger::init();

    let native_options = eframe::NativeOptions::default();

    eframe::run_native(
        "NEXRAD Workbench",
        native_options,
        Box::new(|_cc| Ok(Box::new(WorkbenchApp::default()))),
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
                Box::new(|_cc| Ok(Box::new(WorkbenchApp::default()))),
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

/// Main application state.
#[derive(Default)]
struct WorkbenchApp {
    counter: u32,
}

impl eframe::App for WorkbenchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);

                ui.heading("NEXRAD Workbench");
                ui.add_space(10.0);
                ui.label("Hello world");

                ui.add_space(30.0);
                ui.separator();
                ui.add_space(20.0);

                ui.label(format!("Counter: {}", self.counter));
                ui.add_space(10.0);

                if ui.button("Increment").clicked() {
                    self.counter += 1;
                }
            });
        });
    }
}
