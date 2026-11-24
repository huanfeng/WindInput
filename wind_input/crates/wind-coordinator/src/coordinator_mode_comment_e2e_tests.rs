//! 各作用范围的注释分配走到发往 UI 的候选上——`comment_template_for` 的单元测试证明不了
//! 消费端接上了它。
//!
//! 注释段在发送路径上算、不回写 `state.candidates`，故这里收 UI 通道断言。放在 crate 内
//! 而非 tests/ 下，是因为要预置 caret 绕过首显闸门（见 `ready_coords_bypass_first_show_wait`）。
use super::*;

/// 造协调器并把坐标预置成「已就绪」，使候选能立即下发。
fn coord_with_ui(cfg: Config) -> (Arc<Coordinator>, std::sync::mpsc::Receiver<UiCommand>) {
    coord_with_ui_at(cfg, None)
}

/// 同上，但指定数据目录——方案级那一层住在方案文件里，没有 data_dir 就读不到。
fn coord_with_ui_at(
    cfg: Config,
    data_dir: Option<&std::path::Path>,
) -> (Arc<Coordinator>, std::sync::mpsc::Receiver<UiCommand>) {
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, data_dir);
    *c.last_valid_caret.lock().unwrap() = (100, 200, 20);
    *c.composition_start.lock().unwrap() = (100, 200, true);
    (c, rx)
}

/// 会话位的发送边沿：按键返回时状态变了就得推一次。
///
/// `STATUS_HOTKEY_SESSION` 的载荷是 level-triggered 的，但发送仍需要边沿，而四个热键模式的
/// 进出路径都不碰状态通道。漏推时 DLL 乐观置的位会永久卡在 TRUE，此后 Backspace/Enter/Escape
/// 一律被吃下转发而协调器无会话，「吃了再吐」丢键。
///
/// headless 下观察不到推送，故观察推送的前置 `last_pushed_hotkey_session`：按键返回后它必须
/// 等于当前 `build_status().hotkey_session`，同时钉住 RAII 接线与边沿检测。删掉
/// `SoftKeyboardPushOnDrop::drop` 里的 `push_hotkey_session_if_changed()` 本条即红。
#[test]
fn the_hotkey_session_flag_is_pushed_when_it_changes() {
    use crate::handle_softkeyboard::SoftKeyboardPushOnDrop;
    let (c, _rx) = coord_with_ui(Config::default());
    let cached = |c: &Arc<Coordinator>| {
        c.last_pushed_hotkey_session
            .load(std::sync::atomic::Ordering::Relaxed)
    };

    assert!(!cached(&c), "基线：空闲态缓存应为 false");

    // 进加词 —— 按键出口的 RAII 析构时应把缓存推到 true。
    c.state.lock().unwrap().add_word_active = true;
    {
        let _g = SoftKeyboardPushOnDrop(&c);
    }
    assert!(
        cached(&c),
        "进入加词后必须推一次状态，否则 DLL 侧的乐观置位没有权威值来确认"
    );

    // 退出加词 —— 这一半才是 C2 的要害：DLL 侧已无清位逻辑，全靠这次推送。
    c.state.lock().unwrap().add_word_active = false;
    {
        let _g = SoftKeyboardPushOnDrop(&c);
    }
    assert!(
        !cached(&c),
        "退出加词后必须推一次状态，否则 DLL 侧的位永久卡在 TRUE（C2 复发）"
    );

    // 没变化就不推：缓存值不动，且不该白跑一次状态推送。
    {
        let _g = SoftKeyboardPushOnDrop(&c);
    }
    assert!(!cached(&c), "状态没变时不该改动缓存");

    // 临拼同理 —— 它连焦点变化都没有，改动前靠 ClearComposition 自愈，现在只剩这条路。
    c.state.lock().unwrap().active = Some(ModeKind::TempPinyin);
    {
        let _g = SoftKeyboardPushOnDrop(&c);
    }
    assert!(cached(&c), "进入临拼后必须推");
    c.state.lock().unwrap().active = None;
    {
        let _g = SoftKeyboardPushOnDrop(&c);
    }
    assert!(!cached(&c), "退出临拼后必须推");

    // ── 命中 key_down 热键 ⇒ 无条件推，哪怕状态前后都没变 ──
    //
    // C++ 在 WM_HOTKEY 上乐观置位且不看动作，制造的是协调器观察不到的 DLL 侧状态；比较法够不着它。
    // 出厂的 `open_add_word_dialog` 就既不改 `add_word_active` 也不推状态，只靠设置端窗口的焦点
    // 往返顺带纠正——那是副作用不是机制。去掉 `push_hotkey_session_if_changed` 里的 `forced ||` 本段即红。
    {
        // 状态前后都是 false（模拟 open_add_word_dialog），但命中过热键。
        assert!(!c.build_status().hotkey_session, "前提：此刻不该有会话");
        c.last_pushed_hotkey_session
            .store(false, std::sync::atomic::Ordering::Relaxed);
        c.hotkey_session_force_push
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            c.push_hotkey_session_if_changed(),
            "命中过热键 ⇒ 必须推，哪怕协调器这边状态前后都没变（DLL 侧那个乐观置位\
                 只有这一次推送能纠正）"
        );
        assert!(
            !c.hotkey_session_force_push
                .load(std::sync::atomic::Ordering::Relaxed),
            "force_push 必须被消费掉（swap 而非 load），否则会一直强推"
        );
        // 消费之后，状态没变就不该再推。
        assert!(
            !c.push_hotkey_session_if_changed(),
            "标志已消费且状态没变 ⇒ 不该再推"
        );
    }

    // ── 不变量：两条支路走完，缓存都必须等于当前实际值 ──
    //
    // `drop` 软键盘变了走 `after_softkeyboard_change`（推完整状态后对齐缓存），否则走
    // `push_hotkey_session_if_changed`。前者若推完不对齐缓存，下一次按键会白推一次 IPC。
    for (name, sk_dirty) in [("常规支路", false), ("软键盘支路", true)] {
        c.state.lock().unwrap().add_word_active = true;
        c.softkeyboard_dirty
            .store(sk_dirty, std::sync::atomic::Ordering::Relaxed);
        {
            let _g = SoftKeyboardPushOnDrop(&c);
        }
        assert_eq!(
            cached(&c),
            c.build_status().hotkey_session,
            "{name}: 走完之后缓存必须与实际值一致"
        );
        c.state.lock().unwrap().add_word_active = false;
        c.softkeyboard_dirty
            .store(sk_dirty, std::sync::atomic::Ordering::Relaxed);
        {
            let _g = SoftKeyboardPushOnDrop(&c);
        }
        assert_eq!(
            cached(&c),
            c.build_status().hotkey_session,
            "{name}: 退出后缓存同样要与实际值一致"
        );
    }
}

