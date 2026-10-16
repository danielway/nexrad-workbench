//! Dialogs as one instrument ("One Instrument" §12): which dialogs and
//! popovers are open, in what order, what Escape closes, which keyboard
//! shortcuts still fire, and what a destructive confirmation says.
//!
//! **The stack.** Every open dialog or popover is one entry in a
//! [`DialogStack`], ordered by when it opened (the last is on top). The shell
//! does not push and pop it by hand: each frame it describes what is open as
//! a [`DialogFlags`] snapshot and [`DialogStack::sync`] reconciles the order —
//! entries that closed drop out wherever they were, newly opened ones go on
//! top. That keeps the dozen places that open a dialog (a button, a shortcut,
//! an effect, a deep link) untouched while giving one answer to "what is on
//! top".
//!
//! - **Escape** closes the top entry only ([`DialogStack::escape`]); a
//!   non-dismissible entry (the first-visit site picker) swallows it.
//! - **Shortcuts** are suppressed while anything is open, except those the
//!   top entry owns ([`DialogStack::allows`]).
//! - **Generations** let a dialog reset its form when it is opened again
//!   ([`DialogStack::generation`]) without a close-time hook.
//!
//! **Confirmations.** Destructive actions (clear cache, reset settings,
//! delete event) open a [`Confirm`]; [`confirm_vm`] lists exactly what goes
//! and what stays, and [`confirmed_intent`] is what confirming does.

use crate::core::session::Clock;
use crate::core::time_field::format_time_field;
use crate::core::Intent;

/// One kind of dialog or popover. Each appears in the stack at most once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DialogId {
    /// The site picker popover under the top bar's site control.
    SitePicker,
    /// "Pick on map": the Sites layer is on until a site is clicked.
    PickSiteOnMap,
    /// The keyboard map.
    ShortcutsHelp,
    /// The long-range download confirmation.
    RangeDownload,
    /// The activity sheet.
    ActivitySheet,
    /// The scan inspector popover.
    ScanInspector,
    /// The VCP forecast diagnostics (developer).
    VcpForecast,
    /// The event create / edit dialog.
    Event,
    /// The alerts list.
    AlertsList,
    /// One alert's detail (back to the list when the list is open).
    AlertDetail,
    /// The mPING key dialog.
    Mping,
    /// The phone's settings sheet.
    MobileSettings,
    /// A destructive confirmation ([`Confirm`]).
    Confirm,
}

impl DialogId {
    /// Every id, in the order [`DialogFlags::open`] reports simultaneous
    /// opens (a stable tie-break for entries that open on the same frame).
    pub(crate) const ALL: [DialogId; 13] = [
        DialogId::SitePicker,
        DialogId::PickSiteOnMap,
        DialogId::ShortcutsHelp,
        DialogId::RangeDownload,
        DialogId::ActivitySheet,
        DialogId::ScanInspector,
        DialogId::VcpForecast,
        DialogId::Event,
        DialogId::AlertsList,
        DialogId::AlertDetail,
        DialogId::Mping,
        DialogId::MobileSettings,
        DialogId::Confirm,
    ];

    /// The stable key the shell uses for this entry's egui layers.
    pub(crate) fn key(self) -> &'static str {
        match self {
            DialogId::SitePicker => "site_picker",
            DialogId::PickSiteOnMap => "pick_site_on_map",
            DialogId::ShortcutsHelp => "shortcuts_help",
            DialogId::RangeDownload => "range_download",
            DialogId::ActivitySheet => "activity_sheet",
            DialogId::ScanInspector => "scan_inspector",
            DialogId::VcpForecast => "vcp_forecast",
            DialogId::Event => "event",
            DialogId::AlertsList => "alerts_list",
            DialogId::AlertDetail => "alert_detail",
            DialogId::Mping => "mping",
            DialogId::MobileSettings => "mobile_settings",
            DialogId::Confirm => "confirm",
        }
    }

    /// Whether this entry draws over the map (a dialog or popover window).
    /// "Pick on map" is a mode, not a window: it is in the stack so Escape
    /// cancels it and shortcuts pause, but it draws nothing to restack.
    pub(crate) fn has_window(self) -> bool {
        !matches!(self, DialogId::PickSiteOnMap)
    }
}

