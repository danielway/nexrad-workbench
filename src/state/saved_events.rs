//! Saved events persisted to localStorage.
//!
//! Each event captures a notable weather occurrence with a name, radar site,
//! and time range. Events are displayed on the timeline and can be navigated
//! to from the right panel.

use serde::{Deserialize, Serialize};

/// A user-saved weather event bookmark.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedEvent {
    /// Unique identifier (epoch millis at creation).
    pub id: u64,
    /// User-defined event name.
    pub name: String,
    /// NEXRAD site identifier (e.g., "KDMX").
    pub site_id: String,
    /// Event start time (Unix seconds).
    pub start_time: f64,
    /// Event end time (Unix seconds).
    pub end_time: f64,
}

/// Collection of saved events, persisted to localStorage.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedEvents {
    #[serde(default)]
    pub events: Vec<SavedEvent>,
}

impl SavedEvents {
    pub(crate) const STORAGE_KEY: &'static str = "nexrad_saved_events";

    /// localStorage keys a settings reset should delete. Saved events stay.
    pub(crate) fn keys_to_reset(keys: impl IntoIterator<Item = String>) -> Vec<String> {
        keys.into_iter()
            .filter(|key| key != Self::STORAGE_KEY)
            .collect()
    }

    /// Load saved events from localStorage.
    pub(crate) fn load() -> Self {
        let window = match web_sys::window() {
            Some(w) => w,
            None => return Self::default(),
        };

        let storage = match window.local_storage() {
            Ok(Some(s)) => s,
            _ => return Self::default(),
        };

        let json = match storage.get_item(Self::STORAGE_KEY) {
            Ok(Some(s)) => s,
            _ => return Self::default(),
        };

        match serde_json::from_str(&json) {
            Ok(events) => {
                log::debug!("Loaded saved events from localStorage");
                events
            }
            Err(e) => {
                log::warn!("Failed to parse saved events: {}", e);
                Self::default()
            }
        }
    }

    /// Save events to localStorage.
    pub(crate) fn save(&self) {
        let window = match web_sys::window() {
            Some(w) => w,
            None => return,
        };

        let storage = match window.local_storage() {
            Ok(Some(s)) => s,
            _ => return,
        };

        let json = match serde_json::to_string(self) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Failed to serialize saved events: {}", e);
                return;
            }
        };

        if let Err(e) = storage.set_item(Self::STORAGE_KEY, &json) {
            log::warn!("Failed to save events: {:?}", e);
        }
    }

    /// Add a new event and persist immediately.
    pub(crate) fn add(&mut self, name: String, site_id: String, start_time: f64, end_time: f64) {
        let id = js_sys::Date::now() as u64;
        self.events.push(SavedEvent {
            id,
            name,
            site_id,
            start_time,
            end_time,
        });
        self.save();
    }

    /// Remove an event by ID and persist immediately.
    pub(crate) fn remove(&mut self, id: u64) {
        self.events.retain(|e| e.id != id);
        self.save();
    }

    /// Update an existing event and persist immediately.
    pub(crate) fn update(&mut self, id: u64, name: String, start_time: f64, end_time: f64) {
        if let Some(event) = self.events.iter_mut().find(|e| e.id == id) {
            event.name = name;
            event.start_time = start_time;
            event.end_time = end_time;
            self.save();
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // `save()` is a no-op under wasm-bindgen-test in node (no window), so these
    // exercise the pure in-memory collection logic without touching storage.

    fn ev(id: u64, name: &str) -> SavedEvent {
        SavedEvent {
            id,
            name: name.to_string(),
            site_id: "KDMX".to_string(),
            start_time: 1000.0,
            end_time: 2000.0,
        }
    }

    #[wasm_bindgen_test]
    fn default_is_empty() {
        assert!(SavedEvents::default().events.is_empty());
    }

    #[wasm_bindgen_test]
    fn settings_reset_keeps_saved_events_key() {
        let keys = vec![
            "nexrad_user_preferences".to_string(),
            SavedEvents::STORAGE_KEY.to_string(),
            "nexrad_storage_settings".to_string(),
            "nexrad_volume_KDMX".to_string(),
        ];
        assert_eq!(
            SavedEvents::keys_to_reset(keys),
            vec![
                "nexrad_user_preferences".to_string(),
                "nexrad_storage_settings".to_string(),
                "nexrad_volume_KDMX".to_string(),
            ]
        );
    }

    #[wasm_bindgen_test]
    fn serde_round_trips_events() {
        let s = SavedEvents {
            events: vec![ev(1, "Derecho"), ev(2, "Tornado outbreak")],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: SavedEvents = serde_json::from_str(&json).unwrap();
        // SavedEvents has no Debug derive, so compare via PartialEq directly.
        assert!(back == s);
    }

    #[wasm_bindgen_test]
    fn deserialize_tolerates_missing_events_field() {
        // `#[serde(default)]` on `events` means an empty object loads as empty.
        let back: SavedEvents = serde_json::from_str("{}").unwrap();
        assert!(back.events.is_empty());
    }

    #[wasm_bindgen_test]
    fn remove_drops_only_the_matching_id() {
        let mut s = SavedEvents {
            events: vec![ev(1, "a"), ev(2, "b"), ev(3, "c")],
        };
        s.remove(2);
        let ids: Vec<u64> = s.events.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![1, 3]);
    }

    #[wasm_bindgen_test]
    fn remove_absent_id_is_a_noop() {
        let mut s = SavedEvents {
            events: vec![ev(1, "a")],
        };
        s.remove(99);
        assert_eq!(s.events.len(), 1);
    }

    #[wasm_bindgen_test]
    fn update_mutates_matching_event_fields() {
        let mut s = SavedEvents {
            events: vec![ev(1, "old"), ev(2, "keep")],
        };
        s.update(1, "new".to_string(), 5.0, 9.0);
        let e = &s.events[0];
        assert_eq!(e.name, "new");
        assert_eq!(e.start_time, 5.0);
        assert_eq!(e.end_time, 9.0);
        // site_id and id are untouched; the other event is untouched.
        assert_eq!(e.site_id, "KDMX");
        assert_eq!(e.id, 1);
        assert_eq!(s.events[1].name, "keep");
    }

    #[wasm_bindgen_test]
    fn update_absent_id_is_a_noop() {
        let mut s = SavedEvents {
            events: vec![ev(1, "a")],
        };
        s.update(42, "x".to_string(), 0.0, 0.0);
        assert_eq!(s.events[0].name, "a");
    }

    #[wasm_bindgen_test]
    fn add_appends_one_event_with_given_fields() {
        let mut s = SavedEvents::default();
        s.add("Hail".to_string(), "KFWS".to_string(), 100.0, 200.0);
        assert_eq!(s.events.len(), 1);
        let e = &s.events[0];
        assert_eq!(e.name, "Hail");
        assert_eq!(e.site_id, "KFWS");
        assert_eq!(e.start_time, 100.0);
        assert_eq!(e.end_time, 200.0);
    }
}
