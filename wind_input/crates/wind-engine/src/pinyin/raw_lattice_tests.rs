use super::super::PinyinEngine;
use super::super::lattice::{LatticeBuilder, LatticeNode};
use super::super::shuangpin::{Layout, ShuangpinConverter};
use crate::engine::Engine;
use std::collections::HashSet;
use std::sync::Arc;
use wind_dict::DictManager;
use wind_dict::cached::CachedDict;
use wind_dict::codetable::CodetableDict;
use wind_store::Store;

/// 闸门裕度护栏：没挂模型走模型关那档；挂了按权重缩放，权重 0.5 时仍是标定值 10，权重调高
/// 裕度随之放大（转移分整体放大）；模型不报权重时按 0.5。
#[test]
fn gate_margin_guard() {
    use super::super::viterbi::ViterbiDecoder;
    assert_eq!(super::default_gate_margin(None), 5.0);
    assert_eq!(super::default_gate_margin(Some(Some(0.5))), 10.0);
    assert_eq!(super::default_gate_margin(Some(None)), 10.0);
    assert_eq!(super::default_gate_margin(Some(Some(0.0))), 5.0);
    assert!((super::default_gate_margin(Some(Some(0.8))) - 13.0).abs() < 1e-9);
    assert!((super::default_gate_margin(Some(Some(0.3))) - 8.0).abs() < 1e-9);
    // 引擎从解码器读：没挂模型 = None，挂了不报权重的 = Some(None)。
    assert_eq!(ViterbiDecoder::new().grammar_weight(), None);
    let null = ViterbiDecoder::with_grammar(Arc::new(super::super::grammar::NullGrammar));
    assert_eq!(null.grammar_weight(), Some(None));
}

fn xiaohe() -> ShuangpinConverter {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../data/schemas/shuangpin/xiaohe.toml");
    ShuangpinConverter::new(Layout::from_toml(&p).expect("小鹤布局"))
}

fn dict(words: &[(&str, &str, i32)]) -> CachedDict {
    let mut raw = CodetableDict::empty();
    for (i, (code, text, w)) in words.iter().enumerate() {
        raw.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    CachedDict::Memory(raw)
}

fn find<'a>(nodes: &'a [Vec<LatticeNode>], word: &str) -> Option<&'a LatticeNode> {
    nodes.iter().flatten().find(|n| n.word == word)
}

const WORDS: &[(&str, &str, i32)] = &[
    ("ni", "你", 9000),
    ("hao", "好", 9000),
    ("nihao", "你好", 50000),
    ("ma", "吗", 8000),
];

/// 节点跨度是击键偏移，音节掩码与规范码在全拼域。
#[test]
fn raw_nodes_cover_keystroke_span() {
    let conv = xiaohe();
    let keys = "nihc";
    let edges = conv.pair_edges(keys);
    let b = LatticeBuilder::new();
    let nodes = b.build_raw(keys, &edges, &dict(WORDS), None, true);
    assert_eq!(nodes.len(), keys.len() + 1);
    let n = find(&nodes, "你好").expect("你好");
    assert_eq!((n.start, n.end), (0, 4));
    assert_eq!(n.syl_mask, 0b101, "ni|hao 全拼域");
    assert_eq!(n.syllables, vec!["ni", "hao"]);
    assert_eq!(n.canon, Some(("nihao".to_string(), 0b101)));
    let h = find(&nodes, "好").unwrap();
    assert_eq!((h.start, h.end), (2, 4));
}

/// `'` 不断词，归到前一节点的末尾，图上不留空洞。
#[test]
fn raw_nodes_absorb_separator() {
    let conv = xiaohe();
    let keys = "ni'hc";
    let edges = conv.pair_edges(keys);
    let nodes = LatticeBuilder::new().build_raw(keys, &edges, &dict(WORDS), None, true);
    let n = find(&nodes, "你好").unwrap();
    assert_eq!((n.start, n.end), (0, 5));
    let ni = find(&nodes, "你").unwrap();
    assert_eq!((ni.start, ni.end), (0, 3));
    let h = find(&nodes, "好").unwrap();
    assert_eq!((h.start, h.end), (3, 5));
}

/// 用户词按全拼码查进图；屏蔽词由调用方的 `drop_blocked_nodes` 统一滤掉。
#[test]
fn raw_nodes_include_user_and_blocked_filtered() {
    // 文件名带进程号，并发跑测试互不踩；用完删掉。
    let p = std::env::temp_dir().join(format!(
        "wind_pinyin_raw_lattice_user_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    let store = Arc::new(Store::open(&p).unwrap());
    store
        .add_user_word("pinyin", "gailun", "盖伦", 500, 0b1001)
        .unwrap();
    // 无边界的用户词不进整句图（同 add_store_nodes 约束 2）。
    store
        .add_user_word("pinyin", "gailun", "该轮", 500, 0)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));

    let conv = xiaohe();
    // 小鹤：gd = gai，ly = lun
    let keys = "gdly";
    let edges = conv.pair_edges(keys);
    let words = &[
        ("gai", "该", 9000),
        ("lun", "论", 9000),
        ("gailun", "概论", 1217),
    ];
    let b = LatticeBuilder::new();
    let mut nodes = b.build_raw(keys, &edges, &dict(words), None, true);
    b.add_store_nodes_raw(keys, &edges, &dm, None, true, &mut nodes);
    let u = find(&nodes, "盖伦").expect("用户词进图");
    assert_eq!((u.start, u.end, u.syl_mask), (0, 4, 0b1001));
    assert!(find(&nodes, "该轮").is_none(), "无边界用户词不进图");

    assert!(find(&nodes, "概论").is_some(), "前提：系统词在图里");
    let eng = PinyinEngine::new(Default::default(), dict(&[]));
    eng.set_blocked_words(Arc::new(HashSet::from(["概论".to_string()])));
    eng.drop_blocked_nodes(&mut nodes);
    assert!(find(&nodes, "概论").is_none(), "屏蔽词不进图");
    assert!(find(&nodes, "盖伦").is_some());
    drop(dm);
    drop(store);
    let _ = std::fs::remove_file(&p);
}

