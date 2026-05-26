// Data Worker — ES module worker that offloads heavy data operations from the main thread.
//
// All expensive NEXRAD operations (bzip2 decompression, record decode, sweep extraction,
// IDB I/O) run here to keep the UI thread responsive. Communication uses postMessage
// with Transferable ArrayBuffers for zero-copy data transfer of float arrays.
//
// Protocol (all request/response pairs carry a numeric `id` for correlation):
//
//   Lifecycle:
//     Main → Worker:  { type: 'init', jsUrl, wasmUrl }
//     Worker → Main:  { type: 'ready' }
//
//   Archive ingest (full file → split, decode, store in IDB):
//     Main → Worker:  { type: 'ingest', id, data: ArrayBuffer, siteId, timestampSecs, fileName }
//     Worker → Main:  { type: 'ingested', id, result: { scanKey, recordsStored, elevationNumbers, sweeps, vcp, timing... } }
//
//   Chunk ingest (real-time streaming, one chunk at a time):
//     Main → Worker:  { type: 'ingest_chunk', id, data: ArrayBuffer, siteId, timestampSecs, chunkIndex, isStart, isEnd, fileName }
//     Worker → Main:  { type: 'chunk_ingested', id, result: { scanKey, sweepsStored, elevationsCompleted, sweeps, vcp, ... } }
//
//   Single-elevation render (read pre-computed sweep from IDB):
//     Main → Worker:  { type: 'render', id, scanKey, elevationNumber, product }
//     Worker → Main:  { type: 'decoded', id, azimuths: ArrayBuffer, gateValues: ArrayBuffer, azimuthCount, gateCount, scale, offset, ... }
//
//   Volume render (all elevations packed for 3D ray marching):
//     Main → Worker:  { type: 'render_volume', id, scanKey, product, elevationNumbers }
//     Worker → Main:  { type: 'volume_decoded', id, buffer: ArrayBuffer, sweepMeta, wordSize, ... }
//
//   Live render (partial sweep from in-memory accumulator, synchronous):
//     Main → Worker:  { type: 'render_live', id, elevationNumber, product }
//     Worker → Main:  { type: 'live_decoded', id, azimuths: ArrayBuffer, gateValues: ArrayBuffer, ... }
//
//   Errors:
//     Worker → Main:  { type: 'error', id, message }
//
// CONTRACT: every `type` string above MUST match the corresponding variant
// in `RequestType` / `ResponseType` (src/nexrad/decode_worker/types.rs).
// Those enums are the Rust-side single source of truth; the round-trip is
// pinned by `request_type_strings_are_snake_case` and
// `response_type_strings_roundtrip` unit tests. Adding a new message type
// requires changes in BOTH places.

let wasm = null;

// Produce a useful string for any thrown value. Naive `String(err)` on a
// plain object yields "[object Object]" — that's what produced the
// unhelpful "worker[object Object]" chip in the UI. Walk through the
// common cases (string, Error, anything with a usable .message, plain
// object) before giving up.
function describeError(err) {
    if (err == null) return 'Unknown error';
    if (typeof err === 'string') return err;
    if (typeof err.message === 'string' && err.message) return err.message;
    try {
        const json = JSON.stringify(err);
        if (json && json !== '{}' && json !== 'null') return json;
    } catch (_) {
        // Cyclic or non-serializable — fall through.
    }
    const s = String(err);
    if (s && s !== '[object Object]') return s;
    const ctor = err.constructor && err.constructor.name;
    return ctor ? `Non-Error object thrown (${ctor})` : 'Non-Error object thrown';
}

// Classify a caught exception into a structured { kind, message } pair so
// the Rust receive path can dispatch on the kind instead of regex-matching
// the message. Rust code that throws can opt into a specific kind by
// throwing an object with `kind` and `message` fields; otherwise we map
// known DOMException names to kinds. New kinds must be added to
// `WorkerErrorKind` in src/nexrad/decode_worker/types.rs and pinned by the
// `worker_error_kind_deserializes_known_strings` test.
function classifyError(err) {
    if (err && typeof err === 'object' && typeof err.kind === 'string') {
        return { kind: err.kind, message: describeError(err.message || err.kind) };
    }
    if (err == null) {
        return { kind: 'unknown', message: 'Unknown error' };
    }
    const name = (err && err.name) || '';
    const message = describeError(err);
    if (name === 'QuotaExceededError') {
        return { kind: 'quota_exceeded', message };
    }
    if (name === 'NotFoundError') {
        return { kind: 'not_found', message };
    }
    if (
        name === 'DataError' ||
        name === 'InvalidStateError' ||
        name === 'TransactionInactiveError' ||
        name === 'ConstraintError' ||
        name === 'AbortError'
    ) {
        return { kind: 'idb_failure', message };
    }
    return { kind: 'unknown', message };
}

