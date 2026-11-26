use super::*;
use crate::secrets::{FileSecretStore, Secrets};
use crate::testing::{FakeCore, MemFactory};
use wind_sync::MemBackend;
use wind_sync::backend::{BackendError, ObjMeta};

const TEST_URL: &str = "http://127.0.0.1/x";

fn test_target(url: &str) -> Target {
    Target::Webdav {
        url: url.into(),
        user: "u".into(),
    }
}

fn runner_with(
    dir: &std::path::Path,
    factory: Box<dyn BackendFactory>,
    dek: Option<Dek>,
    device_name: &str,
) -> (Runner, Receiver<Input>) {
    runner_at(dir, factory, dek, device_name, TEST_URL, TEST_URL)
}

/// `saved_url`：保存凭据时的存储地址；`url`：本次启动的设置里的存储地址。
fn runner_at(
    dir: &std::path::Path,
    factory: Box<dyn BackendFactory>,
    dek: Option<Dek>,
    device_name: &str,
    saved_url: &str,
    url: &str,
) -> (Runner, Receiver<Input>) {
    std::fs::create_dir_all(dir.join("user")).unwrap();
    let state_dir = dir.join("state");
    let store = FileSecretStore::new(state_dir.join(crate::secrets::FILE_NAME));
    let dek_set = dek.is_some();
    Secrets {
        dek: dek.map(|d| d.to_base64()),
        password: Some("pw".into()),
        backend_id: backend_fingerprint(Some(&test_target(saved_url))),
        dek_backend_id: dek_set
            .then(|| backend_fingerprint(Some(&test_target(saved_url))))
            .flatten(),
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
            backend: Some(test_target(url)),
            device_name: device_name.into(),
            ..Default::default()
        },
        link: Arc::new(FakeCore::default()),
        factory,
        timing: Timing::default(),
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

/// 已有同步库的远端与它的同步密钥。
fn initialized_remote() -> (MemBackend, String) {
    let mem = MemBackend::new();
    let key = remote::init_remote(&mem).unwrap().to_key_string();
    (mem, key)
}

/// 用同步密钥加入已有远端后不同步，确认身份后才发布清单。
#[test]
fn joined_device_does_not_sync_until_identity_confirmed() {
    let d = tempfile::tempdir().unwrap();
    let (mem, key) = initialized_remote();
    let (mut r, rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    r.command(Op::ImportKey, &json!({"key": key})).unwrap();
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
    let dek = remote::init_remote(&mem).unwrap();
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
    r.command(Op::ImportKey, &json!({"key": dek.to_key_string()}))
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
    let rep = r.command(Op::CreateKey, &Value::Null).unwrap();
    assert!(rep["key"].as_str().is_some_and(|k| k.len() == 64));
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
    r.command(Op::CreateKey, &Value::Null).unwrap();
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

/// 重新导入本机已持有的同步密钥不会让身份回到未确认。
#[test]
fn reimporting_the_held_key_keeps_identity() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    r.command(Op::CreateKey, &Value::Null).unwrap();
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
    a.command(Op::CreateKey, &Value::Null).unwrap();
    round(&mut a, &rxa);
    let (mut b, rxb) = runner_with(db.path(), Box::new(MemFactory(mem.clone())), None, "lap");
    let key = a.dek().unwrap().to_key_string();
    b.command(Op::ImportKey, &json!({"key": key})).unwrap();
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

/// 新设备粘贴同步密钥加入：按加入处理，须先确认身份。
#[test]
fn importing_key_on_a_new_device_needs_identity() {
    let d = tempfile::tempdir().unwrap();
    let (mem, key) = initialized_remote();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    r.command(Op::ImportKey, &json!({"key": key})).unwrap();
    assert_needs_identity(&mut r);
}

/// 未确认身份时再次粘贴同一密钥不会顺带确认身份。
#[test]
fn reimporting_key_while_unconfirmed_keeps_needing_identity() {
    let d = tempfile::tempdir().unwrap();
    let (mem, key) = initialized_remote();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    r.command(Op::ImportKey, &json!({"key": key.clone()}))
        .unwrap();
    r.command(Op::ImportKey, &json!({"key": key})).unwrap();
    assert_needs_identity(&mut r);
}

/// 空远端生成同步密钥：身份自动确认，可直接同步，导出的密钥与回执一致。
#[test]
fn create_key_on_empty_remote_confirms_identity() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    let rep = r.command(Op::CreateKey, &Value::Null).unwrap();
    let key = rep["key"].as_str().unwrap().to_string();
    assert!(Dek::from_key_string(&key).is_ok());
    assert!(!r.status().needs_identity);
    assert!(r.status().joined);
    assert_eq!(
        r.command(Op::ExportKey, &Value::Null).unwrap()["key"],
        json!(key)
    );
    r.command(Op::SyncNow, &Value::Null).unwrap();
}

/// 远端已有同步库时不能再生成密钥。
#[test]
fn create_key_on_existing_remote_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let (mem, _key) = initialized_remote();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    let e = r.command(Op::CreateKey, &Value::Null).unwrap_err();
    assert_eq!(e, "远端已有同步库，请粘贴同步密钥加入");
    assert!(!r.status().joined);
}

/// 同步密钥错、格式坏、远端没有同步库：各自的提示，且不动本机。
#[test]
fn import_key_rejects_wrong_malformed_and_missing_remote() {
    let d = tempfile::tempdir().unwrap();
    let (mem, key) = initialized_remote();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    for bad in [Dek::generate().to_key_string(), "AAAA".into(), "".into()] {
        let e = r.command(Op::ImportKey, &json!({"key": bad})).unwrap_err();
        assert_eq!(e, "同步密钥不对");
    }
    assert!(!r.status().joined);
    r.command(Op::ImportKey, &json!({"key": key.to_uppercase()}))
        .unwrap();
    assert!(r.status().joined);

    let d2 = tempfile::tempdir().unwrap();
    let (mut empty, _rx2) = runner_with(
        d2.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    let e = empty
        .command(Op::ImportKey, &json!({"key": key}))
        .unwrap_err();
    assert_eq!(e, "远端还没有同步库");
}

/// 重置远端：换新密钥并回执，旧密钥不能再加入，本机身份确认。
#[test]
fn reset_remote_issues_a_new_key_and_confirms_identity() {
    let d = tempfile::tempdir().unwrap();
    let (mem, old) = initialized_remote();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    r.command(Op::ImportKey, &json!({"key": old.clone()}))
        .unwrap();
    assert!(r.status().needs_identity);
    let e = r.command(Op::ResetRemote, &json!({})).unwrap_err();
    assert!(e.contains("confirm"), "{e}");
    let rep = r
        .command(Op::ResetRemote, &json!({"confirm": true}))
        .unwrap();
    let fresh = rep["key"].as_str().unwrap().to_string();
    assert_ne!(fresh, old);
    assert!(!r.status().needs_identity);
    let b = mem.clone();
    assert!(remote::join_with_key(&b, &old).is_err());
    assert!(remote::join_with_key(&b, &fresh).is_ok());
}

fn put_info(mem: &MemBackend, dek: &Dek, id: &str, group: &str, platform: &str) {
    let mut st = SyncState::new(id);
    st.group = group.into();
    let mut info = local_info(&SyncSettings::default(), &st, 1);
    info.platform = platform.into();
    put_device_info(mem, &dek.devinfo_key(), id, &info).unwrap();
}

/// 不能恢复为另一类设备（桌面认领手机）：报错、不备份、不动本机。
#[test]
fn claim_rejects_cross_class_device() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let dek = remote::init_remote(&mem).unwrap();
    put_info(&mem, &dek, "phone", "phones", "android");
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    r.command(Op::ImportKey, &json!({"key": dek.to_key_string()}))
        .unwrap();
    let e = r
        .command(Op::ClaimDevice, &json!({"dev": "phone"}))
        .unwrap_err();
    assert!(e.contains("不是同一类设备"), "{e}");
    assert!(r.status().needs_identity);
    assert!(!d.path().join("user/data/.backup").exists());
}

/// 加入不存在或跨类别的组：报错且不动本机。
#[test]
fn join_group_rejects_missing_or_cross_class_groups() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let dek = remote::init_remote(&mem).unwrap();
    put_info(&mem, &dek, "phone", "phones", "android");
    put_info(&mem, &dek, "desk", "office", "linux");
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem)), None, "pc");
    std::fs::write(d.path().join("user/config.toml"), "mine = 1\n").unwrap();
    r.command(Op::ImportKey, &json!({"key": dek.to_key_string()}))
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
    r.command(Op::CreateKey, &Value::Null).unwrap();
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
    let mut ctx = SyncCtx::new(now_unix(), Namespace::new(DeviceClass::Desktop, "pc"));
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
    a.command(Op::CreateKey, &Value::Null).unwrap();
    std::fs::write(da.path().join("user/config.toml"), "a = 1\n").unwrap();
    round(&mut a, &rxa);
    let dek = a.dek().unwrap();
    mem.delete(&manifest::device_info_key("c")).unwrap();
    put_info(&mem, &dek, "x", "c", "linux");

    let (mut b, _rxb) = runner_with(db.path(), Box::new(MemFactory(mem.clone())), None, "lap");
    std::fs::write(db.path().join("user/config.toml"), "b = 1\n").unwrap();
    b.command(Op::ImportKey, &json!({"key": dek.to_key_string()}))
        .unwrap();
    let rep = b.command(Op::ClaimDevice, &json!({"dev": "c"})).unwrap();
    assert_eq!(rep["group"], "c-2");
    assert_eq!(b.status().device, "c");
    assert_eq!(
        std::fs::read_to_string(db.path().join("user/config.toml")).unwrap(),
        "b = 1\n"
    );
}

