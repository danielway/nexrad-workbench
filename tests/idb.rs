//! Integration tests for `IndexedDbStore` against real IndexedDB.
//!
//! These run in headless Firefox via `wasm-bindgen-test`. They cover the
//! orchestration / consistency invariants that the pure-Rust unit tests in
//! `mod logic` cannot model — most importantly:
//!
//! - **Cross-store atomicity**: `upsert_scan`, `delete_scan`, and
//!   `clear_all` each touch multiple object stores; the tests verify the
//!   right stores end up in the right state.
//! - **Touch contract**: the first `upsert_scan` for a scan key must seed
//!   `scan_touches`; subsequent merges must preserve it; `get_sweep` must
//!   bump it.
//! - **Phantom-entry prevention**: `ElevationUpload`s with empty blobs are
//!   dropped by the IDB layer so the manifest can't claim sweeps that
//!   weren't written.
//! - **Manifest/blob agreement across incremental flushes**: chunk ingest
//!   builds the index entry across multiple `upsert_scan` calls; each call
//!   must leave the manifest consistent with the blobs actually in storage.
//! - **Eviction**: ordering by `scan_touches`, missing-touch sorts first.
//!
//! Each test runs in its own IndexedDB database (via
//! `IndexedDbStore::with_database_name`) so siblings don't collide.
//!
//! Pre-commit does NOT run this suite — Firefox + geckodriver are heavy
//! dependencies. CI runs them in a dedicated job. Run locally with:
//!
//! ```sh
//! cargo test --test idb
//! ```
//!
//! (after installing Firefox + geckodriver and `cargo install wasm-bindgen-cli`).

#![cfg(target_arch = "wasm32")]

use nexrad_workbench::data::indexeddb::IndexedDbStore;
use nexrad_workbench::data::keys::{
    ElevationUpload, ProductBlob, ScanHeader, ScanKey, SiteId, SweepDataKey, SweepTiming,
    UnixMillis,
};
use nexrad_workbench::data::vcp_timing::ExtractedVcp;
use std::sync::atomic::{AtomicU64, Ordering};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Returns a fresh, never-before-used IndexedDB database name. Combines a
/// per-process counter with the high-resolution clock so reruns across
/// test invocations don't collide either.
fn fresh_db_name() -> String {
    use web_time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("nexrad-idb-test-{}-{}", ns, id)
}

/// Convenience wrapper around `fresh_db_name` for tests that don't need a
/// second handle.
fn fresh_store() -> IndexedDbStore {
    IndexedDbStore::with_database_name(fresh_db_name())
}

fn scan_key(site: &str, ms: i64) -> ScanKey {
    ScanKey::new(site, UnixMillis(ms))
}

fn header(scan: ScanKey) -> ScanHeader {
    ScanHeader {
        scan,
        vcp: None,
        file_name: None,
    }
}

fn header_named(scan: ScanKey, file_name: &str) -> ScanHeader {
    ScanHeader {
        scan,
        vcp: None,
        file_name: Some(file_name.to_string()),
    }
}

fn timing() -> SweepTiming {
    SweepTiming {
        start_secs: 1700000000.5,
        end_secs: 1700000020.5,
        elevation_angle: 0.5,
        start_azimuth: 12.3,
    }
}

