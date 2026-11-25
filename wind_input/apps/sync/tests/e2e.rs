//! 库级端到端：两三个「设备」（各自的用户目录、状态目录、机密文件、假 core）经同一个内存后端同步。
//! 主循环、文件监听、命令分派、LocalStore 写入与 core 请求序列都是真实代码；
//! 只有 core（假通道，请求一律成功）和远端存储（内存）是替身。进程级见 `process.rs`。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use wind_sync::{DeviceClass, MemBackend, backend::BackendError};
use wind_sync_host::secrets::{FILE_NAME, FileSecretStore};
use wind_sync_host::testing::{FakeCore, MemFactory};
use wind_sync_host::{Config, Input, Runner, Timing};
use wind_sync_ipc::{Backend, FromSync, Op, Status, SyncSettings, ToSync};

const WAIT: Duration = Duration::from_secs(15);
const UD: &str = "data/pinyin/user.dict.yaml";
/// 所有设备共享的资源文件（设置类文件按组同步，跨组断言用它）。
const SCHEMA: &str = "schemas/s.schema.toml";

fn settings() -> SyncSettings {
    SyncSettings {
        enabled: true,
        backend: Some(Backend::Webdav {
            url: "http://example.invalid/dav".into(),
            user: "u".into(),
        }),
        ..Default::default()
    }
}

struct Node {
    dir: TempDir,
    core: Arc<FakeCore>,
    tx: Sender<Input>,
    handle: Option<JoinHandle<()>>,
    next_id: u64,
    remote: MemBackend,
}

fn timing() -> Timing {
    Timing {
        first_delay: Duration::from_secs(3600),
        debounce: Duration::from_millis(300),
        settings_delay: Duration::from_secs(3600),
        retry_delay: Duration::from_millis(200),
    }
}

impl Node {
    fn new(remote: &MemBackend, watch: bool) -> Node {
        Self::with(remote, watch, settings())
    }

    fn with(remote: &MemBackend, watch: bool, settings: SyncSettings) -> Node {
        Self::spawn(
            remote,
            watch,
            settings,
            DeviceClass::Desktop,
            false,
            timing(),
        )
    }

