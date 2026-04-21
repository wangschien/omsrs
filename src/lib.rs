//! omsrs — Rust port of omspy's order-management core.

pub mod models;
pub mod parity_gate;
pub mod utils;

pub use models::{BasicPosition, QuantityMatch};
pub use utils::{
    create_basic_positions_from_orders_dict, dict_filter, update_quantity, OrderRecord, UQty,
};
