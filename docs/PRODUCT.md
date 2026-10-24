# NEXRAD Workbench — Product Specification

**Product spec and decision record · June 2026, revised through October 2026**

**Status:** This is the reference for the product's model: the timeline,
selection and playback, live tethering, acquisition and caching, and the
honesty rules for what the canvas shows. It was written against the June 2026
build and revised as later work changed it, including the October 2026
"One Instrument" redesign. Where it describes how a surface *looks* and the
running app differs, the app is right. Forward-looking ideas are in
**§16 Deferred / future work**. The numbered **Alignment decisions** at the end
are the rationale for the timeline/playback/acquisition UX. Code comments cite
this document by section ("PRODUCT.md §5.1", "alignment §N"), so keep section
numbers stable when editing.

---

## 1. Overview & Goals

A tool for exploring weather radar on a single, zoomable timeline. It serves
both **archived data** (roughly 1991 through ~30 minutes before now) and
**real-time streaming** data, and renders the selected moment onto a full-bleed
canvas with accompanying metadata.

The application runs **entirely in the browser** via WebAssembly: data is
fetched directly from public sources, decoded, and rendered client-side. There
is no backend.

The interface is organized around one idea: a timeline that shows *where data
exists and whether we have it*, paired with a playback position that determines
*what is being rendered right now*. The design north star is *powerful but
invisible* — rich data sources, cache state, and acquisition activity are all
legible at a glance, yet a first-time user sees only a familiar video-player-like
scrubber. Depth is revealed spatially through zoom, not through menus.

**Data source.** The product is **NEXRAD Level II (WSR-88D)** specific. The
domain model below — volume scans, VCPs, the 1991 floor, the 3-or-6-chunk
real-time structure, Message-Type-5 VCP headers, radial moments — targets that
source directly. It is not source-agnostic.

**Audience.** Weather enthusiasts, researchers, and operational forecasters. The
design deliberately spans the range from a casual "what's happening right now"
glance to power-user archival analysis, and from desktop to phone.

### 1.1 Goals

1. Surface the available data sources (live stream + archive) and the local
   cache transparently, without requiring the user to manage them.
2. Make acquisition activity (what is downloading, what is queued, what is
   coming next) visible in place, ambiently, and honestly.
3. Provide flexible playback: scrubbing, variable speed, frame stepping, looping
   (including a live-updating loop), and live following.
4. Render the selected moment faithfully in 2D and 3D, with the displayed
   frame's true timestamp always legible.
5. Scale gracefully from a casual glance to power-user archival analysis — and
   from desktop to phone.

**Non-goals (v1):** collaborative sessions, alerting/notifications,
offline-first guarantees beyond the existing cache, more than one site on a
phone, and a product, tilt rule or playback of its own for each site.

## 2. Design Principles

1. **Decouple acquisition, cache, and playback.** Acquisition is the library
   receiving books; playback is reading. Pausing reading never stops deliveries.
   The timeline is the shared map where both are visible.
2. **Invisible, reactive acquisition.** Downloading is a side effect of what the
   user looks at, not a separate manual chore. The user navigates; acquisition
   reacts.
3. **Guarded acquisition.** Because acquisition is automatic, it must be hard to
   accidentally trigger large downloads, and the app must clearly signal when it
   cannot get the data it needs.
4. **Borrow the video-buffer grammar.** Hollow = available, filled =
   downloaded, playhead = position, red dot at the right edge = live. Users
   arrive already fluent; the timeline is "a buffer bar that learned the data's
   structure."
5. **The canvas never lies about time.** The displayed frame's timestamp is the
   primary readout. Any discrepancy between playhead time and rendered-frame time
   is surfaced, never hidden.
6. **Zoom is the disclosure mechanism.** Detail emerges spatially (semantic
   zoom), not through settings or modes the user must discover.
7. **One visual channel per concern.** Cache state → fill; acquisition activity
   → motion; user intent → overlays; data structure → containers. No channel
   double-booked.
8. **Motion means routine work; red means failure or live.** Pulsing is never an
   alarm. Red is reserved for the live edge and for errors — nothing else.
9. **Always responsive.** Heavy work (decompression, decoding, storage I/O) runs
   off the UI thread. The interface never freezes on data.
10. **Resilient actions.** User actions do simple, consistent things regardless
    of state — enqueue, toggle, seek. The user needs no mental model of the app's
    mode to predict what a click will do.

## 3. Domain Model & Terminology

| Concept | Definition |
|---|---|
| **Radar site** | A physical radar installation, identified by a short site ID (e.g. `KDMX`). A site fixes the geographic origin of its own disc, scans and stream; the map can show several at once (below). |
| **Radial** | A single ray of data along one azimuth. The smallest unit. |
| **Sweep / tilt / elevation** | One full 360° rotation at a single elevation, composed of radials. SAILS/MRLE VCPs may revisit low tilts mid-volume. |
| **Volume scan** | A complete set of sweeps across several elevations, ~5 minutes. The unit an archive file represents. |
| **Dead time** | The gap between sweeps while the instrument changes elevation and adjusts speed for the next sweep's parameters. |
| **VCP (Volume Coverage Pattern)** | Dictates the pattern of sweeps (elevations, products, ordering, timing) for each scan. Carried in the scan's header record (Message Type 5). |
| **Product / Elevation** | Filterable dimensions enumerated by the active VCP. A cut is identified by its **elevation number** within the VCP, unique within a scan even when the literal angle repeats (SAILS/MRLE supplemental cuts); storage keys and the inspector use it. A held **tilt is an angle**, not a number: it matches every cut whose angle rounds to the same 0.1° and that plays the same part of a split pair (surveillance for reflectivity, Doppler otherwise), in volume order — the base cut and each SAILS/MRLE repeat. |
| **Chunk** | Live-streaming delivery unit; 3 or 6 chunks compose a sweep. |
| **Frame** | UI term: a sweep matching the *currently selected product + tilt* — something the canvas can render. The timeline's primary unit. Under SAILS ×3 a held 0.5° is four frames a volume (about one every two minutes); under VCP 35 it is one. |
| **Live edge** | The right boundary of acquired data; "now" minus dissemination latency. |
| **Tether** | Whether the playhead is following the live edge. |
| **Loop window** | A time/frame range playback cycles over; may be pinned to the live edge. |

**Sites and the focus.** A desktop map shows up to three sites at once; a
phone shows one, and a desktop window narrowed into the phone's layout keeps
only the focus. Every site on the map is the same kind of thing: each has
its own scans, frame, stream and disc, all at the one shared playhead,
product and tilt rule, drawn older frame first so the newer one is on top.
Each disc carries its own range rings and Rays marks, the focus's at full
strength and the others' at half.
One of them is the **focus**: the surfaces that hold a single value (the
tilt ladder, the frame readout, the age pill, the caption's first words, the
timeline's full track, the overview's storm bars, the calendar) follow it.
What can be a list or a sum covers every site: the top bar's site chip reads
`KTBW · +2`, the caption names other sites whose frame is off the playhead
(§11.3), hovering a disc gives its site's frame time and age, the probe
reads every disc under the cursor, the activity chip sums the streams
(§4), each other site has a lane under the track (§5), and a range download
fetches and prices every site (§10.1). The 3D views draw every site (§9.4)
and storm cells are detected once over every site shown (§13.2). The site
picker adds a site ("+ Also show"), removes one, moves the focus or keeps
only the focus; picking a site plainly replaces the set. The map has its own
centre, set when the focus changes or from a link, apart from any radar's
position. Preferences remember the focus; the set itself travels only in the
link.

### 3.1 Archive vs. streaming

- **Archive data** is downloaded as complete scans, after the fact.
- **Streaming data** arrives as **chunks**, downloaded individually in real time.
  A sweep is composed of **3 or 6 chunks**. As chunks accrete into complete
  sweeps and complete scans, they become byte-equivalent to the archive.

**Records and chunks are the same underlying unit.** Archive files are a sequence
of records (a header record followed by data records); real-time exposes those
same records as chunks delivered incrementally. The first record of any scan is
the **header record**, which carries the VCP and operational parameters —
without it, subsequent records cannot be interpreted.

**Equivalence guarantee:** a fully-streamed scan is equivalent to the same scan
downloaded later from the archive. The cache treats them identically once
complete, so a scan streamed in real time needs no re-download if revisited.

### 3.2 Three categories of time

Three distinct time concepts coexist and must not be conflated:

