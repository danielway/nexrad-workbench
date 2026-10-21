#!/usr/bin/env python3
"""Tests for build_index.py, offline (a fake bucket stands in for S3):

    python3 tools/activity_index/test_build_index.py
"""

import argparse
import json
import os
import sys
import tempfile
import unittest
import urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import build_index as bi  # noqa: E402

OCT_9_2024 = bi.day_number(bi.dt.date(2024, 10, 9))


def listing(keys, truncated=False, token=None, prefixes=()):
    parts = ['<?xml version="1.0" encoding="UTF-8"?><ListBucketResult>']
    parts.append("<IsTruncated>%s</IsTruncated>" % ("true" if truncated else "false"))
    for key, size in keys:
        parts.append("<Contents><Key>%s</Key><Size>%s</Size></Contents>" % (key, size))
    for p in prefixes:
        parts.append("<CommonPrefixes><Prefix>%s</Prefix></CommonPrefixes>" % p)
    if token:
        parts.append("<NextContinuationToken>%s</NextContinuationToken>" % token)
    parts.append("</ListBucketResult>")
    return "".join(parts)


class FakeBucket:
    """Objects by key; answers delimited and plain prefix listings, a page of
    `page` keys at a time."""

    def __init__(self, objects, page=1000, fail=()):
        self.objects = dict(objects)
        self.page = page
        self.fail = dict(fail)  # prefix -> statuses to return first
        self.calls = []

    def __call__(self, url):
        q = dict(urllib.parse.parse_qsl(urllib.parse.urlsplit(url).query))
        prefix = q["prefix"]
        self.calls.append(prefix)
        if self.fail.get(prefix):
            status = self.fail[prefix].pop(0)
            code = "SlowDown" if status == 503 else "AccessDenied"
            return status, "<Error><Code>%s</Code></Error>" % code
        keys = sorted(k for k in self.objects if k.startswith(prefix))
        if "delimiter" in q:
            prefixes = sorted({prefix + k[len(prefix):].split("/", 1)[0] + "/" for k in keys})
            return 200, listing([], prefixes=prefixes)
        start = int(q.get("continuation-token", 0))
        chunk = keys[start:start + self.page]
        more = start + self.page < len(keys)
        return 200, listing([(k, self.objects[k]) for k in chunk], truncated=more,
                            token=str(start + self.page) if more else None)


def args(out, sites_file, **kw):
    a = argparse.Namespace(out=out, sites_file=sites_file, sites=None, since=None, until=None,
                           budget=1000, concurrency=4, max_minutes=5.0, dry_run=False,
                           backfill_months=False, month_files=bi.DEFAULT_MONTH_FILES)
    for k, v in kw.items():
        setattr(a, k, v)
    return a


