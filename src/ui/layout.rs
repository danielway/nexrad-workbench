//! Declarative layer tree for the app's chrome panels and modals.
//!
//! Modeled on the corner-chrome [`Overlay`](super::canvas_overlays::Overlay)
//! pattern: each panel and modal is a zero-sized marker type that impls
//! [`Layer`], and the per-frame [`render_layout`] dispatcher iterates a
//! static slice in z-order, calling each layer's [`Layer::visible`]
//! predicate before [`Layer::render`].
//!
//! The seam is bounded:
//!
//! - **In scope:** the 6 chrome panels (top/bottom/left/right desktop,
//!   mobile_top, mobile_chrome) and the 10 modals. Each had 3–10 subsystem
//!   refs threaded through `main.rs` and an early-return guard checked
//!   inside the function body. Both collapse into the trait.
//! - **Out of scope:** `render_canvas_with_geo` (owns `CentralPanel`,
//!   threads geo_layers + gpu state not carried here) and `handle_shortcuts`
//!   (input consumption, not rendering). Both stay as explicit calls in
//!   `WorkbenchApp::update`.
//!
//! Layouts (mobile vs desktop) are two local slices built inside
//! [`render_layout`]. There is intentionally no `LayoutProvider` trait —
//! a single function suffices for two layouts, and a third layout
//! (tablet, kiosk) would justify the trait if it ever lands.
//!
//! The slice itself lives for the call's stack frame; each entry is a
//! `&'static` reference to a zero-sized marker. This matches the
//! [`render_chrome_overlays`](super::canvas_overlays) pattern and avoids
//! the `Sync` bound that a top-level `static` would require.
//!
//! ## Borrow contract
//!
//! [`LayoutCtx`] carries `&mut` references to every subsystem each panel
//! or modal might touch. Each `Layer::render` impl destructures the ctx
//! and reborrows only the fields it needs — Rust's disjoint-field-borrow
//! support keeps this sound. Adding a new field to `LayoutCtx` is the
//! only friction point; it ripples nowhere because impls already
//! destructure by name.

use super::alerts_modal::AlertsModalsLayer;
use super::bottom_panel::BottomPanelLayer;
use super::event_modal::EventModalLayer;
use super::left_panel::LeftPanelLayer;
use super::mobile::{MobileChromeLayer, MobileSettingsModalLayer, MobileTopBarLayer};
use super::mping_modal::MpingModalLayer;
use super::network_panel::NetworkLogLayer;
use super::right_panel::RightPanelLayer;
use super::shortcuts::ShortcutsHelpLayer;
use super::site_modal::SiteModalLayer;
use super::stats_modal::StatsModalLayer;
use super::top_bar::TopBarLayer;
use super::vcp_forecast_modal::VcpForecastModalLayer;
use super::wipe_modal::WipeModalLayer;
use crate::state::AppState;
use crate::subsystem::{Acquisition, Chrome, Derived, Diagnostics, Live, Playback, Timeline};
use crate::ui::modal_states::ModalStates;
use eframe::egui;

/// Distinguishes egui constraints on render order.
///
/// `Chrome` calls `SidePanel::show` or `TopBottomPanel::show` and must
/// run before any `CentralPanel`. `Modal` calls `egui::Window::show` and
/// can run anywhere afterward.
///
/// Within a kind, the static-slice order must match ascending
/// [`Layer::z_order`] (debug-checked in [`render_layout`]).
///
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    Chrome,
    Modal,
}

/// Per-frame context every layer can read.
///
/// Mutable references throughout because layers update subsystem state
/// (chrome toggles, playback transport, live filter sync). Disjoint
/// field access is the borrow contract — impls destructure the ctx and
/// reborrow individual fields.
pub struct LayoutCtx<'a> {
    pub ctx: &'a egui::Context,
    pub state: &'a mut AppState,
    pub timeline: &'a Timeline,
    pub live: &'a mut Live,
    pub playback: &'a mut Playback,
    pub acquisition: &'a mut Acquisition,
    pub chrome: &'a mut Chrome,
    pub diagnostics: &'a mut Diagnostics,
    pub derived: &'a Derived,
    pub modals: &'a mut ModalStates,
}

/// A chrome panel or modal in the declarative layout tree.
///
/// Implementors are zero-sized marker types; behavior lives in the impl
/// (matching the [`Overlay`](super::canvas_overlays::Overlay) pattern).
pub trait Layer {
    /// Which egui dispatch slot this layer occupies. Chrome layers run
    /// before any modal layer in a single frame.
    fn kind(&self) -> LayerKind;

    /// Lower draws / dispatches earlier within the same kind. Z-order
    /// is data, not order-of-call.
    fn z_order(&self) -> i32;

    /// Whether this layer should render this frame. Default: always.
    /// Modals override this with their open-flag predicate, absorbing
    /// what used to be a per-function early-return guard.
    fn visible(&self, _ctx: &LayoutCtx) -> bool {
        true
    }

    /// Render the layer. `ctx` carries every subsystem; impls
    /// destructure to take only the fields they touch.
    fn render(&self, ctx: &mut LayoutCtx);
}

