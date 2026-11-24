use super::*;
use std::io::{PipeReader, PipeWriter, pipe};
use wind_sync_ipc::Backend;

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
    Arc::new(SyncHost::new(
        Some(paths),
        sup,
        Box::new(move || installed),
        timeout,
    ))
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
                let tag = match p.get("path").and_then(Value::as_str) {
                    Some(path) => format!("{m} {path}"),
                    None => m.to_string(),
                };
                lock(&executed).push(tag);
                Ok(json!({ "ran": m }))
            })
        },
    );
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
    Rig {
        host,
        stub: Stub {
            from_core,
            to_core: child_stdout_w,
        },
        events,
        executed,
        dir,
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
    r.stub.send(FromSync::Status(Status {
        phase: "idle".into(),
        last_ok: Some(100),
        pending: 2,
        conflicts: 1,
        conflict_paths: vec!["config.conflict-a-1.toml".into()],
        has_secret: true,
        joined: true,
        ..Default::default()
    }));
    wait_for(|| phase(&r.host) == "idle");
    let st = r.host.rpc("sync.status", &Value::Null).unwrap();
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
    r.stub.send(FromSync::Status(Status {
        phase: "idle".into(),
        last_ok: Some(100),
        pending: 2,
        conflicts: 1,
        conflict_paths: vec!["config.conflict-a-1.toml".into()],
        has_secret: true,
        joined: true,
        ..Default::default()
    }));
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
        .send(FromSync::command_reply(id, Err("口令错误".into())));
    assert!(
        call.join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("口令错误")
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
