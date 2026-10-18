//! The phone (one-instrument §13): the same instrument at 390 px.
//!
//! The phone draws the same view-models as the desktop (the top bar, the
//! session's chip and cap, the ladder, the rail's tabs); this module holds the
//! few decisions that are the phone's own:
//!
//! - **Which layout** a viewport gets ([`device_layout`]). Landscape phones
//!   keep the phone layout, with the sheet on the right.
//! - **Where the sheet sits** ([`sheet_rect`]): a bottom sheet in portrait,
//!   on the right in landscape, inside the safe area.
//! - **When the chrome hides** ([`ChromeAutoHide::step`]): after 5 s of
//!   playback without a touch, never on the first tether, never under a
//!   menu or a finger; and how the map holds still while it slides
//!   ([`hold_map_still`]).
//! - **What the mini timeline spans** ([`mini_window`]): the last three
//!   volumes up to now and the ghost of the next, or the playhead's own
//!   three volumes when it is far from now.
//! - **What a press on it is** ([`PressTrack`]): a tap, a scrub, or a
//!   long-press that opens the inspector, never two of them. Its zoom stops,
//!   pinch and snapping are [`crate::core::phone_strip`]'s.
//! - **What the phone cannot show**: 3D ([`NO_3D_NOTICE`]).
//!
//! All pure; `ui::mobile` draws what these return.

// ───────────────────────────────────────────────────────────────────────────
// Layout
// ───────────────────────────────────────────────────────────────────────────

/// Below this width every viewport gets the phone layout, touch or not.
pub(crate) const PHONE_MAX_NARROW_PX: f32 = 500.0;

/// A touch viewport whose short side is under this gets the phone layout in
/// either orientation (a phone's short side is 320–430 px; a tablet's is
/// 744 px and up).
pub(crate) const PHONE_MAX_SHORT_SIDE_PX: f32 = 600.0;

/// What the layout rule reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LayoutInputs {
    /// The content rect, in logical points.
    pub width: f32,
    pub height: f32,
    /// A touch has been seen this session.
    pub touch_seen: bool,
    /// The browser reports a coarse primary pointer (a finger), known before
    /// the first touch.
    pub coarse_pointer: bool,
    /// A forced layout (`Some(true)` phone, `Some(false)` desktop).
    pub forced: Option<bool>,
}

/// The chrome a viewport gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum DeviceLayout {
    #[default]
    Desktop,
    /// The phone chrome. `landscape` puts the sheet on the right.
    Phone { landscape: bool },
}

impl DeviceLayout {
    pub(crate) fn is_phone(self) -> bool {
        matches!(self, DeviceLayout::Phone { .. })
    }

    pub(crate) fn is_landscape_phone(self) -> bool {
        matches!(self, DeviceLayout::Phone { landscape: true })
    }
}

/// The layout for a viewport. A finger on a viewport whose short side is a
/// phone's gets the phone layout in either orientation, so turning a phone on
/// its side keeps the phone chrome (the inventory's §14.1 defect was a 390 px
/// tall desktop layout). A narrow window gets it without a touch. The coarse
/// pointer is known at load, so a phone does not start on the desktop layout
/// and flip on its first touch.
pub(crate) fn device_layout(i: LayoutInputs) -> DeviceLayout {
    let short = i.width.min(i.height);
    let touch = i.touch_seen || i.coarse_pointer;
    let auto = i.width < PHONE_MAX_NARROW_PX || (touch && short < PHONE_MAX_SHORT_SIDE_PX);
    if i.forced.unwrap_or(auto) {
        DeviceLayout::Phone {
            landscape: i.width > i.height,
        }
    } else {
        DeviceLayout::Desktop
    }
}

/// Safe-area insets in points: `(top, right, bottom, left)`.
pub(crate) type Insets = (f32, f32, f32, f32);