/// The keyboard shortcuts a dialog can own (fire even while it is on top).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnedShortcut {
    /// `?` toggles the keyboard map, so it also closes it.
    ToggleHelp,
}

impl OwnedShortcut {
    /// The dialog that owns this shortcut.
    pub(crate) fn owner(self) -> DialogId {
        match self {
            OwnedShortcut::ToggleHelp => DialogId::ShortcutsHelp,
        }
    }
}

/// What is open this frame, as the shell reads it off its flags.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct DialogFlags {
    pub site_picker: bool,
    /// The site picker cannot be dismissed: no site has been chosen yet.
    pub site_required: bool,
    pub pick_site_on_map: bool,
    pub shortcuts_help: bool,
    pub range_download: bool,
    pub activity_sheet: bool,
    pub scan_inspector: bool,
    pub vcp_forecast: bool,
    pub event: bool,
    pub alerts_list: bool,
    pub alert_detail: bool,
    pub mping: bool,
    pub mobile_settings: bool,
    pub confirm: bool,
}

impl DialogFlags {
    /// The open entries, each with whether Escape / a click outside may
    /// close it.
    pub(crate) fn open(&self) -> Vec<OpenDialog> {
        DialogId::ALL
            .iter()
            .copied()
            .filter(|id| self.is_open(*id))
            .map(|id| OpenDialog {
                id,
                dismissible: !(id == DialogId::SitePicker && self.site_required),
            })
            .collect()
    }

    fn is_open(&self, id: DialogId) -> bool {
        match id {
            DialogId::SitePicker => self.site_picker,
            DialogId::PickSiteOnMap => self.pick_site_on_map,
            DialogId::ShortcutsHelp => self.shortcuts_help,
            DialogId::RangeDownload => self.range_download,
            DialogId::ActivitySheet => self.activity_sheet,
            DialogId::ScanInspector => self.scan_inspector,
            DialogId::VcpForecast => self.vcp_forecast,
            DialogId::Event => self.event,
            DialogId::AlertsList => self.alerts_list,
            DialogId::AlertDetail => self.alert_detail,
            DialogId::Mping => self.mping,
            DialogId::MobileSettings => self.mobile_settings,
            DialogId::Confirm => self.confirm,
        }
    }
}

/// One open entry, as reported by the shell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OpenDialog {
    pub id: DialogId,
    pub dismissible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Entry {
    id: DialogId,
    dismissible: bool,
    generation: u64,
}

/// The ordered stack of open dialogs and popovers (last = top).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DialogStack {
    entries: Vec<Entry>,
    /// Bumped on every open, so each opening has its own generation.
    opens: u64,
    /// Set by [`Self::sync`] when the order changed; the shell restacks
    /// windows and clears it.
    changed: bool,
}

impl DialogStack {
    /// Reconcile with what is open now. Entries that closed drop out where
    /// they were; newly opened entries go on top, in `open`'s order.
    pub(crate) fn sync(&mut self, open: &[OpenDialog]) {
        let before = self.entries.len();
        self.entries.retain(|e| open.iter().any(|o| o.id == e.id));
        let mut changed = self.entries.len() != before;
        for o in open {
            match self.entries.iter_mut().find(|e| e.id == o.id) {
                Some(e) => e.dismissible = o.dismissible,
                None => {
                    self.opens += 1;
                    self.entries.push(Entry {
                        id: o.id,
                        dismissible: o.dismissible,
                        generation: self.opens,
                    });
                    changed = true;
                }
            }
        }
        self.changed |= changed;
    }

