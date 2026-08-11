//! National radar mosaic overlay.
//!
//! Fetches a CONUS base-reflectivity composite PNG (NOAA NCEP MRMS via
//! GeoServer WMS) and makes it available as a GPU texture for painting
//! under per-site radar data. Polls only while the layer is enabled;
//! dropping the layer releases the texture and stops polling. Each refresh
//! routes through `crate::net::retry::with_retry`, so transient failures get
//! a short burst of exponential-backoff retries instead of waiting for the
//! full refresh interval to roll around.

use crate::net::retry::{with_retry, Verdict, DEFAULT_POLICY};
use eframe::egui;
use futures_channel::oneshot;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// NOAA NCEP GeoServer WMS endpoint serving the MRMS quality-controlled
/// CONUS base reflectivity. Returns a single PNG rendered to whatever bbox
/// and pixel size we request, with `Access-Control-Allow-Origin: *`.
/// Updated by the upstream MRMS feed every ~2 min.
const MOSAIC_URL: &str = concat!(
    "https://opengeo.ncep.noaa.gov/geoserver/conus/conus_bref_qcd/ows",
    "?service=WMS&version=1.1.1&request=GetMap",
    "&layers=conus_bref_qcd",
    "&bbox=-126,24,-66,50",
    "&width=1200&height=520",
    "&srs=EPSG:4326",
    "&format=image/png&transparent=true&styles=",
);

/// Bounds of the composite in degrees: (min_lon, min_lat, max_lon, max_lat).
/// Must match the bbox in [`MOSAIC_URL`] above so the image registers
/// correctly under the map projection.
const MOSAIC_BOUNDS: (f64, f64, f64, f64) = (-126.0, 24.0, -66.0, 50.0);

/// How often to refetch while enabled (seconds). Matches source cadence.
/// Applied for both successful and failed attempts — transient failures are
/// already absorbed by `with_retry`'s in-burst backoff, so a persistent
/// failure just means the next 120 s tick will retry.
const REFRESH_INTERVAL_SECS: f64 = 120.0;

enum FetchOutcome {
    Loaded {
        image: egui::ColorImage,
        fetched_at: f64,
    },
    Failed {
        attempted_at: f64,
    },
}

/// Holds the current mosaic texture and drives background refreshes.
pub(crate) struct NationalMosaic {
    texture: Option<egui::TextureHandle>,
    /// Timestamp (seconds) of the last attempt — successful or not. Used to
    /// gate the next attempt against `REFRESH_INTERVAL_SECS`.
    last_attempt_ts: Option<f64>,
    /// Wall-clock seconds when the currently held texture was successfully
    /// fetched. Cleared with the texture; never set on failed attempts.
    image_time: Option<f64>,
    in_flight: Rc<RefCell<bool>>,
    sender: Sender<FetchOutcome>,
    receiver: Receiver<FetchOutcome>,
}

impl Default for NationalMosaic {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            texture: None,
            last_attempt_ts: None,
            image_time: None,
            in_flight: Rc::new(RefCell::new(false)),
            sender,
            receiver,
        }
    }
}

impl NationalMosaic {
    /// Per-frame tick. When `enabled`, kicks off a fetch if none is in
    /// flight and the texture is stale; when disabled, drops the texture
    /// so no GPU memory is held while the layer is off.
    pub(crate) fn poll_tick(&mut self, ctx: &egui::Context, enabled: bool) {
        if !enabled {
            if self.texture.is_some() || self.last_attempt_ts.is_some() || self.image_time.is_some()
            {
                self.texture = None;
                clear_on_disable(&mut self.last_attempt_ts, &mut self.image_time);
            }
            while self.receiver.try_recv().is_ok() {}
            return;
        }

        while let Ok(outcome) = self.receiver.try_recv() {
            match outcome {
                FetchOutcome::Loaded { image, fetched_at } => {
                    let handle =
                        ctx.load_texture("national_mosaic", image, egui::TextureOptions::LINEAR);
                    self.texture = Some(handle);
                    record_loaded(&mut self.last_attempt_ts, &mut self.image_time, fetched_at);
                }
                FetchOutcome::Failed { attempted_at } => {
                    record_failed(&mut self.last_attempt_ts, attempted_at);
                }
            }
        }

        if *self.in_flight.borrow() {
            return;
        }

        let now = js_sys::Date::now() / 1000.0;
        let due = match self.last_attempt_ts {
            None => true,
            Some(ts) => now - ts >= REFRESH_INTERVAL_SECS,
        };
        if !due {
            return;
        }

        *self.in_flight.borrow_mut() = true;
        let sender = self.sender.clone();
        let in_flight = self.in_flight.clone();
        let ctx_clone = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = match with_retry(&DEFAULT_POLICY, "national_mosaic", |_attempt| async {
                match fetch_and_decode(MOSAIC_URL).await {
                    Ok(image) => Verdict::Ok(image),
                    Err(e) => {
                        // The HtmlImageElement load API doesn't surface HTTP
                        // status codes, so we can't distinguish 4xx from 5xx
                        // from network failure. Treat all errors as transient
                        // and let the policy's attempt budget bound it.
                        log::debug!("National mosaic fetch attempt failed: {}", e);
                        Verdict::Retry { after: None }
                    }
                }
            })
            .await
            {
                Ok(image) => FetchOutcome::Loaded {
                    image,
                    fetched_at: js_sys::Date::now() / 1000.0,
                },
                Err(msg) => {
                    log::warn!("National mosaic fetch failed: {}", msg);
                    FetchOutcome::Failed {
                        attempted_at: js_sys::Date::now() / 1000.0,
                    }
                }
            };
            let _ = sender.send(outcome);
            ctx_clone.request_repaint();
            *in_flight.borrow_mut() = false;
        });
    }

    /// Current texture, if loaded.
    pub(crate) fn texture(&self) -> Option<&egui::TextureHandle> {
        self.texture.as_ref()
    }

    /// Wall-clock seconds when the currently displayed mosaic image was
    /// successfully fetched. `None` when no texture is held.
    pub(crate) fn image_time(&self) -> Option<f64> {
        self.image_time
    }

    /// Mosaic geographic bounds as (min_lon, min_lat, max_lon, max_lat).
    pub(crate) fn bounds(&self) -> (f64, f64, f64, f64) {
        MOSAIC_BOUNDS
    }
}

