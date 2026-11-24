//! S2：用户上屏过的词（用户词 + 临时词）参与整句解码。
//!
//! 整句词图从系统词库建，用户词挂在 `PinyinEngine::store_layers` 上，需由 `add_store_nodes`
//! 并进图，否则「盖伦」单独打得出、「有盖伦吗」被打散，调过权重的词在整句里也不认。
//!
//! 出厂只出一条整句，用户词进图是赢者通吃，故断言都落在「整句那一条」上，而不是「候选列表
//! 里有没有」（那由 step 6 的 store 层召回负责）。
//!
//! 自带 wdat 夹具 + 真 redb store，不依赖 `build_dev/data`。

use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_store::Store;

/// 系统词库：够组出一句「有盖伦吗」的竞争者，但没有「盖伦」这个词；
/// `gai`/`lun` 的单字都在，不开 S2 时整句会把它拆开。
fn sys_dict(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_s2_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // 单字（虚词「有」「吗」给足权重，让整句本身成立）。
    // 同一个 code 只能 `add_with_boundary` 一次：writer 不去重，DAT 构建假设编码唯一，
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
    // `gailun` 下取真实词库的数值：「概论」w=1217，比手动加词出厂权重 1200
    // （`handle_addword.rs::ADD_WORD_WEIGHT`）高，靠 `USER_NODE_BONUS` 才翻得过。
    // gai|lun → 位 0/3
    w.add_with_boundary("gailun".into(), vec![("概论".into(), 1217, 0, 0b1001)]);
    // 另给 `goulun` 放一个高权重系统词，供「调权重覆盖系统词」用例单独用。
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

/// 取整句候选（`is_sentence`）的文本。整句只有一条，故返回 Option。
fn sentence(e: &PinyinEngine, input: &str) -> Option<String> {
    e.convert(input, 100)
        .ok()?
        .candidates
        .into_iter()
        .find(|c| c.is_sentence)
        .map(|c| c.text)
}

/// 造过「盖伦」之后，整句里也要认它。
///
/// 同时钉住 [`USER_NODE_BONUS`] 的存在意义：用户词取手动加词出厂值 1200，低于同码「概论」
/// 1217；没有加成时按设计流程走完（造词 + 开开关）整句仍不认。
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

/// 用户给系统词库已有的词调了权重，整句要认新权重。
///
/// 钉住 `add_store_nodes` 同词同起点时取 `log_prob` 较大者，而不是像简拼节点那样
/// 「已存在就跳过」；跳过的话用户词对所有已在系统库里的词都无效。
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

/// 加成是「势均力敌时胜出」，不是碾压：用户词不该压过真正的高频词。
///
/// `USER_NODE_BONUS` = +2.0 ≈ weight ×7.4：出厂 1200 等效约 8900，翻得过概论 1217，
/// 仍输给 90000 那一档；用户没明确表态时不该掀翻真正常用得多的词。
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

/// 残码整句（step 2c）也要认用户词：前半句用自造词、末尾还没打完，是最常见的形态。
///
/// `yougailunm` = 有 + 盖伦 + 残码 `m`（由 `add_partial_final_nodes` 补成「吗」）。
/// 两类节点在同一张图上各管各的跨度，这条用例钉住它们能串起来。
#[test]
fn partial_final_sentence_also_uses_user_words() {
    let s = store("partial");
    s.add_user_word("pinyin", "gailun", "盖伦", 1200, 0b1001)
        .unwrap();
    let got = sentence(&engine("partial_on", s, true), "yougailunm");
    assert!(
        got.as_deref().unwrap_or("").contains("盖伦"),
        "残码整句应认用户词，实际: {got:?}"
    );
}

/// 临时词也进图：它是用户上屏过的词，「用过即转正」这道闸在草稿 → 临时那一跳；
/// 否则系统库没有的词要用够晋升次数才进得了整句（见 `pinyin_sentence_user_words_abbrev.rs`）。
/// 权重取出厂自动学词值 800（coordinator `LEARN_ADD_WEIGHT`），同样要赢过「概论」1217。
#[test]
fn temp_words_enter_the_lattice() {
    let s = store("temp");
    s.learn_temp_word("pinyin", "gailun", "盖伦", 800, 0b1001)
        .unwrap();
    let got = sentence(&engine("temp_on", s, true), "yougailunma");
    assert_eq!(got.as_deref(), Some("有盖伦吗"));
}

/// 硬约束：无 boundary 的用户词不进图（隐性造词 / 手输码）。
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

/// 开关关闭时，整句结果与「根本没有 store 层」逐位相同：出厂默认路径上老用户不该看到变化。
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
/// 截断后它仍然赢（本就该赢），但幅度落在词频轴内；断言不 panic、不溢出、
/// 且行为与一个正常高权重用户词一致。
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

// ── 模糊音：用户词节点同样按模糊打法入图（`add_store_nodes` 的模糊分支）。
//
// 记录存规范码 + 规范码坐标的边界，整句按所打码点查，故模糊打法下需单独的模糊分支，
// 自造词才进得了整句（系统词节点的模糊在 `build` 里）。

/// [`engine`] + 模糊音 n=l。
fn engine_n_l(tag: &str, s: Arc<Store>, on: bool, fuzzy: bool) -> PinyinEngine {
    engine(tag, s, on).with_fuzzy(wind_engine::pinyin::fuzzy::FuzzyConfig {
        n_l: fuzzy,
        ..Default::default()
    })
}

/// 主诉求：n=l 下 `gainun`（`nun` 是模糊拼写层的边）整句认出 `gailun` 的「盖伦」。
#[test]
fn fuzzy_typing_joins_user_word_into_sentence() {
    let s = store("fz_join");
    s.add_user_word("pinyin", "gailun", "盖伦", 1200, 0b1001)
        .unwrap();
    assert_eq!(
        sentence(&engine_n_l("fz_join_on", s, true, true), "yougainunma").as_deref(),
        Some("有盖伦吗"),
        "模糊打法下整句应认得用户词"
    );
}

/// 关模糊音 / 关开关：模糊打法的整句都不出「盖伦」。
#[test]
fn fuzzy_user_word_sentence_needs_both_switches() {
    for (tag, on, fuzzy) in [("fz_off", true, false), ("fz_s2off", false, true)] {
        let s = store(tag);
        s.add_user_word("pinyin", "gailun", "盖伦", 1200, 0b1001)
            .unwrap();
        let got = sentence(&engine_n_l(tag, s, on, fuzzy), "yougainunma");
        assert!(
            !got.as_deref().unwrap_or("").contains("盖伦"),
            "[{tag}] 不该出「盖伦」，实际: {got:?}"
        );
    }
}

/// 边界对不上变体码切分（ga|ilun）、或无边界的记录，模糊分支同样拒收。
#[test]
fn fuzzy_branch_rejects_mismatched_boundary() {
    for (tag, boundary) in [("fz_bad", 0b101u64), ("fz_none", 0)] {
        let s = store(tag);
        s.add_user_word("pinyin", "gailun", "盖伦", 1200, boundary)
            .unwrap();
        let got = sentence(&engine_n_l(tag, s, true, true), "yougainunma");
        assert!(
            !got.as_deref().unwrap_or("").contains("盖伦"),
            "[{tag}] 边界 {boundary:#b} 不得进图，实际: {got:?}"
        );
    }
}

/// 开模糊但打的是精确码：与关模糊时整句逐位相同（模糊分支只多出同词的更低分节点）。
#[test]
fn fuzzy_on_exact_typing_unchanged() {
    let s = store("fz_exact");
    s.add_user_word("pinyin", "gailun", "盖伦", 1200, 0b1001)
        .unwrap();
    for input in ["yougailunma", "yougoulunma"] {
        assert_eq!(
            sentence(&engine_n_l("fz_exact_on", s.clone(), true, true), input),
            sentence(&engine_n_l("fz_exact_off", s.clone(), true, false), input),
            "{input}"
        );
    }
}

/// 模糊整句的造词码：整句 `code` 留所打码，`meta.learn_code` 是各节点规范码的拼接。
///
/// - 有「盖伦」用户词：S2 的用户词模糊节点，规范码 = 记录自身的码与边界；
/// - 无用户词：系统词模糊节点「概论」（wdat 带真值边界 gai|lun），规范码取词条自带边界。
///
/// 两种情况整句都是 you|gai|lun|ma（位 0/3/6/9），「有」「吗」是精确节点、取所打码那段。
#[test]
fn fuzzy_sentence_carries_canonical_learn_code() {
    let full = |e: &PinyinEngine| {
        e.convert("yougainunma", 100)
            .unwrap()
            .candidates
            .into_iter()
            .find(|c| c.is_sentence)
            .expect("应产出整句")
    };
    let want = Some(("yougailunma".to_string(), 0b10_0100_1001u64));

    let s = store("fz_learn_user");
    s.add_user_word("pinyin", "gailun", "盖伦", 1200, 0b1001)
        .unwrap();
    let c = full(&engine_n_l("fz_learn_user", s, true, true));
    assert_eq!(c.text, "有盖伦吗");
    assert_eq!(c.code, "yougainunma", "候选码仍是所打码");
    assert_eq!(c.meta.learn_code, want);

    let c = full(&engine_n_l(
        "fz_learn_sys",
        store("fz_learn_sys"),
        true,
        true,
    ));
    assert!(
        c.text.contains("概论"),
        "前提：无用户词时整句走系统词「概论」，实际 {}",
        c.text
    );
    assert_eq!(c.meta.learn_code, want);

    // 精确打法：造词码为 None（即候选码本身）。
    let c = full_exact(&engine_n_l(
        "fz_learn_exact",
        store("fz_learn_exact"),
        true,
        true,
    ));
    assert_eq!(c.meta.learn_code, None);
}

fn full_exact(e: &PinyinEngine) -> wind_candidate::Candidate {
    e.convert("yougailunma", 100)
        .unwrap()
        .candidates
        .into_iter()
        .find(|c| c.is_sentence)
        .expect("应产出整句")
}

/// 系统词模糊节点的词条边界与变体边界同域：有边界就必须对上某条切分展开出的变体边界，
/// 对上了造词码就带这份边界。
///
/// 词条「西昂」登记的真值是 xi|ang（位 0/2）。只开 ian=iang 时 `xian` 只能整音节展开成
/// `xiang`（位 0），切法对不上、不进图；开 an=ang 时 `xi|an → xi|ang` 对得上，造词码的边界
/// 跟着走；否则学进去的「西昂多练」少一个音节，简拼索引算出的声母串是 `xdl` 而不是 `xadl`。
#[test]
fn system_fuzzy_node_boundary_must_match_variant_segmentation() {
    let dir = std::env::temp_dir().join("wind_s2_canon_entry_boundary");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("duolian".into(), vec![("多练".into(), 5000, 0, 0b1001)]);
    w.add_with_boundary("xiang".into(), vec![("西昂".into(), 5000, 0, 0b101)]);
    w.write(&wdat).unwrap();
    let sentence = |fz: wind_engine::pinyin::fuzzy::FuzzyConfig| {
        let dict = CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具");
        PinyinEngine::new(PyConfig::default(), dict)
            .with_fuzzy(fz)
            .convert("xianduolian", 100)
            .unwrap()
            .candidates
            .into_iter()
            .find(|c| c.is_sentence)
    };

    let c = sentence(wind_engine::pinyin::fuzzy::FuzzyConfig {
        ian_iang: true,
        ..Default::default()
    });
    assert!(
        c.as_ref().is_none_or(|c| !c.text.contains("西昂")),
        "只开 ian=iang 时「西昂」的切法对不上，不该进整句：{:?}",
        c.map(|c| c.text)
    );

    let c = sentence(wind_engine::pinyin::fuzzy::FuzzyConfig {
        an_ang: true,
        ..Default::default()
    })
    .expect("应产出整句");
    assert_eq!(c.text, "西昂多练");
    // xi|ang|duo|lian → 位 0/2/5/8
    assert_eq!(
        c.meta.learn_code,
        Some(("xiangduolian".to_string(), 0b1_0010_0101))
    );
}

/// 残码整句（step 2c）同样带造词码：模糊节点取规范码，残码位保持所打的那几个字母
/// （`LatticeNode::canon` 对残码节点恒 `None`：补出的字读什么是预测，不是用户打出的码）。
#[test]
fn partial_final_fuzzy_sentence_learn_code() {
    let s = store("fz_learn_partial");
    s.add_user_word("pinyin", "gailun", "盖伦", 1200, 0b1001)
        .unwrap();
    let c = engine_n_l("fz_learn_partial", s, true, true)
        .convert("yougainunm", 100)
        .unwrap()
        .candidates
        .into_iter()
        .find(|c| c.is_sentence && c.code == "yougainunm")
        .expect("应产出残码整句");
    assert!(c.text.starts_with("有盖伦"), "实际 {}", c.text);
    // you|gai|lun|m → 位 0/3/6/9
    assert_eq!(
        c.meta.learn_code,
        Some(("yougailunm".to_string(), 0b10_0100_1001u64))
    );
}

/// 词条：(码, [(词, 权重)], 边界)。
type Entry<'a> = (&'a str, &'a [(&'a str, i32)], u64);

