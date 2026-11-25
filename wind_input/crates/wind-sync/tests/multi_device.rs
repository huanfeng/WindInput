//! 多设备模拟：多个 LocalStore + SyncState 共享一个 MemBackend。

use wind_sync::backend::{BackendError, MemBackend};
use wind_sync::crypto::{Dek, KdfParams};
use wind_sync::engine::{SyncCtx, SyncReport, sync_once};
use wind_sync::error::SyncError;
use wind_sync::local::MemLocal;
use wind_sync::manifest;
use wind_sync::remote;
use wind_sync::scope::ScopeFlags;
use wind_sync::state::SyncState;

fn test_ns() -> wind_sync::scope::Namespace {
    wind_sync::scope::Namespace::new(wind_sync::DeviceClass::Desktop, "g")
}

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
    fn sync(&mut self, b: &MemBackend) -> Result<SyncReport, SyncError> {
        let mut ctx = SyncCtx::new(ScopeFlags::ALL, T0, test_ns());
        ctx.force = true;
        sync_once(b, &mut self.local, &mut self.state, &self.dek, &ctx)
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

const H: &str = "---\nname: user\ncolumns: [code, text, weight]\n...\n";
fn dict(rows: &[&str]) -> String {
    format!("{H}{}\n", rows.join("\n"))
}
const UD: &str = "data/pinyin/user.dict.yaml";

#[test]
fn one_way_propagation() {
    let (b, mut d) = setup(2);
    d[0].local.set("config.toml", "a = 1\n");
    d[0].local.set("themes/t.json", "{}");
    let r = d[0].ok(&b);
    assert_eq!(r.uploaded.len(), 2);
    let r = d[1].ok(&b);
    assert_eq!(r.downloaded.len(), 2);
    assert_eq!(d[1].local.text("config.toml").unwrap(), "a = 1\n");
    assert_eq!(d[1].local.batches, 1, "一批写入合并成一次");
    // 再同步一轮：无动作、不写新清单
    let g = d[1].state.gen_;
    let r = d[1].ok(&b);
    assert!(r.uploaded.is_empty() && r.downloaded.is_empty() && r.manifest_gen.is_none());
    assert_eq!(d[1].state.gen_, g);
}

#[test]
fn bidirectional_different_files() {
    let (b, mut d) = setup(2);
    d[0].local.set("config.toml", "a = 1\n");
    d[0].ok(&b);
    d[1].ok(&b);
    d[0].local.set("themes/a.json", "A");
    d[1].local.set("themes/b.json", "B");
    d[0].ok(&b);
    d[1].ok(&b);
    d[0].ok(&b);
    for dev in &d {
        assert_eq!(dev.local.text("themes/a.json").as_deref(), Some("A"));
        assert_eq!(dev.local.text("themes/b.json").as_deref(), Some("B"));
    }
}

#[test]
fn dict_double_edit_merges() {
    let (b, mut d) = setup(2);
    d[0].local.set(UD, &dict(&["a\t啊\t1", "b\t吧\t1"]));
    d[0].ok(&b);
    d[1].ok(&b);
    // 同时：0 加 c、改 a 权重 5；1 加 d、改 a 权重 9、删 b
    d[0].local
        .set(UD, &dict(&["a\t啊\t5", "b\t吧\t1", "c\t从\t1"]));
    d[1].local.set(UD, &dict(&["a\t啊\t9", "d\t的\t1"]));
    d[0].ok(&b);
    let r = d[1].ok(&b);
    assert_eq!(r.merged, vec![UD.to_string()]);
    d[0].ok(&b);
    let (t0, t1) = (d[0].local.text(UD).unwrap(), d[1].local.text(UD).unwrap());
    assert_eq!(t0, t1, "两端收敛");
    for want in ["a\t啊\t9", "c\t从\t1", "d\t的\t1"] {
        assert!(t0.contains(want), "{want} 缺失: {t0}");
    }
    assert!(!t0.contains("b\t吧"), "b 的删除应传播: {t0}");
    assert!(!t0.contains("a\t啊\t5"));
}

#[test]
fn toml_key_conflict_is_reported() {
    let (b, mut d) = setup(2);
    d[0].local
        .set("config.toml", "[ui]\nfont = 12\ntheme = \"a\"\n");
    d[0].ok(&b);
    d[1].ok(&b);
    d[0].local
        .set("config.toml", "[ui]\nfont = 14\ntheme = \"a\"\n");
    d[1].local
        .set("config.toml", "[ui]\nfont = 16\ntheme = \"b\"\n");
    d[0].ok(&b);
    let r = d[1].ok(&b);
    assert_eq!(r.merged, vec!["config.toml".to_string()]);
    assert_eq!(r.conflicts.len(), 1);
    assert_eq!(r.conflicts[0].keys[0].key, "ui.font");
    assert_eq!(r.conflicts[0].keys[0].dropped, "14");
    d[0].ok(&b);
    let t: toml::Table = d[0].local.text("config.toml").unwrap().parse().unwrap();
    assert_eq!(t["ui"]["font"].as_integer(), Some(16));
    assert_eq!(t["ui"]["theme"].as_str(), Some("b"));
    assert_eq!(
        d[0].local.text("config.toml"),
        d[1].local.text("config.toml")
    );
}

#[test]
fn theme_conflict_makes_copy_and_propagates() {
    let (b, mut d) = setup(2);
    d[0].local.set("themes/x.json", "v0");
    d[0].ok(&b);
    d[1].ok(&b);
    d[0].local.set("themes/x.json", "from0");
    d[1].local.set("themes/x.json", "from1");
    d[0].ok(&b);
    let r = d[1].ok(&b);
    let c = &r.conflicts[0];
    assert_eq!(c.path, "themes/x.json");
    let copy = c.copy.clone().unwrap();
    assert_eq!(copy, "themes/x.conflict-dev0-2026-11-24.json");
    assert_eq!(
        d[1].local.text("themes/x.json").unwrap(),
        "from1",
        "后同步者胜出"
    );
    assert_eq!(d[1].local.text(&copy).unwrap(), "from0");
    d[0].ok(&b);
    assert_eq!(d[0].local.text("themes/x.json").unwrap(), "from1");
    assert_eq!(d[0].local.text(&copy).unwrap(), "from0");
    // 稳定：再来一轮没有新动作
    let r = d[0].ok(&b);
    assert!(r.uploaded.is_empty() && r.downloaded.is_empty() && r.conflicts.is_empty());
}

#[test]
fn delete_propagates_and_stale_device_does_not_resurrect() {
    let (b, mut d) = setup(3);
    d[0].local.set("themes/x.json", "v");
    d[0].ok(&b);
    d[1].ok(&b);
    d[2].ok(&b); // dev2 的清单里留着 x
    d[0].local.files.remove("themes/x.json");
    let r = d[0].ok(&b);
    assert_eq!(r.deleted_remote.len(), 1);
    let r = d[1].ok(&b);
    assert_eq!(r.deleted_local.len(), 1);
    assert!(d[1].local.text("themes/x.json").is_none());
    // dev0 / dev1 再同步：dev2 的陈旧清单不能让文件复活
    d[0].ok(&b);
    d[1].ok(&b);
    assert!(d[0].local.text("themes/x.json").is_none());
    assert!(d[1].local.text("themes/x.json").is_none());
    d[2].ok(&b);
    assert!(d[2].local.text("themes/x.json").is_none());
}

#[test]
fn delete_vs_modify_keeps_modification() {
    let (b, mut d) = setup(2);
    d[0].local.set("themes/x.json", "v0");
    d[0].ok(&b);
    d[1].ok(&b);
    d[0].local.files.remove("themes/x.json");
    d[1].local.set("themes/x.json", "edited");
    d[0].ok(&b);
    d[1].ok(&b);
    d[0].ok(&b);
    assert_eq!(d[0].local.text("themes/x.json").unwrap(), "edited");
    assert_eq!(d[1].local.text("themes/x.json").unwrap(), "edited");
}

#[test]
fn three_devices_converge() {
    let (b, mut d) = setup(3);
    for (i, dev) in d.iter_mut().enumerate() {
        dev.local
            .set(UD, &dict(&[&format!("k{i}\t词{i}\t1"), "same\t同\t1"]));
        dev.local.set(&format!("themes/{i}.json"), "x");
    }
    for _ in 0..3 {
        for dev in d.iter_mut() {
            dev.ok(&b);
        }
    }
    let t0 = d[0].local.text(UD).unwrap();
    for dev in &d {
        assert_eq!(dev.local.text(UD).unwrap(), t0);
        assert_eq!(dev.local.files.len(), d[0].local.files.len());
    }
    for i in 0..3 {
        assert!(t0.contains(&format!("k{i}\t词{i}")));
    }
}

#[test]
fn put_failure_midway_then_rerun_is_correct_and_not_duplicated() {
    let (b, mut d) = setup(2);
    for i in 0..4 {
        d[0].local
            .set(&format!("themes/{i}.json"), &format!("c{i}"));
    }
    let before = b.keys("").len();
    b.fail_put_nth(3, BackendError::Transient("boom".into()));
    let e = d[0].sync(&b).unwrap_err();
    assert!(e.retryable());
    assert_eq!(
        b.keys(&manifest::devices_prefix()).len(),
        0,
        "清单不能先于 blob"
    );
    assert_eq!(d[0].state.pending.len(), 4);
    assert!(d[0].state.files.is_empty(), "失败不提交状态");
    d[0].ok(&b);
    assert!(d[0].state.pending.is_empty());
    assert_eq!(b.keys(&manifest::blobs_prefix()).len(), 4);
    assert_eq!(b.keys(&manifest::devices_prefix()).len(), 1);
    assert_eq!(before + 4 + 1, b.keys("").len());
    d[1].ok(&b);
    for i in 0..4 {
        assert_eq!(
            d[1].local.text(&format!("themes/{i}.json")).unwrap(),
            format!("c{i}")
        );
    }
}

#[test]
fn manifest_put_failure_rerun_after_download_and_merge() {
    let (b2, mut e) = setup(2);
    e[0].local.set(UD, &dict(&["a\t啊\t1"]));
    e[0].ok(&b2);
    e[1].ok(&b2);
    e[0].local.set(UD, &dict(&["a\t啊\t1", "b\t吧\t1"]));
    e[0].local.set("themes/y.json", "new");
    e[0].ok(&b2);
    e[1].local.set(UD, &dict(&["a\t啊\t1", "c\t从\t1"]));
    b2.fail_all(Some(BackendError::Transient("down".into())));
    assert!(e[1].sync(&b2).is_err());
    b2.fail_all(None);
    // 失败发生在读取阶段，本地未动；放开后一次成功
    e[1].ok(&b2);
    e[0].ok(&b2);
    let t = e[0].local.text(UD).unwrap();
    assert_eq!(Some(t.clone()), e[1].local.text(UD));
    assert!(t.contains("b\t吧") && t.contains("c\t从"));
    assert_eq!(e[1].local.text("themes/y.json").unwrap(), "new");
    // 再失败在清单 put：本地已写、blob 已传，重跑后无重复 blob/清单
    e[0].local.set("themes/z.json", "z");
    let puts = b2.put_count();
    b2.fail_put_nth(2, BackendError::Transient("boom".into())); // blob ok，清单失败
    assert!(e[0].sync(&b2).is_err());
    e[0].ok(&b2);
    assert!(b2.put_count() >= puts + 2);
    e[1].ok(&b2);
    assert_eq!(e[1].local.text("themes/z.json").unwrap(), "z");
    let gens = b2.keys(&format!("{}/devices/dev0/", manifest::ROOT));
    assert!(gens.len() <= 2, "最多保留两代: {gens:?}");
}

#[test]
fn manifest_retention_keeps_two_gens() {
    let (b, mut d) = setup(1);
    for i in 0..5 {
        d[0].local.set("themes/x.json", &format!("v{i}"));
        d[0].ok(&b);
    }
    let ks = b.keys(&format!("{}/devices/dev0/", manifest::ROOT));
    assert_eq!(ks.len(), 2, "{ks:?}");
    assert!(ks.iter().any(|k| k.ends_with("manifest.5")));
    assert!(ks.iter().any(|k| k.ends_with("manifest.4")));
}

#[test]
fn wrong_passphrase_rejected_and_key_import_joins() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b, "right", KdfParams::FAST_TEST).unwrap();
    assert!(matches!(
        remote::init_remote(&b, "x", KdfParams::FAST_TEST),
        Err(SyncError::AlreadyInitialized)
    ));
    assert!(matches!(
        remote::join_with_passphrase(&b, "wrong"),
        Err(SyncError::Crypto(
            wind_sync::crypto::CryptoError::BadPassphrase
        ))
    ));
    assert_eq!(remote::join_with_passphrase(&b, "right").unwrap(), dek);
    // 导出密钥 → 新设备导入
    let exported = dek.to_base64();
    let dek2 = remote::join_with_key(&b, &exported).unwrap();
    let mut a = Dev::new("a", &dek);
    a.local.set("config.toml", "k = 1\n");
    a.ok(&b);
    let mut c = Dev::new("c", &dek2);
    c.ok(&b);
    assert_eq!(c.local.text("config.toml").unwrap(), "k = 1\n");
    // 错密钥
    assert!(remote::join_with_key(&b, &Dek::generate().to_base64()).is_err());
    let mut bad = Dev::new("bad", &Dek::generate());
    let e = bad.sync(&b).unwrap_err();
    assert!(!e.retryable());
}

