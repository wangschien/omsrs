use omsrs::host_v1::recovery::{ProtectiveCancelOutcome, RecoveryCadence};
use omsrs::host_v1::risk::{
    classify_position, AuthorityHold, AuthorityReleaseProof, ChannelError, ChannelFault, HoldCause,
    ObservationChannel, PositionFreshness, PositionObservation,
};
use omsrs::lifecycle::VenueOrderId;

fn proof() -> AuthorityReleaseProof {
    AuthorityReleaseProof {
        book_healthy: true,
        position_matches: true,
        orders_match: true,
        lineage_resolved: true,
        reconciled_at_ns: 80,
    }
}

fn acknowledge_both(hold: &mut AuthorityHold) {
    hold.subscribe_started(ObservationChannel::Fill, 101, 20);
    hold.subscribe_started(ObservationChannel::OrderState, 202, 20);
    assert!(hold.observe_ack(Some(ObservationChannel::Fill), Some(101), 301));
    assert!(hold.observe_ack(Some(ObservationChannel::OrderState), None, 302));
}

#[test]
fn channel_ack_and_elapsed_time_never_replace_authority_proof() {
    let mut hold = AuthorityHold::default();
    assert!(hold.begin_generation(10));
    acknowledge_both(&mut hold);
    assert!(hold.channels_healthy());
    assert!(!hold.try_release(proof()));
    hold.observe_position_result(Some(0), 0, 40);
    for index in 0..4 {
        let mut incomplete = proof();
        match index {
            0 => incomplete.book_healthy = false,
            1 => incomplete.position_matches = false,
            2 => incomplete.orders_match = false,
            _ => incomplete.lineage_resolved = false,
        }
        assert!(!hold.try_release(incomplete));
        assert_eq!(hold.trusted_at(), None);
    }
    assert!(hold.try_release(proof()));
    assert_eq!(hold.trusted_at(), Some(80));

    let mut timeout = AuthorityHold::default();
    timeout.begin_generation(0);
    timeout.subscribe_started(ObservationChannel::Fill, 9, 100);
    assert_eq!(timeout.expiring_ack_channels(109, 10), vec![]);
    assert_eq!(
        timeout.expiring_ack_channels(110, 10),
        vec![(ObservationChannel::Fill, 9)]
    );
    assert!(timeout.ack_expired(110, 10));
    assert!(timeout.is_held());
    assert!(!timeout.try_release(proof()));
    assert!(!timeout.retry_due(ObservationChannel::Fill, 119, 10));
    assert!(timeout.retry_due(ObservationChannel::Fill, 120, 10));
}

#[test]
fn healthy_channel_deadline_sentinel_is_neutral_for_waits_and_never_releases_a_hold() {
    let mut hold = AuthorityHold::default();
    hold.begin_generation(0);
    acknowledge_both(&mut hold);
    assert!(hold.channels_healthy());
    let due = hold.next_channel_due_in_ns(100, 10);
    assert_eq!(due, u64::MAX);
    assert_eq!(50u64.min(due), 50);
    assert!(hold.expiring_ack_channels(100, 10).is_empty());
    assert!(hold.is_held());
    assert!(!hold.try_release(proof()));
    assert_eq!(hold.trusted_at(), None);
}

#[test]
fn channel_errors_correlate_command_before_current_subscription() {
    let mut hold = AuthorityHold::default();
    hold.begin_generation(0);
    hold.subscribe_started(ObservationChannel::Fill, 101, 1);
    hold.subscribe_started(ObservationChannel::OrderState, 202, 1);
    assert_eq!(
        hold.on_error(Some(999), None, ChannelFault::Rejected, 2),
        ChannelError::Unrelated
    );
    assert!(!hold.observe_ack(Some(ObservationChannel::Fill), Some(999), 300));
    assert!(!hold.observe_ack(None, Some(101), 300));
    assert_eq!(
        hold.on_error(Some(101), None, ChannelFault::Rejected, 2),
        ChannelError::SubscribeRejected(ObservationChannel::Fill)
    );
    hold.subscribe_started(ObservationChannel::Fill, 303, 3);
    assert!(hold.observe_ack(Some(ObservationChannel::Fill), Some(303), 400));
    assert_eq!(
        hold.on_error(Some(202), Some(400), ChannelFault::Gap, 4),
        ChannelError::ChannelLost(ObservationChannel::OrderState)
    );
    assert!(hold.reasons().contains(&HoldCause::ChannelGap));
    assert!(!hold.channels_healthy());
    assert!(hold.needs_resubscribe(ObservationChannel::OrderState));
    assert_eq!(
        hold.on_error(None, Some(400), ChannelFault::Gap, 5),
        ChannelError::ChannelLost(ObservationChannel::Fill)
    );
    assert_eq!(
        hold.on_error(None, Some(400), ChannelFault::Gap, 6),
        ChannelError::Unrelated
    );
}

