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


def summarize_day(files):
    """`(volumes, mean_bytes, format)` for one site-day's files."""
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
    return len(volumes), mean, fmt


# ---------------------------------------------------------------------------
# Per-site file
# ---------------------------------------------------------------------------


class SiteIndex:
    """One site's indexed days: day number -> (volumes, mean_bytes, format)."""

    def __init__(self, site):
        self.site = site
        self.days = {}
        self.changed = False

    def covers(self, day):
        return day in self.days

    def record(self, day, summary):
        if self.days.get(day) != summary:
            self.days[day] = summary
            self.changed = True

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
        return {
            "v": FORMAT_VERSION,
            "site": self.site,
            "built": built_ms,
            "first_day": first,
            "vols": vols,
            "mean": means,
            "fmt": runs,
        }

    @classmethod
    def from_json(cls, site, doc):
        """Read a file written by `to_json`; anything malformed raises
        ValueError (the caller then starts that site over)."""
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
        return out


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
            return SiteIndex.from_json(site, json.load(f))
    except (ValueError, KeyError, TypeError) as e:
        print("warning: %s unreadable (%s); rebuilding it" % (path, e), file=sys.stderr)
        index = SiteIndex(site)
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
    return summarize_day(files)


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
    last_day = last_final_day(now)
    if args.until:
        last_day = min(last_day, day_number(dt.date.fromisoformat(args.until)))
    todo = plan_days(indexes, first_day, last_day)
    log("%d sites, %d days with gaps (%s .. %s), budget %d"
        % (len(sites), len(todo), day_date(first_day), day_date(last_day), args.budget))

    lister = lister or Lister(args.budget)
    built_ms = int(now * 1000)
    listed = failed = 0
    last_checkpoint = time.monotonic()

    def save(final):
        written = 0
        for idx in indexes.values():
            if idx.changed and not args.dry_run:
                write_json(os.path.join(args.out, idx.site + ".json"), idx.to_json(built_ms))
                idx.changed = False
                written += 1
        if final and not args.dry_run:
            write_status(args.out, indexes, first_day, last_day, built_ms)
        return written

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        for b in range(0, len(todo), DAYS_PER_BATCH):
            if time.monotonic() > deadline:
                log("time limit reached")
                break
            batch = todo[b:b + DAYS_PER_BATCH]
            # Leave room for at least a presence listing per day.
            if lister.remaining() < len(batch) + 1:
                log("budget reached")
                break
            presence = {}
            futures = {pool.submit(sites_present, lister, day): day for day, _ in batch}
            for fut in concurrent.futures.as_completed(futures):
                try:
                    presence[futures[fut]] = fut.result()
                except BudgetExhausted:
                    pass
                except Exception as e:  # noqa: BLE001 - logged, retried next run
                    failed += 1
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
                        indexes[site].record(day, (0, 0, ""))
            futures = {pool.submit(list_site_day, lister, s, d): (s, d) for s, d in jobs}
            exhausted = False
            for fut in concurrent.futures.as_completed(futures):
                site, day = futures[fut]
                try:
                    indexes[site].record(day, fut.result())
                    listed += 1
                except BudgetExhausted:
                    exhausted = True
                except Exception as e:  # noqa: BLE001 - left as a gap, retried next run
                    failed += 1
                    log("%s %s failed: %s" % (site, day_date(day), e))
            if time.monotonic() - last_checkpoint > CHECKPOINT_SECS:
                save(False)
                last_checkpoint = time.monotonic()
                log("checkpoint: %d site-days listed, %d requests" % (listed, lister.used))
            if exhausted:
                log("budget reached")
                break

    written = save(True)
    remaining = sum(len(l) for _, l in plan_days(indexes, first_day, last_day))
    log("listed %d site-days, %d failed, %d requests (%d retries), %d files written, "
        "%d site-days still to index, %.0fs"
        % (listed, failed, lister.used, lister.retries, written, remaining,
           time.monotonic() - started))
    return remaining


def write_status(out_dir, indexes, first_day, last_day, built_ms):
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
    write_json(os.path.join(out_dir, "status.json"), {
        "v": FORMAT_VERSION,
        "built": built_ms,
        "through": str(day_date(last_day)),
        "complete": gaps == 0,
        "missing_site_days": gaps,
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
    p.add_argument("--dry-run", action="store_true", help="list but write nothing")
    run(p.parse_args(argv))


if __name__ == "__main__":
    main()
