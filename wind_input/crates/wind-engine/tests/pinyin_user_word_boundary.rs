//! 用户词音节边界的跨层贯通测试（store → DictLayer → engine → Candidate）。
//!
//! 边界从用户词表（redb 24B value）出发，要穿过 `record_to_candidate` → `convert` 才能到达
//! 候选，中途任何一处重建 `Candidate`（如 `..Default::default()` 收尾）都会把它丢掉。丢了不报错，
//! 只会让双拼边界校验、长词上浮判据、自动造词沿用边界静默走「无边界」分支。
//! 见 docs/design/pinyin-code-domains.md §3 L2。
//!
//! 词典缺失时自动跳过。

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

/// `words` = (code, text, weight, boundary)。每个用例独立目录，避免串扰。
fn manager(dir: &Path, tag: &str, words: &[(&str, &str, i32, u64)]) -> EngineManager {
    let root = std::env::temp_dir().join(format!("wind_uw_boundary_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let store = Arc::new(Store::open(root.join("user_data.db")).expect("打开 store"));
    for (code, text, weight, boundary) in words {
        store
            .add_user_word("pinyin", code, text, *weight, *boundary)
            .expect("写入用户词");
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    // 必须显式设回 2 / 3，不能吃出厂默认（4 / 5）：本文件的 2~3 音节样本在出厂门槛下
    // 召回层就没了，而要验的 boundary 抵达与长词上浮都以候选在场为前提。
    // 召回门槛的出厂行为由 `pinyin_completion_recall_gate` 守。
    cfg.schema.pinyin.completion.min_syllables = 2;
    cfg.schema.pinyin.completion.max_extra_syllables = 3;
    EngineManager::with_store_override(&cfg, Some(dir), Some(store), Some(root.join("ov")))
}

/// 同 [`manager`]，但激活**双拼**方案（用户词仍写 `pinyin`——拼音族 data_schema_id 折叠）。
fn manager_shuangpin(dir: &Path, tag: &str, words: &[(&str, &str, i32, u64)]) -> EngineManager {
    let root = std::env::temp_dir().join(format!("wind_uw_boundary_{tag}"));
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
    // 同 [`manager`]：召回门槛设回旧出厂值，让样本进得来。
    cfg.schema.pinyin.completion.min_syllables = 2;
    cfg.schema.pinyin.completion.max_extra_syllables = 3;
    EngineManager::with_store_override(&cfg, Some(dir), Some(store), Some(root.join("ov")))
}

/// 返回 (候选位次, boundary, is_promoted_completion)。未命中为 None。
fn find(mgr: &EngineManager, input: &str, text: &str) -> Option<(usize, u64, bool)> {
    let r = mgr.convert(input, 30);
    let i = r.candidates.iter().position(|c| c.text == text)?;
    let c = &r.candidates[i];
    Some((i, c.boundary, c.is_promoted_completion))
}

/// 边界须原样到达候选，且精确匹配与前缀补全两条路径都要带上。
#[test]
fn user_word_boundary_reaches_candidate() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    // 「大菠萝哥」da|bo|luo|ge → 起始字节位 {0,2,4,7}
    const B: u64 = 0b10010101;
    let mgr = manager(&dir, "reach", &[("daboluoge", "大菠萝哥", 5000, B)]);

    assert_eq!(
        find(&mgr, "daboluoge", "大菠萝哥").map(|t| t.1),
        Some(B),
        "整串精确命中须带边界"
    );
    assert_eq!(
        find(&mgr, "daboluo", "大菠萝哥").map(|t| t.1),
        Some(B),
        "前缀补全须带边界（长词上浮判据吃这个值）"
    );
}

/// 简拼须按真值边界投影声母，不得用 `maximum_match` 现猜。
///
/// 「西安宁」真值 `xi|an|ning` ⇒ 简拼 `xan`；`maximum_match` 切成 `xian|ning` ⇒ 猜出 `xn`，
/// 既漏又错。本用例必须用歧义切分码，`maximum_match` 恰好猜对的样本测不出这个缺陷。
#[test]
fn user_word_abbrev_uses_true_boundary_not_maximum_match() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    const B: u64 = 0b10101; // xi|an|ning
    let with = manager(&dir, "abbr_on", &[("xianning", "西安宁", 5000, B)]);
    let without = manager(&dir, "abbr_off", &[("xianning", "西安宁", 5000, 0)]);

    let hit = |m: &EngineManager, q: &str| {
        m.convert(q, 30)
            .candidates
            .iter()
            .any(|c| c.text == "西安宁")
    };

    assert!(hit(&with, "xan"), "真值边界下 xi|an|ning 的简拼应为 xan");
    assert!(
        !hit(&with, "xn"),
        "xn 是 maximum_match 猜的 xian|ning 的投影，不该再命中"
    );

    // 对照组 = 修复前的行为，方向恰好相反
    assert!(
        !hit(&without, "xan"),
        "无边界只能退回 DAG 猜，真简拼打不出（这正是修复前的现场）"
    );
    assert!(hit(&without, "xn"), "无边界时反而是假简拼命中");

    // 全拼整串不受影响
    assert!(hit(&with, "xianning"));
    assert!(hit(&without, "xianning"));
}

/// 简拼候选保留全拼码，让词频与全拼输入共用同一个计数（记账走候选的 `code`）；
/// 覆盖成简拼串会让同一个词在 `xan` 与 `xianning` 下各记各的。
///
/// 同时锁住 `consumed_length`：判据是 `query.starts_with(&c.code)`，简拼下不成立 ⇒ 取
/// `query.len()`，仍是消费整串。别以为 code 变长消费也会变长。
#[test]
fn user_word_abbrev_keeps_full_pinyin_code() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    const B: u64 = 0b10101; // xi|an|ning
    let mgr = manager(&dir, "abbr_code", &[("xianning", "西安宁", 5000, B)]);

    let r = mgr.convert("xan", 30);
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "西安宁")
        .expect("简拼 xan 应命中「西安宁」");

    assert_eq!(
        c.code, "xianning",
        "简拼候选须保留全拼码，词频才能与全拼输入共用计数"
    );
    assert_eq!(c.boundary, B, "边界与全拼码同域，一并保留");
    assert!(c.is_abbrev, "仍标记为简拼层（排序沉底靠它）");
    assert_eq!(
        c.consumed_length, 3,
        "简拼消费整串（xan 共 3 字节），不因 code 变长而变"
    );

    // 全拼输入时同一个词的 code 相同 —— 这正是两者共用词频键的前提
    let r2 = mgr.convert("xianning", 30);
    let c2 = r2
        .candidates
        .iter()
        .find(|c| c.text == "西安宁")
        .expect("全拼应命中");
    assert_eq!(c2.code, c.code, "简拼与全拼下的 code 必须一致");
}

