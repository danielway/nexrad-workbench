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

## 2. Why two stores

The split is driven by access patterns. Sweep blobs are large
(megabytes) and read by exact key on the render hot path; they need
zero-copy access from JS to GPU. Scan-index entries are tiny, queried
in bulk for timeline and eviction work, and benefit from structured
clone (so they round-trip Rust types without a JSON detour).

Co-locating them in a single store would force every timeline query to
either drag blob data through memory, or maintain a parallel index
anyway.

## 3. Concurrency model

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

## 4. Key-range queries

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

## 5. API surface

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

## 6. Errors

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

## 7. Data flow

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

## 8. What lives outside this module

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
