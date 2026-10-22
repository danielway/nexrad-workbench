//! Which site a worker reply belongs to.
//!
//! The app holds one [`crate::subsystem::sites::SiteState`] per active radar
//! and shares one worker pool between them, so every reply has to find its
//! site again. Replies that carry a stored scan key route by the key's site;
//! a reply whose site is no longer in the list (the user switched away while
//! it was in flight) is dropped rather than applied to whichever site is
//! there now.

use crate::data::{ScanKey, SiteId};

/// The index of the site `scan_key` belongs to, or `None` when that site is
/// not in `ids`.
pub(crate) fn route(scan_key: &ScanKey, ids: &[SiteId]) -> Option<usize> {
    route_site(&scan_key.site, ids)
}

/// [`route`] for a reply that names its site without a scan (a cache load).
pub(crate) fn route_site(site: &SiteId, ids: &[SiteId]) -> Option<usize> {
    ids.iter().position(|id| id == site)
}

/// Whether `focus` names an element of a list of `len` sites. The list is
/// never empty, so a valid focus always exists.
pub(crate) fn focus_valid(focus: usize, len: usize) -> bool {
    focus < len
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::keys::UnixMillis;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn key(site: &str) -> ScanKey {
        ScanKey::new(site, UnixMillis(1_700_000_000_000))
    }

    fn ids(list: &[&str]) -> Vec<SiteId> {
        list.iter().map(|s| SiteId::new(*s)).collect()
    }

    #[wasm_bindgen_test]
    fn route_finds_the_site_of_the_key() {
        let list = ids(&["KDMX", "KTLX", "KFTG"]);
        assert_eq!(route(&key("KDMX"), &list), Some(0));
        assert_eq!(route(&key("KTLX"), &list), Some(1));
        assert_eq!(route(&key("KFTG"), &list), Some(2));
    }

    #[wasm_bindgen_test]
    fn route_drops_a_site_not_in_the_list() {
        let list = ids(&["KDMX"]);
        assert_eq!(route(&key("KTLX"), &list), None);
        assert_eq!(route(&key("KDMX"), &[]), None);
    }

    #[wasm_bindgen_test]
    fn route_is_case_sensitive_like_the_storage_key() {
        // Site ids are stored upper case; a lower-case key is a different
        // site as far as the store is concerned.
        assert_eq!(route(&key("kdmx"), &ids(&["KDMX"])), None);
    }

    #[wasm_bindgen_test]
    fn route_site_matches_route_on_the_key_site() {
        let list = ids(&["KDMX", "KTLX"]);
        assert_eq!(route_site(&SiteId::new("KTLX"), &list), Some(1));
        assert_eq!(route_site(&SiteId::new("KFTG"), &list), None);
        assert_eq!(
            route_site(&key("KDMX").site, &list),
            route(&key("KDMX"), &list)
        );
    }

    #[wasm_bindgen_test]
    fn focus_valid_needs_an_element() {
        assert!(focus_valid(0, 1));
        assert!(focus_valid(2, 3));
        assert!(!focus_valid(1, 1));
        assert!(!focus_valid(0, 0));
    }
}
