//! CapsLock 大写模式路由验证（不需要词典文件）。
//! 覆盖三条路径：字母透传 / 标点透传 / 全角提交。
use super::*;

fn coord_cn() -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    // 关闭智能符号，避免 CommitAndHoldComposition 干扰标点断言
    cfg.input.symbol.smart_mode = false;
    Coordinator::new_headless(cfg, None)
}

/// 构造最简按键事件
fn kev(key_code: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 向 coordinator 注入 CapsLock 状态（模拟 C++ 端发 key_up + toggles 位）。
fn set_caps_lock(c: &Coordinator, on: bool) {
    let mut ev = kev(0x14 /* VK_CAPITAL */, EVENT_KEY_UP);
    ev.toggles = if on { 0x01 } else { 0x00 };
    c.handle_key_event(&ev);
}

/// CapsLock 开启期间的按键事件：真实 C++ 每键都带 toggles 快照（GetKeyState 实时值），
/// caps 开着时 bit0=1。handle_key_event 入口会按此快照校准镜像，故必须如实构造。
fn kev_caps(key_code: u32, event_type: u8) -> KeyEventData {
    let mut ev = kev(key_code, event_type);
    ev.toggles = 0x01;
    ev
}

/// 每键 toggles 快照校准镜像：英文模式（TSF 不吃 VK_CAPITAL）或在其它应用/输入法
/// 期间切换大写时，专门的状态通知不会到达、镜像陈旧——此校准是 cancel_on_mode_switch
/// 在"英文+大写"场景能生效的前提。
#[test]
fn key_event_toggles_recalibrates_caps_mirror() {
    let c = coord_cn();
    assert!(!c.state.lock().unwrap().caps_lock);
    // 未收到过 VK_CAPITAL 通知，但按键快照显示 caps 已开 → 入口校准。
    c.handle_key_event(&kev_caps(0x41, EVENT_KEY_DOWN));
    assert!(
        c.state.lock().unwrap().caps_lock,
        "入口应按 toggles 快照校准 CapsLock 镜像"
    );
}

// ── 字母透传 ────────────────────────────────────────────────────────────

#[test]
fn capslock_on_letter_passthrough() {
    let c = coord_cn();
    set_caps_lock(&c, true);
    // 字母 A：中文 + CapsLock + 无 session → 系统产生大写 A，coordinator 不介入
    let action = c.handle_key_event(&kev_caps(0x41, EVENT_KEY_DOWN));
    assert!(
        matches!(action, KeyAction::PassThrough),
        "中文+CapsLock+字母应透传，实际: {:?}",
        action
    );
}

#[test]
fn capslock_off_letter_enters_chinese_flow() {
    let c = coord_cn();
    // CapsLock 关：字母进入中文输入流
    let action = c.handle_key_event(&kev(0x41, EVENT_KEY_DOWN));
    assert!(
        matches!(action, KeyAction::UpdateComposition { .. }),
        "CapsLock关+字母应进输入流，实际: {:?}",
        action
    );
}

// ── 标点透传（无 input session）──────────────────────────────────────────

#[test]
fn capslock_on_punct_no_session_passthrough() {
    let c = coord_cn();
    set_caps_lock(&c, true);
    // VK 0xBC = ','，无 input_buffer → 透传给系统
    let action = c.handle_key_event(&kev_caps(0xBC, EVENT_KEY_DOWN));
    assert!(
        matches!(action, KeyAction::PassThrough),
        "中文+CapsLock+无session+标点应透传，实际: {:?}",
        action
    );
}

/// 中文 + CapsLock + 英文配对开 → 配对键与「英文 + 大写」同样出一对、可跳出。
/// DLL 对配对键开了透传例外（`_IsEnglishHalfEatenPunctKey`）吃下转发，此处若仍
/// PassThrough 就是「吃了再吐」，配对静默失效。
#[test]
fn capslock_on_english_pair_key_inserts_pair() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.auto_pair.english = true;
    let c = Coordinator::new_headless(cfg, None);
    set_caps_lock(&c, true);
    let mut ev = kev_caps(0x39 /* '9' */, EVENT_KEY_DOWN);
    ev.modifiers = MOD_SHIFT;
    match c.handle_key_event(&ev) {
        KeyAction::InsertTextWithCursor {
            text,
            cursor_offset,
        } => {
            assert_eq!(text, "()");
            assert_eq!(cursor_offset, 1);
        }
        other => panic!("中文+CapsLock+Shift+9 应插入英文配对，实际: {:?}", other),
    }
    // 非配对标点仍按原样透传（保留原始 WM_KEYDOWN）。
    assert!(matches!(
        c.handle_key_event(&kev_caps(0xBC, EVENT_KEY_DOWN)),
        KeyAction::PassThrough
    ));
}

