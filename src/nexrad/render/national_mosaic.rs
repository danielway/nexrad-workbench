//! National radar mosaic overlay.
//!
//! Fetches a CONUS base-reflectivity composite PNG (NOAA NCEP MRMS via
//! GeoServer WMS) and makes it available as a GPU texture for painting
//! under per-site radar data. Polls only while the layer is enabled;
//! dropping the layer releases the texture and stops polling. Each refresh
//! routes through `crate::net::retry::with_retry`, so transient failures get
//! a short burst of exponential-backoff retries instead of waiting for the
//! full refresh interval to roll around.
//!
//! Product valid time comes from the layer's WMS `time` dimension
//! (`GetCapabilities` `Extent/@default`), not the client wall clock. The
//! subsequent `GetMap` request pins `TIME=` to that same ISO8601 instant so
//! the texture and canvas stamp describe one product.

use crate::net::err_text;
use crate::net::retry::{with_retry, Verdict, DEFAULT_POLICY};
use eframe::egui;
use futures_channel::oneshot;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

/// Base OWS endpoint for the CONUS quality-controlled base reflectivity layer.
const MOSAIC_OWS: &str = "https://opengeo.ncep.noaa.gov/geoserver/conus/conus_bref_qcd/ows";

/// WMS GetCapabilities URL — advertises the layer's current `time` default.
const CAPABILITIES_URL: &str = concat!(
    "https://opengeo.ncep.noaa.gov/geoserver/conus/conus_bref_qcd/ows",
    "?service=WMS&version=1.1.1&request=GetCapabilities",
);

/// Bounds of the composite in degrees: (min_lon, min_lat, max_lon, max_lat).
/// Must match the bbox in [`mosaic_getmap_url`] so the image registers
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
        /// Wall-clock seconds when this attempt finished (refresh gating).
        fetched_at: f64,
        /// MRMS product valid time (unix seconds) from WMS `time` default.
        product_time: f64,
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
    /// MRMS product valid time (unix seconds) for the currently held texture.
    /// Cleared with the texture; never set on failed attempts.
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
                FetchOutcome::Loaded {
                    image,
                    fetched_at,
                    product_time,
                } => {
                    let handle =
                        ctx.load_texture("national_mosaic", image, egui::TextureOptions::LINEAR);
                    self.texture = Some(handle);
                    record_loaded(
                        &mut self.last_attempt_ts,
                        &mut self.image_time,
                        fetched_at,
                        product_time,
                    );
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
                match fetch_mosaic_product().await {
                    Ok(loaded) => Verdict::Ok(loaded),
                    Err(e) => {
                        log::debug!("National mosaic fetch attempt failed: {}", e);
                        Verdict::Retry { after: None }
                    }
                }
            })
            .await
            {
                Ok((image, product_time)) => FetchOutcome::Loaded {
                    image,
                    fetched_at: js_sys::Date::now() / 1000.0,
                    product_time,
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

    /// MRMS product valid time (unix seconds) for the currently displayed
    /// mosaic. `None` when no texture is held.
    pub(crate) fn image_time(&self) -> Option<f64> {
        self.image_time
    }

    /// Mosaic geographic bounds as (min_lon, min_lat, max_lon, max_lat).
    pub(crate) fn bounds(&self) -> (f64, f64, f64, f64) {
        MOSAIC_BOUNDS
    }
}

/// Pure state transition for a successful mosaic load.
fn record_loaded(
    last_attempt_ts: &mut Option<f64>,
    image_time: &mut Option<f64>,
    fetched_at: f64,
    product_time: f64,
) {
    *last_attempt_ts = Some(fetched_at);
    *image_time = Some(product_time);
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

/// Build a timed GetMap URL so the PNG matches the advertised product time.
fn mosaic_getmap_url(time_iso: &str) -> String {
    // TIME is percent-encoded only for the separators that matter in a query;
    // ISO8601 uses `:` and the server accepts them unencoded, matching other
    // opengeo clients.
    format!(
        "{base}?service=WMS&version=1.1.1&request=GetMap\
         &layers=conus_bref_qcd\
         &bbox=-126,24,-66,50\
         &width=1200&height=520\
         &srs=EPSG:4326\
         &format=image/png&transparent=true&styles=\
         &time={time}",
        base = MOSAIC_OWS,
        time = time_iso,
    )
}

/// Pull the default product time from a WMS 1.1.1 GetCapabilities body.
///
/// Looks for `Extent name="time" ... default="..."` (the opengeo MRMS layout).
/// Returns the raw ISO8601 string from the attribute.
fn parse_wms_time_default(xml: &str) -> Option<&str> {
    // Prefer an Extent (or Dimension) whose name is time and that carries default=.
    for marker in ["Extent name=\"time\"", "Dimension name=\"time\""] {
        let mut rest = xml;
        while let Some(idx) = rest.find(marker) {
            let after = &rest[idx + marker.len()..];
            let end = after.find('>').unwrap_or(after.len());
            let attrs = &after[..end];
            if let Some(v) = attr_value(attrs, "default") {
                return Some(v);
            }
            rest = &after[end..];
        }
    }
    None
}

/// Read `name="value"` (double-quoted) from a fragment of XML attributes.
fn attr_value<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let start = attrs.find(&key)? + key.len();
    let tail = &attrs[start..];
    let end = tail.find('"')?;
    Some(&tail[..end])
}

/// Parse an ISO8601 / RFC3339 instant (e.g. `2026-08-11T19:52:12Z`) to unix
/// seconds. Accepts optional fractional seconds.
fn parse_iso8601_to_unix_secs(s: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp() as f64 + (dt.timestamp_subsec_millis() as f64) / 1000.0)
}

