use super::*;
use crate::secrets::{FileSecretStore, Secrets};
use crate::testing::{FakeCore, MemFactory};
use wind_sync::backend::{BackendError, ObjMeta};
use wind_sync::{KdfParams, MemBackend};

fn runner_with(
    dir: &std::path::Path,
    factory: Box<dyn BackendFactory>,
    dek: Option<Dek>,
    device_name: &str,
) -> (Runner, Receiver<Input>) {
    std::fs::create_dir_all(dir.join("user")).unwrap();
    let state_dir = dir.join("state");
    let store = FileSecretStore::new(state_dir.join(crate::secrets::FILE_NAME));
    Secrets {
        dek: dek.map(|d| d.to_base64()),
        password: Some("pw".into()),
    }
    .save(&store)
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let cfg = Config {
        userdata_dir: dir.join("user"),
        secrets: Box::new(store),
        state_dir,
        settings: SyncSettings {
            enabled: true,
            backend: Some(Target::Webdav {
                url: "http://127.0.0.1/x".into(),
                user: "u".into(),
            }),
            device_name: device_name.into(),
            ..Default::default()
        },
        link: Arc::new(FakeCore::default()),
        factory,
        timing: Timing::default(),
        kdf: KdfParams::FAST_TEST,
        watch: false,
        class: DeviceClass::Desktop,
        portable: false,
        on_demand: false,
    };
    (Runner::new(cfg, tx), rx)
}

fn runner(dir: &std::path::Path) -> (Runner, Receiver<Input>) {
    runner_with(
        dir,
        Box::new(MemFactory(MemBackend::new())),
        Some(Dek::generate()),
        "",
    )
}

fn retired(r: &Runner) -> Vec<&str> {
    r.state.retired_ids.iter().map(|x| x.id.as_str()).collect()
}

fn wait_round(rx: &Receiver<Input>) -> Box<Round> {
    loop {
        if let Input::RoundDone(x) = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            return x;
        }
    }
}

/// 跑完整一轮（起工作线程、等结果、收尾）。
fn round(r: &mut Runner, rx: &Receiver<Input>) {
    r.start_round();
    let x = wait_round(rx);
    r.finish_round(*x);
}

/// 回声记录在下一轮扫描之后清掉，本轮重新写过的保留。
#[test]
fn echo_entries_from_earlier_rounds_are_dropped_after_the_next_scan() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, rx) = runner(d.path());
    lock(&r.echo).insert("config.toml".into(), "h1".into());
    r.start_round();
    let round = wait_round(&rx);
    // 工作线程里改写了的条目（模拟本轮写入）不得被清
    lock(&r.echo).insert("themes/t/a.toml".into(), "h2".into());
    r.finish_round(*round);
    let e = lock(&r.echo);
    assert!(!e.contains_key("config.toml"), "旧条目应被清: {e:?}");
    assert_eq!(e.get("themes/t/a.toml").map(String::as_str), Some("h2"));
}

fn info_at(t: u64) -> DeviceInfo {
    local_info(&SyncSettings::default(), &SyncState::new("pc"), t)
}

/// 设备信息：没写过必写；同一 UTC 日不重写；跨日才刷新；改名随时写。
#[test]
fn device_info_refresh_rules() {
    let d0 = 20_000 * DAY_SECS;
    let written = info_at(d0 + 100);
    assert!(info_stale(None, &info_at(d0), true));
    assert!(!info_stale(
        Some(&written),
        &info_at(d0 + DAY_SECS - 101),
        true
    ));
    assert!(info_stale(Some(&written), &info_at(d0 + DAY_SECS), true));
    assert!(!info_stale(
        Some(&written),
        &info_at(d0 + 5 * DAY_SECS),
        false
    ));
    let mut renamed = info_at(d0 + 200);
    renamed.name = "别名".into();
    assert!(info_stale(Some(&written), &renamed, false));
    let mut caps = info_at(d0 + 200);
    caps.caps.push("clip".into());
    assert!(info_stale(Some(&written), &caps, false));
}

/// 名册条目带上本机的组号与组名。
#[test]
fn device_info_carries_group() {
    let mut st = SyncState::new("pc");
    st.group = "home".into();
    st.group_name = "家".into();
    st.group_name_at = 7;
    let i = local_info(&SyncSettings::default(), &st, 1);
    assert_eq!(
        (i.group.as_str(), i.group_name.as_str(), i.group_name_at),
        ("home", "家", 7)
    );
    // 组号为空时按单机组（= 设备号）
    let i = local_info(&SyncSettings::default(), &SyncState::new("pc"), 1);
    assert_eq!(i.group, "pc");
}

