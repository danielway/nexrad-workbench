//! Storm activity for the overview's zoomed-out bars.
//!
//! The signal is the archive's own file sizes: a volume full of echo
//! compresses worse than a clear-air one, so a day's mean volume size tracks
//! how stormy it was (rank correlation 0.88 per day against decoded
//! reflectivity coverage on KTBW). It costs one S3 listing per site-day and
//! no downloads.
//!
//! Sizes aren't comparable across sites or across the radar upgrades
//! (super-resolution in 2008, dual-pol from 2011), so heights are scaled
//! against a per-site, per-format baseline: roughly the site's quiet-day
//! size. A storm day runs about 7x that on KTBW, which sets the top of the
//! scale.
//!
//! Everything here is pure: parsing the listing, summarizing a day, the
//! [`ActivityBook`] of known days and its heights, and the fetch plan
//! ([`ActivityFetcher`]). The shell (`app::storm_activity`) runs the
//! requests and the IndexedDB reads and writes.

use crate::data::{DayActivity, SiteId};
use std::collections::{BTreeMap, HashMap, HashSet};

const DAY_SECS: i64 = 86_400;

/// A storm day's mean volume size, as a multiple of the quiet-day baseline,
/// that fills the bar. KTBW's hurricane days ran ~7x its quietest day.
pub(crate) const ACTIVITY_RANGE: f64 = 8.0;

/// Known days needed in one format before the site's own quiet-day size
/// replaces the default baseline.
pub(crate) const MIN_DAYS_FOR_SITE_BASELINE: usize = 10;

/// Percentile of a site's day means taken as its quiet-day baseline.
const BASELINE_PERCENTILE: f64 = 0.1;

/// Most days the overview will list on its own. Wider views (several years)
/// show only the days already known; zooming in fills them.
pub(crate) const MAX_AUTO_FETCH_DAYS: usize = 400;

/// Listings in flight at once.
pub(crate) const MAX_IN_FLIGHT: usize = 4;

/// How long a failed day waits before it is listed again.
pub(crate) const FAILURE_BACKOFF_MS: f64 = 60_000.0;

/// A listing fetched this long after its day ended is final.
const FINAL_AFTER_DAY_END_MS: i64 = 3 * 3600 * 1000;

/// A listing of a day still filling in is reused for this long.
const OPEN_DAY_TTL_MS: i64 = 10 * 60 * 1000;

// ---------------------------------------------------------------------------
// Listing → day summary
// ---------------------------------------------------------------------------

/// One page of an S3 `ListObjectsV2` response: each object's file name (the
/// key's last path segment) and size in bytes.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct S3Listing {
    pub files: Vec<(String, u64)>,
    pub truncated: bool,
}

/// Parse a `ListObjectsV2` XML body. Tolerant by design: anything it can't
/// read is skipped rather than failing the whole day.
pub(crate) fn parse_s3_listing(xml: &str) -> S3Listing {
    let tag = |body: &str, name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = body.find(&open)? + open.len();
        let end = body[start..].find(&close)? + start;
        Some(body[start..end].to_string())
    };
    let mut files = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Contents>") {
        let after = &rest[start + "<Contents>".len()..];
        let Some(end) = after.find("</Contents>") else {
            break;
        };
        let body = &after[..end];
        if let (Some(key), Some(size)) = (tag(body, "Key"), tag(body, "Size")) {
            if let Ok(size) = size.trim().parse::<u64>() {
                let name = key.rsplit('/').next().unwrap_or(&key).to_string();
                files.push((name, size));
            }
        }
        rest = &after[end..];
    }
    let truncated = tag(xml, "IsTruncated").is_some_and(|t| t.trim() == "true");
    S3Listing { files, truncated }
}

/// The format suffix of a volume file name: `"V06"` for
/// `KTBW20241009_000313_V06`, `"V03"` for `…_V03.gz`, empty for the
/// unsuffixed pre-2008 files.
pub(crate) fn volume_format(name: &str) -> &str {
    let stem = name.strip_suffix(".gz").unwrap_or(name);
    match stem.rsplit_once('_') {
        Some((_, suffix))
            if suffix.len() >= 2
                && suffix.starts_with('V')
                && suffix[1..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            suffix
        }
        _ => "",
    }
}

