//! 只测端到端测不到的那几件：配置默认、编码栏标识、自动隐藏计时、UI 下发。
//!
//! 按键行为一律不在这里测——联想候选住在 `state.candidates` 里，翻页/高亮/
//! 二三候选/数字选词走的都是主输入路的既有分支，验它们必须有真实词库，
//! 那组在 `tests/assoc_end_to_end.rs`。
use super::*;
use std::sync::Arc;
use wind_config::Config;

/// 智能联想档 + 标点源（标点源不依赖词库，headless 下也出得来）。
fn coord_smart() -> Arc<Coordinator> {
    coord_with(|_| {})
}

/// 同上，但可以再改几项配置。
///
/// 显式开 `punct`（桌面基线是关的）：headless 下唯一不依赖词库的联想源就是标点，不开的话
/// 大多用例测的是空候选、静默失去意义。桌面出厂不出标点由 wind-assoc 的
/// `desktop_smart_yields_no_punct_by_default` 钉。
fn coord_with(tweak: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = "smart".to_string();
    cfg.input.association.punct = true;
    tweak(&mut cfg);
    Coordinator::new_headless(cfg, None)
}

/// 表里统计出的标点（与旧表的 `$`）不占头名：头名留给词，标点最早排第二。
#[test]
fn bigram_punct_successor_never_leads() {
    let table = wind_assoc::bigram::AssocBigram::parse("吗\t？ ！ 的 呢\n好的\t$ 我 谢谢\n".into())
        .unwrap();
    let src = BigramWords { table: &table };
    let shown = |text: &str| -> Vec<String> {
        let ctx = AssocContext {
            text,
            preceding: "",
            char_wise: false,
            boundary_broken: false,
        };
        src.suggest(&ctx, 9).into_iter().map(|h| h.text).collect()
    };
    assert_eq!(shown("吗"), ["的", "？", "！", "呢"]);
    assert_eq!(shown("好的"), ["我", "，", "谢谢"]);
}

/// 下一词表只在用得上的档位读进内存：词语联想档（PC 常开的那档）不碰它。
/// 看的是 `OnceLock` 有没有被初始化过——无数据文件时初始化成 `None` 也算碰过。
#[test]
fn bigram_table_loads_only_when_smart() {
    let word = coord_with(|c| c.input.association.kind = "word".to_string());
    word.assoc_hits(&word.assoc_config(), "", "中", false);
    assert!(word.assoc_bigram.get().is_none(), "词语联想档读了下一词表");

    let smart = coord_smart();
    smart.assoc_hits(&smart.assoc_config(), "", "中", false);
    assert!(smart.assoc_bigram.get().is_some());
}