/// 身份未确认的本机号在名册里属于别人、组号恰为回退号：回退取别的组号。
#[test]
fn claim_fallback_group_checks_own_roster_entry() {
    let mem = MemBackend::new();
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut a, rxa) = runner_with(da.path(), Box::new(MemFactory(mem.clone())), None, "c");
    a.command(Op::CreateKey, &Value::Null).unwrap();
    std::fs::write(da.path().join("user/config.toml"), "a = 1\n").unwrap();
    round(&mut a, &rxa);
    let dek = a.dek().unwrap();
    mem.delete(&manifest::device_info_key("c")).unwrap();
    put_info(&mem, &dek, "lap", "c", "linux");

    let (mut b, _rxb) = runner_with(db.path(), Box::new(MemFactory(mem.clone())), None, "lap");
    b.command(Op::ImportKey, &json!({"key": dek.to_key_string()}))
        .unwrap();
    let rep = b.command(Op::ClaimDevice, &json!({"dev": "c"})).unwrap();
    assert_ne!(rep["group"], "c");
}

struct FailFactory;

impl BackendFactory for FailFactory {
    fn build(&self, _: &SyncSettings, _: &str) -> Result<Box<dyn Backend>, String> {
        Err("连接被拒绝".into())
    }
}

fn setup_pc(r: &mut Runner) {
    r.command(Op::CreateKey, &Value::Null).unwrap();
    r.command(Op::ConfirmIdentity, &Value::Null).ok();
}

