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

# Simplification is *topology-aware* (TopoJSON-style): we find junction vertices
# — where the set of zones touching changes (a vertex with ≠2 distinct
# neighbours across all rings) — then Douglas-Peucker each "arc" between
# junctions. A border arc shared by two adjacent zones is the identical vertex
# sequence on both, so it simplifies identically and the shared border stays
# shared. The runtime dissolve can then merge a multi-zone alert into one clean
# region with no internal borders. (A naive per-ring DP simplified each zone
# independently, breaking the sharing and leaving gaps the union couldn't bridge.)
SIMPLIFY_EPSILON = float(os.environ.get("ZONE_EPS", "0.02"))
# Decimals for vertex matching (the source shares exact border vertices) and for
# the emitted coordinates.
MATCH_DECIMALS = 6
OUT_DECIMALS = 4
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


def normalize_ring(ring):
    """Round to `MATCH_DECIMALS` and drop consecutive duplicates. Returns an
    *open* list of (x, y) tuples (no repeated closing vertex), or [] if degenerate.
    """
    pts = []
    for x, y in ring:
        p = (round(x, MATCH_DECIMALS), round(y, MATCH_DECIMALS))
        if not pts or pts[-1] != p:
            pts.append(p)
    if len(pts) > 1 and pts[0] == pts[-1]:
        pts.pop()
    return pts if len(pts) >= 3 else []


def dp_arc(pts, eps):
    """Douglas-Peucker on an open polyline; always keeps both endpoints."""
    if len(pts) <= 2:
        return list(pts)
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
                t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / seg2))
                cx, cy = ax + t * dx, ay + t * dy
                d = ((px - cx) ** 2 + (py - cy) ** 2) ** 0.5
            if d > dmax:
                idx, dmax = i, d
        if idx != -1:
            keep[idx] = True
            stack.append((lo, idx))
            stack.append((idx, hi))
    return [pts[i] for i in range(len(pts)) if keep[i]]


def simplify_ring_topo(pts, is_junction):
    """Simplify an open ring (list of tuples) arc-by-arc between junctions.

    Each maximal run between two junction vertices is DP-simplified as a unit.
    Because a shared border is the identical sequence on both adjacent zones and
    its endpoints are junctions kept on both, the simplified arc is identical too
    — so the shared border survives. Returns a closed ring of [x, y] lists.
    """
    n = len(pts)
    jidx = [i for i in range(n) if is_junction(pts[i])]

    if len(jidx) < 2:
        # No shared topology (an isolated loop): DP the whole ring from a
        # deterministic anchor (its lexicographically smallest vertex) so any
        # duplicate of this ring simplifies identically.
        start = min(range(n), key=lambda i: pts[i])
        rot = pts[start:] + pts[:start]
        simp = dp_arc(rot + [rot[0]], SIMPLIFY_EPSILON)
        if simp[-1] == simp[0]:
            simp.pop()
        out = simp
    else:
        out = []
        for k in range(len(jidx)):
            a = jidx[k]
            b = jidx[(k + 1) % len(jidx)]
            arc = [pts[(a + t) % n] for t in range(((b - a) % n) + 1)]  # inclusive
            simp = dp_arc(arc, SIMPLIFY_EPSILON)
            out.extend(simp[:-1])  # drop shared endpoint (next arc starts there)

    if len(out) < 3:
        return []
    ring = [[round(x, OUT_DECIMALS), round(y, OUT_DECIMALS)] for x, y in out]
    ring.append([ring[0][0], ring[0][1]])
    return ring


def zone_id(id_rule, rec):
    if id_rule == "stateC":
        return rec["STATE"] + "C" + rec["FIPS"][-3:]
    if id_rule == "idfield":
        return rec["ID"]
    return rec["STATE"] + "Z" + rec["ZONE"]  # stateZ: forecast & fire


def build():
    from collections import defaultdict

    # Pass 1: read and normalize every zone's rings (no simplification yet).
    zones = []  # (key, [open_ring_tuples, ...])
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
            norm = []
            for ring in rings:
                xs = [p[0] for p in ring]
                ys = [p[1] for p in ring]
                if max(xs) - min(xs) < MIN_RING_EXTENT and max(ys) - min(ys) < MIN_RING_EXTENT:
                    continue  # negligible sliver/island
                nr = normalize_ring(ring)
                if nr:
                    norm.append(nr)
            if norm:
                zones.append((f"{prefix}/{zone_id(id_rule, rec)}", norm))
                count += 1
        print(f"  {count} zones read")

    # Junctions: a vertex whose adjacency across all rings isn't a simple
    # pass-through (≠2 distinct neighbours) — i.e. where the touching set of
    # zones changes. Arcs run between junctions; shared arcs are identical on
    # both adjacent zones, so DP-simplifying them keeps the shared border shared.
    neighbors = defaultdict(set)
    for _, rings in zones:
        for r in rings:
            m = len(r)
            for i in range(m):
                a = r[i]
                b = r[(i + 1) % m]
                neighbors[a].add(b)
                neighbors[b].add(a)
    junctions = {v for v, nb in neighbors.items() if len(nb) != 2}
    print(f"  {len(junctions)} junction vertices of {len(neighbors)}")

    # Pass 2: simplify each ring arc-by-arc, group into polygons, output.
    out = {}
    for key, rings in zones:
        polygons = []
        for r in rings:
            simp = simplify_ring_topo(r, lambda v: v in junctions)
            if len(simp) < 4:
                continue
            if signed_area(r) < 0 or not polygons:  # outer (CW) or first
                polygons.append([simp])
            else:  # hole (CCW)
                polygons[-1].append(simp)
        if not polygons:
            continue
        # Skip antimeridian-spanning zones (Alaska Aleutians / Pacific marine,
        # with parts at both +179° and -179°): their bbox spans the globe, so
        # they'd register as "in view" everywhere. Irrelevant to a CONUS tool.
        all_x = [x for poly in polygons for ring in poly for x, _ in ring]
        if max(all_x) - min(all_x) > 180.0:
            continue
        out[key] = polygons

    os.makedirs(os.path.dirname(OUT_PATH), exist_ok=True)
    with open(OUT_PATH, "w") as f:
        json.dump(out, f, separators=(",", ":"))
    size = os.path.getsize(OUT_PATH)
    print(f"wrote {OUT_PATH}: {len(out)} zones, {size / 1e6:.2f} MB")


if __name__ == "__main__":
    build()