/// 下一词表不在按键线程上读：没预热时第一次取一定是暂缺（同时派出后台加载），
/// 之后由后台线程装好。
#[test]
fn bigram_table_loads_off_key_thread() {
    let dir = std::env::temp_dir().join(format!("wind_assoc_bigram_bg_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assoc")).unwrap();
    std::fs::write(dir.join("assoc/assoc_bigram.txt"), "中\t国 文\n").unwrap();
    let mut cfg = Config::default();
    cfg.input.association.kind = "smart".to_string();
    let c = Coordinator::new_headless(cfg, Some(&dir));
    assert!(c.assoc_bigram_path.is_some(), "夹具的下一词表没被解析到");

    assert!(c.assoc_bigram().is_none(), "按键路径同步读了下一词表");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while c.assoc_bigram.get().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(c.assoc_bigram().is_some(), "后台加载没有装好下一词表");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preceding_text_follows_host_edits() {
    use wind_host::EditOp;
    let c = coord_with(|_| {});
    c.set_preceding_text("你好，我们");
    c.note_host_edits(&[EditOp::Commit("今".into())]);
    assert_eq!(c.preceding_text(), "你好，我们今");
    c.note_host_edits(&[EditOp::DeleteBackward { count: 2 }]);
    assert_eq!(c.preceding_text(), "你好，我");
    c.note_host_edits(&[EditOp::ReplaceBackward {
        count: 1,
        text: ".".into(),
    }]);
    assert_eq!(c.preceding_text(), "你好，.");
    c.note_host_edits(&[EditOp::MoveCursor { delta: -1 }]);
    assert_eq!(c.preceding_text(), "", "挪了光标算不出前文，等宿主报");
    // 只留末尾若干字
    c.set_preceding_text(&"字".repeat(PRECEDING_KEEP_CHARS + 5));
    assert_eq!(c.preceding_text().chars().count(), PRECEDING_KEEP_CHARS);
    c.note_host_edits(&[EditOp::Commit("尾".into())]);
    assert_eq!(c.preceding_text().chars().count(), PRECEDING_KEEP_CHARS);
    assert!(c.preceding_text().ends_with('尾'));
}

fn enter(c: &Coordinator, text: &str) -> bool {
    let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
    c.maybe_enter_assoc(&mut st, text, "")
}

fn texts(c: &Coordinator) -> Vec<String> {
    c.debug_assoc_texts()
}

/// 联想态挂的占位组合跟随焦点应用的 compat 规则——Edge（规则 zwsp）下是 ZWSP，
/// 未配规则的应用仍是空格。变异检验：`assoc_composition` 改回常量空格 ⇒ 红。
#[test]
fn assoc_placeholder_follows_the_focused_apps_rule() {
    fn placeholder_of(act: KeyAction) -> String {
        match act {
            KeyAction::CommitThenDeferComposition {
                deferred_composition,
                ..
            } => deferred_composition,
            KeyAction::InsertText {
                new_composition: Some(c),
                ..
            } => c,
            other => panic!("进联想态应上屏并重开占位组合，实得 {other:?}"),
        }
    }
    let c = coord_smart();
    c.test_focus_zwsp_app(42, "msedge.exe");
    let act = {
        let mut st = c.state.lock().unwrap();
        c.auto_commit_then_assoc(&mut st, "你好".into(), "你好")
    };
    assert!(c.state.lock().unwrap().assoc_active(), "前提：进了联想态");
    assert_eq!(
        placeholder_of(act),
        "\u{200B}",
        "Edge 下联想占位应是零宽空格"
    );

    // 对照：焦点换成未配规则的进程 ⇒ 空格。
    let c = coord_smart();
    c.test_focus_zwsp_app(42, "msedge.exe");
    c.active_compat.lock().unwrap().pid = 43;
    c.pid_names.lock().unwrap().insert(43, "wps.exe".into());
    c.refresh_active_compat_lookups();
    let act = {
        let mut st = c.state.lock().unwrap();
        c.auto_commit_then_assoc(&mut st, "你好".into(), "你好")
    };
    assert_eq!(placeholder_of(act), " ", "未配规则的应用保持空格");
}

/// `assoc_placeholder` 只改联想态：规则里 composition_placeholder=zwsp、assoc_placeholder=blank
/// 时，联想占位是盲文空白，而打字占位（`composition_placeholder()`）仍是 ZWSP。
/// 变异检验：`assoc_composition` 改回 `composition_placeholder()` ⇒ 红。
#[test]
fn assoc_placeholder_overrides_only_the_assoc_state() {
    use wind_config::app_compat::{AppCompat, AppCompatRule, AssocPlaceholder, PlaceholderChar};
    let c = coord_smart();
    c.test_set_app_compat(AppCompat::from_rules(vec![AppCompatRule {
        process: "x.exe".into(),
        composition_placeholder: Some(PlaceholderChar::Zwsp),
        assoc_placeholder: Some(AssocPlaceholder::Blank),
        ..Default::default()
    }]));
    c.pid_names.lock().unwrap().insert(7, "x.exe".into());
    c.active_compat.lock().unwrap().pid = 7;
    c.refresh_active_compat_lookups();
    assert_eq!(
        c.assoc_composition(),
        "\u{2800}",
        "联想态用 assoc_placeholder"
    );
    assert_eq!(c.composition_placeholder(), "\u{200B}", "打字占位不受影响");
}

/// 无占位联想下用户挪动光标（点单元格）⇒ 立刻退出联想；自提交回声不算；有占位时不走此路。
/// 变异检验：去掉 `handle_selection_changed` 里的退出分支 ⇒ 第一段红。
/// 命令输出 / 软键盘点击 / 悬停提示插进来的不是联想的下文：收掉无占位联想（打了自提交点，
/// 光标回声不会再顺带收）。鼠标选词是联想自己的出口，不在此列。
#[test]
fn non_input_commits_exit_bare_assoc() {
    use crate::session_exit::ActionOrigin;
    use wind_config::app_compat::{AppCompat, AppCompatRule, AssocPlaceholder};
    for (origin, expect_exit) in [
        (ActionOrigin::Command, true),
        (ActionOrigin::SoftKeyboardClick, true),
        (ActionOrigin::Tooltip, true),
        (ActionOrigin::Pointer, false),
    ] {
        let c = coord_smart();
        c.test_set_app_compat(AppCompat::from_rules(vec![AppCompatRule {
            process: "x.exe".into(),
            assoc_placeholder: Some(AssocPlaceholder::None),
            ..Default::default()
        }]));
        c.pid_names.lock().unwrap().insert(7, "x.exe".into());
        c.active_compat.lock().unwrap().pid = 7;
        c.refresh_active_compat_lookups();
        {
            let mut st = c.state.lock().unwrap();
            c.auto_commit_then_assoc(&mut st, "你好".into(), "你好");
            assert!(st.assoc_active(), "前提：进了联想态");
        }
        let st = c.state.lock().unwrap();
        let _ = c.finish_session_action(st, origin, Coordinator::commit_action("★".into(), true));
        assert_eq!(
            !c.state.lock().unwrap().assoc_active(),
            expect_exit,
            "{origin:?}"
        );
    }
}

#[test]
fn bare_assoc_exits_when_user_moves_the_caret() {
    use wind_bridge::handler::MessageHandler;
    use wind_config::app_compat::{AppCompat, AppCompatRule, AssocPlaceholder};
    for (placeholder, expect_exit) in [
        (AssocPlaceholder::None, true),
        (AssocPlaceholder::Space, false),
    ] {
        let c = coord_smart();
        c.test_set_app_compat(AppCompat::from_rules(vec![AppCompatRule {
            process: "x.exe".into(),
            assoc_placeholder: Some(placeholder),
            ..Default::default()
        }]));
        c.pid_names.lock().unwrap().insert(7, "x.exe".into());
        c.active_compat.lock().unwrap().pid = 7;
        c.refresh_active_compat_lookups();
        {
            let mut st = c.state.lock().unwrap();
            c.auto_commit_then_assoc(&mut st, "你好".into(), "你好");
            assert!(st.assoc_active(), "前提：进了联想态");
        }
        // 刚自提交：回声，联想不动。
        *c.last_self_commit.lock().unwrap() = Some(std::time::Instant::now());
        c.handle_selection_changed(0);
        assert!(
            c.state.lock().unwrap().assoc_active(),
            "{placeholder:?}：回声不该退联想"
        );
        // 久远的自提交 ⇒ 用户移动。
        *c.last_self_commit.lock().unwrap() = None;
        c.handle_selection_changed(0);
        assert_eq!(
            !c.state.lock().unwrap().assoc_active(),
            expect_exit,
            "{placeholder:?}：用户移动光标后联想是否退出"
        );
    }
}

/// 无占位联想里 Del 落到透传、被改判成「收口 + 交还」而退出联想后，会话位缓存必须对齐成 FALSE，
/// 否则 DLL 侧的位只能靠本地清位兜底。
/// 变异检验：去掉 `handle_key_event_policed` 里的对账推送 ⇒ 红。
#[test]
fn bare_assoc_released_by_passthrough_realigns_the_session_cache() {
    use std::sync::atomic::Ordering;
    use wind_bridge::handler::{KeyEventData, MessageHandler};
    use wind_config::app_compat::{AppCompat, AppCompatRule, AssocPlaceholder};
    let c = coord_smart();
    c.test_set_app_compat(AppCompat::from_rules(vec![AppCompatRule {
        process: "x.exe".into(),
        assoc_placeholder: Some(AssocPlaceholder::None),
        ..Default::default()
    }]));
    c.pid_names.lock().unwrap().insert(7, "x.exe".into());
    c.active_compat.lock().unwrap().pid = 7;
    c.refresh_active_compat_lookups();
    {
        let mut st = c.state.lock().unwrap();
        c.auto_commit_then_assoc(&mut st, "你好".into(), "你好");
        assert!(st.assoc_active(), "前提：进了联想态");
    }
    c.last_pushed_hotkey_session.store(true, Ordering::Relaxed); // 进联想时已推过 TRUE
    let del = KeyEventData {
        key_code: 0x2E,
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 1,
        prev_char: 0,
        seq_char: 0,
    };
    let act = c.handle_key_event_policed(&del);
    assert!(
        matches!(act, KeyAction::ClearCompositionThenPassThrough),
        "Del 应被改判成收口+交还，实得 {act:?}"
    );
    assert!(!c.state.lock().unwrap().assoc_active(), "联想应已退出");
    assert!(
        !c.last_pushed_hotkey_session.load(Ordering::Relaxed),
        "退出联想后会话位缓存必须对齐成 FALSE（即已推过 FALSE）"
    );
}

/// `assoc_placeholder = "none"`：联想态不挂占位组合——`assoc_composition()` 为空串，
/// 上屏回的是不带新组合的 `InsertText`；会话判据 `hotkey_session_now()` 与
/// `assoc_session_hint()` 为真，退出联想后两者回落。
/// 变异检验：`hotkey_session_now` 去掉 `assoc_bare_session_now` 一支 ⇒ 红。
#[test]
fn assoc_none_mounts_no_placeholder_but_keeps_the_session_flag() {
    use wind_bridge::handler::MessageHandler;
    use wind_config::app_compat::{AppCompat, AppCompatRule, AssocPlaceholder};
    let c = coord_smart();
    c.test_set_app_compat(AppCompat::from_rules(vec![AppCompatRule {
        process: "x.exe".into(),
        assoc_placeholder: Some(AssocPlaceholder::None),
        ..Default::default()
    }]));
    c.pid_names.lock().unwrap().insert(7, "x.exe".into());
    c.active_compat.lock().unwrap().pid = 7;
    c.refresh_active_compat_lookups();
    assert_eq!(c.assoc_composition(), "");
    assert!(!c.hotkey_session_now(), "前提：尚未进联想");
    let act = {
        let mut st = c.state.lock().unwrap();
        c.auto_commit_then_assoc(&mut st, "你好".into(), "你好")
    };
    assert!(c.state.lock().unwrap().assoc_active(), "前提：进了联想态");
    match act {
        KeyAction::InsertText {
            new_composition,
            has_new_composition,
            ..
        } => assert!(new_composition.is_none() && !has_new_composition),
        other => panic!("none 时应是不带新组合的上屏，实得 {other:?}"),
    }
    assert!(
        c.hotkey_session_now(),
        "联想会话要经 STATUS_HOTKEY_SESSION 告知 DLL"
    );
    assert!(c.assoc_session_hint(), "上屏应答要带 ASSOC_SESSION 标志");
    {
        let mut st = c.state.lock().unwrap();
        c.exit_assoc(&mut st, AssocExit::Timeout);
    }
    assert!(
        !c.hotkey_session_now() && !c.assoc_session_hint(),
        "退出后回落"
    );
}

/// 无占位联想（`none`）时，宿主上屏后上报的远处光标只落缓存、不重新锚定候选窗
/// （否则 WPS 下联想窗会飘到远处）；有占位组合时照常走显示决策。
/// 变异检验：去掉 `message_handler` 里的 `bare_assoc` 判断 ⇒ 第一段红。
#[test]
fn bare_assoc_ignores_far_caret_reports_for_anchoring() {
    use std::sync::atomic::Ordering;
    use wind_bridge::handler::{CaretData, MessageHandler};
    use wind_config::app_compat::{AppCompat, AppCompatRule, AssocPlaceholder};
    let far = CaretData {
        x: 1643,
        y: 756,
        height: 20,
        composition_start_x: 0,
        composition_start_y: 0,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    };
    for (placeholder, expect_cache_only) in [
        (AssocPlaceholder::None, true),
        (AssocPlaceholder::Space, false),
    ] {
        let c = coord_smart();
        c.test_set_app_compat(AppCompat::from_rules(vec![AppCompatRule {
            process: "x.exe".into(),
            assoc_placeholder: Some(placeholder),
            ..Default::default()
        }]));
        c.pid_names.lock().unwrap().insert(7, "x.exe".into());
        c.active_compat.lock().unwrap().pid = 7;
        c.refresh_active_compat_lookups();
        {
            let mut st = c.state.lock().unwrap();
            c.auto_commit_then_assoc(&mut st, "你好".into(), "你好");
            assert!(st.assoc_active(), "前提：进了联想态");
        }
        c.caret_cache_is_idle_report.store(false, Ordering::Relaxed);
        c.handle_caret_update(&far);
        assert_eq!(
            c.caret_cache_is_idle_report.load(Ordering::Relaxed),
            expect_cache_only,
            "{placeholder:?}：远处光标上报是否只落缓存"
        );
    }
}

/// 桌面默认关，而且是靠基线段本身就是 `"off"`——不再有哨兵值参与。
///
/// 断言前提值本身：`[input.association]` 一旦又被写成某个平台哨兵，这里立刻报出来。
/// 平台差异的唯一落点是 `[mobile.association]`，桌面构建根本不读它
/// （由 `InputProfile::config_overlay` 守着，见 wind-assoc 的 `mobile_section_switches_*`）。
#[test]
fn desktop_defaults_to_off() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    assert_eq!(cfg.input.association.kind, "off", "前提：桌面基线就是 off");
    assert_eq!(
        cfg.mobile.association.kind, "smart",
        "反向对照：移动端段确实是另一个值，否则本测试测不出任何东西"
    );
    let c = Coordinator::new_headless(cfg, None);
    assert!(!enter(&c, "你好"), "桌面不该进联想");
}

