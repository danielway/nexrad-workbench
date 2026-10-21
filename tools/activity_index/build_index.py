#!/usr/bin/env python3
"""Build the storm-activity index: one small file per radar site holding each
UTC day's volume count, mean volume size and format, so the app's timeline
overview reads one file per site instead of listing the archive day by day.

Each run is incremental and resumable. It reads the files already in the
output directory, lists the days they don't cover yet (newest first, across
all sites), merges the results in, and stops at a request budget or a time
limit. Run nightly, it first backfills the archive over a few weeks and from
then on only tops up the newest days. See README.md for the file format.

Standard library only:

    python3 tools/activity_index/build_index.py --out activity-index
    python3 tools/activity_index/build_index.py --out /tmp/idx \\
        --sites KTBW --since 2024-10-07 --until 2024-10-10

Every day summary mirrors `core::storm_activity::{parse_s3_listing,
summarize_day}` in the app, so an index day and a day the app lists live
agree.
"""

import argparse
import concurrent.futures
import datetime as dt
import json
import os
import random
import re
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

BUCKET_URL = "https://unidata-nexrad-level2.s3.amazonaws.com"
FORMAT_VERSION = 1
# The month files' format (`"v"` in `<SITE>/<YYYY-MM>.json`).
MONTH_FORMAT_VERSION = 1
# Month files kept by default: the newest this many months (see README.md
# for why not the whole archive).
DEFAULT_MONTH_FILES = 12
U32_MAX = 2**32 - 1
DAY_SECS = 86_400
# A day is final (and indexed) once it ended this long ago; until then the
# app lists it live. Matches FINAL_AFTER_DAY_END_MS in the app.
FINAL_AFTER_SECS = 3 * 3600
# The archive's first days (the bucket starts 1991-06-05).
ARCHIVE_START = dt.date(1991, 6, 1)
# Days whose presence (which sites have data) is listed together, before
# their site listings go out.
DAYS_PER_BATCH = 8
MAX_ATTEMPTS = 6
CHECKPOINT_SECS = 300
USER_AGENT = "nexrad-workbench-activity-index (+https://github.com/danielway/nexrad-workbench)"


def day_number(date):
    return (date - dt.date(1970, 1, 1)).days


def day_date(number):
    return dt.date(1970, 1, 1) + dt.timedelta(days=number)


# ---------------------------------------------------------------------------
# Listing parse and day summary (mirrors core::storm_activity)
# ---------------------------------------------------------------------------


def _tag(body, name):
    open_tag, close_tag = "<%s>" % name, "</%s>" % name
    start = body.find(open_tag)
    if start < 0:
        return None
    start += len(open_tag)
    end = body.find(close_tag, start)
    if end < 0:
        return None
    return body[start:end]


_U64 = re.compile(r"\+?[0-9]+")


def parse_s3_listing(xml):
    """`(files, truncated, next_token)`: each object's file name (the key's
    last path segment) and size, as `parse_s3_listing` in the app reads
    them."""
    files = []
    rest = xml
    while True:
        start = rest.find("<Contents>")
        if start < 0:
            break
        after = rest[start + len("<Contents>"):]
        end = after.find("</Contents>")
        if end < 0:
            break
        body = after[:end]
        key, size = _tag(body, "Key"), _tag(body, "Size")
        if key is not None and size is not None:
            size = size.strip()
            if _U64.fullmatch(size) and int(size) < 2**64:
                files.append((key.rsplit("/", 1)[-1], int(size)))
        rest = after[end:]
    truncated = (_tag(xml, "IsTruncated") or "").strip() == "true"
    token = _tag(xml, "NextContinuationToken")
    return files, truncated, token


def parse_common_prefixes(xml):
    """The `<CommonPrefixes><Prefix>` values of a delimited listing."""
    return re.findall(r"<CommonPrefixes><Prefix>([^<]*)</Prefix></CommonPrefixes>", xml)


def volume_format(name):
    stem = name[:-3] if name.endswith(".gz") else name
    if "_" not in stem:
        return ""
    suffix = stem.rsplit("_", 1)[1]
    if len(suffix) >= 2 and suffix[0] == "V" and all("0" <= c <= "9" for c in suffix[1:]):
        return suffix
    return ""


