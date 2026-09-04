//! 用户词在**短输入**（单字母 / 远距离前缀）下不得夺走首选。
//!
//! 背景：step 6 合并用户词时，同文分支原先无条件 `existing.weight.max(c.weight)`，
//! 绕过了新增分支的 `should_promote_user_completion` 与 `promotion_cap` 两道约束。
//!
//! 系统前缀补全的取数放开后（`completion_limit` 由固定 30 改为跟随请求量），用户词与系统
//! 候选同文的概率大增，该缺口随之显形：配了高权重的用户词「筛选」(shaixuan) 会在只打一个
//! `s` 时被抬到 20 亿权重、夺走首选，把「是」「上」这些高频单字挤下去。
//!
//! 修复是让合并分支与新增分支**对称**。本测试同时钉三件事：
//! 1. 短输入下用户词不上位（正例）；
//! 2. 精确输入下用户提权仍全效（**反向对照**——否则"修复"可能是把用户词整个废掉）；
//! 3. 排位不随系统补全条数变化（该 bug 的直接特征）。
//!
//! 词库缺失时自动跳过。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wind_config::Config;
use wind_engine::EngineManager;
use wind_store::Store;

/// 词库目录。`build_dev/data` 优先，回退到 `build/data`。
///
/// ⚠️ **回退不是多余的**：`build_dev/data` 由构建流程删除后重建，重建窗口内它整个不存在。
/// 本测试族命中过一次——三个用例齐刷刷走「跳过」分支、报 `3 passed`、耗时 0.00s，
/// 看上去与真跑通过毫无区别。**判据是耗时**：真跑约 0.2s 以上。
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
/// 权重取 20 亿（与 `pinyin_user_word_merge` 同款极端值）：它远超任何系统词
/// （单字「是」约 1180 万），若约束失效必然排到第 0 位，判据不会似是而非。
///
/// ## 语义变更（step 6.3 音节数闸门上线后）
///
/// 本条原先还断言「筛选」**仍应可达**（约束是"不上浮"而非"丢弃"）。
/// [`STRICT_SYLLABLE_MATCH_MAX`] 上线后这一半反转了：`s` 只表达 1 个音节，
/// 2 音节的「筛选」与系统词「所以」「时间」受同一条规则处置 —— **一律不产出**。
/// 用户词在此不享有豁免，否则「加了词的人看到词组、没加的人看不到」，
/// 同一个输入长度下行为不一致。可达性由 `user_word_returns_when_input_grows`
/// 反向对照守住：多打一个字母它就回来。
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

/// **反向对照**：输入一长，用户词必须回来。
///
/// 缺了这条，上面那个断言可以靠「把用户词整个废掉」来满足 —— 那是远更严重的 bug
/// （用户加的词永远打不出来）。
///
/// ⚠️ 门槛落在 `shaix` 而非 `sh`/`shai`：「筛选」切分为 `shai|xuan`，第 2 个音节起始位
/// 是 4，输入要长到 5 字节才把它圈进来（used 才从 1 变 2）。`sh`(2)、`shai`(4) 仍是
/// 「只起了 1 个音节的头」，此时不出 2 音节词是**正确**行为 —— 与 `dian` 要打到
/// `dianh` 才出「电话」完全同构。
///
/// ⚠️ **2026-09-04：`shaixu` 从这个列表里移除**（音节边界对齐上线）。它的 DAG 切分是
/// `shai|xu`，而 `xu` 本身是合法音节 —— 放「筛选」(`shai|xuan`) 出来就等于允许把已打完
/// 的音节继续拉长，那正是「打 `shen` 出 `sheng` 的字」的同一条通道。两者在音节结构上
/// 完全同形（`shaixuan` 到 `xuan` 结束、`sheng` 到 `sheng` 结束），不可能只挡一个；
/// 主流拼音输入法取的也都是严格档。判据见 `wind_dict::cached::prefix_syllable_aligned`。
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

/// 上一条的**边界侧**：`shaixu` 下「筛选」不可达，是音节边界对齐的**预期结果**，不是
/// 「用户词失效」。
///
/// 两条合起来才说得清拦的是什么：拦的是「跨越用户已打完的音节 `xu`」，不是「用户词」。
/// 同一个词在残码位（`shaix`）与打完整（`shaixuan`）时都照常可达 —— 上一条钉的正是这个。
/// 少了本条，日后有人把 `shaixu` 加回去、或把对齐判据整个回退，都不会有任何测试变红。
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
/// 缺了这条，上面那个断言可以靠「把用户词彻底废掉」来满足 —— 那是另一个更严重的 bug。
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
/// 这是该 bug 最直接的特征：同一份用户词，补全取 30 条时沉底、取 300 条时却成了首选
/// —— 因为条数变大后它与系统补全同文，走上了无约束的合并分支。
/// `max_candidates` 同时决定 `completion_limit`（`clamp(30, 1000)`），故传 30 与 300
/// 即可对比两种补全规模。
///
/// ⚠️ **刻意不断言「前 N 名完全一致」**。候选的取数阶段用词库原始权重（wdat 的 top-N），
/// 排序阶段用引擎权重（unigram 放大后的字频），两者不是同一套：实测 wdat 对 `s` 返回的
/// top-30 是「是/所以/上/时/说/虽然…」，而引擎重排后是「是/上/时/说/所/岁…」。于是取
/// 30 条与取 300 条选中的**集合本就不同**，头部自然会变。那是取数键与排序键不一致这个
/// **既存架构问题**，与本用例要钉的用户词约束无关；放开补全反而缓解了它（更多高频单字
/// 得以进入候选）。若在此断言前 N 名一致，测试会因那个无关问题而红。
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
