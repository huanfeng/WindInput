//! 自动造词写进词库的**编码**必须是规范的词条码，不是候选的匹配用码。
//!
//! 现场：开模糊音(sh_s + en_eng)打 `senrikl`，分步选「生日」「快乐」上屏后，
//! 临时词库里躺着 `senrikuaile → 生日快乐` —— 前半 `senri` 是用户敲的模糊原码、
//! 后半 `kuaile` 是词典全拼码，两段分处两个域。这条码**用任何方式都打不出来**：
//! `senrikl` 不行、`shengrikuaile` 也不行，只有一字不差敲 `senrikuaile` 才行。
//!
//! 根子是 `committed_segs` 存的是候选的匹配用码，而那个码上绑着三个别的用途
//! （`consumed_length` 的 `starts_with` 判据、preedit 跟随、词频记账），不能为造词改掉。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

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
fn has_dict() -> bool {
    data_dir()
        .join("schemas/pinyin/rime_frost.dict.yaml")
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
fn cfg() -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.schema.pinyin.fuzzy.enabled = true;
    c.schema.pinyin.fuzzy.sh_s = true;
    c.schema.pinyin.fuzzy.en_eng = true;
    c.schema.pinyin.auto_learn.enabled = true;
    c
}
/// 翻页找候选并选中，返回上屏文本（None = 未上屏，仍在组合区）。
fn pick(coord: &Coordinator, want: &str) -> Option<String> {
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if t.is_empty() {
            return None;
        }
        if let Some(p) = t.iter().position(|x| x == want) {
            return match coord.handle_key_event_policed(&key(0x31 + p as u32)) {
                wind_bridge::handler::KeyAction::InsertText { text, .. } => Some(text),
                _ => None,
            };
        }
        coord.handle_key_event_policed(&key(0x22));
    }
    None
}

/// 分步组出的词，写库用的码须是**各段规范词条码**的拼接。
#[test]
fn learned_code_uses_dict_code_not_typed_code() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let db = std::env::temp_dir().join("wind_learn_code_norm.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg(), Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    for ch in "senrikl".chars() {
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    assert!(pick(&coord, "生日").is_none(), "选「生日」应留在组合区分步");
    assert_eq!(
        pick(&coord, "快乐").as_deref(),
        Some("生日快乐"),
        "再选「快乐」应整体上屏"
    );

    let temps: Vec<(String, String, u64)> = store
        .search_temp_words_prefix("pinyin", "", 200)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.code, r.text, r.boundary))
        .collect();
    let (code, _, boundary) = temps
        .iter()
        .find(|(_, t, _)| t == "生日快乐")
        .unwrap_or_else(|| panic!("应造出「生日快乐」，实际: {temps:?}"));

    // 用户敲的是 senrikl，但词在词典里登记的码是 shengri + kuaile。
    assert_eq!(
        code, "shengrikuaile",
        "造词码须用规范词条码，不得掺入模糊音原码 senri"
    );
    // 模糊命中的 boundary 此前恒 0（与原码不同域，见 lookup_with_fuzzy），
    // 换成规范码后边界与之同域，必须是真值 —— 否则简拼索引算不出 srkl。
    assert_ne!(*boundary, 0, "规范码的 boundary 须为真值，简拼索引依赖它");
}

/// **真正的验收**：造出来的词，用户下次得真能打出来。
///
/// ⚠️ **必须用系统词库里没有的词**。第一版拿「生日快乐」测，结果是个假护栏：
/// `shengrikuaile` 在系统词库里本就有这个词，`can_type` 恒真 —— 造词码写成什么样
/// 它都绿（实测：把造词改回用候选 `code`，这条照样过）。
/// 改用「圣日快乐」：`senri` 的候选里有「圣日」（同音字），而这个**词**系统词库没有，
/// 所以打 `shengrikuaile` 能出它，只可能来自刚造的那条临时词。
///
/// 两种打法都要通：
/// - 全拼 `shengrikuaile`：直接命中规范码；
/// - 简拼 `srkl`：规范码在简拼路径上同样召得回。
///
/// ⚠️ 简拼这条**不守 boundary**，别把它当 boundary 的护栏：boundary=0 时词进
/// `abbrev_index::group_of` 的兜底组，引擎侧仍会用 DAG 对 `code` 现切声母串召回
/// （实测：变异「码取规范码但 boundary 退回候选那份」下本条仍绿）。
/// boundary 真值由上一条测试的 `assert_ne!(boundary, 0)` 守着 —— 那条在同一个变异下会红。
#[test]
fn learned_word_is_actually_typeable() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let db = std::env::temp_dir().join("wind_learn_code_typeable.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg(), Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    // 造一个系统词库里没有的词（见上方 ⚠️）。
    for ch in "senrikl".chars() {
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    assert!(pick(&coord, "圣日").is_none(), "选「圣日」应留在组合区分步");
    assert_eq!(
        pick(&coord, "快乐").as_deref(),
        Some("圣日快乐"),
        "再选「快乐」应整体上屏"
    );
    // 前提自检：这个词系统词库确实没有，否则下面测的是系统词库而非刚造的词。
    let fresh = Coordinator::new_headless_with_store(
        cfg(),
        Some(&data_dir()),
        Arc::new(Store::open(std::env::temp_dir().join("wind_learn_code_fresh.redb")).unwrap()),
    );
    fresh.prewarm_indexes();
    assert!(
        !type_and_find(&fresh, "shengrikuaile", "圣日快乐"),
        "前提：未造词的干净实例不得打出「圣日快乐」"
    );

    assert!(
        type_and_find(&coord, "shengrikuaile", "圣日快乐"),
        "规范全拼码须能打出刚造的词"
    );
    assert!(
        type_and_find(&coord, "srkl", "圣日快乐"),
        "规范码在简拼路径上同样须召得回"
    );
}

/// 敲入 `input`，翻页找 `want`，之后 Esc 清空。
fn type_and_find(coord: &Coordinator, input: &str, want: &str) -> bool {
    for ch in input.chars() {
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    let mut found = false;
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if t.is_empty() {
            break;
        }
        if t.iter().any(|x| x == want) {
            found = true;
            break;
        }
        coord.handle_key_event_policed(&key(0x22));
    }
    coord.handle_key_event_policed(&key(0x1B));
    found
}
