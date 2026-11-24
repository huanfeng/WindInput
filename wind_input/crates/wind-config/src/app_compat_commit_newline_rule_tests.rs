use super::*;

fn rule(process: &str, style: NewlineStyle) -> CommitNewlineRule {
    CommitNewlineRule {
        process: process.into(),
        comment: String::new(),
        disabled: false,
        style: Some(style),
    }
}

/// 没配 = 跟随全局（`None`），不是「回落到 Keep」——两者在上层的处理不同。
#[test]
fn unlisted_process_follows_global() {
    let c = AppCompat::default().with_commit_newline(vec![rule("WINWORD.EXE", NewlineStyle::Cr)]);
    assert_eq!(c.commit_newline_for("winword.exe"), Some(NewlineStyle::Cr));
    assert_eq!(c.commit_newline_for("Code.exe"), None);
    assert_eq!(c.commit_newline_for(""), None, "空进程名不该命中任何规则");
}

/// 进程名大小写不敏感——两侧都要归一，只归一一侧是常见的半拉子实现。
#[test]
fn process_match_is_case_insensitive() {
    let c = AppCompat::default().with_commit_newline(vec![rule("WINWORD.EXE", NewlineStyle::Cr)]);
    assert_eq!(c.commit_newline_for("WinWord.Exe"), Some(NewlineStyle::Cr));
    let c = AppCompat::default().with_commit_newline(vec![rule("winword.exe", NewlineStyle::Cr)]);
    assert_eq!(c.commit_newline_for("WINWORD.EXE"), Some(NewlineStyle::Cr));
}

/// 拼错的 `style` 只让这一项回落「跟随全局」，不得牵连其它规则。同形态见
/// `unknown_mode_in_toml_degrades_to_follow_global`。
#[test]
fn unknown_style_in_toml_degrades_to_follow_global() {
    let toml = r#"
            [[apps]]
            process = "Foo.exe"
            caret_use_top = true

            [[commit_newline]]
            process = "Bar.exe"
            style = "rn"
        "#;
    let file: AppCompatFile = toml::from_str(toml).expect("style 拼错不得让整份文件解析失败");
    assert_eq!(file.commit_newline[0].style, None, "认不出 = 跟随全局");
    assert!(
        file.apps[0].caret_use_top,
        "别的段必须不受牵连——这正是整份静默跳过最致命的地方"
    );

    // 缺字段同理：只写 process、不写 style 也不能炸。
    let toml2 = r#"
            [[commit_newline]]
            process = "Baz.exe"
        "#;
    let file2: AppCompatFile = toml::from_str(toml2).expect("缺 style 不得解析失败");
    assert_eq!(file2.commit_newline[0].style, None);

    // 认不出/没写的条目进不了查找表，等于这条规则不存在。
    let c = AppCompat::default().with_commit_newline(file.commit_newline);
    assert_eq!(c.commit_newline_for("bar.exe"), None);
}
