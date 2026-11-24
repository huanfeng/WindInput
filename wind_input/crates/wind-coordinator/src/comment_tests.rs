use super::*;

/// 求值闭包：按名取值；名字不在表里即「未知变量」。参数一律忽略。
fn ev<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str, Option<&str>) -> Option<String> + 'a {
    move |n, _arg| {
        pairs
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.to_string())
    }
}

/// 求值闭包：把收到的「名字 + 参数」原样回显，供参数语法的用例断言解析结果。
fn echo_arg() -> impl Fn(&str, Option<&str>) -> Option<String> {
    |n, arg| Some(format!("{n}<{}>", arg.unwrap_or("∅")))
}

// ---------------- 出厂零回归 ----------------

/// 出厂模板 `${code_hint|code}` 必须与模板化前的硬编码行为逐字节一致：引擎产的剩余编码
/// 优先，为空则回退到主码表反查码，两者皆空则不显示。存量用户升级后不该看到变化。
#[test]
fn default_template_reproduces_legacy_behavior() {
    const T: &str = "${code_hint|code_rev|shuangpin}";
    // ① 引擎给了剩余码 → 用它（此时反查码即便存在也不参与，旧逻辑正是 if/else if）
    assert_eq!(
        render(
            T,
            0,
            ev(&[
                ("code_hint", "kao"),
                ("code_rev", "wq"),
                ("shuangpin", "nihc")
            ])
        ),
        "kao"
    );
    // ② 引擎没给 → 回退反查码
    assert_eq!(
        render(T, 0, ev(&[("code_hint", ""), ("code_rev", "wq")])),
        "wq"
    );
    // ③ 都没有 → 不显示。
    // 三个变量都要列出来：`ev()` 对未列出的名字返回 `None`（未知变量名），渲染层会原样
    // 回显 ${shuangpin}，断言拿到的就是字面文本——那是夹具没喂全，不是真机行为。
    assert_eq!(
        render(
            T,
            0,
            ev(&[("code_hint", ""), ("code_rev", ""), ("shuangpin", "")])
        ),
        ""
    );

    // 第三段 `shuangpin` 不打扰老用户：前两段任一非空它就不参与。
    assert_eq!(
        render(
            T,
            0,
            ev(&[("code_hint", ""), ("code_rev", "wq"), ("shuangpin", "nihc")])
        ),
        "wq",
        "有反查码时不该被击键码顶掉"
    );
    // ④ 前两段皆空时才轮到它（没配主码表的双拼用户），填的是原本的空白。
    assert_eq!(
        render(
            T,
            0,
            ev(&[("code_hint", ""), ("code_rev", ""), ("shuangpin", "nihc")])
        ),
        "nihc"
    );
}

// ---------------- 语法 ----------------

#[test]
fn plain_variable_and_literal_text() {
    assert_eq!(
        render("${pinyin}", 0, ev(&[("pinyin", "nǐ hǎo")])),
        "nǐ hǎo"
    );
    assert_eq!(
        render("拼:${pinyin}", 0, ev(&[("pinyin", "nǐ hǎo")])),
        "拼:nǐ hǎo"
    );
}

/// 整个模板是隐式可选段：变量全空时连字面文本一起不显示，
/// 否则会剩下一个孤零零的 `拼:`。
#[test]
fn all_vars_empty_hides_literal_text_too() {
    assert_eq!(render("拼:${pinyin}", 0, ev(&[("pinyin", "")])), "");
    assert_eq!(render("(${a} ${b})", 0, ev(&[("a", ""), ("b", "")])), "");
}

/// 无变量的纯文本模板同样不显示 —— 没有任何变量被填上，按隐式段规则整体消失。
/// （想固定显示一段文字不是注释段的用途，那属于主题。）
#[test]
fn literal_only_template_shows_nothing() {
    assert_eq!(render("拼音", 0, ev(&[])), "");
}

#[test]
fn fallback_takes_first_non_empty() {
    let e = ev(&[("a", ""), ("b", ""), ("c", "C")]);
    assert_eq!(render("${a|b|c}", 0, &e), "C");
    assert_eq!(render("${a|b}", 0, &e), "");
}

