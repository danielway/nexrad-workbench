//! Mobile / touch UI.
//!
//! Phase 1: multi-touch gesture digestion for the 2D canvas (see [`gestures`]).
//! Phase 3: mobile top bar + action-bar chrome that replaces the desktop
//! panels when [`AppState::is_mobile`](crate::state::AppState::is_mobile) is
//! true. Detailed controls live in the settings modal (see [`settings_modal`]).

pub mod gestures;
mod scrubber;
mod settings_modal;
mod tabs;
mod top_bar;

pub(crate) use settings_modal::render_mobile_settings_modal;
pub(crate) use tabs::render_mobile_chrome;
pub(crate) use top_bar::render_mobile_top_bar;

/// iOS safe-area insets in CSS pixels: `(top, right, bottom, left)`.
///
/// Non-zero only when installed as a home-screen PWA on a device that
/// reserves space for the status bar or home indicator. Always zero in
/// desktop browsers and Chrome responsive mode, which is why the chrome
/// looks perfect there but clips under the status bar on real iPhones.
///
/// Reads CSS custom properties set in `index.html` via `getComputedStyle`,
/// dispatched through a pre-declared `window.__nexradSafeAreaInsets()`
/// helper to avoid enabling the `CssStyleDeclaration` feature in web-sys.
pub(crate) fn safe_area_insets() -> (f32, f32, f32, f32) {
    use wasm_bindgen::{JsCast, JsValue};

    let Some(window) = web_sys::window() else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let global: JsValue = window.into();
    let Ok(fn_val) = js_sys::Reflect::get(&global, &"__nexradSafeAreaInsets".into()) else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let Some(func) = fn_val.dyn_ref::<js_sys::Function>() else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let Ok(result) = func.call0(&JsValue::NULL) else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let read = |key: &str| -> f32 {
        js_sys::Reflect::get(&result, &key.into())
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32
    };
    (read("top"), read("right"), read("bottom"), read("left"))
}
