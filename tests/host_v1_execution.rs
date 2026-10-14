use omsrs::execution::{
    ExecutionEvidence, ExecutionScope, ExecutionSource, InventoryDisposition, LedgerId,
};
use omsrs::host_v1::risk::{
    AdmissionAction, AdmissionFacts, AdmissionReasons, AuthorityReason, GuardReason, NetTrust,
};
use omsrs::host_v1::{HostError, OrderBook};
use omsrs::lifecycle::{ClientOrderId, FillId, Side};

fn ledger() -> LedgerId {
    LedgerId {
        scope: ExecutionScope("scope-a".into()),
        market: "market-a".into(),
    }
}
fn evidence(fee: Option<u64>) -> ExecutionEvidence {
    ExecutionEvidence {
        scope: ledger().scope,
        market: "market-a".into(),
        execution_id: FillId("fill-a".into()),
        side: Side::BuyYes,
        qty: 100,
        price_cents: 40,
        ts_ns: 1,
        venue_order_id: None,
        claimed_client_order_id: None,
        fee_cents: fee,
        source: ExecutionSource::WsFill,
    }
}

#[test]
fn canonical_execution_duplicate_and_fee_upgrade_do_not_rebook_quantity() {
    let book = OrderBook::new(ledger());
    let transaction = book.prepare_execution(None, &evidence(None)).unwrap();
    assert!(matches!(
        transaction.outcomes[0].inventory,
        InventoryDisposition::Ingested(_)
    ));
    assert_eq!(book.inventory().net(), 0);
    let mut records = vec![transaction.journal.unwrap()];
    let book = OrderBook::from_committed_records(ledger(), &records).unwrap();
    assert_eq!(book.inventory().net(), 100);
    let duplicate = book.prepare_execution(None, &evidence(None)).unwrap();
    assert_eq!(
        duplicate.outcomes[0].inventory,
        InventoryDisposition::Duplicate
    );
    let upgrade = book.prepare_execution(None, &evidence(Some(3))).unwrap();
    assert_eq!(
        upgrade.outcomes[0].inventory,
        InventoryDisposition::Upgraded
    );
    records.push(upgrade.journal.unwrap());
    let book = OrderBook::from_committed_records(ledger(), &records).unwrap();
    assert_eq!(book.inventory().net(), 100);
    assert_eq!(
        book.inventory()
            .get(&FillId("fill-a".into()))
            .unwrap()
            .fee_cents,
        Some(3)
    );
    assert_eq!(book.inventory().view().executions, 1);
}

#[test]
fn explicit_missing_owner_is_rejected_instead_of_falling_back_to_unowned_inventory() {
    let book = OrderBook::new(ledger());
    let cid = ClientOrderId("absent".into());
    assert!(
        matches!(book.prepare_execution(Some(&cid), &evidence(None)),
        Err(HostError::MissingClient(found)) if found == cid)
    );
    assert_eq!(book.inventory().net(), 0);
}

#[test]
fn independent_risk_reasons_intersect_and_exit_cancel_requires_separate_authority() {
    use AdmissionAction::*;
    let facts = AdmissionFacts {
        net_trusted: NetTrust::Trusted {
            reconciled_at_ns: 7,
        },
        before_pause: true,
    };
    let mut reasons = AdmissionReasons::default();
    assert!(reasons.admits(New, facts));
    assert!(!reasons.admits(CancelExit, facts));
    reasons.guards.insert(GuardReason::DailyLoss);
    assert!(!reasons.admits(New, facts));
    assert!(reasons.admits(ExitReduce, facts));
    reasons.authority.insert(AuthorityReason::AuthorityHold);
    assert!(!reasons.admits(ExitReduce, facts));
    assert!(reasons.admits(ExitKeep, facts));
    reasons.authority.clear();
    assert!(!reasons.admits(New, facts));
    reasons.guards.insert(GuardReason::IdentityUnverified);
    for action in [New, CancelEntry, CancelExit, ExitKeep, ExitReduce] {
        assert!(!reasons.admits(action, facts));
    }
}