class ParseAndSummarize(unittest.TestCase):
    def test_parses_like_the_app(self):
        xml = listing([
            ("2024/10/09/KTBW/KTBW20241009_000313_V06", "15194370"),
            ("2024/10/09/KTBW/KTBW20241009_000830_V06_MDM", "4096"),
            ("2024/10/09/KTBW/bad", "x"),
        ])
        files, truncated, token = bi.parse_s3_listing(xml)
        self.assertEqual(files, [("KTBW20241009_000313_V06", 15194370),
                                 ("KTBW20241009_000830_V06_MDM", 4096)])
        self.assertFalse(truncated)
        self.assertIsNone(token)
        self.assertTrue(bi.parse_s3_listing("<IsTruncated>true</IsTruncated>")[1])
        self.assertEqual(bi.parse_s3_listing("")[0], [])

    def test_formats(self):
        self.assertEqual(bi.volume_format("KTBW20241009_000313_V06"), "V06")
        self.assertEqual(bi.volume_format("KTBW20100615_000236_V03.gz"), "V03")
        self.assertEqual(bi.volume_format("KTBW19950615_000156.gz"), "")
        self.assertEqual(bi.volume_format("KTBW20241009_000830_V06_MDM"), "")

    def test_summarizes_like_the_app(self):
        files = [("KTBW20241009_000313_V06", 20_000_000), ("KTBW20241009_000830_V06", 24_000_000),
                 ("KTBW20241009_000830_V06_MDM", 4096), ("KTBW20241009_001000_V06", 0)]
        self.assertEqual(bi.summarize_day(files), (2, 22_000_000, "V06"))
        self.assertEqual(bi.summarize_day([]), (0, 0, ""))
        # A tie between formats goes to the greater one, as BTreeMap
        # max_by_key does.
        tie = [("A_000000_V03", 1), ("A_000100_V06", 2)]
        self.assertEqual(bi.summarize_day(tie)[2], "V06")
        # Integer mean.
        self.assertEqual(bi.summarize_day([("a", 1), ("b", 2)])[1], 1)

    def test_start_times_like_the_app(self):
        self.assertEqual(bi.volume_start_secs("KTBW20241009_000313_V06"), 193)
        self.assertEqual(bi.volume_start_secs("KTBW20241009_235959_V06"), 86_399)
        self.assertIsNone(bi.volume_start_secs("KTBW20241009_246000_V06"))
        self.assertIsNone(bi.volume_start_secs("KTBW20241009-000313_V06"))
        self.assertIsNone(bi.volume_start_secs("KTBW_000313_V06"))

    def test_keeps_the_days_format_volumes_in_time_order(self):
        files = [("KTBW20241009_001000_V06", 30), ("KTBW20241009_000313_V06", 20),
                 ("KTBW20241009_000500_V03", 99), ("KTBW20241009_000830_V06_MDM", 4096),
                 ("KTBW20241009_000900_V06", 0), ("KTBW20241009_xx_V06", 10),
                 ("KTBW20241009_002000_V06", 2**40)]
        summary, scans = bi.summarize_day_with_scans(files)
        # Every volume counts toward the summary (the V03 one, the one
        # without a time, the huge one); only the day's format with a start
        # time is kept per volume, its size capped like the app's u32.
        self.assertEqual(summary[0], 5)
        self.assertEqual(summary[2], "V06")
        self.assertEqual(scans, [(193, 20), (600, 30), (1200, 2**32 - 1)])


class FileFormat(unittest.TestCase):
    def test_round_trips_with_gaps_and_format_runs(self):
        idx = bi.SiteIndex("KTBW")
        idx.record(100, (200, 3_000_000, ""))
        idx.record(101, (0, 0, ""))
        idx.record(103, (210, 4_000_000, "V03"))
        idx.record(104, (190, 9_000_000, "V06"))
        doc = idx.to_json(5)
        self.assertEqual(doc["first_day"], 100)
        self.assertEqual(doc["vols"], [200, 0, None, 210, 190])
        self.assertEqual(doc["mean"], [3_000_000, 0, None, 4_000_000, 9_000_000])
        self.assertEqual(doc["fmt"], [[0, ""], [3, "V03"], [4, "V06"]])
        back = bi.SiteIndex.from_json("KTBW", json.loads(json.dumps(doc)))
        self.assertEqual(back.days, idx.days)

    def test_an_empty_index(self):
        doc = bi.SiteIndex("KTBW").to_json(5)
        self.assertEqual((doc["vols"], doc["mean"], doc["fmt"]), ([], [], []))
        self.assertEqual(bi.SiteIndex.from_json("KTBW", doc).days, {})

    def test_rejects_another_version_or_site(self):
        doc = bi.SiteIndex("KTBW").to_json(5)
        with self.assertRaises(ValueError):
            bi.SiteIndex.from_json("KMLB", doc)
        with self.assertRaises(ValueError):
            bi.SiteIndex.from_json("KTBW", dict(doc, v=2))

    def test_reads_the_site_list(self):
        here = os.path.dirname(os.path.abspath(__file__))
        sites = bi.read_sites(os.path.join(here, "..", "..", "src", "data", "sites.rs"))
        self.assertIn("KTBW", sites)
        self.assertGreater(len(sites), 150)
        self.assertEqual(len(sites), len(set(sites)))


