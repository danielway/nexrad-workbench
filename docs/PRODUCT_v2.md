# Weather Radar Visualization Tool — Product Document

*Draft. Items marked **(proposed)** are defaults I've filled in for previously open questions and need your sign-off; everything else reflects requirements you've already stated. Open decisions are collected in §8.*

---

## 1. Overview & Goals

A tool for exploring weather radar data on a single, zoomable timeline. It serves both **archived data** (roughly 1991 through ~30 minutes before now) and **real-time streaming** data, and renders the selected moment onto a canvas with accompanying metadata.

The interface is organized around one central idea: a timeline that shows *where data exists and whether we have it*, paired with a playback position that determines *what is being rendered right now*.

**Apparent data source (proposed / confirm):** the domain model below — volume scans, VCPs, the 1991 floor, and the 3-or-6-chunk real-time structure — matches NEXRAD Level II (WSR-88D). The spec assumes that target; flag if it should be source-agnostic.

### Guiding principles

- **Invisible acquisition.** Downloading is a side effect of what the user looks at, not a separate manual chore. The user navigates; data acquisition reacts.
- **Guarded acquisition.** Because acquisition is automatic, it must be hard to accidentally trigger large downloads, and the app must clearly signal when it cannot get the data it needs.
- **Progressive detail.** The timeline reveals more structure as the user zooms in — contiguous blocks become scans, scans become sweeps.
- **The current moment is always legible.** The playback position is always clearly visible, and it is unambiguous which sweep(s) are feeding the canvas.

---

## 2. Domain Model

| Concept | Definition |
|---|---|
| **Radial** | A single ray of data along one azimuth. The smallest unit. |
| **Sweep** | One full rotation at a single elevation, composed of radials. |
| **Volume scan** | A complete set of sweeps across several elevations. The unit an archive file represents. |
| **Dead time** | The gap between sweeps while the instrument changes elevation and adjusts speed for the next sweep's parameters. |
| **VCP (Volume Coverage Pattern)** | Dictates the pattern of sweeps (elevations, products, ordering, timing) for each scan. |
| **Product / Elevation** | Filterable dimensions enumerated by the active VCP. |

### Archive vs. streaming

- **Archive data** is downloaded as complete scans, after the fact.
- **Streaming data** arrives as **chunks**, downloaded individually in real time. A sweep is composed of **3 or 6 chunks**. As chunks accrete into complete sweeps and complete scans, they become byte-equivalent to the archive.

**Equivalence guarantee:** a fully-streamed scan is equivalent to the same scan downloaded later from the archive. The cache should treat them identically once complete, so a scan streamed in real time needs no re-download if revisited.

---

## 3. The Timeline

The timeline spans the full available history (~1991 → now − ~30 min) and is **zoomable**, with the level of structural detail tied to zoom:

- **Zoomed out:** contiguous blocks indicating *where data is available* (and whether we hold it).
- **Mid zoom:** individual scan boundaries become visible.
- **Zoomed in:** the constituent **sweeps** within each scan are shown.

### 3.1 Selection vs. playback position

These are two distinct concepts the timeline must show simultaneously:

- **Selection** — a single position *or* a range. Represents the user's region of interest.
- **Playback position** — the single instant actively being rendered to the canvas and reflected in the metadata panels. Always clearly visible.

How a range selection bounds or loops playback is an open decision (§8).

### 3.2 Timeline layers

The timeline is best modeled as stacked semantic layers, each rendering differently per zoom level:

1. **Availability** — contiguous blocks of what is cached/downloaded.
2. **Scan & sweep structure** — scan boundaries and, when zoomed in, the sweeps within them. **Partially-downloaded scans render with a partial fill** so the user can see a scan is incomplete.
3. **Archive positions (estimated).** Archive object names expose only the scan's *start* time, so the position/spacing of subsequent scans is estimated from the **average scan duration for the active VCP**. These positions are guesses and should render with a distinct **uncertainty treatment**, visually separate from confirmed cached scans. *(proposed: treat estimated archive positions and confirmed cached scans as two different visual states.)*
4. **Projection** — upcoming sweeps and scans, projected forward from VCP timing + dead time, rendered as **ghosted** markers so the user can anticipate what's coming.
5. **Playback markers** — the **1–2 actively rendered sweeps**, clearly highlighted.
6. **Acquisition status** — in real-time mode, the **"up next"** sweep/chunk we're waiting on, marked distinctly and tied to the acquisition state (§7).

---

## 4. Rendering & Sweep Matching

### 4.1 Filters

The user selects an **elevation** and **product** of interest from the active VCP. The filter does double duty:

- It **drives sweep matching** for rendering (below).
- It **scopes acquisition** (§5): only matching data is fetched/cached.

### 4.2 Sweep matching — the 0–2 rule

For any playback position, the app renders **0, 1, or 2 sweeps**:

- **0 sweeps** — no matching sweep precedes the playback position within the **lookback range**. Nothing is rendered.
- **1 sweep** — either exactly one sweep matches, *or* multi-sweep mode is disabled. The single matching sweep is displayed.
- **2 sweeps** — multi-sweep mode is enabled *and* multiple sweeps match. Portions of both are rendered.

