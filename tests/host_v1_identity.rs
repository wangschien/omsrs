use omsrs::execution::{BatchOutcome, ExecutionScope, LedgerId, OrderTxnRecord};
use omsrs::host_v1::{identity::LocalOrdinal, OrderBook};
use omsrs::lifecycle::{ClientOrderId, JournalRecord, OrderCtx, OrderState, Side, VenueOrderId};

fn ledger() -> LedgerId {
    LedgerId {
        scope: ExecutionScope("scope-a".into()),
        market: "market-a".into(),
    }
}
fn bound(wire: &str, seq: u64) -> JournalRecord {
    let cid = ClientOrderId("client-a".into());
    let wire = VenueOrderId(wire.into());
    let mut ctx = OrderCtx::new(cid.clone(), "market-a", "group-a", Side::BuyYes, 40, 100);
    ctx.venue_order_id = Some(wire.clone());
    JournalRecord::OrderTxn(Box::new(OrderTxnRecord {
        ledger: ledger(),
        client_order_id: cid.clone(),
        seq,
        event: "SubmitResponse".into(),
        outcome: BatchOutcome::Accept,
        core_records: vec![JournalRecord::VenueBoundCid {
            client_order_id: cid,
            venue_order_id: wire.clone(),
        }],
        state_after: OrderState::Accepted {
            venue_order_id: wire,
        },
        ctx_after: ctx,
    }))
}

#[test]
fn canonical_lookup_preserves_ambiguous_and_missing_results() {
    let book =
        OrderBook::from_committed_records(ledger(), &[bound("wire-a", 1), bound("wire-b", 2)])
            .unwrap();
    assert!(book
        .wire_for_client(&ClientOrderId("client-a".into()))
        .is_none());
    assert!(book
        .wire_for_client(&ClientOrderId("absent".into()))
        .is_none());
    assert_eq!(book.view().orders, 1);
}

#[test]
fn wire_borrow_is_bound_to_aggregate_rather_than_temporary_lookup_key() {
    let book = OrderBook::from_committed_records(ledger(), &[bound("wire-a", 1)]).unwrap();
    let borrowed = {
        let temporary_key = ClientOrderId("client-a".into());
        book.wire_for_client(&temporary_key).unwrap()
    };
    assert_eq!(borrowed.0, "wire-a");
}

#[test]
fn ordinal_scalar_roundtrips_zero_and_exhaustion_without_authority() {
    for raw in [0, 1, u64::MAX - 1, u64::MAX] {
        assert_eq!(LocalOrdinal::from_raw(raw).raw(), raw);
        assert_eq!(
            serde_json::to_string(&LocalOrdinal::from_raw(raw)).unwrap(),
            raw.to_string()
        );
    }
}

#[test]
fn aggregate_ordinal_binding_borrows_stored_ids_without_authority() {
    let mut book = OrderBook::new(ledger());
    let cid = ClientOrderId("client-a".into());
    book.insert_initial(OrderCtx::new(
        cid.clone(),
        "market-a",
        "group-a",
        Side::BuyYes,
        40,
        100,
    ))
    .unwrap();
    let local = book.allocate_ordinal();
    assert_eq!(local.raw(), 1);
    {
        let temporary_client = String::from("client-a");
        book.bind_ordinal(
            local,
            VenueOrderId("wire-a".into()),
            Some(&temporary_client),
        );
    }
    let borrowed_wire = {
        let temporary_key = String::from("wire-a");
        book.wire_for_ordinal(book.ordinal_for_wire(&temporary_key).unwrap())
            .unwrap()
    };
    let borrowed_client = {
        let temporary_key = String::from("client-a");
        book.client_for_ordinal(book.ordinal_for_client(&temporary_key).unwrap())
            .unwrap()
    };
    assert_eq!(borrowed_wire.0, "wire-a");
    assert_eq!(borrowed_client.0, "client-a");
    let identities = book.ordinal_view();
    assert_eq!(identities.allocated, 1);
    assert_eq!(identities.wire_by_local.len(), 1);
    assert_eq!(identities.client_by_local.len(), 1);
    assert!(std::ptr::eq(
        identities.wire_by_local.get(&local).unwrap(),
        borrowed_wire
    ));
    assert!(std::ptr::eq(
        identities.client_by_local.get(&local).unwrap(),
        borrowed_client
    ));
    assert!(book.wire_for_client(&cid).is_none());
    assert!(book.registry().get(book.ledger(), borrowed_wire).is_none());
    let row = book.order(&cid).unwrap();
    assert_eq!(row.state, &OrderState::New);
    assert!(row.ctx.venue_order_id.is_none());
    assert!(!row.ctx.authority_is_fresh());
    assert_eq!(book.inventory().net(), 0);
}