def is_volume(name, size):
    return size > 0 and "_MDM" not in name


def volume_start_secs(name):
    """When a volume started, in seconds after its UTC midnight, from its
    file name (`KTBW20241009_000313_V06` -> 193); None when the name has no
    time. Mirrors `volume_start_secs` in the app."""
    if name[12:13] != "_":
        return None
    t = name[13:19]
    if len(t) != 6 or not all("0" <= c <= "9" for c in t):
        return None
    h, m, s = int(t[0:2]), int(t[2:4]), int(t[4:6])
    if h >= 24 or m >= 60 or s >= 60:
        return None
    return h * 3600 + m * 60 + s


def summarize_day(files):
    """`(volumes, mean_bytes, format)` for one site-day's files."""
    return summarize_day_with_scans(files)[0]


def summarize_day_with_scans(files):
    """`((volumes, mean_bytes, format), scans)`: the day summary, and the
    `(start_secs, bytes)` of each volume the app keeps for that day (the
    day's format only, with a start time in its name, sizes capped to u32),
    in time order, as `summarize_day` in the app keeps them."""
    volumes = [(n, s) for n, s in files if is_volume(n, s)]
    counts = {}
    for name, _ in volumes:
        f = volume_format(name)
        counts[f] = counts.get(f, 0) + 1
    # The app takes `max_by_key` over a BTreeMap: on a tie, the greatest
    # format string wins.
    fmt, best = "", -1
    for f in sorted(counts):
        if counts[f] >= best:
            fmt, best = f, counts[f]
    total = sum(s for _, s in volumes)
    mean = total // len(volumes) if volumes else 0
    scans = []
    for name, size in volumes:
        if volume_format(name) != fmt:
            continue
        secs = volume_start_secs(name)
        if secs is not None:
            scans.append((secs, min(size, U32_MAX)))
    scans.sort()
    return (len(volumes), mean, fmt), scans


# ---------------------------------------------------------------------------
# Per-site file
# ---------------------------------------------------------------------------


def runs_of(days):
    """`[[start, count], ...]` runs of consecutive day numbers."""
    out = []
    for d in sorted(days):
        if out and out[-1][0] + out[-1][1] == d:
            out[-1][1] += 1
        else:
            out.append([d, 1])
    return out


class SiteIndex:
    """One site's indexed days: day number -> (volumes, mean_bytes, format),
    and which of them have their volumes in a month file."""

    def __init__(self, site):
        self.site = site
        self.days = {}
        # Days whose volumes are in a published month file.
        self.scanned = set()
        # Days listed this run whose volumes aren't written yet: day ->
        # [(start_secs, bytes)].
        self.new_scans = {}
        self.changed = False

    def covers(self, day):
        return day in self.days

    def record(self, day, summary):
        if self.days.get(day) != summary:
            self.days[day] = summary
            self.changed = True

    def record_scans(self, day, scans):
        self.new_scans[day] = list(scans)

    def to_json(self, built_ms):
        if not self.days:
            first, last = 0, -1
        else:
            first, last = min(self.days), max(self.days)
        vols, means, runs = [], [], []
        current = None
        for i, day in enumerate(range(first, last + 1)):
            entry = self.days.get(day)
            if entry is None:
                vols.append(None)
                means.append(None)
                continue
            v, m, f = entry
            vols.append(v)
            means.append(m if v > 0 else 0)
            if v > 0 and f != current:
                runs.append([i, f])
                current = f
        scans = [[d - first, n] for d, n in runs_of(d for d in self.scanned if d in self.days)]
        return {
            "v": FORMAT_VERSION,
            "site": self.site,
            "built": built_ms,
            "first_day": first,
            "vols": vols,
            "mean": means,
            "fmt": runs,
            "scans": scans,
        }

    @classmethod
    def from_json(cls, site, doc):
        """Read a file written by `to_json`; anything malformed raises
        ValueError (the caller then starts that site over). A file from
        before month files has no `scans`: none of its days have them."""
        out = cls(site)
        if doc.get("v") != FORMAT_VERSION or doc.get("site") != site:
            raise ValueError("version or site mismatch")
        first = doc["first_day"]
        vols, means, runs = doc["vols"], doc["mean"], doc["fmt"]
        if not isinstance(first, int) or len(vols) != len(means):
            raise ValueError("bad shape")
        runs = sorted((int(i), str(f)) for i, f in runs)
        r, fmt = 0, ""
        for i, (v, m) in enumerate(zip(vols, means)):
            while r < len(runs) and runs[r][0] <= i:
                fmt = runs[r][1]
                r += 1
            if v is None:
                continue
            if not isinstance(v, int) or not isinstance(m, int) or v < 0 or m < 0:
                raise ValueError("bad day %d" % i)
            out.days[first + i] = (v, m if v > 0 else 0, fmt if v > 0 else "")
        for start, count in doc.get("scans", []):
            for d in range(first + int(start), first + int(start) + int(count)):
                if d in out.days:
                    out.scanned.add(d)
        return out


