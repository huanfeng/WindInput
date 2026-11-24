//! 简拼召回的两道窗口：索引取码上限，与前缀回退里系统层/用户层的配额分配。
//!
//! 两条都不是排序问题：它们在召回层就把词丢了，候选窗开到 300 也捞不回来，调频同样够不着
//! （位次重排只能重排已经进了列表的候选）。纯简拼与混合简拼查的是同一张 `AbbrevSection`，
//! 窗口必须一致，「打得更省事的写法反而查得更少」就是判据错位。取码上限的依据见
//! `docs/architecture/candidate-sorting-rules.md` 的 `ABBREV_INDEX_LIMIT` 条。
//!
//! 自带 wdat 夹具，不依赖 `build_dev/data`（理由同 `pinyin_mixed_abbrev.rs`：简拼索引只有 mmap
//! 词典才有，依赖真实词库的测试在该目录缺失时会静默跳过）。

use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_store::Store;

/// 简拼键 `bcx` 下的 12 条 3 音节词，权重按真实 `cn_dicts` 量级递减。
///
/// 12 条多于旧的取码上限 10，且目标词「拜城县」刻意排在最后一位（w=1，真实词库里也垫底），
/// 正是「按权重取前 N 个码」会切掉的位置。
const BCX_ENTRIES: &[(&str, &str, i32, u64)] = &[
    // 码, 词, 权重, boundary（bit i = 位置 i 是音节起点）
    ("buchuxian", "不出现", 517, 0b100101), // bu|chu|xian   → 0,2,5
    ("beichexiao", "被撤销", 274, 0b1001001), // bei|che|xiao  → 0,3,6
    ("beichongxin", "被重新", 221, 0b100001001), // bei|chong|xin → 0,3,8
    ("bucengxiang", "不曾想", 201, 0b1000101), // bu|ceng|xiang → 0,2,6
    ("buchixiang", "不吃香", 160, 0b100101), // bu|chi|xiang  → 0,2,5
    ("beichaoxian", "北朝鲜", 133, 0b10001001), // bei|chao|xian → 0,3,7
    ("bichuxi", "必出席", 117, 0b100101),   // bi|chu|xi     → 0,2,5
    ("buchuxi", "不出席", 109, 0b100101),   // bu|chu|xi     → 0,2,5
    ("bucaoxin", "不操心", 105, 0b100101),  // bu|cao|xin    → 0,2,5
    ("bucaixin", "不采信", 79, 0b100101),   // bu|cai|xin    → 0,2,5
    ("buchangxiao", "不畅销", 71, 0b10000101), // bu|chang|xiao → 0,2,7
    ("baichengxian", "拜城县", 1, 0b100001001), // bai|cheng|xian → 0,3,8
];

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_abbrev_window_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 系统词库：`bcx` 键下 `count` 条词 + 一条无关的 `nihao`（让全拼路径有东西可出）。
fn sys_dict(tag: &str, count: usize) -> CachedDict {
    sys_dict_with(tag, &BCX_ENTRIES[..count.min(BCX_ENTRIES.len())])
}

/// 同 [`sys_dict`]，但 `bcx` 键下放的是指定的那几条。
fn sys_dict_with(tag: &str, picked: &[(&str, &str, i32, u64)]) -> CachedDict {
    let dir = tmp_dir(tag);
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("nihao".into(), vec![("你好".into(), 5328, 0, 0b101)]);
    w.add_abbrev("nh".into(), vec![("nihao".into(), 5328)]);

    for (code, text, weight, boundary) in picked {
        w.add_with_boundary(
            (*code).into(),
            vec![((*text).into(), *weight, 0, *boundary)],
        );
    }
    // AbbrevSection 存的是全拼码（v5），按权重降序供 `search_abbrev` 截断。
    w.add_abbrev(
        "bcx".into(),
        picked
            .iter()
            .map(|(c, _, w, _)| ((*c).into(), *w))
            .collect(),
    );
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn store(tag: &str) -> Arc<Store> {
    let p = std::env::temp_dir().join(format!("wind_abbrev_window_{tag}.redb"));
    let _ = std::fs::remove_file(&p);
    Arc::new(Store::open(&p).unwrap())
}

fn engine(tag: &str, count: usize) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), sys_dict(tag, count))
}

