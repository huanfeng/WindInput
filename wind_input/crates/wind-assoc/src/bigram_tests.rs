use super::*;

fn table() -> AssocBigram {
    // 按字节序排好：今(E4BB8A) < 我(E68891) < 谢(E8B0A2)
    AssocBigram::parse("# 注释\n今天\t$ 是 我 晚上\n我\t的 $ 不\n谢谢\t你 大家 $\n".into()).unwrap()
}

/// 随包的表整份常驻内存（文本 + 每行 4 字节偏移），PC 用户对内存敏感。重新蒸馏
/// （`scripts/lm/build_assoc.sh`）让它变大时在这里拦下。确要放宽：先把读端改成内存映射
/// （见 `docs/design/association.md` §4.3），再把两处的上限一起改。
#[test]
fn shipped_table_parses_within_memory_budget() {
    const MAX_BYTES: usize = 2560 * 1024;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../data/assoc/assoc_bigram.txt"
    );
    let text = std::fs::read_to_string(path).expect("读随包的联想表");
    let t = AssocBigram::parse(text).expect("随包的联想表能解析");
    let resident = t.text.len() + t.lines.len() * std::mem::size_of::<u32>();
    assert!(
        resident <= MAX_BYTES,
        "联想表常驻 {resident} 字节，超过 {MAX_BYTES}"
    );
    assert!(t.len() > 10_000, "联想表只有 {} 行，像是生成坏了", t.len());
}

#[test]
fn exact_and_suffix_lookup() {
    let t = table();
    assert_eq!(t.len(), 3);
    assert_eq!(
        t.lookup("今天").unwrap(),
        ("今天", vec!["$", "是", "我", "晚上"])
    );
    // 整句上屏：取末尾在表里的那段
    assert_eq!(t.lookup("我们今天").unwrap().0, "今天");
    assert_eq!(t.lookup("你和我").unwrap(), ("我", vec!["的", "$", "不"]));
    assert!(t.lookup("中").is_none());
    assert!(t.lookup("").is_none());
}

#[test]
fn borrows_from_preceding_text() {
    // parse 要求按上文字节序升序，这里排好再拼
    let mut rows = [
        ("今天", "是 晚上"),
        ("天下", "第一"),
        ("下", "午 面"),
        ("天", "气"),
        ("中国人", "民"),
        ("国人", "的"),
        ("中国", "人"),
    ];
    rows.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let text: String = rows.iter().map(|(c, s)| format!("{c}\t{s}\n")).collect();
    let t = AssocBigram::parse(text).unwrap();
    // 逐字上屏「今」「天」：借「今」凑成「今天」
    assert_eq!(
        t.lookup_after("我们今", "天", false).unwrap(),
        vec!["是", "晚上"]
    );
    // 「今天」已成词，「天」不借给「下」
    assert_eq!(
        t.lookup_after("今天", "下", false).unwrap(),
        vec!["午", "面"]
    );
    // 借得多的先试：「中国人」而不是「国人」
    assert_eq!(t.lookup_after("中国", "人", false).unwrap(), vec!["民"]);
    // 没有前文 = 只看上屏的
    assert_eq!(t.lookup_after("", "天", false).unwrap(), vec!["气"]);
    assert!(t.lookup_after("今天", "鬻", false).is_none());
}

#[test]
fn char_wise_rows_for_single_chars() {
    // 「~」(0x7E) 排在汉字前面
    let t = AssocBigram::parse("~我\t们 的\n我\t的 是\n我们\t的\n".into()).unwrap();
    assert_eq!(t.lookup_after("", "我", true).unwrap(), vec!["们", "的"]);
    assert_eq!(t.lookup_after("", "我", false).unwrap(), vec!["的", "是"]);
    // 多字上屏不走逐字行
    assert_eq!(t.lookup_after("", "我们", true).unwrap(), vec!["的"]);
    // 前文里真有个「~」也不会借成逐字行的键
    assert_eq!(t.lookup_after("a~", "我", false).unwrap(), vec!["的", "是"]);
}

#[test]
fn unsorted_input_is_rejected() {
    assert!(AssocBigram::parse("我\t的\n今天\t是\n".into()).is_err());
}

#[test]
fn crlf_and_blank_lines_are_tolerated() {
    let t = AssocBigram::parse("今天\t是 我\r\n\n我\t的\r\n".into()).unwrap();
    assert_eq!(t.lookup("今天").unwrap().1, vec!["是", "我"]);
    assert_eq!(t.lookup("我").unwrap().1, vec!["的"]);
}
