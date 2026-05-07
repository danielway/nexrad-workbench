//! Integration tests for `IndexedDbStore` against real IndexedDB.
//!
//! These run in headless Firefox via `wasm-bindgen-test`. They cover the
//! orchestration / consistency invariants that the pure-Rust unit tests in
//! `mod logic` cannot model — most importantly:
//!
//! - **Cross-store atomicity**: `create_scan`, `put_scan`, `delete_scan`,
//!   and `clear_all` each touch multiple object stores; the tests verify
//!   the right stores end up in the right state.
//! - **Touch contract**: `create_scan` must seed `scan_touches`, `put_scan`
//!   must NOT, `get_sweep` must bump it. This is the contract the create vs
//!   put split exists to enforce.
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
    CachedSweep, ExtractedVcp, ScanIndexEntry, ScanKey, SiteId, SweepDataKey, UnixMillis,
};
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

/// A minimal `ScanIndexEntry` with no VCP and the given byte size.
fn entry_for(scan: ScanKey, size_bytes: u64) -> ScanIndexEntry {
    ScanIndexEntry {
        scan,
        vcp: None,
        file_name: None,
        cached_sweeps: Vec::new(),
        total_size_bytes: size_bytes,
    }
}

fn sweep_blob(scan: &ScanKey, elev: u8, product: &str, bytes: usize) -> (String, Vec<u8>) {
    let key = SweepDataKey::new(scan.clone(), elev, product);
    (key.to_storage_key(), vec![0u8; bytes])
}

fn sweep_key(scan: &ScanKey, elev: u8, product: &str) -> SweepDataKey {
    SweepDataKey::new(scan.clone(), elev, product)
}

// ---------------------------------------------------------------------------
// create_scan / put_scan / delete_scan / clear_all
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
async fn create_scan_writes_blob_index_and_touch_atomically() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    let mut entry = entry_for(scan.clone(), 100);
    entry.file_name = Some("KDMX-2023.nexrad".to_string());
    let blob = sweep_blob(&scan, 1, "reflectivity", 100);

    let before = UnixMillis::now();
    store
        .create_scan(&entry, std::slice::from_ref(&blob))
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

    // Index entry round-trips.
    let read = store
        .scan_availability(&scan)
        .await
        .unwrap()
        .expect("scan-index entry should be present");
    assert_eq!(read.file_name, Some("KDMX-2023.nexrad".to_string()));
    assert_eq!(read.total_size_bytes, 100);

    // scan_touches was seeded with a timestamp around the create_scan window.
    // ±25 ms slop accommodates Date.now() rounding under Chromium's Spectre
    // mitigations — the JS clock isn't strictly monotonic at sub-millisecond
    // resolution from Rust's perspective.
    let touch = store
        .read_touch(&scan)
        .await
        .unwrap()
        .expect("create_scan should seed scan_touches");
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
async fn put_scan_does_not_seed_touch_for_orphan_entry() {
    // The "stranded data" path: put_scan called without a prior create_scan.
    // The contract is that scan_touches is left alone, which means the entry
    // sorts to position 0 in eviction (and gets reclaimed first). This test
    // verifies the touch is NOT created.
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    let entry = entry_for(scan.clone(), 50);

    store.put_scan(&entry, &[]).await.unwrap();

    // Index entry exists.
    assert!(store.scan_availability(&scan).await.unwrap().is_some());
    // No touch was written.
    assert_eq!(store.read_touch(&scan).await.unwrap(), None);
}

#[wasm_bindgen_test]
async fn put_scan_preserves_existing_touch() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    let entry = entry_for(scan.clone(), 100);

    // First write seeds the touch.
    store.create_scan(&entry, &[]).await.unwrap();
    let initial = store
        .read_touch(&scan)
        .await
        .unwrap()
        .expect("create_scan should have seeded scan_touches");

    // Sleep ~25 ms so any put-induced touch would be observably later.
    gloo_timers::future::TimeoutFuture::new(25).await;

    // Subsequent put_scan must NOT update scan_touches.
    let mut updated = entry.clone();
    updated.total_size_bytes = 500;
    store.put_scan(&updated, &[]).await.unwrap();

    let after = store
        .read_touch(&scan)
        .await
        .unwrap()
        .expect("touch still present");
    assert_eq!(initial, after, "put_scan must not modify scan_touches");

    // The entry itself was updated.
    let read_entry = store.scan_availability(&scan).await.unwrap().unwrap();
    assert_eq!(read_entry.total_size_bytes, 500);
}

