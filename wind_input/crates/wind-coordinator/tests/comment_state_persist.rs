//! 注释持久态写回：`set_comment_state` 写用户层 `ui.comment.state`，重启后仍生效。
//!
//! 用户目录经 `WIND_DATADIR_CONF` 重定向到临时目录，一个进程只能重定向一次，故单开一个
//! 测试二进制（也覆盖命令栏 `ime.toggle` / `ime.set` 的 `comment` 目标、Ctrl 单击的 persist 档）。
//! 夹具自造；只有 Ctrl 单击那段要候选，借 `build_dev/data`，缺了就跳过。

use std::path::Path;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::comment_lib::{CommentState, SwitchKey, SwitchMode};
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP, MOD_CTRL, MOD_LCTRL};

fn user_value(user: &Path) -> Option<String> {
    let text = std::fs::read_to_string(user.join("config.toml")).ok()?;
    let v: toml::Value = toml::from_str(&text).unwrap();
    Some(
        v.get("ui")?
            .get("comment")?
            .get("state")?
            .as_str()?
            .to_string(),
    )
}

#[test]
fn set_comment_state_and_commands_write_back() {
    let tmp = std::env::temp_dir().join(format!("wind_coord_comment_state-{}", std::process::id()));
    let root = tmp.join("install");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // SAFETY: 本文件仅此一个测试函数，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录须已重定向，否则本测试会读写真实用户配置"
    );

    let load = || Config::load(Config::data_dir().as_deref()).unwrap();
    let coord = Coordinator::new_headless(load(), None);
    assert_eq!(coord.debug_effective_comment_state(), CommentState::Default);

    // 先翻转一次：设持久态要把它清掉，否则看到的是「详细」取反。
    coord.debug_toggle_comment_session();
    assert_eq!(coord.debug_effective_comment_state(), CommentState::Detail);
    coord.debug_set_comment_state(CommentState::Detail).unwrap();
    assert_eq!(user_value(&user).as_deref(), Some("detail"), "应写回用户层");
    assert_eq!(
        coord.debug_effective_comment_state(),
        CommentState::Detail,
        "内存配置当场生效，且翻转已清"
    );

    drop(coord);
    let coord = Coordinator::new_headless(load(), None);
    assert_eq!(
        coord.debug_effective_comment_state(),
        CommentState::Detail,
        "重建协调器后仍是详细"
    );

    // 命令栏 `ime.toggle("comment")`：持久态翻转并写回；再来一次翻回。
    coord.debug_run_command(r#"$CC("切注释", ime.toggle("comment"))"#);
    assert_eq!(coord.debug_effective_comment_state(), CommentState::Default);
    assert_eq!(
        user_value(&user).as_deref(),
        Some("default"),
        "应写回用户层"
    );
    coord.debug_run_command(r#"$CC("切注释", ime.toggle("comment"))"#);
    assert_eq!(coord.debug_effective_comment_state(), CommentState::Detail);
    assert_eq!(user_value(&user).as_deref(), Some("detail"));

    // `ime.set("comment", ...)`：直达指定态；非法值不改动。
    coord.debug_run_command(r#"$CC("设注释", ime.set("comment", "default"))"#);
    assert_eq!(user_value(&user).as_deref(), Some("default"));
    coord.debug_run_command(r#"$CC("设注释", ime.set("comment", "on"))"#);
    assert_eq!(
        user_value(&user).as_deref(),
        Some("default"),
        "非法值不改动"
    );
    drop(coord);

    // Ctrl 单击（persist 档）：有候选时翻转持久态并写回。词典缺失时跳过这一段。
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if data.join("schemas/wubi86.schema.toml").exists() {
        let mut cfg = load();
        cfg.schema.available = vec!["wubi86".into()];
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        cfg.input.association.kind = "off".into();
        cfg.ui.comment.switch_key = SwitchKey::Ctrl;
        cfg.ui.comment.switch_mode = SwitchMode::Persist;
        let coord = Coordinator::new_headless(cfg, Some(&data));
        coord.prewarm_indexes();
        for ch in ['G', 'G'] {
            coord.handle_key_event(&key(ch as u32, 0, EVENT_KEY_DOWN));
        }
        assert!(coord.debug_candidate_count() > 0, "前置条件：gg 有候选");
        let act = coord.handle_key_event(&key(0xA2, MOD_CTRL | MOD_LCTRL, EVENT_KEY_UP));
        assert!(matches!(act, KeyAction::Consumed), "实得 {act:?}");
        assert_eq!(coord.debug_effective_comment_state(), CommentState::Detail);
        assert_eq!(
            user_value(&user).as_deref(),
            Some("detail"),
            "Ctrl 单击应写回用户层"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

fn key(key_code: u32, modifiers: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}