    fn spawn(
        remote: &MemBackend,
        watch: bool,
        settings: SyncSettings,
        class: DeviceClass,
        on_demand: bool,
        timing: Timing,
    ) -> Node {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("user")).unwrap();
        let core = Arc::new(FakeCore::default());
        let (tx, rx) = mpsc::channel();
        let state_dir = dir.path().join("state");
        let cfg = Config {
            userdata_dir: dir.path().join("user"),
            secrets: Box::new(FileSecretStore::new(state_dir.join(FILE_NAME))),
            state_dir,
            settings,
            link: core.clone(),
            factory: Box::new(MemFactory(remote.clone())),
            timing,
            watch,
            class,
            portable: false,
            on_demand,
        };
        let tx2 = tx.clone();
        let handle = std::thread::spawn(move || Runner::new(cfg, tx2).run(rx));
        let n = Node {
            dir,
            core,
            tx,
            handle: Some(handle),
            next_id: 1,
            remote: remote.clone(),
        };
        n.wait_status("初始状态", |_| true);
        n
    }

    fn user(&self) -> std::path::PathBuf {
        self.dir.path().join("user")
    }

    fn put(&self, rel: &str, c: &str) {
        let p = self.user().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, c).unwrap();
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.user().join(rel)).ok()
    }

    fn cmd(&mut self, op: Op, args: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.tx
            .send(Input::Core(ToSync::Command { id, op, args }))
            .unwrap();
        let t = Instant::now();
        loop {
            for m in self.core.sent.lock().unwrap().iter() {
                if let FromSync::CommandReply {
                    id: i,
                    result,
                    error,
                } = m
                    && *i == id
                {
                    return match error {
                        Some(e) => Err(e.clone()),
                        None => Ok(result.clone().unwrap_or_default()),
                    };
                }
            }
            assert!(t.elapsed() < WAIT, "命令 {op:?} 没有回执");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn statuses(&self) -> Vec<Status> {
        self.core
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                FromSync::Status(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    fn status(&self) -> Status {
        self.statuses().pop().expect("至少有初始状态")
    }

    fn wait_status(&self, what: &str, f: impl Fn(&Status) -> bool) -> Status {
        let t = Instant::now();
        loop {
            if let Some(s) = self.statuses().last().filter(|s| f(s)) {
                return s.clone();
            }
            assert!(
                t.elapsed() < WAIT,
                "等待状态「{what}」超时，当前 {:?}",
                self.status()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 触发一轮并等它跑完（先见 syncing、再见非 syncing），返回结束状态。
    fn sync(&mut self) -> Status {
        let n = self.statuses().len();
        self.cmd(Op::SyncNow, Value::Null).unwrap();
        let t = Instant::now();
        loop {
            let st = self.statuses();
            if let Some(i) = st[n..].iter().position(|s| s.phase == "syncing")
                && let Some(done) = st[n + i..].iter().find(|s| s.phase != "syncing")
            {
                return done.clone();
            }
            assert!(t.elapsed() < WAIT, "同步没有结束: {:?}", self.status());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn syncing_count(&self) -> usize {
        self.statuses()
            .iter()
            .filter(|s| s.phase == "syncing")
            .count()
    }

    /// 设置凭据，再生成同步密钥（空远端）或粘贴密钥加入（已有同步库，随后确认身份）。
    /// 密钥由首台设备记在远端的测试专用键里，供后来的节点取用。
    fn setup(&mut self) -> Value {
        self.cmd(Op::SetSecret, json!({"password": "pw"})).unwrap();
        if self.remote.raw_get(HARNESS_KEY).is_none() {
            let r = self.cmd(Op::CreateKey, Value::Null).unwrap();
            let key = r["key"].as_str().unwrap().as_bytes().to_vec();
            self.remote.raw_put(HARNESS_KEY, key);
            r
        } else {
            let r = self.join_existing();
            self.cmd(Op::ConfirmIdentity, Value::Null).unwrap();
            r
        }
    }

    /// 已设凭据后用首台设备的同步密钥加入，不确认身份。
    fn join_existing(&mut self) -> Value {
        let key = String::from_utf8(self.remote.raw_get(HARNESS_KEY).unwrap()).unwrap();
        self.cmd(Op::ImportKey, json!({"key": key})).unwrap()
    }

    /// 加入 `other` 所在的设备组，用组的设置覆盖本机。
    fn join_group_of(&mut self, other: &Node) -> Value {
        let group = other.status().group;
        assert!(!group.is_empty());
        self.cmd(Op::JoinGroup, json!({"group": group, "mode": "theirs"}))
            .unwrap()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.tx.send(Input::Core(ToSync::Shutdown));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 首台设备把同步密钥记在这里（不在同步库目录内），供测试里后来的节点加入。
const HARNESS_KEY: &str = "test-harness/key";

const DICT_HEAD: &str = "---\nname: user\ncolumns: [code, text, weight]\n...\n";

#[test]
fn two_devices_converge_with_core_requests() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, false);
    let mut b = Node::new(&remote, false);

    // 未设置前：配置了后端，但没有凭据与密钥
    assert_eq!(a.status().phase, "needs_setup");
    assert!(!a.status().has_secret);
    assert!(a.cmd(Op::SyncNow, Value::Null).is_err());
    assert!(a.cmd(Op::ExportKey, Value::Null).is_err());

    let r = a.cmd(Op::SetSecret, json!({"password": "pw"}));
    assert!(r.is_ok());
    let t = a.cmd(Op::TestConnection, Value::Null).unwrap();
    assert_eq!(t["remote_initialized"], false);
    let created = a.cmd(Op::CreateKey, Value::Null).unwrap();
    let key = created["key"].as_str().unwrap().to_string();
    assert_eq!(key.len(), 64, "52 字符 + 12 个分隔符");
    assert_eq!(a.status().phase, "idle");
    assert!(a.status().has_secret);

    a.put("config.toml", "a = 1\n");
    a.put(UD, &format!("{DICT_HEAD}nihao\t你好\t100\n"));
    a.put("themes/t/theme.toml", "x = 1\n");
    let s = a.sync();
    assert_eq!(s.phase, "idle", "{s:?}");
    assert!(s.last_ok.is_some());

    // B：同步密钥不对 → 报「同步密钥不对」；正确密钥 → 加入
    b.cmd(Op::SetSecret, json!({"password": "pw"})).unwrap();
    let t = b.cmd(Op::TestConnection, Value::Null).unwrap();
    assert_eq!(
        (t["remote_initialized"].as_bool(), t["joined"].as_bool()),
        (Some(true), Some(false))
    );
    let e = b.cmd(Op::CreateKey, Value::Null).unwrap_err();
    assert_eq!(e, "远端已有同步库，请粘贴同步密钥加入");
    let e = b.cmd(Op::ImportKey, json!({"key": "AAAA"})).unwrap_err();
    assert_eq!(e, "同步密钥不对");
    assert_eq!(b.status().phase, "needs_setup");
    b.cmd(Op::ImportKey, json!({"key": key})).unwrap();
    // 加入后先确认身份，此前不同步
    assert!(b.status().needs_identity);
    let e = b.cmd(Op::SyncNow, Value::Null).unwrap_err();
    assert!(e.contains("确认本机身份"), "{e}");
    b.join_group_of(&a);
    assert!(!b.status().needs_identity);

    let s = b.sync();
    assert_eq!(s.phase, "idle", "{s:?}");
    assert_eq!(b.read("config.toml").as_deref(), Some("a = 1\n"));
    assert_eq!(b.read(UD), a.read(UD));
    assert_eq!(b.read("themes/t/theme.toml").as_deref(), Some("x = 1\n"));
    assert_eq!(
        b.core.methods(),
        // 本机没有这个词库文件：不开编辑会话，直接写，靠 dict.reload 纳入
        vec!["config.reload", "theme.reloadFiles", "dict.reload"]
    );
    assert_eq!(
        b.core
            .calls
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.0 == "theme.reloadFiles")
            .unwrap()
            .1,
        json!({"ids": ["t"]})
    );

    // 反向：B 改配置与词库，A 收到并发出对应请求
    a.core.clear_calls();
    b.put("config.toml", "a = 2\n");
    b.put(
        UD,
        &format!("{DICT_HEAD}nihao\t你好\t100\nshijie\t世界\t50\n"),
    );
    assert_eq!(b.sync().phase, "idle");
    assert_eq!(a.sync().phase, "idle");
    assert_eq!(a.read("config.toml").as_deref(), Some("a = 2\n"));
    assert!(a.read(UD).unwrap().contains("世界"));
    assert_eq!(
        a.core.methods(),
        vec![
            "data.beginEdit pinyin/user.dict.yaml",
            "data.endEdit pinyin/user.dict.yaml",
            "config.reload",
        ]
    );

    // 查看到的同步密钥与生成时一致，让第三台设备加入
    assert_eq!(
        a.cmd(Op::ExportKey, Value::Null).unwrap()["key"],
        json!(key)
    );
    let mut c = Node::new(&remote, false);
    c.cmd(Op::SetSecret, json!({"password": "pw"})).unwrap();
    c.cmd(Op::ImportKey, json!({"key": key})).unwrap();
    assert!(c.status().needs_identity);
    c.join_group_of(&a);
    assert_eq!(c.sync().phase, "idle");
    assert_eq!(c.read("config.toml").as_deref(), Some("a = 2\n"));
}

#[test]
fn begin_edit_refusal_blocks_the_batch_then_recovers() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, false);
    let mut b = Node::new(&remote, false);
    a.setup();
    a.put(SCHEMA, "a = 1\n");
    a.put(UD, &format!("{DICT_HEAD}x\tX\t1\n"));
    a.sync();
    b.setup();
    // 本机已有这个词库（内容不同，会走合并写入）：写入前要开编辑会话
    let local_ud = format!("{DICT_HEAD}y\tY\t1\n");
    b.put(UD, &local_ud);

    b.core.fail_on("data.beginEdit pinyin/user.dict.yaml");
    let s = b.sync();
    assert_eq!(s.phase, "error", "{s:?}");
    assert!(s.last_error.unwrap().contains("拒绝开始编辑"));
    assert!(b.read(SCHEMA).is_none(), "本批一个文件都不写");
    assert_eq!(b.read(UD).as_deref(), Some(local_ud.as_str()));

    b.core.fail.lock().unwrap().clear();
    let s = b.sync();
    assert_eq!(s.phase, "idle", "{s:?}");
    assert_eq!(b.read(SCHEMA).as_deref(), Some("a = 1\n"));
    assert!(s.last_error.is_none());
}

#[test]
fn joined_flag_follows_create_and_import_key() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, false);
    assert!(!a.status().joined);
    a.setup();
    a.sync();
    assert!(a.status().joined);
    let mut b = Node::new(&remote, false);
    b.cmd(Op::SetSecret, json!({"password": "pw"})).unwrap();
    assert!(!b.status().joined);
    b.join_existing();
    assert!(b.status().joined);
}

#[test]
fn conflict_paths_are_reported_capped_and_vanish_when_deleted() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, false);
    a.setup();
    a.put("config.toml", "a = 1\n");
    a.put("themes/t/x.conflict-d-1.toml", "x");
    let s = a.sync();
    assert_eq!(s.conflicts, 1, "{s:?}");
    assert_eq!(s.conflict_paths, vec!["themes/t/x.conflict-d-1.toml"]);

    std::fs::remove_file(a.user().join("themes/t/x.conflict-d-1.toml")).unwrap();
    let s = a.sync();
    assert_eq!(s.conflicts, 0, "{s:?}");
    assert!(s.conflict_paths.is_empty());

    for i in 0..60 {
        a.put(&format!("config.conflict-d{i:02}-1.toml"), "x");
    }
    let s = a.sync();
    assert_eq!(s.conflicts, 60, "总数照实报");
    assert_eq!(s.conflict_paths.len(), wind_sync_ipc::MAX_CONFLICT_PATHS);
}

#[test]
fn auth_failure_halts_until_user_acts() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, false);
    a.setup();
    a.put("config.toml", "a = 1\n");
    assert_eq!(a.sync().phase, "idle");

    remote.fail_all(Some(BackendError::Auth("HTTP 401".into())));
    a.put("config.toml", "a = 2\n");
    let s = a.sync();
    assert_eq!(s.phase, "halted", "{s:?}");
    assert!(
        s.last_error.as_deref().unwrap().starts_with("认证失败"),
        "{s:?}"
    );

    // 凭据改好后解除停止态
    remote.fail_all(None);
    a.cmd(Op::SetSecret, json!({"password": "new"})).unwrap();
    assert_eq!(a.sync().phase, "idle");

    // 网络类错误：error 而非 halted，文案区分
    remote.fail_all(Some(BackendError::Transient("超时".into())));
    let s = a.sync();
    assert_eq!(s.phase, "error", "{s:?}");
    assert!(s.last_error.unwrap().starts_with("网络不可达"));
}

