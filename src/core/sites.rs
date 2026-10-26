//! The site set: which radars are on the map, which one is the focus, and
//! which site a worker reply belongs to.
//!
//! The app holds one [`crate::subsystem::sites::SiteState`] per active radar
//! and a focus index. The decisions here work on the list of ids alone:
//!
//! - [`cap`]: how many sites a layout shows (three on a desktop, one on a
//!   phone), and [`fit_to_layout`]: what a change of layout does to the set.
//! - [`add`], [`remove`], [`set_focus`], [`only_focus`], [`select`]: what a
//!   change to the set does, or why it is refused ([`Refusal::words`] is what
//!   the picker says).
//! - [`draw_order`]: the order the canvas draws the discs in, older frame
//!   first. Where discs overlap the order does not decide what shows:
//!   [`rivals`] does.
//! - [`rivals`]: the other discs a disc yields its pixels to. Each pixel
//!   where discs overlap shows the nearest radar whose data reaches it.
//! - [`frame_union`]: the Frames-mode frame list across every site.
//! - [`route`]: the site a worker reply belongs to. Replies that carry a
//!   stored scan key route by the key's site; a reply whose site is no longer
//!   in the list (the user removed it while the reply was in flight) is
//!   dropped rather than applied to whichever site is there now.

use crate::core::phone::DeviceLayout;
use crate::data::{ScanKey, SiteId};

/// How many sites a desktop shows at once.
pub(crate) const DESKTOP_CAP: usize = 3;

/// How many sites `layout` shows at once: three on a desktop, one on a
/// phone.
pub(crate) fn cap(layout: DeviceLayout) -> usize {
    if layout.is_phone() {
        1
    } else {
        DESKTOP_CAP
    }
}

/// What the set does when the layout becomes `layout`: one that shows fewer
/// sites than the `len` on the map (a desktop window narrowed into the
/// phone's layout) keeps only the focus. `None` when the set fits.
pub(crate) fn fit_to_layout(len: usize, focus: usize, layout: DeviceLayout) -> Option<Change> {
    (len > cap(layout)).then_some(Change::Only { keep: focus })
}

/// What a change to the site set does, for the shell to carry out on the
/// list of `SiteState`s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// Push the site at the end of the list. The focus stays.
    Add,
    /// Drop the element at `index`; `focus` is the focus index afterwards.
    Remove { index: usize, focus: usize },
    /// Move the focus to `index`.
    Focus { index: usize },
    /// Keep only the element at `keep`, which becomes the focus (index 0).
    Only { keep: usize },
}

/// Why a change to the site set was refused. [`Refusal::words`] is what the
/// picker shows beside the action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The set is full for this layout.
    AtCap { cap: usize },
    /// The site is already in the set.
    AlreadyShown,
    /// The site is not in the set.
    NotShown,
    /// The set would be empty.
    LastSite,
    /// The site is already the focus.
    AlreadyFocus,
    /// Only the focus is shown already.
    AlreadyOnly,
}

impl Refusal {
    /// The words for the refusal ("3 sites max").
    pub(crate) fn words(self) -> String {
        match self {
            Refusal::AtCap { cap: 1 } => "1 site max".to_string(),
            Refusal::AtCap { cap } => format!("{cap} sites max"),
            Refusal::AlreadyShown => "already on the map".to_string(),
            Refusal::NotShown => "not on the map".to_string(),
            Refusal::LastSite => "the only site on the map".to_string(),
            Refusal::AlreadyFocus => "already the focus".to_string(),
            Refusal::AlreadyOnly => "the only site on the map".to_string(),
        }
    }
}

/// Add `id` to the set `ids` under a cap of `cap` sites.
pub(crate) fn add(ids: &[SiteId], id: &SiteId, cap: usize) -> Result<Change, Refusal> {
    if ids.contains(id) {
        Err(Refusal::AlreadyShown)
    } else if ids.len() >= cap {
        Err(Refusal::AtCap { cap })
    } else {
        Ok(Change::Add)
    }
}

/// Remove `id` from the set. Removing the focus moves the focus to the first
/// remaining site; removing a site before the focus keeps the same site
/// focused.
pub(crate) fn remove(ids: &[SiteId], focus: usize, id: &SiteId) -> Result<Change, Refusal> {
    let index = route_site(id, ids).ok_or(Refusal::NotShown)?;
    if ids.len() <= 1 {
        return Err(Refusal::LastSite);
    }
    let focus = match index.cmp(&focus) {
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Less => focus - 1,
        std::cmp::Ordering::Greater => focus,
    };
    Ok(Change::Remove { index, focus })
}