#[test]
fn risk_reason_matrix_preserves_trust_and_pause_boundaries() {
    use AdmissionAction::*;
    use GuardReason::*;
    let actions = [New, CancelEntry, CancelExit, ExitKeep, ExitReduce];
    for trusted in [false, true] {
        for before_pause in [false, true] {
            let facts = AdmissionFacts {
                net_trusted: if trusted {
                    NetTrust::Trusted {
                        reconciled_at_ns: 1,
                    }
                } else {
                    NetTrust::Untrusted
                },
                before_pause,
            };
            for reason in [
                DailyLoss,
                DailyLossUnprovable,
                WindowStop,
                MtmInvalid,
                StatusUnproven,
                StatusUnknown,
                TradingPaused,
                ExchangePaused,
                ScheduleClosed,
                ScheduleUnknown,
                FeeUnverified,
                InvalidBook,
                LimitsUnverified,
                IdentityUnverified,
            ] {
                let expected = match reason {
                    IdentityUnverified => [false, false, false, false, false],
                    ExchangePaused => [false, false, false, true, false],
                    TradingPaused | InvalidBook | ScheduleUnknown => {
                        [false, true, false, true, false]
                    }
                    ScheduleClosed => [false, true, false, true, before_pause],
                    _ => [false, true, false, true, trusted],
                };
                let mut reasons = AdmissionReasons::default();
                reasons.guards.insert(reason);
                for (action, allowed) in actions.into_iter().zip(expected) {
                    assert_eq!(
                        reasons.admits(action, facts),
                        allowed,
                        "{reason:?} {action:?} {facts:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn incomplete_book_observation_preserves_hold_and_counts_complete_invalid_updates() {
    use omsrs::host_v1::risk::{BookGuard, InvalidBook};
    let mut guard = BookGuard::default();
    guard.observe_class(Some(Some(InvalidBook::Locked)));
    guard.observe_class(None);
    assert_eq!(guard.current, Some(InvalidBook::Locked));
    assert!(!guard.healthy_seen);
    assert_eq!((guard.episodes, guard.invalid_updates), (1, 1));
    guard.observe_class(Some(Some(InvalidBook::Crossed)));
    assert_eq!((guard.episodes, guard.invalid_updates), (1, 2));
    guard.observe_class(Some(None));
    assert!(guard.healthy_seen);
    assert_eq!(guard.current, None);
    guard.observe_class(Some(Some(InvalidBook::Crossed)));
    assert_eq!((guard.episodes, guard.invalid_updates), (2, 3));
    guard.episodes = u64::MAX;
    guard.invalid_updates = u64::MAX;
    guard.observe_class(Some(None));
    guard.observe_class(Some(Some(InvalidBook::Locked)));
    assert_eq!(
        (guard.episodes, guard.invalid_updates),
        (u64::MAX, u64::MAX)
    );
}

#[test]
fn status_probe_failures_and_dead_producer_preserve_distinct_pause_reasons() {
    use omsrs::host_v1::risk::{StatusGuard, StatusObservation};
    let mut guard = StatusGuard::default();
    assert_eq!(guard.current(), Some(GuardReason::StatusUnproven));
    guard.completed_probe(StatusObservation::Active);
    assert_eq!(guard.current(), None);
    guard.completed_probe(StatusObservation::Failed);
    assert_eq!(guard.current(), None);
    guard.completed_probe(StatusObservation::Failed);
    assert_eq!(guard.current(), Some(GuardReason::StatusUnknown));
    guard.completed_probe(StatusObservation::TradingPaused);
    guard.completed_probe(StatusObservation::Failed);
    assert_eq!(guard.current(), Some(GuardReason::TradingPaused));
    guard.completed_probe(StatusObservation::ExchangePaused);
    assert_eq!(guard.current(), Some(GuardReason::ExchangePaused));
    guard.completed_probe(StatusObservation::Active);
    guard.completed_probe(StatusObservation::WatchClosed);
    assert_eq!(guard.current(), Some(GuardReason::StatusUnknown));
    for _ in 0..300 {
        guard.completed_probe(StatusObservation::Failed);
    }
    assert_eq!(guard.current(), Some(GuardReason::StatusUnknown));
    guard.completed_probe(StatusObservation::Active);
    assert_eq!(guard.current(), None);
}
