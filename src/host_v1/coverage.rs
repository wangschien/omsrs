//! Ephemeral coverage arithmetic over canonical member facts.

use std::collections::{BTreeMap, BTreeSet};

use crate::lifecycle::{ClientOrderId, VenueOrderId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CoverKey<'a> {
    Wire(&'a VenueOrderId),
    Client(&'a ClientOrderId),
}

/// Borrowed member facts, not a persistent order projection. Wire identity must
/// include a pending amend's old/new association; None means an independent
/// unbound order. Quantity is None for a member outside the selected live,
/// reducing set. Callers derive these facts from the canonical owner.
#[derive(Debug, Clone, Copy)]
pub struct CoverageMember<'a> {
    pub client: &'a ClientOrderId,
    pub wire: Option<&'a VenueOrderId>,
    pub quantity: Option<u64>,
    pub unresolvable: bool,
    pub cancelling: bool,
    pub superseded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageError {
    DuplicateClient,
    QuantityOverflow,
}

/// Three distinct bounds: wire-minimum lower cover, per-row admission upper
/// cover, and wire-maximum retained cover. An unresolvable selected member
/// contributes zero to LOWER and makes both UPPER bounds unavailable.
#[derive(Debug)]
pub struct CoverageBounds<'a> {
    keys: BTreeMap<&'a ClientOrderId, CoverKey<'a>>,
    lower_by_wire: BTreeMap<CoverKey<'a>, u64>,
    lower: u64,
    upper_rows: Option<u64>,
    upper_wires: Option<u64>,
}

impl<'a> CoverageBounds<'a> {
    pub fn from_members(
        members: impl IntoIterator<Item = CoverageMember<'a>>,
    ) -> Result<Self, CoverageError> {
        let mut keys = BTreeMap::new();
        let mut lower_by_wire = BTreeMap::<_, u64>::new();
        let mut upper_by_wire = BTreeMap::<_, u64>::new();
        let mut upper_rows = 0u64;
        let mut unresolvable = false;
        for member in members {
            let key = member
                .wire
                .map(CoverKey::Wire)
                .unwrap_or(CoverKey::Client(member.client));
            if keys.insert(member.client, key).is_some() {
                return Err(CoverageError::DuplicateClient);
            }
            let Some(quantity) = member.quantity else {
                continue;
            };
            unresolvable |= member.unresolvable;
            upper_rows = upper_rows
                .checked_add(quantity)
                .ok_or(CoverageError::QuantityOverflow)?;
            upper_by_wire
                .entry(key)
                .and_modify(|value| *value = (*value).max(quantity))
                .or_insert(quantity);
            if member.superseded {
                continue;
            }
            let quantity = if member.unresolvable || member.cancelling {
                0
            } else {
                quantity
            };
            lower_by_wire
                .entry(key)
                .and_modify(|value| *value = (*value).min(quantity))
                .or_insert(quantity);
        }
        let total = |values: &BTreeMap<CoverKey<'a>, u64>| {
            values
                .values()
                .try_fold(0u64, |sum, quantity| sum.checked_add(*quantity))
                .ok_or(CoverageError::QuantityOverflow)
        };
        let lower = total(&lower_by_wire)?;
        let upper_wires = total(&upper_by_wire)?;
        Ok(Self {
            keys,
            lower_by_wire,
            lower,
            upper_rows: (!unresolvable).then_some(upper_rows),
            upper_wires: (!unresolvable).then_some(upper_wires),
        })
    }

    pub fn lower(&self) -> u64 {
        self.lower
    }

    pub fn upper_rows(&self) -> Option<u64> {
        self.upper_rows
    }

    pub fn upper_wires(&self) -> Option<u64> {
        self.upper_wires
    }

    /// Remove each contributing wire once, preserving the caller's cancel set.
    /// Duplicate members/cancels never remove a wire's lower cover twice.
    pub fn cancelled_lower<'q>(&self, clients: impl IntoIterator<Item = &'q ClientOrderId>) -> u64 {
        let mut seen = BTreeSet::new();
        let mut total = 0;
        for client in clients {
            if let Some(key) = self.keys.get(client) {
                if seen.insert(*key) {
                    if let Some(quantity) = self.lower_by_wire.get(key) {
                        // This distinct subset cannot exceed the checked total.
                        total += quantity;
                    }
                }
            }
        }
        total
    }
}
