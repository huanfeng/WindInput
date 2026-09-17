//! 前缀补全的「音节数对齐」分档（`completion_extra_syllables`）不得被无边界词条绕过。
//!
//! 现场：用户导入扩展词库后，打 `meiy` 时 3 音节词涌进候选、把单字「没」压到第 59 位。
//! 根因不是音节数约束写错，而是**它有个漏网口**：`extra` 由
//! `boundary.count_ones() - 输入音节数` 算，而 `boundary == 0`（导入词库/手输码/旧数据）
//! 时 `count_ones()` 也是 0，`saturating_sub` 一减就是 0 —— 于是不管几个音节，
//! 全部算作「与输入完全对齐」，既躲过分档、又拿到残码上浮特权。
//!
//! 同一个漏网口简拼路径堵过（改用 `effective_boundary` 对码现切补出音节数，
//! 其注释：「那是本判据唯一的漏网口，手输码用户词/旧词典条目会绕过音节数约束」），
//! 前缀补全这一处当时漏了。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

/// 夹具的要害是**两条 3 音节词只差一个 boundary**：
///
/// | 码 | 词 | boundary | 说明 |
/// |---|---|---|---|
/// | `meiyige` | 每一个甲 | `0b101001`（真值 mei\|yi\|ge，起点 0/3/5） | 正常词条 |
/// | `meiyige` | 每一个乙 | `0` | 模拟导入词库 / 手输码 |
///
/// 同码、同音节数、同权重，唯一差别是有没有边界信息 —— 两者的分档必须一致。
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

    let jia = find("每一个甲");
    let yi = find("每一个乙");
    // 两条同码同音节数，要么都在、要么都不在；在的话分档必须相同。
    match (jia, yi) {
        (Some(a), Some(b)) => {
            assert_eq!(
                a.completion_extra_syllables, b.completion_extra_syllables,
                "同码同音节数、只差 boundary 的两条，分档必须一致：\
                 有边界 extra={}，无边界 extra={}",
                a.completion_extra_syllables, b.completion_extra_syllables
            );
            assert_eq!(
                a.is_promoted_completion, b.is_promoted_completion,
                "上浮特权也必须一致"
            );
        }
        (None, None) => {}
        (a, b) => panic!(
            "两条只差 boundary 的同形词条，一条进了候选另一条没进：甲={:?} 乙={:?}",
            a.map(|c| c.completion_extra_syllables),
            b.map(|c| c.completion_extra_syllables)
        ),
    }

    // 具体到分档值：输入 meiy 表达 2 个音节，3 音节词多预测了 1 个 ⇒ extra = 1。
    if let Some(c) = yi {
        assert_eq!(
            c.completion_extra_syllables, 1,
            "3 音节词在 2 音节输入下 extra 须为 1"
        );
    }
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
