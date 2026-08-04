# Audit: `src/ui/**` vs. the Functional-Core / Thin-Shell Standard

Part of the [2026-07-25 architecture review](README.md).

**Scope:** 60 files, 23,790 LOC under `src/ui/`. Standard per `docs/CORE_SHELL.md`: UI renders a view-model and emits `AppCommand` intents — no state mutation, no business logic, no I/O. Citations are `file:line` as of this date.

**Headline:** The standard holds cleanly on the *rendering leaves* (all of `canvas_overlays/*`, the timeline *paint* submodules, `left_panel.rs`, `alerts_modal.rs` — 34 files have zero `&mut`/intent-sink). It is broadly violated on the *interactive/orchestration surface* — exactly the "P5 partial / QA-gated" boundary the migration log admits is deferred (`docs/CORE_SHELL.md:178-194`). The gap is real, large, and — most damagingly — *inconsistent*: several single user actions are half-intent, half-direct-mutation.

Signature-level `&mut AppState` appears 95× across 26 files, but that overcounts (most are sink params). The load-bearing metric is **actual mutation sites**: ~143 direct field assignments + 24 `.set_*()` mutator calls + 64 `chrome.* =` toggle assignments in `src/ui/`.

---

## 1. Interactive surface mutates subsystem state directly, at scale (core violation)

The entire interactive layer bypasses `AppCommand` and writes `&mut` state inline. Worst offenders by direct-mutation density:

- **`src/ui/timeline/interaction.rs`** — 100% direct mutation, zero intents. Seek/scrub/select/zoom all write `playback.state` directly: `set_playback_position` (`:258`), `set_selection`/`apply_selection_as_bounds` (`:63-66,:93`), `begin/update/end_selection_drag` (`:81,:87,:91`), `timeline_view_start +=` (`:217,:300`), `playing = false` (`:170`), `set_timeline_zoom` (`:303`), `anchor_selection_to_live` (`:322`). Also writes `state.selection_just_finalized` (`:67,:95`) and `state.status_message` (`:239`).
- **`src/ui/shortcuts.rs`** — 17 `&mut AppState`; the single largest mutation site. Direct writes to `playback.state.speed` (`:559-566`), `playback.state.set_playback_position` (`:477,:525`), `state.viz_state.product` (`:731`), `state.viz_state.elevation_selection` (`:765`), `state.advanced_mode` (`:312`), `playback.state.set_selection` (`:667`), `timeline_view_start` (`:633`). Only *one* intent in the whole 1041-line file (`ReturnToLive`, `:718`).
- **`src/ui/canvas_interaction.rs`** — camera fully mutated inline: `camera.zoom/pan_pivot/orbit/free_look/free_translate/move_pivot_to/recenter` (`:41-140`), `set_pan_offset`/`set_zoom` (`:228-271`), `distance_start`/`distance_end` (`:160-164`).
- **`src/ui/playback_controls.rs`** — 8 `&mut`; `set_playback_position` (`:146`), `timeline_view_start =` (`:151`), `speed` via `selectable_value(&mut …)` (`:316`), `loop_mode =` (`:391`), `clear_selection` (`:359`).
- **`src/ui/right_panel.rs`** — 10 `&mut`; 19 two-way widget bindings (see §2).

Two-way widget bindings that write state directly total 23 across the UI (16 `checkbox(&mut …)`, 4 sliders/drag-values, 3 selectable/radio), concentrated in `right_panel.rs` (19). These are the "must stay `&mut`" cases the log flags (`docs/CORE_SHELL.md:191-193`), but they are un-annotated and indistinguishable from the avoidable ones.

**Trivial-projection reads (acceptable):** `canvas_overlays/*` all take `&` and only paint; `timeline/{scan_track,ruler,tooltips,frame_cells,calendar,overlays,strokes}.rs` are pure render leaves; `left_panel.rs` reads only. These are not violations.

---

## 2. Inconsistent seams — the same action is half-intent, half-mutation (most damaging)

This is worse than uniform direct-mutation because it defeats the test contract *and* hides which mechanism is authoritative.

