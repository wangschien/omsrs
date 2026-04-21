//! Broker trait — the abstract interface `Order::execute/modify/cancel` call
//! into (PORT-PLAN §2, §13). `Broker` objects know nothing about `Order`
//! semantics; they receive flat kwarg maps and return broker-assigned ids.
//!
//! Upstream uses Python's `**kwargs` everywhere, so the trait's args are
//! `HashMap<String, serde_json::Value>` rather than typed structs. That keeps
//! the parity tests (which assert arbitrary kwarg contents) readable.

use std::collections::HashMap;

use serde_json::Value;

pub trait Broker: Send + Sync {
    fn order_place(&self, args: HashMap<String, Value>) -> Option<String>;
    fn order_modify(&self, args: HashMap<String, Value>);
    fn order_cancel(&self, args: HashMap<String, Value>);

    /// Attribute-name lists the broker wants copied onto `order_place` /
    /// `order_modify` / `order_cancel` calls. Default-empty mirrors
    /// upstream: `hasattr(broker, name)` is `False` ⇒ nothing copied.
    fn attribs_to_copy_execute(&self) -> Option<Vec<String>> {
        None
    }
    fn attribs_to_copy_modify(&self) -> Option<Vec<String>> {
        None
    }
    fn attribs_to_copy_cancel(&self) -> Option<Vec<String>> {
        None
    }
}
