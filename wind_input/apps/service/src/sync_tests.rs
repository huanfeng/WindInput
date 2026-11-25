use super::*;
use std::io::{PipeReader, PipeWriter, pipe};
use wind_sync_ipc::{Backend, RoundStats};

/// 桩同步进程：在测试里扮演管道另一头（不起真进程——测试二进制的 stdout 会混进
/// libtest 的头部输出，污染帧流）。
struct Stub {
    from_core: mpsc::Receiver<ToSync>,
    to_core: PipeWriter,
}

impl Stub {
    fn next(&self) -> ToSync {
        self.from_core
            .recv_timeout(Duration::from_secs(5))
            .expect("等不到 core 发来的帧")
    }
    fn send(&mut self, m: FromSync) {
        write_frame(&mut self.to_core, &m).unwrap();
    }
}

struct Rig {
    host: Arc<SyncHost>,
    stub: Stub,
    events: Arc<Mutex<Vec<Value>>>,
    executed: Arc<Mutex<Vec<String>>>,
    notices: Arc<Mutex<Vec<SyncNotice>>>,
    dir: tempfile::TempDir,
}

fn webdav(enabled: bool) -> SyncSettings {
    SyncSettings {
        enabled,
        backend: Some(Backend::Webdav {
            url: "https://dav.example.com/x".into(),
            user: "me".into(),
        }),
        ..Default::default()
    }
}

fn host_in(dir: &Path, installed: bool, timeout: Duration) -> Arc<SyncHost> {
    let paths = Paths {
        settings_file: dir.join("state/sync/settings.toml"),
        state_dir: dir.join("state/sync"),
        userdata_dir: dir.join("user"),
        suffix: "_t".into(),
    };
    let sup = Supervisor::new(|| Err("测试不拉起真进程".into()));
    SyncHost::new(Some(paths), sup, Box::new(move || installed), timeout)
}

/// 造一个已接上桩的 host；`pre` 在接上前写入设置文件。
fn rig_with(pre: Option<SyncSettings>, timeout: Duration) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    if let Some(s) = pre {
        save_settings(&dir.path().join("state/sync/settings.toml"), &s).unwrap();
    }
    let host = host_in(dir.path(), true, timeout);
    let events = Arc::new(Mutex::new(Vec::new()));
    let executed = Arc::new(Mutex::new(Vec::new()));
    host.set_hooks(
        {
            let events = events.clone();
            Box::new(move |v| lock(&events).push(v))
        },
        {
            let executed = executed.clone();
            Box::new(move |m, p| {
                let ids = p.get("ids").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                });
                let tag = match (p.get("path").and_then(Value::as_str), ids) {
                    (Some(path), _) => format!("{m} {path}"),
                    (None, Some(ids)) => format!("{m} {ids}"),
                    (None, None) => m.to_string(),
                };
                lock(&executed).push(tag);
                Ok(json!({ "ran": m }))
            })
        },
    );
    let notices = Arc::new(Mutex::new(Vec::new()));
    {
        let notices = notices.clone();
        host.set_notify(Box::new(move |n| lock(&notices).push(n)));
    }
    let stub = attach_stub(&host);
    Rig {
        host,
        stub,
        events,
        executed,
        notices,
        dir,
    }
}

/// 把桩接到 host 上，相当于监护器拉起了同步进程。
fn attach_stub(host: &Arc<SyncHost>) -> Stub {
    let (child_stdin_r, child_stdin_w): (PipeReader, PipeWriter) = pipe().unwrap();
    let (child_stdout_r, child_stdout_w) = pipe().unwrap();
    let (tx, from_core) = mpsc::channel();
    std::thread::spawn(move || {
        let mut r = child_stdin_r;
        while let Ok(Some(m)) = read_frame::<_, ToSync>(&mut r) {
            if tx.send(m).is_err() {
                break;
            }
        }
    });
    host.attach(&mut lock(&host.inner), child_stdin_w, child_stdout_r);
    Stub {
        from_core,
        to_core: child_stdout_w,
    }
}

fn rig() -> Rig {
    rig_with(Some(webdav(true)), COMMAND_TIMEOUT)
}