/// 可选段：段内变量全空则整段消失，含段内的装饰字符。
///
/// 这是 `{}` 存在的理由：否则 `${code}{ (${pinyin})}` 在拼音为空时显示 `wq ()`，
/// 空括号靠 trim / 折叠空白删不掉。
#[test]
fn optional_group_vanishes_when_all_its_vars_empty() {
    const T: &str = "${code}{ (${pinyin})}";
    assert_eq!(
        render(T, 0, ev(&[("code", "wq"), ("pinyin", "nǐ hǎo")])),
        "wq (nǐ hǎo)"
    );
    assert_eq!(
        render(T, 0, ev(&[("code", "wq"), ("pinyin", "")])),
        "wq",
        "拼音为空时整段消失，不得留下 `()`"
    );
}

/// 段内只要有一个变量非空，整段保留（空的那个按空串替换）。
#[test]
fn group_survives_if_any_var_filled() {
    const T: &str = "{(拼: ${pinyin} ${chaizi})}";
    assert_eq!(
        render(T, 0, ev(&[("pinyin", "nǐ hǎo"), ("chaizi", "女子")])),
        "(拼: nǐ hǎo 女子)"
    );
    assert_eq!(
        render(T, 0, ev(&[("pinyin", "nǐ hǎo"), ("chaizi", "")])),
        "(拼: nǐ hǎo)",
        "空变量须吞掉紧邻空白，否则是 `(拼: nǐ hǎo )`"
    );
    assert_eq!(render(T, 0, ev(&[("pinyin", ""), ("chaizi", "")])), "");
}

/// 空变量只吞一个紧邻空白——吞到底会把用户有意排的版式抹平。
#[test]
fn empty_var_eats_exactly_one_space() {
    assert_eq!(
        render("${a}   ${b}", 0, ev(&[("a", "A"), ("b", "")])),
        "A",
        "尾部空白由 trim 收拾"
    );
    assert_eq!(
        render("${a}   ${b}!", 0, ev(&[("a", "A"), ("b", "")])),
        "A  !",
        "只吞一个，其余留给用户的版式"
    );
}

/// 段内变量的 `}` 不得被误当作段结束符——扫描段边界时必须跳过 `${…}`。
/// 写错会让 `{(${pinyin})}` 解析成段 `(${pinyin`，后面 `)}` 变字面文本。
#[test]
fn group_scan_skips_variable_braces() {
    assert_eq!(
        render("{[${a}]}", 0, ev(&[("a", "X")])),
        "[X]",
        "段内变量的右花括号不是段结束"
    );
    assert_eq!(render("{[${a}]}", 0, ev(&[("a", "")])), "");
}

/// 可选段可嵌套：内段只管自己的变量，外段在内外任一变量非空时保留。
/// 悬停提示「拆字 / 拼音」合并段的模板 `{${chaizi}{ [${chaizi_code}]}\t}` 就是这个形态；
/// 不配对的话内段会吞掉外段的右括号，`\t}` 沦为字面文本。
#[test]
fn groups_nest() {
    const T: &str = "{${a}{ [${b}]}|}${c}";
    let r = |a, b, c| render(T, 0, ev(&[("a", a), ("b", b), ("c", c)]));
    assert_eq!(r("A", "B", "C"), "A [B]|C");
    assert_eq!(r("A", "", "C"), "A|C", "内段消失不牵连外段");
    assert_eq!(r("", "", "C"), "C", "内外全空整段消失");
    assert_eq!(r("", "B", "C"), "[B]|C", "内段有值即撑住外段");
    // 未闭合的内段：外段配不上对，退化为字面文本，后面的段照常解析。
    assert_eq!(render("{x{${a}}", 0, ev(&[("a", "A")])), "{xA");
}

/// 计数谓词：不计数的变量照常输出，但撑不起整个模板或可选段。
#[test]
fn uncounted_var_renders_but_does_not_fill() {
    let e = ev(&[("char", "好"), ("r", "")]);
    let t = Template::parse("${char}：${r}");
    assert_eq!(t.render(&e, &|n| n != "char"), ("好：".to_string(), false));
    assert_eq!(t.render(&e, &|_| true), ("好：".to_string(), true));
    // 不 trim：首尾空白是内容（悬停提示的原始行要逐字节还原）。
    let lead = Template::parse("\t${char} ");
    assert_eq!(lead.render(&e, &|_| true).0, "\t好 ");
    assert!(lead.has_literal('\t'));
    assert!(
        !Template::parse("${char}").has_literal('\t'),
        "变量值不算字面"
    );
}