- **Actual** — parsed from radial headers or read from the wall clock. Drives the
  canvas, the playback cursor, and completed timeline blocks.
- **Projected collection** — when the radar will *physically sample* a future
  sweep, computed from VCP timing and dead time.
- **Projected availability** — when a chunk will *be downloadable* from the
  source (collection + a typical 5–15 s ingest lag).

Timeline projections (§5) sit at *collection* times; failure detection and
countdowns (§11) compare against *availability* times. A chunk past its
collection time is not yet late if the ingest-lag window hasn't elapsed.

## 4. Screen Architecture

- **Canvas** — full-bleed, dominant. Chrome is dark and muted so reflectivity
  palettes own the color space.
- **Top readouts** — radar site (the focus, with the count of other sites on
  the map), product/tilt selector, **displayed-frame timestamp** (primary),
  data age when tethered ("updated 1m ago"), local time with UTC on tap.
- **Bottom cluster** — transport row (selection chip, play/pause, frame step,
  speed) above the timeline. The **selection chip** names what plays — `LIVE`,
  a point ("At 14:02:10 · 2:17 behind"), a fixed range, or a range ending now
  ("Last 6 frames → now") — and opens one menu grouped by shape: go live, stop
  looping, stop the stream; the last N frames / minutes → now; loop what's in
  view.
- **Timeline** — an **overview** lane above the **main strip**: 48 hours of
  coverage (wider when the strip shows more) with a box marking what the strip
  shows. It slides with now while its right edge is on the live edge, can
  slide anywhere in the archive, and zooms out to the whole NEXRAD era. It
  moves the view, never playback.
- **Activity chip** — tiny, near the transport (and in the mobile top bar);
  always present so its absence is never ambiguous: a muted "Up to date" when
  idle, "↓ 2 Downloading" with a pulse when acquiring. With more than one
  site streaming it sums them ("Receiving 3 sites · 1 stalled"). Tap opens
  the activity sheet.
- **Scan inspector** — popover/sheet opened from a scan (right-click on desktop,
  long-press on touch): lists every sweep with tilt, size, cache state, per-chunk
  progress, tap-to-fetch.
- **Activity sheet** — the single acquisition-transparency surface: a stage
  strip (Queued › Downloading › Processing › Finishing) with live counts,
  rolling throughput, active/queued/recent downloads with sizes and
  cancel/retry, a section per site with its stream, the acquisition policy
  toggles, and a collapsed **Details**
  disclosure holding session totals and the recent-request log. Raw request
  counts and deep diagnostics (per-phase timings, worker queue depth, FPS,
  cross-origin isolation, VCP forecast) appear inside Details only in
  developer mode.

## 5. The Timeline

The timeline spans the full available history of the focus site
(~1991 → now − ~30 min) and is **zoomable**, with structural detail tied to
zoom. It is the single shared map of three decoupled systems: **acquisition**,
**cache**, and **playback**.

The main strip is the focus's track. Each other site on the map gets an
11 px lane under it, in the order the sites were added, with its ID at the
left: one block per volume, solid when it is on the device, hollow when it
is only listed, hatched while it waits to download, pulsing while it
downloads, a red edge when its download failed, and the volume its
stream is collecting solid up to the newest radial with a dashed outline to
its projected end (red from where the chunks stopped when that stream is
stalled). One now line crosses the strip and the lanes. The selection strip,
the zoom and the overview are shared; the overview's bars are the focus's.

### 5.1 Selection vs. playback position

Two distinct concepts the timeline shows simultaneously:

- **Selection** — a single position *or* a range; the user's region of interest.
  A range selection becomes a **playback loop boundary** during play (§7). The
  selection does not constrain manual seeks.
- **Playback position** — the single instant actively rendered to the canvas and
  reflected in the metadata panels. Always clearly visible.

Scrubbing while tethered to live detaches from live (acquisition continues in the
background; the user can return to "now" to re-tether — see §6).

### 5.2 Visual channels

One channel per concern; the strip must read correctly in grayscale.

| Concern | Channel | Treatment |
|---|---|---|
| Cache state | Fill | Hollow / solid / segmented partial / dashed ghost |
| Acquisition activity | Motion | Pulse on in-flight cells; faint hatch on queued |
| User intent | Overlays | Playhead line + handle; amber range wash over the frames, with its bar + handles on the selection strip |
| Data structure | Containers | Scan blocks containing frame cells; tick rail with time labels |

### 5.3 Frame-cell states

| State | Visual |
|---|---|
| Available (server only) | Hollow outline |
| Downloaded | Solid fill |
| In flight (live) | Segmented fill animating as chunks land (3 or 6 segments) |
| In flight (archive) | Pulse fill inside the cell (archive downloads carry no chunk telemetry) |
| Queued | Faint hatch |
| Projected (future) | Dashed ghost at predicted time; nearest shows a countdown ("0.5° in ~40s") |
| Failed | Small alert tick; tap to retry |
| Actively rendered | Accent ring; snaps cell-to-cell as the playhead crosses frame boundaries |

**Accent budget:** at most **three accent colors visible at once** — playhead,
live edge, active-frame ring. Everything else is neutral fills/textures.

### 5.4 Frames-first strip

The strip's primary cells are **frames of the currently selected product/tilt**,
not the raw sweep inventory. The full volume structure renders only as a faint
sub-texture inside each scan container. The **scan inspector** (§4) carries the
complete breakdown — all tilts, SAILS revisits, sizes, chunk progress, manual
fetch. This aligns the timeline with what the canvas can actually show and
collapses the hardest density problem; the inspector serves power users better
than the strip ever could.

Partially-downloaded scans render with a partial fill so an incomplete scan is
visible at a glance. Archive scan positions beyond the first are estimated from
the active VCP's average scan duration (object names expose only the start time);
estimated positions render with an uncertainty treatment distinct from confirmed
cached scans.

### 5.5 Zoom tiers (semantic zoom)

| Tier | Range (approx) | Shows | Playback |
|---|---|---|---|
| **Micro** | minutes → ~1–2 hr visible | Scan containers with frame cells, chunk segments, ghosts + countdown | Either mode (§8.2) |
| **Macro** | hours → a few days | Each scan is a block spanning the volume's real duration — solid across downloaded sweeps, hollow across the rest; per-sweep hairlines once wide enough; gap glyphs where real spacing exceeds threshold. Blocks merge into a coverage fill once denser than ~1 per 3px | Either mode (§8.2) |
| **Overview** | above the strip: 48 h by default, zooming out to the whole NEXRAD era | Per-scan coverage, then day → week → month → quarter bars as it zooms out, with a box marking the strip's view. A bar's height is storm activity: its stormiest day, from the archive's file sizes. The strip itself shows at most 3 days | Never changes playback. Clicking a bar goes to its stormiest known day |

- **Zoom is view-only.** Changing tier never detaches the playhead from live,
  ends a loop, pauses one, or switches between Frames and Rays.
- **One tier state machine** owns the boundary, stored on playback state; all
  zoom writes go through it (the tier is never re-derived from raw zoom). Tuning
  constants are co-located in one place.
- **Snap with hysteresis** so pinch gestures never flicker at a boundary:
  Micro↔Macro nominal **1.0 px/s** (enter Micro ≥ **1.15**, exit Micro ≤ **0.87**
  px/s).
- **Morph animation:** frame cells visibly collapse into scan ticks (and expand
  back). Playhead and live edge stay spatially stable through the transition.
- The strip stops at 3 days; reaching further back is the overview's job, so
  the strip never needs a calendar renderer or year-wide label soup.
