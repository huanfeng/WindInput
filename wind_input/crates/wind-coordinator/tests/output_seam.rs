//! 输出接缝守护：协调器里「根据 `session_tx` 选通道」的分支只许住在 `session_output.rs`。
//!
//! 生产代码（`src/` 下去掉 `#[cfg(test)]` 模块、跳过 `*_tests.rs`、`session_output.rs`、
//! `ui_sender.rs`）里，`UiCommand::HideCandidates` / `ShowToast` / `ShowStatusTip` /
//! `UpdateToolbar` 这四个 token 一个都不许出现：它们有中立事件对应物，直接构造/发送会让
//! 移动端（`new_session`）漏掉。桌面专用的 `SkinSignal` 同理：判通道的分支只在 emit_* 里。查的是 token 而不是 `.send(` 调用，所以限定路径
//! （`wind_ui_types::UiCommand::…`）和先存变量再发的写法也逃不掉。
//! `UpdateCandidates` 只允许 `coordinator.rs`、`handle_addword.rs` 各一处，且都在
//! 所属函数的 `session_tx` 分支之后。
//!
//! 用标准库递归遍历，不加依赖。

use std::fs;
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 去掉 `#[cfg(test)]` 起头的 `mod`（按花括号配平；不理会字符串/注释里的花括号，
/// 配平失败会让断言偏严而不是偏松）。`#[cfg(test)] mod x;` 这种以分号结尾的只丢这一句。
fn strip_cfg_test(src: &str) -> String {
    const ATTR: &str = "#[cfg(test)]";
    let mut out = String::new();
    let mut rest = src;
    while let Some(i) = rest.find(ATTR) {
        out.push_str(&rest[..i]);
        let after = &rest[i + ATTR.len()..];
        let trimmed = after.trim_start();
        if !trimmed.starts_with("mod ") && !trimmed.starts_with("pub mod ") {
            out.push_str(ATTR);
            rest = after;
            continue;
        }
        let semi = after.find(';');
        let open = after.find('{');
        match (semi, open) {
            (Some(sc), Some(ob)) if sc < ob => rest = &after[sc + 1..],
            (Some(sc), None) => rest = &after[sc + 1..],
            (_, Some(open)) => {
                let mut depth = 0usize;
                let mut end = after.len();
                for (j, ch) in after[open..].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = open + j + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                rest = &after[end..];
            }
            (None, None) => rest = "",
        }
    }
    out.push_str(rest);
    out
}

/// 文本里 `UiCommand::<变体>` 的所有出现位置（行号），变体名按完整标识符比较。
fn tokens_of(src: &str, variants: &[&str]) -> Vec<(String, usize)> {
    const P: &str = "UiCommand::";
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(i) = src[from..].find(P) {
        let at = from + i;
        let name: String = src[at + P.len()..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if variants.contains(&name.as_str()) {
            hits.push((name, src[..at].matches('\n').count() + 1));
        }
        from = at + P.len();
    }
    hits
}

/// (相对路径, 去掉测试代码后的文本)。行号与函数边界都基于这一份文本。
fn production_sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    files
        .into_iter()
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            n != "session_output.rs" && n != "ui_sender.rs" && !n.ends_with("_tests.rs")
        })
        .map(|p| {
            let rel = p.strip_prefix(&src).unwrap().display().to_string();
            (rel, strip_cfg_test(&fs::read_to_string(&p).unwrap()))
        })
        .collect()
}

const NEUTRAL: [&str; 5] = [
    "HideCandidates",
    "ShowToast",
    "ShowStatusTip",
    "UpdateToolbar",
    // 桌面专用（中立通道不发），同样得经 emit_* 判过通道。
    "SkinSignal",
];

#[test]
fn neutral_variants_do_not_appear_outside_emit_helpers() {
    let mut bad = Vec::new();
    for (rel, src) in production_sources() {
        for (name, line) in tokens_of(&src, &NEUTRAL) {
            bad.push(format!(
                "{rel}（去测试后第 {line} 行）出现 UiCommand::{name}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "这些应改走 session_output.rs 的 emit_*：\n{}",
        bad.join("\n")
    );
}

/// 自检：token 查找能抓到限定路径和变量写法，剥离能认出 `mod x;`。
#[test]
fn scanner_catches_qualified_and_variable_forms() {
    let q = "x.send(wind_ui_types::UiCommand::HideCandidates)";
    assert_eq!(tokens_of(q, &NEUTRAL).len(), 1);
    let v = "let cmd = UiCommand::ShowToast { a: 1 };\nself.ui_tx.send(cmd);";
    assert_eq!(tokens_of(v, &NEUTRAL).len(), 1);
    assert!(tokens_of("UiCommand::HideCandidatesX", &NEUTRAL).is_empty());
    let semi = "#[cfg(test)]\nmod x;\nfn prod() { UiCommand::HideCandidates }";
    assert_eq!(tokens_of(&strip_cfg_test(semi), &NEUTRAL).len(), 1);
    let blk = "#[cfg(test)]\nmod t { fn a() { UiCommand::HideCandidates } }\nfn p() {}";
    assert!(tokens_of(&strip_cfg_test(blk), &NEUTRAL).is_empty());
}

#[test]
fn update_candidates_only_after_session_branch() {
    let mut found: Vec<(String, usize, String)> = Vec::new();
    for (rel, src) in production_sources() {
        for (_, line) in tokens_of(&src, &["UpdateCandidates"]) {
            found.push((rel.clone(), line, src.clone()));
        }
    }
    let mut files: Vec<&str> = found.iter().map(|(f, _, _)| f.as_str()).collect();
    files.sort();
    assert_eq!(
        files,
        vec!["coordinator.rs", "handle_addword.rs"],
        "UpdateCandidates 只许这两处各一次: {:?}",
        found.iter().map(|(f, l, _)| (f, l)).collect::<Vec<_>>()
    );
    // 同一份（去测试后的）文本里，往前找最近的函数头，其后必须有 `session_tx.is_some()`。
    for (rel, line, src) in &found {
        let before: String = src.lines().take(*line - 1).collect::<Vec<_>>().join("\n");
        let fn_start = ["\n    pub(crate) fn ", "\n    pub fn ", "\n    fn "]
            .iter()
            .filter_map(|p| before.rfind(p))
            .max();
        let branch = before.rfind("self.session_tx.is_some()");
        assert!(
            matches!((branch, fn_start), (Some(b), Some(f)) if b > f),
            "{rel}（去测试后第 {line} 行）的 UpdateCandidates 之前，同一函数内应有 session_tx 分支"
        );
    }
}