#[test]
fn reset_remote_issues_a_new_key_and_old_one_stops_working() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, false);
    a.setup();
    a.put("config.toml", "a = 1\n");
    a.sync();
    let old = a.cmd(Op::ExportKey, Value::Null).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let mut b = Node::new(&remote, false);
    b.setup();
    b.join_group_of(&a);
    assert_eq!(b.sync().phase, "idle");
    assert_eq!(b.read("config.toml").as_deref(), Some("a = 1\n"));

    // 重置远端：缺确认标志拒绝；确认后清空，以本机为准重新上传
    assert!(a.cmd(Op::ResetRemote, json!({})).is_err());
    assert!(a.cmd(Op::ResetRemote, json!({"confirm": false})).is_err());
    a.put("config.toml", "a = 9\n");
    let r = a.cmd(Op::ResetRemote, json!({"confirm": true})).unwrap();
    let fresh = r["key"].as_str().unwrap().to_string();
    assert_ne!(fresh, old);
    // 重置后立即写回本机设备信息，清单要等下一轮
    assert_eq!(remote.keys("windinput-sync/v2/devices/").len(), 1);
    assert_eq!(a.sync().phase, "idle");
    assert_eq!(remote.keys("windinput-sync/v2/devices/").len(), 2);
    // 旧密钥不能再加入，新密钥可以
    let mut c = Node::new(&remote, false);
    c.cmd(Op::SetSecret, json!({"password": "pw"})).unwrap();
    assert_eq!(
        c.cmd(Op::ImportKey, json!({"key": old})).unwrap_err(),
        "同步密钥不对"
    );
    c.cmd(Op::ImportKey, json!({"key": fresh})).unwrap();
    // 两设备：A 生成 → B 粘贴 → 确认身份 → 互通
    c.join_group_of(&a);
    assert_eq!(c.sync().phase, "idle");
    assert_eq!(c.read("config.toml").as_deref(), Some("a = 9\n"));
}

