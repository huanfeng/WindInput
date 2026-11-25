//! 双拼九宫格（小鹤为主）。键位：`ni` = 64，`hc`（hao）= 42，`wo` = 96，`mf`（men）= 63，
//! `xi` = 94，`aj`（an）= 25。

use std::sync::Arc;

use super::tests::{base_words, engine, find, texts};
use super::*;
use crate::engine::{ConvertOptions, Engine};

fn layout(id: &str) -> Layout {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../../data/schemas/shuangpin/{id}.toml"));
    Layout::from_toml(&p).unwrap_or_else(|e| panic!("加载 {id} 失败: {e}"))
}

fn sp_engine(tag: &str, id: &str) -> PinyinEngine {
    engine(tag, &base_words()).with_shuangpin(ShuangpinConverter::new(layout(id)))
}

fn t9(eng: &PinyinEngine, keys: &str) -> ConvertResult {
    let opts = ConvertOptions {
        keypad: Some(Arc::new(KeypadMap::t9())),
        ..Default::default()
    };
    eng.convert_with_opts(keys, 100, opts).unwrap()
}

/// 两键一对，每对取字母的笛卡尔积：`6442` 是 ni|hc → 你好。
#[test]
fn pairs_decode_through_layout() {
    let eng = sp_engine("sp_pair", "xiaohe");
    let r = t9(&eng, "6442");
    assert_eq!(r.candidates[0].text, "你好", "{:?}", texts(&r));
    assert_eq!(find(&r, "你好").consumed_length, 4);
    assert_eq!(r.preedit_display, "ni'hao");
}

/// 造词码是全拼坐标，候选边界是击键坐标：两者在双拼下不同。
#[test]
fn learn_code_in_full_pinyin_boundary_in_keys() {
    let eng = sp_engine("sp_learn", "xiaohe");
    let r = t9(&eng, "96636442");
    let s = r
        .candidates
        .iter()
        .find(|c| c.is_sentence)
        .expect("没有整句");
    assert_eq!(s.text, "我们你好");
    assert_eq!(
        s.meta.learn_code,
        Some(("womennihao".to_string(), 0b1010_0101))
    );
    assert_eq!(s.boundary, 0b101_0101);
    assert_eq!(s.pinyin, "wo'men'ni'hao");
}

/// 输入末尾落单的键是残缺声母：只补全成词或出单字，消费整串。
#[test]
fn trailing_lone_key_is_partial_initial() {
    let eng = sp_engine("sp_lone", "xiaohe");
    let r = t9(&eng, "644");
    let c = find(&r, "你好");
    assert!(c.is_prefix);
    assert_eq!(c.consumed_length, 3);
    // 4 = g/h/i，小鹤 i 是 ch：残缺声母不会带出 g/h/ch 以外的字。
    let r = t9(&eng, "4");
    assert!(texts(&r).contains(&"好"), "{:?}", texts(&r));
    assert!(!texts(&r).contains(&"安"), "{:?}", texts(&r));
}

/// `'` 重置配对：`96'63` 与 `9663` 同为 wo|mf；`9'663` 让 9 落单，配不出「我们」。
#[test]
fn separator_resets_pairing() {
    let eng = sp_engine("sp_sep", "xiaohe");
    assert!(texts(&t9(&eng, "9663")).contains(&"我们"));
    let r = t9(&eng, "96'63");
    assert_eq!(find(&r, "我们").consumed_length, 5);
    assert!(!texts(&t9(&eng, "9'663")).contains(&"我们"));
}

/// 零声母按布局规则：小鹤 `aj` = an（25），`xi aj` = 西安。
#[test]
fn zero_initial_follows_layout() {
    let eng = sp_engine("sp_zero", "xiaohe");
    assert!(texts(&t9(&eng, "25")).contains(&"安"));
    assert!(texts(&t9(&eng, "9425")).contains(&"西安"));
}

/// 布局用到 `;` 等符号键时九宫格打不全：判为不支持，转换返回空。
#[test]
fn symbol_key_layouts_are_unsupported() {
    assert!(shuangpin_layout_is_letter_only(&layout("xiaohe")));
    assert!(!shuangpin_layout_is_letter_only(&layout("mspy")));
    let eng = sp_engine("sp_mspy", "mspy");
    assert!(!eng.keypad_supported());
    assert!(t9(&eng, "6442").candidates.is_empty());
    assert!(sp_engine("sp_xh", "xiaohe").keypad_supported());
}