/// `STATUS_HOTKEY_SESSION` 的判据：四个热键模式算会话，别的都不算。
///
/// C++ 侧 `_HasInputSession()` 无条件镜像这一位：漏一个模式，那个模式下 Backspace/Enter/Escape
/// 透传给宿主；多算一个，这三个键被吃下转发而协调器无会话。特别钉住软键盘不算——
/// 判据搬到服务端之后，这里是唯一能挡住同类错误的地方。
#[test]
fn hotkey_session_flag_covers_exactly_the_hotkey_modes() {
    let (c, _rx) = coord_with_ui(Config::default());

    // 基线：什么都没有 ⇒ 不是会话。
    assert!(
        !c.build_status().hotkey_session,
        "空闲态不该报「热键模式活着」"
    );

    // 普通打字（有 input_buffer、有候选）也不算 —— 那条路由 composition 撑会话。
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "abc".to_string();
    }
    assert!(
        !c.build_status().hotkey_session,
        "普通打字不是热键模式，会话由 composition 撑"
    );
    c.state.lock().unwrap().input_buffer.clear();

    // 五个模式逐个开、逐个关。别名用来躲 clippy::type_complexity。
    type StateMutator = Box<dyn Fn(&mut State)>;
    let cases: [(&str, StateMutator); 5] = [
        ("加词", Box::new(|st: &mut State| st.add_word_active = true)),
        (
            "临拼",
            Box::new(|st: &mut State| st.active = Some(ModeKind::TempPinyin)),
        ),
        (
            "特殊",
            Box::new(|st: &mut State| st.active = Some(ModeKind::Special(0))),
        ),
        (
            "生僻字",
            Box::new(|st: &mut State| st.active = Some(ModeKind::RareChar)),
        ),
        (
            "反查",
            Box::new(|st: &mut State| st.active = Some(ModeKind::Reverse)),
        ),
    ];
    for (name, enter) in cases {
        {
            let mut st = c.state.lock().unwrap();
            enter(&mut st);
        }
        assert!(
            c.build_status().hotkey_session,
            "{name}模式活着时必须报会话，否则该模式下 Enter/Esc/Backspace 会透传给宿主"
        );
        {
            let mut st = c.state.lock().unwrap();
            st.add_word_active = false;
            st.active = None;
        }
        assert!(
            !c.build_status().hotkey_session,
            "{name}模式退出后必须清掉，否则那三个键会被吃下转发而协调器无会话"
        );
    }

    // 软键盘不算热键模式会话。
    c.softkeyboard_active
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let st = c.build_status();
    assert!(
        st.soft_keyboard,
        "前提：软键盘该报开着（否则下面那条断言测不到东西）"
    );
    assert!(
        !st.hotkey_session,
        "软键盘不是热键模式 —— 它正是前一版 DLL 侧判据卡死的那个来源"
    );
    c.softkeyboard_active
        .store(false, std::sync::atomic::Ordering::Relaxed);

    // 临英也不算：它有自己的 composition，不走占位那条路。
    c.state.lock().unwrap().active = Some(ModeKind::TempEnglish);
    assert!(
        !c.build_status().hotkey_session,
        "临英由 composition 撑会话，不该算进本位"
    );
    c.state.lock().unwrap().active = None;
}

