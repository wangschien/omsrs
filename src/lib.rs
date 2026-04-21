//! omsrs — Rust port of omspy's order-management core.

pub mod broker;
pub mod brokers;
pub mod clock;
pub mod models;
pub mod order;
pub mod parity_gate;
pub mod persistence;
pub mod replica_broker;
pub mod simulation;
pub mod utils;
pub mod virtual_broker;

pub use broker::{rename, Broker};
pub use brokers::Paper;
pub use clock::{clock_system_default, Clock, MockClock, SystemClock};
pub use models::{BasicPosition, OrderBook, OrderLock, QuantityMatch, Quote};
pub use order::{Order, OrderInit};
pub use persistence::{PersistenceError, PersistenceHandle};
pub use utils::{
    create_basic_positions_from_orders_dict, dict_filter, update_quantity, OrderRecord, UQty,
};
