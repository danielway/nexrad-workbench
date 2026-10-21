//! Storm activity for the overview: bar heights zoomed out, and each
//! volume's height zoomed in to scans.
//!
//! The signal is the archive's own file sizes: a volume full of echo
//! compresses worse than a clear-air one, so a day's mean volume size tracks
//! how stormy it was (rank correlation 0.88 per day against decoded
//! reflectivity coverage on KTBW). It costs one S3 listing per site-day and
//! no downloads. The same listing carries every volume's own size, kept so
//! that zooming into a tall day still shows which hours held the echo.
//!
//! Sizes aren't comparable across sites or across the radar upgrades
//! (super-resolution in 2008, dual-pol from 2011), so heights are scaled
//! against a per-site, per-format baseline: roughly the site's quiet-day
//! size. A storm day runs about 7x that on KTBW, which sets the top of the
//! scale.
//!
//! Heights are stretched to the view ([`ViewScale`]): the view's typical
//! day (or volume) sits near the floor and its biggest near the top, so the
//! days that stand out in what's on screen are the tall ones.
//!
//! Most days come from the published index (`core::activity_index`): one
//! file per site, built nightly, with every finished day's count and mean
//! size but no per-volume sizes, and for recent months one file per month
//! with every volume's size. The fetch plan only lists the days it doesn't
//! cover, and, zoomed in to volumes ([`needs_scans`]), fetches the month
//! files for the days they hold and lists only the days they don't.
//!
//! Everything here is pure: parsing the listing, summarizing a day, the
//! [`ActivityBook`] of known days and its heights, and the fetch plan
//! ([`ActivityFetcher`]). The shell (`app::storm_activity`) runs the
//! requests and the IndexedDB reads and writes.

use crate::core::activity_index::MonthKey;
use crate::core::overview::{detail_for, OverviewDetail};
use crate::data::{DayActivity, SiteId};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
    size > 0 && crate::core::acquisition::is_volume_file_name(name)
}

/// When a volume started, in seconds after its UTC midnight, from its file
/// name (`KTBW20241009_000313_V06` → 193).
pub(crate) fn volume_start_secs(name: &str) -> Option<u32> {
    let time = name.get(13..19).filter(|_| name.get(12..13) == Some("_"))?;
    if !time.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let field = |i: usize| time[i..i + 2].parse::<u32>().ok();
    let (h, m, s) = (field(0)?, field(2)?, field(4)?);
    (h < 24 && m < 60 && s < 60).then_some(h * 3600 + m * 60 + s)
}

/// Reduce one day's listing to its activity summary. The format is the most
/// common one that day (a day can straddle an upgrade); the per-volume sizes
/// keep only that format's volumes, so they stay comparable.
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
    let mut scans: Vec<(u32, u32)> = volumes
        .iter()
        .filter(|(name, _)| volume_format(name) == format)
        .filter_map(|(name, size)| {
            Some((
                volume_start_secs(name)?,
                (*size).min(u32::MAX as u64) as u32,
            ))
        })
        .collect();
    scans.sort_unstable();
    DayActivity {
        site: site.clone(),
        day_start,
        volumes: volumes.len() as u32,
        mean_bytes: total.checked_div(volumes.len() as u64).unwrap_or(0),
        format,
        fetched_at_ms,
        has_scans: true,
        scan_secs: scans.iter().map(|(t, _)| *t).collect(),
        scan_bytes: scans.iter().map(|(_, b)| *b).collect(),
    }
}

/// The earliest `fetched_at_ms` at which a listing of the day starting at
/// `day_start` is final. Index days carry it, since the index only holds
/// days that had ended that long before it was built.
pub(crate) fn final_fetched_at_ms(day_start: i64) -> i64 {
    (day_start + DAY_SECS) * 1000 + FINAL_AFTER_DAY_END_MS
}

/// Whether a summary was taken after its day had finished arriving.
pub(crate) fn is_final(day: &DayActivity) -> bool {
    day.fetched_at_ms >= final_fetched_at_ms(day.day_start)
}

/// Whether a stored summary can stand for its day, or the day should be
/// listed again: it was fetched before the day finished arriving and that
/// was a while ago, or the view draws volumes (`need_scans`) and it has no
/// per-volume sizes (an index day, or a record stored before they were
/// kept).
pub(crate) fn is_fresh(day: &DayActivity, now_ms: i64, need_scans: bool) -> bool {
    (day.has_scans || !need_scans)
        && (is_final(day) || now_ms - day.fetched_at_ms < OPEN_DAY_TTL_MS)
}

/// Whether the overview, `lane_px` wide across `span_secs`, draws each
/// volume ([`OverviewDetail::Scans`]) and so needs per-volume sizes, which
/// only a live listing has. Wider views draw day bars, which the index
/// alone fills.
pub(crate) fn needs_scans(lane_px: f64, span_secs: f64) -> bool {
    span_secs > 0.0 && lane_px > 0.0 && detail_for(lane_px / span_secs) == OverviewDetail::Scans
}