#[test]
fn change_passphrase_keeps_old_exported_key_valid() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b, "old", KdfParams::FAST_TEST).unwrap();
    let exported = dek.to_base64();
    let mut a = Dev::new("a", &dek);
    a.local.set("config.toml", "k = 1\n");
    a.ok(&b);
    assert!(remote::change_passphrase(&b, "nope", "new", KdfParams::FAST_TEST).is_err());
    remote::change_passphrase(&b, "old", "new", KdfParams::FAST_TEST).unwrap();
    assert!(remote::join_with_passphrase(&b, "old").is_err());
    assert_eq!(remote::join_with_passphrase(&b, "new").unwrap(), dek);
    let dek2 = remote::join_with_key(&b, &exported).unwrap();
    let mut c = Dev::new("c", &dek2);
    c.ok(&b);
    assert_eq!(c.local.text("config.toml").unwrap(), "k = 1\n");
    a.ok(&b); // 原设备不受影响
}

#[test]
fn swapped_objects_are_rejected() {
    let (b, mut d) = setup(2);
    d[0].local.set("themes/a.json", "AAA");
    d[0].local.set("themes/b.json", "BBB");
    d[0].ok(&b);
    // 把 blob A 的密文放到 blob B 的键下
    let blobs = b.keys(&manifest::blobs_prefix());
    assert_eq!(blobs.len(), 2);
    let (ka, kb) = (&blobs[0], &blobs[1]);
    let ca = b.raw_get(ka).unwrap();
    b.raw_put(kb, ca);
    let r = d[1].ok(&b);
    assert_eq!(r.downloaded.len(), 1, "被调换的那个不能当成真内容");
    assert!(r.warnings.iter().any(|w| w.contains("损坏或被调换")));
    let got: Vec<_> = d[1].local.files.values().collect();
    assert!(
        got.iter()
            .all(|v| v.as_slice() == b"AAA" || v.as_slice() == b"BBB")
    );
    assert_eq!(d[1].local.files.len(), 1);

    // 清单调换：dev0 的清单放到 dev2 的目录下 → 忽略并告警
    let (b2, mut e) = setup(2);
    e[0].local.set("themes/a.json", "A");
    e[0].ok(&b2);
    let mk = b2.keys(&format!("{}/devices/dev0/", manifest::ROOT))[0].clone();
    let raw = b2.raw_get(&mk).unwrap();
    b2.raw_put(&manifest::manifest_key("evil", 1), raw);
    let r = e[1].ok(&b2);
    assert!(r.warnings.iter().any(|w| w.contains("evil")));
    assert_eq!(r.downloaded.len(), 1); // 仍从真正的 dev0 取到
}