#[test]
fn sync_now_returns_ticket_and_round_advances() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    setup_pc(&mut r);
    std::fs::write(d.path().join("user/config.toml"), "a = 1\n").unwrap();
    let before = r.status().round;
    let rep = r.command(Op::SyncNow, &Value::Null).unwrap();
    assert_eq!(rep["queued"], true);
    assert_eq!(rep["ticket"], before + 1);
    round(&mut r, &rx);
    let st = r.status();
    assert_eq!(st.round, before + 1);
    let last = st.last_round.unwrap();
    assert!(last.ok && last.forced, "{last:?}");
    assert_eq!(last.uploaded, 1);
    assert_eq!(last.error, None);
}

#[test]
fn sync_now_while_syncing_reports_busy() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    setup_pc(&mut r);
    r.command(Op::SyncNow, &Value::Null).unwrap();
    r.start_round();
    let rep = r.command(Op::SyncNow, &Value::Null).unwrap();
    assert_eq!(rep["queued"], false);
    assert_eq!(rep["busy"], true);
    assert_eq!(rep["ticket"], r.status().round + 1);
    let x = wait_round(&rx);
    r.finish_round(*x);
    assert!(!r.force_now, "不应再排一轮");
    assert_eq!(r.status().round, 1);
}

#[test]
fn failed_round_still_advances_round() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    setup_pc(&mut r);
    r.factory = Arc::new(FailFactory);
    r.command(Op::SyncNow, &Value::Null).unwrap();
    round(&mut r, &rx);
    let st = r.status();
    assert_eq!(st.round, 1);
    let last = st.last_round.unwrap();
    assert!(!last.ok && last.forced, "{last:?}");
    assert!(last.error.unwrap().contains("连接被拒绝"));
}

#[test]
fn free_retry_round_does_not_advance_round() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    setup_pc(&mut r);
    r.command(Op::SyncNow, &Value::Null).unwrap();
    r.start_round();
    let mut x = wait_round(&rx);
    x.result = Ok(Err(SyncError::Local(format!("{RETRY_TAG}文件正被编辑"))));
    r.finish_round(*x);
    assert_eq!(r.status().round, 0);
    assert_eq!(r.status().last_round, None);
    round(&mut r, &rx);
    let st = r.status();
    assert_eq!(st.round, 1);
    let last = st.last_round.unwrap();
    assert!(last.ok && last.forced, "{last:?}");
}

#[test]
fn skipped_round_does_not_advance_round() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    setup_pc(&mut r);
    r.start_round();
    let mut x = wait_round(&rx);
    x.result = Ok(Ok(SyncReport {
        skipped: true,
        ..Default::default()
    }));
    r.finish_round(*x);
    assert_eq!(r.status().round, 0);
}

/// 命令改变了可报告状态时，Status 必须先于 CommandReply 发出（core 才不会拿旧的停摆态放弃票）。
#[test]
fn status_is_reported_before_command_reply() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner_with(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        None,
        "pc",
    );
    setup_pc(&mut r);
    let core = Arc::new(FakeCore::default());
    r.cfg.link = core.clone();
    r.state.retry.halted = Some("停摆原因".into());
    r.report();
    assert_eq!(r.status().phase, "halted");
    r.handle(Input::Core(ToSync::Command {
        id: 7,
        op: Op::SyncNow,
        args: Value::Null,
    }));
    let sent = core.sent.lock().unwrap();
    let reply = sent
        .iter()
        .position(|m| matches!(m, FromSync::CommandReply { .. }))
        .expect("应有回执");
    assert!(
        matches!(&sent[reply - 1], FromSync::Status(s) if s.phase != "halted"),
        "回执前应先收到非停摆的状态: {sent:?}"
    );
}

