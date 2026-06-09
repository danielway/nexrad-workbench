#!/usr/bin/env python3
"""Generate the bundled NWS zone-geometry asset.

The NWS active-alerts feed issues most watches, advisories, and statements
against UGC *zones* (forecast/county/fire) rather than a drawn polygon, and the
API has no bulk-geometry endpoint — resolving every referenced zone at runtime
would mean ~1k requests for a typical national alert set. Instead we bake the
zone polygons into a single asset, generated here from the official NWS
shapefiles, simplified to keep the download small.

Output: assets/zone_geometry.json — a map of "<type>/<id>" (matching the tail of
an alert's `affectedZones` URL, e.g. "forecast/ALZ019") to a list of polygons,
each polygon a list of rings, each ring a list of [lon, lat] pairs.

Run with no external dependencies:

    python3 tools/build_zone_geometry.py

Re-run when NWS revises the zones (a few times a year); bump the SOURCES dates to
the current files listed at weather.gov/gis/{PublicZones,Counties,FireZones}.
"""

import io
import json
import os
import struct
import urllib.request
import zipfile

BASE = "https://www.weather.gov/source/gis/Shapefiles/"

# Current revision files. Update the dates when NWS publishes a new revision
# (filenames listed at weather.gov/gis/{PublicZones,Counties,FireZones,MarineZones}).
#
# `prefix` is the path segment the alert feed uses in its `affectedZones` URLs,
# which is how we key each zone. Note marine zones are referenced as
# ".../zones/forecast/ANZ531" — under `forecast`, not a marine-specific path —
# so they share that prefix and (because their ids like "ANZ531" never collide
# with land "STATE+Z+zone" ids) coexist with land forecast zones.
#
# `id` selects how the zone id is built from shapefile attributes:
#   stateZ  -> STATE + "Z" + ZONE      (public forecast, fire)
#   stateC  -> STATE + "C" + FIPS[-3:] (county)
#   idfield -> the ID attribute as-is  (marine)
SOURCES = {
    "forecast": {"file": "WSOM/z_16ap26.zip", "prefix": "forecast", "id": "stateZ"},
    "county": {"file": "County/c_16ap26.zip", "prefix": "county", "id": "stateC"},
    "fire": {"file": "WSOM/fz16ap26.zip", "prefix": "fire", "id": "stateZ"},
    "marine": {"file": "WSOM/mz16ap26.zip", "prefix": "forecast", "id": "idfield"},
}

# Douglas-Peucker tolerance in degrees (~0.012° ≈ 1.3 km) and coordinate
# rounding. Generous enough to shrink the asset substantially while keeping
# zone outlines visually faithful at the zoom levels alerts are viewed.
SIMPLIFY_EPSILON = float(os.environ.get("ZONE_EPS", "0.02"))
COORD_DECIMALS = int(os.environ.get("ZONE_DEC", "3"))
# Drop rings whose bbox is smaller than this (degrees) — tiny offshore islands
# and slivers that add points but are invisible at alert-viewing zoom.
MIN_RING_EXTENT = float(os.environ.get("ZONE_MINEXT", "0.02"))

OUT_PATH = os.path.join(os.path.dirname(__file__), "..", "assets", "zone_geometry.json")


def fetch(url):
    # Cache the (large, rarely-changing) source zips under /tmp so re-runs and
    # tolerance tuning don't re-download tens of MB each time.
    cache = os.path.join("/tmp", "nexrad_zones_" + url.rsplit("/", 1)[-1])
    if os.path.exists(cache):
        with open(cache, "rb") as f:
            return f.read()
    req = urllib.request.Request(url, headers={"User-Agent": "nexrad-workbench-build"})
    data = urllib.request.urlopen(req, timeout=180).read()
    with open(cache, "wb") as f:
        f.write(data)
    return data


def dbf_records(dbf):
    """Yield each .dbf record as a dict of field -> stripped string."""
    _, hdrsize, recsize = struct.unpack_from("<IHH", dbf, 4)
    fields = []
    off = 32
    while dbf[off] != 0x0D:
        name = dbf[off : off + 11].split(b"\x00")[0].decode("latin1")
        flen = dbf[off + 16]
        fields.append((name, flen))
        off += 32
    numrec = struct.unpack_from("<I", dbf, 4)[0]
    pos = hdrsize
    for _ in range(numrec):
        rec = dbf[pos + 1 : pos + recsize]  # skip the deletion flag byte
        out = {}
        fp = 0
        for name, flen in fields:
            out[name] = rec[fp : fp + flen].decode("latin1").strip()
            fp += flen
        yield out
        pos += recsize