/// Whether an index day should take the place of what the book holds for
/// that day. Index days are final, so they replace anything but a final
/// live listing with per-volume sizes, which knows more.
pub(crate) fn index_replaces(existing: Option<&DayActivity>) -> bool {
    existing.is_none_or(|known| !(known.has_scans && is_final(known)))
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

/// A word for a level, for hover text. It names an amount of echo, not a
/// kind of weather: file size tracks how much of the scan holds echo, and
/// in summer clear-air echo (insects, birds) fills a scan on a dry day
/// about as much as light rain does.
pub(crate) fn activity_word(level: f32) -> &'static str {
    if level >= 0.66 {
        "Lots of echo"
    } else if level >= 0.4 {
        "Some echo"
    } else if level >= 0.15 {
        "Little echo"
    } else {
        "Clear"
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
pub(crate) const MIN_BAR_FRAC: f32 = 0.1;

/// The least spread of levels a view stretches across its full height, so
/// a view of uniformly quiet days stays short instead of blowing small
/// differences up into peaks.
const MIN_VIEW_SPREAD: f32 = 0.3;

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

/// How the levels in view map to heights. The view's median level sits at
/// the floor and its highest at the top (or the median plus
/// [`MIN_VIEW_SPREAD`], whichever is higher), so a run of ordinary days
/// reads as low and the ones that stand out in this view are tall. Words in
/// hover text still come from the absolute level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ViewScale {
    floor: f32,
    top: f32,
}

impl Default for ViewScale {
    /// Absolute levels, for a view with nothing known.
    fn default() -> Self {
        Self {
            floor: 0.0,
            top: 1.0,
        }
    }
}

impl ViewScale {
    /// The scale for the levels in view.
    pub(crate) fn from_levels(levels: impl IntoIterator<Item = f32>) -> Self {
        let mut levels: Vec<f32> = levels.into_iter().filter(|l| l.is_finite()).collect();
        if levels.is_empty() {
            return Self::default();
        }
        levels.sort_unstable_by(f32::total_cmp);
        let floor = levels[(levels.len() - 1) / 2];
        let top = levels[levels.len() - 1].max(floor + MIN_VIEW_SPREAD);
        Self { floor, top }
    }

    /// A height as a fraction of the lane for a level.
    pub(crate) fn height_frac(&self, level: f32) -> f32 {
        let t = ((level - self.floor) / (self.top - self.floor)).clamp(0.0, 1.0);
        MIN_BAR_FRAC + (1.0 - MIN_BAR_FRAC) * t
    }
}

/// The longest a volume is drawn when the next one is missing or far off.
const MAX_SCAN_SECS: f64 = 15.0 * 60.0;

/// One volume's size, for the zoomed-in overview.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScanActivity {
    /// When the volume started (Unix seconds).
    pub start: f64,
    /// When the next one started, at most [`MAX_SCAN_SECS`] later.
    pub end: f64,
    pub bytes: u32,
    pub level: f32,
}

/// The volumes of the known days overlapping a window.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct ScanSizes {
    /// Every volume of those days, in time order.
    pub scans: Vec<ScanActivity>,
    /// The known days' starts (Unix seconds), outages included; coverage
    /// inside them is drawn from `scans`.
    pub known_days: Vec<i64>,
}

impl ScanSizes {
    /// Whether the day holding `ts` is known.
    pub(crate) fn knows(&self, ts: f64) -> bool {
        self.known_days.binary_search(&day_start_of(ts)).is_ok()
    }

    /// The volume drawn at `ts`, if any.
    pub(crate) fn at(&self, ts: f64) -> Option<&ScanActivity> {
        let i = self.scans.partition_point(|s| s.start <= ts);
        self.scans[..i].last().filter(|s| ts < s.end)
    }

    /// The volumes overlapping `[start, end]`, each with its height as a
    /// fraction of the lane on the scale of the volumes in view
    /// ([`ViewScale`]): one bar per volume, as the zoomed-in overview and
    /// the phone strip's Day stop draw them.
    pub(crate) fn bars(&self, start: f64, end: f64) -> Vec<(ScanActivity, f32)> {
        let lo = self.scans.partition_point(|s| s.end < start);
        let hi = self.scans.partition_point(|s| s.start <= end);
        let visible = &self.scans[lo..hi.max(lo)];
        let scale = ViewScale::from_levels(visible.iter().map(|s| s.level));
        visible
            .iter()
            .map(|s| (*s, scale.height_frac(s.level)))
            .collect()
    }

    /// The parts of `spans` (Unix seconds) on days whose sizes aren't
    /// known, split at UTC midnights: where a lane shows plain coverage
    /// because there are no bars to show yet.
    pub(crate) fn unknown_pieces(
        &self,
        spans: impl IntoIterator<Item = (f64, f64)>,
    ) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for (start, end) in spans {
            let mut t = start;
            while t < end {
                let piece_end = end.min((day_start_of(t) + DAY_SECS) as f64);
                if !self.knows(t) {
                    out.push((t, piece_end));
                }
                t = piece_end;
            }
        }
        out
    }
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
    /// Whether the view draws volumes ([`needs_scans`]), so days without
    /// per-volume sizes are listed too.
    pub need_scans: bool,
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
}

/// Every known site-day summary, with per-site baselines kept current.
#[derive(Debug, Default)]
pub(crate) struct ActivityBook {
    days: HashMap<SiteId, BTreeMap<i64, DayActivity>>,
    baselines: HashMap<(SiteId, String), f64>,
    /// Each site's first and last day in its published index, as read.
    index_range: HashMap<SiteId, (i64, i64)>,
    /// Each site's index days whose volumes a month file holds.
    month_days: HashMap<SiteId, BTreeSet<i64>>,
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

    /// Add a site's index days ([`crate::core::activity_index::parse_index`]),
    /// keeping any final live listing that has per-volume sizes
    /// ([`index_replaces`]). Returns how many were taken.
    pub(crate) fn insert_index(&mut self, days: impl IntoIterator<Item = DayActivity>) -> usize {
        let days: Vec<DayActivity> = days.into_iter().collect();
        for d in &days {
            let range = self
                .index_range
                .entry(d.site.clone())
                .or_insert((d.day_start, d.day_start));
            range.0 = range.0.min(d.day_start);
            range.1 = range.1.max(d.day_start);
        }
        let fresh: Vec<DayActivity> = days
            .into_iter()
            .filter(|d| index_replaces(self.day(&d.site, d.day_start)))
            .collect();
        let taken = fresh.len();
        self.insert_many(fresh);
        taken
    }

    /// Record which of `site`'s index days have their volumes in a month
    /// file ([`crate::core::activity_index::SiteIndex::month_days`]).
    pub(crate) fn set_month_days(&mut self, site: &SiteId, days: impl IntoIterator<Item = i64>) {
        let days: BTreeSet<i64> = days.into_iter().collect();
        if days.is_empty() {
            self.month_days.remove(site);
        } else {
            self.month_days.insert(site.clone(), days);
        }
    }

    /// Whether a month file holds the volumes of `site`'s day starting at
    /// `day_start`.
    pub(crate) fn in_month_file(&self, site: &SiteId, day_start: i64) -> bool {
        self.month_days
            .get(site)
            .is_some_and(|days| days.contains(&day_start))
    }

