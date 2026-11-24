use super::role_of;

/// 从 `from` 起第一个 `{` 开始、按花括号配对截出函数 / 闭包体（跳过字符串字面量）。
fn body_after<'a>(src: &'a str, anchor: &str) -> &'a str {
    let at = src
        .find(anchor)
        .unwrap_or_else(|| panic!("源码里找不到 {anchor:?}——入口改名了？同步改这条测试"));
    let b = src.as_bytes();
    let open = at + src[at..].find('{').expect("入口后应有函数体");
    let (mut depth, mut i, mut in_str) = (0usize, open, false);
    while i < b.len() {
        match (in_str, b[i]) {
            (true, b'\\') => i += 1,
            (true, b'"') => in_str = false,
            (false, b'"') => in_str = true,
            (false, b'{') => depth += 1,
            (false, b'}') => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..=i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("{anchor:?} 的函数体没闭合");
}

/// 抽出 `"名字"` 后紧跟 `=>`、`|`、`if` 的变量名（match 分支的形态）。
fn arm_names(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(q) = rest.find('"') {
        let after = &rest[q + 1..];
        let Some(end) = after.find('"') else { break };
        let name = &after[..end];
        let tail = after[end + 1..].trim_start();
        let is_arm = tail.starts_with("=>")
            || (tail.starts_with('|') && !tail.starts_with("||"))
            || tail.starts_with("if ");
        if is_arm && !name.is_empty() && name.bytes().all(|c| c.is_ascii_lowercase() || c == b'_') {
            out.push(name.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

fn entry_names(extra: Option<(&str, &str)>) -> Vec<String> {
    let comment = include_str!("comment.rs");
    let tooltip = include_str!("tooltip.rs");
    let coordinator = include_str!("coordinator.rs");
    let mut bodies = vec![
        body_after(comment, "pub(crate) fn eval_var("),
        body_after(comment, "pub(crate) fn eval_text_var_ex("),
        body_after(comment, "pub(crate) fn reverse_text_var("),
        body_after(comment, "pub(crate) fn role_of("),
        body_after(tooltip, "pub(crate) fn char_var("),
        body_after(tooltip, "let cand_eval = |name: &str, arg: Option<&str>|"),
        body_after(
            coordinator,
            "let cand_eval = |name: &str, arg: Option<&str>|",
        ),
    ];
    if let Some((src, anchor)) = extra {
        bodies.push(body_after(src, anchor));
    }
    bodies.into_iter().flat_map(arm_names).collect()
}

fn unlisted(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = names
        .iter()
        .map(|n| role_of(n).to_string())
        .filter(|r| !wind_ui_types::TEXT_ROLES.contains(&r.as_str()))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 各入口旁的变量名常量表与入口的 match 分支逐一相符（设置页预览按它们判场景可用性）。
#[test]
fn entry_name_tables_match_their_bodies() {
    let comment = include_str!("comment.rs");
    let tooltip = include_str!("tooltip.rs");
    let coordinator = include_str!("coordinator.rs");
    let cases: [(&str, &str, &[&str]); 6] = [
        (comment, "pub(crate) fn eval_var(", super::EVAL_VAR_NAMES),
        (
            comment,
            "pub(crate) fn eval_text_var_ex(",
            super::EVAL_TEXT_VAR_NAMES,
        ),
        (
            comment,
            "pub(crate) fn reverse_text_var(",
            super::REVERSE_TEXT_VAR_NAMES,
        ),
        (
            tooltip,
            "pub(crate) fn char_var(",
            crate::tooltip::CHAR_VAR_NAMES,
        ),
        (
            tooltip,
            "let cand_eval = |name: &str, arg: Option<&str>|",
            crate::tooltip::TOOLTIP_CAND_VAR_NAMES,
        ),
        (
            coordinator,
            "let cand_eval = |name: &str, arg: Option<&str>|",
            crate::tooltip::TOOLTIP_COORD_VAR_NAMES,
        ),
    ];
    for (src, anchor, table) in cases {
        let mut got = arm_names(body_after(src, anchor));
        got.sort();
        got.dedup();
        let mut want: Vec<String> = table.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want, "{anchor} 的分支与常量表对不上");
    }
}

#[test]
fn every_evaluable_variable_is_a_listed_role() {
    let names = entry_names(None);
    // 防空转：几个确定存在的变量必须被扫到。
    for must in [
        "code_hint",
        "readings",
        "word_code",
        "full_text",
        "char",
        "code",
    ] {
        assert!(
            names.iter().any(|n| n == must),
            "扫描漏了 {must}：{names:?}"
        );
    }
    assert_eq!(
        unlisted(&names),
        Vec::<String>::new(),
        "这些变量能求值却不在 TEXT_ROLES 里——主题给它们配的角色色会静默不生效"
    );
}

/// 扫描确实抓得住：往一个 match 里塞个清单外的名字就报出来。
#[test]
fn scan_catches_an_unlisted_arm() {
    const FAKE: &str = r#"fn fake(name: &str) -> Option<String> {
            Some(match name { "pinyin" => x(), "no_such_role" if y => z(), _ => return None })
        }"#;
    let names = entry_names(Some((FAKE, "fn fake(")));
    assert_eq!(unlisted(&names), vec!["no_such_role".to_string()]);
}
