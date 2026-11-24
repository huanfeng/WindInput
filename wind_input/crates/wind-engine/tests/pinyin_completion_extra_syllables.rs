//! 前缀补全的「音节数对齐」分档（`completion_extra_syllables`）不得被无边界词条绕过。
//!
//! `extra` 由 `boundary.count_ones() - 输入音节数` 算，`boundary == 0`（导入词库/手输码/旧数据）时
//! `count_ones()` 也是 0，`saturating_sub` 一减就是 0：不管几个音节都算作「与输入完全对齐」，既躲过
//! 分档、又拿到残码上浮特权（`meiy` 下 3 音节词涌进候选、把单字「没」压到几十位）。须像简拼路径那样
//! 用 `effective_boundary` 对码现切补出音节数。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

/// 夹具的要害是三条同码 3 音节词：
///
/// | 码 | 词 | weight | boundary | 用途 |
/// |---|---|---|---|---|
/// | `meiyige` | 每一个甲 | 30000 | `0b101001`（真值 mei\|yi\|ge，起点 0/3/5） | 正常词条 |
/// | `meiyige` | 每一个乙 | 30000 | `0` | 模拟导入词库 / 手输码 |
/// | `meiyige` | 每一个丙 | 50 | `0` | 上浮判据的下侧对照 |
///
/// 甲/乙 同码、同音节数、同权重，唯一差别是有没有边界信息，两者的分档必须一致。
///
/// 丙存在的理由：`meiy` 下 3 音节词的 `distance` 是 2，走 `distance > COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES`
/// ⇒ `weight < COMPLETION_FAR_WEIGHT_FLOOR` 那条分支。甲乙的 30000 远在 FLOOR(100) 之上，单靠它们断言
/// 「应上浮」恒真；丙的 50 落在 FLOOR 之下，构成一上一下的对照。
fn fixture(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_comp_extra_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("meiyou".into(), vec![("没有".into(), 169583, 0, 0b1001)]);
    w.add_with_boundary("mei".into(), vec![("没".into(), 50000, 0, 0b1)]);
    w.add_with_boundary(
        "meiyige".into(),
        vec![
            ("每一个甲".into(), 30000, 0, 0b101001),
            ("每一个乙".into(), 30000, 1, 0),
            ("每一个丙".into(), 50, 2, 0),
        ],
    );
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn engine(tag: &str) -> PinyinEngine {
    // 放宽召回门槛，让 3 音节补全进得来（出厂 4/5 会在召回层就挡掉，测不到分档）。
    let c = PyConfig {
        completion_min_syllables: 1,
        completion_max_extra_syllables: 5,
        ..Default::default()
    };
    PinyinEngine::new(c, fixture(tag))
}

/// 无边界词条的 `extra` 必须与同形有边界词条一致，不得被算作 0。
#[test]
fn no_boundary_entry_does_not_bypass_syllable_alignment() {
    let e = engine("bypass");
    let r = e.convert("meiy", 50).expect("convert 成功");
    let find = |t: &str| r.candidates.iter().find(|c| c.text == t);

    // 两条都必须在候选里，这是全部断言的前提：boundary 写成能切出非法音节的值（如 `0b10001` ⇒ `meiy|ige`）
    // 时两条补全会一起被过滤掉，若用 match 放过 `(None, None)` 测试照样报 ok。
    let all: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
    let jia = find("每一个甲")
        .unwrap_or_else(|| panic!("前提不成立：有边界那条没进候选，实际候选 {all:?}"));
    let yi = find("每一个乙")
        .unwrap_or_else(|| panic!("前提不成立：无边界那条没进候选，实际候选 {all:?}"));

    assert_eq!(
        jia.completion_extra_syllables, yi.completion_extra_syllables,
        "同码同音节数、只差 boundary 的两条，分档必须一致：\
         有边界 extra={}，无边界 extra={}",
        jia.completion_extra_syllables, yi.completion_extra_syllables
    );

    // 具体到分档值：输入 meiy 表达 2 个音节，3 音节词多预测了 1 个 ⇒ extra = 1。
    // 只断言「两条一致」的话，把分档整个改成恒 0 也照样通过。
    assert_eq!(
        yi.completion_extra_syllables, 1,
        "3 音节词在 2 音节输入下 extra 须为 1"
    );

    // 上浮特权同理，须钉具体取值：只写 `assert_eq!(jia.x, yi.x)` 时「两条都是 false」同样成立。
    //
    // 上浮的原因是走远距离分支而权重够高：`distance = eb.count_ones() - completed_syls = 3 - 1 = 2`
    // （`y` 是残码），2 > COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES(1) ⇒ FAR 分支 ⇒ weight 30000 ≥
    // COMPLETION_FAR_WEIGHT_FLOOR(100) ⇒ 不降级。下侧由 `low_weight_far_completion_keeps_tier_without_promotion` 守。
    // 别把 `distance` 与 `extra` 混为一谈：`extra = distance - (有残码 ? 1 : 0) = 1`。
    assert!(
        jia.is_promoted_completion && yi.is_promoted_completion,
        "距输入 1 个音节的补全应上浮：有边界={} 无边界={}",
        jia.is_promoted_completion,
        yi.is_promoted_completion
    );
}

/// 回归护栏：有边界的正常候选分档不变。
#[test]
fn normal_entries_keep_their_tier() {
    let e = engine("normal");
    let r = e.convert("meiy", 50).expect("convert 成功");
    let meiyou = r
        .candidates
        .iter()
        .find(|c| c.text == "没有")
        .expect("没有 应产出");
    assert_eq!(
        meiyou.completion_extra_syllables, 0,
        "2 音节补全对齐输入，extra=0"
    );
    assert!(meiyou.is_promoted_completion, "近距离补全仍应上浮");
}

/// 上浮判据的下侧对照：权重低于 `COMPLETION_FAR_WEIGHT_FLOOR` 的远距离补全不上浮，但分档照算，
/// 档位只看音节数，与权重无关。
///
/// 本文件的夹具是 wdat 系统词库，不经 `should_promote_user_completion`；这条守的是 step4 的
/// `COMPLETION_FAR_WEIGHT_FLOOR`。用户词那条判据的下侧对照在
/// `pinyin_user_word_no_boundary_tier.rs::fallback_branch_far_word_promotes_for_neither`。
#[test]
fn low_weight_far_completion_keeps_tier_without_promotion() {
    let e = engine("floor");
    let r = e.convert("meiy", 50).expect("convert 成功");
    let all: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
    let bing = r
        .candidates
        .iter()
        .find(|c| c.text == "每一个丙")
        .unwrap_or_else(|| panic!("前提不成立：低权重词应被召回（降级不等于丢弃），实际 {all:?}"));

    assert!(
        !bing.is_promoted_completion,
        "weight 50 < COMPLETION_FAR_WEIGHT_FLOOR(100)，距离 2 的补全不该上浮"
    );
    assert_eq!(
        bing.completion_extra_syllables, 1,
        "分档只看音节数：3 音节词在 2 音节输入下 extra 恒为 1，与权重无关"
    );
}
