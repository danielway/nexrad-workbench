//! The site picker (One Instrument answers, "Choosing a site"): one popover
//! under the site control. One search field takes a site ID, a city, a state
//! or a zip; the groups under it answer the three ways people pick — the
//! sites near them, the ones they were just looking at, and the full list.
//!
//! Pure: [`site_picker_vm`] turns the query, the site set on the map, the
//! recent list and the "near you" fix into rows; [`on_location`] decides what
//! a geolocation or zip result does. The egui popover is `ui::site_modal`.
//!
//! **Several sites.** The [`SiteSet`] is the list of sites on the map and the
//! focus index. Once more than one site is shown, an "On the map" group heads
//! the list: the focus has the filled circle (another row's circle moves the
//! focus), the others a ticked box (unticking removes the site), each with
//! its frame's age, and the group header offers "Only KTBW". Every row
//! elsewhere offers "+ Also show" until the cap, where the action says why
//! it is disabled; a row already on the map reads "on the map". A plain
//! click on a row is still a pick: that site alone. On a phone (a cap of one)
//! the picker is the single-site picker.

use crate::core::session::{format_age, Stream};
use crate::core::sites::Refusal;
use crate::core::{Intent, LocationResult, LocationSource};
use crate::data::sites::{all_sites_sorted, haversine_distance, NexradSite};
use crate::data::SiteId;

/// How many sites the Near you group lists.
pub(crate) const NEAR_YOU_COUNT: usize = 4;
/// How many sites the Recent group remembers.
pub(crate) const RECENT_CAP: usize = 5;

/// One site on the map, as the picker reads it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OnMap {
    pub id: String,
    /// Age of the frame on its canvas, in seconds; `None` with no frame.
    pub age_secs: Option<f64>,
    /// Its live stream's health ([`Stream::Off`] when not streaming).
    pub stream: Stream,
}

impl OnMap {
    /// A site with no frame and no stream.
    #[cfg(test)]
    pub(crate) fn idle(id: &str) -> Self {
        Self {
            id: id.to_string(),
            age_secs: None,
            stream: Stream::Off,
        }
    }
}

/// The sites on the map, in list order, and which one is the focus (the
/// one the single-valued surfaces describe).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SiteSet {
    pub sites: Vec<OnMap>,
    pub focus: usize,
    /// How many sites the layout shows ([`crate::core::sites::cap`]).
    pub cap: usize,
}

impl SiteSet {
    /// A desktop set holding only `id`.
    #[cfg(test)]
    pub(crate) fn single(id: &str) -> Self {
        Self {
            sites: vec![OnMap::idle(id)],
            focus: 0,
            cap: crate::core::sites::DESKTOP_CAP,
        }
    }

    fn ids(&self) -> Vec<SiteId> {
        self.sites
            .iter()
            .map(|s| SiteId::new(s.id.clone()))
            .collect()
    }

    fn role(&self, id: &str) -> SiteRole {
        match self.sites.iter().position(|s| s.id == id) {
            Some(i) if i == self.focus => SiteRole::Focus,
            Some(_) => SiteRole::Shown,
            None => SiteRole::None,
        }
    }

    /// What a row's secondary action offers for `id`.
    fn action(&self, id: &str) -> RowAction {
        if self.cap <= 1 {
            return RowAction::None;
        }
        match crate::core::sites::add(&self.ids(), &SiteId::new(id), self.cap) {
            Ok(_) => RowAction::AlsoShow,
            Err(Refusal::AlreadyShown) => RowAction::OnTheMap,
            Err(refusal) => RowAction::AtCap(refusal.words()),
        }
    }
}

/// A row's relationship to the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SiteRole {
    /// Not on the map.
    None,
    /// The focus (the filled circle).
    Focus,
    /// On the map beside the focus (a ticked box in the On the map group).
    Shown,
}

/// A row's second action, beside the plain click that picks the site alone.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RowAction {
    /// No second action (a phone).
    None,
    /// "+ Also show" on hover.
    AlsoShow,
    /// The set is full: "+ Also show · 3 sites max", disabled. Carries the
    /// refusal's words.
    AtCap(String),
    /// The site is on the map: the row reads "on the map".
    OnTheMap,
}