/// Whether a listed file is a radar volume (not an `_MDM` metadata file or
/// an empty placeholder).
fn is_volume(name: &str, size: u64) -> bool {
    size > 0 && !name.contains("_MDM")
}

/// Reduce one day's listing to its activity summary. The format is the most
/// common one that day (a day can straddle an upgrade).
pub(crate) fn summarize_day(
    site: &SiteId,
    day_start: i64,
    files: &[(String, u64)],
    fetched_at_ms: i64,
) -> DayActivity {
    let volumes: Vec<&(String, u64)> = files.iter().filter(|(n, s)| is_volume(n, *s)).collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (name, _) in &volumes {
        *counts.entry(volume_format(name)).or_default() += 1;
    }
    let format = counts
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(f, _)| f.to_string())
        .unwrap_or_default();
    let total: u64 = volumes.iter().map(|(_, s)| *s).sum();
    DayActivity {
        site: site.clone(),
        day_start,
        volumes: volumes.len() as u32,
        mean_bytes: total.checked_div(volumes.len() as u64).unwrap_or(0),
        format,
        fetched_at_ms,
    }
}

/// Whether a stored summary can stand for its day, or the day should be
/// listed again (it was fetched before the day finished arriving, and that
/// was a while ago).
pub(crate) fn is_fresh(day: &DayActivity, now_ms: i64) -> bool {
    let day_end_ms = (day.day_start + DAY_SECS) * 1000;
    day.fetched_at_ms >= day_end_ms + FINAL_AFTER_DAY_END_MS
        || now_ms - day.fetched_at_ms < OPEN_DAY_TTL_MS
}

// ---------------------------------------------------------------------------
// Heights
// ---------------------------------------------------------------------------

/// A typical quiet-day mean volume size for a format, used until a site has
/// enough known days for its own. From KTBW listings: ~0.5 MB before 2008,
/// ~1.5–2 MB with super-resolution, ~3.3 MB on a quiet dual-pol day.
pub(crate) fn default_baseline_bytes(format: &str) -> f64 {
    let version = format.strip_prefix('V').and_then(|v| v.parse::<u32>().ok());
    match version {
        None => 450_000.0,
        Some(v) if v < 6 => 1_500_000.0,
        Some(_) => 3_500_000.0,
    }
}

/// A site's quiet-day baseline for one format: the low percentile of its
/// known day means once there are enough, else the default. Never above the
/// default (a sample of only storm days must not flatten them) and never
/// below a third of it.
pub(crate) fn baseline_bytes(format: &str, day_means: &[u64]) -> f64 {
    let default = default_baseline_bytes(format);
    let mut means: Vec<u64> = day_means.iter().copied().filter(|m| *m > 0).collect();
    if means.len() < MIN_DAYS_FOR_SITE_BASELINE {
        return default;
    }
    means.sort_unstable();
    let idx = ((means.len() - 1) as f64 * BASELINE_PERCENTILE).round() as usize;
    (means[idx] as f64).clamp(default / 3.0, default)
}

/// How stormy a day was, `0..=1`: the log of its mean size over the
/// baseline, with [`ACTIVITY_RANGE`] times the baseline filling the bar.
pub(crate) fn activity_level(mean_bytes: u64, baseline: f64) -> f32 {
    if mean_bytes == 0 || baseline <= 0.0 {
        return 0.0;
    }
    let level = (mean_bytes as f64 / baseline).ln() / ACTIVITY_RANGE.ln();
    level.clamp(0.0, 1.0) as f32
}

/// A word for a level, for hover text.
pub(crate) fn activity_word(level: f32) -> &'static str {
    if level >= 0.66 {
        "Stormy"
    } else if level >= 0.4 {
        "Active"
    } else if level >= 0.15 {
        "Some weather"
    } else {
        "Quiet"
    }
}

