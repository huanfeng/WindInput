use super::*;

fn roles(t: &StyledText) -> Vec<(&str, Option<&str>, bool)> {
    t.spans()
        .iter()
        .map(|s| {
            (
                &t.as_str()[s.start as usize..s.end as usize],
                s.role,
                s.color.is_some(),
            )
        })
        .collect()
}

#[test]
fn comment_scene_renders_sample_with_roles() {
    let s = sample(
        "${code_hint|code_rev} $[accent]{(${pinyin})}",
        &TemplateScene::Comment,
    );
    assert_eq!(s.text.as_str(), "hao (nǐ hǎo)");
    assert_eq!(
        roles(&s.text),
        vec![
            ("hao", Some("code_hint"), false),
            (" ", Some("literal"), false),
            ("(", Some("literal"), true),
            ("nǐ hǎo", Some("pinyin"), true),
            (")", Some("literal"), true),
        ]
    );
    assert!(s.problems.is_empty());
}

/// 预览样例的分隔符同样带标记（设置页预览才看得出 separator 配色）；文字不变。
#[test]
fn sample_marks_separators() {
    for (tpl, text, sep) in [
        ("${chaizi_all:／}", "亻尔／女子", "／"),
        ("${chaizi_code_all}", "wqiy vbg", " "),
        ("${code_rev_all}", "wq/wqvb", "/"),
        ("${dict_all:}", "hello问候", ""),
        ("${dict_all}", "hello；问候", "；"),
    ] {
        let s = sample(tpl, &TemplateScene::Comment);
        assert_eq!(s.text.as_str(), text, "{tpl}");
        let got: Vec<&str> = s
            .text
            .spans()
            .iter()
            .filter(|sp| sp.separator)
            .map(|sp| &s.text.as_str()[sp.start as usize..sp.end as usize])
            .collect();
        let want: Vec<&str> = if sep.is_empty() { vec![] } else { vec![sep] };
        assert_eq!(got, want, "{tpl}");
    }
    let s = sample(
        "${unicode_all:,}",
        &TemplateScene::TooltipContent { each: "".into() },
    );
    assert_eq!(s.text.as_str(), "U+4F60,U+597D");
    assert_eq!(s.text.spans().iter().filter(|sp| sp.separator).count(), 1);
}

/// 分隔符参数里的内联色：样例照样上色、不报未知变量；颜色写法按模板坐标列出供诊断。
#[test]
fn sample_lists_separator_inline_color() {
    let tpl = "${chaizi_all:$[nope,hover=x]{／}}";
    let s = sample(tpl, &TemplateScene::Comment);
    assert_eq!(s.text.as_str(), "亻尔／女子");
    assert!(s.problems.is_empty(), "{:?}", s.problems);
    assert_eq!(s.colors.len(), 1);
    let c = &s.colors[0];
    assert_eq!(&tpl[c.start..c.end], "nope,hover=x");
    assert_eq!(c.notes.len(), 1, "未知 key= 同样提示");
    let sep = s.text.spans().iter().find(|sp| sp.separator).unwrap();
    assert!(sep.color.is_some());
}

#[test]
fn per_char_content_one_row_per_han() {
    let s = sample(
        "${char}：${readings}",
        &TemplateScene::TooltipContent { each: "han".into() },
    );
    assert_eq!(s.text.as_str(), "你：nǐ\n好：hǎo/hào");
    let one = sample(
        "${char}：${readings:1}",
        &TemplateScene::TooltipContent {
            each: "char".into(),
        },
    );
    assert_eq!(one.text.as_str(), "你：nǐ\n好：hǎo");
}

#[test]
fn label_scene_marks_title() {
    let s = sample("编码{(${code_source})}", &TemplateScene::TooltipLabel);
    assert_eq!(s.text.as_str(), "编码(五笔)");
    assert!(s.text.spans().iter().all(|sp| sp.in_title));
}

