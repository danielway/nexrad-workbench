//! Mobile chrome auto-hide policy (spec §13 phone: "Canvas full-bleed; chrome
//! auto-hides during playback, tap to reveal").
//!
//! While playback is actively playing on mobile, the top bar + bottom chrome
//! disappear after a short idle so the canvas is full-bleed; a tap on the
//! canvas reveals them again. Pausing always reveals chrome, and any open
//! modal/sheet or an in-progress gesture suppresses hiding.
//!
//! This is the pure decision ([`should_hide_chrome`]); the per-frame
//! bookkeeping (idle timer + reveal latch) lives on
//! [`MobileChromeAutoHide`](crate::state::MobileChromeAutoHide), kept in the
//! `state` layer so `subsystem::Chrome` can own it without depending on `ui`.

use crate::state::MOBILE_CHROME_IDLE_HIDE_SECS;

/// Inputs to the auto-hide decision for one frame. All sourced from
/// already-resolved per-frame state so the decision stays a pure function.
#[derive(Clone, Copy, Debug)]
pub struct AutoHideInputs {
    /// egui `input.time` (monotonic seconds).
    pub now_secs: f64,
    /// When the user last interacted with the chrome/canvas (tap, drag, reveal).
    pub last_interaction_secs: f64,
    /// Whether playback is actively advancing (archive playing or tethered live).
    pub is_playing: bool,
    /// Whether any modal / sheet is open (suppresses hiding).
    pub modal_open: bool,
    /// Whether a touch/gesture is currently in progress (suppresses hiding).
    pub gesture_active: bool,
}

/// Whether the mobile chrome should be hidden this frame.
///
/// Hides only while genuinely playing, after [`MOBILE_CHROME_IDLE_HIDE_SECS`]
/// of no interaction, and never while paused, while a modal/sheet is open, or
/// mid-gesture. Pure so the state machine is testable without egui.
pub fn should_hide_chrome(input: AutoHideInputs) -> bool {
    if !input.is_playing || input.modal_open || input.gesture_active {
        return false;
    }
    let idle = input.now_secs - input.last_interaction_secs;
    idle >= MOBILE_CHROME_IDLE_HIDE_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MobileChromeAutoHide;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn base() -> AutoHideInputs {
        AutoHideInputs {
            now_secs: 100.0,
            last_interaction_secs: 100.0,
            is_playing: true,
            modal_open: false,
            gesture_active: false,
        }
    }

    #[wasm_bindgen_test]
    fn paused_chrome_stays_visible() {
        let input = AutoHideInputs {
            is_playing: false,
            now_secs: 1_000.0,
            last_interaction_secs: 0.0, // long idle
            ..base()
        };
        assert!(!should_hide_chrome(input));
    }

    #[wasm_bindgen_test]
    fn playing_hides_after_idle_threshold() {
        let input = AutoHideInputs {
            now_secs: 100.0 + MOBILE_CHROME_IDLE_HIDE_SECS,
            last_interaction_secs: 100.0,
            ..base()
        };
        assert!(should_hide_chrome(input));
    }

    #[wasm_bindgen_test]
    fn playing_stays_visible_within_idle_window() {
        let input = AutoHideInputs {
            now_secs: 100.0 + MOBILE_CHROME_IDLE_HIDE_SECS - 0.5,
            last_interaction_secs: 100.0,
            ..base()
        };
        assert!(!should_hide_chrome(input));
    }

    #[wasm_bindgen_test]
    fn modal_open_suppresses_hide() {
        let input = AutoHideInputs {
            now_secs: 100.0 + MOBILE_CHROME_IDLE_HIDE_SECS + 10.0,
            last_interaction_secs: 100.0,
            modal_open: true,
            ..base()
        };
        assert!(!should_hide_chrome(input));
    }

    #[wasm_bindgen_test]
    fn active_gesture_suppresses_hide() {
        let input = AutoHideInputs {
            now_secs: 100.0 + MOBILE_CHROME_IDLE_HIDE_SECS + 10.0,
            last_interaction_secs: 100.0,
            gesture_active: true,
            ..base()
        };
        assert!(!should_hide_chrome(input));
    }

    #[wasm_bindgen_test]
    fn tap_reveals_then_hides_again_after_idle() {
        let mut h = MobileChromeAutoHide::default();
        // Start playing, long idle (sentinel last-interaction) → would hide.
        assert!(should_hide_chrome(AutoHideInputs {
            now_secs: 50.0,
            last_interaction_secs: h.last_interaction_secs,
            ..base()
        }));
        // Tap to reveal at t=50: resets idle timer, so it's visible now.
        h.touch(50.0);
        assert!(!should_hide_chrome(AutoHideInputs {
            now_secs: 50.5,
            last_interaction_secs: h.last_interaction_secs,
            ..base()
        }));
        // After the idle window passes again, it hides once more.
        assert!(should_hide_chrome(AutoHideInputs {
            now_secs: 50.0 + MOBILE_CHROME_IDLE_HIDE_SECS + 0.1,
            last_interaction_secs: h.last_interaction_secs,
            ..base()
        }));
    }
}
