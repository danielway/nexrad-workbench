# Consolidated Manual-QA Checklist

The single human touchpoint for two merged workstreams: the functional-core /
thin-shell migration (previously pending in
[CORE_SHELL_MIGRATION_LOG.md](../CORE_SHELL_MIGRATION_LOG.md) §2) and the
2026-07 architecture-health program
([README.md](README.md)). Everything else in both is verified by the compiler
and 1,931 headless tests.

Run `trunk serve`. Deep-link recipe: `?site=KDMX&t=<unix_seconds>` opens a
detached archive view at that moment; add `&rt=true` for live, `&dev=true` for
dev chrome. Bisect any regression against the commit list in
`git log --oneline 3a407c3..HEAD`.

**Read this first — the one intentional behavior change.** Explicitly stopping
live (the timeline now-cap's "click to stop", and the mobile LIVE button) now
tears down the worker stream. Previously the indicator went dark while the app
kept downloading forever. *Detaching* (any seek/jog while live) still keeps the
background stream alive on purpose — the LIVE chip hollows out and counts lag to
show it. Verify both halves of that distinction below.

---

## A. Transport, go-live, and stop-live

The heaviest-changed area (`ui/transport.rs` was deleted; its logic is now a
tested core reducer). Work through all of it.

- [ ] Timeline now-cap with no stream reads `◉ GO LIVE`; clicking starts a
      stream, pins the playhead to now, sets Realtime speed, clears any selection.
- [ ] Now-cap while tethered reads `◉ LIVE`; clicking stops it — playhead freezes
      on the live edge, status bar shows "Live mode stopped", **and network
      activity ceases** (dev tools: no further chunk requests).
- [ ] Now-cap when detached with a background stream reads `◉ REJOIN`; clicking
      re-pins instantly with no re-acquisition.
- [ ] Scroll "now" off-screen: the edge chip appears at the correct edge, and
      clicking it both scrolls the view to now and attaches, in one action.
- [ ] Transport-row `◉ GO LIVE` starts a stream identically to the now-cap.
- [ ] Transport-row hollow `◉ LIVE · m:ss behind` re-tethers on click.
- [ ] The `L` key re-tethers / starts a stream.
- [ ] Archive: play button and spacebar both start and stop playback; the
      position advances at the selected speed.
- [ ] Archive at the widest (Archive tier) zoom: play is refused, button stays PLAY.
- [ ] While tethered, the button reads PAUSE; pressing it freezes to archive at
      the live edge, the LIVE button hollows and starts counting lag, **and the
      background stream keeps running** (this is detach, not stop).
- [ ] Same freeze with "Pause live stream while reviewing" ON: the stream stops
      immediately instead.
- [ ] After a freeze, play resumes ordinary archive playback from the pause point.
- [ ] Spacebar does nothing while a text field has focus.
- [ ] Mobile: play/pause behaves as above; solid `◉ LIVE` tap freezes in place
      (no status message); hollow `◉ LIVE` rejoins; hollow `◉ GO LIVE` starts a
      stream at realtime speed.

## B. Site selection and the site modal

Location I/O moved out of the UI behind effects; site selection now flows
through an intent.

- [ ] Click a NEXRAD site marker on the map: the map retargets, camera recenters,
      pan resets, timeline reloads, alerts refresh — **and no stale radar image
      from the old site flashes** during the switch.
- [ ] Click the already-active site marker: nothing happens.
- [ ] Pick a site from the searchable list → modal closes, map retargets.
- [ ] Filter to exactly one result and press Enter → same.
- [ ] "Use My Location" → browser permission prompt → resolves to the nearest
      site → modal closes.
- [ ] Deny the permission → the error shows on the welcome screen (modal does not
      hang in "pending").
- [ ] Zip entry: a valid zip selects the nearest site; an invalid one ("1234",
      "abcde", empty) shows "Please enter a valid 5-digit zip code" and does not
      fire a request.
- [ ] A zip with no match (e.g. 00000) reports "Zip code not found".
- [ ] Mobile action bar's location button opens the modal already pending and
      resolves the same way.
- [ ] First visit (no saved site): after picking a site, the app opens tethered
      to live.
- [ ] Switching sites while a live stream runs: the stream restarts on the new
      site, and the old site's radar/timeline are cleared.

## C. Canvas interaction

One handler, previously half intents and half direct mutation.

- [ ] Distance tool: first click sets the start, second draws the measurement,
      third restarts from a new start point.
- [ ] Click an mPING marker → popover opens; click empty map → it dismisses.
- [ ] Click an alert polygon → the detail modal opens; with overlapping alerts,
      the **highest-severity** one opens.
- [ ] Hover data-probe: tooltip shows correct lat/lon, azimuth, range, and
      product value (including during sweep animation, over both the current and
      previous-sweep regions).

## D. Right panel — layers

All eight sibling checkboxes moved from direct writes to one intent.

- [ ] Each of NEXRAD Sites, State Lines, County Lines, Cities, Labels, National
      Mosaic, Alert Warnings, Alert Watches & Advisories, Storm Reports toggles
      its overlay on and off, with the tick staying in sync.
- [ ] Layers gated to live data (National Mosaic, both alert layers, mPING) stay
      greyed while viewing archive, with their disabled tooltips.
- [ ] "My Location": on → permission prompt → dot appears; a denied lookup shows
      the error **and** auto-unticks the checkbox; off → dot clears.
- [ ] The mPING gear opens the API-key modal; Save with a changed key refetches
      and closes; Save unchanged just closes; Clear wipes reports.

## E. Panels, overlays, and readouts

Time formatting was consolidated onto one primitive — check any clock readout.

- [ ] Toggle local/UTC: **every** clock readout flips in the same frame (top bar,
      canvas overlay, transport, timeline ruler, inspector, saved-events list,
      alert detail times).
- [ ] Canvas overlay timestamps show the right date and sub-second precision in
      both zones.
- [ ] Left panel (Advanced + sidebar): azimuth dial, elevation, VCP number/name,
      elevation-list highlight, and scan progress track the playhead in both
      archive and live; all freeze above 30× speed.
- [ ] Top bar: a status message fades after ~8 s and clears at ~10 s.
- [ ] Top-bar alert chip: one alert in view → click opens detail; several → click
      opens the list; a list row opens detail; "Refresh" re-fetches.
- [ ] Alert detail "Show on map" centers the 2D view on the alert, enables its
      layer class, and closes the modal.
- [ ] The version pill opens the GitHub releases page in a new tab.

## F. Render and acquisition (reducer extractions)

Non-visual logic moved wholesale into tested reducers; these confirm the shell
still drives them the same way.

- [ ] Scrub the timeline → frames update without duplicate or stuttering renders.
- [ ] Change elevation or product → repaints immediately; re-selecting the same
      one does not re-fetch.
- [ ] Play through a sweep boundary in Micro zoom → the next elevation is
      prefetched (no stutter at the boundary).
- [ ] 3D volume toggle still renders and updates while scrubbing.
- [ ] Sweep animation (Advanced + Micro + pref on): the line sweeps smoothly and
      holds its last position between sweeps rather than snapping to 0°.
- [ ] Fast-forward above 30×: sweep line and azimuth freeze (no flashing).
- [ ] National-mosaic cutout stays a stable coverage circle while scrubbing
      elevations/products, including at a high-latitude site.
- [ ] Select a short timeline range → it downloads immediately; select a >6 h
      range → the confirm modal appears and "Download Anyway" proceeds.
- [ ] With "autofetch while scrubbing" off, scrubbing does not trigger downloads,
      but the inspector's explicit fetch still works.

## G. Live ingest (longest-running check)

- [ ] Start live on an active site and watch a full volume: chunks arrive,
      elevations accumulate, the partial sweep renders progressively, and the
      volume completes without the playhead drifting.
- [ ] Across a volume boundary: the previous sweep promotes to the under-layer
      and the new volume starts cleanly.
- [ ] Let it run through a VCP change if one occurs: the elevation list updates
      and the selected cut snaps to the nearest new angle.

## H. Storage and persistence

- [ ] URL bar updates ~1×/sec while panning/zooming/scrubbing; reload restores
      the view.
- [ ] A preference change (palette, local/UTC, advanced mode) survives reload.
- [ ] Downloads sheet: Pause/Resume queue, and per-row Cancel / Move up / Move
      down / Retry / Skip all act on the correct operation.
- [ ] Scan inspector: per-tilt "Fetch", "Fetch whole scan", and "Loop from here".
- [ ] Clear cache and Wipe all behave, and the app recovers without a reload.

## I. Volumetric renderer overhaul

Six commits reworking `VolumeRayRenderer` and the volume packer against the
artifacts visible in the 3-D view (missing slabs, azimuth seam, ring banding,
stepped isosurface). The decisions are headlessly tested in
`core::volume_plan`; these confirm the GPU side matches. Enable "Volume
Rendering" in the right panel with the view in Globe 3D.

- [ ] **Nothing regressed to blank.** The volume renders at all — a GLSL
      compile failure logs "uniform not found" warnings and shows nothing.
- [ ] Load a storm on a SAILS-active VCP (212): no missing low-level slab, no
      venetian-blind gaps between elevation cones.
- [ ] Orbit the camera across radar north: no hard vertical seam, and no radial
      streaks fanning out through clear-air gaps.
- [ ] Top-down and low oblique views: no concentric ring banding; any residual
      undersampling reads as fine noise rather than shells.
- [ ] Long-range reflectivity is visible past 300 km, out to the sweep's real
      extent (~460 km), with no hard circular cutoff.
- [ ] Orbiting does not change apparent density — opacity is now view- and
      step-independent.
- [ ] Storm cores read as translucent volume with visible interior structure,
      not a hard first-hit surface. The opacity slider spans a useful range
      (its visual meaning was deliberately recalibrated).
- [ ] Anvil tops at long range sit at plausible altitudes, and cone placement
      agrees with the 2-D tilt view at range.
- [ ] Scrub with the volume enabled: it updates per scan without leaking or
      stalling (the texture is now reused across same-shaped uploads).
- [ ] Frame time is acceptable on an integrated GPU; if grazing rays look
      undersampled, `MAX_STEPS` is the knob.
- [ ] 2-D regression pass: scrub, elevation/product switch, hover probe. No
      shared code was touched, so this should be unaffected.
