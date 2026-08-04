# Audit: State/Logic Layers (`src/state/`, `src/subsystem/`, `src/app/`, `src/core/`)

Part of the [2026-07-25 architecture review](README.md). Scope: the four
overlapping layers plus `src/main.rs` and `src/lib.rs`, measured against the
binding standard in [CORE_SHELL.md](../CORE_SHELL.md). Citations are
`file:line` as of this date.

Headline numbers: **34 fields on `AppState`; 18 `AppCommand` variants; 3
`Effect` variants; 447 `pub` fields in `src/state/`; ~4,000 lines of untested
decision+effect logic in `src/app/` (`impl WorkbenchApp`); ~146 direct `&mut`
state-assignment sites in `src/ui/` vs 49 `push_command` intent emissions; the
view-model is 4 fields wide while panels read raw state ~460 times.**

---

## 1. The functional-core migration is ~real in vocabulary, ~10% real in fact — the decision+effect tangle still lives in `src/app/` (MOST SIGNIFICANT)

`src/core/` is essentially a **contract shim plus ~30 extracted pure helpers**. `core/mod.rs:25-40` just re-exports `Effect`, `Intent` (a bare alias of `AppCommand`, `core/intent.rs:13`), and a handful of `decide_*` functions. The pure functions that do exist are genuinely good and tested: `core/canvas.rs` (~14 fns), `core/diagnostics.rs` (`select_alert_at`, `reduce`, `DiagnosticsVm`), `core/panels.rs` (`query_radar_state_at_timestamp:106`), `core/acquisition.rs`, `core/persist.rs`, `core/render.rs`. But they are a thin skim off the top.

The actual behavior — the "decide what to do and do it" logic — lives in `impl WorkbenchApp` methods under `src/app/` and is neither pure nor testable without a browser:

- `app/worker_results.rs` is **1,056 lines**. `handle_chunk_ingested_outcome` alone runs `worker_results.rs:203-613` (410 lines) interleaving live-engine mutation, projection bookkeeping, logging, GPU promote calls, and render dispatch. `handle_decoded_outcome:615-805` and `handle_live_decoded_outcome:807-965` do GPU `update_data`/`promote_current_to_previous` uploads inline.
- `app/render_loop.rs` `advance_playback:14-284` decides sweep matching, elevation snapping, prefetch, and caption honesty, and `sync_prev_sweep_texture:308-468` locks the GPU renderer and calls `clear_previous_data`/`update_previous_data` inline.
- `app/acquisition_intent.rs` (695 lines) computes prefetch windows *and* calls `download_channel.fetch_listing(...)` inline (`:357`, `:437`, `:590`).

`docs/CORE_SHELL.md:59,237-240` admits exactly this ("the decision+effect *tangle* lives mostly in `src/app/*`"). The measured reality: **~25 inline side-effect call sites in `src/app/`** (worker dispatch, download/listing fetch, GPU upload, `spawn_local`), against **3** that route through the `Effect` runtime.

**What good looks like:** the `handle_diagnostics_intent` path (`command_dispatch.rs:111-126` → pure `core::diagnostics::reduce` → `apply_effects`) is the one place the standard is actually followed end-to-end. The `app/*` orchestration methods should collapse into pure `decide(state, intent) -> Vec<Effect>` reducers with the I/O bodies moved behind the effect runtime — i.e. finish P5/P6, which the log marks as deferred/partial.

---

## 2. Intent/effect contract reality check — the boundary exists but carries a small minority of traffic

**Intents (`AppCommand`, `state/mod.rs:89-140`): 18 variants**, and they are overwhelmingly *effect triggers*, not ordinary state changes: `RefreshTimeline, ClearCache, StartLive, ReturnToLive, ApplyLoopPreset, ClearLoop, CheckEviction, WipeAll, PauseQueue, ResumeQueue, RetryFailed, FetchScan, SkipFailed, CancelOperation, ReorderOperation, RetryWorker, Diagnostics, ShowAlertOnMap`.

How much mutation flows through them vs direct `&mut`:
- `push_command` is emitted **49 times** across 17 UI files (`grep push_command src/ui`).
- Direct `&mut` state assignment in `src/ui/` is **~146 sites** (`(state|playback|chrome|live|…).field = …`), plus mutating method calls, and **276 UI function signatures take `&mut` to state/subsystems**. Ordinary changes — camera/zoom (`state.viz_state.*`, read/written 182×), product, elevation, layer toggles, transport, modal toggles — all bypass `AppCommand`. The intent queue governs cross-cutting/effectful actions only; the standard's goal of "the *only* way the UI changes anything" (`CORE_SHELL.md:57`) is far off.