/// A rect in points: `(x, y, width, height)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SheetRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The sheet's share of a portrait viewport's height.
pub(crate) const SHEET_PORTRAIT_SHARE: f32 = 0.78;
/// The landscape sheet's width: its share of the viewport, within bounds.
pub(crate) const SHEET_LANDSCAPE_SHARE: f32 = 0.48;
pub(crate) const SHEET_LANDSCAPE_MIN_W: f32 = 320.0;
pub(crate) const SHEET_LANDSCAPE_MAX_W: f32 = 420.0;

/// Where the sheet sits: a bottom sheet in portrait, a full-height sheet on
/// the right in landscape, inside the safe area on every side.
pub(crate) fn sheet_rect(width: f32, height: f32, landscape: bool, insets: Insets) -> SheetRect {
    let (top, right, bottom, left) = insets;
    let usable_w = (width - left - right).max(0.0);
    let usable_h = (height - top - bottom).max(0.0);
    if landscape {
        let w = (width * SHEET_LANDSCAPE_SHARE)
            .clamp(SHEET_LANDSCAPE_MIN_W, SHEET_LANDSCAPE_MAX_W)
            .min(usable_w);
        SheetRect {
            x: width - right - w,
            y: top,
            w,
            h: usable_h,
        }
    } else {
        let h = (height * SHEET_PORTRAIT_SHARE).min(usable_h);
        SheetRect {
            x: left,
            y: height - bottom - h,
            w: usable_w,
            h,
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Chrome auto-hide
// ───────────────────────────────────────────────────────────────────────────

/// Seconds of playback without a touch before the chrome hides.
pub(crate) const CHROME_IDLE_HIDE_SECS: f64 = 5.0;

/// The chrome's slide, in seconds (egui's panel animation runs this long).
pub(crate) const CHROME_SLIDE_SECS: f32 = 0.2;

/// One frame's facts for the auto-hide decision.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct AutoHideInputs {
    /// The frame clock, in seconds.
    pub now: f64,
    /// Archive or loop playback is advancing.
    pub playing: bool,
    /// The playhead is pinned to now.
    pub pinned: bool,
    /// A dialog, the sheet or a menu is open: the chrome stays.
    pub menu_open: bool,
    /// A finger (or button) is down: the chrome stays.
    pub gesture_active: bool,
    /// A press began this frame, anywhere.
    pub pressed: bool,
}

/// What the shell does with the chrome this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AutoHideOut {
    /// Draw the chrome (it slides in or out when this changes).
    pub visible: bool,
    /// This frame's press revealed the chrome: the map must not also pan.
    pub swallow_press: bool,
    /// Ask for a frame this many seconds from now, when the chrome would
    /// hide; `None` when nothing is counting down.
    pub recheck_after_secs: Option<f64>,
}

/// The auto-hide's memory between frames. Lives on the chrome subsystem.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ChromeAutoHide {
    /// When playback last started or the user last pressed.
    last_interaction: f64,
    /// Playback was running on the last frame.
    was_playback: bool,
    /// The playhead has left the boot tether at least once.
    left_first_tether: bool,
    /// The chrome is hidden, as decided this frame.
    pub hidden: bool,
    /// This frame's press revealed the chrome (read by the canvas).
    pub revealed_this_frame: bool,
}

impl ChromeAutoHide {
    /// Decide this frame. The chrome hides only while playback runs, after
    /// [`CHROME_IDLE_HIDE_SECS`] with no press. Playback is archive or loop
    /// playback, or a tether to now the user asked for: the app opens
    /// tethered, and that first tether never hides the chrome. The idle
    /// clock starts when playback starts, so the chrome never vanishes the
    /// moment Play is pressed. Any press resets it; a press on hidden chrome
    /// reveals it and is swallowed; an open menu or a finger keeps it.
    pub(crate) fn step(&mut self, i: AutoHideInputs) -> AutoHideOut {
        if !i.pinned {
            self.left_first_tether = true;
        }
        let playback = i.playing || (i.pinned && self.left_first_tether);
        let reveal = self.hidden && i.pressed;
        if (playback && !self.was_playback) || i.pressed {
            self.last_interaction = i.now;
        }
        self.was_playback = playback;

        let idle = i.now - self.last_interaction;
        let held = i.menu_open || i.gesture_active;
        let hide = playback && !held && idle >= CHROME_IDLE_HIDE_SECS;
        self.hidden = hide;
        self.revealed_this_frame = reveal;
        AutoHideOut {
            visible: !hide,
            swallow_press: reveal,
            recheck_after_secs: (playback && !hide && !held)
                .then(|| (CHROME_IDLE_HIDE_SECS - idle).max(0.0)),
        }
    }

