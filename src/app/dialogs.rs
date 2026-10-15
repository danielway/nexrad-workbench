//! Shell side of the dialog stack: read the open flags into
//! [`DialogFlags`], keep [`crate::subsystem::Chrome::dialogs`] in step, and
//! execute [`DialogIntent`]s by writing the flags the dialogs render from.
//!
//! The decisions — what Escape closes, what is dismissible, what a
//! confirmation does, whether an event is saveable — are the core's
//! ([`crate::core::dialogs`]); this file only maps them onto the flags.

use super::command_dispatch::DispatchState;
use crate::core::diagnostics::DiagnosticsIntent;
use crate::core::dialogs::{confirmed_intent, validate_event, DialogFlags, DialogId, DialogIntent};
use crate::subsystem::{Chrome, Diagnostics};

/// What is open this frame, read off the chrome and overlay flags.
pub(crate) fn dialog_flags(chrome: &Chrome, diagnostics: &Diagnostics) -> DialogFlags {
    DialogFlags {
        site_picker: chrome.site_modal_open,
        site_required: chrome.site_required,
        pick_site_on_map: chrome.site_map_pick.is_some(),
        shortcuts_help: chrome.shortcuts_help_visible,
        range_download: chrome.range_download_modal.is_some(),
        activity_sheet: chrome.activity_sheet_open,
        scan_inspector: chrome.scan_inspector.is_some(),
        vcp_forecast: chrome.vcp_forecast_open,
        event: chrome.event_modal_open,
        alerts_list: diagnostics.alerts.list_modal_open,
        alert_detail: diagnostics.alerts.selected_alert_id.is_some(),
        mping: diagnostics.mping.settings_modal_open,
        mobile_settings: chrome.mobile_settings_open,
        confirm: chrome.confirm.is_some(),
    }
}

/// Bring the stack in step with the flags. Called before the layout renders
/// and around every dialog intent.
pub(crate) fn sync_dialogs(chrome: &mut Chrome, diagnostics: &Diagnostics) {
    // The Storage section's Reset button still raises this flag; it opens
    // the shared confirmation.
    if std::mem::take(&mut chrome.reset_settings_modal_open) {
        chrome.confirm = Some(crate::core::dialogs::Confirm::ResetSettings);
    }
    let open = dialog_flags(chrome, diagnostics).open();
    chrome.dialogs.sync(&open);
}

impl DispatchState<'_> {
    /// Apply one dialog intent.
    pub(crate) fn dialog(&mut self, intent: DialogIntent) {
        sync_dialogs(self.chrome, self.diagnostics);
        match intent {
            DialogIntent::Escape => {
                if let Some(id) = self.chrome.dialogs.escape() {
                    self.close_dialog(id);
                }
            }
            DialogIntent::Close(id) => {
                let stack = &self.chrome.dialogs;
                if !stack.contains(id) || stack.dismissible(id) {
                    self.close_dialog(id);
                }
            }
            DialogIntent::RequestConfirm(confirm) => self.chrome.confirm = Some(confirm),
            DialogIntent::Confirm => {
                if let Some(confirm) = self.chrome.confirm.take() {
                    self.state.push_command(confirmed_intent(confirm));
                }
            }
            DialogIntent::SaveEvent {
                editing,
                name,
                site_id,
                start,
                end,
            } => {
                if validate_event(&name, start, end).is_ok() {
                    let name = name.trim().to_string();
                    match editing {
                        Some(id) => self.state.saved_events.update(id, name, start, end),
                        None => self.state.saved_events.add(name, site_id, start, end),
                    }
                    self.close_dialog(DialogId::Event);
                }
            }
            DialogIntent::DeleteEvent(id) => {
                self.state.saved_events.remove(id);
                if self.chrome.event_modal_editing_id == Some(id) {
                    self.close_dialog(DialogId::Event);
                }
            }
            DialogIntent::PickSiteOnMap => {
                let geo = &mut self.state.layer_state.geo;
                if self.chrome.site_map_pick.is_none() {
                    self.chrome.site_map_pick = Some(geo.nexrad_sites);
                }
                geo.nexrad_sites = true;
                self.chrome.site_modal_open = false;
                self.state
                    .toasts
                    .notice("Click a site on the map \u{00b7} Esc cancels");
            }
        }
        sync_dialogs(self.chrome, self.diagnostics);
    }

    /// "Loop from here → now": a range from the scan to now with the
    /// playhead at the scan. The inspector closes only when the range was
    /// made; otherwise a toast says why (it is not hidden behind the
    /// inspector).
    pub(crate) fn loop_from_scan(&mut self, scan_start: f64) {
        let now = self.state.frame_now.secs();
        // The same span rule the strip's range gesture obeys.
        if !crate::core::selection_span_allowed(scan_start, now) {
            self.state
                .toasts
                .failure("That scan is more than 7 days back; a loop can span 7 days at most");
            return;
        }
        let pause = self.state.pause_stream_while_reviewing;
        if self
            .live
            .select_range(&mut self.playback.state, scan_start, now, true, now, pause)
        {
            self.playback.state.seek_within_range(scan_start);
            // Arm the bulk fetch for the span (selection = the fetch).
            self.state.selection_just_finalized = self.playback.state.selection_range();
            self.chrome.scan_inspector = None;
        } else {
            self.state
                .toasts
                .failure("Couldn't make a loop from that scan");
        }
    }

    /// Close an overlay dialog through the diagnostics reducer (its close
    /// intents are plain state writes with no effects).
    fn diagnostics_close(&mut self, intent: DiagnosticsIntent) {
        let effects = crate::core::diagnostics::reduce(
            crate::core::diagnostics::DiagnosticsStateMut {
                alerts: &mut self.diagnostics.alerts,
                mping: &mut self.diagnostics.mping,
                gps: &mut self.diagnostics.gps,
                gps_layer_active: &mut self.state.layer_state.geo.gps_location,
            },
            intent,
        );
        debug_assert!(effects.is_empty(), "closing a dialog performs no I/O");
    }

    /// Close one entry by clearing the flag it renders from.
    fn close_dialog(&mut self, id: DialogId) {
        let c = &mut *self.chrome;
        match id {
            DialogId::SitePicker => c.site_modal_open = false,
            DialogId::PickSiteOnMap => {
                if let Some(was_on) = c.site_map_pick.take() {
                    self.state.layer_state.geo.nexrad_sites = was_on;
                }
                // A first visit still needs a site: back to the picker.
                if c.site_required {
                    c.site_modal_open = true;
                }
            }
            DialogId::ShortcutsHelp => c.shortcuts_help_visible = false,
            DialogId::RangeDownload => c.range_download_modal = None,
            DialogId::ActivitySheet => c.activity_sheet_open = false,
            DialogId::ScanInspector => c.scan_inspector = None,
            DialogId::VcpForecast => c.vcp_forecast_open = false,
            DialogId::Event => {
                c.event_modal_open = false;
                c.event_modal_editing_id = None;
            }
            DialogId::AlertsList => self.diagnostics_close(DiagnosticsIntent::CloseAlertList),
            DialogId::AlertDetail => self.diagnostics_close(DiagnosticsIntent::ClearAlertSelection),
            DialogId::Mping => self.diagnostics_close(DiagnosticsIntent::CloseMpingSettings),
            DialogId::MobileSettings => c.mobile_settings_open = false,
            DialogId::Confirm => c.confirm = None,
        }
    }
}