**Effects (`core/effect.rs:37-47`): 3 variants** — `PushUrl`, `SavePreferences(Box<UserPreferences>)`, `StartGeolocation`. `apply_effects` (`app/effects.rs:16-36`) is exhaustive over exactly those three. Real side-effect *categories* in the app number roughly twelve (HTTP download, S3 listing, worker ingest/render `postMessage`, GPU texture upload, IDB read/write, localStorage, history, geolocation, timers, favicon/title, repaint scheduling, network-monitor attach). **Only 3 go through `Effect`;** the rest are inline. Worker/GPU effects instead use the endorsed "local action enum" idiom (`PrevSweepAction` in `playback_manager.rs`, `DesiredDisplay`) — but those still *execute* inline in `app/render_loop.rs:414-467` and `app/live_mode.rs`, so they are effect-*shaped* without an effect *runtime*.

**`app/command_dispatch.rs` structure:** `handle_command:46-107` is a giant match over all 18 variants that dispatches to `handle_*` methods **which do I/O inline** — `handle_wipe_all:291` spawns `facade.clear_all()` + `localStorage.clear()` + `location().reload()`; `handle_check_eviction:327` spawns `facade.check_and_evict`; `handle_clear_cache:274`, `handle_refresh_timeline:306` drive cache channels; `handle_fetch_scan:214` / `handle_retry_failed:164` mutate `download_queue`. Three `spawn_local` sites live directly in this dispatcher. It is a router into imperative methods, not a reducer. The lone exception is `handle_diagnostics_intent:111-126` (pure reduce + effects).

**What good looks like:** grow `AppCommand`→`Intent` to cover the interactive surface (camera/transport/layer/product/modal) and widen `Effect` to carry worker-dispatch and GPU-upload descriptors (or formally bless the local-action-enum executors as *the* effect runtime and route them through one seam), so `apply_effects` is the single I/O choke point.

---

## 3. `AppState` god-object — 34 fields, and 14 of them are not domain state

`AppState` (`state/mod.rs:143-287`) has **34 `pub` fields**. Breaking them down:

- **Per-frame derived caches that shouldn't be persistent state (5):** `frame_now:147`, `is_dark:197`, `is_mobile:266`, `width_tier:282`, `render_cache:286` — all recomputed each frame (`refresh_mobile_mode:598`, `apply_frame_setup`).
- **Ad-hoc coordination / one-shot signal flags (7):** `commands:170` (the queue), `selection_just_finalized:176`, `auto_position_on_timeline_load:180`, `start_live_on_site_select:235`, `status_message_set_ms:160`, `touch_seen_ever:272`, `worker_init_error:257`. More of the same live off-struct: `alerts.rs:24 refresh_requested`, `mping.rs:35 invalidate_requested`, `chrome.rs:62 mobile_geolocate_requested`.
- **UI-local state in global state (2):** `status_message:156` (a display string) and `datetime_picker:183` — a full `DateTimePickerState` (open flag + six text-edit buffers, `state/mod.rs:425-500`). A modal's transient text-input state sitting on the root app-state struct.
- The remaining ~20 are genuine config/preferences and domain data.

So **~41% of `AppState` is per-frame scratch, cross-component signaling, or UI-modal state**, not the "root application state" the doc-comment claims (`:142`). `VizState` (`viz.rs:323`) is a second-tier god object with **25 fields** (camera, product, elevation, displayed/previous sweeps, storm cells, staleness, caption…).

**What good looks like:** per-frame derived values (`is_dark`, `is_mobile`, `width_tier`, `frame_now`) belong on the `Derived` view-model, not `AppState`; coordination flags should become intents/effects (a `SelectionFinalized(a,b)` intent instead of a drained `Option` field); `datetime_picker`/`status_message` belong with the other modal/UI state in `ui::ModalStates` or `Chrome`.

---

## 4. `src/state/` triage — mostly pure, well-tested data+logic (good core candidates), a few tangles, some cross-layer coupling

`src/state/` is the healthiest layer: **447 `pub` fields across 24 modules**, and it is largely pure — only `preferences.rs`, `saved_events.rs`, `settings.rs`, `url_state.rs` touch I/O. Coverage is real: 61/60/51/39/39/38 `wasm_bindgen_test`s in the six biggest files.

