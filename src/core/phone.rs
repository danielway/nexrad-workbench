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