/// 只含给定词条与简拼索引（声母串, 全拼码）的 wdat 系统词库。
fn wdat_dict(tag: &str, entries: &[Entry], abbrevs: &[(&str, &str)]) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_s2_learn_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    for (code, words, boundary) in entries {
        w.add_with_boundary(
            (*code).into(),
            words
                .iter()
                .enumerate()
                .map(|(i, (t, wt))| ((*t).into(), *wt, i as u32, *boundary))
                .collect(),
        );
    }
    for (abbrev, code) in abbrevs {
        w.add_abbrev((*abbrev).into(), vec![((*code).into(), 1000)]);
    }
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn n_l() -> wind_engine::pinyin::fuzzy::FuzzyConfig {
    wind_engine::pinyin::fuzzy::FuzzyConfig {
        n_l: true,
        ..Default::default()
    }
}

/// 转换 `input`，取码为 `code` 的整句候选。
fn sentence_with_code(e: &PinyinEngine, input: &str, code: &str) -> wind_candidate::Candidate {
    e.convert(input, 100)
        .unwrap()
        .candidates
        .into_iter()
        .find(|c| c.is_sentence && c.code == code)
        .unwrap_or_else(|| panic!("{input} 应产出码为 {code} 的整句"))
}

/// step 2c 的延迟定夺档（1 个完整音节 + 残码，`short_sentence_pending`）同样带造词码。
///
/// `nunm`：`nun` 经 n=l 模糊命中「论」（规范码 lun），残码 `m` 补成「吗」。残码位保留所打字母。
#[test]
fn deferred_partial_sentence_learn_code() {
    let dict = wdat_dict(
        "deferred",
        &[
            ("lun", &[("论", 60_000)], 0b1),
            ("ma", &[("吗", 300_000)], 0b1),
        ],
        &[],
    );
    let e = PinyinEngine::new(PyConfig::default(), dict).with_fuzzy(n_l());
    let c = sentence_with_code(&e, "nunm", "nunm");
    assert_eq!(c.text, "论吗");
    // lun|m → 位 0/3
    assert_eq!(c.meta.learn_code, Some(("lunm".to_string(), 0b1001)));
}