/// 对照：英文配对关时配对键仍透传（DLL 判据 `ShouldEat` 同样为假，不吃）。
#[test]
fn capslock_on_pair_key_passthrough_when_english_pair_off() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.auto_pair.english = false;
    let c = Coordinator::new_headless(cfg, None);
    set_caps_lock(&c, true);
    let mut ev = kev_caps(0x39, EVENT_KEY_DOWN);
    ev.modifiers = MOD_SHIFT;
    assert!(matches!(c.handle_key_event(&ev), KeyAction::PassThrough));
}

#[test]
fn capslock_off_punct_commits_chinese_punct() {
    let c = coord_cn();
    let action = c.handle_key_event(&kev(0xBC, EVENT_KEY_DOWN));
    // CapsLock 关 + 中文标点：',' → "，"
    let text = match &action {
        KeyAction::InsertText { text, .. } => text.clone(),
        other => panic!("CapsLock关+逗号应上屏中文标点，实际: {:?}", other),
    };
    assert_eq!(text, "，", "实际文本: {:?}", text);
}

// ── 智能符号 HoldComposition：press2 的替换语义 ──────────────────────────

/// press1 把中文符号放进 TSF 组合（hold 预览态），press2 必须返回
/// `CommitReplacingHeld` 而非普通 `InsertText`。
///
/// 两者 IPC 载荷同构，C++ 端只靠 action 的 flags 位判断覆盖还是追加 held 符号；
/// 退回 InsertText 会打出「，,」。
#[test]
fn smart_symbol_hold_press2_replaces_held_symbol() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = true;
    cfg.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
    let c = Coordinator::new_headless(cfg, None);

    // press1：空缓冲 + 中文标点 → 中文符号进组合态，等 press2
    let a1 = c.handle_key_event(&kev(0xBC, EVENT_KEY_DOWN));
    match &a1 {
        KeyAction::HoldComposition { text, .. } => {
            assert_eq!(text, "，", "press1 应把中文逗号放进组合")
        }
        other => panic!("press1 应开 hold 组合，实际: {:?}", other),
    }

    // press2：超时窗口内重按同键 → 英文符号 + 替换语义
    let a2 = c.handle_key_event(&kev(0xBC, EVENT_KEY_DOWN));
    match &a2 {
        KeyAction::CommitReplacingHeld { text, .. } => {
            assert_eq!(text, ",", "press2 应换成英文逗号")
        }
        other => panic!(
            "press2 必须返回 CommitReplacingHeld（替换语义），实际: {:?}",
            other
        ),
    }
}

// ── 全角模式：提交全角字符 ───────────────────────────────────────────────

#[test]
fn capslock_on_fullwidth_letter_commits_uppercase_fullwidth() {
    let c = coord_cn();
    c.state.lock().unwrap().full_width = true;
    set_caps_lock(&c, true);
    // CapsLock ON + 无 Shift + 字母 A → 大写 A → 全角 "Ａ"
    let action = c.handle_key_event(&kev_caps(0x41, EVENT_KEY_DOWN));
    match &action {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(
                text, "Ａ",
                "CapsLock+全角+A应输出全角大写，实际: {:?}",
                text
            );
        }
        other => panic!("CapsLock+全角+字母应上屏，实际: {:?}", other),
    }
}

