# IndexedDB store

The browser-side cache. Holds pre-computed sweep blobs and per-scan
metadata so that scrubbing, elevation changes, and timeline queries do
not have to re-decompress and re-decode archive data on every render.
Implemented in [`src/data/indexeddb/`](../src/data/indexeddb/)
(`mod.rs` = the store, `helpers.rs` = key ranges/transaction plumbing,
`logic.rs` = the pure decision functions); wrapped by `DataFacade`
([`src/data/facade.rs`](../src/data/facade.rs)) for cache-eviction
policy.

The store runs in both the main thread and the Web Worker (the worker
holds a long-lived connection so ingest/render don't pay open-time
overhead). Access goes through `js_sys::global()` rather than `window`
so the same code compiles and runs in both contexts.

## 1. Schema

Database `nexrad-workbench`, version `6`. Three object stores; all keyed
by string.

| Store          | Key format                          | Value                                                            |
| -------------- | ----------------------------------- | ---------------------------------------------------------------- |
| `sweeps`       | `SITE\|SCAN_MS\|ELEV_NUM\|PRODUCT`  | `ArrayBuffer` (raw gate values + 72-byte header — see §2a)       |
| `scan_index`   | `SITE\|SCAN_MS`                     | `ScanIndexEntry` (structured-cloned via `serde-wasm-bindgen`)    |
| `scan_touches` | `SITE\|SCAN_MS`                     | Unix-millisecond timestamp (stored as a JS number); seeded by a scan's first `upsert_scan` and bumped on each render |

Schema upgrades are **destructive**: `onupgradeneeded` deletes every
existing object store and recreates them. The cache is treated as
ephemeral — losing it on a version bump is acceptable, and that simpler
upgrade path is what lets us evolve the binary blob layout freely.

## 2. Payload contents

The values in each store have specific shapes. The `sweeps` payload is a
hand-rolled binary blob optimized for zero-copy GPU upload, defined in
[`src/data/blob_format.rs`](../src/data/blob_format.rs); the
`scan_index` payload is a serialized Rust struct holding the metadata
the rest of the codebase reasons about, defined in
[`src/data/keys.rs`](../src/data/keys.rs).

### 2a. `sweeps` value: `PrecomputedSweep` blob

A single sweep's gate values, header, and per-radial metadata in one
contiguous `ArrayBuffer`. Serialized by `PrecomputedSweep::to_bytes`
and parsed at read time by `parse_sweep_header` (which returns scalar
metadata + byte offsets, no allocation).

**Header — 72 bytes, little-endian:**

| Offset  | Type | Field                   | Notes                                              |
| -------:| ---- | ----------------------- | -------------------------------------------------- |
| `0..4`  | u32  | `azimuth_count`         | Number of radials                                  |
| `4..8`  | u32  | `gate_count`            | Number of gates per radial                         |
| `8..16` | f64  | `first_gate_range_km`   | Distance from radar to the first gate's centroid   |
| `16..24`| f64  | `gate_interval_km`      | Spacing between adjacent gates                     |
| `24..32`| f64  | `max_range_km`          | Distance to the far edge of the last gate          |
| `32..36`| f32  | `scale`                 | Physical = (raw − offset) / scale                  |
| `36..40`| f32  | `offset`                | Same                                               |
| `40..44`| u32  | `radial_count`          | Echoed for convenience (== `azimuth_count` today)  |
| `44`    | u8   | `data_word_size`        | `1` for u8 (REF, VEL, SW), `2` for u16 (CFP, dual-pol) |
| `45`    | u8   | `format_version`        | `0` = no radial_times, `1` = with radial_times     |
| `46..48`| —    | reserved                | 2 bytes of padding                                 |
| `48..52`| f32  | `mean_elevation`        | Average elevation angle of the sweep (degrees)     |
| `52..56`| —    | reserved                | 4 bytes of f64 alignment pad                       |
| `56..64`| f64  | `sweep_start_secs`      | Unix seconds, earliest radial collection time      |
| `64..72`| f64  | `sweep_end_secs`        | Unix seconds, latest radial collection time        |

**Body — three array sections:**

```
72 ──────────────────► azimuths              (f32 × azimuth_count, sorted)
72 + az·4 ───────────► radial_times          (f64 × azimuth_count, version 1 only;
                                              parallel to azimuths, Unix seconds)
72 + az·4 + rt_size ─► gate_values            (u8 or u16 × azimuth_count × gate_count,
                                              row-major: radial-major, gate-minor)
```