**Pure data+logic — strong core candidates (keep, promote):**
- `playback.rs` (**2,651 lines**, 16 types, 143 fns, 61 tests) — the time model. Holds `PlaybackState:755`, `TimeModel:612`, `MacroPlaybackState:127`, loop/tier/speed enums. Zero cross-layer imports; pure timeline math. This is the model of what a core module should be — it is just mis-sized and mis-located under `state/` rather than `core/`.
- `radar_data.rs` (1,833 lines, 107 fns, 60 tests) — `RadarTimeline`/`Scan`/`Sweep`/`Radial`; scan-at-timestamp lookups. Couples only to `data::ScanCompleteness` + `nexrad::ScanMetadata` (`:3-4`).
- `viz.rs` (1,016 lines, 22 tests), `playback_manager.rs` (1,507; holds the `PrevSweepAction`/`DesiredDisplay` resolvers the standard cites as its effect-as-data prototype), `calendar.rs`, `vcp_forecast.rs` (1,162; `derive_volume_forecast`).

**Tangles / mis-scoped:**
- `timeline_view.rs` (1,833 lines) and `live_mode.rs` (1,401) are pure but **coupled up into `nexrad::projection`** (`timeline_view.rs:36-37`, `live_mode.rs:600-601` import `ScanProjection`, `VolumeObservations`, `StreamingPlan`) — the "state" layer reaching into the nexrad pipeline's projection engine. `LiveModeState` (`live_mode.rs:122`) is a state machine, but the live *derivation* it feeds now lives half here and half on the `Live` subsystem's `refresh` (`subsystem/live.rs:140-177`), which mutates the shared projection engine — decision logic on a "state owner."
- `acquisition.rs` (1,500 lines) mixes pure operation-tracking with `js_sys::Date::now` wall-clock reads (8 sites) — an injected-clock candidate.
- **Persistence files do inline I/O that the standard says are effects:** `settings.rs:42/71`, `saved_events.rs:36/65` (`localStorage get_item/set_item`), `url_state.rs:227-229` (`history.replace_state_with_url`), `preferences.rs`. These *are* now invoked from the `Effect` runtime (`Effect::SavePreferences`/`PushUrl` → `app/effects.rs:31,26`), but the I/O bodies still live in `state/`, so the "pure" layer isn't uniformly pure.

**File-organization smell:** `app/selection_download.rs` actually contains the generic `advance_download_queue` pump (`:16`), while selection-fetch logic lives in `app/acquisition_intent.rs` (`pump_selection_fetch:491`) and download-outcome handling in `app/download.rs`. The filenames don't match their contents.

