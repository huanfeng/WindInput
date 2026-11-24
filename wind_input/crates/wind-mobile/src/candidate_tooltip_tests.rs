use super::*;

#[test]
fn tooltip_by_absolute_index() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let user = std::env::temp_dir().join(format!("wind_mobile_tip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = false;
    let (coord, _rx) =
        Coordinator::new_session(cfg, Some(&data), Some(&user), InputProfile::mobile());
    let m = MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: user.clone(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    };
    assert!(m.candidate_tooltip(0).is_empty(), "无候选时为空");
    for ch in ['W', 'Q', 'V', 'B'] {
        dispatch_key(&m.coord, ch as u32, 0);
    }
    let all = m.candidates(0, MAX_WINDOW);
    assert!(all.total > 0, "前提：wqvb 有候选");
    assert!(m.candidate_tooltip(all.total).is_empty(), "越界为空");
    // 编码段靠反查索引，第一次取时才在后台建：轮询到它出现（模拟器外通常一两百毫秒）
    let mut code_line = None;
    for _ in 0..100 {
        let tip = m.candidate_tooltip(0);
        assert!(
            tip.iter().all(|s| !s.lines.is_empty()),
            "段不应为空: {tip:?}"
        );
        code_line = tip
            .iter()
            .find(|s| s.title.as_deref().is_some_and(|t| t.starts_with("编码")))
            .map(|s| s.lines.join(" "));
        if code_line.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let code_line = code_line.expect("编码段应在索引建好后出现");
    assert!(
        code_line.contains("wqvb"),
        "首选的编码里应有 wqvb: {code_line}"
    );
    let _ = std::fs::remove_dir_all(&user);
}

/// 候选窗带注释：五笔打 a，有剩余编码的候选注释非空；不带注释的接口恒空、文本两边一致。
#[test]
fn window_with_comments_matches_items() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let user = std::env::temp_dir().join(format!("wind_mobile_cmt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = false;
    let (coord, _rx) =
        Coordinator::new_session(cfg, Some(&data), Some(&user), InputProfile::mobile());
    let m = MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: user.clone(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    };
    dispatch_key(&m.coord, 'A' as u32, 0);
    let plain = m.candidates(0, 20);
    let rich = m.candidates_with_comments(0, 20);
    assert!(plain.comments.is_empty(), "不带注释的接口不该算注释");
    assert_eq!(plain.items, rich.items, "两个接口的候选文本应一致");
    assert_eq!(rich.comments.len(), rich.items.len(), "注释与候选一一对应");
    assert!(
        rich.comments.iter().any(|c| !c.is_empty()),
        "五笔单码 a 的候选里应有带剩余编码的: {:?}",
        rich.items.iter().zip(&rich.comments).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&user);
}

/// 滑到尾部再取：五笔单码首批 100 条（has_more），要第 90～150 条时核心扩容重建、
/// 标 rebuilt，总数变多；要的范围在已加载之内时不重建。
#[test]
fn window_grows_past_loaded_candidates() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let user = std::env::temp_dir().join(format!("wind_mobile_grow_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = false;
    let (coord, _rx) =
        Coordinator::new_session(cfg, Some(&data), Some(&user), InputProfile::mobile());
    let m = MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: user.clone(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    };
    dispatch_key(&m.coord, 'A' as u32, 0);
    let first = m.candidates(0, 24);
    assert!(!first.rebuilt, "首屏在已加载范围内，不该重建");
    let loaded = first.total;
    assert!(loaded >= 90, "前提：五笔单码首批至少 90 条，实得 {loaded}");
    let more = m.candidates(loaded - 10, 60);
    assert!(more.rebuilt, "要的范围超出已加载且还有更多，应扩容重建");
    assert!(
        more.total > loaded,
        "扩容后总数应变多：{loaded} → {}",
        more.total
    );
    assert_eq!(more.items.len(), 60, "扩容后这一段应取满");
    let again = m.candidates(0, 24);
    assert!(!again.rebuilt, "已加载范围内再取不重建");
    let _ = std::fs::remove_dir_all(&user);
}

/// 单字母预热只查引擎、不碰输入状态：预热完没有残留编码 / 候选，接着照常打字。
/// 码表方案直接跳过（函数自己判引擎类型）。
#[test]
fn prewarm_leaves_no_composition() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/pinyin.schema.toml").exists() {
        eprintln!("跳过：拼音方案不存在");
        return;
    }
    let user = std::env::temp_dir().join(format!("wind_mobile_warm_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    let (coord, _rx) =
        Coordinator::new_session(cfg, Some(&data), Some(&user), InputProfile::mobile());
    let m = MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: user.clone(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    };
    m.coord.prewarm_single_initials();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(
        m.candidates(0, 5).total,
        0,
        "预热不该留下候选（它不走按键路径）"
    );
    dispatch_key(&m.coord, 'N' as u32, 0);
    dispatch_key(&m.coord, 'I' as u32, 0);
    assert!(m.candidates(0, 5).total > 0, "预热后照常打字出候选");
    let _ = std::fs::remove_dir_all(&user);
}