fn engine_with_store(tag: &str, count: usize, s: Arc<Store>) -> PinyinEngine {
    engine_with_store_dict(sys_dict(tag, count), s)
}

fn engine_with_store_dict(dict: CachedDict, s: Arc<Store>) -> PinyinEngine {
    let dm = wind_dict::manager::DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        s.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(s, "pinyin")));
    PinyinEngine::new(PyConfig::default(), dict).with_store_layers(Arc::new(dm))
}

fn texts(e: &PinyinEngine, input: &str, max: usize) -> Vec<String> {
    e.convert(input, max)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

/// Stage 1：纯简拼的取码窗口必须容得下整个键。
///
/// 「纯简拼键即答案，取前 N 就是最终候选」只在键下词条少于 N 时成立；截断在召回层，调频也救不回来。
#[test]
fn plain_abbrev_recall_window_covers_the_whole_key() {
    let e = engine("plain_window", BCX_ENTRIES.len());
    let t = texts(&e, "bcx", 300);

    assert!(
        t.contains(&"拜城县".to_string()),
        "键 bcx 下第 12 条（w=1）应召回得到，实际候选: {t:?}"
    );
    // 顺带钉住「不是靠放大候选窗蒙对的」：热门词当然也都在。
    assert!(t.contains(&"不出现".to_string()), "首条也应在: {t:?}");
}

/// Stage 1b：混合简拼与纯简拼查的是同一张表，窗口必须一致。
///
/// `baicx`（bai + c + x）与 `bcx` 指向同一个键，用户敲得更详细，不该反而召回得更少。
#[test]
fn mixed_and_plain_abbrev_share_one_recall_window() {
    let e = engine("share_window", BCX_ENTRIES.len());
    let plain = texts(&e, "bcx", 300);
    let mixed = texts(&e, "baicx", 300);

    assert!(
        mixed.contains(&"拜城县".to_string()),
        "混合简拼应命中: {mixed:?}"
    );
    assert!(
        plain.contains(&"拜城县".to_string()),
        "纯简拼同样应命中（同一张表、同一个键）: {plain:?}"
    );
}

/// Stage 2：前缀回退里，用户词不与系统词抢同一份配额。
///
/// `bcxrmzf` 想要的是「拜城县」+ 后续分段。系统层 ①② 与用户层 ③④ 各记各的配额基准
/// （`abbrev_fallback.per_cut_*`）；共用一份时系统高频词占满席位，用户词在长输入下静默出局，
/// 而那正是最需要分段上屏、自动造词的场景。
#[test]
fn prefix_fallback_gives_store_layer_its_own_quota() {
    let s = store("fallback_quota");
    s.add_user_word("pinyin", "baichengxian", "拜城县", 1200, 0b100001001)
        .unwrap();
    // 系统层 6 条（旧的单切点配额值）。出厂 `per_cut_long` 已是 50，这里已占不满配额，
    // 用户词即使与系统层共享配额也进得来。
    let e = engine_with_store("fallback_quota", 6, s);

    let long = texts(&e, "bcxrmzf", 300);
    assert!(
        long.contains(&"拜城县".to_string()),
        "长串下用户词应仍进候选（它只解释前 3 码，走部分上屏）: {long:?}"
    );

    // 对照：短串本来就出得来，不能因为改配额把它弄丢。
    let short = texts(&e, "bcx", 300);
    assert!(
        short.contains(&"拜城县".to_string()),
        "短串下用户词本就该在: {short:?}"
    );
}

/// 按文本找候选，返回 `(位次, 候选)`。
fn cand<'a>(
    r: &'a [wind_candidate::Candidate],
    text: &str,
) -> Option<(usize, &'a wind_candidate::Candidate)> {
    r.iter().enumerate().find(|(_, c)| c.text == text)
}