The fragment shader applies `physical = (raw - offset) / scale`
per-pixel. Raw values `0` (below threshold) and `1` (range folded) are
sentinels — the shader checks `v > 1.5` before converting. Because the
linear transform is invariant under interpolation, bilinear/smoothing
filters on the GPU operate on raw values.

### 2b. `scan_index` value: `ScanIndexEntry`

A `Serialize`/`Deserialize` Rust struct. Round-trips through
`serde-wasm-bindgen` (no JSON detour) and IDB stores the result via
the structured-clone algorithm. Five fields, two roles:

- **Plan** (`vcp`): the full ordered elevation cuts the radar intends to
  scan. Static — comes from the Message Type 5 record. Carries waveform,
  PRF, azimuth-rate, and SAILS/MRLE/base-tilt flags per cut.
- **Cached state** (`cached_sweeps`): the realized subset that has been
  ingested and stored under this scan key. Each entry corresponds to one
  VCP cut; carries measured-from-radial timing and the products whose
  blobs were successfully written.

The two columns are correlated but neither derives from the other —
joined on `elevation_number`.

| Field               | Type                       | Meaning                                                                                          |
| ------------------- | -------------------------- | ------------------------------------------------------------------------------------------------ |
| `scan`              | `ScanKey`                  | `{ site, scan_start: UnixMillis }` — the storage key, kept in the value for self-description     |
| `vcp`               | `Option<ExtractedVcp>`     | Full Volume Coverage Pattern. `None` until the volume header record is decoded                   |
| `file_name`         | `Option<String>`           | Source archive file name (archive) or synthetic `live_<site>_<ts>.nexrad` (real-time)            |
| `cached_sweeps`     | `Vec<CachedSweep>`         | The sweeps actually stored under this scan key. Drives the timeline + completeness               |
| `total_size_bytes`  | `u64`                      | Sum of every sweep blob's size for this scan; drives `total_cache_size` and eviction sizing      |

Derived (accessor methods, not stored):

| Method                              | Definition                                                              |
| ----------------------------------- | ----------------------------------------------------------------------- |
| `has_vcp() -> bool`                 | `vcp.is_some()`                                                         |
| `planned_sweep_count() -> Option<u32>` | `vcp.as_ref().map(|v| v.elevations.len() as u32)`                       |
| `cached_sweep_count() -> u32`       | Count of unique `elevation_number` values                               |
| `end_timestamp_secs() -> Option<i64>` | `cached_sweeps.iter().map(|s| s.end as i64).max()`                      |
| `completeness() -> ScanCompleteness` | `from_counts(has_vcp(), cached_sweep_count(), planned_sweep_count())`   |

`CachedSweep` (each entry of `cached_sweeps`):

| Field              | Type          | Meaning                                                                          |
| ------------------ | ------------- | -------------------------------------------------------------------------------- |
| `start`, `end`     | `f64`         | Unix seconds, sub-second precision; earliest/latest radial collection time       |
| `elevation`        | `f32`         | Elevation angle in degrees                                                       |
| `elevation_number` | `u8`          | 1-based index used in sweep storage keys; the join key against `vcp.elevations`  |
| `start_azimuth`    | `f32`         | Azimuth (degrees) of the chronologically first radial — used for VCP forecasts   |
| `cached_products`  | `Vec<String>` | Product strings (e.g. `"reflectivity"`) whose sweep blobs were successfully stored under this scan key |

`ExtractedVcp` and `ExtractedVcpElevation`
([`src/data/vcp_timing.rs`](../src/data/vcp_timing.rs)) carry the
per-volume scan strategy (cuts, waveforms, azimuth rates) used by the
timing model and forecast UI.

### 2c. `scan_touches` value: a timestamp

A bare Unix-millisecond timestamp (a JS number). Owns LRU bookkeeping
end-to-end: seeded by a scan's **first** `upsert_scan` at ingest time so
freshly-cached scans have a place in the order, and bumped by
`touch_scan` after every sweep render. `evict_to_size` joins this store
with `scan_index` and sorts by the touch value. See §11 for why access
tracking lives in its own store rather than as a field on
`ScanIndexEntry`.

## 3. Why three stores

The split is driven by access patterns. Sweep blobs are large
(megabytes) and read by exact key on the render hot path; they need
zero-copy access from JS to GPU. Scan-index entries are tiny, queried
in bulk for timeline and eviction work, and benefit from structured
clone (so they round-trip Rust types without a JSON detour).
`scan_touches` is split off because access bumps would otherwise race
chunk-ingest's read-modify-write of the index entry — see §11.