- **"Go live" is split three ways.** `src/ui/timeline/now_edge.rs:317-336` emits `AppCommand::StartLive` (`:318`) *and, in the same function*, directly mutates `playback.state.clear_selection()` (`:317`), `playback.state.speed = Realtime` (`:319`), `playback.state.playing = false` (`:330`), `live.exit_live(...)` (`:335`), `state.status_message = …` (`:336`). Meanwhile `src/ui/transport.rs:29-66` performs the *entire* play/pause + live-detach decision as pure direct mutation (no intent), and `src/ui/playback_controls.rs:589-605` emits `ReturnToLive`/`StartLive` as intents. Three transport surfaces, three different seam conventions for the same operations.
- **Canvas click, one handler, two conventions** (`src/ui/canvas_interaction.rs`): mPING/alert selection and popover-dismiss go through intents (`push_command(AppCommand::Diagnostics(...))`, `:183,:203,:212`), but site selection in the *same* click block calls `apply_site_selection(...)` which mutates `state.viz_state`/`chrome` directly (`site_modal.rs:115-142`), and the distance tool writes `state.viz_state.distance_*` directly (`:160-164`).
- **Layer toggles are inconsistent within one panel** (`src/ui/right_panel.rs:278-364`): GPS and mPING toggles emit intents via change-detection (`EnableGps`/`DisableGps` `:320-328`, `OpenMpingSettings` `:358`), but the eight sibling checkboxes on the *same* list (`states`, `counties`, `cities`, `labels`, `nexrad_sites`, `national_mosaic`, `alerts_warnings`, `alerts_other`) mutate `state.layer_state.geo.*` directly (`:278-310`). The P2 log claims the right panel "now emit[s] intents instead of mutating overlay state" (`docs/CORE_SHELL.md:145-148`) — true only for the GPS/mPING slice; the alert-layer toggles it names are still `&mut`.

**Model citizen (for contrast):** `src/ui/alerts_modal.rs` — every mutation goes through `push_command` (`:50,:76,:88,:148,:164,:180,:293,:298`); its two `&mut AppState` are pure intent sinks. This is the P2 reference slice done correctly and shows the target shape.

---

## 3. I/O performed directly in the shell (explicit `Effect`-boundary violation)

- **`src/ui/site_modal.rs` — the worst offender.** Real network + device I/O inline: browser geolocation via `web_sys::window().navigator().geolocation()` + `get_current_position_with_error_callback` (`:153-205`), HTTP geocoding via `window.fetch_with_str` against `api.zippopotam.us` (`:210,:246`), `wasm_bindgen_futures::spawn_local` (`:212` — the *only* `spawn_local` in `src/ui/`), plus a retry policy (`with_retry`, `:214`). 8 `js_sys` refs. This is a full acquisition pipeline living in a UI modal; P2 introduced `Effect::StartGeolocation` (`docs/CORE_SHELL.md:143`) but this executor was never moved behind it.
- **`src/ui/top_bar.rs:604`** — `web_sys::window().open_with_url_and_target(...)` (navigation I/O) inline in the version-label click.
- **`src/ui/mobile/mod.rs:135-150`** — DOM read via `web_sys::window()` + `js_sys::Reflect` calling `__nexradSafeAreaInsets()`.
- **Wall-clock reads bypass the injected clock.** `src/ui/canvas.rs` calls `js_sys::Date::new_0()` at `:650,:680,:700` and `js_sys::Date::new(...)` at `:748` for time formatting, and `time_format.rs:54-70` reaches into `js_sys::Date::to_locale_time_string_with_options`. The codebase *has* an injected `FrameNow` clock (`derived.frame_now_secs`, used at `canvas.rs:104`; P1, `docs/CORE_SHELL.md:129-135`) — these Date reads sidestep it.

---

## 4. `LayoutCtx` / parameter passing — no view-model; grab-bag of `&mut`

There is **no coherent per-panel view-model.** The only read-only projections are `subsystem::Derived` (minimal, ~4 fields per the log) and the P2 `DiagnosticsVm`. Everything else is threaded as mutable subsystem references.

- **`LayoutCtx`** (`src/ui/layout.rs:82-96`) carries 11 fields, **8 of them `&mut`** (`state` + `live`, `playback`, `acquisition`, `chrome`, `diagnostics`, `modals`). Its own doc concedes it "carries `&mut` references to every subsystem each panel or modal might touch" (`:33-37`). Panels destructure and reborrow — this is a whole-app mutable handle, not a view-model. Constructed in `src/main.rs:897-909`.
- **The two big entry points bypass even that** and take positional grab-bags: `render_canvas_with_geo` — **10 params** (`canvas.rs:22-33`, `main.rs:913`); `handle_shortcuts` — 6 params (`shortcuts.rs:356`, `main.rs:927`).
- **29 `#[allow(clippy::too_many_arguments)]` in `src/ui/`** (repo-wide ~53 per `docs/CORE_SHELL.md:250`). Worst signatures: `draw_top_bar` 9 params (`top_bar.rs:37`), `render_timeline` 8 (`timeline/mod.rs:295`), `compute_gpu_sweep_state` 6 (`canvas.rs:557`), `render_layers_section` 6 (`right_panel.rs:264`), `render_radar_sweep` (`canvas_overlays/sweep.rs:52`).

