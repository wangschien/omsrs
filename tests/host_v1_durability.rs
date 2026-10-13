use omsrs::host_v1::*;
use omsrs::lifecycle::{ClientOrderId, Effect, JournalRecord};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Debug)]
struct FilePort {
    file: File,
    writes: AtomicU64,
    data: AtomicU64,
    fail_write: AtomicU64,
    fail_fence: AtomicU64,
    operations: std::sync::Mutex<Vec<JournalOperation>>,
}
impl JournalPort for FilePort {
    fn read_all(&self) -> std::io::Result<Vec<u8>> {
        let mut bytes = vec![0; self.len()? as usize];
        self.file.read_exact_at(&mut bytes, 0)?;
        Ok(bytes)
    }
    fn len(&self) -> std::io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }
    fn append_ordered(&self, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
        if self.fail_write.swap(0, Ordering::SeqCst) > 0 {
            return Err(std::io::Error::other("client-injected write failure"));
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.file.write_all_at(bytes, offset)
    }
    fn truncate(&self, keep: u64) -> std::io::Result<()> {
        self.file.set_len(keep)
    }
    fn fence(&self, kind: FenceKind) -> std::io::Result<()> {
        if self
            .fail_fence
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(std::io::Error::other("client-injected fence failure"));
        }
        match kind {
            FenceKind::Data => {
                self.data.fetch_add(1, Ordering::SeqCst);
                self.file.sync_data()
            }
            FenceKind::All => self.file.sync_all(),
        }
    }
    fn observe(&self, event: JournalDiagnostic<'_>) {
        match event {
            JournalDiagnostic::BackgroundFenceFailure { operation, .. }
            | JournalDiagnostic::ShutdownFenceFailure { operation, .. } => {
                self.operations.lock().unwrap().push(operation)
            }
            _ => {}
        }
    }
    fn current_thread_id(&self) -> u64 {
        std::fs::read_to_string("/proc/thread-self/stat")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
            .unwrap_or(0)
    }
}
fn port() -> Arc<FilePort> {
    Arc::new(FilePort {
        file: tempfile::tempfile().unwrap(),
        writes: AtomicU64::new(0),
        data: AtomicU64::new(0),
        fail_write: AtomicU64::new(0),
        fail_fence: AtomicU64::new(0),
        operations: std::sync::Mutex::new(Vec::new()),
    })
}
fn effects() -> Vec<Effect> {
    vec![
        Effect::AppendFsync(JournalRecord::SubmitUnknown),
        Effect::BackfillUnknown {
            client_order_id: ClientOrderId("client-a".into()),
        },
        Effect::AppendFsync(JournalRecord::SubmitUnknown),
    ]
}

