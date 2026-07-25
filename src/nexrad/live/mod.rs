//! Real-time streaming: the live chunk feed and its sequencing model.
//! Covers the realtime channel that polls AWS for chunks and drives the
//! streaming loop ([`realtime`]) and the streaming lifecycle state machine
//! ([`streaming_state`]). The elevation/product filter shared with timing
//! projections ([`crate::core::StreamingFilter`]) and the per-volume
//! chunk-arrival plan consumed by the scheduler and diagnostics
//! ([`crate::core::StreamingPlan`]) live in the pure core.

pub(crate) mod realtime;
pub(crate) mod streaming_state;