#[test]
fn argument_errors_are_readable() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, false);
    for (op, args) in [
        (Op::SetSecret, json!({})),
        (Op::ImportKey, json!({})),
        (Op::TestConnection, Value::Null), // 还没存凭据
    ] {
        let e = a.cmd(op, args).unwrap_err();
        assert!(!e.is_ascii(), "应是中文文案: {e}");
    }
}

#[test]
fn not_configured_phases() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, SyncSettings::default());
    assert_eq!(a.status().phase, "not_configured");
    assert!(a.cmd(Op::SyncNow, Value::Null).is_err());
    // 设置变更后进入 needs_setup
    a.tx.send(Input::Core(ToSync::Settings(settings())))
        .unwrap();
    a.wait_status("needs_setup", |s| s.phase == "needs_setup");
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < WAIT, "等待「{what}」超时");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn write(root: &Path, rel: &str, c: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

#[test]
fn file_changes_trigger_sync_after_debounce_and_echo_is_suppressed() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, true);
    let mut b = Node::new(&remote, true);
    a.setup();
    a.put("config.toml", "a = 1\n");
    a.sync();
    b.setup();
    b.join_group_of(&a);
    b.sync();
    assert_eq!(b.read("config.toml").as_deref(), Some("a = 1\n"));

    // A 的本地改动在去抖后自动上传，不需要任何命令
    let puts = remote.put_count();
    let a_runs = a.syncing_count();
    write(&a.user(), "config.toml", "a = 2\n");
    wait_until("A 自动同步", || a.syncing_count() > a_runs);
    wait_until("A 上传完成", || {
        remote.put_count() > puts && a.status().phase == "idle"
    });

    // 范围外的文件不触发
    let a_runs = a.syncing_count();
    write(&a.user(), "meta.toml", "x");
    write(&a.user(), "data/pinyin/temp.dict.yaml", "x");
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(a.syncing_count(), a_runs, "范围外文件不应触发同步");

    // B 经手动同步收到后，它自己写下的文件不会被当作本地改动再触发一轮
    assert_eq!(b.sync().phase, "idle");
    assert_eq!(b.read("config.toml").as_deref(), Some("a = 2\n"));
    let b_runs = b.syncing_count();
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(b.syncing_count(), b_runs, "回声应被抑制");
}

// ── 审查修复 ────────────────────────────────────────────

fn send_settings(n: &Node, s: SyncSettings) {
    n.tx.send(Input::Core(ToSync::Settings(s))).unwrap();
}

