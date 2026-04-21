//! `omspy.simulation.virtual.ReplicaBroker` port (PORT-PLAN §8 R7).
//!
//! ReplicaBroker is a matching engine — fills drive off `OrderFill` config
//! per plan §12. The R7 tests depend heavily on upstream's "same Python
//! object across multiple collections" semantic (`id(order) ==
//! id(broker.orders[...]) == id(broker._user_orders[...][i]) ==
//! id(broker.pending[i]) == id(broker.fills[i].order)`), so we model
//! shared ownership via `Arc<parking_lot::Mutex<VOrder>>` — all collections
//! hold handles to the same locked cell. Callers do `handle.lock()` to
//! read or mutate state; identity is `Arc::ptr_eq`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use crate::simulation::{Instrument, OrderType, Side, VOrder, VOrderInit};

pub type OrderHandle = Arc<Mutex<VOrder>>;

#[derive(Debug)]
pub struct ReplicaFill {
    pub order: OrderHandle,
    pub last_price: f64,
}

pub struct ReplicaBroker {
    pub name: String,
    pub instruments: HashMap<String, Instrument>,
    pub orders: HashMap<String, OrderHandle>,
    pub users: HashSet<String>,
    pub pending: Vec<OrderHandle>,
    pub completed: Vec<OrderHandle>,
    pub fills: Vec<ReplicaFill>,
    pub user_orders: HashMap<String, Vec<OrderHandle>>,
}

impl Default for ReplicaBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplicaBroker {
    pub fn new() -> Self {
        let mut users = HashSet::new();
        users.insert("default".into());
        Self {
            name: "replica".into(),
            instruments: HashMap::new(),
            orders: HashMap::new(),
            users,
            pending: Vec::new(),
            completed: Vec::new(),
            fills: Vec::new(),
            user_orders: HashMap::new(),
        }
    }

    /// Upstream `update(instruments)` — overwrites by `inst.name`.
    pub fn update(&mut self, instruments: Vec<Instrument>) {
        for inst in instruments {
            let name = inst.name.clone();
            self.instruments.insert(name, inst);
        }
    }

    /// Upstream `order_place(**kwargs)`. kwargs include `symbol`, `side`,
    /// `quantity`, `price`, `trigger_price`, `order_type`, `user`.
    /// Rejects with `Status::REJECTED` if the symbol isn't in instruments.
    pub fn order_place(&mut self, mut args: HashMap<String, Value>) -> OrderHandle {
        let user = args
            .remove("user")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "default".into());
        let order_id = uuid::Uuid::new_v4().simple().to_string();

        let init = VOrderInit {
            order_id: order_id.clone(),
            symbol: args
                .get("symbol")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            quantity: args.get("quantity").and_then(Value::as_f64).unwrap_or(0.0),
            side: args.get("side").and_then(value_to_side),
            price: args.get("price").and_then(Value::as_f64),
            trigger_price: args.get("trigger_price").and_then(Value::as_f64),
            order_type: args.get("order_type").and_then(value_to_order_type),
            ..Default::default()
        };

        let order_result = VOrder::from_init(init);
        let order = order_result.expect("ReplicaBroker rejects invalid side via upstream ValueError path — upstream doesn't guard either");
        let handle: OrderHandle = Arc::new(Mutex::new(order));
        self.orders.insert(order_id.clone(), handle.clone());
        self.user_orders
            .entry(user.clone())
            .or_default()
            .push(handle.clone());

        // Ensure the user is tracked even if it's new.
        self.users.insert(user);

        let symbol = handle.lock().symbol.clone();
        if !self.instruments.contains_key(&symbol) {
            let mut g = handle.lock();
            g.status_message = Some(format!("REJECTED: Symbol {symbol} not found on the system"));
            g.canceled_quantity = g.quantity;
            g.pending_quantity = 0.0;
            drop(g);
            self.completed.push(handle.clone());
            return handle;
        }