/// 发布 meta 总是失败的内存后端。
#[derive(Clone)]
struct MetaPutFails(MemBackend);

impl Backend for MetaPutFails {
    fn list(&self, prefix: &str) -> Result<Vec<ObjMeta>, BackendError> {
        self.0.list(prefix)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, BackendError> {
        self.0.get(key)
    }
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), BackendError> {
        if key == manifest::meta_key() {
            return Err(BackendError::Transient("写 meta 失败（桩）".into()));
        }
        self.0.put(key, bytes)
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.0.delete(key)
    }
}

struct MetaPutFailsFactory(MetaPutFails);

impl BackendFactory for MetaPutFailsFactory {
    fn build(&self, _: &SyncSettings, _: &str) -> Result<Box<dyn Backend>, String> {
        Ok(Box::new(self.0.clone()))
    }
}

/// 写入总是失败的机密存储。
struct FailingStore;

impl crate::secrets::SecretStore for FailingStore {
    fn read(&self) -> std::io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn write(&self, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("磁盘已满（桩）"))
    }
    fn delete(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 本机存不下密钥：不建库，远端保持为空。
#[test]
fn create_key_does_not_publish_when_local_save_fails() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    r.cfg.secrets = Box::new(FailingStore);
    assert!(r.command(Op::CreateKey, &Value::Null).is_err());
    assert!(mem.raw_get(&manifest::meta_key()).is_none());
    assert!(r.secrets.dek.is_none());
}

/// 发布 meta 失败：本机密钥退回原值，不留下用户没见过的密钥。
#[test]
fn create_key_rolls_back_local_key_when_publish_fails() {
    let d = tempfile::tempdir().unwrap();
    let f = MetaPutFails(MemBackend::new());
    let (mut r, _rx) = runner_with(d.path(), Box::new(MetaPutFailsFactory(f)), None, "pc");
    assert!(r.command(Op::CreateKey, &Value::Null).is_err());
    assert!(r.secrets.dek.is_none());
    assert!(!r.status().joined);
    // 磁盘上的也是旧值（重新从存储读）
    assert!(Secrets::load(&*r.cfg.secrets).unwrap().dek.is_none());
}

/// 重置时发布失败：本机仍持有原密钥。
#[test]
fn reset_remote_keeps_old_local_key_when_publish_fails() {
    let d = tempfile::tempdir().unwrap();
    let dek = Dek::generate();
    let old = dek.to_base64();
    let f = MetaPutFails(MemBackend::new());
    let (mut r, _rx) = runner_with(d.path(), Box::new(MetaPutFailsFactory(f)), Some(dek), "pc");
    assert!(
        r.command(Op::ResetRemote, &json!({"confirm": true}))
            .is_err()
    );
    assert_eq!(r.secrets.dek.as_deref(), Some(old.as_str()));
}

/// 重置时本机存不下新密钥：不动远端。
#[test]
fn reset_remote_does_not_touch_remote_when_local_save_fails() {
    let d = tempfile::tempdir().unwrap();
    let (mem, _key) = initialized_remote();
    mem.raw_put("windinput-sync/v1/blobs/x", vec![1]);
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    r.cfg.secrets = Box::new(FailingStore);
    assert!(
        r.command(Op::ResetRemote, &json!({"confirm": true}))
            .is_err()
    );
    assert!(mem.raw_get("windinput-sync/v1/blobs/x").is_some());
}

/// 停用期间改了存储地址：下次启动丢弃旧凭据，且不会拿它去连新地址；磁盘上也已清掉。
#[test]
fn startup_drops_password_when_backend_changed_while_disabled() {
    let d = tempfile::tempdir().unwrap();
    let (r, _rx) = runner_at(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        Some(Dek::generate()),
        "pc",
        TEST_URL,
        "http://evil.example/x",
    );
    assert!(r.secrets.password.is_none());
    assert!(r.secrets.dek.is_some(), "同步密钥与存储位置无关，保留");
    assert!(r.fresh_backend(None).is_err(), "没有凭据可用");
    let store = FileSecretStore::new(d.path().join("state").join(crate::secrets::FILE_NAME));
    assert!(Secrets::load(&store).unwrap().password.is_none());
}

/// 存储位置没变：凭据保留。
#[test]
fn startup_keeps_password_when_backend_unchanged() {
    let d = tempfile::tempdir().unwrap();
    let (r, _rx) = runner(d.path());
    assert_eq!(r.secrets.password.as_deref(), Some("pw"));
}

/// 旧版没有指纹的凭据按「对不上」处理。
#[test]
fn startup_drops_password_without_fingerprint() {
    let d = tempfile::tempdir().unwrap();
    let state_dir = d.path().join("state");
    let store = FileSecretStore::new(state_dir.join(crate::secrets::FILE_NAME));
    Secrets {
        dek: None,
        password: Some("pw".into()),
        backend_id: None,
        dek_backend_id: None,
    }
    .save(&store)
    .unwrap();
    let mut s = Secrets::load(&store).unwrap();
    assert!(drop_password_if_backend_moved(
        &mut s,
        Some(&test_target(TEST_URL))
    ));
    assert!(s.password.is_none());
}