#[test]
fn changing_backend_location_drops_saved_password_but_other_edits_keep_it() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, false);
    a.setup();
    assert!(a.status().has_secret);

    let mut s = settings();
    s.interval_min = 30;
    send_settings(&a, s.clone());
    std::thread::sleep(Duration::from_millis(200));
    assert!(a.status().has_secret, "只改间隔不应清凭据");

    for url in ["http://other.invalid/dav", "http://example.invalid/dav2"] {
        a.cmd(Op::SetSecret, json!({"password": "pw"})).unwrap();
        assert!(a.status().has_secret);
        let mut moved = s.clone();
        moved.backend = Some(Backend::Webdav {
            url: url.into(),
            user: "u".into(),
        });
        send_settings(&a, moved.clone());
        a.wait_status("凭据被清", |st| !st.has_secret);
        s = moved;
    }
    // 换账号同理
    a.cmd(Op::SetSecret, json!({"password": "pw"})).unwrap();
    let mut other_user = s.clone();
    other_user.backend = Some(Backend::Webdav {
        url: "http://example.invalid/dav2".into(),
        user: "someone-else".into(),
    });
    send_settings(&a, other_user);
    a.wait_status("凭据被清", |st| !st.has_secret);
    assert!(a.status().joined, "密钥不受影响");
}

/// 让 b 在 beginEdit 处停住，造出「同步进行中」。返回 (进入标志, 放行标志)。
fn block_begin_edit(core: &FakeCore) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (e, r) = (entered.clone(), release.clone());
    *core.on_call.lock().unwrap() = Some(Box::new(move |m, _| {
        if m == "data.beginEdit" {
            e.store(true, Ordering::SeqCst);
            while !r.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }));
    (entered, release)
}

/// a 已把 UD 同步上去，b 加入且本地有内容不同的 UD（合并写入要开编辑会话）。
fn pair_with_local_dict(remote: &MemBackend) -> (Node, Node) {
    let mut a = Node::new(remote, false);
    let mut b = Node::new(remote, false);
    a.setup();
    a.put(UD, &format!("{DICT_HEAD}x\tX\t1\n"));
    a.sync();
    b.setup();
    b.put(UD, &format!("{DICT_HEAD}y\tY\t1\n"));
    (a, b)
}

fn spin_until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < WAIT, "等待超时：{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn state_changing_commands_are_refused_during_a_round_but_reads_work() {
    let remote = MemBackend::new();
    let (_a, mut b) = pair_with_local_dict(&remote);
    let (entered, release) = block_begin_edit(&b.core);
    b.tx.send(Input::Core(ToSync::Command {
        id: 900,
        op: Op::SyncNow,
        args: Value::Null,
    }))
    .unwrap();
    spin_until("进入 beginEdit", || entered.load(Ordering::SeqCst));

    for (op, args) in [
        (Op::CreateKey, Value::Null),
        (Op::SetSecret, json!({"password": "x"})),
        (Op::ImportKey, json!({"key": "k"})),
        (Op::ResetRemote, json!({"confirm": true})),
        (Op::ConfirmMassDelete, Value::Null),
    ] {
        let e = b.cmd(op, args).unwrap_err();
        assert!(e.contains("正在同步"), "{op:?}: {e}");
    }
    assert!(b.cmd(Op::ExportKey, Value::Null).unwrap()["key"].is_string());
    assert!(b.cmd(Op::SyncNow, Value::Null).is_ok());
    assert_eq!(b.status().phase, "syncing");

    release.store(true, Ordering::SeqCst);
    // 本轮结束后排队的 sync_now 再跑一轮；最终回到 idle
    spin_until("同步结束", || {
        let st = b.status();
        st.phase == "idle" && st.last_ok.is_some()
    });
    // 被拒的命令没有生效：同步密钥仍能导出
    assert!(b.cmd(Op::ExportKey, Value::Null).is_ok());
}

#[test]
fn shutdown_mid_round_ends_the_edit_session_and_writes_nothing() {
    let remote = MemBackend::new();
    let (_a, mut b) = pair_with_local_dict(&remote);
    let before = b.read(UD).unwrap();
    let (entered, release) = block_begin_edit(&b.core);
    b.tx.send(Input::Core(ToSync::Command {
        id: 901,
        op: Op::SyncNow,
        args: Value::Null,
    }))
    .unwrap();
    spin_until("进入 beginEdit", || entered.load(Ordering::SeqCst));
    b.tx.send(Input::Core(ToSync::Shutdown)).unwrap();
    release.store(true, Ordering::SeqCst);
    b.handle.take().unwrap().join().unwrap();

    let m = b.core.methods();
    assert!(
        m.contains(&"data.endEdit pinyin/user.dict.yaml".to_string()),
        "{m:?}"
    );
    assert_eq!(b.read(UD).unwrap(), before, "取消后不写文件");
}

#[test]
fn user_opened_session_gives_a_hint_then_syncs_after_it_closes() {
    let remote = MemBackend::new();
    let (_a, mut b) = pair_with_local_dict(&remote);
    let mine = b.read(UD).unwrap();
    b.core
        .user_sessions
        .lock()
        .unwrap()
        .push("pinyin/user.dict.yaml".into());
    let s = b.sync();
    assert_eq!(s.phase, "error", "{s:?}");
    assert!(s.last_error.unwrap().contains("正在被编辑"));
    assert!(
        !b.core
            .methods()
            .iter()
            .any(|m| m.starts_with("data.beginEdit")),
        "不碰用户的会话"
    );
    assert_eq!(b.read(UD).unwrap(), mine);

    b.core.user_sessions.lock().unwrap().clear();
    spin_until("会话关闭后同步成功", || {
        let st = b.status();
        st.phase == "idle" && st.last_ok.is_some()
    });
    assert!(b.read(UD).unwrap().contains('X'), "远端内容已合并进来");
}

