//! The site picker (One Instrument answers, "Choosing a site"): one popover
//! under the site control. One search field takes a site ID, a city, a state
//! or a zip; the groups under it answer the three ways people pick — the
//! sites near them, the ones they were just looking at, and the full list.
//!
//! Pure: [`site_picker_vm`] turns the query, the site set on the map, the
//! recent list and the "near you" fix into rows; [`on_location`] decides what
//! a geolocation or zip result does. The egui popover is `ui::site_modal`.
//!
//! **Several sites, later.** The model already speaks of a [`SiteSet`] — one
//! primary and its companions — and rows carry a [`SiteRole`]. Today the set
//! always has no companions, so the "On the map" group is never built; when
//! companions arrive the picker gains that group at the top and every row a
//! second action, with nothing else in the picker changing.

use crate::core::{LocationResult, LocationSource};
use crate::data::sites::{all_sites_sorted, haversine_distance, NexradSite};

/// How many sites the Near you group lists.
pub(crate) const NEAR_YOU_COUNT: usize = 4;
/// How many sites the Recent group remembers.
pub(crate) const RECENT_CAP: usize = 5;

/// The sites on the map: one primary that the single-valued surfaces
/// describe, and companions (none yet — single-site build).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SiteSet {
    pub primary: String,
    pub companions: Vec<String>,
}

impl SiteSet {
    /// A set holding only `primary`.
    pub(crate) fn single(primary: &str) -> Self {
        Self {
            primary: primary.to_string(),
            companions: Vec::new(),
        }
    }

    fn role(&self, id: &str) -> SiteRole {
        if id == self.primary {
            SiteRole::Primary
        } else if self.companions.iter().any(|c| c == id) {
            SiteRole::Companion
        } else {
            SiteRole::None
        }
    }
}

/// A row's relationship to the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SiteRole {
    /// Not on the map.
    None,
    /// The primary site (the filled circle; "current").
    Primary,
    /// Shown alongside the primary (future).
    Companion,
}

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

    // On the map: only once a companion exists (the single-site build never
    // has one).
    if !set.companions.is_empty() {
        let rows = std::iter::once(&set.primary)
            .chain(set.companions.iter())
            .filter_map(|id| crate::data::get_site(id))
            .map(|s| row(s, set, None))
            .collect();
        groups.push(SiteGroup {
            kind: GroupKind::OnTheMap,
            title: "On the map".into(),
            rows,
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
        groups.push(SiteGroup {
            kind: GroupKind::NearYou,
            title: format!("Near {zip}"),
            rows,
        });
    } else if q.is_empty() {
        if let NearYou::Found { lat, lon, zip } = input.near {
            groups.push(SiteGroup {
                kind: GroupKind::NearYou,
                title: match zip {
                    Some(z) => format!("Near {z}"),
                    None => "Near you".into(),
                },
                rows: near_rows(*lat, *lon),
            });
        } else {
            groups.push(SiteGroup {
                kind: GroupKind::NearYou,
                title: "Near you".into(),
                rows: Vec::new(),
            });
        }
        let recent: Vec<SiteRow> = input
            .recent
            .iter()
            .filter_map(|id| crate::data::get_site(id))
            .map(|s| row(s, set, None))
            .collect();
        if !recent.is_empty() {
            groups.push(SiteGroup {
                kind: GroupKind::Recent,
                title: "Recent".into(),
                rows: recent,
            });
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
                current = Some(SiteGroup {
                    kind: GroupKind::State(s.state),
                    title: s.state.unwrap_or("Outside the US states").to_string(),
                    rows: Vec::new(),
                });
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
        groups.push(SiteGroup {
            kind: GroupKind::Matches,
            title: "Matches".into(),
            rows,
        });
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
    /// Pick this site now (the phone's "use my location" button picks the
    /// nearest; the picker's link only lists them).
    pub pick: Option<(&'static str, f64, f64)>,
    /// Also show the user's position on the map (a physical fix, not a zip).
    pub show_location: Option<(f64, f64)>,
}

/// Decide what a geolocation / zip result does. `zip` is the zip the lookup
/// was for (it names the group); `auto_pick` picks the nearest site.
pub(crate) fn on_location(
    result: LocationResult,
    zip: Option<String>,
    auto_pick: bool,
) -> LocationOutcome {
    match result {
        LocationResult::Success { lat, lon, source } => {
            let Some(site) = crate::data::nearest_site(lat, lon) else {
                return LocationOutcome {
                    near: NearYou::Failed("Could not find a nearby site".into()),
                    pick: None,
                    show_location: None,
                };
            };
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
                pick: auto_pick.then_some((site.id, site.lat, site.lon)),
                show_location,
            }
        }
        LocationResult::Error(msg) => LocationOutcome {
            near: NearYou::Failed(msg),
            pick: None,
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
        assert_eq!(first.role, SiteRole::Primary);
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

    #[wasm_bindgen_test]
    fn no_companions_no_on_the_map_group() {
        let v = vm("", &NearYou::Idle, &[]);
        assert!(v.groups.iter().all(|g| g.kind != GroupKind::OnTheMap));
        let set = SiteSet {
            primary: "KTLX".into(),
            companions: vec!["KINX".into()],
        };
        let v = site_picker_vm(&SitePickerInputs {
            query: "",
            sites: &set,
            recent: &[],
            near: &NearYou::Idle,
        });
        assert_eq!(v.groups[0].kind, GroupKind::OnTheMap);
        assert_eq!(v.groups[0].rows[0].role, SiteRole::Primary);
        assert_eq!(v.groups[0].rows[1].role, SiteRole::Companion);
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
        let out = on_location(ok, None, false);
        assert!(matches!(out.near, NearYou::Found { zip: None, .. }));
        assert_eq!(out.pick, None);
        assert_eq!(out.show_location, Some((41.58, -93.62)));

        let zip = LocationResult::Success {
            lat: 41.58,
            lon: -93.62,
            source: LocationSource::Zip,
        };
        let out = on_location(zip, Some("50309".into()), true);
        assert!(matches!(&out.near, NearYou::Found { zip: Some(z), .. } if z == "50309"));
        assert_eq!(out.pick.map(|p| p.0), Some("KDMX"));
        assert_eq!(out.show_location, None, "a zip is not where you are");

        let err = on_location(LocationResult::Error("denied".into()), None, true);
        assert_eq!(err.near, NearYou::Failed("denied".into()));
        assert_eq!(err.pick, None);
    }

    #[wasm_bindgen_test]
    fn distances_read_in_miles() {
        assert_eq!(distance_label(100.0), "62 mi");
        assert_eq!(distance_label(5.0), "3.1 mi");
    }
}