/// 加词（`add_word_via_composition = true`）由协调器自己发的占位跟随焦点应用的
/// compat 规则——Edge（规则 zwsp）下是 ZWSP。变异检验：加词处改回常量空格 ⇒ 红。
#[test]
fn add_word_placeholder_follows_the_focused_apps_rule() {
    let mut cfg = Config::default();
    cfg.input.caret.add_word_via_composition = true;
    let (c, _rx) = coord_with_ui(cfg);
    c.test_focus_zwsp_app(42, "msedge.exe");
    let act = {
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        c.enter_add_word_mode(&mut st)
    };
    match act {
        KeyAction::UpdateComposition { text, caret_pos } => {
            assert_eq!(text, "\u{200B}", "Edge 下加词占位应是零宽空格");
            assert_eq!(caret_pos, 0, "光标落在占位前");
        }
        other => panic!("开关打开 ⇒ 应发占位 composition，实际: {other:?}"),
    }
}

/// 占位 composition 的逐模式开关（`[input.caret]`），在出厂配置下逐条验。
///
/// 直达热键进入加词/临拼/特殊/生僻字时组合区为空，发一个占位 composition 撑开 range 取坐标；
/// 而占位建在当前 selection 上，用户选中着文字时会被替换掉。关掉即不发，坐标退到
/// GUITHREADINFO/GetCaretPos 回退链。
///
/// 必须用 `Config::default()`、不动 `preedit_display`：出厂是 `app_inline`，若改成别的值测，
/// 会整条绕开出厂路径。去掉任一 `enter_*` 里的开关判断，对应那条即红。
///
/// TODO：本用例里断言具体值的两处钉的是「占位归谁造」的契约（加词由协调器造，overlay 由 C++
/// 兜底补），不是开关；应拆成独立用例（`factory_add_word_sends_the_placeholder_itself` /
/// `hotkey_entry_leaves_the_placeholder_to_the_dll`），免得整理开关测试时被当冗余删掉。
#[test]
fn caret_switches_are_read_in_the_factory_config() {
    // 前提先钉死：出厂就是 app_inline。这条若变了，本测试的前提说明也要跟着改。
    assert_eq!(
        Config::default().ui.candidate.preedit_display,
        "app_inline",
        "出厂 preedit_display 变了，请重新审视本组测试的前提"
    );

    // ── 加词：display 恒为空，开关直接决定发不发占位 ──
    let enter_add_word = |c: &Arc<Coordinator>| {
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        c.enter_add_word_mode(&mut st)
    };

    // 出厂加词是 false（见 `CaretPlacementConfig::default`）：加词弹的是预览窗，对坐标精度不敏感，
    // 回退链够用，「保住用户选中的字」赢过「坐标最准」。改回 true 会红在本行。
    assert!(
        !Config::default().input.caret.add_word_via_composition,
        "出厂加词开关变了，请重新审视 t123 的取舍与本组测试的前提"
    );

    let (c, _rx) = coord_with_ui(Config::default());
    match enter_add_word(&c) {
        // 出厂 false ⇒ 完全不碰 composition。不能退而发空组合区：对 composition
        // 做任何操作都可能清空选中内容（与宿主有关）。
        KeyAction::Consumed => {}
        other => {
            panic!("出厂加词应不建任何组合区（否则仍会吃掉选中的文字），实际: {other:?}")
        }
    }

    let mut on = Config::default();
    on.input.caret.add_word_via_composition = true;
    let (c_on, _rx) = coord_with_ui(on);
    match enter_add_word(&c_on) {
        // 打开 ⇒ 必须发非空占位：空组合区在 WPS 等宿主上拿到退化矩形（height=0），预览窗没有坐标来源。
        // 断言到具体的值：加词的 display 恒为空，占位由协调器自己造，这一半钉的是「占位归谁造」。
        KeyAction::UpdateComposition { text, .. } => assert_eq!(
            text,
            wind_bridge::handler::COMPOSITION_PLACEHOLDER,
            "开关打开 ⇒ 须由协调器自己发占位空格撑开 range"
        ),
        other => panic!("开关打开 ⇒ 应发占位 composition，实际: {other:?}"),
    }
}