- **Storm activity** comes from file sizes in the archive listing: a volume
  full of echo compresses worse, so a day's mean volume size tracks how
  stormy it was (rank correlation 0.88 per day against reflectivity
  coverage on KTBW, 0.83 on KDMX). What it measures is how much of the
  scan holds echo, so hover text names an amount ("Lots of echo", "Some
  echo", "Little echo", "Clear") rather than a kind of weather: a dry
  summer day full of insect echo reads "Some echo". Heights are scaled per
  site and per volume format (pre-2008, super-resolution, dual-pol),
  against the site's quiet-day size across all seasons. A same-season
  baseline was tried and dropped: summer's quiet days are larger, but so
  are its rain days, and scaling by season ranked KDMX days against rain
  worse (0.58 vs 0.82).
  Heights are stretched to the view: its typical day sits near the floor
  and its biggest at the top (a spread of at least 0.3 levels, so a quiet
  view stays low), so the days that stand out in what's on screen are the
  tall ones. Zoomed in to scans, each volume is drawn by its own size from
  the same listing, so a tall day keeps showing which hours held the echo,
  and hovering a volume names its time and size.
  Day sizes come from a precomputed index: a nightly GitHub Action lists
  every site's days and publishes one small file per site (daily volume
  count, mean size and format; ~70 KB gzipped for the whole era) beside
  the app on GitHub Pages. The app reads a site's file once per session
  (kept in IndexedDB, revalidated by ETag), so bars appear at any zoom for
  one request. Only the days the index lacks (the last day or two, gaps
  while it backfills) are listed live, a few at a time once the overview
  stops moving. Zoomed in to scans, the per-volume sizes for the newest
  twelve months come from the index's month files (one per site-month,
  ~50 KB gzipped for a busy month), one request for a month in view;
  only days no month file holds (older months, the newest day or two) are
  listed live, for their per-volume sizes. Without an index (dev builds)
  every day is listed live as before. Listed days are stored, so each is listed once. Days the
  radar recorded nothing show as a red line.

### 5.6 Saved events (bookmarks)

A named range selection can be persisted as a **saved event** — a reusable
bookmark across sessions. Saved events render as colored bars on the timeline,
surface in a list grouped by site, and offer a "navigate to" action that jumps
the playback position and selection to the event's range. An event from
another site picks that site the way the site picker does: it becomes the
only site on the map and the focus, and if it was already on the map it keeps
its scans rather than being added twice.

## 6. Live Streaming & the Tether Model

There is **one continuous timeline**; "live" is its right edge. Live is not a
mode — it is a tether state.

- **Tethered:** solid LIVE indicator; the playhead rides the live edge. In
  Rays, new chunks draw in on the canvas as they arrive; in Frames, the last
  complete sweep holds until the next one finishes (§8.2).
- **Detached:** scrubbing backward auto-detaches. The selection chip turns into
  a point and shows the lag ("At 14:02:10 · 2:14 behind"). Streaming **continues in the background**
  by default, filling the cache at the right edge. One tap re-tethers with a
  brief animated catch-up.
- **Pause while tethered:** the frame freezes; a "behind live by 0:42" counter
  grows; resuming plays from the pause point (now detached). Detachment is made
  explicit via the chip's change. (Play/pause while tethered does **not**
  enter the pinned loop.)
- **Session start:** the app opens **tethered to live** (the dominant use
  case). The default live experience is **single-frame following**; the
  pinned loop (§7) is opt-in via the selection chip's menu, one tap away.
  Picking another site while tethered re-tethers on the new site.
- **Several sites:** while live, every site on the map streams on its own
  channel, and a site added while live starts its stream at once. The tether,
  the stop and the stream's phase (connecting, receiving, waiting) belong to
  the session, not to a site: the phase follows the focus's chunks, the
  session streams from the first site's start until the last site's stream
  ends (a site whose stream fails stops alone), and going live or stopping
  acts on every site together.
- **Stopping:** stop is explicit and symmetric with go-live. While tethered,
  the timeline now-cap ("◉ LIVE"), the selection chip's "Stop the stream", and
  **Shift+L** all stop the stream at the live edge; on mobile (which has no
  now-cap) the chip's sheet owns stop and leaves the playhead in place. Stop ≠ freeze: pause-while-tethered keeps
  the stream filling the cache, stop tears the channel down. A status line
  confirms the stop.
- **Streaming visibility:** every status surface (selection chip, now cap,
  age pill, activity line, canvas caption) projects one per-frame
  `core::session::SessionVm` — four facts (stream, playhead, selection,
  frame) — so "is data actually arriving?" reads the same everywhere. The
  transport activity line carries the moving part — "receiving 2.4° · 3 of 6",
  "next 0.5° in ~0:40", or an amber "stalled · 2.4° is 0:42 late" — the
  now cap stays a two-word pill (LIVE / REJOIN / GO LIVE), and pulsing is
  reserved for data genuinely moving (connecting/receiving), never for a
  routine wait.
- **Idle-stop:** while detached, background streaming auto-stops after **60 min**
  to bound S3 chunk polling; "return to live" stays instant for any realistic
  browsing detour.
- **Policy:** a data-saver toggle ("pause live stream while reviewing") in the
  activity sheet stops the stream immediately for metered connections. Default off;
  the 60-min idle-stop is the safety backstop for the default case.

## 7. Loop System

- **Modes:** **Loop** (wrap to start), **Ping-Pong** (reverse at the endpoints),
  and **Once** (stop at the end).
- **Creation order:** presets first (the selection chip's "last N frames → now"
  and "last 30 min → now"); custom range second: drag along the **selection
  strip** between the tick rail and the frames, or press I/O. The frames below
  only move the playhead, so no modifier turns a scrub into a selection.
- **Default & presets:** default preset is **last 6 frames**. The preset menu
  offers **4 / 6 / 10 frames**, **30 min / 1 h** durations, and **pin to live** —
  the same defaults on all device classes.
- **Window basis:** frame-count windows are preferred in Micro (scan spacing
  varies); duration windows are offered as preset alternatives.
- **Pinned sliding loop:** dropping the selection strip's right handle on the
  live edge snaps and pins it — the handle turns red, and the window slides
  forward as sweeps arrive. This is the core "loop the last N while still
  streaming" experience.
- **Incorporation rule:** newly arrived frames enter the loop **at the wrap
  point**, never mid-cycle, so the loop never visibly pops.

## 8. Playback

### 8.1 Modes

- **Radial mode.** Animates a sweep line, revealing/rendering radials as the line
  passes. The current sweep is composited with the previous one into a single
  continuous 360° render (see §9.2). The live "sweeping" presentation.
- **Frames mode.** Whole-sweep renders with no sweep-line animation — each sweep
  is a discrete frame.

### 8.2 Playback mode & speed

Playback timing is the user's explicit choice of **Frames** or **Rays**, set by
a toggle beside the speed control and carried in the link (§13.4); each mode's
speed is saved with the preferences. Zoom never changes it (§5.5).

- **Rays — true-time playback.** The position advances evenly in wall-clock
  time; radials are revealed at their actual collection timestamps behind a
  rotating sweep line, in archive playback and live alike. Live, each chunk
  draws in as it arrives and the canvas tag reads "streaming 2/3". Optional
  data-age desaturation fades the oldest data behind the line. Speeds are
  realtime multiples: **1×, 2×, 15×, 30×, 60×, 120×, 300× (default), 600×,
  1200×** (e.g. 300× ≈ 5 minutes of weather per real second). Real-time lock is
  available only in Rays.
- **Frames — frame-stepping.** The position snaps between matching sweeps as
  equidistant frames, giving each sweep equal screen time regardless of true
  collection duration or surrounding dead time. Speeds are frame rates:
  **1 / 2 / 5 / 10 / 15 fps** (5 fps default). Frames only ever shows whole
  sweeps: live, the last complete sweep for the selected tilt holds until the
  next one finishes, and the canvas tag reads "collecting 2/3" meanwhile.
  With several sites the frames are every site's frames together; frames of
  different sites within a minute of each other are one frame at the later
  time, so a step shows each site's new sweep. Frames is the default mode.

The speed control shows × in Rays and fps in Frames. Switching modes swaps the
speed list and snaps the active speed to the nearest equivalent, so the
perceived rhythm doesn't lurch. A live lookback replay always steps frames.
Going live in Rays locks the speed to 1× without overwriting the saved speed,
which comes back on leaving live.

### 8.3 Gap glyphs

On the zoomed-out strip, **gap glyphs** mark where true spacing exceeds a threshold (outages,
VCP changes) so equidistant playback doesn't deceive.

### 8.4 Radial-animation gating

Radial-level canvas animation (drawing radials as chunks arrive/replay) is
Rays mode itself; there is no separate toggle. The sweep line is dropped at
high speeds (above 30×) where it would only strobe. While live with a fixed
tilt selected, the reveal pauses while the antenna is on other tilts.

## 9. Rendering & Sweep Matching

### 9.1 Filters