/// step 2b 混合整句：全拼段的模糊节点取规范码，简拼段取回查出的完整全拼（简拼节点的
/// `canon` = 词条码 + 规范边界），整句造词码是完整全拼。
#[test]
fn mixed_abbrev_sentence_learn_code() {
    let dict = wdat_dict(
        "mixed",
        &[
            // bu|zhi|dao → 位 0/2/5
            ("buzhidao", &[("不知道", 500_000)], 0b100101),
            // gai|lun → 位 0/3
            ("gailun", &[("概论", 50_000)], 0b1001),
        ],
        &[("bzd", "buzhidao")],
    );
    let e = PinyinEngine::new(PyConfig::default(), dict).with_fuzzy(n_l());
    let c = sentence_with_code(&e, "bzdgainun", "bzdgainun");
    assert_eq!(c.text, "不知道概论");
    // bu|zhi|dao|gai|lun → 位 0/2/5/8/11
    assert_eq!(
        c.meta.learn_code,
        Some(("buzhidaogailun".to_string(), 0b1001_0010_0101))
    );
}

/// 出厂配置（模糊关）下混合整句同样带完整全拼造词码：只要有简拼段就带，
/// 不退回整句 `code`（击键串 `bzdgailun`）。
#[test]
fn mixed_abbrev_sentence_learn_code_without_fuzzy() {
    let dict = wdat_dict(
        "mixed_nofz",
        &[
            ("buzhidao", &[("不知道", 500_000)], 0b100101),
            ("gailun", &[("概论", 50_000)], 0b1001),
        ],
        &[("bzd", "buzhidao")],
    );
    let e = PinyinEngine::new(PyConfig::default(), dict);
    let c = sentence_with_code(&e, "bzdgailun", "bzdgailun");
    assert_eq!(c.text, "不知道概论");
    assert_eq!(
        c.meta.learn_code,
        Some(("buzhidaogailun".to_string(), 0b1001_0010_0101))
    );
    // 精确全拼整句（无简拼段）：仍为 None。
    let c = sentence_with_code(&e, "buzhidaogailun", "buzhidaogailun");
    assert_eq!(c.meta.learn_code, None);
}
