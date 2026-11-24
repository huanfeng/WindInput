//! 出厂剪贴板反查短语（`cofc`）的守门。
//!
//! 系统短语语法错时 `evaluate_phrase` 返回 Err、短语层只记 warn 丢弃整条，用户侧与「剪贴板为空」
//! 同形，只能靠测试守。
//!
//! 读的是仓库 `data/system.phrases.toml` 而非 `build_dev/data/`：后者是部署产物，在 worktree 里
//! 是指向主仓的符号链接，拿它跑等于验主仓的旧文件。

use std::path::PathBuf;

fn repo_phrases() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data/system.phrases.toml")
}

fn layer() -> wind_phrase::PhraseLayer {
    let p = repo_phrases();
    assert!(p.is_file(), "找不到出厂短语文件：{}", p.display());
    wind_phrase::PhraseLayer::load(&p)
}

/// `cofc` 必须能解析并求值出「剪贴板整串内容的反查」，display 原样等于宿主返回的反查结果。
///
/// 词条写的是 `dict.rev(clip())`：省略 n 即整串反查，宿主拿到的必须是整个剪贴板文本（「好吧」）
/// 而不是第 1 个字。查不到的情形由下一条负责。
#[test]
fn factory_cofc_parses_and_renders_clipboard_lookup() {
    let clip = |_n: i64| "好吧".to_string();
    let reverse = |text: &str, fmt: &str, _m: usize| {
        assert_eq!(text, "好吧", "出厂词条应查剪贴板整串（省略 n 即整串）");
        // 必须是 `code_rev_all`（全部码位）而非 `code_rev`（仅最长全码）：反查回答「这个字怎么打」，
        // 简码才是最有用的答案。（`code_all` / `code` 是兼容别名。）
        assert!(
            fmt.contains("${code_rev_all}") && fmt.contains("${pinyin}"),
            "默认版式应含全部码位与拼音，实际收到 {fmt:?}"
        );
        "好吧: hbba hǎo ba".to_string()
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
    assert_eq!(hits[0].text, "好吧: hbba hǎo ba");
    assert!(
        hits[0].command_src.is_some(),
        "出厂 cofc 是 $CC：显示与上屏必须分开，否则查不到时无法既提示又不上屏"
    );
}

/// 打前缀 `co` 时，`cofc` 在导航列表里的标签。
///
/// 前缀列举走廉价的 `NavCtx`（不读剪贴板、不查词库，免得每条候选都给按键线程摊开销），
/// `dict.rev` 在那里恒为空，`default` 回落到提示语，提示语因此兼作命令名。改提示语时要顺带
/// 想想它在这个位置读起来是否成立。
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

/// 剪贴板为空 / 查不到时出一条提示候选而不是一片空白（否则与「功能坏了」同形）。
///
/// 同时钉住提示语不能上屏：display 只管显示，上屏走 `type(dict.rev(clip()))`，查不到时求值为
/// 空串。若词条改回纯模板 `{default(dict.rev(clip()), "提示")}`，提示语会被打进用户文档，
/// 这条会因 `command_src` 为 None 而变红。
#[test]
fn factory_cofc_hints_when_empty_and_never_commits_the_hint() {
    let reverse = |_t: &str, _f: &str, _m: usize| String::new();

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