    /// The phone layout is not on screen: nothing hides, nothing is
    /// swallowed.
    pub(crate) fn reset_visible(&mut self) {
        self.hidden = false;
        self.revealed_this_frame = false;
    }
}

/// The canvas's frame: its center and the viewport it sits in, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CanvasFrame {
    pub center: (f32, f32),
    pub viewport: (f32, f32),
}

/// How far to move the map's pan so it holds still while the chrome slides.
/// The map is centered on the canvas, so a canvas that grows when the chrome
/// hides would carry the map with it; shifting the pan by the opposite of the
/// center's move keeps every point of the map where it was. A changed
/// viewport (a rotation, a resize) is a new layout, not a slide: no shift.
pub(crate) fn hold_map_still(prev: Option<CanvasFrame>, now: CanvasFrame) -> (f32, f32) {
    match prev {
        Some(p) if p.viewport == now.viewport => {
            (p.center.0 - now.center.0, p.center.1 - now.center.1)
        }
        _ => (0.0, 0.0),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The mini timeline
// ───────────────────────────────────────────────────────────────────────────

/// A volume's length when the timeline has too few to measure (VCP 212's
/// is about four and a half minutes).
pub(crate) const DEFAULT_VOLUME_SECS: f64 = 300.0;

/// How many volumes the mini timeline shows.
pub(crate) const MINI_VOLUMES: f64 = 3.0;

/// The room right of now, as a share of the window, where the next volume's
/// ghost draws.
pub(crate) const MINI_LEAD_SHARE: f64 = 0.25;

/// The mini timeline's time window, in Unix seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MiniWindow {
    pub start: f64,
    pub end: f64,
}

impl MiniWindow {
    pub(crate) fn span(&self) -> f64 {
        self.end - self.start
    }
}

/// The median gap between volume starts, if there are enough to measure.
fn volume_secs(starts: &[f64]) -> Option<f64> {
    let mut gaps: Vec<f64> = starts
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|g| *g > 30.0)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(f64::total_cmp);
    Some(gaps[gaps.len() / 2])
}

/// A volume's typical length for the mini timeline: the median gap between
/// `volume_starts`, else [`DEFAULT_VOLUME_SECS`], within 2–15 minutes.
pub(crate) fn typical_volume_secs(volume_starts: &[f64]) -> f64 {
    volume_secs(volume_starts)
        .unwrap_or(DEFAULT_VOLUME_SECS)
        .clamp(120.0, 900.0)
}

/// The room right of now where the next volume's ghost and the cap draw:
/// the same seconds at every zoom stop.
pub(crate) fn mini_lead_secs(volume_starts: &[f64]) -> f64 {
    MINI_VOLUMES * typical_volume_secs(volume_starts) * MINI_LEAD_SHARE
}

/// Whether the mini timeline's window is anchored to now: pinned, or a
/// playhead within two volumes of now (and not past the ghost room).
pub(crate) fn mini_near_now(now: f64, playhead: f64, pinned: bool, volume_starts: &[f64]) -> bool {
    let vol = typical_volume_secs(volume_starts);
    pinned || (playhead <= now + mini_lead_secs(volume_starts) && now - playhead <= 2.0 * vol)
}