    /// The top entry.
    pub(crate) fn top(&self) -> Option<DialogId> {
        self.entries.last().map(|e| e.id)
    }

    /// Whether nothing is open.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `id` is open.
    pub(crate) fn contains(&self, id: DialogId) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    /// The open entries, bottom to top.
    pub(crate) fn ids(&self) -> impl Iterator<Item = DialogId> + '_ {
        self.entries.iter().map(|e| e.id)
    }

    /// Which opening of `id` this is (`None` when it is closed). A dialog
    /// whose form must start fresh on every open compares this with the
    /// generation it last initialised for.
    pub(crate) fn generation(&self, id: DialogId) -> Option<u64> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.generation)
    }

    /// Whether `id` may be closed by Escape or a click outside.
    pub(crate) fn dismissible(&self, id: DialogId) -> bool {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .is_some_and(|e| e.dismissible)
    }

    /// What Escape closes: the top entry, if it is dismissible. A
    /// non-dismissible top swallows Escape rather than letting it reach the
    /// entry underneath.
    pub(crate) fn escape(&self) -> Option<DialogId> {
        self.entries.last().filter(|e| e.dismissible).map(|e| e.id)
    }

    /// Whether a global shortcut may fire. With nothing open, every one may;
    /// otherwise only one the top entry owns (`owner` is `None` for the
    /// shortcuts no dialog owns).
    pub(crate) fn allows(&self, owned: Option<OwnedShortcut>) -> bool {
        match self.top() {
            None => true,
            Some(top) => owned.is_some_and(|s| s.owner() == top),
        }
    }

    /// Take the "order changed" mark (the shell restacks once per change).
    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

// ── Destructive confirmations ──────────────────────────────────────────────

/// An action waiting for confirmation: the destructive ones, and the
/// phone's site switch from a map tap ([`crate::core::canvas::MapPick::intent_for`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Confirm {
    /// Delete every downloaded scan on this device.
    ClearCache,
    /// Restore settings and preferences to their defaults, then reload.
    ResetSettings,
    /// Delete one saved event.
    DeleteEvent(u64),
    /// Switch to the site a phone tap landed near. A finger is wide and the
    /// map is small, so a tap near a site asks before switching.
    SwitchSite {
        id: &'static str,
        lat: f64,
        lon: f64,
    },
}

/// The facts a confirmation lists, read by the shell from state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfirmFacts {
    /// Bytes in the IndexedDB cache.
    pub cache_bytes: u64,
    /// Saved events (kept by Clear cache and Reset).
    pub saved_events: usize,
    /// The event a [`Confirm::DeleteEvent`] names: `(name, site, start, end)`.
    pub event: Option<(String, String, f64, f64)>,
    /// What a [`Confirm::ResetSettings`] reads to list what it clears.
    pub reset: ResetFacts,
}

/// The facts behind the reset's lists ([`crate::core::prefs::reset_plan`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ResetFacts {
    pub has_mping_key: bool,
    pub has_preferred_site: bool,
    /// The user asked to forget the key and the site too.
    pub forget_key_and_site: bool,
}

/// What a confirmation shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConfirmVm {
    pub title: String,
    /// What will be deleted, one line each.
    pub deletes: Vec<String>,
    /// What stays, one line each.
    pub keeps: Vec<String>,
    /// A consequence after the action (the reload).
    pub note: Option<String>,
    /// The confirming button's label.
    pub action: String,
    /// Whether the confirming button is the destructive one (outlined) or
    /// the primary (filled). A site switch deletes nothing.
    pub destructive: bool,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The confirmation's words for `confirm`.