#[test]
fn capslock_on_fullwidth_shift_letter_commits_lowercase_fullwidth() {
    let c = coord_cn();
    c.state.lock().unwrap().full_width = true;
    set_caps_lock(&c, true);
    // CapsLock ON + Shift + 字母 A → 翻转大小写 → 小写 a → 全角 "ａ"
    let mut ev = kev_caps(0x41, EVENT_KEY_DOWN);
    ev.modifiers = MOD_SHIFT;
    let action = c.handle_key_event(&ev);
    match &action {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(
                text, "ａ",
                "CapsLock+Shift+全角+A应输出全角小写，实际: {:?}",
                text
            );
        }
        other => panic!("CapsLock+Shift+全角+字母应上屏，实际: {:?}", other),
    }
}

#[test]
fn capslock_on_fullwidth_punct_commits_fullwidth() {
    let c = coord_cn();
    c.state.lock().unwrap().full_width = true;
    set_caps_lock(&c, true);
    // ',' 经英全列转换后上屏（不透传）
    let action = c.handle_key_event(&kev_caps(0xBC, EVENT_KEY_DOWN));
    assert!(
        matches!(action, KeyAction::InsertText { .. }),
        "CapsLock+全角+标点应上屏，实际: {:?}",
        action
    );
}

// ── CapsLock 状态切换正确传播 ────────────────────────────────────────────

#[test]
fn capslock_toggle_updates_state() {
    let c = coord_cn();
    assert!(
        !c.state.lock().unwrap().caps_lock,
        "初始 CapsLock 应为 false"
    );
    set_caps_lock(&c, true);
    assert!(
        c.state.lock().unwrap().caps_lock,
        "set_caps_lock(true) 后应为 true"
    );
    set_caps_lock(&c, false);
    assert!(
        !c.state.lock().unwrap().caps_lock,
        "set_caps_lock(false) 后应为 false"
    );
}

/// 配了 `capslock` 会话绑定的协调器（钩子在 headless 下装不上，故 slot 恒为 None）。
fn coord_with_capslock(verb: &str) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.keys
        .session_actions
        .insert("capslock".to_string(), verb.to_string());
    Coordinator::new_headless(cfg, None)
}

/// keyup 到达即证明钩子没吃这个键，服务端一律服从钩子的结论，否则翻页与转大写同时生效。
///
/// `SHOULD_EAT` 由服务端置位、钩子在按键瞬间读取，中间隔着 IPC 往返且会被隐藏/失焦归零，
/// 不一致窗口无法消除，只能以钩子为准。
#[test]
fn capslock_key_up_defers_to_hook_when_installed() {
    // 钩子装着：CapsLock 的 keyup 不再执行会话动作（系统已经翻转，别再抢）。
    assert!(!Coordinator::key_up_session_action_allowed(
        keymap::VK_CAPITAL,
        true
    ));
    // 钩子没装（非 Windows / 安装失败 / 用户没配）：keyup 是唯一退路，必须放行。
    assert!(Coordinator::key_up_session_action_allowed(
        keymap::VK_CAPITAL,
        false
    ));
    // 其余 keyup-only 键与钩子无关，两种情况都放行——CapsLock 是唯一有专用拦截器的键。
    for vk in [keymap::VK_LSHIFT, keymap::VK_RCONTROL] {
        assert!(
            Coordinator::key_up_session_action_allowed(vk, true),
            "{vk:#X}"
        );
        assert!(
            Coordinator::key_up_session_action_allowed(vk, false),
            "{vk:#X}"
        );
    }
}

/// 钩子漏吃那一次，状态同步分支只校准镜像，不得毁掉正在打的编码。
///
/// 配了会话绑定的用户按 CapsLock 不是想切大写；系统的翻转撤不回，但不该再把编码
/// 上屏/丢弃（`take_input_on_mode_switch`），否则翻页时编码莫名没了。
#[test]
fn capslock_state_sync_keeps_pending_input_when_bound() {
    let c = coord_with_capslock("page_prev");
    assert!(c.capslock_bound(), "夹具应已绑定 capslock");
    {
        let mut s = c.state.lock().unwrap();
        s.input_buffer = "nihao".to_string();
    }
    // 无候选 ⇒ 会话动作返回 None ⇒ 落到 CapsLock 状态同步分支。
    let mut ev = kev(0x14, EVENT_KEY_UP);
    ev.toggles = 0x01;
    c.handle_key_event(&ev);
    let s = c.state.lock().unwrap();
    assert!(s.caps_lock, "镜像仍须同步成系统的真实锁定态");
    assert_eq!(s.input_buffer, "nihao", "绑定态下不得动输入缓冲");
}