**Counter-example of a good internal API:** `timeline::TimelineFrame<'a>` (`timeline/mod.rs:59-88`) is a genuine read-only per-frame projection with shared `ts_to_x`/`x_to_ts` helpers — child renderers take `(painter, &TimelineFrame, specifics)`. This is what a view-model looks like; nothing else in the UI has one.

---

## 5. Business logic / derivations computed inline in paint & interaction code

- **Frame-rate decisions in paint** (`canvas.rs:225-248`): the shell computes `live_has_active_sweep`, `live_has_moving_line`, and branches to pick 33ms vs 100ms repaint cadence — a behavioral decision derived from live-model internals, inline.
- **`compute_gpu_sweep_state`** (`canvas.rs:557-…`) still does sweep-matching business logic inline: `find_recent_scan(playback_ts, 15.0*60.0)` + elevation filter + `rfind(start_time <= playback_ts)` fallback chain (`:583-600`), even after P3. Mutates `state.viz_state.last_visible_bounds` (`canvas.rs:55`).
- **`compute_sweep_line_azimuth`** (`canvas.rs:629-646`) — pure math correctly delegated to `core::canvas::sweep_line_azimuth`, but the freeze threshold `speed > 30.0 → None` (`:635`) and the `_state` param (unused) remain; the log claims this freeze rule (`animation_frozen`) went to `core::panels` (`docs/CORE_SHELL.md:184-186`) — the canvas copy wasn't switched over.
- **`transport.rs:29-66`** — the ARCHIVE / LIVE-NOW / LIVE-LOOKBACK branching + data-saver policy is decision logic in a UI file (it even has `#[cfg(test)] coverage_tests`, proving it is headless-testable and therefore in the wrong layer per the "if you can't test it without the browser…" rule, inverted).
- **`shortcuts.rs`** — timeline-zoom clamp/hysteresis/detach decision (`:605-637`), speed-ladder cycling (`:556-567`), product cycling `products[(idx+1)%len]` (`:731`). Some pure helpers are extracted (`view_start_anchored_at`, `:579`) but the deciding orchestration mutates directly.
- **Status-message change-detection** (`top_bar.rs:48-56`) — stashes prev message in `ctx.data` temp storage and stamps `state.status_message_set_ms` — derivation + mutation the core should own.

---

## 6. Two intent-emission mechanisms (avoidable inconsistency)

`AppCommand` is thin (18 variants, `state/mod.rs:90`) and emitted through **two different channels**:

1. Canonical: `state.push_command(AppCommand::…)` (~40 sites).
2. Ad-hoc `commands: &mut Vec<AppCommand>` parameter — `queue_sheet.rs:162` (`:120,:125,:234-259`) and `scan_inspector.rs:251` (`:181,:316`), each flushed by a wrapper (`queue_sheet.rs:153`, `scan_inspector.rs:209`).
3. A third variant: `acquisition_drawer.rs:154` builds a *local* `Vec<AppCommand>`, fills it (`:223-251`), then drains into `push_command` (`:261-262`).

Three ways to spell "emit an intent" for structurally identical queue/inspector actions.

---

## 7. Canvas overlays — declarative registry only for corner-chrome; data-flow is a hardcoded sequence

The `Overlay` registry (`canvas_overlays/mod.rs:75-116`) is genuinely declarative but covers **only the 4 corner-chrome overlays** (info/color-scale/compass/scale-bar, `:97-102`). The 10 **data-flow overlays** (radar GPU, alerts under/over radar, mPING, sites, geo lines/labels, national mosaic, sweep, data probe, distance) remain a hardcoded imperative sequence in `render_canvas_with_geo` (`canvas.rs:158-357`), with interleaved mid-render computed inputs (`gpu_sweep`, `radar_cutout`, `sweep_info`). The module doc (`:12-21`) and migration log (`docs/CORE_SHELL.md:253-255`) both explicitly defer this. So the "Overlay registry" mentioned in docs is real but partial. `canvas_interaction`'s coupling to camera/viz state is total: every branch reads/writes `state.viz_state.camera` or `state.viz_state.{zoom,pan_offset}` directly (`canvas_interaction.rs:41-271`).

---

## 8. Duplication & consistency

