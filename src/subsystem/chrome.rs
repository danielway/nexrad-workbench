//! Chrome subsystem: UI shell visibility flags + modal-open booleans.
//!
//! Owns the "is this panel/modal showing right now?" state that's
//! distinct from the domain data UI panels render. Folding these flags
//! together gives panels a single typed handle for chrome decisions
//! (sidebar layout, which modal to render) instead of reading scattered
//! booleans off [`AppState`](crate::state::AppState).
//!
//! **Placement rule** (the other half is on [`crate::ui::ModalStates`]):
//! transient UI state splits on one question — *is it "what is on screen"
//! or "what has the user typed"?*
//!
//! - **What is on screen** — which panel/modal is open, which tab is
//!   active, what a modal was opened *for* — lives here. Both the UI (a
//!   toggle) and the shell (an effect opening the site modal) write it,
//!   and the layout tree reads it to decide what to render. It sits in
//!   `subsystem` rather than `ui` precisely so both layers can touch it
//!   without an illegal `app → ui` edge.
//! - **What the user has typed** — search filters, form fields, text
//!   buffers two-way bound to egui widgets — lives on `ui::ModalStates`.
//!   Nothing outside the owning widget reads it.
//!
//! Anything that is neither is domain state: theme, dev mode, mobile
//! detection, advanced mode and other preference-style state stay on
//! `AppState` (or `UserPreferences`), because business logic reads them
//! in many places that have nothing to do with chrome rendering.

/// Owner of UI chrome visibility + modal-open state.
#[derive(Default)]
pub(crate) struct Chrome {
    /// Whether the panel is open beside its rail (desktop only). Folded, the
    /// rail's 44 px of tab icons stays.
    pub right_sidebar_visible: bool,
    /// The panel's open tab (one-instrument §8).
    pub rail_tab: crate::core::panels::RailTab,
    /// The chrome this frame's viewport gets (one-instrument §13): the
    /// desktop panels, or the phone's, with the sheet on the right in
    /// landscape. Decided by [`crate::core::phone::device_layout`].
    pub device_layout: crate::core::phone::DeviceLayout,
    /// The browser reports a coarse primary pointer, read once at start-up
    /// so a phone gets the phone layout before its first touch.
    pub coarse_pointer: bool,
    /// Whether the keyboard shortcut help overlay is open.
    pub shortcuts_help_visible: bool,
    /// Whether the site selection modal is open.
    pub site_modal_open: bool,
    /// The destructive confirmation on screen, if any (one-instrument §12).
    pub confirm: Option<crate::core::dialogs::Confirm>,
    /// The open dialogs and popovers in stacking order: what Escape closes
    /// and whether global shortcuts fire. Synced from the flags each frame.
    pub dialogs: crate::core::dialogs::DialogStack,
    /// No site has been chosen yet (first visit): the site picker cannot be
    /// dismissed. Cleared by every site pick — list, search, zip, location
    /// or map.
    pub site_required: bool,
    /// "Pick on map" is armed: the Sites layer is on until a site is clicked.
    /// Holds whether the layer was on before, to restore it.
    pub site_map_pick: Option<bool>,
    /// When set, the range-download confirm modal is open and carries the
    /// pending `(start, end)` selection range to bulk-download on confirm.
    /// `Some` doubles as the open flag; the range is the modal's own snapshot,
    /// independent of the live selection.
    pub range_download_modal: Option<(f64, f64)>,
    /// Whether the VCP forecast diagnostics modal is open.
    pub vcp_forecast_open: bool,
    /// Whether the activity sheet (spec §5/§10: pipeline stages, active /
    /// queued / recent downloads, throughput, and acquisition policy toggles)
    /// is open. Opened by the ambient activity chip in either layout.
    pub activity_sheet_open: bool,
    /// Whether the activity sheet's Details disclosure is expanded. Real state
    /// rather than egui memory so it survives a re-layout and is testable.
    pub activity_details_open: bool,
    /// When set, the scan inspector (spec §5/§6.3: full per-scan volume
    /// breakdown — every tilt with cache state, size, chunk progress, and
    /// tap-to-fetch) is open for the scan whose container start time (Unix
    /// seconds) this holds. Opened by right-click (desktop) / long-press
    /// (touch) on a scan container. `Some` doubles as the open flag.
    pub scan_inspector: Option<f64>,
    /// Whether the event create/edit modal is open.
    pub event_modal_open: bool,
    /// Event ID being edited (`None` = creating new).
    pub event_modal_editing_id: Option<u64>,
    /// Whether the mobile settings modal (opened from the bottom-bar
    /// ellipsis) is currently visible.
    pub mobile_settings_open: bool,
    /// Active tab inside the mobile settings modal.
    pub mobile_settings_tab: super::super::state::MobileSettingsTab,
    /// Latched when the mobile bottom-bar location button is tapped.
    /// The main update loop consumes this flag and kicks off
    /// geolocation against `SiteModalState`.
    pub mobile_geolocate_requested: bool,
    /// Mobile chrome auto-hide bookkeeping (spec §13 phone: chrome auto-hides
    /// during playback, tap to reveal). The mobile layout reads this each frame
    /// to decide whether to draw the top bar / bottom chrome; the canvas updates
    /// it on a reveal tap. Inert on desktop (desktop chrome never auto-hides).
    pub mobile_auto_hide: super::super::state::MobileChromeAutoHide,
}

impl Chrome {
    /// Construct with the default-visible flags pre-set.
    ///
    /// The desktop panel starts open on View; everything else starts
    /// hidden.
    pub(crate) fn new() -> Self {
        Self {
            right_sidebar_visible: true,
            ..Default::default()
        }
    }
}
