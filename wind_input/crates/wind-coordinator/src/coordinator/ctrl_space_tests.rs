use super::*;
use wind_ipc::protocol::ModeSwitchSource as Src;

const ALL_SOURCES: [Src; 5] = [
    Src::Unknown,
    Src::CompartmentOpenClose,
    Src::CompartmentConversion,
    Src::CtrlSpaceKey,
    Src::Menu,
];

/// 出厂（开）：任何来源、任何方向都不拒绝——升级后行为必须逐字不变。
#[test]
fn enabled_never_rejects() {
    for src in ALL_SOURCES {
        for (req, cur, ctrl) in [
            (false, true, true),
            (true, false, true),
            (false, true, false),
        ] {
            assert!(
                !ctrl_space_switch_rejected(true, req, cur, src, ctrl),
                "{src:?}"
            );
        }
    }
}

/// 关闭后的矩阵：只拒「用户按出来的翻转」。
#[test]
fn disabled_matrix() {
    for (req, cur) in [(false, true), (true, false)] {
        // compartment / 旧版 DLL：Ctrl 按住才算 Ctrl+空格。
        for src in [
            Src::CompartmentOpenClose,
            Src::CompartmentConversion,
            Src::Unknown,
        ] {
            assert!(
                ctrl_space_switch_rejected(false, req, cur, src, true),
                "{src:?}+Ctrl"
            );
            assert!(
                !ctrl_space_switch_rejected(false, req, cur, src, false),
                "{src:?} 无 Ctrl 是宿主写的"
            );
        }
        // 按键侧兜底：恒算用户按的。
        assert!(ctrl_space_switch_rejected(
            false,
            req,
            cur,
            Src::CtrlSpaceKey,
            false
        ));
        assert!(ctrl_space_switch_rejected(
            false,
            req,
            cur,
            Src::CtrlSpaceKey,
            true
        ));
        // 菜单：用户明确点了目标，永远照办。
        assert!(!ctrl_space_switch_rejected(
            false,
            req,
            cur,
            Src::Menu,
            true
        ));
    }
    // 不是翻转（请求 == 当前）：无所谓拒绝。
    for src in ALL_SOURCES {
        assert!(!ctrl_space_switch_rejected(false, true, true, src, true));
    }
}

fn msg(v: bool) -> Vec<u8> {
    wind_ipc::codec::encode_sync_config(
        wind_ipc::protocol::CONFIG_KEY_CTRL_SPACE_TOGGLE,
        &wind_ipc::codec::encode_ctrl_space_toggle_value(v),
    )
}

/// 推送值 = 有效值：开关与 `key_actions` 的 `none` 两个入口都要反映到 DLL。
#[test]
fn push_carries_effective_value() {
    let c = Coordinator::new_headless(Config::default(), None);
    let tok = (700u64 << 32) | 1;
    let cap = c.push_server.attach_capture_client(tok);

    c.push_ctrl_space_toggle_config(tok);
    assert_eq!(
        cap.try_iter().collect::<Vec<_>>(),
        vec![msg(true)],
        "出厂推开"
    );

    c.refresh_config_in_memory(|cfg| cfg.keys.ctrl_space_toggle = false);
    c.push_ctrl_space_toggle_config(0);
    assert_eq!(
        cap.try_iter().collect::<Vec<_>>(),
        vec![msg(false)],
        "关开关 ⇒ 广播关"
    );

    c.refresh_config_in_memory(|cfg| {
        cfg.keys.ctrl_space_toggle = true;
        cfg.keys
            .key_actions
            .insert("ctrl+space".into(), "none".into());
    });
    c.push_ctrl_space_toggle_config(tok);
    assert_eq!(
        cap.try_iter().collect::<Vec<_>>(),
        vec![msg(false)],
        "自定义按键绑 none ⇒ 同样推关"
    );
}

/// 造一个「中文 + CapsLock 开 + 正在组码」的现场，开关按 `toggle` 设。
fn caps_and_composing(toggle: bool) -> std::sync::Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.capslock.cancel_on_mode_switch = true;
    cfg.keys.ctrl_space_toggle = toggle;
    let c = Coordinator::new_headless(cfg, None);
    {
        let mut s = c.state.lock().unwrap();
        s.caps_lock = true;
        s.input_buffer = "nihao".into();
    }
    assert!(c.is_chinese_mode(), "前提：中文起步");
    c
}

/// 被拒的 Ctrl+空格 是「什么都没发生」：不取消大写、不动组码缓冲、不翻模式。
///
/// 拒绝块必须排在 `cancel_caps_on_switch` 与 `take_input_on_mode_switch` **之前**——挪到
/// 后面的话，用户按了一个已关闭的键，大写被关掉、正在打的编码被上屏/丢弃，模式却没切。
/// 取消大写的可观测量是注入防抖戳 `last_caps_inject`：它在真正注入**之前**落下，Linux 上
/// 注入失败也照样有戳，因此对照组（开关开着）在本机同样能看到它被置位。
#[test]
fn rejected_switch_leaves_caps_and_composition_alone() {
    use wind_bridge::handler::MessageHandler;
    let c = caps_and_composing(false);
    let (status, commit) = c.handle_system_mode_switch(false, Src::CompartmentOpenClose, true);
    assert!(status.expect("拒绝也回包").chinese_mode);
    assert!(commit.is_empty(), "拒绝时不得上屏编码");
    assert!(c.is_chinese_mode());
    assert!(
        c.last_caps_inject.lock().unwrap().is_none(),
        "拒绝时不得尝试取消大写"
    );
    let s = c.state.lock().unwrap();
    assert!(s.caps_lock, "大写镜像不变");
    assert_eq!(s.input_buffer, "nihao", "组码缓冲原样保留");
}

/// 对照组：开关开着时同一现场会取消大写、收走缓冲——证明上一条的断言不是恒真。
#[test]
fn accepted_switch_does_cancel_caps_and_take_input() {
    use wind_bridge::handler::MessageHandler;
    let c = caps_and_composing(true);
    c.handle_system_mode_switch(false, Src::CompartmentOpenClose, true);
    assert!(
        c.last_caps_inject.lock().unwrap().is_some(),
        "开着时应尝试取消大写"
    );
    assert!(
        c.state.lock().unwrap().input_buffer.is_empty(),
        "开着时切换应收走组码缓冲"
    );
}