/// 换了存储：旧密钥对不上新库 -> 未加入；同一个库搬了地方 -> 自动已加入；新地址是空库 -> 未加入。
#[test]
fn test_connection_joined_needs_matching_remote_meta() {
    let d = tempfile::tempdir().unwrap();
    let old_remote = MemBackend::new();
    let dek = remote::init_remote(&old_remote).unwrap();
    let make = |remote: MemBackend, url: &str, dir: &std::path::Path| {
        runner_at(
            dir,
            Box::new(MemFactory(remote)),
            Some(Dek::from_base64(&dek.to_base64()).unwrap()),
            "pc",
            TEST_URL,
            url,
        )
    };
    // 别的库（有 meta 但密钥不同）
    let (mut r, _rx) = make(initialized_remote().0, "http://other.example/x", d.path());
    assert!(!r.status().joined, "地址变了，未核对前不算已加入");
    let t = r
        .command(Op::TestConnection, &json!({"password": "pw"}))
        .unwrap();
    assert_eq!(
        (t["remote_initialized"].as_bool(), t["joined"].as_bool()),
        (Some(true), Some(false))
    );
    assert!(!r.status().joined);

    // 同一个库换了地址：核对通过后自动已加入，密钥保留
    let d2 = tempfile::tempdir().unwrap();
    let (mut r, _rx) = make(old_remote.clone(), "http://moved.example/x", d2.path());
    assert!(!r.status().joined);
    let t = r
        .command(Op::TestConnection, &json!({"password": "pw"}))
        .unwrap();
    assert_eq!(t["joined"], true);
    assert!(r.status().joined);
    assert!(r.secrets.dek.is_some());

    // 空的新库
    let d3 = tempfile::tempdir().unwrap();
    let (mut r, _rx) = make(MemBackend::new(), "http://empty.example/x", d3.path());
    let t = r
        .command(Op::TestConnection, &json!({"password": "pw"}))
        .unwrap();
    assert_eq!(
        (t["remote_initialized"].as_bool(), t["joined"].as_bool()),
        (Some(false), Some(false))
    );
    assert!(!r.status().joined);
}

fn s3(prefix: &str) -> Target {
    Target::S3 {
        endpoint: "https://s3.example".into(),
        region: String::new(),
        bucket: "b".into(),
        prefix: prefix.into(),
        access_key_id: "k".into(),
        path_style: false,
    }
}

/// S3 同一端点与桶、前缀不同：库位置不同；凭据指纹不变。
#[test]
fn library_id_includes_s3_prefix_but_credential_fingerprint_does_not() {
    assert_ne!(library_id(Some(&s3("a"))), library_id(Some(&s3("b"))));
    assert_eq!(library_id(Some(&s3("a"))), library_id(Some(&s3("/a/"))));
    assert_eq!(
        backend_fingerprint(Some(&s3("a"))),
        backend_fingerprint(Some(&s3("b")))
    );
}

/// 核对结论只对核对时的位置有效，位置变了不被标成已核对。
#[test]
fn mark_verified_ignores_stale_library() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner_at(
        d.path(),
        Box::new(MemFactory(MemBackend::new())),
        Some(Dek::generate()),
        "pc",
        "http://old.example/x",
        "http://new.example/x",
    );
    assert!(!r.joined());
    r.mark_verified(library_id(Some(&test_target("http://old.example/x"))));
    assert!(!r.joined());
    r.mark_verified(library_id(Some(&test_target("http://new.example/x"))));
    assert!(r.joined());
}

/// 远端库没了或密钥对不上：之前的已加入结论作废（test_connection 与同步轮）。
#[test]
fn verified_flag_is_invalidated_by_missing_or_foreign_remote() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let dek = remote::init_remote(&mem).unwrap();
    let (mut r, rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), Some(dek), "pc");
    assert!(r.joined());
    // 远端被别人重置成另一把密钥
    remote::reset_remote(&mem).unwrap();
    let t = r.command(Op::TestConnection, &Value::Null).unwrap();
    assert_eq!(t["joined"], false);
    assert!(!r.joined());
    let disk = Secrets::load(&*r.cfg.secrets).unwrap();
    assert!(disk.dek_backend_id.is_none());

    // 同步轮遇到 BadKey 同样作废
    let d2 = tempfile::tempdir().unwrap();
    let mem2 = MemBackend::new();
    let dek2 = remote::init_remote(&mem2).unwrap();
    let (mut r2, rx2) = runner_with(
        d2.path(),
        Box::new(MemFactory(mem2.clone())),
        Some(dek2),
        "pc",
    );
    r2.command(Op::ConfirmIdentity, &Value::Null).ok();
    remote::reset_remote(&mem2).unwrap();
    assert!(r2.joined());
    r2.command(Op::SyncNow, &Value::Null).ok();
    round(&mut r2, &rx2);
    assert!(!r2.joined());
    let _ = rx;
}

