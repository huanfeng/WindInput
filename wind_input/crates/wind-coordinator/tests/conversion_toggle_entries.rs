//! 简繁切换入口（热键 / 菜单）在「转换器按需加载」下仍然可用（`docs/design/memory-footprint.md`
//! §4.2）：出厂两个方向都不预载，切换入口先同步加载再切；数据缺失时提示且**不切**。
//!
//! 切换会把开关落盘（`input.s2t.enabled`），故整棵目录树经便携标记重定向到临时目录
//! （`WIND_INSTALL_ROOT` + `portable_mode`），一个进程只能重定向一次，故单开测试二进制、
//! 用例经 `LOCK` 串行。词库与 opencc 数据取 `build_dev/data`（构造时显式传入）。
//!
//! ⚠️ 依赖 `build_dev/data`；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL, MOD_SHIFT};
use wind_ui_types::MenuCmd;

static LOCK: Mutex<()> = Mutex::new(());

const VK_J: u32 = 0x4A;
const VK_SPACE: u32 = 0x20;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir().join("schemas/pinyin.schema.toml").exists()
        && data_dir().join("opencc/STPhrases.octrie").exists()
}

/// 便携根目录（进程内只建一次）：用户层落在 `<root>/userdata`，不碰真实用户配置。
fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        // ⚠️ 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
        let root = std::env::temp_dir().join(format!("wind_conv_toggle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("userdata")).unwrap();
        std::fs::write(root.join(wind_config::variant::PORTABLE_MARKER_NAME), "").unwrap();
        // SAFETY: 在任何 OnceLock（variant / 路径缓存）初始化之前设置；用例先经 `LOCK` 串行。
        unsafe {
            std::env::set_var("WIND_INSTALL_ROOT", &root);
        }
        assert_eq!(
            Config::user_config_dir(),
            Some(root.join("userdata")),
            "前置条件：用户目录须已重定向，否则本测试会读写真实用户配置"
        );
        root
    })
}

fn open(data: &Path) -> Arc<Coordinator> {
    let _ = std::fs::remove_file(root().join("userdata/config.toml"));
    let mut c = Config::default();
    c.schema.active = "pinyin".into();
    c.schema.available = vec!["pinyin".into()];
    c.input.default.chinese_mode = true;
    Coordinator::new_headless(c, Some(data))
}

fn key(vk: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 打 `hanzi` 并上屏「汉字」那条（数字键选），返回上屏文本。
fn commit_hanzi(c: &Coordinator) -> String {
    for ch in "hanzi".chars() {
        c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
    let pos = c
        .debug_page_texts()
        .iter()
        .position(|t| t == "汉字")
        .expect("拼音 hanzi 首页应有「汉字」");
    let act = if pos == 0 {
        c.handle_key_event(&key(VK_SPACE, 0))
    } else {
        c.handle_key_event(&key(0x31 + pos as u32, 0))
    };
    match act {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("应上屏 InsertText，实际 {other:?}"),
    }
}

/// 热键（出厂 `ctrl+shift+j` = toggle_s2t）：开之前没加载，按下后加载并生效。
#[test]
fn hotkey_toggle_loads_converter_and_converts() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    let c = open(&data_dir());
    assert_eq!(
        c.debug_conversion(),
        ((false, false), (false, false)),
        "出厂两向都关、都不预载"
    );
    c.handle_key_event(&key(VK_J, MOD_CTRL | MOD_SHIFT));
    assert_eq!(c.debug_conversion(), ((true, false), (true, false)));
    assert_eq!(commit_hanzi(&c), "漢字", "热键开启简入繁出后应上屏繁体");
}

/// 菜单 / 工具栏 / 命令栏共用的 `handle_menu_command` 路径。
#[test]
fn menu_toggle_loads_converter_and_converts() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    let c = open(&data_dir());
    assert_eq!(c.debug_conversion().1, (false, false), "出厂不预载");
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t);
    assert_eq!(c.debug_conversion(), ((true, false), (true, false)));
    assert_eq!(commit_hanzi(&c), "漢字", "菜单开启简入繁出后应上屏繁体");

    // 关掉：不卸载（切换常来回），上屏回到简体。
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t);
    assert_eq!(c.debug_conversion().0, (false, false));
    assert_eq!(commit_hanzi(&c), "汉字");
}

/// 数据缺失：两条入口都提示且不切（菜单路径此前没有这道检查，会把开关拨上而上屏不转）。
#[test]
fn missing_data_keeps_conversion_off_on_both_entries() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    // 只有方案、没有 opencc 的数据根：schemas 软链过去。
    let bare = root().join("bare_data");
    if !bare.exists() {
        std::fs::create_dir_all(&bare).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(data_dir().join("schemas"), bare.join("schemas")).unwrap();
        #[cfg(not(unix))]
        return;
    }
    let c = open(&bare);
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t);
    assert_eq!(
        c.debug_conversion(),
        ((false, false), (false, false)),
        "菜单：数据缺失不切"
    );
    c.handle_key_event(&key(VK_J, MOD_CTRL | MOD_SHIFT));
    assert_eq!(
        c.debug_conversion(),
        ((false, false), (false, false)),
        "热键：数据缺失不切"
    );
}