// ---------------- 变量参数 `${name:arg}` ----------------

/// 参数不 trim：`${chaizi_all: · }` 里那两个空格正是用户要的分隔符。
///
/// 名字仍 trim（`${ pinyin }` 这种手滑要认）：名字的空白是手滑，参数的空白是内容。
#[test]
fn variable_arg_preserves_whitespace_but_name_is_trimmed() {
    assert_eq!(
        render("${chaizi_all: · }", 0, echo_arg()),
        "chaizi_all< · >"
    );
    assert_eq!(render("${ pinyin }", 0, echo_arg()), "pinyin<∅>");
}

/// 只切第一个冒号——分隔符本身可以含冒号。
#[test]
fn variable_arg_splits_on_first_colon_only() {
    assert_eq!(render("${x:a:b}", 0, echo_arg()), "x<a:b>");
}

/// 参数可为空串（`${x:}`）：与「无参数」区分开，前者是「显式要求不加分隔」。
#[test]
fn empty_arg_differs_from_absent_arg() {
    assert_eq!(render("${x:}", 0, echo_arg()), "x<>");
    assert_eq!(render("${x}", 0, echo_arg()), "x<∅>");
}

/// 参数与回退链共存：每一段各自解析自己的参数。
#[test]
fn arg_works_inside_fallback_chain() {
    let e = |n: &str, arg: Option<&str>| match n {
        "a" => Some(String::new()), // 已知但空 → 回退
        "b" => Some(format!("B<{}>", arg.unwrap_or("∅"))),
        _ => None,
    };
    assert_eq!(render("${a:x|b:y}", 0, e), "B<y>");
}

/// 未知变量名原样回显并计作已填充 —— 拼错一定看得见。
///
/// 若当空处理，用户只看到「配了没反应」。
#[test]
fn unknown_variable_is_echoed_verbatim() {
    assert_eq!(
        render("${pinyn}", 0, ev(&[("pinyin", "nǐ hǎo")])),
        "${pinyn}"
    );
    // 回退链里遇到未知名即停（它是错误提示，不是"空值"，不该被跳过）。
    assert_eq!(
        render("${nope|pinyin}", 0, ev(&[("pinyin", "X")])),
        "${nope}"
    );
}

/// 语法写坏不 panic、不吞内容：未闭合的 `${` / `{` 退化成字面文本。
/// 但字面文本里没有变量被填上 ⇒ 按隐式段规则整体不显示。
#[test]
fn malformed_template_degrades_to_literal_text() {
    assert_eq!(render("${unclosed", 0, ev(&[])), "");
    assert_eq!(render("{unclosed", 0, ev(&[])), "");
    // 与真变量混排时，字面部分原样保留。
    assert_eq!(render("${a} ${bad", 0, ev(&[("a", "A")])), "A ${bad");
}

/// 中文字面文本按字符推进，不得在 UTF-8 中间切开（切开会 panic）。
#[test]
fn multibyte_literal_text_is_safe() {
    assert_eq!(
        render("【读音】${pinyin}（完）", 0, ev(&[("pinyin", "nǐ")])),
        "【读音】nǐ（完）"
    );
}

// ---------------- 截断 ----------------

#[test]
fn max_chars_truncates_with_ellipsis() {
    let e = ev(&[("a", "zhōng guó rén")]);
    assert_eq!(render("${a}", 0, &e), "zhōng guó rén", "0 = 不限");
    assert_eq!(render("${a}", 5, &e), "zhōng…");
    // 按字符而非字节计——带声调字母是多字节，按字节会截在半个字符上。
    assert_eq!(render("${a}", 100, &e), "zhōng guó rén");
}

// ---------------- 变量求值：拼音 / 拆字 ----------------

