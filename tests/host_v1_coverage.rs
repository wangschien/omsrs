use omsrs::host_v1::coverage::{CoverageBounds, CoverageError, CoverageMember};
use omsrs::lifecycle::{ClientOrderId, VenueOrderId};

fn member<'a>(
    client: &'a ClientOrderId,
    wire: Option<&'a VenueOrderId>,
    qty: u64,
) -> CoverageMember<'a> {
    CoverageMember {
        client,
        wire,
        quantity: Some(qty),
        unresolvable: false,
        cancelling: false,
        superseded: false,
    }
}

#[test]
fn lower_minimum_row_upper_and_wire_upper_keep_distinct_meanings() {
    let (old, new, extra) = (
        ClientOrderId("old".into()),
        ClientOrderId("new".into()),
        ClientOrderId("extra".into()),
    );
    let (wire, other) = (VenueOrderId("wire-a".into()), VenueOrderId("wire-b".into()));
    let bounds = CoverageBounds::from_members([
        member(&old, Some(&wire), 70),
        member(&new, Some(&wire), 90),
        member(&extra, Some(&other), 30),
    ])
    .unwrap();
    assert_eq!(bounds.lower(), 100);
    assert_eq!(bounds.upper_rows(), Some(190));
    assert_eq!(bounds.upper_wires(), Some(120));
    assert_eq!(bounds.cancelled_lower([&old, &new, &old]), 70);
    assert_eq!(bounds.cancelled_lower([&old, &extra]), 100);
}

#[test]
fn pending_cancel_zeros_the_entire_wire_lower_while_uppers_retain_every_leg() {
    let (old, new) = (ClientOrderId("old".into()), ClientOrderId("new".into()));
    let wire = VenueOrderId("wire-a".into());
    let mut pending = member(&old, Some(&wire), 70);
    pending.cancelling = true;
    let bounds = CoverageBounds::from_members([pending, member(&new, Some(&wire), 90)]).unwrap();
    assert_eq!(bounds.lower(), 0);
    assert_eq!(bounds.upper_rows(), Some(160));
    assert_eq!(bounds.upper_wires(), Some(90));
    assert_eq!(bounds.cancelled_lower([&new]), 0);
}

#[test]
fn applied_superseded_member_is_excluded_only_from_lower_cover() {
    let (old, new) = (ClientOrderId("old".into()), ClientOrderId("new".into()));
    let wire = VenueOrderId("wire-a".into());
    let mut superseded = member(&old, Some(&wire), 70);
    superseded.superseded = true;
    superseded.cancelling = true;
    let bounds = CoverageBounds::from_members([superseded, member(&new, Some(&wire), 90)]).unwrap();
    assert_eq!(bounds.lower(), 90);
    assert_eq!(bounds.upper_rows(), Some(160));
    assert_eq!(bounds.upper_wires(), Some(90));
}

#[test]
fn unresolvable_member_makes_upper_unavailable_and_lower_conservative() {
    let (old, new) = (ClientOrderId("old".into()), ClientOrderId("new".into()));
    let wire = VenueOrderId("wire-a".into());
    let mut unresolved = member(&old, Some(&wire), 70);
    unresolved.unresolvable = true;
    let bounds = CoverageBounds::from_members([unresolved, member(&new, Some(&wire), 90)]).unwrap();
    assert_eq!(bounds.lower(), 0);
    assert_eq!(bounds.upper_rows(), None);
    assert_eq!(bounds.upper_wires(), None);
}

#[test]
fn client_and_wire_keys_never_collide_and_unbound_orders_remain_distinct() {
    let (a, b) = (ClientOrderId("same".into()), ClientOrderId("other".into()));
    let wire = VenueOrderId("same".into());
    let bounds =
        CoverageBounds::from_members([member(&a, None, 70), member(&b, Some(&wire), 90)]).unwrap();
    assert_eq!(bounds.lower(), 160);
    assert_eq!(bounds.upper_wires(), Some(160));
    assert_eq!(bounds.cancelled_lower([&a]), 70);
    assert_eq!(
        bounds.cancelled_lower([&ClientOrderId("missing".into())]),
        0
    );
}

#[test]
fn omitted_quantity_keeps_wire_identity_without_adding_cover_or_blocking_upper() {
    let (old, new) = (ClientOrderId("old".into()), ClientOrderId("new".into()));
    let wire = VenueOrderId("wire-a".into());
    let mut excluded = member(&old, Some(&wire), 70);
    excluded.quantity = None;
    excluded.unresolvable = true;
    let bounds = CoverageBounds::from_members([excluded, member(&new, Some(&wire), 90)]).unwrap();
    assert_eq!(bounds.lower(), 90);
    assert_eq!(bounds.upper_rows(), Some(90));
    assert_eq!(bounds.upper_wires(), Some(90));
    assert_eq!(bounds.cancelled_lower([&old, &new]), 90);
}

#[test]
fn duplicate_members_and_overflow_are_explicit_errors() {
    let (a, b) = (ClientOrderId("a".into()), ClientOrderId("b".into()));
    assert!(matches!(
        CoverageBounds::from_members([member(&a, None, 1), member(&a, None, 2)]),
        Err(CoverageError::DuplicateClient)
    ));
    assert!(matches!(
        CoverageBounds::from_members([member(&a, None, u64::MAX), member(&b, None, 1)]),
        Err(CoverageError::QuantityOverflow)
    ));
}
