#![cfg(target_arch = "wasm32")]

use eframe::wasm_bindgen::{self, prelude::*};

mod app;

#[wasm_bindgen(start)]
pub async fn start() -> Result<(), JsValue> {
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    eframe::WebRunner::new()
        .start(
            "canvas_id",
            eframe::WebOptions::default(),
            Box::new(|_cc| Box::<App>::default()),
        )
        .await?;

    Ok(())
}