The user selects a **tilt** (an angle; every visit of it in the volume is a
frame — §3) and a **product** from the available set: reflectivity (REF),
velocity (VEL), spectrum width (SW), differential reflectivity (ZDR),
correlation coefficient (CC) and differential phase (KDP). Clutter Filter
Power is not offered until the worker decodes it as its own product (the
decoder keeps its moment for that day). The filter does double duty: it **drives
sweep matching** for rendering (below) and it **scopes acquisition** (§10) — only
matching data is fetched/cached.

### 9.2 Sweep matching — the 0–2 rule

For any playback position, the app renders **0, 1, or 2 sweeps**:

- **0 sweeps** — no matching sweep precedes the playback position within the
  lookback range. Nothing is rendered (rather than showing stale data).
- **1 sweep** — exactly one sweep matches, *or* multi-sweep mode is disabled. The
  single matching sweep is displayed.
- **2 sweeps** — multi-sweep mode is enabled *and* multiple sweeps match;
  portions of both are rendered.

**Lookback range** is the maximum time the matcher looks *backward* from the
playback position for the most recent matching (filtered) sweep. It is a fixed
**15 minutes** — long enough to cover a full VCP cycle with margin. Beyond it,
data is considered too old to represent "now" and the app shows nothing.

**Two-sweep compositing & radial behavior.** During the transition between an old
and a new sweep, the freshest data covers only part of the 360°. The app renders
the **older sweep as a static backdrop** and composites the **newer sweep over
it**: in radial mode the newer sweep's sweep line divides the canvas — pixels
already passed by the line show the new sweep, pixels ahead of it show the prior
sweep — yielding a complete picture (newest data where it exists, prior data
elsewhere) rather than a half-empty render. The older sweep does not animate; it
is the frozen backdrop the newer radial line paints over.

### 9.3 Metadata panels

The accompanying panels always describe whatever sweep(s) are currently rendered,
staying in sync with the playback position.

### 9.4 Views

The radar projects to the canvas in two views (2D by default); sweep matching and
the playback model are identical across them — only the projection differs.

- **2D map.** Equirectangular projection centered on the map's own centre
  (§3), with every site's disc on it, drawn alongside the toggleable overlays
  in §13.