/// 反向对照：显式开启后确实进得去。少了它，上面那条可能只是因为联想压根不工作。
#[test]
fn smart_kind_enters() {
    let c = coord_smart();
    assert!(enter(&c, "你好"));
    assert_eq!(texts(&c)[0], "，");
}

/// 联想候选必须带上 `Assoc` 来源——那是它唯一的身份标记，词频/造词/调序全靠它跳过。
#[test]
fn candidates_carry_assoc_source() {
    let c = coord_smart();
    assert!(enter(&c, "你好"));
    let st = c.state.lock().unwrap();
    assert!(
        st.candidates
            .iter()
            .all(|x| x.source == CandidateSource::Assoc),
        "整批候选都该标成 Assoc，否则「是不是联想」的判据会时真时假"
    );
    assert!(st.assoc_active());
}

/// 普通候选不该被认成联想（`assoc_active` 的反向对照）。
#[test]
fn normal_candidates_are_not_assoc() {
    let c = coord_smart();
    {
        let mut st = c.state.lock().unwrap();
        st.candidates = vec![Candidate {
            text: "普通".into(),
            source: CandidateSource::Pinyin,
            ..Default::default()
        }];
    }
    assert!(!c.state.lock().unwrap().assoc_active());
    assert!(texts(&c).is_empty());
}

