use super::{anchor_probe_point, avoid_unset_sentinel};

/// emoji 扩展候选的「删除」项恒灰显：它没有词库落点，写端 `candidate_op` 同样拒绝。
/// 两端必须同时成立——只灰一端就是「点得动却没反应」或「明明不能用却亮着」。
#[test]
fn emoji_candidate_delete_menu_is_disabled() {
    let cand = wind_candidate::Candidate {
        text: "😄".into(),
        is_emoji_suggestion: true,
        ..Default::default()
    };
    let (label, enabled) = super::candidate_delete_menu(&cand, false);
    assert_eq!(label, "在此编码下隐藏");
    assert!(!enabled, "emoji 扩展候选不可删除 / 隐藏");
}

/// 删除项文案按来源：按文字屏蔽的词「屏蔽该词」（含拼音用户词 / 临时词），短语、码表用户词 /
/// 临时词、group 成员不变，其余「在此编码下隐藏」。
#[test]
fn candidate_delete_menu_labels() {
    use wind_candidate::Candidate;
    let sys = Candidate {
        text: "你好".into(),
        ..Default::default()
    };
    let mut user = sys.clone();
    user.meta.is_user_dict = true;
    let mut temp = sys.clone();
    temp.meta.is_temp_dict = true;
    let phrase = Candidate {
        text: "电话".into(),
        is_phrase: true,
        ..Default::default()
    };
    let member = Candidate {
        text: "甲".into(),
        is_phrase: true,
        is_group_member: true,
        ..Default::default()
    };
    let label = |c: &Candidate, b| super::candidate_delete_menu(c, b);
    assert_eq!(label(&sys, true), ("屏蔽该词", true));
    assert_eq!(label(&user, true), ("屏蔽该词", true));
    assert_eq!(label(&temp, true), ("屏蔽该词", true));
    assert_eq!(label(&sys, false), ("在此编码下隐藏", true));
    assert_eq!(label(&user, false), ("删除用户词", true));
    assert_eq!(label(&temp, false), ("删除临时词", true));
    assert_eq!(label(&phrase, false), ("禁用短语", true));
    assert_eq!(label(&member, false), ("删除词条", false));
}

/// 锚点落盘查屏必须退 1px，否则贴右/下边缘时存到邻屏的 key 下。
///
/// `clamp_to_work_area` 把工具栏贴到 A 屏右边缘时窗口 right 恰等于 A 屏工作区 right，
/// 拿这个排他边界去 `MonitorFromPoint` 会落在 B 屏（`DEFAULTTONEAREST` 不报错只答错），
/// 于是回 A 屏读不到记录、切到 B 屏却读到 A 屏坐标，工具栏跑回 A 屏。
///
/// 与 `wind-ui` 两个窗口的 `Placement::probe_point` 是同一个不变量：那边管「按哪块屏排版」、
/// 这边管「存进哪块屏的 key」，两边都错会互相掩盖。
#[test]
fn anchor_probe_point_steps_inside_the_exclusive_edge() {
    // A 屏工作区 (0, 0, 1920, 1040)，工具栏被钳到右下角 ⇒ 窗口 right/bottom 恰好贴界。
    assert_eq!(
        anchor_probe_point(1920, 1040),
        (1919, 1039),
        "退 1px 才在 A 屏内；不退则落到右邻屏 x=1920 那一列"
    );
    // 负坐标副屏（在主屏左侧）同样成立：退 1 后仍在该屏内、仍是负的。
    let (x, y) = anchor_probe_point(0, 1040);
    assert_eq!((x, y), (-1, 1039));
    assert!(x < 0, "左侧副屏的探针点必须仍为负，否则落到主屏");
}

/// 「候选窗首显」四档表自身的自洽性：id 与档位都不得重复，且三个真实档位一个不少。
///
/// 钉的是加档/改档时的漏改：表里写重一个 id 或漏掉某一档没有编译信号，表现为
/// 「菜单里两项互相抢选中态」或「某一档点不出来」。
#[test]
fn first_show_menu_table_is_self_consistent() {
    use crate::coordinator::Coordinator;
    use std::collections::BTreeSet;
    use wind_config::app_compat::FirstShowMode as F;

    let table = Coordinator::FIRST_SHOW_MENU;
    let ids: BTreeSet<u8> = table.iter().map(|(id, _, _)| *id).collect();
    assert_eq!(ids.len(), table.len(), "菜单 id 有重复：{ids:?}");

    let modes: Vec<_> = table.iter().map(|(_, m, _)| *m).collect();
    for want in [None, Some(F::Fast), Some(F::Wait), Some(F::Instant)] {
        assert!(
            modes.contains(&want),
            "{want:?} 在菜单里点不出来（表漏了一档）"
        );
    }
    assert_eq!(modes.len(), 4, "多出了表外的档位：{modes:?}");

    // 标签非空：`M::leaf` 接受空串，空标签在菜单里是一条看不见但可点的项。
    assert!(table.iter().all(|(_, _, label)| !label.is_empty()));
}