#[test]
fn scope_filter_limits_what_syncs() {
    let (b, mut d) = setup(2);
    d[0].local.set("config.toml", "a = 1\n");
    d[0].local.set("themes/t.json", "{}");
    d[0].local.set("meta.toml", "dir_id = 1");
    d[0].local.set("data/.backup/x", "x");
    d[0].local.set("data/pinyin/freq.redb", "x");
    let mut ctx = SyncCtx::new(
        ScopeFlags {
            themes: false,
            ..ScopeFlags::ALL
        },
        T0,
        test_ns(),
    );
    ctx.force = true;
    let dev = &mut d[0];
    sync_once(&b, &mut dev.local, &mut dev.state, &dev.dek, &ctx).unwrap();
    d[1].ok(&b);
    assert!(d[1].local.text("config.toml").is_some());
    assert!(d[1].local.text("themes/t.json").is_none(), "主题开关关");
    assert!(d[1].local.text("meta.toml").is_none());
    assert!(d[1].local.text("data/.backup/x").is_none());
    assert!(d[1].local.text("data/pinyin/freq.redb").is_none());
}

#[test]
fn retry_backoff_and_halt_through_sync_once() {
    let (b, mut d) = setup(1);
    d[0].local.set("config.toml", "a = 1\n");
    b.fail_all(Some(BackendError::Transient("net".into())));
    let ctx = SyncCtx::new(ScopeFlags::ALL, 1000, test_ns());
    let dev = &mut d[0];
    assert!(sync_once(&b, &mut dev.local, &mut dev.state, &dev.dek, &ctx).is_err());
    assert_eq!(dev.state.retry.next_at, 1030);
    // 等待期内不执行
    b.fail_all(None);
    let r = sync_once(
        &b,
        &mut dev.local,
        &mut dev.state,
        &dev.dek,
        &SyncCtx::new(ScopeFlags::ALL, 1010, test_ns()),
    )
    .unwrap();
    assert!(r.skipped);
    let r = sync_once(
        &b,
        &mut dev.local,
        &mut dev.state,
        &dev.dek,
        &SyncCtx::new(ScopeFlags::ALL, 1031, test_ns()),
    )
    .unwrap();
    assert!(!r.skipped && r.uploaded.len() == 1);
    assert_eq!(dev.state.retry.failures, 0);
    // 认证失败：停止，之后连 force 也不执行
    b.fail_all(Some(BackendError::Auth("401".into())));
    assert!(
        sync_once(
            &b,
            &mut dev.local,
            &mut dev.state,
            &dev.dek,
            &SyncCtx::new(ScopeFlags::ALL, 2000, test_ns())
        )
        .is_err()
    );
    b.fail_all(None);
    let mut f = SyncCtx::new(ScopeFlags::ALL, 9999, test_ns());
    f.force = true;
    assert!(matches!(
        sync_once(&b, &mut dev.local, &mut dev.state, &dev.dek, &f),
        Err(SyncError::Halted(_))
    ));
    dev.state.retry.clear_halt();
    assert!(sync_once(&b, &mut dev.local, &mut dev.state, &dev.dek, &f).is_ok());
}