    /// Add a month file's days
    /// ([`crate::core::activity_index::parse_month`]), keeping any final
    /// live listing (which knows the same). Returns the days taken, for the
    /// shell to store like listed days.
    pub(crate) fn insert_month(
        &mut self,
        days: impl IntoIterator<Item = DayActivity>,
    ) -> Vec<DayActivity> {
        let fresh: Vec<DayActivity> = days
            .into_iter()
            .filter(|d| index_replaces(self.day(&d.site, d.day_start)))
            .collect();
        self.insert_many(fresh.clone());
        fresh
    }

    /// The first and last day (UTC midnights) of `site`'s published index,
    /// once it has been read; `None` without one.
    pub(crate) fn index_range(&self, site: &SiteId) -> Option<(i64, i64)> {
        self.index_range.get(site).copied()
    }

    /// Every known day starting in `[first, last]` (UTC midnights) with its
    /// level, `None` for an outage day, in time order.
    pub(crate) fn levels(&self, site: &SiteId, first: i64, last: i64) -> Vec<(i64, Option<f32>)> {
        let Some(days) = self.days.get(site) else {
            return Vec::new();
        };
        if last < first {
            return Vec::new();
        }
        days.range(first..=last)
            .map(|(start, day)| (*start, self.level_of(day)))
            .collect()
    }

    /// The volumes starting in `[start, end)` (Unix seconds), once every UTC
    /// day the span touches has been listed with per-volume sizes (or is a
    /// known outage); `None` while any of them is unknown or known only
    /// from the index. This is the listing the phone strip's Day stop makes
    /// for a day in view.
    pub(crate) fn listing(&self, site: &SiteId, start: f64, end: f64) -> Option<Vec<ScanActivity>> {
        if end <= start {
            return Some(Vec::new());
        }
        let days = self.days.get(site)?;
        let mut day = day_start_of(start);
        let last = day_start_of(end - 0.001);
        while day <= last {
            let known = days.get(&day)?;
            if !(known.has_scans || known.volumes == 0) {
                return None;
            }
            day += DAY_SECS;
        }
        Some(
            self.scans(site, start, end)
                .scans
                .into_iter()
                .filter(|s| s.start >= start && s.start < end)
                .collect(),
        )
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
        Some(activity_level(day.mean_bytes, self.baseline(day)))
    }

    /// The quiet-day baseline for a day's site and format.
    fn baseline(&self, day: &DayActivity) -> f64 {
        self.baselines
            .get(&(day.site.clone(), day.format.clone()))
            .copied()
            .unwrap_or_else(|| default_baseline_bytes(&day.format))
    }

    /// Every volume of the known days overlapping `[start, end)` (Unix
    /// seconds), with levels against the site's day baseline.
    pub(crate) fn scans(&self, site: &SiteId, start: f64, end: f64) -> ScanSizes {
        let mut out = ScanSizes::default();
        let Some(days) = self.days.get(site) else {
            return out;
        };
        let first = day_start_of(start);
        let last = end.ceil() as i64;
        if last <= first {
            return out;
        }
        for (day_start, day) in days.range(first..last) {
            out.known_days.push(*day_start);
            let baseline = self.baseline(day);
            for (secs, bytes) in day.scan_secs.iter().zip(&day.scan_bytes) {
                let start = (*day_start + *secs as i64) as f64;
                out.scans.push(ScanActivity {
                    start,
                    end: start + MAX_SCAN_SECS,
                    bytes: *bytes,
                    level: activity_level(*bytes as u64, baseline),
                });
            }
        }
        for i in 1..out.scans.len() {
            let next = out.scans[i].start;
            let prev = &mut out.scans[i - 1];
            prev.end = prev.end.min(next).max(prev.start);
        }
        out
    }

    /// Starts of the known outage days (the radar recorded nothing) in
    /// `[start, end)` (Unix seconds). Only days after the site's first day
    /// with data count: before that the radar wasn't there yet, which is
    /// not an outage.
    pub(crate) fn outage_days(&self, site: &SiteId, start: f64, end: f64) -> Vec<i64> {
        let Some(days) = self.days.get(site) else {
            return Vec::new();
        };
        let Some(first_data) = days.values().find(|d| d.volumes > 0).map(|d| d.day_start) else {
            return Vec::new();
        };
        let first = day_start_of(start).max(first_data);
        let last = end.ceil() as i64;
        if last <= first {
            return Vec::new();
        }
        days.range(first..last)
            .filter(|(_, d)| d.volumes == 0)
            .map(|(day, _)| *day)
            .collect()
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

/// Every how many days a window of `count` days lists: every day up to
/// [`MAX_AUTO_FETCH_DAYS`], then one day in `ceil(count / MAX)`, so a
/// window of years still lists about [`MAX_AUTO_FETCH_DAYS`] days.
pub(crate) fn sample_stride_days(count: usize) -> i64 {
    count.div_ceil(MAX_AUTO_FETCH_DAYS).max(1) as i64
}

/// UTC midnight at or before `ts` (Unix seconds).
pub(crate) fn day_start_of(ts: f64) -> i64 {
    (ts.floor() as i64).div_euclid(DAY_SECS) * DAY_SECS
}

/// What to fetch next ([`ActivityFetcher::plan`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct ActivityPlan {
    /// Days to list from the archive, nearest the middle first.
    pub list: Vec<i64>,
    /// Month files to fetch from the index, for the days they hold.
    pub months: Vec<MonthKey>,
}

/// In-flight and failed listings and month fetches, and which sites' stored
/// days have been read back.
#[derive(Debug, Default)]
pub(crate) struct ActivityFetcher {
    pending: HashSet<(SiteId, i64)>,
    backoff_until_ms: HashMap<(SiteId, i64), f64>,
    loaded_sites: HashSet<SiteId>,
    loading_sites: HashSet<SiteId>,
    months_pending: HashSet<(SiteId, MonthKey)>,
    month_backoff_until_ms: HashMap<(SiteId, MonthKey), f64>,
    /// Month files fetched this session: a day they were meant to hold but
    /// didn't (a file older than the site's index) is listed instead.
    months_done: HashSet<(SiteId, MonthKey)>,
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