/// The mini timeline's window. `volume_starts` are the containers' starts,
/// oldest first (received, archived, live and the next volume's ghost).
///
/// Near now (pinned, or a playhead within two volumes of now) it shows the
/// last three volumes that have begun, with room right of now for the next
/// volume's ghost and the cap. A playhead further back gets its own three
/// volumes centered on it, and the now cap moves to the edge.
pub(crate) fn mini_window(
    now: f64,
    playhead: f64,
    pinned: bool,
    volume_starts: &[f64],
) -> MiniWindow {
    let vol = typical_volume_secs(volume_starts);
    let span = MINI_VOLUMES * vol;
    let lead = mini_lead_secs(volume_starts);
    if !mini_near_now(now, playhead, pinned, volume_starts) {
        return MiniWindow {
            start: playhead - span / 2.0,
            end: playhead + span / 2.0,
        };
    }
    let begun: Vec<f64> = volume_starts
        .iter()
        .copied()
        .filter(|&s| s <= now)
        .collect();
    let third_last = begun
        .len()
        .checked_sub(MINI_VOLUMES as usize)
        .map(|i| begun[i])
        .filter(|&s| now - s <= 2.0 * span);
    let mut start = third_last.unwrap_or(now - span);
    if !pinned {
        // Keep the playhead off the left edge.
        start = start.min(playhead - 0.1 * span);
    }
    MiniWindow {
        start,
        end: now + lead,
    }
}

/// How long a still finger must stay down to be a long-press.
pub(crate) const LONG_PRESS_SECS: f64 = 0.5;

/// How far a finger may move (points) and still be a tap or a long-press:
/// egui's drag threshold, so a press that has begun to scrub can never also
/// become a long-press.
pub(crate) const PRESS_SLOP_PX: f32 = 6.0;

/// One press on a surface that seeks on a tap, scrubs on a drag and opens
/// the inspector on a long-press.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PressTrack {
    start_secs: f64,
    start: (f32, f32),
    /// Moved past [`PRESS_SLOP_PX`]: a scrub for the rest of the press.
    scrubbing: bool,
    /// The long-press fired.
    fired: bool,
    /// A pinch took the press over: it neither seeks, scrubs nor
    /// long-presses for the rest of its life.
    cancelled: bool,
}

/// What a press did this frame.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct PressOut {
    /// The long-press fired this frame, at the press's start.
    pub long_press: Option<(f32, f32)>,
    /// The press is (still) a candidate for a long-press: ask for frames.
    pub waiting: bool,
    /// The press ended after its long-press: the release is not a tap.
    pub swallow_release: bool,
}

impl PressTrack {
    /// Advance a press. `down` is the finger on this surface; `pos` where it
    /// is. Returns the press to keep (none once released) and what it did.
    pub(crate) fn step(
        prev: Option<PressTrack>,
        down: bool,
        pos: Option<(f32, f32)>,
        now: f64,
    ) -> (Option<PressTrack>, PressOut) {
        let (true, Some(p)) = (down, pos) else {
            let swallow = prev.is_some_and(|t| t.fired || t.cancelled);
            return (
                None,
                PressOut {
                    swallow_release: swallow,
                    ..Default::default()
                },
            );
        };
        let mut t = prev.unwrap_or(PressTrack {
            start_secs: now,
            start: p,
            scrubbing: false,
            fired: false,
            cancelled: false,
        });
        let (dx, dy) = (p.0 - t.start.0, p.1 - t.start.1);
        if (dx * dx + dy * dy).sqrt() > PRESS_SLOP_PX {
            t.scrubbing = true;
        }
        let mut out = PressOut::default();
        if !t.scrubbing && !t.fired && !t.cancelled {
            if now - t.start_secs >= LONG_PRESS_SECS {
                t.fired = true;
                out.long_press = Some(t.start);
            } else {
                out.waiting = true;
            }
        }
        (Some(t), out)
    }

    /// The press fired its long-press: it neither seeks nor scrubs.
    #[cfg(test)]
    pub(crate) fn fired(&self) -> bool {
        self.fired
    }

    /// A pinch took this press over ([`Self::cancel`]).
    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled
    }

    /// Hand the press to a pinch: from now on it neither seeks, scrubs nor
    /// long-presses, and its release is not a tap.
    pub(crate) fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// The press may still seek: neither long-pressed nor taken by a pinch.
    pub(crate) fn seeks(&self) -> bool {
        !self.fired && !self.cancelled
    }
}