#[test]
fn mass_deletion_halts_with_readable_message_until_confirmed() {
    let remote = MemBackend::new();
    let mut a = Node::new(&remote, false);
    a.setup();
    for i in 0..25 {
        a.put(&format!("schemas/s{i}.schema.toml"), "x = 1\n");
    }
    assert_eq!(a.sync().phase, "idle");
    for i in 0..25 {
        std::fs::remove_file(a.user().join(format!("schemas/s{i}.schema.toml"))).unwrap();
    }
    let s = a.sync();
    assert_eq!(s.phase, "halted", "{s:?}");
    let msg = s.last_error.unwrap();
    assert!(
        msg.contains("25/25") && msg.contains("看似被删除") && msg.contains("为防误删已暂停"),
        "{msg}"
    );

    // 没有等待确认时调用会被拒
    let mut c = Node::new(&remote, false);
    assert!(c.cmd(Op::ConfirmMassDelete, Value::Null).is_err());

    let n = a.statuses().len();
    a.cmd(Op::ConfirmMassDelete, Value::Null).unwrap();
    spin_until("确认后跑完一轮", || {
        let st = a.statuses();
        st[n..].iter().any(|s| s.phase == "syncing") && st.last().is_some_and(|s| s.phase == "idle")
    });
    assert_eq!(a.status().last_error, None);
}

fn named(name: &str) -> SyncSettings {
    SyncSettings {
        device_name: name.into(),
        ..settings()
    }
}

fn devices(n: &mut Node) -> Vec<Value> {
    n.cmd(Op::ListDevices, Value::Null).unwrap()["devices"]
        .as_array()
        .unwrap()
        .clone()
}

fn find<'a>(l: &'a [Value], name: &str) -> &'a Value {
    l.iter()
        .find(|d| d["name"] == name)
        .unwrap_or_else(|| panic!("名册里没有 {name}: {l:?}"))
}

#[test]
fn device_roster_lists_both_devices_with_self_marker() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("办公室"));
    let mut b = Node::with(&remote, false, settings());
    a.setup();
    b.setup();

    let la = devices(&mut a);
    assert_eq!(la.len(), 2, "{la:?}");
    let me = find(&la, "办公室");
    assert_eq!(me["is_self"], true);
    assert_eq!(me["readable"], true);
    assert_eq!(me["caps"], json!(["files"]));
    assert!(["windows", "macos", "linux", "android"].contains(&me["platform"].as_str().unwrap()));
    // 产品版本（docs/VERSION），不是同步组件 crate 的版本
    let product = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/VERSION"),
    )
    .unwrap();
    assert_eq!(me["app_version"], product.trim());
    assert!(me["last_seen"].as_u64().unwrap() > 1_700_000_000);
    assert!(me["id"].as_str().unwrap().len() > 3);

    // B 没填名字：用主机名（非空），在 A 看来不是本机
    let lb = devices(&mut b);
    let b_in_b = lb.iter().find(|d| d["is_self"] == true).unwrap();
    assert!(!b_in_b["name"].as_str().unwrap().is_empty());
    let b_in_a = la.iter().find(|d| d["id"] == b_in_b["id"]).unwrap();
    assert_eq!(b_in_a["is_self"], false);
    assert_eq!(b_in_a["name"], b_in_b["name"]);
}

#[test]
fn renaming_is_published_immediately() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("旧名"));
    let mut b = Node::with(&remote, false, named("乙"));
    a.setup();
    b.setup();
    assert!(find(&devices(&mut b), "旧名")["is_self"] == false);

    a.tx.send(Input::Core(ToSync::Settings(named("新名"))))
        .unwrap();
    // A 的命令与设置变更按序经同一个主循环：A 回了执行结果，新名就已写出
    devices(&mut a);
    let l = devices(&mut b);
    assert_eq!(find(&l, "新名")["is_self"], false);
    assert!(l.iter().all(|d| d["name"] != "旧名"));
}

#[test]
fn remove_device_requires_confirm_and_refuses_self() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("甲"));
    let mut b = Node::with(&remote, false, named("乙"));
    a.setup();
    b.setup();
    let la = devices(&mut a);
    let self_id = find(&la, "甲")["id"].as_str().unwrap().to_string();
    let other_id = find(&la, "乙")["id"].as_str().unwrap().to_string();

    let e = a
        .cmd(Op::RemoveDevice, json!({"dev": other_id}))
        .unwrap_err();
    assert!(e.contains("confirm"), "{e}");
    let e = a
        .cmd(Op::RemoveDevice, json!({"dev": other_id, "confirm": false}))
        .unwrap_err();
    assert!(e.contains("confirm"), "{e}");
    let e = a
        .cmd(Op::RemoveDevice, json!({"dev": self_id, "confirm": true}))
        .unwrap_err();
    assert!(e.contains("本机"), "{e}");
    assert!(a.cmd(Op::RemoveDevice, json!({"confirm": true})).is_err());
    assert!(
        a.cmd(Op::RemoveDevice, json!({"dev": "../x", "confirm": true}))
            .is_err()
    );
    assert_eq!(devices(&mut a).len(), 2, "被拒的移除不得改动名册");
}

