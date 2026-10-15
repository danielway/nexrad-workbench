//! Day listings with file sizes, for the overview's storm-activity bars.
//!
//! The app's own listing path (`download::fetch_archive_listing`) goes
//! through `nexrad_data::aws::archive::list_files`, which in the published
//! 1.0.0-rc.7 drops each file's size. Until the app moves to a release with
//! `list_files_with_sizes`, this issues the same S3 `ListObjectsV2` request
//! itself and parses it with [`crate::core::storm_activity::parse_s3_listing`].
//!
//! Results come back over a channel polled each frame, like the download
//! channel. Stored summaries are read and written through the
//! [`MainThreadStore`].

use super::download::NetworkStats;
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
    /// A site's stored day summaries (empty when the read failed).
    Loaded {
        site: SiteId,
        days: Vec<DayActivity>,
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
    /// Read `site`'s stored day summaries.
    pub(crate) fn load(&self, ctx: egui::Context, store: MainThreadStore, site: SiteId) {
        let sender = self.sender.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let days = match store.day_activity_for_site(&site).await {
                Ok(days) => days,
                Err(e) => {
                    log::warn!("Reading storm activity for {} failed: {}", site, e);
                    Vec::new()
                }
            };
            let _ = sender.send(ActivityMessage::Loaded { site, days });
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
