use super::*;

#[test]
fn flatten_keeps_op_and_copy_leaves_and_inlines_submenus() {
    let items = vec![
        MenuItemSpec::leaf("置顶", MenuKind::Op(CandidateOp::MoveTop), false, false),
        MenuItemSpec::leaf(
            "在此编码下隐藏",
            MenuKind::Op(CandidateOp::HideHere),
            true,
            false,
        ),
        MenuItemSpec::separator(),
        MenuItemSpec::label("标题"),
        MenuItemSpec::leaf(
            "命令",
            MenuKind::Command(MenuCmd::OpenSettings),
            true,
            false,
        ),
        MenuItemSpec {
            label: "更多".into(),
            kind: MenuKind::Submenu,
            enabled: true,
            checked: false,
            children: vec![MenuItemSpec::leaf(
                "设为生僻字",
                MenuKind::Op(CandidateOp::ToggleCommon),
                true,
                false,
            )],
        },
        MenuItemSpec::leaf("复制", MenuKind::Copy, true, false),
    ];
    let mut out = Vec::new();
    flatten_candidate_menu(&items, &mut out);
    let got: Vec<(&str, &str, bool)> = out
        .iter()
        .map(|i| (i.label.as_str(), i.action.as_str(), i.enabled))
        .collect();
    assert_eq!(
        got,
        [
            ("置顶", "move_top", false),
            ("在此编码下隐藏", "hide_here", true),
            ("设为生僻字", "toggle_common", true),
            ("复制", "copy", true),
        ]
    );
}

/// 「固定到第 N 位」子菜单摊平成 `pin_at_N`，文案补全；动作名能解析回原操作。
#[test]
fn pin_at_submenu_flattens_to_pin_at_n() {
    let items = vec![MenuItemSpec::submenu(
        "固定到第 N 位",
        vec![
            MenuItemSpec::leaf("第 1 位", MenuKind::Op(CandidateOp::PinAt(1)), true, false),
            MenuItemSpec::leaf("第 2 位", MenuKind::Op(CandidateOp::PinAt(2)), false, false),
        ],
    )];
    let mut out = Vec::new();
    flatten_candidate_menu(&items, &mut out);
    let got: Vec<(&str, &str, bool)> = out
        .iter()
        .map(|i| (i.label.as_str(), i.action.as_str(), i.enabled))
        .collect();
    assert_eq!(
        got,
        [
            ("固定到第 1 位", "pin_at_1", true),
            ("固定到第 2 位", "pin_at_2", false),
        ]
    );
    for n in 1..=wind_ui_types::PIN_AT_MAX {
        let op = CandidateOp::PinAt(n);
        assert_eq!(
            candidate_menu_action(op)
                .as_deref()
                .and_then(candidate_menu_op),
            Some(op)
        );
    }
    assert_eq!(candidate_menu_op("pin_at_x"), None);
}

/// 移动端只摊平前 9 个「第 N 位」。
#[test]
fn pin_at_capped_to_nine_on_mobile() {
    let kids = (1..=12)
        .map(|n| {
            MenuItemSpec::leaf(
                format!("第 {n} 位"),
                MenuKind::Op(CandidateOp::PinAt(n)),
                true,
                false,
            )
        })
        .collect();
    let mut out = Vec::new();
    flatten_candidate_menu(&[MenuItemSpec::submenu("固定到第 N 位", kids)], &mut out);
    let got: Vec<&str> = out.iter().map(|i| i.action.as_str()).collect();
    assert_eq!(
        got,
        (1..=9).map(|n| format!("pin_at_{n}")).collect::<Vec<_>>()
    );
}

/// 真词库下走一遍门面：第二页候选取得到菜单、置顶生效；copy / 未知 action / 灰显项 /
/// 越界都返回 false 或空。词典缺失时跳过（计数照绿）。
#[test]
fn menu_and_run_by_absolute_index() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let user = std::env::temp_dir().join(format!("wind_mobile_cm_{}", std::process::id()));
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
    assert!(m.candidate_menu(0).is_empty(), "无候选时菜单为空");
    for ch in ['A', 'A'] {
        dispatch_key(&m.coord, ch as u32, 0);
    }
    let pp = m.coord.debug_page_texts().len();
    let all = m.candidates(0, MAX_WINDOW);
    assert!(all.total > pp + 1, "前提：aa 有第二页");
    let n = pp + 1;
    let target = all.items[n].clone();

    let menu = m.candidate_menu(n);
    let actions: Vec<&str> = menu.iter().map(|i| i.action.as_str()).collect();
    assert_eq!(actions.first(), Some(&"move_top"), "{menu:?}");
    assert_eq!(actions.last(), Some(&"copy"), "{menu:?}");
    assert!(!actions.contains(&""), "分隔线不应出现: {menu:?}");

    assert!(!m.candidate_menu_run(n, "copy"), "复制归宿主");
    assert!(!m.candidate_menu_run(n, "bogus"), "未知 action");
    assert!(
        !m.candidate_menu_run(0, "move_top"),
        "首位置顶是灰的，不执行"
    );
    assert!(!m.candidate_menu_run(all.total, "move_top"), "越界");
    assert!(m.candidate_menu(all.total).is_empty(), "越界菜单为空");

    assert!(m.candidate_menu_run(n, "move_top"));
    assert_eq!(m.candidates(0, 1).items, [target], "置顶后成首选");
    let _ = std::fs::remove_dir_all(&user);
}