pub(crate) fn confirm_vm(
    confirm: Confirm,
    facts: &ConfirmFacts,
    format_bytes: fn(u64) -> String,
    clock: &dyn Clock,
    now: f64,
) -> ConfirmVm {
    let events = plural(facts.saved_events, "saved event", "saved events");
    match confirm {
        Confirm::ClearCache => ConfirmVm {
            title: "Clear cached radar data?".into(),
            deletes: vec![format!(
                "All radar data downloaded on this device ({}), for every site",
                format_bytes(facts.cache_bytes)
            )],
            keeps: vec![events, "Settings and preferences".into()],
            note: Some("Scans download again when you view them.".into()),
            action: "Clear cache".into(),
            destructive: true,
        },
        Confirm::ResetSettings => {
            let current = crate::core::Preferences {
                mping_api_key: facts.reset.has_mping_key.then(String::new),
                preferred_site: facts.reset.has_preferred_site.then(String::new),
                ..Default::default()
            };
            let plan = crate::core::prefs::reset_plan(&current, facts.reset.forget_key_and_site);
            ConfirmVm {
                title: "Reset settings?".into(),
                deletes: plan.clears.iter().map(|s| s.to_string()).collect(),
                // The plan's "Saved events" is said with its count.
                keeps: plan
                    .keeps
                    .iter()
                    .map(|s| match *s {
                        "Saved events" => events.clone(),
                        other => other.to_string(),
                    })
                    .collect(),
                note: Some("The page reloads with a clean link after resetting.".into()),
                action: "Reset settings".into(),
                destructive: true,
            }
        }
        Confirm::DeleteEvent(_) => {
            let line = match &facts.event {
                Some((name, site, start, end)) => format!(
                    "\u{201c}{name}\u{201d} · {site} · {} \u{2192} {}",
                    format_time_field(*start, now, clock),
                    format_time_field(*end, now, clock)
                ),
                None => "This saved event".into(),
            };
            ConfirmVm {
                title: "Delete event?".into(),
                deletes: vec![line],
                keeps: vec!["Cached radar data for its time range".into()],
                note: None,
                action: "Delete event".into(),
                destructive: true,
            }
        }
        Confirm::SwitchSite { id, .. } => {
            let name = crate::data::get_site(id).map(|s| match s.state {
                Some(st) => format!("{}, {st}", title_case(s.name)),
                None => title_case(s.name),
            });
            ConfirmVm {
                title: format!("Switch to {id}?"),
                deletes: Vec::new(),
                keeps: Vec::new(),
                note: Some(match name {
                    Some(name) => format!("{name}. The map moves to this radar."),
                    None => "The map moves to this radar.".into(),
                }),
                action: format!("Switch to {id}"),
                destructive: false,
            }
        }
    }
}

/// "OKLAHOMA CITY" → "Oklahoma City" (site names are stored in capitals);
/// the initialisms in the site list stay capitals ("VANCE AFB" → "Vance
/// AFB").
fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            if matches!(w, "AFB" | "AB" | "JFK") {
                return w.to_string();
            }
            let mut c = w.chars();
            match c.next() {
                Some(f) => f
                    .to_uppercase()
                    .chain(c.flat_map(|x| x.to_lowercase()))
                    .collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What confirming does.
pub(crate) fn confirmed_intent(confirm: Confirm) -> Intent {
    match confirm {
        Confirm::ClearCache => Intent::ClearCacheNow,
        Confirm::ResetSettings => Intent::ResetSettingsNow,
        Confirm::DeleteEvent(id) => Intent::Dialog(DialogIntent::DeleteEvent(id)),
        Confirm::SwitchSite { id, lat, lon } => Intent::SelectSite {
            site_id: id.to_string(),
            lat,
            lon,
        },
    }
}

/// Dialog intents: Escape, a close mark or a click outside, confirmations,
/// and the event dialog's writes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DialogIntent {
    /// Escape was pressed with no widget claiming it: close the top entry.
    Escape,
    /// Close one entry (its close mark, Cancel, or a click outside).
    Close(DialogId),
    /// Open a destructive confirmation.
    RequestConfirm(Confirm),
    /// The confirmation's destructive button.
    Confirm,
    /// Save the event dialog: a new event when `editing` is `None`.
    SaveEvent {
        editing: Option<u64>,
        name: String,
        site_id: String,
        start: f64,
        end: f64,
    },
    /// Delete a saved event (after its confirmation).
    DeleteEvent(u64),
    /// The site picker's "Pick on map".
    PickSiteOnMap,
}

