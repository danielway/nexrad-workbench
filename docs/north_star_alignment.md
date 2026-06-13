# North-star alignment — decisions log

Companion to `product_north_star.md` (v0.1). That brief left several items ⚠️ OPEN
and the codebase deviated from it in known ways. This file records the product
decisions taken for the alignment pass of June 2026, so the spec's open items have
one canonical answer and QA knows what behavior is intended.

## Decisions on the spec's open questions (§17)

1. **Default live experience** (§17.1): tethered **single-frame following**.
   The pinned loop is opt-in via the loop preset control ("pin to live",
   "last N frames"), one tap away. Play/pause while tethered follows §7's
   pause semantics (freeze + behind-live counter), not loop entry.
2. **Zoom tier thresholds + hysteresis** (§17.2): one stored tier state machine
   owns the boundary. Micro↔Macro nominal boundary 1.0 px/s with ~±15%
   hysteresis (enter Micro ≥ 1.15 px/s, exit Micro ≤ 0.87 px/s). Archive is
   span-based: enter when the visible span exceeds 60 h, exit below 48 h.
   Values are tunable constants in one place.
3. **Canvas treatment while fetching** (§17.3): **caption only**
   ("showing 2:41 · fetching 2:51…"), no canvas shimmer. Cheapest, honest,
   reads in grayscale and to screen readers.
4. **Chunk segmentation** (§17.4): display 3 vs 6 chunks **faithfully** (already
   the live behavior). Archive downloads have no chunk telemetry; in-flight
   archive cells use the pulse fill inside the frame cell instead of fake segments.
5. **Background streaming on open / site switch** (§17.5): the app opens
   **tethered to live** for the selected site (per §7 DECIDED). Site switch while
   tethered re-tethers on the new site. While detached, streaming continues in
   the background; a safety idle-stop applies after 60 min detached (raised from
   15 min — pragmatic S3-cost bound the spec doesn't forbid; the data-saver
   toggle is the user-facing control).
6. **Prefetch across tilts on product/tilt switch** (§17.6): no proactive
   refetch. Archive scans download whole volumes, so cached scans already have
   all tilts; the existing playhead-window prefetch covers the rest.
7. **Loop window defaults** (§17.7): default preset **last 6 frames**; preset
   menu offers 4 / 6 / 10 frames and 30 min / 1 h durations plus "pin to live".
   Same defaults on all device classes.
8. **Offline / stale ghosts** (§17.8): ghosts are emitted only while the
   projection engine has a live stream. When the stream dies, ghosts disappear
   rather than going stale. Revisit post-v1.

## Scope decisions (per §15 cut order)

- **Frames-first strip**: done (cut order says "do regardless").
- **Custom loop dragging on mobile**: cut; mobile gets presets + handles remain
  desktop-only.
- **Radial-level canvas animation**: deferred to v2 (existing sweep animation
  stays, gated as today).
- **Year-scale strip zoom**: removed, replaced by the Archive calendar tier.
- **Manual download management**: stays demoted; queue sheet shows what the
  system did, plus cancel/retry and policy toggles.
- **Tablet tier** (§13 row 2): out of scope for this pass; touch devices ≥600 px
  get the desktop layout as today.
- **Wi-Fi-only toggle** (§10): not implementable in a browser (no reliable
  network-type API); the queue sheet ships "auto-fetch while scrubbing" and
  "pause live stream while reviewing" (data-saver). `navigator.connection.saveData`
  may later seed the data-saver default.

## Other interpretation calls

- **Keyboard**: ←/→ frame step, Shift+←/→ scan step, I/O loop in/out, plain L
  go-live (one-way re-tether), +/− timeline zoom anchored at the playhead,
  Space play/pause. Camera pan keeps WASD; arrows no longer pan the camera.
  Speed up/down moves to [ / ].
- **Scan inspector entry**: right-click a scan (desktop), long-press (touch).
  The old map-probe "Inspector" tool is renamed "Data probe" to free the word.
- **Failure model**: a failed download no longer error-pauses the whole queue;
  failures are per-cell (alert tick, tap to retry) and retry actually re-enqueues.
- **Frame definition**: everywhere (macro frame list, lookback window, stepping)
  a frame is a sweep matching the selected **product + tilt** (§4).
- **Top readouts**: displayed-frame timestamp becomes the primary top-bar
  readout ("2:41:07 PM CDT · 0.5°"), tap toggles local/UTC; local is the
  default. "Updated 1m ago" age appears when tethered.
- **Jargon at Level 0**: the VOLUMES/TILTS lane headers are removed; structure
  is conveyed by containment, vocabulary by the inspector.
