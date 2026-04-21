//! `MockBroker` — a minimal `unittest.mock.MagicMock`-shaped stand-in that
//! records every `order_place` / `order_modify` / `order_cancel` call made
//! through it. Tests read the call history back via `call_args_list()` and
//! friends, mirroring upstream's `broker.order_place.call_args_list[0]`.

use std::collections::HashMap;
use std::sync::Mutex;

use omsrs::Broker;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct MockBroker {
    place_calls: Mutex<Vec<HashMap<String, Value>>>,
    modify_calls: Mutex<Vec<HashMap<String, Value>>>,
    cancel_calls: Mutex<Vec<HashMap<String, Value>>>,
    place_returns: Mutex<Vec<Option<String>>>,
    attrs_execute: Mutex<Option<Vec<String>>>,
    attrs_modify: Mutex<Option<Vec<String>>>,
    attrs_cancel: Mutex<Option<Vec<String>>>,
}

impl MockBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_place_return(&self, v: Option<String>) {
        self.place_returns.lock().unwrap().push(v);
    }

    pub fn set_place_side_effect(&self, vals: Vec<Option<String>>) {
        let mut guard = self.place_returns.lock().unwrap();
        guard.clear();
        guard.extend(vals);
    }

    pub fn set_attribs_to_copy_execute(&self, v: Option<Vec<String>>) {
        *self.attrs_execute.lock().unwrap() = v;
    }

    pub fn set_attribs_to_copy_modify(&self, v: Option<Vec<String>>) {
        *self.attrs_modify.lock().unwrap() = v;
    }

    pub fn set_attribs_to_copy_cancel(&self, v: Option<Vec<String>>) {
        *self.attrs_cancel.lock().unwrap() = v;
    }

    pub fn place_calls(&self) -> Vec<HashMap<String, Value>> {
        self.place_calls.lock().unwrap().clone()
    }

    pub fn modify_calls(&self) -> Vec<HashMap<String, Value>> {
        self.modify_calls.lock().unwrap().clone()
    }

    pub fn cancel_calls(&self) -> Vec<HashMap<String, Value>> {
        self.cancel_calls.lock().unwrap().clone()
    }

    pub fn place_call_count(&self) -> usize {
        self.place_calls.lock().unwrap().len()
    }

    pub fn modify_call_count(&self) -> usize {
        self.modify_calls.lock().unwrap().len()
    }

    pub fn cancel_call_count(&self) -> usize {
        self.cancel_calls.lock().unwrap().len()
    }
}

impl Broker for MockBroker {
    fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        self.place_calls.lock().unwrap().push(args);
        // Drain-front semantics: `side_effect = range(100000, 100010)` pops
        // the first queued return on each call. If the queue is empty, fall
        // back to a fresh stringified auto-id so tests that don't care
        // about the return still see something non-None.
        let mut guard = self.place_returns.lock().unwrap();
        if guard.is_empty() {
            Some(format!(
                "MOCK-{}",
                self.place_calls.lock().unwrap().len()
            ))
        } else {
            guard.remove(0)
        }
    }

    fn order_modify(&self, args: HashMap<String, Value>) {
        self.modify_calls.lock().unwrap().push(args);
    }

    fn order_cancel(&self, args: HashMap<String, Value>) {
        self.cancel_calls.lock().unwrap().push(args);
    }

    fn attribs_to_copy_execute(&self) -> Option<Vec<String>> {
        self.attrs_execute.lock().unwrap().clone()
    }

    fn attribs_to_copy_modify(&self) -> Option<Vec<String>> {
        self.attrs_modify.lock().unwrap().clone()
    }

    fn attribs_to_copy_cancel(&self) -> Option<Vec<String>> {
        self.attrs_cancel.lock().unwrap().clone()
    }
}
