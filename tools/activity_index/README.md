# Storm-activity index

`build_index.py` writes one file per radar site with each UTC day's volume
count, mean volume size and format, the numbers behind the timeline
overview's storm-activity bars, and, for the newest months, one file per
site-month with every volume's start time and size, which the overview
draws zoomed in to volumes. The nightly workflow
(`.github/workflows/activity-index.yml`) publishes them under
`activity-index/` on the `gh-pages` branch. The app reads
`activity-index/<SITE>.json` from its own site once per session instead of
listing the archive one day at a time, and, zoomed in to volumes,
`activity-index/<SITE>/<YYYY-MM>.json` for a month in view instead of
listing each of its days. Days the index doesn't have yet (the last day or
two, gaps while the backfill runs, and months without a month file) are
still listed live.

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
- `scans`: `[index, count]` runs of the days whose volumes are in a month
  file (below). Missing in files written before month files: none are.
- `built`: when the file was written (Unix ms).

About 150 KB raw, 70 KB gzipped (as GitHub Pages serves it) for the full
archive. A new format means a new `v`; the app ignores versions it doesn't
know and falls back to live listing.

## Month files (`v: 1`)

`<SITE>/<YYYY-MM>.json` holds one site-month's volumes, the same ones the
app keeps from a live listing of each day:

```json
{"v":1,"site":"KTLX","month":"2026-10","built":1792520404977,"complete":false,
 "days":[null,
         [204,6899183,"V06",[199,422,418],[7120343,7011873,6923302]],
         [0,0,"",[],[]]]}
```

- `days[i]`: day `i + 1` of the month, always as many entries as the month
  has days. `null` is a day without month data yet (not final, not listed,
  or not backfilled), which the app lists itself.
- A day is `[volumes, mean_bytes, format, starts, sizes]`. The first three
  are the day file's numbers. `starts` and `sizes` are the volumes of the
  day's format that have a start time in their name (the app keeps only
  those, so a day straddling an upgrade compares like with like), in time
  order: `starts` holds seconds after UTC midnight delta-encoded (the first
  is the offset itself, each next one the gap since the previous; mostly
  three digits), `sizes` each volume's size in bytes (capped at 2^32 - 1).
  `[0,0,"",[],[]]` is a day the radar recorded nothing.
- `complete`: every day of the month is there. Only final days are ever
  written, so a complete month never changes again; an incomplete one gains
  days as they are listed.

A month file is written whenever one of its days is listed. A busy month
(about 300 volumes a day) is about 115 KB raw and 48 KB gzipped (KTLX,
October 2026: 409 volumes in 5.2 KB, 2.2 KB gzipped, about 13 bytes a
volume raw).

Month files are kept for the newest `--month-files` months (default 12,
counting the month of the newest final day); older ones are deleted and
their days unmarked, and the app lists those days live as before. The
whole archive holds about 330 million volumes, about 4 GB in this
encoding, well past GitHub Pages' 1 GB site limit; recent years add about
16 million volumes, about 200 MB, a year. Twelve months of every site is
about 200 MB on `gh-pages`. Since each night only appends days to the end of
the current months' files, git stores those updates as small deltas.

`status.json` sits beside the site files: what's covered, how many site-days
are still missing, and whether the backfill is complete, for the day files and (`month_files`)
for the month files. The app doesn't read it.

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
before. Every day it lists also goes into its month file.

The nightly run never lists a day again once it's indexed, so the days
indexed before month files existed have none. `--backfill-months` (the
workflow's `backfill_months` input, ticked by default on a manual run, never
on the schedule) lists them after the new days: indexed days with volumes
and no month data, within the kept months, newest first, one request each
(no presence listing: the day file already says the site had data); days
without volumes get their month data for free. A backfilled day is marked
in the day file's `scans` and is never listed again, so a run that stops at
the budget or time limit picks up where it left off. Twelve months of every
site is about 58,000 site-days.

### Backfilling the month files

1. Actions, *Build activity index*, *Run workflow* on `main`.
2. Leave `backfill_months` ticked; set `budget` to `100000` and
   `max_minutes` to `120` (the archive backfill ran about 100,000
   requests in 40 minutes, so 58,000 listings take well under an hour;
   the extra time is slack).
3. Run it. The log's last line and `status.json`'s
   `month_files.missing_site_days` say how many are left; `0` (and
   `month_files.complete: true`) means done. Otherwise run it again.

### If the nightly run stops

GitHub disables a public repository's scheduled workflows after 60 days
without activity. The workflow's `keepalive` job enables it again every
night, which should keep that from happening. If it lapses anyway, the
Actions tab shows *Build activity index* as disabled: *Enable workflow*,
then *Run workflow* once to catch up. Nothing in the app breaks meanwhile;
days past `status.json`'s `through` are listed live, as before the index.

## Running it

```bash
# Tests (offline, a fake bucket)
python3 tools/activity_index/test_build_index.py

# A few days of one site
python3 tools/activity_index/build_index.py --out /tmp/idx \
    --sites KTBW --since 2024-10-05 --until 2024-10-11

# Month files for two already-indexed days (copy the site's published
# KTLX.json into /tmp/idx first): two requests
python3 tools/activity_index/build_index.py --out /tmp/idx \
    --sites KTLX --since 2026-10-17 --until 2026-10-18 --backfill-months
```

The site list is read from `src/data/sites.rs`, so a site added there is
picked up by the next run.
