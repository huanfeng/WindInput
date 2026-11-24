use super::*;
use wind_dict::codetable::CodetableDict;

/// 用 (code, text, weight) 三元组建内存拼音词典。
fn dict_from(entries: &[(&str, &str, i32)]) -> CachedDict {
    let mut d = CodetableDict::empty();
    for (code, text, weight) in entries {
        d.merge_single(code.to_string(), text.to_string(), *weight, 0);
    }
    CachedDict::Memory(d)
}

/// 扁平码（不关心边界的断言用）。
fn gen_py(entries: &[(&str, &str, i32)], word: &str) -> Option<String> {
    gen_py_spaced(entries, word).map(|s| s.replace(' ', ""))
}

/// 引擎的原始产出：带空格的音节码。空格即边界，是本模块的真相源。
fn gen_py_spaced(entries: &[(&str, &str, i32)], word: &str) -> Option<String> {
    let dict = dict_from(entries);
    let idx = CharPinyinIndex::build(&dict);
    generate_word_pinyin(&dict, &idx, word)
}

/// 落库口径：按 `wind_store::wdict::split_spaced_code` 拆成 `(flat, boundary)`，
/// 即用户词表里最终存下的形态。
fn gen_py_stored(entries: &[(&str, &str, i32)], word: &str) -> Option<(String, u64)> {
    gen_py_spaced(entries, word).map(|s| wind_store::wdict::split_spaced_code(&s))
}

/// 同权重的多个读音按 [`STANDARD_SYLLABLES`] 的次序排，与词条写进词典的先后无关。
///
/// 样本给「长」40 个读音、两档权重交错，排序必须真的搬动元素；只有两个或全部同权重的读音时
/// 排序输入本就有序，排序键漏掉音节 id 也照样绿。
#[test]
fn equal_weight_readings_follow_syllable_table_order() {
    let pos = |s: &str| STANDARD_SYLLABLES.iter().position(|x| *x == s).unwrap();
    let syls: Vec<&str> = STANDARD_SYLLABLES
        .iter()
        .step_by(9)
        .take(40)
        .copied()
        .collect();
    let weight = |i: usize| if i.is_multiple_of(2) { 100 } else { 500 };
    // 故意按音节表倒序写进词典；另配两个字作陪。
    let mut entries: Vec<(&str, &str, i32)> = syls
        .iter()
        .enumerate()
        .rev()
        .map(|(i, s)| (*s, "长", weight(i)))
        .collect();
    entries.extend([("ni", "你", 1), ("a", "阿", 3)]);
    let dict = dict_from(&entries);
    let idx = CharPinyinIndex::build(&dict);

    // 期望：先 500 档、再 100 档，档内按音节表次序（`syls` 本身就是音节表次序）。
    let mut want: Vec<&str> = syls
        .iter()
        .enumerate()
        .filter(|(i, _)| weight(*i) == 500)
        .map(|(_, s)| *s)
        .collect();
    want.extend(
        syls.iter()
            .enumerate()
            .filter(|(i, _)| weight(*i) == 100)
            .map(|(_, s)| *s),
    );
    assert!(
        want.windows(2).take(19).all(|w| pos(w[0]) < pos(w[1])),
        "前提：档内升序"
    );
    let n = idx.reading_count('长').unwrap();
    let got: Vec<&str> = (0..n).map(|i| idx.reading('长', i).unwrap()).collect();
    assert_eq!(got, want, "同权重读音须按音节表次序");
    assert_eq!(idx.representative('长'), Some(want[0]));
}

/// 造词须同时产出音节边界；三条产码路径（整词消歧 / 子词切分 / 逐字兜底）都要带边界。
///
/// 断言落在带空格的产出上：空格即边界，不会出现 bitmask 与 code 各存一份而不一致。
#[test]
fn generate_word_pinyin_carries_boundary() {
    let entries = &[
        ("ni", "你", 100),
        ("hao", "好", 100),
        ("nihao", "你好", 500),
        ("chong", "重", 50),
        ("zhong", "重", 900),
        ("qing", "庆", 100),
        ("chongqing", "重庆", 800),
    ];
    // 整词命中
    assert_eq!(gen_py_spaced(entries, "你好").as_deref(), Some("ni hao"));
    // 整词消歧（重庆读 chongqing 而非 zhongqing）
    assert_eq!(
        gen_py_spaced(entries, "重庆").as_deref(),
        Some("chong qing")
    );
    // 单字：整串一个音节，无内部边界可标
    assert_eq!(gen_py_spaced(entries, "你").as_deref(), Some("ni"));
    // 逐字兜底（整词不在词典）
    assert_eq!(gen_py_spaced(entries, "你重").as_deref(), Some("ni zhong"));

    // 落库形态：key 扁平、边界随 value 存下
    assert_eq!(
        gen_py_stored(entries, "你好"),
        Some(("nihao".into(), 0b101))
    );
    assert_eq!(
        gen_py_stored(entries, "重庆"),
        Some(("chongqing".into(), 0b100001))
    );
    assert_eq!(
        gen_py_stored(entries, "你重"),
        Some(("nizhong".into(), 0b101))
    );
    // 单音节的 `0b1` 不经文本往返（见 wdict::single_syllable_boundary_is_lossy_by_design）。
    // 单音节无切分歧义，且 0 在消费端一律是「放行」，不会误杀。
    assert_eq!(gen_py_stored(entries, "你"), Some(("ni".into(), 0)));
}