Co-locating sweeps + index in a single store would force every
timeline query to either drag blob data through memory, or maintain a
parallel index anyway.

## 4. Concurrency model

### 3a. Open coalescing

`open()` is safe to call concurrently. The first caller drives the
underlying `indexedDB.open(...)`; subsequent callers that arrive while
it is in flight register a `oneshot::Receiver` and wait for the same
completion. Without this, multiple `spawn_local` tasks racing on a
fresh store would each issue their own open, since the database handle
is only stored after the initial `.await` resumes.

The state machine has three states (see `OpenState`):

```
   Closed  ──open()──►  Opening(waiters)  ──open_database resolves──►  Open(IdbDatabase)
                          ▲                                                │
                          └────────── reset to Closed on error ────────────┘
```

### 3b. The `.await`-inside-readwrite trap

In WASM, IDB transactions auto-commit when the event loop yields. Any
`.await` inside a readwrite transaction yields, the transaction
commits, and any subsequent `IdbObjectStore` operation fails silently
or with `TransactionInactiveError`.

The store enforces this at the type level. Writes go through
`write_tx`, which hands the closure a `WriteTransaction`:

```rust
async fn write_tx<F, T>(&self, store_names: &[&str], f: F) -> Result<T, DataError>
where
    F: FnOnce(&WriteTransaction) -> Result<T, DataError>;
```

The closure is `FnOnce` (not `async FnOnce`), so the compiler rejects
`.await` inside it. `WriteTransaction` additionally holds
`PhantomData<*const ()>` to make the type `!Send`, blocking accidental
moves across await points in non-WASM contexts.

The actual transaction-completion wait (`wait_for_transaction`) happens
*after* the closure returns, which is the only safe place to await.

### 3c. Atomic scan mutations

`upsert_scan`, deletion, and access touches perform scan mutations in a
single readwrite transaction spanning all three stores. Initial reads are
queued synchronously; their request callbacks synchronously queue dependent
writes before the transaction is allowed to complete. There is still no
`await` inside the transaction.

IndexedDB serializes overlapping readwrite transactions across database
connections, so this is correct across workers and tabs. The merge key is
`(scan, elevation_number, product)`: product membership is unique and sorted,
retries with identical bytes are no-ops, and replacement accounting uses the
actual old blob length. Concurrent differing bytes or timing are unexpected;
the later serialized transaction wins and logs a warning.

## 5. Key-range queries

Storage keys are pipe-delimited prefixes. The store exploits this so
that single-site or single-scan operations don't have to scan the full
table.

```
sweeps:      "KDMX|1715000000000|3|reflectivity"
              ▲      ▲              ▲   ▲
              │      │              │   └─ product
              │      │              └───── elevation number
              │      └──────────────────── scan start (Unix ms)
              └─────────────────────────── site

scan_index:  "KDMX|1715000000000"
```

Two range helpers cover the common cases:

| Range                 | Bounds                                  | Used by                                              |
| --------------------- | --------------------------------------- | ---------------------------------------------------- |
| `site_prefix_range`   | `"KDMX\|"` .. `"KDMX\|\u{FFFF}"`         | `list_scans`                                         |
| `scan_prefix_range`   | `"KDMX\|MS\|"` .. `"KDMX\|MS\|\u{FFFF}"`  | `delete_scan` (sweep-blob prefix delete) |

`\u{FFFF}` sorts after any character that appears in real keys, giving
a tight inclusive upper bound. `delete_scan` issues a single
range-delete against `sweeps` rather than constructing one key per
`(elevation × product)` pair, so it is correct regardless of which
products were actually stored.

### Lex order vs numeric order

`SITE|MS` keys put a decimal millisecond timestamp after the pipe.
Lexicographic order matches numeric order **only when all timestamps
have the same number of digits**. Unix-millisecond timestamps are
13 digits from 2001 (`10^12`) through 2286 (`10^13 − 1`), which covers
every value the workbench will ever see. The `list_scans` time filter
runs in Rust after the site-prefix range scan, so even pathological
inputs would degrade to "read more entries than necessary," not
"return wrong results."

## 6. API surface

Construct with `IndexedDbStore::new()` (cheap — no I/O). Call `open()`
once before use; subsequent calls are no-ops. All methods are `async`.

