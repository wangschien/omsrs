//! omsrs — Rust port of omspy's order-management core.

pub mod clock;
pub mod models;
pub mod parity_gate;
pub mod utils;

pub use clock::{clock_system_default, Clock, MockClock, SystemClock};
pub use models::{BasicPosition, OrderBook, OrderLock, QuantityMatch, Quote};
pub use utils::{
    create_basic_positions_from_orders_dict, dict_filter, update_quantity, OrderRecord, UQty,
};