# ---------------------------------------------------------------------------
# Month files
# ---------------------------------------------------------------------------


def month_of(day):
    """`(year, month)` of a day number."""
    d = day_date(day)
    return d.year, d.month


def month_days(month):
    """The day numbers of `(year, month)`, in order."""
    y, m = month
    first = day_number(dt.date(y, m, 1))
    nxt = day_number(dt.date(y + (m == 12), m % 12 + 1, 1))
    return range(first, nxt)


def month_name(month):
    return "%04d-%02d" % month


def month_path(out_dir, site, month):
    return os.path.join(out_dir, site, month_name(month) + ".json")


def oldest_month_kept(last_day, keep):
    """The oldest month whose file is kept when the newest `keep` months are
    (counting the month of `last_day`); None keeps every month."""
    if not keep:
        return None
    y, m = month_of(last_day)
    n = y * 12 + (m - 1) - (keep - 1)
    return n // 12, n % 12 + 1


class MonthFile:
    """One site-month's volumes: day number -> (volumes, mean_bytes, format,
    [(start_secs, bytes)])."""

    def __init__(self, site, month):
        self.site = site
        self.month = month
        self.days = {}

    def complete(self):
        return all(d in self.days for d in month_days(self.month))

    def to_json(self, built_ms):
        days = []
        for d in month_days(self.month):
            entry = self.days.get(d)
            if entry is None:
                days.append(None)
                continue
            v, m, f, scans = entry
            deltas, prev = [], 0
            for secs, _ in scans:
                deltas.append(secs - prev)
                prev = secs
            days.append([v, m if v > 0 else 0, f if v > 0 else "", deltas, [b for _, b in scans]])
        return {
            "v": MONTH_FORMAT_VERSION,
            "site": self.site,
            "month": month_name(self.month),
            "built": built_ms,
            "complete": self.complete(),
            "days": days,
        }

    @classmethod
    def from_json(cls, site, month, doc):
        out = cls(site, month)
        if (doc.get("v") != MONTH_FORMAT_VERSION or doc.get("site") != site
                or doc.get("month") != month_name(month)):
            raise ValueError("version, site or month mismatch")
        span = month_days(month)
        entries = doc["days"]
        if len(entries) != len(span):
            raise ValueError("bad length")
        for d, entry in zip(span, entries):
            if entry is None:
                continue
            v, m, f, deltas, sizes = entry
            if len(deltas) != len(sizes) or len(deltas) > v:
                raise ValueError("bad day %s" % day_date(d))
            secs, scans = 0, []
            for dt_, b in zip(deltas, sizes):
                secs += int(dt_)
                scans.append((secs, int(b)))
            out.days[d] = (int(v), int(m), str(f), scans)
        return out


def load_month(out_dir, site, month):
    """The month's file, or an empty one when it's missing or unreadable
    (its days then count as without month data)."""
    path = month_path(out_dir, site, month)
    if not os.path.exists(path):
        return MonthFile(site, month)
    try:
        with open(path, encoding="utf-8") as f:
            return MonthFile.from_json(site, month, json.load(f))
    except (ValueError, KeyError, TypeError) as e:
        print("warning: %s unreadable (%s); rewriting it" % (path, e), file=sys.stderr)
        return MonthFile(site, month)