fn wait_for(mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < end, "等待条件超时");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn phase(h: &SyncHost) -> String {
    h.rpc("sync.status", &Value::Null).unwrap()["phase"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn hello_comes_first_with_paths_and_settings() {
    let r = rig();
    match r.stub.next() {
        ToSync::Hello {
            suffix,
            userdata_dir,
            state_dir,
            settings,
            portable: _,
        } => {
            assert_eq!(suffix, "_t");
            assert!(userdata_dir.ends_with("user"));
            assert!(state_dir.ends_with("sync"));
            assert_eq!(settings, webdav(true));
        }
        m => panic!("第一帧应是 Hello: {m:?}"),
    }
}

#[test]
fn status_is_cached_and_broadcast_without_secrets() {
    let mut r = rig();
    let status = Status {
        phase: "idle".into(),
        last_ok: Some(100),
        pending: 2,
        conflicts: 1,
        conflict_paths: vec!["config.conflict-a-1.toml".into()],
        has_secret: true,
        joined: true,
        device: "pc".into(),
        group: "home".into(),
        group_name: "家".into(),
        needs_identity: true,
        ..Default::default()
    };
    r.stub.send(FromSync::Status(status.clone()));
    wait_for(|| phase(&r.host) == "idle");
    let st = r.host.rpc("sync.status", &Value::Null).unwrap();
    assert_eq!(
        (
            &st["device"],
            &st["group"],
            &st["group_name"],
            &st["needs_identity"]
        ),
        (&json!("pc"), &json!("home"), &json!("家"), &json!(true))
    );
    assert_eq!(st["pending"], 2);
    assert_eq!(st["conflicts"], 1);
    assert_eq!(st["joined"], true);
    assert_eq!(st["conflict_paths"][0], "config.conflict-a-1.toml");
    assert_eq!(st["last_ok"], 100);
    assert_eq!(st["running"], true);
    let ev = lock(&r.events).clone();
    assert_eq!(ev.last().unwrap()["phase"], "idle", "状态变化应广播");

    let got = r.host.rpc("sync.getSettings", &Value::Null).unwrap();
    assert_eq!(got["has_secret"], true);
    assert_eq!(got["installed"], true);
    let text = got.to_string();
    assert!(!text.contains("password") && !text.contains("secret_key"));

    // 同样的状态再发一次不重复广播。
    let n = lock(&r.events).len();
    r.stub.send(FromSync::Status(status));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(lock(&r.events).len(), n);
}

#[test]
fn requests_outside_whitelist_are_rejected() {
    let mut r = rig();
    let _hello = r.stub.next();
    r.stub.send(FromSync::Request {
        id: 1,
        method: "system.restart".into(),
        params: json!({}),
    });
    r.stub.send(FromSync::Request {
        id: 2,
        method: "dict.reload".into(),
        params: json!({}),
    });
    match r.stub.next() {
        ToSync::Reply { id: 1, error, .. } => {
            assert!(error.unwrap().contains("白名单"));
        }
        m => panic!("{m:?}"),
    }
    match r.stub.next() {
        ToSync::Reply {
            id: 2,
            result,
            error,
        } => {
            assert!(error.is_none());
            assert_eq!(result.unwrap()["ran"], "dict.reload");
        }
        m => panic!("{m:?}"),
    }
    assert_eq!(*lock(&r.executed), vec!["dict.reload".to_string()]);
}

fn begin(r: &mut Rig, id: u64, method: &str, path: &str) {
    r.stub.send(FromSync::Request {
        id,
        method: method.into(),
        params: json!({ "path": path }),
    });
    assert!(matches!(r.stub.next(), ToSync::Reply { error: None, .. }));
}

fn end_count(r: &Rig, path: &str) -> usize {
    lock(&r.executed)
        .iter()
        .filter(|m| **m == format!("data.endEdit {path}"))
        .count()
}

/// 同步进程被强杀（管道断开）时，经它开的编辑会话由 core 代为结束。
#[test]
fn sessions_left_open_by_a_dead_sync_process_are_ended_by_core() {
    let mut r = rig();
    let _hello = r.stub.next();
    begin(&mut r, 1, "data.beginEdit", "pinyin/user.dict.yaml");
    begin(&mut r, 2, "data.beginEdit", "phrases.dict.yaml");
    begin(&mut r, 3, "data.endEdit", "phrases.dict.yaml");
    let executed = r.executed.clone();
    drop(r.stub);
    wait_for(|| lock(&executed).contains(&"data.endEdit pinyin/user.dict.yaml".to_string()));
    // 已正常结束的不重复结束
    assert_eq!(
        lock(&executed)
            .iter()
            .filter(|m| m.as_str() == "data.endEdit phrases.dict.yaml")
            .count(),
        1
    );
}

#[test]
fn stopping_the_host_ends_sessions_still_open() {
    let mut r = rig();
    let _hello = r.stub.next();
    begin(&mut r, 1, "data.beginEdit", "pinyin/user.dict.yaml");
    r.host.stop();
    assert_eq!(end_count(&r, "pinyin/user.dict.yaml"), 1);
}

#[test]
fn failed_begin_is_not_tracked() {
    let r = rig();
    r.host
        .track_edit(1, "data.beginEdit", &json!({"path": "a"}), false);
    r.host.release_held(None);
    assert_eq!(end_count(&r, "a"), 0);
}

fn ext_request(r: &mut Rig, id: u64, method: &str, ids: &[&str]) {
    r.stub.send(FromSync::Request {
        id,
        method: method.into(),
        params: json!({ "ids": ids }),
    });
    assert!(matches!(r.stub.next(), ToSync::Reply { error: None, .. }));
}

fn count(r: &Rig, tag: &str) -> usize {
    lock(&r.executed).iter().filter(|m| *m == tag).count()
}

#[test]
fn extension_suspend_and_resume_are_whitelisted() {
    assert!(REQUEST_WHITELIST.contains(&"extensions.suspend"));
    assert!(REQUEST_WHITELIST.contains(&"extensions.resume"));
}

/// 同步进程在暂停扩展期间被强杀：core 代为恢复，否则扩展一直停着。
#[test]
fn extensions_suspended_by_a_dead_sync_process_are_resumed_by_core() {
    let mut r = rig();
    let _hello = r.stub.next();
    ext_request(&mut r, 1, "extensions.suspend", &["a", "b"]);
    ext_request(&mut r, 2, "extensions.resume", &["a"]);
    let executed = r.executed.clone();
    drop(r.stub);
    wait_for(|| lock(&executed).contains(&"extensions.resume b".to_string()));
    // 已正常恢复的不重复恢复
    assert!(!lock(&executed).contains(&"extensions.resume a,b".to_string()));
    assert_eq!(
        lock(&executed)
            .iter()
            .filter(|m| m.as_str() == "extensions.resume a")
            .count(),
        1
    );
}

#[test]
fn stopping_the_host_resumes_suspended_extensions() {
    let mut r = rig();
    let _hello = r.stub.next();
    ext_request(&mut r, 1, "extensions.suspend", &["a"]);
    r.host.stop();
    assert_eq!(count(&r, "extensions.resume a"), 1);
}

#[test]
fn edit_sessions_query_is_whitelisted() {
    assert!(REQUEST_WHITELIST.contains(&"data.editSessions"));
}

#[test]
fn every_whitelisted_method_is_executed() {
    let mut r = rig();
    let _hello = r.stub.next();
    for (i, m) in REQUEST_WHITELIST.iter().enumerate() {
        r.stub.send(FromSync::Request {
            id: i as u64,
            method: m.to_string(),
            params: json!({}),
        });
        assert!(matches!(r.stub.next(), ToSync::Reply { error: None, .. }));
    }
    assert_eq!(lock(&r.executed).len(), REQUEST_WHITELIST.len());
}

#[test]
fn command_round_trip() {
    let mut r = rig();
    let _hello = r.stub.next();
    let host = r.host.clone();
    let call = std::thread::spawn(move || {
        host.rpc(
            "sync.command",
            &json!({ "op": "export_key", "args": { "x": 1 } }),
        )
    });
    let id = match r.stub.next() {
        ToSync::Command { id, op, args } => {
            assert_eq!(op, Op::ExportKey);
            assert_eq!(args, json!({ "x": 1 }));
            id
        }
        m => panic!("{m:?}"),
    };
    r.stub
        .send(FromSync::command_reply(id, Ok(json!({"key": "abc"}))));
    assert_eq!(call.join().unwrap().unwrap(), json!({"key": "abc"}));
    assert!(lock(&r.host.pending).is_empty());

    // 同步进程回错误 -> 调用方拿到错误文本。
    let host = r.host.clone();
    let call = std::thread::spawn(move || host.rpc("sync.command", &json!({ "op": "sync_now" })));
    let ToSync::Command { id, .. } = r.stub.next() else {
        panic!()
    };
    r.stub
        .send(FromSync::command_reply(id, Err("同步密钥不对".into())));
    assert!(
        call.join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("同步密钥不对")
    );
}

#[test]
fn command_times_out_and_cleans_up() {
    let mut r = rig_with(Some(webdav(true)), Duration::from_millis(200));
    let _hello = r.stub.next();
    let e = r
        .host
        .rpc("sync.command", &json!({ "op": "sync_now" }))
        .unwrap_err();
    assert!(e.to_string().contains("超时"));
    assert!(lock(&r.host.pending).is_empty());
    let _ = r.stub.next(); // 命令帧
    // 超时后迟到的回执被忽略，不崩。
    r.stub.send(FromSync::command_reply(1, Ok(json!({}))));
}

#[test]
fn command_errors_without_process_or_with_bad_op() {
    let dir = tempfile::tempdir().unwrap();
    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    let e = host
        .rpc("sync.command", &json!({ "op": "sync_now" }))
        .unwrap_err();
    assert!(e.to_string().contains("未运行"));
    assert!(host.rpc("sync.command", &json!({ "op": "nope" })).is_err());
    assert!(host.rpc("sync.command", &json!({})).is_err());
}

#[test]
fn command_right_after_enabling_waits_for_the_link() {
    let dir = tempfile::tempdir().unwrap();
    save_settings(&dir.path().join("state/sync/settings.toml"), &webdav(true)).unwrap();
    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    let h2 = host.clone();
    let call = std::thread::spawn(move || {
        h2.rpc(
            "sync.command",
            &json!({ "op": "set_secret", "args": { "password": "p" } }),
        )
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(!call.is_finished(), "进程应在运行但链路未建立：命令应等待");
    let mut stub = attach_stub(&host);
    assert!(matches!(stub.next(), ToSync::Hello { .. }));
    let ToSync::Command { id, op, .. } = stub.next() else {
        panic!("连上后应补发命令")
    };
    assert_eq!(op, Op::SetSecret);
    stub.send(FromSync::command_reply(id, Ok(json!({ "ok": true }))));
    assert_eq!(call.join().unwrap().unwrap(), json!({ "ok": true }));
}

#[test]
fn command_waiting_for_the_link_times_out_with_the_old_error() {
    let dir = tempfile::tempdir().unwrap();
    save_settings(&dir.path().join("state/sync/settings.toml"), &webdav(true)).unwrap();
    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    let _ = host.link_wait.set(Duration::from_millis(200));
    let t = Instant::now();
    let e = host
        .rpc("sync.command", &json!({ "op": "sync_now" }))
        .unwrap_err();
    assert!(e.to_string().contains("未运行"));
    assert!(t.elapsed() >= Duration::from_millis(200), "应等满再报错");
    assert!(lock(&host.pending).is_empty());
}

#[test]
fn command_when_disabled_fails_without_waiting() {
    let dir = tempfile::tempdir().unwrap();
    save_settings(&dir.path().join("state/sync/settings.toml"), &webdav(false)).unwrap();
    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    let t = Instant::now();
    let e = host
        .rpc("sync.command", &json!({ "op": "sync_now" }))
        .unwrap_err();
    assert!(e.to_string().contains("未运行"));
    assert!(t.elapsed() < Duration::from_secs(1), "未启用应立即报错");
}

#[test]
fn process_exit_fails_waiting_command() {
    let r = rig();
    let Rig { host, stub, .. } = r;
    let h2 = host.clone();
    let call = std::thread::spawn(move || h2.rpc("sync.command", &json!({ "op": "sync_now" })));
    wait_for(|| !lock(&host.pending).is_empty());
    drop(stub.to_core); // 同步进程 stdout 关闭
    let e = call.join().unwrap().unwrap_err();
    assert!(e.to_string().contains("已退出"));
    assert_eq!(phase(&host), "starting", "已启用但进程不在：等待重启");
}

#[test]
fn disabling_pushes_settings_then_shutdown() {
    let r = rig();
    let _hello = r.stub.next();
    r.host
        .rpc("sync.setSettings", &json!({ "settings": webdav(false) }))
        .unwrap();
    assert!(matches!(r.stub.next(), ToSync::Settings(s) if !s.enabled));
    assert!(matches!(r.stub.next(), ToSync::Shutdown));
    assert!(!lock(&r.host.inner).sup.wanted());
    assert_eq!(phase(&r.host), "disabled");
    assert!(lock(&r.host.inner).link.is_none());
}

#[test]
fn settings_are_validated_persisted_and_secret_free() {
    let dir = tempfile::tempdir().unwrap();
    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    let file = dir.path().join("state/sync/settings.toml");

    let bad = json!({ "enabled": true });
    assert!(host.rpc("sync.setSettings", &bad).is_err());
    assert!(!file.exists(), "校验失败不落盘");

    host.rpc("sync.setSettings", &json!({ "settings": webdav(true) }))
        .unwrap();
    assert_eq!(load_settings(&file), webdav(true));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.to_lowercase().contains("password"), "{text}");
    assert!(lock(&host.inner).sup.wanted(), "启用且已安装 -> 期望运行");

    // 重新构造（相当于服务重启）读回同一份设置。
    let again = host_in(dir.path(), true, COMMAND_TIMEOUT);
    assert!(lock(&again.inner).sup.wanted());
    assert_eq!(
        again.rpc("sync.getSettings", &Value::Null).unwrap()["settings"]["enabled"],
        true
    );
}

#[test]
fn s3_settings_round_trip_through_toml() {
    let s = SyncSettings {
        enabled: true,
        backend: Some(Backend::S3 {
            endpoint: "https://s3.example.com".into(),
            region: "auto".into(),
            bucket: "b".into(),
            prefix: "w/".into(),
            access_key_id: "AK".into(),
            path_style: true,
        }),
        interval_min: 30,
        ..Default::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("s.toml");
    save_settings(&f, &s).unwrap();
    assert_eq!(load_settings(&f), s);
}

#[test]
fn corrupt_settings_file_falls_back_to_default_without_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("state/sync/settings.toml");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "enabled = [").unwrap();
    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    assert!(!lock(&host.inner).settings.enabled);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "enabled = [");
}

#[test]
fn not_installed_is_reported_and_never_started() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("state/sync/settings.toml");
    save_settings(&f, &webdav(true)).unwrap();
    let host = host_in(dir.path(), false, COMMAND_TIMEOUT);
    assert!(!lock(&host.inner).sup.wanted(), "未安装不设期望态");
    host.tick(Instant::now());
    assert_eq!(phase(&host), "not_installed");
    let g = host.rpc("sync.getSettings", &Value::Null).unwrap();
    assert_eq!(g["installed"], false);
}

