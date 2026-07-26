//! Networking primitives shared across data sources.
//!
//! The [`retry`] module holds the single retry policy applied consistently to
//! every outbound HTTP request the app makes (S3 archive, S3 real-time chunks,
//! NWS alerts, zip-code geocoding, NOAA mosaic); [`err_text`] renders the
//! `JsValue` errors those requests reject with.

pub(crate) mod retry;

use wasm_bindgen::JsValue;

/// Render a rejected fetch/promise `JsValue` as a human-readable string.
///
/// Browser rejections arrive as a bare string, an `Error`-shaped object with a
/// `message` field, or something else entirely; this tries each in turn so log
/// lines and user-facing error text never degrade to `JsValue(...)` noise.
pub(crate) fn err_text(v: JsValue) -> String {
    v.as_string()
        .or_else(|| {
            js_sys::Reflect::get(&v, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{:?}", v))
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn plain_string_rejection_passes_through() {
        assert_eq!(err_text(JsValue::from_str("boom")), "boom");
    }

    #[wasm_bindgen_test]
    fn error_shaped_object_uses_its_message() {
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("message"),
            &JsValue::from_str("nope"),
        )
        .unwrap();
        assert_eq!(err_text(obj.into()), "nope");
    }

    #[wasm_bindgen_test]
    fn other_values_fall_back_to_debug() {
        // No string, no `message` field — the Debug rendering is the floor, and
        // it must still say something about the value.
        let out = err_text(JsValue::from_f64(42.0));
        assert!(!out.is_empty());
    }
}
