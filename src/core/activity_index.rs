//! The published storm-activity index: one file per site, built nightly
//! from the archive listings (`tools/activity_index/`), holding each UTC
//! day's volume count, mean volume size and format.
//!
//! With it the overview's day bars need no listing at all: the shell reads a
//! site's file once per session (cached in IndexedDB and revalidated by
//! ETag), and [`crate::core::storm_activity::ActivityFetcher::plan`] only
//! lists the days the index doesn't cover (the newest, and gaps while the
//! backfill runs). Without an index (a dev build, or before the first
//! nightly run) everything is listed live as before.
//!
//! Zoomed in to volumes, the per-volume sizes come from the index's month
//! files (`activity-index/<SITE>/<YYYY-MM>.json`) for the days the site
//! file marks as having them (its `scans` runs): the plan fetches the
//! month instead of listing each of its days, and lists only the days no
//! month file holds. A month's days are stored like listed days, so a month
//! is fetched again only for days the cache doesn't have.
//!
//! Everything here is pure: the files' paths, parsing them into
//! [`DayActivity`] values, and deciding what a fetch's response means for
//! the cached copy ([`resolve_index`], [`resolve_month`]).

use crate::core::storm_activity::{final_fetched_at_ms, volume_format};
use crate::data::{ActivityIndexRecord, DayActivity, SiteId};
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;

/// The file format this build reads (`"v"` in the file).
pub(crate) const INDEX_FORMAT_VERSION: u32 = 1;

/// The month files' format this build reads (`"v"` in a month file).
pub(crate) const MONTH_FORMAT_VERSION: u32 = 1;

/// More volumes than a radar records in a day (one a minute is ~1,440), so
/// a corrupt month file can't make the parser allocate without bound.
const MAX_DAY_VOLUMES: usize = 10_000;

/// Directory of the index files, beside the deployed site.
const INDEX_DIR: &str = "activity-index";

/// More days than the archive will ever hold (~270 years), so a corrupt
/// file can't make the parser allocate without bound.
const MAX_INDEX_DAYS: usize = 100_000;

const DAY_SECS: i64 = 86_400;

/// The site's index file, as a path on the app's own origin.
///
/// The index sits at the root of the GitHub Pages deploy, beside
/// `index.html`. A PR preview is deployed under `pr-preview/pr-N/` of the
/// same site, so its index is the production one: the path is cut back to
/// before `pr-preview/`. Otherwise it is the page's own directory
/// (`/nexrad-workbench/` in production, `/` under `trunk serve`).
pub(crate) fn index_path(page_path: &str, site: &SiteId) -> String {
    format!(
        "{}{INDEX_DIR}/{}.json",
        deploy_root(page_path),
        file_id(site)
    )
}

/// A site-month's file of volumes (`activity-index/KTLX/2026-10.json`),
/// beside the site's index file ([`index_path`]).
pub(crate) fn month_path(page_path: &str, site: &SiteId, month: MonthKey) -> String {
    format!(
        "{}{INDEX_DIR}/{}/{}.json",
        deploy_root(page_path),
        file_id(site),
        month.name()
    )
}

/// The deploy's root directory, ending in `/` (see [`index_path`]).
fn deploy_root(page_path: &str) -> &str {
    let root = match page_path.find("/pr-preview/") {
        Some(i) => &page_path[..=i],
        None => match page_path.rfind('/') {
            Some(i) => &page_path[..=i],
            None => "/",
        },
    };
    if root.starts_with('/') {
        root
    } else {
        "/"
    }
}

/// The site id as it may appear in a path.
fn file_id(site: &SiteId) -> String {
    site.0
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// A UTC calendar month, the unit of the index's month files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct MonthKey {
    pub year: i32,
    /// 1..=12.
    pub month: u32,
}

impl MonthKey {
    /// The month holding the day starting at `day_start` (Unix seconds).
    pub(crate) fn of_day(day_start: i64) -> Option<Self> {
        let date = chrono::DateTime::from_timestamp(day_start, 0)?.date_naive();
        Some(Self {
            year: date.year(),
            month: date.month(),
        })
    }

