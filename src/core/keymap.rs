//! The keyboard map's decisions (one-instrument §11): every shortcut stays
//! on the map, the ones inactive in the current view are greyed, and the
//! modifier is spelled the platform's way (Cmd on macOS).
//!
//! The bindings themselves are the shell's registry (`ui::shortcuts`); each
//! names its [`KeyScope`], which both gates firing and greys its row here.

/// Where a shortcut works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyScope {
    /// In every view.
    Always,
    /// Only in the 2D map (the key moves the camera in 3D).
    Only2D,
    /// Only on the 3D globe.
    Only3D,
}

impl KeyScope {
    /// Fires (and reads as active) in this view.
    pub(crate) fn active(self, is_3d: bool) -> bool {
        match self {
            KeyScope::Always => true,
            KeyScope::Only2D => !is_3d,
            KeyScope::Only3D => is_3d,
        }
    }
}

/// A key label in the platform's words: `Ctrl` is `Cmd` on macOS, where the
/// same egui `command` modifier is the ⌘ key.
pub(crate) fn platform_key_label(label: &str, mac: bool) -> String {
    if mac {
        label.replace("Ctrl", "Cmd")
    } else {
        label.to_string()
    }
}

/// One row of the keyboard map.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KeymapRow {
    pub key: String,
    pub description: &'static str,
    /// Works in the current view; greyed otherwise.
    pub active: bool,
}

/// The map's rows for `(key, description, scope)` entries, in order.
pub(crate) fn keymap_rows(
    entries: impl IntoIterator<Item = (&'static str, &'static str, KeyScope)>,
    is_3d: bool,
    mac: bool,
) -> Vec<KeymapRow> {
    entries
        .into_iter()
        .map(|(key, description, scope)| KeymapRow {
            key: platform_key_label(key, mac),
            description,
            active: scope.active(is_3d),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn scopes_by_view() {
        assert!(KeyScope::Always.active(false) && KeyScope::Always.active(true));
        assert!(KeyScope::Only2D.active(false) && !KeyScope::Only2D.active(true));
        assert!(!KeyScope::Only3D.active(false) && KeyScope::Only3D.active(true));
    }

    #[wasm_bindgen_test]
    fn says_cmd_on_macos_only() {
        assert_eq!(platform_key_label("Ctrl", true), "Cmd");
        assert_eq!(platform_key_label("Ctrl+Shift+L", true), "Cmd+Shift+L");
        assert_eq!(platform_key_label("Ctrl", false), "Ctrl");
        assert_eq!(platform_key_label("Shift+E", true), "Shift+E");
    }

    #[wasm_bindgen_test]
    fn every_shortcut_stays_and_inactive_ones_grey() {
        let entries = [
            ("S", "Open site selection (2D)", KeyScope::Only2D),
            ("N", "Align North up (3D)", KeyScope::Only3D),
            ("Ctrl", "¼× camera speed", KeyScope::Always),
        ];
        let rows = keymap_rows(entries, true, true);
        assert_eq!(rows.len(), 3);
        assert!(!rows[0].active);
        assert!(rows[1].active);
        assert_eq!(rows[2].key, "Cmd");
        let rows = keymap_rows(entries, false, false);
        assert!(rows[0].active && !rows[1].active);
        assert_eq!(rows[2].key, "Ctrl");
    }
}
