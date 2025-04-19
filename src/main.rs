#![cfg(not(target_arch = "wasm32"))]

mod app;

fn main() -> Result<(), eframe::Error> {
    env_logger::init();

    let options = eframe::NativeOptions {
        ..Default::default()
    };

    eframe::run_native(
        "NEXRAD Workbench",
        options,
        Box::new(|_cc| Box::<app::App>::default()),
    )
}
