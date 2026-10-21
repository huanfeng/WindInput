//! 码表整句 × 满码自动上屏 / 满码空码清空的端到端验证（论坛 t350）。
//!
//! 口径：整句开启时两者**都让位**（同顶码，判据看功能开没开），否则第 4 码就把正在组的
//! 句子截断——唯一全码当场上屏（`trwu` →「我们」）、空码当场清空（`qwrr`）。
//! 每条都配关整句的反向对照，证明让位来自整句开关而非现场本身。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
}

fn key(k: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

/// 用例结束时删掉临时目录。
struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 协调器 + 它的临时目录。⚠️ 字段顺序即析构顺序：先放掉协调器（及其 store / 引擎），
/// 再删目录——反过来就是在删一个还开着的库。
struct Fixture {
    coord: Arc<Coordinator>,
    _root: TempRoot,
}

impl std::ops::Deref for Fixture {
    type Target = Coordinator;
    fn deref(&self) -> &Coordinator {
        &self.coord
    }
}

/// 方案 `schema` 的 `[engine.codetable]` 段写 `body`；目录名带 tag + pid，并行用例 /
/// 并行的 `cargo test` 不互相删库。
fn coord_for(schema: &str, tag: &str, body: &str, global_auto: bool) -> Fixture {
    let root = std::env::temp_dir().join(format!("wind_sentence_ac_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ov = root.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join(format!("{schema}.toml")),
        format!("[engine.codetable]\n{body}"),
    )
    .unwrap();
    let store = Arc::new(Store::open(root.join("user_data.db")).unwrap());
    let mut c = Config::default();
    c.schema.available = vec![schema.into()];
    c.schema.active = schema.into();
    c.input.default.chinese_mode = true;
    c.schema.codetable.auto_commit_at_full = global_auto;
    let coord =
        Coordinator::new_headless_with_store_override(c, Some(&data_dir()), store, Some(ov));
    Fixture {
        coord,
        _root: TempRoot(root),
    }
}

fn coord(tag: &str, body: &str) -> Fixture {
    coord_for("wubi86", tag, body, false)
}

/// 逐键敲 `code`，返回最后一键的动作。
fn press(coord: &Coordinator, code: &str) -> KeyAction {
    let mut last = None;
    for ch in code.chars() {
        last = Some(coord.handle_key_event(&key(u32::from(ch.to_ascii_uppercase()))));
    }
    last.unwrap()
}

const AUTO: &str = "auto_commit_at_full = true\n";
const CLEAR: &str = "clear_on_empty_max = true\n";
const SENTENCE: &str = "sentence_input = true\n";

#[test]
fn unique_full_code_does_not_auto_commit_when_sentence_on() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    // 反向对照：关整句时 `trwu`（唯一全码「我们」）第 4 码即上屏。
    let off = coord("auto_off", AUTO);
    match press(&off, "trwu") {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "我们"),
        other => panic!("前提：关整句时 trwu 应满码上屏，实际 {other:?}"),
    }
    let on = coord("auto_on", &format!("{AUTO}{SENTENCE}"));
    let action = press(&on, "trwu");
    assert!(
        !matches!(action, KeyAction::InsertText { .. }),
        "开整句时不得满码上屏，实际 {action:?}"
    );
    assert_eq!(
        on.debug_page_texts().first().map(String::as_str),
        Some("我们")
    );
}

#[test]
fn sentence_survives_auto_commit_at_full() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let coord = coord("auto_long", &format!("{AUTO}{SENTENCE}"));
    // 我 trnt / 们 wuny：连打 8 码，期间任何一键都不得上屏（整句候选本身也不算唯一全码）。
    for ch in "trntwuny".chars() {
        let action = coord.handle_key_event(&key(u32::from(ch.to_ascii_uppercase())));
        assert!(
            !matches!(action, KeyAction::InsertText { .. }),
            "第 {ch} 键不该自动上屏，实际 {action:?}"
        );
    }
    // 只钉「句子还在、以我们开头」：`wuny` 末尾的 `y` 怎么切归解码质量，不归本条。
    let page = coord.debug_page_texts();
    assert!(
        page.first().is_some_and(|t| t.starts_with("我们")),
        "整句应仍在组，实际 {page:?}"
    );
}

#[test]
fn empty_full_code_does_not_clear_when_sentence_on() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    // 反向对照：关整句时 `qwrr`（五笔 86 空码）第 4 码即清空。
    let off = coord("clear_off", CLEAR);
    let action = press(&off, "qwrr");
    assert!(
        matches!(action, KeyAction::ClearComposition),
        "前提：关整句时 qwrr 应清空，实际 {action:?}"
    );
    let on = coord("clear_on", &format!("{CLEAR}{SENTENCE}"));
    let action = press(&on, "qwrr");
    assert!(
        matches!(action, KeyAction::UpdateComposition { .. }),
        "开整句时空码不得清空，实际 {action:?}"
    );
}

/// 带拼音子引擎的混输方案写了 `sentence_input` 也不让位：那里整句根本不会触发
/// （超码长归拼音），白白关掉满码上屏只会得到「配置没反应、别的功能却坏了」。
///
/// 满码上屏走全局开关（混输方案的 `[engine.codetable]` 行为项不经 override 折叠到主引擎，
/// 与 `phrase_vs_auto_commit.rs` 的混输夹具同法）。
#[test]
fn mixed_with_pinyin_keeps_auto_commit_despite_sentence_input() {
    if !data_dir()
        .join("schemas/wubi86_pinyin.schema.toml")
        .exists()
    {
        eprintln!("跳过：缺 build_dev 混输方案");
        return;
    }
    let mixed = coord_for("wubi86_pinyin", "mixed", SENTENCE, true);
    match press(&mixed, "trwu") {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "我们"),
        other => panic!("混输带拼音时整句不生效，满码上屏应照常，实际 {other:?}"),
    }
}