fn initialized_remote() -> MemBackend {
    let mem = MemBackend::new();
    remote::init_remote(&mem, "p", KdfParams::FAST_TEST).unwrap();
    mem
}

/// 用口令加入已有远端后不同步，确认身份后才发布清单。
#[test]
fn joined_device_does_not_sync_until_identity_confirmed() {
    let d = tempfile::tempdir().unwrap();
    let mem = initialized_remote();
    let (mut r, rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    let rep = r
        .command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    assert_eq!(rep["mode"], "join");
    let st = r.status();
    assert!(st.needs_identity, "{st:?}");
    assert_eq!(st.device, "pc");
    let e = r.command(Op::SyncNow, &Value::Null).unwrap_err();
    assert!(e.contains("请先确认本机身份"), "{e}");
    assert!(r.next_wake(Instant::now()).is_none(), "未确认不自动调度");
    assert!(mem.keys(&manifest::device_dir_prefix("pc")).is_empty());

    let rep = r.command(Op::ConfirmIdentity, &Value::Null).unwrap();
    assert_eq!(rep["device"], "pc");
    let st = r.status();
    assert!(!st.needs_identity);
    assert_eq!(st.group, "pc");
    r.command(Op::SyncNow, &Value::Null).unwrap();
    round(&mut r, &rx);
    assert!(mem.raw_get(&manifest::manifest_key("pc", 1)).is_some());
}

/// 确认身份时号已被别的设备占用：取首个空闲的带序号的号，且不把别人的号当旧号删除。
#[test]
fn confirm_identity_picks_a_free_suffix_when_id_is_taken() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let dek = remote::init_remote(&mem, "p", KdfParams::FAST_TEST).unwrap();
    for id in ["pc", "pc-2"] {
        put_device_info(
            &mem,
            &dek.devinfo_key(),
            id,
            &local_info(&SyncSettings::default(), &SyncState::new(id), 1),
        )
        .unwrap();
    }
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    r.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    let rep = r.command(Op::ConfirmIdentity, &Value::Null).unwrap();
    assert_eq!(rep["device"], "pc-3");
    assert_eq!(r.status().device, "pc-3");
    assert_eq!(r.status().group, "pc-3");
    assert!(r.state.retired_ids.is_empty(), "{:?}", r.state.retired_ids);
}

/// 内存后端，可让某个键片段下的删除失败。
#[derive(Clone)]
struct Flaky {
    inner: MemBackend,
    fail_delete: Arc<Mutex<Option<String>>>,
}

impl Backend for Flaky {
    fn list(&self, prefix: &str) -> Result<Vec<ObjMeta>, BackendError> {
        self.inner.list(prefix)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, BackendError> {
        self.inner.get(key)
    }
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), BackendError> {
        self.inner.put(key, bytes)
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        if let Some(f) = lock(&self.fail_delete).as_deref()
            && key.contains(f)
        {
            return Err(BackendError::Transient("删除失败（桩）".into()));
        }
        self.inner.delete(key)
    }
}

struct FlakyFactory(Flaky);

impl BackendFactory for FlakyFactory {
    fn build(&self, _: &SyncSettings, _: &str) -> Result<Box<dyn Backend>, String> {
        Ok(Box::new(self.0.clone()))
    }
}

/// 改号后的一轮删掉远端旧号目录；删除失败则旧号留在待删列表，下一轮再删。
#[test]
fn renamed_device_removes_old_id_after_a_round_and_retries_on_failure() {
    let d = tempfile::tempdir().unwrap();
    let flaky = Flaky {
        inner: MemBackend::new(),
        fail_delete: Arc::new(Mutex::new(None)),
    };
    let mem = flaky.inner.clone();
    let (mut r, rx) = runner_with(d.path(), Box::new(FlakyFactory(flaky.clone())), None, "pc");
    let rep = r
        .command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    assert_eq!(rep["mode"], "init");
    std::fs::write(d.path().join("user/config.toml"), "a = 1\n").unwrap();
    round(&mut r, &rx);
    assert!(mem.raw_get(&manifest::manifest_key("pc", 1)).is_some());

    *lock(&flaky.fail_delete) = Some("/devices/pc/".into());
    r.command(Op::RenameDevice, &json!({"id": "home"})).unwrap();
    assert_eq!(r.status().device, "home");
    round(&mut r, &rx);
    assert_eq!(retired(&r), ["pc"]);
    assert!(!mem.keys(&manifest::device_dir_prefix("pc")).is_empty());
    assert!(mem.raw_get(&manifest::manifest_key("home", 1)).is_some());

    *lock(&flaky.fail_delete) = None;
    round(&mut r, &rx);
    assert!(r.state.retired_ids.is_empty());
    assert!(mem.keys(&manifest::device_dir_prefix("pc")).is_empty());
}