impl RowAction {
    /// The words on the row's right: "+ Also show", "+ Also show · 3 sites
    /// max" or "on the map".
    pub(crate) fn label(&self) -> Option<String> {
        match self {
            RowAction::None => None,
            RowAction::AlsoShow => Some("+ Also show".to_string()),
            RowAction::AtCap(why) => Some(format!("+ Also show \u{00B7} {why}")),
            RowAction::OnTheMap => Some("on the map".to_string()),
        }
    }

    /// The action shows only while the row is hovered ("on the map" always
    /// shows).
    pub(crate) fn on_hover_only(&self) -> bool {
        matches!(self, RowAction::AlsoShow | RowAction::AtCap(_))
    }
}

/// The live dot beside an On the map row's age.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveDot {
    /// Streaming: a filled red dot.
    Streaming,
    /// The stream stalled: a hollow red dot.
    Stalled,
}

/// An On the map row's right side: the frame's age ("2 min old") or, for a
/// stalled stream, "stalled · 6 min".
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OnMapStatus {
    pub text: Option<String>,
    pub dot: Option<LiveDot>,
}

fn on_map_status(site: &OnMap) -> OnMapStatus {
    let dot = match site.stream {
        Stream::Stalled => Some(LiveDot::Stalled),
        s if s.is_running() => Some(LiveDot::Streaming),
        _ => None,
    };
    let text = site.age_secs.map(|age| {
        let words = format_age(age);
        if dot == Some(LiveDot::Stalled) {
            format!(
                "stalled \u{00B7} {}",
                words.strip_suffix(" old").unwrap_or(&words)
            )
        } else {
            words
        }
    });
    OnMapStatus {
        text: text.or_else(|| (dot == Some(LiveDot::Stalled)).then(|| "stalled".to_string())),
        dot,
    }
}

/// What the On the map group's header offers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeaderAction {
    /// "Only KTBW".
    pub label: String,
    pub intent: Intent,
}

/// The words under the On the map group.
pub(crate) const ON_THE_MAP_NOTE: &str = "The filled circle is the focus: the ladder, the frame \
     readout, the age pill and the timeline track follow it. Click another row's circle to move \
     the focus; untick a box to remove a site.";

/// Where the Near you group is.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum NearYou {
    /// Not asked yet: the group is a "Use my location" link.
    #[default]
    Idle,
    /// Waiting on the browser or the zip lookup: a spinner in the group.
    Locating,
    /// A fix: the group lists the nearest sites with distances. `zip` names
    /// the zip the fix came from (the group then reads "Near 50309").
    Found {
        lat: f64,
        lon: f64,
        zip: Option<String>,
    },
    /// The lookup failed; the group says why and offers the link again.
    Failed(String),
}

/// One row of the picker.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SiteRow {
    pub id: &'static str,
    /// "Oklahoma City, OK".
    pub place: String,
    /// A TDWR (terminal radar) site, tagged as such.
    pub tdwr: bool,
    pub role: SiteRole,
    /// "38 mi" in the Near you group.
    pub distance: Option<String>,
    pub lat: f64,
    pub lon: f64,
    /// The second action ("+ Also show"); [`RowAction::None`] in the On the
    /// map group, whose rows carry [`Self::on_map`] instead.
    pub action: RowAction,
    /// In the On the map group: the age or stall beside the row.
    pub on_map: Option<OnMapStatus>,
}

impl SiteRow {
    /// A plain click: this site alone.
    pub(crate) fn pick_intent(&self) -> Intent {
        Intent::SelectSite {
            site_id: self.id.to_string(),
            lat: self.lat,
            lon: self.lon,
        }
    }

    /// The "+ Also show" action, when it is enabled.
    pub(crate) fn also_intent(&self) -> Option<Intent> {
        (self.action == RowAction::AlsoShow).then(|| Intent::AddSite {
            site_id: self.id.to_string(),
            lat: self.lat,
            lon: self.lon,
        })
    }

