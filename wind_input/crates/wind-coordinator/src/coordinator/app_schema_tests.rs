use super::*;
use std::path::PathBuf;

/// 两个自造码表方案 za / zb 的数据目录 + 空 override 目录（不碰真实用户目录）。
fn fixture(tag: &str) -> (Arc<Coordinator>, PathBuf) {
    fixture_with(tag, |_| {})
}

/// 同 [`fixture`]，另可改配置。
fn fixture_with(tag: &str, tweak: impl FnOnce(&mut Config)) -> (Arc<Coordinator>, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("wind_app_schema_unit_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("data/schemas");
    for id in ["za", "zb"] {
        std::fs::create_dir_all(schemas.join(id)).unwrap();
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"{id}\"\n\
                     [engine]\ntype = \"codetable\"\n\
                     [engine.codetable]\nmax_code_length = 4\n\
                     [[dictionaries]]\nid = \"main\"\npath = \"{id}/{id}.dict.yaml\"\ndefault = true\n"
            ),
        )
        .unwrap();
        std::fs::write(
            schemas.join(format!("{id}/{id}.dict.yaml")),
            format!("---\nname: {id}\nversion: \"1\"\n...\n甲\ta\n"),
        )
        .unwrap();
    }
    std::fs::create_dir_all(dir.join("override")).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["za".into(), "zb".into()];
    cfg.schema.active = "za".into();
    tweak(&mut cfg);
    let c = Coordinator::new_headless_with_override(
        cfg,
        Some(&dir.join("data")),
        Some(dir.join("override")),
    );
    let rules = vec![wind_config::app_compat::AppCompatRule {
        process: "code.exe".into(),
        schema: Some("zb".into()),
        ..Default::default()
    }];
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);
    (c, dir)
}

