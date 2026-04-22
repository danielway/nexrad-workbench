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

let wasm = null;

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
            self.postMessage({ type: 'error', id: 0, message: 'Worker init failed: ' + String(err) });
        }
        return;
    }

    if (!wasm) {
        self.postMessage({ type: 'error', id: msg.id, message: 'Worker not initialized' });
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
            self.postMessage({ type: 'error', id: msg.id, message: String(err) });
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
                skipOverlapDelete: msg.skipOverlapDelete || false,
                isLastInSweep: msg.isLastInSweep || false,
            });
            self.postMessage({ type: 'chunk_ingested', id: msg.id, result: result });
        } catch (err) {
            self.postMessage({ type: 'error', id: msg.id, message: String(err) });
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
            self.postMessage({ type: 'error', id: msg.id, message: String(err) });
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
            self.postMessage({ type: 'error', id: msg.id, message: String(err) });
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
            self.postMessage({ type: 'error', id: msg.id, message: String(err) });
        }
        return;
    }
};