/// 输入诊断 HUD 整套在 macOS 未实现（`ShowInputDiag` 落在 forwarder 的兜底臂），
/// 菜单里不该留一个点了没反应的项。
///
/// 也证明 `#[cfg]` 确实作用到了 `vec![]` 元素上：挂错位置（如 `M::leaf` 的实参上）照样能编过，只是不生效。
#[test]
fn input_diag_hud_menu_item_is_platform_gated() {
    use crate::coordinator::Coordinator;
    use wind_config::Config;

    let c = Coordinator::new_headless(Config::default(), None);
    fn contains(items: &[wind_ui_types::MenuItemSpec], label: &str) -> bool {
        items
            .iter()
            .any(|i| i.label == label || contains(&i.children, label))
    }
    assert_eq!(
        contains(&c.build_main_menu_items(), "输入诊断 HUD"),
        !cfg!(ext_presenter),
        "HUD 菜单项的平台门控与当前平台不符"
    );
    // 同一子菜单里的邻项必须还在——防止 cfg 把整块 vec 或相邻项一起吞掉。
    assert!(
        contains(&c.build_main_menu_items(), "密码框强制英文"),
        "邻项被误伤"
    );
}

/// 工具栏分格右键：每一格要么有定制、要么明确回落主菜单，且回落这条路不可断。
///
/// 隐藏齿轮后，右键工具栏是主菜单仅剩的鼠标入口（`toolbar-customization.md` §2.2 判据③）。
/// 用穷举而非逐个点名：`ToolbarAction` 加新变体而没被想过时会红。
#[test]
fn every_toolbar_cell_either_customizes_or_falls_back() {
    use crate::coordinator::Coordinator;
    use wind_config::Config;
    use wind_ui_types::ToolbarAction as A;

    let c = Coordinator::new_headless(Config::default(), None);
    // 有定制的格：菜单非空（空表在 `show_toolbar_menu` 里同样回落，但那是兜底，
    // 不该是这几格的常态）。
    for a in [
        A::ToggleMode,
        A::SwitchEngine,
        A::TogglePunct,
        A::ToggleWidth,
        A::ToggleS2t,
        A::ToggleSoftKeyboard,
    ] {
        let items = c
            .build_toolbar_cell_menu(a)
            .unwrap_or_else(|| panic!("{a:?} 应有定制菜单"));
        assert!(!items.is_empty(), "{a:?} 的定制菜单是空的");
    }
    // 没定制的格：必须返回 None 才会回落主菜单。
    for a in [A::OpenSettings, A::Custom(0)] {
        assert!(
            c.build_toolbar_cell_menu(a).is_none(),
            "{a:?} 不该有定制菜单——它要回落完整主菜单"
        );
    }
}

/// 每份分格菜单都必须留一条通往完整主菜单的路。
///
/// 判据③（`toolbar-customization.md` §2.2）：隐藏齿轮不会锁死用户。分格右键把功能格的右键
/// 让给了精简菜单，不补这一条，隐藏齿轮的用户就只剩 12dp 的拖动柄通向主菜单。
///
/// 上面那条穷举测试证不了这件事（它断言的是「这些格有自己的菜单」），两条一起才钉住判据③。
#[test]
fn every_cell_menu_keeps_a_way_back_to_the_main_menu() {
    use crate::coordinator::Coordinator;
    use wind_config::Config;
    use wind_ui_types::{MenuCmd, MenuKind, ToolbarAction as A};

    let c = Coordinator::new_headless(Config::default(), None);
    for a in [
        A::ToggleMode,
        A::SwitchEngine,
        A::TogglePunct,
        A::ToggleWidth,
        A::ToggleS2t,
        A::ToggleSoftKeyboard,
    ] {
        let items = c
            .build_toolbar_cell_menu(a)
            .unwrap_or_else(|| panic!("{a:?}"));
        assert!(
            items
                .iter()
                .any(|i| i.kind == MenuKind::Command(MenuCmd::OpenMainMenu)),
            "{a:?} 的分格菜单没有回主菜单的入口，隐藏齿轮后用户只剩 12dp 拖动柄"
        );
    }
}

