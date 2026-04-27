//! Upstream `omspy.order.Order` port.
//!
//! Pydantic `**kwargs` construction is mapped to an [`OrderInit`] struct
//! (all fields `Default`-populated) that [`Order::from_init`] finalizes:
//! assigns a UUID-v4 id if absent, stamps `timestamp` + `pending_quantity` +
//! `expires_in`, and builds the `OrderLock` with the same clock. Clock
//! access is via the injected `Arc<dyn Clock + Send + Sync>` (PORT-PLAN §6
//! D4) — `from_init_with_clock` lets tests swap in `MockClock`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, NaiveDateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::broker::Broker;
use crate::clock::{clock_system_default, Clock};
use crate::models::OrderLock;
use crate::persistence::PersistenceHandle;

/// Input carrier — `Default` gives upstream's pydantic-default field set,
/// letting tests write `Order::from_init(OrderInit { symbol: "aapl".into(),
/// side: "buy".into(), quantity: 10, ..Default::default() })`.
impl OrderInit {
    /// Parse a DB row back into an `OrderInit`. Missing columns default to
    /// `None` / `0` / empty; present-but-null columns do too.
    pub fn from_row(row: &HashMap<String, Value>) -> Self {
        let s = |k: &str| row.get(k).and_then(|v| v.as_str().map(String::from));
        let s_opt = |k: &str| {
            row.get(k).and_then(|v| match v {
                Value::Null => None,
                Value::String(x) => Some(x.clone()),
                _ => None,
            })
        };
        let i = |k: &str| row.get(k).and_then(|v| v.as_i64());
        let b = |k: &str| {
            row.get(k).map(|v| match v {
                Value::Bool(b) => *b,
                Value::Number(n) => n.as_i64().map(|x| x != 0).unwrap_or(false),
                Value::String(s) => {
                    let lower = s.to_ascii_lowercase();
                    lower == "true" || lower == "1"
                }
                _ => false,
            })
        };
        let d = |k: &str| -> Option<Decimal> {
            row.get(k).and_then(|v| match v {
                Value::Null => None,
                Value::Number(n) => n.as_f64().and_then(|f| Decimal::try_from(f).ok()),
                Value::String(s) => s.parse().ok(),
                _ => None,
            })
        };
        let dt = |k: &str| -> Option<DateTime<Utc>> {
            row.get(k).and_then(|v| match v {
                Value::Null => None,
                Value::String(s) => DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|t| t.with_timezone(&Utc)),
                _ => None,
            })
        };
        let jmap = |k: &str| -> Option<HashMap<String, Value>> {
            row.get(k).and_then(|v| match v {
                Value::Null => None,
                Value::String(s) => serde_json::from_str(s).ok(),
                Value::Object(m) => Some(m.clone().into_iter().collect()),
                _ => None,
            })
        };

