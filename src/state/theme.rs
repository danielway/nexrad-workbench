//! Theme mode state — dark mode only.

/// Theme mode (always dark).
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    #[default]
    Dark,
}

impl ThemeMode {
    /// Always dark mode.
    pub fn is_dark(&self) -> bool {
        true
    }
}

/// Load theme mode (always dark).
pub fn load_theme_mode() -> ThemeMode {
    ThemeMode::Dark
}
