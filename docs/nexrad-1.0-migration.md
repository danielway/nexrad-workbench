# nexrad 1.0 migration hand-off

Delete this document and `nexrad-1.0-migration.patch` once the workbench is on nexrad 1.0.0. They describe a
one-time move and have no value afterwards.

This document is for whoever upgrades nexrad-workbench from the nexrad 1.0.0 release candidates to 1.0. It says what
the upgrade touches, which changes the compiler will not catch, how to validate the result, and what to report back
to the nexrad repository. It was written against nexrad `main` at commit `c677246bf06c03ceebd6aad10b40d52cc784310a` and workbench `main` at
commit `c90f270`.

The upgrade has two phases. First, validate the workbench against the nexrad `main` branch before the 1.0.0 crates
are published, while any problem found is still cheap to fix upstream. Second, once the six crates are on crates.io,
switch to the published versions.

## 1. Read first

- [`docs/migrating-to-1.0.md`](https://github.com/danielway/nexrad/blob/main/docs/migrating-to-1.0.md) in the nexrad
  repository. Read "Changes that apply to every crate", then the sections for `nexrad`, `nexrad-data`, `nexrad-decode`
  and `nexrad-render` (the four crates the workbench uses), then the appendix tables for the same four crates. Every
  rename and signature change below has a row there.
- The `CHANGELOG.md` of each of those four crates, `## [Unreleased]` section (it becomes `## [1.0.0]` at release).

## 2. Dependencies

The workbench currently pins:

```toml
nexrad = { version = "1.0.0-rc.4", default-features = false, features = ["wasm"] }
nexrad-data = "1.0.0-rc.7"
nexrad-decode = "1.0.0-rc.3"
nexrad-render = "1.0.0-rc.4"
```

### Phase 1: validate against nexrad `main`

Keep the version requirements as they are and add a patch section that points all six crates at the same nexrad
commit. All six must be patched, including `nexrad-model` and `nexrad-process`, which the workbench only reaches
transitively; otherwise Cargo mixes patched and registry crates and the model types stop matching.

```toml
[patch.crates-io]
nexrad = { git = "https://github.com/danielway/nexrad", rev = "c677246bf06c03ceebd6aad10b40d52cc784310a" }
nexrad-model = { git = "https://github.com/danielway/nexrad", rev = "c677246bf06c03ceebd6aad10b40d52cc784310a" }
nexrad-decode = { git = "https://github.com/danielway/nexrad", rev = "c677246bf06c03ceebd6aad10b40d52cc784310a" }
nexrad-data = { git = "https://github.com/danielway/nexrad", rev = "c677246bf06c03ceebd6aad10b40d52cc784310a" }
nexrad-render = { git = "https://github.com/danielway/nexrad", rev = "c677246bf06c03ceebd6aad10b40d52cc784310a" }
nexrad-process = { git = "https://github.com/danielway/nexrad", rev = "c677246bf06c03ceebd6aad10b40d52cc784310a" }
```

A local checkout works the same way with `path = "../nexrad/<crate>"` entries.

### Phase 2: switch to the published crates

Once all six crates are published, set the four requirements to `"1.0.0"` and delete the patch section.

### Optional, in either phase

- The facade's `wasm` feature turns on `process`, `serde` and `uom`, none of which the workbench uses.
  `default-features = false, features = ["aws", "render"]` builds less.
- The workbench depends on the facade and on three crates directly. Using `nexrad::data`, `nexrad::decode` and
  `nexrad::render` instead of the direct dependencies removes the possibility of version skew. The comments on the
  direct dependencies in `Cargo.toml` are stale either way.

## 3. Compile-time changes, by workbench file

The workbench was compiled against nexrad `main` for `wasm32-unknown-unknown`. The first pass reported 95 errors in
17 classes; after mechanical fixes, a second pass reported 16 in 4 new classes, a third 2, and a fourth none. Every
class is listed here with its replacement. Paths are the workbench's.

| File | Change | Guide section |
|---|---|---|
| `src/nexrad/acquisition/download.rs` | `nexrad_data::result::aws::AWSError` is `nexrad_data::AwsError`; `Error::AWS` is `Error::Aws`; `AwsError::S3GetObject(_)` is a struct variant, match `S3GetObject { .. }`. The five-variant transient match equals `aws.is_transient()`. | nexrad-data |
| same | `archive::list_files(site, date)` takes the date by value; `archive::download_file(&id)` takes a reference. | nexrad-data |
| same | `archive::Identifier::new(name)` is `Identifier::parse(&name)`, which returns `Result`. Names from `list_files` always parse, but do not `expect` in app code. | nexrad-data |
| `src/nexrad/decode/record_decode.rs`, `ingest_phases.rs`, `worker_api/ingest.rs` | `Product::moment_data(&radial)` and `Product::cfp_moment_data(&radial)` are one call, `radial.moment(product)`. `DataMoment` and the CFP moment type are gone; the unified `MomentData` keeps the raw fixed-point accessors (`scale`, `offset`, `data_word_size`, raw values) the GPU textures read. | nexrad-model |
| same | `Radial::collection_timestamp()` is `collection_timestamp_millis()`. Angle accessors end in `_degrees` (`azimuth_angle_degrees`, `elevation_angle_degrees`). | nexrad-model |
| same | `nexrad_render::Product` is no longer re-exported; use `nexrad::model::data::Product`. | nexrad-render |
| `src/nexrad/decode/ingest_phases.rs` | `MessageContents::VolumeCoveragePattern` is `RdaVolumeCoveragePattern`. `ElevationDataBlock::elevation_angle()` (`f64`) is `elevation_angle_degrees()` (`f32`); `azimuth_rate()` is `azimuth_rate_degrees_per_second()`. `Header::pattern_number()` is `vcp_number_raw()`. | nexrad-decode |
| same | `DataBlock` no longer derefs to its contents: `vol_block.volume_coverage_pattern_number()` is `vol_block.inner().vcp_number_raw()`. | nexrad-decode |
| `src/core/timing/*.rs`, `src/core/domain/forecast.rs` | `WaveformType::{CS, CDW, CDWO, B, SPP, Unknown}` are `{ContiguousSurveillance, ContiguousDopplerWithAmbiguityResolution, ContiguousDopplerWithoutAmbiguityResolution, Batch, StaggeredPulsePair, Unknown(u8)}`. `ChannelConfiguration::{SZ2Phase, UnknownPhase}` are `{Sz2Phase, Unknown(u8)}`. Matches need `Unknown(_)`. | nexrad-decode |
| `src/nexrad/live/streaming_state.rs`, `streaming/{acquire,backfill,poll}.rs` | `list_chunks_in_volume(site, volume, 100)` loses its third argument and returns the whole newest generation sorted by sequence. The old cap kept the lowest sequences, so `.last()` was wrong for generations over 100 chunks; it is right now. | nexrad-data |
| same | `download_chunk` returns `DownloadedChunk { identifier, chunk, attempts }` instead of a tuple. The struct is `#[non_exhaustive]`: build it with `DownloadedChunk::new(identifier, chunk, attempts)` and destructure with `..`. | nexrad-data |
| `streaming/persist.rs` | `VolumeIndex::new(n)` returns `Result` (`AwsError::InvalidVolumeIndex` outside `1..=999`). A persisted index that fails to parse should be treated as "no hint", not dropped silently. | nexrad-data |
| `src/nexrad/render/color_table.rs`, `gpu_renderer/textures.rs`, `src/ui/canvas_overlays/color_scale.rs` | `Product` is `#[non_exhaustive]`, so exhaustive matches need a `_` arm. Make that arm log or fall back visibly: a product added in a later 1.x release should not render silently with the wrong range. | nexrad-model |

Unchanged in signature, in case you look for them: `ChunkIdentifier::new`, `get_latest_volume` and its
`LatestVolumeResult { volume, calls }`, `volume::File::new`, `File::records`, `Record::decompress`, `Record::messages`
(but see item 5 in section 5), `Chunk::Start`, `ChunkType`.

## 4. The seed patch

`nexrad-1.0-migration.patch`, next to this file, is the set of mechanical edits that made the workbench compile for
`wasm32-unknown-unknown` against nexrad `main` (22 files, about 180 lines added and 160 removed; `Cargo.lock` and the
dependency section are left out). Apply it with `git apply` on a checkout of `c90f270` or rebase it by hand.

It is compile-only. No test was run, and no behaviour was reviewed. Before keeping it, review these hunks:

- `download.rs`: `Identifier::parse(file_name).expect(..)`. Return an error instead.
- `persist.rs`: `VolumeIndex::new(v).ok()?` drops an invalid persisted index silently.
- `chunk_timing_stats.rs`: `"Unknown" => WaveformType::Unknown(0)` and `"unknown_phase" => ChannelConfiguration::Unknown(0)`
  in the string round-trips lose the raw code. Decide whether the stats keys should carry it.
- `streaming_state.rs`: a `__d` temporary where a destructuring `let` reads better.
- Every new `_` arm on `Product`, `WaveformType` and `ChannelConfiguration`.

## 5. What the compiler will not catch

1. **Persisted waveform strings.** `ingest_phases.rs` stores `format!("{:?}", e.waveform_type())` in the
   `scan_index` IndexedDB entries, and `fallback_azimuth_rate` in `data/vcp.rs` matches those strings against
   `"CS" | "CDW" | ...`. After the upgrade new entries hold `"ContiguousSurveillance"` and the fallback misses, while
   old entries keep the short codes. Store an explicit mapping (the short codes `chunk_timing_stats.rs` already uses
   are a good choice) and bump the IndexedDB schema so the cache does not hold two vocabularies.
2. **Persisted timing stats.** `ChunkTimingStats` is written to localStorage under `PERSIST_SCHEMA_VERSION` (3).
   Its keys are built from the decode enums through the workbench's own `to_str`/`from_str` functions, so the
   serialized form only changes if those functions change. If you change them (for example to carry the `Unknown`
   code), bump the version so old payloads are dropped rather than misread.
3. **The negative-elevation workaround is dead.** `ingest_phases.rs` wraps angles above 180 degrees to negative.
   nexrad-decode decodes the sign correctly now (nexrad #146). The workaround is harmless; remove it.
4. **Error text.** Every wrapping error variant (`nexrad::Error::{Data, Decode, Model, Io, ..}`,
   `nexrad_data::Error::{Io, Decode, Model}`) now displays only its layer name, such as `decode error`; the detail
   is in `source()`. Wherever the workbench shows a nexrad error with `to_string()` or `{}`, format the source chain
   (the guide's cross-crate section has a snippet), or the UI loses the cause.
5. **Decoding is strict.** `Record::messages()`, `Record::radials()` and `File::scan()` return `Error::Decode` for
   the first message they cannot decode. The release candidates skipped such messages with a log warning and returned
   the rest. A realtime chunk with a damaged tail therefore now fails as a whole where the worker used to get partial
   radials. Decide whether to surface that or to fall back to `nexrad_decode::messages::iter_messages`, which yields a
   result per message. `File::records()` also no longer panics on truncated input; it returns
   `Error::TruncatedRecord`.
6. **Listing size.** Without the 100-key cap, `list_chunks_in_volume` pages through the whole generation, one request
   per thousand keys. Check nothing timed the call or assumed at most 100 results.
7. **Realtime semantics the workbench does not use.** `ChunkIterator`, `chunk_stream` and the facade's new
   `stream_scans` changed or appeared; the workbench has its own loop and is unaffected. Likewise
   `GateStatus::Filtered`, `download_latest(site)` and `download_at`.
8. **Timing sample pollution (pre-existing).** The realtime loop records the Start-to-sequence-2 hop as an
   inter-chunk sample in elevation 1's first-in-sweep bucket. nexrad-data skips that sample since #184; the
   workbench's own loop should skip it too (do not record when the previous chunk is the Start chunk).

## 6. Validation checklist

- [ ] Phase 1 patch section in place; `cargo check --target wasm32-unknown-unknown` and `cargo test` pass, including
      the `wasm-bindgen-test` suite the workbench runs.
- [ ] `trunk serve` build runs. Archive path: list a site and date, download a file, decode and render all six
      products. Realtime path: run the loop across at least one volume rollover; timing projections populate.
- [ ] Upgrade path: start with IndexedDB and localStorage populated by an rc build and confirm the app starts and
      either migrates or discards the old payloads (items 1 and 2 above).
- [ ] Error path: point the app at a site that does not exist and confirm the UI message still carries the cause
      (item 4).
- [ ] Phase 2: versions set to `1.0.0`, patch section removed, the same checks repeated.

## 7. Report back

Open an issue on `danielway/nexrad` with: anything in `docs/migrating-to-1.0.md` that was wrong or missing for the
workbench; any API the workbench needs that 1.0 does not offer (these become 1.1 candidates; known ones so far are
`ChunkIdentifier::next_chunk`, which `with_sequence` only partly replaces, and the realtime timing types, which the
workbench keeps its own copies of); and the final diff of the upgrade.