### The combined write: `upsert_scan`

| Method                                              | Purpose                                                                        |
| --------------------------------------------------- | ------------------------------------------------------------------------------ |
| `upsert_scan(&ScanHeader, &[ElevationUpload])`      | The single write entry point: create-or-merge blobs + scan-index entry, atomic |

`ScanHeader` carries the scan key plus the header-level metadata
(`vcp`, `file_name`); each `ElevationUpload` carries one elevation's
timing (`SweepTiming`) and its product blobs (`ProductBlob`s). All
types live in [`src/data/keys.rs`](../src/data/keys.rs).

`upsert_scan` dispatches internally on `scan_availability`:

- **First write** (no existing entry): derive a fresh `ScanIndexEntry`
  from the header and the uploads, then write blobs + index + an
  initial `scan_touches = now` in one cross-store readwrite
  transaction. The initial touch is what gives the scan its place in
  the LRU order.
- **Merge** (entry exists): fill in `vcp` / `file_name` only if
  currently `None`, update the one manifest row for each supplied elevation,
  union its durable product keys, and apply actual blob replacement deltas to
  `total_size_bytes`.
  **`scan_touches` is preserved** — a chunk-ingest flush doesn't
  refresh the access timestamp (which would conflate writes with
  reads and break LRU).

Contract details:

- Blobs + index are written atomically — a mid-write failure can't
  leave orphan blobs or a phantom entry. The browser-quota pre-check
  (§Quota) covers the blob batch.
- `ElevationUpload`s with empty `blobs` are dropped silently: the
  manifest is derived from the blobs actually being written, so no
  `CachedSweep` can claim a sweep that doesn't exist in storage.