fn empty_round(r: &Runner, learn: Option<Result<(), String>>, now: u64) -> Round {
    Round {
        state: r.state.clone(),
        result: Err("x".into()),
        conflicts: Vec::new(),
        now,
        echo_before: HashMap::new(),
        failures_before: 0,
        library_id: None,
        info_written: None,
        learn,
        removed_with_data: None,
    }
}

#[test]
fn learn_skipped_within_30min() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner(d.path());
    r.learn_last_ok = 10_000;
    assert!(!r.learn_due(10_000 + LEARN_INTERVAL_SECS - 1, false));
    assert!(r.learn_due(10_000 + LEARN_INTERVAL_SECS, false));
}

#[test]
fn forced_round_runs_learn() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner(d.path());
    r.learn_last_ok = 10_000;
    assert!(r.learn_due(10_001, true));
}

#[test]
fn failed_learn_does_not_advance_last_ok() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner(d.path());
    r.learn_last_ok = 5;
    let x = empty_round(&r, Some(Err("boom".into())), 9_999);
    r.finish_round(x);
    assert_eq!(r.learn_last_ok, 5);
}

#[test]
fn failed_learn_backs_off_instead_of_retrying_every_round() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner(d.path());
    let t = 100_000;
    r.finish_round(empty_round(&r, Some(Err("boom".into())), t));
    assert!(!r.learn_due(t + 1, false), "失败后不在下一轮立刻重跑");
    assert!(r.learn_due(t + backoff_secs(1), false));
    assert!(r.learn_due(t + 1, true), "强制不受退避限制");
    let t2 = t + backoff_secs(1);
    r.finish_round(empty_round(&r, Some(Err("boom".into())), t2));
    assert!(
        !r.learn_due(t2 + backoff_secs(1), false),
        "连续失败退避加长"
    );
    assert!(r.learn_due(t2 + backoff_secs(2), false));
    r.finish_round(empty_round(&r, Some(Ok(())), t2 + backoff_secs(2)));
    assert_eq!((r.learn_failures, r.learn_retry_at), (0, 0));
}

#[test]
fn ok_learn_advances_last_ok() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner(d.path());
    let x = empty_round(&r, Some(Ok(())), 9_999);
    r.finish_round(x);
    assert_eq!(r.learn_last_ok, 9_999);
}

#[test]
fn learn_round_runs_when_any_content_on() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner(d.path());
    r.settings.content = wind_sync_ipc::SyncContent {
        learned: false,
        completion: true,
        stats: false,
    };
    assert!(r.learn_wanted(LEARN_INTERVAL_SECS, false));
}

#[test]
fn learn_round_never_runs_when_all_content_off() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, _rx) = runner(d.path());
    r.settings.content = wind_sync_ipc::SyncContent {
        learned: false,
        completion: false,
        stats: false,
    };
    assert!(!r.learn_wanted(u64::MAX / 2, true));
}

/// 工作线程跑一轮学习数据：本机贡献发布到学习通道，成功时间记入私有状态，重启后载入。
#[test]
fn round_publishes_learned_data_and_restart_loads_last_ok() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let (mut r, rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    setup_pc(&mut r);
    let freq = d.path().join("user/data/pinyin/freq.table.yaml");
    std::fs::create_dir_all(freq.parent().unwrap()).unwrap();
    std::fs::write(
        &freq,
        "---\ncolumns: [code, text, count, last_used]\nwind:\n  kind: freq\n  escape: true\n...\n# no comment\nni\t你\t3\t1\n",
    )
    .unwrap();
    r.command(Op::SyncNow, &Value::Null).unwrap();
    round(&mut r, &rx);
    assert!(r.learn_last_ok > 0);
    let prefix = wind_sync::manifest::learn_prefix(&r.state.device);
    assert!(
        mem.keys(&prefix).iter().any(|k| k.contains("/index.")),
        "{:?}",
        mem.keys(&prefix)
    );
    let last_ok = r.learn_last_ok;
    let dek = r.dek().unwrap();
    drop(r);
    let (r2, _rx2) = runner_with(d.path(), Box::new(MemFactory(mem)), Some(dek), "pc");
    assert_eq!(r2.learn_last_ok, last_ok);
}

// ── 学习数据：改号、移除、清除 ─────────────────────────

const FREQ_FILE: &str = "user/data/pinyin/freq.table.yaml";

fn write_freq(dir: &std::path::Path, n: u64) {
    let p = dir.join(FREQ_FILE);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        format!("---\ncolumns: [code, text, count, last_used]\nwind:\n  kind: freq\n  escape: true\n...\n# no comment\nni\t你\t{n}\t1\n"),
    )
    .unwrap();
}

