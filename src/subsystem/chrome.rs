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
    /// Whether the left sidebar is visible (desktop only).
    pub left_sidebar_visible: bool,
    /// Whether the right sidebar is visible (desktop only).
    pub right_sidebar_visible: bool,
    /// Whether the keyboard shortcut help overlay is open.
    pub shortcuts_help_visible: bool,
    /// Whether the site selection modal is open.
    pub site_modal_open: bool,
    /// Whether the wipe-all-data confirmation modal is open.
    pub wipe_modal_open: bool,
    /// When set, the range-download confirm modal is open and carries the
    /// pending `(start, end)` selection range to bulk-download on confirm.
    /// `Some` doubles as the open flag; the range is the modal's own snapshot,
    /// independent of the live selection.
    pub range_download_modal: Option<(f64, f64)>,
    /// Whether the stats/perf modal is open.
    pub stats_detail_open: bool,
    /// Whether the VCP forecast diagnostics modal is open.
    pub vcp_forecast_open: bool,
    /// Whether the network log modal is open.
    pub network_log_open: bool,
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
    /// The two desktop sidebars start visible; everything else starts
    /// hidden. Matches the previous `AppState` defaults.
    pub(crate) fn new() -> Self {
        Self {
            left_sidebar_visible: true,
            right_sidebar_visible: true,
            ..Default::default()
        }
    }
}
