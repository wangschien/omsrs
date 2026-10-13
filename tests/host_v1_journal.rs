use omsrs::host_v1::*;
use omsrs::lifecycle::{Effect, FillId, JournalRecord, VenueOrderId};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

#[derive(Debug)]
struct FilePort {
    file: File,
    events: std::sync::Mutex<Vec<String>>,
    fail_truncate: std::sync::atomic::AtomicBool,
    fail_repair_fence: std::sync::atomic::AtomicBool,
}
impl JournalPort for FilePort {
    fn read_all(&self) -> std::io::Result<Vec<u8>> {
        let mut bytes = vec![0; self.file.metadata()?.len() as usize];
        self.file.read_exact_at(&mut bytes, 0)?;
        Ok(bytes)
    }
    fn len(&self) -> std::io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }
    fn append_ordered(&self, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
        self.file.write_all_at(bytes, offset)
    }
    fn truncate(&self, keep: u64) -> std::io::Result<()> {
        self.events.lock().unwrap().push("truncate".into());
        if self
            .fail_truncate
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(std::io::Error::other("client-injected truncate failure"));
        }
        self.file.set_len(keep)
    }
    fn fence(&self, kind: FenceKind) -> std::io::Result<()> {
        self.events.lock().unwrap().push(format!("fence:{kind:?}"));
        if self
            .fail_repair_fence
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(std::io::Error::other(
                "client-injected repair fence failure",
            ));
        }
        match kind {
            FenceKind::Data => self.file.sync_data(),
            FenceKind::All => self.file.sync_all(),
        }
    }
    fn current_thread_id(&self) -> u64 {
        0
    }
    fn observe(&self, event: JournalDiagnostic<'_>) {
        match event {
            JournalDiagnostic::Recovery(d) => self.events.lock().unwrap().push(format!(
                "diagnostic:{}:{}:{}:{:?}:{:?}",
                d.line, d.records_end, d.discarded_bytes, d.reason, d.preview
            )),
            _ => self.events.lock().unwrap().push(format!("{event:?}")),
        }
    }
}
fn port() -> Arc<FilePort> {
    Arc::new(FilePort {
        file: tempfile::tempfile().unwrap(),
        events: std::sync::Mutex::new(Vec::new()),
        fail_truncate: std::sync::atomic::AtomicBool::new(false),
        fail_repair_fence: std::sync::atomic::AtomicBool::new(false),
    })
}

fn line(r: &JournalRecord) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(r).unwrap();
    bytes.push(b'\n');
    bytes
}
fn fill(id: &str, qty: u64) -> JournalRecord {
    JournalRecord::Fill {
        fill_id: FillId(id.into()),
        qty,
        price_cents: 50,
        ts_ns: 1,
        fee_cents: None,
        venue_order_id: Some(VenueOrderId("wire-a".into())),
    }
}