    fn first_date(self) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(self.year, self.month, 1)
    }

    /// UTC midnight of the month's first day (Unix seconds).
    pub(crate) fn first_day_start(self) -> Option<i64> {
        Some(
            self.first_date()?
                .and_hms_opt(0, 0, 0)?
                .and_utc()
                .timestamp(),
        )
    }

    /// How many days the month has.
    pub(crate) fn days(self) -> Option<u32> {
        let first = self.first_date()?;
        let next = if self.month == 12 {
            NaiveDate::from_ymd_opt(self.year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(self.year, self.month + 1, 1)?
        };
        Some((next - first).num_days() as u32)
    }

    /// `"2026-10"`, as the month file is named.
    pub(crate) fn name(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
}

/// Why an index file couldn't be read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IndexError {
    /// Not JSON, or not this shape.
    Malformed(String),
    /// A format this build doesn't know.
    Version(u32),
    /// Another site's file.
    WrongSite(String),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed index: {e}"),
            Self::Version(v) => write!(f, "index format v{v} not supported"),
            Self::WrongSite(s) => write!(f, "index is for {s}"),
        }
    }
}

#[derive(Deserialize)]
struct IndexFile {
    v: u32,
    site: String,
    first_day: i64,
    vols: Vec<Option<u32>>,
    mean: Vec<Option<u64>>,
    fmt: Vec<(usize, String)>,
    /// `(index, count)` runs of the days with a month file entry; missing
    /// in files built before month files.
    #[serde(default)]
    scans: Vec<(usize, usize)>,
}

/// What a site's index file says: its days, and which of them have their
/// volumes in a month file.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct SiteIndex {
    pub days: Vec<DayActivity>,
    /// Starts of the indexed days whose volumes a month file holds, in
    /// time order.
    pub month_days: Vec<i64>,
}

/// Whether `format` is one a listing could have produced (`""`, `"V06"`).
fn is_format(format: &str) -> bool {
    format.is_empty() || (format.len() <= 8 && volume_format(&format!("x_{format}")) == format)
}

/// The days in a site's index file, each marked final (an index only holds
/// days that had ended three hours before it was built) and without
/// per-volume sizes, and the days its month files hold. Days the file has
/// as `null` (not indexed yet) are left out, so they stay unknown and get
/// listed.
pub(crate) fn parse_index(site: &SiteId, text: &str) -> Result<SiteIndex, IndexError> {
    #[derive(Deserialize)]
    struct Version {
        v: u32,
    }
    let version: Version =
        serde_json::from_str(text).map_err(|e| IndexError::Malformed(e.to_string()))?;
    if version.v != INDEX_FORMAT_VERSION {
        return Err(IndexError::Version(version.v));
    }
    let file: IndexFile =
        serde_json::from_str(text).map_err(|e| IndexError::Malformed(e.to_string()))?;
    if file.v != INDEX_FORMAT_VERSION {
        return Err(IndexError::Version(file.v));
    }
    if file.site != site.0 {
        return Err(IndexError::WrongSite(file.site));
    }
    let malformed = |what: &str| Err(IndexError::Malformed(what.to_string()));
    if file.vols.len() != file.mean.len() {
        return malformed("vols and mean differ in length");
    }
    if file.vols.len() > MAX_INDEX_DAYS {
        return malformed("too many days");
    }
    if !(0..=MAX_INDEX_DAYS as i64).contains(&file.first_day) {
        return malformed("first_day out of range");
    }
    if file.fmt.windows(2).any(|w| w[0].0 >= w[1].0)
        || file
            .fmt
            .iter()
            .any(|(i, f)| *i >= file.vols.len() || !is_format(f))
    {
        return malformed("bad format runs");
    }
    if file.scans.windows(2).any(|w| w[0].0 + w[0].1 > w[1].0)
        || file
            .scans
            .iter()
            .any(|(i, n)| i.checked_add(*n).is_none_or(|end| end > file.vols.len()))
    {
        return malformed("bad scans runs");
    }

    let mut runs = file.fmt.iter().peekable();
    let mut format = "";
    let mut out = Vec::with_capacity(file.vols.len());
    for (i, (vols, mean)) in file.vols.iter().zip(&file.mean).enumerate() {
        while let Some((_, f)) = runs.next_if(|(start, _)| *start <= i) {
            format = f;
        }
        let (volumes, mean_bytes) = match (vols, mean) {
            (None, None) => continue,
            (Some(v), Some(m)) => (*v, if *v == 0 { 0 } else { *m }),
            _ => return malformed("vols and mean disagree on a missing day"),
        };
        let day_start = (file.first_day + i as i64) * DAY_SECS;
        out.push(DayActivity {
            site: site.clone(),
            day_start,
            volumes,
            mean_bytes,
            format: if volumes == 0 {
                String::new()
            } else {
                format.to_string()
            },
            fetched_at_ms: final_fetched_at_ms(day_start),
            has_scans: false,
            scan_secs: Vec::new(),
            scan_bytes: Vec::new(),
        });
    }
    let month_days = file
        .scans
        .iter()
        .flat_map(|(i, n)| *i..i + n)
        .filter(|i| file.vols[*i].is_some())
        .map(|i| (file.first_day + i as i64) * DAY_SECS)
        .collect();
    Ok(SiteIndex {
        days: out,
        month_days,
    })
}