/// 非嵌入模式：编码栏标识进联想时填上、退出时随候选一并清掉。
///
/// 它借用 `state.preedit`——那个字段平时装真实编码，清理漏了就会留到下一次打字。
#[test]
fn hint_fills_and_clears_when_not_inline() {
    let c = coord_with(|cfg| {
        cfg.ui.candidate.preedit_display = "candidate_top".to_string();
    });
    assert!(enter(&c, "你好"));
    assert_eq!(c.state.lock().unwrap().preedit, "联想输入");
    {
        let mut st = c.state.lock().unwrap();
        c.exit_assoc(&mut st, AssocExit::Dismiss);
    }
    assert!(
        c.state.lock().unwrap().preedit.is_empty(),
        "退出必须把标识清掉"
    );
}

/// 嵌入模式下不给标识：候选窗本来没有编码栏，联想自动弹出时凭空多一栏会让窗口变高又变矮。
///
/// `app_inline` 是出厂默认，所以这条覆盖的是绝大多数用户的实际路径。
#[test]
fn no_hint_when_inline() {
    let c = coord_smart(); // Config::default() ⇒ preedit_display = app_inline
    assert!(enter(&c, "你好"), "前提：进了联想态");
    assert!(
        c.state.lock().unwrap().preedit.is_empty(),
        "嵌入模式不该给编码栏标识——那会让候选窗高度一跳"
    );
}

