//! Mobile / touch UI.
//!
//! Phase 1: multi-touch gesture digestion for the 2D canvas (see [`gestures`]).
//! Phase 3: mobile top bar + action-bar chrome that replaces the desktop
//! panels when [`AppState::is_mobile`](crate::state::AppState::is_mobile) is
//! true. Detailed controls live in the settings modal (see [`settings_modal`]).

pub(crate) mod gestures;
mod go_to_day;
mod settings_modal;
mod tabs;
mod top_bar;

pub(in crate::ui) use go_to_day::GoToDayLayer;
pub(in crate::ui) use settings_modal::MobileSettingsModalLayer;
pub(in crate::ui) use tabs::MobileChromeLayer;
pub(in crate::ui) use top_bar::MobileTopBarLayer;

/// Resolve the phone chrome's auto-hide for this frame (one-instrument
/// §13). Called once, before layout, so the top bar, the bottom chrome and
/// the canvas all read the same `chrome.mobile_auto_hide`. The decision is
/// [`crate::core::phone::ChromeAutoHide::step`]; this only reads egui's input
/// into it and schedules the frame at which the chrome would hide.
pub(crate) fn resolve_mobile_auto_hide(
    ctx: &eframe::egui::Context,
    playback: &crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    let (now, pressed, gesture_active) = ctx.input(|i| {
        (
            i.time,
            i.pointer.any_pressed(),
            i.pointer.any_down() || i.any_touches(),
        )
    });
    // An open dialog, the sheet, or any menu or combo box keeps the chrome.
    let menu_open = !chrome.dialogs.is_empty() || eframe::egui::Popup::is_any_open(ctx);
    let out = chrome
        .mobile_auto_hide
        .step(crate::core::phone::AutoHideInputs {
            now,
            playing: playback.state.playing,
            pinned: playback.state.is_pinned(),
            menu_open,
            gesture_active,
            pressed,
        });
    if let Some(secs) = out.recheck_after_secs {
        ctx.request_repaint_after(std::time::Duration::from_millis(
            (secs * 1000.0).ceil().max(1.0) as u64,
        ));
    }
}

/// Whether the browser's primary pointer is coarse (a finger), from
/// `matchMedia("(pointer: coarse)")`. A browser fact, read once at start-up
/// and handed to the core's layout rule.
pub(crate) fn coarse_pointer() -> bool {
    use wasm_bindgen::{JsCast, JsValue};
    let global: JsValue = js_sys::global().into();
    let Ok(f) = js_sys::Reflect::get(&global, &"matchMedia".into()) else {
        return false;
    };
    let Some(f) = f.dyn_ref::<js_sys::Function>() else {
        return false;
    };
    let Ok(list) = f.call1(&global, &"(pointer: coarse)".into()) else {
        return false;
    };
    js_sys::Reflect::get(&list, &"matches".into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

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
