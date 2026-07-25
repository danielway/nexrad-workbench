//! Real-time streaming: the live chunk feed and its sequencing model.
//! Covers the realtime channel that polls AWS for chunks and drives the
//! streaming loop ([`realtime`]), the streaming lifecycle state machine
//! ([`streaming_state`]), the elevation/product filter shared with timing
//! projections ([`streaming_filter`]), and the per-volume chunk-arrival
//! plan consumed by the scheduler and diagnostics ([`streaming_plan`]).

pub(crate) mod realtime;
pub(crate) mod streaming_filter;
pub(crate) mod streaming_plan;
pub(crate) mod streaming_state;