    /// Record that a month file fetch was sent.
    pub(crate) fn start_month(&mut self, site: &SiteId, month: MonthKey) {
        self.months_pending.insert((site.clone(), month));
    }

    /// Record a month file fetch's outcome: done for the session (its days
    /// are in the book, and any it lacked get listed), or failed and backing
    /// off.
    pub(crate) fn finish_month(&mut self, site: &SiteId, month: MonthKey, ok: bool, now_ms: f64) {
        let key = (site.clone(), month);
        self.months_pending.remove(&key);
        if ok {
            self.month_backoff_until_ms.remove(&key);
            self.months_done.insert(key);
        } else {
            self.month_backoff_until_ms
                .insert(key, now_ms + FAILURE_BACKOFF_MS);
        }
    }

    /// What to fetch next for `site` across `[start, end)`, nearest the
    /// middle first, within the in-flight limit: the days that aren't known
    /// (or are stale, or lack the per-volume sizes a zoomed-in view needs),
    /// aren't in flight and aren't backing off. Zoomed in, a day a month
    /// file holds ([`ActivityBook::in_month_file`]) is fetched with its
    /// month instead of listed, once per session; only the days no month
    /// file holds are listed. Nothing until the site's stored days are
    /// read. A window wider than [`MAX_AUTO_FETCH_DAYS`] (years) keeps
    /// listing in the background on a sample of its days
    /// ([`sample_stride_days`]), so the bars fill in instead of the lane
    /// staying empty.
    pub(crate) fn plan(
        &self,
        book: &ActivityBook,
        site: &SiteId,
        window: &ActivityWindow,
        now_ms: f64,
    ) -> ActivityPlan {
        let mut plan = ActivityPlan::default();
        if !self.is_loaded(site) {
            return plan;
        }
        let free = MAX_IN_FLIGHT.saturating_sub(self.pending.len() + self.months_pending.len());
        if free == 0 {
            return plan;
        }
        let (first, count) = window.days();
        if count == 0 {
            return plan;
        }
        let stride = sample_stride_days(count);
        let middle = (window.start + window.end) / 2.0;
        let mut wanted: Vec<i64> = (0..count as i64)
            .map(|i| first + i * DAY_SECS)
            // Sampled on whole multiples of the stride since the epoch, so
            // sliding the window keeps the same days.
            .filter(|day| (day / DAY_SECS).rem_euclid(stride) == 0)
            .filter(|day| {
                let key = (site.clone(), *day);
                !self.pending.contains(&key)
                    && self
                        .backoff_until_ms
                        .get(&key)
                        .is_none_or(|until| now_ms >= *until)
                    && book
                        .day(site, *day)
                        .is_none_or(|known| !is_fresh(known, now_ms as i64, window.need_scans))
            })
            .collect();
        wanted.sort_by(|a, b| {
            let da = ((*a + DAY_SECS / 2) as f64 - middle).abs();
            let db = ((*b + DAY_SECS / 2) as f64 - middle).abs();
            da.total_cmp(&db)
        });
        for day in wanted {
            if plan.list.len() + plan.months.len() >= free {
                break;
            }
            let month = MonthKey::of_day(day)
                .filter(|_| window.need_scans && book.in_month_file(site, day));
            match month {
                Some(month) if !self.months_done.contains(&(site.clone(), month)) => {
                    let key = (site.clone(), month);
                    let waiting = self.months_pending.contains(&key)
                        || self
                            .month_backoff_until_ms
                            .get(&key)
                            .is_some_and(|until| now_ms < *until);
                    if !waiting && !plan.months.contains(&month) {
                        plan.months.push(month);
                    }
                }
                _ => plan.list.push(day),
            }
        }
        plan
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
            has_scans: true,
            scan_secs: Vec::new(),
            scan_bytes: Vec::new(),
        }
    }

    fn window(start: f64, end: f64, era_start: f64, now_secs: f64) -> ActivityWindow {
        ActivityWindow {
            start,
            end,
            era_start,
            now_secs,
            need_scans: false,
        }
    }