**What good looks like:** the pure, tested modules (`playback.rs`, `radar_data.rs`, `playback_manager.rs`, `viz.rs`, `vcp_forecast.rs`) are already "the core" in all but directory name — they should live under `core/` (or `core/` should re-export them as the canonical surface, per `core/mod.rs:18-23`'s stated intent). The four persistence files' I/O bodies should move behind the effect runtime, leaving pure `(prefs) -> Json` encoders in `state/`.

---

## 5. `subsystem/` — real ownership boundaries, but thin; one is a newtype; the "view-model" is 4 fields

The 7 subsystems (`subsystem/mod.rs:14-30`) are **genuine ownership boundaries, not pass-throughs** — each folds a `state/` slice together with the manager/channel it belongs to, killing a real "two-objects-in-sync" hazard (documented at `subsystem/acquisition.rs:1-21`, `subsystem/live.rs:6-14`). But they carry little logic of their own:

- **`Playback` is a 1-field newtype** — `subsystem/playback.rs:19-23` is literally `struct Playback { state: PlaybackState }`. Every call site pays a `.state.` indirection (`playback.state.` appears **192×** in `src/ui/`) for zero added behavior.
- `Timeline` (`subsystem/timeline.rs:21-31`) = two fields (`scans` + `shadow_scan_boundaries`), no methods.
- `Chrome` (`subsystem/chrome.rs:17-68`) = a bag of ~13 modal/visibility booleans — correct as an owner, but it's pure data.
- `Render` (`subsystem/render.rs`) and `Acquisition` (`subsystem/acquisition.rs`) compose two/three fields each.
- Only `Live` (`subsystem/live.rs`) and `Diagnostics` (`subsystem/diagnostics.rs`) carry real per-frame behavior (`Live::refresh:140`, `Diagnostics::tick:78`), and `Live::refresh` mutates the shared projection engine — decision logic on a state owner.

**`subsystem/derived.rs` (the "view-model out" seam) is 4 fields:** `frame_now_secs`, `visible_bounds`, `data_is_live`, `effective_sweep_animation` (`derived.rs:31-47`), read **14×** in `src/ui/`. Against that, panels read raw internals directly at scale: `playback.state.*` **192×**, `state.viz_state.*` **182×**, `live.mode_state.*` **35×**, `acquisition.state.*` **33×**, `live.radar_model/frame_projection/engine` **19×** — **~460 direct internal reads**. The view-model covers roughly **3%** of what panels actually consume. The only *complete* per-panel view-model is `DiagnosticsVm` (`core/diagnostics.rs:232`, built in `main.rs:893`), the P2 reference slice.

**What good looks like:** collapse `Playback`/`Timeline` newtypes if they never earn behavior; and, crucially, widen `Derived` into real per-panel view-models (as `DiagnosticsVm` already demonstrates) so `left_panel`/`right_panel`/`timeline`/`transport` read a projection instead of reaching into `playback.state`/`viz_state` ~370 times.

---

## 6. Encapsulation — `pub` is the default everywhere; layers reach through each other freely

Fields are `pub` almost universally: **447 `pub` fields in `src/state/`** (e.g. `viz.rs` exposes 50 across 11 types; `vcp_forecast.rs` 54; `playback.rs` 30). Because everything is `pub`, every layer reaches into every other layer's internals directly rather than through methods:

- UI reaches four levels deep into subsystem-owned state: `self.playback.state.time_model.is_pinned()` and `state.viz_state.camera.center_on(...)` patterns are pervasive; `state.viz_state.*` is *written* directly from `src/ui/` (part of the ~146 assignment sites) and from `command_dispatch.rs:139-155` (`handle_show_alert_on_map` sets `layer_state.geo.*`, `viz_state.center_lat/lon`, calls `camera.center_on`).
- The `update()` loop hands out **~19 simultaneous `&mut` borrows** of state/subsystems into UI render functions (`main.rs:897-933`: `LayoutCtx { state, live, playback, acquisition, chrome, diagnostics, … }` all `&mut`, plus `render_canvas_with_geo` and `handle_shortcuts` each taking 5-6 more). The layering is nominal; any panel can mutate anything.
- `WorkbenchApp` (`main.rs:173-227`) itself is a 4th state location (13 fields) beyond `AppState` + 7 subsystems: `gpu`, `geo_layers`, `persistence`, `modals`, `last_favicon_mode`. **Modal/transient UI state is fragmented across three homes** — `Chrome` (open booleans), `AppState.datetime_picker` (`mod.rs:183`), and `WorkbenchApp.modals: ui::ModalStates` (site/event/mping, `main.rs:217` / `ui/modal_states.rs:24-33`).

So the "three overlapping state layers" the docs concede (`CORE_SHELL.md:237`) is, counted precisely, **four**: `AppState` (34 fields) + the 7 subsystems + `WorkbenchApp`'s own 13 fields + `ui::ModalStates`. `main.rs` is not a thin coordinator either — `apply_url_params:243-409` is 166 lines of direct view-state mutation, and `update:721-935` is a hand-ordered 21-step sequence whose ordering invariants are load-bearing (documented `main.rs:703-720`).

**What good looks like:** make `state/` fields private with accessor methods (the codebase already funnels some reads through helpers like `AppState::effective_sweep_animation:635`, `show_advanced:578` — extend that discipline), give each subsystem a typed API instead of `pub` fields, and pass panels a `&ViewModel` + an intent sink instead of ~19 `&mut` handles (the P5 `LayoutCtx → &ViewModel` reshape the log explicitly defers).

---

## Summary scorecard

| Seam | Standard's target | Measured reality |
|---|---|---|
| Intents in (`AppCommand`) | only way UI mutates | 18 variants, 49 emissions; ~146 direct `&mut` sites bypass it |
| Effects out (`Effect`) | all I/O described as data | 3 variants; ~25 inline side-effects in `app/`; 1 exemplary path (diagnostics) |
| View-model out (`Derived`) | complete per-panel projection | 4 fields, read 14×; ~460 direct internal reads; 1 complete VM (`DiagnosticsVm`) |
| Functional core | owns all state+logic | ~30 pure fns; the bulk of decisions in ~4,000 lines of `impl WorkbenchApp` |
| State ownership | subsystems + core | split 4 ways: `AppState`(34) + 7 subsystems + `WorkbenchApp`(13) + `ModalStates` |

The diagnosis in one line: **the contract types and the pure-`state/` foundation are sound and well-tested; the migration has not moved the decision+effect mass out of `src/app/`, so the boundary is real but mostly unused, and the "god object" problem has been *distributed* across four state homes rather than resolved.**
