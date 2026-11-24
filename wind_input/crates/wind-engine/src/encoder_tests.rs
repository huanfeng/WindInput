use super::*;
use std::collections::HashMap;

/// 五笔 86 标准三条规则（与 `data/schemas/wubi86.schema.toml` 一致）。
fn wubi_spec() -> EncoderSpec {
    EncoderSpec {
        max_word_length: 10,
        exclude_patterns: Vec::new(),
        rules: vec![
            EncoderRule {
                length_equal: 2,
                length_in_range: Vec::new(),
                formula: "AaAbBaBb".into(),
            },
            EncoderRule {
                length_equal: 3,
                length_in_range: Vec::new(),
                formula: "AaBaCaCb".into(),
            },
            EncoderRule {
                length_equal: 0,
                length_in_range: vec![4, 10],
                formula: "AaBaCaZa".into(),
            },
        ],
    }
}

fn codes() -> HashMap<char, String> {
    // 取真实五笔全码，便于人工核对。
    [
        ('你', "wqiy"),
        ('好', "vbg"),
        ('我', "trnt"),
        ('中', "khk"),
        ('国', "lgyi"),
        ('人', "wwww"),
        ('民', "nav"),
        ('共', "awu"),
        ('和', "tkg"),
    ]
    .into_iter()
    .map(|(c, s)| (c, s.to_string()))
    .collect()
}

fn lookup(m: &HashMap<char, String>) -> impl Fn(char) -> Option<String> + '_ {
    move |c| m.get(&c).cloned()
}

#[test]
fn parses_formula_pairs() {
    let steps = parse_formula("AaAbBaBb").unwrap();
    assert_eq!(
        steps,
        vec![
            FormulaStep {
                char_index: 0,
                code_index: 0
            },
            FormulaStep {
                char_index: 0,
                code_index: 1
            },
            FormulaStep {
                char_index: 1,
                code_index: 0
            },
            FormulaStep {
                char_index: 1,
                code_index: 1
            },
        ]
    );
}

/// `Z` 是末字记号，不能被当成 A=0 序列里的第 26 个字。
#[test]
fn z_means_last_char() {
    let steps = parse_formula("Za").unwrap();
    assert_eq!(steps[0].char_index, -1);
}

#[test]
fn rejects_malformed_formula() {
    assert!(parse_formula("Aa B").is_none(), "含空格应判非法");
    assert!(parse_formula("AaB").is_none(), "奇数长度应判非法");
    assert!(parse_formula("aA").is_none(), "大小写颠倒应判非法");
    assert!(parse_formula("").is_none(), "空公式应判非法");
    assert!(
        parse_formula("A字").is_none(),
        "非 ASCII 应判非法且不 panic"
    );
}

/// 二字词：各取前两码。你(wqiy)+好(vbg) → wq+vb = wqvb。
#[test]
fn two_char_word_takes_first_two_codes_each() {
    let m = codes();
    let code = calc_word_code("你好", &wubi_spec(), lookup(&m)).unwrap();
    assert_eq!(code, "wqvb");
}

/// 三字词：前两字首码 + 末字前两码。中(k)+国(l)+人(ww) → klww。
#[test]
fn three_char_word_uses_third_rule() {
    let m = codes();
    let code = calc_word_code("中国人", &wubi_spec(), lookup(&m)).unwrap();
    assert_eq!(code, "klww");
}

/// 四字：前三字首码 + 末字首码。中(k)+国(l)+人(w)+民(n) → klwn。
#[test]
fn four_char_word_uses_range_rule() {
    let m = codes();
    let code = calc_word_code("中国人民", &wubi_spec(), lookup(&m)).unwrap();
    assert_eq!(code, "klwn");
}

/// 五字及以上：`Za` 取的是末字，中间字全部跳过——中(k)+国(l)+人(w)+末字和(t)。
/// 这条专门盯住 `Z` 的语义：若把 `Z` 当成 A=0 序列的第 26 个字，此处会越界报错而非取到「和」。
#[test]
fn five_char_word_skips_middle_and_takes_last() {
    let m = codes();
    let code = calc_word_code("中国人民和", &wubi_spec(), lookup(&m)).unwrap();
    assert_eq!(code, "klwt");
}