#[test]
fn recovery_preserves_record_bytes_cr_and_missing_newline() {
    let p = port();
    let r = JournalRecord::SubmitUnknown;
    let bytes = serde_json::to_vec(&r).unwrap();
    for suffix in [b"".as_slice(), b"\n", b"\r\n"] {
        p.truncate(0).unwrap();
        let mut input = bytes.clone();
        input.extend_from_slice(suffix);
        p.append_ordered(0, &input).unwrap();
        let recovered = recover_journal(p.as_ref(), true).unwrap();
        assert_eq!(recovered.records.len(), 1);
        assert!(!recovered.dropped_torn);
        assert_eq!(recovered.records_end, input.len() as u64);
        assert_eq!(p.read_all().unwrap(), input);
    }
}
#[test]
fn recovery_returns_only_prefix_for_all_three_torn_arms() {
    // Client-injected hole, invalid UTF-8 and malformed record; no recorded input.
    for prefix in [Vec::new(), line(&JournalRecord::SubmitUnknown)] {
        for fault in [b"\n".as_slice(), b"\xff\n", b"{\n"] {
            let p = port();
            let mut bytes = prefix.clone();
            bytes.extend_from_slice(fault);
            bytes.extend_from_slice(&line(&JournalRecord::SubmitUnknown));
            bytes.extend_from_slice(b"\n\n");
            p.append_ordered(0, &bytes).unwrap();
            let r = recover_journal(p.as_ref(), true).unwrap();
            assert!(r.dropped_torn);
            assert_eq!(r.records.len(), usize::from(!prefix.is_empty()));
            assert_eq!(r.records_end, prefix.len() as u64);
            assert!(r.discarded_bytes > 0);
            assert_eq!(p.read_all().unwrap(), prefix);
        }
    }
    // Diagnostic delivery precedes either repair failure and survives Err.
    for operation in [
        JournalOperation::RepairTruncate,
        JournalOperation::RepairFence,
    ] {
        let p = port();
        let prefix = line(&JournalRecord::SubmitUnknown);
        let mut bytes = prefix.clone();
        bytes.extend_from_slice(b"{broken\n");
        bytes.extend_from_slice(&line(&JournalRecord::SubmitUnknown));
        p.append_ordered(0, &bytes).unwrap();
        if operation == JournalOperation::RepairTruncate {
            p.fail_truncate
                .store(true, std::sync::atomic::Ordering::SeqCst);
        } else {
            p.fail_repair_fence
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let err = recover_journal(p.as_ref(), true).unwrap_err();
        assert!(matches!(err, JournalError::Io { operation: actual, .. } if actual == operation));
        let events = p.events.lock().unwrap();
        assert!(events[0].starts_with(&format!("diagnostic:2:{}:", prefix.len())));
        assert!(
            events[0].contains("Json")
                && events[0].contains("{broken")
                && events[0].contains("error")
        );
        assert_eq!(events[1], "truncate");
        if operation == JournalOperation::RepairFence {
            assert_eq!(events[2], "fence:All");
        }
    }
}
#[test]
fn all_torn_arms_share_one_return_site() {
    let src = include_str!("../src/host_v1/persistence.rs");
    let start = src
        .find("pub fn recover_journal")
        .expect("loader must exist");
    let rest = &src[start..];
    let end = rest
        .find("\nfn truncate_journal_to")
        .expect("loader right boundary must exist");
    let body = rest[..end]
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(body.matches("return torn_truncate_return(").count(), 3);
    assert!(!body.contains("truncate_journal_to(port, truncate_torn"));
}
#[test]
fn unknown_variant_refuses_without_rewrite() {
    let p = port();
    let mut bytes = line(&JournalRecord::SubmitUnknown);
    bytes.extend_from_slice(b"{\"FutureRecord\":{}}\n");
    p.append_ordered(0, &bytes).unwrap();
    assert!(matches!(
        recover_journal(p.as_ref(), true),
        Err(JournalError::UnknownVariant { .. })
    ));
    assert_eq!(p.read_all().unwrap(), bytes);
}
#[test]
fn known_malformed_and_zero_padding_are_torn_without_skipping() {
    for bad in [b"{\"Fill\":{}}\n".as_slice(), b"\0\0\n"] {
        let p = port();
        let mut bytes = line(&JournalRecord::SubmitUnknown);
        bytes.extend_from_slice(bad);
        p.append_ordered(0, &bytes).unwrap();
        let r = recover_journal(p.as_ref(), true).unwrap();
        assert!(r.dropped_torn);
        assert_eq!(r.records.len(), 1);
    }
}
#[test]
fn padding_is_not_a_record_or_discarded_content() {
    let p = port();
    let mut bytes = line(&JournalRecord::SubmitUnknown);
    let end = bytes.len();
    bytes.extend_from_slice(b"\n\n\n");
    p.append_ordered(0, &bytes).unwrap();
    let r = recover_journal(p.as_ref(), true).unwrap();
    assert_eq!(r.records_end, end as u64);
    assert_eq!(r.discarded_bytes, 0);
    assert!(!r.dropped_torn);
    assert_eq!(p.read_all().unwrap(), bytes);
}
#[test]
fn indexed_first_fill_clone_and_reset_equal_linear_scan() {
    let mut j = MemoryJournal::new();
    for r in [fill("fill-a", 20), fill("fill-a", 90), fill("fill-b", 3)] {
        j.apply_effects(&[Effect::AppendFsync(r)]).unwrap();
        assert_eq!(
            j.wire_fill_qty("wire-a"),
            wire_fill_qty_scan(j.records(), "wire-a")
        );
    }
    assert_eq!(j.wire_fill_qty("wire-a"), Some(23));
    assert!(j.has_fill_id("fill-a"));
    assert!(journal_contains_fill(j.records(), "fill-a"));
    assert!(!journal_contains_fill(j.records(), "missing-fill"));
    let clone = j.clone();
    assert_eq!(clone.wire_fill_qty("wire-a"), Some(23));
    j.clear();
    assert_eq!(j.wire_fill_qty("wire-a"), None);
    assert!(!j.has_fill_id("fill-a"));
    assert_eq!(clone.len(), 3);
    // The default is_empty includes non-record effects, as the legacy trait did.
    assert!(j.is_empty());
    j.apply_effects(&[Effect::BackfillUnknown {
        client_order_id: omsrs::lifecycle::ClientOrderId("effects-only".into()),
    }])
    .unwrap();
    assert!(j.records().is_empty());
    assert_eq!(j.other_effects().len(), 1);
    assert!(!j.is_empty());
}
#[test]
fn neutral_fee_signed_cash_and_fractional_conservation() {
    for revenue in [dec!(0.3333), dec!(-0.3333)] {
        let mut a = OrderFeeAccumulator::new(BalancePrecision::Cent);
        for _ in 0..20 {
            let b = a
                .apply_fill(FeeInput {
                    revenue,
                    trade_fee: dec!(0.0011),
                })
                .unwrap();
            assert!(b.rounding_fee >= Decimal::ZERO);
            assert!(a.rounding_acc < dec!(0.01));
            assert_eq!(
                a.economic_net_fee(),
                a.total_trade_fee() + a.total_rounding_fee() - a.total_rebate()
            );
        }
    }
}
#[test]
fn neutral_fee_rebate_credit_does_not_clamp_economic_net() {
    let mut a = OrderFeeAccumulator::new(BalancePrecision::Cent);
    a.rounding_acc = dec!(0.009);
    let b = a
        .apply_fill(FeeInput {
            revenue: dec!(0.001),
            trade_fee: Decimal::ZERO,
        })
        .unwrap();
    assert_eq!(b.rebate, dec!(0.01));
    assert_eq!(b.net_fee, Decimal::ZERO);
    assert_eq!(a.economic_net_fee(), dec!(-0.009));
}
#[test]
fn neutral_fee_legacy_snapshot_and_atomic_count_overflow() {
    let mut a = OrderFeeAccumulator::new(BalancePrecision::Centicent);
    a.fill_count = u64::MAX;
    let before = a.clone();
    assert!(a
        .apply_fill(FeeInput {
            revenue: dec!(-0.3),
            trade_fee: dec!(0.01)
        })
        .is_err());
    assert_eq!(a, before);
    let mut snapshot = serde_json::to_value(&a).unwrap();
    snapshot["total_net_fee"] = serde_json::json!("999");
    assert_eq!(
        serde_json::from_value::<OrderFeeAccumulator>(snapshot).unwrap(),
        a
    );
    assert!(!serde_json::to_string(&a).unwrap().contains("total_net_fee"));
}
#[test]
fn neutral_fee_invalid_input_and_snapshot_refuse_without_mutation() {
    let mut a = OrderFeeAccumulator::new(BalancePrecision::Cent);
    let before = a.clone();
    assert!(a
        .apply_fill(FeeInput {
            revenue: dec!(0.5),
            trade_fee: dec!(-0.01)
        })
        .is_err());
    assert_eq!(a, before);
    let mut snapshot = serde_json::to_value(&a).unwrap();
    snapshot["total_trade_fee"] = serde_json::json!(Decimal::MAX.to_string());
    assert!(serde_json::from_value::<OrderFeeAccumulator>(snapshot).is_err());
}
