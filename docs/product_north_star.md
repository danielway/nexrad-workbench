# Weather Radar Viewer — Timeline, Playback & Acquisition UX

**Product brief · v0.1 · June 12, 2026 (open items resolved June 2026)**
**Status:** Implemented on `simplify-user-interface`. The June 2026 alignment
pass resolved the spec's open items — see **Alignment decisions** at the end.
One forward-looking experiment (§13 tape-scrub) remains flagged ⚠️ OPEN.

---

## 1. Summary

A responsive weather radar application centered on a full-bleed canvas that renders radar sweeps, with a zoomable timeline that serves as the single shared map of three decoupled systems: **acquisition** (live streaming + archival fetch), **cache** (what is downloaded locally), and **playback** (what the canvas renders). The design north star is *powerful but invisible*: rich data sources, cache state, and acquisition activity are all legible at a glance, yet a first-time user sees only a familiar video-player-like scrubber. Depth is revealed spatially through zoom rather than through menus.

This brief covers the timeline system, the live/tether model, looping, playback semantics, acquisition transparency, interaction, and responsive behavior. It does not cover radar rendering, product/colorimetry, or data pipeline architecture except where they constrain UX.

## 2. Goals

1. Surface the available data sources (live stream + archive) and the local cache transparently, without requiring the user to manage them.
2. Make acquisition activity (what is downloading, what is queued, what is coming next) visible in place, ambiently, and honestly.
3. Provide flexible playback: scrubbing, variable speed, frame stepping, looping (including a live-updating loop), and live following.
4. Scale gracefully from a casual "what's happening right now" glance to power-user archival analysis — and from desktop to phone.

**Non-goals (v1):** multi-radar mosaic timelines, collaborative sessions, alerting/notifications, offline-first guarantees beyond the existing cache.

## 3. Design principles

1. **Decouple acquisition, cache, and playback.** Acquisition is the library receiving books; playback is reading. Pausing reading never stops deliveries. The timeline is the shared map where both are visible.
2. **Borrow the video-buffer grammar.** Gray = available, filled = buffered/downloaded, playhead = position, red dot at right edge = live. Users arrive already fluent; our timeline is "a buffer bar that learned the data's structure."
3. **The canvas never lies about time.** The displayed frame's timestamp is the primary readout. Any discrepancy between playhead time and rendered-frame time is surfaced, never hidden.
4. **Zoom is the disclosure mechanism.** Detail emerges spatially (semantic zoom), not through settings or modes the user must discover.
5. **One visual channel per concern.** Cache state → fill; acquisition activity → motion; user intent → overlays; data structure → containers. No channel double-booked.
6. **Motion means routine work; red means failure or live.** Pulsing is never an alarm. Red is reserved for the live edge and for errors — nothing else.

## 4. Domain & terminology

| Term | Meaning |
|---|---|
| **Scan** | A full volume scan, ~5 minutes, composed of sweeps. |
| **Sweep / tilt / elevation** | One 360° rotation at a fixed elevation angle. SAILS-style VCPs may revisit low tilts mid-volume. |
| **Radial** | A single beam of data within a sweep. |
| **Chunk** | Live-streaming delivery unit; 3 or 6 chunks compose a sweep. |
| **Frame** | UI term: a sweep matching the *currently selected product + tilt* — i.e., something the canvas can render. The timeline's primary unit. |
| **Live edge** | The right boundary of acquired data; "now" minus dissemination latency. |
| **Tether** | Whether the playhead is following the live edge. |
| **Loop window** | A time/frame range playback cycles over; may be pinned to the live edge. |

## 5. Screen architecture

- **Canvas** — full-bleed, dominant. Chrome is dark and muted so reflectivity palettes own the color space.
- **Top readouts** — radar site, product/tilt selector, **displayed-frame timestamp** (primary), data age when live ("updated 1m ago"), local time with UTC on tap.
- **Bottom cluster** — transport row (play/pause, frame step, speed, loop preset, LIVE button) above the timeline.
- **Timeline** — a thin **minimap** sliver (whole-session coverage, doubles as fast navigation) above the **main strip**.
- **Status chip** — tiny, near the transport; hidden when idle; "↓ 2" with spinner when acquiring; tap opens the queue sheet.
- **Scan inspector** — popover/sheet opened from a scan (tap/long-press): lists every sweep with tilt, size, cache state, per-chunk progress, tap-to-fetch.
- **Queue sheet** — active/queued downloads with sizes, cancel/retry, and acquisition policy toggles.

## 6. Timeline system

### 6.1 Visual channels