/// 缺码整词作废，且错误携带具体是哪个字——这是排查造词不生效的关键线索。
#[test]
fn missing_code_fails_whole_word_with_char() {
    let m = codes();
    let err = calc_word_code("你囧好", &wubi_spec(), lookup(&m)).unwrap_err();
    assert_eq!(err, EncodeError::MissingCode { ch: '囧' });
}

/// 公式要第 2 码但该字只有 1 位码（简码被误当全码时的典型症状）→ 明确报错，不静默截断。
#[test]
fn code_shorter_than_formula_needs_reports_which_char() {
    let mut m = codes();
    m.insert('好', "v".into()); // 模拟错取了简码
    let err = calc_word_code("你好", &wubi_spec(), lookup(&m)).unwrap_err();
    assert_eq!(
        err,
        EncodeError::CodeTooShort {
            ch: '好',
            code: "v".into(),
            need: 1,
        }
    );
}

#[test]
fn rejects_single_char_and_missing_rules() {
    let m = codes();
    assert_eq!(
        calc_word_code("你", &wubi_spec(), lookup(&m)).unwrap_err(),
        EncodeError::TooShort
    );
    let empty = EncoderSpec::default();
    assert_eq!(
        calc_word_code("你好", &empty, lookup(&m)).unwrap_err(),
        EncodeError::NoRules
    );
}

/// 精确规则优先于区间规则，与书写顺序无关（区间写在前也不应抢走 length_equal 的匹配）。
#[test]
fn exact_rule_wins_over_range_regardless_of_order() {
    let spec = EncoderSpec {
        max_word_length: 10,
        exclude_patterns: Vec::new(),
        rules: vec![
            EncoderRule {
                length_equal: 0,
                length_in_range: vec![2, 10],
                formula: "AaBa".into(),
            },
            EncoderRule {
                length_equal: 2,
                length_in_range: Vec::new(),
                formula: "AaAbBaBb".into(),
            },
        ],
    };
    let m = codes();
    assert_eq!(
        calc_word_code("你好", &spec, lookup(&m)).unwrap(),
        "wqvb",
        "词长 2 应命中 length_equal 规则而非区间规则"
    );
}

// ───────────── 只用中文部分取码 ─────────────

fn encode(word: &str) -> Result<String, EncodeError> {
    let m = codes();
    encode_text(word, &wubi_spec(), |c, _| m.get(&c).cloned())
}

/// 姓名+手机号：数字整段跳过，按「张三」那部分取码（两者同码可接受）。
#[test]
fn digits_are_skipped_and_han_part_is_encoded() {
    assert_eq!(encode("你好13800138000").unwrap(), "wqvb");
    assert_eq!(encode("你1好").unwrap(), "wqvb", "夹在中间的数字同样跳过");
    assert_eq!(encode("中国人abc").unwrap(), "klww", "ASCII 字母跳过");
    assert_eq!(encode("中国 人-民").unwrap(), "klwn", "空白与符号跳过");
    assert_eq!(encode("你好，").unwrap(), "wqvb", "全角标点同样跳过");
}

/// 码表里有码的非汉字（`〇` 在五笔里是 `llll`、私用区字、部首…）照样参与取码，
/// 只有取不到码的非汉字才跳过。按「是不是汉字」一刀切会把「二〇二六」算成「二二六」的码。
#[test]
fn non_han_with_a_code_still_takes_part() {
    let mut m = codes();
    for (c, code) in [('〇', "llll"), ('二', "fgg"), ('六', "uygy")] {
        m.insert(c, code.into());
    }
    let enc = |w: &str| encode_text(w, &wubi_spec(), |c, _| m.get(&c).cloned());
    // 四字规则 AaBaCaZa：二(f)〇(l)二(f)六(u)。
    assert_eq!(enc("二〇二六").unwrap(), "flfu");
    assert_eq!(enc("二〇二六1").unwrap(), "flfu", "没码的数字仍跳过");
}

/// 剩 1 个汉字按单字规则（直取全码），不进词组公式。
#[test]
fn single_han_left_uses_single_char_rule() {
    assert_eq!(encode("你123").unwrap(), "wqiy");
}

/// 一个汉字都不剩 → 仍失败，不能退化成空码或拿符号去凑。
#[test]
fn no_han_left_still_fails() {
    assert_eq!(encode("13800138000"), Err(EncodeError::NoHanChars));
    assert_eq!(encode("ab"), Err(EncodeError::NoHanChars));
}