fn upload(elev: u8, products: &[(&'static str, usize)]) -> ElevationUpload {
    ElevationUpload {
        elevation_number: elev,
        timing: timing(),
        blobs: products
            .iter()
            .map(|(name, bytes)| ProductBlob {
                product: name,
                bytes: vec![0u8; *bytes],
            })
            .collect(),
    }
}

fn sweep_key(scan: &ScanKey, elev: u8, product: &str) -> SweepDataKey {
    SweepDataKey::new(scan.clone(), elev, product)
}

// ---------------------------------------------------------------------------
// upsert_scan — first write
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn upsert_first_write_atomic() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);

    let before = UnixMillis::now();
    store
        .upsert_scan(
            &header_named(scan.clone(), "KDMX-2023.nexrad"),
            &[upload(1, &[("reflectivity", 100)])],
        )
        .await
        .unwrap();
    let after = UnixMillis::now();

    // Sweep blob round-trips.
    let buf = store
        .get_sweep(&sweep_key(&scan, 1, "reflectivity"))
        .await
        .unwrap()
        .expect("sweep blob should be present");
    assert_eq!(buf.byte_length(), 100);

    // Index entry round-trips with the derived manifest.
    let read = store
        .scan_availability(&scan)
        .await
        .unwrap()
        .expect("scan-index entry should be present");
    assert_eq!(read.file_name, Some("KDMX-2023.nexrad".to_string()));
    assert_eq!(read.total_size_bytes, 100);
    assert_eq!(read.cached_sweeps.len(), 1);
    assert_eq!(read.cached_sweeps[0].elevation_number, 1);
    assert_eq!(
        read.cached_sweeps[0].cached_products,
        vec!["reflectivity".to_string()]
    );

    // scan_touches was seeded with a timestamp around the upsert window.
    // ±25 ms slop accommodates Date.now() rounding under Chromium's Spectre
    // mitigations — the JS clock isn't strictly monotonic at sub-millisecond
    // resolution from Rust's perspective.
    let touch = store
        .read_touch(&scan)
        .await
        .unwrap()
        .expect("first upsert should seed scan_touches");
    let slop_ms = 25;
    assert!(
        touch.0 >= before.0 - slop_ms && touch.0 <= after.0 + slop_ms,
        "touch {:?} should be near [{:?}, {:?}] (±{}ms)",
        touch,
        before,
        after,
        slop_ms
    );
}

#[wasm_bindgen_test]
async fn upsert_first_write_with_no_uploads_still_seeds_touch() {
    // The empty-uploads case (e.g. an early chunk flush that hasn't produced
    // any complete sweeps yet) must still seed the touch so the entry has an
    // LRU placement. Otherwise the orphan-eviction logic would reclaim it
    // before any blobs land.
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    store.upsert_scan(&header(scan.clone()), &[]).await.unwrap();

    assert!(store.scan_availability(&scan).await.unwrap().is_some());
    assert!(
        store.read_touch(&scan).await.unwrap().is_some(),
        "even an empty-uploads first write must seed scan_touches"
    );
}

// ---------------------------------------------------------------------------
// upsert_scan — merge semantics
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn upsert_merge_preserves_existing_touch() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);

    store
        .upsert_scan(
            &header(scan.clone()),
            &[upload(1, &[("reflectivity", 100)])],
        )
        .await
        .unwrap();
    let initial = store
        .read_touch(&scan)
        .await
        .unwrap()
        .expect("first upsert should seed scan_touches");

    // Sleep ~25 ms so any inadvertent re-seed would shift the timestamp.
    gloo_timers::future::TimeoutFuture::new(25).await;

    store
        .upsert_scan(&header(scan.clone()), &[upload(2, &[("reflectivity", 50)])])
        .await
        .unwrap();

    let after = store
        .read_touch(&scan)
        .await
        .unwrap()
        .expect("touch still present");
    assert_eq!(initial, after, "merge upsert must not modify scan_touches");

    // Both sweeps are present.
    let entry = store.scan_availability(&scan).await.unwrap().unwrap();
    assert_eq!(entry.cached_sweeps.len(), 2);
    assert_eq!(entry.total_size_bytes, 150);
}

