//! Decode pipeline (Fat Worker): turning archive/chunk bytes into sweeps.
//! Covers per-record decode and sweep extraction ([`record_decode`]), the
//! pure ingest phase helpers ([`ingest_phases`]), the `#[wasm_bindgen]`
//! entry points `worker.js` dispatches to inside the Web Worker
//! ([`worker_api`]), and the main-thread worker pool that sends requests
//! and receives decoded results ([`decode_worker`]).

pub(crate) mod decode_worker;
pub(crate) mod ingest_phases;
pub(crate) mod record_decode;
pub(crate) mod worker_api;
