//! 写入与限制：一轮只调一次 apply、可执行位随写入落位、排除路径移出清单、超大文件跳过不读。

use wind_sync::backend::{Backend, MemBackend};
use wind_sync::crypto::Dek;
use wind_sync::engine::{MAX_FILE_BYTES, SyncCtx, SyncReport, sync_once};
use wind_sync::local::{LocalError, LocalStore, MemLocal};
use wind_sync::manifest::{self, Entry, Manifest};
use wind_sync::remote;
use wind_sync::scope::{DeviceClass, Namespace};
use wind_sync::state::SyncState;

const T0: u64 = 1_795_478_400;

fn ns() -> Namespace {
    Namespace::new(DeviceClass::Desktop, "g")
}

fn run(b: &MemBackend, dek: &Dek, local: &mut dyn LocalStore, st: &mut SyncState) -> SyncReport {
    let mut ctx = SyncCtx::new(T0, ns());
    ctx.force = true;
    sync_once(b, local, st, dek, &ctx).expect("同步应成功")
}

fn setup() -> (MemBackend, Dek) {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b).unwrap();
    (b, dek)
}

/// 记录引擎对本地的调用序列。
#[derive(Default)]
struct Rec {
    inner: MemLocal,
    calls: Vec<String>,
}

impl LocalStore for Rec {
    fn list(&self) -> Result<Vec<String>, LocalError> {
        self.inner.list()
    }
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, LocalError> {
        self.inner.read(path)
    }
    fn write_batch(&mut self, files: &[(String, Vec<u8>)]) -> Result<(), LocalError> {
        self.calls.push("write_batch".into());
        self.inner.write_batch(files)
    }
    fn delete(&mut self, paths: &[String]) -> Result<(), LocalError> {
        self.calls.push("delete".into());
        self.inner.delete(paths)
    }
    fn apply(
        &mut self,
        writes: &[(String, Vec<u8>)],
        exec: &[String],
        deletes: &[String],
    ) -> Result<(), LocalError> {
        self.calls.push(format!("apply exec={exec:?}"));
        self.inner.apply(writes, exec, deletes)
    }
    fn is_exec(&self, path: &str) -> Result<bool, LocalError> {
        self.inner.is_exec(path)
    }
    fn set_exec(&mut self, paths: &[String]) -> Result<(), LocalError> {
        self.calls.push(format!("set_exec {paths:?}"));
        self.inner.set_exec(paths)
    }
}

#[test]
fn writes_and_deletes_go_through_one_apply_per_round() {
    let (b, dek) = setup();
    let (mut la, mut sa) = (MemLocal::new(), SyncState::new("a"));
    let (mut lb, mut sb) = (Rec::default(), SyncState::new("b"));
    la.set("extensions/k/run", "v1");
    la.set("extensions/k/old.dll", "x");
    run(&b, &dek, &mut la, &mut sa);
    run(&b, &dek, &mut lb, &mut sb);
    la.set("extensions/k/run", "v2");
    la.files.remove("extensions/k/old.dll");
    run(&b, &dek, &mut la, &mut sa);
    lb.calls.clear();
    let r = run(&b, &dek, &mut lb, &mut sb);
    assert_eq!(r.downloaded.len(), 1);
    assert_eq!(r.deleted_local.len(), 1);
    assert_eq!(lb.calls, vec!["apply exec=[]".to_string()]);
    assert_eq!(lb.inner.applies, 2);
}

#[test]
fn exec_bit_of_downloaded_file_is_given_to_apply_not_set_afterwards() {
    let (b, dek) = setup();
    let (mut la, mut sa) = (MemLocal::new(), SyncState::new("a"));
    let (mut lb, mut sb) = (Rec::default(), SyncState::new("b"));
    la.set("extensions/k/run", "bin");
    la.exec.insert("extensions/k/run".into());
    run(&b, &dek, &mut la, &mut sa);
    run(&b, &dek, &mut lb, &mut sb);
    assert_eq!(
        lb.calls,
        vec!["apply exec=[\"extensions/k/run\"]".to_string()]
    );
    assert!(lb.inner.is_exec("extensions/k/run").unwrap());
}