- An empty `elevations` slice is permitted — the entry is written (or
  its header fields merged) without any blob writes (used by
  chunk-ingest flushes that don't produce new blobs).
- Concurrent upserts serialize at the database. They retain independent
  elevations and never publish blobs without the corresponding manifest.

### Sweep blobs

| Method                                                 | Purpose                                                        |
| ------------------------------------------------------ | -------------------------------------------------------------- |
| `get_sweep(&SweepDataKey) -> Option<ArrayBuffer>`      | Single-key read; returns the JS buffer directly (no Rust copy) |

`get_sweep` deliberately does not deserialize. Sweeps are uploaded to
the GPU as `R32F` textures from a JS-side `ArrayBuffer`, and dragging
the bytes through Rust memory before transferring them would add a
multi-MB copy on the render hot path. Taking a `&SweepDataKey` instead
of a stringified key lets the method extract the scan portion to fire
`touch_scan` after the read (see §11).

### Scan index

| Method                                                            | Purpose                                                  |
| ----------------------------------------------------------------- | -------------------------------------------------------- |
| `scan_availability(&ScanKey) -> Option<ScanIndexEntry>`           | Single-key read                                          |
| `list_scans(site, start, end) -> Vec<ScanIndexEntry>`             | Site-prefix range, then time filter, sorted by start     |

There is no entry-only writer: every index update goes through
`upsert_scan` so the blobs and the metadata that describes them stay
in lockstep. An `upsert_scan` call with an empty `elevations` slice
covers the "metadata-only update" case.

### Cache management

| Method                                | Purpose                                                                                          |
| ------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `total_cache_size() -> u64`           | Sum of `total_size_bytes` across all `scan_index` entries                                        |
| `evict_to_size(target_bytes) -> u32`  | Read entries + touches, sort by `scan_touches` (absent ⇒ evict-first), delete oldest first |
| `delete_scan(&ScanKey) -> u64`        | One cross-store transaction: range-delete blobs + drop index entry + drop touch. Used by eviction; public so integration tests can verify per-scan deletion |
| `clear_all()`                         | `IdbObjectStore::clear` on all three stores; preserves schema and version                        |

`clear_all` does **not** call `deleteDatabase`. `deleteDatabase` blocks
until every connection (including the worker's) closes, which would
hang while the worker holds its long-lived connection.

### Quota

| Method (associated)                                       | Purpose                                                                  |
| --------------------------------------------------------- | ------------------------------------------------------------------------ |
| `IndexedDbStore::estimate_storage_quota() -> Option<…>`   | Wraps `navigator.storage.estimate()`; works in Window and Worker         |

Returned as `StorageQuotaEstimate { quota, usage }` (bytes); call
`.remaining()` for available space. `upsert_scan` consults this before
writing and returns `DataError::QuotaExceeded` when the blob batch
plus the ingest headroom (5 MB, `QuotaPolicy::DEFAULT` in
[`src/data/quota.rs`](../src/data/quota.rs)) would not fit, instead of
letting IDB fail mid-transaction. The decision itself is the pure
`decide_quota` in `indexeddb/logic.rs` (unit-tested).

## 7. Errors

`DataError` is an enum, not a string:

| Variant              | When                                                |
| -------------------- | --------------------------------------------------- |
| `NotOpen`            | Operation issued before `open()` succeeded          |
| `TransactionFailed`  | Transaction-level failure (open, commit, scope)     |
| `RequestFailed`      | Single-request failure inside a transaction         |
| `QuotaExceeded`      | Browser storage estimate insufficient for the batch |
| `SerdeError`         | `serde-wasm-bindgen` round-trip failed              |

Errors from `JsValue` are formatted via the `js_err` helper, which
extracts `name`/`message` when the value looks like a `DOMException`,
and falls back to `{:?}` otherwise. This gives readable strings like
`"QuotaExceededError: ..."` instead of opaque `JsValue(...)` blobs.

`DataError::kind()` classifies every error as `Transient` (retry may
succeed: `NotOpen`, aborted/timed-out
transactions), `Quota` (succeeds only after eviction), or `Permanent`
— so callers pick retry/give-up/evict behavior without matching every
variant. IDB failures are classified by the DOMException name `js_err`
puts at the front of the message.

## 8. Data flow

```
  ┌────────────────────┐    raw bytes    ┌────────────────────┐
  │ Acquisition        │────────────────►│ Worker: ingest     │
  │ (main thread)      │  postMessage    │ decode + extract   │
  └────────────────────┘  (transferable) └─────────┬──────────┘
                                                   │
                                       upsert_scan (atomic)
                                                   │
                                                   ▼
                                          ┌──────────────────┐
                                          │   IndexedDB      │
                                          │   sweeps         │
                                          │   scan_index     │
                                          └────────┬─────────┘
                                                   │
                                       get_sweep (worker)
                                       scan_availability / list_scans (main)
                                                   │
                                                   ▼
                                          ┌──────────────────┐
                                          │ Render:          │
                                          │ worker reads,    │
                                          │ transfers buffer │
                                          │ → main GPU R32F  │
                                          └──────────────────┘
```

Two notable shapes:

- **Per-chunk ingest** (`worker_ingest_chunk`,
  `src/nexrad/decode/worker_api/ingest.rs`) accumulates sweeps in a
  thread-local until an elevation completes, then hands a `ScanHeader`
  plus the completed `ElevationUpload`s to `upsert_scan`, which does
  the read-merge-write internally (guarded per scan key, §3c). The
  per-worker `CHUNK_ACCUM` thread-local serializes per-scan so no
  concurrent writer exists in practice. The Start chunk also reads any
  pre-existing entry for the scan key and pre-populates
  `completed_elevations` so a resume doesn't reprocess sweeps that are
  already cached.

- **Archive ingest** (`worker_ingest`) decodes the entire volume, then
  writes the scan-index entry + all sweep blobs in a single
  `upsert_scan` call. Archive and real-time entries for the same
  physical volume can coexist in the cache when their `scan_start`
  keys differ; LRU eviction reclaims space over time.

## 9. Test coverage

Two tiers, both wired into CI:

**Pure-Rust unit tests** (`#[wasm_bindgen_test]` blocks in `src/`, run in
node — no browser). Cover the decision math: key-range bound strings,
eviction sort order, throttle decision boundaries, quota headroom math,
time-window filter inclusivity, `ScanIndexEntry` accessor results
including the SAILS/MRLE overshoot completeness case. ~50 ms total. Runs
on every commit via the pre-commit hook (`cargo test --bin
nexrad-workbench`) and in the `tests` CI job.

**Browser-driven integration tests** (`tests/idb.rs`, `run_in_browser`,
real IndexedDB in headless Chromium). Cover the orchestration that the
pure-Rust mock can't model:

- Cross-store atomicity: a first `upsert_scan` writes blobs + index +
  `scan_touches` together (even with no uploads); `delete_scan` clears
  all three; `clear_all` empties everything.
- Upsert contract: a merge preserves the existing touch; header fields
  fill in only when currently `None`; incremental flushes keep the
  manifest and the blobs in agreement; phantom (blob-less) elevations
  are dropped. `get_sweep` bumps the touch (verified by polling for
  the fire-and-forget write).
- Eviction integration: oldest-touch-first ordering against a real DB,
  and the "missing-touch evicts first" cleanup path that the
  pure-Rust eviction-order test claims.
- Round-trip fidelity: `ScanIndexEntry` with VCP, `CachedSweep`, and
  `cached_products` survive structured-clone via `serde-wasm-bindgen`.

Each test runs against a fresh, uniquely-named database (counter +
high-resolution clock) so siblings don't collide. `IndexedDbStore`
exposes `with_database_name` for this purpose. The `read_touch` method
is `#[doc(hidden)] pub` for the same reason — production code never
needs single-key touch reads.

Runs in the `idb-integration` CI job (~30 s including chromium install).
Not part of pre-commit.

## 10. What lives outside this module

- The `ScanIndexEntry` struct shape and accessor methods
  ([`src/data/keys.rs`](../src/data/keys.rs)) — `has_vcp()`,
  `planned_sweep_count()`, `cached_sweep_count()`,
  `end_timestamp_secs()`, `completeness()`. The IDB module stores and
  retrieves entries but doesn't reason about their plan/cached split.

- LRU policy decisions and browser-quota thresholds
  ([`src/data/facade.rs`](../src/data/facade.rs)) —
  `IndexedDbStore::evict_to_size` only knows "evict until this
  size"; whether to evict (and to what target) is `DataFacade`'s job.

- Cross-tab scan mutations rely on IndexedDB transaction serialization; no
  process-local lock or worker affinity is required.

## 11. Access-time tracking

LRU eviction wants the order "least recently *used*," and `scan_touches`
is the single source of truth for that. Two writers feed it:

- `upsert_scan` seeds an entry with `now` on a scan's **first** write,
  so freshly cached scans have a place in the LRU order before any
  render.
- `touch_scan(&ScanKey)` (fired fire-and-forget by `get_sweep` after a
  render) bumps the timestamp.

**Why a separate store, not a field on `ScanIndexEntry`.**
Chunk-ingest is a read-modify-write on the index entry to merge new
chunk state in (`upsert_scan`'s internal `scan_availability` → merge →
write). If a touch did the same RMW dance against the same entry, the
two could interleave:

```
T1  ingest reads entry (3 sweeps)
T2  touch  reads entry (3 sweeps)
T3  ingest writes (4 sweeps, +new sweep meta)
T4  touch  writes (3 sweeps, last_accessed_at = now)   ← clobbers ingest
```

The window is sub-millisecond and the race is rare, but when it
happens it loses a chunk's worth of merge state. A dedicated
single-field store (`scan_touches`) sidesteps the problem entirely:
the touch path never reads or writes `scan_index`, so it cannot
collide with merges.

**Why merge writes don't refresh the touch.** `upsert_scan`'s merge
path deliberately leaves `scan_touches` alone. Otherwise every
chunk-ingest flush during a streaming volume would refresh the access
timestamp, re-conflating "last write" with "last access" — the exact
problem the split was supposed to fix. Only the first write for a
scan key seeds the touch; the create-vs-merge dispatch is internal to
`upsert_scan`, so callers can't get this contract wrong.

**Throttle.** `IndexedDbStore` holds an in-memory
`HashMap<ScanKey, UnixMillis>` of recent touches. A second touch for
the same scan within 60 s is a no-op. This keeps fast scrubbing from
queueing dozens of writes per second; LRU only needs minute-grain
ordering anyway. The map is per-store, lost on tab close — that's
fine, the IDB store has the persistent state.

**Fire-and-forget.** `touch_scan` returns immediately after the
throttle check. The IDB write is dispatched via
`wasm_bindgen_futures::spawn_local` and the render path doesn't await
it. Errors are logged at debug level. Worst case, a touch is lost and
the next one (after the throttle expires) writes a slightly newer
timestamp, which is still correct LRU behaviour.

**Eviction sort.** `evict_to_size` reads `scan_index` and
`scan_touches` once each, then sorts:

```rust
entries.sort_by_key(|e| touches.get(&e.scan).copied().unwrap_or(UnixMillis(0)).0);
```

A missing touch entry sorts to position 0 → evicted first. With the
single `upsert_scan` entry point a touch-less entry can no longer be
produced by a caller mistake; it remains possible only anomalously
(fresh schema upgrade, corrupted commit), and eviction cleans it up
first.