class MonthFiles(unittest.TestCase):
    def test_months_and_their_days(self):
        self.assertEqual(bi.month_of(OCT_9_2024), (2024, 10))
        days = bi.month_days((2024, 2))
        self.assertEqual(len(days), 29)
        self.assertEqual(bi.day_date(days[0]), bi.dt.date(2024, 2, 1))
        self.assertEqual(len(bi.month_days((2024, 12))), 31)
        self.assertEqual(bi.month_name((2024, 2)), "2024-02")
        self.assertEqual(bi.oldest_month_kept(OCT_9_2024, 12), (2023, 11))
        self.assertEqual(bi.oldest_month_kept(OCT_9_2024, 1), (2024, 10))
        self.assertEqual(bi.oldest_month_kept(OCT_9_2024, 10), (2024, 1))
        self.assertIsNone(bi.oldest_month_kept(OCT_9_2024, 0))

    def test_round_trips_with_time_deltas(self):
        mf = bi.MonthFile("KTBW", (2024, 10))
        first = bi.month_days((2024, 10))[0]
        mf.days[first] = (3, 21, "V06", [(193, 20), (600, 30)])
        mf.days[first + 2] = (0, 0, "", [])
        doc = mf.to_json(5)
        self.assertEqual(doc["month"], "2024-10")
        self.assertFalse(doc["complete"])
        self.assertEqual(len(doc["days"]), 31)
        self.assertEqual(doc["days"][0], [3, 21, "V06", [193, 407], [20, 30]])
        self.assertIsNone(doc["days"][1])
        self.assertEqual(doc["days"][2], [0, 0, "", [], []])
        back = bi.MonthFile.from_json("KTBW", (2024, 10), json.loads(json.dumps(doc)))
        self.assertEqual(back.days, mf.days)
        for d in bi.month_days((2024, 10)):
            mf.days.setdefault(d, (0, 0, "", []))
        self.assertTrue(mf.to_json(5)["complete"])
        with self.assertRaises(ValueError):
            bi.MonthFile.from_json("KTBW", (2024, 11), doc)
        with self.assertRaises(ValueError):
            bi.MonthFile.from_json("KTBW", (2024, 10), dict(doc, v=2))

    def test_day_file_scans_runs_round_trip(self):
        idx = bi.SiteIndex("KTBW")
        for d in (100, 101, 102, 104):
            idx.record(d, (1, 1, "V06"))
        idx.scanned = {100, 101, 104}
        doc = idx.to_json(5)
        self.assertEqual(doc["scans"], [[0, 2], [4, 1]])
        self.assertEqual(bi.SiteIndex.from_json("KTBW", doc).scanned, {100, 101, 104})
        # A file from before month files reads as having none.
        del doc["scans"]
        self.assertEqual(bi.SiteIndex.from_json("KTBW", doc).scanned, set())