/// 空标识 = 不显示（用户可以关掉它）。
#[test]
fn empty_hint_shows_nothing() {
    let c = coord_with(|cfg| {
        cfg.ui.candidate.preedit_display = "candidate_top".to_string();
        cfg.input.association.hint = String::new();
    });
    assert!(enter(&c, "你好"));
    assert!(c.state.lock().unwrap().preedit.is_empty());
}

/// 自动隐藏计时到期即收窗。
#[test]
fn hide_timer_closes_the_window() {
    let c = coord_smart();
    assert!(enter(&c, "你好"));
    let t = *c.assoc_hide_token.lock().unwrap();
    c.fire_assoc_hide(t);
    assert!(texts(&c).is_empty(), "计时到期该收窗");
    assert!(c.state.lock().unwrap().preedit.is_empty(), "标识一并清掉");
}

/// 退出联想态必须作废未触发的计时。
///
/// 不作废的话它会在下一轮联想里提前把窗收掉（两次上屏间隔短于 `hide_after_ms` 时）。
#[test]
fn exiting_invalidates_pending_hide_timer() {
    let c = coord_smart();
    assert!(enter(&c, "你好"));
    let t1 = *c.assoc_hide_token.lock().unwrap();
    {
        let mut st = c.state.lock().unwrap();
        c.exit_assoc(&mut st, AssocExit::Dismiss);
    }
    assert_ne!(
        t1,
        *c.assoc_hide_token.lock().unwrap(),
        "退出时须 bump 令牌"
    );
    assert!(enter(&c, "你好"), "重新进入");
    c.fire_assoc_hide(t1); // 陈旧令牌
    assert!(!texts(&c).is_empty(), "陈旧计时不得收掉新一轮的联想窗");
}