#[wasm_bindgen_test]
async fn delete_scan_clears_blobs_index_and_touch() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    let entry = entry_for(scan.clone(), 200);
    let blobs = vec![
        sweep_blob(&scan, 1, "reflectivity", 100),
        sweep_blob(&scan, 1, "velocity", 100),
        sweep_blob(&scan, 3, "reflectivity", 0),
    ];

    store.create_scan(&entry, &blobs).await.unwrap();
    assert!(store
        .get_sweep(&sweep_key(&scan, 1, "reflectivity"))
        .await
        .unwrap()
        .is_some());
    assert!(store.scan_availability(&scan).await.unwrap().is_some());
    assert!(store.read_touch(&scan).await.unwrap().is_some());

    let bytes_freed = store.delete_scan(&scan).await.unwrap();
    assert_eq!(bytes_freed, 200);

    // All three stores cleared for this scan.
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
        .create_scan(
            &entry_for(keep.clone(), 50),
            &[sweep_blob(&keep, 1, "reflectivity", 50)],
        )
        .await
        .unwrap();
    store
        .create_scan(
            &entry_for(drop.clone(), 50),
            &[sweep_blob(&drop, 1, "reflectivity", 50)],
        )
        .await
        .unwrap();

    store.delete_scan(&drop).await.unwrap();

    // Surviving scan untouched.
    assert!(store.scan_availability(&keep).await.unwrap().is_some());
    assert!(store
        .get_sweep(&sweep_key(&keep, 1, "reflectivity"))
        .await
        .unwrap()
        .is_some());
    assert!(store.read_touch(&keep).await.unwrap().is_some());
    // Target scan gone.
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
            .create_scan(
                &entry_for(s.clone(), 10),
                &[sweep_blob(s, 1, "reflectivity", 10)],
            )
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
    // immediately after `create_scan` would be a no-op. To exercise the
    // bump path, we use *two* stores backed by the same database: the first
    // does the create (and seeds the touch), the second reads (and its
    // throttle map is empty, so the touch fires).
    let db_name = fresh_db_name();
    let writer = IndexedDbStore::with_database_name(db_name.clone());
    let reader = IndexedDbStore::with_database_name(db_name);
    let scan = scan_key("KDMX", 1700000000000);

    writer
        .create_scan(
            &entry_for(scan.clone(), 100),
            &[sweep_blob(&scan, 1, "reflectivity", 100)],
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
    let entry = ScanIndexEntry {
        scan: scan.clone(),
        vcp: Some(ExtractedVcp {
            number: 215,
            elevations: Vec::new(),
        }),
        file_name: Some("source.nexrad".to_string()),
        cached_sweeps: vec![CachedSweep {
            start: 1700000000.5,
            end: 1700000020.5,
            elevation: 0.5,
            elevation_number: 1,
            start_azimuth: 12.3,
            cached_products: vec!["reflectivity".to_string()],
        }],
        total_size_bytes: 100,
    };

    store.create_scan(&entry, &[]).await.unwrap();
    let read = store.scan_availability(&scan).await.unwrap().unwrap();
    assert_eq!(read.file_name, Some("source.nexrad".to_string()));
    assert_eq!(read.cached_sweeps.len(), 1);
    assert_eq!(read.cached_sweeps[0].elevation_number, 1);
    assert_eq!(read.cached_sweeps[0].cached_products, vec!["reflectivity"]);
    assert_eq!(read.vcp.as_ref().map(|v| v.number), Some(215));
}

#[wasm_bindgen_test]
async fn list_scans_filters_by_site_and_window() {
    let store = fresh_store();
    let kdmx_in = [1700000000000, 1700000060000, 1700000120000];
    let kdmx_out = [1699999000000, 1700001000000];
    let ktlx_in_window = 1700000030000;

    for ms in kdmx_in.iter().chain(kdmx_out.iter()) {
        store
            .create_scan(&entry_for(scan_key("KDMX", *ms), 10), &[])
            .await
            .unwrap();
    }
    store
        .create_scan(&entry_for(scan_key("KTLX", ktlx_in_window), 10), &[])
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
    for (i, size) in [(100u64, 100u64), (200, 200), (300, 300)]
        .iter()
        .enumerate()
    {
        let s = scan_key("KDMX", 1700000000000 + i as i64);
        store.create_scan(&entry_for(s, size.0), &[]).await.unwrap();
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

    // Sequence the creates so each scan_touches is monotonic and distinct.
    store
        .create_scan(&entry_for(oldest.clone(), 100), &[])
        .await
        .unwrap();
    gloo_timers::future::TimeoutFuture::new(15).await;
    store
        .create_scan(&entry_for(middle.clone(), 100), &[])
        .await
        .unwrap();
    gloo_timers::future::TimeoutFuture::new(15).await;
    store
        .create_scan(&entry_for(newest.clone(), 100), &[])
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
async fn evict_to_size_evicts_orphan_entries_first() {
    // A scan written via put_scan WITHOUT a prior create_scan has no
    // scan_touches entry — the cleanup path is to evict it first regardless
    // of its size or insertion order.
    let store = fresh_store();
    let touched = scan_key("KDMX", 1700000000000);
    let orphan = scan_key("KDMX", 1700000060000);

    // Create the touched entry first (gets a scan_touches timestamp).
    store
        .create_scan(&entry_for(touched.clone(), 100), &[])
        .await
        .unwrap();
    gloo_timers::future::TimeoutFuture::new(10).await;
    // Use put_scan for the orphan — never creates a touch, by contract.
    store
        .put_scan(&entry_for(orphan.clone(), 100), &[])
        .await
        .unwrap();

    // Evict to 150 → only one scan needs to go. Despite being newer in
    // insertion order, the orphan must go first.
    let evicted = store.evict_to_size(150).await.unwrap();
    assert_eq!(evicted, 1);
    assert!(store.scan_availability(&orphan).await.unwrap().is_none());
    assert!(store.scan_availability(&touched).await.unwrap().is_some());
}

#[wasm_bindgen_test]
async fn evict_to_size_no_op_when_already_under_target() {
    let store = fresh_store();
    let scan = scan_key("KDMX", 1700000000000);
    store
        .create_scan(&entry_for(scan.clone(), 100), &[])
        .await
        .unwrap();
    let evicted = store.evict_to_size(1_000_000).await.unwrap();
    assert_eq!(evicted, 0);
    assert!(store.scan_availability(&scan).await.unwrap().is_some());
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
