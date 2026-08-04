//! Long-press recognition primitive (touch).
//!
//! A long-press is a touch that stays down, roughly still, for at least
//! [`LONG_PRESS_SECS`]. egui has no built-in long-press, so this tracks the
//! press lifetime in `egui` temp memory keyed by a widget id, and fires once
//! per press when the threshold is crossed.
//!
//! Introduced for the scan inspector's touch entry (spec §12 "Long-press
//! (touch) → open scan inspector"); kept generic so later phases (P9 mobile
//! transport, P10 calendar) can reuse the same gesture without re-deriving it.

use eframe::egui::{self, Pos2, Response};

/// How long a touch must stay down (and roughly still) to count as a
/// long-press. ~500 ms matches the platform convention for context menus.
pub(crate) const LONG_PRESS_SECS: f64 = 0.5;

/// Maximum movement (screen points) allowed during the hold before it's
/// treated as a drag/scrub instead of a long-press.
const MOVE_TOLERANCE_PTS: f32 = 12.0;

/// Per-press state stashed in egui temp memory.
#[derive(Clone, Copy)]
struct PressState {
    start_secs: f64,
    start_pos: Pos2,
    fired: bool,
}

/// Detect a long-press on `response`. Returns `Some(pos)` exactly once, on the
/// frame the hold crosses [`LONG_PRESS_SECS`] without having moved past
/// [`MOVE_TOLERANCE_PTS`]. Only meaningful for touch input — pointers use
/// right-click instead — so callers gate on a touch having been seen.
///
/// `id` namespaces the press state so multiple long-press surfaces don't
/// collide. State auto-clears when the press ends.
pub(crate) fn detect(ctx: &egui::Context, response: &Response, id: egui::Id) -> Option<Pos2> {
    let key = id.with("long_press");
    let now = ctx.input(|i| i.time);
    let down = response.is_pointer_button_down_on();

    if !down {
        // Press ended (or never started) — clear any tracked state.
        ctx.memory_mut(|m| m.data.remove::<PressState>(key));
        return None;
    }

    let pos = response.interact_pointer_pos()?;
    let mut state = ctx
        .memory(|m| m.data.get_temp::<PressState>(key))
        .unwrap_or(PressState {
            start_secs: now,
            start_pos: pos,
            fired: false,
        });

    // Moved too far → treat as a drag, not a long-press. Reset the anchor so a
    // settle-then-hold doesn't immediately fire.
    if (pos - state.start_pos).length() > MOVE_TOLERANCE_PTS {
        state.start_secs = now;
        state.start_pos = pos;
        state.fired = false;
        ctx.memory_mut(|m| m.data.insert_temp(key, state));
        return None;
    }

    let mut result = None;
    if !state.fired && now - state.start_secs >= LONG_PRESS_SECS {
        state.fired = true;
        result = Some(state.start_pos);
    } else {
        // Keep the hold alive so the timer keeps advancing toward the threshold.
        ctx.request_repaint();
    }
    ctx.memory_mut(|m| m.data.insert_temp(key, state));
    result
}