#[test]
fn state_roundtrips_through_json_between_runs() {
    let (b, mut d) = setup(2);
    d[0].local.set("themes/a.json", "A");
    d[0].ok(&b);
    d[1].ok(&b);
    let j = serde_json::to_string(&d[1].state).unwrap();
    d[1].state = serde_json::from_str(&j).unwrap();
    d[0].local.set("themes/a.json", "A2");
    d[0].ok(&b);
    let r = d[1].ok(&b);
    assert_eq!(r.downloaded.len(), 1);
    assert_eq!(d[1].local.text("themes/a.json").unwrap(), "A2");
}

#[test]
fn gc_removes_only_old_unreferenced_blobs() {
    let (b, mut d) = setup(1);
    b.set_now(T0);
    d[0].local.set("themes/a.json", "v1");
    d[0].ok(&b);
    for v in ["v2", "v3", "v4"] {
        d[0].local.set("themes/a.json", v);
        d[0].ok(&b);
    }
    assert_eq!(b.keys(&manifest::blobs_prefix()).len(), 4);
    let dek = d[0].dek.clone();
    // 不够老：不删
    assert!(
        remote::gc_blobs(&b, &dek, T0 + 86400, 7 * 86400)
            .unwrap()
            .is_empty()
    );
    // v1 只被已清理的旧代引用，v2 被保留的上一代引用
    let removed = remote::gc_blobs(&b, &dek, T0 + 8 * 86400, 7 * 86400).unwrap();
    assert_eq!(removed.len(), 2);
    assert_eq!(b.keys(&manifest::blobs_prefix()).len(), 2);
    // 活着的内容仍可被新设备取到
    let mut n = Dev::new("n", &dek);
    n.ok(&b);
    assert_eq!(n.local.text("themes/a.json").unwrap(), "v4");
}