/// Whether the event dialog's fields describe a saveable event, and the
/// message to show when they do not.
pub(crate) fn validate_event(name: &str, start: f64, end: f64) -> Result<(), &'static str> {
    if name.trim().is_empty() {
        Err("Name the event to save it")
    } else if start >= end {
        Err("The start must be before the end")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::time_field::UtcCalendar;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn open(ids: &[DialogId]) -> Vec<OpenDialog> {
        ids.iter()
            .map(|&id| OpenDialog {
                id,
                dismissible: true,
            })
            .collect()
    }

    #[wasm_bindgen_test]
    fn empty_stack_allows_every_shortcut_and_escape_closes_nothing() {
        let s = DialogStack::default();
        assert!(s.is_empty());
        assert!(s.allows(None));
        assert!(s.allows(Some(OwnedShortcut::ToggleHelp)));
        assert_eq!(s.escape(), None);
    }

    #[wasm_bindgen_test]
    fn newly_opened_goes_on_top_and_order_is_kept() {
        let mut s = DialogStack::default();
        s.sync(&open(&[DialogId::Event]));
        // Confirm opens over the event dialog; the flags list it after Event
        // anyway, but a later open must still land on top regardless of the
        // flags' order.
        s.sync(&open(&[DialogId::Confirm, DialogId::Event]));
        assert_eq!(s.top(), Some(DialogId::Confirm));
        s.sync(&open(&[
            DialogId::AlertsList,
            DialogId::Confirm,
            DialogId::Event,
        ]));
        assert_eq!(
            s.ids().collect::<Vec<_>>(),
            vec![DialogId::Event, DialogId::Confirm, DialogId::AlertsList]
        );
    }

    #[wasm_bindgen_test]
    fn escape_closes_only_the_top() {
        let mut s = DialogStack::default();
        s.sync(&open(&[DialogId::AlertsList]));
        s.sync(&open(&[DialogId::AlertsList, DialogId::AlertDetail]));
        assert_eq!(s.escape(), Some(DialogId::AlertDetail));
        // The shell closes it; the list is back on top.
        s.sync(&open(&[DialogId::AlertsList]));
        assert_eq!(s.escape(), Some(DialogId::AlertsList));
        s.sync(&[]);
        assert_eq!(s.escape(), None);
    }

    #[wasm_bindgen_test]
    fn closing_a_lower_entry_keeps_the_top() {
        let mut s = DialogStack::default();
        s.sync(&open(&[DialogId::ActivitySheet]));
        s.sync(&open(&[DialogId::ActivitySheet, DialogId::VcpForecast]));
        s.sync(&open(&[DialogId::VcpForecast]));
        assert_eq!(s.top(), Some(DialogId::VcpForecast));
        assert!(!s.contains(DialogId::ActivitySheet));
    }

    #[wasm_bindgen_test]
    fn a_non_dismissible_top_swallows_escape() {
        let mut s = DialogStack::default();
        s.sync(&[OpenDialog {
            id: DialogId::SitePicker,
            dismissible: false,
        }]);
        assert_eq!(s.escape(), None);
        assert!(!s.dismissible(DialogId::SitePicker));
        // Once a site is chosen it becomes dismissible in place.
        s.sync(&open(&[DialogId::SitePicker]));
        assert_eq!(s.escape(), Some(DialogId::SitePicker));
    }

    #[wasm_bindgen_test]
    fn shortcuts_are_suppressed_unless_the_top_owns_them() {
        let mut s = DialogStack::default();
        s.sync(&open(&[DialogId::Event]));
        assert!(!s.allows(None));
        assert!(!s.allows(Some(OwnedShortcut::ToggleHelp)));
        s.sync(&open(&[DialogId::Event, DialogId::ShortcutsHelp]));
        assert!(s.allows(Some(OwnedShortcut::ToggleHelp)));
        assert!(!s.allows(None));
        // Pick-on-map is a mode, but it still pauses the shortcuts.
        s.sync(&open(&[DialogId::PickSiteOnMap]));
        assert!(!s.allows(None));
    }

    #[wasm_bindgen_test]
    fn every_opening_has_its_own_generation() {
        let mut s = DialogStack::default();
        s.sync(&open(&[DialogId::Event]));
        let first = s.generation(DialogId::Event).unwrap();
        s.sync(&open(&[DialogId::Event]));
        assert_eq!(s.generation(DialogId::Event), Some(first));
        s.sync(&[]);
        assert_eq!(s.generation(DialogId::Event), None);
        s.sync(&open(&[DialogId::Event]));
        assert!(s.generation(DialogId::Event).unwrap() > first);
    }

    #[wasm_bindgen_test]
    fn changes_are_marked_once() {
        let mut s = DialogStack::default();
        assert!(!s.take_changed());
        s.sync(&open(&[DialogId::Event]));
        assert!(s.take_changed());
        s.sync(&open(&[DialogId::Event]));
        assert!(!s.take_changed());
        s.sync(&[]);
        assert!(s.take_changed());
    }

    #[wasm_bindgen_test]
    fn flags_report_open_entries_and_the_required_site_picker() {
        let flags = DialogFlags {
            site_picker: true,
            site_required: true,
            confirm: true,
            ..Default::default()
        };
        let open = flags.open();
        assert_eq!(open.len(), 2);
        assert_eq!(open[0].id, DialogId::SitePicker);
        assert!(!open[0].dismissible);
        assert_eq!(open[1].id, DialogId::Confirm);
        assert!(open[1].dismissible);
        assert!(DialogFlags::default().open().is_empty());
    }

    #[wasm_bindgen_test]
    fn keys_are_unique() {
        let keys: std::collections::HashSet<_> = DialogId::ALL.iter().map(|d| d.key()).collect();
        assert_eq!(keys.len(), DialogId::ALL.len());
    }

    fn bytes(b: u64) -> String {
        format!("{b} B")
    }

    #[wasm_bindgen_test]
    fn clear_cache_lists_what_goes_and_what_stays() {
        let facts = ConfirmFacts {
            cache_bytes: 512,
            saved_events: 1,
            ..Default::default()
        };
        let vm = confirm_vm(Confirm::ClearCache, &facts, bytes, &UtcCalendar, 0.0);
        assert!(vm.deletes[0].contains("(512 B)"));
        assert!(vm.keeps.contains(&"1 saved event".to_string()));
        assert_eq!(vm.action, "Clear cache");
    }

    #[wasm_bindgen_test]
    fn reset_keeps_events_and_cache_and_says_it_reloads() {
        let facts = ConfirmFacts {
            saved_events: 2,
            ..Default::default()
        };
        let vm = confirm_vm(Confirm::ResetSettings, &facts, bytes, &UtcCalendar, 0.0);
        assert!(vm.keeps.contains(&"2 saved events".to_string()));
        assert!(vm.keeps.contains(&"Cached radar data".to_string()));
        assert!(vm.deletes.contains(&"Developer mode".to_string()));
        assert!(vm.note.unwrap().contains("reload"));
    }

    #[wasm_bindgen_test]
    fn reset_keeps_the_key_and_site_unless_asked() {
        let mut facts = ConfirmFacts {
            reset: ResetFacts {
                has_mping_key: true,
                has_preferred_site: true,
                forget_key_and_site: false,
            },
            ..Default::default()
        };
        let vm = confirm_vm(Confirm::ResetSettings, &facts, bytes, &UtcCalendar, 0.0);
        assert!(vm.keeps.contains(&"mPING API key".to_string()));
        assert!(vm.keeps.contains(&"Preferred site".to_string()));
        facts.reset.forget_key_and_site = true;
        let vm = confirm_vm(Confirm::ResetSettings, &facts, bytes, &UtcCalendar, 0.0);
        assert!(vm.deletes.contains(&"mPING API key".to_string()));
        assert!(vm.deletes.contains(&"Preferred site".to_string()));
    }

    #[wasm_bindgen_test]
    fn delete_event_names_the_event() {
        // 2024-01-15 12:30:45 → 14:00:00 UTC, shown on that day.
        let facts = ConfirmFacts {
            event: Some(("Derecho".into(), "KDMX".into(), 1705321845.0, 1705327200.0)),
            ..Default::default()
        };
        let vm = confirm_vm(
            Confirm::DeleteEvent(7),
            &facts,
            bytes,
            &UtcCalendar,
            1705321845.0,
        );
        assert_eq!(vm.deletes.len(), 1);
        let line = &vm.deletes[0];
        assert!(line.contains("Derecho") && line.contains("KDMX"));
        assert!(line.contains("12:30:45") && line.contains("14:00:00"));
    }

    #[wasm_bindgen_test]
    fn switch_site_names_the_radar_and_is_not_destructive() {
        let vm = confirm_vm(
            Confirm::SwitchSite {
                id: "KTLX",
                lat: 35.33,
                lon: -97.28,
            },
            &ConfirmFacts::default(),
            bytes,
            &UtcCalendar,
            0.0,
        );
        assert_eq!(vm.title, "Switch to KTLX?");
        assert!(vm.deletes.is_empty() && vm.keeps.is_empty());
        assert!(!vm.destructive);
        assert_eq!(vm.action, "Switch to KTLX");
        assert!(vm.note.unwrap().starts_with("Oklahoma City, OK."));
    }

    #[wasm_bindgen_test]
    fn site_names_read_in_title_case_keeping_short_initialisms() {
        assert_eq!(title_case("OKLAHOMA CITY"), "Oklahoma City");
        assert_eq!(title_case("VANCE AFB"), "Vance AFB");
        assert_eq!(title_case("SAN ANTONIO"), "San Antonio");
        assert_eq!(title_case("FT WAYNE"), "Ft Wayne");
    }

    #[wasm_bindgen_test]
    fn confirming_a_site_switch_selects_the_site() {
        assert_eq!(
            confirmed_intent(Confirm::SwitchSite {
                id: "KTLX",
                lat: 35.33,
                lon: -97.28,
            }),
            Intent::SelectSite {
                site_id: "KTLX".into(),
                lat: 35.33,
                lon: -97.28,
            }
        );
    }

    #[wasm_bindgen_test]
    fn every_other_confirmation_is_destructive() {
        for c in [
            Confirm::ClearCache,
            Confirm::ResetSettings,
            Confirm::DeleteEvent(1),
        ] {
            let vm = confirm_vm(c, &ConfirmFacts::default(), bytes, &UtcCalendar, 0.0);
            assert!(vm.destructive);
        }
    }

    #[wasm_bindgen_test]
    fn confirming_runs_the_destructive_intent() {
        assert_eq!(confirmed_intent(Confirm::ClearCache), Intent::ClearCacheNow);
        assert_eq!(
            confirmed_intent(Confirm::ResetSettings),
            Intent::ResetSettingsNow
        );
        assert_eq!(
            confirmed_intent(Confirm::DeleteEvent(3)),
            Intent::Dialog(DialogIntent::DeleteEvent(3))
        );
    }

    #[wasm_bindgen_test]
    fn event_validation_names_the_problem() {
        assert!(validate_event("Derecho", 1.0, 2.0).is_ok());
        assert!(validate_event("  ", 1.0, 2.0).unwrap_err().contains("Name"));
        assert!(validate_event("x", 2.0, 2.0)
            .unwrap_err()
            .contains("before"));
    }
}
