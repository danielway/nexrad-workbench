# Audit: Cross-Cutting Dependency & Entropy Metrics

Part of the [2026-07-25 architecture review](README.md).

Scope: `src/` (11 top-level modules + `main.rs`/`lib.rs`), ~94k LOC. Edges derived from 329 `use crate::` statements (single + 6 grouped `use crate::{…}` forms; `app`'s grouped imports folded in). `main.rs` is the crate root that *defines* `WorkbenchApp`, so `src/app/*` imports it as `crate::WorkbenchApp`; `main.rs` itself uses bare `state::`/`nexrad::` paths (not `use crate::`) and is reported separately as the composition root.

Intended layering (from `docs/CORE_SHELL.md`): `core (pure) ← state ← subsystem ← app (shell) → ui (projection)`; `data/geo/net/alerts/mping` are leaf libraries. `nexrad` is unlisted; audited as a library.

## 1. Dependency matrix

Rows = importing module, columns = imported module, cell = count of `use crate::` edges. Diagonal `(n)` = intra-module self-imports. `·` = 0.

| ↓imports \ →| alerts | app | core | data | geo | mping | net | nexrad | state | subsys | ui |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **alerts** | (4) | · | · | · | · | · | 1 | · | 1 | · | · |
| **app** | · | (1) | 2 | 2 | · | · | · | 5 | 11 | · | · |
| **core** | 3 | · | (2) | · | · | · | · | · | **14** | · | · |
| **data** | · | · | · | (9) | · | · | · | · | · | · | · |
| **geo** | · | · | · | · | (3) | · | · | · | 2 | · | · |
| **mping** | · | · | · | 1 | · | · | 1 | · | 1 | · | · |
| **net** | · | · | · | · | · | · | · | · | · | · | · |
| **nexrad** | · | · | 2 | 15 | 2 | · | 3 | (31) | 12 | · | · |
| **state** | 1 | · | · | 21 | 4 | 1 | · | 17 | (21) | · | 2 |
| **subsystem** | 1 | · | · | 1 | · | 1 | · | 7 | 8 | (1) | · |
| **ui** | 3 | · | 4 | 6 | 14 | 4 | 1 | 9 | **51** | 10 | (16) |
| *main.rs (root)* | · | · | 3 | 3 | 12 | · | · | 18 | 20 | 20 | 10 |

**Fan-in (imported-by, excl. self) — hub ranking:**

| module | afferent | notes |
|---|---|---|
| **state** | **100** | imported by 8 of 11 modules; the god-module |
| data | 46 | clean leaf, heavy but correct-direction |
| nexrad | 38 | |
| geo | 20 | |
| subsystem | 10 | only ui reads it (the view-model seam) |
| core / alerts | 8 / 8 | |
| mping / net | 6 / 6 | |
| ui | 2 | should be 0 — see violations |
| app | 0 | |

**Fan-out (imports-out, excl. self):** `ui`=102 (into 9 modules) is the biggest emitter; `state`=46; `nexrad`=34; `app`=20; `subsystem`=18; `core`=17. `data`=0 and `net`=0 are the only true leaves.

**Cycles.** Every 2-cycle in the codebase runs through `state`:

| cycle | edges | severity |
|---|---|---|
| **state ↔ nexrad** | state→nexrad 17, nexrad→state 12 | **heaviest cycle** |
| **state ↔ ui** | ui→state 51, state→ui 2 | worst by volume (51) |
| state ↔ geo | state→geo 4, geo→state 2 | moderate |
| state ↔ alerts | 1 / 1 | thin |
| state ↔ mping | 1 / 1 | thin |
| **core → state → nexrad → core** (3-cycle) | core→state 14, state→nexrad 17, nexrad→core 2 | the "pure core" is *inside* a cycle |

Worst hubs: **`state` is both the top sink (100 in) and a top source (46 out)** — it is the entropy nexus. **`ui` is the top source (102 out)** reaching into 9 modules. `nexrad` (self-loop 31, the densest intra-module coupling) is the second nexus.

## 2. Layering violations (wrong-direction edges)

Direct answers to the posed checks:
- **Does `core` import `state`? YES — 14 edges (headline violation).** The doc says `state → core`; `state → core` **does not exist** (verified: zero hits). The arrow is fully reversed — `core` sits *above* `state`, not beneath it.
- **Does `core` import a library? YES — `core → alerts` (3).**
- **Does `data` import `state`? NO.** `data` is a clean leaf (0 outward edges). `net` likewise.
- **Does anything import `ui`? YES — `state → ui` (2 edges).**
- **Do leaf libraries import `state`? YES — `nexrad`→state 12, `geo`→state 2, `mping`→state 1, `alerts`→state 1.**

### Worst 5 violating edges (with evidence)

**① `core → state` (14) — the "pure base" depends on the layer above it.** The core's *central intent type is defined in `state`*, and its pure functions take `state`-owned data types as inputs:
- `src/core/intent.rs:13` — `pub use crate::state::AppCommand as Intent;` (core::Intent *is* a state type re-export)
- `src/core/persist.rs:14` — `use crate::state::{AppState, PlaybackState, UserPreferences};`
- `src/core/canvas.rs:17` — `use crate::state::radar_data::Sweep;` / `src/core/render.rs:11` — `use crate::state::radar_data::Scan;`
- `src/core/diagnostics.rs:18` — `use crate::state::{AlertsState, GpsState, MpingState};`

Load-bearing: yes. Core is genuinely *pure* (no I/O — see §4e) but not *independent*; it is structurally welded to `state`'s data model. To make `core` the base, `Scan`/`Sweep`/`AppState`/`AppCommand` would need to move below it.

**② `nexrad → state` (12) — leaf library reaches up into state.** Half of the heaviest cycle. The imported types are render/detection concepts that `nexrad` itself produces but that are *housed in `state`*:
- `src/nexrad/persistence_manager.rs:10` — `use crate::state::{self, AppState, UserPreferences};`
- `src/nexrad/detection/mod.rs:16` & `detection/features.rs:11` — `use crate::state::StormCellInfo;` (storm-cell detector importing its own output type from state)
- `src/nexrad/render_coordinator.rs:9` — `use crate::state::SweepIdentity;`
- `src/nexrad/globe_radar_renderer.rs:9`, `volume_ray_renderer.rs:10`, `gpu_renderer/mod.rs:10` — `use crate::state::RenderProcessing;`

Load-bearing: yes — but symptomatic of misplaced types (§3), not intrinsic coupling.

**③ `core → alerts` (3) — base importing a library.** `src/core/diagnostics.rs:16` — `use crate::alerts::{Alert, AlertSeverity};` (the pure `select_alert_at` hit-test needs the alert domain type). Direction-wise the base pulling from a library is inverted; here it is unavoidable given `Alert` lives in `alerts`.

**④ `state → ui` (2) — state importing the projection layer.** Two misplaced UI types (both load-bearing):
- `src/state/gps.rs:13` — `use crate::ui::LocationResult;` — `GpsState` stores `UnboundedSender<LocationResult>` / `drain_results() -> Vec<LocationResult>`; the enum is defined at `src/ui/site_modal.rs:31` as `pub(crate) enum LocationResult`. A GPS-result type owned by state's data flow lives in a UI modal.
- `src/state/app_mode.rs:5` — `use crate::ui::colors::mode;` — used at `app_mode.rs:48-50` to map `AppMode → mode::IDLE/ARCHIVE/LIVE` (favicon color). UI color constants pulled into state.

**⑤ `nexrad → core` (2) — library importing core, closing the 3-cycle.** `src/nexrad/persistence_manager.rs:9` — `use crate::core::{decide_persist, Effect, PersistDecision};`; `src/nexrad/gpu_renderer/inspect.rs:4` — `use crate::core::canvas::{…}`. Combined with ① and state→nexrad, this makes `core → state → nexrad → core` a genuine dependency cycle through the "functional core."

**Seam-bypass (not a direction violation, but the doc's stated goal):** `ui → state` = **51** vs `ui → subsystem` = **10**. Panels read `state` internals ~5× more than the `subsystem::Derived` view-model they are supposed to read exclusively. This is the "panels read subsystem internals directly at scale / P5 partial" admission, quantified.

## 3. Type placement — most-imported cross-boundary types

Top individual types crossing module boundaries (single + grouped `use` forms):

| rank | type | count | lives in | placement |
|---|---|---|---|---|
| 1 | `state::AppState` | ~28 | state | correct home, but the god-object hub (drives fan-in 100) |
| 2 | `geo::MapProjection` | 9 | geo | correct |
| 3 | `geo::Camera` | 6 | geo | correct |
| 4 | `data::ScanKey` | 6 | data | correct |
| 5 | `state::radar_data::{Scan, Sweep, Radial}` | ~8 | state | **MISPLACED** — domain data model; pulled into `core` (canvas/render) + `nexrad` (projection). Sole reason for core→state & part of nexrad→state |
| 6 | `nexrad::ScanBoundary` | 4 | nexrad | correct (library type) |
| 7 | `state::StormCellInfo` | 3 | state | **MISPLACED** — produced by `nexrad::detection`, housed in state → forces nexrad→state |
| 8 | `state::RenderProcessing` | 3 | state | **MISPLACED** — consumed by 3 nexrad renderers → forces nexrad→state |
| 9 | `state::SweepIdentity` | 3 | state | **MISPLACED** — consumed by `nexrad::render_coordinator` |
| 10 | `data::CachedSweep` / `data::ScanCompleteness` | 3 / 3 | data | correct |
| — | `mping::StormReport` | 3 | mping | correct |
| — | `state::AppCommand` (→ `core::Intent`) | 2+ | state | **MISPLACED** — core's central intent type is a state re-export |
| — | `ui::LocationResult` / `ui::colors::mode` | 1 / 1 | ui | **MISPLACED** — pulled *down* into state (state→ui) |

**Conclusion:** four clusters of misplaced types generate essentially every layering violation. Rehoming `radar_data::{Scan,Sweep,Radial}`, the render/detection structs (`RenderProcessing`, `SweepIdentity`, `StormCellInfo`), `AppCommand`, and `LocationResult`/color constants into a neutral base module would erase core→state, most of nexrad↔state, and state→ui in one move.

## 4. Entropy metrics

### (a) `#[allow(...)]` suppressions — 154 total

| module | total | too_many_args | dead_code | other |
|---|---|---|---|---|
| **nexrad** | 60 | 21 | 31 | 8 |
| **state** | 38 | 1 | 37 | 0 |
| **ui** | 32 | 29 | 3 | 0 |
| geo | 13 | 2 | 8 | 3 |
| data | 8 | 0 | 8 | — |
| app / core / net | 1 / 1 / 1 | — | — | — |
| alerts / mping / subsystem / main.rs | 0 | — | — | — |

Codebase-wide by kind: **`dead_code` 87**, **`too_many_arguments` 52** (matches the doc's "~53 sites"; concentrated in `ui` 29 + `nexrad` 21), `unused_imports` 8, `type_complexity`/`excessive_precision` 2 each. The two dominant signals: (1) **87 dead-code suppressions** (state 37 + nexrad 31) — a large kept-but-unused / speculative-`pub` surface; (2) 52 `too_many_arguments` in the paint/decode/timeline hot paths (`ui`, `nexrad`), the "bundle into context structs" cleanup the doc defers.

### (b) Files > 800 lines — 36 files

Largest: `state/playback.rs` **2651**, `geo/camera.rs` **2181**, `nexrad/realtime/streaming.rs` **2044** (the explicitly deferred file), `data/sites.rs` 1886, `state/timeline_view.rs` 1833, `state/radar_data.rs` 1833, `data/keys.rs` 1800, `state/playback_manager.rs` 1507, `state/acquisition.rs` 1500, `ui/vcp_forecast_modal.rs` 1490, `state/live_mode.rs` 1401, `nexrad/projection/status.rs` 1306, `state/vcp_forecast.rs` 1162, `nexrad/download_queue.rs` 1104, `app/worker_results.rs` **1056**, `core/diagnostics.rs` 1049, `ui/shortcuts.rs` 1041, … `main.rs` 945. State (11) and nexrad (10) dominate the >800 set. Most are logic-bearing and *are* tested (§5); the exception is `app/worker_results.rs` (see §5).

### (c) Function visibility

Codebase: `pub fn` **926**, `pub(crate) fn` **83**, `pub(super) fn` **107**, private `fn` **2745**.

| module | pub | pub(crate) | private | read |
|---|---|---|---|---|
| app | **0** | 36 | 27 | deliberate — shell methods, all crate-internal |
| core | 28 | 0 | 168 | deliberate narrow pure API |
| ui | 57 | 23 | 600 | mostly-private panels (good) |
| **nexrad** | **348** | 13 | 722 | **default-pub, not deliberate** |
| **state** | **300** | 6 | 646 | **default-pub, not deliberate** |
| subsystem | 15 | 0 | 22 | thin owners |

Verdict: visibility is used **deliberately in `app` (100% `pub(crate)`) and `core` (narrow `pub`)**, but the two hub modules `state`+`nexrad` export **~650 `pub fn` into a single-binary crate whose only external consumer is `lib.rs`'s tiny `data` facade** (per `lib.rs:15`). That `pub` is decorative, and the wide-open surface is precisely what enables the state↔nexrad↔ui cycles. Encapsulation is weakest exactly where the coupling is worst.

### (d) TODO/FIXME/HACK/XXX — 1 total

**One** marker in the entire tree (in `ui`); `todo!()`/`unimplemented!()` = 0. Inline debt markers are effectively absent — the project parks deferred work in prose (`docs/CORE_SHELL.md` "Related deferred cleanups", `CORE_SHELL_MIGRATION_LOG.md`) rather than code comments. Low inline-entropy signal; do not mistake for low actual debt (the cycles above are the debt).

### (e) `#[cfg(target_arch = "wasm32")]` blocks — 0

**Zero** `cfg(target_arch)` / `cfg(target…)` anywhere. There is no platform-splatter *and* no platform-gating — the crate compiles unconditionally to wasm32 (trunk). Platform coupling instead shows as direct `web_sys`/`js_sys`/`wasm_bindgen` calls, distributed as: nexrad 713, state **581**, ui 338, data 166, geo 139, alerts 98, mping 64, subsystem 23, net 17, app 10, main.rs 13. **`core` = 154, but 152 are `#[wasm_bindgen_test]` test attributes and 2 are in comments/tests — core has zero production platform calls**, so the doc's "core touches no web_sys/js_sys" claim holds. Notably, **`state` has 581 real platform hits** (e.g. `state/preferences.rs:221` `web_sys::window()`, `state/saved_events.rs:90`/`errors.rs:99` `js_sys::Date::now()`) — so `state` is *not* pure; it does localStorage/clock I/O inline, consistent with the doc's "honest note" that state is "data + already-pure decision fns" (not fully pure).

## 5. Test topology

Measured test functions (`#[test]` + `#[wasm_bindgen_test]`) — total **~1854** (nearly all tests use `#[wasm_bindgen_test]`; state's 515 aligns with the doc's "~533 pure tests"):

| module | tests | test mods | tests / kLOC | untested? |
|---|---|---|---|---|
| **state** | **515** | 38 | ~28 | well-covered |
| **nexrad** | **506** | 57 | ~19 | well-covered |
| **ui** | 274 | 31 | ~12 | covered (see note) |
| **core** | **140** | 14 | ~40 | best density — pure core claim holds |
| data | 137 | 10 | ~21 | |
| geo | 132 | 8 | ~23 | |
| alerts | 74 | 8 | ~30 | |
| mping | 44 | 8 | ~33 | |
| subsystem | 20 | 3 | ~21 | thin owners, lightly covered |
| net | 11 | 1 | ~29 | |
| **app** | **0** | 0 | **0** | **ZERO TESTS** |
| main.rs | 1 | — | ~1 | shell root, effectively untested |

**Modules with zero tests: `app` (3,883 LOC, 0 tests, 0 test modules).** `main.rs` has 1.

**Does test placement match "pure core is tested"?** Yes for the intent, with one real gap:
- **The core is the *best*-tested module by density (~40 tests/kLOC, 140 tests).** All large logic files carry substantial suites: `state/playback.rs`=61, `geo/camera.rs`=65, `state/radar_data.rs`=60, `state/timeline_view.rs`=39, `core/diagnostics.rs`=38, `data/keys.rs`=48, even `nexrad/realtime/streaming.rs`=24. The "pure logic is tested" claim is well supported.
- **The untested surface is the shell — mostly acceptable, with one genuine gap.** `main.rs` (945 LOC, 1 test) and `ui/top_bar.rs` (947, 1 test) are projection/composition and fit the "shallow visual QA only" carve-out. **But `src/app/worker_results.rs` (1,056 LOC, 0 tests)** is *logic-bearing* (decode-worker result ingestion, scan assembly, cache updates), not paint — and the whole `app` module (3,883 LOC of `update()` orchestration) has **zero** tests. This is exactly the doc's honest admission ("src/app/ … where most of the untested decision+effect tangle lives") and is the one place where the biggest untested file is a *decision* file rather than a *shell* file — the real coverage gap, and precisely the target of the P5/P6 migration that remains deferred.

**Bottom line:** the tested/untested split honors the standard — pure `core` and `state` decision logic are densely covered; the untested code is the shell. The single exception, `app/worker_results.rs` + the rest of `src/app/`, is logic masquerading as shell and is the concrete residual gap.