/// 撞号停止显示为错误态；改号后解除，且不删除与别人共用的旧号。
#[test]
fn device_id_conflict_is_an_error_until_renamed() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    r.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    let msg = sync_error(&SyncError::DeviceIdConflict("pc".into()));
    r.state.retry.halted = Some(msg.clone());
    r.state.retry.last_error = Some(msg.clone());
    let st = r.status();
    assert_eq!(st.phase, "error");
    assert_eq!(st.last_error.as_deref(), Some(msg.as_str()));
    assert!(r.next_wake(Instant::now()).is_none());

    r.command(Op::RenameDevice, &json!({"id": "pc-b"})).unwrap();
    assert!(r.state.retry.halted.is_none());
    assert!(r.next_wake(Instant::now()).is_some());
    assert!(r.state.retired_ids.is_empty(), "旧号仍属另一台设备");
}

/// 重新导入本机已持有的密钥不会让身份回到未确认。
#[test]
fn reimporting_the_held_key_keeps_identity() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    r.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    let key = r.command(Op::ExportKey, &Value::Null).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    r.command(Op::ImportKey, &json!({"key": key})).unwrap();
    assert!(!r.status().needs_identity);
    assert_eq!(r.status().device, "pc");
    // 已确认时再确认是空操作，不改组
    r.state.group = "home".into();
    let rep = r.command(Op::ConfirmIdentity, &Value::Null).unwrap();
    assert_eq!(
        (&rep["device"], &rep["group"]),
        (&json!("pc"), &json!("home"))
    );
}

/// 已确认身份的设备认领别的号后，原号在下一轮从远端删除。
#[test]
fn claiming_another_id_retires_the_previous_one() {
    let mem = MemBackend::new();
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut a, rxa) = runner_with(da.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    a.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    round(&mut a, &rxa);
    let (mut b, rxb) = runner_with(db.path(), Box::new(MemFactory(mem.clone())), None, "lap");
    b.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    b.command(Op::ConfirmIdentity, &Value::Null).unwrap();
    round(&mut b, &rxb);
    assert!(!mem.keys(&manifest::device_dir_prefix("lap")).is_empty());

    b.command(Op::ClaimDevice, &json!({"dev": "pc"})).unwrap();
    assert_eq!(retired(&b), ["lap"]);
    round(&mut b, &rxb);
    assert!(mem.keys(&manifest::device_dir_prefix("lap")).is_empty());
}

fn assert_needs_identity(r: &mut Runner) {
    assert!(r.status().needs_identity, "{:?}", r.status());
    let e = r.command(Op::SyncNow, &Value::Null).unwrap_err();
    assert!(e.contains("请先确认本机身份"), "{e}");
}

/// 新设备凭旧口令改口令：密钥换成远端的，按加入处理，须先确认身份。
#[test]
fn changing_passphrase_from_a_new_device_still_needs_identity() {
    let d = tempfile::tempdir().unwrap();
    let mem = initialized_remote();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    let rep = r
        .command(
            Op::SetPassphrase,
            &json!({"passphrase": "q", "old_passphrase": "p"}),
        )
        .unwrap();
    assert_eq!(rep["mode"], "change");
    assert_needs_identity(&mut r);
}

/// 未确认身份时重输同一口令（走改口令）不会顺带确认身份。
#[test]
fn reentering_passphrase_while_unconfirmed_keeps_needing_identity() {
    let d = tempfile::tempdir().unwrap();
    let mem = initialized_remote();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    r.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    let rep = r
        .command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    assert_eq!(rep["mode"], "change");
    assert_needs_identity(&mut r);
}