def month_files_on_disk(out_dir, site):
    """The months `site` has a file for."""
    out = set()
    try:
        names = os.listdir(os.path.join(out_dir, site))
    except FileNotFoundError:
        return out
    for name in names:
        m = re.fullmatch(r"(\d{4})-(\d{2})\.json", name)
        if m:
            out.add((int(m.group(1)), int(m.group(2))))
    return out


def write_months(out_dir, idx, built_ms, oldest, dry_run=False):
    """Merge `idx`'s newly listed volumes into its month files (months from
    `oldest` on; None for all), mark those days as having month data, and
    return how many files were written."""
    by_month = {}
    for day, scans in idx.new_scans.items():
        month = month_of(day)
        if oldest is not None and month < oldest:
            continue
        if day in idx.days:
            by_month.setdefault(month, []).append((day, scans))
    idx.new_scans = {}
    written = 0
    for month, days in sorted(by_month.items()):
        mf = load_month(out_dir, idx.site, month)
        # Whatever the file no longer holds isn't month data any more.
        for d in month_days(month):
            if d not in mf.days:
                idx.scanned.discard(d)
        for day, scans in days:
            v, m, f = idx.days[day]
            mf.days[day] = (v, m, f, scans)
            idx.scanned.add(day)
        idx.changed = True
        if not dry_run:
            os.makedirs(os.path.join(out_dir, idx.site), exist_ok=True)
            write_json(month_path(out_dir, idx.site, month), mf.to_json(built_ms))
        written += 1
    return written


def prune_months(out_dir, idx, oldest, dry_run=False):
    """Drop `idx`'s month files older than `oldest`, and their days' marks."""
    if oldest is None:
        return 0
    removed = 0
    for month in sorted(month_files_on_disk(out_dir, idx.site)):
        if month < oldest:
            if not dry_run:
                os.remove(month_path(out_dir, idx.site, month))
            removed += 1
    stale = {d for d in idx.scanned if month_of(d) < oldest}
    if stale:
        idx.scanned -= stale
        idx.changed = True
    return removed


def read_sites(sites_rs):
    with open(sites_rs, encoding="utf-8") as f:
        text = f.read()
    sites = re.findall(r'\bid:\s*"([A-Z0-9]{4})"', text)
    seen, out = set(), []
    for s in sites:
        if s not in seen:
            seen.add(s)
            out.append(s)
    return out


def load_index(out_dir, site):
    path = os.path.join(out_dir, site + ".json")
    if not os.path.exists(path):
        return SiteIndex(site)
    try:
        with open(path, encoding="utf-8") as f:
            index = SiteIndex.from_json(site, json.load(f))
    except (ValueError, KeyError, TypeError) as e:
        print("warning: %s unreadable (%s); rebuilding it" % (path, e), file=sys.stderr)
        index = SiteIndex(site)
        index.changed = True
        return index
    # A day marked as having month data whose month file is gone (deleted
    # by hand, say) is backfilled again.
    on_disk = month_files_on_disk(out_dir, site)
    gone = {d for d in index.scanned if month_of(d) not in on_disk}
    if gone:
        index.scanned -= gone
        index.changed = True
    return index


def write_json(path, doc):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(doc, f, separators=(",", ":"))
        f.write("\n")
    os.replace(tmp, path)


# ---------------------------------------------------------------------------
# Requests
# ---------------------------------------------------------------------------


class BudgetExhausted(Exception):
    pass


