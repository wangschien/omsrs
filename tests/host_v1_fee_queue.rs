use omsrs::host_v1::fee::{FeeEnrichQueue, FEE_ENRICH_MAX_ATTEMPTS, FEE_ENRICH_QUEUE_CAPACITY};

#[derive(Debug, PartialEq, Eq)]
struct MoveOnlySignal {
    fill_id: String,
    bytes: Vec<u8>,
}

#[test]
fn fifo_preserves_non_clone_payload_and_new_attempt_count() {
    let mut queue = FeeEnrichQueue::default();
    assert!(queue.is_empty());
    assert!(queue.pop().is_none());
    assert_eq!((queue.high_water(), queue.dropped()), (0, 0));
    for n in 0..3 {
        queue.push_new(
            format!("order-{n}"),
            MoveOnlySignal {
                fill_id: format!("fill-{n}"),
                bytes: vec![n; n as usize + 1],
            },
        );
    }
    for n in 0..3 {
        let item = queue.pop().unwrap();
        assert_eq!(item.order_id(), format!("order-{n}"));
        assert_eq!(item.signal().fill_id, format!("fill-{n}"));
        assert_eq!(item.signal().bytes, vec![n; n as usize + 1]);
        assert_eq!(item.attempts(), 0);
    }
    assert_eq!(
        (queue.len(), queue.high_water(), queue.dropped()),
        (0, 3, 0)
    );
}

#[test]
fn capacity_evicts_oldest_and_keeps_all_remaining_payloads_in_order() {
    assert_eq!(FEE_ENRICH_QUEUE_CAPACITY, 64);
    let mut queue = FeeEnrichQueue::default();
    for n in 0..66 {
        queue.push_new(format!("order-{n}"), n);
    }
    assert_eq!(
        (queue.len(), queue.high_water(), queue.dropped()),
        (64, 64, 2)
    );
    for n in 2..66 {
        let item = queue.pop().unwrap();
        assert_eq!(item.order_id(), format!("order-{n}"));
        assert_eq!(*item.signal(), n);
        assert_eq!(item.attempts(), 0);
    }
    assert!(queue.pop().is_none());
    assert_eq!((queue.high_water(), queue.dropped()), (64, 2));
}

#[test]
fn failed_attempts_join_tail_and_drop_on_the_third_failure() {
    assert_eq!(FEE_ENRICH_MAX_ATTEMPTS, 3);
    let mut queue = FeeEnrichQueue::default();
    queue.push_new("a".into(), 101);
    queue.push_new("b".into(), 202);
    let first_a = queue.pop().unwrap();
    queue.retry_or_drop(first_a);
    assert_eq!(queue.dropped(), 0);
    let b = queue.pop().unwrap();
    assert_eq!((b.order_id(), *b.signal(), b.attempts()), ("b", 202, 0));
    let second_a = queue.pop().unwrap();
    assert_eq!(
        (second_a.order_id(), *second_a.signal(), second_a.attempts()),
        ("a", 101, 1)
    );
    queue.retry_or_drop(second_a);
    let third_a = queue.pop().unwrap();
    assert_eq!(
        (third_a.order_id(), *third_a.signal(), third_a.attempts()),
        ("a", 101, 2)
    );
    queue.retry_or_drop(third_a);
    assert_eq!(
        (queue.len(), queue.high_water(), queue.dropped()),
        (0, 2, 1)
    );
}

#[test]
fn exhaustive_miss_counts_once_and_does_not_retry_or_lower_high_water() {
    let mut queue = FeeEnrichQueue::default();
    queue.push_new("a".into(), 1);
    queue.push_new("b".into(), 2);
    let missed = queue.pop().unwrap();
    assert_eq!(missed.order_id(), "a");
    queue.drop_item();
    assert_eq!(
        (queue.len(), queue.high_water(), queue.dropped()),
        (1, 2, 1)
    );
    let next = queue.pop().unwrap();
    assert_eq!(
        (next.order_id(), *next.signal(), next.attempts()),
        ("b", 2, 0)
    );
    assert!(queue.pop().is_none());
    assert_eq!(queue.dropped(), 1);
}

#[test]
fn retry_into_a_refilled_queue_evicts_oldest_and_preserves_retry_count() {
    let mut queue = FeeEnrichQueue::default();
    queue.push_new("retry".into(), 900);
    let retry = queue.pop().unwrap();
    for n in 0..64 {
        queue.push_new(format!("fresh-{n}"), n);
    }
    queue.retry_or_drop(retry);
    assert_eq!(
        (queue.len(), queue.high_water(), queue.dropped()),
        (64, 64, 1)
    );
    for n in 1..64 {
        let item = queue.pop().unwrap();
        assert_eq!(item.order_id(), format!("fresh-{n}"));
        assert_eq!(*item.signal(), n);
        assert_eq!(item.attempts(), 0);
    }
    let retry = queue.pop().unwrap();
    assert_eq!(
        (retry.order_id(), *retry.signal(), retry.attempts()),
        ("retry", 900, 1)
    );
    assert!(queue.is_empty());
}

#[test]
fn duplicate_requests_remain_distinct_without_coalescing_the_payloads() {
    let mut queue = FeeEnrichQueue::default();
    queue.push_new("same-order".into(), "first");
    queue.push_new("same-order".into(), "second");
    let first = queue.pop().unwrap();
    let second = queue.pop().unwrap();
    assert_eq!((first.order_id(), *first.signal()), ("same-order", "first"));
    assert_eq!(
        (second.order_id(), *second.signal()),
        ("same-order", "second")
    );
    assert_eq!(
        (queue.len(), queue.high_water(), queue.dropped()),
        (0, 2, 0)
    );
}
