//! 英文候选的位次下限（码表 `english_merge.{exact,complete}_position`、混输
//! `schema.mix.english_{exact,complete}_position`）经 `EngineManager::active_english_positions`
//! 送到协调器的 `place_english_after_common_exact`；拼音方案不受影响。
//! 词库缺失时跳过（worktree 里要自备 `build_dev/data`）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn ready(schema: &str) -> bool {
    let d = data_dir();
    let ok = d.join(format!("schemas/{schema}.schema.toml")).exists()
        && d.join("schemas/english/en.dict.yaml").exists();
    if !ok {
        eprintln!("跳过：缺少 {schema} 方案或英文词库");
    }
    ok
}

fn key(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn base(schema: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.into()];
    cfg.schema.active = schema.into();
    cfg.input.default.chinese_mode = true;
    cfg
}

/// 打 `input`，返回英文 `word` 在首页的下标与首页条数。
fn english_index(cfg: Config, input: &str, word: &str, tag: &str) -> (usize, usize) {
    let path = std::env::temp_dir().join(format!("wind_en_pos_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for c in input.chars() {
        coord.handle_key_event(&key(c.to_ascii_uppercase() as u32));
    }
    let page = coord.debug_page_texts();
    let i = page
        .iter()
        .position(|t| t == word)
        .unwrap_or_else(|| panic!("首页应有英文候选 {word}，实际 {page:?}"));
    (i, page.len())
}

fn wubi(f: impl FnOnce(&mut Config)) -> Config {
    let mut cfg = base("wubi86");
    cfg.schema.codetable.english_merge.enable = true;
    f(&mut cfg);
    cfg
}

/// 回归：五笔打 `git`（无中文全码）出厂下完整单词英文排第 1；`exact_position` 能把它压后。
#[test]
fn codetable_exact_position() {
    if !ready("wubi86") {
        return;
    }
    let (natural, len) = english_index(wubi(|_| {}), "git", "git", "ct_default");
    assert_eq!(natural, 0, "五笔 git 没有中文全码，出厂下英文 git 应排第 1");
    assert!(len >= 4, "前提：首页得有足够的中文候选（{len}）");
    let (placed, _) = english_index(
        wubi(|c| c.schema.codetable.english_merge.exact_position = 3),
        "git",
        "git",
        "ct_floor",
    );
    assert_eq!(placed, 2, "exact_position = 3 应压到第 3 位");
    // 0 回退出厂值 1。
    let (zero, _) = english_index(
        wubi(|c| c.schema.codetable.english_merge.exact_position = 0),
        "git",
        "git",
        "ct_zero",
    );
    assert_eq!(zero, 0);
}

/// 混输的位次下限同样经协调器生效。
#[test]
fn mixed_exact_position() {
    if !ready("wubi86_pinyin") {
        return;
    }
    let mix = |pos: usize| {
        let mut cfg = base("wubi86_pinyin");
        cfg.schema.mix.enable_english = true;
        cfg.schema.mix.english_exact_position = pos;
        cfg
    };
    let (natural, len) = english_index(mix(1), "git", "git", "mix_default");
    assert!(
        natural + 2 < len,
        "前提：英文后面还得有中文（{natural} / {len}）"
    );
    let (placed, _) = english_index(mix(natural + 3), "git", "git", "mix_floor");
    assert_eq!(
        placed,
        natural + 2,
        "english_exact_position 应把英文压到第 {} 位",
        natural + 3
    );
}

/// 拼音方案不受码表 / 混输的位次配置影响。
#[test]
fn pinyin_ignores_position_config() {
    if !ready("pinyin") {
        return;
    }
    let py = |pos: usize| {
        let mut cfg = base("pinyin");
        cfg.schema.pinyin.english_merge.enable = true;
        cfg.schema.codetable.english_merge.exact_position = pos;
        cfg.schema.codetable.english_merge.complete_position = pos;
        cfg.schema.mix.english_exact_position = pos;
        cfg.schema.mix.english_complete_position = pos;
        cfg
    };
    let (natural, len) = english_index(py(1), "hello", "hello", "py_default");
    assert!(
        natural + 2 < len,
        "前提：英文后面还得有中文（{natural} / {len}）"
    );
    assert_eq!(
        english_index(py(natural + 3), "hello", "hello", "py_floor").0,
        natural
    );
}

/// 混输分段续转：选走拼音「你」后，剩下的 `hello` 交给拼音方案转换（`convert_with`，不混英文）。
/// 混输的位次配置不得作用到这一页上——两种配置下续转页逐条相同。
#[test]
fn mixed_pinyin_continuation_ignores_position_config() {
    if !ready("wubi86_pinyin") || !ready("pinyin") {
        return;
    }
    const VK_1: u32 = 0x31;
    let run = |pos: usize, tag: &str| -> Vec<String> {
        let mut cfg = base("wubi86_pinyin");
        cfg.schema.mix.enable_english = true;
        cfg.schema.mix.english_exact_position = pos;
        cfg.schema.mix.english_complete_position = pos;
        let path = std::env::temp_dir().join(format!("wind_en_pos_cont_{tag}.redb"));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
        for c in "nihello".chars() {
            coord.handle_key_event(&key(c.to_ascii_uppercase() as u32));
        }
        let page = coord.debug_page_texts();
        let i = page
            .iter()
            .position(|t| t == "你")
            .unwrap_or_else(|| panic!("前提：nihello 首页应有「你」，实际 {page:?}"));
        coord.handle_key_event(&key(VK_1 + i as u32));
        coord.debug_page_texts()
    };
    let base_page = run(1, "p1");
    assert!(
        base_page.iter().any(|t| t == "和理论哦"),
        "前提：续转后应是 hello 的拼音页，实际 {base_page:?}"
    );
    assert_eq!(run(5, "p5"), base_page, "续转页不该随混输位次配置变化");
}