#[test]
fn reconnect_discards_old_channel_and_net_proof_and_orders_hold_notifications() {
    let mut hold = AuthorityHold::default();
    hold.begin_generation(10);
    assert_eq!(
        hold.take_entered_effects(),
        vec![HoldCause::FillUnsubscribed]
    );
    acknowledge_both(&mut hold);
    hold.observe_position_result(Some(0), 0, 40);
    assert!(hold.try_release(proof()));
    hold.enter(HoldCause::ConnectionDead, 90);
    hold.enter(HoldCause::ConnectionDead, 91);
    assert_eq!(hold.take_entered_effects(), vec![HoldCause::ConnectionDead]);
    assert!(hold.needs_reconnect());
    assert!(!hold.try_release(proof()));
    assert!(hold.begin_generation(100));
    assert_eq!(
        hold.take_entered_effects(),
        vec![HoldCause::FillUnsubscribed]
    );
    assert_eq!(hold.last_rest_net(), None);
    assert_eq!(
        hold.on_error(None, Some(301), ChannelFault::Gap, 101),
        ChannelError::Unrelated
    );
    acknowledge_both(&mut hold);
    assert!(!hold.try_release(proof()));
    hold.observe_position_result(Some(0), 0, 150);
    assert!(hold.try_release(AuthorityReleaseProof {
        reconciled_at_ns: 160,
        ..proof()
    }));
    assert_eq!(hold.trusted_at(), Some(160));
    assert_eq!(hold.generation(), 2);
}

#[test]
fn position_freshness_uses_execution_second_and_stale_never_refreshes_authority() {
    use PositionFreshness::*;
    assert_eq!(classify_position(PositionObservation::Absent, 0, 0), Match);
    assert_eq!(
        classify_position(PositionObservation::Absent, 1, 0),
        Mismatch
    );
    assert_eq!(classify_position(PositionObservation::Absent, 0, 1), Stale);
    assert_eq!(classify_position(PositionObservation::Failed, 0, 0), Failed);
    let row = |net, updated_ts_ns| PositionObservation::Row { net, updated_ts_ns };
    assert_eq!(
        classify_position(row(10, Some(2_000_000_000)), 10, 2_500_000_001),
        Match
    );
    assert_eq!(
        classify_position(row(10, Some(1_999_999_999)), 10, 2_500_000_001),
        Stale
    );
    assert_eq!(classify_position(row(10, None), 9, 2_500_000_001), Mismatch);
    let mut hold = AuthorityHold::default();
    hold.begin_generation(0);
    hold.observe_position_class(Stale, Some(100), 0, 1);
    assert_eq!(hold.last_rest_net(), None);
    assert!(!hold.reasons().contains(&HoldCause::NetMismatch));
    hold.observe_position_class(Match, Some(0), 0, 2);
    assert_eq!(hold.last_rest_net(), Some(0));
    hold.observe_position_class(Stale, Some(100), 0, 3);
    assert_eq!(hold.last_rest_net(), Some(0));
    hold.observe_position_class(Failed, Some(0), 0, 4);
    assert!(hold.reasons().contains(&HoldCause::ReconcileFailed));
}

#[test]
fn protective_cancel_shares_wire_budget_and_latches_gone() {
    let mut recovery = RecoveryCadence::default();
    let wire = VenueOrderId("wire-a".into());
    for _ in 0..6 {
        assert!(recovery.protective_cancel_due(&wire, 0));
        recovery.note_protective_cancel(&wire, ProtectiveCancelOutcome::Sent, 0);
    }
    assert!(!recovery.protective_cancel_due(&wire, u64::MAX));
    assert_eq!(recovery.cancel_suppressed(), 1);
    let other = VenueOrderId("wire-b".into());
    assert!(recovery.protective_cancel_due(&other, 0));
    recovery.note_protective_cancel(&other, ProtectiveCancelOutcome::Gone, 0);
    assert!(!recovery.protective_cancel_due(&other, u64::MAX));
}

#[test]
fn cancel_failures_preserve_post_increment_backoff_and_unproven_hold() {
    let mut recovery = RecoveryCadence::default();
    let wire = VenueOrderId("wire-a".into());
    recovery.note_protective_cancel(&wire, ProtectiveCancelOutcome::Failed, 10);
    assert!(!recovery.protective_cancel_due(&wire, 500_000_009));
    assert!(recovery.protective_cancel_due(&wire, 500_000_010));
    recovery.note_protective_cancel(&wire, ProtectiveCancelOutcome::NotFoundUnproven, 20);
    assert!(!recovery.protective_cancel_due(&wire, 10_000_000_019));
    assert!(recovery.protective_cancel_due(&wire, 10_000_000_020));
    recovery.note_protective_cancel(&wire, ProtectiveCancelOutcome::StillThere, u64::MAX - 1);
    assert!(!recovery.protective_cancel_due(&wire, u64::MAX - 1));
    assert!(recovery.protective_cancel_due(&wire, u64::MAX));
}

#[test]
fn authority_backoff_uses_pre_increment_count_caps_and_discharge_forgets() {
    let mut recovery = RecoveryCadence::default();
    let wire = VenueOrderId("wire-a".into());
    let mut now = 1;
    for delay in [
        500_000_000,
        1_000_000_000,
        2_000_000_000,
        4_000_000_000,
        4_000_000_000,
    ] {
        recovery.note_authority_read(&wire, false, now);
        assert!(!recovery.authority_read_due(&wire, now + delay - 1));
        assert!(recovery.authority_read_due(&wire, now + delay));
        now += delay;
    }
    recovery.note_authority_read(&wire, true, now);
    assert!(recovery.authority_read_due(&wire, 0));
    recovery.note_authority_read(&wire, false, u64::MAX - 1);
    assert!(!recovery.authority_read_due(&wire, u64::MAX - 1));
    assert!(recovery.authority_read_due(&wire, u64::MAX));
    assert_eq!(recovery.read_suppressed(), 6);
}