#[test]
fn removed_device_comes_back_readable_after_its_next_round() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("甲"));
    let mut b = Node::with(&remote, false, named("乙"));
    a.setup();
    b.setup();
    a.put("config.toml", "a = 1\n");
    a.sync();
    b.sync();
    let la = devices(&mut a);
    let b_id = find(&la, "乙")["id"].as_str().unwrap().to_string();

    let r = a
        .cmd(Op::RemoveDevice, json!({"dev": b_id, "confirm": true}))
        .unwrap();
    assert_eq!(r["ok"], true);
    let l = devices(&mut a);
    assert_eq!(l.len(), 1, "{l:?}");

    // 被移除的设备再同步一轮（无本地改动）：发现自己的 info 没了就补写，别人看到的是可读的原名
    b.sync();
    let l = devices(&mut a);
    let back = l
        .iter()
        .find(|d| d["id"] == b_id.as_str())
        .expect("重新出现");
    assert_eq!(back["readable"], true, "{back:?}");
    assert_eq!(back["name"], "乙");
}

#[test]
fn list_devices_needs_a_joined_device() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("甲"));
    assert!(a.cmd(Op::ListDevices, Value::Null).is_err());
}

/// 桌面与手机共用同一远端：词库互通，config.toml 各自为政。
#[test]
fn mobile_and_desktop_share_dicts_but_not_config() {
    let remote = MemBackend::new();
    let mut pc = Node::new(&remote, false);
    let mut ph = Node::spawn(
        &remote,
        false,
        settings(),
        DeviceClass::Mobile,
        true,
        timing(),
    );
    pc.put("config.toml", "who = \"pc\"\n");
    pc.put(
        UD,
        "---\nname: user\ncolumns: [code, text, weight]\n...\na\t啊\t1\n",
    );
    ph.put("config.toml", "who = \"phone\"\n");
    pc.setup();
    pc.sync();
    ph.setup();
    ph.sync();
    pc.sync();
    assert!(ph.read(UD).unwrap().contains("a\t啊"));
    assert_eq!(ph.read("config.toml").unwrap(), "who = \"phone\"\n");
    assert_eq!(pc.read("config.toml").unwrap(), "who = \"pc\"\n");
    assert!(
        ph.core.methods().iter().any(|m| m == "dict.reload"),
        "{:?}",
        ph.core.methods()
    );
    assert!(!ph.core.methods().iter().any(|m| m == "config.reload"));
}

/// 按需模式：设置、机密变更与定时都不自行起一轮，只有 sync_now 才跑。
#[test]
fn on_demand_runner_only_syncs_when_asked() {
    let remote = MemBackend::new();
    let t = Timing {
        first_delay: Duration::from_millis(50),
        settings_delay: Duration::from_millis(50),
        ..timing()
    };
    let mut n = Node::spawn(&remote, false, settings(), DeviceClass::Mobile, true, t);
    n.setup();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(n.syncing_count(), 0, "未要求时不应同步");
    n.sync();
    assert_eq!(n.syncing_count(), 1);
}

// ── 设备组 ──────────────────────────────────────────────

/// 组内互通设置，退出后各自为政；组名经名册传给组员。
#[test]
fn group_join_leave_and_group_name() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("甲"));
    let mut b = Node::with(&remote, false, named("乙"));
    a.setup();
    a.put("config.toml", "a = 1\n");
    a.sync();
    a.cmd(Op::SetGroupName, json!({"name": "家"})).unwrap();
    assert_eq!(a.status().group_name, "家");

    b.put("config.toml", "b = 1\n");
    b.cmd(Op::SetSecret, json!({"password": "pw"})).unwrap();
    b.join_existing();
    let e = b
        .cmd(Op::JoinGroup, json!({"group": "x", "mode": "both"}))
        .unwrap_err();
    assert!(e.contains("mode"), "{e}");
    b.join_group_of(&a);
    assert_eq!(b.read("config.toml").as_deref(), Some("a = 1\n"));
    let st = b.status();
    assert_eq!(st.group, a.status().group);
    assert_eq!(st.group_name, "家");
    assert!(!st.needs_identity);
    let backups: Vec<_> = std::fs::read_dir(b.user().join("data/.backup/sync"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1, "覆盖前备份本机设置");
    assert_eq!(b.sync().phase, "idle");

    let l = devices(&mut a);
    let b_in_a = find(&l, "乙");
    assert_eq!(b_in_a["group"], st.group.as_str());
    assert_eq!(b_in_a["group_name"], "家");

    // 退出后新建单机组：组号不同，改设置互不影响
    let r = b.cmd(Op::LeaveGroup, json!({"name": "乙的组"})).unwrap();
    assert_ne!(r["group"], st.group.as_str());
    assert_eq!(b.status().group_name, "乙的组");
    b.put("config.toml", "b = 2\n");
    b.sync();
    a.sync();
    assert_eq!(a.read("config.toml").as_deref(), Some("a = 1\n"));
}

