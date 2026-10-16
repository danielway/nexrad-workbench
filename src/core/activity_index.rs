//! The published storm-activity index: one file per site, built nightly
//! from the archive listings (`tools/activity_index/`), holding each UTC
//! day's volume count, mean volume size and format.
//!
//! With it the overview's day bars need no listing at all: the shell reads a
//! site's file once per session (cached in IndexedDB and revalidated by
//! ETag), and [`crate::core::storm_activity::ActivityFetcher::plan`] only
//! lists the days the index doesn't cover (the newest, and gaps while the
//! backfill runs) or, zoomed in to volumes, the days it has no per-volume
//! sizes for. Without an index (a dev build, or before the first nightly
//! run) everything is listed live as before.
//!
//! Everything here is pure: the file's path, parsing it into
//! [`DayActivity`] values, and deciding what a fetch's response means for
//! the cached copy ([`resolve_index`]).

use crate::core::storm_activity::{final_fetched_at_ms, volume_format};
use crate::data::{ActivityIndexRecord, DayActivity, SiteId};
use serde::Deserialize;

/// The file format this build reads (`"v"` in the file).
pub(crate) const INDEX_FORMAT_VERSION: u32 = 1;

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
    let root = match page_path.find("/pr-preview/") {
        Some(i) => &page_path[..=i],
        None => match page_path.rfind('/') {
            Some(i) => &page_path[..=i],
            None => "/",
        },
    };
    let root = if root.starts_with('/') { root } else { "/" };
    let id: String = site
        .0
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("{root}{INDEX_DIR}/{id}.json")
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
}

/// Whether `format` is one a listing could have produced (`""`, `"V06"`).
fn is_format(format: &str) -> bool {
    format.is_empty() || (format.len() <= 8 && volume_format(&format!("x_{format}")) == format)
}

/// The days in a site's index file, each marked final (an index only holds
/// days that had ended three hours before it was built) and without
/// per-volume sizes. Days the file has as `null` (not indexed yet) are left
/// out, so they stay unknown and get listed.
pub(crate) fn parse_index(site: &SiteId, text: &str) -> Result<Vec<DayActivity>, IndexError> {
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
    Ok(out)
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
            Ok(days) => IndexOutcome {
                days,
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
            Ok(days) => IndexOutcome {
                days,
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
        let days = parse_index(&site(), &file()).unwrap();
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
        assert_eq!(parse_index(&site(), empty), Ok(Vec::new()));
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
        assert_eq!(bad(r#"{"v":2,"whatever":true}"#), IndexError::Version(2));
        assert_eq!(
            parse_index(&SiteId::new("KMLB"), &file()).unwrap_err(),
            IndexError::WrongSite("KTBW".into())
        );
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
