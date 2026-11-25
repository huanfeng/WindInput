//! 代码审查问题的回归测试（多设备模拟）。

use wind_sync::backend::{Backend, BackendError, MemBackend, ObjMeta};
use wind_sync::crypto::{Dek, KdfParams};
use wind_sync::engine::{SyncCtx, SyncReport, sync_once};
use wind_sync::error::SyncError;
use wind_sync::local::{LocalError, LocalStore, MemLocal};
use wind_sync::manifest;
use wind_sync::remote;
use wind_sync::scope::ScopeFlags;
use wind_sync::state::SyncState;

const T0: u64 = 1_795_478_400; // 2026-11-24

struct Dev {
    local: MemLocal,
    state: SyncState,
    dek: Dek,
}

impl Dev {
    fn new(name: &str, dek: &Dek) -> Dev {
        Dev {
            local: MemLocal::new(),
            state: SyncState::new(name),
            dek: dek.clone(),
        }
    }
    fn sync_with(&mut self, b: &MemBackend, ctx: &SyncCtx) -> Result<SyncReport, SyncError> {
        sync_once(b, &mut self.local, &mut self.state, &self.dek, ctx)
    }
    fn sync(&mut self, b: &MemBackend) -> Result<SyncReport, SyncError> {
        let mut ctx = SyncCtx::new(ScopeFlags::ALL, T0);
        ctx.force = true;
        self.sync_with(b, &ctx)
    }
    fn ok(&mut self, b: &MemBackend) -> SyncReport {
        self.sync(b).expect("同步应成功")
    }
}

fn setup(n: usize) -> (MemBackend, Vec<Dev>) {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b, "pw", KdfParams::FAST_TEST).unwrap();
    let devs = (0..n).map(|i| Dev::new(&format!("dev{i}"), &dek)).collect();
    (b, devs)
}

// ───────── 1. 大批量删除护栏 ─────────

#[test]
fn mass_deletion_is_halted_until_confirmed() {
    let (b, mut d) = setup(2);
    for i in 0..30 {
        d[0].local
            .set(&format!("themes/t{i}.json"), &format!("{i}"));
    }
    d[0].ok(&b);
    d[1].ok(&b);
    assert_eq!(d[1].local.files.len(), 30);

    d[0].local.files.clear(); // 用户目录被卸载/迁移
    let gens_before = b.keys(&manifest::devices_prefix());
    let e = d[0].sync(&b).unwrap_err();
    assert!(e.is_mass_deletion(), "{e:?}");
    assert_eq!(
        b.keys(&manifest::devices_prefix()),
        gens_before,
        "不得发墓碑"
    );
    d[1].ok(&b);
    assert_eq!(d[1].local.files.len(), 30, "其它设备不受影响");

    // 用户确认：解除停止态并放行
    d[0].state.retry.clear_halt();
    let mut ctx = SyncCtx::new(ScopeFlags::ALL, T0);
    ctx.force = true;
    ctx.allow_mass_delete = true;
    let r = d[0].sync_with(&b, &ctx).unwrap();
    assert_eq!(r.deleted_remote.len(), 30);
}

#[test]
fn small_deletion_is_not_guarded() {
    let (b, mut d) = setup(1);
    for i in 0..5 {
        d[0].local.set(&format!("themes/t{i}.json"), "x");
    }
    d[0].ok(&b);
    d[0].local.files.clear();
    assert_eq!(d[0].ok(&b).deleted_remote.len(), 5);
}

struct FailList;
impl LocalStore for FailList {
    fn list(&self) -> Result<Vec<String>, LocalError> {
        Err(LocalError("磁盘不可读".into()))
    }
    fn read(&self, _: &str) -> Result<Option<Vec<u8>>, LocalError> {
        Ok(None)
    }
    fn write_batch(&mut self, _: &[(String, Vec<u8>)]) -> Result<(), LocalError> {
        Ok(())
    }
    fn delete(&mut self, _: &[String]) -> Result<(), LocalError> {
        Ok(())
    }
}

#[test]
fn local_list_failure_is_an_error_not_empty() {
    let (b, mut d) = setup(1);
    d[0].local.set("themes/a.json", "x");
    d[0].ok(&b);
    let mut ctx = SyncCtx::new(ScopeFlags::ALL, T0);
    ctx.force = true;
    let dek = d[0].dek.clone();
    let r = sync_once(&b, &mut FailList, &mut d[0].state, &dek, &ctx);
    assert!(matches!(r, Err(SyncError::Local(_))));
    assert!(d[0].state.files.values().all(|e| !e.deleted));
}

