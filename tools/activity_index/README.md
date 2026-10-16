# Storm-activity index

`build_index.py` writes one file per radar site with each UTC day's volume
count, mean volume size and format, the numbers behind the timeline
overview's storm-activity bars. The nightly workflow
(`.github/workflows/activity-index.yml`) publishes them under
`activity-index/` on the `gh-pages` branch, and the app reads
`activity-index/<SITE>.json` from its own site once per session instead of
listing the archive one day at a time. Days the index doesn't have yet (the
last day or two, and gaps while the backfill runs) are still listed live.

Each day is summarized exactly as the app summarizes a live listing
(`core::storm_activity::{parse_s3_listing, summarize_day}`): the same
`ListObjectsV2` request (`?list-type=2&prefix=YYYY/MM/DD/SITE`), `_MDM` and
empty files skipped, the most common format suffix that day (ties to the
greater one), and the integer mean of the volumes' sizes. Where the app
gives up on a truncated page, the builder follows the continuation token.

## File format (`v: 1`)

```json
{"v":1,"site":"KTBW","built":1792108854055,"first_day":20001,
 "vols":[257,229,null,0,216],
 "mean":[10055994,17043173,null,0,21873695],
 "fmt":[[0,"V06"]]}
```

- `first_day`: the first entry's UTC day, as days since 1970-01-01.
- `vols[i]`, `mean[i]`: day `first_day + i`'s volume count and mean size in
  bytes. `0, 0` is a day the radar recorded nothing; `null, null` is a day
  not indexed yet (a listing failed), which the app lists itself.
- `fmt`: `[index, format]` runs. A day with volumes has the format of the
  last run starting at or before it; a day without has `""`.
- `built`: when the file was written (Unix ms).

About 150 KB raw, 70 KB gzipped (as GitHub Pages serves it) for the full
archive. A new format means a new `v`; the app ignores versions it doesn't
know and falls back to live listing.

`status.json` sits beside the site files: what's covered, how many site-days
are still missing, and whether the backfill is complete. The app doesn't read
it.

## Pacing

A run reads the existing files, then works through the days some site
doesn't cover, newest first. For each day one delimited listing
(`prefix=YYYY/MM/DD/&delimiter=/`) says which sites have any data; sites
without are recorded as empty days for free, and only the rest are listed.
It stops at `--budget` requests (default 100,000, retries included) or
`--max-minutes` (default 40), runs `--concurrency` requests at once (default
24), backs off exponentially on 5xx, 429 and `SlowDown` (pausing every
worker on a throttle), and checkpoints the files every five minutes. Days
that ended under three hours ago aren't final and are left to the app.

The full archive is about 1.65 million site-days with data, so the nightly
run backfills it over roughly 17 nights; after that each night only lists
the newest day for every site (about 200 requests), plus any day that failed
before.

## Running it

```bash
# Tests (offline, a fake bucket)
python3 tools/activity_index/test_build_index.py

# A few days of one site
python3 tools/activity_index/build_index.py --out /tmp/idx \
    --sites KTBW --since 2024-10-05 --until 2024-10-11
```

The site list is read from `src/data/sites.rs`, so a site added there is
picked up by the next run.