/// 问题区间是**模板**坐标：`${pinyn}` 在模板里的位置，而不是输出里回显的位置。
#[test]
fn unknown_variables_are_reported_in_template_offsets() {
    let tpl = "码:{${code_hint}} ${pinyn|dict}";
    let s = sample(tpl, &TemplateScene::Comment);
    assert_eq!(s.problems.len(), 1);
    let p = &s.problems[0];
    assert_eq!(&tpl[p.start..p.end], "${pinyn|dict}");
    assert!(p.message.contains("pinyn") && !p.message.contains("dict"));
    // 兼容别名不算未知。
    assert!(
        sample("${code}${code_all}", &TemplateScene::Comment)
            .problems
            .is_empty()
    );
}

/// `${dict_all}` 有样例值，分隔符参数生效（默认 `；`）。
#[test]
fn dict_all_sample_uses_separator() {
    let s = sample("${dict_all}", &TemplateScene::Comment);
    assert!(s.problems.is_empty());
    assert_eq!(s.text.as_str(), "hello；问候");
    let s = sample(
        "${dict_all:/}",
        &TemplateScene::TooltipContent { each: "".into() },
    );
    assert_eq!(s.text.as_str(), "hello/问候");
}

/// 内联色的位置（SPEC 区间，含嵌套在可选段 / 内联色里的）与未知 key。
#[test]
fn color_specs_with_positions_and_notes() {
    let tpl = "{a $[accent,hover=x]{b $[#GG]{c}}}";
    let s = sample(tpl, &TemplateScene::Comment);
    let got: Vec<(&str, Vec<String>)> = s
        .colors
        .iter()
        .map(|c| (&tpl[c.start..c.end], c.notes.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "accent,hover=x",
                vec!["不认识的「hover」，已忽略（目前只支持 selected=）".to_string()]
            ),
            ("#GG", vec![])
        ]
    );
    // 结构不成立的 `$[` 不算内联色。
    assert!(
        sample("$[accent${code_hint}", &TemplateScene::Comment)
            .colors
            .is_empty()
    );
}

/// ★ 变量按场景放行，与真实求值链一致：注释里没有气泡的候选级变量，整段里没有逐字变量。
/// 场景外的变量原样回显（真实候选栏 / 气泡里就是这样），并报「此处不可用」。
#[test]
fn variables_are_scoped_by_scene() {
    let whole = TemplateScene::TooltipContent { each: "".into() };
    let han = TemplateScene::TooltipContent { each: "han".into() };
    let unavailable = |tpl: &str, scene: &TemplateScene| {
        sample(tpl, scene)
            .problems
            .iter()
            .any(|p| p.message.starts_with("此处不可用"))
    };
    // 注释：气泡候选级变量不可用，原样回显。
    let s = sample("${word_code}", &TemplateScene::Comment);
    assert_eq!(s.text.as_str(), "${word_code}");
    assert!(unavailable("${word_code}", &TemplateScene::Comment));
    for v in [
        "${full_text}",
        "${debug}",
        "${readings}",
        "${char}",
        "${unicode}",
    ] {
        assert!(unavailable(v, &TemplateScene::Comment), "{v}");
    }
    // 段名 / 整段：候选级变量可用，逐字变量不可用。
    assert_eq!(
        sample("${word_code}", &TemplateScene::TooltipLabel)
            .text
            .as_str(),
        "wqvb"
    );
    assert!(!unavailable("${code_source}${unicode_all}", &whole));
    assert!(unavailable("${char}", &whole));
    assert!(unavailable("${readings}", &TemplateScene::TooltipLabel));
    // 逐字：全部可用。
    assert!(!unavailable(
        "${char}${readings}${unicode}${word_code}${code_hint}",
        &han
    ));
    // 注释里 eval_var 的变量照常可用。
    assert!(!unavailable(
        "${code_hint}${emoji}${dict}${dict_all}${code}",
        &TemplateScene::Comment
    ));
    // 未知变量仍按「未知」报，不混进「不可用」。
    let s = sample("${pinyn}", &TemplateScene::Comment);
    assert!(s.problems[0].message.starts_with("未知变量"));
    assert_eq!(s.problems.len(), 1);
}