    /// An index day: final, without per-volume sizes.
    fn index_day(day_start: i64, mean_bytes: u64) -> DayActivity {
        DayActivity {
            fetched_at_ms: final_fetched_at_ms(day_start),
            has_scans: false,
            ..day(day_start, mean_bytes, "V06")
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
        assert!(is_final(&d));
        assert!(is_fresh(&d, end_ms + 400 * 86_400_000, true));

        d.fetched_at_ms = end_ms - 3600 * 1000; // listed while the day ran
        assert!(!is_final(&d));
        assert!(is_fresh(&d, d.fetched_at_ms + 60_000, true));
        assert!(!is_fresh(&d, d.fetched_at_ms + 11 * 60_000, true));
        assert!(!is_fresh(&d, d.fetched_at_ms + 11 * 60_000, false));
        assert!(final_fetched_at_ms(OCT_9_2024) == end_ms + 3 * 3600 * 1000);
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
    fn words_name_amounts_of_echo_on_the_kdmx_study() {
        // KDMX's quiet-day size over three years is ~2.07 MB.
        let b = 2_070_000.0;
        let word = |mb: f64| activity_word(activity_level((mb * 1e6) as u64, b));
        // Greenfield EF4 day and the 2020 derecho: rain over much of the scan.
        assert_eq!(word(15.30), "Lots of echo");
        assert_eq!(word(10.36), "Lots of echo");
        // A dry July day: a quarter of the scan holds insects, under 1% rain.
        assert_eq!(word(6.69), "Some echo");
        // A mid-March day with a few light showers.
        assert_eq!(word(3.27), "Little echo");
        // Clear January air.
        assert_eq!(word(1.61), "Clear");
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
            .list
            .is_empty());

        assert!(fetcher.begin_load(&site()));
        assert!(!fetcher.begin_load(&site()));
        fetcher.finish_load(&site());
        let plan = fetcher
            .plan(&book, &site(), &window(start, end, 0.0, now), now * 1000.0)
            .list;
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
        let plan = fetcher
            .plan(
                &book,
                &site(),
                &window(OCT_9_2024 as f64, end, 0.0, 2e9),
                now_ms,
            )
            .list;
        assert_eq!(plan, vec![OCT_9_2024 + 3 * DAY_SECS]);

        // The failed day comes back after its backoff.
        let later = now_ms + FAILURE_BACKOFF_MS;
        let plan = fetcher
            .plan(
                &book,
                &site(),
                &window(OCT_9_2024 as f64, end, 0.0, 2e9),
                later,
            )
            .list;
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
        let plan = fetcher.plan(&book, &site(), &w, now * 1000.0).list;
        assert_eq!(plan, vec![OCT_9_2024]);

        // Years across: still listing, on a stable sample of the days.
        let w = window(0.0, 4e8, 0.0, 2e9);
        let stride = sample_stride_days(w.days().1);
        assert!(stride > 1);
        let years = fetcher.plan(&book, &site(), &w, 2e12).list;
        assert_eq!(years.len(), MAX_IN_FLIGHT);
        assert!(years.iter().all(|d| (d / DAY_SECS) % stride == 0));
        // The days nearest the middle come first.
        let middle = 2e8;
        assert!(years
            .iter()
            .all(|d| (*d as f64 - middle).abs() < (stride * DAY_SECS * 3) as f64));
    }

    #[wasm_bindgen_test]
    fn outages_count_only_after_the_site_first_has_data() {
        let mut book = ActivityBook::default();
        book.insert_many([
            day(OCT_9_2024 - 2 * DAY_SECS, 0, "V06"),
            day(OCT_9_2024, 5_000_000, "V06"),
            day(OCT_9_2024 + DAY_SECS, 0, "V06"),
            day(OCT_9_2024 + 2 * DAY_SECS, 6_000_000, "V06"),
            day(OCT_9_2024 + 3 * DAY_SECS, 0, "V06"),
        ]);
        let all = book.outage_days(
            &site(),
            (OCT_9_2024 - 5 * DAY_SECS) as f64,
            (OCT_9_2024 + 10 * DAY_SECS) as f64,
        );
        assert_eq!(all, vec![OCT_9_2024 + DAY_SECS, OCT_9_2024 + 3 * DAY_SECS]);
        // A window clips them.
        let some = book.outage_days(
            &site(),
            (OCT_9_2024 + 2 * DAY_SECS) as f64,
            (OCT_9_2024 + 10 * DAY_SECS) as f64,
        );
        assert_eq!(some, vec![OCT_9_2024 + 3 * DAY_SECS]);
        assert!(book.outage_days(&SiteId::new("KXXX"), 0.0, 2e9).is_empty());
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
    }

    #[wasm_bindgen_test]
    fn the_view_scale_puts_typical_days_low_and_standouts_high() {
        // A wet-season run on KTBW: most days ~0.45, two big ones.
        let levels = [0.40, 0.42, 0.44, 0.45, 0.46, 0.48, 0.60, 0.80];
        let scale = ViewScale::from_levels(levels);
        assert_eq!(scale.height_frac(0.40), MIN_BAR_FRAC);
        assert_eq!(scale.height_frac(0.45), MIN_BAR_FRAC);
        assert_eq!(scale.height_frac(0.80), 1.0);
        let mid = scale.height_frac(0.60);
        assert!(mid > 0.4 && mid < 0.7, "{mid}");

        // A uniformly quiet view stays low.
        let quiet = ViewScale::from_levels([0.0, 0.02, 0.05, 0.08]);
        assert!(quiet.height_frac(0.08) < 0.35);

        // Nothing known: absolute levels.
        let none = ViewScale::from_levels([]);
        assert_eq!(none.height_frac(0.0), MIN_BAR_FRAC);
        assert_eq!(none.height_frac(1.0), 1.0);
        assert_eq!(none.height_frac(7.0), 1.0);
    }

    #[wasm_bindgen_test]
    fn reads_volume_start_times() {
        assert_eq!(volume_start_secs("KTBW20241009_000313_V06"), Some(193));
        assert_eq!(volume_start_secs("KTBW19950615_235956.gz"), Some(86_396));
        assert_eq!(volume_start_secs("KTBW20241009_250000_V06"), None);
        assert_eq!(volume_start_secs("bad"), None);
    }

    #[wasm_bindgen_test]
    fn keeps_each_volumes_size_in_time_order() {
        let files = vec![
            ("KTBW20241009_000830_V06".to_string(), 24_000_000),
            ("KTBW20241009_000313_V06".to_string(), 20_000_000),
            ("KTBW20241009_000830_V06_MDM".to_string(), 4096),
            ("KTBW20241009_001200_V03".to_string(), 9_000_000),
        ];
        let d = summarize_day(&site(), OCT_9_2024, &files, 5);
        assert!(d.has_scans);
        assert_eq!(d.scan_secs, vec![193, 510]);
        assert_eq!(d.scan_bytes, vec![20_000_000, 24_000_000]);
    }

    #[wasm_bindgen_test]
    fn a_day_without_volume_sizes_is_listed_again_only_zoomed_in() {
        let mut d = day(OCT_9_2024, 5_000_000, "V06");
        assert!(is_fresh(&d, 2e12 as i64, true));
        d.has_scans = false;
        assert!(is_fresh(&d, 2e12 as i64, false));
        assert!(!is_fresh(&d, 2e12 as i64, true));
    }

    #[wasm_bindgen_test]
    fn volumes_are_needed_only_where_the_overview_draws_them() {
        let h = 3600.0;
        // A 1000 px lane across two days draws volumes; across a year, bars.
        assert!(needs_scans(1000.0, 48.0 * h));
        assert!(!needs_scans(1000.0, 365.0 * 24.0 * h));
        // The same rule the overview draws by.
        for span in [6.0 * h, 3.0 * 24.0 * h, 20.0 * 24.0 * h, 60.0 * 24.0 * h] {
            assert_eq!(
                needs_scans(800.0, span),
                detail_for(800.0 / span) == OverviewDetail::Scans
            );
        }
        // Not measured yet: nothing drawn, nothing needed.
        assert!(!needs_scans(0.0, 48.0 * h));
        assert!(!needs_scans(1000.0, 0.0));
    }

    fn loaded_fetcher() -> ActivityFetcher {
        let mut fetcher = ActivityFetcher::default();
        fetcher.begin_load(&site());
        fetcher.finish_load(&site());
        fetcher
    }

    #[wasm_bindgen_test]
    fn a_window_the_index_covers_plans_nothing() {
        let fetcher = loaded_fetcher();
        let mut book = ActivityBook::default();
        // Thirty years of index days, the view across all of them.
        let first = OCT_9_2024 - 30 * 365 * DAY_SECS;
        let days: Vec<DayActivity> = (0..=30 * 365)
            .map(|i| index_day(first + i * DAY_SECS, 4_000_000))
            .collect();
        assert_eq!(book.insert_index(days), 30 * 365 + 1);
        let now = (OCT_9_2024 + 2 * DAY_SECS) as f64;
        let w = window(first as f64, (OCT_9_2024 + DAY_SECS) as f64, 0.0, now);
        assert!(fetcher
            .plan(&book, &site(), &w, now * 1000.0)
            .list
            .is_empty());
        // A week of it, still drawn as day bars: nothing either.
        let w = window(
            (OCT_9_2024 - 6 * DAY_SECS) as f64,
            (OCT_9_2024 + DAY_SECS) as f64,
            0.0,
            now,
        );
        assert!(fetcher
            .plan(&book, &site(), &w, now * 1000.0)
            .list
            .is_empty());
    }

    #[wasm_bindgen_test]
    fn the_index_leaves_its_gaps_and_the_newest_days_to_listing() {
        let fetcher = loaded_fetcher();
        let mut book = ActivityBook::default();
        // Indexed through OCT_9_2024 except a gap two days before it.
        book.insert_index(
            (0..10)
                .filter(|i| *i != 2)
                .map(|i| index_day(OCT_9_2024 - i * DAY_SECS, 4_000_000)),
        );
        let now = (OCT_9_2024 + DAY_SECS + DAY_SECS / 2) as f64;
        let w = window((OCT_9_2024 - 9 * DAY_SECS) as f64, now + 3600.0, 0.0, now);
        let mut plan = fetcher.plan(&book, &site(), &w, now * 1000.0).list;
        plan.sort_unstable();
        assert_eq!(plan, vec![OCT_9_2024 - 2 * DAY_SECS, OCT_9_2024 + DAY_SECS]);
    }

    #[wasm_bindgen_test]
    fn zoomed_in_index_days_are_listed_for_their_volumes() {
        let fetcher = loaded_fetcher();
        let mut book = ActivityBook::default();
        book.insert_index([
            index_day(OCT_9_2024, 21_000_000),
            index_day(OCT_9_2024 + DAY_SECS, 4_000_000),
        ]);
        let now = 2e9;
        let mut w = window(
            OCT_9_2024 as f64,
            (OCT_9_2024 + 2 * DAY_SECS) as f64,
            0.0,
            now,
        );
        assert!(fetcher
            .plan(&book, &site(), &w, now * 1000.0)
            .list
            .is_empty());
        w.need_scans = true;
        let mut plan = fetcher.plan(&book, &site(), &w, now * 1000.0).list;
        plan.sort_unstable();
        assert_eq!(plan, vec![OCT_9_2024, OCT_9_2024 + DAY_SECS]);

        // Once listed (with volumes), the index doesn't take them back.
        let mut listed = day(OCT_9_2024, 21_500_000, "V06");
        listed.scan_secs = vec![0];
        listed.scan_bytes = vec![21_500_000];
        book.insert_many([listed.clone()]);
        assert_eq!(book.insert_index([index_day(OCT_9_2024, 21_000_000)]), 0);
        assert_eq!(book.day(&site(), OCT_9_2024), Some(&listed));
        let plan = fetcher.plan(&book, &site(), &w, now * 1000.0).list;
        assert_eq!(plan, vec![OCT_9_2024 + DAY_SECS]);
    }

    fn month(year: i32, month: u32) -> MonthKey {
        MonthKey { year, month }
    }

    /// A month file's day for `day_start`, as `parse_month` makes it.
    fn month_day(day_start: i64) -> DayActivity {
        DayActivity {
            has_scans: true,
            scan_secs: vec![193, 600],
            scan_bytes: vec![20_000_000, 22_000_000],
            ..index_day(day_start, 21_000_000)
        }
    }

    #[wasm_bindgen_test]
    fn zoomed_in_days_a_month_file_holds_fetch_the_month_not_listings() {
        let mut fetcher = loaded_fetcher();
        let mut book = ActivityBook::default();
        // Sep 29 .. Oct 10 2024 indexed; the month files hold Sep 30 and
        // Oct 1..9, not Sep 29 (not backfilled) or Oct 10 (built before it).
        let sep_29 = OCT_9_2024 - 10 * DAY_SECS;
        book.insert_index((0..12).map(|i| index_day(sep_29 + i * DAY_SECS, 4_000_000)));
        book.set_month_days(&site(), (1..11).map(|i| sep_29 + i * DAY_SECS));
        assert!(book.in_month_file(&site(), OCT_9_2024));
        assert!(!book.in_month_file(&site(), sep_29));
        assert!(!book.in_month_file(&SiteId::new("KMLB"), OCT_9_2024));

        let now = 2e9;
        let now_ms = now * 1000.0;
        let mut w = window(
            (OCT_9_2024 - 3 * DAY_SECS) as f64,
            (OCT_9_2024 + DAY_SECS) as f64,
            0.0,
            now,
        );
        // Day bars: the index alone, nothing to fetch.
        assert_eq!(
            fetcher.plan(&book, &site(), &w, now_ms),
            ActivityPlan::default()
        );
        // Zoomed in to four days of October: one month file, no listings.
        w.need_scans = true;
        let plan = fetcher.plan(&book, &site(), &w, now_ms);
        assert_eq!(plan.months, vec![month(2024, 10)]);
        assert!(plan.list.is_empty());

        // While it's in flight nothing else goes out for those days.
        fetcher.start_month(&site(), month(2024, 10));
        assert_eq!(
            fetcher.plan(&book, &site(), &w, now_ms),
            ActivityPlan::default()
        );

        // Across the turn of the month: both months, and only the day no
        // month file holds is listed.
        let wide = window(sep_29 as f64, (OCT_9_2024 + 2 * DAY_SECS) as f64, 0.0, now);
        let wide = ActivityWindow {
            need_scans: true,
            ..wide
        };
        fetcher.finish_month(&site(), month(2024, 10), false, now_ms);
        let plan = fetcher.plan(&book, &site(), &wide, now_ms);
        // October is backing off after its failure; September goes out.
        assert_eq!(plan.months, vec![month(2024, 9)]);
        assert!(plan
            .list
            .iter()
            .all(|d| *d == sep_29 || *d == OCT_9_2024 + DAY_SECS));
        let later = now_ms + FAILURE_BACKOFF_MS;
        let plan = fetcher.plan(&book, &site(), &wide, later);
        assert_eq!(plan.months.len(), 2);
        assert!(plan.months.contains(&month(2024, 10)));
        assert!(plan.months.contains(&month(2024, 9)));
        let mut listed = plan.list.clone();
        listed.sort_unstable();
        assert_eq!(listed, vec![sep_29, OCT_9_2024 + DAY_SECS]);

        // The month arrives: its days have volumes and nothing is left.
        let days: Vec<DayActivity> = (0..9)
            .map(|i| month_day(OCT_9_2024 - (8 - i) * DAY_SECS))
            .collect();
        assert_eq!(book.insert_month(days).len(), 9);
        fetcher.finish_month(&site(), month(2024, 10), true, later);
        assert!(fetcher.plan(&book, &site(), &w, later).list.is_empty());
        let scans = book.scans(&site(), OCT_9_2024 as f64, (OCT_9_2024 + DAY_SECS) as f64);
        assert_eq!(scans.scans.len(), 2);
        assert_eq!(scans.scans[0].bytes, 20_000_000);
        assert!(book
            .listing(&site(), OCT_9_2024 as f64, (OCT_9_2024 + DAY_SECS) as f64)
            .is_some());
    }

    #[wasm_bindgen_test]
    fn a_day_its_fetched_month_file_lacks_is_listed() {
        let mut fetcher = loaded_fetcher();
        let mut book = ActivityBook::default();
        book.insert_index([index_day(OCT_9_2024, 4_000_000)]);
        book.set_month_days(&site(), [OCT_9_2024]);
        let now = 2e9;
        let w = ActivityWindow {
            need_scans: true,
            ..window(OCT_9_2024 as f64, (OCT_9_2024 + DAY_SECS) as f64, 0.0, now)
        };
        // A stale month file (or a 404, or one that didn't parse) came
        // back without the day: it's listed, and the month isn't asked for
        // again this session.
        fetcher.start_month(&site(), month(2024, 10));
        assert!(book.insert_month(Vec::new()).is_empty());
        fetcher.finish_month(&site(), month(2024, 10), true, now * 1000.0);
        let plan = fetcher.plan(&book, &site(), &w, now * 1000.0);
        assert_eq!(plan.list, vec![OCT_9_2024]);
        assert!(plan.months.is_empty());
    }

    #[wasm_bindgen_test]
    fn month_days_keep_final_listings_and_clear_with_the_index() {
        let mut book = ActivityBook::default();
        let listed = day(OCT_9_2024, 21_500_000, "V06");
        book.insert_many([listed.clone()]);
        let taken = book.insert_month([month_day(OCT_9_2024), month_day(OCT_9_2024 + DAY_SECS)]);
        assert_eq!(taken.len(), 1);
        assert_eq!(book.day(&site(), OCT_9_2024), Some(&listed));
        book.set_month_days(&site(), [OCT_9_2024]);
        book.set_month_days(&site(), []);
        assert!(!book.in_month_file(&site(), OCT_9_2024));
    }

    #[wasm_bindgen_test]
    fn index_days_replace_all_but_final_listings_with_volumes() {
        assert!(index_replaces(None));
        // A day listed while it ran, or a record without volume sizes.
        let mut open = day(OCT_9_2024, 5_000_000, "V06");
        open.fetched_at_ms = OCT_9_2024 * 1000;
        assert!(index_replaces(Some(&open)));
        let mut bare = day(OCT_9_2024, 5_000_000, "V06");
        bare.has_scans = false;
        assert!(index_replaces(Some(&bare)));
        assert!(!index_replaces(Some(&day(OCT_9_2024, 5_000_000, "V06"))));
    }

    #[wasm_bindgen_test]
    fn scans_span_the_known_days_with_levels_and_ends() {
        let mut quiet = day(OCT_9_2024, 4_000_000, "V06");
        quiet.scan_secs = vec![0, 300, 86_000];
        quiet.scan_bytes = vec![3_500_000, 3_500_000, 28_000_000];
        let mut next = day(OCT_9_2024 + DAY_SECS, 4_000_000, "V06");
        next.scan_secs = vec![100];
        next.scan_bytes = vec![4_000_000];
        let mut book = ActivityBook::default();
        book.insert_many([quiet, next]);

        let t0 = OCT_9_2024 as f64;
        let sizes = book.scans(&site(), t0 + 3600.0, t0 + DAY_SECS as f64 + 60.0);
        assert_eq!(sizes.known_days, vec![OCT_9_2024, OCT_9_2024 + DAY_SECS]);
        assert_eq!(sizes.scans.len(), 4);
        assert_eq!(sizes.scans[0].end, t0 + 300.0);
        // A gap longer than a volume: drawn at most MAX_SCAN_SECS.
        assert_eq!(sizes.scans[1].end, t0 + 300.0 + MAX_SCAN_SECS);
        // The day's last volume runs to the next day's first.
        assert_eq!(sizes.scans[2].end, t0 + 86_400.0 + 100.0);
        assert_eq!(sizes.scans[0].level, 0.0);
        assert_eq!(sizes.scans[2].level, 1.0);

        assert!(sizes.knows(t0 + 10.0));
        assert!(!sizes.knows(t0 + 2.0 * DAY_SECS as f64));
        assert_eq!(sizes.at(t0 + 400.0).map(|s| s.start), Some(t0 + 300.0));
        assert_eq!(sizes.at(t0 + 300.0 + MAX_SCAN_SECS + 1.0), None);
        assert_eq!(sizes.at(t0 - 1.0), None);
    }

    #[wasm_bindgen_test]
    fn the_index_range_and_levels_read_back_per_site() {
        let mut book = ActivityBook::default();
        assert_eq!(book.index_range(&site()), None);
        book.insert_index([
            index_day(OCT_9_2024, 4_000_000),
            index_day(OCT_9_2024 + 3 * DAY_SECS, 0),
            index_day(OCT_9_2024 - 400 * DAY_SECS, 4_000_000),
        ]);
        assert_eq!(
            book.index_range(&site()),
            Some((OCT_9_2024 - 400 * DAY_SECS, OCT_9_2024 + 3 * DAY_SECS))
        );
        let levels = book.levels(&site(), OCT_9_2024, OCT_9_2024 + 3 * DAY_SECS);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].0, OCT_9_2024);
        assert!(levels[0].1.is_some());
        assert_eq!(levels[1], (OCT_9_2024 + 3 * DAY_SECS, None), "an outage");
        assert!(book.levels(&SiteId::new("KFWS"), 0, i64::MAX).is_empty());
    }

    #[wasm_bindgen_test]
    fn a_span_is_listed_once_every_day_it_touches_has_volume_sizes() {
        let t0 = OCT_9_2024 as f64;
        // A local day 05:00–05:00 UTC touches two UTC days.
        let (start, end) = (t0 + 5.0 * 3600.0, t0 + 29.0 * 3600.0);
        let mut book = ActivityBook::default();
        assert_eq!(book.listing(&site(), start, end), None);
        let mut first = day(OCT_9_2024, 4_000_000, "V06");
        first.scan_secs = vec![3600, 6 * 3600, 20 * 3600];
        first.scan_bytes = vec![3_000_000, 9_000_000, 4_000_000];
        book.insert_many([first]);
        // The second day is only in the index: no per-volume sizes yet.
        book.insert_index([index_day(OCT_9_2024 + DAY_SECS, 4_000_000)]);
        assert_eq!(book.listing(&site(), start, end), None);
        let mut second = day(OCT_9_2024 + DAY_SECS, 4_000_000, "V06");
        second.scan_secs = vec![3600, 10 * 3600];
        second.scan_bytes = vec![5_000_000, 6_000_000];
        book.insert_many([second]);
        let listed = book.listing(&site(), start, end).expect("listed");
        let starts: Vec<f64> = listed.iter().map(|s| s.start - t0).collect();
        // 01:00 on the first day is before the span; 10:00 on the second
        // is after it.
        assert_eq!(starts, vec![6.0 * 3600.0, 20.0 * 3600.0, 25.0 * 3600.0]);
        // An outage day counts as listed (nothing to wait for).
        book.insert_many([day(OCT_9_2024 + 2 * DAY_SECS, 0, "V06")]);
        let out = book.listing(
            &site(),
            t0 + 2.0 * DAY_SECS as f64,
            t0 + 3.0 * DAY_SECS as f64,
        );
        assert_eq!(out, Some(Vec::new()));
    }

    #[wasm_bindgen_test]
    fn windows_past_the_auto_fetch_limit_list_a_sample() {
        assert_eq!(sample_stride_days(1), 1);
        assert_eq!(sample_stride_days(MAX_AUTO_FETCH_DAYS), 1);
        assert_eq!(sample_stride_days(MAX_AUTO_FETCH_DAYS + 1), 2);
        assert_eq!(sample_stride_days(30 * 365), 28);
    }

    #[wasm_bindgen_test]
    fn day_start_is_utc_midnight() {
        assert_eq!(day_start_of(OCT_9_2024 as f64 + 3600.5), OCT_9_2024);
        assert_eq!(day_start_of(OCT_9_2024 as f64), OCT_9_2024);
        assert_eq!(day_start_of(-1.0), -DAY_SECS);
    }

    fn scan(start: f64, level: f32) -> ScanActivity {
        ScanActivity {
            start,
            end: start + 300.0,
            bytes: 1,
            level,
        }
    }

    #[wasm_bindgen_test]
    fn bars_are_the_volumes_in_view_on_the_views_scale() {
        let sizes = ScanSizes {
            scans: vec![
                scan(0.0, 0.9),
                scan(300.0, 0.1),
                scan(600.0, 0.2),
                scan(900.0, 0.6),
                scan(1_200.0, 0.1),
            ],
            known_days: vec![0],
        };
        let bars = sizes.bars(350.0, 1_000.0);
        // The one ending before the window is out; the one starting after
        // it is out; the ones touching it are in.
        assert_eq!(
            bars.iter().map(|(s, _)| s.start).collect::<Vec<_>>(),
            vec![300.0, 600.0, 900.0]
        );
        // Stretched to what is in view: the median at the floor, the
        // stormiest near the top.
        let scale = ViewScale::from_levels([0.1, 0.2, 0.6]);
        for (s, frac) in &bars {
            assert_eq!(*frac, scale.height_frac(s.level));
        }
        assert_eq!(bars[1].1, MIN_BAR_FRAC);
        assert!(bars[2].1 > 0.95);
        assert!(sizes.bars(5_000.0, 6_000.0).is_empty());
    }

    #[wasm_bindgen_test]
    fn unknown_pieces_split_at_midnight_and_drop_known_days() {
        let d = DAY_SECS as f64;
        let sizes = ScanSizes {
            scans: Vec::new(),
            known_days: vec![DAY_SECS],
        };
        // A span from the evening of day 0 into day 2: day 1 is known.
        let pieces = sizes.unknown_pieces([(0.75 * d, 2.25 * d)]);
        assert_eq!(pieces, vec![(0.75 * d, d), (2.0 * d, 2.25 * d)]);
        assert!(sizes.unknown_pieces([(d + 10.0, d + 20.0)]).is_empty());
    }
}
