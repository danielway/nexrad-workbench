//! Networking primitives shared across data sources.
//!
//! Today this is just the [`retry`] module — a single retry policy applied
//! consistently to every outbound HTTP request the app makes (S3 archive,
//! S3 real-time chunks, NWS alerts, zip-code geocoding, NOAA mosaic).

pub(crate) mod retry;