// ───────────── 并发多版本（A、C 互不知情，B 同时看到两者）─────────────

/// 把 `from` 里某前缀下的对象拷到 `to`。
fn copy_prefix(from: &MemBackend, to: &MemBackend, prefix: &str) {
    for k in from.keys(prefix) {
        to.raw_put(&k, from.raw_get(&k).unwrap());
    }
}

fn fork(b: &MemBackend) -> MemBackend {
    let f = MemBackend::new();
    copy_prefix(b, &f, "");
    f
}

/// dev0(A) 与 dev2(C) 并发改同一文件；C 看不到 A 且 lamport 更大。返回真实后端。
/// 之后 dev1(B) 同时看到两者。
fn concurrent_a_c(path: &str, base: &str, a: &str, c: &str) -> (MemBackend, Vec<Dev>) {
    let (b, mut d) = setup(3);
    d[0].local.set(path, base);
    d[0].ok(&b);
    d[1].ok(&b);
    d[2].ok(&b);
    let pre = fork(&b);
    d[0].local.set(path, a);
    d[0].ok(&b);
    // C 在看不到 A 的后端上改两次（lamport 更大），再把自己的对象放回真实后端
    d[2].local.set(path, "tmp");
    d[2].ok(&pre);
    d[2].local.set(path, c);
    d[2].ok(&pre);
    copy_prefix(&pre, &b, &manifest::blobs_prefix());
    copy_prefix(&pre, &b, &format!("{}/devices/dev2/", manifest::ROOT));
    (b, d)
}

