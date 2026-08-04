//! Library facade for the binary crate.
//!
//! This crate is primarily a binary (`src/main.rs`). The library half
//! exists so that integration tests in `tests/` can link against and
//! exercise modules that today are bin-internal — most notably
//! `data::indexeddb`, where the orchestration layer (cross-store
//! atomicity, the create_scan/put_scan touch contract, eviction) is too
//! tied to real IndexedDB to test in pure Rust.
//!
//! The bin still uses its own `mod data;` declarations in `main.rs`, so
//! the data module is compiled twice (once for the bin, once for the
//! lib). Tolerable — the alternative is a wide refactor of `main.rs` to
//! consume the library facade, which buys little for now.

pub mod data;
