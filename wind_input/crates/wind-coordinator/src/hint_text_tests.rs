use super::*;

fn fh(i: u16, s: &str) -> PinyinHint {
    PinyinHint {
        char_index: i,
        correct: s.into(),
        kind: HintKind::Fuzzy,
    }
}

fn segs(t: &StyledText) -> Vec<(&str, Option<&'static str>)> {
    let s = t.as_str();
    let mut out = Vec::new();
    let mut at = 0;
    for sp in t.spans() {
        let (a, b) = (sp.start as usize, sp.end as usize);
        if at < a {
            out.push((&s[at..a], None));
        }
        out.push((&s[a..b], sp.role));
        at = b;
    }
    if at < s.len() {
        out.push((&s[at..], None));
    }
    out
}

/// `${correct_pinyin}` 用引擎给的整词音节，不看候选码边界：`boundary == 0`（用户手输码）
/// 也出值；音节数对不上字数则空。
#[test]
fn correct_pinyin_text_uses_engine_syllables() {
    let mut c = Candidate {
        text: "中国".into(),
        code: "zongguo".into(),
        ..Default::default()
    };
    c.meta.learn_code = Some(("zhongguo".into(), 0));
    c.meta.pinyin_hints = vec![fh(0, "zhong")];
    c.meta.hint_syllables = vec!["zhong".into(), "guo".into()];
    let h = PinyinHintConfig::default();
    assert_eq!(correct_pinyin_text(&c, &h, None), "zhong guo");
    c.meta.hint_syllables.pop();
    assert_eq!(correct_pinyin_text(&c, &h, None), "");
    c.meta.pinyin_hints.clear();
    c.meta.hint_syllables = vec!["zhong".into(), "guo".into()];
    assert_eq!(correct_pinyin_text(&c, &h, None), "", "无提示即空");
}

fn kh(i: u16, s: &str, kind: HintKind) -> PinyinHint {
    PinyinHint {
        char_index: i,
        correct: s.into(),
        kind,
    }
}

fn only(fuzzy: bool, misread: bool, correction: bool) -> PinyinHintConfig {
    PinyinHintConfig {
        fuzzy,
        misread,
        correction,
        ..Default::default()
    }
}

#[test]
fn hint_kind_enabled_follows_source_flags() {
    let h = only(false, true, false);
    assert!(!hint_kind_enabled(&h, HintKind::Fuzzy));
    assert!(hint_kind_enabled(&h, HintKind::Misread));
    assert!(!hint_kind_enabled(&h, HintKind::Correction));
    let h = only(true, false, true);
    assert!(hint_kind_enabled(&h, HintKind::Fuzzy));
    assert!(!hint_kind_enabled(&h, HintKind::Misread));
    assert!(hint_kind_enabled(&h, HintKind::Correction));
}

/// 出厂来源：模糊、误读勾，纠错不勾。
#[test]
fn default_sources_exclude_correction() {
    let h = PinyinHintConfig::default();
    assert!(hint_kind_enabled(&h, HintKind::Fuzzy));
    assert!(hint_kind_enabled(&h, HintKind::Misread));
    assert!(!hint_kind_enabled(&h, HintKind::Correction));
}

#[test]
fn enabled_hints_keeps_only_checked_sources() {
    let hints = [
        kh(0, "ping", HintKind::Fuzzy),
        kh(1, "guo", HintKind::Misread),
        kh(2, "gong", HintKind::Correction),
    ];
    assert_eq!(
        enabled_hints(&only(false, true, false), &hints),
        vec![kh(1, "guo", HintKind::Misread)]
    );
    assert_eq!(
        enabled_hints(&only(true, false, true), &hints),
        vec![
            kh(0, "ping", HintKind::Fuzzy),
            kh(2, "gong", HintKind::Correction)
        ]
    );
    assert!(enabled_hints(&only(false, false, false), &hints).is_empty());
}

