//! 端到端守卫：开了模糊音，按模糊打法也要打得出**用户词**（论坛 t215 / A2-44）。
//!
//! 现场：c=ch 开启后，`caijiuduolian` 能出用户加的「菜就多练」，`chaijiuduolian` 不能。
//! 多打几遍也没用 —— 造词写库用的是规范码（`learn_code`），学进去的仍是
//! `caijiuduolian`。根因是 step 6 只拿用户敲的码查 store，系统词库那条路有
//! `lookup_with_fuzzy` 的变体展开，用户词这条路没有。
//!
//! 内联夹具的单测（`pinyin::tests::fuzzy_*user*`）钉的是召回本身；本文件钉的是
//! 「真实词库下这条候选排得到用户看得见的地方」——系统词库的整句、子短语、模糊单字
//! 都在同一串上竞争，内联夹具里系统词典是空的，测不出这一层。
//!
//! 词库不存在时跳过（判据是耗时 0.0x 秒）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wind_config::Config;
use wind_engine::EngineManager;
use wind_store::Store;

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("build_dev")
        .join("data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

/// 用户词表为 `words`、模糊音开 c=ch（`fuzzy = false` 则全关）的引擎。
fn manager(dir: &Path, tag: &str, words: &[(&str, &str)], fuzzy: bool) -> EngineManager {
    let words: Vec<(&str, &str, u64)> = words.iter().map(|&(c, t)| (c, t, 0)).collect();
    manager_s2(dir, tag, &words, fuzzy, false)
}

/// 同 [`manager`]，但用户词带边界，且 `s2` 控制 `sentence_uses_user_words`（整句认用户词）。
fn manager_s2(
    dir: &Path,
    tag: &str,
    words: &[(&str, &str, u64)],
    fuzzy: bool,
    s2: bool,
) -> EngineManager {
    let root = std::env::temp_dir().join(format!("wind_pinyin_fuzzy_user_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let store = Arc::new(Store::open(root.join("user_data.db")).expect("打开 store"));
    for (code, text, boundary) in words {
        store
            .add_user_word("pinyin", code, text, 1200, *boundary)
            .expect("写入用户词");
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    cfg.input.fuzzy.enabled = fuzzy;
    cfg.input.fuzzy.ch_c = fuzzy;
    cfg.schema.pinyin.sentence_uses_user_words = s2;
    EngineManager::with_store_override(
        &cfg,
        Some(dir),
        Some(store),
        Some(root.join("schema_overrides")),
    )
}

fn rank(mgr: &EngineManager, input: &str, text: &str) -> Option<usize> {
    mgr.convert_with("pinyin", input, 50)
        .candidates
        .iter()
        .position(|c| c.text == text)
}

/// 用户原话里的那一串：翘舌打法出平舌码的用户词，且在首页（出厂每页 9 条）。
#[test]
fn retroflex_typing_recalls_flat_user_word_on_first_page() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    const WORD: &str = "菜就多练";
    let words = [("caijiuduolian", WORD)];

    // 前提：系统词库自己组不出这个词，否则测到的不是用户词这条路。
    let base = manager(&dir, "base", &[], true);
    assert_eq!(
        rank(&base, "chaijiuduolian", WORD),
        None,
        "前提：无用户词时 chaijiuduolian 不得出「{WORD}」"
    );

    let mgr = manager(&dir, "on", &words, true);
    for input in ["chaijiuduolian", "chai'jiu'duo'lian", "caijiuduolian"] {
        let r = rank(&mgr, input, WORD);
        println!("[c=ch] {input} → 「{WORD}」 rank {r:?}");
        assert!(
            r.is_some_and(|i| i < 9),
            "{input} 应在首页出用户词「{WORD}」，实际 rank {r:?}"
        );
    }
}

/// 关模糊音：翘舌打法召不回（对照组，证明上一条的命中来自模糊路径），平舌照旧。
#[test]
fn fuzzy_off_keeps_user_word_exact_only() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    const WORD: &str = "菜就多练";
    let mgr = manager(&dir, "off", &[("caijiuduolian", WORD)], false);
    assert_eq!(rank(&mgr, "chaijiuduolian", WORD), None);
    assert_eq!(rank(&mgr, "caijiuduolian", WORD), Some(0));
}

/// 整句那一条（`is_sentence`）的文本。
fn sentence(mgr: &EngineManager, input: &str) -> Option<String> {
    mgr.convert_with("pinyin", input, 50)
        .candidates
        .into_iter()
        .find(|c| c.is_sentence)
        .map(|c| c.text)
}

/// 整句（`sentence_uses_user_words` 开）：翘舌打法下用户词「菜就多练」作为**整句中的一段**
/// 出现（`wo` + `chaijiuduolian`）。step 6 只给得出整词候选，整句这一条靠
/// `lattice::add_store_nodes` 的模糊分支——记录存的是规范码 `caijiuduolian`。
///
/// ⚠️ 单拿两字词「菜就」测不出来：真实词库下 `caijiu` 精确打法的整句也是「才就」——
/// 才 / 就 都在虚词表里，各拿 +2.0 优待并豁免每词罚，合计压过 w=1200 + 加成的用户词。
/// 那是 S2 标定的事，与模糊无关（精确打法同样输），这里不拿它当判据。
#[test]
fn fuzzy_typing_puts_user_word_into_sentence() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    // cai|jiu|duo|lian → 位 0/3/6/9
    let words = [("caijiuduolian", "菜就多练", 0b10_0100_1001u64)];
    let cases = [
        // (tag, 模糊, S2, 输入, 期望整句含「菜就多练」)
        ("s2_on", true, true, "wochaijiuduolian", true),
        ("s2_on_exact", true, true, "wocaijiuduolian", true),
        ("s2_off", true, false, "wochaijiuduolian", false),
        ("fz_off", false, true, "wochaijiuduolian", false),
        ("fz_off_exact", false, true, "wocaijiuduolian", true),
    ];
    for (tag, fz, s2, input, want) in cases {
        let got = sentence(&manager_s2(&dir, tag, &words, fz, s2), input);
        println!("[{tag}] {input} → {got:?}");
        assert_eq!(
            got.as_deref().unwrap_or("").contains("菜就多练"),
            want,
            "[{tag}] {input} 整句: {got:?}"
        );
    }
}

/// 边界对不上变体码切分的记录，模糊分支不收（同精确分支的约束 2）。
#[test]
fn fuzzy_sentence_rejects_record_with_wrong_boundary() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    for (tag, boundary) in [("bad_mask", 0b10_0100_0101u64), ("no_mask", 0)] {
        let words = [("caijiuduolian", "菜就多练", boundary)];
        let got = sentence(
            &manager_s2(&dir, tag, &words, true, true),
            "wochaijiuduolian",
        );
        assert!(
            !got.as_deref().unwrap_or("").contains("菜就多练"),
            "[{tag}] 边界 {boundary:#b} 不得进整句: {got:?}"
        );
    }
}