#[test]
fn three_devices_concurrent_dict_edits_all_survive() {
    let (b, mut d) = setup(3);
    d[0].local.set(UD, &dict(&["k\t基\t1"]));
    d[0].ok(&b);
    d[1].ok(&b);
    d[2].ok(&b);
    let pre = fork(&b);
    d[0].local.set(UD, &dict(&["k\t基\t1", "a\t甲\t1"]));
    d[0].ok(&b);
    d[2].local.set(UD, &dict(&["k\t基\t1", "c\t丙\t1"]));
    d[2].ok(&pre);
    d[2].local
        .set(UD, &dict(&["k\t基\t1", "c\t丙\t1", "c2\t丙二\t1"]));
    d[2].ok(&pre);
    copy_prefix(&pre, &b, &manifest::blobs_prefix());
    copy_prefix(&pre, &b, &format!("{}/devices/dev2/", manifest::ROOT));
    d[1].local.set(UD, &dict(&["k\t基\t1", "b\t乙\t1"]));
    let r = d[1].ok(&b);
    assert_eq!(r.merged, vec![UD.to_string()]);
    for _ in 0..2 {
        for dev in d.iter_mut() {
            dev.ok(&b);
        }
    }
    let t = d[0].local.text(UD).unwrap();
    for want in ["k\t基", "a\t甲", "b\t乙", "c\t丙\t", "c2\t丙二"] {
        assert!(t.contains(want), "{want} 丢失: {t}");
    }
    assert_eq!(d[1].local.text(UD).unwrap(), t);
    assert_eq!(d[2].local.text(UD).unwrap(), t);
}