// ───────── 2. 崩溃重跑不复用 lamport ─────────

#[test]
fn rerun_after_crash_does_not_reuse_lamport() {
    let (b, mut d) = setup(2);
    d[0].local.set("themes/x.json", "v1");
    let snapshot = d[0].state.clone();
    d[0].ok(&b); // 清单已写远端
    d[1].ok(&b);
    assert_eq!(d[1].local.text("themes/x.json").unwrap(), "v1");
    // 崩溃：状态库没提交；用户又改了内容
    d[0].state = snapshot;
    d[0].local.set("themes/x.json", "v2");
    d[0].ok(&b);
    d[1].ok(&b);
    assert_eq!(d[1].local.text("themes/x.json").unwrap(), "v2");
}

#[test]
fn crash_rerun_with_idle_observer_keeps_lamport_monotonic() {
    let (b, mut d) = setup(2);
    d[0].local.set("themes/x.json", "v1");
    let snapshot = d[0].state.clone();
    d[0].ok(&b);
    d[1].ok(&b); // Z 拉取，无本地改动
    // 抹掉 Z 的清单：只剩 A 自己的清单记录着已用过的 lamport
    for k in b.keys("windinput-sync/v2/devices/dev1/") {
        b.delete(&k).unwrap();
    }
    d[0].state = snapshot;
    d[0].local.set("themes/x.json", "v2");
    d[0].ok(&b);
    assert!(d[0].state.files["desktop/themes/x.json"].lamport >= 2);
    d[1].ok(&b);
    assert_eq!(d[1].local.text("themes/x.json").unwrap(), "v2");
}

// ───────── 3. 并发首次初始化 ─────────

/// 我方 put meta 之后，另一台设备紧接着覆盖它。
struct RacingMeta {
    inner: MemBackend,
}
impl Backend for RacingMeta {
    fn list(&self, p: &str) -> Result<Vec<ObjMeta>, BackendError> {
        self.inner.list(p)
    }
    fn get(&self, k: &str) -> Result<Option<Vec<u8>>, BackendError> {
        self.inner.get(k)
    }
    fn put(&self, k: &str, v: &[u8]) -> Result<(), BackendError> {
        self.inner.put(k, v)?;
        if k == manifest::meta_key() {
            let other =
                wind_sync::crypto::Meta::new(&Dek::generate(), "other", KdfParams::FAST_TEST)
                    .unwrap()
                    .to_json();
            self.inner.put(k, &other)?;
        }
        Ok(())
    }
    fn delete(&self, k: &str) -> Result<(), BackendError> {
        self.inner.delete(k)
    }
}

#[test]
fn concurrent_init_loser_gets_error() {
    let b = RacingMeta {
        inner: MemBackend::new(),
    };
    let r = remote::init_remote(&b, "pw", KdfParams::FAST_TEST);
    assert_eq!(r.unwrap_err(), SyncError::AlreadyInitialized);
}

// ───────── 4. GC 与跳过旧 blob ─────────

#[test]
fn old_existing_blob_is_refreshed_on_upload() {
    let (b, mut d) = setup(1);
    b.set_now(1000);
    d[0].local.set("themes/x.json", "v1");
    let snapshot = d[0].state.clone();
    d[0].ok(&b);
    d[0].state = snapshot; // 状态库没提交，重跑会再次上传同一 blob
    b.set_now(T0);
    d[0].ok(&b);
    let blobs = b.list(&manifest::blobs_prefix()).unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].modified, Some(T0), "旧 blob 要刷新时间，防被 GC");
}

fn blobs_listed(b: &MemBackend) -> usize {
    b.calls()
        .list_prefixes
        .iter()
        .filter(|p| **p == manifest::blobs_prefix())
        .count()
}

#[test]
fn few_blobs_use_stat_not_list() {
    let (b, mut d) = setup(1);
    for i in 0..3 {
        d[0].local
            .set(&format!("themes/t{i}.json"), &format!("v{i}"));
    }
    b.reset_calls();
    d[0].ok(&b);
    assert_eq!(blobs_listed(&b), 0, "少量 blob 不应列举 blobs/");
    assert_eq!(b.calls().stat, 3, "每个待传 blob 一次 stat");
    assert_eq!(b.keys(&manifest::blobs_prefix()).len(), 3);
}

