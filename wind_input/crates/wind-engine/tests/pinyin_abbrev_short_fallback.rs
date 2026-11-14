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

/// 首声母单字不设小限额：和直接打 `b` 一样，能挑到词库里该声母下的所有单字。
#[test]
fn initial_single_chars_are_not_capped_to_a_handful() {
    let dir = std::env::temp_dir().join("wind_abbrev_short_fallback_many");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary(
        "buxiangzhidao".into(),
        vec![("不想知道".into(), 96, 0, 0b1001000010001)],
    );
    w.add_abbrev("bxzd".into(), vec![("buxiangzhidao".into(), 96)]);
    // 20 个 b 开头的单字（同码不同字），远多于词组前缀的单切点限额
    let hanzi: Vec<char> = "把爸吧罢霸坝拔跋靶耙钯魃茇菝詙鲌鲅岜灞鲃".chars().collect();
    let items: Vec<(String, i32, u32, u64)> = hanzi
        .iter()
        .enumerate()
        .map(|(i, c)| (c.to_string(), 1000 - i as i32, i as u32, 0b1))
        .collect();
    w.add_with_boundary("ba".into(), items);
    w.write(&wdat).unwrap();
    let e = PinyinEngine::new(
        PyConfig::default(),
        CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具"),
    );
    let t = ui_texts(&e, "bxzd");
    let singles = t.iter().filter(|x| x.chars().count() == 1).count();
    assert_eq!(singles, hanzi.len(), "单字应全部可选: {t:?}");
}

/// 两个字母的简拼（`bx`）也要有首声母单字：只有词组、无法选单字是此前的缺口。
#[test]
fn two_letter_abbrev_also_gets_initial_single_chars() {
    let e = PinyinEngine::new(PyConfig::default(), fixture("two_letter"));
    let t = ui_texts(&e, "bx");
    assert!(pos(&t, "必须") < pos(&t, "不"), "词组在前、单字在后: {t:?}");
    assert!(
        t.contains(&"把".to_string()),
        "两字母简拼也补首声母单字: {t:?}"
    );
}

/// 词组档的条数受内部配置控制：`per_cut_short` 置 1 时 `bx` 切点只留权重最高的 1 条。
#[test]
fn per_cut_limit_comes_from_internal_config() {
    use wind_engine::pinyin::AbbrevFallbackConfig;
    let cfg = PyConfig {
        abbrev_fallback: AbbrevFallbackConfig {
            per_cut_short: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let e = PinyinEngine::new(cfg, fixture("cfg"));
    let t = ui_texts(&e, "bxzd");
    // 夹具里 bx 有「必须(51514)、不想(21277)」两条，限额 1 ⇒ 只留「必须」
    assert!(t.contains(&"必须".to_string()), "{t:?}");
    assert!(
        !t.contains(&"不想".to_string()),
        "per_cut_short=1 时第二条该被截: {t:?}"
    );
    // 默认配置则两条都在
    let e = PinyinEngine::new(PyConfig::default(), fixture("cfg_default"));
    let t = ui_texts(&e, "bxzd");
    assert!(t.contains(&"不想".to_string()), "{t:?}");
}

/// 首音节是完整音节（`baxzd` = ba + xzd）同样不补 `b` 单字——不能只靠 `u` 这类字母碰巧过关。
#[test]
fn no_initial_single_char_when_first_syllable_is_ba() {
    let e = PinyinEngine::new(PyConfig::default(), fixture("ba_first"));
    let t = ui_texts(&e, "baxzd");
    assert!(
        t.iter().all(|x| x.chars().count() != 1 || x != "不"),
        "ba 已打全，不补 b 开头的单字: {t:?}"
    );
}

/// 手动分隔符：回退词组的消费数要换算回含 `'` 的原始输入（`b'z'd` 选 `bz` 的词组只吃 `b'z`）。
#[test]
fn separator_input_maps_fallback_consumed_length_to_raw() {
    let dir = std::env::temp_dir().join("wind_abbrev_short_fallback_sep");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("buzhidao".into(), vec![("不知道".into(), 96, 0, 0b10101)]);
    w.add_with_boundary("bangzhu".into(), vec![("帮助".into(), 5000, 0, 0b10001)]);
    w.add_abbrev("bzd".into(), vec![("buzhidao".into(), 96)]);
    w.add_abbrev("bz".into(), vec![("bangzhu".into(), 5000)]);
    w.write(&wdat).unwrap();
    let e = PinyinEngine::new(
        PyConfig::default(),
        CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具"),
    );
    let r = e.convert("b'z'd", 50).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "帮助")
        .unwrap_or_else(|| panic!("缺少回退词组「帮助」: {:?}", r.candidates));
    assert_eq!(c.consumed_length, 3, "b'z 共 3 字节，剩 `'d`/`d` 续打");
}

/// 两字母就是声母（`sh`）时首字母 `s` 不是用户的意图，不补 s 开头的单字。
#[test]
fn bare_shengmu_pair_does_not_get_first_letter_singles() {
    let dir = std::env::temp_dir().join("wind_abbrev_short_fallback_sh");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("shi".into(), vec![("是".into(), 9000, 0, 0b1)]);
    w.add_with_boundary("si".into(), vec![("四".into(), 8000, 0, 0b1)]);
    w.write(&wdat).unwrap();
    let e = PinyinEngine::new(
        PyConfig::default(),
        CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具"),
    );
    let t = ui_texts(&e, "sh");
    assert!(
        !t.contains(&"四".to_string()),
        "sh 不该补 s 声母单字: {t:?}"
    );
}

/// 候选上限收紧时（真机约 290 条），词库单字自带的高权重基数不能把词组挤出窗口。
#[test]
fn tight_limit_keeps_phrase_prefix_before_single_chars() {
    let dir = std::env::temp_dir().join("wind_abbrev_short_fallback_tight");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary(
        "buxiangzhidao".into(),
        vec![("不想知道".into(), 96, 0, 0b1001000010001)],
    );
    w.add_with_boundary("bixu".into(), vec![("必须".into(), 51514, 0, 0b101)]);
    w.add_abbrev("bxzd".into(), vec![("buxiangzhidao".into(), 96)]);
    w.add_abbrev("bx".into(), vec![("bixu".into(), 51514)]);
    let hanzi: Vec<char> = "把爸吧罢霸坝拔跋靶耙钯魃茇菝詙鲌鲅岜灞鲃".chars().collect();
    let items: Vec<(String, i32, u32, u64)> = hanzi
        .iter()
        .enumerate()
        .map(|(i, c)| (c.to_string(), 10_000_000 + 1000 - i as i32, i as u32, 0b1))
        .collect();
    w.add_with_boundary("ba".into(), items);
    w.write(&wdat).unwrap();
    let e = PinyinEngine::new(
        PyConfig::default(),
        CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具"),
    );
    let mut cands = e
        .convert("bxzd", 5)
        .map(|r| r.candidates)
        .unwrap_or_default();
    cands.sort_by(|a, b| candidate_display_order(a, b, false, false, "bxzd"));
    let t: Vec<String> = cands.into_iter().map(|c| c.text).collect();
    assert!(
        t.contains(&"必须".to_string()),
        "上限 5 时词组也要留下: {t:?}"
    );
}