/// 「你X好」保护不能被连带打穿：X 是取不到码的汉字（生僻字）时仍整词作废。
/// 只有非汉字才可跳过——把生僻字也跳过，就会静默造出「你好」的码。
#[test]
fn rare_han_without_code_still_fails_whole_word() {
    assert_eq!(
        encode("你囧好123"),
        Err(EncodeError::MissingCode { ch: '囧' })
    );
}

/// 词本身就是单个字符：保持直取口径（不论是否汉字），取不到报 MissingCode。
#[test]
fn single_char_word_keeps_direct_lookup() {
    assert_eq!(encode("你").unwrap(), "wqiy");
    assert_eq!(encode("a"), Err(EncodeError::MissingCode { ch: 'a' }));
}

/// 公式对每个字要取到第几位，要如实告诉码源——码源据此挑一条够长的码。
#[test]
fn code_source_is_told_how_many_codes_each_char_needs() {
    let m = codes();
    let seen = std::cell::RefCell::new(Vec::new());
    calc_word_code_by("中国人", &wubi_spec(), |c, need| {
        seen.borrow_mut().push((c, need));
        m.get(&c).cloned()
    })
    .unwrap();
    // AaBaCaCb：中、国只取第 1 码，人取到第 2 码。
    assert_eq!(seen.into_inner(), vec![('中', 1), ('国', 1), ('人', 2)]);
}

// ───────────── 按用户实际打的码挑全码 ─────────────

/// 多读音同长（嘞 le…/lw…）：全码表按权重挑了 le，用户打的是 lw → 必须取 lw 那条。
#[test]
fn hint_picks_the_reading_user_typed() {
    let all = ["lekk", "lwkk"];
    assert_eq!(
        pick_hinted_code("lw", Some("lekk"), all.iter().copied(), 4, 2).as_deref(),
        Some("lwkk")
    );
}

/// 全码表的那条本就与用户所打一致 → 用它（它是按权重挑过的）。
#[test]
fn hint_prefers_full_table_code_when_consistent() {
    let all = ["lw", "lwka", "lwkk"];
    assert_eq!(
        pick_hinted_code("lw", Some("lwkk"), all.iter().copied(), 4, 2).as_deref(),
        Some("lwkk")
    );
}

/// 用户打的是 1 位简码（没 → m）、公式要第 2 位：取同前缀里够长的码，不取 m 本身。
#[test]
fn short_hint_is_extended_to_a_long_enough_code() {
    let all = ["m", "mody", "mwdy"];
    assert_eq!(
        pick_hinted_code("mw", Some("m"), all.iter().copied(), 4, 2).as_deref(),
        Some("mwdy")
    );
    assert_eq!(
        pick_hinted_code("m", Some("m"), all.iter().copied(), 4, 2).as_deref(),
        Some("mody"),
        "全码表那条不够长 → 退到同前缀的更长码（同长取先出现者）"
    );
}

/// 超过码长上限的怪码（6 码扩展码）不参与；与用户所打对不上的（临拼出的拼音码）→ None，
/// 由调用方回退全码表。
#[test]
fn hint_respects_cap_and_falls_back_when_inconsistent() {
    let all = ["m", "okuvuu"];
    assert_eq!(
        pick_hinted_code("ok", Some("m"), all.iter().copied(), 4, 2),
        None
    );
    let all = ["lekk"];
    assert_eq!(
        pick_hinted_code("lei", Some("lekk"), all.iter().copied(), 4, 2),
        None
    );
}

/// 词长超出所有规则覆盖范围 → 明确报 NoMatchingRule，不静默返回空串。
#[test]
fn word_longer_than_all_rules_reports_no_matching_rule() {
    let m = codes();
    let spec = EncoderSpec {
        max_word_length: 10,
        exclude_patterns: Vec::new(),
        rules: vec![EncoderRule {
            length_equal: 2,
            length_in_range: Vec::new(),
            formula: "AaAbBaBb".into(),
        }],
    };
    assert_eq!(
        calc_word_code("中国人", &spec, lookup(&m)).unwrap_err(),
        EncodeError::NoMatchingRule { word_len: 3 }
    );
}