- **Time formatting is fragmented across 3+ approaches.** `time_format.rs` (`format_clock_12h`, `format_updated_ago`) is used in only 2 files (`top_bar.rs`, `mobile/top_bar.rs`). The `timeline` module exposes a *separate* family (`format_timestamp`, `_compact`, `_full`, `DateTimeComponents::from_timestamp`). And **raw inline `chrono`/`js_sys::Date` formatting is copy-pasted** in `event_modal.rs:101-108`, `right_panel.rs:682-683`, `canvas.rs:649-716` (`format_time_short`/`format_time_full`), and `canvas_overlays/mping.rs:235-236`. Same "format a UTC/local timestamp" logic, four hand-rolled copies.
- **Desktop vs mobile chrome is *mostly* well-factored** (a positive): `mobile/settings_modal.rs:280-303` delegates straight into `right_panel::render_{layers,rendering,tools,events,storage}_section`. The residual duplication is transport: `mobile/tabs.rs:273-357` re-implements the loop-preset menu + return-to-live/start-live control building that `playback_controls.rs:406-605` also builds — same `AppCommand` vocabulary (`ApplyLoopPreset`/`ClearLoop`/`ReturnToLive`/`StartLive`), duplicated widget code.
- **Modals are consistent** (a positive): `modal_helper::modal_backdrop` is used by 12 modals; `ModalStates` aggregator (`modal_states.rs`) centralizes transient form state. Exceptions: `shortcuts.rs` help modal and `site_modal.rs` handle `Key::Escape` themselves (`site_modal.rs` both uses the backdrop *and* rolls its own Escape). Minor.

---

## 9. Dead weight

- **35 underscore-prefixed unused `&`-params in `shortcuts.rs`** alone (51 total across `src/ui/`). Every handler in the dispatch table (`handle_frame_step`, `handle_scan_step`, `handle_speed_step`, `handle_zoom_step`, … `shortcuts.rs:437-817`) takes the full `(state, live, timeline, playback, chrome, ctx)` grab-bag but uses 1–2 — e.g. `handle_speed_step(_state, _live, _timeline, playback, _chrome, ctx)` (`:546-552`) has 4 dead params. This is the grab-bag anti-pattern in its purest form.
- **Vestigial param kept to avoid churn:** `transport.rs:29` `_timeline: &Timeline`, documented as "unused now … kept in the signature so … call sites … don't churn" (`:26-28`). Same for `_state` in `compute_sweep_line_azimuth` (`canvas.rs:630`) and `compute_gpu_sweep_state`'s unused threading.
- **`#[allow(dead_code)]`**: `colors.rs`, `canvas_overlays/mod.rs:66` (`OverlayContext.derived`, "currently unused … available so future predicates can gate").

---

## Summary table

| Area | Metric | Evidence |
|---|---|---|
| Direct mutation sites | ~143 field assigns + 24 `.set_*` + 64 `chrome.* =` | `src/ui/` grep |
| `&mut AppState` in signatures | 95 across 26 files | shortcuts 17, right_panel 10, playback_controls 8 |
| Two-way widget bindings | 23 (19 in right_panel) | `right_panel.rs:278-364` |
| `too_many_arguments` allows | 29 | worst: `top_bar.rs:37` (9), `canvas.rs:22` (10 params) |
| Unused `&`-params | 51 (35 in shortcuts) | `shortcuts.rs:437-817` |
| I/O in shell | geolocation+fetch+spawn_local (site_modal), open_url (top_bar), DOM read (mobile), Date reads (canvas/time_format) | §3 |
| Intent channels | 3 (push_command / `&mut Vec` param / local-Vec-drain) | §6 |
| Inconsistent "split" seams | ≥4 (go-live ×3, canvas click, layer toggles) | §2 |
| Declarative overlays | 4 of 14 (corner-chrome only) | `canvas_overlays/mod.rs:97` |

**Where the standard already holds:** `alerts_modal.rs`, all `canvas_overlays/*`, timeline render leaves, `left_panel.rs`, the `modal_helper`/`ModalStates`/`TimelineFrame` infrastructure. **Where it doesn't:** the interactive spine — `shortcuts.rs`, `transport.rs`, `timeline/{interaction,now_edge}.rs`, `canvas_interaction.rs`, `playback_controls.rs`, `right_panel.rs` inputs, and `site_modal.rs` (I/O). This matches the migration log's own "P5 partial / QA-gated" admission; the finding is that the deferral is broad and, critically, *seam-inconsistent* rather than uniformly-deferred.