/// The one line a 3D link gets on the phone, which opens it in 2D.
pub(crate) const NO_3D_NOTICE: &str = "3D isn't available on the phone yet, so this opens in 2D";

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn vp(width: f32, height: f32, touch: bool) -> LayoutInputs {
        LayoutInputs {
            width,
            height,
            touch_seen: touch,
            coarse_pointer: false,
            forced: None,
        }
    }

    #[wasm_bindgen_test]
    fn a_portrait_phone_gets_the_phone_layout() {
        assert_eq!(
            device_layout(vp(390.0, 844.0, true)),
            DeviceLayout::Phone { landscape: false }
        );
        // Narrow enough without a touch, too.
        assert!(device_layout(vp(390.0, 844.0, false)).is_phone());
    }

    #[wasm_bindgen_test]
    fn a_landscape_phone_keeps_the_phone_layout_with_the_sheet_on_the_right() {
        let l = device_layout(vp(844.0, 390.0, true));
        assert_eq!(l, DeviceLayout::Phone { landscape: true });
        assert!(l.is_landscape_phone());
    }

    #[wasm_bindgen_test]
    fn a_coarse_pointer_decides_before_the_first_touch() {
        let first_frame = LayoutInputs {
            coarse_pointer: true,
            ..vp(844.0, 390.0, false)
        };
        assert!(device_layout(first_frame).is_phone());
        // A mouse on a short, wide desktop window stays desktop.
        assert_eq!(
            device_layout(vp(1280.0, 420.0, false)),
            DeviceLayout::Desktop
        );
    }

    #[wasm_bindgen_test]
    fn tablets_and_desktops_get_the_desktop_layout() {
        assert_eq!(
            device_layout(vp(768.0, 1024.0, true)),
            DeviceLayout::Desktop
        );
        assert_eq!(
            device_layout(vp(1440.0, 900.0, false)),
            DeviceLayout::Desktop
        );
        // A 550 px desktop window without touch stays desktop.
        assert_eq!(
            device_layout(vp(550.0, 900.0, false)),
            DeviceLayout::Desktop
        );
    }

    #[wasm_bindgen_test]
    fn the_sheet_rises_from_the_bottom_in_portrait() {
        let r = sheet_rect(390.0, 844.0, false, (47.0, 0.0, 34.0, 0.0));
        assert_eq!(r.x, 0.0);
        assert_eq!(r.w, 390.0);
        assert!((r.h - 844.0 * SHEET_PORTRAIT_SHARE).abs() < 0.01);
        assert!(
            (r.y + r.h - (844.0 - 34.0)).abs() < 0.01,
            "above the home bar"
        );
    }

    #[wasm_bindgen_test]
    fn the_sheet_sits_on_the_right_in_landscape() {
        let r = sheet_rect(844.0, 390.0, true, (0.0, 47.0, 21.0, 47.0));
        assert!((r.w - 844.0 * SHEET_LANDSCAPE_SHARE).abs() < 0.01);
        assert!(
            (r.x + r.w - (844.0 - 47.0)).abs() < 0.01,
            "inside the notch inset"
        );
        assert_eq!(r.y, 0.0);
        assert_eq!(r.h, 390.0 - 21.0);
        // A small landscape phone keeps the minimum width.
        let small = sheet_rect(568.0, 320.0, true, (0.0, 0.0, 0.0, 0.0));
        assert_eq!(small.w, SHEET_LANDSCAPE_MIN_W);
    }

    fn frame(now: f64, playing: bool, pinned: bool) -> AutoHideInputs {
        AutoHideInputs {
            now,
            playing,
            pinned,
            ..Default::default()
        }
    }

    #[wasm_bindgen_test]
    fn the_first_tether_never_hides_the_chrome() {
        let mut h = ChromeAutoHide::default();
        for t in 0..60 {
            let out = h.step(frame(t as f64, false, true));
            assert!(out.visible, "visible at {t} s on the boot tether");
            assert_eq!(out.recheck_after_secs, None);
        }
    }

    #[wasm_bindgen_test]
    fn playback_hides_the_chrome_after_five_seconds_not_at_once() {
        let mut h = ChromeAutoHide::default();
        // Paused for a long while, then Play at t = 100.
        assert!(h.step(frame(10.0, false, false)).visible);
        let out = h.step(frame(100.0, true, false));
        assert!(out.visible, "the idle clock starts at Play");
        assert_eq!(out.recheck_after_secs, Some(CHROME_IDLE_HIDE_SECS));
        assert!(h.step(frame(104.9, true, false)).visible);
        let out = h.step(frame(105.0, true, false));
        assert!(!out.visible);
        assert!(h.hidden);
        assert_eq!(out.recheck_after_secs, None);
        // Pausing brings it back.
        assert!(h.step(frame(106.0, false, false)).visible);
    }

    #[wasm_bindgen_test]
    fn a_tether_the_user_asks_for_counts_as_playback() {
        let mut h = ChromeAutoHide::default();
        h.step(frame(0.0, false, true)); // boot tether
        h.step(frame(5.0, false, false)); // the user detaches
        h.step(frame(20.0, false, true)); // and goes live again
        assert!(h.step(frame(24.0, false, true)).visible);
        assert!(!h.step(frame(25.0, false, true)).visible);
    }

    #[wasm_bindgen_test]
    fn a_press_reveals_hidden_chrome_and_is_swallowed() {
        let mut h = ChromeAutoHide::default();
        h.step(frame(0.0, true, false));
        assert!(!h.step(frame(6.0, true, false)).visible);
        let out = h.step(AutoHideInputs {
            pressed: true,
            ..frame(7.0, true, false)
        });
        assert!(out.visible);
        assert!(out.swallow_press);
        assert!(h.revealed_this_frame);
        // The next frame is an ordinary one, and the clock restarted at 7 s.
        let out = h.step(frame(11.9, true, false));
        assert!(out.visible && !out.swallow_press);
        assert!(!h.step(frame(12.0, true, false)).visible);
    }

    #[wasm_bindgen_test]
    fn a_press_on_visible_chrome_resets_the_timer_without_swallowing() {
        let mut h = ChromeAutoHide::default();
        h.step(frame(0.0, true, false));
        let out = h.step(AutoHideInputs {
            pressed: true,
            ..frame(4.0, true, false)
        });
        assert!(out.visible && !out.swallow_press);
        assert!(h.step(frame(8.0, true, false)).visible);
        assert!(!h.step(frame(9.0, true, false)).visible);
    }

    #[wasm_bindgen_test]
    fn a_menu_or_a_finger_keeps_the_chrome() {
        let mut h = ChromeAutoHide::default();
        h.step(frame(0.0, true, false));
        let menu = h.step(AutoHideInputs {
            menu_open: true,
            ..frame(30.0, true, false)
        });
        assert!(menu.visible);
        assert_eq!(menu.recheck_after_secs, None);
        let finger = h.step(AutoHideInputs {
            gesture_active: true,
            ..frame(31.0, true, false)
        });
        assert!(finger.visible);
        // Let go and nothing holds it.
        assert!(!h.step(frame(32.0, true, false)).visible);
    }

    #[wasm_bindgen_test]
    fn the_map_holds_still_while_the_chrome_slides_but_not_on_rotation() {
        let a = CanvasFrame {
            center: (195.0, 400.0),
            viewport: (390.0, 844.0),
        };
        // The chrome slid away: the canvas grew and its center moved 20 px.
        let b = CanvasFrame {
            center: (195.0, 420.0),
            ..a
        };
        assert_eq!(hold_map_still(Some(a), b), (0.0, -20.0));
        assert_eq!(hold_map_still(None, b), (0.0, 0.0));
        let rotated = CanvasFrame {
            center: (422.0, 195.0),
            viewport: (844.0, 390.0),
        };
        assert_eq!(hold_map_still(Some(a), rotated), (0.0, 0.0));
    }

    #[wasm_bindgen_test]
    fn pinned_the_mini_timeline_shows_the_last_three_volumes_and_the_ghost() {
        // Volumes every 300 s; the live one began at 1_200; the ghost at 1_500.
        let starts = [0.0, 300.0, 600.0, 900.0, 1_200.0, 1_500.0];
        let w = mini_window(1_320.0, 1_320.0, true, &starts);
        assert_eq!(w.start, 600.0, "three volumes that have begun");
        assert!((w.end - (1_320.0 + MINI_LEAD_SHARE * 900.0)).abs() < 1e-9);
        assert!(w.end > 1_500.0, "room for the ghost's first cut");
    }

    #[wasm_bindgen_test]
    fn with_no_volumes_the_window_still_ends_at_now() {
        let w = mini_window(10_000.0, 10_000.0, true, &[]);
        assert_eq!(
            w.span(),
            3.0 * DEFAULT_VOLUME_SECS * (1.0 + MINI_LEAD_SHARE)
        );
        assert!(w.end > 10_000.0);
    }

    #[wasm_bindgen_test]
    fn detached_near_now_keeps_now_in_view_and_the_playhead_off_the_edge() {
        let starts = [0.0, 300.0, 600.0, 900.0, 1_200.0];
        let w = mini_window(1_320.0, 800.0, false, &starts);
        assert_eq!(w.start, 600.0);
        assert!(w.start < 800.0 - 0.05 * 900.0);
        assert!(w.end > 1_320.0);
    }

    #[wasm_bindgen_test]
    fn far_from_now_the_window_centers_on_the_playhead() {
        let starts = [0.0, 300.0, 600.0, 900.0];
        let w = mini_window(100_000.0, 600.0, false, &starts);
        assert_eq!(w.start, 600.0 - 450.0);
        assert_eq!(w.end, 600.0 + 450.0);
    }

    #[wasm_bindgen_test]
    fn a_still_hold_is_a_long_press_once() {
        let (t, out) = PressTrack::step(None, true, Some((10.0, 10.0)), 0.0);
        assert!(out.waiting && out.long_press.is_none());
        let (t, out) = PressTrack::step(t, true, Some((12.0, 11.0)), 0.3);
        assert!(out.waiting);
        let (t, out) = PressTrack::step(t, true, Some((12.0, 11.0)), 0.5);
        assert_eq!(out.long_press, Some((10.0, 10.0)));
        assert!(t.unwrap().fired());
        let (t, out) = PressTrack::step(t, true, Some((12.0, 11.0)), 0.9);
        assert_eq!(out.long_press, None, "fires once");
        assert!(!out.waiting);
        let (t, out) = PressTrack::step(t, false, None, 1.0);
        assert!(t.is_none());
        assert!(out.swallow_release, "the release is not a tap");
    }

    #[wasm_bindgen_test]
    fn a_press_that_starts_to_scrub_never_becomes_a_long_press() {
        let (t, _) = PressTrack::step(None, true, Some((10.0, 10.0)), 0.0);
        let (t, _) = PressTrack::step(t, true, Some((18.0, 10.0)), 0.1);
        // Back over the start and held: still a scrub.
        let (t, out) = PressTrack::step(t, true, Some((10.0, 10.0)), 2.0);
        assert_eq!(out.long_press, None);
        assert!(!out.waiting);
        let (_, out) = PressTrack::step(t, false, None, 2.1);
        assert!(!out.swallow_release);
    }

    #[wasm_bindgen_test]
    fn a_forced_layout_wins() {
        let forced_desktop = LayoutInputs {
            forced: Some(false),
            ..vp(390.0, 844.0, true)
        };
        assert_eq!(device_layout(forced_desktop), DeviceLayout::Desktop);
        let forced_phone = LayoutInputs {
            forced: Some(true),
            ..vp(1440.0, 900.0, false)
        };
        assert_eq!(
            device_layout(forced_phone),
            DeviceLayout::Phone { landscape: true }
        );
    }
}
