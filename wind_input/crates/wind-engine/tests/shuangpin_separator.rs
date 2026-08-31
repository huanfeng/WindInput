//! 双拼下的手动音节分隔符 `'`。
//!
//! 双拼每 2 键一音节，配对起点由「前面消耗了几个键」决定。用户一旦想让某个键单独作
//! 简拼声母，其后**所有**键的配对都会错位——这是双拼特有的、打分无从挽回的歧义
//! （`pinyin-mixed-abbrev.md` 记过「双拼下 `xan` 是三声母还是 xa+残码，本身歧义，
//! 故意未处理」）。`'` 就是补上的那个手段：它在 `ShuangpinConverter::convert` 里
//! 充当配对的硬边界。
//!
//! ⚠️ 这些用例**必须走真词库**：混合简拼（声母段 + 完整音节）的召回靠
//! `wind_store::abbrev_index`，内存夹具建不出那个索引，拿它测会一律空候选、
//! 分不清是分隔符没生效还是索引不在场。词典缺失时自动跳过。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wind_config::Config;
use wind_engine::EngineManager;
use wind_store::Store;

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

/// 激活双拼（出厂 `shuangpin.schema.toml` 用小鹤布局）。`words` = (code, text, weight, boundary)。
fn shuangpin(dir: &Path, tag: &str, words: &[(&str, &str, i32, u64)]) -> EngineManager {
    let root = std::env::temp_dir().join(format!("wind_sp_sep_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let store = Arc::new(Store::open(root.join("user_data.db")).expect("打开 store"));
    for (code, text, weight, boundary) in words {
        store
            .add_user_word("pinyin", code, text, *weight, *boundary)
            .expect("写入用户词");
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    cfg.schema.pinyin.completion.min_syllables = 2;
    cfg.schema.pinyin.completion.max_extra_syllables = 3;
    EngineManager::with_store_override(&cfg, Some(dir), Some(store), Some(root.join("ov")))
}

fn texts(mgr: &EngineManager, input: &str) -> Vec<String> {
    mgr.convert(input, 30)
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect()
}

/// ★★ 本功能的核心：分隔符让「简拼声母 + 完整音节」在双拼下可达。
///
/// 小鹤下「你好」= `nihc`。用户想打「你」的简拼再补「好」的全码，敲 `nhc`：
/// `nh` 被当成一对（→ `nang`）、`c` 落成残码，得 `nangc` —— 与用户意图毫无关系。
/// 加一撇 `n'hc` 后 `n` 单独成段（声母）、`hc` 独立配对（→ `hao`），得 `nhao`，
/// 正是混合简拼认得的形状。
///
/// ⚠️ 两个断言缺一不可：只断言 `n'hc` 命中的话，若哪天 `nhc` 也能歪打正着命中，
/// 这条用例照样绿，而「分隔符改变了切分」这件事根本没被测到。
#[test]
fn separator_enables_initial_plus_syllable() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = shuangpin(&dir, "core", &[]);

    let without = texts(&mgr, "nhc");
    assert!(
        !without.contains(&"你好".to_string()),
        "基线：不加分隔符时 nhc 被读成 nang+c，不该出「你好」，实际 {:?}",
        &without[..without.len().min(8)]
    );

    let with = texts(&mgr, "n'hc");
    assert!(
        with.contains(&"你好".to_string()),
        "n'hc = 声母 n + 音节 hao，应召回「你好」，实际 {:?}",
        &with[..with.len().min(8)]
    );
}

/// 分隔符在**已经能配对**的位置上同样是硬边界，不改变结果、也不吞键。
///
/// 「西安」小鹤 = `xiaj`（xi|an）。`xi'aj` 的一撇落在本来就有的音节边界上，
/// 候选不变，但 `consumed_length` 要多算那一键——用户确实按了它，少算会让
/// 分步上屏后残留一个孤立的 `'`。
#[test]
fn separator_on_existing_boundary_is_transparent_but_counted() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = shuangpin(&dir, "onboundary", &[]);

    let plain = mgr.convert("xiaj", 30);
    let sep = mgr.convert("xi'aj", 30);

    let p = plain.candidates.iter().find(|c| c.text == "西安");
    let s = sep.candidates.iter().find(|c| c.text == "西安");
    assert!(p.is_some(), "基线：xiaj 应出「西安」");
    let (p, s) = (p.unwrap(), s.expect("xi'aj 也应出「西安」"));

    assert_eq!(p.consumed_length, 4, "xiaj 消费 4 键");
    assert_eq!(
        s.consumed_length, 5,
        "xi'aj 消费 5 键（含那一撇），否则上屏后会留下孤立的分隔符"
    );
}

/// 预编辑区里手动的一撇与自动分段的一撇**合流**，不叠加。
///
/// 双拼 preedit 显示的是原始击键并按音节边界自动插 `'`。手动 `'` 已经在击键串里，
/// 若重建时仍按段 `join("'")`，就会在它旁边再插一个（`n'hc` → `n''hc`），
/// 用户看到自己没按过的第二撇，退格还得按两次。
///
/// ⚠️ 不变量：**去掉所有 `'` 后必须恰好还原击键串**（同 `render_keystroke_preedit`
/// 那条）。只断言「不含 `''`」是不够的——`nh'c` 这种切错位置的也不含 `''`。
#[test]
fn preedit_merges_manual_and_auto_separators() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = shuangpin(&dir, "preedit", &[]);

    for keys in ["n'hc", "xi'aj", "ni'", "'ni", "n''hc", "nihc"] {
        let pre = mgr.convert(keys, 5).preedit_display;
        assert_eq!(
            pre.replace('\'', ""),
            keys.replace('\'', ""),
            "preedit 去掉分隔符后必须还原击键串：{keys:?} → {pre:?}"
        );
        assert!(
            !pre.contains("'''"),
            "{keys:?} 的 preedit 出现了叠加的分隔符：{pre:?}"
        );
    }

    assert_eq!(
        mgr.convert("n'hc", 5).preedit_display,
        "n'hc",
        "手动那一撇原地保留，不额外加"
    );
    assert_eq!(
        mgr.convert("nihc", 5).preedit_display,
        "ni'hc",
        "无手动分隔符时自动分段照旧"
    );
}

/// 零回归：不含 `'` 的双拼输入，行为与改动前逐位一致。
#[test]
fn plain_shuangpin_is_untouched() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = shuangpin(&dir, "plain", &[]);

    let r = mgr.convert("nihc", 30);
    let nihao = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .expect("nihc 应出「你好」");
    assert_eq!(nihao.consumed_length, 4);
    assert_eq!(r.preedit_display, "ni'hc");

    // 纯简拼（双拼下靠原始击键判定）不受影响。
    let sp = shuangpin(&dir, "plain_abbr", &[("xianning", "西安宁", 5000, 0b10101)]);
    assert!(
        texts(&sp, "xan").contains(&"西安宁".to_string()),
        "双拼下纯简拼 xan 仍应命中"
    );
}

/// 只由分隔符组成的输入不产候选、也不 panic。
#[test]
fn separator_only_input_is_inert() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = shuangpin(&dir, "inert", &[]);
    for keys in ["'", "''", "'''"] {
        let r = mgr.convert(keys, 10);
        assert!(
            r.candidates.is_empty(),
            "{keys:?} 不该产出候选，实际 {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }
}