/// 分格菜单里的开关，文案与勾选态必须与主菜单里同一个开关逐字相同；
/// 否则用户会当成两件事，或两处菜单的勾选态互相矛盾。
#[test]
fn cell_menu_labels_match_the_main_menu() {
    use crate::coordinator::Coordinator;
    use wind_config::Config;
    use wind_ui_types::ToolbarAction as A;

    let c = Coordinator::new_headless(Config::default(), None);
    let main = c.build_main_menu_items();
    let find = |items: &[wind_ui_types::MenuItemSpec], label: &str| -> Option<bool> {
        items.iter().find(|i| i.label == label).map(|i| i.checked)
    };

    let cell = c.build_toolbar_cell_menu(A::TogglePunct).expect("标点格");
    for label in ["全角", "中文标点", "简入繁出"] {
        assert_eq!(
            find(&cell, label),
            find(&main, label),
            "{label} 在分格菜单与主菜单里对不上（文案或勾选态）"
        );
    }
    // 顺序也要对齐，而按 label 查找对顺序是盲的——上面那个循环换成任意排列
    // 都照样绿。同一组开关在两处排得不一样，用户每次都得重新找。
    let order: Vec<&str> = cell
        .iter()
        .map(|i| i.label.as_str())
        .filter(|l| ["全角", "中文标点", "简入繁出"].contains(l))
        .collect();
    let main_order: Vec<&str> = main
        .iter()
        .map(|i| i.label.as_str())
        .filter(|l| ["全角", "中文标点", "简入繁出"].contains(l))
        .collect();
    assert_eq!(order, main_order, "三个开关在两处的排列顺序不一致");

    // 中英格给的是主菜单「输入方案」子菜单的原样内容（后面另挂「更多…」，见
    // `every_cell_menu_keeps_a_way_back_to_the_main_menu`，故是前缀相等而非全等）。
    let cell = c.build_toolbar_cell_menu(A::ToggleMode).expect("中英格");
    let sub = main
        .iter()
        .find(|i| i.label == "输入方案")
        .expect("主菜单缺「输入方案」");
    assert_eq!(
        cell.get(..sub.children.len()),
        Some(&sub.children[..]),
        "中英格右键与「输入方案」子菜单不同源"
    );
}

/// 状态图标开关必须同时出现在 macOS 的两棵菜单树里，且文案一致。
///
/// IMK 输入源菜单走精简树 `build_menu_items_macos()`，候选框右键与状态指示器走完整树
/// `build_main_menu_items()`，两棵树各自维护。
// 只在 macOS：Linux 的文案是「显示工具栏」，由
// `linux_panel_tests::main_menu_has_toolbar_and_softkeyboard_on_linux` 钉住。
#[cfg(target_os = "macos")]
#[test]
fn toolbar_toggle_present_in_both_macos_menu_trees() {
    use super::TOOLBAR_MENU_LABEL;
    use crate::coordinator::Coordinator;
    use wind_config::Config;

    let c = Coordinator::new_headless(Config::default(), None);
    fn contains(items: &[wind_ui_types::MenuItemSpec], label: &str) -> bool {
        items
            .iter()
            .any(|i| i.label == label || contains(&i.children, label))
    }
    assert!(
        contains(&c.build_menu_items_macos(), TOOLBAR_MENU_LABEL),
        "IMK 精简菜单缺状态图标开关——关掉图标后开关本身也没了（入口自锁）"
    );
    assert!(
        contains(&c.build_main_menu_items(), TOOLBAR_MENU_LABEL),
        "完整菜单缺状态图标开关"
    );
    // 文案必须是 macOS 语义：这里显隐的是 NSStatusItem，不是 Windows 的悬浮工具栏。
    assert_eq!(TOOLBAR_MENU_LABEL, "显示状态图标");
}