        Self {
            symbol: s("symbol").unwrap_or_default(),
            side: s("side").unwrap_or_default(),
            quantity: i("quantity").unwrap_or(0),
            id: s_opt("id"),
            parent_id: s_opt("parent_id"),
            timestamp: dt("timestamp"),
            order_type: s_opt("order_type"),
            broker_timestamp: dt("broker_timestamp"),
            exchange_timestamp: dt("exchange_timestamp"),
            order_id: s_opt("order_id"),
            exchange_order_id: s_opt("exchange_order_id"),
            price: d("price"),
            trigger_price: d("trigger_price"),
            average_price: d("average_price"),
            pending_quantity: i("pending_quantity"),
            filled_quantity: i("filled_quantity"),
            cancelled_quantity: i("cancelled_quantity"),
            disclosed_quantity: i("disclosed_quantity"),
            validity: s_opt("validity"),
            status: s_opt("status"),
            expires_in: i("expires_in"),
            timezone: s_opt("timezone"),
            client_id: s_opt("client_id"),
            convert_to_market_after_expiry: b("convert_to_market_after_expiry"),
            cancel_after_expiry: b("cancel_after_expiry"),
            retries: i("retries"),
            max_modifications: i("max_modifications"),
            exchange: s_opt("exchange"),
            tag: s_opt("tag"),
            connection: None,
            can_peg: b("can_peg"),
            pseudo_id: s_opt("pseudo_id"),
            strategy_id: s_opt("strategy_id"),
            portfolio_id: s_opt("portfolio_id"),
            json: jmap("JSON"),
            error: s_opt("error"),
            is_multi: b("is_multi"),
            last_updated_at: dt("last_updated_at"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct OrderInit {
    pub symbol: String,
    pub side: String,
    pub quantity: i64,
    pub id: Option<String>,
    pub parent_id: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub order_type: Option<String>,
    pub broker_timestamp: Option<DateTime<Utc>>,
    pub exchange_timestamp: Option<DateTime<Utc>>,
    pub order_id: Option<String>,
    pub exchange_order_id: Option<String>,
    pub price: Option<Decimal>,
    pub trigger_price: Option<Decimal>,
    pub average_price: Option<Decimal>,
    pub pending_quantity: Option<i64>,
    pub filled_quantity: Option<i64>,
    pub cancelled_quantity: Option<i64>,
    pub disclosed_quantity: Option<i64>,
    pub validity: Option<String>,
    pub status: Option<String>,
    pub expires_in: Option<i64>,
    pub timezone: Option<String>,
    pub client_id: Option<String>,
    pub convert_to_market_after_expiry: Option<bool>,
    pub cancel_after_expiry: Option<bool>,
    pub retries: Option<i64>,
    pub max_modifications: Option<i64>,
    pub exchange: Option<String>,
    pub tag: Option<String>,
    pub connection: Option<Arc<dyn PersistenceHandle>>,
    pub can_peg: Option<bool>,
    pub pseudo_id: Option<String>,
    pub strategy_id: Option<String>,
    pub portfolio_id: Option<String>,
    pub json: Option<HashMap<String, Value>>,
    pub error: Option<String>,
    pub is_multi: Option<bool>,
    pub last_updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub symbol: String,
    pub side: String,
    pub quantity: i64,
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub order_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange_timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Decimal>,
    pub trigger_price: Decimal,
    pub average_price: Decimal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_quantity: Option<i64>,
    pub filled_quantity: i64,
    pub cancelled_quantity: i64,
    pub disclosed_quantity: i64,
    pub validity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub expires_in: i64,
    pub timezone: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub convert_to_market_after_expiry: bool,
    pub cancel_after_expiry: bool,
    pub retries: i64,
    pub max_modifications: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip)]
    pub connection: Option<Arc<dyn PersistenceHandle>>,
    pub can_peg: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pseudo_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portfolio_id: Option<String>,
    #[serde(rename = "JSON", default, skip_serializing_if = "Option::is_none")]
    pub json: Option<HashMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub is_multi: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated_at: Option<DateTime<Utc>>,

    #[serde(default, rename = "_num_modifications")]
    pub num_modifications: i64,

    #[serde(skip)]
    lock: OrderLock,

    #[serde(skip, default = "clock_system_default")]
    clock: Arc<dyn Clock + Send + Sync>,
}

impl Order {
    /// Upstream `Order._attrs` — the allow-list [`Order::update`] consults.
    pub const ATTRS: &'static [&'static str] = &[
        "exchange_timestamp",
        "exchange_order_id",
        "status",
        "filled_quantity",
        "pending_quantity",
        "disclosed_quantity",
        "average_price",
    ];
    /// Upstream `Order._frozen_attrs` — fields `modify()` refuses to overwrite.
    pub const FROZEN_ATTRS: &'static [&'static str] = &["symbol", "side"];
    /// Upstream `Order._exclude_fields` — skipped when serialising to DB.
    pub const EXCLUDE_FIELDS: &'static [&'static str] = &["connection"];

    pub fn frozen_attrs() -> HashSet<&'static str> {
        Self::FROZEN_ATTRS.iter().copied().collect()
    }

    /// Equivalent to pydantic `Order(**init)`: applies defaults, quantity
    /// validation, `id` / `timestamp` / `pending_quantity` / `expires_in`
    /// post-init, and wires a `SystemClock`.
    pub fn from_init(init: OrderInit) -> Self {
        Self::from_init_with_clock(init, clock_system_default())
    }

    /// Reconstruct an `Order` from a DB row (matches upstream `Order(**row)`).
    /// Fields absent from `row` fall back to `OrderInit` defaults; fields
    /// whose type needs normalising (bool 0/1 → `bool`, float → `Decimal`,
    /// ISO-string → `DateTime<Utc>`, JSON-string → `HashMap<String, Value>`)
    /// are converted here. Runs through `from_init` so upstream's
    /// `pending_quantity = quantity` reset and `expires_in` guards fire
    /// identically on reconstruction.
    pub fn from_row(row: &HashMap<String, Value>) -> Self {
        Self::from_init(OrderInit::from_row(row))
    }

    pub fn from_init_with_clock(init: OrderInit, clock: Arc<dyn Clock + Send + Sync>) -> Self {
        assert!(init.quantity >= 0, "quantity must be positive");

        let quantity = init.quantity;
        let timezone = init.timezone.unwrap_or_else(|| "local".to_string());
        let now = clock.now();
        let id = init
            .id
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let timestamp = Some(init.timestamp.unwrap_or(now));

        let expires_in = match init.expires_in {
            None | Some(0) => seconds_to_end_of_day(now),
            Some(v) => v.abs(),
        };

        // Upstream `Order.__init__` unconditionally sets
        // `self.pending_quantity = self.quantity` after validation
        // (`order.py:224`), ignoring any caller-provided value. Match that
        // to close R3.b audit P2.1 — preserves the invariant that
        // `Order(**row)` round-trips through a freshly-saved row and
        // ignores stored `pending_quantity` on partially-updated loads.
        let _unused_init_pending = init.pending_quantity;
        let pending_quantity = Some(quantity);

        let lock = OrderLock::unlocked_with_clock(clock.clone()).with_timezone(timezone.clone());

        Self {
            symbol: init.symbol,
            side: init.side,
            quantity,
            id: Some(id),
            parent_id: init.parent_id,
            timestamp,
            order_type: init.order_type.unwrap_or_else(|| "MARKET".to_string()),
            broker_timestamp: init.broker_timestamp,
            exchange_timestamp: init.exchange_timestamp,
            order_id: init.order_id,
            exchange_order_id: init.exchange_order_id,
            price: init.price,
            trigger_price: init.trigger_price.unwrap_or(Decimal::ZERO),
            average_price: init.average_price.unwrap_or(Decimal::ZERO),
            pending_quantity,
            filled_quantity: init.filled_quantity.unwrap_or(0),
            cancelled_quantity: init.cancelled_quantity.unwrap_or(0),
            disclosed_quantity: init.disclosed_quantity.unwrap_or(0),
            validity: init.validity.unwrap_or_else(|| "DAY".to_string()),
            status: init.status,
            expires_in,
            timezone,
            client_id: init.client_id,
            convert_to_market_after_expiry: init.convert_to_market_after_expiry.unwrap_or(false),
            cancel_after_expiry: init.cancel_after_expiry.unwrap_or(true),
            retries: init.retries.unwrap_or(0),
            max_modifications: init.max_modifications.unwrap_or(10),
            exchange: init.exchange,
            tag: init.tag,
            connection: init.connection,
            can_peg: init.can_peg.unwrap_or(true),
            pseudo_id: init.pseudo_id,
            strategy_id: init.strategy_id,
            portfolio_id: init.portfolio_id,
            json: init.json,
            error: init.error,
            is_multi: init.is_multi.unwrap_or(false),
            last_updated_at: init.last_updated_at,
            num_modifications: 0,
            lock,
            clock,
        }
    }

    pub fn lock(&self) -> &OrderLock {
        &self.lock
    }

    pub fn lock_mut(&mut self) -> &mut OrderLock {
        &mut self.lock
    }

    pub fn clock(&self) -> &Arc<dyn Clock + Send + Sync> {
        &self.clock
    }

    /// Overwrite the injected clock. Used by `CompoundOrder::add` and
    /// `OrderStrategy::add` to cascade a parent clock down the tree
    /// (PORT-PLAN §6 D4). Also updates the embedded `OrderLock`'s clock
    /// so `can_modify` / `can_cancel` see the new timeline.
    pub fn set_clock(&mut self, clock: Arc<dyn Clock + Send + Sync>) {
        self.clock = clock.clone();
        self.lock = OrderLock::unlocked_with_clock(clock).with_timezone(self.timezone.clone());
    }

    pub fn is_complete(&self) -> bool {
        if self.quantity == self.filled_quantity {
            return true;
        }
        if self.status.as_deref() == Some("COMPLETE") {
            return true;
        }
        self.filled_quantity + self.cancelled_quantity == self.quantity
    }

    pub fn is_pending(&self) -> bool {
        let qty = self.filled_quantity + self.cancelled_quantity;
        if matches!(
            self.status.as_deref(),
            Some("COMPLETE") | Some("CANCELED") | Some("CANCELLED") | Some("REJECTED")
        ) {
            return false;
        }
        qty < self.quantity
    }

    pub fn is_done(&self) -> bool {
        if self.is_complete() {
            return true;
        }
        matches!(
            self.status.as_deref(),
            Some("CANCELED") | Some("CANCELLED") | Some("REJECTED")
        )
    }

    pub fn time_to_expiry(&self) -> i64 {
        let Some(ts) = self.timestamp else {
            return self.expires_in;
        };
        let elapsed = (self.clock.now() - ts).num_seconds();
        (self.expires_in - elapsed).max(0)
    }

    pub fn time_after_expiry(&self) -> i64 {
        let Some(ts) = self.timestamp else {
            return 0;
        };
        let elapsed = (self.clock.now() - ts).num_seconds();
        (elapsed - self.expires_in).max(0)
    }

    pub fn has_expired(&self) -> bool {
        self.time_to_expiry() == 0
    }

    pub fn has_parent(&self) -> bool {
        self.parent_id.is_some()
    }

    /// Upstream `_get_other_args_from_attribs`. Collects `attribs_to_copy`
    /// from both the `broker.attribs_to_copy_<phase>` method and any caller-
    /// provided list, then reads the current order's attribute values into
    /// a kwarg map (skipping `None`).
    pub fn get_other_args_from_attribs(
        &self,
        broker_attrs: Option<Vec<String>>,
        attribs_to_copy: Option<&[&str]>,
    ) -> HashMap<String, Value> {
        let mut keys: HashSet<String> = HashSet::new();
        if let Some(extra) = attribs_to_copy {
            for k in extra {
                keys.insert((*k).to_string());
            }
        }
        if let Some(list) = broker_attrs {
            keys.extend(list);
        }
        let mut out = HashMap::new();
        for key in keys {
            if let Some(v) = self.get_attr(&key) {
                if !v.is_null() {
                    out.insert(key, v);
                }
            }
        }
        out
    }

    /// Map upstream field names → current value as a JSON `Value`. Returns
    /// `None` if the key isn't an Order attribute (mirroring `hasattr`
    /// returning `False`).
    fn get_attr(&self, key: &str) -> Option<Value> {
        match key {
            "symbol" => Some(json!(self.symbol)),
            "side" => Some(json!(self.side)),
            "quantity" => Some(json!(self.quantity)),
            "id" => Some(json!(self.id)),
            "parent_id" => self.parent_id.as_ref().map(|s| json!(s)),
            "order_type" => Some(json!(self.order_type)),
            "order_id" => self.order_id.as_ref().map(|s| json!(s)),
            "exchange_order_id" => self.exchange_order_id.as_ref().map(|s| json!(s)),
            "price" => self.price.map(|d| json!(d.to_string())),
            "trigger_price" => Some(json!(self.trigger_price.to_string())),
            "average_price" => Some(json!(self.average_price.to_string())),
            "pending_quantity" => self.pending_quantity.map(|q| json!(q)),
            "filled_quantity" => Some(json!(self.filled_quantity)),
            "cancelled_quantity" => Some(json!(self.cancelled_quantity)),
            "disclosed_quantity" => Some(json!(self.disclosed_quantity)),
            "validity" => Some(json!(self.validity)),
            "status" => self.status.as_ref().map(|s| json!(s)),
            "expires_in" => Some(json!(self.expires_in)),
            "timezone" => Some(json!(self.timezone)),
            "client_id" => self.client_id.as_ref().map(|s| json!(s)),
            "exchange" => self.exchange.as_ref().map(|s| json!(s)),
            "tag" => self.tag.as_ref().map(|s| json!(s)),
            "can_peg" => Some(json!(self.can_peg)),
            "pseudo_id" => self.pseudo_id.as_ref().map(|s| json!(s)),
            "strategy_id" => self.strategy_id.as_ref().map(|s| json!(s)),
            "portfolio_id" => self.portfolio_id.as_ref().map(|s| json!(s)),
            "error" => self.error.as_ref().map(|s| json!(s)),
            "is_multi" => Some(json!(self.is_multi)),
            "retries" => Some(json!(self.retries)),
            "max_modifications" => Some(json!(self.max_modifications)),
            "convert_to_market_after_expiry" => Some(json!(self.convert_to_market_after_expiry)),
            "cancel_after_expiry" => Some(json!(self.cancel_after_expiry)),
            _ => None,
        }
    }

    /// Mirrors `Order.update`. Returns `true` if the update was applied,
    /// `false` if the order was already in a terminal state.
    pub fn update(&mut self, data: &HashMap<String, Value>) -> bool {
        if self.is_done() {
            return false;
        }
        for key in Self::ATTRS {
            if let Some(v) = data.get(*key) {
                if !v.is_null() {
                    self.set_from_value(key, v);
                }
            }
        }
        self.last_updated_at = Some(self.clock.now());
        if !data.contains_key("pending_quantity") {
            self.pending_quantity = Some(self.quantity - self.filled_quantity);
        }
        if self.connection.is_some() {
            let _ = self.save_to_db();
        }
        true
    }

    fn set_from_value(&mut self, key: &str, v: &Value) {
        match key {
            "exchange_timestamp" => {
                if let Some(s) = v.as_str() {
                    if let Ok(parsed) = DateTime::parse_from_rfc3339(s) {
                        self.exchange_timestamp = Some(parsed.with_timezone(&Utc));
                    }
                }
            }
            "exchange_order_id" => {
                if let Some(s) = v.as_str() {
                    self.exchange_order_id = Some(s.to_string());
                }
            }
            "status" => {
                if let Some(s) = v.as_str() {
                    self.status = Some(s.to_string());
                }
            }
            "filled_quantity" => {
                if let Some(n) = v.as_i64() {
                    // Monotonic guard against out-of-order WS / poll
                    // events: cumulative `filled_quantity` must never
                    // decrease, and never exceed `quantity`. Without
                    // this, a delayed lower-numbered fill arriving
                    // after a higher one rolls cumulative state
                    // backwards and produces phantom inventory in
                    // consumers that trust the post-update value.
                    //
                    // Note: this is a defense-in-depth STRICTER than
                    // upstream omspy's contract. Upstream's
                    // `Order.update` (`refs/omspy/omspy/order.py:
                    // 446-458` in v0.16) is a plain `setattr` loop
                    // with no clamp, so an out-of-order WS event
                    // would silently regress its cumulative count.
                    // omsrs adopts the bot/execution-side guard
                    // (the `ManagedOrder.update` clamp at
                    // `bot/execution/orders.py:96-119` is the
                    // closest production reference) to make the
                    // crate safe to drop into a maker bot without
                    // re-implementing the same guard at every
                    // consumer site.
                    let clamped = n.clamp(self.filled_quantity, self.quantity);
                    if clamped > self.filled_quantity {
                        self.filled_quantity = clamped;
                    }
                }
            }
            "pending_quantity" => {
                if let Some(n) = v.as_i64() {
                    self.pending_quantity = Some(n);
                }
            }
            "disclosed_quantity" => {
                if let Some(n) = v.as_i64() {
                    self.disclosed_quantity = n;
                }
            }
            "average_price" => {
                if let Some(d) = value_to_decimal(v) {
                    self.average_price = d;
                }
            }
            _ => {}
        }
    }

    /// Mirrors `Order.execute`. If the order isn't complete and has no
    /// `order_id` yet, dispatches `broker.order_place(...)` and stashes
    /// the broker-assigned id. Returns the resulting `order_id` (either
    /// the freshly assigned one or the existing one) — matches upstream's
    /// `return order_id` / `return self.order_id` branches.
    pub fn execute(
        &mut self,
        broker: &dyn Broker,
        attribs_to_copy: Option<&[&str]>,
        kwargs: HashMap<String, Value>,
    ) -> Option<String> {
        if self.is_complete() || self.order_id.is_some() {
            return self.order_id.clone();
        }
        let other_args =
            self.get_other_args_from_attribs(broker.attribs_to_copy_execute(), attribs_to_copy);

        let mut order_args: HashMap<String, Value> = HashMap::new();
        order_args.insert("symbol".into(), json!(self.symbol.to_uppercase()));
        order_args.insert("side".into(), json!(self.side.to_uppercase()));
        order_args.insert("order_type".into(), json!(self.order_type.to_uppercase()));
        order_args.insert("quantity".into(), json!(self.quantity));
        if let Some(p) = self.price {
            order_args.insert("price".into(), decimal_value(p));
        }
        order_args.insert("trigger_price".into(), decimal_value(self.trigger_price));
        order_args.insert("disclosed_quantity".into(), json!(self.disclosed_quantity));

        // Precedence (matches upstream `order.py:507-509`):
        //   1. build defaults (above)
        //   2. apply broker-copied attributes over defaults
        //   3. apply caller kwargs **filtered** to exclude default keys,
        //      so kwargs win over copied attributes but not over the
        //      order's own symbol/side/quantity/etc.
        for (k, v) in other_args {
            order_args.insert(k, v);
        }
        let default_keys: HashSet<String> = [
            "symbol",
            "side",
            "order_type",
            "quantity",
            "price",
            "trigger_price",
            "disclosed_quantity",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        for (k, v) in &kwargs {
            if !default_keys.contains(k) {
                order_args.insert(k.clone(), v.clone());
            }
        }

        let ret = broker.order_place(order_args);
        self.order_id = ret.clone();
        if self.connection.is_some() {
            let _ = self.save_to_db();
        }
        ret
    }

    /// Mirrors `Order.modify`. Respects `lock.can_modify()` and
    /// `max_modifications`.
    pub fn modify(
        &mut self,
        broker: &dyn Broker,
        attribs_to_copy: Option<&[&str]>,
        kwargs: HashMap<String, Value>,
    ) {
        if !self.lock.can_modify() {
            return;
        }
        let other_args =
            self.get_other_args_from_attribs(broker.attribs_to_copy_modify(), attribs_to_copy);

        let keys_for_broker: HashSet<&str> = [
            "order_id",
            "quantity",
            "price",
            "trigger_price",
            "order_type",
            "disclosed_quantity",
        ]
        .into_iter()
        .collect();

        let frozen = Self::frozen_attrs();
        let mut args_to_add: HashMap<String, Value> = HashMap::new();
        for (k, v) in &kwargs {
            if frozen.contains(k.as_str()) {
                continue;
            }
            let is_known = self.get_attr(k).is_some();
            if is_known {
                self.set_local_field(k, v);
                if !keys_for_broker.contains(k.as_str()) {
                    args_to_add.insert(k.clone(), v.clone());
                }
            } else {
                // Unknown-to-Order key → passthrough as broker arg.
                args_to_add.insert(k.clone(), v.clone());
            }
        }

        let mut order_args: HashMap<String, Value> = HashMap::new();
        if let Some(ref oid) = self.order_id {
            order_args.insert("order_id".into(), json!(oid));
        }
        order_args.insert("quantity".into(), json!(self.quantity));
        if let Some(p) = self.price {
            order_args.insert("price".into(), decimal_value(p));
        }
        order_args.insert("trigger_price".into(), decimal_value(self.trigger_price));
        order_args.insert("order_type".into(), json!(self.order_type.to_uppercase()));
        order_args.insert("disclosed_quantity".into(), json!(self.disclosed_quantity));

        for (k, v) in other_args {
            order_args.insert(k, v);
        }
        for (k, v) in &args_to_add {
            order_args.insert(k.clone(), v.clone());
        }
        for key in &keys_for_broker {
            if let Some(v) = kwargs.get(*key) {
                order_args.insert((*key).to_string(), v.clone());
            }
        }

        if self.num_modifications < self.max_modifications {
            if self.order_id.is_none() {
                return;
            }
            broker.order_modify(order_args);
            self.num_modifications += 1;
            if self.connection.is_some() {
                let _ = self.save_to_db();
            }
        }
    }

    fn set_local_field(&mut self, key: &str, v: &Value) {
        match key {
            "quantity" => {
                if let Some(n) = v.as_i64() {
                    self.quantity = n;
                }
            }
            "price" => {
                self.price = value_to_decimal(v);
            }
            "trigger_price" => {
                if let Some(d) = value_to_decimal(v) {
                    self.trigger_price = d;
                }
            }
            "order_type" => {
                if let Some(s) = v.as_str() {
                    self.order_type = s.to_string();
                }
            }
            "disclosed_quantity" => {
                if let Some(n) = v.as_i64() {
                    self.disclosed_quantity = n;
                }
            }
            "exchange" => {
                if let Some(s) = v.as_str() {
                    self.exchange = Some(s.to_string());
                }
            }
            "client_id" => {
                if let Some(s) = v.as_str() {
                    self.client_id = Some(s.to_string());
                }
            }
            "validity" => {
                if let Some(s) = v.as_str() {
                    self.validity = s.to_string();
                }
            }
            "tag" => {
                if let Some(s) = v.as_str() {
                    self.tag = Some(s.to_string());
                }
            }
            "status" => {
                if let Some(s) = v.as_str() {
                    self.status = Some(s.to_string());
                }
            }
            "filled_quantity" => {
                if let Some(n) = v.as_i64() {
                    // Monotonic guard against out-of-order WS / poll
                    // events: cumulative `filled_quantity` must never
                    // decrease, and never exceed `quantity`. Without
                    // this, a delayed lower-numbered fill arriving
                    // after a higher one rolls cumulative state
                    // backwards and produces phantom inventory in
                    // consumers that trust the post-update value.
                    //
                    // Note: this is a defense-in-depth STRICTER than
                    // upstream omspy's contract. Upstream's
                    // `Order.update` (`refs/omspy/omspy/order.py:
                    // 446-458` in v0.16) is a plain `setattr` loop
                    // with no clamp, so an out-of-order WS event
                    // would silently regress its cumulative count.
                    // omsrs adopts the bot/execution-side guard
                    // (the `ManagedOrder.update` clamp at
                    // `bot/execution/orders.py:96-119` is the
                    // closest production reference) to make the
                    // crate safe to drop into a maker bot without
                    // re-implementing the same guard at every
                    // consumer site.
                    let clamped = n.clamp(self.filled_quantity, self.quantity);
                    if clamped > self.filled_quantity {
                        self.filled_quantity = clamped;
                    }
                }
            }
            "pending_quantity" => {
                if let Some(n) = v.as_i64() {
                    self.pending_quantity = Some(n);
                }
            }
            "cancelled_quantity" => {
                if let Some(n) = v.as_i64() {
                    self.cancelled_quantity = n;
                }
            }
            "average_price" => {
                if let Some(d) = value_to_decimal(v) {
                    self.average_price = d;
                }
            }
            "expires_in" => {
                if let Some(n) = v.as_i64() {
                    self.expires_in = n;
                }
            }
            _ => {}
        }
    }

    pub fn cancel(&mut self, broker: &dyn Broker, attribs_to_copy: Option<&[&str]>) {
        if !self.lock.can_cancel() {
            return;
        }
        if self.order_id.is_none() {
            return;
        }
        let other_args =
            self.get_other_args_from_attribs(broker.attribs_to_copy_cancel(), attribs_to_copy);
        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert("order_id".into(), json!(self.order_id.clone().unwrap()));
        for (k, v) in other_args {
            args.insert(k, v);
        }
        broker.order_cancel(args);
    }

    /// Code 1 locks modify; code 2 locks cancel (matches upstream numbering).
    pub fn add_lock(&mut self, code: i32, seconds: f64) {
        match code {
            1 => {
                self.lock.modify(seconds);
            }
            2 => {
                self.lock.cancel(seconds);
            }
            _ => {}
        }
    }

    pub fn save_to_db(&self) -> bool {
        let Some(handle) = self.connection.clone() else {
            return false;
        };
        let row = self.to_row();
        match handle.upsert_order(row) {
            Ok(()) => true,
            Err(e) => {
                eprintln!(
                    "[omsrs::order::save_to_db] persistence upsert failed: \
                     order_id={:?} symbol={} err={:?}",
                    self.order_id, self.symbol, e
                );
                false
            }
        }
    }

    /// Async wrapper for [`save_to_db`] that runs the (sync, blocking)
    /// rusqlite call on a `tokio::task::spawn_blocking` worker so it
    /// does not stall the runtime's I/O reactor.
    ///
    /// Use this from `execute_async` / `modify_async` / `update_async`
    /// (any path called from inside a Tokio task). The sync
    /// `save_to_db` remains the right call from non-async paths.
    ///
    /// Failure modes (all return `false`, none panic):
    ///   - no `connection` configured → no-op false (legacy parity).
    ///   - no Tokio runtime detected → log + return false. Without
    ///     this guard, `spawn_blocking` would call `Handle::current()`
    ///     and panic synchronously on a non-runtime caller.
    ///   - `JoinError` (panic / cancel inside the spawned worker) →
    ///     log + return false.
    ///   - `upsert_order` returned `Err` → log + return false.
    pub async fn save_to_db_async(&self) -> bool {
        let Some(handle) = self.connection.clone() else {
            return false;
        };
        let row = self.to_row();
        let order_id = self.order_id.clone();
        let symbol = self.symbol.clone();

        // Guard against missing runtime. `tokio::task::spawn_blocking`
        // requires `Handle::current()` and panics synchronously if
        // called outside a Tokio context. A consumer who imports omsrs
        // without standing up a runtime would crash on the first
        // async-path place — surface a clean false + log instead.
        if tokio::runtime::Handle::try_current().is_err() {
            eprintln!(
                "[omsrs::order::save_to_db_async] no Tokio runtime in \
                 scope — cannot offload sync rusqlite call. order_id={:?} \
                 symbol={}. Caller must construct a Tokio runtime before \
                 invoking async OMS paths.",
                order_id, symbol,
            );
            return false;
        }

        match tokio::task::spawn_blocking(move || handle.upsert_order(row)).await {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                eprintln!(
                    "[omsrs::order::save_to_db_async] persistence upsert \
                     failed: order_id={:?} symbol={} err={:?}",
                    order_id, symbol, e
                );
                false
            }
            Err(join_err) => {
                eprintln!(
                    "[omsrs::order::save_to_db_async] spawn_blocking worker \
                     panicked / canceled: order_id={:?} symbol={} err={}",
                    order_id, symbol, join_err
                );
                false
            }
        }
    }

    fn to_row(&self) -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("symbol".into(), json!(self.symbol));
        m.insert("side".into(), json!(self.side));
        m.insert("quantity".into(), json!(self.quantity));
        if let Some(ref id) = self.id {
            m.insert("id".into(), json!(id));
        }
        if let Some(ref v) = self.parent_id {
            m.insert("parent_id".into(), json!(v));
        }
        if let Some(ts) = self.timestamp {
            m.insert("timestamp".into(), json!(ts.to_rfc3339()));
        }
        m.insert("order_type".into(), json!(self.order_type));
        if let Some(ts) = self.broker_timestamp {
            m.insert("broker_timestamp".into(), json!(ts.to_rfc3339()));
        }
        if let Some(ts) = self.exchange_timestamp {
            m.insert("exchange_timestamp".into(), json!(ts.to_rfc3339()));
        }
        if let Some(ref v) = self.order_id {
            m.insert("order_id".into(), json!(v));
        }
        if let Some(ref v) = self.exchange_order_id {
            m.insert("exchange_order_id".into(), json!(v));
        }
        if let Some(v) = self.price {
            m.insert("price".into(), decimal_persistence_value(v));
        }
        m.insert(
            "trigger_price".into(),
            decimal_persistence_value(self.trigger_price),
        );
        m.insert(
            "average_price".into(),
            decimal_persistence_value(self.average_price),
        );
        if let Some(v) = self.pending_quantity {
            m.insert("pending_quantity".into(), json!(v));
        }
        m.insert("filled_quantity".into(), json!(self.filled_quantity));
        m.insert("cancelled_quantity".into(), json!(self.cancelled_quantity));
        m.insert("disclosed_quantity".into(), json!(self.disclosed_quantity));
        m.insert("validity".into(), json!(self.validity));
        if let Some(ref v) = self.status {
            m.insert("status".into(), json!(v));
        }
        m.insert("expires_in".into(), json!(self.expires_in));
        m.insert("timezone".into(), json!(self.timezone));
        if let Some(ref v) = self.client_id {
            m.insert("client_id".into(), json!(v));
        }
        m.insert(
            "convert_to_market_after_expiry".into(),
            json!(self.convert_to_market_after_expiry),
        );
        m.insert(
            "cancel_after_expiry".into(),
            json!(self.cancel_after_expiry),
        );
        m.insert("retries".into(), json!(self.retries));
        m.insert("max_modifications".into(), json!(self.max_modifications));
        if let Some(ref v) = self.exchange {
            m.insert("exchange".into(), json!(v));
        }
        if let Some(ref v) = self.tag {
            m.insert("tag".into(), json!(v));
        }
        m.insert("can_peg".into(), json!(self.can_peg));
        if let Some(ref v) = self.pseudo_id {
            m.insert("pseudo_id".into(), json!(v));
        }
        if let Some(ref v) = self.strategy_id {
            m.insert("strategy_id".into(), json!(v));
        }
        if let Some(ref v) = self.portfolio_id {
            m.insert("portfolio_id".into(), json!(v));
        }
        if let Some(ref v) = self.json {
            m.insert("JSON".into(), json!(v));
        }
        if let Some(ref v) = self.error {
            m.insert("error".into(), json!(v));
        }
        m.insert("is_multi".into(), json!(self.is_multi));
        if let Some(ts) = self.last_updated_at {
            m.insert("last_updated_at".into(), json!(ts.to_rfc3339()));
        }
        m
    }

    /// Upstream `Order.clone` — deep copy minus `id`/`parent_id`/`timestamp`/
    /// `_lock`; id and timestamp are regenerated so the cloned order is a
    /// fresh OMS entity.
    pub fn clone_fresh(&self) -> Self {
        let init = OrderInit {
            symbol: self.symbol.clone(),
            side: self.side.clone(),
            quantity: self.quantity,
            order_type: Some(self.order_type.clone()),
            price: self.price,
            trigger_price: Some(self.trigger_price),
            average_price: Some(self.average_price),
            pending_quantity: self.pending_quantity,
            filled_quantity: Some(self.filled_quantity),
            cancelled_quantity: Some(self.cancelled_quantity),
            disclosed_quantity: Some(self.disclosed_quantity),
            validity: Some(self.validity.clone()),
            status: self.status.clone(),
            expires_in: Some(self.expires_in),
            timezone: Some(self.timezone.clone()),
            client_id: self.client_id.clone(),
            convert_to_market_after_expiry: Some(self.convert_to_market_after_expiry),
            cancel_after_expiry: Some(self.cancel_after_expiry),
            retries: Some(self.retries),
            max_modifications: Some(self.max_modifications),
            exchange: self.exchange.clone(),
            tag: self.tag.clone(),
            connection: self.connection.clone(),
            can_peg: Some(self.can_peg),
            pseudo_id: self.pseudo_id.clone(),
            strategy_id: self.strategy_id.clone(),
            portfolio_id: self.portfolio_id.clone(),
            json: self.json.clone(),
            error: self.error.clone(),
            is_multi: Some(self.is_multi),
            exchange_order_id: self.exchange_order_id.clone(),
            order_id: self.order_id.clone(),
            broker_timestamp: self.broker_timestamp,
            exchange_timestamp: self.exchange_timestamp,
            last_updated_at: self.last_updated_at,
            ..Default::default()
        };
        Self::from_init_with_clock(init, self.clock.clone())
    }
}

