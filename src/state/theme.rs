//! Theme mode state — dark mode only.

/// Theme mode (always dark).
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeMode {
    #[default]
    Dark,
}

impl ThemeMode {
    /// Always dark mode.
    pub(crate) fn is_dark(&self) -> bool {
        true
    }
}

/// Load theme mode (always dark).
pub(crate) fn load_theme_mode() -> ThemeMode {
    ThemeMode::Dark
}