/// 操作联想候选期间（hold）计时作废：到期回调拿的是旧令牌，收不掉窗。
#[test]
fn hold_suspends_hide_timer() {
    let c = coord_smart();
    assert!(enter(&c, "你好"));
    let t = *c.assoc_hide_token.lock().unwrap();
    c.assoc_hold(true);
    c.fire_assoc_hide(t);
    assert!(!texts(&c).is_empty(), "按住候选条期间计时不得收窗");
}

/// 松手后重新起计时：新令牌到期照常收窗，旧令牌仍无效。
#[test]
fn release_rearms_hide_timer() {
    let c = coord_smart();
    assert!(enter(&c, "你好"));
    let t0 = *c.assoc_hide_token.lock().unwrap();
    c.assoc_hold(true);
    c.assoc_hold(false);
    let t1 = *c.assoc_hide_token.lock().unwrap();
    assert_ne!(t0, t1, "松手须重新起计时");
    c.fire_assoc_hide(t0);
    assert!(!texts(&c).is_empty(), "旧计时作废");
    c.fire_assoc_hide(t1);
    assert!(texts(&c).is_empty(), "松手后重新计满到期照常收窗");
}

/// 不在联想态时 hold 什么也不做：不动令牌（不误伤之后那一轮联想的计时）。
#[test]
fn hold_outside_assoc_is_noop() {
    let c = coord_smart();
    let t = *c.assoc_hide_token.lock().unwrap();
    c.assoc_hold(true);
    c.assoc_hold(false);
    assert_eq!(t, *c.assoc_hide_token.lock().unwrap());
}

/// `hide_after_ms = 0` = 不自动隐藏。
#[test]
fn zero_hide_after_ms_keeps_it_open() {
    let c = coord_with(|cfg| cfg.input.association.hide_after_ms = 0);
    assert!(enter(&c, "你好"));
    assert!(!texts(&c).is_empty());
}

/// 超时收窗必须留下「宿主组合成了孤儿」的标记。
///
/// 超时收窗后 TSF 侧仍是 `composing=1 inputSession=1`，下一键被 DLL 吃下转发，服务端已
/// 回落 `PassThrough`，「吃了再吐」时不补发 `WM_KEYDOWN` 的宿主直接丢键。
#[test]
fn timeout_leaves_placeholder_orphaned() {
    use std::sync::atomic::Ordering;
    let c = coord_smart();
    assert!(enter(&c, "你好"));
    assert!(
        !c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
        "刚进联想态时组合有主，不该有孤儿标记"
    );
    let t = *c.assoc_hide_token.lock().unwrap();
    c.fire_assoc_hide(t);
    assert!(
        c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
        "超时收窗没有按键应答可搭载收口动作，必须留标记等下一次按键"
    );
}

