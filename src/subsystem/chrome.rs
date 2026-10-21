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
//! detection and other preference-style state stay on
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
    /// The browser reports a touch screen, read at start-up (so a phone gets
    /// the phone layout before its first touch) and again whenever the
    /// viewport changes size (a rotation, or an emulator's device toggle).
    pub touch_screen: bool,
    /// The viewport size `touch_screen` was last read at.
    pub touch_screen_read_at: Option<(f32, f32)>,
    /// Whether the keyboard shortcut help overlay is open.
    pub shortcuts_help_visible: bool,
    /// Whether the site selection modal is open.
    pub site_modal_open: bool,
    /// The destructive confirmation on screen, if any (one-instrument §12).
    pub confirm: Option<crate::core::dialogs::Confirm>,
    /// The reset confirmation's "also forget the mPING key and preferred
    /// site" choice. Off each time the confirmation opens.
    pub reset_forgets_key_and_site: bool,
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
    /// pending `(start, end)` selection range to bulk-download on confirm,
    /// with its cost priced at release. `Some` doubles as the open flag; the
    /// range is the modal's own snapshot, independent of the live selection.
    pub range_download_modal: Option<crate::core::acquisition::RangeDownloadPrompt>,
    /// Whether the VCP forecast diagnostics modal is open.
    pub vcp_forecast_open: bool,
    /// Whether the activity sheet (spec §5/§10: pipeline stages, active /
    /// queued / recent downloads, throughput, and acquisition policy toggles)
    /// is open. Opened by the ambient activity chip in either layout.
    pub activity_sheet_open: bool,
    /// The app's error count (`ErrorContext::pushed_count`) when the
    /// activity sheet last showed the errors; the chip counts the rest
    /// ([`crate::core::activity::errors_acked`]).
    pub errors_acked: u64,
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
    /// Whether the phone's sheet (the rail's tabs, opened from the
    /// transport row) is open. Its tab is [`Self::rail_tab`].
    pub mobile_settings_open: bool,
    /// The phone chrome's auto-hide (one-instrument §13), decided by
    /// [`crate::core::phone::ChromeAutoHide::step`] once a frame before the
    /// layout, and read by the phone bars and the canvas. Inert on desktop.
    pub mobile_auto_hide: crate::core::phone::ChromeAutoHide,
    /// The phone strip's zoom stop (phone timeline §1). Cuts by default;
    /// not persisted and not in the link. Changed only through
    /// [`crate::core::phone_strip::strip_zoom_after`].
    pub phone_strip_zoom: crate::core::phone_strip::StripZoom,
    /// The phone's calendar sheet, "Go to a day" (phone timeline §2): the
    /// month or year on screen. `Some` doubles as the open flag. Changed
    /// only by [`crate::core::Intent::OpenGoToDay`],
    /// [`crate::core::Intent::Calendar`] and closing it.
    pub go_to_day: Option<crate::core::calendar::CalendarSheet>,
    /// The phone strip's long-press sheet, with the time and scan it caught
    /// (phone timeline §4). `Some` doubles as the open flag.
    pub press_sheet: Option<crate::core::phone_range::PressSheet>,
    /// The phone's chip sheet is open.
    pub chip_sheet_open: bool,
    /// A day landed on from the calendar sheet that may still move to its
    /// stormiest volume once its listing arrives
    /// ([`crate::core::calendar::settle_landing`]).
    pub go_to_day_landing: Option<crate::core::calendar::Landing>,
    /// The phone strip's window and lane width this frame, written by the
    /// strip, read back by it as the last window (so a detached window holds
    /// still) and by the storm-activity pump at the zoomed-out stops.
    pub phone_strip_view: Option<(crate::core::phone::MiniWindow, f32)>,
    /// The phone strip owns a two-finger gesture this frame: the map must
    /// not zoom or pan on it. Reset before the strip draws each frame.
    pub phone_strip_claims_touch: bool,
    /// The frame under the playhead is on its way (the row decided it,
    /// [`crate::core::phone_row::LoadingVm`]): written by the strip each
    /// frame, read by the map's dim and pill, the chip and the top bar's
    /// clock.
    pub phone_loading: Option<crate::core::phone_row::LoadingVm>,
    /// The row shows now this frame (the LIVE button then needs no jump).
    pub phone_now_in_view: bool,
    /// The start of the volume under the playhead, for the chip's point.
    pub phone_selected_start: Option<f64>,
    /// The canvas's frame on the last phone frame, so the map can hold still
    /// while the chrome slides ([`crate::core::phone::hold_map_still`]).
    pub last_canvas: Option<crate::core::phone::CanvasFrame>,
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
