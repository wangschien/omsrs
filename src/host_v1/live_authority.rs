//! Current authority tokens and unresolved-lineage projections.
use super::journal_projection::held_terminal_identity;
use super::live_admission::AdmissionClient;
use super::live_state::LiveOmsBookState;
use crate::execution::{InventoryHealth, OrderRef};
use crate::lifecycle::{ClientOrderId as OmsClientOrderId, OrderState, VenueOrderId};
use crate::observation::{AmendPhase, Readiness};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityReadToken {
    pub wire: VenueOrderId,
    pub generation: u64,
    pub current_cid: Option<OmsClientOrderId>,
    pub inventory_generation: u64,
    pub authority_hold_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UaLineageRef {
    pub new_cid: String,
    pub old_cid: String,
    pub wire: String,
    pub exit: bool,
}

impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn authority_read_token(&self, wire: &VenueOrderId) -> Option<AuthorityReadToken> {
        let ws = self.public_fields.registry.get(&self.ledger, wire)?;
        let current_cid = ws
            .members
            .iter()
            .find(|m| m.role == crate::observation::MemberRole::Current)
            .map(|m| m.cid.clone())
            .or_else(|| {
                if ws.authority_hold {
                    ws.proved_current_cid
                        .as_ref()
                        .filter(|cid| {
                            ws.members.iter().any(|m| {
                                &m.cid == *cid && m.role == crate::observation::MemberRole::Terminal
                            })
                        })
                        .cloned()
                } else {
                    None
                }
            })
            .or_else(|| held_terminal_identity(ws).cloned());
        Some(AuthorityReadToken {
            wire: wire.clone(),
            generation: ws.generation,
            current_cid,
            inventory_generation: self.public_fields.inventory.generation(),
            authority_hold_revision: ws.authority_hold_revision,
        })
    }

    pub fn authority_terminal_identity(&self, wire: &VenueOrderId) -> Option<OmsClientOrderId> {
        self.public_fields
            .registry
            .get(&self.ledger, wire)
            .and_then(|ws| held_terminal_identity(ws).cloned())
    }

    pub fn authority_token_still_current(&self, token: &AuthorityReadToken) -> bool {
        self.authority_read_token(&token.wire).as_ref() == Some(token)
    }

    pub fn not_ready_is_entry_unknown_lineage_only<C: AdmissionClient<R>>(
        &self,
        fill_apply_failed: bool,
    ) -> bool {
        if fill_apply_failed
            || matches!(
                self.public_fields.inventory.view().health,
                InventoryHealth::Conflicted { .. }
            )
            || !self.public_fields.g2_proven
            || self.public_fields.stopped
        {
            return false;
        }
        let is_entry = |cid: &str| C::is_entry_tag(cid);
        let mut found = 0usize;
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        let mut unknown_cids: std::collections::BTreeSet<String> = Default::default();
        for row in self.by_client.values() {
            let Some(wire) = row.ctx.venue_order_id.as_ref() else {
                continue;
            };
            let Some(ws) = self.public_fields.registry.get(&self.ledger, wire) else {
                continue;
            };
            let Some(lineage) = ws.amend_lineage.as_ref() else {
                continue;
            };
            if lineage.phase != AmendPhase::Unknown {
                continue;
            }
            if !is_entry(lineage.old_cid.0.as_str()) || !is_entry(lineage.new_cid.0.as_str()) {
                return false;
            }
            unknown_cids.insert(lineage.old_cid.0.clone());
            unknown_cids.insert(lineage.new_cid.0.clone());
            if seen.insert(wire.0.clone())
                && matches!(ws.kind, crate::observation::WireKind::Scoped)
                && ws.tracked
            {
                found += 1;
            }
        }
        let mut unknown_rows = 0usize;
        for o in self.by_client.values() {
            if matches!(
                o.state,
                OrderState::SubmitUnknown | OrderState::UnknownNoMatch
            ) {
                if !unknown_cids.contains(&o.ctx.client_order_id.0) {
                    return false;
                }
                unknown_rows += 1;
            }
        }
        if found == 0 && unknown_rows == 0 {
            return false;
        }
        let ledger = &self.ledger;
        match self.public_fields.registry.readiness(
            ledger,
            |cid| {
                self.by_client.get(&cid.0).map(|o| OrderRef {
                    ledger,
                    cursor: o.cursor,
                    state: &o.state,
                    ctx: &o.ctx,
                })
            },
            &self.public_fields.inventory,
        ) {
            Ok(Readiness::Ready) => unknown_rows > 0,
            Ok(Readiness::Unresolved {
                member_debt: 0,
                wire_residual: 0,
                unresolved_venue_only: 0,
                tracked_scoped,
            }) => tracked_scoped == found,
            _ => false,
        }
    }

    pub fn has_unresolved_unknown_amend<C: AdmissionClient<R>>(&self) -> bool {
        self.unresolved_unknown_amend_is_exit::<C>().is_some()
    }

    pub fn unresolved_unknown_amend_is_exit<C: AdmissionClient<R>>(&self) -> Option<bool> {
        let all = self.unresolved_unknown_amends::<C>();
        if all.is_empty() {
            None
        } else {
            Some(all.iter().any(|(_, exit)| *exit))
        }
    }

    pub fn unresolved_unknown_amends<C: AdmissionClient<R>>(&self) -> Vec<(String, bool)> {
        self.unresolved_unknown_amend_lineages::<C>()
            .into_iter()
            .map(|l| (l.new_cid, l.exit))
            .collect()
    }

    pub fn unresolved_unknown_amend_lineages<C: AdmissionClient<R>>(&self) -> Vec<UaLineageRef> {
        let mut found = BTreeMap::<String, UaLineageRef>::new();
        for row in self.by_client.values() {
            let Some(wire) = row.ctx.venue_order_id.as_ref() else {
                continue;
            };
            let Some(lineage) = self
                .public_fields
                .registry
                .get(&self.ledger, wire)
                .and_then(|ws| ws.amend_lineage.clone())
            else {
                continue;
            };
            let unresolved = (lineage.phase == AmendPhase::Unknown
                || (lineage.phase == AmendPhase::RestDue
                    && self
                        .by_client
                        .get(&lineage.new_cid.0)
                        .is_some_and(|r| matches!(r.state, OrderState::SubmitUnknown))))
                && [&lineage.old_cid, &lineage.new_cid].iter().any(|cid| {
                    self.by_client.get(&cid.0).is_some_and(|r| {
                        !matches!(
                            r.state,
                            OrderState::Filled | OrderState::Canceled | OrderState::Terminal
                        ) && !r.state.is_submission_closed()
                    })
                });
            if unresolved {
                let exit = !C::is_entry_tag(&lineage.new_cid.0);
                found.insert(
                    lineage.new_cid.0.clone(),
                    UaLineageRef {
                        new_cid: lineage.new_cid.0.clone(),
                        old_cid: lineage.old_cid.0.clone(),
                        wire: wire.0.clone(),
                        exit,
                    },
                );
            }
        }
        found.into_values().collect()
    }

    pub fn note_lagged_unknown_create(&mut self, cid: &str) {
        self.lagged_unknown_creates.retain(|key| {
            self.by_client
                .get(key)
                .is_some_and(|row| matches!(row.state, OrderState::SubmitUnknown))
        });
        if !self
            .public_fields
            .registry
            .has_client(&self.ledger, &OmsClientOrderId(cid.to_string()))
        {
            self.lagged_unknown_creates.insert(cid.to_owned());
        }
    }

    pub fn lagged_unknown_create_keys<C: AdmissionClient<R>>(&self) -> Vec<(String, bool)> {
        self.lagged_unknown_creates
            .iter()
            .filter(|cid| {
                self.by_client
                    .get(*cid)
                    .is_some_and(|row| matches!(row.state, OrderState::SubmitUnknown))
            })
            .map(|cid| (cid.clone(), !C::is_entry_tag(cid)))
            .collect()
    }
}
