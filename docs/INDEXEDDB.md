# IndexedDB store

The browser-side cache. Holds pre-computed sweep blobs and per-scan
metadata so that scrubbing, elevation changes, and timeline queries do
not have to re-decompress and re-decode archive data on every render.
Implemented in [`src/data/indexeddb.rs`](../src/data/indexeddb.rs);
wrapped by `DataFacade` for cache-eviction policy.

The store runs in both the main thread and the Web Worker (the worker
holds a long-lived connection so ingest/render don't pay open-time
overhead). Access goes through `js_sys::global()` rather than `window`
so the same code compiles and runs in both contexts.

## 1. Schema

Database `nexrad-workbench`, version `3`. Two object stores; both keyed
by string.

| Store        | Key format               | Value                                                          |
| ------------ | ------------------------ | -------------------------------------------------------------- |
| `sweeps`     | `SITE\|SCAN_MS\|ELEV_NUM\|PRODUCT` | `ArrayBuffer` (raw gate values + 72-byte header — see `keys.rs`) |
| `scan_index` | `SITE\|SCAN_MS`            | `ScanIndexEntry` (structured-cloned via `serde-wasm-bindgen`)  |

Schema upgrades are **destructive**: `onupgradeneeded` deletes every
existing object store and recreates them. The cache is treated as
ephemeral — losing it on a version bump is acceptable, and that simpler
upgrade path is what lets us evolve the binary blob layout freely.

## 2. Payload contents

The values in each store have specific shapes. The `sweeps` payload is a
hand-rolled binary blob optimized for zero-copy GPU upload; the
`scan_index` payload is a serialized Rust struct holding the metadata
the rest of the codebase reasons about. Both are defined in
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
the structured-clone algorithm.

| Field                     | Type                  | Meaning                                                                                       |
| ------------------------- | --------------------- | --------------------------------------------------------------------------------------------- |
| `scan`                    | `ScanKey`             | `{ site, scan_start: UnixMillis }` — the storage key, kept in the value for self-description  |
| `has_vcp`                 | `bool`                | True once the volume header (record 0) has been ingested                                      |
| `vcp`                     | `Option<ExtractedVcp>`| Full Volume Coverage Pattern: number + ordered elevations with waveform/PRF/SAILS/MRLE flags  |
| `expected_records`        | `Option<u32>`         | Predicted total records, derived from `vcp.elevations.len()`                                  |
| `present_records`         | `u32`                 | Records actually ingested so far. Equals `expected_records` when complete                     |
| `file_name`               | `Option<String>`      | Source archive file name (archive ingest only); `None` for real-time scans                    |
| `total_size_bytes`        | `u64`                 | Sum of every sweep blob's size for this scan; drives `total_cache_size` and eviction          |
| `updated_at`              | `UnixMillis`          | Last write time. Bumped by every `merge_chunk`                                                |
| `last_accessed_at`        | `UnixMillis`          | Set at creation; not bumped by reads (see "Access-time tracking" below)                       |
| `end_timestamp_secs`      | `Option<i64>`         | Latest radial collection time across all sweeps (Unix seconds); fills in after decode         |
| `sweeps`                  | `Option<Vec<SweepMeta>>` | Per-sweep metadata: start/end times, elevation, `available_products`. Drives the timeline |
| `has_precomputed_sweeps`  | `bool`                | True once at least one sweep blob is stored under this scan key                               |

`SweepMeta` (each entry of `sweeps`):

| Field                | Type        | Meaning                                                                          |
| -------------------- | ----------- | -------------------------------------------------------------------------------- |
| `start`, `end`       | `f64`       | Unix seconds, sub-second precision; earliest/latest radial collection time       |
| `elevation`          | `f32`       | Elevation angle in degrees                                                       |
| `elevation_number`   | `u8`        | 1-based index used in sweep storage keys                                         |
| `start_azimuth`      | `f32`       | Azimuth (degrees) of the chronologically first radial — used for VCP forecasts   |
| `available_products` | `Vec<String>` | Product strings (e.g. `"reflectivity"`) for which a sweep blob was successfully stored |

`ExtractedVcp` and `ExtractedVcpElevation` carry the per-volume scan
strategy (cuts, waveforms, azimuth rates) used by the timing model and
forecast UI.

## 3. Why two stores

The split is driven by access patterns. Sweep blobs are large
(megabytes) and read by exact key on the render hot path; they need
zero-copy access from JS to GPU. Scan-index entries are tiny, queried
in bulk for timeline and eviction work, and benefit from structured
clone (so they round-trip Rust types without a JSON detour).

Co-locating them in a single store would force every timeline query to
either drag blob data through memory, or maintain a parallel index
anyway.

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

### 3c. Read-modify-write is not atomic

Because of (3b), no single transaction can read a value, mutate it,
and write it back — the `await` between read and write would commit
the read transaction. The IDB layer therefore exposes only `get` and
`put`; callers compose them.

This is racy in principle. In practice, the only RMW caller is the
real-time chunk ingest loop, which serializes per-scan via the
per-worker `CHUNK_ACCUM` thread-local: each scan has at most one
writer at a time. The invariant lives at the call site
(`worker_api/ingest.rs`) where the developer can see it, rather than
hidden inside the IDB module.

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
| `scan_prefix_range`   | `"KDMX\|MS\|"` .. `"KDMX\|MS\|\u{FFFF}"`  | crate-private `delete_scan` (sweep-blob prefix delete) |

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

### Combined writes

| Method                                                       | Purpose                                                                                |
| ------------------------------------------------------------ | -------------------------------------------------------------------------------------- |
| `put_scan(&entry, &[(key, bytes)])`                          | Atomic write of a scan-index entry plus its sweep blobs in one cross-store transaction |