fn read_freq(dir: &std::path::Path) -> Option<u64> {
    use wind_sync::learn::{Key, LearnKind, norm_key, table};
    let b = std::fs::read(dir.join(FREQ_FILE)).ok()?;
    let f = table::parse(LearnKind::Freq, Some(&b)).unwrap();
    let nk = norm_key(LearnKind::Freq, &Key("ni".into(), "你".into()));
    f.rows.get(&nk).map(|r| r.1)
}

fn forced_round(r: &mut Runner, rx: &Receiver<Input>) {
    r.command(Op::SyncNow, &Value::Null).unwrap();
    round(r, rx);
}

fn has_learn(mem: &MemBackend, dev: &str) -> bool {
    !mem.keys(&wind_sync::manifest::learn_prefix(dev)).is_empty()
}

#[test]
fn rename_moves_prefix() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let (mut r, rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    setup_pc(&mut r);
    write_freq(d.path(), 3);
    forced_round(&mut r, &rx);
    assert!(has_learn(&mem, "pc"));
    r.command(Op::RenameDevice, &json!({"id": "home"})).unwrap();
    assert!(
        !has_learn(&mem, "pc"),
        "{:?}",
        mem.keys("windinput-sync/v1/learn/")
    );
    assert!(has_learn(&mem, "home"));
    forced_round(&mut r, &rx);
    assert!(!has_learn(&mem, "pc"));
    assert_eq!(read_freq(d.path()), Some(3));
}

/// pc 3、lap 4 同步到 7。
struct Pair {
    da: tempfile::TempDir,
    a: Runner,
    rxa: Receiver<Input>,
    db: tempfile::TempDir,
    mem: MemBackend,
    b: Runner,
    rxb: Receiver<Input>,
}

fn learn_pair() -> Pair {
    let mem = MemBackend::new();
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut a, rxa) = runner_with(da.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    setup_pc(&mut a);
    write_freq(da.path(), 3);
    forced_round(&mut a, &rxa);
    let (mut b, rxb) = runner_with(db.path(), Box::new(MemFactory(mem.clone())), None, "lap");
    let key = a.dek().unwrap().to_key_string();
    b.command(Op::ImportKey, &json!({"key": key})).unwrap();
    b.command(Op::ConfirmIdentity, &Value::Null).unwrap();
    write_freq(db.path(), 4);
    forced_round(&mut b, &rxb);
    forced_round(&mut a, &rxa);
    assert_eq!(read_freq(da.path()), Some(7));
    assert_eq!(read_freq(db.path()), Some(7));
    Pair {
        da,
        a,
        rxa,
        db,
        mem,
        b,
        rxb,
    }
}

#[test]
fn remove_keeps_learn_data_and_counts() {
    let mut p = learn_pair();
    p.b.command(Op::RemoveDevice, &json!({"dev": "pc", "confirm": true}))
        .unwrap();
    assert!(has_learn(&p.mem, "pc"));
    assert_eq!(p.b.status().removed_with_data, ["pc"]);
    forced_round(&mut p.b, &p.rxb);
    assert_eq!(read_freq(p.db.path()), Some(7));
    assert_eq!(p.b.status().removed_with_data, ["pc"]);
}

#[test]
fn purge_drops_device_and_counts_fall() {
    let mut p = learn_pair();
    let e =
        p.b.command(Op::PurgeDevice, &json!({"device": "pc"}))
            .unwrap_err();
    assert!(e.contains("名册"), "{e}");
    p.b.command(Op::RemoveDevice, &json!({"dev": "pc", "confirm": true}))
        .unwrap();
    assert!(
        p.b.command(Op::PurgeDevice, &json!({"device": "lap"}))
            .is_err()
    );
    p.b.command(Op::PurgeDevice, &json!({"device": "pc"}))
        .unwrap();
    assert!(!has_learn(&p.mem, "pc"));
    assert!(p.b.status().removed_with_data.is_empty());
    forced_round(&mut p.b, &p.rxb);
    assert_eq!(read_freq(p.db.path()), Some(4));
}

#[test]
fn reset_remote_then_rejoin_keeps_totals() {
    let mut p = learn_pair();
    let key =
        p.a.command(Op::ResetRemote, &json!({"confirm": true}))
            .unwrap()["key"]
            .as_str()
            .unwrap()
            .to_string();
    forced_round(&mut p.a, &p.rxa);
    write_freq(p.db.path(), 9);
    p.b.command(Op::ImportKey, &json!({"key": key})).unwrap();
    p.b.command(Op::ConfirmIdentity, &Value::Null).unwrap();
    forced_round(&mut p.b, &p.rxb);
    forced_round(&mut p.a, &p.rxa);
    assert_eq!(read_freq(p.da.path()), Some(9));
    assert_eq!(read_freq(p.db.path()), Some(9));
}

