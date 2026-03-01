// Decode Worker — ES module worker that offloads nexrad::load() from the main thread.
//
// Protocol:
//   Main → Worker:  { type: 'init',   jsUrl, wasmUrl }
//   Worker → Main:  { type: 'ready' }
//   Main → Worker:  { type: 'decode', id, data: ArrayBuffer }
//   Worker → Main:  { type: 'decoded', id, data: ArrayBuffer, decodeMs }
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

    if (msg.type === 'decode') {
        if (!wasm) {
            self.postMessage({ type: 'error', id: msg.id, message: 'Worker not initialized' });
            return;
        }

        try {
            const start = performance.now();
            // worker_decode: &[u8] -> Result<Vec<u8>, JsValue>
            // Input: raw compressed NEXRAD archive bytes
            // Output: bincode-serialized Scan
            const inputBytes = new Uint8Array(msg.data);
            const resultBytes = wasm.worker_decode(inputBytes);
            const decodeMs = performance.now() - start;

            // Transfer the result buffer (zero-copy ownership move)
            const buffer = resultBytes.buffer;
            self.postMessage(
                { type: 'decoded', id: msg.id, data: buffer, decodeMs: decodeMs },
                [buffer]
            );
        } catch (err) {
            self.postMessage({ type: 'error', id: msg.id, message: String(err) });
        }
    }
};
