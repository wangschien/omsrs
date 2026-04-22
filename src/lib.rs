//! omsrs — Rust port of omspy's order-management core.

pub mod async_broker;
pub mod async_virtual_broker;
pub mod broker;
pub mod brokers;
pub mod clock;
pub mod compound_order;
pub mod models;
pub mod order;
pub mod order_strategy;
pub mod parity_gate;
pub mod persistence;
pub mod replica_broker;
pub mod simulation;
pub mod utils;
pub mod virtual_broker;

pub use async_broker::{AsyncBroker, AsyncSymbolTransformer};
pub use async_virtual_broker::AsyncVirtualBroker;
pub use broker::{rename, Broker};
pub use brokers::{AsyncPaper, Paper};
pub use clock::{clock_system_default, Clock, MockClock, SystemClock};
pub use models::{BasicPosition, OrderBook, OrderLock, QuantityMatch, Quote};
pub use order::{Order, OrderInit};
pub use persistence::{PersistenceError, PersistenceHandle};
pub use utils::{
    create_basic_positions_from_orders_dict, dict_filter, update_quantity, OrderRecord, UQty,
};