#[test]
fn many_blobs_use_one_list_not_stat() {
    let (b, mut d) = setup(1);
    for i in 0..40 {
        d[0].local
            .set(&format!("themes/t{i}.json"), &format!("v{i}"));
    }
    b.reset_calls();
    d[0].ok(&b);
    assert_eq!(blobs_listed(&b), 1, "超过阈值一次 list");
    assert_eq!(b.calls().stat, 0);
    assert_eq!(b.keys(&manifest::blobs_prefix()).len(), 40);
}

#[test]
fn fresh_existing_blob_is_not_reuploaded_by_stat_path() {
    let (b, mut d) = setup(1);
    b.set_now(T0);
    d[0].local.set("themes/x.json", "v1");
    let snapshot = d[0].state.clone();
    d[0].ok(&b);
    d[0].state = snapshot;
    let blob_puts = b.calls().put;
    b.reset_calls();
    d[0].ok(&b);
    assert_eq!(b.calls().stat, 1);
    // 只可能多出清单的 put，blob 不重传
    assert!(b.calls().put <= 1, "{:?} (前次 {blob_puts})", b.calls());
    let blobs = b.list(&manifest::blobs_prefix()).unwrap();
    assert_eq!(blobs[0].modified, Some(T0));
}

#[test]
fn old_existing_blob_is_refreshed_on_list_path() {
    // 超过阈值走 list 路径：旧 blob 同样要刷新
    let (b, mut d) = setup(1);
    b.set_now(1000);
    for i in 0..40 {
        d[0].local
            .set(&format!("themes/t{i}.json"), &format!("v{i}"));
    }
    let snapshot = d[0].state.clone();
    d[0].ok(&b);
    d[0].state = snapshot;
    b.set_now(T0);
    b.reset_calls();
    d[0].ok(&b);
    assert_eq!(blobs_listed(&b), 1);
    let blobs = b.list(&manifest::blobs_prefix()).unwrap();
    assert_eq!(blobs.len(), 40);
    assert!(
        blobs.iter().all(|o| o.modified == Some(T0)),
        "旧 blob 要刷新"
    );
}

// ───────── 5. 冲突副本不覆盖 ─────────

#[test]
fn second_same_day_conflict_does_not_overwrite_copy() {
    let (b, mut d) = setup(2);
    d[0].local.set("themes/x.json", "v0");
    d[0].ok(&b);
    d[1].ok(&b);
    d[0].local.set("themes/x.json", "from0");
    d[1].local.set("themes/x.json", "from1");
    d[0].ok(&b);
    d[1].ok(&b);
    d[0].ok(&b);
    d[0].local.set("themes/x.json", "from0b");
    d[0].ok(&b);
    d[1].local.set("themes/x.json", "from1b");
    d[1].ok(&b);
    let texts: Vec<String> = d[1]
        .local
        .files
        .iter()
        .filter(|(k, _)| k.contains("conflict"))
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
        .collect();
    assert!(texts.contains(&"from0".to_string()), "{texts:?}");
    assert!(texts.contains(&"from0b".to_string()), "{texts:?}");
}

// ───────── 6. absorbed 传递 ─────────

#[test]
fn absorbed_is_transitive_across_later_edits() {
    let (b, mut d) = setup(3);
    d[0].local.set("themes/x.json", "v0");
    d[0].ok(&b);
    d[1].ok(&b);
    d[2].ok(&b);
    d[0].local.set("themes/x.json", "vA");
    d[0].ok(&b);
    d[2].local.set("themes/x.json", "vC");
    d[2].ok(&b); // 吸收 A，存副本
    d[2].local.set("themes/x.json", "vE");
    d[2].ok(&b);
    let r = d[1].ok(&b);
    assert!(r.conflicts.is_empty(), "{:?}", r.conflicts);
    assert_eq!(d[1].local.text("themes/x.json").unwrap(), "vE");
    assert_eq!(
        d[1].local.files.len(),
        2,
        "只有原文件和 C 当时生成的那份副本"
    );
}

// ───────── 8. 键校验 ─────────

#[test]
fn reset_skips_traversal_keys() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b, "p", KdfParams::FAST_TEST).unwrap();
    b.raw_put("windinput-sync/v2/../keep", vec![1]);
    b.raw_put("windinput-sync/v2/blobs/ok", vec![1]);
    remote::reset_remote(&b, &dek, "p2", KdfParams::FAST_TEST).unwrap();
    assert!(b.raw_get("windinput-sync/v2/../keep").is_some());
    assert!(b.raw_get("windinput-sync/v2/blobs/ok").is_none());
}