/// Make `id` the focus.
pub(crate) fn set_focus(ids: &[SiteId], focus: usize, id: &SiteId) -> Result<Change, Refusal> {
    match route_site(id, ids) {
        None => Err(Refusal::NotShown),
        Some(index) if index == focus => Err(Refusal::AlreadyFocus),
        Some(index) => Ok(Change::Focus { index }),
    }
}

/// Keep only the focus.
pub(crate) fn only_focus(ids: &[SiteId], focus: usize) -> Result<Change, Refusal> {
    if ids.len() <= 1 {
        Err(Refusal::AlreadyOnly)
    } else {
        Ok(Change::Only { keep: focus })
    }
}

/// What picking a site from the list does (`SelectSite`): the picked site
/// becomes the only site and the focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Select {
    /// The picked site is the focus already and the only site: nothing moves.
    Unchanged,
    /// The picked site is in the set: keep that element alone (its scans and
    /// frame stay) and focus it.
    Keep { index: usize },
    /// The picked site is not in the set: keep the focus element alone and
    /// point it at the picked site.
    Retarget { keep: usize },
}

/// Decide what picking `id` does to the set.
pub(crate) fn select(ids: &[SiteId], focus: usize, id: &SiteId) -> Select {
    match route_site(id, ids) {
        Some(index) if index == focus && ids.len() == 1 => Select::Unchanged,
        Some(index) => Select::Keep { index },
        None => Select::Retarget { keep: focus },
    }
}

/// Most rivals a disc has: every other site a desktop can show.
pub(crate) const MAX_RIVALS: usize = DESKTOP_CAP - 1;

/// The discs that take pixels from disc `i` (indices into the list; `None`
/// for a site with nothing on screen): every other site that has data.
///
/// Where discs overlap, a pixel shows the nearest radar whose data reaches
/// it. Each disc's shader gets its rivals' centres and data ranges and
/// leaves out the pixels a rival is nearer to and covers, so the split is
/// the same on every frame. The older rule, newer frame on top, flipped the
/// whole overlap from one radar to another whenever a playing loop stepped
/// past one site's scan and not the other's.
pub(crate) fn rivals<T: Copy>(discs: &[Option<T>], i: usize) -> Vec<T> {
    discs
        .iter()
        .enumerate()
        .filter(|&(j, _)| j != i)
        .filter_map(|(_, d)| *d)
        .take(MAX_RIVALS)
        .collect()
}

/// The order the canvas draws the sites in, as indices into the list:
/// older frame first. A site with no frame draws first; ties keep list
/// order. Overlaps are split by [`rivals`], so the order shows only where
/// two radars are exactly as near.
pub(crate) fn draw_order(frame_times: &[Option<f64>]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..frame_times.len()).collect();
    order.sort_by(|&a, &b| {
        let (ta, tb) = (frame_times[a], frame_times[b]);
        match (ta, tb) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        }
    });
    order
}

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

