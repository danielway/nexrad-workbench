//! Theme mode state and OS preference detection.

/// Theme mode selection.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    /// Follow OS preference.
    #[default]
    System,
    /// Force dark mode.
    Dark,
    /// Force light mode.
    Light,
}

impl ThemeMode {
    pub fn label(&self) -> &'static str {
        match self {
            ThemeMode::System => "System",
            ThemeMode::Dark => "Dark",
            ThemeMode::Light => "Light",
        }
    }

    pub fn all() -> &'static [ThemeMode] {
        &[ThemeMode::System, ThemeMode::Dark, ThemeMode::Light]
    }

    /// Resolve to a concrete dark/light boolean.
    pub fn is_dark(&self) -> bool {
        match self {
            ThemeMode::System => detect_os_dark_mode(),
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
        }
    }
}

/// Detect OS dark mode preference via the `prefers-color-scheme` media query.
fn detect_os_dark_mode() -> bool {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return true, // Default to dark if no window
    };
    match window.match_media("(prefers-color-scheme: dark)") {
        Ok(Some(mql)) => mql.matches(),
        _ => true, // Default to dark
    }
}

const STORAGE_KEY: &str = "nexrad-theme-mode";

/// Load theme mode from localStorage.
pub fn load_theme_mode() -> ThemeMode {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return ThemeMode::default(),
    };
    let storage = match window.local_storage() {
        Ok(Some(s)) => s,
        _ => return ThemeMode::default(),
    };
    match storage.get_item(STORAGE_KEY) {
        Ok(Some(ref val)) => match val.as_str() {
            "dark" => ThemeMode::Dark,
            "light" => ThemeMode::Light,
            _ => ThemeMode::System,
        },
        _ => ThemeMode::default(),
    }
}

/// Save theme mode to localStorage.
pub fn save_theme_mode(mode: ThemeMode) {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return,
    };
    let storage = match window.local_storage() {
        Ok(Some(s)) => s,
        _ => return,
    };
    let val = match mode {
        ThemeMode::System => "system",
        ThemeMode::Dark => "dark",
        ThemeMode::Light => "light",
    };
    let _ = storage.set_item(STORAGE_KEY, val);
}