/// 子词切分路径：段自身是多音节整词时，段内边界须并入全局，不能把整段当一个音节。
#[test]
fn subword_segmentation_preserves_inner_boundary() {
    let entries = &[
        ("ni", "你", 100),
        ("hao", "好", 100),
        ("nihao", "你好", 500),
        ("a", "啊", 100),
    ];
    // 「你好啊」整词不在词典 → 子词切分：段「你好」(ni hao) + 单字「啊」(a)。
    assert_eq!(
        gen_py_spaced(entries, "你好啊").as_deref(),
        Some("ni hao a")
    );
    assert_eq!(
        gen_py_stored(entries, "你好啊"),
        Some(("nihaoa".into(), 0b100101))
    );
}

/// 超长词（拼接 >64 字节）：bitmask 装不下，落库时整体降级为 0；但带空格的产出本身不受限，
/// 边界在传输/导出层面得以保全。
#[test]
fn overlong_word_keeps_spaces_though_mask_degrades() {
    let entries = &[("zhuang", "装", 100)];
    let word: String = std::iter::repeat_n('装', 12).collect(); // 12*6=72B
    let spaced = gen_py_spaced(entries, &word).expect("应能逐字兜底产码");
    assert_eq!(spaced.split(' ').count(), 12, "12 个音节全部保留");
    assert_eq!(
        gen_py_stored(entries, &word).unwrap().1,
        0,
        "落库时超 64B 整体降级为无边界（bitmask 的固有上限）"
    );
}

/// 多音字按权重择优：费→fei(1000) 而非 bi(50)，强→qiang(1000) 而非 jiang(80)。
#[test]
fn multi_pron_by_weight() {
    let entries = &[
        ("fei", "费", 1000),
        ("bi", "费", 50),
        ("qiang", "强", 1000),
        ("jiang", "强", 80),
        ("xiao", "晓", 1000),
    ];
    assert_eq!(gen_py(entries, "费").as_deref(), Some("fei"));
    assert_eq!(gen_py(entries, "强").as_deref(), Some("qiang"));
    // 整词不在词典 → 逐字代表读音兜底
    assert_eq!(gen_py(entries, "费晓强").as_deref(), Some("feixiaoqiang"));
}

/// 整词命中覆盖逐字代表读音：重→代表音 zhong，但"重庆"整词读 chongqing。
#[test]
fn whole_word_overrides_per_char() {
    let entries = &[
        ("zhong", "重", 1000),
        ("chong", "重", 80),
        ("qing", "庆", 1000),
        ("chongqing", "重庆", 500),
    ];
    assert_eq!(gen_py(entries, "重庆").as_deref(), Some("chongqing"));
}

/// 长词子词切分继承读音：长→代表音 zhang，但经"长江"+"三角洲"得 changjiangsanjiaozhou。
#[test]
fn subword_segmentation() {
    let entries = &[
        ("zhang", "长", 1000),
        ("chang", "长", 500),
        ("jiang", "江", 1000),
        ("san", "三", 1000),
        ("jiao", "角", 1000),
        ("zhou", "洲", 1000),
        ("changjiang", "长江", 600),
        ("sanjiaozhou", "三角洲", 500),
    ];
    assert_eq!(
        gen_py(entries, "长江三角洲").as_deref(),
        Some("changjiangsanjiaozhou")
    );
}

/// 简单整词命中。
#[test]
fn simple_whole_word() {
    let entries = &[
        ("ni", "你", 1000),
        ("hao", "好", 1000),
        ("nihao", "你好", 800),
    ];
    assert_eq!(gen_py(entries, "你好").as_deref(), Some("nihao"));
}

/// 含无读音字符 → None。
#[test]
fn unknown_char_returns_none() {
    let entries = &[("ni", "你", 1000)];
    // "你X"：X 无任何读音
    assert_eq!(gen_py(entries, "你X"), None);
}
