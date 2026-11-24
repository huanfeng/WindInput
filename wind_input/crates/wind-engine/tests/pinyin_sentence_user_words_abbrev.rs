//! 简拼整句也认用户词，且临时词也算用户词。
//!
//! 场景：系统词库没有的「拜城县」上屏过一次进了临时层，之后打 `bcxrmzf` 整句要能用上它。
//! 需要两点同时成立：
//!
//! 1. `add_store_nodes` 收临时词：临时词本身就是上屏过的词，「用过即转正」的质量闸在
//!    草稿 → 临时这一跳；滑窗杂词住在草稿层，照旧不进图。
//! 2. 简拼整句（step ②b）的用户词走简拼查询：按全拼码点查对声母串 `bcx` 必然落空，
//!    系统词有 `add_abbrev_nodes`，用户层也要有对应的一半。
//!
//! 自带 wdat 夹具 + 真 redb store，不依赖 `build_dev/data`。

use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_store::Store;

/// bai|cheng|xian → 位 0/3/8
const BCX_BOUNDARY: u64 = 0b100001001;

/// 系统词库：`bcx` 下只有「不出现」，`rmzf` 下是「人民政府」，没有「拜城县」。
fn sys_dict(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_s2abbr_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    // bu|chu|xian → 0/2/5
    w.add_with_boundary(
        "buchuxian".into(),
        vec![("不出现".into(), 517, 0, 0b100101)],
    );
    // ren|min|zheng|fu → 0/3/6/11
    w.add_with_boundary(
        "renminzhengfu".into(),
        vec![("人民政府".into(), 90_000, 0, 0b100001001001)],
    );
    w.add_abbrev("bcx".into(), vec![("buchuxian".into(), 517)]);
    w.add_abbrev("rmzf".into(), vec![("renminzhengfu".into(), 90_000)]);
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn store(tag: &str) -> Arc<Store> {
    let p = std::env::temp_dir().join(format!("wind_s2abbr_{tag}.redb"));
    let _ = std::fs::remove_file(&p);
    Arc::new(Store::open(&p).unwrap())
}

fn engine(tag: &str, s: Arc<Store>, on: bool) -> PinyinEngine {
    let dm = wind_dict::manager::DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        s.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(s, "pinyin")));
    let cfg = PyConfig {
        sentence_uses_user_words: on,
        ..Default::default()
    };
    PinyinEngine::new(cfg, sys_dict(tag)).with_store_layers(Arc::new(dm))
}

fn sentence(e: &PinyinEngine, input: &str) -> Option<String> {
    e.convert(input, 100)
        .ok()?
        .candidates
        .into_iter()
        .find(|c| c.is_sentence)
        .map(|c| c.text)
}

/// 临时词 + 纯简拼长串。
#[test]
fn temp_word_joins_abbrev_sentence() {
    let s = store("temp");
    // 出厂自动学词权重（coordinator `LEARN_ADD_WEIGHT`）。
    s.learn_temp_word("pinyin", "baichengxian", "拜城县", 800, BCX_BOUNDARY)
        .unwrap();

    let off = sentence(&engine("temp_off", s.clone(), false), "bcxrmzf");
    assert_eq!(
        off.as_deref(),
        Some("不出现人民政府"),
        "前提：开关关闭时整句只认系统词"
    );
    let on = sentence(&engine("temp_on", s, true), "bcxrmzf");
    assert_eq!(on.as_deref(), Some("拜城县人民政府"));
}

/// 已晋升的用户词同样要能走简拼段（第 2 道闸与临时/晋升无关）。
#[test]
fn promoted_word_joins_abbrev_sentence() {
    let s = store("user");
    s.add_user_word("pinyin", "baichengxian", "拜城县", 1200, BCX_BOUNDARY)
        .unwrap();
    let on = sentence(&engine("user_on", s, true), "bcxrmzf");
    assert_eq!(on.as_deref(), Some("拜城县人民政府"));
}

