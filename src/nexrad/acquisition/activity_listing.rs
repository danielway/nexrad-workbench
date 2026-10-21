//! Day listings with file sizes, for the overview's storm-activity bars.
//!
//! The app's own listing path (`download::fetch_archive_listing`) goes
//! through `nexrad_data::aws::archive::list_files`, which in the published
//! 1.0.0-rc.7 drops each file's size. Until the app moves to a release with
//! `list_files_with_sizes`, this issues the same S3 `ListObjectsV2` request
//! itself and parses it with [`crate::core::storm_activity::parse_s3_listing`].
//!
//! Before any listing, a site's published activity index is read once per
//! session from the app's own origin (`activity-index/<SITE>.json`, see
//! [`crate::core::activity_index`]): its cached copy comes out of
//! IndexedDB, is revalidated with `If-None-Match`, and a new copy replaces
//! it. Without an index (404, a dev server) the days are listed as before.
//! Zoomed in to volumes, a month the index has a month file for is fetched
//! from beside it (`activity-index/<SITE>/<YYYY-MM>.json`) instead of
//! listing its days; its days are then stored like listed ones.
//!
//! Results come back over a channel polled each frame, like the download
//! channel. Stored summaries are read and written through the
//! [`MainThreadStore`].

use super::download::NetworkStats;
use crate::core::activity_index::{
    index_path, month_path, resolve_index, resolve_month, revalidation_tag, IndexOutcome,
    IndexResponse, MonthKey,
};
use crate::core::storm_activity::{parse_s3_listing, summarize_day};
use crate::data::{DayActivity, MainThreadStore, SiteId};
use crate::net::retry::{with_retry, Verdict, DEFAULT_POLICY};
use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

/// The archive bucket the listings come from.
const ARCHIVE_BUCKET_URL: &str = "https://unidata-nexrad-level2.s3.amazonaws.com";

/// A finished activity request.
pub(crate) enum ActivityMessage {
    /// A site's stored day summaries (empty when the read failed), its
    /// published index's days (empty without an index), and which of those
    /// a month file holds the volumes of.
    Loaded {
        site: SiteId,
        days: Vec<DayActivity>,
        index_days: Vec<DayActivity>,
        month_days: Vec<i64>,
    },
    /// A month file's days (none when it isn't published or doesn't parse),
    /// or why it couldn't be fetched.
    Month {
        site: SiteId,
        month: MonthKey,
        result: Result<Vec<DayActivity>, String>,
    },
    /// One day's listing, summarized, or why it failed.
    Listed {
        site: SiteId,
        day_start: i64,
        result: Result<DayActivity, String>,
    },
}

/// Runs activity reads, listings and writes off the frame loop.
pub(crate) struct ActivityChannel {
    sender: Sender<ActivityMessage>,
    receiver: Receiver<ActivityMessage>,
}

impl Default for ActivityChannel {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self { sender, receiver }
    }
}

