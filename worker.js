// Data Worker — ES module worker that offloads heavy data operations from the main thread.
//
// Protocol:
//   Main → Worker:  { type: 'init',   jsUrl, wasmUrl }
//   Worker → Main:  { type: 'ready' }
//
//   Main → Worker:  { type: 'ingest',  id, data: ArrayBuffer, siteId, timestampSecs, fileName }
//   Worker → Main:  { type: 'ingested', id, result: { recordsStored, scanKey, elevationMap, totalMs } }
//
//   Main → Worker:  { type: 'render',  id, scanKey, elevationNumber, product, interpolation }
//   Worker → Main:  { type: 'rendered', id, imageData: ArrayBuffer, width, height, renderTimeMs, radialCount, fetchMs }
//
//   Worker → Main:  { type: 'error',   id, message }

let wasm = null;

self.onmessage = async function (e) {
    const msg = e.data;

    if (msg.type === 'init') {
        try {
            // Dynamically import the Trunk-generated wasm-bindgen JS module.
            // The main thread passes the hashed URLs it discovers from the DOM.
            const mod = await import(msg.jsUrl);
            await mod.default(msg.wasmUrl);
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

    if (msg.type === 'render') {
        try {
            // worker_render: JsValue -> Promise<JsValue>
            // Input: { scanKey, elevationNumber, product, interpolation }
            // Output: { imageData: ArrayBuffer, width, height, renderTimeMs, radialCount, fetchMs }
            const result = await wasm.worker_render({
                scanKey: msg.scanKey,
                elevationNumber: msg.elevationNumber,
                product: msg.product,
                interpolation: msg.interpolation,
            });

            // Transfer the pixel buffer (zero-copy)
            const imageData = result.imageData;
            self.postMessage(
                {
                    type: 'rendered',
                    id: msg.id,
                    imageData: imageData,
                    width: result.width,
                    height: result.height,
                    renderTimeMs: result.renderTimeMs,
                    radialCount: result.radialCount,
                    fetchMs: result.fetchMs,
                    // Sub-timings for diagnostics
                    idbOpenMs: result.idbOpenMs,
                    listMs: result.listMs,
                    blobFetchMs: result.blobFetchMs,
                    idbFetchMs: result.idbFetchMs,
                    decompressMs: result.decompressMs,
                    decodeMs: result.decodeMs,
                    buildMs: result.buildMs,
                    renderMs: result.renderMs,
                    matchingRecords: result.matchingRecords,
                    totalRecords: result.totalRecords,
                    blobBytes: result.blobBytes,
                },
                [imageData]
            );
        } catch (err) {
            self.postMessage({ type: 'error', id: msg.id, message: String(err) });
        }
        return;
    }
};
