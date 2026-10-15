//! Restart reconstruction and journal/evidence conversion over canonical state.
use super::journal_projection::{
    journal_ledger, journal_order_cids, late_fill_after_terminal_cids, reconstruct_pending_wires,
    resting_wire_for_open_projection,
};
use super::live_admission::{AdmissionClient, AdmissionIntent};
use super::live_state::{LiveOmsBookState, LiveOmsOrder};
use crate::execution::{ExecutionEvidence, ExecutionSource, MarketInventory};
use crate::lifecycle::{
    ClientOrderId as OmsClientOrderId, FillRecord, JournalRecord, OrderCtx, OrderEvent, Side,
    VenueOrderId,
};
use crate::observation::{
    AmendLineage, AmendPhase, WireRegistry, WorkingView, rebuild_order_with_observations,
};
use anyhow::Result;
use std::collections::BTreeMap;
impl<R, Stage> LiveOmsBookState<R, Stage> {
    pub fn rebuild_from_journal<C: AdmissionClient<R>>(
        &mut self,
        records: &[JournalRecord],
    ) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let Some(ledger) = journal_ledger(records) else {
            return Ok(());
        };
        anyhow::ensure!(
            ledger.scope.0 == self.public_fields.expected_subaccount.to_string(),
            "journal scope does not match configured subaccount"
        );
        self.ledger = ledger.clone();
        self.public_fields.inventory =
            MarketInventory::rebuild(ledger.clone(), records).map_err(|e| {
                self.public_fields.stopped = true;
                anyhow::anyhow!("inventory rebuild STOP: {e:?}")
            })?;
        self.public_fields.registry = WireRegistry::rebuild(records, &self.public_fields.inventory)
            .map_err(|e| {
                self.public_fields.stopped = true;
                anyhow::anyhow!("registry rebuild STOP: {e:?}")
            })?;
        let mut by_client = BTreeMap::new();
        for cid in journal_order_cids(records) {
            match rebuild_order_with_observations(&ledger, &cid, records) {
                Ok(Some((state, ctx, cursor))) => {
                    let (role, reduce_only) = C::restored_role(&cid.0);
                    by_client.insert(
                        cid.0.clone(),
                        LiveOmsOrder {
                            state,
                            ctx,
                            canceled_gc_seen_ns: None,
                            venue_working_centi: None,
                            venue_initial_centi: None,
                            role,
                            reduce_only,
                            order_age_anchor_ns: None,
                            cursor,
                            cancel_gone: None,
                        },
                    );
                }
                Ok(None) => {}
                Err(e) => {
                    self.public_fields.stopped = true;
                    return Err(anyhow::anyhow!("order rebuild STOP {}: {e:?}", cid.0));
                }
            }
        }
        self.by_client = by_client;
        self.late_after_terminal = late_fill_after_terminal_cids(records);
        self.last_applied_exec_ts_ns = 0;
        for record in records {
            if let JournalRecord::ExecutionTxn(txn) = record {
                self.note_applied_exec_ts(&txn.executions);
            }
        }
        self.staged_user_orders.clear();
        for record in records {
            if let JournalRecord::UserOrderStaged {
                client_order_id,
                evidence,
                ..
            } = record
            {
                if let Some(evidence) = evidence {
                    self.staged_user_orders
                        .insert(client_order_id.0.clone(), evidence.clone());
                } else {
                    self.staged_user_orders.remove(&client_order_id.0);
                }
            }
        }
        self.fold_pos = records.len() as u64;
        self.public_fields.g2_proven = false;
        self.pending_wires = reconstruct_pending_wires(
            records,
            &self.public_fields.registry,
            &self.ledger,
            &self.by_client,
        );
        self.replayed_http_pending = self
            .pending_wires
            .iter()
            .filter(|wire| {
                self.public_fields
                    .registry
                    .get(&self.ledger, wire)
                    .and_then(|state| state.amend_lineage.as_ref())
                    .is_some_and(|lineage| lineage.phase == AmendPhase::HttpPending)
            })
            .cloned()
            .collect();
        Ok(())
    }

    pub fn strip_unresolved_slot_creates<C, I: AdmissionIntent<C>>(&self, intents: &mut Vec<I>) {
        let mut block_buy = false;
        let mut block_sell = false;
        for o in self.by_client.values() {
            if o.state.is_submission_closed() || o.state.is_terminal_or_halt() {
                continue;
            }
            let cid = OmsClientOrderId(o.ctx.client_order_id.as_str().to_string());
            if matches!(
                self.public_fields
                    .registry
                    .member_working_view(&self.ledger, &cid),
                WorkingView::Unresolved
            ) {
                match o.ctx.side {
                    Side::BuyYes => block_buy = true,
                    Side::SellYes => block_sell = true,
                }
            }
        }
        if !block_buy && !block_sell {
            return;
        }
        intents.retain(|i| match i.side() {
            Side::BuyYes => !block_buy,
            Side::SellYes => !block_sell,
        });
    }

    pub fn prepare_amend_lineage_record(
        &self,
        old_cid: &str,
        new_cid: &str,
        wire: &str,
        desired_remaining: u64,
    ) -> Result<JournalRecord> {
        let wire = VenueOrderId(wire.to_string());
        let lineage = AmendLineage {
            old_cid: OmsClientOrderId(old_cid.to_string()),
            new_cid: OmsClientOrderId(new_cid.to_string()),
            pre_generation: self.public_fields.registry.marker(&self.ledger, &wire).0,
            phase: AmendPhase::HttpPending,
            declared_remaining: desired_remaining,
            applied_at_utc_ns: None,
        };
        self.public_fields
            .registry
            .prepare_amend_lineage(&self.ledger, &wire, lineage)
            .map_err(|e| anyhow::anyhow!("amend lineage admission: {e:?}"))
    }

    pub fn fill_evidences(
        &self,
        ctx: &OrderCtx,
        event: &OrderEvent,
        source: ExecutionSource,
    ) -> Vec<ExecutionEvidence> {
        let fills: Vec<FillRecord> = match event {
            OrderEvent::Fill {
                fill_id,
                qty,
                price_cents,
                ts_ns,
                venue_order_id,
                fee_cents,
            } => vec![FillRecord {
                fill_id: fill_id.clone(),
                qty: *qty,
                price_cents: *price_cents,
                ts_ns: *ts_ns,
                venue_order_id: venue_order_id.clone(),
                fee_cents: *fee_cents,
            }],
            OrderEvent::ImmediateFillBackfillResult { fills } => fills.clone(),
            OrderEvent::UnknownBackfillResult { matched, .. } => {
                matched.iter().flat_map(|m| m.fills.clone()).collect()
            }
            OrderEvent::ReconcileResult { fills, .. } => fills.clone(),
            _ => Vec::new(),
        };
        fills
            .into_iter()
            .map(|f| ExecutionEvidence {
                scope: self.ledger.scope.clone(),
                market: ctx.market.clone(),
                execution_id: f.fill_id,
                side: ctx.side,
                qty: f.qty,
                price_cents: f.price_cents,
                ts_ns: f.ts_ns,
                venue_order_id: f.venue_order_id,
                claimed_client_order_id: Some(ctx.client_order_id.clone()),
                fee_cents: f.fee_cents,
                source: source.clone(),
            })
            .collect()
    }

    pub fn claimed_resting(&self, cid: &str) -> bool {
        self.by_client
            .get(cid)
            .is_some_and(|o| resting_wire_for_open_projection(&o.state).is_some())
    }
}
