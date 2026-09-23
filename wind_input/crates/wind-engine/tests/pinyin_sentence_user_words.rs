//! S2：已晋升的用户词参与整句解码。
//!
//! 整句词图此前只从系统词库建（`lattice.rs::build` 只吃一个 `CachedDict`），用户词挂在
//! `PinyinEngine::store_layers` 上、两者从不相交 ⇒ 自造词**根本没参与整句分词**：
//! 「盖伦」单独打得出、「有盖伦吗」被打散（t134）；手动调过权重的词在整句里也不认（GH#93）。
//!
//! ⚠️ **整句没有 N-best**（`ViterbiResult` 只有一条 `words`）：用户词进图是赢者通吃，
//! 赢了整句就变、输了什么都看不见。故本文件的断言都落在「整句那一条」上，
//! 而不是「候选列表里有没有」—— 后者早就有了（step 6 的 store 层召回），不是本项要修的。
//!
//! 自带 wdat 夹具 + 真 redb store，不依赖 `build_dev/data`。

use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_store::Store;

/// 系统词库：够组出一句「有盖伦吗」的竞争者，但**没有**「盖伦」这个词。
///
/// `gai`/`lun` 各自的单字都在，所以不开 S2 时整句会把它拆成两个单字（或别的组合），
/// 这正是 t134 的现场。
fn sys_dict(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_s2_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // 单字（虚词「有」「吗」给足权重，让整句本身成立）。
    // ⚠️ 同一个 code 只能 `add_with_boundary` **一次**：writer 不去重，而 DAT 构建
    // 假设编码唯一（`datformat.rs:114`「唯一编码 → 区间内至多一个 len==depth」），
    // 同码分两次加会在建树时越界 panic。故同码的词合并成一个 vec。
    for (code, entries) in [
        ("you", vec![("有", 500_000)]),
        ("ma", vec![("吗", 300_000)]),
        ("gai", vec![("该", 80_000), ("盖", 20_000)]),
        ("lun", vec![("论", 60_000), ("轮", 30_000)]),
        ("gou", vec![("狗", 40_000)]),
    ] {
        w.add_with_boundary(
            code.into(),
            entries
                .into_iter()
                .enumerate()
                .map(|(i, (t, wt))| (t.into(), wt, i as u32, 0b1))
                .collect(),
        );
    }
    // `gailun` 下放**真机的实际数值**：cn_dicts 里「概论」w=1217（`base.dict.yaml:84218`），
    // 而手动加词的出厂权重是 1200（`handle_addword.rs::ADD_WORD_WEIGHT`）——**低 17 分**。
    // 用户按设计流程走完（造词 + 开开关）后整句仍然不认，正是 `USER_NODE_BONUS` 要解决的。
    // gai|lun → 位 0/3
    w.add_with_boundary("gailun".into(), vec![("概论".into(), 1217, 0, 0b1001)]);
    // 另给 `goulun` 放一个高权重系统词，供 GH#93（用户调权重覆盖系统词）单独用。
    w.add_with_boundary("goulun".into(), vec![("狗论".into(), 90_000, 0, 0b1001)]);
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn store(tag: &str) -> Arc<Store> {
    let p = std::env::temp_dir().join(format!("wind_s2_{tag}.redb"));
    let _ = std::fs::remove_file(&p);
    Arc::new(Store::open(&p).unwrap())
}

fn engine(tag: &str, s: Arc<Store>, on: bool) -> PinyinEngine {
    let dm = wind_dict::manager::DictManager::new();
    dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
        s.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_dict::StoreTempLayer::new(s, "pinyin")));
    let cfg = PyConfig {
        sentence_uses_user_words: on,
        ..Default::default()
    };
    PinyinEngine::new(cfg, sys_dict(tag)).with_store_layers(Arc::new(dm))
}

/// 取整句候选（`is_sentence`）的文本。整句只有一条，故返回 Option。
fn sentence(e: &PinyinEngine, input: &str) -> Option<String> {
    e.convert(input, 100)
        .ok()?
        .candidates
        .into_iter()
        .find(|c| c.is_sentence)
        .map(|c| c.text)
}

/// t134：造过「盖伦」之后，整句里也要认它。
///
/// ⚠️ 这一条同时钉住 [`USER_NODE_BONUS`] 的存在意义：用户词权重取的是**手动加词的出厂值
/// 1200**，而同码的「概论」w=1217 比它高。没有那份加成时，用户按设计流程走完
/// （造词 + 开开关）整句**仍然不认** —— 那等于这个功能对默认路径上的用户不存在。
#[test]
fn promoted_user_word_joins_the_sentence() {
    let s = store("joins");
    // gai|lun → 位 0/3
    s.add_user_word("pinyin", "gailun", "盖伦", 1200, 0b1001)
        .unwrap();

    let off = sentence(&engine("joins_off", s.clone(), false), "yougailunma");
    assert!(
        !off.as_deref().unwrap_or("").contains("盖伦"),
        "开关关闭时用户词不该进整句，实际: {off:?}"
    );

    let on = sentence(&engine("joins_on", s, true), "yougailunma");
    assert_eq!(
        on.as_deref(),
        Some("有盖伦吗"),
        "开关打开后整句应认得用户词"
    );
}

