use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::order_book::OrderBook;
use crate::lifecycle::{ClientOrderId, VenueOrderId};

/// Scalar handle only. A local ordinal is not an order authority proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalOrdinal(u64);

impl LocalOrdinal {
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Borrowed canonical indexes for observation and private host serialization.
/// This view borrows the maps and copies the allocation counter.
#[derive(Debug, Clone, Copy)]
pub struct OrdinalView<'a> {
    pub wire_by_local: &'a BTreeMap<LocalOrdinal, VenueOrderId>,
    pub local_by_wire: &'a BTreeMap<String, LocalOrdinal>,
    pub local_by_client: &'a BTreeMap<String, LocalOrdinal>,
    pub client_by_local: &'a BTreeMap<LocalOrdinal, ClientOrderId>,
    pub allocated: u64,
}

impl OrderBook {
    /// Canonical member/lineage lookup. Ambiguity preserves the registry's
    /// `None` result; this is distinct from local ordinal transport lookup.
    pub fn wire_for_client(&self, client: &ClientOrderId) -> Option<&VenueOrderId> {
        self.registry().wire_for_client(self.ledger(), client)
    }
}

/// Process-local identity indexes owned solely by the canonical wire registry.
/// These rows do not establish lifecycle membership or authority.
#[derive(Debug, Default)]
pub(crate) struct OrdinalIdentity {
    wire_by_local: BTreeMap<LocalOrdinal, VenueOrderId>,
    local_by_wire: BTreeMap<String, LocalOrdinal>,
    local_by_client: BTreeMap<String, LocalOrdinal>,
    client_by_local: BTreeMap<LocalOrdinal, ClientOrderId>,
    next: u64,
}

impl OrdinalIdentity {
    pub(crate) fn view(&self) -> OrdinalView<'_> {
        OrdinalView {
            wire_by_local: &self.wire_by_local,
            local_by_wire: &self.local_by_wire,
            local_by_client: &self.local_by_client,
            client_by_local: &self.client_by_local,
            allocated: self.next,
        }
    }

    pub(crate) fn allocate(&mut self) -> LocalOrdinal {
        self.next = self.next.saturating_add(1);
        if self.next == 0 {
            self.next = 1;
        }
        LocalOrdinal::from_raw(self.next)
    }

    pub(crate) fn bind(&mut self, local: LocalOrdinal, wire: VenueOrderId, client: Option<&str>) {
        self.local_by_wire.insert(wire.0.clone(), local);
        self.wire_by_local.insert(local, wire);
        if let Some(client) = client {
            self.local_by_client.insert(client.to_owned(), local);
            self.client_by_local
                .insert(local, ClientOrderId(client.to_owned()));
        }
    }

    pub(crate) fn wire(&self, local: LocalOrdinal) -> Option<&VenueOrderId> {
        self.wire_by_local.get(&local)
    }

    pub(crate) fn local_for_wire(&self, wire: &str) -> Option<LocalOrdinal> {
        self.local_by_wire.get(wire).copied()
    }

    pub(crate) fn local_for_client(&self, client: &str) -> Option<LocalOrdinal> {
        self.local_by_client.get(client).copied()
    }

    pub(crate) fn client(&self, local: LocalOrdinal) -> Option<&ClientOrderId> {
        self.client_by_local.get(&local)
    }

    pub(crate) fn forget(&mut self, local: LocalOrdinal) {
        if let Some(wire) = self.wire_by_local.remove(&local) {
            self.local_by_wire.remove(&wire.0);
        }
        if let Some(client) = self.client_by_local.remove(&local) {
            self.local_by_client.remove(&client.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalOrdinal, OrdinalIdentity};

    #[test]
    fn allocator_starts_at_one_and_repeats_max_when_exhausted() {
        let mut fresh = OrdinalIdentity::default();
        assert_eq!(fresh.allocate(), LocalOrdinal::from_raw(1));
        let mut exhausted = OrdinalIdentity {
            next: u64::MAX - 1,
            ..OrdinalIdentity::default()
        };
        assert_eq!(exhausted.allocate().raw(), u64::MAX);
        assert_eq!(exhausted.allocate().raw(), u64::MAX);
        assert_eq!(exhausted.allocate().raw(), u64::MAX);
    }
}