#[wasm_bindgen_test]
async fn upsert_merge_fills_header_only_if_currently_none() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);

    // First upsert sets neither vcp nor file_name.
    store.upsert_scan(&header(scan.clone()), &[]).await.unwrap();

    // Merge supplies both — they should land.
    let mut h2 = header(scan.clone());
    h2.vcp = Some(ExtractedVcp {
        number: 215,
        elevations: Vec::new(),
    });
    h2.file_name = Some("source.nexrad".to_string());
    store.upsert_scan(&h2, &[]).await.unwrap();

    let entry = store.scan_availability(&scan).await.unwrap().unwrap();
    assert_eq!(entry.vcp.as_ref().map(|v| v.number), Some(215));
    assert_eq!(entry.file_name, Some("source.nexrad".to_string()));

    // A later merge with different values must NOT overwrite — fields are
    // fill-in-if-None, not replace.
    let mut h3 = header(scan.clone());
    h3.vcp = Some(ExtractedVcp {
        number: 999,
        elevations: Vec::new(),
    });
    h3.file_name = Some("different.nexrad".to_string());
    store.upsert_scan(&h3, &[]).await.unwrap();

    let entry = store.scan_availability(&scan).await.unwrap().unwrap();
    assert_eq!(
        entry.vcp.as_ref().map(|v| v.number),
        Some(215),
        "vcp must not be overwritten on merge"
    );
    assert_eq!(
        entry.file_name,
        Some("source.nexrad".to_string()),
        "file_name must not be overwritten on merge"
    );
}

#[wasm_bindgen_test]
async fn upsert_incremental_keeps_manifest_and_blobs_in_agreement() {
    // The chunk-ingest pattern: each flush calls upsert_scan with the new
    // elevation's blobs. The persisted manifest must accumulate, and every
    // claimed (elev, product) must have a matching blob in STORE_SWEEPS.
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);

    let flushes = [
        upload(1, &[("reflectivity", 100), ("velocity", 80)]),
        upload(2, &[("reflectivity", 110)]),
        upload(3, &[("reflectivity", 90), ("velocity", 70)]),
    ];

    for flush in &flushes {
        store
            .upsert_scan(&header(scan.clone()), std::slice::from_ref(flush))
            .await
            .unwrap();
    }

    let entry = store.scan_availability(&scan).await.unwrap().unwrap();
    assert_eq!(entry.cached_sweeps.len(), 3);
    assert_eq!(entry.total_size_bytes, 100 + 80 + 110 + 90 + 70);

    // Every claim resolves to a real blob.
    for sweep in &entry.cached_sweeps {
        for product in &sweep.cached_products {
            let buf = store
                .get_sweep(&sweep_key(&scan, sweep.elevation_number, product))
                .await
                .unwrap();
            assert!(
                buf.is_some(),
                "manifest claims ({}, {}) but no blob found",
                sweep.elevation_number,
                product
            );
        }
    }
}

// ---------------------------------------------------------------------------
// upsert_scan — phantom-entry prevention
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn upsert_drops_elevation_with_no_blobs() {
    // ElevationUpload with empty `blobs` is the structural analogue of the
    // bug we hit: caller "saw radials" but extracted nothing usable. The
    // IDB layer must drop the upload silently — never write a phantom
    // CachedSweep that the resolver would later request and the worker
    // would fail on.
    let store = fresh_store();
    let scan = scan_key("KDMX", 1778602368000);
    let phantom = ElevationUpload {
        elevation_number: 1,
        timing: timing(),
        blobs: Vec::new(),
    };

    store
        .upsert_scan(&header(scan.clone()), &[phantom])
        .await
        .unwrap();

    let entry = store
        .scan_availability(&scan)
        .await
        .unwrap()
        .expect("header still writes even with no uploads");
    assert!(
        entry.cached_sweeps.is_empty(),
        "phantom elevation must not produce a CachedSweep"
    );
    assert_eq!(entry.total_size_bytes, 0);
}