/// 尾部落单键补出残码节点，跨度是那一个击键。
#[test]
fn raw_nodes_tail_key_partial() {
    let conv = xiaohe();
    let keys = "nihcm";
    let r = conv.convert(keys);
    let edges = conv.pair_edges(keys);
    let b = LatticeBuilder::new();
    let mut nodes = b.build_raw(keys, &edges, &dict(WORDS), None, true);
    let partial = r.partial_initial.expect("尾键残码");
    b.add_partial_final_raw(keys, 4, &partial, &dict(WORDS), &mut nodes);
    let m = find(&nodes, "吗").expect("残码节点");
    assert_eq!((m.start, m.end, m.syl_mask), (4, 5, 1));
    assert_eq!(m.canon, Some(("m".to_string(), 1)));
    // 小鹤 `u` 作声母是 sh：残码按全拼声母补。
    let keys = "nihcu";
    let mut nodes = b.build_raw(keys, &conv.pair_edges(keys), &dict(WORDS), None, true);
    let words = &[("shi", "是", 9000)];
    b.add_partial_final_raw(keys, 4, "sh", &dict(words), &mut nodes);
    assert!(find(&nodes, "是").is_some());
}

/// 整句终点须恰为末槽终点；末槽含紧随的 `'`。
#[test]
fn sentence_end_requires_last_slot() {
    let conv = xiaohe();
    assert_eq!(
        super::last_slot_end("nihcmd", &conv.pair_edges("nihcmd")),
        Some(6)
    );
    assert_eq!(
        super::last_slot_end("nihc'", &conv.pair_edges("nihc'")),
        Some(5)
    );
    assert_eq!(super::last_slot_end("", &[]), None);
    assert_eq!(super::sentence_end(6, Some(6)), Some(6));
    assert_eq!(super::sentence_end(4, Some(6)), None);
    assert_eq!(super::sentence_end(4, None), None);
}

/// 模糊音读音按处数扣分，所选读音写进规范码。
#[test]
fn raw_nodes_fuzzy_reading_penalized() {
    let mut conv = xiaohe();
    let fz = super::super::fuzzy::FuzzyConfig {
        zh_z: true,
        ..Default::default()
    };
    conv.set_fuzzy(&fz);
    // 小鹤 zs = zong；开 z=zh 时边上另有 zhong
    let keys = "zsgo";
    let edges = conv.pair_edges(keys);
    let words = &[("zhongguo", "中国", 90000), ("zongguo", "宗国", 90000)];
    let nodes = LatticeBuilder::new().build_raw(keys, &edges, &dict(words), Some(&fz), true);
    let exact = find(&nodes, "宗国").unwrap();
    let fuzzy = find(&nodes, "中国").unwrap();
    assert_eq!((fuzzy.start, fuzzy.end), (0, 4));
    assert_eq!(fuzzy.canon.as_ref().unwrap().0, "zhongguo");
    assert!(
        (exact.log_prob - fuzzy.log_prob - std::f64::consts::LN_2).abs() < 1e-9,
        "同权重时模糊那条恰多扣一处"
    );
}

// ===== 辅码边 =====