/// Dispatch the layout for this device once per frame.
///
/// Builds a local slice of chrome panels + modals based on `is_mobile`,
/// then walks it in order. The slice contract: all `Chrome` layers
/// appear before any `Modal` layers, and within each kind the order
/// matches ascending `z_order`. Both invariants are debug-checked.
pub fn render_layout(is_mobile: bool, ctx: &mut LayoutCtx) {
    let layers: &[&dyn Layer] = if is_mobile {
        &[
            // Chrome: 2 panels above the canvas.
            &MobileTopBarLayer, // z=10
            &MobileChromeLayer, // z=20
            // Modals: same set as desktop, in z-order.
            &SiteModalLayer,           // z=10
            &ShortcutsHelpLayer,       // z=20
            &WipeModalLayer,           // z=30
            &StatsModalLayer,          // z=40
            &VcpForecastModalLayer,    // z=50
            &NetworkLogLayer,          // z=60
            &EventModalLayer,          // z=70
            &AlertsModalsLayer,        // z=80
            &MpingModalLayer,          // z=90
            &MobileSettingsModalLayer, // z=100
        ]
    } else {
        &[
            // Chrome: 4 panels around the canvas.
            &TopBarLayer,      // z=10
            &BottomPanelLayer, // z=20
            &LeftPanelLayer,   // z=30
            &RightPanelLayer,  // z=40
            // Modals: same set as mobile, in z-order.
            &SiteModalLayer,           // z=10
            &ShortcutsHelpLayer,       // z=20
            &WipeModalLayer,           // z=30
            &StatsModalLayer,          // z=40
            &VcpForecastModalLayer,    // z=50
            &NetworkLogLayer,          // z=60
            &EventModalLayer,          // z=70
            &AlertsModalsLayer,        // z=80
            &MpingModalLayer,          // z=90
            &MobileSettingsModalLayer, // z=100
        ]
    };

    debug_assert!(
        is_kind_partitioned(layers),
        "layout slice must list all Chrome layers before any Modal layers",
    );
    debug_assert!(
        is_z_order_ascending_within_kind(layers),
        "layout slice must be sorted by z_order ascending within each kind",
    );

    for layer in layers {
        if layer.visible(ctx) {
            layer.render(ctx);
        }
    }
}

fn is_kind_partitioned(layers: &[&dyn Layer]) -> bool {
    let mut seen_modal = false;
    for layer in layers {
        match layer.kind() {
            LayerKind::Chrome => {
                if seen_modal {
                    return false;
                }
            }
            LayerKind::Modal => seen_modal = true,
        }
    }
    true
}

fn is_z_order_ascending_within_kind(layers: &[&dyn Layer]) -> bool {
    let mut prev_chrome: Option<i32> = None;
    let mut prev_modal: Option<i32> = None;
    for layer in layers {
        let z = layer.z_order();
        let slot = match layer.kind() {
            LayerKind::Chrome => &mut prev_chrome,
            LayerKind::Modal => &mut prev_modal,
        };
        if let Some(prev) = *slot {
            if z < prev {
                return false;
            }
        }
        *slot = Some(z);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Marker {
        kind: LayerKind,
        z: i32,
    }

    impl Layer for Marker {
        fn kind(&self) -> LayerKind {
            self.kind
        }
        fn z_order(&self) -> i32 {
            self.z
        }
        fn render(&self, _ctx: &mut LayoutCtx) {}
    }

    #[test]
    fn kind_partitioned_accepts_chrome_then_modal() {
        let a = Marker {
            kind: LayerKind::Chrome,
            z: 10,
        };
        let b = Marker {
            kind: LayerKind::Modal,
            z: 10,
        };
        let layers: Vec<&dyn Layer> = vec![&a, &b];
        assert!(is_kind_partitioned(&layers));
    }

    #[test]
    fn kind_partitioned_rejects_modal_before_chrome() {
        let a = Marker {
            kind: LayerKind::Modal,
            z: 10,
        };
        let b = Marker {
            kind: LayerKind::Chrome,
            z: 10,
        };
        let layers: Vec<&dyn Layer> = vec![&a, &b];
        assert!(!is_kind_partitioned(&layers));
    }

    #[test]
    fn z_order_ascending_within_kind_independently() {
        let a = Marker {
            kind: LayerKind::Chrome,
            z: 30,
        };
        let b = Marker {
            kind: LayerKind::Modal,
            z: 10,
        };
        let layers: Vec<&dyn Layer> = vec![&a, &b];
        assert!(is_z_order_ascending_within_kind(&layers));
    }

    #[test]
    fn z_order_ascending_within_kind_rejects_descent() {
        let a = Marker {
            kind: LayerKind::Chrome,
            z: 30,
        };
        let b = Marker {
            kind: LayerKind::Chrome,
            z: 10,
        };
        let layers: Vec<&dyn Layer> = vec![&a, &b];
        assert!(!is_z_order_ascending_within_kind(&layers));
    }
}