#[wasm_bindgen_test]
async fn upsert_drops_phantom_alongside_real_uploads() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    let phantom = ElevationUpload {
        elevation_number: 7,
        timing: timing(),
        blobs: Vec::new(),
    };
    let real = upload(1, &[("reflectivity", 50)]);

    store
        .upsert_scan(&header(scan.clone()), &[phantom, real])
        .await
        .unwrap();

    let entry = store.scan_availability(&scan).await.unwrap().unwrap();
    assert_eq!(entry.cached_sweeps.len(), 1);
    assert_eq!(entry.cached_sweeps[0].elevation_number, 1);
    assert!(
        store
            .get_sweep(&sweep_key(&scan, 7, "reflectivity"))
            .await
            .unwrap()
            .is_none(),
        "phantom elevation must not have written any blob"
    );
}

// ---------------------------------------------------------------------------
// delete_scan / clear_all
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn delete_scan_clears_blobs_index_and_touch() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    let uploads = [
        upload(1, &[("reflectivity", 100), ("velocity", 100)]),
        upload(3, &[("reflectivity", 1)]),
    ];

    store
        .upsert_scan(&header(scan.clone()), &uploads)
        .await
        .unwrap();
    assert!(store
        .get_sweep(&sweep_key(&scan, 1, "reflectivity"))
        .await
        .unwrap()
        .is_some());
    assert!(store.scan_availability(&scan).await.unwrap().is_some());
    assert!(store.read_touch(&scan).await.unwrap().is_some());

    let bytes_freed = store.delete_scan(&scan).await.unwrap();
    assert_eq!(bytes_freed, 100 + 100 + 1);

    for (elev, product) in [(1, "reflectivity"), (1, "velocity"), (3, "reflectivity")] {
        assert!(
            store
                .get_sweep(&sweep_key(&scan, elev, product))
                .await
                .unwrap()
                .is_none(),
            "blob ({}, {}) should be deleted",
            elev,
            product
        );
    }
    assert!(store.scan_availability(&scan).await.unwrap().is_none());
    assert_eq!(store.read_touch(&scan).await.unwrap(), None);
}

#[wasm_bindgen_test]
async fn delete_scan_does_not_affect_other_scans() {
    let store = fresh_store();
    let keep = scan_key("KDMX", 1700000000000);
    let drop = scan_key("KDMX", 1700000060000);

    store
        .upsert_scan(&header(keep.clone()), &[upload(1, &[("reflectivity", 50)])])
        .await
        .unwrap();
    store
        .upsert_scan(&header(drop.clone()), &[upload(1, &[("reflectivity", 50)])])
        .await
        .unwrap();

    store.delete_scan(&drop).await.unwrap();

    assert!(store.scan_availability(&keep).await.unwrap().is_some());
    assert!(store
        .get_sweep(&sweep_key(&keep, 1, "reflectivity"))
        .await
        .unwrap()
        .is_some());
    assert!(store.read_touch(&keep).await.unwrap().is_some());
    assert!(store.scan_availability(&drop).await.unwrap().is_none());
}

#[wasm_bindgen_test]
async fn clear_all_empties_every_store() {
    let store = fresh_store();
    let scans = [
        scan_key("KDMX", 1700000000000),
        scan_key("KDMX", 1700000060000),
        scan_key("KTLX", 1700000000000),
    ];
    for s in &scans {
        store
            .upsert_scan(&header(s.clone()), &[upload(1, &[("reflectivity", 10)])])
            .await
            .unwrap();
    }

    store.clear_all().await.unwrap();

    for s in &scans {
        assert!(store.scan_availability(s).await.unwrap().is_none());
        assert!(store
            .get_sweep(&sweep_key(s, 1, "reflectivity"))
            .await
            .unwrap()
            .is_none());
        assert_eq!(store.read_touch(s).await.unwrap(), None);
    }
    assert_eq!(store.total_cache_size().await.unwrap(), 0);
}

// ---------------------------------------------------------------------------
// get_sweep + touch contract
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn get_sweep_returns_none_for_missing_key() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    assert!(store
        .get_sweep(&sweep_key(&scan, 1, "reflectivity"))
        .await
        .unwrap()
        .is_none());
}