/// 未绑定时保持原语义：切大写按「切英文」处置待输入（`commit_on_switch`）。
/// 与上一条成对——分野是「配没配」，不是「有没有会话」。
#[test]
fn capslock_state_sync_still_takes_input_when_unbound() {
    let c = coord_cn();
    assert!(!c.capslock_bound(), "夹具不应绑定 capslock");
    {
        let mut s = c.state.lock().unwrap();
        s.input_buffer = "nihao".to_string();
    }
    let mut ev = kev(0x14, EVENT_KEY_UP);
    ev.toggles = 0x01;
    c.handle_key_event(&ev);
    let s = c.state.lock().unwrap();
    assert!(s.caps_lock);
    assert!(s.input_buffer.is_empty(), "未绑定时维持既有的清空语义");
}

/// 打字中按 CapsLock 开大写（`toggles` 置 1）的那一下 keyup，返回上屏文本。
fn caps_on_commit(c: &Coordinator) -> String {
    let mut ev = kev(keymap::VK_CAPITAL, EVENT_KEY_UP);
    ev.toggles = 0x01;
    match c.handle_key_event(&ev) {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("开大写应上屏编码，实际: {other:?}"),
    }
}

fn coord_commit_upper(on: bool) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.capslock.commit_upper = on;
    Coordinator::new_headless(cfg, None)
}

#[test]
fn caps_on_commits_code_uppercase_when_enabled() {
    let c = coord_commit_upper(true);
    c.state.lock().unwrap().input_buffer = "wps".to_string();
    assert_eq!(caps_on_commit(&c), "WPS");
    assert!(c.state.lock().unwrap().caps_lock, "之后是大写状态");
}

#[test]
fn caps_on_commits_code_as_typed_by_default() {
    let c = coord_commit_upper(false);
    c.state.lock().unwrap().input_buffer = "wps".to_string();
    assert_eq!(caps_on_commit(&c), "wps");
}

/// 已转换前缀是选定的字词，不转大写。
#[test]
fn caps_on_upper_keeps_committed_prefix() {
    let c = coord_commit_upper(true);
    {
        let mut s = c.state.lock().unwrap();
        s.committed_text = "用iPhone".to_string();
        s.input_buffer = "wps".to_string();
    }
    assert_eq!(caps_on_commit(&c), "用iPhoneWPS");
}

#[test]
fn caps_on_upper_applies_to_temp_english_buffer() {
    let c = coord_commit_upper(true);
    {
        let mut s = c.state.lock().unwrap();
        s.active = Some(ModeKind::TempEnglish);
        s.temp_english_buffer = "Hello".to_string();
    }
    assert_eq!(caps_on_commit(&c), "HELLO");
}

#[test]
fn caps_on_upper_temp_english_full_width() {
    let c = coord_commit_upper(true);
    {
        let mut s = c.state.lock().unwrap();
        s.active = Some(ModeKind::TempEnglish);
        s.temp_english_buffer = "Hi".to_string();
        s.full_width = true;
    }
    assert_eq!(caps_on_commit(&c), "ＨＩ");
}

/// 字母引导符随原码转大写。
#[test]
fn caps_on_upper_temp_pinyin_letter_guide() {
    let c = coord_commit_upper(true);
    {
        let mut s = c.state.lock().unwrap();
        s.active = Some(ModeKind::TempPinyin);
        s.temp_pinyin_prefix = "z".to_string();
        s.temp_pinyin_buffer = "ni".to_string();
    }
    assert_eq!(caps_on_commit(&c), "ZNI");
}

/// 只按了字母引导符（缓冲空）也转。
#[test]
fn caps_on_upper_temp_pinyin_guide_only() {
    let c = coord_commit_upper(true);
    {
        let mut s = c.state.lock().unwrap();
        s.active = Some(ModeKind::TempPinyin);
        s.temp_pinyin_prefix = "z".to_string();
    }
    assert_eq!(caps_on_commit(&c), "Z");
}