/// 三个 overlay 模式（临拼 / 特殊 / 生僻字）的开关只对直达热键那条生效。
///
/// 直达热键传 `key_code = 0`，组合区为空，建 composition 只为取坐标；引导键（`\`、`z`）进入时
/// 组合区有真实内容，必须建 composition，开关不能干预——下半段钉这条边界。
#[test]
fn overlay_caret_switches_only_gate_the_hotkey_entry() {
    // special / rare_char 共用一个形状：关掉开关 + 直达热键 ⇒ Consumed。临拼的进入点签名不同，
    // 单列在下面。别名用来躲 clippy::type_complexity。
    type ConfigMutator = fn(&mut Config);
    type HotkeyProbe = fn(&Arc<Coordinator>) -> KeyAction;
    let cases: [(&str, ConfigMutator, HotkeyProbe); 2] = [
        (
            "special",
            |c: &mut Config| c.input.caret.special_via_composition = false,
            |c: &Arc<Coordinator>| {
                let mut st = c.state.lock().unwrap();
                st.chinese_mode = true;
                c.enter_special_mode(&mut st, 0, 0) // key_code=0 = 直达热键
            },
        ),
        (
            "rare_char",
            |c: &mut Config| c.input.caret.rare_char_via_composition = false,
            |c: &Arc<Coordinator>| {
                let mut st = c.state.lock().unwrap();
                st.chinese_mode = true;
                c.enter_rare_char_mode(&mut st, 0)
            },
        ),
    ];

    for (name, turn_off, enter) in cases {
        let (c_on, _rx) = coord_with_ui(Config::default());
        // 这一半连 text 为空一起钉：这三个模式的占位不在协调器造，而是发空组合区、由 C++ 兜底补空格
        // （`TextService.cpp` 的 `isPlaceholder` 分支）。Rust 侧再补一次就成了双重占位。
        match enter(&c_on) {
            KeyAction::UpdateComposition { text, .. } => assert!(
                text.is_empty(),
                "{name}: 直达热键进入应发空组合区（占位由 C++ 兜底补），实际 text={text:?}"
            ),
            other => {
                panic!("{name}: 出厂开关为 true ⇒ 应建 composition 取坐标，实际: {other:?}")
            }
        }

        let mut off = Config::default();
        turn_off(&mut off);
        let (c_off, _rx) = coord_with_ui(off);
        match enter(&c_off) {
            KeyAction::Consumed => {}
            other => panic!("{name}: 开关关闭 + 直达热键 ⇒ 应 Consumed，实际: {other:?}"),
        }
    }

    // ── 临拼：调用点签名与上面两个不同，单列 ──
    //
    // 调用点在 `commit_and_enter_temp_pinyin` 的 `match committed` 里，两支待遇不同
    // （`Some` 那支刻意不看开关），改那个函数时很容易把 `None` 那支一并改掉。
    {
        let mut off = Config::default();
        off.input.caret.temp_pinyin_via_composition = false;
        let (c_tp, _rx) = coord_with_ui(off);
        let act = {
            let mut st = c_tp.state.lock().unwrap();
            st.chinese_mode = true;
            // 缓冲为空 ⇒ take_committed_with_highlight 回 None ⇒ 走看开关的那一支。
            assert!(
                st.input_buffer.is_empty(),
                "前提：缓冲必须为空，否则走的是顶字那一支"
            );
            c_tp.commit_and_enter_temp_pinyin(&mut st, 0, "pinyin".to_string())
        };
        match act {
            KeyAction::Consumed => {}
            other => panic!("临拼：开关关闭 + 直达热键 ⇒ 应 Consumed，实际: {other:?}"),
        }

        let (c_on, _rx) = coord_with_ui(Config::default());
        let act_on = {
            let mut st = c_on.state.lock().unwrap();
            st.chinese_mode = true;
            c_on.commit_and_enter_temp_pinyin(&mut st, 0, "pinyin".to_string())
        };
        assert!(
            matches!(act_on, KeyAction::UpdateComposition { .. }),
            "临拼：出厂开关为 true ⇒ 应建 composition 取坐标，实际: {act_on:?}"
        );
    }

    // ── 边界三：有半成品可上屏时，临拼开关不参与 ──
    //
    // 走 `Some(committed)` 那一支本来就不插占位（`InsertText { new_composition: None }`），
    // 与开关想避免的「为取坐标而插占位」不是一回事。
    {
        let mut off = Config::default();
        off.input.caret.temp_pinyin_via_composition = false;
        let (c, _rx) = coord_with_ui(off);
        let act = {
            let mut st = c.state.lock().unwrap();
            st.chinese_mode = true;
            // 「有半成品可上屏」= `committed_text` 非空（已确认的转换前缀），
            // 不是 input_buffer 非空 —— `take_committed` 取的是前者。
            st.committed_text = "你好".to_string();
            st.candidates.clear(); // 无候选 ⇒ 直接回 committed_text
            c.commit_and_enter_temp_pinyin(&mut st, 0, "pinyin".to_string())
        };
        // 断言到具体类型而不是「不是 Consumed」：顶字那支发的是
        // `InsertText { new_composition: None }`（prefix 为空 ⇒ 不带新组合区）。
        // 只否定 Consumed 的话，哪天这支漂成别的 KeyAction 也照样绿。
        match act {
            KeyAction::InsertText {
                text,
                new_composition,
                ..
            } => {
                assert_eq!(text, "你好", "顶字那支应把已确认的前缀上屏");
                assert!(
                    new_composition.is_none(),
                    "prefix 为空 ⇒ 不该带新组合区（带了就是又插了一次占位），实际: {new_composition:?}"
                );
            }
            other => panic!(
                "有半成品可上屏时走的是顶字那一支，开关不该干预（干预会丢掉已打的码），实际: {other:?}"
            ),
        }
    }

    // ── 边界一：引导键进入（display 非空）时，开关不得干预 ──
    // 关掉开关，但用真实引导键 `z`（VK 0x5A）进入 ⇒ 组合区有内容，必须照常建。
    let mut off = Config::default();
    off.input.caret.rare_char_via_composition = false;
    let (c, _rx) = coord_with_ui(off);
    let act = {
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        c.enter_rare_char_mode(&mut st, 0x5A)
    };
    match act {
        KeyAction::UpdateComposition { text, .. } => assert!(
            !text.is_empty(),
            "引导键进入时组合区是真实内容，开关不该把它清空"
        ),
        other => panic!("引导键进入 ⇒ 必须建 composition 显示引导符，实际: {other:?}"),
    }

    // ── 边界二：引导键映射不出前缀字符时，不得被当成直达热键吞掉 ──
    //
    // `vk_to_prefix_char_with_letters` 只认标点表和 A-Z，其余 `None` ⇒ `display` 为空。判据若用
    // `display.is_empty()` 代理「直达热键」，引导键就会被吞（模式进了、组合区却没建）。真判据是
    // `key_code == 0` 这个显式哨兵。
    assert!(
        wind_keys::keymap::vk_to_prefix_char_with_letters(0x70).is_none(),
        "前提：VK_F1(0x70) 映射不出前缀字符，否则本用例选错了键"
    );
    let mut off2 = Config::default();
    off2.input.caret.rare_char_via_composition = false;
    let (c2, _rx) = coord_with_ui(off2);
    let act2 = {
        let mut st = c2.state.lock().unwrap();
        st.chinese_mode = true;
        c2.enter_rare_char_mode(&mut st, 0x70) // 非 0 ⇒ 是引导键进入，只是映射不出字符
    };
    assert!(
        matches!(act2, KeyAction::UpdateComposition { .. }),
        "key_code != 0 是引导键进入，哪怕映射不出前缀也不得吞键，实际: {act2:?}"
    );
}