    /// An On the map row's control: the circle of a non-focus row moves the
    /// focus to it; the box of a non-focus row removes it. The focus's own
    /// circle does nothing.
    pub(crate) fn control_intents(&self) -> Option<(Intent, Intent)> {
        (self.on_map.is_some() && self.role == SiteRole::Shown).then(|| {
            (
                Intent::SetFocus {
                    site_id: self.id.to_string(),
                },
                Intent::RemoveSite {
                    site_id: self.id.to_string(),
                },
            )
        })
    }
}

/// Which group a block of rows is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GroupKind {
    /// The primary and companions (only once companions exist).
    OnTheMap,
    NearYou,
    Recent,
    /// Search results.
    Matches,
    /// All sites, one group per state (`None` = outside the US states).
    State(Option<&'static str>),
}

/// A titled block of rows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SiteGroup {
    pub kind: GroupKind,
    pub title: String,
    pub rows: Vec<SiteRow>,
    /// The header's action ("Only KTBW" on the On the map group).
    pub header_action: Option<HeaderAction>,
    /// Words under the group ([`ON_THE_MAP_NOTE`]).
    pub note: Option<&'static str>,
}

impl SiteGroup {
    fn plain(kind: GroupKind, title: String, rows: Vec<SiteRow>) -> Self {
        Self {
            kind,
            title,
            rows,
            header_action: None,
            note: None,
        }
    }
}

/// What the Enter key does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EnterAction {
    /// Nothing matches.
    None,
    /// Take this site (the first match).
    Pick(SiteRow),
    /// Look the zip up (the query is a zip with no fix for it yet).
    LookUpZip(String),
}

/// The picker's view-model.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SitePickerVm {
    pub groups: Vec<SiteGroup>,
    pub enter: EnterAction,
    /// The query is a zip: the Near you group offers to look it up.
    pub zip_query: Option<String>,
    /// Near you is waiting on a fix.
    pub locating: bool,
    /// Why the last lookup failed.
    pub location_error: Option<String>,
    /// How many rows match (all sites for an empty query).
    pub match_count: usize,
}

/// Whether `id` is a TDWR site. Every TDWR ID starts with T; no WSR-88D's
/// does.
pub(crate) fn is_tdwr(id: &str) -> bool {
    id.starts_with('T')
}

fn row(site: &NexradSite, set: &SiteSet, distance: Option<String>) -> SiteRow {
    SiteRow {
        id: site.id,
        place: site.place(),
        tdwr: is_tdwr(site.id),
        role: set.role(site.id),
        distance,
        lat: site.lat,
        lon: site.lon,
        action: set.action(site.id),
        on_map: None,
    }
}

/// "38 mi" (US miles, the units the site list's users read).
pub(crate) fn distance_label(km: f64) -> String {
    let mi = km * 0.621_371;
    if mi < 10.0 {
        format!("{mi:.1} mi")
    } else {
        format!("{mi:.0} mi")
    }
}

/// The `n` sites nearest a point, nearest first, with distances.
pub(crate) fn nearest(lat: f64, lon: f64, n: usize) -> Vec<(&'static NexradSite, f64)> {
    let mut all: Vec<_> = all_sites_sorted()
        .into_iter()
        .map(|s| (s, haversine_distance(lat, lon, s.lat, s.lon)))
        .collect();
    all.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(n);
    all
}

/// A 5-digit US zip in the query.
pub(crate) fn as_zip(query: &str) -> Option<String> {
    let q = query.trim();
    (q.len() == 5 && q.bytes().all(|b| b.is_ascii_digit())).then(|| q.to_string())
}

/// How well `site` matches an uppercase query (lower is better), or `None`.
fn match_rank(site: &NexradSite, q: &str) -> Option<u8> {
    let name = site.name;
    let state = site.state.unwrap_or("");
    if site.id == q {
        Some(0)
    } else if site.id.starts_with(q) {
        Some(1)
    } else if state == q {
        Some(2)
    } else if name.starts_with(q) || name.split_whitespace().any(|w| w.starts_with(q)) {
        Some(3)
    } else if site.id.contains(q) || name.contains(q) {
        Some(4)
    } else {
        // "Oklahoma City, OK" typed as it is shown.
        let shown = format!("{name}, {state}");
        shown.contains(q).then_some(5)
    }
}

