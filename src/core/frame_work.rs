//! What a site is doing about the frame the playhead asks of it.
//!
//! The canvas caption says why a map is blank or behind the playhead. For
//! archive data the honest answer is a step in the pipeline that brings the
//! volume at the playhead: finding it in the day's listing, waiting for a
//! download slot, downloading it, decoding it, or failing at one of those.
//! [`decide`] picks that step for one site from plain facts the shell
//! gathers per site each frame ([`WorkFacts`]); the caption rule
//! ([`crate::core::canvas::decide_canvas_caption`]) turns it into words.
//!
//! Every site is decided the same way, so the focus and the other sites on
//! the map read alike, whether the playhead rides now, plays a range or
//! sits on a day years back.

use crate::core::OperationId;

/// How far back from the playhead a listed volume still counts as the one
/// it calls for, when no listed volume spans it: the newest volume of the
/// day, while the next one has not been listed yet.
pub(crate) const TRAILING_VOLUME_SECS: i64 = 900;

/// Volume keys within this many seconds are the same volume (a download is
/// keyed by its listing time, a worker's result by the volume header).
const SAME_VOLUME_SECS: i64 = 30;

/// One step in bringing the playhead's volume to the map.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) enum SiteWork {
    /// Nothing is on its way: the volume is on the device, or the archive
    /// has nothing at the playhead, or nobody asked for it.
    #[default]
    Idle,
    /// The day's listing is being fetched, to find the volume.
    Listing,
    /// The day's listing failed; it is tried again after a wait.
    ListingFailed,
    /// The volume waits for a free download slot.
    Queued { scan_start: i64 },
    /// The volume is downloading: bytes so far, and the total when the
    /// response gave one.
    Downloading {
        scan_start: i64,
        received: u64,
        total: Option<u64>,
    },
    /// The volume is downloaded and being decoded and stored.
    Processing { scan_start: i64 },
    /// The volume's download failed. `retry` is the operation a retry
    /// starts again.
    Failed {
        scan_start: i64,
        error: String,
        retry: OperationId,
    },
}

impl SiteWork {
    /// Whether something is on its way (the map will change on its own).
    pub(crate) fn is_busy(&self) -> bool {
        matches!(
            self,
            SiteWork::Listing
                | SiteWork::Queued { .. }
                | SiteWork::Downloading { .. }
                | SiteWork::Processing { .. }
        )
    }
}

/// A download that failed, as the acquisition log keeps it.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct FailedDownload {
    pub scan_start: i64,
    pub error: String,
    pub id: OperationId,
}

/// The facts [`decide`] reads for one site. The shell gathers them each
/// frame from the site's timeline, listing, queue, ledger and log.
#[derive(Clone, Default, Debug)]
pub(crate) struct WorkFacts {
    /// The playhead, Unix seconds.
    pub playhead: f64,
    /// The site's timeline holds a volume at the playhead.
    pub on_device: bool,
    /// The listed volumes of the playhead's day and the days around it,
    /// `(start, end)`, Unix seconds.
    pub listed: Vec<(i64, i64)>,
    /// The playhead's day has a listing.
    pub day_listed: bool,
    /// The playhead's day's listing is being fetched.
    pub listing_pending: bool,
    /// The playhead's day's listing failed and waits to be tried again.
    pub listing_failed: bool,
    /// Volumes waiting for a download slot.
    pub queued: Vec<i64>,
    /// Volumes downloading.
    pub downloading: Vec<i64>,
    /// Volumes asked for and not yet on the timeline (queued, downloading,
    /// decoding, or stored and waiting for the timeline).
    pub requested: Vec<i64>,
    /// Bytes received by each download in flight.
    pub bytes: Vec<(i64, u64, Option<u64>)>,
    /// Failed downloads, oldest first.
    pub failed: Vec<FailedDownload>,
}

/// The listed volume the playhead calls for: the one spanning it, else the
/// newest one that started at most [`TRAILING_VOLUME_SECS`] before it.
pub(crate) fn wanted_volume(listed: &[(i64, i64)], playhead: f64) -> Option<i64> {
    let pos = playhead.floor() as i64;
    listed
        .iter()
        .find(|&&(start, end)| start <= pos && pos < end)
        .map(|&(start, _)| start)
        .or_else(|| {
            listed
                .iter()
                .filter(|&&(start, _)| start <= pos && pos - start <= TRAILING_VOLUME_SECS)
                .map(|&(start, _)| start)
                .max()
        })
}

fn near(keys: &[i64], key: i64) -> bool {
    keys.iter().any(|&k| (k - key).abs() < SAME_VOLUME_SECS)
}

/// The step bringing the playhead's volume to the map. A volume on the
/// device needs nothing. Otherwise the listed volume the playhead calls for
/// ([`wanted_volume`]) is followed through the pipeline, latest stage
/// first so a volume is in one step only: downloading, waiting for a slot,
/// decoding, or failed. With no listed volume yet, the day's listing is the
/// step.
pub(crate) fn decide(facts: &WorkFacts) -> SiteWork {
    if facts.on_device {
        return SiteWork::Idle;
    }
    let Some(start) = wanted_volume(&facts.listed, facts.playhead) else {
        return if facts.listing_pending {
            SiteWork::Listing
        } else if facts.listing_failed {
            SiteWork::ListingFailed
        } else if !facts.day_listed && !facts.requested.is_empty() {
            // Asked for before the listing came in (a link's first volume).
            SiteWork::Listing
        } else {
            SiteWork::Idle
        };
    };
    if near(&facts.downloading, start) {
        let (received, total) = facts
            .bytes
            .iter()
            .find(|(k, ..)| (k - start).abs() < SAME_VOLUME_SECS)
            .map_or((0, None), |&(_, r, t)| (r, t));
        return SiteWork::Downloading {
            scan_start: start,
            received,
            total,
        };
    }
    if near(&facts.queued, start) {
        return SiteWork::Queued { scan_start: start };
    }
    if near(&facts.requested, start) {
        return SiteWork::Processing { scan_start: start };
    }
    if let Some(f) = facts
        .failed
        .iter()
        .rev()
        .find(|f| (f.scan_start - start).abs() < SAME_VOLUME_SECS)
    {
        return SiteWork::Failed {
            scan_start: start,
            error: f.error.clone(),
            retry: f.id,
        };
    }
    SiteWork::Idle
}