/// CapsLock 被勾成中英切换键（C++ 不带「仅状态通知」标记）时同一支、同一结果。
#[test]
fn caps_on_upper_when_capslock_is_toggle_key() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.capslock.commit_upper = true;
    cfg.keys.toggle_mode_keys = vec!["capslock".to_string()];
    let c = Coordinator::new_headless(cfg, None);
    c.state.lock().unwrap().input_buffer = "wps".to_string();
    let mut ev = kev(keymap::VK_CAPITAL, EVENT_KEY_UP);
    ev.toggles = 0x01;
    ev.modifiers = 0x0100; // KEYMOD_CAPSLOCK，不带 0x8000「仅状态通知」
    match c.handle_key_event(&ev) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "WPS"),
        other => panic!("开大写应上屏编码，实际: {other:?}"),
    }
}

fn coord_caps_toggle_key() -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.keys.toggle_mode_keys = vec!["lshift".to_string(), "capslock".to_string()];
    Coordinator::new_headless(cfg, None)
}

fn caps_up(c: &Coordinator, held_ms: u32) {
    c.handle_capslock_hook_event(wind_keys::capslock_hook::CapsEvent::Down { first: true });
    c.handle_capslock_hook_event(wind_keys::capslock_hook::CapsEvent::Up {
        held_ms,
        interrupted: false,
    });
}

#[test]
fn caps_toggle_key_is_recognized() {
    assert!(coord_caps_toggle_key().capslock_is_toggle_key());
    assert!(!coord_cn().capslock_is_toggle_key());
}

/// CapsLock 作切换键：短按切中英，再按切回。
#[test]
fn caps_toggle_key_short_press_toggles_mode() {
    let c = coord_caps_toggle_key();
    caps_up(&c, 120);
    assert!(!c.is_chinese_mode(), "短按应切到英文");
    caps_up(&c, 120);
    assert!(c.is_chinese_mode(), "再短按应切回中文");
}

/// 打字中短按当成切英文：编码按 commit_on_switch 处置，不留在缓冲里。
#[test]
fn caps_toggle_key_short_press_while_composing_switches_to_english() {
    let c = coord_caps_toggle_key();
    c.state.lock().unwrap().input_buffer = "wps".to_string();
    caps_up(&c, 120);
    assert!(!c.is_chinese_mode());
    assert!(c.state.lock().unwrap().input_buffer.is_empty());
}

/// 开了 commit_upper：CapsLock 短按切英文时编码同样转大写上屏（经 push 出口，这里看
/// 统计记账的上屏原码）；没开时原样。
#[test]
fn caps_toggle_key_short_press_commit_upper_follows_config() {
    for (on, want) in [(true, "WPS"), (false, "wps")] {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.input.symbol.smart_mode = false;
        cfg.input.capslock.commit_upper = on;
        cfg.keys.toggle_mode_keys = vec!["capslock".to_string()];
        let c = Coordinator::new_headless(cfg, None);
        c.state.lock().unwrap().input_buffer = "wps".to_string();
        c.debug_capture_stat_events();
        caps_up(&c, 120);
        assert!(!c.is_chinese_mode());
        let texts: Vec<String> = c
            .debug_take_stat_events()
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert!(
            texts.iter().any(|t| t == want),
            "commit_upper={on} 应上屏 {want}，实际 {texts:?}"
        );
    }
}

/// 长按是开关大写，不切中英（大写由注入的 CapsLock 经系统翻转，这里只验不切中英）。
#[test]
fn caps_toggle_key_long_press_does_not_toggle_mode() {
    let c = coord_caps_toggle_key();
    caps_up(&c, CAPS_TOGGLE_LONG_PRESS_MS);
    assert!(c.is_chinese_mode());
}

/// 大写开着时短按是关大写，不切中英。
#[test]
fn caps_toggle_key_short_press_with_caps_on_does_not_toggle_mode() {
    let c = coord_caps_toggle_key();
    c.state.lock().unwrap().caps_lock = true;
    caps_up(&c, 120);
    assert!(c.is_chinese_mode());
}

