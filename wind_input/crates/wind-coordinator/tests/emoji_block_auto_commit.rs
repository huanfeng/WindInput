//! `input.emoji.block_auto_commit`：候选里有追加出来的 emoji / 附加模块符号时，不做全码唯一
//! 自动上屏（码表与短语两侧）。出厂关，行为同既往。
//!
//! emoji 表只从 `Config::data_dir()`（安装根）加载（headless 构造器也不加载，经
//! `debug_sync_emoji_dict` 补上），不认构造时注入的数据目录，故整棵目录树经
//! 便携标记重定向到临时目录（`WIND_INSTALL_ROOT` + `portable_mode`），`data` 链到
//! `build_dev/data`。一个进程只能重定向一次，故单开测试二进制、用例经 `LOCK` 串行。
//!
//! ⚠️ 依赖 `build_dev/data`；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

static LOCK: Mutex<()> = Mutex::new(());

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("emoji/frost_emoji.txt").exists()
}

/// 便携根目录（进程内只建一次）：`data` 链到 `build_dev/data`，缓存与用户层落在根下。
fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        // 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
        let root = std::env::temp_dir().join(format!("wind_emoji_block_ac-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("userdata")).unwrap();
        let data = data_dir().canonicalize().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&data, root.join("data")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&data, root.join("data")).unwrap();
        std::fs::write(root.join(wind_config::variant::PORTABLE_MARKER_NAME), "").unwrap();
        // SAFETY: 在任何路径缓存初始化之前设置；用例先经 `LOCK` 串行。
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

/// wubi86 + 全码唯一自动上屏 + emoji 扩展（出厂附加模块含白霜 Emoji）；`block` 即新选项。
fn open(tag: &str, block: bool, phrase: Option<(&str, &str)>) -> Arc<Coordinator> {
    root();
    let store_path = root().join(format!("{tag}.redb"));
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());
    if let Some((code, text)) = phrase {
        store.add_phrase(code, text, 0, 1000).unwrap();
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = true;
    cfg.input.emoji.enabled = true;
    cfg.input.emoji.block_auto_commit = block;
    let c = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    c.debug_sync_emoji_dict();
    c
}

/// 逐键敲完 `code`，返回末键的动作与其后的候选面。
fn type_code(c: &Coordinator, code: &str) -> (KeyAction, Vec<String>) {
    let mut last = None;
    for ch in code.chars() {
        let vk = (ch.to_ascii_uppercase() as u32) & 0xFF;
        last = Some(c.handle_key_event(&KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
        }));
    }
    (last.unwrap(), c.debug_all_candidate_texts())
}

fn committed(a: &KeyAction) -> Option<&str> {
    match a {
        KeyAction::InsertText { text, .. } => Some(text),
        _ => None,
    }
}

/// 出厂关：`aarx` 戒指（白霜 Emoji 补 💍）照旧全码唯一自动上屏。
#[test]
fn off_keeps_auto_commit_with_appended_candidate() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 五笔方案 / emoji 数据");
        return;
    }
    let c = open("off_ct", false, None);
    let (a, cands) = type_code(&c, "aarx");
    assert_eq!(
        committed(&a),
        Some("戒指"),
        "选项关时应照旧自动上屏；候选面={cands:?}"
    );
}

/// 开启：首选挂了追加候选 ⇒ 不自动上屏，追加的 💍 留在候选面上；不挂的码照常上屏。
#[test]
fn on_blocks_codetable_auto_commit_only_when_appended() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 五笔方案 / emoji 数据");
        return;
    }
    let c = open("on_ct", true, None);
    let (a, cands) = type_code(&c, "aarx");
    assert!(
        committed(&a).is_none(),
        "有追加候选时不得自动上屏，实际 {a:?}；候选面={cands:?}"
    );
    assert_eq!(cands.first().map(String::as_str), Some("戒指"));
    assert!(
        cands.iter().any(|t| t.contains('💍')),
        "追加的 emoji 应在候选面上：{cands:?}"
    );

    let c = open("on_ct_plain", true, None);
    let (a, cands) = type_code(&c, "aaad");
    assert_eq!(
        committed(&a),
        Some("工期"),
        "没有追加候选的唯一码应照常自动上屏；候选面={cands:?}"
    );
}

/// 短语侧同口径：唯一精确码短语挂了追加候选，选项关照常上屏、开则不上屏。
#[test]
fn phrase_auto_commit_follows_the_same_option() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !has_data() {
        eprintln!("跳过：缺 build_dev 五笔方案 / emoji 数据");
        return;
    }
    // `xxkm` 在五笔码表里无词条、也无更长后继，短语是唯一候选；「工厂」→ 🏭。
    let c = open("off_ph", false, Some(("xxkm", "工厂")));
    let (a, cands) = type_code(&c, "xxkm");
    assert_eq!(
        committed(&a),
        Some("工厂"),
        "选项关时短语照常自动上屏；候选面={cands:?}"
    );

    let c = open("on_ph", true, Some(("xxkm", "工厂")));
    let (a, cands) = type_code(&c, "xxkm");
    assert!(
        committed(&a).is_none(),
        "短语挂了追加候选时不得自动上屏，实际 {a:?}；候选面={cands:?}"
    );
    assert!(cands.iter().any(|t| t.contains('🏭')), "候选面={cands:?}");
}