#[wasm_bindgen_test]
async fn get_sweep_bumps_touch() {
    // Production's `touch_scan` has a 60 s in-memory throttle, so a `get_sweep`
    // immediately after `upsert_scan` would be a no-op. To exercise the
    // bump path, we use *two* stores backed by the same database: the first
    // does the upsert (and seeds the touch), the second reads (and its
    // throttle map is empty, so the touch fires).
    let db_name = fresh_db_name();
    let writer = IndexedDbStore::with_database_name(db_name.clone());
    let reader = IndexedDbStore::with_database_name(db_name);
    let scan = scan_key("KDMX", 1700000000000);

    writer
        .upsert_scan(
            &header(scan.clone()),
            &[upload(1, &[("reflectivity", 100)])],
        )
        .await
        .unwrap();
    let initial = writer.read_touch(&scan).await.unwrap().unwrap();

    // Pause so the touch's `now()` is observably later than `initial`.
    gloo_timers::future::TimeoutFuture::new(25).await;

    let _buf = reader
        .get_sweep(&sweep_key(&scan, 1, "reflectivity"))
        .await
        .unwrap()
        .expect("blob present");

    // The touch is fired fire-and-forget via spawn_local; poll briefly.
    let bumped = wait_for_touch_bump(&reader, &scan, initial).await;
    assert!(bumped, "get_sweep should fire a fresh touch after read");
}

// ---------------------------------------------------------------------------
// scan_availability / list_scans / total_cache_size
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn scan_availability_round_trips_full_entry() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    let mut h = header_named(scan.clone(), "source.nexrad");
    h.vcp = Some(ExtractedVcp {
        number: 215,
        elevations: Vec::new(),
    });

    store
        .upsert_scan(&h, &[upload(1, &[("reflectivity", 100)])])
        .await
        .unwrap();

    let read = store.scan_availability(&scan).await.unwrap().unwrap();
    assert_eq!(read.file_name, Some("source.nexrad".to_string()));
    assert_eq!(read.cached_sweeps.len(), 1);
    assert_eq!(read.cached_sweeps[0].elevation_number, 1);
    assert_eq!(read.cached_sweeps[0].cached_products, vec!["reflectivity"]);
    assert_eq!(read.vcp.as_ref().map(|v| v.number), Some(215));
    assert_eq!(read.total_size_bytes, 100);
}

#[wasm_bindgen_test]
async fn list_scans_filters_by_site_and_window() {
    let store = fresh_store();
    let kdmx_in = [1700000000000, 1700000060000, 1700000120000];
    let kdmx_out = [1699999000000, 1700001000000];
    let ktlx_in_window = 1700000030000;

    for ms in kdmx_in.iter().chain(kdmx_out.iter()) {
        store
            .upsert_scan(&header(scan_key("KDMX", *ms)), &[])
            .await
            .unwrap();
    }
    store
        .upsert_scan(&header(scan_key("KTLX", ktlx_in_window)), &[])
        .await
        .unwrap();

    let listed = store
        .list_scans(
            &SiteId::new("KDMX"),
            UnixMillis(1700000000000),
            UnixMillis(1700000120000),
        )
        .await
        .unwrap();

    let starts: Vec<i64> = listed.iter().map(|e| e.scan.scan_start.0).collect();
    assert_eq!(starts, kdmx_in.to_vec(), "wrong site/window/order");
}

#[wasm_bindgen_test]
async fn total_cache_size_sums_all_entries() {
    let store = fresh_store();
    let sizes = [100, 200, 300];
    for (i, size) in sizes.iter().enumerate() {
        let s = scan_key("KDMX", 1700000000000 + i as i64);
        store
            .upsert_scan(&header(s), &[upload(1, &[("reflectivity", *size)])])
            .await
            .unwrap();
    }
    assert_eq!(store.total_cache_size().await.unwrap(), 600);
}

