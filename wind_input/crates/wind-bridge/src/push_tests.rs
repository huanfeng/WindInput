use super::*;

/// 锁定 push 管道名格式：变体后缀紧跟 `wind_input_push`，per-user SID 后缀再在其后，
/// 与 TSF Globals.cpp 一致，否则 TSF 连不上 push 管道。SID 段随机器/用户变化，
/// 故只断言前缀不变量；Windows 上另断言确有 `_S-` 后缀。
#[test]
fn test_push_pipe_name_suffix_position() {
    let dev = PushServer::new(PushConfig {
        suffix: "_dev".into(),
        write_timeout_ms: 30_000,
    });
    let dev_name = dev.pipe_name();
    assert!(
        dev_name.starts_with(r"\\.\pipe\wind_input_push_dev"),
        "变体后缀须紧跟 wind_input_push，实得 {dev_name}"
    );

    let release = PushServer::new(PushConfig {
        suffix: String::new(),
        write_timeout_ms: 30_000,
    });
    let rel_name = release.pipe_name();
    assert!(
        rel_name.starts_with(r"\\.\pipe\wind_input_push"),
        "实得 {rel_name}"
    );

    // Windows：SID 后缀必在变体后缀之后（`..._push_dev_S-...`）。
    #[cfg(windows)]
    {
        assert!(
            dev_name.contains(r"\wind_input_push_dev_S-"),
            "缺 per-user SID 后缀，实得 {dev_name}"
        );
        assert!(
            rel_name.contains(r"\wind_input_push_S-"),
            "缺 per-user SID 后缀，实得 {rel_name}"
        );
    }
}

/// push_to_token 必须精确匹配、无兜底：activation push 的 hostRenderAvail 位按事件源
/// 计算，错发给别的客户端会触发 Band 窗口销毁重建循环（真机踩坑）。
#[test]
fn push_to_token_exact_match_no_fallback() {
    let srv = PushServer::new(PushConfig::default());
    let (tx, rx) = push_channel();
    srv.clients_for_test().lock().unwrap().push(PushClient {
        token: 0xAA_0000_0001,
        tx,
        hooked: false,
        ready_sent: true,
    });

    // 命中：精确 token 投递
    assert!(srv.push_to_token(0xAA_0000_0001, &[1, 2, 3]));
    assert_eq!(rx.try_recv().unwrap(), vec![1, 2, 3]);

    // 未命中：即使只有一个客户端也不得兜底投递
    assert!(!srv.push_to_token(0xBB_0000_0002, &[9]));
    assert!(rx.try_recv().is_err(), "不匹配的 token 不得收到任何帧");
}

/// 对端不读（写线程卡在 WriteFile）时，队列满即把它移出客户端表：内存封顶在
/// [`PUSH_QUEUE_CAP`] 帧，且别的客户端照常收帧。此前是无界队列，卡多久涨多久。
#[test]
fn stalled_client_is_dropped_when_queue_fills() {
    let srv = PushServer::new(PushConfig::default());
    let (stuck_tx, stuck_rx) = push_channel(); // 持有但从不读 = 卡住的对端
    let (live_tx, live_rx) = push_channel();
    let clients = srv.clients_for_test();
    {
        let mut c = clients.lock().unwrap();
        c.push(PushClient {
            token: 1,
            tx: stuck_tx,
            hooked: true,
            ready_sent: true,
        });
        c.push(PushClient {
            token: 2,
            tx: live_tx,
            hooked: true,
            ready_sent: true,
        });
    }
    for i in 0..PUSH_QUEUE_CAP + 10 {
        srv.push_to_active(&[i as u8]);
        while live_rx.try_recv().is_ok() {}
    }
    let tokens: Vec<u64> = clients.lock().unwrap().iter().map(|c| c.token).collect();
    assert_eq!(tokens, vec![2], "卡住的客户端该被移除，正常的留下");
    assert_eq!(
        stuck_rx.try_iter().count(),
        PUSH_QUEUE_CAP,
        "积压不得超过上限"
    );
}

fn add_test_client(srv: &PushServer, token: u64) {
    let (tx, _rx) = push_channel();
    srv.clients_for_test().lock().unwrap().push(PushClient {
        token,
        tx,
        hooked: false,
        ready_sent: false,
    });
}

/// hook 注册前已完成握手的客户端必须被补跑：`start()` 与注册 hook 之间隔着
/// `Coordinator::new()`，这段窗口里重连的 SearchHost 会丢掉 activation push，
/// HostRender 永不激活。
#[test]
fn hook_registration_replays_clients_connected_before_it() {
    let srv = PushServer::new(PushConfig::default());
    add_test_client(&srv, 0xAA_0000_0001);
    add_test_client(&srv, 0xBB_0000_0001);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_c = seen.clone();
    srv.set_client_connected_hook(Box::new(move |t| seen_c.lock().unwrap().push(t)));

    let mut got = seen.lock().unwrap().clone();
    got.sort_unstable();
    assert_eq!(
        got,
        vec![0xAA_0000_0001, 0xBB_0000_0001],
        "注册前连接的客户端必须全部补跑，否则 SearchHost 永久停留本地渲染"
    );
}