class Lister:
    """Bounded, budgeted, retrying GETs against the archive bucket."""

    def __init__(self, budget, fetch=None):
        self.budget = budget
        self.used = 0
        self.retries = 0
        self.lock = threading.Lock()
        self.pause_until = 0.0
        self.fetch = fetch or self._urlopen

    @staticmethod
    def _urlopen(url):
        req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return resp.status, resp.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode("utf-8", "replace")

    def remaining(self):
        with self.lock:
            return self.budget - self.used

    def _take(self):
        with self.lock:
            if self.used >= self.budget:
                raise BudgetExhausted()
            self.used += 1

    def get(self, url):
        """The body of a 200, retrying 5xx / 429 / SlowDown / network errors
        with exponential backoff. Raises on anything else, or when the
        retries run out."""
        for attempt in range(MAX_ATTEMPTS):
            wait = self.pause_until - time.monotonic()
            if wait > 0:
                time.sleep(wait)
            self._take()
            try:
                status, body = self.fetch(url)
            except (urllib.error.URLError, OSError, TimeoutError) as e:
                status, body = 0, str(e)
            if status == 200:
                return body
            retryable = status in (0, 408, 429) or status >= 500 or "SlowDown" in body
            if not retryable or attempt == MAX_ATTEMPTS - 1:
                raise RuntimeError("HTTP %s for %s" % (status, url))
            with self.lock:
                self.retries += 1
            delay = min(30.0, 0.5 * 2**attempt) * (0.5 + random.random())
            if status in (429, 503) or "SlowDown" in body:
                # Everyone backs off, not just this request.
                with self.lock:
                    self.pause_until = max(self.pause_until, time.monotonic() + delay)
            time.sleep(delay)
        raise RuntimeError("unreachable")

    def list_all(self, query):
        """Every page of a ListObjectsV2 query, concatenated."""
        bodies, token = [], None
        while True:
            q = dict(query, **({"continuation-token": token} if token else {}))
            body = self.get(BUCKET_URL + "/?" + urllib.parse.urlencode(q, safe="/"))
            bodies.append(body)
            _, truncated, token = parse_s3_listing(body)
            if not truncated or not token:
                return bodies


def day_prefix(day):
    return day_date(day).strftime("%Y/%m/%d")


def sites_present(lister, day):
    """Which sites have anything under the day's prefix, from one delimited
    listing (a site that isn't there has no data that day: an outage, or the
    radar wasn't built yet)."""
    present = set()
    for body in lister.list_all({"list-type": "2", "prefix": day_prefix(day) + "/", "delimiter": "/"}):
        for p in parse_common_prefixes(body):
            seg = p.rstrip("/").rsplit("/", 1)[-1]
            present.add(seg)
        for name, _ in parse_s3_listing(body)[0]:
            present.add(name)
    return present


def list_site_day(lister, site, day):
    # The same prefix the app lists (no trailing slash).
    files = []
    for body in lister.list_all({"list-type": "2", "prefix": "%s/%s" % (day_prefix(day), site)}):
        files.extend(parse_s3_listing(body)[0])
    return summarize_day_with_scans(files)


# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------


def last_final_day(now):
    """The newest day that ended at least FINAL_AFTER_SECS before `now`."""
    return (int(now) - FINAL_AFTER_SECS) // DAY_SECS - 1


def plan_days(indexes, first_day, last_day):
    """`[(day, [sites lacking it])]`, newest day first, for every day some
    site's index doesn't cover."""
    out = []
    for day in range(last_day, first_day - 1, -1):
        lacking = [s for s, idx in indexes.items() if not idx.covers(day)]
        if lacking:
            out.append((day, lacking))
    return out


def plan_backfill(indexes, first_day, last_day):
    """`[(day, [sites])]`, newest day first: indexed days with volumes that
    have no month data yet. (Indexed days without volumes need no listing;
    `mark_empty_days` records them.)"""
    out = []
    for day in range(last_day, first_day - 1, -1):
        sites = [s for s, idx in indexes.items()
                 if idx.days.get(day, (0,))[0] > 0 and day not in idx.scanned
                 and day not in idx.new_scans]
        if sites:
            out.append((day, sites))
    return out


def mark_empty_days(indexes, first_day, last_day):
    """Give every indexed day without volumes (an outage, or before the
    radar existed) its month data, which is just "nothing", for free."""
    for idx in indexes.values():
        for day, (v, _, _) in idx.days.items():
            if first_day <= day <= last_day and v == 0 and day not in idx.scanned:
                idx.record_scans(day, [])


