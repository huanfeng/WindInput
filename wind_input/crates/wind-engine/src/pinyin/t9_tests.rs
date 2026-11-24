use std::sync::Arc;

use super::*;
use crate::engine::{ConvertOptions, Engine};
use crate::pinyin::Config;
use wind_dict::cached::CachedDict;
use wind_dict::codetable::CodetableDict;

/// 词条 `(字, 空格分隔的拼音, 权重)` 写成 rime yaml 再载入，词条带真值音节边界。
fn engine(tag: &str, entries: &[(&str, &str, i32)]) -> PinyinEngine {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("wind_t9_{tag}_{}.dict.yaml", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        for (text, code, w) in entries {
            writeln!(f, "{text}\t{code}\t{w}").unwrap();
        }
    }
    let dict = CodetableDict::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    PinyinEngine::new(Config::default(), CachedDict::Memory(dict))
}

fn base_words() -> Vec<(&'static str, &'static str, i32)> {
    vec![
        ("你", "ni", 9000),
        ("尼", "ni", 800),
        ("米", "mi", 900),
        ("好", "hao", 8000),
        ("敢", "gan", 600),
        ("干", "gan", 700),
        ("哈", "ha", 500),
        ("你好", "ni hao", 7000),
        ("你敢", "ni gan", 300),
        ("先", "xian", 4000),
        ("西", "xi", 3000),
        ("安", "an", 2500),
        ("西安", "xi an", 2000),
        ("我", "wo", 9500),
        ("们", "men", 6000),
        ("我们", "wo men", 8000),
    ]
}

fn t9(eng: &PinyinEngine, keys: &str) -> ConvertResult {
    let opts = ConvertOptions {
        keypad: Some(Arc::new(KeypadMap::t9())),
        ..Default::default()
    };
    eng.convert_with_opts(keys, 100, opts).unwrap()
}

fn texts(r: &ConvertResult) -> Vec<&str> {
    r.candidates.iter().map(|c| c.text.as_str()).collect()
}

fn find<'a>(r: &'a ConvertResult, text: &str) -> &'a Candidate {
    r.candidates
        .iter()
        .find(|c| c.text == text)
        .unwrap_or_else(|| panic!("候选里没有「{text}」：{:?}", texts(r)))
}

/// 键面映射按电话键盘；字母键只代表自己，分词键不代表任何字母。
#[test]
fn t9_map_admits_phone_letters_and_locked_letters() {
    let m = KeypadMap::t9();
    assert!(m.admits(b'7', b's') && m.admits(b'9', b'z') && !m.admits(b'2', b'd'));
    assert!(m.admits(b'n', b'n') && !m.admits(b'n', b'm'));
    assert!(m.is_code_char('\'') && m.is_code_char('5') && !m.is_code_char('1'));
}

/// 同一串键的多种拼法都解得出词，整词按权重排在前面。
#[test]
fn ambiguous_keys_decode_every_spelling() {
    let eng = engine("amb", &base_words());
    let r = t9(&eng, "64426");
    assert_eq!(r.candidates[0].text, "你好", "{:?}", texts(&r));
    assert!(texts(&r).contains(&"你敢"));
    assert_eq!(find(&r, "你好").consumed_length, 5);
}

/// 预编辑显示首选的拼音而不是数字。
#[test]
fn preedit_shows_pinyin_of_best_reading() {
    let eng = engine("pre", &base_words());
    assert_eq!(t9(&eng, "64426").preedit_display, "ni'hao");
}

/// 造词码是全拼：九宫格打出的词在 26 键下能查回来。
#[test]
fn learn_code_is_full_pinyin() {
    let eng = engine("learn", &base_words());
    let r = t9(&eng, "64426");
    let c = find(&r, "你好");
    assert_eq!(c.meta.learn_code, Some(("nihao".to_string(), 0b101)));
    assert_eq!(c.pinyin, "ni'hao");
}

/// 词典真值切分要和键上的切分一致：`9426` 既是 xian 也是 xi|an。
#[test]
fn both_segmentations_are_candidates() {
    let eng = engine("seg", &base_words());
    let r = t9(&eng, "9426");
    assert!(texts(&r).contains(&"先") && texts(&r).contains(&"西安"));
}

/// 分词键是硬边界：音节不能跨过它，消费长度含分词键。
#[test]
fn separator_forbids_crossing_syllables() {
    let eng = engine("sep", &base_words());
    let r = t9(&eng, "94'26");
    assert!(!texts(&r).contains(&"先"), "{:?}", texts(&r));
    let c = find(&r, "西安");
    assert_eq!(c.consumed_length, 5);
    let xi = find(&r, "西");
    assert_eq!(xi.code, "94'");
    assert!(xi.is_partial);
}

/// 已锁定的字母只代表自己：`ni` 锁定后不再出 mi 开头的词。
#[test]
fn locked_letters_narrow_the_reading() {
    let eng = engine("lock", &base_words());
    let r = t9(&eng, "ni'426");
    assert!(texts(&r).contains(&"你好"));
    assert!(!texts(&r).contains(&"米"));
    let r = t9(&eng, "mi'426");
    assert!(!texts(&r).contains(&"你好"), "{:?}", texts(&r));
}

/// 末音节没打完时补全成词，标为前缀候选，仍消费整串。
#[test]
fn trailing_partial_syllable_completes_words() {
    let eng = engine("tail", &base_words());
    let r = t9(&eng, "6442");
    let c = find(&r, "你好");
    assert!(c.is_prefix);
    assert_eq!(c.consumed_length, 4);
}

/// 只有一个键时也有候选（残缺音节的单字）。
#[test]
fn single_key_offers_characters() {
    let eng = engine("one", &base_words());
    let r = t9(&eng, "9");
    assert!(texts(&r).contains(&"我"), "{:?}", texts(&r));
}

/// 两个词拼成整串时出整句候选。
#[test]
fn sentence_spans_multiple_words() {
    let eng = engine("sent", &base_words());
    let r = t9(&eng, "9663664426");
    let s = r
        .candidates
        .iter()
        .find(|c| c.is_sentence)
        .expect("没有整句");
    assert_eq!(s.text, "我们你好");
    assert_eq!(s.consumed_length, 10);
    assert_eq!(r.preedit_display, "wo'men'ni'hao");
}

/// 查询串里有非编码字符（`1`、标点）时不解码。
#[test]
fn foreign_chars_yield_nothing() {
    let eng = engine("foreign", &base_words());
    assert!(t9(&eng, "641").candidates.is_empty());
}

/// 不带 keypad 时数字串照旧按全拼处理（电脑端行为不变）。
#[test]
fn without_keypad_digits_are_not_decoded() {
    let eng = engine("plain", &base_words());
    let r = eng.convert("64426", 100).unwrap();
    assert!(!r.candidates.iter().any(|c| c.text == "你好"));
}

/// 吃满整串的词典词压过单字拼成的整句，即便词图选了整句（词权重极低）。
#[test]
fn whole_word_outranks_char_sentence() {
    let eng = engine(
        "demote",
        &[
            ("不", "bu", 100_000_000),
            ("带", "dai", 90_000_000),
            ("布袋", "bu dai", 1),
        ],
    );
    let keys = "28324";
    let mut r = t9(&eng, keys);
    let s = r
        .candidates
        .iter()
        .find(|c| c.is_sentence)
        .expect("没有整句");
    assert_eq!(s.text, "不带");
    assert!(s.is_sentence_demoted);
    r.candidates
        .sort_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, keys));
    assert_eq!(r.candidates[0].text, "布袋", "{:?}", texts(&r));
}