- **3D globe.** WebGL2 sphere with a single **Google-Earth-style orbit camera**:
  it orbits a pivot point on the surface (always the screen center) at a
  distance, with tilt and heading. Left-drag pans the globe (the grabbed point
  sticks to the cursor), right/middle/Ctrl-drag tilts and rotates, scroll zooms
  toward the cursor, and double-click flies the pivot to the clicked point with
  a smooth transition. Zoomed out it behaves like a planet camera; zoomed in it
  orbits the pivot — no mode boundary. A fresh 3D view (and R) opens over the
  map centre tilted 40° toward north, so it reads as 3D rather than as the
  flat map. The globe draws a **volumetric ray-march** by default that
  composites all elevations of each site's scan into a 3D pass with a
  user-controlled density cutoff; turning it off (View tab, "Draw every
  tilt") renders the selected elevation as a surface. Every site on the map
  draws: surfaces older frame first as in 2D, then volumes from the farthest
  from the camera to the nearest, so two overlapping volumes read as two
  translucent bodies. A desktop holds a volume for every site on the map; a
  phone holds one. The ladder, the colour scale and the caption describe the
  focus. While volume drawing is on, archive fetches keep whole
  volumes whatever tilt is selected, and the sweep-level controls stand down
  (the session's tilt role, `DrawsAll`): the ladder keeps its bars and caret
  but highlights no row and takes no clicks, every cut on the device wears the
  on-screen outline, the top-bar tilt chip reads "All tilts", and Rays is
  disabled with Frames playing (a Rays choice is held and comes back when the
  volume stops drawing every tilt). A volume the device holds only part of
  says so in the caption ("drawing 5 of 23 cuts · fetching the rest").

## 10. Acquisition & Cache

Acquisition is **reactive and invisible**: a function of *(playback position,
selection, viewport, active filter)*. The app fetches what's needed to render the
current position and the near future, scoped by the filter — never more by
default.

### 10.1 Two classes of acquisition

- **Implicit prefetch** — bounded and fully automatic. While paused it covers
  exactly the scan under the playhead (the render anchor); while playing it
  keeps a speed-scaled lead of at least one volume ahead in the playback
  direction so frame advances never wait on a cold fetch. There is no trailing
  prefetch — backward jogs are served on-frame by the same at-or-before anchor
  resolution. It is **debounced (300 ms)** so transient positions
  produced while scrubbing or zooming don't fire fetches; the view must settle
  first (the debounce collapses to zero during playback). Subject to a cap of
  **4 concurrent downloads** (below the browser's per-origin limit) and a
  **256 MB per-session** auto-fetch volume backstop against runaway background
  downloading.
- **Explicit bulk** — a wide range selection does **not** silently download
  everything inside it. It fills in lazily as the playback position approaches,
  or via an explicit "download this range" action. A finalized selection spanning
  **≤ 6 hours** (~70+ volumes at ~5 MB each) downloads immediately; a longer span
  first shows an estimate (scan count / approximate size) and asks for
  confirmation. A range is fetched for every site on the map, and the
  confirmation prices them together (`3 sites · 90 scans, 1.3 GB to
  download`).

This split is the primary guardrail against accidental mass downloads.

### 10.2 Filter-scoped fetching

- **Archive:** only sweeps matching the active filter are cached. (Archive scans
  download as whole volumes, so a cached scan already holds all tilts — switching
  product/tilt needs no proactive refetch.)
- **Streaming:** only chunks matching the filter are downloaded, and the app
  waits between them rather than greedily pulling the full chunk stream.

**Mid-volume entry.** When streaming begins partway through a volume, the
**header record** (§3) is always co-fetched alongside the latest chunk so the VCP
is available to interpret what follows. The user does not see this as a separate
step.

### 10.3 Live-edge prediction

Dashed **ghost cells** mark predicted arrival times derived from the current
VCP cadence; the nearest ghost shows a countdown, and cells transition
ghost → filling → filled. This teaches the radar's rhythm without documentation.
Ghosts are emitted **only while a live stream exists**; when the stream dies they
disappear rather than going stale.

### 10.4 Cache

Completed scans (archive or streamed) are cached identically per the equivalence
guarantee (§3). Storage is bounded by a user-configurable quota
(default **2 GB**); eviction is **least-recently-used** by per-scan touch
timestamp, targeting **80% of quota** to avoid thrashing near the limit. Reads
bump the touch timestamp, throttled to **once per scan per minute** so heavy
scrubbing doesn't churn the LRU order.

### 10.5 Aggregate status

The activity chip ("↓ 3 Downloading", or "Up to date" when idle) opens the
**activity sheet**: the stage strip, throughput, the download list with sizes
and cancel/retry, and policy toggles: *auto-fetch while scrubbing* and *pause
live stream while reviewing* (data-saver). A Wi-Fi-only toggle is not
implementable in a browser (no reliable network-type API);
`navigator.connection.saveData` may later seed the data-saver default.

**One number, one meaning.** The chip's count is *scans still to fetch* —
queued plus downloading — and nothing else. Decoding and post-ingest settling
change the word, never the number, so the count always matches the actionable
rows in the sheet. The stages draw on disjoint sources (the operation queue,
the decode workers' pending jobs, the request ledger's ingest→timeline
blackout), so a scan is never counted twice. Raw HTTP requests in flight —
which include archive listings the user never asked for — are deliberately
excluded from every user-facing count and surface only as a developer row.

**Throughput, not ETA.** The sheet shows a rolling 10-second transfer rate. It
shows no ETA: queued scan sizes are only a flat per-scan estimate (the S3
listing exposes no sizes), so a remaining-time figure would be an estimate
built on an estimate. An idle window renders as "—", never a fabricated 0 B/s.

## 11. States, Failure & Canvas Honesty

Because VCP timing and dead time are known, the app computes both the **expected
collection time** and the **expected availability time** for the next
chunk/sweep (§3) and detects problems precisely rather than guessing.

### 11.1 Acquisition states

| State | Meaning | Indication |
|---|---|---|
| **Live** | Real-time data is arriving on or before expected availability; we're keeping up. | Normal up-next marker on the timeline. |
| **Lagging** | Past expected availability but still progressing toward arrival. | Up-next marker shows a "behind" treatment. |
| **Stalled** | Significantly past expected availability with no progress. | Distinct stalled indicator at the up-next position. |
| **Failed** | Source error; we cannot acquire (stream dropped, archive gap). | Clear failure state with a manual retry affordance. |

### 11.2 Failure model & recovery

Failures are **per-cell**, not global: a failed download shows an alert tick on
the affected cell and does not error-pause the whole queue; tapping it
re-enqueues the download. Transient problems retry automatically with
**backoff**; persistent ones surface manual retry rather than retrying forever.
A volume that arrives cut short is downloaded once more without a word, while
the playhead still wants it; only a second failure is reported.
Every failure is visible both on the timeline (at the affected position) and in
the canvas/metadata area, so a user never mistakes "no data available" for "app
is broken" or vice versa. Red styling is reserved for failures and the live dot
only.

### 11.3 Canvas honesty rules

1. The displayed-frame timestamp is the primary readout
   (e.g. "2:41:07 PM CDT · 0.5°").
2. When playhead time ≠ frame time (undownloaded region, gap), keep showing the
   most recent available frame and surface the discrepancy via **caption only**
   ("showing 2:41 · fetching 2:51…") — no canvas shimmer. Cheapest, honest, reads
   in grayscale and to screen readers. With several sites the caption names
   up to two other sites whose frame is more than a frame off the playhead,
   newest first ("KJAX showing 02:57:12 · KMLB 02:56:33 · playhead
   03:00:00"); a blank focus still says why first.
3. At the live edge, show data age ("updated 1m ago") — radar "live" is minutes
   old by nature; acknowledging it builds trust exactly when severe weather makes
   users notice lag.
4. Local time is primary; UTC is available on tap (enthusiasts want Zulu).

## 12. Interaction Model

| Input | Behavior |
|---|---|
| Press/drag on strip | Seek immediately on press; drag scrubs |
| Scroll / pinch | Zoom, anchored at cursor/pinch center (with snap hysteresis) |
| Overview: drag the view box / its sides | Move the strip's view / zoom it |
| Overview: drag or sideways-scroll the lane; click it | Slide the overview; center the strip's view there |
| Overview: scroll | Zoom the overview, from hours out to the whole archive; zoomed out to bars, a click goes to that day |
| Drag on the selection strip | Draw a range; drag a handle to resize; drop the right handle on the live edge to pin it to now; click to clear |
| Right-click a scan (desktop) / long-press (touch) | Open the scan inspector (includes "loop from here") |

**Keyboard:** Space play/pause · ←/→ frame step · Shift+←/→ scan step · I/O loop
in/out · plain **L** go-live (one-way re-tether) · **Shift+L** stop live (the
explicit inverse; neither key is a toggle, so go-live can never accidentally
stop the stream) · +/− timeline zoom anchored at the playhead · **[** / **]**
speed down/up. Camera pan uses WASD (the arrow keys
no longer pan the camera); in 3D, **Q/E** rotate the heading and **R/F/N**
reset / fly back to the map centre / align north via animated fly-to.
**1/2** select 2D/3D;
**T** toggles.

## 13. Overlays, Tools & Platforms

### 13.1 Overlays

Independent rendering layers that can be toggled on/off:

- **Geographic** — state lines, county lines, cities, labels, other radar site
  markers.
- **National mosaic** — a CONUS-wide quality-controlled base-reflectivity
  composite, fetched live and refreshed periodically. Rendered with a **circular
  cutout** around the focus site so its render isn't obscured.
- **NWS active alerts** — polygon footprints of active alerts, plus a top-bar
  chip surfacing the highest-severity alert intersecting the visible map.
- **mPING storm reports** — crowdsourced surface observation markers.
- **My Location** — a one-shot GPS dot for the user's current position.

**Where the chrome sits.** Everything over the map that is not data has one
place, decided by one layout rule (`core::map_chrome`) on every canvas from a
390 px phone to a desktop: the map toolbar top left, the colour scale top
right, and a **bottom tray** along the map's bottom edge for the scale bar (the
compass in 3D) and the layer legends. The scale bar sits at the tray's left
end; the legends pack in from its right end, the mosaic legend in the corner
and the mPING legend next in. A legend that does not fit beside the others
wraps to a row above, again from the right, and a row that reaches up beside
the colour scale stops short of it. Nothing overlaps and nothing leaves the
canvas; an item with no room at all is left out. The canvas caption rises over
the tray where it would cover it.

**Labels give way to chrome and to each other.** A map label (city, state,
site) never prints under the toolbar, the colour scale or the tray, or half off
the canvas: it is hidden, not nudged, so a label always marks its place. A
radar site's ID label is hidden when a city label or another site's label sits
within 12 px of it (KFSD beside Sioux Falls, KOAX beside Omaha); the site's
dot stays, and the label of every site on the map always prints.

### 13.2 Tools

Interactive measurement and analysis affordances on the 2D canvas:

- **Data probe** — hover for lat/lon, azimuth/range, the underlying gate value,
  and the radial's collection timestamp; where discs overlap, one line per
  site under the cursor, the focus first. (Named "Data probe" to free
  "inspector"
  for the scan inspector in §4.)
- **Distance Measure** — click two points to read the great-circle distance
  (km, nm and mi; the readout sits past the end point, or the first spot
  beside the line that covers neither end nor the map chrome). A third click starts over. Dragging
  an end moves it; the map pans only when the drag starts away from both
  ends.
  - **Snap.** With storm cells and Tracks on, an end placed or dropped
    within 16 px (24 px on the phone) of a lettered track's centroid snaps
    to it and shows the letter beside its marker; a ring under the pointer
    previews what a click would snap to. Only lettered tracks snap: a cell
    seen once, with no letter yet, does not. An end placed on the My
    location dot snaps to it and follows the fix.
  - **Follow.** A snapped end follows its storm's centroid as the volume
    changes, scrubbing or live, and the line and readout follow it. A
    coasting track keeps it. When the track ends (missing from a finished
    volume; also scrubbing back before it had a letter, turning Tracks off,
    or changing site) the end lets go where the storm was last seen and
    draws muted, still lettered; the readout says `Track B ended: point
    stays where it was last seen`. Dragging a snapped end away unsnaps it.
  - **Readout.** Under the distance, a storm to a fixed point (a map point,
    My location or an ended track) adds its closest approach from the
    track's motion: `Closest approach 4.2 km in 18 min · high confidence`;
    "Closest approach: motion not known yet" while the motion is below the
    forecast arrow's confidence, "Moving away: closest approach has passed",
    "Closest approach more than an hour out", or "B is stationary". Two
    storms add how the gap changes: `Converging at 12 km/h · closest 3.1 km
    in 25 min`, `Diverging at 8 km/h`, "Holding about the same distance"
    (under 2 km/h), or "A and B: motion not known yet".
  - The measurement is not carried in the link.
- **Storm Cells** (Layers › Analysis; over any product, 2D only). The
  detector runs on the volume's lowest reflectivity cut whatever product or
  tilt is on screen, so the cells stay over velocity, CC and the rest (a
  couplet inside a cell). It segments that sweep at nested thresholds 5 dB
  apart and reports *cells* (the finest distinct storms) and *groups* (one
  envelope holding two or more cells: a supercell line, an MCS, a
  hurricane's shield).
  - **Footprint.** A cell's footprint is its share of the envelope: its core
    (the echo at the level where it became distinct) grown outward over the
    envelope, strongest echo first, so the cells of a group tile it and two
    storms split along the weakest echo between them. A lone cell's footprint
    is its whole envelope. Area, mean dBZ, centroid and the hit test refer to
    the footprint.
  - **Outline.** What is drawn picks out the storms without interior
    borders. Every envelope, a group's or a lone cell's, draws as one faint,
    thin solid ring (1 px; 1.5 px on the phone, at about two thirds of the
    cell stroke's opacity over a thin halo, simplified to about 2.5 km so a
    rainband reads as a smooth ring). Inside it each cell draws its core:
    for a cell in a group, the echo at the level where it became distinct;
    for a lone cell, the echo around its peak at the core level (envelope
    + 10 dB). Cores are one light stroke over a thin dark halo (1.6 px; 2 px
    on the phone) so they read on every part of the palette. Intensity is
    never the stroke's color.
  - **Chip.** The cell's peak dBZ sits in a small mono chip at the outline's
    top-right (11 px; 12 px on the phone), or its bottom-right when that
    spot is taken. Chips are placed strongest first and skipped when both
    spots would crowd one already placed; an outline smaller
    than about 14 px on screen carries none. With Tracks on the chip leads
    with the track's letter (`B 58`) and ends with ↑ or ↓ when the peak
    moved more than 3 dB over the last three volumes.
  - **Card.** On the desktop, hovering inside a footprint opens a card beside
    the pointer: peak, area, mean dBZ, the level at which the cell became
    distinct from its neighbours ("Distinct at 50 dBZ"), group membership
    ("1 of 5 in this group") and the bearing and range from the radar. On the
    phone a tap inside a cell selects it (its outline thickens) and a compact
    card sits above the bottom of the map; a tap anywhere else closes it.
    When footprints nest, the strongest cell wins. With the data probe on,
    the probe names the cell instead (`Cell 62 dBZ · 48 km²`, `Cell B …`
    when tracked) and the hover card stays away. With Tracks on the card
    also gives the trend in its title (`D · 64 dBZ peak ↑ from 60`), motion
    (`Moving NE 47 km/h, high confidence`, or `Stationary`), age (`Tracked
    6 volumes · 25 min`) and, when My location is on, the cell's distance
    and bearing from you and its closest approach (`Closest to you: 14 km
    in 22 min`; withheld beyond an hour, "Moving away from you" when it
    is). The phone's card folds motion and age into one line ("Moving NE 82 km/h, high confidence · 34 min").
    For closest approach to any other point, or between two storms, snap a
    distance measurement to the track (Distance Measure, above).
  - **Column** (the card's vertical extent). Once the volume's higher
    reflectivity tilts are on device, each cell's footprint is sampled on
    every tilt (one cut per tilt), and the card adds the cell's echo top
    and the strongest echo in its column with its height (`Echo top ~13 km
    · max 63 dBZ at 3 km`; `≥` when the highest tilt still has echo, so the
    real top is above). The echo top is the highest sample of at least
    18 dBZ over the footprint (from a tilt with three or more such gates);
    heights are beam heights above the radar (4/3 earth radius); values pass
    a three-gate median along each radial so a single gate cannot set them.
    It is the column over the lowest-cut footprint, so a storm that leans
    far off it is sampled only where it overhangs it. While the volume's
    tilts are not all on device (a live volume still arriving, a volume
    fetched cut by cut) the line says "(partial volume)"; the phone folds it
    into one line. The columns come after the cells, so they never delay
    them, and only for the displayed volume. Nothing on the map changes
    (no 3D drawing).
  - **Tracks** (the checkbox under the slider). Cells are tracked across
    volumes (SCIT-style: predicted from their recent motion, matched by
    distance, area and peak, coasting two volumes before a track ends).
    A track gets a letter (A–Z, then AA, AB…) once it has been seen in
    two volumes, or at once when it splits off a lettered storm; a cell
    seen only once shows its peak alone and uses no letter up, so letters
    go only to storms that last. A letter stays with its storm and is
    never handed out again until the letters start over (another site is
    picked, or the map goes from one site to several or back). The storm's
    path, every centroid seen (up to 24, about two hours), draws as small
    dots on a thin
    line that fades with age, newest strongest; a track that split off
    another starts with a short dashed join to the parent's centroid at the
    split. A dashed arrow points to where the cell will be in 30 minutes
    once its motion is confident (three volumes on a steady course).
    Turning tracks on also detects up to twelve volumes (about an hour)
    before the one on screen, nearest first, among those already on the
    device; it never downloads volumes just for a path. The volume on
    screen does not wait for them: its tracks gain motion and path as they
    land.
    Scrubbing back over volumes already seen, or a loop wrapping, shows
    the same letters. A volume whose cells arrive late (a loop that runs
    ahead of detection) is tracked when they do, and the volumes after it
    are tracked again; letters already shown stay with their storms.
    Scrubbing more than 20 minutes before the first volume seen
    starts over with new letters.
  - **Rays and live.** Rays shows the previous sweep ahead of the sweep line
    and the current one behind it; cells belong to the current sweep, so a
    cell draws only once its whole footprint is behind the line and never
    sits on another sweep's pixels. Cells on a live sweep that is still
    arriving are provisional: muted outline, hollow chip, and the card says
    so.
  - **Slider.** "Min dBZ" (20–60, default 35) is the envelope: the weakest
    echo that can belong to a cell. A cell needs a core 10 dB above it. The
    slider works over every product; off reflectivity it notes "Cells from
    reflectivity".
  - **Several sites.** With more than one site on the map, the cells are
    detected once on a composite of every site's lowest reflectivity cut:
    one 500 m grid around the echo, taking the higher reflectivity where
    discs overlap. A storm two radars see is one storm with one outline,
    one chip and one letter, and the tracks, paths and arrows come from
    one tracker over the whole map. A storm is seen anew only when the
    radar that shapes it cuts again, so on the composite a track coasts at
    least 13 minutes before it ends, and a new storm gets its letter at
    that radar's second cut. The card measures bearing and range
    from the nearest site that sees the cell and ends with the sites that
    see it (`KTBW · KMLB`). These cells have no column yet: the card
    leaves out the echo top and column line, which are one radar's. In
    Rays a cell waits for the sweep line of every site that sees it.
    Distance measurements snap to these tracks. Going from one site to
    several, or back, starts the letters over and lets snapped endpoints
    go; adding a third site or taking one of three away keeps them. With
    one site nothing changes: cells come from that radar's own sweep at
    full resolution.
  - **3D** draws no cells, card or probe line.
  - The link carries the overlay as `cells=<dbz>` and tracks as `tracks=1`
    (§13.4).

### 13.3 Responsive behavior

| Breakpoint | Timeline | Notes |
|---|---|---|
| Desktop ≥1200px | Overview + full strip (~56px) + transport row + readouts | Inspector as popover or side panel; hover tooltips |
| Touch, short side ≥500 pt | Desktop layout with larger targets | Sheets instead of side panels; pinch zoom. (A distinct tablet tier is out of scope this pass — §16.) |
| Phone (a touch screen whose short side is under 500 pt, in either orientation; or any window under 500 px wide) | A 120 px dock: a 66 px row of volumes at three zoom stops over a transport row (play · selection chip · NOW / 3 H / 24 H · LIVE when not live); the calendar sheet stands in for the overview | Canvas full-bleed; turned on its side the phone keeps this layout (map full width, the top bar above, the dock along the bottom, its row's blocks spread across the width) and its sheets move to a full-height side sheet on the right that scrolls; a mouse on a short wide window stays desktop; chrome auto-hides during playback, tap to reveal; ranges are never drawn (presets and the long-press sheet; a range shows as one amber bracket); one site on the map (a link's `also` is ignored); 3D view is desktop-only |

**The phone timeline is a row of volumes you can touch.** Not a ruler with
marks on it: each volume is one mark, and everything the row says is in how
the marks look. At **NOW** each volume is a rounded block as wide as its span
(less a 3 px gap): **filled** slate when it is on the device, with faint
separators between its cuts, **hollow** when it is not; the one under the
playhead has a **white outline** (a filled one turns lighter); the **live**
volume grows from its start to the red now line, filled by the share of its
cuts received. Its start time sits under each block (`23:49`, `live` under
the live one; the selected block's label reads brighter, and labels give
way to each other rather than overlap). At **3 H** each volume is an 11 px
column as tall as the weather it held (its file size in the day's listing,
on the view's scale) over a 6 px floor, so a quiet day is a row of short
even columns; filled on the device, white under the playhead, with hour
labels. At **24 H** the storm hill is a smooth area through its half-hour
samples in the storm blue (it never overshoots a sample or dips below the
floor), the stretch on the device drawn in slate and a 2 px white line
at the playhead, labelled 06 · 12 · 18 · 00. At every stop a red 2 px now
line with a small `NOW` cap marks now when it is in the window, and a
selected range is **one amber bracket** above the marks (its end on the now
line when it is pinned to now); its ends are not draggable. There is no
ring, no playhead line at NOW or 3 H, no on-device underline, no ruler, no
dashed pending ring, no now tab and no range handles, and the phone draws no
canvas caption.

A thumb gets three stops rather than a continuous zoom: **NOW** (four
blocks: three complete volumes and the live one, with the live volume's
whole expected span kept so now sits about 80 % across and the row holds
still while it grows, widened to show a whole selected range), **3 H** and
**24 H**. A pinch moves one stop and settles on release; the small
NOW · 3 H · 24 H control in the transport row shows the stop and taps to
another. A **drag** scrubs cut by cut inside a block on the device and volume
by volume elsewhere (just inside a volume's start, its first cut), with a
still finger holding the playhead, so when a volume arrives its cuts appear
in place and nothing jumps; a **tap** goes to the tapped volume's first cut
and drops the pin; a **long-press** opens the sheet below. Pinned, now sits
near the right edge; at NOW the row steps one volume when the next begins,
and at 3 H and 24 H it slides with now; detached, the window
centres on the playhead when it lands somewhere new, holds still while you
scrub inside it and slides near the edges. Zoom is independent of the
selection, and landscape keeps the same spans with more pixels per unit. The
row takes presses up to 12 px above it, and the map neither pans on those
nor zooms on the row's pinch.

**Loading is shown, not announced.** While the frame under the playhead is
not on the device (a tapped or scrubbed hollow volume, a day from the
calendar), its block turns white at once and carries a progress arc (its
share is the bytes received over the volume's size while it downloads,
from 45 % to 80 %, with the stage standing in where no size is known:
queued 15 %, downloading 45 %, decoding 80 %; with nothing known it spins), the map dims 40 % with the old frame still visible under
it, and a small pill in the middle says `Loading 23:49`. The chip's outline
is dashed meanwhile and the top bar's clock already reads the time asked
for. When the frame lands the dim lifts and the block fills.

**The transport row** is four things: a 42 px round play/pause; the
**selection chip**, a 38 px pill with one short form per state that never
truncates — red filled `● Live`, amber `Last 4 frames` or `11:28 → 11:34`
for a loop, grey `23:49` for a point — which opens the chip sheet (below); the
NOW · 3 H · 24 H control; and a red outlined **LIVE** only while the
selection is not pinned to now (it rejoins a running stream or starts one,
bringing now into view first). Stepping is a tap on the next block, the
speed is in the sheet's View tab, and failures are a dot on the bell.

**The top bar** says what you see, once: the site chip, one clock (the frame
on screen, `23:49:40`, the zone a small suffix; dated `Oct 19 · 23:49:40`
only when the frame is not from today; a tap toggles local / UTC), one age
pill (red `● LIVE` when live, grey `20 min ago` at a point, amber
`⟲ 13 min ago` during a loop), the bell (the alerts in view, with an amber
dot when a download failed or an error is unseen; while the dot shows a tap
opens the activity sheet) and ≡, the sheet. The lag lives in the age pill,
not the chip.

**The calendar sheet is the phone's overview ("Go to a day").** There is no
room for an overview lane on a phone, so the way to any day since 1991 is a
calendar: a bottom sheet in portrait, a right sheet in landscape, titled "Go
to a day" with the site at the right. It opens from the chip sheet's "Go to
a day…". A
month at a time, Monday first, every day is a 44×32 cell shaded in storm blue
by its activity from the nightly index, on the month's own scale so one storm
day reads strong; a 2 px slate underline marks the days held on device; today
is outlined in red (red is now); days after today are dimmed and inert, and a
day the index does not know yet (newer than it, or a gap) is drawn as a
hairline, never as a quiet day. ‹ › and a horizontal swipe move a month
(June 1991 to this month); the month name opens the years the site's index
reaches, a year opens its twelve months, a month its grid. Days are the
chosen zone's (local or UTC). The grid makes no S3 request: it reads the
index already on device (fetched once per site, as the desktop does). A
tapped day stops a running stream (an archive day is not live, so nothing
downloads behind it) and puts the row at **3 H**, centred on the landing volume with its
columns around it and LIVE in the transport row, and pauses on that
day's stormiest volume (its largest file) from the one listing the strip
makes for that day; until that
listing arrives the playhead waits at the day's local midday, then moves to
the stormiest volume once, unless the user has touched the playhead since.
While that volume downloads the playhead waits just inside its start (10 s
in, as a scrub lands there, never on the listed time); when it is in,
the playhead steps onto its first frame so the map shows it and its
column fills.
The sheet's month is not persisted or in the link.

**The calendar on the desktop.** The desktop has the overview lane, but
reaching a day years back by zooming it is slow, so the same calendar, for
the focus site, opens as a popover above the playhead clock: from the
calendar button beside the
clock, from **G**, or from the chip menu's "Go to a day… G". A picked day
lands as on the phone (stream stopped, paused on the stormiest volume) and
the strip shows that whole day with the overview at 48 h around it. Under
the grid, "Or type a time" is the clock's own field: a date and time typed
there (`2024-10-09 23:30Z`, `Oct 9 2024 7:30 PM`) lands exactly there, paused,
with the stream stopped and the overview framed around it.

**The chip sheet.** On the phone a tap on the selection chip opens a bottom
sheet (the calendar's component, on the Esc stack) titled "Playing" in place
of the desktop's chip menu, with the verbs first: **Live** · the newest frame
(red glyph); **Loop the last** · 4 · 6 · 10 frames, the numbers inline and
the one in use bright (amber glyph; a tap off the numbers loops the length in
use, or 4); **Loop what I see** · the span of the row's window (amber glyph;
off when the window holds nothing yet); **Go to a day…**; and **Stop the
stream**, only while a stream runs. The row that names what plays now is
highlighted (Live, Loop the last, or Loop what I see for a fixed range).
Every row is built from the same menu model as the desktop's menu and does
what its entry does; the desktop keeps its menu.

**Ranges without drawing.** On the phone a horizontal drag on the strip is a
scrub, so a range is never drawn. It is made from the chip sheet or from the
**long-press sheet**. A long-press on the strip opens a small sheet about the
volume under the finger, headed by the time, the tilt and the product
(`16:42:17 · 0.5° · reflectivity`), with three rows: **Loop from here** · to
now (amber glyph: a range pinned to now starting at that volume's start),
**Loop to here** (only for a loop pinned to now or a start parked with the
desktop's I key: it keeps the start and ends at that volume's end, a fixed
range however near now, unless it is the newest volume, which ends at now)
and **Inspect this scan**.
A row that makes no sense is disabled and says why at its right (this is
now; start a loop first; before the loop's start; longer than 7 days; no
scan here). A new range starts paused, as on the desktop. Once a range
exists the row shows it as one amber bracket at every stop; its ends are
moved from the sheet, not dragged. A range over **2 hours** on the phone (6
hours on the desktop) asks before it downloads, and the confirmation says the
real size from the listing. Saving a range as an event is a desktop action.

### 13.4 Links and preferences

One rule: **the link is what you see; preferences are how you like it.** The
URL is rewritten with `replaceState` about once a second, so any view is
shareable as a single link and a reload reopens it. The codec and the boot
decision are `core::link`; preferences are `core::prefs`.

**In the link**

| What | Key | Notes |
|---|---|---|
| Site | `site` | The focus. An unknown site falls back to the preferred site |
| Other sites on the map | `also` | In order (`also=KMLB,KJAX`). Unknown sites and those over the layout's cap are dropped; ignored on the phone |
| Playhead time | `t` | Unix seconds; not written while pinned |
| Selection | `sel` | `now` (pinned point), `last6f` / `last1800s` (range ending now), `START~END` (fixed range); absent = a point at `t` |
| Play mode | `mode` | `frames` or `rays` |
| Product | `product` | Short code: `REF`, `VEL`, `SW`, `ZDR`, `CC` or `KDP`. A product that is not offered (`CFP`) is ignored and the default applies |
| Tilt | `tilt` | An angle (`0.5`) or `latest` |
| Map center | `lat`, `lon` | The map's own centre; absent, the map centres on the focus site |
| 2D/3D, camera, map and timeline zoom, volume rendering and cutoff | `v` | Base64 JSON blob; its old `rt` flag is read as `sel=now` |
| Storm cells | `cells` | Present means on; the value is the envelope threshold in dBZ (`cells=35`). Absent leaves the preference |
| Storm cell tracks | `tracks` | `tracks=1` on, `tracks=0` off; written only while cells and tracks are both on. Absent leaves the preference |
| Developer flag | `dev=true` | Wins for that session without being saved |

A link with a pinned selection opens pinned; one with a time (or a fixed
range) opens there, detached. Opening with no link opens pinned to live at the
preferred site, or with the site picker on a first visit. `?ui=` is retired
and ignored.

**In preferences** (localStorage, versioned)

| What | Notes |
|---|---|
| Layers, rendering (interpolation, opacity, age desaturation) | |
| Speed per mode | One for Frames, one for Rays |
| Time zone | Local or UTC |
| Data-saving policies | Auto-fetch while scrubbing; pause stream while reviewing |
| Loop style | Loop, ping-pong or once |
| Panel tab and collapse | |
| Tools | Data probe, distance, storm cells and their dBZ threshold |
| Developer mode | Settings › Developer |
| mPING key, preferred site, recent sites | |
| Storage quota | Its own key |

Older preference blobs are migrated once: the single speed is filed under the
mode it was saved with, the play mode and tilt they carried move to the link,
and the retired Basic/Advanced setting is read and discarded, so every control
is visible. **Reset** lists what it clears and keeps, keeps the mPING key and
the preferred site unless asked, and reloads with a clean URL.

## 14. Progressive Disclosure Ladder

Depth replaces modes: there is no Basic/Advanced switch. Every surface is
simple at a glance and reveals more where you look harder, and the same
controls exist at every width — the phone leaves out only the keyboard map and
the Developer row. Developer mode (Settings › Developer, or `?dev=true`) only
adds diagnostics.

- **Level 0 (first launch / casual):** canvas + simple scrubber + LIVE + play.
  Everything automatic. No jargon — "frame," not "sweep." The VOLUMES/TILTS lane
  headers are absent; structure is conveyed by containment.
- **Level 1 (via zoom):** scan structure, ghosts + countdown, loop presets, speed
  control.
- **Level 2 (power):** scan inspector, manual fetch, activity sheet + policies,
  keyboard map, UTC. Vocabulary may use real terms (tilt, VCP).

## 15. Risks, Accessibility & Mitigations

| Risk | Mitigation |
|---|---|
| "Christmas tree" timeline | ≤3 accents at once; neutral fills/textures elsewhere; grayscale test; dark muted chrome vs. vivid reflectivity palettes |
| Snap flicker at zoom boundary | Hysteresis (different enter/exit thresholds) |
| Mode-switch disorientation | Morph animation; playhead + live edge spatially stable landmarks |
| Equidistant playback hides outages | Gap glyphs between distant ticks |
| Acquisition motion reads as alarm | Motion = routine; red = failure/live only |
| "Live" feels laggy in severe weather | Data-age readout; ghost countdown sets expectations |
| Accidental mass downloads | Implicit/explicit acquisition split; concurrency + session-volume caps; bulk-confirm threshold |
| Accessibility | State = fill + shape, never hue alone; reduced-motion replaces pulses with static progress; full keyboard operation; screen reader announces the displayed-frame timestamp |

## 16. Deferred / Future Work

Explicitly out of scope for the current pass — recorded so the boundary is clear,
not unfinished work:

- **Tape-scrub (mobile):** a fixed-center playhead with the "tape" dragging
  beneath it on phones — more thumb-accurate, but inverts the shared mental model.
  Ship drag-anywhere-scrubs first; A/B the tape later.
- **Tablet tier:** a layout distinct from desktop/phone. For now touch devices
  whose short side is 500 pt or more get the desktop layout.
- **Offline / stale ghosts:** today ghosts vanish when the stream dies; richer
  handling of stale projections is a post-v1 revisit.
- **Wi-Fi-only acquisition toggle:** not implementable in a browser today; revisit
  if a reliable network-type signal becomes available.
- **Live loop-preset discoverability:** the July 2026 UX audit asked for "a play
  option that plays within X time or frames of *now*" while streaming — which is
  exactly what the loop presets already do (Pin to live / Last 4-10 frames /
  Last 30 min / Last 1 h, §7). The capability exists and is one tap away; what is
  missing is that a user watching live never learns it is there. Addressed by
  the selection chip (Oct 2026): the presets sit in the same menu as LIVE, read
  "Last 6 frames → now", and always attach to the live edge.
- **Blended compositing:** where discs overlap, the newer frame is drawn on
  top. A blend (maximum reflectivity, distance weighting) would be drawn on
  the composite grid the storm cells already use (§13.2).
- **An overview lane per site:** the overview's bars stay the focus's.
- **Listing only the focus's days:** every site on the map lists the days in
  view, so three sites make three times the archive listings. Narrowing the
  listings to the focus is worth doing only if that cost shows.

---

## Alignment decisions (June 2026) — decision record

This section is the rationale ledger for the timeline/playback/acquisition UX
decisions the implementation follows. The spec sections above are the design;
the entries here record *why*, with stable §-numbers the codebase cites as
"alignment §N" / "#N". (Merged here from the former `north_star_alignment.md`,
deleted in the June 2026 docs cleanup.)

1. **Default live experience:** tethered **single-frame following**. The pinned
   loop is opt-in via the loop preset control, one tap away. Play/pause while
   tethered follows §6's pause semantics (freeze + behind-live counter), not loop
   entry.
2. **Zoom tier thresholds + hysteresis:** one stored tier state machine owns the
   boundary. Micro↔Macro nominal 1.0 px/s with ~±15% hysteresis (enter Micro ≥
   1.15 px/s, exit Micro ≤ 0.87 px/s). Values are tunable constants in one place.
3. **Canvas treatment while fetching:** **caption only** ("showing 2:41 ·
   fetching 2:51…"), no canvas shimmer. Cheapest, honest, reads in grayscale and
   to screen readers.
4. **Chunk segmentation:** display 3 vs 6 chunks **faithfully** (the live
   behavior). Archive downloads have no chunk telemetry; in-flight archive cells
   use a pulse fill inside the frame cell instead of fake segments.
5. **Background streaming on open / site switch:** the app opens **tethered to
   live**. A site switch while tethered re-tethers on the new site; with
   several sites on the map every site streams (§6). While detached,
   streaming continues in the background; a safety
   idle-stop applies after 60 min detached (a pragmatic S3-cost bound; the
   data-saver toggle is the user-facing control).
6. **Prefetch across tilts on product/tilt switch:** no proactive refetch.
   Archive scans download whole volumes, so cached scans already have all tilts;
   the existing playhead-window prefetch covers the rest.
7. **Loop window defaults:** default preset **last 6 frames**; the preset menu
   offers 4 / 6 / 10 frames and 30 min / 1 h durations plus "pin to live". Same
   defaults on all device classes.
8. **Offline / stale ghosts:** ghosts are emitted only while the projection
   engine has a live stream. When the stream dies, ghosts disappear rather than
   going stale. Revisit post-v1 (§16).

### Scope & interpretation calls

- **Frames-first strip:** done — the design's biggest simplification win.
- **Custom loop dragging on mobile:** cut; mobile gets the chip's presets, the
  selection strip is desktop-only.
- **Year-scale reach:** the strip stops at 3 days; the overview above it
  zooms out to the whole NEXRAD Level II era (1991→now), coarsening its bars
  from scans to quarters rather than stretching a strip.
- **Storm-activity bars:** the overview's bars are as tall as their stormiest
  day, and zoomed in each volume is as tall as its own size, using archive
  file sizes as the activity signal (no backend, no downloads).
- **Manual download management:** stays demoted; the activity sheet shows what the
  system did, plus cancel/retry and policy toggles.
- **Tablet tier:** out of scope this pass; touch devices whose short side is
  500 pt or more get the desktop layout.
- **Wi-Fi-only toggle:** not implementable in a browser (no reliable
  network-type API); the activity sheet ships "auto-fetch while scrubbing" and
  "pause live stream while reviewing".
- **Scan inspector entry:** right-click a scan (desktop), long-press (touch). The
  old map-probe "Inspector" tool is renamed "Data probe" to free the word.
- **Failure model:** a failed download no longer error-pauses the whole queue;
  failures are per-cell (alert tick, tap to retry) and retry re-enqueues.
- **Frame definition:** everywhere (macro frame list, lookback window, stepping) a
  frame is a sweep matching the selected **product + tilt**.
- **Top readouts:** the displayed-frame timestamp is the primary top-bar readout;
  tap toggles local/UTC (local default). "Updated 1m ago" age appears when
  tethered.
- **Keyboard:** ←/→ frame step, Shift+←/→ scan step, I/O loop in/out, plain L
  go-live, +/− timeline zoom anchored at the playhead, Space play/pause, [ / ]
  speed. Camera pan keeps WASD; arrows no longer pan the camera.

---

*Consolidated June 2026 from the original product requirements draft
(2026-05-23) and the timeline/playback/acquisition UX brief (v0.1, 2026-06-12;
open items resolved in the June 2026 alignment pass). All resolved values reflect
the shipped implementation on `simplify-user-interface`.*
