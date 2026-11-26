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

/// 命令栏 `last()` 要的是「实际上屏的文本」：简入繁出时屏幕上是繁体，`last()` 就该是繁体，
/// 否则汉典搜索 / 反查拿到的是另一个字（「发」≠「發」）。
fn last_text(c: &Coordinator, n: u32) -> String {
    c.debug_eval_cc_text(&format!("$CC(\"x\", type(last({n})))"))
        .expect("前提：纯文本命令应能同步求值")
}

#[test]
fn last_returns_converted_text_when_s2t_on() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    let c = open(&data_dir());
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t);
    assert_eq!(commit_hanzi(&c), "漢字", "前提：简入繁出已生效");
    assert_eq!(last_text(&c, 1), "漢字", "last() 应是屏幕上实际上屏的繁体");
}

/// 上屏当时是什么样，历史就记什么样：事后关掉转换，`last()` 不跟着变回简体。
#[test]
fn last_keeps_committed_form_after_conversion_turned_off() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    let c = open(&data_dir());
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t);
    assert_eq!(commit_hanzi(&c), "漢字");
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t);
    assert_eq!(last_text(&c, 1), "漢字");
}

/// 转换关着时 `last()` 与上屏一致（回归护栏，现状即满足）。
#[test]
fn last_is_plain_when_conversion_off() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    let c = open(&data_dir());
    assert_eq!(commit_hanzi(&c), "汉字");
    assert_eq!(last_text(&c, 1), "汉字");
}

/// 重复上屏的事实源仍是转换前的简体：它上屏时会再过一次转换，记转换后的形态会二次转换。
#[test]
fn repeat_candidate_still_uses_unconverted_text() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    let c = open(&data_dir());
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t);
    assert_eq!(commit_hanzi(&c), "漢字");
    c.handle_key_event(&key(0xBA, 0)); // `;` 进快捷输入，空缓冲注入重复上屏候选
    assert_eq!(c.debug_active_mode(), Some("mix"), "前提：`;` 进快捷输入");
    assert_eq!(
        c.debug_page_texts().first().map(String::as_str),
        Some("汉字"),
        "重复上屏候选取转换前的简体"
    );
}

/// 配置里本来就开着简入繁出（启动即生效，没有经过开关切换）：上屏后 `last()` 也要是繁体，
/// 事后切回简体不改变它。
#[test]
fn last_is_converted_when_s2t_on_from_config_at_startup() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    let _ = std::fs::remove_file(root().join("userdata/config.toml"));
    let mut cfg = Config::default();
    cfg.schema.active = "pinyin".into();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.input.default.chinese_mode = true;
    cfg.input.s2t.enabled = true;
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    assert_eq!(
        c.debug_conversion().0,
        (true, false),
        "前提：启动即开简入繁出"
    );
    assert_eq!(commit_hanzi(&c), "漢字", "前提：上屏为繁体");
    assert_eq!(last_text(&c, 1), "漢字", "启动即开时 last() 也应是繁体");
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t); // 切回简体
    assert_eq!(last_text(&c, 1), "漢字", "切回简体不改变已上屏的 last()");
}

/// 放行首显闸门后，取此后下发的最近一次候选页文本。
fn pushed_candidate_texts(
    c: &Coordinator,
    rx: &std::sync::mpsc::Receiver<wind_ui_types::UiCommand>,
) -> Option<Vec<String>> {
    c.handle_caret_update(&wind_bridge::handler::CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    rx.try_iter()
        .filter_map(|cmd| match cmd {
            wind_ui_types::UiCommand::UpdateCandidates { candidates, .. } => Some(
                candidates
                    .iter()
                    .map(|i| i.text.as_str().to_string())
                    .collect(),
            ),
            _ => None,
        })
        .last()
}

/// 组合中用热键切简繁：候选窗要当场重绘成新的形态，不能等下一次按键。
#[test]
fn hotkey_toggle_refreshes_candidate_window_while_composing() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    let _ = std::fs::remove_file(root().join("userdata/config.toml"));
    let mut cfg = Config::default();
    cfg.schema.active = "pinyin".into();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.input.default.chinese_mode = true;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    for ch in "hanzi".chars() {
        c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
    let before = pushed_candidate_texts(&c, &rx).expect("前提：组合中应下发候选页");
    assert!(
        before.iter().any(|t| t == "汉字"),
        "前提：简体态: {before:?}"
    );
    c.handle_key_event(&key(VK_J, MOD_CTRL | MOD_SHIFT));
    let after = rx
        .try_iter()
        .filter_map(|cmd| match cmd {
            wind_ui_types::UiCommand::UpdateCandidates { candidates, .. } => Some(
                candidates
                    .iter()
                    .map(|i| i.text.as_str().to_string())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .last()
        .expect("切换简繁应当场重下发候选页");
    assert!(after.iter().any(|t| t == "漢字"), "应显示繁体: {after:?}");
}

/// `cozd` 的提示文本（`汉典:{last(1)}`）显示实际上屏的形态：先上屏「漢字」再切回简体，
/// 提示仍是「漢字」，与执行时搜索的字一致。
#[test]
fn cozd_label_shows_committed_form() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库 / opencc 数据");
        return;
    }
    let c = open(&data_dir());
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t);
    assert_eq!(commit_hanzi(&c), "漢字", "前提：繁体上屏");
    c.debug_run_menu_cmd(MenuCmd::ToggleS2t); // 切回简体
    // 出厂 `cozd`（system.phrases.toml）：该环境不加载短语层，原样装一条进来。
    c.debug_install_phrases(vec![wind_phrase::PhraseSeed {
        code: "cozd".into(),
        text: r#"$CC("汉典:{last(1)}", open("https://www.zdic.net/hans/{url(last(1))}"))"#.into(),
        weight: 2000,
        position: 0,
        is_system: true,
        category: String::new(),
    }]);
    for ch in "cozd".chars() {
        c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
    let shown = c.debug_page_display_texts();
    assert!(
        shown.iter().any(|t| t == "汉典:漢字"),
        "提示应是上屏形态「汉典:漢字」: {shown:?}"
    );
}