/// 已确认的设备改口令后仍是已确认。
#[test]
fn confirmed_device_changing_passphrase_stays_confirmed() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    r.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    let rep = r
        .command(Op::SetPassphrase, &json!({"passphrase": "p2"}))
        .unwrap();
    assert_eq!(rep["mode"], "change");
    assert!(!r.status().needs_identity);
    r.command(Op::SyncNow, &Value::Null).unwrap();
}

fn put_info(mem: &MemBackend, dek: &Dek, id: &str, group: &str, platform: &str) {
    let mut st = SyncState::new(id);
    st.group = group.into();
    let mut info = local_info(&SyncSettings::default(), &st, 1);
    info.platform = platform.into();
    put_device_info(mem, &dek.devinfo_key(), id, &info).unwrap();
}

/// 加入不存在或跨类别的组：报错且不动本机。
#[test]
fn join_group_rejects_missing_or_cross_class_groups() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let dek = remote::init_remote(&mem, "p", KdfParams::FAST_TEST).unwrap();
    put_info(&mem, &dek, "phone", "phones", "android");
    put_info(&mem, &dek, "desk", "office", "linux");
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    std::fs::write(d.path().join("user/config.toml"), "mine = 1\n").unwrap();
    r.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    for g in ["nope", "phones"] {
        let e = r
            .command(Op::JoinGroup, &json!({"group": g, "mode": "theirs"}))
            .unwrap_err();
        assert!(e.contains("不存在"), "{g}: {e}");
    }
    assert_eq!(
        std::fs::read_to_string(d.path().join("user/config.toml")).unwrap(),
        "mine = 1\n"
    );
    assert!(!d.path().join("user/data/.backup").exists());
    assert!(r.status().needs_identity);
    assert_eq!(r.status().device, "pc");
    // 同类别的组可以加入
    r.command(Op::JoinGroup, &json!({"group": "office", "mode": "mine"}))
        .unwrap();
    assert_eq!(r.status().group, "office");
}

/// 旧号在改号后被别处继续发布：本机不删它，并移出待删列表。
#[test]
fn retired_id_reused_elsewhere_is_not_deleted() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let (mut r, rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    r.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    std::fs::write(d.path().join("user/config.toml"), "a = 1\n").unwrap();
    round(&mut r, &rx);
    let copy = r.state.clone();
    r.command(Op::RenameDevice, &json!({"id": "home"})).unwrap();
    // 复制出的目录以旧号接着发布
    let dek = r.dek().unwrap();
    let mut other = copy;
    let mut local = wind_sync::MemLocal::new();
    local.set("config.toml", "a = 1\n");
    local.set("schemas/x.schema.toml", "x = 1\n");
    let mut ctx = SyncCtx::new(
        ScopeFlags::ALL,
        now_unix(),
        Namespace::new(DeviceClass::Desktop, "pc"),
    );
    ctx.force = true;
    sync_once(&mem, &mut local, &mut other, &dek, &ctx).unwrap();
    assert_eq!(other.gen_, 2);

    round(&mut r, &rx);
    assert!(r.state.retired_ids.is_empty());
    assert!(
        mem.raw_get(&manifest::manifest_key("pc", 2)).is_some(),
        "不删别处的清单"
    );
}

/// 认领的设备没有名册条目：回退的组号已被他人使用时改取空闲组号，且不覆盖本机设置。
#[test]
fn claim_fallback_group_is_deduplicated() {
    let mem = MemBackend::new();
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut a, rxa) = runner_with(da.path(), Box::new(MemFactory(mem.clone())), None, "c");
    a.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    std::fs::write(da.path().join("user/config.toml"), "a = 1\n").unwrap();
    round(&mut a, &rxa);
    let dek = a.dek().unwrap();
    mem.delete(&manifest::device_info_key("c")).unwrap();
    put_info(&mem, &dek, "x", "c", "linux");

    let (mut b, _rxb) = runner_with(db.path(), Box::new(MemFactory(mem.clone())), None, "lap");
    std::fs::write(db.path().join("user/config.toml"), "b = 1\n").unwrap();
    b.command(Op::SetPassphrase, &json!({"passphrase": "p"}))
        .unwrap();
    let rep = b.command(Op::ClaimDevice, &json!({"dev": "c"})).unwrap();
    assert_eq!(rep["group"], "c-2");
    assert_eq!(b.status().device, "c");
    assert_eq!(
        std::fs::read_to_string(db.path().join("user/config.toml")).unwrap(),
        "b = 1\n"
    );
}