fn own_manifest(b: &MemBackend, dek: &Dek, dev: &str) -> Manifest {
    let (d, g) = b
        .list(&manifest::devices_prefix())
        .unwrap()
        .iter()
        .filter_map(|o| manifest::parse_manifest_key(&o.key))
        .filter(|(d, _)| d == dev)
        .max_by_key(|(_, g)| *g)
        .unwrap();
    let raw = b.get(&manifest::manifest_key(&d, g)).unwrap().unwrap();
    Manifest::decode(&dek.files_key(), &d, g, &raw).unwrap()
}

#[test]
fn excluded_state_entries_are_dropped_without_delete_marker() {
    let (b, dek) = setup();
    let (mut la, mut sa) = (MemLocal::new(), SyncState::new("a"));
    let (mut lb, mut sb) = (MemLocal::new(), SyncState::new("b"));
    la.set("config.toml", "x=1");
    sa.files.insert(
        "data/pinyin/temp.wjl".into(),
        Entry {
            hash: "h".into(),
            base: None,
            deleted: false,
            lamport: 1,
            device: "a".into(),
            absorbed: vec![],
            exec: false,
        },
    );
    lb.set("data/pinyin/temp.wjl", "b 自己的");
    let r = run(&b, &dek, &mut la, &mut sa);
    assert!(r.deleted_remote.is_empty(), "{r:?}");
    assert!(!sa.files.contains_key("data/pinyin/temp.wjl"));
    assert!(
        !own_manifest(&b, &dek, "a")
            .files
            .contains_key("data/pinyin/temp.wjl")
    );
    // 范围内的本组设置条目照常保留
    assert!(sa.files.contains_key("groups/g/config.toml"));
    run(&b, &dek, &mut lb, &mut sb);
    assert_eq!(lb.text("data/pinyin/temp.wjl").as_deref(), Some("b 自己的"));
}

#[test]
fn oversized_file_is_skipped_with_warning_and_not_deleted() {
    let (b, dek) = setup();
    let (mut la, mut sa) = (MemLocal::new(), SyncState::new("a"));
    let (mut lb, mut sb) = (MemLocal::new(), SyncState::new("b"));
    la.set("emoji/big.txt", "小");
    run(&b, &dek, &mut la, &mut sa);
    run(&b, &dek, &mut lb, &mut sb);
    la.files.insert(
        "emoji/big.txt".into(),
        vec![b'x'; MAX_FILE_BYTES as usize + 1],
    );
    let r = run(&b, &dek, &mut la, &mut sa);
    assert!(
        r.uploaded.is_empty() && r.deleted_remote.is_empty(),
        "{r:?}"
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w == "文件过大未同步：emoji/big.txt"),
        "{:?}",
        r.warnings
    );
    assert!(sa.files.get("emoji/big.txt").is_some_and(|e| !e.deleted));
    // 对端改了：本机超大文件不被覆盖
    lb.set("emoji/big.txt", "改");
    run(&b, &dek, &mut lb, &mut sb);
    run(&b, &dek, &mut la, &mut sa);
    assert_eq!(la.files["emoji/big.txt"].len(), MAX_FILE_BYTES as usize + 1);
}

/// 只报大小、不许读：引擎应先看大小再决定读不读。
struct SizeOnly(MemLocal);

impl LocalStore for SizeOnly {
    fn list(&self) -> Result<Vec<String>, LocalError> {
        self.0.list()
    }
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, LocalError> {
        assert_ne!(path, "emoji/big.txt", "超大文件不应被读入");
        self.0.read(path)
    }
    fn write_batch(&mut self, files: &[(String, Vec<u8>)]) -> Result<(), LocalError> {
        self.0.write_batch(files)
    }
    fn delete(&mut self, paths: &[String]) -> Result<(), LocalError> {
        self.0.delete(paths)
    }
    fn size(&self, path: &str) -> Result<Option<u64>, LocalError> {
        Ok((path == "emoji/big.txt").then_some(MAX_FILE_BYTES + 1))
    }
}

#[test]
fn oversized_file_is_not_read() {
    let (b, dek) = setup();
    let mut l = SizeOnly(MemLocal::new());
    l.0.set("emoji/big.txt", "占位");
    l.0.set("emoji/a.txt", "a");
    let mut st = SyncState::new("a");
    let r = run(&b, &dek, &mut l, &mut st);
    assert_eq!(r.uploaded, vec!["emoji/a.txt".to_string()]);
    assert_eq!(
        r.warnings,
        vec!["文件过大未同步：emoji/big.txt".to_string()]
    );
}