/// 同文合并：系统库已有的词（真实词库里「拜城县」w=1），用户学过之后简拼下也要认用户的权重，
/// 并带上来源标记。简拼的用户层召回（整串 step 6.1 与前缀回退 ③④）遇到同文系统候选不能直接跳过，
/// 要与全拼（step 6）一样同文合并、weight 取 max。
#[test]
fn learned_word_already_in_system_dict_is_lifted_in_plain_abbrev() {
    let s = store("merge_plain");
    s.learn_temp_word("pinyin", "baichengxian", "拜城县", 800, 0b100001001)
        .unwrap();
    let e = engine_with_store("merge_plain", BCX_ENTRIES.len(), s);
    let r = e.convert("bcx", 300).unwrap().candidates;
    let (i, c) = cand(&r, "拜城县").expect("应召回");
    assert_eq!(
        i,
        0,
        "w=800 高于键下所有系统词（最高 517），应排首位: {:?}",
        texts(&e, "bcx", 5)
    );
    assert!(
        c.meta.is_temp_dict,
        "合并后要带临时词标记（右键删除按它选表）"
    );
}

#[test]
fn learned_word_already_in_system_dict_is_lifted_in_mixed_abbrev() {
    let s = store("merge_mixed");
    s.learn_temp_word("pinyin", "baichengxian", "拜城县", 800, 0b100001001)
        .unwrap();
    let e = engine_with_store("merge_mixed", BCX_ENTRIES.len(), s);
    let r = e.convert("baicx", 300).unwrap().candidates;
    let (_, c) = cand(&r, "拜城县").expect("应召回");
    assert_eq!(c.weight, 800, "混合简拼下同样取用户权重");
    assert!(c.meta.is_temp_dict);
}

/// 前缀回退（`bcxrmzf` 的分段候选）同理。系统层只放两条，让「拜城县」先由系统层 ① 推进切点
/// `bcx` 的配额，用户层 ③④ 再命中它时走的是合并分支，而不是新推入分支。
#[test]
fn learned_word_already_in_system_dict_is_lifted_in_prefix_fallback() {
    let s = store("merge_fallback");
    s.learn_temp_word("pinyin", "baichengxian", "拜城县", 800, 0b100001001)
        .unwrap();
    let picked = [BCX_ENTRIES[0], BCX_ENTRIES[11]];
    assert_eq!(picked[1].1, "拜城县", "夹具前提");
    let e = engine_with_store_dict(sys_dict_with("merge_fallback", &picked), s);
    let r = e.convert("bcxrmzf", 300).unwrap().candidates;
    let (_, c) = cand(&r, "拜城县").expect("应召回");
    assert!(c.is_partial, "前提：这是前缀回退的分段候选");
    assert_eq!(c.weight, 800, "合并后取用户权重");
    assert!(c.meta.is_temp_dict);
}

/// 同一个词同时在用户层与临时层：合并后两个来源标记都要在，右键删除才会两张表都删。
///
/// `merge_search` 已把两层同文合成一条、两个标记都为真；合并函数不能写成 if/else 只置一个。
#[test]
fn merge_keeps_both_store_origins() {
    let s = store("merge_both");
    s.add_user_word("pinyin", "baichengxian", "拜城县", 1200, 0b100001001)
        .unwrap();
    s.learn_temp_word("pinyin", "baichengxian", "拜城县", 800, 0b100001001)
        .unwrap();
    let e = engine_with_store("merge_both", BCX_ENTRIES.len(), s);
    let r = e.convert("bcx", 300).unwrap().candidates;
    let (_, c) = cand(&r, "拜城县").expect("应召回");
    assert!(
        c.meta.is_user_dict && c.meta.is_temp_dict,
        "两层来源都要保留: user={} temp={}",
        c.meta.is_user_dict,
        c.meta.is_temp_dict
    );
}

/// 合并只提权不降权（同 step 6）：用户权重低于系统时保留系统值。
#[test]
fn merge_never_lowers_system_weight() {
    let s = store("merge_lower");
    // 「不出现」系统 w=517，用户库里给它 10。
    s.add_user_word("pinyin", "buchuxian", "不出现", 10, 0b100101)
        .unwrap();
    let e = engine_with_store("merge_lower", BCX_ENTRIES.len(), s);
    let r = e.convert("bcx", 300).unwrap().candidates;
    let (i, c) = cand(&r, "不出现").expect("应召回");
    assert_eq!((i, c.weight), (0, 517));
    assert!(c.meta.is_user_dict);
}