struct FakeAux(Vec<(char, &'static str)>);

impl crate::engine::AuxCharLookup for FakeAux {
    fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
        self.0
            .iter()
            .any(|(c, code)| *c == ch && code.starts_with(prefix))
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// 辅码模式建图：全偏移键对 + 辅码表。
fn build_aux(
    keys: &str,
    words: &[(&str, &str, i32)],
    table: &FakeAux,
    fuzzy: Option<&super::super::fuzzy::FuzzyConfig>,
) -> (Vec<Vec<LatticeNode>>, super::AuxSpans) {
    let mut conv = xiaohe();
    if let Some(f) = fuzzy {
        conv.set_fuzzy(f);
    }
    let ctx = super::AuxCtx::new(table);
    LatticeBuilder::new().build_raw_aux(
        keys,
        &conv.pair_edges_all(keys),
        &dict(words),
        fuzzy,
        true,
        Some(&ctx),
    )
}

fn at<'a>(
    nodes: &'a [Vec<LatticeNode>],
    word: &str,
    span: (usize, usize),
) -> Option<&'a LatticeNode> {
    nodes
        .iter()
        .flatten()
        .find(|n| n.word == word && (n.start, n.end) == span)
}

fn aux_of(spans: &super::AuxSpans, word: &str, span: (usize, usize)) -> Vec<(usize, String)> {
    spans.get(span.0, span.1, word).unwrap_or_default().to_vec()
}

const SHI: &[(&str, &str, i32)] = &[("shi", "是", 9000), ("ni", "你", 9000), ("hao", "好", 9000)];

/// 小鹤 `ui` = shi，`uu` 作「是」的辅码：「是」跨 `[0,4)`，规范码不含辅码。
#[test]
fn aux_edge_builds_char_with_aux() {
    let (nodes, spans) = build_aux("uiuu", SHI, &FakeAux(vec![('是', "uu")]), None);
    let n = at(&nodes, "是", (0, 4)).expect("带辅码的「是」");
    assert_eq!(n.canon, Some(("shi".to_string(), 1)));
    assert_eq!(n.syllables, vec!["shi"]);
    assert_eq!(n.syl_mask, 1);
    assert_eq!(aux_of(&spans, "是", (0, 4)), vec![(0, "uu".to_string())]);
    // 1 位前缀同样合法。
    assert!(at(&nodes, "是", (0, 3)).is_some());
    assert_eq!(aux_of(&spans, "是", (0, 3)), vec![(0, "u".to_string())]);
    assert!(at(&nodes, "是", (0, 2)).is_some(), "无辅码读法仍在");
    assert!(spans.get(0, 2, "是").is_none());

    // 不给辅码表：只有无辅码读法。
    let conv = xiaohe();
    let plain =
        LatticeBuilder::new().build_raw("uiuu", &conv.pair_edges("uiuu"), &dict(SHI), None, true);
    let spans: Vec<_> = plain
        .iter()
        .flatten()
        .filter(|n| n.word == "是")
        .map(|n| (n.start, n.end))
        .collect();
    assert_eq!(spans, vec![(0, 2)]);
}

/// 辅码字母不在该字任一码的开头上 → 不建该节点。
#[test]
fn aux_edge_requires_table_hit() {
    let table = FakeAux(vec![('是', "or")]);
    let (nodes, spans) = build_aux("uiuu", SHI, &table, None);
    assert!(at(&nodes, "是", (0, 3)).is_none());
    assert!(at(&nodes, "是", (0, 4)).is_none());
    let (nodes, spans2) = build_aux("uio", SHI, &table, None);
    assert!(at(&nodes, "是", (0, 3)).is_some(), "前缀 o 命中 or");
    assert_eq!(aux_of(&spans2, "是", (0, 3)), vec![(0, "o".to_string())]);
    // 多字词要求字数 = 音节数，且每处辅码都核对它所跟的那个字。
    let words = &[("nihao", "你好", 50000)];
    let table = FakeAux(vec![('你', "rx"), ('好', "nz")]);
    let (nodes, spans3) = build_aux("nirhc", words, &table, None);
    assert!(at(&nodes, "你好", (0, 5)).is_some(), "词中辅码");
    assert_eq!(aux_of(&spans3, "你好", (0, 5)), vec![(0, "r".to_string())]);
    let (nodes, _) = build_aux("ninhc", words, &table, None);
    assert!(at(&nodes, "你好", (0, 5)).is_none(), "n 不是「你」的辅码");
    assert!(spans.0.is_empty());
}

/// 辅码不跨 `'`：辅码须紧跟音节两键、同在一个 `'` 段内；辅码后的 `'` 并入辅码。
#[test]
fn aux_not_across_separator() {
    let words = &[("ni", "你", 9000), ("hao", "好", 9000), ("ao", "奥", 9000)];
    let table = FakeAux(vec![('你', "hc"), ('好', "ao")]);
    // `ni'h'ao`：h 与 ni 之间隔着 `'`，不当辅码；`ao` 前也有 `'`。
    let (nodes, spans) = build_aux("ni'h'ao", words, &table, None);
    assert!(spans.0.is_empty(), "不应有辅码节点");
    assert!(at(&nodes, "你", (0, 3)).is_some());
    assert!(nodes.iter().flatten().all(|n| n.word != "你" || n.end == 3));
    // `nih'ao`：h 紧跟 ni，是辅码，其后的 `'` 并入，「奥」接在 4 上。
    let (nodes, spans) = build_aux("nih'ao", words, &table, None);
    assert_eq!(aux_of(&spans, "你", (0, 4)), vec![(0, "h".to_string())]);
    assert!(at(&nodes, "奥", (4, 6)).is_some(), "辅码后的 ' 不留空洞");
    // 两位辅码不能夹 `'`。
    let (_, spans) = build_aux("nih'c", words, &table, None);
    assert!(spans.get(0, 5, "你").is_none());
    // 音节后紧跟 `'` 的不接辅码：`hc'ao` 的「好」不吃 ao。
    let (_, spans) = build_aux("hc'ao", words, &table, None);
    assert!(spans.0.is_empty());
}

/// 用户词、模糊读法与辅码并存时不丢节点。
#[test]
fn aux_with_user_word_and_fuzzy() {
    let p = std::env::temp_dir().join(format!(
        "wind_pinyin_raw_lattice_aux_user_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    let store = Arc::new(Store::open(&p).unwrap());
    store
        .add_user_word("pinyin", "gailun", "盖伦", 500, 0b1001)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    let words = &[
        ("gai", "该", 9000),
        ("lun", "论", 9000),
        ("gailun", "概论", 1217),
    ];
    let table = FakeAux(vec![('伦', "ab"), ('论', "yz")]);
    let ctx = super::AuxCtx::new(&table);
    let conv = xiaohe();
    let keys = "gdlya";
    let edges = conv.pair_edges_all(keys);
    let b = LatticeBuilder::new();
    let (mut nodes, mut spans) =
        b.build_raw_aux(keys, &edges, &dict(words), None, true, Some(&ctx));
    b.add_store_nodes_raw_aux(
        keys,
        &edges,
        &dm,
        None,
        true,
        Some(&ctx),
        &mut nodes,
        &mut spans,
    );
    assert!(at(&nodes, "盖伦", (0, 5)).is_some(), "用户词带辅码");
    assert_eq!(aux_of(&spans, "盖伦", (0, 5)), vec![(1, "a".to_string())]);
    assert!(at(&nodes, "盖伦", (0, 4)).is_some(), "无辅码用户词仍在");
    assert!(at(&nodes, "概论", (0, 4)).is_some(), "系统词仍在");
    assert!(at(&nodes, "概论", (0, 5)).is_none(), "a 不是「论」的辅码");
    drop(dm);
    drop(store);
    let _ = std::fs::remove_file(&p);

    // 模糊：小鹤 zs = zong，开 z=zh 时另有 zhong；辅码打在词中字后。
    let fz = super::super::fuzzy::FuzzyConfig {
        zh_z: true,
        ..Default::default()
    };
    let words = &[("zhongguo", "中国", 90000), ("zongguo", "宗国", 90000)];
    let table = FakeAux(vec![('中', "xk"), ('国', "gw")]);
    let (nodes, spans) = build_aux("zsxgo", words, &table, Some(&fz));
    let n = at(&nodes, "中国", (0, 5)).expect("模糊 + 词中辅码");
    assert_eq!(n.canon.as_ref().unwrap().0, "zhongguo");
    assert_eq!(aux_of(&spans, "中国", (0, 5)), vec![(0, "x".to_string())]);
    assert!(at(&nodes, "宗国", (0, 5)).is_none(), "「宗」无辅码 x");
    let (nodes, _) = build_aux("zsgo", words, &table, Some(&fz));
    assert!(at(&nodes, "中国", (0, 4)).is_some() && at(&nodes, "宗国", (0, 4)).is_some());
}

/// 同词同跨度去重：辅码数不同时取分高者，平分留辅码少的；辅码数相同时同旧路径。
#[test]
fn aux_dedupe_prefers_plain_on_tie() {
    use super::should_replace;
    assert!(
        !should_replace((false, 0, -1.0), (false, 1, -1.0)),
        "平分留无辅码"
    );
    assert!(should_replace((false, 1, -1.0), (false, 0, -1.0)));
    assert!(
        should_replace((false, 0, -2.0), (false, 1, -1.0)),
        "辅码只在严格高分时胜"
    );
    assert!(!should_replace((false, 0, -1.0), (true, 1, -2.0)));
    assert!(
        should_replace((true, 0, -1.0), (false, 0, -5.0)),
        "同辅码数：精确恒胜"
    );
    assert!(!should_replace((false, 0, -5.0), (true, 0, -1.0)));
}

/// 选读法按罚分比：3 处模糊（≈2.08）轻于 2 位辅码（4.0），不按「处数 + 字母数」计数比。
#[test]
fn pick_shape_orders_by_penalty() {
    let shape = |edits, aux_len| super::Shape {
        mask: 0,
        syllables: Default::default(),
        edits,
        exact: false,
        aux: Default::default(),
        aux_len,
    };
    let shapes = [shape(0, 2), shape(3, 0)];
    let picked = super::pick_shape(&shapes, |_| true, "", None).unwrap();
    assert_eq!((picked.edits, picked.aux_len), (3, 0));
    // 同罚分时取辅码少的。
    let old = super::set_aux_letter_penalty(super::FUZZY_SYLLABLE_LOG_PENALTY);
    let shapes = [shape(0, 1), shape(1, 0)];
    let picked = super::pick_shape(&shapes, |_| true, "", None).unwrap();
    super::set_aux_letter_penalty(old);
    assert_eq!((picked.edits, picked.aux_len), (1, 0));
}

/// 辅码模式底图：引擎 + 全偏移键对 + 表。
fn aux_base(keys: &str, eng: &PinyinEngine, table: &FakeAux) -> super::RawBase {
    let conv = xiaohe();
    let edges = conv.pair_edges(keys);
    let all = conv.pair_edges_all(keys);
    let partial = conv.convert(keys).partial_initial;
    let aux = super::AuxInput {
        edges_all: &all,
        lookup: table,
        tail_partial: partial.as_deref(),
    };
    eng.raw_base_lattice(keys, &edges, Some(aux)).expect("底图")
}

const NIHAO_H: &[(&str, &str, i32)] = &[
    ("ni", "你", 9000),
    ("hao", "好", 9000),
    ("nihao", "你好", 50000),
    ("he", "和", 8000),
];

/// 辅码模式 step 2 终点同 P1 取末槽终点；整串另经辅码可达时由 `tail_end` 给出整串。
#[test]
fn aux_sentence_end_is_last_slot_with_tail() {
    let eng = PinyinEngine::new(Default::default(), dict(NIHAO_H));
    let table = FakeAux(vec![('好', "hn")]);
    let ends = |keys: &str| {
        let b = aux_base(keys, &eng, &table);
        (b.sentence().map(|(_, e)| e), b.tail_end())
    };
    // m 不是「好」的辅码，5 不可达：只有末槽 `hc` 的终点 4。
    assert_eq!(ends("nihcm"), (Some(4), None));
    // 偶数长：`pl` 不成音节，6 不可达。
    assert_eq!(ends("nihcpl"), (Some(4), None));
    // 整串经辅码可达：step 2 仍取末槽，整串另给（奇 / 偶）。
    assert_eq!(ends("nihch"), (Some(4), Some(5)));
    // `hn`（hiao）不成音节，末槽仍是 `hc`。
    assert_eq!(ends("nihchn"), (Some(4), Some(6)));
}

/// 奇数长 2c：整串另可达时那条整句归 step 2，2c 的图只剩无辅码节点与残码节点（同 P1）。
#[test]
fn aux_partial_lattice_is_plain_beside_tail() {
    let eng = PinyinEngine::new(Default::default(), dict(NIHAO_H));
    let table = FakeAux(vec![('好', "hn")]);
    let keys = "nihch";
    let partial = xiaohe().convert(keys).partial_initial.expect("尾键残码");
    assert_eq!(partial, "h");
    let base = aux_base(keys, &eng, &table);
    let (lattice, end, _) = eng
        .raw_partial_lattice(keys, base, &partial)
        .expect("2c 底图");
    assert_eq!(end, 5);
    let at_end: Vec<(usize, &str, u8)> = lattice[5]
        .iter()
        .map(|n| (n.start, n.word.as_str(), n.aux))
        .collect();
    assert!(
        !at_end.is_empty() && at_end.iter().all(|&(st, _, aux)| st == 4 && aux == 0),
        "只剩残码节点：{at_end:?}"
    );
    assert!(lattice.iter().flatten().all(|n| n.aux == 0));
}

/// 同起点同终点同词只留一个节点，辅码分布随它登记：`nihhhx` 的「你行」可读作「你(h)行 + x」
/// 或「你 行(hx)」，图里只有一条，`trailing_aux_len` 与它登记的分布一致。
#[test]
fn trailing_aux_len_follows_kept_node() {
    let words: &[(&str, &str, i32)] = &[
        ("ni", "你", 9000),
        ("hang", "行", 9000),
        ("nihang", "你行", 50000),
    ];
    let eng = PinyinEngine::new(Default::default(), dict(words));
    let table = FakeAux(vec![('你', "h"), ('行', "hx"), ('行', "x")]);
    let keys = "nihhhx";
    let base = aux_base(keys, &eng, &table);
    let at_end: Vec<_> = base.lattice[6]
        .iter()
        .filter(|n| n.start == 0 && n.word == "你行")
        .collect();
    assert_eq!(at_end.len(), 1, "{at_end:?}");
    assert_eq!(at_end[0].aux, 2);
    let conv = xiaohe();
    let ctx = super::AuxCtx::new(&table);
    let (_, spans) = eng.lattice_builder.build_raw_aux(
        keys,
        &conv.pair_edges_all(keys),
        &eng.dict,
        None,
        true,
        Some(&ctx),
    );
    let dist = spans.get(0, 6, "你行").expect("登记了辅码").to_vec();
    let last = dist
        .iter()
        .find(|(i, _)| *i == 1)
        .map_or(0, |(_, p)| p.len());
    assert!(
        dist == vec![(0, "h".to_string()), (1, "x".to_string())]
            || dist == vec![(1, "hx".to_string())],
        "{dist:?}"
    );
    assert_eq!(base.trailing_aux_len(0, "你行") as usize, last, "{dist:?}");
}

const QIHOU: &[(&str, &str, i32)] = &[
    ("qi", "启", 9000),
    ("qi", "气", 9000),
    ("qihou", "气候", 50000),
    ("he", "和", 8000),
    ("wo", "我", 9000),
];

/// 末键落单：多字残码补全节点与「音节 + 1 位辅码」节点同在 `lattice[n]`。
#[test]
fn tail_completion_alongside_aux_nodes() {
    let eng = PinyinEngine::new(Default::default(), dict(QIHOU)).with_shuangpin(xiaohe());
    let table = FakeAux(vec![('启', "h")]);
    let base = aux_base("qih", &eng, &table);
    let at_end: Vec<(usize, &str, u8)> = base.lattice[3]
        .iter()
        .map(|n| (n.start, n.word.as_str(), n.aux))
        .collect();
    assert!(at_end.contains(&(0, "启", 1)), "音节 + 辅码：{at_end:?}");
    assert!(at_end.contains(&(0, "气候", 0)), "补全节点：{at_end:?}");
    let n = base.lattice[3].iter().find(|n| n.word == "气候").unwrap();
    assert_eq!(n.canon, Some(("qihou".to_string(), 0b101)));
    // 补全不改终点判据：step 2 取末槽 2，整串仍由辅码读法可达。
    assert_eq!(base.sentence().map(|(_, e)| e), Some(2));
    assert_eq!(base.tail_end(), Some(3));
    // 起点逐个往前退：`woqih` 从 2 起补出「气候」，从 0 起要 3 字词（没有）。
    let base = aux_base("woqih", &eng, &table);
    let starts: Vec<(usize, &str)> = base.lattice[5]
        .iter()
        .filter(|n| n.aux == 0 && n.end - n.start > 1)
        .map(|n| (n.start, n.word.as_str()))
        .collect();
    assert_eq!(starts, vec![(2, "气候")]);
}

/// 只在「经辅码读法到得了整串、且按键对真值切分末键落单」时补：到不了整串时不补；
/// 辅码把奇偶错开才落单的末键（真值切分是偶数长）不补。
#[test]
fn tail_completion_only_beside_trailing_aux() {
    let eng = PinyinEngine::new(Default::default(), dict(QIHOU)).with_shuangpin(xiaohe());
    let multi = |base: &super::RawBase, n: usize| -> Vec<String> {
        base.lattice[n]
            .iter()
            .filter(|x| x.aux == 0 && x.word.chars().count() > 1)
            .map(|x| x.word.clone())
            .collect()
    };
    let base = aux_base("woqih", &eng, &FakeAux(vec![('我', "x")]));
    assert!(multi(&base, 5).is_empty(), "整串不可达");
    let table = FakeAux(vec![('我', "h"), ('启', "h")]);
    let base = aux_base("wohqih", &eng, &table);
    assert!(
        base.lattice[6].iter().any(|x| x.word == "启" && x.aux == 1),
        "前提：辅码读法到整串"
    );
    assert!(multi(&base, 6).is_empty(), "{:?}", multi(&base, 6));
}

/// 补全节点只在辅码模式下进图：不给表时底图与 P1 相同。
#[test]
fn tail_completion_absent_without_aux() {
    let eng = PinyinEngine::new(Default::default(), dict(QIHOU)).with_shuangpin(xiaohe());
    let conv = xiaohe();
    let base = eng
        .raw_base_lattice("woqih", &conv.pair_edges("woqih"), None)
        .expect("底图");
    assert!(base.lattice[5].is_empty(), "{:?}", base.lattice[5]);
}

/// 2c 补残码节点时与 `lattice[n]` 上已有的同起点同词节点去重：新节点分高才替换。
#[test]
fn partial_lattice_dedupes_residual_by_score() {
    let eng = PinyinEngine::new(Default::default(), dict(WORDS));
    let node = |log_prob| crate::pinyin::viterbi::WordNode {
        start: 4,
        end: 5,
        word: "吗".to_string(),
        syl_mask: 1,
        log_prob,
        canon: Some(("m".to_string(), 1)),
        aux: 0,
    };
    let base = |lp| {
        let mut lattice = vec![Vec::new(); 6];
        lattice[5].push(node(lp));
        super::RawBase {
            lattice,
            reach: 4,
            // 没有末槽 ⇒ 无 `tail_end`，`lattice[n]` 不清空，走去重这一支。
            last_slot_end: None,
            node_reach: Some(vec![true; 6]),
            tail_aux: Vec::new(),
            partial_hits: None,
            aux_spans: Default::default(),
        }
    };
    let ma = |lattice: &[Vec<crate::pinyin::viterbi::WordNode>]| -> Vec<f64> {
        lattice[5]
            .iter()
            .filter(|n| n.word == "吗" && n.start == 4)
            .map(|n| n.log_prob)
            .collect()
    };
    let (low, ..) = eng.raw_partial_lattice("nihcm", base(-1e9), "m").unwrap();
    let fresh = ma(&low);
    assert_eq!(fresh.len(), 1, "同起点同词只留一条");
    assert!(fresh[0] > -1e9, "分低的旧节点被替换：{fresh:?}");
    let (high, ..) = eng.raw_partial_lattice("nihcm", base(1e9), "m").unwrap();
    assert_eq!(ma(&high), vec![1e9], "分高的旧节点保留");
}

/// 一个词带多处辅码：每处核对它所跟的那个字。
#[test]
fn aux_multiple_spans_in_one_word() {
    let words = &[("nihaoma", "你好吗", 50000)];
    let table = FakeAux(vec![('你', "rx"), ('吗', "xy")]);
    let (nodes, spans) = build_aux("nirhcmax", words, &table, None);
    assert!(at(&nodes, "你好吗", (0, 8)).is_some());
    assert_eq!(
        aux_of(&spans, "你好吗", (0, 8)),
        vec![(0, "r".to_string()), (2, "x".to_string())]
    );
    let (nodes, spans) = build_aux("nirxhcmaxy", words, &table, None);
    assert_eq!(
        aux_of(&spans, "你好吗", (0, 10)),
        vec![(0, "rx".to_string()), (2, "xy".to_string())]
    );
    assert!(at(&nodes, "你好吗", (0, 10)).is_some());
    // 第二处不合法（z 不是「吗」的辅码）→ 整个节点不成立。
    let (nodes, _) = build_aux("nirhcmaz", words, &table, None);
    assert!(at(&nodes, "你好吗", (0, 8)).is_none());
    // 辅码打在「好」后：表里没有「好」→ 不成立。
    let (nodes, _) = build_aux("nihcrma", words, &table, None);
    assert!(at(&nodes, "你好吗", (0, 7)).is_none());
}

/// 批量判定（首字母掩码）与逐字判定产出的节点与辅码位置逐项相同：P2 的小词库夹具 + 多处辅码。
#[test]
fn aux_admits_batch_equals_per_char() {
    let fz = super::super::fuzzy::FuzzyConfig {
        zh_z: true,
        n_l: true,
        ..Default::default()
    };
    let multi: &[(&str, &str, i32)] = &[
        ("nihaoma", "你好吗", 50000),
        ("ni", "你", 9000),
        ("ni", "泥", 3000),
        ("hao", "好", 9000),
        ("ma", "吗", 8000),
        ("ma", "妈", 7000),
    ];
    let table = FakeAux(vec![
        ('是', "uu"),
        ('是', "ri"),
        ('你', "rx"),
        ('泥', "sh"),
        ('吗', "xy"),
        ('妈', "nm"),
        ('好', "nz"),
        ('好', "zn"),
        ('气', "qr"),
        ('启', "hu"),
        ('候', "rh"),
        ('和', "hk"),
    ]);
    type Case = (&'static str, &'static [(&'static str, &'static str, i32)]);
    let cases: &[Case] = &[
        ("uiuu", SHI),
        ("uiu", SHI),
        ("uio", SHI),
        ("nihcz", NIHAO_H),
        ("nizhcn", NIHAO_H),
        ("nirhcmax", multi),
        ("nirxhcmaxy", multi),
        ("nirhcmaz", multi),
        ("nishcnmamn", multi),
        ("uir", SHI),
        ("qiqhz", QIHOU),
        ("qihzr", QIHOU),
        ("qiqrhzrh", QIHOU),
        ("qihhz", QIHOU),
    ];
    for &(keys, words) in cases {
        for fuzzy in [None, Some(&fz)] {
            let mut conv = xiaohe();
            if let Some(f) = fuzzy {
                conv.set_fuzzy(f);
            }
            let edges = conv.pair_edges_all(keys);
            let d = dict(words);
            let run = |per_char: bool| {
                let ctx = super::AuxCtx::new(&table).with_per_char(per_char);
                LatticeBuilder::new().build_raw_aux(keys, &edges, &d, fuzzy, true, Some(&ctx))
            };
            let (want, want_spans) = run(true);
            let (got, got_spans) = run(false);
            assert_eq!(format!("{got:?}"), format!("{want:?}"), "{keys} {fuzzy:?}");
            for n in want.iter().flatten() {
                assert_eq!(
                    aux_of(&got_spans, &n.word, (n.start, n.end)),
                    aux_of(&want_spans, &n.word, (n.start, n.end)),
                    "{keys} {}",
                    n.word
                );
            }
        }
    }
}

/// 无辅码表时节点与 P1 单链逐项相同（`p1_reference` 是改造前的实现）。
#[test]
fn aux_none_equals_p1() {
    let fz = super::super::fuzzy::FuzzyConfig {
        zh_z: true,
        n_l: true,
        ..Default::default()
    };
    let words: &[(&str, &str, i32)] = &[
        ("ni", "你", 9000),
        ("li", "里", 8000),
        ("hao", "好", 9000),
        ("nihao", "你好", 50000),
        ("lihao", "里好", 100),
        ("ma", "吗", 8000),
        ("shi", "是", 9000),
        ("shu", "书", 9000),
        ("zhongguo", "中国", 90000),
        ("zongguo", "宗国", 90000),
        ("guo", "国", 9000),
        ("zhong", "中", 9000),
    ];
    let d = dict(words);
    let b = LatticeBuilder::new();
    for fuzzy in [None, Some(&fz)] {
        let mut conv = xiaohe();
        if let Some(f) = fuzzy {
            conv.set_fuzzy(f);
        }
        for keys in [
            "nihc", "ni'hc", "nihcm", "nihcma", "uiuu", "zsgo", "nihc'", "'nihc", "nipl", "plnihc",
            "nihcuiuu",
        ] {
            let edges = conv.pair_edges(keys);
            let (new, spans) = b.build_raw_aux(keys, &edges, &d, fuzzy, true, None);
            let old = p1_reference::build_raw(&b, keys, &edges, &d, fuzzy, true);
            assert!(spans.0.is_empty());
            assert_eq!(
                fmt_nodes(&new),
                fmt_nodes(&old),
                "{keys} fuzzy={}",
                fuzzy.is_some()
            );
            assert!(
                !fmt_nodes(&new).is_empty() || keys == "nipl" || keys == "plnihc",
                "{keys}"
            );
        }
    }
}

fn fmt_nodes(nodes: &[Vec<LatticeNode>]) -> Vec<String> {
    nodes
        .iter()
        .enumerate()
        .flat_map(|(q, v)| {
            v.iter().map(move |n| {
                format!(
                    "{q}:{}-{} {} {:?} {:b} {} {:?}",
                    n.start,
                    n.end,
                    n.word,
                    n.syllables,
                    n.syl_mask,
                    n.log_prob.to_bits(),
                    n.canon
                )
            })
        })
        .collect()
}

/// 改造前（单条槽链）的 `build_raw`，只作 `aux_none_equals_p1` 的对照。
mod p1_reference {
    use super::super::super::fuzzy::{FuzzyConfig, FuzzyMatcher};
    use super::super::super::lattice::{
        FUZZY_LAYER_BUDGET, FUZZY_SYLLABLE_LOG_PENALTY, LatticeBuilder, LatticeNode, score_node,
    };
    use super::super::super::shuangpin::RawEdge;
    use super::super::{is_slot_edge, slot_span};
    use std::collections::{BTreeMap, HashMap};
    use wind_dict::cached::CachedDict;

    struct Slot {
        start: usize,
        end: usize,
        options: Vec<(String, usize)>,
    }
    struct Shape {
        mask: u64,
        syllables: Vec<String>,
        edits: usize,
        exact: bool,
    }
    type Spans = BTreeMap<(usize, String), Vec<Shape>>;
    struct RawChain {
        slots: Vec<Slot>,
        linked: Vec<bool>,
        reachable: Vec<bool>,
    }

    impl RawChain {
        fn new(keys: &str, edges: &[RawEdge], fuzzy: Option<&FuzzyConfig>) -> Self {
            let b = keys.as_bytes();
            let fuzzy = fuzzy.filter(|f| f.any_enabled());
            let slots: Vec<Slot> = edges
                .iter()
                .filter(|e| is_slot_edge(b, e))
                .map(|e| {
                    let (start, end) = slot_span(b, e);
                    let first = &e.syllables[0];
                    let mut options = vec![(first.clone(), 0)];
                    if let Some(f) = fuzzy {
                        options.extend(FuzzyMatcher::fuzzy_variants_scored(first, f));
                    }
                    Slot {
                        start,
                        end,
                        options,
                    }
                })
                .collect();
            let linked: Vec<bool> = slots.windows(2).map(|w| w[0].end == w[1].start).collect();
            let mut reachable = Vec::with_capacity(slots.len());
            for i in 0..slots.len() {
                reachable.push(if i == 0 {
                    slots[0].start == 0
                } else {
                    reachable[i - 1] && linked[i - 1]
                });
            }
            Self {
                slots,
                linked,
                reachable,
            }
        }

        fn spans_from(&self, i: usize, max: usize, can_extend: &dyn Fn(&str) -> bool) -> Spans {
            type Combo = (String, u64, Vec<String>, usize, bool);
            let mut spans = Spans::new();
            let mut layer: Vec<Combo> = vec![(String::new(), 0, Vec::new(), 0, true)];
            for k in 0..max {
                let j = i + k;
                if j >= self.slots.len() || (k > 0 && !self.linked[j - 1]) {
                    break;
                }
                let mut next: Vec<Combo> = Vec::new();
                for (code, mask, syls, edits, exact) in &layer {
                    if !code.is_empty() && !can_extend(code) {
                        continue;
                    }
                    let bit = if code.len() < 64 {
                        1u64 << code.len()
                    } else {
                        0
                    };
                    for (idx, (opt, opt_edits)) in self.slots[j].options.iter().enumerate() {
                        let mut s = syls.clone();
                        s.push(opt.clone());
                        next.push((
                            format!("{code}{opt}"),
                            mask | bit,
                            s,
                            edits + opt_edits,
                            *exact && idx == 0,
                        ));
                    }
                }
                if next.is_empty() {
                    break;
                }
                if next.len() > FUZZY_LAYER_BUDGET {
                    next.sort_by_key(|c| c.3);
                    next.truncate(FUZZY_LAYER_BUDGET);
                }
                for (code, mask, syls, edits, exact) in &next {
                    spans.entry((j, code.clone())).or_default().push(Shape {
                        mask: *mask,
                        syllables: syls.clone(),
                        edits: *edits,
                        exact: *exact,
                    });
                }
                layer = next;
            }
            spans
        }
    }

    fn node(p: usize, q: usize, word: String, code: &str, s: &Shape, lp: f64) -> LatticeNode {
        LatticeNode {
            start: p,
            end: q,
            word,
            syllables: s.syllables.clone(),
            syl_mask: s.mask,
            log_prob: lp,
            canon: Some((code.to_string(), s.mask)),
        }
    }

    pub(super) fn build_raw(
        b: &LatticeBuilder,
        keys: &str,
        edges: &[RawEdge],
        dict: &CachedDict,
        fuzzy_config: Option<&FuzzyConfig>,
        require_reachable: bool,
    ) -> Vec<Vec<LatticeNode>> {
        let mut nodes: Vec<Vec<LatticeNode>> = vec![Vec::new(); keys.len() + 1];
        let chain = RawChain::new(keys, edges, fuzzy_config);
        let mut seen: HashMap<String, (usize, bool)> = HashMap::new();
        for i in 0..chain.slots.len() {
            if require_reachable && !chain.reachable[i] {
                continue;
            }
            let p = chain.slots[i].start;
            let spans = chain.spans_from(i, b.max_word_len(), &|c| dict.has_longer_code(c));
            let mut seen_j = None;
            for ((j, code), shapes) in &spans {
                if seen_j != Some(*j) {
                    seen.clear();
                    seen_j = Some(*j);
                }
                let q = chain.slots[*j].end;
                let exact = shapes.iter().find(|s| s.exact);
                for hit in dict.search_with_boundary(code) {
                    let shape = match exact {
                        Some(s) if hit.boundary == 0 || hit.boundary == s.mask => s,
                        Some(_) => continue,
                        None => match shapes
                            .iter()
                            .filter(|s| hit.boundary == 0 || s.mask == hit.boundary)
                            .min_by_key(|s| s.edits)
                        {
                            Some(s) => s,
                            None => continue,
                        },
                    };
                    let log_prob = score_node(&hit.text, code, hit.weight)
                        - FUZZY_SYLLABLE_LOG_PENALTY * shape.edits as f64;
                    let is_fuzzy = !shape.exact;
                    if let Some((at, existing_fuzzy)) = seen.get_mut(hit.text.as_str()) {
                        let existing = &mut nodes[q][*at];
                        let replace = match (*existing_fuzzy, is_fuzzy) {
                            (true, false) => true,
                            (false, true) => false,
                            _ => log_prob > existing.log_prob,
                        };
                        if replace {
                            *existing = node(p, q, hit.text, code, shape, log_prob);
                            *existing_fuzzy = is_fuzzy;
                        }
                        continue;
                    }
                    seen.insert(hit.text.clone(), (nodes[q].len(), is_fuzzy));
                    nodes[q].push(node(p, q, hit.text, code, shape, log_prob));
                }
            }
        }
        nodes
    }
}