#[test]
fn unknown_sync_method_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    assert!(host.rpc("sync.nope", &Value::Null).is_err());
}

#[test]
fn stale_link_messages_are_ignored() {
    let mut r = rig();
    // 模拟重新拉起：epoch 前进后，旧读线程的 Status 不得写进状态。
    lock(&r.host.inner).epoch += 1;
    lock(&r.host.inner).link.as_mut().unwrap().epoch += 1;
    r.stub.send(FromSync::Status(Status {
        phase: "old".into(),
        ..Default::default()
    }));
    std::thread::sleep(Duration::from_millis(150));
    assert_ne!(phase(&r.host), "old");
    let _ = &r.dir;
}

// ── 立即同步与提示 ─────────────────────────────────────

fn round_status(phase: &str, round: u64, last: Option<RoundStats>) -> Status {
    Status {
        phase: phase.into(),
        round,
        last_round: last,
        ..Default::default()
    }
}

fn ok_round(downloaded: u32, uploaded: u32) -> Option<RoundStats> {
    Some(RoundStats {
        ok: true,
        downloaded,
        uploaded,
        ..Default::default()
    })
}

fn fail_round(why: &str) -> Option<RoundStats> {
    Some(RoundStats {
        ok: false,
        error: Some(why.into()),
        ..Default::default()
    })
}

