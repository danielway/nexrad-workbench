# Functional-Core / Thin-Shell Migration — Decisions Log

Running log of decisions, assumptions, and resolutions made during the autonomous
migration to the functional-core / thin-shell standard (see
[CORE_SHELL.md](CORE_SHELL.md)). Branch: `functional-core-migration`.

Each entry: the ambiguity or choice, the option taken, and the rationale. The
consolidated manual-QA checklist and final report live at the end of this file
once the migration is complete.

## Conventions adopted

- **`core` module location.** A new `src/core/` module holds the contract
  vocabulary (`Intent`, `Effect`) and the pure decision functions the shell
  executes. Pre-existing pure decision fns in `src/state/**` stay where they are
  (moving 500+ tests and their call sites is pure churn and risk); `core`
  re-exports them so there is one canonical "this is the core" surface, matching
  the roadmap's "pure `derive_*` fns re-homed under the core" with a re-export
  rather than a physical move where a move buys nothing.
- **Behavior preservation is paramount.** Every numeric default, clamp, ordering,
  and observable behavior is preserved exactly. Where a decision is extracted into
  a pure fn, the shell call site is reduced to: build inputs → call core → execute
  returned effects, with the same values as before.

## Decisions

### D0 — Baseline
- Branched `functional-core-migration` off `simplify-user-interface` @ `09241a3`.
- Verified green baseline before any change: `cargo check` clean,
  `cargo clippy --bin nexrad-workbench -- -D warnings` clean,
  `cargo test --bin nexrad-workbench` = 510 passed / 0 failed.

### D1 — P0 contract types
- **Module name `core`.** Declared `mod core;` in `main.rs`. Rust's std `core`
  crate is referenced only via leading-`::` paths (derive macros) and one
  qualified `i_overlay::core::` import, so a crate-level `core` module does not
  collide. Chose `core` over `app_core`/`fcs` because the docs and standard call
  it "the core" — clarity wins.
- **`Intent` = alias of `AppCommand`** (`pub use crate::state::AppCommand as Intent`)
  rather than a rename. Renaming 200+ call sites is churn with no behavior value;
  the alias establishes the contract name now and the type grows into the superset
  as P5 folds UI mutations into it.
- **`Effect` is the simple cross-cutting effect type, not a monolith.** The
  codebase already returns effects-as-data via local enums (`PrevSweepAction`,
  `DesiredDisplay`, `QueueAction`). The roadmap says "imitate PrevSweepAction."
  So `core::Effect` carries only effects that share one executor (URL push,
  prefs save, geolocation later); heavy per-decision effects (GPU buffer uploads,
  worker dispatch) keep their own local action enums at the granularity that fits
  a buffer/`postMessage`. This is the lowest-risk, most idiomatic reading and
  avoids an Effect variant that has to carry `Vec<f32>` + `egui::Context`.
- **Derive additions (behavior-neutral):** `ViewState` gained `Debug, Clone,
  PartialEq`; `UserPreferences` gained `Debug`; `InterpolationMode` gained
  `Debug` — all so `Effect` can carry them and be `assert_eq!`-able in headless
  tests. Pure data enums/structs; no logic change.
- **`#[allow(unused_imports)]` on the `core` re-exports** until P1 consumes them
  (bin crates flag `pub use` as unused). Same staging idiom as the existing
  `#[allow(dead_code)]` on `PrevSweepAction`.

### D2 — P1 effect boundary + injectable clock
- **Wall-clock throttle, not monotonic.** The old throttle used
  `web_time::Instant` (monotonic). The roadmap says "extend the existing
  `FrameNow` seam," and `FrameNow` is wall-clock (Unix seconds from `Date::now`).
  Switched the throttle to compare injected wall-clock seconds. Behavior is
  observably identical for a ~1/sec gate; the only divergence is if the system
  clock jumps backward by >1s mid-session (extremely rare, and self-corrects next
  push). This is the cost of making the decision clock-injectable/testable, which
  the standard requires. Seeded `last_url_push_secs` with the construction-time
  wall clock so the first push still waits a throttle window (preserving the old
  `Instant::now()` seed semantics).
- **`decide_persist` returns `PersistDecision`, not bare `Vec<Effect>`.** The
  roadmap sketches `-> Vec<Effect>`; I return `{ effects, last_url_push_secs?,
  saved_preferences? }`. Reason: the throttle marker and prefs snapshot are the
  decision's "next state" — folding them into the return keeps the bookkeeping
  pure and directly testable ("unchanged prefs emit no `SavePreferences` and
  advance nothing"), instead of having the manager re-derive them by inspecting
  the effects. `decision.effects` still *is* the `Vec<Effect>`.
- **Effect runtime lives at `src/app/effects.rs`** as `impl WorkbenchApp`
  (`apply_effects`/`apply_effect`). One exhaustive match so a new variant forces
  a shell decision. This is the "effect runtime / imperative shell" box from the
  standard's diagram.