/// 认领已有设备：沿用其号与组，组的设置覆盖本机（先备份），随后照常同步。
#[test]
fn claim_device_restores_identity_and_settings() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("old-pc"));
    a.setup();
    a.put("config.toml", "a = 1\n");
    a.put(UD, &format!("{DICT_HEAD}x\tX\t1\n"));
    a.sync();
    let a_id = a.status().device;
    let a_group = a.status().group;
    drop(a);

    let mut n = Node::with(&remote, false, named("new-pc"));
    n.put("config.toml", "n = 1\n");
    n.cmd(Op::SetSecret, json!({"password": "pw"})).unwrap();
    n.join_existing();
    let r = n.cmd(Op::ClaimDevice, json!({"dev": a_id})).unwrap();
    assert_eq!(r["group"], a_group.as_str());
    let st = n.status();
    assert_eq!(
        (st.device.as_str(), st.group.as_str()),
        (a_id.as_str(), a_group.as_str())
    );
    assert!(!st.needs_identity);
    assert_eq!(n.read("config.toml").as_deref(), Some("a = 1\n"));
    assert_eq!(n.sync().phase, "idle");
    assert!(n.read(UD).unwrap().contains("X"));
    let backup = std::fs::read_dir(n.user().join("data/.backup/sync"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read_to_string(backup.join("config.toml")).unwrap(),
        "n = 1\n"
    );
}

/// 两台同名设备：后加入的那台确认身份时自动改用带序号的号。
#[test]
fn same_default_id_gets_a_suffix_on_confirm() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("Desk"));
    let mut b = Node::with(&remote, false, named("Desk"));
    a.setup();
    assert_eq!(a.status().device, "desk");
    b.setup();
    assert_eq!(b.status().device, "desk-2");
    assert_eq!(b.sync().phase, "idle");
    assert_eq!(a.sync().phase, "idle");
}

/// 组号与名册全部成员查重：A 退组得 pc-2 后，同名的 B 确认身份不会落进 A 的组。
#[test]
fn confirmed_group_does_not_collide_with_existing_groups() {
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("pc"));
    a.setup();
    let r = a.cmd(Op::LeaveGroup, json!({})).unwrap();
    assert_eq!(r["group"], "pc-2");
    a.put("config.toml", "a = 1\n");
    a.sync();

    let mut b = Node::with(&remote, false, named("pc"));
    b.put("config.toml", "b = 1\n");
    b.setup();
    let st = b.status();
    assert_eq!(st.device, "pc-2");
    assert_ne!(st.group, "pc-2", "{st:?}");
    b.sync();
    a.sync();
    assert_eq!(a.read("config.toml").as_deref(), Some("a = 1\n"));
}

/// 覆盖本机设置时删除失败（写入已成功）：进停止态，不把半成品同步进旧组；重新加入成功后恢复。
#[cfg(unix)]
#[test]
fn half_finished_join_halts_until_retried() {
    use std::os::unix::fs::PermissionsExt;
    let remote = MemBackend::new();
    let mut a = Node::with(&remote, false, named("甲"));
    a.setup();
    a.put("config.toml", "a = 1\n");
    a.sync();
    let mut b = Node::with(&remote, false, named("乙"));
    b.put("data/x/dicts.toml", "d = 1\n");
    b.setup();
    b.sync();
    let old_group = b.status().group;
    let gen_keys = remote.keys("windinput-sync/v2/devices/").len();

    let dir = b.user().join("data/x");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let e = b
        .cmd(
            Op::JoinGroup,
            json!({"group": a.status().group, "mode": "theirs"}),
        )
        .unwrap_err();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(e.contains("加入设备组未完成"), "{e}");
    assert_eq!(
        b.read("config.toml").as_deref(),
        Some("a = 1\n"),
        "写入已发生"
    );
    let st = b.status();
    assert_eq!(st.phase, "halted", "{st:?}");
    assert_eq!(st.group, old_group);
    assert!(
        b.cmd(Op::SyncNow, Value::Null)
            .unwrap_err()
            .contains("未完成")
    );
    // 改设置也不解除
    send_settings(
        &b,
        SyncSettings {
            interval_min: 30,
            ..named("乙")
        },
    );
    b.wait_status("设置已生效", |s| s.phase == "halted");
    assert!(b.cmd(Op::SyncNow, Value::Null).is_err());
    assert_eq!(
        remote.keys("windinput-sync/v2/devices/").len(),
        gen_keys,
        "没有新清单"
    );

    b.join_group_of(&a);
    assert_eq!(b.status().phase, "idle");
    assert!(b.read("data/x/dicts.toml").is_none());
    assert_eq!(b.sync().phase, "idle");
    a.sync();
    assert_eq!(a.read("config.toml").as_deref(), Some("a = 1\n"));
}