/// 补跑与连接线程的自发触发必须互斥去重（同一客户端恰好触发一次）。
#[test]
fn connected_hook_fires_exactly_once_per_client() {
    let srv = PushServer::new(PushConfig::default());
    add_test_client(&srv, 0xAA_0000_0001);

    let count = Arc::new(AtomicU64::new(0));
    let count_c = count.clone();
    srv.set_client_connected_hook(Box::new(move |_| {
        count_c.fetch_add(1, Ordering::Relaxed);
    }));
    assert_eq!(count.load(Ordering::Relaxed), 1, "补跑应触发一次");

    // 模拟连接线程随后走到自己的触发点：认领已被补跑拿走，不得重复触发
    assert!(
        !claim_connected_hook(&srv.clients_for_test(), 0xAA_0000_0001),
        "已补跑的客户端不得被连接线程再次认领"
    );

    // 断开的客户端不得被认领（不给死连接推帧）
    assert!(!claim_connected_hook(&srv.clients_for_test(), 0xDEAD_0000));
}

/// 重连（同 token 重新入表）必须能再次触发 hook——DLL 重连后需要重新 setup。
#[test]
fn reconnected_client_can_be_claimed_again() {
    let srv = PushServer::new(PushConfig::default());
    add_test_client(&srv, 0xAA_0000_0001);
    assert!(claim_connected_hook(
        &srv.clients_for_test(),
        0xAA_0000_0001
    ));

    // 重连：连接路径 retain 掉旧条目后压入新条目（hooked 复位）
    srv.clients_for_test()
        .lock()
        .unwrap()
        .retain(|c| c.token != 0xAA_0000_0001);
    add_test_client(&srv, 0xAA_0000_0001);
    assert!(
        claim_connected_hook(&srv.clients_for_test(), 0xAA_0000_0001),
        "重连后必须可再次认领，否则 DLL 重连不会重新 setup"
    );
}

/// 就绪前连上的客户端不能先拿到 SERVICE_READY，要等 `mark_ready` 补发：否则 DLL 的
/// IME_ACTIVATED 被未就绪的 DeferredHandler 丢弃，工具栏要等切焦点或按键才出来。
#[test]
fn clients_registered_before_ready_get_service_ready_on_mark_ready() {
    let srv = PushServer::new(PushConfig::default());
    let (tx, rx) = push_channel();
    register_client(&srv.clients_for_test(), &srv.ready, 0xAA_0000_0001, tx);
    assert!(
        rx.try_recv().is_err(),
        "未就绪时不得发 SERVICE_READY，否则 DLL 的全量同步会被未就绪的处理器吞掉"
    );

    srv.mark_ready();
    assert_eq!(rx.try_recv().ok(), Some(service_ready_frame()));

    srv.mark_ready();
    assert!(rx.try_recv().is_err(), "重复 mark_ready 不得重发");
}

/// 就绪后连上的客户端照旧在注册时立刻拿到 SERVICE_READY（平时的快速重启路径）。
#[test]
fn clients_registered_after_ready_get_service_ready_immediately() {
    let srv = PushServer::new(PushConfig::default());
    srv.mark_ready();
    let (tx, rx) = push_channel();
    register_client(&srv.clients_for_test(), &srv.ready, 0xAA_0000_0001, tx);
    assert_eq!(rx.try_recv().ok(), Some(service_ready_frame()));
    assert!(rx.try_recv().is_err(), "只发一次");
}

/// 补发的 SERVICE_READY 必须排在 hook 补跑推的握手配置之前：DLL 收到 SERVICE_READY
/// 会把会话级开关（标题采集）清零，晚到就会冲掉刚推来的新值。main.rs 先 `mark_ready`
/// 再 `set_client_connected_hook`，同一条发送队列保证这个顺序。
#[test]
fn service_ready_precedes_replayed_handshake_config() {
    let srv = Arc::new(PushServer::new(PushConfig::default()));
    let (tx, rx) = push_channel();
    register_client(&srv.clients_for_test(), &srv.ready, 0xAA_0000_0001, tx);

    srv.mark_ready();
    let s = srv.clone();
    srv.set_client_connected_hook(Box::new(move |t| {
        s.push_to_token(t, b"cfg");
    }));

    assert_eq!(rx.try_recv().ok(), Some(service_ready_frame()));
    assert_eq!(rx.try_recv().ok(), Some(b"cfg".to_vec()));
}
