//! 码表空码组词（`[engine.codetable] compose_input`）的端到端验证：真实五笔词库 + 拼音词库。
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

struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 字段顺序即析构顺序：先放掉协调器，再删目录。
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

fn coord_for(schema: &str, tag: &str, body: &str) -> Fixture {
    coord_with(schema, tag, body, |_| {})
}

fn coord_with(schema: &str, tag: &str, body: &str, tweak: impl FnOnce(&mut Config)) -> Fixture {
    let root = std::env::temp_dir().join(format!("wind_compose_{tag}_{}", std::process::id()));
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
    tweak(&mut c);
    let coord =
        Coordinator::new_headless_with_store_override(c, Some(&data_dir()), store, Some(ov));
    Fixture {
        coord,
        _root: TempRoot(root),
    }
}

fn press(coord: &Coordinator, code: &str) -> KeyAction {
    let mut last = None;
    for ch in code.chars() {
        last = Some(coord.handle_key_event(&key(u32::from(ch.to_ascii_uppercase()))));
    }
    last.unwrap()
}

const ON: &str = "compose_input = true\n";

#[test]
fn empty_code_shows_composed_words_only_when_enabled() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let on = coord_for("wubi86", "on", ON);
    press(&on, "wfbn");
    // `wfbn` 是「付了」的词组码，码表没收这个词。
    let page = on.debug_page_texts();
    assert!(page.iter().any(|t| t == "付了"), "{page:?}");

    let off = coord_for("wubi86", "off", "");
    press(&off, "wfbn");
    assert!(off.debug_page_texts().is_empty(), "出厂不开：空码仍是空的");
}

#[test]
fn top_code_does_not_commit_composed_word() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    // `Config::default()` 的顶码是关的（出厂开是 config.toml 给的），这里显式打开。
    let c = coord_for(
        "wubi86",
        "top",
        "compose_input = true\ntop_code_commit = true\n",
    );
    press(&c, "wfbn");
    assert!(!c.debug_page_texts().is_empty(), "前置：出了组词候选");
    let act = press(&c, "q");
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "第 5 码不把组词候选顶上屏，实际 {act:?}"
    );
    assert_eq!(c.debug_input_buffer(), "wfbnq", "接着组合，同未开组词时");
}

#[test]
fn mixed_input_keeps_pinyin_ahead_of_composed_words() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let c = coord_for("wubi86_pinyin", "mixed", ON);
    // `yige` 五笔空码、又是拼音「一个」：组词候选（「词汇表」）沉在全部拼音之后。
    press(&c, "yige");
    let all: Vec<String> = c
        .debug_candidate_triples()
        .into_iter()
        .map(|t| t.0)
        .collect();
    assert_eq!(all.first().map(String::as_str), Some("一个"), "{all:?}");
    assert_eq!(all.last().map(String::as_str), Some("词汇表"), "{all:?}");
    c.handle_key_event(&key(wind_keys::keymap::VK_ESCAPE));
    // 拼音不认的五笔空码：混输下照样出组词。
    press(&c, "wfbn");
    assert!(
        c.debug_page_texts().iter().any(|t| t == "付了"),
        "{:?}",
        c.debug_page_texts()
    );
}

/// 组词不能把英文从前缀补全页降成只收整词：`fina` 五笔空码、组出「教学改革」，英文补全照常给。
#[test]
fn composed_words_do_not_shrink_english_completions() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let c = coord_with("wubi86", "en", ON, |c| {
        c.schema.codetable.english_merge.enable = true
    });
    press(&c, "fina");
    let all: Vec<String> = c
        .debug_candidate_triples()
        .into_iter()
        .map(|t| t.0)
        .collect();
    assert!(
        all.iter().any(|t| t == "financial"),
        "英文前缀补全：{all:?}"
    );
    assert_eq!(
        all.last().map(String::as_str),
        Some("教学改革"),
        "组词在英文之后：{all:?}"
    );
}