/// 误读来源暂无产出者：手塞一条误读提示，`${correct_pinyin}` 随误读勾选出值 / 为空。
#[test]
fn correct_pinyin_text_filters_by_source() {
    let mut c = Candidate {
        text: "说服".into(),
        ..Default::default()
    };
    c.meta.pinyin_hints = vec![kh(1, "fu", HintKind::Misread)];
    c.meta.hint_syllables = vec!["shuo".into(), "fu".into()];
    assert_eq!(
        correct_pinyin_text(&c, &only(false, true, false), None),
        "shuo fu"
    );
    assert_eq!(correct_pinyin_text(&c, &only(true, false, true), None), "");
}

/// 误读提示内嵌：过滤后照常插入。
#[test]
fn misread_hint_inlines_when_checked() {
    let hints = [kh(1, "fu", HintKind::Misread)];
    let kept = enabled_hints(&only(false, true, false), &hints);
    assert_eq!(hint_styled_text("说服", &kept).as_str(), "说服(fu)");
    let kept = enabled_hints(&only(true, false, true), &hints);
    assert!(hint_styled_text("说服", &kept).is_empty());
}

/// 非模糊来源的 `kind` 在下标换算与带调替换里原样保留。
#[test]
fn non_fuzzy_kind_survives_remap_and_tone() {
    let hints = [
        kh(2, "gong", HintKind::Correction),
        kh(3, "ping", HintKind::Misread),
    ];
    assert_eq!(
        remap_hints("拼过公平", "公平", &hints),
        Some(vec![
            kh(0, "gong", HintKind::Correction),
            kh(1, "ping", HintKind::Misread)
        ])
    );
    let reverse = wind_reverse::ReverseLookup::default();
    assert_eq!(toned_hints("拼过公平", &hints, &reverse), hints.to_vec());
}

#[test]
fn hint_styled_text_inserts_after_char() {
    let t = hint_styled_text("苹果", &[fh(0, "ping")]);
    assert_eq!(t.as_str(), "苹(ping)果");
    assert_eq!(
        segs(&t),
        vec![("苹", None), ("(ping)", Some("pinyin_hint")), ("果", None)]
    );
}

#[test]
fn hint_styled_text_last_char_and_multiple() {
    let t = hint_styled_text("心情", &[fh(0, "xin"), fh(1, "qing")]);
    assert_eq!(t.as_str(), "心(xin)情(qing)");
    assert_eq!(t.spans().len(), 2);
}

#[test]
fn hint_styled_text_out_of_range_is_empty() {
    assert!(hint_styled_text("苹果", &[fh(2, "ping")]).is_empty());
    assert!(hint_styled_text("苹果", &[]).is_empty());
}

#[test]
fn remap_hints_simplified_to_traditional_same_len() {
    assert_eq!(
        remap_hints("苹果", "蘋果", &[fh(0, "ping")]),
        Some(vec![fh(0, "ping")])
    );
}

#[test]
fn remap_hints_len_change_drops() {
    assert_eq!(remap_hints("苹果", "苹果果", &[fh(0, "ping")]), None);
}

#[test]
fn remap_hints_suffix_display() {
    assert_eq!(
        remap_hints("拼过苹果", "苹果", &[fh(2, "ping")]),
        Some(vec![fh(0, "ping")])
    );
    assert_eq!(
        remap_hints("拼过苹果", "苹果", &[fh(0, "pin"), fh(2, "ping")]),
        Some(vec![fh(0, "ping")]),
        "落在被隐藏前段的提示丢弃"
    );
}

#[test]
fn styled_for_shown_shifts_by_prefix() {
    let t = styled_for_shown("苹果", "苹果", "*", &[fh(0, "ping")]);
    assert_eq!(t.as_str(), "*苹(ping)果");
}

#[test]
fn styled_for_shown_drops_truncated_hint() {
    let hints = [fh(0, "ping"), fh(2, "ping")];
    let t = styled_for_shown("苹果平过", "苹果…", "", &hints);
    assert_eq!(t.as_str(), "苹(ping)果…");
    // 被标的字全被截掉 ⇒ 不标
    let t = styled_for_shown("苹果平过", "苹果…", "", &[fh(2, "ping")]);
    assert!(t.is_empty());
}