/// 边界真值切分：`你好` 的 `nihao` + `0b101`（音节起于字节 0 和 2）→ `["ni","hao"]`。
#[test]
fn boundary_splits_syllables() {
    assert_eq!(syllables_of("nihao", 0b101), vec!["ni", "hao"]);
    // 单音节的 boundary 是 0b1（「整串是一个音节」是真信息）。
    assert_eq!(syllables_of("hao", 0b1), vec!["hao"]);
    assert_eq!(
        syllables_of("zhongguoren", 1 | 1 << 5 | 1 << 8),
        vec!["zhong", "guo", "ren"]
    );
}

/// `boundary == 0` 的语义是「无边界信息」而非「单音节」：要降级到推断路径，
/// 否则五笔码 `wqvb` 会被当作一个拼音音节送去筛读音表。
#[test]
fn zero_boundary_falls_through_to_inference() {
    let c = Candidate {
        text: "你好".into(),
        code: "nihao".into(),
        boundary: 0, // ← 要害
        source: CandidateSource::Pinyin,
        ..Default::default()
    };
    assert_eq!(
        pinyin_text(
            &c,
            |t| {
                assert_eq!(t, "你好");
                "ni hao".to_string()
            },
            |_, syls| {
                assert_eq!(
                    syls,
                    Some(&["ni", "hao"][..]),
                    "boundary=0 应走推断，而非把 code 整串当一个音节"
                );
                "nǐ hǎo".to_string()
            }
        ),
        "nǐ hǎo"
    );
}

/// 非拼音来源（五笔候选）走引擎按词推断，而不是直接退到逐字首音——后者会把「行长」
/// 注成 `xíng cháng`。断言落在有没有把推断结果当音节传下去上。
///
/// 顺带钉住：即便 `boundary` 恰好有值也不得当拼音边界用（那是别的编码域的字段值）。
#[test]
fn non_pinyin_candidate_infers_syllables_instead_of_first_reading() {
    let c = Candidate {
        text: "行长".into(),
        code: "tfta".into(), // 五笔码
        boundary: 0b101,     // ← 有值，但不属于拼音域
        source: CandidateSource::CodeTable,
        ..Default::default()
    };
    assert_eq!(
        pinyin_text(
            &c,
            |t| {
                assert_eq!(t, "行长", "推断应按候选文本而非编码");
                "hang zhang".to_string()
            },
            |t, syls| {
                assert_eq!(t, "行长");
                assert_eq!(
                    syls,
                    Some(&["hang", "zhang"][..]),
                    "推断出的音节必须传下去消歧，否则「行长」会显示成 xíng cháng"
                );
                "háng zhǎng".to_string()
            }
        ),
        "háng zhǎng"
    );
}

/// 推断失败（含非汉字、生僻多音字超组合数护栏）→ 交由查表层逐字取最常用读音。
/// 这是最后的兜底，不是常态路径。
#[test]
fn failed_inference_falls_back_to_per_char_readings() {
    let c = Candidate {
        text: "你好".into(),
        source: CandidateSource::CodeTable,
        ..Default::default()
    };
    assert_eq!(
        pinyin_text(
            &c,
            |_| String::new(), // 推断失败
            |_, syls| {
                assert!(syls.is_none(), "推断失败时不得传入空音节序列");
                "nǐ hǎo".to_string()
            }
        ),
        "nǐ hǎo"
    );
}

/// 拼音来源候选优先用词条真值，不走推断。
///
/// 词条自带的 `code`+`boundary` 比枚举笛卡尔积回查词典更可靠也更省。
/// 闭包里 panic 是断言手段：一旦实现改成无条件先推断，会以「不该被调用」失败。
#[test]
fn pinyin_candidate_prefers_entry_truth_over_inference() {
    let c = Candidate {
        text: "行长".into(),
        code: "hangzhang".into(),
        boundary: 1 | 1 << 4, // hang|zhang
        source: CandidateSource::Pinyin,
        ..Default::default()
    };
    assert_eq!(
        pinyin_text(
            &c,
            |_| unreachable!("拼音来源候选不得走推断"),
            |_, syls| {
                assert_eq!(
                    syls,
                    Some(&["hang", "zhang"][..]),
                    "词条音节须原样传下去消歧"
                );
                "háng zhǎng".to_string()
            }
        ),
        "háng zhǎng"
    );
}
