//! Ordered journal ownership over a caller-provided raw IO resource.
use super::types::{
    DurabilityAck, JournalDiagnostic, JournalDurability, JournalError, JournalOperation,
    RecoveredJournal, RecoveryDiagnostic, RecoveryReason,
};
use crate::lifecycle::{CancelOutcome, Effect, JournalRecord};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Condvar, Mutex, MutexGuard,
};

type Result<T> = std::result::Result<T, JournalError>;
const PREALLOC_CHUNK: u64 = 1 << 20;
const SHUTDOWN_RETRIES: u32 = 3;

fn io_error(operation: JournalOperation) -> impl FnOnce(std::io::Error) -> JournalError {
    move |source| JournalError::Io { operation, source }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceKind {
    Data,
    All,
}

/// Implementations perform only raw IO. Prefix, padding, recovery, append
/// offsets, retry policy and publication are decided by this module.
/// The caller retains the resource's existing exclusive-writer discipline.
pub trait JournalPort: std::fmt::Debug + Send + Sync + 'static {
    fn read_all(&self) -> std::io::Result<Vec<u8>>;
    fn len(&self) -> std::io::Result<u64>;
    fn append_ordered(&self, offset: u64, bytes: &[u8]) -> std::io::Result<()>;
    fn truncate(&self, keep: u64) -> std::io::Result<()>;
    fn fence(&self, kind: FenceKind) -> std::io::Result<()>;
    fn current_thread_id(&self) -> u64;
    fn observe(&self, diagnostic: JournalDiagnostic<'_>);
}

/// Recovery can precede acquisition of a writable append handle.
pub trait JournalRecoveryPort: std::fmt::Debug {
    fn read_recovery_bytes(&self) -> std::io::Result<Vec<u8>>;
    fn truncate_recovery_prefix(&self, keep: u64) -> std::io::Result<()>;
    fn fence_recovery(&self) -> std::io::Result<()>;
    fn observe_recovery(&self, diagnostic: RecoveryDiagnostic<'_>);
}
impl<P: JournalPort> JournalRecoveryPort for P {
    fn read_recovery_bytes(&self) -> std::io::Result<Vec<u8>> {
        self.read_all()
    }
    fn truncate_recovery_prefix(&self, keep: u64) -> std::io::Result<()> {
        self.truncate(keep)
    }
    fn fence_recovery(&self) -> std::io::Result<()> {
        self.fence(FenceKind::All)
    }
    fn observe_recovery(&self, diagnostic: RecoveryDiagnostic<'_>) {
        self.observe(JournalDiagnostic::Recovery(diagnostic));
    }
}

/// The destination selects the order of preparation, repair and writer opening.
/// These methods perform raw resource acquisition only.
pub trait JournalSource: JournalRecoveryPort {
    type Port: JournalPort;
    fn prepare_resource(&self) -> std::io::Result<()>;
    fn open_writer(&self) -> std::io::Result<Self::Port>;
}

fn content_end_of(data: &[u8]) -> usize {
    data.iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// Decode a prefix; unknown variants are refused without invoking any repair.
pub fn recover_journal<P: JournalRecoveryPort>(
    port: &P,
    truncate_torn: bool,
) -> Result<RecoveredJournal> {
    let data = port
        .read_recovery_bytes()
        .map_err(io_error(JournalOperation::RecoveryRead))?;
    let mut records = Vec::new();
    let mut records_end = 0;
    let mut saw_padding = false;
    let mut line_no = 0;
    let mut offset = 0;
    while offset < data.len() {
        let nl = data[offset..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| offset + p);
        let (line_end, next) = match nl {
            Some(n) => (n, n + 1),
            None => (data.len(), data.len()),
        };
        let line_bytes = &data[offset..line_end];
        let line_bytes = line_bytes.strip_suffix(&[b'\r']).unwrap_or(line_bytes);
        line_no += 1;
        if !line_bytes.iter().all(|b| b.is_ascii_whitespace()) {
            if saw_padding {
                return torn_truncate_return(
                    port,
                    truncate_torn,
                    records,
                    records_end,
                    &data,
                    line_no,
                    RecoveryReason::Hole,
                );
            }
            let line = match std::str::from_utf8(line_bytes) {
                Ok(line) => line,
                Err(_) => {
                    return torn_truncate_return(
                        port,
                        truncate_torn,
                        records,
                        records_end,
                        &data,
                        line_no,
                        RecoveryReason::InvalidUtf8,
                    );
                }
            };
            match serde_json::from_str::<JournalRecord>(line) {
                Ok(rec) => {
                    records.push(rec);
                    records_end = next;
                }
                Err(e) => {
                    if serde_json::from_str::<serde_json::Value>(line).is_ok()
                        && e.to_string().contains("unknown variant")
                    {
                        return Err(JournalError::UnknownVariant {
                            line: line_no,
                            message: e.to_string(),
                        });
                    }
                    return torn_truncate_return(
                        port,
                        truncate_torn,
                        records,
                        records_end,
                        &data,
                        line_no,
                        RecoveryReason::Json { error: &e, line },
                    );
                }
            }
        } else {
            saw_padding = true;
        }
        offset = next;
    }
    Ok(RecoveredJournal {
        records,
        dropped_torn: false,
        records_end: records_end as u64,
        discarded_bytes: 0,
    })
}

fn truncate_journal_to<P: JournalRecoveryPort>(
    port: &P,
    truncate_torn: bool,
    keep: usize,
    file_len: usize,
) -> Result<()> {
    if !truncate_torn || keep >= file_len {
        return Ok(());
    }
    port.truncate_recovery_prefix(keep as u64)
        .map_err(io_error(JournalOperation::RepairTruncate))?;
    port.fence_recovery()
        .map_err(io_error(JournalOperation::RepairFence))?;
    Ok(())
}

fn torn_truncate_return<P: JournalRecoveryPort>(
    port: &P,
    truncate_torn: bool,
    records: Vec<JournalRecord>,
    records_end: usize,
    data: &[u8],
    line: usize,
    reason: RecoveryReason<'_>,
) -> Result<RecoveredJournal> {
    let content_end = content_end_of(data);
    let discarded_bytes = content_end.saturating_sub(records_end) as u64;
    let preview = match &reason {
        RecoveryReason::Json { line, .. } => Some(line.chars().take(80).collect::<String>()),
        _ => None,
    };
    port.observe_recovery(RecoveryDiagnostic {
        line,
        records_end: records_end as u64,
        discarded_bytes,
        reason: &reason,
        preview: preview.as_deref(),
    });
    truncate_journal_to(port, truncate_torn, records_end, data.len())?;
    Ok(RecoveredJournal {
        records,
        dropped_torn: true,
        records_end: records_end as u64,
        discarded_bytes,
    })
}

pub fn for_each_journal_record(records: &[JournalRecord], visit: &mut impl FnMut(&JournalRecord)) {
    for r in records {
        visit(r);
        match r {
            JournalRecord::OrderTxn(o) => for_each_journal_record(&o.core_records, visit),
            JournalRecord::ExecutionTxn(e) => {
                if let Some(o) = e.order.as_ref() {
                    for_each_journal_record(&o.core_records, visit);
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Default)]
pub struct JournalIndex {
    scanned: usize,
    fill_ids: HashSet<String>,
    cancel_not_found: HashSet<String>,
    wire_fills: HashMap<String, HashMap<String, u64>>,
    booked_volume: u64,
}
impl JournalIndex {
    fn sync(&mut self, records: &[JournalRecord]) {
        if records.len() < self.scanned {
            *self = Self::default();
        }
        let new = &records[self.scanned..];
        for_each_journal_record(new, &mut |r| match r {
            JournalRecord::Fill { fill_id, .. } | JournalRecord::FillCid { fill_id, .. } => {
                self.fill_ids.insert(fill_id.0.clone());
            }
            JournalRecord::ExecutionTxn(e) => {
                for o in &e.executions {
                    self.fill_ids.insert(o.evidence.execution_id.0.clone());
                }
            }
            JournalRecord::CancelOutcomeCid {
                client_order_id,
                outcome: CancelOutcome::NotFound,
            } => {
                self.cancel_not_found.insert(client_order_id.0.clone());
            }
            _ => {}
        });
        self.booked_volume += booked_volume_scan(new);
        for r in new {
            if let JournalRecord::Fill {
                fill_id,
                qty,
                venue_order_id: Some(v),
                ..
            }
            | JournalRecord::FillCid {
                fill_id,
                qty,
                venue_order_id: Some(v),
                ..
            } = r
            {
                self.wire_fills
                    .entry(v.0.clone())
                    .or_default()
                    .entry(fill_id.0.clone())
                    .or_insert(*qty);
            }
        }
        self.scanned = records.len();
    }
}

pub fn booked_volume_scan(records: &[JournalRecord]) -> u64 {
    records
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ExecutionTxn(e) => Some(
                e.executions
                    .iter()
                    .filter(|o| {
                        matches!(
                            o.inventory,
                            crate::execution::InventoryDisposition::Ingested(_)
                        )
                    })
                    .map(|o| o.evidence.qty)
                    .sum::<u64>(),
            ),
            _ => None,
        })
        .sum()
}

pub fn wire_fill_qty_scan(records: &[JournalRecord], wire: &str) -> Option<u64> {
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let mut any = false;
    for r in records {
        let (id, qty, vid) = match r {
            JournalRecord::Fill {
                fill_id,
                qty,
                venue_order_id: Some(v),
                ..
            }
            | JournalRecord::FillCid {
                fill_id,
                qty,
                venue_order_id: Some(v),
                ..
            } => (&fill_id.0, *qty, &v.0),
            _ => continue,
        };
        if vid != wire {
            continue;
        }
        any = true;
        if seen.insert(id.clone()) {
            total = total.saturating_add(qty);
        }
    }
    any.then_some(total)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
#[derive(Debug, Default)]
pub struct JournalIndexCell(Mutex<JournalIndex>);
impl Clone for JournalIndexCell {
    fn clone(&self) -> Self {
        Self::default()
    }
}
impl JournalIndexCell {
    fn reset(&self) {
        *lock(&self.0) = JournalIndex::default();
    }
    fn with<R>(&self, records: &[JournalRecord], f: impl FnOnce(&JournalIndex) -> R) -> R {
        let mut g = lock(&self.0);
        g.sync(records);
        f(&g)
    }
}

pub trait OmsJournal: std::fmt::Debug {
    fn apply_effects(&mut self, effects: &[Effect]) -> Result<DurabilityAck>;
    fn commit_now(&mut self) -> Result<DurabilityAck>;
    fn fsync_errors(&self) -> u64;
    fn truncated_bytes(&self) -> u64;
    fn records(&self) -> &[JournalRecord];
    fn other_effects(&self) -> &[Effect];
    fn has_fill_id(&self, id: &str) -> bool {
        journal_contains_fill(self.records(), id)
    }
    fn has_cancel_not_found(&self, client: &str) -> bool {
        journal_has_cancel_not_found(self.records(), client)
    }
    fn wire_fill_qty(&self, wire: &str) -> Option<u64> {
        wire_fill_qty_scan(self.records(), wire)
    }
    fn booked_volume(&self) -> u64 {
        booked_volume_scan(self.records())
    }
    fn len(&self) -> usize {
        self.records().len()
    }
    fn is_empty(&self) -> bool {
        self.records().is_empty() && self.other_effects().is_empty()
    }
}

pub fn journal_contains_fill(records: &[JournalRecord], trade_id: &str) -> bool {
    fn rec_has(r: &JournalRecord, trade_id: &str) -> bool {
        match r {
            JournalRecord::Fill { fill_id, .. } | JournalRecord::FillCid { fill_id, .. }
                if fill_id.0 == trade_id =>
            {
                true
            }
            JournalRecord::OrderTxn(o) => o.core_records.iter().any(|c| rec_has(c, trade_id)),
            JournalRecord::ExecutionTxn(e) => {
                e.executions
                    .iter()
                    .any(|o| o.evidence.execution_id.0 == trade_id)
                    || e.order
                        .as_ref()
                        .is_some_and(|o| o.core_records.iter().any(|c| rec_has(c, trade_id)))
            }
            _ => false,
        }
    }
    records.iter().any(|r| rec_has(r, trade_id))
}

pub fn journal_has_cancel_not_found(records: &[JournalRecord], client: &str) -> bool {
    let mut found = false;
    for_each_journal_record(records, &mut |record| {
        if matches!(record, JournalRecord::CancelOutcomeCid { client_order_id, outcome: CancelOutcome::NotFound } if client_order_id.0 == client)
        {
            found = true;
        }
    });
    found
}

#[derive(Debug, Default, Clone)]
pub struct MemoryJournal {
    pub records: Vec<JournalRecord>,
    pub other_effects: Vec<Effect>,
    index: JournalIndexCell,
}
impl MemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn clear(&mut self) {
        self.records.clear();
        self.index.reset();
        self.other_effects.clear();
    }
}
impl OmsJournal for MemoryJournal {
    fn apply_effects(&mut self, effects: &[Effect]) -> Result<DurabilityAck> {
        for e in effects {
            match e {
                Effect::AppendFsync(r) => self.records.push(r.clone()),
                other => self.other_effects.push(other.clone()),
            }
        }
        Ok(DurabilityAck::Volatile { records_end: 0 })
    }
    fn commit_now(&mut self) -> Result<DurabilityAck> {
        Ok(DurabilityAck::Volatile { records_end: 0 })
    }
    fn fsync_errors(&self) -> u64 {
        0
    }
    fn truncated_bytes(&self) -> u64 {
        0
    }
    fn records(&self) -> &[JournalRecord] {
        &self.records
    }
    fn other_effects(&self) -> &[Effect] {
        &self.other_effects
    }
    fn has_fill_id(&self, id: &str) -> bool {
        self.index.with(&self.records, |i| i.fill_ids.contains(id))
    }
    fn has_cancel_not_found(&self, client: &str) -> bool {
        self.index
            .with(&self.records, |i| i.cancel_not_found.contains(client))
    }
    fn wire_fill_qty(&self, wire: &str) -> Option<u64> {
        self.index.with(&self.records, |i| {
            i.wire_fills
                .get(wire)
                .map(|m| m.values().fold(0u64, |a, q| a.saturating_add(*q)))
        })
    }
    fn booked_volume(&self) -> u64 {
        self.index.with(&self.records, |i| i.booked_volume)
    }
}

#[derive(Debug)]
struct FsyncPump<P: JournalPort> {
    port: Arc<P>,
    state: Arc<(Mutex<(bool, bool)>, Condvar, Mutex<()>)>,
    bg_barriers: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    tid: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl<P: JournalPort> FsyncPump<P> {
    fn start(port: Arc<P>) -> Result<Self> {
        let state = Arc::new((Mutex::new((false, false)), Condvar::new(), Mutex::new(())));
        let bg_barriers = Arc::new(AtomicU64::new(0));
        let errors = Arc::new(AtomicU64::new(0));
        let tid = Arc::new(AtomicU64::new(0));
        let (p, st, ba, er, id) = (
            port.clone(),
            state.clone(),
            bg_barriers.clone(),
            errors.clone(),
            tid.clone(),
        );
        let handle = std::thread::Builder::new()
            .name("oms-journal-fsync".into())
            .spawn(move || {
                id.store(p.current_thread_id(), Ordering::Relaxed);
                let mut fails: u32 = 0;
                loop {
                    let mut g = lock(&st.0);
                    while !g.0 && !g.1 {
                        g = st.1.wait(g).unwrap_or_else(|e| e.into_inner());
                    }
                    let done = g.1;
                    let dirty = g.0;
                    g.0 = false;
                    drop(g);
                    if dirty {
                        let result = {
                            let _fence_guard = lock(&st.2);
                            let result = p.fence(FenceKind::Data);
                            match &result {
                                Ok(()) => {
                                    ba.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(_) => {
                                    er.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            result
                        };
                        match result {
                            Ok(()) => {
                                fails = 0;
                            }
                            Err(e) => {
                                let n = fails + 1;
                                if n <= 3 || n.is_power_of_two() {
                                    p.observe(JournalDiagnostic::BackgroundFenceFailure {
                                        operation: JournalOperation::BackgroundFence,
                                        attempt: n,
                                        error: &e,
                                    });
                                }
                                lock(&st.0).0 = true;
                                fails = fails.saturating_add(1);
                                if done && fails >= SHUTDOWN_RETRIES {
                                    return;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(
                                    1u64 << fails.min(6),
                                ));
                                continue;
                            }
                        }
                    }
                    if done && !dirty {
                        return;
                    }
                }
            })
            .map_err(io_error(JournalOperation::SpawnWorker))?;
        Ok(Self {
            port,
            state,
            bg_barriers,
            errors,
            tid,
            handle: Some(handle),
        })
    }
    fn note_dirty(&self) {
        lock(&self.state.0).0 = true;
        self.state.1.notify_one();
    }
    fn shutdown(&mut self) -> bool {
        lock(&self.state.0).1 = true;
        self.state.1.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        match self.port.fence(FenceKind::Data) {
            Ok(()) => true,
            Err(e) => {
                self.errors.fetch_add(1, Ordering::Relaxed);
                self.port.observe(JournalDiagnostic::ShutdownFenceFailure {
                    operation: JournalOperation::ShutdownFence,
                    error: &e,
                });
                false
            }
        }
    }
}

/// Bound to an owner and record prefix, not an all-state snapshot or prepare order.
#[derive(Debug)]
pub struct PreparedJournal {
    owner: Arc<()>,
    records_at: usize,
    offset_at: u64,
    records: Vec<JournalRecord>,
    other: Vec<Effect>,
    bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct FileJournal<P: JournalPort> {
    identity: Arc<()>,
    durability: JournalDurability,
    port: Arc<P>,
    pump: Option<FsyncPump<P>>,
    write_off: u64,
    capacity: u64,
    buf: Vec<u8>,
    records: Vec<JournalRecord>,
    index: JournalIndexCell,
    other_effects: Vec<Effect>,
    truncated_bytes: u64,
    fg_barriers: u64,
    prealloc_exhausted: u64,
    prealloc_exceeded: u64,
    startup_prealloc_bytes: u64,
    last_explicit_commit_len: usize,
    /// Only completed foreground fences establish this prefix assurance.
    stable_end: Option<u64>,
    /// Certificate-only sticky errors; legacy fsync_errors remains background-only.
    foreground_fence_errors: u64,
}
impl<P: JournalPort> FileJournal<P> {
    pub fn open(
        port: Arc<P>,
        durability: JournalDurability,
        prealloc: Option<u64>,
    ) -> Result<Self> {
        let recovered = recover_journal(port.as_ref(), true)?;
        let capacity = port.len().map_err(io_error(JournalOperation::Metadata))?;
        Self::from_recovered(port, capacity, recovered, durability, prealloc)
    }

    pub fn open_source<S: JournalSource<Port = P>>(
        source: &S,
        durability: JournalDurability,
        prealloc: Option<u64>,
    ) -> Result<Self> {
        source
            .prepare_resource()
            .map_err(io_error(JournalOperation::PrepareResource))?;
        let recovered = recover_journal(source, true)?;
        let port = source
            .open_writer()
            .map_err(io_error(JournalOperation::OpenWriter))?;
        let capacity = port.len().map_err(io_error(JournalOperation::Metadata))?;
        let port = Arc::new(port);
        Self::from_recovered(port, capacity, recovered, durability, prealloc)
    }

    fn from_recovered(
        port: Arc<P>,
        capacity: u64,
        recovered: RecoveredJournal,
        durability: JournalDurability,
        prealloc: Option<u64>,
    ) -> Result<Self> {
        let existing_len = recovered.records.len();
        let stable_end = if recovered.records_end == 0 {
            Some(0)
        } else {
            None
        };
        let mut j = Self {
            identity: Arc::new(()),
            durability,
            port,
            pump: None,
            write_off: recovered.records_end,
            capacity,
            buf: Vec::with_capacity(4096),
            records: recovered.records,
            index: JournalIndexCell::default(),
            other_effects: Vec::new(),
            truncated_bytes: recovered.discarded_bytes,
            fg_barriers: 0,
            prealloc_exhausted: 0,
            prealloc_exceeded: 0,
            startup_prealloc_bytes: prealloc.unwrap_or(0),
            last_explicit_commit_len: existing_len,
            stable_end,
            foreground_fence_errors: 0,
        };
        match prealloc {
            Some(bytes) => {
                let target = j.write_off.saturating_add(bytes);
                if j.capacity < target {
                    j.pad_to(target)?;
                }
            }
            None => {
                if j.write_off >= j.capacity {
                    j.pad_to(j.write_off.max(j.capacity).saturating_add(PREALLOC_CHUNK))?;
                }
            }
        }
        if durability == JournalDurability::BackgroundFsync {
            j.pump = Some(FsyncPump::start(j.port.clone())?);
        }
        Ok(j)
    }
    fn pad_to(&mut self, target: u64) -> Result<()> {
        let mut off = self.capacity;
        let pad = vec![b'\n'; 64 * 1024];
        while off < target {
            let n = (pad.len() as u64).min(target - off) as usize;
            self.port
                .append_ordered(off, &pad[..n])
                .map_err(io_error(JournalOperation::PaddingWrite))?;
            off += n as u64;
        }
        self.foreground_fence(FenceKind::All, JournalOperation::PaddingFence)?;
        self.capacity = target;
        Ok(())
    }
    fn ensure_capacity(&mut self, need: u64) -> Result<()> {
        let want = self.write_off.saturating_add(need);
        if want < self.capacity {
            return Ok(());
        }
        self.pad_to(want.max(self.capacity).saturating_add(PREALLOC_CHUNK))?;
        self.prealloc_exhausted = self.prealloc_exhausted.saturating_add(1);
        self.prealloc_exceeded = self.prealloc_exceeded.saturating_add(1);
        self.port
            .observe(JournalDiagnostic::PreallocationExhausted {
                count: self.prealloc_exhausted,
                used_bytes: self.write_off,
                startup_prealloc_bytes: self.startup_prealloc_bytes,
                exceeded_count: self.prealloc_exceeded,
            });
        Ok(())
    }
    pub fn prepare_effects(&mut self, effects: &[Effect]) -> Result<PreparedJournal> {
        let mut bytes = std::mem::take(&mut self.buf);
        bytes.clear();
        let mut records = Vec::new();
        let mut other = Vec::new();
        for e in effects {
            match e {
                Effect::AppendFsync(r) => {
                    if let Err(e) = serde_json::to_writer(&mut bytes, r) {
                        bytes.clear();
                        self.buf = bytes;
                        return Err(e.into());
                    }
                    bytes.push(b'\n');
                    records.push(r.clone());
                }
                e => other.push(e.clone()),
            }
        }
        Ok(PreparedJournal {
            owner: self.identity.clone(),
            records_at: self.records.len(),
            offset_at: self.write_off,
            records,
            other,
            bytes,
        })
    }
    pub fn commit_prepared(&mut self, mut txn: PreparedJournal) -> Result<DurabilityAck> {
        if !Arc::ptr_eq(&txn.owner, &self.identity)
            || txn.records_at != self.records.len()
            || txn.offset_at != self.write_off
        {
            return Err(JournalError::StalePreparation);
        }
        let off_at = self.write_off;
        let r = if txn.records.is_empty() {
            Ok(())
        } else {
            self.append_batch(&txn.bytes)
        };
        txn.bytes.clear();
        self.buf = txn.bytes;
        if let Err(e) = r {
            self.write_off = off_at;
            self.index.reset();
            return Err(e);
        }
        self.records.append(&mut txn.records);
        self.other_effects.append(&mut txn.other);
        Ok(self.assurance_ack())
    }

    fn assurance_ack(&self) -> DurabilityAck {
        let errors = self
            .foreground_fence_errors
            .saturating_add(self.fsync_errors());
        if errors != 0 {
            DurabilityAck::Uncertain {
                records_end: self.write_off,
                fence_errors: errors,
            }
        } else if self.stable_end.is_some_and(|end| end >= self.write_off) {
            DurabilityAck::StableStorage {
                records_end: self.write_off,
            }
        } else {
            DurabilityAck::PageCache {
                records_end: self.write_off,
            }
        }
    }

    fn foreground_fence(
        &mut self,
        kind: FenceKind,
        operation: JournalOperation,
    ) -> Result<DurabilityAck> {
        // Only explicit/growth fences in background mode serialize with the
        // worker's fence and error publication. note_dirty keeps its old lock.
        let _fence_guard = self.pump.as_ref().map(|p| lock(&p.state.2));
        if let Err(source) = self.port.fence(kind) {
            self.foreground_fence_errors = self.foreground_fence_errors.saturating_add(1);
            return Err(JournalError::Io { operation, source });
        }
        self.fg_barriers = self.fg_barriers.saturating_add(1);
        self.stable_end = Some(self.write_off);
        Ok(self.assurance_ack())
    }

    fn append_batch(&mut self, bytes: &[u8]) -> Result<()> {
        self.ensure_capacity(bytes.len() as u64)?;
        self.port
            .append_ordered(self.write_off, bytes)
            .map_err(io_error(JournalOperation::AppendWrite))?;
        self.write_off += bytes.len() as u64;
        match self.durability {
            JournalDurability::SyncEveryCall => {
                self.foreground_fence(FenceKind::Data, JournalOperation::AppendFence)?;
            }
            JournalDurability::BackgroundFsync => {
                if let Some(p) = &self.pump {
                    p.note_dirty();
                }
            }
        }
        Ok(())
    }
    pub fn fg_barriers(&self) -> u64 {
        self.fg_barriers
    }
    pub fn bg_barriers(&self) -> u64 {
        self.pump
            .as_ref()
            .map(|p| p.bg_barriers.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
    pub fn pump_tid(&self) -> u64 {
        self.pump
            .as_ref()
            .map(|p| p.tid.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
    pub fn prealloc_exhausted(&self) -> u64 {
        self.prealloc_exhausted
    }
    pub fn prealloc_exceeded(&self) -> u64 {
        self.prealloc_exceeded
    }
    pub fn last_explicit_commit_len(&self) -> usize {
        self.last_explicit_commit_len
    }
    pub fn records_end(&self) -> u64 {
        self.write_off
    }
    /// Returns the tracked file capacity in bytes without querying the port.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
}
impl<P: JournalPort> OmsJournal for FileJournal<P> {
    fn apply_effects(&mut self, effects: &[Effect]) -> Result<DurabilityAck> {
        let rec_at = self.records.len();
        let other_at = self.other_effects.len();
        let off_at = self.write_off;
        let mut staged = Vec::new();
        let serialize = (|| -> Result<()> {
            for e in effects {
                match e {
                    Effect::AppendFsync(r) => {
                        serde_json::to_writer(&mut self.buf, r)?;
                        self.buf.push(b'\n');
                        staged.push(r.clone());
                    }
                    other => self.other_effects.push(other.clone()),
                }
            }
            Ok(())
        })();
        if let Err(e) = serialize {
            self.buf.clear();
            self.records.truncate(rec_at);
            self.index.reset();
            self.other_effects.truncate(other_at);
            self.write_off = off_at;
            return Err(e);
        }
        if !staged.is_empty() {
            let bytes = std::mem::take(&mut self.buf);
            let result = self.append_batch(&bytes);
            self.buf = bytes;
            self.buf.clear();
            if let Err(e) = result {
                self.records.truncate(rec_at);
                self.index.reset();
                self.other_effects.truncate(other_at);
                self.write_off = off_at;
                return Err(e);
            }
            self.records.append(&mut staged);
        }
        Ok(self.assurance_ack())
    }
    fn commit_now(&mut self) -> Result<DurabilityAck> {
        let ack = self.foreground_fence(FenceKind::Data, JournalOperation::ExplicitFence)?;
        self.last_explicit_commit_len = self.records.len();
        Ok(ack)
    }
    fn fsync_errors(&self) -> u64 {
        self.pump
            .as_ref()
            .map(|p| p.errors.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
    fn truncated_bytes(&self) -> u64 {
        self.truncated_bytes
    }
    fn records(&self) -> &[JournalRecord] {
        &self.records
    }
    fn other_effects(&self) -> &[Effect] {
        &self.other_effects
    }
    fn has_fill_id(&self, id: &str) -> bool {
        self.index.with(&self.records, |i| i.fill_ids.contains(id))
    }
    fn has_cancel_not_found(&self, client: &str) -> bool {
        self.index
            .with(&self.records, |i| i.cancel_not_found.contains(client))
    }
    fn wire_fill_qty(&self, wire: &str) -> Option<u64> {
        self.index.with(&self.records, |i| {
            i.wire_fills
                .get(wire)
                .map(|m| m.values().fold(0u64, |a, q| a.saturating_add(*q)))
        })
    }
    fn booked_volume(&self) -> u64 {
        self.index.with(&self.records, |i| i.booked_volume)
    }
}
impl<P: JournalPort> Drop for FileJournal<P> {
    fn drop(&mut self) {
        if let Some(p) = &mut self.pump {
            if p.shutdown() {
                self.fg_barriers = self.fg_barriers.saturating_add(1);
            }
        }
        let _ = self.port.truncate(self.write_off);
    }
}

#[derive(Debug)]
pub enum SessionJournal<P: JournalPort> {
    Memory(MemoryJournal),
    File(FileJournal<P>),
}

impl<P: JournalPort> SessionJournal<P> {
    pub fn memory() -> Self {
        Self::Memory(MemoryJournal::new())
    }

    pub fn file(port: Arc<P>, durability: JournalDurability) -> Result<Self> {
        Ok(Self::File(FileJournal::open(port, durability, None)?))
    }
    pub fn file_with_prealloc(
        port: Arc<P>,
        durability: JournalDurability,
        prealloc_bytes: u64,
    ) -> Result<Self> {
        Ok(Self::File(FileJournal::open(
            port,
            durability,
            Some(prealloc_bytes),
        )?))
    }
    pub fn prealloc_exhausted(&self) -> u64 {
        match self {
            Self::Memory(_) => 0,
            Self::File(j) => j.prealloc_exhausted(),
        }
    }

    pub fn drain_backfill_unknown(&mut self) -> Vec<crate::lifecycle::ClientOrderId> {
        match self {
            Self::Memory(j) => drain_backfill_unknown_effects(&mut j.other_effects),
            Self::File(j) => drain_backfill_unknown_effects(&mut j.other_effects),
        }
    }

    pub fn retain_backfill_unknown(
        &mut self,
        mut keep: impl FnMut(&crate::lifecycle::ClientOrderId) -> bool,
    ) {
        let effects = match self {
            Self::Memory(j) => &mut j.other_effects,
            Self::File(j) => &mut j.other_effects,
        };
        effects.retain(|e| match e {
            Effect::BackfillUnknown { client_order_id } => keep(client_order_id),
            _ => true,
        });
    }

    pub fn peek_backfill_unknown(&self) -> Vec<crate::lifecycle::ClientOrderId> {
        match self {
            Self::Memory(j) => peek_backfill_unknown_effects(&j.other_effects),
            Self::File(j) => peek_backfill_unknown_effects(&j.other_effects),
        }
    }

    fn other_effects_mut(&mut self) -> &mut Vec<Effect> {
        match self {
            Self::Memory(j) => &mut j.other_effects,
            Self::File(j) => &mut j.other_effects,
        }
    }

    pub fn drain_authority_effects(&mut self) -> Vec<Effect> {
        drain_authority_effects(self.other_effects_mut())
    }

    pub fn requeue_authority_effects(&mut self, effects: Vec<Effect>) {
        self.other_effects_mut().extend(effects);
    }
}

fn drain_authority_effects(effects: &mut Vec<Effect>) -> Vec<Effect> {
    let mut out = Vec::new();
    effects.retain(|e| match e {
        Effect::RequestAuthorityReconcile { .. }
        | Effect::ReconcileCancel { .. }
        | Effect::BackfillFills { .. } => {
            out.push(e.clone());
            false
        }
        _ => true,
    });
    out
}

fn drain_backfill_unknown_effects(
    effects: &mut Vec<Effect>,
) -> Vec<crate::lifecycle::ClientOrderId> {
    let mut out = Vec::new();
    effects.retain(|e| match e {
        Effect::BackfillUnknown { client_order_id } => {
            out.push(client_order_id.clone());
            false
        }
        _ => true,
    });
    out
}

fn peek_backfill_unknown_effects(effects: &[Effect]) -> Vec<crate::lifecycle::ClientOrderId> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::BackfillUnknown { client_order_id } => Some(client_order_id.clone()),
            _ => None,
        })
        .collect()
}

impl<P: JournalPort> OmsJournal for SessionJournal<P> {
    fn commit_now(&mut self) -> Result<DurabilityAck> {
        match self {
            Self::Memory(j) => j.commit_now(),
            Self::File(j) => j.commit_now(),
        }
    }

    fn fsync_errors(&self) -> u64 {
        match self {
            Self::Memory(j) => j.fsync_errors(),
            Self::File(j) => j.fsync_errors(),
        }
    }

    fn truncated_bytes(&self) -> u64 {
        match self {
            Self::Memory(j) => j.truncated_bytes(),
            Self::File(j) => j.truncated_bytes(),
        }
    }

    fn apply_effects(&mut self, effects: &[Effect]) -> Result<DurabilityAck> {
        match self {
            Self::Memory(j) => j.apply_effects(effects),
            Self::File(j) => j.apply_effects(effects),
        }
    }

    fn records(&self) -> &[JournalRecord] {
        match self {
            Self::Memory(j) => j.records(),
            Self::File(j) => j.records(),
        }
    }

    fn has_fill_id(&self, trade_id: &str) -> bool {
        match self {
            Self::Memory(j) => j.has_fill_id(trade_id),
            Self::File(j) => j.has_fill_id(trade_id),
        }
    }

    fn has_cancel_not_found(&self, client: &str) -> bool {
        match self {
            Self::Memory(j) => j.has_cancel_not_found(client),
            Self::File(j) => j.has_cancel_not_found(client),
        }
    }

    fn wire_fill_qty(&self, wire: &str) -> Option<u64> {
        match self {
            Self::Memory(j) => j.wire_fill_qty(wire),
            Self::File(j) => j.wire_fill_qty(wire),
        }
    }

    fn booked_volume(&self) -> u64 {
        match self {
            Self::Memory(j) => j.booked_volume(),
            Self::File(j) => j.booked_volume(),
        }
    }

    fn other_effects(&self) -> &[Effect] {
        match self {
            Self::Memory(j) => j.other_effects(),
            Self::File(j) => j.other_effects(),
        }
    }
}
