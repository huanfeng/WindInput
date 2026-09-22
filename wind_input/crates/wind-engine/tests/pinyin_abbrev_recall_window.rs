//! 简拼召回的两道**窗口**：索引取码上限，与前缀回退里系统层/用户层的配额分配。
//!
//! 两条都不是排序问题——它们在**召回层**就把词丢了，所以候选窗开到 300 也捞不回来，
//! 调频同样够不着（位次重排只能重排已经进了列表的候选）。
//!
//! **修复前**的真机快照（`build_dev/data` 真实词库，`wind_repl`，limit=300）：
//!
//! | 输入 | 解释 | 「拜城县」的位次 |
//! |---|---|---|
//! | `baicx` | 混合简拼，取码上限 64 | 第 77 位，**活** |
//! | `bcx`   | 纯简拼，取码上限 **10** | **未产生**（键 `bcx` 下 48 条，它 w=1 排第 48） |
//! | `bcxrmzf` | 前缀回退，切点 `bcx` | **未产生**（6 席被系统高频词占满） |
//!
//! 「打得更省事的写法反而查得更少」是判据错位的信号：两条路径查的是同一张
//! `AbbrevSection`，窗口大小却差 6.4 倍。修复后同一份词库下 `bcx` 的「拜城县」在第 46 位。
//!
//! 自带 wdat 夹具，不依赖 `build_dev/data`（理由同 `pinyin_mixed_abbrev.rs`：
//! 简拼索引只有 mmap 词典才有，依赖真实词库的测试在该目录缺失时会静默跳过、计数照常绿）。

use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_store::Store;

/// 简拼键 `bcx` 下的 12 条 3 音节词，权重按真实 `cn_dicts` 量级递减。
///
/// 12 > 旧的取码上限 10，且目标词「拜城县」刻意排在**最后一位**（w=1，真实词库里
/// 它就是 48/48）——这正是「按权重取前 N 个码」会切掉的位置。
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
    let dir = tmp_dir(tag);
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("nihao".into(), vec![("你好".into(), 5328, 0, 0b101)]);
    w.add_abbrev("nh".into(), vec![("nihao".into(), 5328)]);

    let picked = &BCX_ENTRIES[..count.min(BCX_ENTRIES.len())];
    for (code, text, weight, boundary) in picked {
        w.add_with_boundary(
            (*code).into(),
            vec![((*text).into(), *weight, 0, *boundary)],
        );
    }
    // AbbrevSection 存的是**全拼码**（v5），按权重降序供 `search_abbrev` 截断。
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
    let dm = wind_dict::manager::DictManager::new();
    dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
        s.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_dict::StoreTempLayer::new(s, "pinyin")));
    PinyinEngine::new(PyConfig::default(), sys_dict(tag, count)).with_store_layers(Arc::new(dm))
}

fn texts(e: &PinyinEngine, input: &str, max: usize) -> Vec<String> {
    e.convert(input, max)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

/// Stage 1：纯简拼的取码窗口必须容得下整个键。
///
/// 旧值 10 的理由是「纯简拼键即答案，取前 10 就是最终候选」——该前提只在**键下词条
/// 少于 10** 时成立。真实词库里 `bcx` 有 48 条，取 10 等于宣布「这个键下只有最热门的
/// 10 个词存在」，且因为截断在召回层，**调频也救不回来**：位次重排只能重排已经进了
/// 列表的候选。
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
/// `bcxrmzf` 想要的是「拜城县」+ 后续分段。切点 `bcx` 上系统层有 6 条高权重词，
/// 恰好占满 `MAX_FALLBACK_PER_CUT`；用户自己造的「拜城县」排在 ③④ 步、共享同一份
/// 配额 ⇒ 静默出局。用户的观感是「短的打得出、长的就没了」，而这恰恰是他**最需要**
/// 分段上屏的场景（一次输入长串连续部分上屏，是自动造词唯一走得通的路径）。
#[test]
fn prefix_fallback_gives_store_layer_its_own_quota() {
    let s = store("fallback_quota");
    s.add_user_word("pinyin", "baichengxian", "拜城县", 1200, 0b100001001)
        .unwrap();
    // 系统层给满 6 条（正好是单切点配额），用户词若与它们共享配额就必被挤掉。
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
