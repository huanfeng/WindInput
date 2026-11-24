//! 混输的拼音子引擎共用 `[schema.pinyin.completion]`，两个旋钮必须照样生效。
//!
//! 混输的拼音次引擎由 `manager.rs` 的 `PinyinConfig` 构造，`completion_min_syllables` /
//! `completion_max_extra_syllables` 直接取自全局 `pg.completion`，与纯拼音方案同一份；同一段里
//! `enable_abbrev`、`enable_partial_final`、`allow_full_pinyin` 则按 `mix_pinyin.is_none()` 分流。
//! 本文件守的是共用配置的第二个消费者：日后给混输也分流这两项，或改动 `completion_syllable_cap`
//! 的调用点，会在这里显形。
//!
//! 必须用真实词库：`CodetableDict::merge_single` 把 `boundary` 硬编码为 0，而召回门槛
//! （`search_prefix_with_boundary_syllable_capped`）按 `boundary.count_ones()` 数音节，内存词典
//! 的词条门槛不成立，会假绿。
//!
//! 样本 `zaim` = zai + 残码 m ⇒ `started = 2`。拼音侧有 2 音节的「在吗」「再买」与 3 音节的
//! 「在美国」「在没有」，恰好跨门槛，与 `pinyin_completion_recall_gate` 同一组样本。

use std::path::PathBuf;
use wind_config::Config;
use wind_engine::EngineManager;

const SCHEMA: &str = "wubi86_pinyin";

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    (p.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists()
        && p.join(format!("schemas/{SCHEMA}.schema.toml")).exists())
    .then_some(p)
}

fn manager(dir: &std::path::Path, min_syl: u32, max_extra: u32) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec![SCHEMA.to_string()];
    cfg.schema.active = SCHEMA.to_string();
    cfg.schema.pinyin.completion.min_syllables = min_syl;
    cfg.schema.pinyin.completion.max_extra_syllables = max_extra;
    EngineManager::new(&cfg, Some(dir))
}

fn texts(mgr: &EngineManager, input: &str) -> Vec<String> {
    mgr.convert_with(SCHEMA, input, 80)
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

/// 出厂 `min_syllables = 4`：`zaim`（started 2）未达门槛，上限收紧到 started 本身
/// ⇒ 混输下同样不给超音节的拼音候选。
#[test]
fn mixed_pinyin_respects_min_syllables() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：混输方案或拼音词库不存在");
        return;
    };
    let t = texts(&manager(&dir, 4, 5), "zaim");
    assert!(!t.is_empty(), "zaim 在混输下应有候选");

    for over in ["在美国", "在没有"] {
        assert!(
            !t.contains(&over.to_string()),
            "started=2 < min=4，混输的拼音侧不该给超音节的「{over}」；实际前 12: {:?}",
            &t[..t.len().min(12)]
        );
    }
    // 反向：音节数对齐的必须还在，否则「没有超音节候选」可能只是拼音侧整批被滤空
    //（混输另有一道「拼音候选须消费整串」的过滤，见 mixed_partial_pinyin_filter）。
    assert!(
        t.iter().any(|s| s == "在吗" || s == "再买"),
        "2 音节拼音候选应正常召回；实际前 12: {:?}",
        &t[..t.len().min(12)]
    );
}

/// 门槛放宽后超音节候选回来 —— 证明上一条不是「混输把拼音候选整批滤掉了」。
#[test]
fn mixed_pinyin_completion_returns_when_gate_relaxed() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：混输方案或拼音词库不存在");
        return;
    };
    let t = texts(&manager(&dir, 2, 3), "zaim");
    assert!(
        t.contains(&"在美国".to_string()),
        "min=2 时 started=2 达门槛、上限 2+3=5，3 音节的「在美国」应召回；实际前 12: {:?}",
        &t[..t.len().min(12)]
    );
}

/// 音节数档位（`completion_extra_syllables`）在混输的拼音候选上同样被标注。
///
/// 档位错了不会让候选消失，只会让它与对齐候选同档 —— 静默的排序退化，故直接断言字段值。
/// 注：档位的施加在协调器（`cmp_completion_extra`），引擎侧只负责标注。
#[test]
fn mixed_pinyin_tags_completion_extra_syllables() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：混输方案或拼音词库不存在");
        return;
    };
    let mgr = manager(&dir, 2, 3);
    let cands = mgr.convert_with(SCHEMA, "zaim", 80).candidates;

    // 取「再买」而非「在吗」：混输对拼音侧另有限流，`zaim` 下 80 条里不含「在吗」。
    // 样本只要跨档（2 音节 vs 3 音节）即可，与具体是哪个词无关。
    for (text, want) in [("再买", 0u8), ("在美国", 1)] {
        let Some(c) = cands.iter().find(|c| c.text == text) else {
            panic!(
                "「{text}」应在候选中；实际前 12: {:?}",
                cands.iter().take(12).map(|c| &c.text).collect::<Vec<_>>()
            );
        };
        assert_eq!(
            c.completion_extra_syllables,
            want,
            "「{text}」{} 音节、started=2 ⇒ extra 应为 {want}",
            c.boundary.count_ones()
        );
    }
}