/// 写一个码表方案文件（只有方案头），返回它的 data_dir。方案层的注释分配在用户配置里，
/// 夹具方案只是给 `assign.schema.<id>` 一个可激活的方案。
fn data_dir_with_schema(tag: &str, id: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_cmt_schema_{}_{tag}", std::process::id()));
    let schemas = dir.join("schemas");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::write(
        schemas.join(format!("{id}.schema.toml")),
        format!("[schema]\nid = \"{id}\"\nname = \"注释测试\"\n[engine]\ntype = \"codetable\"\n"),
    )
    .unwrap();
    dir
}

/// 四格都指向 `id`。
fn all_cells(id: &str) -> wind_config::comment_lib::CommentCells {
    wind_config::comment_lib::CommentCells {
        default_vertical: Some(id.into()),
        default_horizontal: Some(id.into()),
        detail_vertical: Some(id.into()),
        detail_horizontal: Some(id.into()),
    }
}

/// 登记用户模板，返回四格都指向它的分配。
fn add_template(cfg: &mut Config, id: &str, text: &str) -> wind_config::comment_lib::CommentCells {
    cfg.ui
        .comment
        .templates
        .push(wind_config::comment_lib::CommentTemplateDef {
            id: id.into(),
            name: id.into(),
            text: text.into(),
            ..Default::default()
        });
    all_cells(id)
}

/// 临英四格指向模板「临英${code_hint}」。
fn with_temp_english(cfg: &mut Config) {
    let cells = add_template(cfg, "te", "临英${code_hint}");
    cfg.ui
        .comment
        .assign
        .mode
        .insert("temp_english".into(), cells);
}

/// 直接驱动候选下发：造一条候选、进指定模式，然后走真实的 `notify_ui_update`。
///
/// 候选带 `comment`（`${code_hint}` 的取值源）：模板必须含至少一个非空变量，否则「变量全空则
/// 整个模板输出空串」的规则会让纯字面量模板恒渲染成空。
fn emit(c: &Arc<Coordinator>, active: Option<ModeKind>) {
    {
        let mut st = c.state.lock().unwrap();
        st.active = active;
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            comment: "码".into(),
            ..Default::default()
        }];
        st.input_buffer = "a".into();
    }
    let st = c.state.lock().unwrap();
    c.notify_ui_update(&st);
}

/// 取最近一条 `UpdateCandidates` 里首候选的注释段。
fn last_comment(rx: &std::sync::mpsc::Receiver<UiCommand>) -> Option<String> {
    let mut found = None;
    // 排空取最后一条：一次刷新会发多条 UI 命令，取第一条会拿到上一轮残留。
    while let Ok(cmd) = rx.try_recv() {
        if let UiCommand::UpdateCandidates { candidates, .. } = cmd {
            found = candidates.first().map(|c| c.comment.as_str().to_string());
        }
    }
    found
}

fn cfg_with_templates() -> Config {
    let mut c = Config::default();
    // 用字面量而非变量，断言才不依赖词库内容
    c.ui.comment.assign.global = add_template(&mut c, "g", "全局${code_hint}");
    c
}