/// GH#93：用户给**系统词库已有**的词调了权重，整句要认新权重。
///
/// 这一条钉的是 `add_store_nodes` 的第三条约束——同词同起点时取 `log_prob` 较大者，
/// 而不是像简拼节点那样「已存在就跳过」。跳过的话用户词对所有已在系统库里的词完全无效，
/// 而 GH#93 正是「整句输入无法记忆手动调整过的词」。
#[test]
fn user_weight_overrides_the_same_word_from_system_dict() {
    // 对照：不加用户词时，系统库的「狗论」(w=90000) 本就赢下这一句。
    let base = store("override_base");
    assert_eq!(
        sentence(&engine("override_base", base, true), "yougoulunma").as_deref(),
        Some("有狗论吗"),
        "前提：系统词本来是赢家，否则下面的断言证明不了任何事"
    );

    let s = store("override");
    // 用户给同一串码造了别的词并调高权重 —— 整句要认新权重。
    s.add_user_word("pinyin", "goulun", "狗轮", 200_000, 0b1001)
        .unwrap();
    assert_eq!(
        sentence(&engine("override_on", s, true), "yougoulunma").as_deref(),
        Some("有狗轮吗"),
        "用户词权重高于系统同码词时，整句应改用它"
    );
}

/// 加成是「势均力敌时胜出」，**不是碾压**：用户词不该压过真正的高频词。
///
/// `USER_NODE_BONUS` = +2.0 ≈ weight ×7.4。出厂加词 1200 等效到 ~8900，翻得过同码的
/// 中频词（概论 1217），但仍然输给 90000 那一档 —— 后者是「这个词就是比你那个常用得多」，
/// 用户没有明确表态要改的情况下不该被掀翻。
#[test]
fn bonus_wins_close_calls_but_does_not_crush_high_frequency_words() {
    let s = store("notcrush");
    // 与上一条同样的出厂权重，但这次对手是 w=90000 的「狗论」。
    s.add_user_word("pinyin", "goulun", "狗轮", 1200, 0b1001)
        .unwrap();
    assert_eq!(
        sentence(&engine("notcrush_on", s, true), "yougoulunma").as_deref(),
        Some("有狗论吗"),
        "加成不该让出厂权重的用户词压过高频系统词"
    );
}

/// 硬约束：**临时词不进图**。滑窗草稿会造大量杂词，「用过即转正」才是质量闸。
#[test]
fn temp_words_never_enter_the_lattice() {
    let s = store("temp");
    s.learn_temp_word("pinyin", "gailun", "盖伦", 1200, 0b1001)
        .unwrap();
    let got = sentence(&engine("temp_on", s, true), "yougailunma");
    assert!(
        !got.as_deref().unwrap_or("").contains("盖伦"),
        "临时词不得进整句词图，实际: {got:?}"
    );
}

/// 硬约束：**无 boundary 的用户词不进图**（隐性造词 / 手输码）。
///
/// 与 `build` 对系统词的「`boundary == 0` 降级放行」相反：整句的每个节点都要求真值切分，
/// 放进去等于让 Viterbi 按猜出来的切分组句。
#[test]
fn user_words_without_boundary_are_skipped() {
    let s = store("noboundary");
    s.add_user_word("pinyin", "gailun", "盖伦", 1200, 0)
        .unwrap();
    let got = sentence(&engine("noboundary_on", s, true), "yougailunma");
    assert!(
        !got.as_deref().unwrap_or("").contains("盖伦"),
        "无边界用户词不得进整句词图，实际: {got:?}"
    );
}

/// 边界与跨度不符的用户词也要拒（`MaskCheck::Reject`）。
///
/// `gailun` 的合法切分是 gai|lun（位 0/3）；标成位 0/2（ga|ilun）不是这串键敲得出的。
#[test]
fn user_words_with_incompatible_boundary_are_rejected() {
    let s = store("badboundary");
    s.add_user_word("pinyin", "gailun", "盖伦", 1200, 0b101)
        .unwrap();
    let got = sentence(&engine("badboundary_on", s, true), "yougailunma");
    assert!(
        !got.as_deref().unwrap_or("").contains("盖伦"),
        "边界与跨度不符的用户词不得进图，实际: {got:?}"
    );
}

/// 开关关闭时，整句结果与「根本没有 store 层」**逐位相同**。
///
/// 这是出厂默认路径上唯一要保的性质：老用户不该因为这次改动看到任何变化。
#[test]
fn disabled_switch_is_bit_identical_to_no_store_layer() {
    let s = store("identical");
    s.add_user_word("pinyin", "gailun", "盖伦", 2_000_000_000, 0b1001)
        .unwrap();

    let with_store = engine("identical_with", s, false);
    let without = PinyinEngine::new(PyConfig::default(), sys_dict("identical_without"));

    for input in ["yougailunma", "gailun", "yougoulunma", "youma"] {
        assert_eq!(
            sentence(&with_store, input),
            sentence(&without, input),
            "输入 {input}：关闭开关后整句必须与无 store 层时逐位相同"
        );
    }
}

/// weight 上限截断：导入词库可以带 `w=2e9`，那时 `ln(w/DICT_TOTAL)` 会变成正数，
/// 比系统最大词还高近 5 个自然对数单位，整句会变成用户词拼接秀。
///
/// 截断后它仍然赢（本就该赢），但赢的幅度落在词频轴内 —— 这里断言的是**不 panic、
/// 不溢出、且行为与一个正常高权重用户词一致**。
#[test]
fn extreme_weight_is_capped() {
    let s = store("cap");
    s.add_user_word("pinyin", "gailun", "盖伦", 2_000_000_000, 0b1001)
        .unwrap();
    assert_eq!(
        sentence(&engine("cap_on", s, true), "yougailunma").as_deref(),
        Some("有盖伦吗")
    );

    let s2 = store("cap2");
    s2.add_user_word("pinyin", "gailun", "盖伦", 1_000_000, 0b1001)
        .unwrap();
    assert_eq!(
        sentence(&engine("cap2_on", s2, true), "yougailunma").as_deref(),
        Some("有盖伦吗"),
        "截断值本身与超大值应给出同样的结果"
    );
}