/// A month file's day: `[volumes, mean_bytes, format, starts, sizes]`, the
/// starts delta-encoded.
type MonthDay = (u32, u64, String, Vec<u32>, Vec<u32>);

#[derive(Deserialize)]
struct MonthFile {
    v: u32,
    site: String,
    month: String,
    days: Vec<Option<MonthDay>>,
}

/// The days in a site-month's file, each final and with its per-volume
/// sizes, as a live listing of that day would have them. Days the file has
/// as `null` are left out (they get listed).
pub(crate) fn parse_month(
    site: &SiteId,
    month: MonthKey,
    text: &str,
) -> Result<Vec<DayActivity>, IndexError> {
    #[derive(Deserialize)]
    struct Version {
        v: u32,
    }
    let version: Version =
        serde_json::from_str(text).map_err(|e| IndexError::Malformed(e.to_string()))?;
    if version.v != MONTH_FORMAT_VERSION {
        return Err(IndexError::Version(version.v));
    }
    let file: MonthFile =
        serde_json::from_str(text).map_err(|e| IndexError::Malformed(e.to_string()))?;
    if file.site != site.0 {
        return Err(IndexError::WrongSite(file.site));
    }
    let malformed = |what: &str| Err(IndexError::Malformed(what.to_string()));
    let (Some(first), Some(len)) = (month.first_day_start(), month.days()) else {
        return malformed("month out of range");
    };
    if file.v != MONTH_FORMAT_VERSION || file.month != month.name() {
        return malformed("another month");
    }
    if file.days.len() != len as usize {
        return malformed("not one entry per day");
    }
    let mut out = Vec::new();
    for (i, day) in file.days.into_iter().enumerate() {
        let Some((volumes, mean, format, deltas, sizes)) = day else {
            continue;
        };
        if deltas.len() != sizes.len()
            || deltas.len() > volumes as usize
            || deltas.len() > MAX_DAY_VOLUMES
            || !is_format(&format)
        {
            return malformed("bad day");
        }
        let mut secs = 0u32;
        let mut scan_secs = Vec::with_capacity(deltas.len());
        for d in deltas {
            secs = match secs.checked_add(d) {
                Some(s) if s < DAY_SECS as u32 => s,
                _ => return malformed("volume start past the day"),
            };
            scan_secs.push(secs);
        }
        let day_start = first + i as i64 * DAY_SECS;
        out.push(DayActivity {
            site: site.clone(),
            day_start,
            volumes,
            mean_bytes: if volumes == 0 { 0 } else { mean },
            format: if volumes == 0 { String::new() } else { format },
            fetched_at_ms: final_fetched_at_ms(day_start),
            has_scans: true,
            scan_secs,
            scan_bytes: sizes,
        });
    }
    Ok(out)
}