/// The Frames-mode frame list across every site: the union of each site's
/// frame times (sorted), with frames of different sites that fall within
/// `tolerance_secs` of the first frame of their group joined into one frame
/// at the group's latest time, so stepping onto it shows every site's new
/// frame. Two frames of the same site are never joined: a site's own list
/// passes through unchanged.
pub(crate) fn frame_union(per_site: &[Vec<f64>], tolerance_secs: f64) -> Vec<f64> {
    let mut all: Vec<(f64, usize)> = per_site
        .iter()
        .enumerate()
        .flat_map(|(site, times)| times.iter().map(move |&t| (t, site)))
        .filter(|(t, _)| t.is_finite())
        .collect();
    all.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    let mut group: Option<(f64, f64, Vec<usize>)> = None;
    for (t, site) in all {
        if let Some((first, latest, sites)) = group.as_mut() {
            if t - *first <= tolerance_secs && !sites.contains(&site) {
                *latest = t;
                sites.push(site);
                continue;
            }
            out.push(*latest);
        }
        group = Some((t, t, vec![site]));
    }
    if let Some((_, latest, _)) = group {
        out.push(latest);
    }
    out
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

    fn desktop() -> DeviceLayout {
        DeviceLayout::Desktop
    }

    #[wasm_bindgen_test]
    fn cap_is_three_on_a_desktop_and_one_on_a_phone() {
        assert_eq!(cap(desktop()), 3);
        assert_eq!(cap(DeviceLayout::Phone { landscape: false }), 1);
        assert_eq!(cap(DeviceLayout::Phone { landscape: true }), 1);
    }

    #[wasm_bindgen_test]
    fn turning_into_a_phone_keeps_only_the_focus() {
        let phone = DeviceLayout::Phone { landscape: false };
        assert_eq!(fit_to_layout(3, 1, phone), Some(Change::Only { keep: 1 }));
        assert_eq!(
            fit_to_layout(2, 0, DeviceLayout::Phone { landscape: true }),
            Some(Change::Only { keep: 0 })
        );
        // One site fits the phone; any set fits the desktop.
        assert_eq!(fit_to_layout(1, 0, phone), None);
        assert_eq!(fit_to_layout(3, 2, desktop()), None);
        assert_eq!(fit_to_layout(1, 0, desktop()), None);
    }

    #[wasm_bindgen_test]
    fn add_pushes_until_the_cap() {
        let id = SiteId::new("KJAX");
        assert_eq!(add(&ids(&["KTBW"]), &id, 3), Ok(Change::Add));
        assert_eq!(add(&ids(&["KTBW", "KMLB"]), &id, 3), Ok(Change::Add));
        assert_eq!(
            add(&ids(&["KTBW", "KMLB", "KAMX"]), &id, 3),
            Err(Refusal::AtCap { cap: 3 })
        );
        // A phone shows one site.
        assert_eq!(
            add(
                &ids(&["KTBW"]),
                &id,
                cap(DeviceLayout::Phone { landscape: false })
            ),
            Err(Refusal::AtCap { cap: 1 })
        );
    }

    #[wasm_bindgen_test]
    fn add_refuses_a_site_already_shown_even_at_the_cap() {
        let list = ids(&["KTBW", "KMLB", "KJAX"]);
        assert_eq!(
            add(&list, &SiteId::new("KMLB"), 3),
            Err(Refusal::AlreadyShown)
        );
    }

    #[wasm_bindgen_test]
    fn refusals_say_why_in_the_pickers_words() {
        assert_eq!(Refusal::AtCap { cap: 3 }.words(), "3 sites max");
        assert_eq!(Refusal::AtCap { cap: 1 }.words(), "1 site max");
        assert_eq!(Refusal::AlreadyShown.words(), "already on the map");
        assert_eq!(Refusal::NotShown.words(), "not on the map");
        assert_eq!(Refusal::LastSite.words(), "the only site on the map");
        assert_eq!(Refusal::AlreadyFocus.words(), "already the focus");
        assert_eq!(Refusal::AlreadyOnly.words(), "the only site on the map");
    }

    #[wasm_bindgen_test]
    fn removing_the_focus_moves_it_to_the_first_remaining_site() {
        let list = ids(&["KTBW", "KMLB", "KJAX"]);
        assert_eq!(
            remove(&list, 1, &SiteId::new("KMLB")),
            Ok(Change::Remove { index: 1, focus: 0 })
        );
        assert_eq!(
            remove(&list, 0, &SiteId::new("KTBW")),
            Ok(Change::Remove { index: 0, focus: 0 })
        );
    }

    #[wasm_bindgen_test]
    fn removing_another_site_keeps_the_same_site_focused() {
        let list = ids(&["KTBW", "KMLB", "KJAX"]);
        // Before the focus: the focus index moves down with its site.
        assert_eq!(
            remove(&list, 2, &SiteId::new("KTBW")),
            Ok(Change::Remove { index: 0, focus: 1 })
        );
        // After the focus: nothing moves.
        assert_eq!(
            remove(&list, 0, &SiteId::new("KJAX")),
            Ok(Change::Remove { index: 2, focus: 0 })
        );
    }

    #[wasm_bindgen_test]
    fn remove_refuses_the_last_site_and_a_site_not_shown() {
        assert_eq!(
            remove(&ids(&["KTBW"]), 0, &SiteId::new("KTBW")),
            Err(Refusal::LastSite)
        );
        assert_eq!(
            remove(&ids(&["KTBW", "KMLB"]), 0, &SiteId::new("KJAX")),
            Err(Refusal::NotShown)
        );
    }

    #[wasm_bindgen_test]
    fn set_focus_moves_to_a_shown_site() {
        let list = ids(&["KTBW", "KMLB", "KJAX"]);
        assert_eq!(
            set_focus(&list, 0, &SiteId::new("KJAX")),
            Ok(Change::Focus { index: 2 })
        );
        assert_eq!(
            set_focus(&list, 2, &SiteId::new("KJAX")),
            Err(Refusal::AlreadyFocus)
        );
        assert_eq!(
            set_focus(&list, 0, &SiteId::new("KAMX")),
            Err(Refusal::NotShown)
        );
    }

    #[wasm_bindgen_test]
    fn only_focus_keeps_the_focus_alone() {
        assert_eq!(
            only_focus(&ids(&["KTBW", "KMLB", "KJAX"]), 1),
            Ok(Change::Only { keep: 1 })
        );
        assert_eq!(only_focus(&ids(&["KTBW"]), 0), Err(Refusal::AlreadyOnly));
    }

    #[wasm_bindgen_test]
    fn select_makes_the_pick_the_only_site() {
        let list = ids(&["KTBW", "KMLB", "KJAX"]);
        // A shown site keeps its element.
        assert_eq!(
            select(&list, 0, &SiteId::new("KJAX")),
            Select::Keep { index: 2 }
        );
        assert_eq!(
            select(&list, 0, &SiteId::new("KTBW")),
            Select::Keep { index: 0 }
        );
        // Another site retargets the focus element.
        assert_eq!(
            select(&list, 1, &SiteId::new("KAMX")),
            Select::Retarget { keep: 1 }
        );
        // The one site, picked again: nothing moves.
        assert_eq!(
            select(&ids(&["KTBW"]), 0, &SiteId::new("KTBW")),
            Select::Unchanged
        );
    }

    #[wasm_bindgen_test]
    fn draw_order_puts_the_newer_frame_on_top() {
        // KTBW 02:53, KMLB 02:56, KJAX 02:57 (the Milton mock).
        assert_eq!(
            draw_order(&[Some(10_414.0), Some(10_593.0), Some(10_632.0)]),
            vec![0, 1, 2]
        );
        assert_eq!(
            draw_order(&[Some(300.0), Some(100.0), Some(200.0)]),
            vec![1, 2, 0]
        );
    }

    #[wasm_bindgen_test]
    fn draw_order_draws_a_site_with_no_frame_first_and_keeps_ties() {
        assert_eq!(draw_order(&[Some(5.0), None, Some(1.0)]), vec![1, 2, 0]);
        assert_eq!(draw_order(&[None, None]), vec![0, 1]);
        assert_eq!(draw_order(&[Some(1.0), Some(1.0)]), vec![0, 1]);
        assert_eq!(draw_order(&[]), Vec::<usize>::new());
    }

    #[wasm_bindgen_test]
    fn rivals_are_every_other_disc_with_data() {
        let discs = [Some('a'), None, Some('c')];
        assert_eq!(rivals(&discs, 0), vec!['c']);
        assert_eq!(rivals(&discs, 1), vec!['a', 'c']);
        assert_eq!(rivals(&discs, 2), vec!['a']);
        // One site yields to nobody, and a site with no frame is no rival.
        assert_eq!(rivals(&[Some('a')], 0), Vec::<char>::new());
        assert_eq!(rivals(&[Some('a'), None], 0), Vec::<char>::new());
    }

    #[wasm_bindgen_test]
    fn focus_valid_needs_an_element() {
        assert!(focus_valid(0, 1));
        assert!(focus_valid(2, 3));
        assert!(!focus_valid(1, 1));
        assert!(!focus_valid(0, 0));
    }

    #[wasm_bindgen_test]
    fn frame_union_of_one_site_is_its_own_list() {
        // A site's own frames are never joined, however close.
        let frames = vec![100.0, 120.0, 140.0, 400.0];
        assert_eq!(frame_union(std::slice::from_ref(&frames), 60.0), frames);
        assert_eq!(frame_union(&[], 60.0), Vec::<f64>::new());
    }

    #[wasm_bindgen_test]
    fn frame_union_joins_other_sites_frames_within_the_tolerance() {
        // KMLB at 593 and KJAX at 632 are one frame at 632; KTBW's 414
        // stands alone.
        let union = frame_union(&[vec![414.0, 800.0], vec![593.0], vec![632.0]], 60.0);
        assert_eq!(union, vec![414.0, 632.0, 800.0]);
    }

    #[wasm_bindgen_test]
    fn frame_union_keeps_frames_apart_beyond_the_tolerance() {
        let union = frame_union(&[vec![0.0, 300.0], vec![100.0, 400.0]], 60.0);
        assert_eq!(union, vec![0.0, 100.0, 300.0, 400.0]);
        // Within: a group is measured from its first frame.
        let union = frame_union(&[vec![0.0], vec![50.0], vec![100.0]], 60.0);
        assert_eq!(union, vec![50.0, 100.0]);
    }

    #[wasm_bindgen_test]
    fn frame_union_drops_non_finite_times() {
        assert_eq!(
            frame_union(&[vec![f64::NAN, 10.0], vec![f64::INFINITY]], 60.0),
            vec![10.0]
        );
    }
}
