//! 出厂剪贴板反查短语（`cofc`）的守门。
//!
//! **系统短语的语法错误用户零感知**：写错一个函数名或括号，`evaluate_phrase` 返回 Err，
//! 短语层只记一条 warn 就把整条丢掉——用户侧表现为「打了 cofc 什么都没有」，
//! 与「剪贴板是空的」完全同形，靠人是分不出来的。
//!
//! ⚠️ 读的是**仓库** `data/system.phrases.toml` 而不是 `build_dev/data/`：
//! 那份是部署产物，且在 worktree 里是指向主仓的符号链接（改动不会体现），
//! 拿它跑等于验主仓的旧文件。出厂词条的语法是源文件的属性，与部署无关。

use std::path::PathBuf;

fn repo_phrases() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data/system.phrases.toml")
}

fn layer() -> wind_phrase::PhraseLayer {
    let p = repo_phrases();
    assert!(p.is_file(), "找不到出厂短语文件：{}", p.display());
    wind_phrase::PhraseLayer::load(&p)
}

/// ★ `cofc` 必须能解析并求值出「剪贴板那个字的反查」。
///
/// 本条只管**查得到**的主路径：display 原样等于宿主返回的反查结果。
/// 「查不到时怎么办」由下一条负责。
#[test]
fn factory_cofc_parses_and_renders_clipboard_lookup() {
    let clip = |_n: i64| "好人".to_string();
    // 只认第 1 个字，模拟真实反查（第 2 个字不该被这条词条查到）。
    let reverse = |text: &str, fmt: &str| {
        assert_eq!(text, "好", "出厂词条应只查剪贴板第 1 个字");
        // ★ 必须是 `code_rev_all`（全部码位）而不是 `code_rev`（仅最长的全码）——反查回答
        // 的是「这个字怎么打」，简码才是最有用的答案。写死这个断言是为了让人改回只给全码
        // 的那个变量时变红。（0.122 之前这两个变量叫 `code_all` / `code`，现为兼容别名。）
        assert!(
            fmt.contains("${code_rev_all}") && fmt.contains("${pinyin}"),
            "默认版式应含全部码位与拼音，实际收到 {fmt:?}"
        );
        "好: vbg hǎo".to_string()
    };
    let host = wind_phrase::PhraseHost {
        clip: &clip,
        reverse: &reverse,
    };

    let hits = layer().lookup("cofc", &[], &host, &wind_phrase::PhraseScope::ALL);
    assert_eq!(
        hits.len(),
        1,
        "cofc 应恰好产出一条候选（解析失败会得到 0 条）"
    );
    assert_eq!(hits[0].text, "好: vbg hǎo");
    assert!(
        hits[0].command_src.is_some(),
        "出厂 cofc 是 $CC：显示与上屏必须分开，否则查不到时无法既提示又不上屏"
    );
}

/// 打前缀 `co` 时，`cofc` 在导航列表里的标签。
///
/// 前缀列举走廉价的 `NavCtx`（不读剪贴板、不查词库，否则要为每条候选摊一次开销到
/// 按键线程），故 `dict.rev` 在那里恒为空 → `default` 回落到提示语。
///
/// 这**恰好**让提示语兼作命令名：`co` 列表里这一条读作「剪贴板反查（需先复制文字）」，
/// 自解释。改提示语时要顺带想一下它在这个位置读起来是否成立。
#[test]
fn factory_cofc_shows_meaningful_label_in_prefix_nav() {
    let hits = layer().lookup_prefix("co", &[], 1, &wind_phrase::PhraseScope::ALL);
    let cofc: Vec<_> = hits
        .iter()
        .filter(|h| h.nav_code.as_deref() == Some("cofc"))
        .collect();
    assert_eq!(cofc.len(), 1, "打 co 应列出 cofc 这条导航");
    assert!(
        cofc[0].text.contains("剪贴板"),
        "导航标签应可读，实际 {:?}",
        cofc[0].text
    );
}

/// ★ 剪贴板为空 / 查不到时，**出一条提示候选**而不是一片空白。
///
/// 「什么都不出」与「功能坏了」在用户侧完全同形，这是真机反馈过的问题。
///
/// 同时锁住另一半：提示语**不能**被上屏。`$CC` 的 display 只管显示，上屏走
/// `type(dict.rev(clip()))` —— 查不到时它求值为空串，选中即什么都不打。
/// 若有人图省事把词条改回纯模板 `{default(dict.rev(clip()), "提示")}`，
/// 提示语就会变成上屏文本被打进用户文档，这条会因 `command_src` 为 None 而变红。
#[test]
fn factory_cofc_hints_when_empty_and_never_commits_the_hint() {
    let reverse = |_t: &str, _f: &str| String::new();

    for (name, clip_text) in [("剪贴板为空", ""), ("查不到（英文标点）", "Abc")] {
        let clip = |_n: i64| clip_text.to_string();
        let host = wind_phrase::PhraseHost {
            clip: &clip,
            reverse: &reverse,
        };
        let hits = layer().lookup("cofc", &[], &host, &wind_phrase::PhraseScope::ALL);
        assert_eq!(hits.len(), 1, "{name}：应出一条提示候选，而不是什么都没有");
        assert!(
            hits[0].text.contains("剪贴板"),
            "{name}：候选文本应是提示语，实际 {:?}",
            hits[0].text
        );
        assert!(
            hits[0].command_src.is_some(),
            "{name}：提示候选必须是命令候选，其上屏文本由 type() 决定（此时为空）"
        );
    }
}