| Concern | Channel | Treatment |
|---|---|---|
| Cache state | Fill | Hollow / solid / segmented partial / dashed ghost |
| Acquisition activity | Motion | Pulse on in-flight cells; faint hatch on queued |
| User intent | Overlays | Playhead line + handle; translucent loop band + handles |
| Data structure | Containers | Scan blocks containing frame cells; tick rail with time labels |

### 6.2 Frame-cell states

| State | Visual |
|---|---|
| Available (server only) | Hollow outline |
| Downloaded | Solid fill |
| In flight | Segmented fill animating as chunks land (3 or 6 segments) |
| Queued | Faint hatch |
| Projected (future) | Dashed ghost at predicted time; nearest shows countdown ("0.5° in ~40s") |
| Failed | Small alert tick; tap to retry |
| Actively rendered | Accent ring; snaps cell-to-cell as the playhead crosses frame boundaries |

Accent budget: **max three accent colors visible at once** — playhead, live edge, active-frame ring. Everything else is neutral fills/textures. Strip must read correctly in grayscale.

### 6.3 Frames-first simplification ✅ DECIDED

The strip's primary cells are **frames of the currently selected product/tilt**, not the raw sweep inventory. The full volume structure renders only as a faint sub-texture inside each scan container. The **scan inspector** carries the complete breakdown (all tilts, SAILS revisits, sizes, chunk progress, manual fetch). Rationale: aligns the timeline with what the canvas can actually show; collapses the hardest density problem; the inspector serves power users better than the strip ever could.

### 6.4 Zoom tiers (semantic zoom)

| Tier | Range (approx) | Shows | Playback |
|---|---|---|---|
| **Micro** | minutes → ~1–2 hr visible | Scan containers with frame cells, chunk segments, ghosts + countdown | Realtime multiples (1×, 5×, 60×…); renders latest matching frame at/before playhead; optional radial animation |
| **Macro** | hours → a few days | Scans collapse to uniform ticks; gap glyphs where real spacing exceeds threshold | Equidistant frames at a chosen fps (classic radar loop) |
| **Archive** | beyond ~2–3 days | Calendar-style coverage heatmap (per-day availability + cache tone, bookmarks/events) | None — navigator only; tapping a day zooms into Macro there |

- **Snap with hysteresis:** enter/exit thresholds differ so pinch gestures never flicker at a boundary. ✅ DECIDED (Alignment §2): Micro↔Macro nominal 1.0 px/s (enter Micro ≥ 1.15, exit ≤ 0.87 px/s); Archive enters above a 60 h visible span and exits below 48 h. Tunable constants in one place.
- **Morph animation:** frame cells visibly collapse into scan ticks (and expand back). Playhead and live edge stay spatially stable through the transition.
- **Archive tier replaces year-wide strip zoom** ✅ DECIDED: a strip stretched across months produces label soup and disorienting travel; a calendar heatmap (GitHub-contributions grammar) answers "which days have data / weather I care about" directly.

## 7. Live streaming & the tether model ✅ DECIDED

There is **one continuous timeline**; "live" is its right edge. Live is not a mode — it's a tether state.

- **Tethered:** solid LIVE indicator; playhead rides the live edge; new chunks may animate in on the canvas.
- **Detached:** scrubbing backward auto-detaches. LIVE button hollows out and shows lag ("● LIVE · 2:14 behind"). Streaming **continues in the background** by default, filling the cache at the right edge. One tap re-tethers with a brief animated catch-up.
- **Pause while tethered:** frame freezes; a "behind live by 0:42" counter grows; resuming plays from the pause point (now detached) — detachment is made explicit via the button state change.
- **Policy:** a data-saver toggle ("pause live stream while reviewing") for metered connections, in the queue sheet. Default off.
- **Session start:** app opens tethered to live for the selected site (the dominant use case). ✅ DECIDED (Alignment §1): the default live experience is tethered **single-frame following**; the pinned loop (§8) is opt-in via the loop preset, one tap away.

## 8. Loop system

- **Creation order:** presets first ("last 4 frames," "last 30 min," "pin to live"), custom range second (alt/right-drag on desktop; I/O keys; draggable handles once a loop exists).
- **Pinned sliding loop:** dragging the right handle to the live edge snaps and pins — the handle visually fuses with the live dot, and the window slides forward as sweeps arrive. This is the core "loop the last N while still streaming" experience.
- **Window basis:** frame-count windows ("last 6 frames") preferred in Micro, since scan spacing varies; duration windows offered as an alternative in presets.
- **Incorporation rule:** newly arrived frames enter the loop **at the wrap point**, never mid-cycle, so the loop never visibly pops.
- ✅ DECIDED (Alignment §1): the pinned loop is **opt-in** via preset, not the default; default live is single-frame following (§7).