/// What the book knows about a span of days.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct SpanActivity {
    /// Days in the span with a summary.
    pub known_days: u32,
    /// Of those, days the radar recorded nothing.
    pub outage_days: u32,
    /// The stormiest known day's level, if any day had data.
    pub peak: Option<f32>,
    /// That day's start (Unix seconds).
    pub peak_day: Option<i64>,
}

/// How a zoomed-out bar shows its days' activity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BarActivity {
    /// No day in the bar is known yet.
    Unknown,
    /// Every known day in the bar had no data from the radar.
    Outage,
    /// The stormiest known day's level, `0..=1`.
    Level(f32),
}

/// The shortest bar a day with data draws, as a fraction of the lane, so a
/// quiet day still reads as "data here".
pub(crate) const MIN_BAR_FRAC: f32 = 0.15;

impl SpanActivity {
    /// How the bar covering this span draws.
    pub(crate) fn bar(&self) -> BarActivity {
        match self.peak {
            Some(level) => BarActivity::Level(level),
            None if self.known_days > 0 => BarActivity::Outage,
            None => BarActivity::Unknown,
        }
    }

    /// The day a click on this bar goes to: its stormiest known day, else
    /// the day clicked.
    pub(crate) fn go_to_day(&self, clicked_day: i64) -> i64 {
        self.peak_day.unwrap_or(clicked_day)
    }
}

/// A bar's height as a fraction of the lane for a level.
pub(crate) fn bar_height_frac(level: f32) -> f32 {
    MIN_BAR_FRAC + (1.0 - MIN_BAR_FRAC) * level.clamp(0.0, 1.0)
}

/// The overview's time window, for deciding which days to list (Unix
/// seconds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ActivityWindow {
    pub start: f64,
    pub end: f64,
    /// The archive's first day; nothing before it is listed.
    pub era_start: f64,
    pub now_secs: f64,
}

impl ActivityWindow {
    /// Days from the one holding `max(start, era_start)` to the one holding
    /// `min(end, now)`, as `(first, count)`; count 0 when that's empty.
    fn days(&self) -> (i64, usize) {
        let first = day_start_of(self.start.max(self.era_start));
        // A window ending exactly at midnight doesn't reach the next day.
        let last = day_start_of(self.end.min(self.now_secs) - 0.001);
        if last < first {
            return (first, 0);
        }
        (first, ((last - first) / DAY_SECS + 1) as usize)
    }

    /// Whether the overview lists this window's days on its own (it doesn't
    /// past [`MAX_AUTO_FETCH_DAYS`]; the bars then show what's known).
    pub(crate) fn is_listed(&self) -> bool {
        self.days().1 <= MAX_AUTO_FETCH_DAYS
    }
}

/// Every known site-day summary, with per-site baselines kept current.
#[derive(Debug, Default)]
pub(crate) struct ActivityBook {
    days: HashMap<SiteId, BTreeMap<i64, DayActivity>>,
    baselines: HashMap<(SiteId, String), f64>,
}

impl ActivityBook {
    /// Add (or replace) summaries, then refresh the baselines they touch.
    pub(crate) fn insert_many(&mut self, days: impl IntoIterator<Item = DayActivity>) {
        let mut touched: HashSet<(SiteId, String)> = HashSet::new();
        for day in days {
            touched.insert((day.site.clone(), day.format.clone()));
            self.days
                .entry(day.site.clone())
                .or_default()
                .insert(day.day_start, day);
        }
        for (site, format) in touched {
            let means: Vec<u64> = self.days[&site]
                .values()
                .filter(|d| d.format == format && d.volumes > 0)
                .map(|d| d.mean_bytes)
                .collect();
            let baseline = baseline_bytes(&format, &means);
            self.baselines.insert((site, format), baseline);
        }
    }

    /// The summary for the day starting at `day_start`, if known.
    pub(crate) fn day(&self, site: &SiteId, day_start: i64) -> Option<&DayActivity> {
        self.days.get(site)?.get(&day_start)
    }

    /// A known day's level (`None` for an outage day).
    pub(crate) fn level_of(&self, day: &DayActivity) -> Option<f32> {
        if day.volumes == 0 {
            return None;
        }
        let baseline = self
            .baselines
            .get(&(day.site.clone(), day.format.clone()))
            .copied()
            .unwrap_or_else(|| default_baseline_bytes(&day.format));
        Some(activity_level(day.mean_bytes, baseline))
    }