#[test]
fn create_key_fails_when_learn_state_cannot_be_written() {
    let d = tempfile::tempdir().unwrap();
    let mem = MemBackend::new();
    let (mut r, _rx) = runner_with(d.path(), Box::new(MemFactory(mem.clone())), None, "pc");
    std::fs::write(d.path().join("state/learn"), "x").unwrap();
    let e = r.command(Op::CreateKey, &Value::Null).unwrap_err();
    assert!(e.contains("学习数据"), "{e}");
    assert!(mem.raw_get(&manifest::meta_key()).is_none(), "不建库");
    assert!(r.secrets.dek.is_none(), "本机密钥回滚");
}

#[test]
fn reset_failure_leaves_no_new_library_state() {
    let d = tempfile::tempdir().unwrap();
    let flaky = Flaky {
        inner: MemBackend::new(),
        fail_delete: Arc::new(Mutex::new(None)),
    };
    let (mut r, rx) = runner_with(d.path(), Box::new(FlakyFactory(flaky.clone())), None, "pc");
    setup_pc(&mut r);
    write_freq(d.path(), 3);
    forced_round(&mut r, &rx);
    let dirs = || -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d.path().join("state/learn"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    let before = dirs();
    *lock(&flaky.fail_delete) = Some("windinput-sync/".into());
    assert!(
        r.command(Op::ResetRemote, &json!({"confirm": true}))
            .is_err()
    );
    assert_eq!(dirs(), before);
}

#[test]
fn ids_with_learn_data_count_as_taken() {
    let mut p = learn_pair();
    p.b.command(Op::RemoveDevice, &json!({"dev": "pc", "confirm": true}))
        .unwrap();
    let e =
        p.b.command(Op::RenameDevice, &json!({"id": "pc"}))
            .unwrap_err();
    assert!(e.contains("不可用"), "{e}");
    let dc = tempfile::tempdir().unwrap();
    let (mut c, _rxc) = runner_with(dc.path(), Box::new(MemFactory(p.mem.clone())), None, "pc");
    let key = p.b.dek().unwrap().to_key_string();
    c.command(Op::ImportKey, &json!({"key": key})).unwrap();
    c.command(Op::ConfirmIdentity, &Value::Null).unwrap();
    assert_eq!(c.status().device, "pc-2");
}

#[test]
fn interrupted_reset_does_not_drop_devices() {
    let mem = MemBackend::new();
    let flaky = Flaky {
        inner: mem.clone(),
        fail_delete: Arc::new(Mutex::new(None)),
    };
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut a, rxa) = runner_with(da.path(), Box::new(FlakyFactory(flaky.clone())), None, "pc");
    setup_pc(&mut a);
    write_freq(da.path(), 3);
    forced_round(&mut a, &rxa);
    let (mut b, rxb) = runner_with(db.path(), Box::new(MemFactory(mem.clone())), None, "lap");
    let key = a.dek().unwrap().to_key_string();
    b.command(Op::ImportKey, &json!({"key": key})).unwrap();
    b.command(Op::ConfirmIdentity, &Value::Null).unwrap();
    write_freq(db.path(), 4);
    forced_round(&mut b, &rxb);
    forced_round(&mut a, &rxa);
    assert_eq!(read_freq(db.path()), Some(7));
    // 重置删到 learn/pc 之后、meta 之前的某个对象时失败
    mem.raw_put("windinput-sync/v1/learn/zz/obj/x", vec![1]);
    *lock(&flaky.fail_delete) = Some("/learn/zz/".into());
    assert!(
        a.command(Op::ResetRemote, &json!({"confirm": true}))
            .is_err()
    );
    assert!(!has_learn(&mem, "pc"));
    b.command(Op::SyncNow, &Value::Null).unwrap();
    round(&mut b, &rxb);
    assert_eq!(read_freq(db.path()), Some(7), "半毁的远端上不应丢设备");
}

fn replace_sent(core: &FakeCore) -> bool {
    core.methods().iter().any(|m| m == "stats.replaceMonths")
}

/// 已确认身份、原号有学习通道的设备认领别的号：本机统计换成被认领号的（原号继续作为他机计入）。
#[test]
fn claim_by_device_with_learn_lane_replaces_stats() {
    let mut p = learn_pair();
    let core = Arc::new(FakeCore::default());
    p.b.cfg.link = core.clone();
    p.b.command(Op::ClaimDevice, &json!({"dev": "pc"})).unwrap();
    assert!(replace_sent(&core));
}

/// 新装的设备（未确认身份）认领：本机统计按天合并，不整体替换。
#[test]
fn claim_by_new_device_merges_stats() {
    let p = learn_pair();
    let dc = tempfile::tempdir().unwrap();
    let (mut c, _rxc) = runner_with(dc.path(), Box::new(MemFactory(p.mem.clone())), None, "new");
    let core = Arc::new(FakeCore::default());
    c.cfg.link = core.clone();
    let key = p.a.dek().unwrap().to_key_string();
    c.command(Op::ImportKey, &json!({"key": key})).unwrap();
    c.command(Op::ClaimDevice, &json!({"dev": "pc"})).unwrap();
    assert!(!replace_sent(&core));
}