/// 勾选态必须跟随 `toolbar_visible`，否则菜单上是个永远不打勾的死开关。
#[cfg(ext_presenter)]
#[test]
fn toolbar_toggle_reflects_visibility_in_imk_menu() {
    use super::TOOLBAR_MENU_LABEL;
    use crate::coordinator::Coordinator;
    use wind_config::Config;

    let c = Coordinator::new_headless(Config::default(), None);
    let checked = |c: &Coordinator| {
        c.build_menu_items_macos()
            .into_iter()
            .find(|i| i.label == TOOLBAR_MENU_LABEL)
            .expect("菜单项不存在")
            .checked
    };
    let before = checked(&c);
    c.state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .toolbar_visible = !before;
    assert_eq!(checked(&c), !before, "勾选态没跟随 toolbar_visible");
}

/// 只有恰好 (0,0) 被规避，其余坐标（含含 0 分量与负坐标）必须原样落盘。
#[test]
fn only_the_exact_sentinel_is_nudged() {
    assert_eq!(avoid_unset_sentinel(0, 0), (0, 1), "撞哨兵 → 下移 1px");
    // 含 0 分量但非哨兵：不能动，否则用户贴左边/贴顶边的位置会被悄悄改掉
    assert_eq!(avoid_unset_sentinel(0, 5), (0, 5));
    assert_eq!(avoid_unset_sentinel(5, 0), (5, 0));
    // 负坐标：副屏位于主屏左侧/上方时屏幕坐标为负，属合法值
    assert_eq!(avoid_unset_sentinel(-1920, -100), (-1920, -100));
    assert_eq!(avoid_unset_sentinel(100, 200), (100, 200));
}

/// 规避结果自身绝不能再是哨兵，否则等于没修。
#[test]
fn nudged_result_is_never_the_sentinel() {
    assert_ne!(avoid_unset_sentinel(0, 0), (0, 0));
}

#[test]
fn settings_args_skip_empty_and_quote_whitespace() {
    use super::build_settings_args;
    assert_eq!(build_settings_args(&[]), "");
    assert_eq!(build_settings_args(&[("schema", "")]), "", "空值整项跳过");
    assert_eq!(
        build_settings_args(&[("schema", "wubi86"), ("type", "shadow")]),
        "--schema=wubi86 --type=shadow"
    );
    assert_eq!(
        build_settings_args(&[("text", "a b")]),
        "--text=\"a b\"",
        "含空白必须加引号，否则会被 CommandLineToArgvW 拆成两个 argv"
    );
}

/// `build_settings_args` 加的引号必须能被 `settings_argv` 原样还原——两者是一对，
/// 任一侧单独改都会让含空白的值（如加词的 `--text=你 好`）在 macOS 上被拆成两个参数。
#[test]
fn settings_argv_round_trips_quoting() {
    use super::{build_settings_args, settings_argv};
    assert_eq!(settings_argv(Some("dict"), ""), vec!["--page=dict"]);
    assert_eq!(
        settings_argv(Some("dict"), &build_settings_args(&[("schema", "wubi86")])),
        vec!["--page=dict", "--schema=wubi86"]
    );
    // 含空白的值：加引号 → 切词后必须仍是一个 argv，且引号已剥掉。
    assert_eq!(
        settings_argv(
            Some("add-word"),
            &build_settings_args(&[("text", "你 好"), ("code", "nihao")])
        ),
        vec!["--page=add-word", "--text=你 好", "--code=nihao"]
    );
    // 无页 + 无参数 → 空 argv（设置端按默认页处理）。
    assert!(settings_argv(None, "").is_empty());
    // 无页但有参数（`--dark` 这类）：参数不依附于页，须原样带上。
    assert_eq!(settings_argv(None, "--dark"), vec!["--dark"]);
}

/// 附加参数不依附于页：没给页也要原样带上（`--dark`/`--soft` 无页也有意义）。
#[cfg(not(ext_presenter))]
#[test]
fn settings_cmdline_keeps_extra_without_page() {
    use super::settings_cmdline;
    assert_eq!(settings_cmdline(None, ""), "");
    assert_eq!(settings_cmdline(Some("dict"), ""), "--page dict");
    assert_eq!(
        settings_cmdline(Some("dict"), "--schema=wubi86 --type=shadow"),
        "--page dict --schema=wubi86 --type=shadow"
    );
    assert_eq!(
        settings_cmdline(None, "--dark"),
        "--dark",
        "无页时附加参数不得被丢弃"
    );
}