/// Fetch capabilities → product time, then the timed GetMap PNG.
async fn fetch_mosaic_product() -> Result<(egui::ColorImage, f64), String> {
    let caps = fetch_text(CAPABILITIES_URL).await?;
    let time_iso = parse_wms_time_default(&caps)
        .ok_or_else(|| "WMS capabilities missing time default".to_string())?
        .to_string();
    let product_time = parse_iso8601_to_unix_secs(&time_iso)
        .ok_or_else(|| format!("unparseable WMS time default: {time_iso}"))?;
    let image = fetch_and_decode(&mosaic_getmap_url(&time_iso)).await?;
    Ok((image, product_time))
}

/// GET a text body (GetCapabilities XML) via `window.fetch`.
async fn fetch_text(url: &str) -> Result<String, String> {
    let window = web_sys::window().ok_or("no window")?;
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_mode(web_sys::RequestMode::Cors);
    let request = web_sys::Request::new_with_str_and_init(url, &init)
        .map_err(|e| format!("request init failed: {:?}", e))?;
    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(err_text)?;
    let resp: web_sys::Response = resp_value
        .dyn_into()
        .map_err(|_| "invalid response object".to_string())?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let text_promise = resp
        .text()
        .map_err(|e| format!("failed to read body: {}", err_text(e)))?;
    let text_value = JsFuture::from(text_promise).await.map_err(err_text)?;
    text_value
        .as_string()
        .ok_or_else(|| "body not a string".to_string())
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
    fn loaded_sets_product_time_not_fetch_clock() {
        let mut last = None;
        let mut image = None;
        record_loaded(&mut last, &mut image, 1_700_000_100.0, 1_700_000_000.0);
        assert_eq!(last, Some(1_700_000_100.0));
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
        record_loaded(&mut last, &mut image, 75.0, 70.0);
        assert_eq!(image, Some(70.0));
        record_failed(&mut last, 90.0);
        assert_eq!(image, Some(70.0));
    }

    #[wasm_bindgen_test]
    fn parse_wms_time_default_reads_extent_attribute() {
        let xml = r#"
            <Layer>
              <Name>conus_bref_qcd</Name>
              <Dimension name="time" units="ISO8601"/>
              <Extent name="time" default="2026-08-11T19:52:12Z" nearestValue="1">
                2026-08-11T19:50:09.000Z,2026-08-11T19:52:12.000Z
              </Extent>
            </Layer>
        "#;
        assert_eq!(parse_wms_time_default(xml), Some("2026-08-11T19:52:12Z"));
    }

    #[wasm_bindgen_test]
    fn parse_wms_time_default_missing_returns_none() {
        assert_eq!(parse_wms_time_default("<Layer/>"), None);
        assert_eq!(
            parse_wms_time_default(r#"<Extent name="elevation" default="0"/>"#),
            None
        );
    }

    #[wasm_bindgen_test]
    fn parse_iso8601_z_and_fractional() {
        assert_eq!(
            parse_iso8601_to_unix_secs("2026-08-11T19:52:12Z"),
            Some(1_786_477_932.0)
        );
        let frac = parse_iso8601_to_unix_secs("2026-08-11T19:52:12.500Z").unwrap();
        assert!((frac - 1_786_477_932.5).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn parse_iso8601_rejects_garbage() {
        assert_eq!(parse_iso8601_to_unix_secs("not-a-time"), None);
        assert_eq!(parse_iso8601_to_unix_secs(""), None);
    }

    #[wasm_bindgen_test]
    fn getmap_url_embeds_time_and_bbox() {
        let url = mosaic_getmap_url("2026-08-11T19:52:12Z");
        assert!(url.contains("request=GetMap"));
        assert!(url.contains("time=2026-08-11T19:52:12Z"));
        assert!(url.contains("bbox=-126,24,-66,50"));
        assert!(url.starts_with(MOSAIC_OWS));
    }
}