/// `boundary == 0` 的词（手输码、旧版扁平导入）算不出声母串，不能进简拼整句：整句节点要求
/// 真值切分（同 `add_store_nodes` 约束 2）。
///
/// 两道闸：`search_abbrev_exact` 在存储层就不交回无边界组，`syllables_from_boundary` 的切分
/// 校验是第二道。单去任一道不会红，两道同时去掉才红。
#[test]
fn abbrev_store_node_rejects_words_without_boundary() {
    let s = store("nobound");
    s.learn_temp_word("pinyin", "baichengxian", "拜城县", 800, 0)
        .unwrap();
    let on = sentence(&engine("nobound_on", s, true), "bcxrmzf");
    assert_eq!(
        on.as_deref(),
        Some("不出现人民政府"),
        "无边界的词不该进简拼整句"
    );
}

/// 用户层简拼查询不能在存储层按索引键字典序截断：那发生在权重排序与判据之前，还会把
/// 无边界组（`boundary == 0` 的词按码首字母挂着）一并扫进来占名额。
///
/// 场景一：用户手动加过 ≥8 个 b 开头的词（boundary=0，必被判据拒收），不能因此把临时词
/// 「拜城县」挤出图。
#[test]
fn no_boundary_words_do_not_crowd_out_the_abbrev_hit() {
    let s = store("crowd_nobound");
    for i in 0..10 {
        s.add_user_word("pinyin", &format!("ba{i}"), &format!("手{i}"), 1200, 0)
            .unwrap();
    }
    s.learn_temp_word("pinyin", "baichengxian", "拜城县", 800, BCX_BOUNDARY)
        .unwrap();
    let on = sentence(&engine("crowd_nobound_on", s, true), "bcxrmzf");
    assert_eq!(on.as_deref(), Some("拜城县人民政府"));
}

/// 场景二：同一声母组词多，目标词的码字典序靠后（`bachaxi` < `baichengxian`），
/// 应按权重取，不能按键序截掉权重最高的它。
#[test]
fn abbrev_hits_are_ranked_by_weight_not_key_order() {
    let s = store("crowd_group");
    // ba|cha|xi → 位 0/2/5：投影同为 `bcx`，码的字典序排在 `baichengxian` 之前。
    for i in 0..10 {
        s.add_user_word("pinyin", "bachaxi", &format!("巴{i}"), 10, 0b100101)
            .unwrap();
    }
    // 与竞争词同在用户层：各层分别截断后才合并，放在临时层它那层没有对手。
    s.add_user_word("pinyin", "baichengxian", "拜城县", 1200, BCX_BOUNDARY)
        .unwrap();
    let on = sentence(&engine("crowd_group_on", s, true), "bcxrmzf");
    assert_eq!(on.as_deref(), Some("拜城县人民政府"));
}

/// 由音节序列算 (码, 边界)。
fn code_and_mask(syls: &[&str]) -> (String, u64) {
    let mut code = String::new();
    let mut mask = 0u64;
    for s in syls {
        mask |= 1 << code.len();
        code.push_str(s);
    }
    (code, mask)
}

fn sentence_learn_code(e: &PinyinEngine, input: &str) -> (String, Option<(String, u64)>) {
    let c = e
        .convert(input, 100)
        .unwrap()
        .candidates
        .into_iter()
        .find(|c| c.is_sentence)
        .expect("应产出整句");
    (c.text, c.meta.learn_code)
}

/// 简拼整句（②b）的造词码是完整全拼：简拼节点带回规范码与规范边界（`canon`），
/// 不把击键字母 `bcx` / `rmzf` 写进造词码，那种码几乎打不出来，只会在临时层堆积。
///
/// 系统简拼节点（「不出现」「人民政府」）与用户简拼节点（「拜城县」）两种都验。
#[test]
fn abbrev_sentence_learn_code_is_full_pinyin() {
    let (text, learn) =
        sentence_learn_code(&engine("learn_sys", store("learn_sys"), false), "bcxrmzf");
    assert_eq!(text, "不出现人民政府");
    assert_eq!(
        learn,
        Some(code_and_mask(&[
            "bu", "chu", "xian", "ren", "min", "zheng", "fu"
        ]))
    );

    let s = store("learn_user");
    s.learn_temp_word("pinyin", "baichengxian", "拜城县", 800, BCX_BOUNDARY)
        .unwrap();
    let (text, learn) = sentence_learn_code(&engine("learn_user", s, true), "bcxrmzf");
    assert_eq!(text, "拜城县人民政府");
    assert_eq!(
        learn,
        Some(code_and_mask(&[
            "bai", "cheng", "xian", "ren", "min", "zheng", "fu"
        ])),
        "用户简拼节点取记录自己的规范码"
    );
}