/// 上方注释条开关走到发往 UI 的候选上：开 ⇒ `comment_above` 拿到上段、`comment` 只剩下段；
/// 关 ⇒ 不拆分，字面 `\n` 原样留在 `comment`，`comment_above` 为空。
#[test]
fn comment_above_switch_reaches_ui() {
    let run = |on: bool| {
        let mut cfg = Config::default();
        cfg.ui.comment.assign.global =
            add_template(&mut cfg, "g", "上${code_hint}\n下${code_hint}");
        cfg.ui.candidate.comment_above = on;
        let (c, rx) = coord_with_ui(cfg);
        emit(&c, None);
        let mut found = None;
        while let Ok(cmd) = rx.try_recv() {
            if let UiCommand::UpdateCandidates { candidates, .. } = cmd {
                found = candidates.first().map(|c| {
                    (
                        c.comment_above.as_str().to_string(),
                        c.comment.as_str().to_string(),
                    )
                });
            }
        }
        found.expect("应下发候选")
    };
    assert_eq!(
        run(true),
        ("上码".to_string(), "下码".to_string()),
        "开关开：上段进 comment_above"
    );
    assert_eq!(
        run(false),
        (String::new(), "上码\n下码".to_string()),
        "开关关：不拆分，与改动前逐字节一致"
    );
}

#[test]
fn mode_override_reaches_ui() {
    let mut cfg = cfg_with_templates();
    with_temp_english(&mut cfg);
    let (c, rx) = coord_with_ui(cfg);

    emit(&c, None);
    assert_eq!(
        last_comment(&rx),
        Some("全局码".to_string()),
        "无模式时取全局模板"
    );

    emit(&c, Some(ModeKind::TempEnglish));
    assert_eq!(
        last_comment(&rx),
        Some("临英码".to_string()),
        "临英期间必须改用模式级模板——只测 template_for 抓不到消费端没接线"
    );
}

/// `TEXT_ROLES` 契约清单（设计 text-span-colors.md §3.1）里每个变量名都能被某个求值入口
/// 求出值（空串也算）：候选上下文（`eval_var`）、气泡附加（`cand_eval`）、逐字上下文。
/// 判据是「写进模板不会原样回显 `${…}`」；走真实 `notify_ui_update`，气泡附加变量只在它的闭包里。
#[test]
fn every_text_role_is_evaluable_somewhere() {
    const PER_CHAR: &[&str] = &["char", "readings", "unicode"];
    let vars: Vec<&str> = wind_ui_types::TEXT_ROLES
        .iter()
        .copied()
        .filter(|r| !wind_ui_types::STRUCTURAL_ROLES.contains(r))
        .collect();
    let whole: String = vars
        .iter()
        .filter(|v| !PER_CHAR.contains(v))
        .map(|v| format!("${{{v}}}"))
        .collect();
    let per_char: String = PER_CHAR.iter().map(|v| format!("${{{v}}}")).collect();
    // 注释段只有候选上下文（`eval_var`）；气泡的整段求值再加上气泡附加变量。
    const BUBBLE_ONLY: &[&str] = &[
        "word_code",
        "code_source",
        "debug",
        "full_text",
        "unicode_all",
    ];
    let cand_ctx: String = vars
        .iter()
        .filter(|v| !PER_CHAR.contains(v) && !BUBBLE_ONLY.contains(v))
        .map(|v| format!("${{{v}}}"))
        .collect();
    let tooltip_of = |comment_tpl: &str, whole_tpl: &str| {
        let mut cfg = Config::default();
        cfg.ui.comment.assign.global =
            add_template(&mut cfg, "g", &format!("${{code_hint}}{comment_tpl}"));
        let sec = |each: &str, template: &str| wind_config::config::TooltipSection {
            enabled: true,
            label: "段".into(),
            template: template.into(),
            each: each.into(),
            promote: String::new(),
            inline: false,
        };
        cfg.ui.tooltip.sections = vec![sec("", &format!("整{whole_tpl}")), sec("char", &per_char)];
        let (c, rx) = coord_with_ui(cfg);
        emit(&c, None);
        let mut got = None;
        while let Ok(cmd) = rx.try_recv() {
            if let UiCommand::UpdateCandidates { candidates, .. } = cmd {
                got = candidates
                    .first()
                    .map(|c| (c.comment.as_str().to_string(), c.tooltip.to_plain_text()));
            }
        }
        got.expect("应下发候选")
    };
    let (comment, tooltip) = tooltip_of(&cand_ctx, &whole);
    assert!(
        !comment.contains("${"),
        "注释里有求值入口不认的变量：{comment}"
    );
    assert!(
        !tooltip.contains("${"),
        "气泡里有求值入口不认的变量：{tooltip}"
    );
    // 防空转：逐字段的 `${unicode}` 恒有值，气泡一定在；拼错的名字一定会回显。
    assert!(tooltip.contains("U+6D4B"), "气泡应含逐字段：{tooltip}");
    let (comment, tooltip) = tooltip_of("${no_such_var}", "${no_such_var}");
    assert!(comment.contains("${no_such_var}") && tooltip.contains("${no_such_var}"));
}

/// `none` = 本模式不显示注释（与「跟随全局」是两回事），且这条语义要一路走到 UI。
#[test]
fn empty_override_hides_comment_at_ui() {
    let mut cfg = cfg_with_templates();
    cfg.ui
        .comment
        .assign
        .mode
        .insert("temp_pinyin".into(), all_cells("none"));
    let (c, rx) = coord_with_ui(cfg);

    emit(&c, Some(ModeKind::TempPinyin));
    assert_eq!(
        last_comment(&rx),
        Some(String::new()),
        "none 必须让本模式不显示注释，而不是回落全局"
    );
}

