use super::*;

fn sec(title: Option<&str>, inline: bool, lines: &[&str]) -> TooltipSection {
    TooltipSection {
        title: title.map(StyledText::from),
        inline,
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, t)| TooltipLine {
                text: (*t).into(),
                raw: i as u16,
            })
            .collect(),
    }
}

#[test]
fn plain_text_formats_titles_inline_and_untitled_sections() {
    let doc = TooltipDoc {
        sections: vec![
            sec(Some("编码"), false, &["vbg"]),
            sec(Some("Unicode"), true, &["U+597D"]),
            sec(Some("拼音"), false, &["好：hǎo", "人：rén"]),
            sec(Some("原文"), true, &["折成", "两行"]),
            sec(None, false, &["无标题"]),
        ],
    };
    assert_eq!(
        doc.to_plain_text(),
        "[编码]\nvbg\nUnicode: U+597D\n[拼音]\n好：hǎo\n人：rén\n原文: 折成\n两行\n无标题",
        "inline 段标题接第一条显示行，其余显示行照常另起"
    );
    assert_eq!(TooltipDoc::default().to_plain_text(), "");
}

/// 命中换算与纯文本同一份排列：标题行 → raw None；inline 首行 → 第一条原始行；
/// 折行产生的多条显示行 → 同一条原始行。
#[test]
fn hit_at_line_follows_plain_text_layout() {
    let mut wrapped = sec(Some("原文"), false, &["折成", "两行"]);
    wrapped.lines[1].raw = 0;
    let doc = TooltipDoc {
        sections: vec![
            sec(Some("编码"), false, &["vbg"]),
            sec(Some("Unicode"), true, &["U+597D"]),
            wrapped,
        ],
    };
    let h = |section, raw_line| Some(TooltipHit { section, raw_line });
    assert_eq!(doc.hit_at_line(0), h(0, None));
    assert_eq!(doc.hit_at_line(1), h(0, Some(0)));
    assert_eq!(doc.hit_at_line(2), h(1, Some(0)));
    assert_eq!(doc.hit_at_line(3), h(2, None));
    assert_eq!(doc.hit_at_line(4), h(2, Some(0)));
    assert_eq!(
        doc.hit_at_line(5),
        h(2, Some(0)),
        "折行的第二条显示行指回同一原始行"
    );
    assert_eq!(doc.hit_at_line(6), None);
    assert_eq!(doc.to_plain_text().lines().count(), 6);
}

#[test]
fn fingerprint_tracks_content() {
    let a = TooltipDoc {
        sections: vec![sec(Some("拼音"), false, &["你：nǐ"])],
    };
    let mut b = a.clone();
    assert_eq!(a.fingerprint(), b.fingerprint());
    b.sections.insert(0, sec(Some("编码"), false, &["wqvb"]));
    assert_ne!(a.fingerprint(), b.fingerprint(), "前面插入一段即不同");
}

/// 只改颜色（片段样式）时指纹不变、`PartialEq` 却不等；改 raw 下标则指纹变。
#[test]
fn fingerprint_ignores_styles_but_not_raw() {
    let a = TooltipDoc {
        sections: vec![sec(Some("拼音"), false, &["你：nǐ"])],
    };
    let mut styled = a.clone();
    let mut t = StyledText::new();
    t.push(
        "你",
        &SpanStyle {
            role: Some("char"),
            ..Default::default()
        },
    );
    t.push("：nǐ", &SpanStyle::default());
    styled.sections[0].lines[0].text = t;
    assert_eq!(a.fingerprint(), styled.fingerprint(), "只改颜色，指纹不变");
    assert_ne!(a, styled, "含样式的判等要能看出不同");
    let mut raw = a.clone();
    raw.sections[0].lines[0].raw = 3;
    assert_ne!(a.fingerprint(), raw.fingerprint());
}

/// 段名装饰 `[` `]`、inline 的 `: ` 带 title 角色；样式整块与纯文本同一排列。
#[test]
fn styled_decorations_are_title() {
    let doc = TooltipDoc {
        sections: vec![
            sec(Some("编码"), false, &["vbg"]),
            sec(Some("U"), true, &["x"]),
        ],
    };
    let s = doc.to_styled();
    assert_eq!(s.as_str(), doc.to_plain_text());
    let roles: Vec<(u32, u32, Option<&str>)> = s
        .spans()
        .iter()
        .map(|sp| (sp.start, sp.end, sp.role))
        .collect();
    // "[编码]\nvbg\nU: x"：[ 在 0..1，] 在 7..8，": " 在 14..16。
    assert_eq!(
        roles,
        vec![
            (0, 1, Some("title")),
            (7, 8, Some("title")),
            (14, 16, Some("title"))
        ]
    );
}