function postError(id, err, prefix) {
    const cls = classifyError(err);
    const message = prefix ? prefix + cls.message : cls.message;
    self.postMessage({ type: 'error', id, kind: cls.kind, message });
}

self.onmessage = async function (e) {
    const msg = e.data;

    if (msg.type === 'init') {
        try {
            // Dynamically import the Trunk-generated wasm-bindgen JS module.
            // The main thread passes the hashed URLs it discovers from the DOM.
            const mod = await import(msg.jsUrl);
            await mod.default({ module_or_path: msg.wasmUrl });
            wasm = mod;
            self.postMessage({ type: 'ready' });
        } catch (err) {
            self.postMessage({
                type: 'error',
                id: 0,
                kind: 'init_failed',
                message: 'Worker init failed: ' + describeError(err),
            });
        }
        return;
    }

    if (!wasm) {
        self.postMessage({
            type: 'error',
            id: msg.id,
            kind: 'init_failed',
            message: 'Worker not initialized',
        });
        return;
    }

    if (msg.type === 'ingest') {
        try {
            // worker_ingest: JsValue -> Promise<JsValue>
            // Input: { data: ArrayBuffer, siteId, timestampSecs, fileName }
            // Output: { recordsStored, scanKey, elevationMap, totalMs }
            const result = await wasm.worker_ingest({
                data: msg.data,
                siteId: msg.siteId,
                timestampSecs: msg.timestampSecs,
                fileName: msg.fileName,
            });

            self.postMessage({ type: 'ingested', id: msg.id, result: result });
        } catch (err) {
            postError(msg.id, err);
        }
        return;
    }

    if (msg.type === 'ingest_chunk') {
        try {
            const result = await wasm.worker_ingest_chunk({
                data: msg.data,
                siteId: msg.siteId,
                timestampSecs: msg.timestampSecs,
                chunkIndex: msg.chunkIndex,
                isStart: msg.isStart,
                isEnd: msg.isEnd,
                fileName: msg.fileName,
                isLastInSweep: msg.isLastInSweep || false,
            });
            self.postMessage({ type: 'chunk_ingested', id: msg.id, result: result });
        } catch (err) {
            postError(msg.id, err);
        }
        return;
    }

    if (msg.type === 'render_volume') {
        try {
            const result = await wasm.worker_render_volume({
                scanKey: msg.scanKey,
                product: msg.product,
                elevationNumbers: msg.elevationNumbers,
            });

            const { buffer } = result;
            const transferList = [buffer];
            const payload = Object.assign({}, result, {
                type: 'volume_decoded',
                id: msg.id,
            });
            self.postMessage(payload, transferList);
        } catch (err) {
            postError(msg.id, err);
        }
        return;
    }

    if (msg.type === 'render') {
        try {
            // worker_render: JsValue -> Promise<JsValue>
            // Input: { scanKey, elevationNumber, product }
            // Output: { azimuths, gateValues, azimuthCount, gateCount, ... }
            const result = await wasm.worker_render({
                scanKey: msg.scanKey,
                elevationNumber: msg.elevationNumber,
                product: msg.product,
            });

            // Forward all result fields plus type/id; transfer float buffers zero-copy
            const { azimuths, gateValues } = result;
            const transferList = [azimuths, gateValues];
            const payload = Object.assign({}, result, {
                type: 'decoded',
                id: msg.id,
            });
            self.postMessage(payload, transferList);
        } catch (err) {
            postError(msg.id, err);
        }
        return;
    }

    if (msg.type === 'render_live') {
        try {
            // worker_render_live: JsValue -> JsValue (synchronous, reads from memory)
            const result = wasm.worker_render_live({
                product: msg.product,
                elevationNumber: msg.elevationNumber,
            });

            const { azimuths, gateValues } = result;
            const transferList = [azimuths, gateValues];
            const payload = Object.assign({}, result, {
                type: 'live_decoded',
                id: msg.id,
            });
            self.postMessage(payload, transferList);
        } catch (err) {
            postError(msg.id, err);
        }
        return;
    }
};