        let last_price = self.instruments[&symbol].last_price;
        // Apply OrderFill's `as_market` branch in-place on the shared order.
        apply_as_market(&handle, last_price);
        self.pending.push(handle.clone());
        self.fills.push(ReplicaFill {
            order: handle.clone(),
            last_price,
        });
        handle
    }

    /// Upstream `order_modify(order_id, **kwargs)` — overwrites any
    /// attribute whose name matches a VOrder field. Accepts `order_type`
    /// as either an enum or its numeric `Side`-style literal
    /// (`1 = MARKET`, `2 = LIMIT`, `3 = STOP`).
    pub fn order_modify(
        &mut self,
        order_id: &str,
        args: HashMap<String, Value>,
    ) -> Option<OrderHandle> {
        let handle = self.orders.get(order_id)?.clone();
        {
            let mut g = handle.lock();
            for (k, v) in &args {
                match k.as_str() {
                    "price" => {
                        if let Some(n) = v.as_f64() {
                            g.price = Some(n);
                        }
                    }
                    "trigger_price" => {
                        if let Some(n) = v.as_f64() {
                            g.trigger_price = Some(n);
                        }
                    }
                    "quantity" => {
                        if let Some(n) = v.as_f64() {
                            g.quantity = n;
                        }
                    }
                    "order_type" => {
                        if let Some(ot) = value_to_order_type(v) {
                            g.order_type = ot;
                        }
                    }
                    _ => {}
                }
            }
        }
        Some(handle)
    }

    /// Upstream `order_cancel(order_id)` — sets canceled_quantity =
    /// quantity - filled, appends to completed if not already done.
    pub fn order_cancel(&mut self, order_id: &str) -> Option<OrderHandle> {
        let handle = self.orders.get(order_id)?.clone();
        let mut append_completed = false;
        {
            let mut g = handle.lock();
            if !g.is_done() {
                g.canceled_quantity = g.quantity - g.filled_quantity;
                append_completed = true;
            }
        }
        if append_completed {
            self.completed.push(handle.clone());
        }
        Some(handle)
    }

    /// Upstream `run_fill()` — iterates `fills`, runs `update` on each,
    /// moves done orders into `completed`, then filters out done entries
    /// from `fills`.
    pub fn run_fill(&mut self) {
        let mut done_ids = Vec::new();
        let mut done_indices = Vec::new();
        for (i, fill) in self.fills.iter_mut().enumerate() {
            let symbol = fill.order.lock().symbol.clone();
            let Some(inst) = self.instruments.get(&symbol) else {
                continue;
            };
            let last_price = inst.last_price;
            if last_price == 0.0 {
                continue;
            }
            fill.last_price = last_price;
            apply_fill_update(&fill.order, last_price);
            if fill.order.lock().is_done() {
                done_ids.push(fill.order.lock().order_id.clone());
                done_indices.push(i);
            }
        }
        for id in &done_ids {
            if let Some(h) = self.orders.get(id) {
                self.completed.push(h.clone());
            }
        }
        // Remove done fills (upstream:
        // `self.fills = [f for f in self.fills if not f.done]`).
        self.fills.retain(|f| !f.order.lock().is_done());
    }
}

fn value_to_side(v: &Value) -> Option<Side> {
    if let Some(n) = v.as_i64() {
        return match n {
            1 => Some(Side::Buy),
            -1 => Some(Side::Sell),
            _ => None,
        };
    }
    v.as_str().and_then(|s| Side::parse(s).ok())
}

/// `order_type` in the upstream `order_inputs` fixture is sent as an
/// integer enum literal (1 / 2 / 3) — match those.
fn value_to_order_type(v: &Value) -> Option<OrderType> {
    if let Some(n) = v.as_i64() {
        return match n {
            1 => Some(OrderType::Market),
            2 => Some(OrderType::Limit),
            3 => Some(OrderType::Stop),
            _ => None,
        };
    }
    v.as_str().and_then(|s| OrderType::parse(s).ok())
}

/// Mirrors `OrderFill._as_market` on a shared order handle. LIMIT /
/// STOP only — MARKET is a no-op until `apply_fill_update`.
fn apply_as_market(handle: &OrderHandle, last_price: f64) {
    let mut g = handle.lock();
    match g.order_type {
        OrderType::Limit => {
            let Some(price) = g.price else { return };
            let triggered = match g.side {
                Side::Buy => last_price < price,
                Side::Sell => last_price > price,
            };
            if triggered {
                g.filled_quantity = g.quantity;
                g.average_price = Some(last_price);
            }
            g.make_right_quantity();
        }
        OrderType::Stop => {
            let price = g.trigger_price.or(g.price);
            let Some(price) = price else { return };
            let triggered = match g.side {
                Side::Buy => last_price > price,
                Side::Sell => last_price < price,
            };
            if triggered {
                g.filled_quantity = g.quantity;
                g.average_price = Some(last_price);
            }
            g.make_right_quantity();
        }
        OrderType::Market => {}
    }
}

/// Mirrors `OrderFill.update()` on a shared order handle.
fn apply_fill_update(handle: &OrderHandle, last_price: f64) {
    let mut g = handle.lock();
    if g.is_done() {
        return;
    }
    match g.order_type {
        OrderType::Market => {
            g.price = Some(last_price);
            g.average_price = Some(last_price);
            g.filled_quantity = g.quantity;
            g.make_right_quantity();
        }
        OrderType::Limit => {
            let Some(price) = g.price else { return };
            let triggered = match g.side {
                Side::Buy => last_price < price,
                Side::Sell => last_price > price,
            };
            if triggered {
                g.average_price = Some(price);
                g.filled_quantity = g.quantity;
                g.make_right_quantity();
            }
        }
        OrderType::Stop => {
            if g.trigger_price.is_none() {
                g.trigger_price = g.price;
            }
            let Some(trigger) = g.trigger_price else {
                return;
            };
            let triggered = match g.side {
                Side::Buy => last_price > trigger,
                Side::Sell => last_price < trigger,
            };
            if triggered {
                g.average_price = Some(last_price);
                g.filled_quantity = g.quantity;
                g.make_right_quantity();
            }
        }
    }
}