fn notices(r: &Rig) -> Vec<SyncNotice> {
    lock(&r.notices).clone()
}

/// 主动同步一次：立即返回；桩收下 `SyncNow` 命令并按 `reply` 回执，等工作线程收尾。
fn run_active(r: &mut Rig, reply: Result<Value, String>) -> Value {
    let v = r.host.sync_now_active();
    loop {
        if let ToSync::Command { id, op, .. } = r.stub.next() {
            assert_eq!(op, Op::SyncNow);
            r.stub.send(FromSync::command_reply(id, reply));
            break;
        }
    }
    wait_for(|| !r.host.now_in_flight.load(Ordering::SeqCst));
    // 提示在清标志之后才发，等一拍。
    std::thread::sleep(Duration::from_millis(60));
    v
}

fn send_status(r: &mut Rig, s: Status) {
    r.stub.send(FromSync::Status(s.clone()));
    wait_for(|| lock(&r.host.inner).status == s);
    // 通知在状态写入之后才发，再等一拍让 pump 跑完。
    std::thread::sleep(Duration::from_millis(30));
}

#[test]
fn sync_now_when_disabled_does_not_launch() {
    let dir = tempfile::tempdir().unwrap();
    let launched = Arc::new(AtomicU64::new(0));
    let sup = {
        let launched = launched.clone();
        Supervisor::new(move || {
            launched.fetch_add(1, Ordering::SeqCst);
            Err("测试不拉起真进程".into())
        })
    };
    let paths = Paths {
        settings_file: dir.path().join("state/sync/settings.toml"),
        state_dir: dir.path().join("state/sync"),
        userdata_dir: dir.path().join("user"),
        suffix: "_t".into(),
    };
    let host = SyncHost::new(Some(paths), sup, Box::new(|| true), COMMAND_TIMEOUT);
    let got = Arc::new(Mutex::new(Vec::new()));
    {
        let got = got.clone();
        host.set_notify(Box::new(move |n| lock(&got).push(n)));
    }
    let v = host.sync_now_active();
    host.tick(Instant::now());
    assert_eq!(v["outcome"], "disabled");
    assert_eq!(*lock(&got), vec![SyncNotice::Disabled]);
    assert_eq!(launched.load(Ordering::SeqCst), 0);
}