/// 被会话动作用掉的按下，松开时不再切中英。
#[test]
fn caps_toggle_key_claimed_press_does_not_toggle_mode() {
    let c = coord_caps_toggle_key();
    c.caps_press_claimed
        .store(true, std::sync::atomic::Ordering::Relaxed);
    c.handle_capslock_hook_event(wind_keys::capslock_hook::CapsEvent::Up {
        held_ms: 120,
        interrupted: false,
    });
    assert!(c.is_chinese_mode());
}

/// 首个 down 清掉上一次残留的「已被用掉」，这次短按照常切换。
#[test]
fn caps_toggle_key_first_down_clears_stale_claim() {
    let c = coord_caps_toggle_key();
    c.caps_press_claimed
        .store(true, std::sync::atomic::Ordering::Relaxed);
    caps_up(&c, 120);
    assert!(!c.is_chinese_mode());
}

/// 按住期间按了别的键：不算单按，不切中英。
#[test]
fn caps_toggle_key_interrupted_press_does_not_toggle_mode() {
    let c = coord_caps_toggle_key();
    c.handle_capslock_hook_event(wind_keys::capslock_hook::CapsEvent::Down { first: true });
    c.handle_capslock_hook_event(wind_keys::capslock_hook::CapsEvent::Up {
        held_ms: 120,
        interrupted: true,
    });
    assert!(c.is_chinese_mode());
}

/// 密码框里短按是开关大写，不切中英。
#[test]
fn caps_toggle_key_short_press_in_password_field_does_not_toggle_mode() {
    let c = coord_caps_toggle_key();
    c.password_suppress
        .store(true, std::sync::atomic::Ordering::Relaxed);
    caps_up(&c, 120);
    assert!(c.is_chinese_mode());
}

/// 没勾切换键时，钩子（为会话绑定而装）报来的松开什么都不做。
#[test]
fn caps_release_without_toggle_key_does_nothing() {
    let c = coord_cn();
    caps_up(&c, 120);
    assert!(c.is_chinese_mode());
}

/// 只管大小写：`commit_on_switch` 关时照旧丢弃编码。
#[test]
fn caps_on_upper_respects_commit_on_switch_off() {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.capslock.commit_upper = true;
    cfg.keys.commit_on_switch = false;
    let c = Coordinator::new_headless(cfg, None);
    c.state.lock().unwrap().input_buffer = "wps".to_string();
    let mut ev = kev(keymap::VK_CAPITAL, EVENT_KEY_UP);
    ev.toggles = 0x01;
    let act = c.handle_key_event(&ev);
    assert!(
        !matches!(&act, KeyAction::InsertText { text, .. } if !text.is_empty()),
        "不该上屏，实际: {act:?}"
    );
    assert!(c.state.lock().unwrap().input_buffer.is_empty());
}

/// `capslock = "select_candidate:N"` 必须走得到选词出口。
///
/// CapsLock（0x14）与 `VK_LSHIFT..=VK_RCONTROL` 不连号，`handle_select_key_up` 的门若按
/// 那个区间写就挡在门外；`apply_session_action` 对选词类动词又返回 None，绑定成死配置。
#[test]
fn capslock_select_candidate_reaches_select_key_up() {
    let c = coord_with_capslock("select_candidate:3");
    {
        let mut s = c.state.lock().unwrap();
        s.candidates = (0..5)
            .map(|i| Candidate {
                text: format!("候{i}"),
                ..Default::default()
            })
            .collect();
    }
    let act = c.handle_select_key_up(&kev(0x14, EVENT_KEY_UP));
    assert!(
        act.is_some(),
        "CapsLock 的 select_candidate 必须被选词出口接住，而不是落到大写同步"
    );
}

/// 门的值域取 `is_key_up_only_vk` 这个单一真相源，不再各写一份区间。
/// 改回 `VK_LSHIFT..=VK_RCONTROL` 不会报错，只会让 CapsLock 的选词绑定变成死配置。
#[test]
fn select_key_up_gate_covers_all_key_up_only_vks() {
    for vk in [
        keymap::VK_LSHIFT,
        keymap::VK_RSHIFT,
        keymap::VK_LCONTROL,
        keymap::VK_RCONTROL,
        keymap::VK_CAPITAL,
    ] {
        assert!(keymap::is_key_up_only_vk(vk), "{vk:#X} 应属 keyup-only");
    }
}