/// 冷方案不在焦点路径上同步构建：切入时起后台加载，完成后才切。
#[test]
fn cold_target_loads_in_background() {
    let (c, dir) = fixture("cold");
    assert!(
        !c.engine_mgr.is_loaded("zb"),
        "前置条件：headless 不预热，zb 是冷的"
    );
    c.apply_app_schema_on_focus("code.exe", &FocusWindow::default());
    assert!(
        c.app_schema.pending_load.lock().unwrap().is_some(),
        "冷方案必须交给后台线程，而不是在焦点路径上同步构建"
    );
    c.debug_wait_app_schema_load();
    assert_eq!(
        c.engine_mgr.active_schema_id(),
        "zb",
        "加载完成且焦点未离开 ⇒ 切过去"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 加载完成时焦点已离开（或期间手切过）⇒ 不切；意图代际未变才切。
#[test]
fn deferred_switch_is_dropped_when_intent_moved_on() {
    let (c, dir) = fixture("stale");
    c.apply_app_schema_on_focus("code.exe", &FocusWindow::default());
    c.debug_wait_app_schema_load();
    // 回到起点：焦点落到无规则应用（目标 = 全局 za）。
    c.apply_app_schema_on_focus("plain.exe", &FocusWindow::default());
    assert_eq!(c.engine_mgr.active_schema_id(), "za");

    // 模拟「切入 code.exe 时 zb 还冷、加载期间焦点又切走了」。
    let stale = c.app_schema.intent_gen.load(Ordering::SeqCst);
    c.apply_app_schema_on_focus("plain.exe", &FocusWindow::default());
    c.finish_deferred_app_schema(stale, "code.exe", &FocusWindow::default(), "zb");
    assert_eq!(
        c.engine_mgr.active_schema_id(),
        "za",
        "焦点已离开，延后切换必须作废"
    );

    // 对照：代际未变 ⇒ 照切（否则上一条断言在「永远不切」的实现下也会绿）。
    let current = c.app_schema.intent_gen.load(Ordering::SeqCst);
    c.finish_deferred_app_schema(current, "code.exe", &FocusWindow::default(), "zb");
    assert_eq!(c.engine_mgr.active_schema_id(), "zb");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 把 zb 暖好（冷加载一次）再回到 za：之后切入 code.exe 走同步轻量切换。
fn warm_zb_then_back_to_za(c: &Arc<Coordinator>) {
    c.apply_app_schema_on_focus("code.exe", &FocusWindow::default());
    c.debug_wait_app_schema_load();
    c.apply_app_schema_on_focus("plain.exe", &FocusWindow::default());
    assert_eq!(c.engine_mgr.active_schema_id(), "za", "前置：回到全局 za");
    assert!(c.engine_mgr.is_loaded("zb"), "前置：zb 已暖");
}

/// 等 `cond` 成立，最多 2s。只用来给错误实现的反例留出显现时间；正确实现下各用例的
/// 结论与等待时长无关。
fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if cond() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// 锁序：`schema_toggle_origin` 与 `parked_toggle` 永不同时持有。
///
/// 主线程持 `parked_toggle`，后台跑 `switch_schema_light`：若它 take 走记录后仍持着 origin
/// 去等 parked，就构成死锁形状（这里用 `try_lock` 观察而不真等）。正确实现 take 完即放锁。
#[test]
fn toggle_record_locks_are_never_held_together() {
    let (c, dir) = fixture("locks");
    warm_zb_then_back_to_za(&c);
    *c.schema_toggle_origin.lock().unwrap() = Some(SchemaToggleOrigin {
        origin: "zb".into(),
        landing: ToggleLanding {
            generation: c.engine_mgr.schema_generation(),
            chinese_mode: true,
            caps_lock: false,
        },
        trigger_vk: 0,
    });
    let parked = c.app_schema.parked_toggle.lock().unwrap();
    let c2 = c.clone();
    let h =
        std::thread::spawn(move || c2.switch_schema_light("zb", "code.exe", PendingInput::Discard));
    let released = wait_until(|| match c.schema_toggle_origin.try_lock() {
        Ok(g) => g.is_none(),
        Err(std::sync::TryLockError::WouldBlock) => false,
        Err(std::sync::TryLockError::Poisoned(e)) => panic!("origin 锁中毒: {e}"),
    });
    drop(parked);
    h.join().unwrap();
    assert!(
        released,
        "switch_schema_light 持着 schema_toggle_origin 去等 parked_toggle——与另一段的反向锁序构成死锁"
    );
    assert_eq!(c.engine_mgr.active_schema_id(), "zb");
    assert!(
        c.app_schema
            .parked_toggle
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(id, _)| id == "za"),
        "仍有效的去程记录被挂起到来源方案 za 名下"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 冷方案延后切换的「检查 + 切换」与焦点切入互斥。
///
/// 主线程持 `switch_lock` 扮演「焦点切入进行中」，后台带着此刻有效的代际做延后切换，
/// 随后切入完成（代际 +1，目标 za）。不等锁的实现会切到已作废的 zb；正确实现拿到锁时
/// 代际已变，放弃。
#[test]
fn deferred_switch_cannot_interleave_with_a_focus_switch() {
    let (c, dir) = fixture("race");
    warm_zb_then_back_to_za(&c);
    let stale = c.app_schema.intent_gen.load(Ordering::SeqCst);
    let focus_in_progress = c.app_schema.switch_lock.lock().unwrap();
    let c2 = c.clone();
    let h = std::thread::spawn(move || {
        c2.finish_deferred_app_schema(stale, "code.exe", &FocusWindow::default(), "zb")
    });
    // 旧实现下后台线程不等锁，这段时间里就跑完了；新实现下它一直卡在锁上。
    let _ = wait_until(|| h.is_finished());
    // 焦点线程完成这次切入：代际 +1（目标 za = 当前，无需切）。
    c.app_schema.intent_gen.fetch_add(1, Ordering::SeqCst);
    drop(focus_in_progress);
    h.join().unwrap();
    assert_eq!(
        c.engine_mgr.active_schema_id(),
        "za",
        "延后切换的检查与切换之间插进了一次焦点切入，不得再切回旧目标"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 焦点切入触发的轻量切换只丢弃缓冲：那一刻 active token 已是新宿主，按
/// `commit_on_switch` 上屏会把旧应用的码打进新应用。上屏语义只留给冷方案加载完成那条路。
#[test]
fn focus_switch_discards_pending_input_but_deferred_switch_commits() {
    let (c, dir) = fixture_with("discard", |cfg| cfg.keys.commit_on_switch = true);
    warm_zb_then_back_to_za(&c);
    let cap = c.push_server.attach_capture_client((42u64 << 32) | 1);
    let commit_a = wind_ipc::codec::encode_commit_text("a", None, false, true, false);
    let clear = wind_ipc::codec::encode_clear_composition();

    c.state.lock().unwrap().input_buffer = "a".into();
    c.apply_app_schema_on_focus("code.exe", &FocusWindow::default());
    assert_eq!(c.engine_mgr.active_schema_id(), "zb");
    let got: Vec<Vec<u8>> = cap.try_iter().collect();
    assert!(
        !got.contains(&commit_a),
        "焦点切入不得把旧缓冲按 commit_on_switch 上屏到新宿主"
    );
    assert!(!got.contains(&clear), "也不该给新宿主推清组合");
    assert!(
        c.state.lock().unwrap().input_buffer.is_empty(),
        "缓冲必须丢弃"
    );

    // 对照：冷方案加载完成（焦点仍在该应用）那条路照 commit_on_switch 上屏——
    // 否则上面几条断言在「一律不上屏」的实现下也会绿。
    c.apply_app_schema_on_focus("plain.exe", &FocusWindow::default());
    c.state.lock().unwrap().input_buffer = "a".into();
    let _ = cap.try_iter().count();
    let current = c.app_schema.intent_gen.load(Ordering::SeqCst);
    c.finish_deferred_app_schema(current, "code.exe", &FocusWindow::default(), "zb");
    assert_eq!(c.engine_mgr.active_schema_id(), "zb");
    let got: Vec<Vec<u8>> = cap.try_iter().collect();
    assert!(
        got.contains(&commit_a),
        "冷方案加载完成时缓冲属于该应用，按策略上屏"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 固定 id 不在 available：同一 `(进程名, id)` 只 WARN 一次（每次切入/手切都会问规则）。
#[test]
fn invalid_fixed_id_is_reported_once_per_process_and_id() {
    let (c, dir) = fixture("warn");
    let rules = ["code.exe", "vim.exe"]
        .into_iter()
        .map(|p| wind_config::app_compat::AppCompatRule {
            process: p.into(),
            schema: Some("gone".into()),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);
    for _ in 0..3 {
        assert_eq!(
            c.app_schema_rule("code.exe", &FocusWindow::default()),
            None,
            "不在 available ⇒ 未配置"
        );
    }
    let reported = || c.app_schema.warned_invalid.lock().unwrap().len();
    assert_eq!(reported(), 1, "同一 (进程, id) 只报一次");
    assert_eq!(c.app_schema_rule("vim.exe", &FocusWindow::default()), None);
    assert_eq!(reported(), 2, "换一个进程要另报");
    let _ = std::fs::remove_dir_all(&dir);
}