/// The inputs the picker reads.
pub(crate) struct SitePickerInputs<'a> {
    pub query: &'a str,
    pub sites: &'a SiteSet,
    /// Most recent first.
    pub recent: &'a [String],
    pub near: &'a NearYou,
}

/// Build the picker's groups for this frame.
pub(crate) fn site_picker_vm(input: &SitePickerInputs<'_>) -> SitePickerVm {
    let set = input.sites;
    let mut groups = Vec::new();

    // On the map: once more than one site is shown, in list order.
    if set.sites.len() > 1 {
        let rows = set
            .sites
            .iter()
            .filter_map(|on| crate::data::get_site(&on.id).map(|s| (on, s)))
            .map(|(on, s)| SiteRow {
                action: RowAction::None,
                on_map: Some(on_map_status(on)),
                ..row(s, set, None)
            })
            .collect();
        let focus_id = set
            .sites
            .get(set.focus)
            .map(|s| s.id.as_str())
            .unwrap_or("");
        groups.push(SiteGroup {
            kind: GroupKind::OnTheMap,
            title: "On the map".into(),
            rows,
            header_action: Some(HeaderAction {
                label: format!("Only {focus_id}"),
                intent: Intent::OnlyFocus,
            }),
            note: Some(ON_THE_MAP_NOTE),
        });
    }

    let zip_query = as_zip(input.query);
    let (locating, location_error) = match input.near {
        NearYou::Locating => (true, None),
        NearYou::Failed(m) => (false, Some(m.clone())),
        _ => (false, None),
    };

    // Near you, from the fix (for a zip query, only the fix for that zip).
    let near_rows = |lat: f64, lon: f64| -> Vec<SiteRow> {
        nearest(lat, lon, NEAR_YOU_COUNT)
            .into_iter()
            .map(|(s, km)| row(s, set, Some(distance_label(km))))
            .collect()
    };
    let zip_fix = match (input.near, &zip_query) {
        (
            NearYou::Found {
                lat,
                lon,
                zip: Some(z),
            },
            Some(q),
        ) if z == q => Some((*lat, *lon)),
        _ => None,
    };

    let q = input.query.trim().to_uppercase();
    let enter;
    let match_count;
    if let Some(zip) = &zip_query {
        let rows = zip_fix
            .map(|(lat, lon)| near_rows(lat, lon))
            .unwrap_or_default();
        match_count = rows.len();
        enter = match rows.first() {
            Some(first) => EnterAction::Pick(first.clone()),
            None => EnterAction::LookUpZip(zip.clone()),
        };
        groups.push(SiteGroup::plain(
            GroupKind::NearYou,
            format!("Near {zip}"),
            rows,
        ));
    } else if q.is_empty() {
        if let NearYou::Found { lat, lon, zip } = input.near {
            groups.push(SiteGroup::plain(
                GroupKind::NearYou,
                match zip {
                    Some(z) => format!("Near {z}"),
                    None => "Near you".into(),
                },
                near_rows(*lat, *lon),
            ));
        } else {
            groups.push(SiteGroup::plain(
                GroupKind::NearYou,
                "Near you".into(),
                Vec::new(),
            ));
        }
        let recent: Vec<SiteRow> = input
            .recent
            .iter()
            .filter_map(|id| crate::data::get_site(id))
            .map(|s| row(s, set, None))
            .collect();
        if !recent.is_empty() {
            groups.push(SiteGroup::plain(GroupKind::Recent, "Recent".into(), recent));
        }
        // All sites, grouped by state, states alphabetical, the rest last.
        let mut all = all_sites_sorted();
        all.sort_by_key(|s| (s.state.is_none(), s.state, s.id));
        match_count = all.len();
        let mut current: Option<SiteGroup> = None;
        for s in all {
            let starts_new = current
                .as_ref()
                .is_none_or(|g| g.kind != GroupKind::State(s.state));
            if starts_new {
                if let Some(g) = current.take() {
                    groups.push(g);
                }
                current = Some(SiteGroup::plain(
                    GroupKind::State(s.state),
                    s.state.unwrap_or("Outside the US states").to_string(),
                    Vec::new(),
                ));
            }
            if let Some(g) = current.as_mut() {
                g.rows.push(row(s, set, None));
            }
        }
        groups.extend(current);
        enter = EnterAction::None;
    } else {
        let mut ranked: Vec<(u8, &NexradSite)> = all_sites_sorted()
            .into_iter()
            .filter_map(|s| match_rank(s, &q).map(|r| (r, s)))
            .collect();
        ranked.sort_by_key(|(r, s)| (*r, s.id));
        let rows: Vec<SiteRow> = ranked.into_iter().map(|(_, s)| row(s, set, None)).collect();
        match_count = rows.len();
        enter = rows
            .first()
            .cloned()
            .map(EnterAction::Pick)
            .unwrap_or(EnterAction::None);
        groups.push(SiteGroup::plain(GroupKind::Matches, "Matches".into(), rows));
    }

    SitePickerVm {
        groups,
        enter,
        zip_query,
        locating,
        location_error,
        match_count,
    }
}