/// 反向对照：按键路径退出联想不留标记——它自己就带着收口动作回宿主。
///
/// 少了这条，上一条测试在「一进联想态就置位」这种接错线下照样绿。
#[test]
fn key_path_dismiss_leaves_no_orphan() {
    use std::sync::atomic::Ordering;
    let c = coord_smart();
    assert!(enter(&c, "你好"));
    let action = {
        let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
        c.assoc_enter(&mut st)
    };
    assert!(
        matches!(
            action,
            KeyAction::ClearComposition | KeyAction::ClearCompositionThenPassThrough
        ),
        "按键路径的收口动作搭着这次应答就送到宿主了"
    );
    assert!(!c.assoc_placeholder_orphaned.load(Ordering::Relaxed));
}

/// 联想接龙时候选窗不能钉在第一次联想的位置：窗口已在显示时进联想要解开组合起点的两把锁，
/// 下一帧宿主上报的新起点才锁得上、reshow 才跟得过去。
#[test]
fn entering_assoc_unpins_composition_anchor_when_window_is_shown() {
    let c = coord_smart();
    c.debug_lock_anchors(true);
    assert_eq!(c.debug_anchor_locks(), (true, true), "前提：两把锁都锁着");
    assert!(enter(&c, "你好"));
    assert_eq!(c.debug_anchor_locks(), (false, false));
}

/// 反向对照：窗口没在显示（首显流程）时不解锁——那是 `reset_first_show` 的地盘，
/// 本帧刚到的组合起点可能正是首显要用的坐标。
#[test]
fn entering_assoc_keeps_anchor_when_window_is_not_shown() {
    let c = coord_smart();
    c.debug_lock_anchors(false);
    assert!(enter(&c, "你好"));
    assert_eq!(c.debug_anchor_locks(), (true, true));
}

/// 联想态下越界数字键：收掉联想、只出该数字，与 `keys.overflow.number_key` 无关
/// 三档都测：`ignore` 会吞键、`commit*` 会把联想首项顶上屏，都是错的。
#[test]
fn overflow_digit_exits_assoc_and_emits_only_the_digit() {
    for behavior in ["ignore", "commit", "commit_and_input", "clear_and_input"] {
        let c = coord_with(|cfg| cfg.keys.overflow.number_key = behavior.to_string());
        assert!(enter(&c, "你好"), "{behavior}: 应进入联想态");
        let act = {
            let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
            assert!(st.candidates.len() < 9, "前提：数字 9 必须越界");
            c.handle_number_key_select(&mut st, 9)
        };
        match act {
            KeyAction::InsertText { text, .. } => {
                assert_eq!(text, "9", "{behavior}: 只该出数字，不带联想首项")
            }
            other => panic!("{behavior}: 应 InsertText(\"9\")，实际: {other:?}"),
        }
        let st = c.state.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!st.assoc_active(), "{behavior}: 联想应已收掉");
    }
}

/// 孤儿在场时透传改判为「收组合 + 交还按键」——本修复的核心那一格。
#[test]
fn orphan_rewrites_passthrough_into_clear_then_pass() {
    use std::sync::atomic::Ordering;
    let c = coord_smart();
    c.assoc_placeholder_orphaned.store(true, Ordering::Relaxed);
    assert!(matches!(
        c.adopt_orphaned_placeholder(KeyAction::PassThrough),
        KeyAction::ClearCompositionThenPassThrough
    ));
    assert!(
        !c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
        "收口一次即撤标记"
    );
    assert!(
        matches!(
            c.adopt_orphaned_placeholder(KeyAction::PassThrough),
            KeyAction::PassThrough
        ),
        "标记已撤，后续透传不得再被改判——否则每个透传键都白挨一次收口 + 重放"
    );
}

/// 没有孤儿时不得改判。
#[test]
fn passthrough_untouched_without_orphan() {
    let c = coord_smart();
    assert!(matches!(
        c.adopt_orphaned_placeholder(KeyAction::PassThrough),
        KeyAction::PassThrough
    ));
}