/// A download error in a few words, for the caption: its first line, at
/// most [`ERROR_WORDS`] characters.
pub(crate) fn short_error(error: &str) -> String {
    let line = error.lines().next().unwrap_or("").trim();
    // The caption already says the download failed.
    let line = line.strip_prefix("Download failed: ").unwrap_or(line);
    if line.chars().count() <= ERROR_WORDS {
        return line.to_string();
    }
    let cut: String = line.chars().take(ERROR_WORDS - 1).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// The longest error the caption shows whole.
const ERROR_WORDS: usize = 48;

/// Bytes as the caption reads them: `2.1 MB`, `640 KB`.
pub(crate) fn format_mb(bytes: u64) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", (bytes / 1000).max(1))
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const T0: i64 = 1_729_800_000;

    fn facts() -> WorkFacts {
        WorkFacts {
            playhead: (T0 + 100) as f64,
            day_listed: true,
            listed: vec![(T0 - 300, T0), (T0, T0 + 300), (T0 + 300, T0 + 600)],
            ..WorkFacts::default()
        }
    }

    #[wasm_bindgen_test]
    fn a_volume_on_the_device_needs_nothing() {
        let mut f = facts();
        f.on_device = true;
        f.downloading = vec![T0];
        assert_eq!(decide(&f), SiteWork::Idle);
    }

    #[wasm_bindgen_test]
    fn the_playheads_volume_is_followed_through_the_pipeline() {
        let mut f = facts();
        f.requested = vec![T0];
        f.queued = vec![T0];
        assert_eq!(decide(&f), SiteWork::Queued { scan_start: T0 });

        f.queued.clear();
        f.downloading = vec![T0];
        f.bytes = vec![(T0, 2_100_000, Some(4_800_000))];
        assert_eq!(
            decide(&f),
            SiteWork::Downloading {
                scan_start: T0,
                received: 2_100_000,
                total: Some(4_800_000)
            }
        );

        f.downloading.clear();
        assert_eq!(decide(&f), SiteWork::Processing { scan_start: T0 });

        f.on_device = true;
        assert_eq!(decide(&f), SiteWork::Idle);
    }

    #[wasm_bindgen_test]
    fn other_volumes_downloading_do_not_count() {
        let mut f = facts();
        f.downloading = vec![T0 + 300];
        f.requested = vec![T0 + 300];
        assert_eq!(decide(&f), SiteWork::Idle);
    }

    #[wasm_bindgen_test]
    fn a_failed_download_says_why_and_offers_its_retry() {
        let mut f = facts();
        f.failed = vec![
            FailedDownload {
                scan_start: T0,
                error: "first".into(),
                id: 1,
            },
            FailedDownload {
                scan_start: T0 + 2,
                error: "HTTP 503".into(),
                id: 2,
            },
        ];
        assert_eq!(
            decide(&f),
            SiteWork::Failed {
                scan_start: T0,
                error: "HTTP 503".into(),
                retry: 2,
            }
        );
        // Asked for again: the new request wins over the old failure.
        f.queued = vec![T0];
        assert_eq!(decide(&f), SiteWork::Queued { scan_start: T0 });
    }

    #[wasm_bindgen_test]
    fn with_no_listed_volume_the_listing_is_the_step() {
        let mut f = WorkFacts {
            playhead: T0 as f64,
            listing_pending: true,
            ..WorkFacts::default()
        };
        assert_eq!(decide(&f), SiteWork::Listing);
        f.listing_pending = false;
        f.listing_failed = true;
        assert_eq!(decide(&f), SiteWork::ListingFailed);
        f.listing_failed = false;
        assert_eq!(decide(&f), SiteWork::Idle);
        f.requested = vec![T0 - 60];
        assert_eq!(decide(&f), SiteWork::Listing);
    }

    #[wasm_bindgen_test]
    fn the_newest_volume_counts_until_the_next_is_listed() {
        // Live: the playhead is now, past the newest listed volume's end.
        let listed = [(T0 - 600, T0 - 300), (T0 - 300, T0)];
        assert_eq!(wanted_volume(&listed, (T0 + 120) as f64), Some(T0 - 300));
        assert_eq!(
            wanted_volume(&listed, (T0 - 300 + TRAILING_VOLUME_SECS + 1) as f64),
            None
        );
        assert_eq!(wanted_volume(&listed, (T0 - 450) as f64), Some(T0 - 600));
    }

    #[wasm_bindgen_test]
    fn errors_and_sizes_read_short() {
        assert_eq!(short_error("HTTP 503\nmore"), "HTTP 503");
        assert_eq!(short_error("Download failed: HTTP 403"), "HTTP 403");
        let long = "x".repeat(80);
        assert_eq!(short_error(&long).chars().count(), ERROR_WORDS);
        assert_eq!(format_mb(2_100_000), "2.1 MB");
        assert_eq!(format_mb(640_000), "640 KB");
        assert_eq!(format_mb(10), "1 KB");
    }
}