/// What a fetch of a month file means: its days (none for a month that
/// isn't published or doesn't parse, whose days then get listed), or a
/// failure to retry after a backoff (offline, a server error).
pub(crate) fn resolve_month(
    site: &SiteId,
    month: MonthKey,
    response: IndexResponse,
) -> Result<Vec<DayActivity>, String> {
    match response {
        IndexResponse::Fetched { body, .. } => {
            Ok(parse_month(site, month, &body).unwrap_or_default())
        }
        IndexResponse::NotFound => Ok(Vec::new()),
        // Month files aren't revalidated by hand (their days are stored
        // instead), so a 304 can only mean a confused cache.
        IndexResponse::NotModified | IndexResponse::Failed => {
            Err("month file unavailable".to_string())
        }
    }
}

/// How a request for the index file came back.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IndexResponse {
    /// 304: the cached copy is current.
    NotModified,
    /// 200, with the body and its ETag.
    Fetched { body: String, etag: Option<String> },
    /// 404: no index for this site (a dev build, or not published yet).
    NotFound,
    /// Anything else (offline, a server error).
    Failed,
}

/// What a fetch of the index means: the days to use, and what to do with
/// the cached copy.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct IndexOutcome {
    /// The index's days (empty without a usable index).
    pub days: Vec<DayActivity>,
    /// The index's days that have a month file entry.
    pub month_days: Vec<i64>,
    /// A new copy to cache.
    pub save: Option<ActivityIndexRecord>,
    /// Drop the cached copy (the index is gone, or the copy is unreadable).
    pub forget: bool,
}

/// The `If-None-Match` value for revalidating the cached copy.
pub(crate) fn revalidation_tag(cached: Option<&ActivityIndexRecord>) -> Option<&str> {
    cached.and_then(|c| c.etag.as_deref())
}