impl ActivityChannel {
    /// Read `site`'s stored day summaries and its published index.
    pub(crate) fn load(
        &self,
        ctx: egui::Context,
        stats: NetworkStats,
        store: MainThreadStore,
        site: SiteId,
    ) {
        let sender = self.sender.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let days = match store.day_activity_for_site(&site).await {
                Ok(days) => days,
                Err(e) => {
                    log::warn!("Reading storm activity for {} failed: {}", site, e);
                    Vec::new()
                }
            };
            let index = load_index(&stats, &store, &site).await;
            let _ = sender.send(ActivityMessage::Loaded {
                site,
                days,
                index_days: index.days,
                month_days: index.month_days,
            });
            ctx.request_repaint();
        });
    }

    /// Fetch one month file of `site`'s index.
    pub(crate) fn fetch_month(
        &self,
        ctx: egui::Context,
        stats: NetworkStats,
        site: SiteId,
        month: MonthKey,
    ) {
        let sender = self.sender.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let response = match page_path() {
                Some(page) => {
                    let path = month_path(&page, &site, month);
                    stats.request_started();
                    // No If-None-Match of our own: the days are stored, and
                    // the browser's HTTP cache revalidates a repeat.
                    let response = fetch_index(&path, None).await;
                    stats.request_completed(0);
                    response
                }
                None => IndexResponse::Failed,
            };
            let result = resolve_month(&site, month, response);
            match &result {
                Ok(days) => log::info!(
                    "Activity month file {} {}: {} days with volumes",
                    site,
                    month.name(),
                    days.len()
                ),
                Err(e) => log::debug!("Activity month file {} {}: {}", site, month.name(), e),
            }
            let _ = sender.send(ActivityMessage::Month {
                site,
                month,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// List one UTC day and summarize it.
    pub(crate) fn list_day(
        &self,
        ctx: egui::Context,
        stats: NetworkStats,
        site: SiteId,
        day_start: i64,
    ) {
        let sender = self.sender.clone();
        wasm_bindgen_futures::spawn_local(async move {
            stats.request_started();
            let result = list_day_files(&site, day_start).await.map(|files| {
                let now_ms = js_sys::Date::now() as i64;
                summarize_day(&site, day_start, &files, now_ms)
            });
            stats.request_completed(0);
            let _ = sender.send(ActivityMessage::Listed {
                site,
                day_start,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// Store summaries (fire and forget; a failed write only costs a
    /// re-listing next session).
    pub(crate) fn save(&self, store: MainThreadStore, days: Vec<DayActivity>) {
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = store.put_day_activity(&days).await {
                log::warn!("Storing storm activity failed: {}", e);
            }
        });
    }

    /// A finished request, if any.
    pub(crate) fn try_recv(&self) -> Option<ActivityMessage> {
        self.receiver.try_recv().ok()
    }
}

/// The site's index days: the cached copy, revalidated against the
/// published file, with the cache updated as [`resolve_index`] decides.
/// Each IndexedDB step is its own transaction, finished before the next.
async fn load_index(stats: &NetworkStats, store: &MainThreadStore, site: &SiteId) -> IndexOutcome {
    let cached = store.activity_index(site).await.unwrap_or_else(|e| {
        log::debug!(
            "Reading the cached activity index for {} failed: {}",
            site,
            e
        );
        None
    });
    let Some(path) = page_path().map(|p| index_path(&p, site)) else {
        return resolve_index(site, cached, IndexResponse::Failed);
    };
    stats.request_started();
    let response = fetch_index(&path, revalidation_tag(cached.as_ref())).await;
    stats.request_completed(0);
    let mut outcome = resolve_index(site, cached, response);
    if let Some(record) = outcome.save.take() {
        if let Err(e) = store.put_activity_index(&record).await {
            log::warn!("Caching the activity index for {} failed: {}", site, e);
        }
    } else if outcome.forget {
        let _ = store.delete_activity_index(site).await;
    }
    log::debug!(
        "Activity index for {}: {} days, {} in month files",
        site,
        outcome.days.len(),
        outcome.month_days.len()
    );
    outcome
}

/// The page's path (`/nexrad-workbench/` in production), to find the index
/// beside it.
fn page_path() -> Option<String> {
    web_sys::window()?.location().pathname().ok()
}

/// GET an index file, conditionally on `etag`.
async fn fetch_index(path: &str, etag: Option<&str>) -> IndexResponse {
    let Some(window) = web_sys::window() else {
        return IndexResponse::Failed;
    };
    let headers = web_sys::Headers::new().ok();
    if let (Some(headers), Some(etag)) = (&headers, etag) {
        let _ = headers.set("If-None-Match", etag);
    }
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    if let Some(headers) = &headers {
        init.set_headers(headers);
    }
    let Ok(request) = web_sys::Request::new_with_str_and_init(path, &init) else {
        return IndexResponse::Failed;
    };
    let response: web_sys::Response = match JsFuture::from(window.fetch_with_request(&request))
        .await
        .ok()
        .and_then(|v| v.dyn_into().ok())
    {
        Some(r) => r,
        None => return IndexResponse::Failed,
    };
    match response.status() {
        304 => IndexResponse::NotModified,
        404 => IndexResponse::NotFound,
        200 => {
            let etag = response.headers().get("ETag").ok().flatten();
            let Ok(text) = response.text() else {
                return IndexResponse::Failed;
            };
            match JsFuture::from(text).await.ok().and_then(|v| v.as_string()) {
                Some(body) => IndexResponse::Fetched { body, etag },
                None => IndexResponse::Failed,
            }
        }
        _ => IndexResponse::Failed,
    }
}

/// The `ListObjectsV2` URL for one site-day.
fn listing_url(site: &SiteId, day_start: i64) -> Option<String> {
    let date = chrono::DateTime::from_timestamp(day_start, 0)?.date_naive();
    Some(format!(
        "{}/?list-type=2&prefix={}/{}",
        ARCHIVE_BUCKET_URL,
        date.format("%Y/%m/%d"),
        site.0
    ))
}

/// Every file listed for `site` on the day starting at `day_start`, with
/// sizes.
async fn list_day_files(site: &SiteId, day_start: i64) -> Result<Vec<(String, u64)>, String> {
    let url = listing_url(site, day_start).ok_or("bad day")?;
    let body = with_retry(&DEFAULT_POLICY, "activity_list", |_attempt| {
        let url = url.clone();
        async move { fetch_text(&url).await }
    })
    .await?;
    let listing = parse_s3_listing(&body);
    if listing.truncated {
        // A site-day holds a few hundred files; a truncated page means
        // something unexpected, so don't record a partial day.
        return Err("listing truncated".into());
    }
    Ok(listing.files)
}

/// One GET, classified for the retry loop.
async fn fetch_text(url: &str) -> Verdict<String> {
    let global = js_sys::global();
    let promise = if let Some(window) = global.dyn_ref::<web_sys::Window>() {
        window.fetch_with_str(url)
    } else if let Some(worker) = global.dyn_ref::<web_sys::WorkerGlobalScope>() {
        worker.fetch_with_str(url)
    } else {
        return Verdict::Terminal("no fetch".into());
    };
    let response: web_sys::Response = match JsFuture::from(promise).await {
        Ok(v) => match v.dyn_into() {
            Ok(r) => r,
            Err(_) => return Verdict::Terminal("invalid response".into()),
        },
        Err(_) => return Verdict::Retry { after: None },
    };
    let status = response.status();
    if status == 408 || status == 429 || (500..=599).contains(&status) {
        return Verdict::Retry { after: None };
    }
    if !response.ok() {
        return Verdict::Terminal(format!("HTTP {}", status));
    }
    let Ok(text) = response.text() else {
        return Verdict::Terminal("no body".into());
    };
    match JsFuture::from(text).await.ok().and_then(|v| v.as_string()) {
        Some(body) => Verdict::Ok(body),
        None => Verdict::Retry { after: None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn lists_the_site_day_prefix() {
        assert_eq!(
            listing_url(&SiteId::new("KTBW"), 1_728_432_000).as_deref(),
            Some("https://unidata-nexrad-level2.s3.amazonaws.com/?list-type=2&prefix=2024/10/09/KTBW")
        );
    }
}