#[test]
fn active_sync_notifies_done_once() {
    let mut r = rig();
    let _hello = r.stub.next();
    let v = run_active(&mut r, Ok(json!({ "queued": true, "ticket": 3 })));
    assert_eq!(v, json!({ "outcome": "requested" }));
    send_status(&mut r, round_status("idle", 2, ok_round(1, 1)));
    assert!(notices(&r).is_empty(), "round 2 早于票号");
    send_status(&mut r, round_status("idle", 3, ok_round(2, 0)));
    send_status(&mut r, round_status("idle", 4, ok_round(5, 5)));
    assert_eq!(notices(&r), vec![SyncNotice::Done(ok_round(2, 0).unwrap())]);
}

#[test]
fn busy_then_done() {
    let mut r = rig();
    let _hello = r.stub.next();
    let v = run_active(
        &mut r,
        Ok(json!({ "queued": false, "busy": true, "ticket": 5 })),
    );
    assert_eq!(v, json!({ "outcome": "requested" }));
    assert_eq!(notices(&r), vec![SyncNotice::Busy]);
    send_status(&mut r, round_status("idle", 5, ok_round(0, 0)));
    assert_eq!(
        notices(&r),
        vec![SyncNotice::Busy, SyncNotice::Done(ok_round(0, 0).unwrap())]
    );
}

#[test]
fn command_error_is_reported_as_failed() {
    let mut r = rig();
    let _hello = r.stub.next();
    let v = run_active(&mut r, Err("同步密钥不对".into()));
    assert_eq!(v["outcome"], "requested");
    assert_eq!(notices(&r), vec![SyncNotice::Failed("同步密钥不对".into())]);
}

#[test]
fn background_failure_notifies_once_until_recovered() {
    let mut r = rig();
    send_status(&mut r, round_status("error", 1, fail_round("网络不通")));
    send_status(&mut r, round_status("error", 2, fail_round("网络不通")));
    assert_eq!(
        notices(&r),
        vec![SyncNotice::BackgroundFailed("网络不通".into())]
    );
    send_status(&mut r, round_status("idle", 3, ok_round(0, 0)));
    send_status(&mut r, round_status("error", 4, fail_round("又断了")));
    assert_eq!(
        notices(&r),
        vec![
            SyncNotice::BackgroundFailed("网络不通".into()),
            SyncNotice::BackgroundFailed("又断了".into()),
        ]
    );
}