class Runs(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.out = os.path.join(self.tmp.name, "idx")
        self.sites_file = os.path.join(self.tmp.name, "sites.rs")
        with open(self.sites_file, "w") as f:
            f.write('NexradSite { id: "KTBW", }\nNexradSite { id: "KMLB", }\n')
        d = lambda n: bi.day_prefix(OCT_9_2024 - n)  # noqa: E731
        self.bucket = FakeBucket({
            d(0) + "/KTBW/KTBW_000313_V06": 20_000_000,
            d(0) + "/KTBW/KTBW_000830_V06": 24_000_000,
            d(0) + "/KMLB/KMLB_000100_V06": 5_000_000,
            d(1) + "/KTBW/KTBW_000313_V06": 4_000_000,
            # KMLB has nothing on d(1): an outage, recorded without listing.
            d(2) + "/KTBW/KTBW_000313_V06": 3_000_000,
            d(2) + "/KMLB/KMLB_000100_V06": 3_000_000,
        }, page=1)
        # "now" is a day after OCT_9_2024 ended, so it is final.
        self.now = (OCT_9_2024 + 2) * bi.DAY_SECS

    def tearDown(self):
        self.tmp.cleanup()

    def load(self, site):
        with open(os.path.join(self.out, site + ".json")) as f:
            return json.load(f)

    def test_backfills_newest_first_within_a_budget_and_resumes(self):
        since = str(bi.day_date(OCT_9_2024 - 2))
        a = args(self.out, self.sites_file, since=since, budget=7, concurrency=1)
        # Budget 7: presence for the 3 days (3), KTBW day 0 (2 pages), KMLB
        # day 0 (1), KTBW day 1 (1); day 2's listings run out.
        remaining = bi.run(a, lister=bi.Lister(7, fetch=self.bucket), now=self.now, log=lambda *_: None)
        self.assertGreater(remaining, 0)
        ktbw = self.load("KTBW")
        self.assertEqual(ktbw["vols"][-1], 2)
        self.assertEqual(ktbw["mean"][-1], 22_000_000)
        self.assertEqual(self.bucket.calls[0], bi.day_prefix(OCT_9_2024) + "/")

        # The next run finishes without re-listing what's there.
        self.bucket.calls.clear()
        a.budget = 1000
        remaining = bi.run(a, lister=bi.Lister(1000, fetch=self.bucket), now=self.now, log=lambda *_: None)
        self.assertEqual(remaining, 0)
        self.assertNotIn(bi.day_prefix(OCT_9_2024) + "/KTBW", self.bucket.calls)
        ktbw, kmlb = self.load("KTBW"), self.load("KMLB")
        self.assertEqual(ktbw["first_day"], OCT_9_2024 - 2)
        self.assertEqual(ktbw["vols"], [1, 1, 2])
        self.assertEqual(kmlb["vols"], [1, 0, 1])
        self.assertEqual(kmlb["mean"], [3_000_000, 0, 5_000_000])
        self.assertNotIn(bi.day_prefix(OCT_9_2024 - 1) + "/KMLB", self.bucket.calls)
        with open(os.path.join(self.out, "status.json")) as f:
            self.assertTrue(json.load(f)["complete"])

        # Nothing left: a run makes no requests.
        self.bucket.calls.clear()
        bi.run(a, lister=bi.Lister(1000, fetch=self.bucket), now=self.now, log=lambda *_: None)
        self.assertEqual(self.bucket.calls, [])

    def test_leaves_unfinished_days_to_the_app(self):
        # Two hours after OCT_9_2024 ended: not final yet.
        now = (OCT_9_2024 + 1) * bi.DAY_SECS + 2 * 3600
        self.assertEqual(bi.last_final_day(now), OCT_9_2024 - 1)
        now = (OCT_9_2024 + 1) * bi.DAY_SECS + 3 * 3600
        self.assertEqual(bi.last_final_day(now), OCT_9_2024)

    def test_retries_slowdown_and_leaves_failures_as_gaps(self):
        since = str(bi.day_date(OCT_9_2024))
        bi_time_sleep = bi.time.sleep
        bi.time.sleep = lambda s: None
        try:
            self.bucket.fail[bi.day_prefix(OCT_9_2024) + "/KTBW"] = [503, 503]
            self.bucket.fail[bi.day_prefix(OCT_9_2024) + "/KMLB"] = [403]
            a = args(self.out, self.sites_file, since=since)
            remaining = bi.run(a, lister=bi.Lister(1000, fetch=self.bucket), now=self.now,
                               log=lambda *_: None)
        finally:
            bi.time.sleep = bi_time_sleep
        self.assertEqual(self.load("KTBW")["vols"], [2])
        # KMLB's day failed outright: still a gap, for the next run.
        self.assertFalse(os.path.exists(os.path.join(self.out, "KMLB.json")))
        self.assertEqual(remaining, 1)

    def test_new_days_write_month_files(self):
        since = str(bi.day_date(OCT_9_2024 - 2))
        bi.run(args(self.out, self.sites_file, since=since),
               lister=bi.Lister(1000, fetch=self.bucket), now=self.now, log=lambda *_: None)
        ktbw = self.load("KTBW")
        self.assertEqual(ktbw["scans"], [[0, 3]])
        month = self.load_month("KTBW", "2024-10")
        self.assertFalse(month["complete"])
        self.assertEqual(month["days"][8], [2, 22_000_000, "V06", [], []])
        self.assertIsNone(month["days"][9])
        # KMLB's outage day has month data too: nothing.
        self.assertEqual(self.load_month("KMLB", "2024-10")["days"][7], [0, 0, "", [], []])

    def load_month(self, site, month):
        with open(os.path.join(self.out, site, month + ".json")) as f:
            return json.load(f)

    def test_a_corrupt_file_is_rebuilt(self):
        os.makedirs(self.out)
        with open(os.path.join(self.out, "KTBW.json"), "w") as f:
            f.write("{not json")
        since = str(bi.day_date(OCT_9_2024))
        bi.run(args(self.out, self.sites_file, since=since),
               lister=bi.Lister(1000, fetch=self.bucket), now=self.now, log=lambda *_: None)
        self.assertEqual(self.load("KTBW")["vols"], [2])


class Backfill(unittest.TestCase):
    """An index built before month files: its days have no volumes yet."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.out = os.path.join(self.tmp.name, "idx")
        os.makedirs(self.out)
        self.sites_file = os.path.join(self.tmp.name, "sites.rs")
        with open(self.sites_file, "w") as f:
            f.write('NexradSite { id: "KTBW", }\n')
        objects = {}
        idx = bi.SiteIndex("KTBW")
        # Oct 1..9 2024; Oct 5 an outage.
        for n in range(9):
            day = OCT_9_2024 - n
            if n == 4:
                idx.record(day, (0, 0, ""))
                continue
            stamp = bi.day_date(day).strftime("%Y%m%d")
            objects["%s/KTBW/KTBW%s_000313_V06" % (bi.day_prefix(day), stamp)] = 1000 + n
            objects["%s/KTBW/KTBW%s_001000_V06" % (bi.day_prefix(day), stamp)] = 3000 + n
            idx.record(day, (2, 2000 + n, "V06"))
        doc = idx.to_json(1)
        del doc["scans"]
        bi.write_json(os.path.join(self.out, "KTBW.json"), doc)
        self.bucket = FakeBucket(objects)
        self.now = (OCT_9_2024 + 2) * bi.DAY_SECS
        self.since = str(bi.day_date(OCT_9_2024 - 8))

    def tearDown(self):
        self.tmp.cleanup()

    def run_(self, budget=1000, **kw):
        a = args(self.out, self.sites_file, since=self.since, until=str(bi.day_date(OCT_9_2024)),
                 budget=budget, concurrency=1, **kw)
        return bi.run(a, lister=bi.Lister(budget, fetch=self.bucket), now=self.now,
                      log=lambda *_: None)

    def month(self):
        with open(os.path.join(self.out, "KTBW", "2024-10.json")) as f:
            return json.load(f)

    def test_a_nightly_run_never_relists_indexed_days(self):
        self.run_()
        self.assertEqual(self.bucket.calls, [])
        self.assertFalse(os.path.exists(os.path.join(self.out, "KTBW")))

    def test_backfills_newest_first_within_the_budget_and_never_repeats(self):
        self.run_(budget=3, backfill_months=True)
        # Oct 9, 8, 7, newest first; the outage day costs nothing.
        self.assertEqual(self.bucket.calls,
                         [bi.day_prefix(OCT_9_2024 - n) + "/KTBW" for n in range(3)])
        days = self.month()["days"]
        self.assertEqual(days[8], [2, 2000, "V06", [193, 407], [1000, 3000]])
        self.assertEqual(days[4], [0, 0, "", [], []])
        self.assertIsNone(days[5])
        with open(os.path.join(self.out, "KTBW.json")) as f:
            day_file = json.load(f)
        self.assertEqual(day_file["first_day"], OCT_9_2024 - 8)
        self.assertEqual(day_file["scans"], [[4, 1], [6, 3]])

        self.bucket.calls.clear()
        self.run_(backfill_months=True)
        self.assertEqual(len(self.bucket.calls), 5)
        self.assertNotIn(bi.day_prefix(OCT_9_2024) + "/KTBW", self.bucket.calls)
        self.assertEqual(self.month()["days"][0], [2, 2008, "V06", [193, 407], [1008, 3008]])
        with open(os.path.join(self.out, "status.json")) as f:
            self.assertTrue(json.load(f)["month_files"]["complete"])

        self.bucket.calls.clear()
        self.run_(backfill_months=True)
        self.assertEqual(self.bucket.calls, [])

    def test_backfill_stays_inside_the_kept_months(self):
        # Keeping one month from Oct 2024 still covers these days; a "now"
        # in November moves the window past them.
        self.now = (OCT_9_2024 + 30) * bi.DAY_SECS
        self.run_(backfill_months=True, month_files=1)
        self.assertEqual(self.bucket.calls, [])
        self.assertFalse(os.path.exists(os.path.join(self.out, "KTBW", "2024-10.json")))

    def test_old_months_are_pruned(self):
        self.run_(backfill_months=True)
        self.assertTrue(os.path.exists(os.path.join(self.out, "KTBW", "2024-10.json")))
        self.now = (OCT_9_2024 + 30) * bi.DAY_SECS
        self.bucket.calls.clear()
        self.run_(month_files=1)
        self.assertFalse(os.path.exists(os.path.join(self.out, "KTBW", "2024-10.json")))
        with open(os.path.join(self.out, "KTBW.json")) as f:
            self.assertEqual(json.load(f)["scans"], [])

    def test_a_deleted_month_file_is_backfilled_again(self):
        self.run_(backfill_months=True)
        os.remove(os.path.join(self.out, "KTBW", "2024-10.json"))
        self.bucket.calls.clear()
        self.run_(backfill_months=True)
        self.assertEqual(len(self.bucket.calls), 8)
        self.assertEqual(self.month()["days"][8][4], [1000, 3000])


if __name__ == "__main__":
    unittest.main()