/// Remember `id` as the most recent site (front), without duplicates.
pub(crate) fn push_recent(recent: &mut Vec<String>, id: &str) {
    recent.retain(|r| r != id);
    recent.insert(0, id.to_string());
    recent.truncate(RECENT_CAP);
}

/// What a location result does to the picker.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LocationOutcome {
    /// The Near you group's next state.
    pub near: NearYou,
    /// Also show the user's position on the map (a physical fix, not a zip).
    pub show_location: Option<(f64, f64)>,
}

/// Decide what a geolocation / zip result does. `zip` is the zip the lookup
/// was for (it names the group). It lists the nearest sites; it never
/// switches site by itself.
pub(crate) fn on_location(result: LocationResult, zip: Option<String>) -> LocationOutcome {
    match result {
        LocationResult::Success { lat, lon, source } => {
            if crate::data::nearest_site(lat, lon).is_none() {
                return LocationOutcome {
                    near: NearYou::Failed("Could not find a nearby site".into()),
                    show_location: None,
                };
            }
            let show_location = crate::core::effect::enable_location_layer_on_site_pick(source)
                .then_some((lat, lon));
            LocationOutcome {
                near: NearYou::Found {
                    lat,
                    lon,
                    zip: if source == LocationSource::Zip {
                        zip
                    } else {
                        None
                    },
                },
                show_location,
            }
        }
        LocationResult::Error(msg) => LocationOutcome {
            near: NearYou::Failed(msg),
            show_location: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn vm(query: &str, near: &NearYou, recent: &[String]) -> SitePickerVm {
        site_picker_vm(&SitePickerInputs {
            query,
            sites: &SiteSet::single("KDMX"),
            recent,
            near,
        })
    }

    #[wasm_bindgen_test]
    fn empty_query_shows_near_you_recent_and_all_sites_by_state() {
        let recent = vec!["KTLX".to_string()];
        let v = vm("", &NearYou::Idle, &recent);
        assert_eq!(v.groups[0].kind, GroupKind::NearYou);
        assert!(v.groups[0].rows.is_empty(), "idle: just the link");
        assert_eq!(v.groups[1].kind, GroupKind::Recent);
        assert_eq!(v.groups[1].rows[0].id, "KTLX");
        let states: Vec<_> = v.groups[2..].iter().map(|g| g.kind.clone()).collect();
        assert!(states.contains(&GroupKind::State(Some("IA"))));
        assert_eq!(states.last(), Some(&GroupKind::State(None)));
        let total: usize = v.groups[2..].iter().map(|g| g.rows.len()).sum();
        assert_eq!(total, all_sites_sorted().len());
        assert_eq!(v.enter, EnterAction::None);
    }

    #[wasm_bindgen_test]
    fn the_current_site_is_marked_not_excluded() {
        let v = vm("KDMX", &NearYou::Idle, &[]);
        let first = &v.groups[0].rows[0];
        assert_eq!(first.id, "KDMX");
        assert_eq!(first.role, SiteRole::Focus);
        assert_eq!(first.action, RowAction::OnTheMap);
        // Enter takes it: a first visit can keep the default site.
        assert!(matches!(&v.enter, EnterAction::Pick(r) if r.id == "KDMX"));
    }

    #[wasm_bindgen_test]
    fn search_by_city_and_state() {
        let v = vm("oklahoma", &NearYou::Idle, &[]);
        let ids: Vec<_> = v.groups[0].rows.iter().map(|r| r.id).collect();
        assert!(ids.contains(&"KTLX"));
        let ok = vm("ok", &NearYou::Idle, &[]);
        assert!(ok.groups[0].rows.iter().any(|r| r.id == "KINX"));
    }

    #[wasm_bindgen_test]
    fn id_prefix_ranks_before_a_city_match() {
        // "KTL" is an ID prefix of KTLH and KTLX and no city's start.
        let v = vm("ktl", &NearYou::Idle, &[]);
        let rows = &v.groups[0].rows;
        assert!(rows[0].id.starts_with("KTL") && rows[1].id.starts_with("KTL"));
        // An exact ID beats everything.
        let v = vm("ktlx", &NearYou::Idle, &[]);
        assert_eq!(v.groups[0].rows[0].id, "KTLX");
    }

    #[wasm_bindgen_test]
    fn tdwr_sites_are_tagged() {
        let v = vm("oklahoma", &NearYou::Idle, &[]);
        let tokc = v.groups[0].rows.iter().find(|r| r.id == "TOKC").unwrap();
        assert!(tokc.tdwr);
        let ktlx = v.groups[0].rows.iter().find(|r| r.id == "KTLX").unwrap();
        assert!(!ktlx.tdwr);
    }

    #[wasm_bindgen_test]
    fn a_zip_query_looks_up_then_lists_the_nearest() {
        let v = vm("50309", &NearYou::Idle, &[]);
        assert_eq!(v.zip_query.as_deref(), Some("50309"));
        assert_eq!(v.enter, EnterAction::LookUpZip("50309".into()));
        // Des Moines.
        let near = NearYou::Found {
            lat: 41.58,
            lon: -93.62,
            zip: Some("50309".into()),
        };
        let v = vm("50309", &near, &[]);
        assert_eq!(v.groups[0].title, "Near 50309");
        assert_eq!(v.groups[0].rows[0].id, "KDMX");
        assert!(v.groups[0].rows[0].distance.is_some());
        assert!(matches!(&v.enter, EnterAction::Pick(r) if r.id == "KDMX"));
    }

    #[wasm_bindgen_test]
    fn a_location_fix_fills_near_you_with_distances() {
        let near = NearYou::Found {
            lat: 35.33,
            lon: -97.28,
            zip: None,
        };
        let v = vm("", &near, &[]);
        assert_eq!(v.groups[0].title, "Near you");
        assert_eq!(v.groups[0].rows.len(), NEAR_YOU_COUNT);
        assert!(v.groups[0].rows.iter().all(|r| r.distance.is_some()));
    }

    #[wasm_bindgen_test]
    fn locating_and_failure_are_reported() {
        assert!(vm("", &NearYou::Locating, &[]).locating);
        let f = vm("", &NearYou::Failed("denied".into()), &[]);
        assert_eq!(f.location_error.as_deref(), Some("denied"));
    }

    fn milton() -> SiteSet {
        SiteSet {
            sites: vec![
                OnMap {
                    id: "KTBW".into(),
                    age_secs: Some(130.0),
                    stream: Stream::Receiving,
                },
                OnMap {
                    id: "KMLB".into(),
                    age_secs: Some(190.0),
                    stream: Stream::Waiting,
                },
                OnMap {
                    id: "KJAX".into(),
                    age_secs: Some(370.0),
                    stream: Stream::Stalled,
                },
            ],
            focus: 0,
            cap: 3,
        }
    }

    fn vm_for(set: &SiteSet, query: &str, recent: &[String]) -> SitePickerVm {
        site_picker_vm(&SitePickerInputs {
            query,
            sites: set,
            recent,
            near: &NearYou::Idle,
        })
    }

    #[wasm_bindgen_test]
    fn one_site_has_no_on_the_map_group() {
        let v = vm("", &NearYou::Idle, &[]);
        assert!(v.groups.iter().all(|g| g.kind != GroupKind::OnTheMap));
    }

    #[wasm_bindgen_test]
    fn several_sites_head_the_list_with_on_the_map() {
        let v = vm_for(&milton(), "", &[]);
        let g = &v.groups[0];
        assert_eq!(g.kind, GroupKind::OnTheMap);
        assert_eq!(g.title, "On the map");
        let ids: Vec<_> = g.rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, ["KTBW", "KMLB", "KJAX"]);
        assert_eq!(g.rows[0].role, SiteRole::Focus);
        assert_eq!(g.rows[1].role, SiteRole::Shown);
        assert_eq!(g.rows[2].role, SiteRole::Shown);
        assert_eq!(
            g.header_action,
            Some(HeaderAction {
                label: "Only KTBW".into(),
                intent: Intent::OnlyFocus,
            })
        );
        assert_eq!(g.note, Some(ON_THE_MAP_NOTE));
        // The group's rows carry their status, not "+ Also show".
        assert!(g.rows.iter().all(|r| r.action == RowAction::None));
    }

    #[wasm_bindgen_test]
    fn on_the_map_rows_read_their_age_or_stall() {
        let v = vm_for(&milton(), "", &[]);
        let status: Vec<_> = v.groups[0]
            .rows
            .iter()
            .map(|r| r.on_map.clone().unwrap())
            .collect();
        assert_eq!(status[0].text.as_deref(), Some("2 min old"));
        assert_eq!(status[0].dot, Some(LiveDot::Streaming));
        assert_eq!(status[1].text.as_deref(), Some("3 min old"));
        assert_eq!(status[1].dot, Some(LiveDot::Streaming));
        assert_eq!(status[2].text.as_deref(), Some("stalled \u{00B7} 6 min"));
        assert_eq!(status[2].dot, Some(LiveDot::Stalled));
        // Archive: the age alone, no dot.
        let mut set = milton();
        for s in &mut set.sites {
            s.stream = Stream::Off;
        }
        let v = vm_for(&set, "", &[]);
        let jax = v.groups[0].rows[2].on_map.clone().unwrap();
        assert_eq!(jax.text.as_deref(), Some("6 min old"));
        assert_eq!(jax.dot, None);
        // No frame yet: nothing to say.
        set.sites[1].age_secs = None;
        let v = vm_for(&set, "", &[]);
        assert_eq!(v.groups[0].rows[1].on_map.clone().unwrap().text, None);
    }

    #[wasm_bindgen_test]
    fn on_the_map_controls_move_the_focus_or_remove_the_site() {
        let v = vm_for(&milton(), "", &[]);
        let rows = &v.groups[0].rows;
        // The focus's own circle does nothing; its box is the circle.
        assert_eq!(rows[0].control_intents(), None);
        assert_eq!(
            rows[1].control_intents(),
            Some((
                Intent::SetFocus {
                    site_id: "KMLB".into()
                },
                Intent::RemoveSite {
                    site_id: "KMLB".into()
                },
            ))
        );
        // Moving the focus moves the filled circle and the "Only" label.
        let mut set = milton();
        set.focus = 2;
        let v = vm_for(&set, "", &[]);
        assert_eq!(v.groups[0].rows[2].role, SiteRole::Focus);
        assert_eq!(
            v.groups[0].header_action.as_ref().unwrap().label,
            "Only KJAX"
        );
    }

    #[wasm_bindgen_test]
    fn other_rows_offer_also_show_until_the_cap() {
        let set = SiteSet {
            sites: vec![OnMap::idle("KTBW"), OnMap::idle("KMLB")],
            focus: 0,
            cap: 3,
        };
        let v = vm_for(&set, "KAMX", &[]);
        let amx = &v.groups[1].rows[0];
        assert_eq!(amx.id, "KAMX");
        assert_eq!(amx.action, RowAction::AlsoShow);
        assert_eq!(amx.action.label().as_deref(), Some("+ Also show"));
        assert!(amx.action.on_hover_only());
        assert_eq!(
            amx.also_intent(),
            Some(Intent::AddSite {
                site_id: "KAMX".into(),
                lat: amx.lat,
                lon: amx.lon,
            })
        );
        // At the cap the action says why and does nothing.
        let v = vm_for(&milton(), "KAMX", &[]);
        let amx = &v.groups[1].rows[0];
        assert_eq!(amx.action, RowAction::AtCap("3 sites max".into()));
        assert_eq!(
            amx.action.label().as_deref(),
            Some("+ Also show \u{00B7} 3 sites max")
        );
        assert_eq!(amx.also_intent(), None);
    }

    #[wasm_bindgen_test]
    fn rows_already_on_the_map_read_on_the_map() {
        let recent = vec!["KMLB".to_string(), "KDMX".to_string()];
        let v = vm_for(&milton(), "", &recent);
        let recent_group = v
            .groups
            .iter()
            .find(|g| g.kind == GroupKind::Recent)
            .unwrap();
        assert_eq!(recent_group.rows[0].action, RowAction::OnTheMap);
        assert_eq!(
            recent_group.rows[0].action.label().as_deref(),
            Some("on the map")
        );
        assert!(!recent_group.rows[0].action.on_hover_only());
        assert_eq!(recent_group.rows[0].also_intent(), None);
        assert_eq!(recent_group.rows[0].control_intents(), None);
        assert_eq!(
            recent_group.rows[1].action,
            RowAction::AtCap("3 sites max".into())
        );
    }

    #[wasm_bindgen_test]
    fn a_plain_click_picks_the_site_alone() {
        let v = vm_for(&milton(), "KAMX", &[]);
        let amx = &v.groups[1].rows[0];
        assert_eq!(
            amx.pick_intent(),
            Intent::SelectSite {
                site_id: "KAMX".into(),
                lat: amx.lat,
                lon: amx.lon,
            }
        );
    }

    #[wasm_bindgen_test]
    fn a_phone_keeps_the_single_site_picker() {
        let set = SiteSet {
            sites: vec![OnMap::idle("KTBW")],
            focus: 0,
            cap: 1,
        };
        let v = vm_for(&set, "", &["KMLB".to_string()]);
        assert!(v.groups.iter().all(|g| g.kind != GroupKind::OnTheMap));
        assert!(v
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .all(|r| r.action == RowAction::None));
    }

    #[wasm_bindgen_test]
    fn recent_is_most_recent_first_unique_and_capped() {
        let mut r = Vec::new();
        for id in ["A", "B", "C", "A", "D", "E", "F"] {
            push_recent(&mut r, id);
        }
        assert_eq!(r, vec!["F", "E", "D", "A", "C"]);
    }

    #[wasm_bindgen_test]
    fn location_results() {
        let ok = LocationResult::Success {
            lat: 41.58,
            lon: -93.62,
            source: LocationSource::Geolocation,
        };
        let out = on_location(ok, None);
        assert!(matches!(out.near, NearYou::Found { zip: None, .. }));
        assert_eq!(out.show_location, Some((41.58, -93.62)));

        let zip = LocationResult::Success {
            lat: 41.58,
            lon: -93.62,
            source: LocationSource::Zip,
        };
        let out = on_location(zip, Some("50309".into()));
        assert!(matches!(&out.near, NearYou::Found { zip: Some(z), .. } if z == "50309"));
        assert_eq!(out.show_location, None, "a zip is not where you are");

        let err = on_location(LocationResult::Error("denied".into()), None);
        assert_eq!(err.near, NearYou::Failed("denied".into()));
    }

    #[wasm_bindgen_test]
    fn distances_read_in_miles() {
        assert_eq!(distance_label(100.0), "62 mi");
        assert_eq!(distance_label(5.0), "3.1 mi");
    }
}