#[test]
fn active_failure_is_not_double_reported() {
    let mut r = rig();
    let _hello = r.stub.next();
    run_active(&mut r, Ok(json!({ "queued": true, "ticket": 1 })));
    let mut s = round_status("error", 1, fail_round("服务器 500"));
    s.last_error = Some("服务器 500".into());
    send_status(&mut r, s);
    send_status(&mut r, round_status("error", 2, fail_round("服务器 500")));
    assert_eq!(notices(&r), vec![SyncNotice::Failed("服务器 500".into())]);
}

#[test]
fn ticket_abandoned_when_sync_disabled() {
    let mut r = rig();
    let _hello = r.stub.next();
    run_active(&mut r, Ok(json!({ "queued": true, "ticket": 9 })));
    r.host
        .rpc("sync.setSettings", &json!({ "settings": webdav(false) }))
        .unwrap();
    r.host.tick(Instant::now());
    r.host.tick(Instant::now());
    assert_eq!(notices(&r), vec![SyncNotice::Failed("已停用".into())]);
    assert!(lock(&r.host.inner).track.ticket.is_none());
}

#[test]
fn ticket_abandoned_when_halted() {
    let mut r = rig();
    let _hello = r.stub.next();
    run_active(&mut r, Ok(json!({ "queued": true, "ticket": 9 })));
    let mut s = round_status("halted", 1, None);
    s.last_error = Some("设备号被占用".into());
    send_status(&mut r, s);
    assert_eq!(notices(&r), vec![SyncNotice::Failed("设备号被占用".into())]);
}

#[test]
fn ticket_times_out() {
    let mut r = rig();
    let _hello = r.stub.next();
    let base = Instant::now();
    let offset = Arc::new(Mutex::new(Duration::ZERO));
    {
        let offset = offset.clone();
        r.host.set_clock(Box::new(move || base + *lock(&offset)));
    }
    run_active(&mut r, Ok(json!({ "queued": true, "ticket": 9 })));
    *lock(&offset) = Duration::from_secs(9 * 60);
    r.host.pump();
    assert!(notices(&r).is_empty(), "{:?}", notices(&r));
    *lock(&offset) = Duration::from_secs(11 * 60);
    r.host.pump();
    r.host.pump();
    assert_eq!(
        notices(&r),
        vec![SyncNotice::Failed("等待超过 10 分钟".into())]
    );
}

#[test]
fn enabled_hook_fires_initially_and_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    let got = Arc::new(Mutex::new(Vec::new()));
    {
        let got = got.clone();
        host.set_enabled_hook(Box::new(move |e| lock(&got).push(e)));
    }
    host.rpc("sync.setSettings", &json!({ "settings": webdav(true) }))
        .unwrap();
    host.rpc("sync.setSettings", &json!({ "settings": webdav(true) }))
        .unwrap();
    host.rpc("sync.setSettings", &json!({ "settings": webdav(false) }))
        .unwrap();
    assert_eq!(*lock(&got), vec![false, true, false]);
}

#[test]
fn notice_text_formats() {
    let done = |d, u, c| {
        notice_text(&SyncNotice::Done(RoundStats {
            ok: true,
            downloaded: d,
            uploaded: u,
            conflicts: c,
            ..Default::default()
        }))
    };
    assert_eq!(done(0, 0, 0), "同步完成");
    assert_eq!(done(2, 3, 0), "同步完成：收到 2 个文件、上传 3 个");
    assert_eq!(done(0, 3, 0), "同步完成：上传 3 个");
    assert_eq!(done(2, 0, 0), "同步完成：收到 2 个文件");
    assert_eq!(
        done(2, 3, 1),
        "同步完成：收到 2 个文件、上传 3 个，1 个冲突"
    );
    assert_eq!(done(0, 0, 2), "同步完成，2 个冲突");
    assert_eq!(notice_text(&SyncNotice::Failed("x".into())), "同步失败：x");
    assert_eq!(notice_text(&SyncNotice::Busy), "正在同步，完成后会提示");
    assert_eq!(notice_text(&SyncNotice::Disabled), "数据同步未启用");
    assert_eq!(
        notice_text(&SyncNotice::BackgroundFailed("y".into())),
        "数据同步出错：y"
    );
}

#[test]
fn background_failure_dedups_across_retry_cycles() {
    let mut r = rig();
    for st in [
        round_status("error", 1, fail_round("断网")),
        round_status("syncing", 1, fail_round("断网")),
        round_status("error", 2, fail_round("断网")),
        round_status("syncing", 2, fail_round("断网")),
        round_status("idle", 3, ok_round(0, 0)),
        round_status("syncing", 3, ok_round(0, 0)),
        round_status("error", 4, fail_round("又断")),
    ] {
        send_status(&mut r, st);
    }
    assert_eq!(
        notices(&r),
        vec![
            SyncNotice::BackgroundFailed("断网".into()),
            SyncNotice::BackgroundFailed("又断".into()),
        ]
    );
}

#[test]
fn active_failure_then_retries_reports_once() {
    let mut r = rig();
    let _hello = r.stub.next();
    run_active(&mut r, Ok(json!({ "queued": true, "ticket": 1 })));
    for st in [
        round_status("error", 1, fail_round("500")),
        round_status("syncing", 1, fail_round("500")),
        round_status("error", 2, fail_round("500")),
    ] {
        send_status(&mut r, st);
    }
    assert_eq!(notices(&r), vec![SyncNotice::Failed("500".into())]);
}