/// 双拼下简拼要认原始击键，不能拿双拼转换后的全拼去判。
///
/// 简拼只跟敲下的字母有关、与双拼方案无关；而 `convert` 里 `input` 会被双拼转换结果覆盖，
/// 拿它判简拼永远匹配不到用户实际敲的 `xan`，用户词的简拼在双拼下完全不可达。
#[test]
fn shuangpin_abbrev_uses_raw_keystrokes() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    const B: u64 = 0b10101; // xi|an|ning
    let mgr = manager(&dir, "sp_abbr", &[("xianning", "西安宁", 5000, B)]);

    // 全拼下先确认基线成立
    assert!(
        mgr.convert("xan", 30)
            .candidates
            .iter()
            .any(|c| c.text == "西安宁"),
        "全拼下 xan 应命中（基线）"
    );

    // 切到双拼：同样敲 xan，走的是双拼转换路径
    let sp = manager_shuangpin(&dir, "sp_abbr_sp", &[("xianning", "西安宁", 5000, B)]);
    let hit = sp
        .convert("xan", 30)
        .candidates
        .iter()
        .any(|c| c.text == "西安宁");
    assert!(
        hit,
        "双拼下 xan 也应命中——简拼判据须用原始击键，不能用双拼转换后的全拼"
    );
}

