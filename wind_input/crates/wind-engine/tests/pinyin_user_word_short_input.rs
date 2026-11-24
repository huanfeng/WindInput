//! 用户词在短输入（单字母 / 远距离前缀）下不得夺走首选。
//!
//! step 6 合并用户词时，同文分支须与新增分支对称地受 `should_promote_user_completion` 与
//! `promotion_cap` 约束；否则配了高权重的「筛选」会在只打 `s` 时因与系统补全同文而夺走首选。
//! 本测试同时钉三件事：
//! 1. 短输入下用户词不上位（正例）；
//! 2. 精确输入下用户提权仍全效（反向对照，否则修复可能是把用户词整个废掉）；
//! 3. 排位不随系统补全条数变化。
//!
//! 词库缺失时自动跳过。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wind_config::Config;
use wind_engine::EngineManager;
use wind_store::Store;

/// 词库目录。`build_dev/data` 优先，回退到 `build/data`。
///
/// 回退不是多余的：`build_dev/data` 由构建流程删除后重建，窗口内整个不存在，用例会静默走
/// 「跳过」分支（判据是耗时：真跑约 0.2s 以上）。
fn data_dir() -> Option<PathBuf> {
    for d in ["../../../build_dev/data", "../../../build/data"] {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(d);
        if p.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
            return Some(p);
        }
    }
    None
}

/// 每个用例独立的 redb 与 override 目录，避免串扰与污染真实用户目录。
fn tmp_root(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("wind_uw_short_input_{tag}"));
    let _ = std::fs::remove_dir_all(&p);
    let _ = std::fs::create_dir_all(&p);
    p
}

fn manager(dir: &Path, tag: &str, words: &[(&str, &str, i32)]) -> EngineManager {
    let root = tmp_root(tag);
    let store = Arc::new(Store::open(root.join("user_data.db")).expect("打开 store"));
    for (code, text, weight) in words {
        store
            .add_user_word("pinyin", code, text, *weight, 0)
            .expect("写入用户词");
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    EngineManager::with_store_override(
        &cfg,
        Some(dir),
        Some(store),
        Some(root.join("schema_overrides")),
    )
}

fn texts(mgr: &EngineManager, input: &str, limit: usize) -> Vec<String> {
    mgr.convert_with("pinyin", input, limit)
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

/// 高权重用户词不得因「只打了一个声母」就成为首选。
///
/// 权重取 20 亿（同 `pinyin_user_word_merge`）：远超任何系统词，约束失效必然排到第 0 位。
///
/// `s` 只表达 1 个音节，2 音节的「筛选」与系统词「所以」「时间」受同一条音节数闸门处置，
/// 一律不产出（见 `completion_syllable_cap`）；用户词不享有豁免，否则同一输入长度下行为
/// 因加没加词而不同。可达性由 `user_word_returns_when_input_grows` 守。
#[test]
fn high_weight_user_word_does_not_take_top_on_single_letter() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let base = manager(&dir, "base", &[]);
    let with_uw = manager(&dir, "uw", &[("shaixuan", "筛选", 2_000_000_000)]);

    let base_top = texts(&base, "s", 300);
    let uw_all = texts(&with_uw, "s", 300);

    assert_eq!(
        base_top.first(),
        uw_all.first(),
        "挂上高权重用户词后 `s` 的首选不应改变（基线={:?} 实际={:?}）",
        base_top.first(),
        uw_all.first()
    );
    assert!(
        !uw_all.iter().any(|t| t == "筛选"),
        "`s` 只表达 1 个音节，2 音节用户词不该产出（与系统词同一规则）"
    );
}

/// 反向对照：输入一长，用户词必须回来；否则上面的断言可以靠把用户词整个废掉来满足。
///
/// 门槛落在 `shaix` 而非 `sh`/`shai`：后两者只起了 1 个音节的头，不出 2 音节词是正确行为
/// （与 `dian` 要打到 `dianh` 才出「电话」同构）。`shaixu` 不在列表里，见下一条。
#[test]
fn user_word_returns_when_input_grows() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir, "grow", &[("shaixuan", "筛选", 2_000_000_000)]);

    for input in ["shaix", "shaixuan"] {
        let t = texts(&mgr, input, 300);
        assert!(
            t.iter().any(|x| x == "筛选"),
            "`{input}` 下用户词「筛选」必须可达，实际前 10={:?}",
            &t[..10.min(t.len())]
        );
    }
}

/// 上一条的边界侧：`shaixu` 下「筛选」不可达是音节边界对齐的预期结果，不是用户词失效。
///
/// `shaixu` 的 DAG 切分是 `shai|xu`，放「筛选」出来等于把已打完的音节 `xu` 继续拉长，与
/// 「打 `shen` 出 `sheng`」同一条通道（见 `wind_dict::cached::prefix_syllable_aligned`）。
/// 少了本条，把 `shaixu` 加回去或回退对齐判据都不会变红。
#[test]
fn user_word_does_not_cross_a_completed_syllable() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir, "cross", &[("shaixuan", "筛选", 2_000_000_000)]);

    let t = texts(&mgr, "shaixu", 300);
    assert!(
        !t.iter().any(|x| x == "筛选"),
        "`shaixu` 切分为 shai|xu，`xu` 已是完整音节，不该被拉长成 xuan；实际前 10={:?}",
        &t[..10.min(t.len())]
    );
    // 反向：同一输入下**本音节**的候选必须还在，否则「筛选不可达」可能只是召回整个塌了。
    assert!(!t.is_empty(), "`shaixu` 仍应有候选（xu 的字等）");
}

/// **反向对照**：精确输入下用户提权必须仍然全效。
///
/// 缺了这条，上面那个断言可以靠「把用户词彻底废掉」来满足。
#[test]
fn user_word_promotion_still_works_on_exact_input() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir, "exact", &[("shaixuan", "筛选", 2_000_000_000)]);
    let top = texts(&mgr, "shaixuan", 40);
    assert_eq!(
        top.first().map(|s| s.as_str()),
        Some("筛选"),
        "精确输入下高权重用户词应为首选，实际前3={:?}",
        &top[..3.min(top.len())]
    );
}

/// 用户词**不得因系统补全条数变化而窜到前列**。
///
/// `max_candidates` 同时决定 `completion_limit`（`clamp(30, 1000)`），故传 30 与 300 即可对比
/// 两种补全规模：条数变大后用户词与系统补全同文，不得因此走上无约束的合并分支。
///
/// 刻意不断言「前 N 名完全一致」：取数阶段用 wdat 的 top-N，排序阶段用引擎权重，两种规模
/// 选中的集合本就不同、头部自然会变，那与本用例要钉的用户词约束无关。
#[test]
fn user_word_does_not_surface_when_completion_limit_grows() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir, "stable", &[("shaixuan", "筛选", 2_000_000_000)]);

    for limit in [30usize, 300, 1000] {
        let t = texts(&mgr, "s", limit);
        let head = &t[..10.min(t.len())];
        assert!(
            !head.contains(&"筛选".to_string()),
            "补全 {limit} 条时高权重用户词不应出现在前 10：{head:?}"
        );
        assert_ne!(
            t.first().map(|s| s.as_str()),
            Some("筛选"),
            "补全 {limit} 条时用户词不应为首选"
        );
    }
}