#[test]
fn sync_now_returns_without_waiting_for_the_reply() {
    let mut r = rig();
    let _hello = r.stub.next();
    let t = Instant::now();
    let v = r.host.sync_now_active();
    assert_eq!(v["outcome"], "requested");
    assert!(t.elapsed() < Duration::from_secs(2));
    // 回执未到时再点：不另起线程，只会有一帧 SyncNow。
    r.host.sync_now_active();
    let ToSync::Command { id, .. } = r.stub.next() else {
        panic!()
    };
    assert!(
        r.stub
            .from_core
            .recv_timeout(Duration::from_millis(200))
            .is_err()
    );
    r.stub.send(FromSync::command_reply(
        id,
        Ok(json!({ "queued": true, "ticket": 1 })),
    ));
    wait_for(|| !r.host.now_in_flight.load(Ordering::SeqCst));
}

#[test]
fn reply_without_ticket_is_a_failure() {
    let mut r = rig();
    let _hello = r.stub.next();
    run_active(&mut r, Ok(json!({ "queued": true })));
    assert_eq!(
        notices(&r),
        vec![SyncNotice::Failed("同步进程回执缺少票号".into())]
    );
}

#[test]
fn reply_after_abandonment_is_ignored() {
    let mut r = rig();
    let _hello = r.stub.next();
    let base = Instant::now();
    let offset = Arc::new(Mutex::new(Duration::ZERO));
    {
        let offset = offset.clone();
        r.host.set_clock(Box::new(move || base + *lock(&offset)));
    }
    r.host.sync_now_active();
    let ToSync::Command { id, .. } = r.stub.next() else {
        panic!()
    };
    *lock(&offset) = Duration::from_secs(11 * 60);
    r.host.pump();
    assert_eq!(
        notices(&r),
        vec![SyncNotice::Failed("等待超过 10 分钟".into())]
    );
    r.stub.send(FromSync::command_reply(
        id,
        Ok(json!({ "queued": true, "ticket": 1 })),
    ));
    wait_for(|| !r.host.now_in_flight.load(Ordering::SeqCst));
    assert_eq!(notices(&r).len(), 1, "放弃之后的回执不再报");
}

#[test]
fn free_retry_phase_flapping_never_notifies_until_a_round_fails() {
    let mut r = rig();
    send_status(&mut r, round_status("idle", 3, ok_round(0, 0)));
    for st in [
        round_status("error", 3, ok_round(0, 0)),
        round_status("syncing", 3, ok_round(0, 0)),
        round_status("error", 3, ok_round(0, 0)),
        round_status("syncing", 3, ok_round(0, 0)),
        round_status("error", 3, ok_round(0, 0)),
    ] {
        send_status(&mut r, st);
    }
    assert!(
        notices(&r).is_empty(),
        "稍后重试不结束一轮: {:?}",
        notices(&r)
    );
    send_status(&mut r, round_status("error", 4, fail_round("断网")));
    assert_eq!(
        notices(&r),
        vec![SyncNotice::BackgroundFailed("断网".into())]
    );
}

#[test]
fn supervisor_trip_notifies_once() {
    use wind_supervise::backoff::Backoff;
    let dir = tempfile::tempdir().unwrap();
    save_settings(&dir.path().join("state/sync/settings.toml"), &webdav(true)).unwrap();
    let sup = Supervisor::new(|| Err("起不来".into())).with_backoff(Backoff::new(
        Duration::from_millis(1),
        Duration::from_secs(60),
        1,
    ));
    let paths = Paths {
        settings_file: dir.path().join("state/sync/settings.toml"),
        state_dir: dir.path().join("state/sync"),
        userdata_dir: dir.path().join("user"),
        suffix: "_t".into(),
    };
    let host = SyncHost::new(Some(paths), sup, Box::new(|| true), COMMAND_TIMEOUT);
    let got = Arc::new(Mutex::new(Vec::new()));
    {
        let got = got.clone();
        host.set_notify(Box::new(move |n| lock(&got).push(n)));
    }
    for _ in 0..5 {
        host.tick(Instant::now());
    }
    assert_eq!(phase(&host), "error");
    assert_eq!(
        *lock(&got),
        vec![SyncNotice::BackgroundFailed(
            "同步进程反复退出，已停止自动同步".into()
        )]
    );
    for _ in 0..5 {
        host.tick(Instant::now());
    }
    assert_eq!(lock(&got).len(), 1, "保持熔断不再提示");
}

#[test]
fn second_click_while_ticket_pending_says_busy() {
    let mut r = rig();
    let _hello = r.stub.next();
    run_active(&mut r, Ok(json!({ "queued": true, "ticket": 3 })));
    let v = r.host.sync_now_active();
    assert_eq!(v, json!({ "outcome": "requested" }));
    assert_eq!(notices(&r), vec![SyncNotice::Busy]);
}

fn conflict_round(n: u32) -> Option<RoundStats> {
    Some(RoundStats {
        ok: true,
        conflicts: n,
        ..Default::default()
    })
}