    /// What's known about the days starting in `[start, end)` (Unix
    /// seconds).
    pub(crate) fn span(&self, site: &SiteId, start: f64, end: f64) -> SpanActivity {
        let mut out = SpanActivity::default();
        let Some(days) = self.days.get(site) else {
            return out;
        };
        let first = start.ceil() as i64;
        let last = end.ceil() as i64;
        if last <= first {
            return out;
        }
        for (day_start, day) in days.range(first..last) {
            out.known_days += 1;
            match self.level_of(day) {
                None => out.outage_days += 1,
                Some(level) => {
                    if out.peak.is_none_or(|p| level > p) {
                        out.peak = Some(level);
                        out.peak_day = Some(*day_start);
                    }
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Fetch plan
// ---------------------------------------------------------------------------

/// UTC midnight at or before `ts` (Unix seconds).
pub(crate) fn day_start_of(ts: f64) -> i64 {
    (ts.floor() as i64).div_euclid(DAY_SECS) * DAY_SECS
}

/// In-flight and failed listings, and which sites' stored days have been
/// read back.
#[derive(Debug, Default)]
pub(crate) struct ActivityFetcher {
    pending: HashSet<(SiteId, i64)>,
    backoff_until_ms: HashMap<(SiteId, i64), f64>,
    loaded_sites: HashSet<SiteId>,
    loading_sites: HashSet<SiteId>,
}

impl ActivityFetcher {
    /// Whether `site`'s stored days still need reading (and mark them as
    /// being read).
    pub(crate) fn begin_load(&mut self, site: &SiteId) -> bool {
        if self.loaded_sites.contains(site) || self.loading_sites.contains(site) {
            return false;
        }
        self.loading_sites.insert(site.clone());
        true
    }

    /// `site`'s stored days have been read (or the read failed; either way
    /// fetching may start).
    pub(crate) fn finish_load(&mut self, site: &SiteId) {
        self.loading_sites.remove(site);
        self.loaded_sites.insert(site.clone());
    }

    /// Whether `site`'s stored days are in the book.
    pub(crate) fn is_loaded(&self, site: &SiteId) -> bool {
        self.loaded_sites.contains(site)
    }

    /// Record that a listing for this day was sent.
    pub(crate) fn start(&mut self, site: &SiteId, day_start: i64) {
        self.pending.insert((site.clone(), day_start));
    }

    /// Record a listing's outcome.
    pub(crate) fn finish(&mut self, site: &SiteId, day_start: i64, ok: bool, now_ms: f64) {
        let key = (site.clone(), day_start);
        self.pending.remove(&key);
        if ok {
            self.backoff_until_ms.remove(&key);
        } else {
            self.backoff_until_ms
                .insert(key, now_ms + FAILURE_BACKOFF_MS);
        }
    }

    /// The days to list next for `site` across `[start, end)`, nearest the
    /// middle first: days that aren't known (or are stale), aren't in flight
    /// and aren't backing off, within the in-flight limit. Nothing until the
    /// site's stored days are read, and nothing when the window spans more
    /// than [`MAX_AUTO_FETCH_DAYS`] (the bars show what's known).
    pub(crate) fn plan(
        &self,
        book: &ActivityBook,
        site: &SiteId,
        window: &ActivityWindow,
        now_ms: f64,
    ) -> Vec<i64> {
        if !self.is_loaded(site) {
            return Vec::new();
        }
        let free = MAX_IN_FLIGHT.saturating_sub(self.pending.len());
        if free == 0 {
            return Vec::new();
        }
        let (first, count) = window.days();
        if count == 0 || count > MAX_AUTO_FETCH_DAYS {
            return Vec::new();
        }
        let middle = (window.start + window.end) / 2.0;
        let mut wanted: Vec<i64> = (0..count as i64)
            .map(|i| first + i * DAY_SECS)
            .filter(|day| {
                let key = (site.clone(), *day);
                !self.pending.contains(&key)
                    && self
                        .backoff_until_ms
                        .get(&key)
                        .is_none_or(|until| now_ms >= *until)
                    && book
                        .day(site, *day)
                        .is_none_or(|known| !is_fresh(known, now_ms as i64))
            })
            .collect();
        wanted.sort_by(|a, b| {
            let da = ((*a + DAY_SECS / 2) as f64 - middle).abs();
            let db = ((*b + DAY_SECS / 2) as f64 - middle).abs();
            da.total_cmp(&db)
        });
        wanted.truncate(free);
        wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const OCT_9_2024: i64 = 1_728_432_000;

    fn site() -> SiteId {
        SiteId::new("KTBW")
    }

    fn day(day_start: i64, mean_bytes: u64, format: &str) -> DayActivity {
        DayActivity {
            site: site(),
            day_start,
            volumes: if mean_bytes == 0 { 0 } else { 200 },
            mean_bytes,
            format: format.into(),
            fetched_at_ms: (day_start + 2 * DAY_SECS) * 1000,
        }
    }

    fn window(start: f64, end: f64, era_start: f64, now_secs: f64) -> ActivityWindow {
        ActivityWindow {
            start,
            end,
            era_start,
            now_secs,
        }
    }

    #[wasm_bindgen_test]
    fn parses_keys_sizes_and_truncation() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult><Name>unidata-nexrad-level2</Name><IsTruncated>false</IsTruncated>
<Contents><Key>2024/10/09/KTBW/KTBW20241009_000313_V06</Key><LastModified>2024-10-09T00:10:00.000Z</LastModified><Size>15194370</Size></Contents>
<Contents><Key>2024/10/09/KTBW/KTBW20241009_000830_V06_MDM</Key><Size>4096</Size></Contents>
<Contents><Key>2024/10/09/KTBW/bad</Key><Size>x</Size></Contents>
</ListBucketResult>"#;
        let listing = parse_s3_listing(xml);
        assert!(!listing.truncated);
        assert_eq!(
            listing.files,
            vec![
                ("KTBW20241009_000313_V06".to_string(), 15_194_370),
                ("KTBW20241009_000830_V06_MDM".to_string(), 4096),
            ]
        );
        assert!(parse_s3_listing("<IsTruncated>true</IsTruncated>").truncated);
        assert_eq!(parse_s3_listing("").files, Vec::new());
    }

    #[wasm_bindgen_test]
    fn reads_the_format_suffix() {
        assert_eq!(volume_format("KTBW20241009_000313_V06"), "V06");
        assert_eq!(volume_format("KTBW20100615_000236_V03.gz"), "V03");
        assert_eq!(volume_format("KTBW19950615_000156.gz"), "");
        assert_eq!(volume_format("KTBW20241009_000830_V06_MDM"), "");
    }

    #[wasm_bindgen_test]
    fn summarizes_volumes_and_skips_metadata_files() {
        let files = vec![
            ("KTBW20241009_000313_V06".to_string(), 20_000_000),
            ("KTBW20241009_000830_V06".to_string(), 24_000_000),
            ("KTBW20241009_000830_V06_MDM".to_string(), 4096),
            ("KTBW20241009_001000_V06".to_string(), 0),
        ];
        let d = summarize_day(&site(), OCT_9_2024, &files, 5);
        assert_eq!(d.volumes, 2);
        assert_eq!(d.mean_bytes, 22_000_000);
        assert_eq!(d.format, "V06");
        assert_eq!(d.fetched_at_ms, 5);

        let outage = summarize_day(&site(), OCT_9_2024, &[], 5);
        assert_eq!((outage.volumes, outage.mean_bytes), (0, 0));
        assert_eq!(outage.format, "");
    }

    #[wasm_bindgen_test]
    fn a_day_listed_after_it_ended_is_final_and_an_open_day_expires() {
        let end_ms = (OCT_9_2024 + DAY_SECS) * 1000;
        let mut d = day(OCT_9_2024, 5_000_000, "V06");
        d.fetched_at_ms = end_ms + 4 * 3600 * 1000;
        assert!(is_fresh(&d, end_ms + 400 * 86_400_000));

        d.fetched_at_ms = end_ms - 3600 * 1000; // listed while the day ran
        assert!(is_fresh(&d, d.fetched_at_ms + 60_000));
        assert!(!is_fresh(&d, d.fetched_at_ms + 11 * 60_000));
    }

    #[wasm_bindgen_test]
    fn levels_match_the_ktbw_study() {
        let b = default_baseline_bytes("V06");
        // Milton / Ian (~22 MB) near the top, a quiet December day (~3.3 MB)
        // at the bottom, a showery day (~8 MB) in between.
        assert!(activity_level(21_900_000, b) > 0.85);
        assert!(activity_level(3_300_000, b) < 0.05);
        let mid = activity_level(8_400_000, b);
        assert!(mid > 0.3 && mid < 0.6, "{mid}");
        assert_eq!(activity_level(0, b), 0.0);
        assert_eq!(activity_level(200_000_000, b), 1.0);
    }

    #[wasm_bindgen_test]
    fn baselines_scale_by_format_and_site() {
        assert!(default_baseline_bytes("") < default_baseline_bytes("V03"));
        assert!(default_baseline_bytes("V03") < default_baseline_bytes("V06"));
        assert_eq!(default_baseline_bytes("V07"), default_baseline_bytes("V06"));

        // Too few days: the default.
        assert_eq!(baseline_bytes("V06", &[1_000_000; 5]), 3_500_000.0);
        // A drier site: its own quiet size, floored at a third of default.
        let dry: Vec<u64> = (0..30).map(|i| 1_500_000 + i * 100_000).collect();
        assert!((baseline_bytes("V06", &dry) - 1_800_000.0).abs() < 1.0);
        assert_eq!(baseline_bytes("V06", &[100_000; 30]), 3_500_000.0 / 3.0);
        // Only storm days known: never above the default.
        assert_eq!(baseline_bytes("V06", &[20_000_000; 30]), 3_500_000.0);
    }

    #[wasm_bindgen_test]
    fn span_reports_the_stormiest_day_and_outages() {
        let mut book = ActivityBook::default();
        book.insert_many([
            day(OCT_9_2024, 21_900_000, "V06"),
            day(OCT_9_2024 + DAY_SECS, 0, ""),
            day(OCT_9_2024 + 2 * DAY_SECS, 8_000_000, "V06"),
        ]);
        let week = book.span(
            &site(),
            OCT_9_2024 as f64,
            (OCT_9_2024 + 7 * DAY_SECS) as f64,
        );
        assert_eq!(week.known_days, 3);
        assert_eq!(week.outage_days, 1);
        assert_eq!(week.peak_day, Some(OCT_9_2024));
        assert!(week.peak.unwrap() > 0.85);

        let later = book.span(
            &site(),
            (OCT_9_2024 + DAY_SECS) as f64,
            (OCT_9_2024 + 2 * DAY_SECS) as f64,
        );
        assert_eq!(
            (later.known_days, later.outage_days, later.peak),
            (1, 1, None)
        );
        assert_eq!(
            book.span(&SiteId::new("KMTX"), 0.0, 4e9),
            SpanActivity::default()
        );
    }

    #[wasm_bindgen_test]
    fn plan_waits_for_stored_days_then_lists_the_middle_first() {
        let mut fetcher = ActivityFetcher::default();
        let book = ActivityBook::default();
        let start = OCT_9_2024 as f64;
        let end = (OCT_9_2024 + 9 * DAY_SECS) as f64;
        let now = 2e9;
        assert!(fetcher
            .plan(&book, &site(), &window(start, end, 0.0, now), now * 1000.0)
            .is_empty());

        assert!(fetcher.begin_load(&site()));
        assert!(!fetcher.begin_load(&site()));
        fetcher.finish_load(&site());
        let plan = fetcher.plan(&book, &site(), &window(start, end, 0.0, now), now * 1000.0);
        assert_eq!(plan.len(), MAX_IN_FLIGHT);
        assert_eq!(plan[0], OCT_9_2024 + 4 * DAY_SECS);
        assert!(plan.iter().all(|d| (d - OCT_9_2024).abs() <= 6 * DAY_SECS));
    }

    #[wasm_bindgen_test]
    fn plan_skips_known_pending_and_failed_days() {
        let mut fetcher = ActivityFetcher::default();
        fetcher.begin_load(&site());
        fetcher.finish_load(&site());
        let mut book = ActivityBook::default();
        book.insert_many([day(OCT_9_2024, 5_000_000, "V06")]);
        let now_ms = 2e12;
        fetcher.start(&site(), OCT_9_2024 + DAY_SECS);
        fetcher.start(&site(), OCT_9_2024 + 2 * DAY_SECS);
        fetcher.finish(&site(), OCT_9_2024 + 2 * DAY_SECS, false, now_ms);
        let end = (OCT_9_2024 + 4 * DAY_SECS) as f64;
        let plan = fetcher.plan(
            &book,
            &site(),
            &window(OCT_9_2024 as f64, end, 0.0, 2e9),
            now_ms,
        );
        assert_eq!(plan, vec![OCT_9_2024 + 3 * DAY_SECS]);

        // The failed day comes back after its backoff.
        let later = now_ms + FAILURE_BACKOFF_MS;
        let plan = fetcher.plan(
            &book,
            &site(),
            &window(OCT_9_2024 as f64, end, 0.0, 2e9),
            later,
        );
        assert!(plan.contains(&(OCT_9_2024 + 2 * DAY_SECS)));
    }

    #[wasm_bindgen_test]
    fn plan_stays_inside_the_archive_and_before_now_and_skips_huge_windows() {
        let mut fetcher = ActivityFetcher::default();
        fetcher.begin_load(&site());
        fetcher.finish_load(&site());
        let book = ActivityBook::default();
        let now = (OCT_9_2024 + DAY_SECS / 2) as f64;
        let w = window(
            (OCT_9_2024 - DAY_SECS) as f64,
            now + 5.0 * DAY_SECS as f64,
            OCT_9_2024 as f64,
            now,
        );
        let plan = fetcher.plan(&book, &site(), &w, now * 1000.0);
        assert_eq!(plan, vec![OCT_9_2024]);

        let years = fetcher.plan(&book, &site(), &window(0.0, 4e8, 0.0, 2e9), 2e12);
        assert!(years.is_empty());
    }

    #[wasm_bindgen_test]
    fn bars_show_levels_outages_and_unknowns() {
        let unknown = SpanActivity::default();
        assert_eq!(unknown.bar(), BarActivity::Unknown);
        assert_eq!(unknown.go_to_day(7), 7);
        let outage = SpanActivity {
            known_days: 2,
            outage_days: 2,
            ..Default::default()
        };
        assert_eq!(outage.bar(), BarActivity::Outage);
        let stormy = SpanActivity {
            known_days: 3,
            outage_days: 0,
            peak: Some(0.9),
            peak_day: Some(OCT_9_2024),
        };
        assert_eq!(stormy.bar(), BarActivity::Level(0.9));
        assert_eq!(stormy.go_to_day(OCT_9_2024 + DAY_SECS), OCT_9_2024);

        assert_eq!(bar_height_frac(0.0), MIN_BAR_FRAC);
        assert_eq!(bar_height_frac(1.0), 1.0);
        assert_eq!(bar_height_frac(7.0), 1.0);
    }

    #[wasm_bindgen_test]
    fn windows_past_the_auto_fetch_limit_are_not_listed() {
        let day = DAY_SECS as f64;
        let start = OCT_9_2024 as f64;
        assert!(window(start, start + 400.0 * day, 0.0, 2e9).is_listed());
        assert!(!window(start, start + 401.0 * day, 0.0, 2e9).is_listed());
        // Only days the archive covers count.
        assert!(window(start - 5000.0 * day, start + day, start, 2e9).is_listed());
    }

    #[wasm_bindgen_test]
    fn day_start_is_utc_midnight() {
        assert_eq!(day_start_of(OCT_9_2024 as f64 + 3600.5), OCT_9_2024);
        assert_eq!(day_start_of(OCT_9_2024 as f64), OCT_9_2024);
        assert_eq!(day_start_of(-1.0), -DAY_SECS);
    }
}