def run(args, lister=None, now=None, log=print):
    now = time.time() if now is None else now
    started = time.monotonic()
    deadline = started + args.max_minutes * 60
    os.makedirs(args.out, exist_ok=True)

    sites = read_sites(args.sites_file)
    if args.sites:
        wanted = set(args.sites.split(","))
        sites = [s for s in sites if s in wanted]
    if not sites:
        raise SystemExit("no sites")
    indexes = {s: load_index(args.out, s) for s in sites}

    first_day = day_number(dt.date.fromisoformat(args.since)) if args.since else day_number(ARCHIVE_START)
    newest_final = last_final_day(now)
    last_day = newest_final
    if args.until:
        last_day = min(last_day, day_number(dt.date.fromisoformat(args.until)))
    # Month files cover the newest `month_files` months (by the newest final
    # day, whatever --until says), so a run never writes one it would prune.
    oldest = oldest_month_kept(newest_final, args.month_files)
    month_first = first_day if oldest is None else max(first_day, month_days(oldest)[0])
    todo = plan_days(indexes, first_day, last_day)
    backfill = []
    if args.backfill_months:
        mark_empty_days(indexes, month_first, last_day)
        backfill = plan_backfill(indexes, month_first, last_day)
    log("%d sites, %d days with gaps (%s .. %s), %d site-days to backfill month files for "
        "(from %s), budget %d"
        % (len(sites), len(todo), day_date(first_day), day_date(last_day),
           sum(len(l) for _, l in backfill),
           month_name(oldest) if oldest else "the archive start", args.budget))

    lister = lister or Lister(args.budget)
    built_ms = int(now * 1000)
    counts = {"listed": 0, "failed": 0, "months": 0}
    last_checkpoint = [time.monotonic()]

    def record(site, day, result):
        summary, scans = result
        indexes[site].record(day, summary)
        if day >= month_first:
            indexes[site].record_scans(day, scans)

    def save(final):
        written = 0
        for idx in indexes.values():
            counts["months"] += write_months(args.out, idx, built_ms, oldest, args.dry_run)
            if final:
                prune_months(args.out, idx, oldest, args.dry_run)
            if idx.changed and not args.dry_run:
                write_json(os.path.join(args.out, idx.site + ".json"), idx.to_json(built_ms))
                idx.changed = False
                written += 1
        if final and not args.dry_run:
            write_status(args.out, indexes, first_day, last_day, built_ms, oldest)
        return written

    def list_jobs(pool, jobs):
        """List each `(site, day)`; False once the budget ran out."""
        futures = {pool.submit(list_site_day, lister, s, d): (s, d) for s, d in jobs}
        exhausted = False
        for fut in concurrent.futures.as_completed(futures):
            site, day = futures[fut]
            try:
                record(site, day, fut.result())
                counts["listed"] += 1
            except BudgetExhausted:
                exhausted = True
            except Exception as e:  # noqa: BLE001 - left as a gap, retried next run
                counts["failed"] += 1
                log("%s %s failed: %s" % (site, day_date(day), e))
        if time.monotonic() - last_checkpoint[0] > CHECKPOINT_SECS:
            save(False)
            last_checkpoint[0] = time.monotonic()
            log("checkpoint: %d site-days listed, %d requests"
                % (counts["listed"], lister.used))
        return not exhausted

    stopped = False
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        # New days first: one presence listing per day, then each site with
        # data that day.
        for b in range(0, len(todo), DAYS_PER_BATCH):
            if time.monotonic() > deadline:
                log("time limit reached")
                stopped = True
                break
            batch = todo[b:b + DAYS_PER_BATCH]
            # Leave room for at least a presence listing per day.
            if lister.remaining() < len(batch) + 1:
                log("budget reached")
                stopped = True
                break
            presence = {}
            futures = {pool.submit(sites_present, lister, day): day for day, _ in batch}
            for fut in concurrent.futures.as_completed(futures):
                try:
                    presence[futures[fut]] = fut.result()
                except BudgetExhausted:
                    pass
                except Exception as e:  # noqa: BLE001 - logged, retried next run
                    counts["failed"] += 1
                    log("presence %s failed: %s" % (day_date(futures[fut]), e))

            jobs = []
            for day, lacking in batch:
                present = presence.get(day)
                if present is None:
                    continue
                for site in lacking:
                    if any(p.startswith(site) for p in present):
                        jobs.append((site, day))
                    else:
                        record(site, day, ((0, 0, ""), []))
            if not list_jobs(pool, jobs):
                log("budget reached")
                stopped = True
                break

        # Then, when asked, the indexed days without month data, newest
        # first: their sites are known to have data, so no presence listing.
        for b in range(0, 0 if stopped else len(backfill), DAYS_PER_BATCH):
            if time.monotonic() > deadline:
                log("time limit reached")
                break
            if lister.remaining() < 1:
                log("budget reached")
                break
            jobs = [(s, d) for d, sites_ in backfill[b:b + DAYS_PER_BATCH] for s in sites_]
            if not list_jobs(pool, jobs):
                log("budget reached")
                break

    written = save(True)
    remaining = sum(len(l) for _, l in plan_days(indexes, first_day, last_day))
    unscanned = sum(len(l) for _, l in plan_backfill(indexes, month_first, last_day))
    log("listed %d site-days, %d failed, %d requests (%d retries), %d files and %d month files "
        "written, %d site-days still to index, %d without month files, %.0fs"
        % (counts["listed"], counts["failed"], lister.used, lister.retries, written,
           counts["months"], remaining, unscanned, time.monotonic() - started))
    return remaining