**Lookback range** is the maximum time the matcher looks *backward* from the playback position for the most recent matching (filtered) sweep. It bounds staleness — beyond it, data is considered too old to represent "now," and the app shows nothing rather than a stale sweep. *(proposed: make the lookback a defined, possibly configurable parameter rather than unbounded.)*

**Why two sweeps?** During the transition between an old and new sweep, the freshest data covers only part of the 360°. Compositing the newer sweep over the older one yields a complete picture — newest data where it exists, prior data elsewhere — rather than a half-empty render. This pairs directly with radial playback (§6).

### 4.3 Metadata panels

The accompanying panels always describe whatever sweep(s) are currently rendered, staying in sync with the playback position.

---

## 5. Acquisition

Acquisition is **reactive and invisible**: it's a function of *(playback position, selection, viewport, active filter)*. The app fetches what's needed to render the current position and the near future, scoped by the filter — never more by default.

### 5.1 Two classes of acquisition

- **Implicit prefetch** — bounded and fully automatic. Covers the current sweep plus a small lookahead. It is **debounced**, so transient positions produced while the user is actively scrubbing or zooming do not fire fetches; the view must settle first. Subject to hard caps on **concurrent downloads** and **total auto-fetched volume**. *(proposed)*
- **Explicit bulk** — a wide range selection does **not** silently download everything inside it. Instead, the range fills in lazily as the playback position approaches, or via an explicit "download this range" action that first shows an **estimate** (scan count / approximate size) and **confirms above a threshold**. *(proposed)*

This split is the primary guardrail against accidental mass downloads.

### 5.2 Filter-scoped fetching

- **Archive:** only sweeps matching the active filter are cached.
- **Streaming:** only chunks matching the filter are downloaded, and the app waits between them (rather than greedily pulling the full chunk stream).

### 5.3 Cache

Completed scans (archive or streamed) are cached identically per the equivalence guarantee (§2). A bounded cache with an **eviction policy** keeps storage from growing without limit. *(proposed — eviction policy TBD in §8.)*

---

## 6. Playback

### 6.1 Modes

- **Radial mode.** Animates a sweep line, revealing/rendering individual radials as the line passes. The current sweep is composited with the previous one into a single continuous 360° render (see §4.2). This is the live "sweeping" presentation.
- **Frames mode.** Whole-sweep renders with no sweep-line animation — each sweep is a discrete frame.

### 6.2 Zoom-dependent timing

Playback timing changes with zoom level:

- **Zoomed in (close):** the playback position is synchronized to **real radial collection times**. It advances evenly forward in wall-clock time, and radials are revealed at their actual timestamps — true-to-life playback.
- **Zoomed out:** the position **snaps between sweeps as equidistant frames**, giving each sweep equal screen time regardless of its true collection duration or the dead time between sweeps. This keeps zoomed-out playback even and watchable instead of lurching across real, irregular gaps.

---

## 7. States & Failure Handling

Because VCP timing and dead time are known, the app can compute an **expected arrival time** for the next chunk/sweep and detect problems precisely rather than guessing.

### 7.1 Acquisition states *(proposed)*

| State | Meaning | Indication |
|---|---|---|
| **Live** | Real-time data is arriving on schedule; we're keeping up. | Normal up-next marker on the timeline. |
| **Lagging** | Still acquiring, but falling behind the expected cadence. | Up-next marker shows a "behind" treatment. |
| **Stalled** | No new data past its expected arrival time. | Distinct stalled indicator at the up-next position. |
| **Failed** | Source error; we cannot acquire (e.g., stream dropped, archive gap). | Clear failure state with a manual retry affordance. |

### 7.2 Recovery

Transient problems retry automatically with **backoff**; persistent ones surface a **manual retry** rather than retrying forever. The failure must be visible on the timeline (at the affected position) *and* legible in the canvas/metadata area, so a user never mistakes "no data available" for "app is broken" or vice versa.

---

## 8. Open Decisions

Carried forward for resolution:

1. **Audience & primary use cases** — researchers, forecasters, enthusiasts? Shapes defaults (filters, zoom, cache size).
2. **Data source scope** — commit to NEXRAD Level II, or keep source-agnostic?
3. **Selection ↔ playback interaction** — does a range selection bound playback? Loop within it? Does the playback position stay within the selection?
4. **Lookback range** — fixed, VCP-derived, or user-configurable? What default?
5. **Playback speed** — is there a speed multiplier? Does it apply in both modes?
6. **Mid-playback zoom transition** — what happens to timing when the user zooms across the true-time / equidistant boundary while playing?
7. **Multi-sweep + radial interaction** — when two sweeps match in radial mode, do both animate, or does the older one freeze as a backdrop?
8. **Acquisition caps & thresholds** — concrete numbers for concurrency, auto-fetch volume, and the bulk-download confirm threshold.
9. **Cache eviction policy** — LRU, size-capped, age-capped, or pinned-selection-aware?