#[test]
fn background_conflict_notifies_once_until_cleared() {
    let mut r = rig();
    send_status(&mut r, round_status("idle", 1, conflict_round(2)));
    send_status(&mut r, round_status("idle", 2, conflict_round(2)));
    assert_eq!(notices(&r), vec![SyncNotice::BackgroundConflict(2)]);
    send_status(&mut r, round_status("idle", 3, ok_round(0, 0)));
    send_status(&mut r, round_status("idle", 4, conflict_round(1)));
    assert_eq!(
        notices(&r),
        vec![
            SyncNotice::BackgroundConflict(2),
            SyncNotice::BackgroundConflict(1)
        ]
    );
}

#[test]
fn active_round_conflicts_are_in_done_only() {
    let mut r = rig();
    let _hello = r.stub.next();
    run_active(&mut r, Ok(json!({ "queued": true, "ticket": 1 })));
    send_status(&mut r, round_status("idle", 1, conflict_round(3)));
    send_status(&mut r, round_status("idle", 2, conflict_round(3)));
    assert_eq!(
        notices(&r),
        vec![SyncNotice::Done(conflict_round(3).unwrap())]
    );
}

#[test]
fn background_conflict_text() {
    assert_eq!(
        notice_text(&SyncNotice::BackgroundConflict(4)),
        "同步产生 4 个冲突，请到设置页处理"
    );
}

#[test]
fn ticket_timeout_does_not_count_syncing_time_and_ignores_last_error() {
    let mut r = rig();
    let _hello = r.stub.next();
    let base = Instant::now();
    let offset = Arc::new(Mutex::new(Duration::ZERO));
    {
        let offset = offset.clone();
        r.host.set_clock(Box::new(move || base + *lock(&offset)));
    }
    run_active(&mut r, Ok(json!({ "queued": true, "ticket": 9 })));
    *lock(&offset) = Duration::from_secs(11 * 60);
    send_status(&mut r, round_status("syncing", 8, ok_round(0, 0)));
    assert!(notices(&r).is_empty(), "同步中不计时: {:?}", notices(&r));
    *lock(&offset) = Duration::from_secs(15 * 60);
    let mut s = round_status("idle", 8, ok_round(0, 0));
    s.last_error = Some("旧错误".into());
    send_status(&mut r, s);
    assert!(notices(&r).is_empty(), "从同步结束起算: {:?}", notices(&r));
    *lock(&offset) = Duration::from_secs(26 * 60);
    r.host.pump();
    assert_eq!(
        notices(&r),
        vec![SyncNotice::Failed("等待超过 10 分钟".into())]
    );
}

#[test]
fn error_already_present_at_attach_does_not_notify() {
    let mut r = rig();
    let mut s = round_status("error", 0, None);
    s.last_error = Some("网络不通".into());
    send_status(&mut r, s);
    send_status(&mut r, round_status("error", 1, fail_round("网络不通")));
    assert!(notices(&r).is_empty(), "{:?}", notices(&r));
    send_status(&mut r, round_status("idle", 2, ok_round(0, 0)));
    send_status(&mut r, round_status("error", 3, fail_round("又断")));
    assert_eq!(
        notices(&r),
        vec![SyncNotice::BackgroundFailed("又断".into())]
    );
}

fn identity_status() -> Status {
    Status {
        phase: "idle".into(),
        has_secret: true,
        joined: true,
        device: "pc".into(),
        group: "home".into(),
        group_name: "家".into(),
        ..Default::default()
    }
}

/// 同步进程没在跑（停用）时，重启后的 core 仍报出上次的身份；第一份真实状态再覆盖。
#[test]
fn identity_snapshot_survives_restart_while_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let mut off = webdav(false);
    off.enabled = false;
    save_settings(&dir.path().join("state/sync/settings.toml"), &off).unwrap();
    IdentitySnapshot::of(&identity_status()).save(&dir.path().join("state/sync"));

    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    let v = host.view(&lock(&host.inner));
    assert_eq!(v["phase"], "disabled");
    assert_eq!(
        (v["joined"].clone(), v["device"].clone(), v["group"].clone()),
        (json!(true), json!("pc"), json!("home"))
    );
    assert_eq!(v["has_secret"], true);

    // 接上进程后，第一份真实状态到来前不清空
    let mut stub = attach_stub(&host);
    assert!(lock(&host.inner).status.joined);
    let real = Status {
        phase: "idle".into(),
        device: "pc-2".into(),
        ..Default::default()
    };
    stub.send(FromSync::Status(real.clone()));
    wait_for(|| lock(&host.inner).status == real);
    // 真实状态变了身份，快照随之更新
    wait_for(|| IdentitySnapshot::load(&dir.path().join("state/sync")).device == "pc-2");
    assert!(!IdentitySnapshot::load(&dir.path().join("state/sync")).joined);
}

/// 快照文件损坏：按默认处理，不 panic。
#[test]
fn corrupt_identity_snapshot_falls_back_to_default() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state/sync");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(IdentitySnapshot::file(&state), b"{not json").unwrap();
    assert_eq!(IdentitySnapshot::load(&state), IdentitySnapshot::default());
    let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
    assert!(!lock(&host.inner).status.joined);
}