/// 引擎忽略的 SPEC 片段逐条说明：没写 `=` 的 selected、多余逗号、未知 key。
#[test]
fn spec_notes_follow_engine_parse() {
    assert_eq!(
        spec_notes("accent,selected"),
        vec!["「selected」后面要写「=颜色」，已忽略"]
    );
    assert_eq!(spec_notes("accent,"), vec!["多余的逗号，已忽略"]);
    assert!(spec_notes("accent, selected = on_accent").is_empty());
    // 与引擎一致：没写 `=` 的 selected 不产生选中态颜色。
    assert!(InlineColor::parse("accent,selected").selected.is_none());
}

fn with(id: &str, tpl: &str, scene: &TemplateScene) -> String {
    let s = find_sample(Some(id)).unwrap();
    sample_with(tpl, scene, s).text.as_str().to_string()
}

/// 每个中文样例的关键变量：拼音、双拼、反查码、拆字、`${dict:zh_en}`、编码提示。
#[test]
fn chinese_samples_have_their_own_values() {
    let tpl = "${pinyin}|${shuangpin}|${code_rev}|${chaizi_all}|${dict:zh_en}|${code_hint}|${dict}";
    let c = TemplateScene::Comment;
    assert_eq!(
        with("nihao", tpl, &c),
        "nǐ hǎo|nihc|wqvb|亻尔 女子|int. hello|hao|hello"
    );
    assert_eq!(
        with("zhongwen", tpl, &c),
        "zhōng wén|vswf|khyy|口丨 亠乂|n. Chinese language|wen|n. Chinese language"
    );
    assert_eq!(
        with("shurufa", tpl, &c),
        "shū rù fǎ|uurufa|ltif|车俞 丿乀 氵去|n. input method|rufa|n. input method"
    );
    // 中文样例没有英汉库的值。
    assert_eq!(with("zhongwen", "${dict:en_zh}", &c), "");
    let han = TemplateScene::TooltipContent { each: "han".into() };
    assert_eq!(
        with("shurufa", "${char}：${readings} ${chaizi_code}", &han),
        "输：shū lwgj\n入：rù tyi\n法：fǎ ifcy"
    );
}

/// 英文样例：`${dict:en_zh}` 给「词性. 中文义」，中文变量为空、可选段整段消失。
#[test]
fn english_samples_hide_chinese_variables() {
    let c = TemplateScene::Comment;
    for (id, mean) in [
        ("apple", "n. 苹果"),
        ("hello", "int. 喂；你好"),
        ("world", "n. 世界"),
    ] {
        assert_eq!(with(id, "${dict:en_zh}", &c), mean);
        assert_eq!(with(id, "${dict}", &c), mean);
        assert_eq!(with(id, "${dict:zh_en}", &c), "");
        assert_eq!(
            with(
                id,
                "{(${pinyin})}{[${code_rev}]}{${shuangpin}}${chaizi_all}${code_hint}",
                &c
            ),
            ""
        );
        let han = TemplateScene::TooltipContent { each: "han".into() };
        assert_eq!(with(id, "${char}：${readings}", &han), "");
        let each = TemplateScene::TooltipContent {
            each: "char".into(),
        };
        assert_eq!(with(id, "${char}：${chaizi}", &each), "");
    }
}

/// 样例表：id 唯一、中英各至少三个、缺省是「你好」，未知 id 取不到。
#[test]
fn sample_table_shape() {
    assert_eq!(find_sample(None).unwrap().text, "你好");
    assert!(find_sample(Some("nope")).is_none());
    let n = |l| SAMPLES.iter().filter(|s| s.lang == l).count();
    assert!(n(SampleLang::Zh) >= 3 && n(SampleLang::En) >= 3);
    for (i, a) in SAMPLES.iter().enumerate() {
        assert!(SAMPLES[i + 1..].iter().all(|b| b.id != a.id), "{}", a.id);
        assert!(a.chars.iter().all(|c| a.text.contains(c.ch)), "{}", a.id);
    }
}

/// 样例表覆盖契约里的全部变量（否则预览会把合法变量报成未知）。
#[test]
fn every_listed_role_has_a_sample_value() {
    for r in wind_ui_types::TEXT_ROLES {
        if wind_ui_types::STRUCTURAL_ROLES.contains(r) {
            continue;
        }
        assert!(
            sample_value(&SAMPLES[0], r, None, Some('你')).is_some(),
            "{r}"
        );
    }
}
