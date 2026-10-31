//! 简拼逐级补全：整长匹配 → 词组前缀 → 首声母单字（对齐微软拼音）。
//!
//! 此前整串能命中就不再退到更短的切点，`tymh` 这类只有 1 个整长词的输入只剩 1 个候选。
//! 现在整长匹配之后仍追加短切点的词组与首声母单字，界面序里沉在整长匹配之后。
//!
//! 自带 wdat 夹具（简拼索引只有 mmap 词典才有），写法同 `pinyin_mixed_abbrev.rs`。

use wind_candidate::candidate_display_order;
use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

fn fixture(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_abbrev_short_fallback_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // bu|xiang|zhi|dao
    w.add_with_boundary(
        "buxiangzhidao".into(),
        vec![("不想知道".into(), 96, 0, 0b1001000010001)],
    );
    // bu|xiang|zhi
    w.add_with_boundary(
        "buxiangzhi".into(),
        vec![("不想知".into(), 40, 0, 0b100001001)],
    );
    // bi|xu、bu|xiang
    w.add_with_boundary("bixu".into(), vec![("必须".into(), 51514, 0, 0b101)]);
    w.add_with_boundary("buxiang".into(), vec![("不想".into(), 21277, 0, 0b10001)]);
    // 单字
    w.add_with_boundary("bu".into(), vec![("不".into(), 162529, 0, 0b1)]);
    w.add_with_boundary("ba".into(), vec![("把".into(), 90000, 0, 0b1)]);

    w.add_abbrev("bxzd".into(), vec![("buxiangzhidao".into(), 96)]);
    w.add_abbrev("bxz".into(), vec![("buxiangzhi".into(), 40)]);
    w.add_abbrev(
        "bx".into(),
        vec![("bixu".into(), 51514), ("buxiang".into(), 21277)],
    );
    w.write(&wdat).unwrap();

    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

/// 界面序（协调器的 `candidate_display_order`）下的候选文本。
fn ui_texts(e: &PinyinEngine, input: &str) -> Vec<String> {
    let mut cands = e
        .convert(input, 50)
        .map(|r| r.candidates)
        .unwrap_or_default();
    cands.sort_by(|a, b| candidate_display_order(a, b, false, false, input));
    cands.into_iter().map(|c| c.text).collect()
}

fn pos(t: &[String], s: &str) -> usize {
    t.iter()
        .position(|x| x == s)
        .unwrap_or_else(|| panic!("缺少「{s}」: {t:?}"))
}

/// 整长匹配仍排第一，其后是词组前缀（长切点在前），最后才是单字。
#[test]
fn full_match_then_phrase_prefix_then_single_char() {
    let e = PinyinEngine::new(PyConfig::default(), fixture("order"));
    let t = ui_texts(&e, "bxzd");
    assert_eq!(t[0], "不想知道", "整长匹配必须仍是首选: {t:?}");
    assert!(pos(&t, "不想知") < pos(&t, "必须"), "长切点词组在前: {t:?}");
    assert!(pos(&t, "必须") < pos(&t, "不"), "词组在单字之前: {t:?}");
    assert!(t.contains(&"把".to_string()), "首声母单字也要补上: {t:?}");
}

/// 首个音节已经打全（`buxzd` = bu + xzd）时，首字母 `b` 不是用户的意图，不补单字。
#[test]
fn no_initial_single_char_when_first_syllable_is_complete() {
    let e = PinyinEngine::new(PyConfig::default(), fixture("complete_first"));
    let t = ui_texts(&e, "buxzd");
    assert!(
        !t.contains(&"把".to_string()),
        "bu 已打全，不该出 ba 的单字「把」: {t:?}"
    );
}
