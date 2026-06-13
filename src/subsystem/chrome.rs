//! Chrome subsystem: UI shell visibility flags + modal-open booleans.
//!
//! Owns the "is this panel/modal showing right now?" state that's
//! distinct from the domain data UI panels render. Folding these flags
//! together gives panels a single typed handle for chrome decisions
//! (sidebar layout, which modal to render) instead of reading scattered
//! booleans off [`AppState`](crate::state::AppState).
//!
//! Fields here are deliberately limited to "what's open / visible".
//! Theme, dev mode, mobile detection, advanced mode, and other
//! preference-style state stays on `AppState` (or `UserPreferences`)
//! because business logic reads them in many places that have nothing
//! to do with chrome rendering.

/// Owner of UI chrome visibility + modal-open state.
#[derive(Default)]
pub struct Chrome {
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
    /// Whether the user-facing queue sheet (spec §5/§10: active/queued/recent
    /// downloads + acquisition policy toggles) is open. Opened from the status
    /// chip near the transport (desktop) or the mobile top-bar acquiring
    /// indicator. Distinct from the dev-only acquisition drawer.
    pub queue_sheet_open: bool,
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
}

impl Chrome {
    /// Construct with the default-visible flags pre-set.
    ///
    /// The two desktop sidebars start visible; everything else starts
    /// hidden. Matches the previous `AppState` defaults.
    pub fn new() -> Self {
        Self {
            left_sidebar_visible: true,
            right_sidebar_visible: true,
            ..Default::default()
        }
    }
}