#[test]
fn three_devices_concurrent_theme_edits_make_two_copies() {
    let (b, mut d) = concurrent_a_c("themes/x.json", "v0", "vA", "vC");
    d[1].local.set("themes/x.json", "vB");
    let r = d[1].ok(&b);
    let mut copies: Vec<String> = r.conflicts.iter().filter_map(|c| c.copy.clone()).collect();
    copies.sort();
    assert_eq!(
        copies,
        vec![
            "themes/x.conflict-dev0-2026-11-24.json",
            "themes/x.conflict-dev2-2026-11-24.json"
        ]
    );
    assert_eq!(d[1].local.text("themes/x.json").unwrap(), "vB");
    assert_eq!(d[1].local.text(&copies[0]).unwrap(), "vA");
    assert_eq!(d[1].local.text(&copies[1]).unwrap(), "vC");
    // 传播且收敛
    for _ in 0..2 {
        d[0].ok(&b);
        d[2].ok(&b);
        d[1].ok(&b);
    }
    for dev in &d {
        assert_eq!(dev.local.text("themes/x.json").unwrap(), "vB");
        assert_eq!(dev.local.text(&copies[0]).unwrap(), "vA");
        assert_eq!(dev.local.text(&copies[1]).unwrap(), "vC");
        assert_eq!(dev.local.files.len(), 3);
    }
}

#[test]
fn unchanged_b_does_not_lose_lower_lamport_version() {
    // 原缺陷场景：B 本地未改，只看到 A(低) 与 C(高)
    let (b, mut d) = concurrent_a_c("themes/x.json", "v0", "vA", "vC");
    let r = d[1].ok(&b);
    assert_eq!(d[1].local.text("themes/x.json").unwrap(), "vC");
    assert_eq!(r.conflicts.len(), 1);
    let copy = r.conflicts[0].copy.clone().unwrap();
    assert_eq!(
        d[1].local.text(&copy).unwrap(),
        "vA",
        "A 的修改必须留有副本"
    );
}

#[test]
fn absorbed_old_entry_does_not_trigger_copy_again() {
    let (b, mut d) = setup(3);
    d[0].local.set("themes/x.json", "v0");
    d[0].ok(&b);
    d[1].ok(&b);
    d[2].ok(&b);
    d[0].local.set("themes/x.json", "vA");
    d[0].ok(&b);
    d[2].local.set("themes/x.json", "vC");
    let r = d[2].ok(&b); // C 看到 A 并生成副本、吸收 A
    assert_eq!(r.conflicts.len(), 1);
    // B 此时同时看到 A 的旧条目与 C 的新条目：A 已被吸收，不再冲突
    let r = d[1].ok(&b);
    assert!(r.conflicts.is_empty(), "{:?}", r.conflicts);
    assert_eq!(d[1].local.text("themes/x.json").unwrap(), "vC");
    assert_eq!(d[1].local.files.len(), 2, "只有原文件和 C 生成的那一份副本");
}

#[test]
fn lamport_advances_past_remote_lamports() {
    let (b, mut d) = setup(2);
    d[0].local.set("themes/a.json", "1");
    for i in 0..5 {
        d[0].local.set("themes/a.json", &format!("{i}"));
        d[0].ok(&b);
    }
    let remote_max = d[0].state.lamport;
    assert!(remote_max >= 5);
    d[1].local.set("themes/b.json", "new");
    d[1].ok(&b);
    assert!(
        d[1].state.lamport > remote_max,
        "接收清单后 lamport 要越过对方"
    );
    assert!(d[1].state.files["desktop/themes/b.json"].lamport > remote_max);
}