// ---------------------------------------------------------------------------
// evict_to_size
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn evict_to_size_removes_oldest_touched_first() {
    let store = fresh_store();
    let oldest = scan_key("KDMX", 1700000000000);
    let middle = scan_key("KDMX", 1700000060000);
    let newest = scan_key("KDMX", 1700000120000);

    // Sequence the upserts so each scan_touches is monotonic and distinct.
    store
        .upsert_scan(
            &header(oldest.clone()),
            &[upload(1, &[("reflectivity", 100)])],
        )
        .await
        .unwrap();
    gloo_timers::future::TimeoutFuture::new(15).await;
    store
        .upsert_scan(
            &header(middle.clone()),
            &[upload(1, &[("reflectivity", 100)])],
        )
        .await
        .unwrap();
    gloo_timers::future::TimeoutFuture::new(15).await;
    store
        .upsert_scan(
            &header(newest.clone()),
            &[upload(1, &[("reflectivity", 100)])],
        )
        .await
        .unwrap();

    // Total = 300; evict to 150 → drop the two oldest (oldest + middle).
    let evicted = store.evict_to_size(150).await.unwrap();
    assert_eq!(evicted, 2);
    assert!(store.scan_availability(&oldest).await.unwrap().is_none());
    assert!(store.scan_availability(&middle).await.unwrap().is_none());
    assert!(store.scan_availability(&newest).await.unwrap().is_some());
}

#[wasm_bindgen_test]
async fn evict_to_size_no_op_when_already_under_target() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    store
        .upsert_scan(
            &header(scan.clone()),
            &[upload(1, &[("reflectivity", 100)])],
        )
        .await
        .unwrap();
    let evicted = store.evict_to_size(1_000_000).await.unwrap();
    assert_eq!(evicted, 0);
    assert!(store.scan_availability(&scan).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// MainThreadStore::check_and_evict (decision → execution orchestration)
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn check_and_evict_enforces_app_quota_over_real_db() {
    use nexrad_workbench::data::facade::MainThreadStore;

    let store = fresh_store();
    let facade = MainThreadStore::with_store(store.clone());
    let oldest = scan_key("KDMX", 1700000000000);
    let newest = scan_key("KDMX", 1700000060000);
    store
        .upsert_scan(
            &header(oldest.clone()),
            &[upload(1, &[("reflectivity", 100)])],
        )
        .await
        .unwrap();
    gloo_timers::future::TimeoutFuture::new(15).await;
    store
        .upsert_scan(
            &header(newest.clone()),
            &[upload(1, &[("reflectivity", 100)])],
        )
        .await
        .unwrap();

    // Under quota → no eviction, everything stays.
    let (did_evict, count, _warning) = facade.check_and_evict(1_000_000, 150).await.unwrap();
    assert!(!did_evict);
    assert_eq!(count, 0);
    assert!(store.scan_availability(&oldest).await.unwrap().is_some());

    // Over quota (200 > 150) → evict oldest down to the 150 target.
    let (did_evict, count, _warning) = facade.check_and_evict(150, 150).await.unwrap();
    assert!(did_evict);
    assert_eq!(count, 1);
    assert!(store.scan_availability(&oldest).await.unwrap().is_none());
    assert!(store.scan_availability(&newest).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Polls `read_touch` for up to ~250 ms looking for a value strictly
/// later than `baseline`. `touch_scan` is fire-and-forget via
/// `spawn_local`, so the IDB write isn't visible synchronously.
async fn wait_for_touch_bump(store: &IndexedDbStore, scan: &ScanKey, baseline: UnixMillis) -> bool {
    for _ in 0..25 {
        gloo_timers::future::TimeoutFuture::new(10).await;
        if let Ok(Some(t)) = store.read_touch(scan).await {
            if t.0 > baseline.0 {
                return true;
            }
        }
    }
    false
}