/// 双拼下的混合简拼（`xanning` = x + an + ning → 西安宁）不得被边界校验误杀
/// （`pinyin-mixed-abbrev.md` §5 约束 1）。
///
/// 候选 code 是全拼码 `xianning`、击键是 `xanning`，不同域，双拼真值边界比较必然不符；
/// 排序前的 `boundary_compatible` 过滤靠 `is_abbrev` 显式豁免，漏标就整批误杀。
/// 用歧义切分码：`maximum_match` 会把 `xianning` 切成 `xian|ning`。
#[test]
fn shuangpin_mixed_abbrev_survives_boundary_check() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    const B: u64 = 0b10101; // xi|an|ning

    // 全拼基线：确认混合式本身成立（否则下面的双拼断言分不清是哪一层坏了）
    let fp = manager(&dir, "mix_abbr_fp", &[("xianning", "西安宁", 5000, B)]);
    assert!(
        fp.convert("xanning", 30)
            .candidates
            .iter()
            .any(|c| c.text == "西安宁"),
        "全拼下 xanning 应命中（基线）"
    );

    // 切到双拼：同样 7 键，走双拼转换 + 真值边界校验
    let sp = manager_shuangpin(&dir, "mix_abbr_sp", &[("xianning", "西安宁", 5000, B)]);
    let r = sp.convert("xanning", 30);
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "西安宁")
        .unwrap_or_else(|| {
            panic!(
                "双拼下 xanning 也应命中——混合简拼候选须走在边界校验的豁免侧: {:?}",
                r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
            )
        });
    assert!(
        c.is_abbrev,
        "豁免正是靠这个位；它没立起来就说明候选是从别的路径来的，本测试也就失去意义"
    );
}

/// 用户长词打 2 个音节即可上浮，且不取决于词条填没填 boundary。
///
/// 「大菠萝哥」4 音节、`max_extra = 3`，输入 `dabo`（started 2、距词尾 2 ≤ 3）→ 上浮。
/// 手输码用户词恒 `boundary = 0`，但码本身是合法全拼、DAG 现切即得音节数；判据走
/// `effective_boundary`，两侧必须一致。判据没被改成恒真由末尾 `max_extra = 1` 的对照守着。
#[test]
fn user_long_word_promotes_at_two_syllables_regardless_of_boundary() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    const B: u64 = 0b10010101; // da|bo|luo|ge
    let with = manager(&dir, "promo_on", &[("daboluoge", "大菠萝哥", 5000, B)]);
    let without = manager(&dir, "promo_off", &[("daboluoge", "大菠萝哥", 5000, 0)]);

    let hit = find(&with, "dabo", "大菠萝哥");
    assert!(
        hit.is_some_and(|(_, b, promoted)| b == B && promoted),
        "有边界时 dabo 应上浮进完整匹配层，实际 {hit:?}"
    );

    // 无边界那条：**同样上浮**，且 `boundary` 字段仍如实为 0 —— 现切只用于判据，
    // 不回写候选（回写会让下游误以为词条自带音节真值）。
    let hit_wo = find(&without, "dabo", "大菠萝哥");
    assert!(
        hit_wo.is_some_and(|(_, b, promoted)| b == 0 && promoted),
        "无边界时 dabo 也应上浮（码可 DAG 现切出 4 音节），且候选 boundary 仍为 0，实际 {hit_wo:?}"
    );

    // 3 音节两侧同样上浮——确认一致性不只在 2 音节这一档成立。
    assert!(
        find(&with, "daboluo", "大菠萝哥").is_some_and(|(_, _, p)| p),
        "有边界：3 音节上浮"
    );
    assert!(
        find(&without, "daboluo", "大菠萝哥").is_some_and(|(_, _, p)| p),
        "无边界：3 音节同样上浮"
    );

    // 本用例守的是一致性，不是「判据没被改成恒真」：主路径上 step 6.3 的
    // `syllable_cap = started + max_extra` 与判据的 `remaining <= max_extra` 是同一个不等式，
    // 收紧 `max_extra` 只会让词在召回层就没了。该命题挂在 6.7 支路，见
    // `pinyin_user_word_no_boundary_tier.rs::fallback_branch_far_word_promotes_for_neither`。
}
