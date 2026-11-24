use super::*;

fn ctx(text: &str) -> AssocContext<'_> {
    AssocContext {
        text,
        preceding: "",
        char_wise: false,
        boundary_broken: false,
    }
}

#[test]
fn tail_kinds() {
    assert_eq!(tail_kind("你好"), TailKind::Han);
    assert_eq!(tail_kind("2026"), TailKind::Digit);
    assert_eq!(tail_kind("１２"), TailKind::Digit, "全角数字同样算数字");
    assert_eq!(tail_kind("你好，"), TailKind::Punct);
    assert_eq!(tail_kind("hello"), TailKind::Other);
    assert_eq!(tail_kind(""), TailKind::Other);
}

#[test]
fn han_tail_suggests_punctuation() {
    let out = PunctRules.suggest(&ctx("你好"), 3);
    let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(texts, ["，", "。", "、"], "逗号最优先");
}

/// ★ 数字后推荐单位而非标点——移动端体感差异最大的一处。
#[test]
fn digit_tail_suggests_units() {
    let out = PunctRules.suggest(&ctx("2026"), 3);
    let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(texts, ["年", "月", "日"]);
}

/// 语气词结尾：句末标点提到最前，其余顺序不变。
#[test]
fn particle_tail_puts_its_punct_first() {
    let texts = |t| {
        PunctRules
            .suggest(&ctx(t), 3)
            .into_iter()
            .map(|h| h.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(texts("好吗"), ["？", "，", "。"]);
    assert_eq!(texts("好啊"), ["！", "，", "。"]);
    assert_eq!(texts("走吧"), ["。", "，", "、"]);
    assert_eq!(texts("你好"), ["，", "。", "、"], "非语气词不变");
}

/// 标点后不再联想，否则会出现「。」后面接「，」。
#[test]
fn punct_tail_yields_nothing() {
    assert!(PunctRules.suggest(&ctx("你好。"), 5).is_empty());
    assert!(PunctRules.suggest(&ctx("你好,"), 5).is_empty(), "半角同样");
}

/// 英文结尾不插手——英文有自己的输入习惯。
#[test]
fn latin_tail_yields_nothing() {
    assert!(PunctRules.suggest(&ctx("hello"), 5).is_empty());
    assert!(
        PunctRules.suggest(&ctx("你好 "), 5).is_empty(),
        "空白结尾同样"
    );
}

#[test]
fn respects_limit() {
    assert_eq!(PunctRules.suggest(&ctx("你好"), 2).len(), 2);
    assert_eq!(PunctRules.suggest(&ctx("你好"), 0).len(), 0);
    assert_eq!(
        PunctRules.suggest(&ctx("你好"), 99).len(),
        AFTER_HAN.len(),
        "limit 超过表长时给全部，不越界"
    );
}

#[test]
fn score_is_descending_in_table_order() {
    let out = PunctRules.suggest(&ctx("你好"), 4);
    for w in out.windows(2) {
        assert!(w[0].score > w[1].score, "源内分数须严格降序");
    }
}