`put_scan` is the canonical write path — the `sweeps` and `scan_index`
stores hold tightly coupled state (an index entry's `total_size_bytes`
and `sweeps` Vec describe what's in the blob store for that scan), so
they're written together. A mid-write failure can't leave orphan
blobs or a phantom entry. The browser-quota pre-check covers the blob
batch; passing an empty `sweep_blobs` slice writes the entry alone
(used by chunk-ingest flushes that don't produce new blobs).

### Sweep blobs

| Method                                         | Purpose                                                        |
| ---------------------------------------------- | -------------------------------------------------------------- |
| `get_sweep(key) -> Option<ArrayBuffer>`        | Single-key read; returns the JS buffer directly (no Rust copy) |

`get_sweep` deliberately does not deserialize. Sweeps are uploaded to
the GPU as `R32F` textures from a JS-side `ArrayBuffer`, and dragging
the bytes through Rust memory before transferring them would add a
multi-MB copy on the render hot path.

### Scan index

| Method                                                            | Purpose                                                  |
| ----------------------------------------------------------------- | -------------------------------------------------------- |
| `scan_availability(&ScanKey) -> Option<ScanIndexEntry>`           | Single-key read                                          |
| `list_scans(site, start, end) -> Vec<ScanIndexEntry>`             | Site-prefix range, then time filter, sorted by start     |

There is no entry-only writer: every index update goes through
`put_scan` so the blobs and the metadata that describes them stay in
lockstep. A `put_scan` call with an empty `sweep_blobs` slice covers
the "metadata-only update" case.

### Cache management

| Method                                | Purpose                                                                        |
| ------------------------------------- | ------------------------------------------------------------------------------ |
| `total_cache_size() -> u64`           | Sum of `total_size_bytes` across all `scan_index` entries                      |
| `evict_to_size(target_bytes) -> u32`  | Read entries once, sort by `last_accessed_at`, delete oldest until at/below target |
| `clear_all()`                         | `IdbObjectStore::clear` on both stores; preserves schema and version           |

`clear_all` does **not** call `deleteDatabase`. `deleteDatabase` blocks
until every connection (including the worker's) closes, which would
hang while the worker holds its long-lived connection.

### Quota

| Method (associated)                                       | Purpose                                                                  |
| --------------------------------------------------------- | ------------------------------------------------------------------------ |
| `IndexedDbStore::estimate_storage_quota() -> Option<…>`   | Wraps `navigator.storage.estimate()`; works in Window and Worker         |

Returned as `StorageQuotaEstimate { quota, usage }` (bytes); call
`.remaining()` for available space. `put_scan` consults this before
writing and returns `DataError::QuotaExceeded` when the blob batch
plus 5 MB headroom would not fit, instead of letting IDB fail
mid-transaction.

## 7. Errors

`DataError` is an enum, not a string:

| Variant              | When                                                |
| -------------------- | --------------------------------------------------- |
| `NotOpen`            | Operation issued before `open()` succeeded          |
| `TransactionFailed`  | Transaction-level failure (open, commit, scope)     |
| `RequestFailed`      | Single-request failure inside a transaction         |
| `QuotaExceeded`      | Browser storage estimate insufficient for the batch |
| `NotFound`           | Reserved (currently unused — `Option` is preferred) |
| `SerdeError`         | `serde-wasm-bindgen` round-trip failed              |

Errors from `JsValue` are formatted via the `js_err` helper, which
extracts `name`/`message` when the value looks like a `DOMException`,
and falls back to `{:?}` otherwise. This gives readable strings like
`"QuotaExceededError: ..."` instead of opaque `JsValue(...)` blobs.

## 8. Data flow

```
  ┌────────────────────┐    raw bytes    ┌────────────────────┐
  │ Acquisition        │────────────────►│ Worker: ingest     │
  │ (main thread)      │  postMessage    │ decode + extract   │
  └────────────────────┘  (transferable) └─────────┬──────────┘
                                                   │
                                       put_scan (atomic)
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

- **Per-chunk ingest** (`worker_ingest_chunk`) accumulates sweeps in a
  thread-local until an elevation completes, then reads any existing
  scan-index entry, runs `ScanIndexEntry::merge_chunk` in memory, and
  hands the merged entry plus the new sweep blobs to `put_scan` for
  one atomic write. The read-then-write spans two transactions and is
  racy in principle, but the per-worker `CHUNK_ACCUM` thread-local
  serializes per-scan so no concurrent writer exists in practice. The
  Start chunk also reads any pre-existing entry for the scan key and
  pre-populates `completed_elevations` so a resume doesn't reprocess
  sweeps that are already cached.

- **Archive ingest** (`worker_ingest`) decodes the entire volume, then
  writes the scan-index entry + all sweep blobs in a single `put_scan`
  call. Archive and real-time entries for the same physical volume can
  coexist in the cache when their `scan_start` keys differ; LRU
  eviction reclaims space over time.

## 9. What lives outside this module

- `ScanIndexEntry::merge_chunk` and `seed_from_partial`
  ([`src/data/keys.rs`](../src/data/keys.rs)) — domain logic for how
  one chunk's worth of state combines into the persisted scan entry.
  The IDB module knows nothing about VCP precedence, sweep appending,
  or end-timestamp handling.

- LRU policy decisions and browser-quota thresholds
  ([`src/data/facade.rs`](../src/data/facade.rs)) —
  `IndexedDbStore::evict_to_size` only knows "evict until this
  size"; whether to evict (and to what target) is `DataFacade`'s job.

- Cross-tab coordination — none. Multiple tabs of the same site share
  the database but do not coordinate writes; ingest is idempotent at
  the scan-key level so concurrent tabs at worst duplicate work.