/// 不碰宿主组合的动作必须把标记留着。
///
/// 「任何按键都撤标记」会让超时后先按热键的用户永远收不掉组合：`StatusUpdate` 不结束组合
/// （见 `schema_switch_key_action`），`MoveCursorRight` / `DeletePair` 在 C++ 侧也不碰组合。
#[test]
fn non_composition_actions_keep_the_orphan() {
    use std::sync::atomic::Ordering;
    let c = coord_smart();
    for action in [
        KeyAction::StatusUpdate(c.build_status()),
        KeyAction::Consumed,
        KeyAction::MoveCursorRight { count: 1 },
        KeyAction::DeletePair,
    ] {
        c.assoc_placeholder_orphaned.store(true, Ordering::Relaxed);
        let out = c.adopt_orphaned_placeholder(action);
        assert!(
            c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
            "{out:?} 不碰宿主组合，孤儿还在，标记必须留到下一次按键"
        );
    }
}

/// 会结束或替换组合的动作顺带收掉孤儿：动作原样发出，标记撤掉。
#[test]
fn composition_absorbing_actions_clear_the_orphan() {
    use std::sync::atomic::Ordering;
    let c = coord_smart();
    for action in [
        KeyAction::ClearComposition,
        KeyAction::UpdateComposition {
            text: "a".to_string(),
            caret_pos: 1,
        },
        KeyAction::InsertText {
            text: "你".to_string(),
            new_composition: None,
            mode_changed: false,
            chinese_mode: true,
            has_new_composition: false,
        },
    ] {
        c.assoc_placeholder_orphaned.store(true, Ordering::Relaxed);
        let out = c.adopt_orphaned_placeholder(action);
        assert!(
            !c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
            "{out:?} 到了宿主那边会结束或替换组合，孤儿已被顺带收掉"
        );
    }
}

/// 联想候选真的被推到了候选窗上——本功能可见的那一半。
///
/// 预置 caret 绕开首显闸门：headless 无宿主坐标，首帧会被拦下不下发。
#[test]
fn candidates_reach_the_candidate_window() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = "smart".to_string();
    // 桌面基线里 punct 是关的；headless 下它是唯一不依赖词库的源，不开就没候选。
    cfg.input.association.punct = true;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
    c.debug_mark_coords_ready();
    {
        let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
        assert!(c.maybe_enter_assoc(&mut st, "你好", ""));
        c.notify_ui_update(&st);
    }
    let mut last: Option<Vec<String>> = None;
    while let Ok(cmd) = rx.try_recv() {
        if let wind_ui_types::UiCommand::UpdateCandidates { candidates, .. } = cmd {
            last = Some(candidates.into_iter().map(|i| i.text).collect());
        }
    }
    let items = last.expect("联想态该下发 UpdateCandidates");
    assert_eq!(items.first().map(String::as_str), Some("，"));
}

/// 联想态不画模式标记：候选旁边挂个「拼」会让用户以为自己还在打字。
///
/// 不需要为联想加判据，「有候选即隐藏标记」本就适用；加 `|| assoc_active()` 会让
/// 「空则隐藏」守卫在联想态成立、把整个候选窗收掉。
#[test]
fn no_mode_label_in_assoc() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = "smart".to_string();
    // 桌面基线里 punct 是关的；headless 下它是唯一不依赖词库的源，不开就没候选。
    cfg.input.association.punct = true;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
    c.debug_mark_coords_ready();
    {
        let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
        assert!(c.maybe_enter_assoc(&mut st, "你好", ""));
        c.notify_ui_update(&st);
    }
    let mut label = None;
    while let Ok(cmd) = rx.try_recv() {
        if let wind_ui_types::UiCommand::UpdateCandidates { mode_label, .. } = cmd {
            label = Some(mode_label);
        }
    }
    assert_eq!(label.as_deref(), Some(""), "联想态不该显示模式标记");
}

/// 数字进编码只在「移动端 + 英文 + 组码中」三者齐备时成立，缺一不可
#[test]
fn digit_enters_buffer_only_on_mobile_english_composing() {
    assert!(digit_enters_english_buffer(true, true, true));
    assert!(
        !digit_enters_english_buffer(false, true, true),
        "桌面数字照旧选词"
    );
    assert!(
        !digit_enters_english_buffer(true, false, true),
        "没在组码：数字原样出"
    );
    assert!(
        !digit_enters_english_buffer(true, true, false),
        "中文方案：顶屏 + 数字"
    );
}
