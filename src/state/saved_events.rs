//! Saved events persisted to localStorage.
//!
//! Each event captures a notable weather occurrence with a name, radar site,
//! and time range. Events are displayed on the timeline and can be navigated
//! to from the right panel.

use serde::{Deserialize, Serialize};

/// A user-saved weather event bookmark.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedEvent {
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
pub struct SavedEvents {
    #[serde(default)]
    pub events: Vec<SavedEvent>,
}

impl SavedEvents {
    const STORAGE_KEY: &'static str = "nexrad_saved_events";

    /// Load saved events from localStorage.
    pub fn load() -> Self {
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
                log::info!("Loaded saved events from localStorage");
                events
            }
            Err(e) => {
                log::warn!("Failed to parse saved events: {}", e);
                Self::default()
            }
        }
    }

    /// Save events to localStorage.
    pub fn save(&self) {
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
    pub fn add(&mut self, name: String, site_id: String, start_time: f64, end_time: f64) {
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
    pub fn remove(&mut self, id: u64) {
        self.events.retain(|e| e.id != id);
        self.save();
    }

    /// Update an existing event and persist immediately.
    pub fn update(&mut self, id: u64, name: String, start_time: f64, end_time: f64) {
        if let Some(event) = self.events.iter_mut().find(|e| e.id == id) {
            event.name = name;
            event.start_time = start_time;
            event.end_time = end_time;
            self.save();
        }
    }
}