#[test]
fn ordered_batch_one_write_one_fence_and_restart_bytes() {
    let p = port();
    let mut j = FileJournal::open(p.clone(), JournalDurability::SyncEveryCall, Some(4096)).unwrap();
    let writes = p.writes.load(Ordering::SeqCst);
    let base = j.fg_barriers();
    assert!(matches!(
        j.apply_effects(&effects()).unwrap(),
        DurabilityAck::StableStorage { .. }
    ));
    assert_eq!(p.writes.load(Ordering::SeqCst) - writes, 1);
    assert_eq!(j.fg_barriers() - base, 1);
    assert_eq!(p.data.load(Ordering::SeqCst), 1);
    assert_eq!(j.len(), 2);
    assert_eq!(j.other_effects().len(), 1);
    let end = j.records_end();
    let bytes = p.read_all().unwrap()[..end as usize].to_vec();
    drop(j);
    assert_eq!(p.read_all().unwrap(), bytes);
    let r = recover_journal(p.as_ref(), false).unwrap();
    assert_eq!(r.records.len(), 2);
    assert_eq!(r.records_end, end);
}
#[test]
fn write_and_fence_failure_do_not_publish_records_effects_or_offsets() {
    for fence in [false, true] {
        let p = port();
        let mut j =
            FileJournal::open(p.clone(), JournalDurability::SyncEveryCall, Some(4096)).unwrap();
        if fence {
            p.fail_fence.store(1, Ordering::SeqCst)
        } else {
            p.fail_write.store(1, Ordering::SeqCst)
        };
        let error = j.apply_effects(&effects()).unwrap_err();
        let expected = if fence {
            JournalOperation::AppendFence
        } else {
            JournalOperation::AppendWrite
        };
        assert!(matches!(error, JournalError::Io { operation, .. } if operation == expected));
        assert_eq!(j.records_end(), 0);
        assert!(j.records().is_empty());
        assert!(j.other_effects().is_empty());
        j.apply_effects(&[Effect::AppendFsync(JournalRecord::SubmitUnknown)])
            .unwrap();
        drop(j);
        assert_eq!(recover_journal(p.as_ref(), false).unwrap().records.len(), 1);
    }
}
#[test]
fn background_acceptance_is_page_cache_and_explicit_commit_is_stable() {
    let p = port();
    let mut j =
        FileJournal::open(p.clone(), JournalDurability::BackgroundFsync, Some(4096)).unwrap();
    let base = j.fg_barriers();
    assert!(matches!(
        j.apply_effects(&effects()).unwrap(),
        DurabilityAck::PageCache { .. }
    ));
    assert_eq!(j.fg_barriers(), base);
    assert!(matches!(
        j.commit_now().unwrap(),
        DurabilityAck::StableStorage { .. }
    ));
    assert_eq!(j.fg_barriers(), base + 1);
    assert_eq!(j.last_explicit_commit_len(), 2);
}
#[test]
fn background_failure_rearms_retry_and_retains_sticky_error() {
    let p = port();
    let mut j =
        FileJournal::open(p.clone(), JournalDurability::BackgroundFsync, Some(4096)).unwrap();
    p.fail_fence.store(2, Ordering::SeqCst);
    j.apply_effects(&effects()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while j.bg_barriers() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(j.bg_barriers() > 0);
    assert_eq!(j.fsync_errors(), 2);
    assert_eq!(
        *p.operations.lock().unwrap(),
        vec![JournalOperation::BackgroundFence; 2]
    );
    assert!(j.pump_tid() > 0);
    assert!(matches!(
        j.commit_now().unwrap(),
        DurabilityAck::Uncertain {
            fence_errors: 2,
            ..
        }
    ));
    assert!(matches!(
        j.apply_effects(&[]).unwrap(),
        DurabilityAck::Uncertain {
            fence_errors: 2,
            ..
        }
    ));
    assert_eq!(j.fsync_errors(), 2);
    drop(j);
    assert_eq!(recover_journal(p.as_ref(), false).unwrap().records.len(), 2);
}
#[test]
fn startup_reservation_resume_and_exhaustion_preserve_prefix() {
    let p = port();
    let mut j = FileJournal::open(p.clone(), JournalDurability::SyncEveryCall, Some(64)).unwrap();
    assert_eq!(p.len().unwrap(), 64);
    assert_eq!(j.prealloc_exhausted(), 0);
    j.apply_effects(&effects()).unwrap();
    let end = j.records_end();
    drop(j);
    let mut j = FileJournal::open(p.clone(), JournalDurability::SyncEveryCall, Some(64)).unwrap();
    assert_eq!(j.records_end(), end);
    assert_eq!(p.len().unwrap(), end + 64);
    assert_eq!(j.prealloc_exhausted(), 0);
    for _ in 0..4 {
        j.apply_effects(&effects()).unwrap();
    }
    assert_eq!(j.prealloc_exhausted(), 1);
    assert_eq!(j.prealloc_exceeded(), 1);
    drop(j);
    assert_eq!(
        recover_journal(p.as_ref(), false).unwrap().records.len(),
        10
    );
        use syn::{Expr, ImplItem, Item, Stmt, Type};
        let unconditional = |attrs: &[syn::Attribute]| attrs.iter().all(|a|
            !["cfg", "cfg_attr", "path"].iter().any(|name| a.path().is_ident(name)));
        let expression = |source: &str| syn::parse_str::<Expr>(source).expect("valid expected expression");
        let statements = |source: &str| syn::parse_str::<syn::Block>(&format!("{{{source}}}"))
            .expect("valid expected statements").stmts;
        let module_is = |file: &syn::File, name: &str| {
            let found: Vec<_> = file.items.iter().filter_map(|item| match item {
                Item::Mod(m) if m.ident == name => Some(m), _ => None,
            }).collect();
            found.len() == 1 && unconditional(&found[0].attrs) && found[0].content.is_none()
        };
        let normalize_use = |mut tree: syn::UseTree| {
            let mut work = vec![&mut tree];
            while let Some(node) = work.pop() {
                match node {
                    syn::UseTree::Path(p) => work.push(&mut p.tree),
                    syn::UseTree::Group(g) => {
                        g.items = g.items.clone().into_iter().collect();
                        work.extend(g.items.iter_mut());
                    }
                    _ => {}
                }
            }
            drop(work); tree
        };
        let use_is = |file: &syn::File, expected: &str| {
            let expected = syn::parse_str::<syn::ItemUse>(expected).expect("valid expected import");
            file.items.iter().filter(|item| matches!(item, Item::Use(u)
                if unconditional(&u.attrs) && u.vis == expected.vis && normalize_use(u.tree.clone()) == normalize_use(expected.tree.clone()))).count() == 1
        };
        let method = |file: &syn::File, header: &str, name: &str, signature: &str| {
            let expected = syn::parse_str::<syn::ItemImpl>(&format!("{header} {{}}"))
                .expect("valid expected impl");
            let mut signature = syn::parse_str::<syn::ImplItemFn>(&format!("{signature} {{}}"))
                .expect("valid expected method").sig;
            signature.inputs = signature.inputs.into_iter().collect();
            let found: Vec<_> = file.items.iter().filter_map(|item| match item {
                Item::Impl(i) if i.self_ty == expected.self_ty && i.trait_ == expected.trait_
                    && i.generics == expected.generics => Some(i), _ => None,
            }).flat_map(|i| i.items.iter().filter_map(move |item| match item {
                ImplItem::Fn(f) if f.sig.ident == name => Some((i, f)), _ => None,
            })).collect();
            let actual_signature = found.first().map(|(_, f)| {
                let mut sig = f.sig.clone(); sig.inputs = sig.inputs.into_iter().collect(); sig
            });
            if found.len() != 1 || !unconditional(&found[0].0.attrs)
                || !unconditional(&found[0].1.attrs) || actual_signature.as_ref() != Some(&signature) {
                None
            } else { Some(found[0].1.clone()) }
        };
        let field_is = |file: &syn::File, owner: &str, name: &str, ty: &str| {
            let found: Vec<_> = file.items.iter().filter_map(|item| match item {
                Item::Struct(s) if s.ident == owner => Some(s), _ => None,
            }).collect();
            if found.len() != 1 || !unconditional(&found[0].attrs) { return false; }
            let fields: Vec<_> = found[0].fields.iter().filter(|f| f.ident.as_ref().is_some_and(|id| id == name)).collect();
            fields.len() == 1 && unconditional(&fields[0].attrs)
                && fields[0].ty == syn::parse_str::<Type>(ty).expect("valid expected field type")
                && matches!(fields[0].vis, syn::Visibility::Inherited)
        };
    let root = syn::parse_file(include_str!("../src/lib.rs")).expect("parse public module graph");
    let host = syn::parse_file(include_str!("../src/host_v1/mod.rs")).expect("parse persistence module graph");
    let source = syn::parse_file(include_str!("../src/host_v1/persistence.rs")).expect("parse executing persistence");
    let types = syn::parse_file(include_str!("../src/host_v1/types.rs")).expect("parse diagnostic types");
    let graph = module_is(&root, "host_v1") && module_is(&host, "persistence")
        && module_is(&host, "types") && use_is(&host, "pub use persistence::*;")
        && use_is(&host, "pub use types::*;")
        && use_is(&source, "use super::types::{DurabilityAck, JournalDiagnostic, JournalDurability, JournalError, JournalOperation, RecoveredJournal, RecoveryDiagnostic, RecoveryReason};")
        && use_is(&source, "use std::sync::{atomic::{AtomicU64, Ordering}, Arc, Condvar, Mutex, MutexGuard};")
        && ["write_off", "capacity", "prealloc_exhausted", "prealloc_exceeded", "startup_prealloc_bytes"]
            .iter().all(|name| field_is(&source, "FileJournal", name, "u64"));
    let header = "impl<P: JournalPort> FileJournal<P>";
    let ensure = method(&source, header, "ensure_capacity", "fn ensure_capacity(&mut self, need: u64) -> Result<()>")
        .expect("unique synchronous capacity path");
    let append = method(&source, header, "append_batch", "fn append_batch(&mut self, bytes: &[u8]) -> Result<()>")
        .expect("unique synchronous append path");
    let apply = method(&source, "impl<P: JournalPort> OmsJournal for FileJournal<P>", "apply_effects",
        "fn apply_effects(&mut self, effects: &[Effect]) -> Result<DurabilityAck>")
        .expect("unique concrete journal implementation");
    let consts: Vec<_> = source.items.iter().filter_map(|item| match item {
        Item::Const(c) if c.ident == "PREALLOC_CHUNK" => Some(c), _ => None,
    }).collect();
    let chunk = consts.len() == 1 && unconditional(&consts[0].attrs)
        && consts[0].ty.as_ref() == &syn::parse_str::<Type>("u64").unwrap()
        && consts[0].expr.as_ref() == &expression("1 << 20");
    let expected_prefix = statements("let want = self.write_off.saturating_add(need); if want < self.capacity { return Ok(()); } self.pad_to(want.max(self.capacity).saturating_add(PREALLOC_CHUNK))?; self.prealloc_exhausted = self.prealloc_exhausted.saturating_add(1); self.prealloc_exceeded = self.prealloc_exceeded.saturating_add(1);");
    let payload = expression("self.port.observe(JournalDiagnostic::PreallocationExhausted { count: self.prealloc_exhausted, used_bytes: self.write_off, startup_prealloc_bytes: self.startup_prealloc_bytes, exceeded_count: self.prealloc_exceeded, })");
    let direct_observe = ensure.block.stmts.get(5).is_some_and(|s| matches!(s, Stmt::Expr(Expr::MethodCall(call), Some(_))
        if call.receiver.as_ref() == &expression("self.port") && call.method == "observe" && call.args.len() == 1
        && matches!(&call.args[0], Expr::Struct(v) if v.path == syn::parse_str::<syn::Path>("JournalDiagnostic::PreallocationExhausted").unwrap() && v.rest.is_none())));
    let staged_branches: Vec<_> = apply.block.stmts.iter().filter_map(|s| match s {
        Stmt::Expr(Expr::If(i), None) if i.cond.as_ref() == &expression("!staged.is_empty()") => Some(i), _ => None,
    }).collect();
    let publication = staged_branches.len() == 1 && staged_branches[0].else_branch.is_none()
        && staged_branches[0].then_branch.stmts == statements("let bytes = std::mem::take(&mut self.buf); let result = self.append_batch(&bytes); self.buf = bytes; self.buf.clear(); if let Err(e) = result { self.records.truncate(rec_at); self.index.reset(); self.other_effects.truncate(other_at); self.write_off = off_at; return Err(e); } self.records.append(&mut staged);");
    let n1 = graph && chunk && ensure.block.stmts.len() == 7
        && ensure.block.stmts[..5] == expected_prefix
        && direct_observe && ensure.block.stmts.last() == statements("Ok(())").last()
        && append.block.stmts == statements("self.ensure_capacity(bytes.len() as u64)?; self.port.append_ordered(self.write_off, bytes).map_err(io_error(JournalOperation::AppendWrite))?; self.write_off += bytes.len() as u64; match self.durability { JournalDurability::SyncEveryCall => { self.foreground_fence(FenceKind::Data, JournalOperation::AppendFence)?; } JournalDurability::BackgroundFsync => { if let Some(p) = &self.pump { p.note_dirty(); } } } Ok(())")
        && publication;
    let diagnostic_enums: Vec<_> = types.items.iter().filter_map(|item| match item {
        Item::Enum(e) if e.ident == "JournalDiagnostic" => Some(e), _ => None,
    }).collect();
    let diagnostic_fields = diagnostic_enums.len() == 1 && unconditional(&diagnostic_enums[0].attrs)
        && diagnostic_enums[0].variants.iter().filter(|v| v.ident == "PreallocationExhausted").count() == 1
        && diagnostic_enums[0].variants.iter().find(|v| v.ident == "PreallocationExhausted").is_some_and(|v|
            unconditional(&v.attrs) && matches!(&v.fields, syn::Fields::Named(f) if f.named.iter().map(|f|
                (f.ident.as_ref().unwrap().to_string(), f.ty.clone())).collect::<Vec<_>>() ==
                ["count", "used_bytes", "startup_prealloc_bytes", "exceeded_count"].iter().map(|name|
                    (name.to_string(), syn::parse_str::<Type>("u64").unwrap())).collect::<Vec<_>>()));
    let port_traits: Vec<_> = source.items.iter().filter_map(|item| match item {
        Item::Trait(t) if t.ident == "JournalPort" => Some(t), _ => None,
    }).collect();
    let expected_observe = syn::parse_str::<syn::TraitItemFn>("fn observe(&self, diagnostic: JournalDiagnostic<'_>);").unwrap();
    let observe_trait = port_traits.len() == 1 && unconditional(&port_traits[0].attrs)
        && port_traits[0].items.iter().filter(|item| matches!(item, syn::TraitItem::Fn(f) if f.sig.ident == "observe")).count() == 1
        && port_traits[0].items.iter().any(|item| matches!(item, syn::TraitItem::Fn(f)
            if unconditional(&f.attrs) && f.sig == expected_observe.sig && f.default.is_none()));
    let n2 = n1 && diagnostic_fields && observe_trait
        && ensure.block.stmts.get(5) == Some(&Stmt::Expr(payload, Some(Default::default())));
    let pad = method(&source, header, "pad_to", "fn pad_to(&mut self, target: u64) -> Result<()>")
        .expect("unique shared synchronous padding");
    let fence = method(&source, header, "foreground_fence", "fn foreground_fence(&mut self, kind: FenceKind, operation: JournalOperation) -> Result<DurabilityAck>")
        .expect("unique foreground fence");
    let recovered = method(&source, header, "from_recovered", "fn from_recovered(port: Arc<P>, capacity: u64, recovered: RecoveredJournal, durability: JournalDurability, prealloc: Option<u64>) -> Result<Self>")
        .expect("unique recovered owner");
    let open = method(&source, header, "open", "pub fn open(port: Arc<P>, durability: JournalDurability, prealloc: Option<u64>) -> Result<Self>")
        .expect("unique direct opener");
    let open_source = method(&source, header, "open_source", "pub fn open_source<S: JournalSource<Port = P>>(source: &S, durability: JournalDurability, prealloc: Option<u64>) -> Result<Self>")
        .expect("unique resource opener");
    let init = recovered.block.stmts.get(2).is_some_and(|s| matches!(s, Stmt::Local(l)
        if matches!(&l.pat, syn::Pat::Ident(p) if p.ident == "j" && p.mutability.is_some())
        && l.init.as_ref().is_some_and(|i| matches!(i.expr.as_ref(), Expr::Struct(v)
            if v.path.is_ident("Self") && v.rest.is_none() && v.fields.iter().filter(|f| matches!(&f.member,
                syn::Member::Named(n) if n == "startup_prealloc_bytes")).count() == 1
            && v.fields.iter().any(|f| matches!(&f.member, syn::Member::Named(n) if n == "startup_prealloc_bytes")
                && f.expr == expression("prealloc.unwrap_or(0)"))))));
    let startup = statements("match prealloc { Some(bytes) => { let target = j.write_off.saturating_add(bytes); if j.capacity < target { j.pad_to(target)?; } } None => { if j.write_off >= j.capacity { j.pad_to(j.write_off.max(j.capacity).saturating_add(PREALLOC_CHUNK))?; } } } if durability == JournalDurability::BackgroundFsync { j.pump = Some(FsyncPump::start(j.port.clone())?); } Ok(j)");
    let pad_expected = statements("let mut off = self.capacity; let pad = vec![b'\\n'; 64 * 1024]; while off < target { let n = (pad.len() as u64).min(target - off) as usize; self.port.append_ordered(off, &pad[..n]).map_err(io_error(JournalOperation::PaddingWrite))?; off += n as u64; } self.foreground_fence(FenceKind::All, JournalOperation::PaddingFence)?; self.capacity = target; Ok(())");
    let fence_expected = statements("let _fence_guard = self.pump.as_ref().map(|p| lock(&p.state.2)); if let Err(source) = self.port.fence(kind) { self.foreground_fence_errors = self.foreground_fence_errors.saturating_add(1); return Err(JournalError::Io { operation, source }); } self.fg_barriers = self.fg_barriers.saturating_add(1); self.stable_end = Some(self.write_off); Ok(self.assurance_ack())");
    let fence_enums: Vec<_> = source.items.iter().filter_map(|item| match item { Item::Enum(e) if e.ident == "FenceKind" => Some(e), _ => None }).collect();
    let n3 = n1 && n2 && init && recovered.block.stmts.len() == 6
        && recovered.block.stmts[3..] == startup
        && pad.block.stmts == pad_expected && fence.block.stmts == fence_expected
        && open.block.stmts == statements("let recovered = recover_journal(port.as_ref(), true)?; let capacity = port.len().map_err(io_error(JournalOperation::Metadata))?; Self::from_recovered(port, capacity, recovered, durability, prealloc)")
        && open_source.block.stmts == statements("source.prepare_resource().map_err(io_error(JournalOperation::PrepareResource))?; let recovered = recover_journal(source, true)?; let port = source.open_writer().map_err(io_error(JournalOperation::OpenWriter))?; let capacity = port.len().map_err(io_error(JournalOperation::Metadata))?; let port = Arc::new(port); Self::from_recovered(port, capacity, recovered, durability, prealloc)")
        && fence_enums.len() == 1 && unconditional(&fence_enums[0].attrs)
        && fence_enums[0].variants.iter().map(|v| v.ident.to_string()).collect::<Vec<_>>() == ["Data", "All"];
    assert!(n1, "preallocation growth is eager and ordered");
    assert!(n2, "growth reports the exact neutral payload after padding");
    assert!(n3, "startup and growth fence All before publishing capacity");
}
#[test]
fn prepared_transaction_rejects_stale_prefix_and_foreign_owner() {
    let p = port();
    let mut j = FileJournal::open(p, JournalDurability::SyncEveryCall, Some(4096)).unwrap();
    let txn = j.prepare_effects(&effects()).unwrap();
    j.apply_effects(&effects()).unwrap();
    assert!(matches!(
        j.commit_prepared(txn),
        Err(JournalError::StalePreparation)
    ));
    let mut a = FileJournal::open(port(), JournalDurability::SyncEveryCall, Some(4096)).unwrap();
    let mut b = FileJournal::open(port(), JournalDurability::SyncEveryCall, Some(4096)).unwrap();
    let txn = a.prepare_effects(&effects()).unwrap();
    assert!(matches!(
        b.commit_prepared(txn),
        Err(JournalError::StalePreparation)
    ));
    assert_eq!(b.len(), 0);
    // A readable prefix with spare padding has not acquired a fence certificate.
    let p = port();
    let mut bytes = serde_json::to_vec(&JournalRecord::SubmitUnknown).unwrap();
    bytes.extend_from_slice(b"\n\n\n");
    p.append_ordered(0, &bytes).unwrap();
    let mut j = FileJournal::open(p.clone(), JournalDurability::SyncEveryCall, Some(0)).unwrap();
    assert_eq!(j.fg_barriers(), 0);
    assert!(matches!(
        j.apply_effects(&[]).unwrap(),
        DurabilityAck::PageCache { .. }
    ));
    let other = [Effect::BackfillUnknown {
        client_order_id: ClientOrderId("client-b".into()),
    }];
    let txn = j.prepare_effects(&other).unwrap();
    j.apply_effects(&other).unwrap();
    assert!(matches!(
        j.commit_prepared(txn).unwrap(),
        DurabilityAck::PageCache { .. }
    ));
    assert_eq!(j.other_effects().len(), 2);
    assert_eq!(j.fg_barriers(), 0);
    let abandoned = j.prepare_effects(&effects()).unwrap();
    drop(abandoned);
    assert_eq!(j.len(), 1);
    let txn = j.prepare_effects(&effects()).unwrap();
    assert!(matches!(
        j.commit_prepared(txn).unwrap(),
        DurabilityAck::StableStorage { .. }
    ));
    assert_eq!(j.len(), 3);
    let records_before = j.len();
    let effects_before = j.other_effects().len();
    let offset_before = j.records_end();
    let txn = j.prepare_effects(&effects()).unwrap();
    p.fail_write.store(1, Ordering::SeqCst);
    assert!(matches!(
        j.commit_prepared(txn),
        Err(JournalError::Io {
            operation: JournalOperation::AppendWrite,
            ..
        })
    ));
    assert_eq!(
        (j.len(), j.other_effects().len(), j.records_end()),
        (records_before, effects_before, offset_before)
    );
    let txn = j.prepare_effects(&effects()).unwrap();
    j.commit_prepared(txn).unwrap();
    assert_eq!(j.len(), records_before + 2);
    // A failed foreground fence stays uncertain without changing legacy bg count.
    p.fail_fence.store(1, Ordering::SeqCst);
    assert!(matches!(
        j.commit_now(),
        Err(JournalError::Io {
            operation: JournalOperation::ExplicitFence,
            ..
        })
    ));
    assert_eq!(j.fsync_errors(), 0);
    assert!(matches!(
        j.commit_now().unwrap(),
        DurabilityAck::Uncertain {
            fence_errors: 1,
            ..
        }
    ));
}

#[test]
fn resource_open_preserves_prepare_recovery_repair_writer_order() {
    #[derive(Debug)]
    struct Source {
        port: Arc<FilePort>,
        events: std::sync::Mutex<Vec<&'static str>>,
    }
    impl JournalRecoveryPort for Source {
        fn read_recovery_bytes(&self) -> std::io::Result<Vec<u8>> {
            self.events.lock().unwrap().push("read");
            self.port.read_all()
        }
        fn truncate_recovery_prefix(&self, keep: u64) -> std::io::Result<()> {
            self.events.lock().unwrap().push("truncate");
            self.port.truncate(keep)
        }
        fn observe_recovery(&self, _event: RecoveryDiagnostic<'_>) {
            self.events.lock().unwrap().push("diagnostic");
        }
        fn fence_recovery(&self) -> std::io::Result<()> {
            self.events.lock().unwrap().push("repair-fence");
            self.port.fence(FenceKind::All)
        }
    }
    impl JournalSource for Source {
        type Port = FilePort;
        fn prepare_resource(&self) -> std::io::Result<()> {
            self.events.lock().unwrap().push("prepare");
            Ok(())
        }
        fn open_writer(&self) -> std::io::Result<FilePort> {
            self.events.lock().unwrap().push("writer");
            Ok(FilePort {
                file: self.port.file.try_clone()?,
                writes: AtomicU64::new(0),
                data: AtomicU64::new(0),
                fail_write: AtomicU64::new(0),
                fail_fence: AtomicU64::new(0),
                operations: std::sync::Mutex::new(Vec::new()),
            })
        }
    }
    let source = Source {
        port: port(),
        events: std::sync::Mutex::new(Vec::new()),
    };
    source.port.append_ordered(0, b"{\n").unwrap();
    let journal =
        FileJournal::open_source(&source, JournalDurability::SyncEveryCall, Some(128)).unwrap();
    assert_eq!(
        *source.events.lock().unwrap(),
        vec![
            "prepare",
            "read",
            "diagnostic",
            "truncate",
            "repair-fence",
            "writer"
        ]
    );
    drop(journal);
    source.events.lock().unwrap().clear();
    source
        .port
        .append_ordered(0, b"{\"FutureRecord\":{}}\n")
        .unwrap();
    assert!(matches!(
        FileJournal::open_source(&source, JournalDurability::SyncEveryCall, Some(128)),
        Err(JournalError::UnknownVariant { .. })
    ));
    assert_eq!(*source.events.lock().unwrap(), vec!["prepare", "read"]);
}