/// 退出模式后自动回到全局模板——声明式重算的自愈性，无需任何「恢复」动作。
#[test]
fn leaving_mode_restores_global_template() {
    let mut cfg = cfg_with_templates();
    with_temp_english(&mut cfg);
    let (c, rx) = coord_with_ui(cfg);

    emit(&c, Some(ModeKind::TempEnglish));
    assert_eq!(last_comment(&rx), Some("临英码".to_string()));
    emit(&c, None);
    assert_eq!(
        last_comment(&rx),
        Some("全局码".to_string()),
        "退出模式后应自动算回全局，不依赖任何显式恢复"
    );
}

/// 模式无意见 + 方案有意图 + 类型与全局都有值 ⇒ 方案层胜出；类型层排在全局之上。
/// 进模式后必须变回模式级那份，否则就是把方案层错插在了模式层之上。
#[test]
fn schema_layer_reaches_ui_and_mode_wins_over_it() {
    let dir = data_dir_with_schema("wins", "zz_cmt");
    let mut cfg = cfg_with_templates();
    cfg.schema.active = "zz_cmt".into();
    let schema = add_template(&mut cfg, "s", "方案${code_hint}");
    cfg.ui.comment.assign.schema.insert("zz_cmt".into(), schema);
    let ty = add_template(&mut cfg, "t", "类型${code_hint}");
    cfg.ui.comment.assign.by_type.insert("codetable".into(), ty);
    with_temp_english(&mut cfg);
    let (c, rx) = coord_with_ui_at(cfg.clone(), Some(&dir));

    emit(&c, None);
    assert_eq!(
        last_comment(&rx),
        Some("方案码".to_string()),
        "模式没意见时必须落到方案层，而不是直接跳到类型或全局"
    );

    emit(&c, Some(ModeKind::TempEnglish));
    assert_eq!(
        last_comment(&rx),
        Some("临英码".to_string()),
        "模式层排在方案层之上"
    );

    cfg.ui.comment.assign.schema.clear();
    let (c, rx) = coord_with_ui_at(cfg, Some(&dir));
    emit(&c, None);
    assert_eq!(
        last_comment(&rx),
        Some("类型码".to_string()),
        "方案层没意见时落到类型层"
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `none` 在方案层同样成立：本方案不显示注释，不是「跟随全局」。
#[test]
fn schema_empty_template_hides_comment() {
    let dir = data_dir_with_schema("empty", "zz_cmt2");
    let mut cfg = cfg_with_templates();
    cfg.schema.active = "zz_cmt2".into();
    cfg.ui
        .comment
        .assign
        .schema
        .insert("zz_cmt2".into(), all_cells("none"));
    let (c, rx) = coord_with_ui_at(cfg, Some(&dir));

    emit(&c, None);
    assert_eq!(
        last_comment(&rx),
        Some(String::new()),
        "方案层 none 必须让本方案不显示注释，而不是回落全局"
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 方案没有分配（绝大多数方案的常态）时回落全局。
#[test]
fn schema_without_templates_follows_global() {
    let dir = data_dir_with_schema("silent", "zz_cmt3");
    let mut cfg = cfg_with_templates();
    cfg.schema.active = "zz_cmt3".into();
    let other = add_template(&mut cfg, "o", "别的${code_hint}");
    cfg.ui.comment.assign.schema.insert("wubi86".into(), other);
    let (c, rx) = coord_with_ui_at(cfg, Some(&dir));

    emit(&c, None);
    assert_eq!(
        last_comment(&rx),
        Some("全局码".to_string()),
        "方案没意见就该跟随下一层（全局），别的方案的分配不串过来"
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

// ───────────────────── 两个总开关（memory-footprint.md §4.3，S2） ─────────────────────

/// 取最近一条 `UpdateCandidates` 里首候选的 `(上方注释条, 注释)`。
fn last_comment_parts(rx: &std::sync::mpsc::Receiver<UiCommand>) -> Option<(String, String)> {
    let mut found = None;
    while let Ok(cmd) = rx.try_recv() {
        if let UiCommand::UpdateCandidates { candidates, .. } = cmd {
            found = candidates.first().map(|c| {
                (
                    c.comment_above.as_str().to_string(),
                    c.comment.as_str().to_string(),
                )
            });
        }
    }
    found
}

/// 注释总开关在逐层裁决之上：全局 / 方案级 / 模式级各一例，开着时各自出注释，
/// 关掉后全都不出——含 `${code_hint}`（码表引擎产的编码提示）与上方注释条。
#[test]
fn comment_switch_off_overrides_all_three_layers() {
    let dir = data_dir_with_schema("switch", "zz_cmt_sw");
    let run = |enabled: bool| {
        let mut cfg = Config::default();
        cfg.ui.comment.enabled = enabled;
        // 上方注释条一并开着，全局模板带字面 `\n`：开着时上段进上方条，关掉总开关后
        // 上方条也必须为空——不然它可能是因为模板没拆出上段才空的，断言就是恒绿。
        cfg.ui.candidate.comment_above = true;
        cfg.ui.comment.assign.global =
            add_template(&mut cfg, "g", "上${code_hint}\n全局${code_hint}");
        let schema = add_template(&mut cfg, "s", "方案${code_hint}");
        cfg.ui
            .comment
            .assign
            .schema
            .insert("zz_cmt_sw".into(), schema);
        with_temp_english(&mut cfg);
        // 全局层：无方案目录（方案层没意见）、无模式。
        let (g, grx) = coord_with_ui(cfg.clone());
        emit(&g, None);
        let global = last_comment_parts(&grx).expect("应下发候选");
        // 方案层：用户给本方案分配了模板、无模式。
        cfg.schema.active = "zz_cmt_sw".into();
        let (s, srx) = coord_with_ui_at(cfg, Some(&dir));
        emit(&s, None);
        let schema = last_comment_parts(&srx).expect("应下发候选");
        // 模式层：临英。
        emit(&s, Some(ModeKind::TempEnglish));
        let mode = last_comment_parts(&srx).expect("应下发候选");
        [global, schema, mode]
    };
    let none = (String::new(), String::new());
    assert_eq!(
        run(true),
        [
            ("上码".to_string(), "全局码".to_string()),
            (String::new(), "方案码".to_string()),
            (String::new(), "临英码".to_string()),
        ],
        "前置条件：开关开着时三层各出各的注释（含 ${{code_hint}}）"
    );
    assert_eq!(
        run(false),
        [none.clone(), none.clone(), none],
        "注释总开关关掉 ⇒ 三层模板都不出注释，${{code_hint}} 与上方注释条也不出"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 悬停总开关关掉：候选不带气泡，且候选刷新不再调用 `word_codes_display`（计数断言）。
/// 要真实码表方案（`build_dev` 的 wubi86），否则一次也不会查，断言恒绿。
#[test]
fn tooltip_switch_off_skips_rendering_and_word_code_lookup() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/wubi86.schema.toml").exists() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let run = |enabled: bool| {
        let mut cfg = Config::default();
        cfg.schema.active = "wubi86".into();
        cfg.schema.available = vec!["wubi86".into()];
        cfg.ui.tooltip.enabled = enabled;
        let (c, rx) = coord_with_ui_at(cfg, Some(&data));
        let before = WORD_CODE_LOOKUPS.with(|n| n.get());
        emit(&c, None);
        let lookups = WORD_CODE_LOOKUPS.with(|n| n.get()) - before;
        let mut tip = None;
        while let Ok(cmd) = rx.try_recv() {
            if let UiCommand::UpdateCandidates { candidates, .. } = cmd {
                tip = candidates.first().map(|c| c.tooltip.clone());
            }
        }
        (tip.expect("应下发候选"), lookups)
    };
    let (tip, lookups) = run(true);
    assert!(
        !tip.is_empty(),
        "前置条件：开着时候选「测」有气泡（拼音段）"
    );
    assert!(
        lookups > 0,
        "前置条件：开着时编码段逐候选查 word_codes_display"
    );

    let (tip, lookups) = run(false);
    assert!(tip.is_empty(), "悬停关掉 ⇒ 候选不带气泡：{tip:?}");
    assert_eq!(lookups, 0, "悬停关掉 ⇒ 不再调用 word_codes_display");
}

/// 出厂「释义」段走真实链路：挂两个注释库，经 `notify_ui_update` 下发的气泡
/// 里每个库的释义各占一行；查不到的候选没有这一段。
#[test]
fn default_tooltip_dict_all_section_lists_every_library() {
    let dir = std::env::temp_dir().join(format!("wind_tip_dict_all_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let head = "name: t\ncolumns:\n  - text\n  - comment\n...\n";
    let specs: Vec<_> = [("en", "v.配置"), ("it", "[IT]配置…")]
        .iter()
        .map(|(n, cmt)| {
            let p = dir.join(format!("{n}.dict.yaml"));
            std::fs::write(&p, format!("{head}configure\t{cmt}\n")).unwrap();
            (p, Vec::new())
        })
        .collect();
    let (c, rx) = coord_with_ui(Config::default());
    c.reverse.write().unwrap().reload_comments(&specs, None);
    let tip_of = |text: &str| {
        {
            let mut st = c.state.lock().unwrap();
            st.candidates = vec![wind_candidate::Candidate {
                text: text.into(),
                code: text.into(),
                source: wind_candidate::CandidateSource::English,
                ..Default::default()
            }];
            st.input_buffer = text.into();
        }
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
        drop(st);
        let mut tip = None;
        while let Ok(cmd) = rx.try_recv() {
            if let UiCommand::UpdateCandidates { candidates, .. } = cmd {
                tip = candidates.first().map(|c| c.tooltip.to_plain_text());
            }
        }
        tip.expect("应下发候选")
    };
    assert!(
        tip_of("configure").contains("[释义]\nv.配置\n[IT]配置…"),
        "{:?}",
        tip_of("configure")
    );
    assert!(!tip_of("nosuch").contains("[释义]"));
    let _ = std::fs::remove_dir_all(&dir);
}