/// Decide what an index response means, given the cached copy. A good new
/// file replaces the cache; a 304 or a failure keeps using the cached copy;
/// a 404 drops it. A body that doesn't parse (a dev server answering with
/// `index.html`, a format from a newer build) is never cached.
pub(crate) fn resolve_index(
    site: &SiteId,
    cached: Option<ActivityIndexRecord>,
    response: IndexResponse,
) -> IndexOutcome {
    let from_cache = |cached: Option<ActivityIndexRecord>| match cached {
        None => IndexOutcome::default(),
        Some(c) => match parse_index(site, &c.body) {
            Ok(index) => IndexOutcome {
                days: index.days,
                month_days: index.month_days,
                ..Default::default()
            },
            Err(_) => IndexOutcome {
                forget: true,
                ..Default::default()
            },
        },
    };
    match response {
        IndexResponse::NotModified | IndexResponse::Failed => from_cache(cached),
        IndexResponse::NotFound => IndexOutcome {
            forget: cached.is_some(),
            ..Default::default()
        },
        IndexResponse::Fetched { body, etag } => match parse_index(site, &body) {
            Ok(index) => IndexOutcome {
                days: index.days,
                month_days: index.month_days,
                save: Some(ActivityIndexRecord {
                    site: site.clone(),
                    etag,
                    body,
                }),
                forget: false,
            },
            Err(_) => from_cache(cached),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::storm_activity::is_final;
    use wasm_bindgen_test::wasm_bindgen_test;

    const OCT_9_2024_DAY: i64 = 20_005;

    fn site() -> SiteId {
        SiteId::new("KTBW")
    }

    fn file() -> String {
        format!(
            r#"{{"v":1,"site":"KTBW","built":1,"first_day":{OCT_9_2024_DAY},
            "vols":[216,0,null,200,190],
            "mean":[21873695,0,null,4000000,9000000],
            "fmt":[[0,"V06"],[4,"V07"]]}}"#
        )
    }

    #[wasm_bindgen_test]
    fn index_sits_at_the_deploy_root() {
        let p = |path: &str| index_path(path, &site());
        assert_eq!(
            p("/nexrad-workbench/"),
            "/nexrad-workbench/activity-index/KTBW.json"
        );
        assert_eq!(
            p("/nexrad-workbench/index.html"),
            "/nexrad-workbench/activity-index/KTBW.json"
        );
        assert_eq!(
            p("/nexrad-workbench/pr-preview/pr-217/"),
            "/nexrad-workbench/activity-index/KTBW.json"
        );
        assert_eq!(p("/"), "/activity-index/KTBW.json");
        assert_eq!(p(""), "/activity-index/KTBW.json");
        assert_eq!(
            index_path("/", &SiteId::new("../x")),
            "/activity-index/x.json"
        );
    }

    #[wasm_bindgen_test]
    fn parses_days_as_final_without_volume_sizes() {
        let index = parse_index(&site(), &file()).unwrap();
        assert!(index.month_days.is_empty());
        let days = index.days;
        let starts: Vec<i64> = days.iter().map(|d| d.day_start / DAY_SECS).collect();
        // The null day is left out.
        assert_eq!(
            starts,
            vec![
                OCT_9_2024_DAY,
                OCT_9_2024_DAY + 1,
                OCT_9_2024_DAY + 3,
                OCT_9_2024_DAY + 4
            ]
        );
        assert_eq!(days[0].day_start, 1_728_432_000);
        assert_eq!((days[0].volumes, days[0].mean_bytes), (216, 21_873_695));
        assert_eq!(days[0].format, "V06");
        // An outage day has no format.
        assert_eq!((days[1].volumes, days[1].format.as_str()), (0, ""));
        assert_eq!(days[2].format, "V06");
        assert_eq!(days[3].format, "V07");
        assert!(days.iter().all(|d| is_final(d) && !d.has_scans));
        assert!(days.iter().all(|d| d.site == site()));
    }

    #[wasm_bindgen_test]
    fn an_empty_index_has_no_days() {
        let empty = r#"{"v":1,"site":"KTBW","built":1,"first_day":0,"vols":[],"mean":[],"fmt":[]}"#;
        assert_eq!(parse_index(&site(), empty), Ok(SiteIndex::default()));
    }

    #[wasm_bindgen_test]
    fn rejects_malformed_files() {
        let bad = |text: &str| parse_index(&site(), text).unwrap_err();
        assert!(matches!(bad(""), IndexError::Malformed(_)));
        assert!(matches!(
            bad("<!DOCTYPE html><html>"),
            IndexError::Malformed(_)
        ));
        assert!(matches!(bad("{}"), IndexError::Malformed(_)));
        assert!(matches!(
            bad(r#"{"v":1,"site":"KTBW","first_day":5,"vols":[1,2],"mean":[1],"fmt":[]}"#),
            IndexError::Malformed(_)
        ));
        assert!(matches!(
            bad(r#"{"v":1,"site":"KTBW","first_day":5,"vols":[1],"mean":[null],"fmt":[]}"#),
            IndexError::Malformed(_)
        ));
        assert!(matches!(
            bad(r#"{"v":1,"site":"KTBW","first_day":5,"vols":[1],"mean":[3],"fmt":[[3,"V06"]]}"#),
            IndexError::Malformed(_)
        ));
        assert!(matches!(
            bad(r#"{"v":1,"site":"KTBW","first_day":5,"vols":[1],"mean":[3],"fmt":[[0,"<b>"]]}"#),
            IndexError::Malformed(_)
        ));
        assert!(matches!(
            bad(r#"{"v":1,"site":"KTBW","first_day":-5,"vols":[],"mean":[],"fmt":[]}"#),
            IndexError::Malformed(_)
        ));
        for scans in ["[[0,2]]", "[[1,1],[0,1]]", "[[0,1],[0,1]]"] {
            let text = format!(
                r#"{{"v":1,"site":"KTBW","first_day":5,"vols":[1],"mean":[3],"fmt":[],"scans":{scans}}}"#
            );
            assert!(matches!(bad(&text), IndexError::Malformed(_)), "{scans}");
        }
        assert_eq!(bad(r#"{"v":2,"whatever":true}"#), IndexError::Version(2));
        assert_eq!(
            parse_index(&SiteId::new("KMLB"), &file()).unwrap_err(),
            IndexError::WrongSite("KTBW".into())
        );
    }

    #[wasm_bindgen_test]
    fn reads_which_days_have_month_files() {
        // Days 0..=1 and 3..=4 marked; day 2 is null, so it can't be.
        let text = file().replace(
            r#""fmt":[[0,"V06"],[4,"V07"]]"#,
            r#""fmt":[[0,"V06"],[4,"V07"]],"scans":[[0,3],[4,1]]"#,
        );
        let index = parse_index(&site(), &text).unwrap();
        let day = |i: i64| (OCT_9_2024_DAY + i) * DAY_SECS;
        assert_eq!(index.month_days, vec![day(0), day(1), day(4)]);
        let out = resolve_index(
            &site(),
            None,
            IndexResponse::Fetched {
                body: text,
                etag: None,
            },
        );
        assert_eq!(out.month_days, index.month_days);
    }

    fn oct() -> MonthKey {
        MonthKey {
            year: 2024,
            month: 10,
        }
    }

    #[wasm_bindgen_test]
    fn months_name_their_files_and_days() {
        assert_eq!(MonthKey::of_day(1_728_432_000), Some(oct()));
        assert_eq!(oct().name(), "2024-10");
        assert_eq!(oct().days(), Some(31));
        assert_eq!(oct().first_day_start(), Some(1_727_740_800));
        let feb = MonthKey {
            year: 2024,
            month: 2,
        };
        assert_eq!(feb.days(), Some(29));
        let dec = MonthKey {
            year: 2023,
            month: 12,
        };
        assert_eq!(dec.days(), Some(31));
        assert_eq!(
            MonthKey {
                year: 2024,
                month: 13
            }
            .days(),
            None
        );
        assert_eq!(
            month_path("/nexrad-workbench/pr-preview/pr-9/", &site(), oct()),
            "/nexrad-workbench/activity-index/KTBW/2024-10.json"
        );
        assert_eq!(
            month_path("/", &SiteId::new("../x"), oct()),
            "/activity-index/x/2024-10.json"
        );
    }

    /// October 2024 with day 9 (two volumes), day 10 an outage, the rest
    /// not there yet.
    fn month_file() -> String {
        let mut days = vec!["null"; 31];
        days[8] = r#"[3,21000000,"V06",[193,407],[20000000,22000000]]"#;
        days[9] = r#"[0,0,"",[],[]]"#;
        format!(
            r#"{{"v":1,"site":"KTBW","month":"2024-10","built":1,"complete":false,"days":[{}]}}"#,
            days.join(",")
        )
    }

    #[wasm_bindgen_test]
    fn parses_a_month_file_into_final_days_with_volumes() {
        let days = parse_month(&site(), oct(), &month_file()).unwrap();
        assert_eq!(days.len(), 2);
        let d = &days[0];
        assert_eq!(d.day_start, OCT_9_2024_DAY * DAY_SECS);
        assert_eq!(
            (d.volumes, d.mean_bytes, d.format.as_str()),
            (3, 21_000_000, "V06")
        );
        assert_eq!(d.scan_secs, vec![193, 600]);
        assert_eq!(d.scan_bytes, vec![20_000_000, 22_000_000]);
        assert!(d.has_scans && is_final(d));
        let outage = &days[1];
        assert_eq!(outage.day_start, (OCT_9_2024_DAY + 1) * DAY_SECS);
        assert_eq!((outage.volumes, outage.has_scans), (0, true));
        assert!(outage.scan_secs.is_empty());
    }

    #[wasm_bindgen_test]
    fn rejects_malformed_month_files() {
        let bad = |text: &str| parse_month(&site(), oct(), text).unwrap_err();
        assert!(matches!(bad("<!DOCTYPE html>"), IndexError::Malformed(_)));
        assert_eq!(bad(r#"{"v":2}"#), IndexError::Version(2));
        let nov = MonthKey {
            year: 2024,
            month: 11,
        };
        assert!(matches!(
            parse_month(&site(), nov, &month_file()).unwrap_err(),
            IndexError::Malformed(_)
        ));
        assert_eq!(
            parse_month(&SiteId::new("KMLB"), oct(), &month_file()).unwrap_err(),
            IndexError::WrongSite("KTBW".into())
        );
        let edit = |from: &str, to: &str| bad(&month_file().replace(from, to));
        // Too few days for October.
        assert!(matches!(edit("null,", ""), IndexError::Malformed(_)));
        // Starts and sizes differ in length; more volumes than the count.
        assert!(matches!(
            edit("[193,407]", "[193]"),
            IndexError::Malformed(_)
        ));
        assert!(matches!(
            edit("[3,21000000", "[1,21000000"),
            IndexError::Malformed(_)
        ));
        // A start past the end of the day.
        assert!(matches!(
            edit("[193,407]", "[86000,407]"),
            IndexError::Malformed(_)
        ));
        assert!(matches!(
            edit(r#""V06""#, r#""<b>""#),
            IndexError::Malformed(_)
        ));
    }

    #[wasm_bindgen_test]
    fn a_month_that_is_missing_or_unreadable_is_listed_instead() {
        let fetched = |body: &str| IndexResponse::Fetched {
            body: body.into(),
            etag: None,
        };
        assert_eq!(
            resolve_month(&site(), oct(), fetched(&month_file())).map(|d| d.len()),
            Ok(2)
        );
        // 404, or `trunk serve` answering with index.html: no days, done.
        assert_eq!(
            resolve_month(&site(), oct(), IndexResponse::NotFound),
            Ok(Vec::new())
        );
        assert_eq!(
            resolve_month(&site(), oct(), fetched("<!DOCTYPE html>")),
            Ok(Vec::new())
        );
        // Offline: retried later.
        assert!(resolve_month(&site(), oct(), IndexResponse::Failed).is_err());
    }

    fn cached(body: &str) -> ActivityIndexRecord {
        ActivityIndexRecord {
            site: site(),
            etag: Some("\"abc\"".into()),
            body: body.into(),
        }
    }

    #[wasm_bindgen_test]
    fn a_new_index_replaces_the_cached_copy() {
        let out = resolve_index(
            &site(),
            Some(cached("{}")),
            IndexResponse::Fetched {
                body: file(),
                etag: Some("\"def\"".into()),
            },
        );
        assert_eq!(out.days.len(), 4);
        let saved = out.save.unwrap();
        assert_eq!(saved.etag.as_deref(), Some("\"def\""));
        assert_eq!(saved.body, file());
        assert!(!out.forget);
    }

    #[wasm_bindgen_test]
    fn not_modified_or_offline_uses_the_cached_copy() {
        for response in [IndexResponse::NotModified, IndexResponse::Failed] {
            let out = resolve_index(&site(), Some(cached(&file())), response);
            assert_eq!(out.days.len(), 4);
            assert_eq!((out.save, out.forget), (None, false));
        }
        // Offline with nothing cached: no index, nothing to do.
        assert_eq!(
            resolve_index(&site(), None, IndexResponse::Failed),
            IndexOutcome::default()
        );
        // An unreadable cached copy is dropped.
        let out = resolve_index(&site(), Some(cached("junk")), IndexResponse::NotModified);
        assert!(out.days.is_empty() && out.forget);
    }

    #[wasm_bindgen_test]
    fn a_missing_index_falls_back_to_listing_and_drops_the_cache() {
        let out = resolve_index(&site(), Some(cached(&file())), IndexResponse::NotFound);
        assert!(out.days.is_empty() && out.forget && out.save.is_none());
        assert_eq!(
            resolve_index(&site(), None, IndexResponse::NotFound),
            IndexOutcome::default()
        );
    }

    #[wasm_bindgen_test]
    fn a_body_that_is_not_an_index_is_never_cached() {
        // `trunk serve` answers unknown paths with index.html.
        let html = IndexResponse::Fetched {
            body: "<!DOCTYPE html>".into(),
            etag: None,
        };
        assert_eq!(
            resolve_index(&site(), None, html.clone()),
            IndexOutcome::default()
        );
        let out = resolve_index(&site(), Some(cached(&file())), html);
        assert_eq!(out.days.len(), 4);
        assert!(out.save.is_none());
    }

    #[wasm_bindgen_test]
    fn revalidates_with_the_cached_etag() {
        assert_eq!(revalidation_tag(None), None);
        let c = cached("{}");
        assert_eq!(revalidation_tag(Some(&c)), Some("\"abc\""));
    }
}
