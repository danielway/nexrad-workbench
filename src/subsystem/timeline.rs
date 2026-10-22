//! Timeline subsystem: the storm-activity book behind the overview's bars.
//!
//! The per-site scan inventory and listing shadows live on each
//! [`crate::subsystem::sites::SiteState`]; what stays here is shared across
//! sites because it is already keyed by site.

use crate::core::storm_activity::ActivityBook;

/// Owner of the timeline state shared across sites.
#[derive(Default)]
pub(crate) struct Timeline {
    /// Per-site-day storm activity (from archive listing sizes) for the
    /// overview's zoomed-out bars. Kept across site changes; it's keyed by
    /// site.
    pub activity: ActivityBook,
}