/// Pure state transition for a successful mosaic load: stamp both the
/// attempt clock and the image time. Kept free of I/O so unit tests can
/// pin the contract without a GPU texture.
fn record_loaded(last_attempt_ts: &mut Option<f64>, image_time: &mut Option<f64>, fetched_at: f64) {
    *last_attempt_ts = Some(fetched_at);
    *image_time = Some(fetched_at);
}

/// Pure state transition for a failed mosaic attempt: advance the attempt
/// clock only — leave any previously loaded image time alone.
fn record_failed(last_attempt_ts: &mut Option<f64>, attempted_at: f64) {
    *last_attempt_ts = Some(attempted_at);
}

/// Pure disable path: drop texture-associated timestamps.
fn clear_on_disable(last_attempt_ts: &mut Option<f64>, image_time: &mut Option<f64>) {
    *last_attempt_ts = None;
    *image_time = None;
}

/// Fetch a PNG via browser-native image decoding and convert to an
/// `egui::ColorImage`. Runs on the main thread via an offscreen 2D canvas;
/// avoids pulling the `image` crate into the WASM bundle.
async fn fetch_and_decode(url: &str) -> Result<egui::ColorImage, String> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;

    let img = web_sys::HtmlImageElement::new().map_err(|_| "create HtmlImageElement failed")?;
    // Required so the offscreen canvas isn't tainted and getImageData works.
    img.set_cross_origin(Some("anonymous"));

    let (tx, rx) = oneshot::channel::<Result<(), String>>();
    let tx = Rc::new(RefCell::new(Some(tx)));

    let tx_load = tx.clone();
    let onload = Closure::<dyn FnMut()>::new(move || {
        if let Some(tx) = tx_load.borrow_mut().take() {
            let _ = tx.send(Ok(()));
        }
    });
    let tx_err = tx;
    let onerror = Closure::<dyn FnMut()>::new(move || {
        if let Some(tx) = tx_err.borrow_mut().take() {
            let _ = tx.send(Err("image load error".into()));
        }
    });

    img.set_onload(Some(onload.as_ref().unchecked_ref()));
    img.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    // The closures fire at most once. Forget them; the element goes out of
    // scope after this function and the browser GCs the listeners.
    onload.forget();
    onerror.forget();

    img.set_src(url);

    rx.await
        .map_err(|_| "onload channel canceled".to_string())??;

    let w = img.natural_width();
    let h = img.natural_height();
    if w == 0 || h == 0 {
        return Err("image has zero dimensions".into());
    }

    let canvas_el = document
        .create_element("canvas")
        .map_err(|_| "create canvas failed")?;
    let canvas: web_sys::HtmlCanvasElement = canvas_el
        .dyn_into()
        .map_err(|_| "element was not a canvas")?;
    canvas.set_width(w);
    canvas.set_height(h);

    let ctx = canvas
        .get_context("2d")
        .map_err(|_| "get 2d context failed")?
        .ok_or("no 2d context")?;
    let ctx: web_sys::CanvasRenderingContext2d = ctx.dyn_into().map_err(|_| "not a 2d context")?;

    ctx.draw_image_with_html_image_element(&img, 0.0, 0.0)
        .map_err(|_| "drawImage failed")?;

    let image_data = ctx
        .get_image_data(0.0, 0.0, w as f64, h as f64)
        .map_err(|_| "getImageData failed (canvas tainted?)")?;
    let bytes: Vec<u8> = image_data.data().to_vec();

    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        &bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn loaded_sets_image_time() {
        let mut last = None;
        let mut image = None;
        record_loaded(&mut last, &mut image, 1_700_000_000.0);
        assert_eq!(last, Some(1_700_000_000.0));
        assert_eq!(image, Some(1_700_000_000.0));
    }

    #[wasm_bindgen_test]
    fn failed_does_not_touch_image_time() {
        let mut last = Some(100.0);
        let image = Some(100.0);
        record_failed(&mut last, 200.0);
        assert_eq!(last, Some(200.0));
        assert_eq!(image, Some(100.0));
    }

    #[wasm_bindgen_test]
    fn disable_clears_image_time() {
        let mut last = Some(100.0);
        let mut image = Some(100.0);
        clear_on_disable(&mut last, &mut image);
        assert_eq!(last, None);
        assert_eq!(image, None);
    }

    #[wasm_bindgen_test]
    fn failed_then_loaded_updates_image_time() {
        let mut last = None;
        let mut image = None;
        record_failed(&mut last, 50.0);
        assert_eq!(image, None);
        record_loaded(&mut last, &mut image, 75.0);
        assert_eq!(image, Some(75.0));
        record_failed(&mut last, 90.0);
        assert_eq!(image, Some(75.0));
    }
}