def shp_polygons(shp):
    """Yield, per .shp record, a list of rings (each a list of (lon, lat))."""
    n = len(shp)
    pos = 100  # past the 100-byte header
    while pos < n:
        # 8-byte record header: record number + content length, both big-endian.
        content_len = struct.unpack_from(">I", shp, pos + 4)[0] * 2
        rec_start = pos + 8
        shape_type = struct.unpack_from("<I", shp, rec_start)[0]
        if shape_type == 0:  # null shape
            yield []
            pos = rec_start + content_len
            continue
        # Polygon (5), PolygonZ (15), PolygonM (25) share the same leading layout.
        off = rec_start + 4 + 32  # skip shape type + bbox
        num_parts, num_points = struct.unpack_from("<II", shp, off)
        off += 8
        parts = list(struct.unpack_from("<%dI" % num_parts, shp, off))
        off += 4 * num_parts
        coords = struct.unpack_from("<%dd" % (2 * num_points), shp, off)
        rings = []
        for i, start in enumerate(parts):
            end = parts[i + 1] if i + 1 < num_parts else num_points
            ring = [(coords[2 * j], coords[2 * j + 1]) for j in range(start, end)]
            rings.append(ring)
        yield rings
        pos = rec_start + content_len


def signed_area(ring):
    a = 0.0
    n = len(ring)
    for i in range(n):
        x1, y1 = ring[i]
        x2, y2 = ring[(i + 1) % n]
        a += x1 * y2 - x2 * y1
    return a * 0.5


def dp_simplify(ring, eps):
    """Douglas-Peucker on a closed ring; preserves closure and >=4 points."""
    if len(ring) <= 4:
        return ring
    pts = ring
    keep = [False] * len(pts)
    keep[0] = keep[-1] = True
    stack = [(0, len(pts) - 1)]
    while stack:
        lo, hi = stack.pop()
        ax, ay = pts[lo]
        bx, by = pts[hi]
        dx, dy = bx - ax, by - ay
        seg2 = dx * dx + dy * dy
        idx, dmax = -1, eps
        for i in range(lo + 1, hi):
            px, py = pts[i]
            if seg2 == 0.0:
                d = ((px - ax) ** 2 + (py - ay) ** 2) ** 0.5
            else:
                t = ((px - ax) * dx + (py - ay) * dy) / seg2
                t = max(0.0, min(1.0, t))
                cx, cy = ax + t * dx, ay + t * dy
                d = ((px - cx) ** 2 + (py - cy) ** 2) ** 0.5
            if d > dmax:
                idx, dmax = i, d
        if idx != -1:
            keep[idx] = True
            stack.append((lo, idx))
            stack.append((idx, hi))
    out = [pts[i] for i in range(len(pts)) if keep[i]]
    if len(out) >= 4:
        return out
    # Over-collapsed (a big ring reduced below a usable closed loop): decimate
    # the original to a small floor rather than reverting to the full ring.
    step = max(1, len(pts) // 8)
    out = pts[::step]
    if out[-1] != pts[-1]:
        out.append(pts[-1])
    return out


def round_ring(ring):
    return [[round(x, COORD_DECIMALS), round(y, COORD_DECIMALS)] for x, y in ring]


def zone_id(id_rule, rec):
    if id_rule == "stateC":
        return rec["STATE"] + "C" + rec["FIPS"][-3:]
    if id_rule == "idfield":
        return rec["ID"]
    return rec["STATE"] + "Z" + rec["ZONE"]  # stateZ: forecast & fire


def build():
    out = {}
    for kind, cfg in SOURCES.items():
        rel, prefix, id_rule = cfg["file"], cfg["prefix"], cfg["id"]
        print(f"fetching {kind}: {rel}")
        data = fetch(BASE + rel)
        z = zipfile.ZipFile(io.BytesIO(data))
        shp = z.read(next(n for n in z.namelist() if n.lower().endswith(".shp")))
        dbf = z.read(next(n for n in z.namelist() if n.lower().endswith(".dbf")))
        count = 0
        for rec, rings in zip(dbf_records(dbf), shp_polygons(shp)):
            if not rings:
                continue
            # Group rings into polygons: a clockwise (outer) ring starts a new
            # polygon; counter-clockwise (hole) rings attach to the current one.
            polygons = []
            for ring in rings:
                xs = [p[0] for p in ring]
                ys = [p[1] for p in ring]
                if max(xs) - min(xs) < MIN_RING_EXTENT and max(ys) - min(ys) < MIN_RING_EXTENT:
                    continue  # negligible sliver/island
                simp = round_ring(dp_simplify(ring, SIMPLIFY_EPSILON))
                if len(simp) < 4:
                    continue
                if signed_area(ring) < 0 or not polygons:  # outer (CW) or first
                    polygons.append([simp])
                else:  # hole (CCW)
                    polygons[-1].append(simp)
            if polygons:
                out[f"{prefix}/{zone_id(id_rule, rec)}"] = polygons
                count += 1
        print(f"  {count} zones")

    os.makedirs(os.path.dirname(OUT_PATH), exist_ok=True)
    with open(OUT_PATH, "w") as f:
        json.dump(out, f, separators=(",", ":"))
    size = os.path.getsize(OUT_PATH)
    print(f"wrote {OUT_PATH}: {len(out)} zones, {size / 1e6:.2f} MB")


if __name__ == "__main__":
    build()
