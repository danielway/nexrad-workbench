//! Mobile / touch UI.
//!
//! Phase 1: multi-touch gesture digestion for the 2D canvas (see [`gestures`]).
//! Phase 3: mobile top bar + action-bar chrome that replaces the desktop
//! panels when [`AppState::is_mobile`](crate::state::AppState::is_mobile) is
//! true. Detailed controls live in the settings modal (see [`settings_modal`]).

pub(crate) mod auto_hide;
pub(crate) mod gestures;
mod scrubber;
mod settings_modal;
mod tabs;
mod top_bar;

pub(in crate::ui) use settings_modal::MobileSettingsModalLayer;
pub(in crate::ui) use tabs::MobileChromeLayer;
pub(in crate::ui) use top_bar::MobileTopBarLayer;

/// Whether the mobile chrome (top bar + bottom transport) should be hidden
/// this frame (spec §13 phone: "Canvas full-bleed; chrome auto-hides during
/// playback, tap to reveal"). Bridges egui per-frame input into the pure
/// [`auto_hide::should_hide_chrome`] policy; resolved once in
/// [`resolve_mobile_auto_hide`] and stashed for everyone else to read.
///
/// Hides only while genuinely playing and idle past the threshold, never while
/// paused, while any modal/sheet is open, or mid-gesture. Reads (does not
/// mutate) the auto-hide timer.
fn chrome_should_hide(
    ctx: &eframe::egui::Context,
    state: &crate::state::AppState,
    playback: &crate::subsystem::Playback,
    chrome: &crate::subsystem::Chrome,
    diagnostics: &crate::subsystem::Diagnostics,
) -> bool {
    let now_secs = ctx.input(|i| i.time);
    // "Playing" = archive playback advancing OR tethered to the live edge (the
    // feed is conceptually playing). Either should let the canvas go full-bleed.
    let is_playing = playback.state.playing || playback.state.time_model.is_pinned();
    let gesture_active = ctx.input(|i| i.pointer.any_down() || i.any_touches());

    auto_hide::should_hide_chrome(auto_hide::AutoHideInputs {
        now_secs,
        last_interaction_secs: chrome.mobile_auto_hide.last_interaction_secs,
        is_playing,
        modal_open: any_overlay_open(state, chrome, diagnostics),
        gesture_active,
    })
}

/// Resolve the mobile chrome auto-hide for this frame (spec §13). Called once,
/// before layout, so the top bar / bottom chrome `visible()` predicates and the
/// canvas all read the same `chrome.mobile_auto_hide.hidden`.
///
/// Order of operations:
/// 1. Detect a reveal tap — a pointer press while the chrome *was* hidden last
///    frame. Since hidden chrome isn't drawn, the only thing on screen is the
///    canvas, so any fresh press is a reveal: bump the idle timer and latch
///    `revealed_this_frame` so [`crate::ui::canvas`] swallows that press rather
///    than panning the map.
/// 2. Recompute and store `hidden` from the (possibly bumped) timer.
/// 3. While playing and still visible, schedule a single repaint at the hide
///    moment so the chrome slides away on time even if nothing else animates.
pub(crate) fn resolve_mobile_auto_hide(
    ctx: &eframe::egui::Context,
    state: &mut crate::state::AppState,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
    diagnostics: &crate::subsystem::Diagnostics,
) {
    let now_secs = ctx.input(|i| i.time);
    let was_hidden = chrome.mobile_auto_hide.hidden;
    chrome.mobile_auto_hide.revealed_this_frame = false;

    // A press that begins while the chrome is hidden reveals it (and is
    // consumed by the canvas). `any_pressed` is the press *down* edge.
    let pressed = ctx.input(|i| i.pointer.any_pressed());
    if was_hidden && pressed {
        chrome.mobile_auto_hide.touch(now_secs);
        chrome.mobile_auto_hide.revealed_this_frame = true;
    }

    let hidden = chrome_should_hide(ctx, state, playback, chrome, diagnostics);
    chrome.mobile_auto_hide.hidden = hidden;

    // Keep the idle countdown ticking toward the hide moment without spinning.
    if !hidden {
        let is_playing = playback.state.playing || playback.state.time_model.is_pinned();
        if let Some(remaining) = chrome
            .mobile_auto_hide
            .secs_until_hide(now_secs, is_playing)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(
                (remaining * 1000.0).ceil().max(1.0) as u64,
            ));
        }
    }
}

/// Whether any modal, sheet, or popup is open over the mobile chrome — any of
/// these should suppress auto-hide so the user can finish what they opened.
fn any_overlay_open(
    state: &crate::state::AppState,
    chrome: &crate::subsystem::Chrome,
    diagnostics: &crate::subsystem::Diagnostics,
) -> bool {
    chrome.mobile_settings_open
        || chrome.site_modal_open
        || chrome.queue_sheet_open
        || chrome.scan_inspector.is_some()
        || chrome.wipe_modal_open
        || chrome.range_download_modal.is_some()
        || chrome.stats_detail_open
        || chrome.vcp_forecast_open
        || chrome.network_log_open
        || chrome.event_modal_open
        || chrome.shortcuts_help_visible
        || state.datetime_picker.open
        || diagnostics.alerts.list_modal_open
        || diagnostics.alerts.selected_alert_id.is_some()
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
