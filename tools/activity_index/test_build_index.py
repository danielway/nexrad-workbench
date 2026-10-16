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
                           budget=1000, concurrency=4, max_minutes=5.0, dry_run=False)
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

    def test_a_corrupt_file_is_rebuilt(self):
        os.makedirs(self.out)
        with open(os.path.join(self.out, "KTBW.json"), "w") as f:
            f.write("{not json")
        since = str(bi.day_date(OCT_9_2024))
        bi.run(args(self.out, self.sites_file, since=since),
               lister=bi.Lister(1000, fetch=self.bucket), now=self.now, log=lambda *_: None)
        self.assertEqual(self.load("KTBW")["vols"], [2])


if __name__ == "__main__":
    unittest.main()