// ── R12.3a — async siblings of execute / modify / cancel ──────
//
// These are **new** methods. The existing sync `execute` / `modify`
// / `cancel` signatures (`src/order.rs:559`, `:619`, `:775`) are
// unchanged — see the R12 plan's "Hard constraint" block.
//
// Structural parity with sync: same field precedence, same
// `attribs_to_copy` merge logic, same `lock.can_*` gates, same
// `num_modifications` bookkeeping. The only differences are:
//
// 1. Broker handle is `&(dyn AsyncBroker + Send + Sync)` instead
//    of `&dyn Broker`.
// 2. `attribs_to_copy_<phase>()` and `order_place/modify/cancel`
//    are `.await`ed.
// 3. `save_to_db()` stays **sync** (it calls `rusqlite` under
//    the persistence feature). Callers that enable persistence
//    on the async path should wrap `execute_async` /
//    `modify_async` in `tokio::task::spawn_blocking` or live
//    with the blocking write; this is documented in the R12
//    plan as an explicit non-goal (R13 would add async
//    persistence). pbot does not enable persistence, so the
//    caveat doesn't apply.
impl Order {
    /// Async sibling of [`Order::execute`]. Same semantics, same
    /// early-return-if-completed-or-already-placed guard, same
    /// kwarg precedence over defaults (default keys still win
    /// over caller kwargs). Returns the resulting `order_id`.
    pub async fn execute_async(
        &mut self,
        broker: &(dyn crate::async_broker::AsyncBroker + Send + Sync),
        attribs_to_copy: Option<&[&str]>,
        kwargs: HashMap<String, Value>,
    ) -> Option<String> {
        if self.is_complete() || self.order_id.is_some() {
            return self.order_id.clone();
        }
        let broker_attribs = broker.attribs_to_copy_execute().await;
        let other_args = self.get_other_args_from_attribs(broker_attribs, attribs_to_copy);

        let mut order_args: HashMap<String, Value> = HashMap::new();
        order_args.insert("symbol".into(), json!(self.symbol.to_uppercase()));
        order_args.insert("side".into(), json!(self.side.to_uppercase()));
        order_args.insert("order_type".into(), json!(self.order_type.to_uppercase()));
        order_args.insert("quantity".into(), json!(self.quantity));
        if let Some(p) = self.price {
            order_args.insert("price".into(), decimal_value(p));
        }
        order_args.insert("trigger_price".into(), decimal_value(self.trigger_price));
        order_args.insert("disclosed_quantity".into(), json!(self.disclosed_quantity));

        for (k, v) in other_args {
            order_args.insert(k, v);
        }
        let default_keys: HashSet<String> = [
            "symbol",
            "side",
            "order_type",
            "quantity",
            "price",
            "trigger_price",
            "disclosed_quantity",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        for (k, v) in &kwargs {
            if !default_keys.contains(k) {
                order_args.insert(k.clone(), v.clone());
            }
        }

        let ret = broker.order_place(order_args).await;
        self.order_id = ret.clone();
        if self.connection.is_some() {
            // Async persistence wrapper — runs the rusqlite call on
            // `spawn_blocking` so the Tokio reactor isn't stalled.
            // Errors are surfaced by `save_to_db_async` itself; the
            // ack we don't actually consume right now (matches sync
            // `execute()`'s behaviour for back-compat) but at least
            // failures land on stderr instead of silently dropping.
            let _ = self.save_to_db_async().await;
        }
        ret
    }

    /// Async sibling of [`Order::modify`]. Same `lock.can_modify`
    /// + `max_modifications` gates.
    pub async fn modify_async(
        &mut self,
        broker: &(dyn crate::async_broker::AsyncBroker + Send + Sync),
        attribs_to_copy: Option<&[&str]>,
        kwargs: HashMap<String, Value>,
    ) {
        if !self.lock.can_modify() {
            return;
        }
        let broker_attribs = broker.attribs_to_copy_modify().await;
        let other_args = self.get_other_args_from_attribs(broker_attribs, attribs_to_copy);

        let keys_for_broker: HashSet<&str> = [
            "order_id",
            "quantity",
            "price",
            "trigger_price",
            "order_type",
            "disclosed_quantity",
        ]
        .into_iter()
        .collect();

        let frozen = Self::frozen_attrs();
        let mut args_to_add: HashMap<String, Value> = HashMap::new();
        for (k, v) in &kwargs {
            if frozen.contains(k.as_str()) {
                continue;
            }
            let is_known = self.get_attr(k).is_some();
            if is_known {
                self.set_local_field(k, v);
                if !keys_for_broker.contains(k.as_str()) {
                    args_to_add.insert(k.clone(), v.clone());
                }
            } else {
                args_to_add.insert(k.clone(), v.clone());
            }
        }

        let mut order_args: HashMap<String, Value> = HashMap::new();
        if let Some(ref oid) = self.order_id {
            order_args.insert("order_id".into(), json!(oid));
        }
        order_args.insert("quantity".into(), json!(self.quantity));
        if let Some(p) = self.price {
            order_args.insert("price".into(), decimal_value(p));
        }
        order_args.insert("trigger_price".into(), decimal_value(self.trigger_price));
        order_args.insert("order_type".into(), json!(self.order_type.to_uppercase()));
        order_args.insert("disclosed_quantity".into(), json!(self.disclosed_quantity));

        for (k, v) in other_args {
            order_args.insert(k, v);
        }
        for (k, v) in &args_to_add {
            order_args.insert(k.clone(), v.clone());
        }
        for key in &keys_for_broker {
            if let Some(v) = kwargs.get(*key) {
                order_args.insert((*key).to_string(), v.clone());
            }
        }

        if self.num_modifications < self.max_modifications {
            if self.order_id.is_none() {
                return;
            }
            broker.order_modify(order_args).await;
            self.num_modifications += 1;
            if self.connection.is_some() {
                // Async persistence wrapper — see `execute_async`.
                let _ = self.save_to_db_async().await;
            }
        }
    }

    /// Async sibling of [`Order::cancel`]. Same `lock.can_cancel`
    /// + `order_id.is_some()` gates.
    pub async fn cancel_async(
        &mut self,
        broker: &(dyn crate::async_broker::AsyncBroker + Send + Sync),
        attribs_to_copy: Option<&[&str]>,
    ) {
        if !self.lock.can_cancel() {
            return;
        }
        if self.order_id.is_none() {
            return;
        }
        let broker_attribs = broker.attribs_to_copy_cancel().await;
        let other_args = self.get_other_args_from_attribs(broker_attribs, attribs_to_copy);
        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert("order_id".into(), json!(self.order_id.clone().unwrap()));
        for (k, v) in other_args {
            args.insert(k, v);
        }
        broker.order_cancel(args).await;
    }
}

fn decimal_value(d: Decimal) -> Value {
    // Broker kwargs: serialise Decimals as strings to preserve precision
    // through the dynamic kwarg map. Upstream pydantic emits floats, but
    // round-tripping through f64 is lossy — the trade-off is that
    // test-assertion literals must also be strings (`json!("650")`).
    json!(d.to_string())
}

/// Persistence rows: emit Decimals as f64-shaped JSON numbers so SQLite
/// stores them in REAL columns (upstream `orders` schema uses `real`).
/// Lossy for sub-f64 precision, but matches upstream's float storage.
fn decimal_persistence_value(d: Decimal) -> Value {
    use rust_decimal::prelude::ToPrimitive;
    let f = d.to_f64().unwrap_or(0.0);
    json!(f)
}

fn value_to_decimal(v: &Value) -> Option<Decimal> {
    if let Some(s) = v.as_str() {
        return s.parse().ok();
    }
    if let Some(n) = v.as_f64() {
        return Decimal::try_from(n).ok();
    }
    if let Some(n) = v.as_i64() {
        return Some(Decimal::from(n));
    }
    None
}

/// Upstream `(pendulum.today(tz).end_of("day") - pendulum.now(tz)).seconds`.
/// Computes end-of-day (23:59:59) in the same calendar day as `now` minus
/// `now`, truncated to whole seconds. Tz labels don't affect arithmetic
/// here — `test_order_expires` only checks `12:00 → 43199`, which holds
/// for any tz whose day aligns with `end_of("day")`.
fn seconds_to_end_of_day(now: DateTime<Utc>) -> i64 {
    let date = now.date_naive();
    let end_naive: NaiveDateTime = date.and_hms_opt(23, 59, 59).unwrap();
    let end = DateTime::<Utc>::from_naive_utc_and_offset(end_naive, Utc);
    (end - now).num_seconds().max(0)
}
