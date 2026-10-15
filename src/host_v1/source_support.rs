//! Production-source references for downstream contract checks.

pub const LIVE_LIFECYCLE_SOURCE: &str = include_str!("live_lifecycle.rs");
pub const LIVE_FILL_SOURCE: &str = include_str!("live_fill.rs");
pub const CANCEL_ACK_SOURCE: &str = include_str!("cancel_ack.rs");
pub const CANCEL_BOOKING_SOURCE: &str = include_str!("cancel_booking.rs");
pub const BACKFILL_INPUT_SOURCE: &str = include_str!("backfill_input.rs");
pub const RECOVERY_RESULTS_SOURCE: &str = include_str!("recovery_results.rs");