#[test]
fn ordinal_rebinding_keeps_parent_stale_reverse_rows_and_gc_behavior() {
    let mut book = OrderBook::new(ledger());
    let local = book.allocate_ordinal();
    book.bind_ordinal(local, VenueOrderId("wire-old".into()), Some("client-old"));
    book.bind_ordinal(local, VenueOrderId("wire-new".into()), Some("client-new"));
    assert_eq!(book.ordinal_for_wire("wire-old"), Some(local));
    assert_eq!(book.ordinal_for_client("client-old"), Some(local));
    assert_eq!(book.wire_for_ordinal(local).unwrap().0, "wire-new");
    assert_eq!(book.client_for_ordinal(local).unwrap().0, "client-new");
    book.forget_ordinal(local);
    assert!(book.wire_for_ordinal(local).is_none());
    assert!(book.client_for_ordinal(local).is_none());
    assert!(book.ordinal_for_wire("wire-new").is_none());
    assert!(book.ordinal_for_client("client-new").is_none());
    assert_eq!(book.ordinal_for_wire("wire-old"), Some(local));
    assert_eq!(book.ordinal_for_client("client-old"), Some(local));
}

#[test]
fn forgetting_old_local_removes_new_reverse_owner_unconditionally() {
    let mut book = OrderBook::new(ledger());
    let old = book.allocate_ordinal();
    let new = book.allocate_ordinal();
    book.bind_ordinal(old, VenueOrderId("wire-a".into()), Some("client-a"));
    book.bind_ordinal(new, VenueOrderId("wire-a".into()), Some("client-a"));
    assert_eq!(book.ordinal_for_wire("wire-a"), Some(new));
    assert_eq!(book.ordinal_for_client("client-a"), Some(new));
    book.forget_ordinal(old);
    assert_eq!(book.ordinal_for_wire("wire-a"), None);
    assert_eq!(book.ordinal_for_client("client-a"), None);
    assert_eq!(book.wire_for_ordinal(new).unwrap().0, "wire-a");
    assert_eq!(book.client_for_ordinal(new).unwrap().0, "client-a");
}

#[test]
fn clientless_recovery_binding_preserves_existing_client_rows() {
    let mut book = OrderBook::new(ledger());
    let local = book.allocate_ordinal();
    book.bind_ordinal(local, VenueOrderId("wire-old".into()), Some("client-a"));
    book.bind_ordinal(local, VenueOrderId("wire-recovery".into()), None);
    assert_eq!(book.wire_for_ordinal(local).unwrap().0, "wire-recovery");
    assert_eq!(book.client_for_ordinal(local).unwrap().0, "client-a");
    assert_eq!(book.ordinal_for_client("client-a"), Some(local));
    assert_eq!(book.ordinal_for_wire("wire-old"), Some(local));
    assert!(book
        .wire_for_client(&ClientOrderId("client-a".into()))
        .is_none());
}

#[test]
fn forget_unknown_and_trimmed_wire_preserves_parent_noop_behavior() {
    let mut book = OrderBook::new(ledger());
    let local = book.allocate_ordinal();
    book.bind_ordinal(local, VenueOrderId("wire-a".into()), Some("client-a"));
    book.forget_ordinal(LocalOrdinal::from_raw(u64::MAX));
    book.forget_wire_if_known("   ");
    book.forget_wire_if_known("missing");
    assert_eq!(book.ordinal_for_wire("wire-a"), Some(local));
    book.forget_wire_if_known("  wire-a  ");
    assert_eq!(book.ordinal_for_wire("wire-a"), None);
    assert_eq!(book.ordinal_for_client("client-a"), None);
    assert!(book.wire_for_ordinal(local).is_none());
    assert!(book.client_for_ordinal(local).is_none());
}