## 9. Playback semantics

- **Micro:** speed in realtime multiples. Canvas renders the latest matching frame at or before the playhead. **Radial animation** (drawing radials as chunks arrive/replay) is gated to: Micro mode + low speed (≤ ~2×) + live or recent data with chunk timing available. Ship-in-v2 candidate (§15).
- **Macro:** speed in frames-per-second; frames play equidistantly. **Gap glyphs** mark where true spacing exceeds a threshold (outages, VCP changes) so equidistance doesn't deceive.
- **Cadence preservation across the snap:** on mode switch during playback, convert the current effective frame cadence to the nearest setting in the new mode, so perceived rhythm doesn't lurch. (Micro at high multiples already approaches frame cadence — the modes meet conceptually.)
- **Adaptive speed control:** the same control shows × in Micro and fps in Macro.

## 10. Acquisition & transparency

- **Auto-fetch on seek:** the target frame fetches first, then neighbors radiate outward; a loop selection fills with priority. Policy is *legible through behavior* — cause and effect happen where the user acted.
- **Manual fetch:** tap a hollow cell (desktop) or use the scan inspector. Demoted to power-user path permanently; auto-fetch covers ~95% of needs.
- **Live-edge prediction:** dashed ghost cells at predicted arrival times derived from current VCP cadence; nearest ghost shows a countdown; transitions ghost → filling → filled. Teaches the radar's rhythm without documentation.
- **Aggregate status:** the status chip ("↓ 3" + spinner) opens the queue sheet — list, sizes, cancel/retry, toggles (auto-fetch while scrubbing · stream while reviewing · Wi-Fi only).
- **Failures:** alert tick on the cell, tap to retry. Red styling reserved for failures and the live dot only.

## 11. Canvas honesty rules

1. Displayed-frame timestamp is the primary readout (e.g., "2:41:07 PM CDT · 0.5°").
2. When playhead time ≠ frame time (undownloaded region, gap), keep showing the most recent available frame and surface the discrepancy ("showing 2:41 · fetching 2:51…"). ✅ DECIDED (Alignment §3): **caption only**, no canvas shimmer.
3. At the live edge, show data age ("updated 1m ago") — radar "live" is minutes old by nature; acknowledging it builds trust exactly when severe weather makes users notice lag.
4. Local time primary, UTC available on tap (enthusiasts want Zulu).

## 12. Interaction model

| Input | Behavior |
|---|---|
| Press/drag on strip | Seek immediately on press; drag scrubs |
| Scroll / pinch | Zoom, anchored at cursor/pinch center (with snap hysteresis) |
| Minimap drag | Pan / fast navigation |
| Alt- or right-drag | Create loop range (desktop) |
| Long-press (touch) | Open scan inspector (includes "loop from here") |
| Loop handles | Hang below the strip; ≥44pt targets |
| Keyboard | Space play/pause · ←/→ frame step · Shift+←/→ scan step · I/O loop in/out · L go live · +/− zoom |

## 13. Responsive behavior

| Breakpoint | Timeline | Notes |
|---|---|---|
| Desktop ≥1200px | Minimap + full strip (~56px) + transport row + readouts | Inspector as popover or side panel; hover tooltips |
| Tablet | Same, larger targets | Sheets instead of side panels; pinch zoom |
| Phone | Condensed transport (play · LIVE · speed · loop preset) over ~44px strip; minimap collapses to 4px sliver or omitted | Canvas full-bleed; chrome auto-hides during playback, tap to reveal |

⚠️ OPEN (later experiment): fixed-center playhead with the "tape" dragging beneath it on phones — more thumb-accurate, but inverts the shared mental model. Ship drag-anywhere-scrubs first; A/B the tape.

## 14. Progressive disclosure ladder

- **Level 0 (first launch / casual):** canvas + simple scrubber + LIVE + play. Everything automatic. No jargon — "frame," not "sweep."
- **Level 1 (via zoom):** scan structure, ghosts + countdown, loop presets, speed control.
- **Level 2 (power):** scan inspector, manual fetch, queue sheet + policies, keyboard map, UTC. Vocabulary may use real terms (tilt, VCP).

## 15. Scope & cut order

Ranked concessions if needed, first to cut at top:

1. **Full sweep inventory in the strip** → inspector only. (Do regardless — it's the design's biggest simplification win.)
2. **Custom loop dragging on mobile** → presets cover most use; handles remain for the few.
3. **Radial-level canvas animation** → genuine delight, gated narrowly; clean v2 candidate.
4. **Year-scale strip zoom** → already replaced by the Archive calendar tier.
5. **Manual download management UI** → permanently demoted; transparency means *showing* what the system did, not asking permission.

## 16. Risks & mitigations

| Risk | Mitigation |
|---|---|
| "Christmas tree" timeline | ≤3 accents at once; neutral fills/textures elsewhere; grayscale test; dark muted chrome vs. vivid reflectivity palettes |
| Snap flicker at zoom boundary | Hysteresis (different enter/exit thresholds) |
| Mode-switch disorientation | Morph animation; playhead + live edge spatially stable landmarks |
| Equidistant playback hides outages | Gap glyphs between distant ticks |
| Acquisition motion reads as alarm | Motion = routine; red = failure/live only |
| "Live" feels laggy in severe weather | Data-age readout; ghost countdown sets expectations |
| Accessibility | State = fill + shape, never hue alone; reduced-motion replaces pulses with static progress; full keyboard operation; screen reader announces displayed-frame timestamp |

## 17. Open questions — resolved

The v0.1 backlog below was resolved in the June 2026 alignment pass. The
authoritative decisions (with rationale) live in **Alignment decisions** at
the end of this document; code cites them as "alignment §N". Summary:

1. **Default live experience** → tethered single-frame following; pinned loop opt-in (Alignment §1).
2. **Zoom thresholds + hysteresis** → Micro↔Macro 1.0 px/s nominal (enter ≥1.15 / exit ≤0.87); Archive span enter >60 h / exit <48 h (Alignment §2).
3. **Canvas treatment while fetching** → caption only, no shimmer (Alignment §3).
4. **Chunk segmentation** → display 3 vs 6 faithfully (Alignment §4).
5. **Background streaming on open / site switch** → opens tethered; site switch re-tethers; detached streaming continues with a 60-min idle-stop (Alignment §5).
6. **Prefetch across tilts** → no proactive refetch; archive volumes already carry all tilts (Alignment §6).
7. **Loop window defaults** → default last 6 frames; presets 4/6/10 frames + 30 min/1 h + pin to live (Alignment §7).
8. **Offline / stale ghosts** → ghosts emitted only while a live stream exists; they disappear when the stream dies (Alignment §8).

## 18. Suggested build order

Strip + minimap with cell states and auto-fetch → tether model + LIVE button → Micro/Macro snap with morph + adaptive speed → loop presets + pinned sliding loop → scan inspector + queue sheet → Archive calendar tier → radial animation.

---

## Alignment decisions (June 2026)

This pass resolved the spec's open items (§17) and recorded the product
decisions the implementation follows, so the spec has one canonical answer
per open item and QA knows what behavior is intended. References throughout
the codebase cite these as "alignment §N" / "#N".

### Decisions on the spec's open questions (§17)

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

### Scope decisions (per §15 cut order)

- **Frames-first strip**: done (cut order says "do regardless").
- **Custom loop dragging on mobile**: cut; mobile gets presets + handles remain
  desktop-only.
- **Radial-level canvas animation**: deferred to v2 (existing sweep animation
  stays, gated as today).
- **Year-scale strip zoom**: removed, replaced by the Archive calendar tier.
- **Archive calendar layout** (§6.4): the Archive tier intentionally ships a
  **1-D zoomable UTC-day lane** (day cells laid out linearly with month
  separators), not the 2-D week-by-day GitHub-contributions grid the spec
  sketches. The ~56px strip height can't fit readable week-stacked cells without
  growing the panel, so the linear lane is the deliberate height-budget tradeoff
  for this pass. A 2-D weekday grid (independent of the linear zoom scalar) is a
  possible future enhancement.
- **Manual download management**: stays demoted; queue sheet shows what the
  system did, plus cancel/retry and policy toggles.
- **Tablet tier** (§13 row 2): out of scope for this pass; touch devices ≥600 px
  get the desktop layout as today.
- **Wi-Fi-only toggle** (§10): not implementable in a browser (no reliable
  network-type API); the queue sheet ships "auto-fetch while scrubbing" and
  "pause live stream while reviewing" (data-saver). `navigator.connection.saveData`
  may later seed the data-saver default.

### Other interpretation calls

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

---

*v0.1 drafted from UX consultation 2026-06-12; open items resolved in the
June 2026 alignment pass (above). The spec sections are the design; the
Alignment decisions are the canonical resolutions the code implements.*