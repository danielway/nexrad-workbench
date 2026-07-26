//! Pure geocoding-request decisions.
//!
//! The site modal accepts free text in its zip field; whether that text is
//! worth a network round-trip — and what to tell the user when it isn't — is a
//! decision, not I/O. The shell asks here first and only then emits
//! [`Effect::GeocodeZip`](crate::core::Effect::GeocodeZip).

/// Message shown when the zip field doesn't hold a US zip code.
pub(crate) const INVALID_ZIP_MESSAGE: &str = "Please enter a valid 5-digit zip code";

/// Validate a raw zip-field submission.
///
/// `Ok` carries the normalized (trimmed) zip to look up; `Err` carries the
/// message to show in the modal. US zips only — the Zippopotam.us endpoint the
/// shell calls is scoped to `/us/`.
pub(crate) fn decide_zip_submission(raw: &str) -> Result<String, &'static str> {
    let zip = raw.trim();
    if zip.len() == 5 && zip.chars().all(|c| c.is_ascii_digit()) {
        Ok(zip.to_string())
    } else {
        Err(INVALID_ZIP_MESSAGE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn five_digits_pass_and_are_trimmed() {
        assert_eq!(decide_zip_submission("50309"), Ok("50309".to_string()));
        assert_eq!(decide_zip_submission("  50309 "), Ok("50309".to_string()));
        // Leading zeros are significant — no numeric parse anywhere in the path.
        assert_eq!(decide_zip_submission("01001"), Ok("01001".to_string()));
    }

    #[wasm_bindgen_test]
    fn wrong_length_is_rejected() {
        assert_eq!(decide_zip_submission("5030"), Err(INVALID_ZIP_MESSAGE));
        assert_eq!(decide_zip_submission("503090"), Err(INVALID_ZIP_MESSAGE));
        assert_eq!(decide_zip_submission(""), Err(INVALID_ZIP_MESSAGE));
        assert_eq!(decide_zip_submission("   "), Err(INVALID_ZIP_MESSAGE));
    }

    #[wasm_bindgen_test]
    fn non_digits_are_rejected() {
        assert_eq!(decide_zip_submission("5030a"), Err(INVALID_ZIP_MESSAGE));
        assert_eq!(decide_zip_submission("K DMX"), Err(INVALID_ZIP_MESSAGE));
        // Interior whitespace survives the trim and fails the digit check.
        assert_eq!(decide_zip_submission("50 09"), Err(INVALID_ZIP_MESSAGE));
        // Non-ASCII digits (e.g. Arabic-Indic) are not ASCII digits.
        assert_eq!(decide_zip_submission("٥٠٣٠٩"), Err(INVALID_ZIP_MESSAGE));
    }
}
