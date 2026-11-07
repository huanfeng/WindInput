//! 码表整句 N-best 的端到端验证：整句块条数、切换键、组合区按高亮候选显示切法。
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
        seq_char: 0,
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
    let root = std::env::temp_dir().join(format!("wind_sentence_nb_{tag}_{}", std::process::id()));
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

/// 连打「我是中国人」的简码：`qj` 既是二简「旬」，也是一简「我」+「是」，天然有备选。
const CODE: &str = "qjkhlgw";
const NBEST: &str = "sentence_input = true\nsentence_count = 2\nsentence_max_count = 3\nsentence_cycle_key = \"tab\"\n";

fn sentences(c: &Coordinator) -> Vec<String> {
    c.debug_candidate_triples()
        .into_iter()
        .filter(|(_, code, _)| code == CODE)
        .map(|(t, ..)| t)
        .collect()
}

#[test]
fn codetable_sentence_block_follows_schema_count() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let one = coord("nb_one", "sentence_input = true\n");
    press(&one, CODE);
    assert_eq!(sentences(&one).len(), 1, "出厂 1 条：{:?}", sentences(&one));

    let two = coord("nb_two", NBEST);
    press(&two, CODE);
    let page = two.debug_page_texts();
    let s = sentences(&two);
    assert!(s.len() >= 2, "方案写了露 2 条：{s:?}");
    assert_eq!(page[..2], s[..2], "整句块连续摆在最前");
}

#[test]
fn codetable_cycle_key_rotates_window_and_preedit_follows() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let c = coord("nb_cycle", NBEST);
    press(&c, CODE);
    let before = c.debug_page_texts();
    let preedit_before = c.debug_preedit();
    let act = c.handle_key_event(&key(wind_keys::keymap::VK_TAB));
    assert!(
        !matches!(act, KeyAction::PassThrough),
        "池子 ≥2 条时 Tab 归整句切换，实际 {act:?}"
    );
    let after = c.debug_page_texts();
    assert_eq!(after[0], before[1], "窗口后滚一格：原名次 2 到了最前");
    assert_eq!(c.debug_page_info().1, 0, "高亮落在窗口首条");
    // 两条整句切法不同（`q|j` vs `qj`）：组合区跟着高亮候选走，不能停在名次 1 的切法上。
    assert_eq!(
        before[..2],
        ["我是中国人", "旬中国人"],
        "前提：真实词库上的两种读法"
    );
    assert_eq!(preedit_before, "q'j'khlg'w");
    assert_eq!(c.debug_preedit(), "qj'khlg'w");
}