def write_status(out_dir, indexes, first_day, last_day, built_ms, oldest=None):
    """status.json: what the index covers, for people and the workflow log
    (the app doesn't read it)."""
    sites = {}
    gaps = 0
    for site, idx in sorted(indexes.items()):
        missing = sum(1 for d in range(first_day, last_day + 1) if d not in idx.days)
        gaps += missing
        covered = [d for d in idx.days if first_day <= d <= last_day]
        sites[site] = {
            "first": str(day_date(min(covered))) if covered else None,
            "last": str(day_date(max(covered))) if covered else None,
            "missing_days": missing,
        }
    month_first = first_day if oldest is None else max(first_day, month_days(oldest)[0])
    unscanned = sum(len(l) for _, l in plan_backfill(indexes, month_first, last_day))
    write_json(os.path.join(out_dir, "status.json"), {
        "v": FORMAT_VERSION,
        "built": built_ms,
        "through": str(day_date(last_day)),
        "complete": gaps == 0,
        "missing_site_days": gaps,
        "month_files": {
            "from": month_name(oldest) if oldest else None,
            "complete": unscanned == 0,
            "missing_site_days": unscanned,
        },
        "sites": sites,
    })


def main(argv=None):
    here = os.path.dirname(os.path.abspath(__file__))
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--out", required=True, help="directory of per-site files (read and updated)")
    p.add_argument("--sites-file", default=os.path.join(here, "..", "..", "src", "data", "sites.rs"))
    p.add_argument("--sites", help="comma-separated site ids (default: every site in sites.rs)")
    p.add_argument("--since", help="oldest day to index, YYYY-MM-DD (default: archive start)")
    p.add_argument("--until", help="newest day to index, YYYY-MM-DD (default: newest final day)")
    p.add_argument("--budget", type=int, default=int(os.environ.get("INDEX_BUDGET", 100_000)),
                   help="most HTTP requests this run (default 100000)")
    p.add_argument("--concurrency", type=int, default=24)
    p.add_argument("--max-minutes", type=float, default=40.0,
                   help="stop starting new days after this long (default 40)")
    p.add_argument("--backfill-months", action="store_true",
                   help="after the new days, re-list indexed days that have no month file "
                        "data yet, newest first, within the budget")
    p.add_argument("--month-files", type=int, default=DEFAULT_MONTH_FILES,
                   help="keep month files for the newest N months, pruning older ones "
                        "(default %d; 0 keeps every month)" % DEFAULT_MONTH_FILES)
    p.add_argument("--dry-run", action="store_true", help="list but write nothing")
    run(p.parse_args(argv))


if __name__ == "__main__":
    main()
