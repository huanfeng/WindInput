//! 输出接缝守护：协调器里「根据 `session_tx` 选通道」的分支只许住在 `session_output.rs`。
//!
//! 生产代码（`src/` 下去掉 `#[cfg(test)]` 模块、跳过 `session_output.rs` 与 `ui_sender.rs`）
//! 里不得出现发送 `HideCandidates` / `ShowToast` / `ShowStatusTip` / `UpdateToolbar` 的调用：
//! 这四种有中立事件对应物，直接发会让移动端（`new_session`）漏掉。
//! `UpdateCandidates` 只允许 `coordinator.rs`、`handle_addword.rs` 各一处，且都在
//! `session_tx` 分支之后。
//!
//! 用标准库递归遍历，不加依赖。匹配覆盖换行写法（`.send(\n    UiCommand::…`）。

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

/// 去掉 `#[cfg(test)]` 起头的 `mod … { … }`（按花括号配平；不理会字符串/注释里的花括号，
/// 对本仓够用，配平失败会让断言偏严而不是偏松）。
fn strip_cfg_test(src: &str) -> String {
    let mut out = String::new();
    let mut rest = src;
    while let Some(i) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..i]);
        let after = &rest[i + "#[cfg(test)]".len()..];
        // 只剥 `mod`；`#[cfg(test)] fn / use` 之类保留（它们很短，也不含发送）
        let trimmed = after.trim_start();
        if !trimmed.starts_with("mod ") && !trimmed.starts_with("pub mod ") {
            out.push_str("#[cfg(test)]");
            rest = after;
            continue;
        }
        let Some(open) = after.find('{') else {
            // `#[cfg(test)] mod x;`：整句丢掉
            rest = after;
            continue;
        };
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
    out.push_str(rest);
    out
}

/// 找出所有 `.send(` 后紧跟（可跨空白换行）`UiCommand::<variant>` 的位置，返回 (变体, 行号)。
fn sends_of(src: &str, variants: &[&str]) -> Vec<(String, usize)> {
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(i) = src[from..].find(".send(") {
        let at = from + i;
        let after = src[at + ".send(".len()..].trim_start();
        if let Some(tail) = after.strip_prefix("UiCommand::") {
            let name: String = tail
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if variants.contains(&name.as_str()) {
                let line = src[..at].matches('\n').count() + 1;
                hits.push((name, line));
            }
        }
        from = at + ".send(".len();
    }
    hits
}

fn production_sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    files
        .into_iter()
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            n != "session_output.rs" && n != "ui_sender.rs"
        })
        .map(|p| {
            let rel = p.strip_prefix(&src).unwrap().display().to_string();
            (rel, strip_cfg_test(&fs::read_to_string(&p).unwrap()))
        })
        .collect()
}

#[test]
fn neutral_variants_are_only_sent_through_emit_helpers() {
    let banned = [
        "HideCandidates",
        "ShowToast",
        "ShowStatusTip",
        "UpdateToolbar",
    ];
    let mut bad = Vec::new();
    for (rel, src) in production_sources() {
        for (name, line) in sends_of(&src, &banned) {
            bad.push(format!("{rel}:{line} 直接发送 UiCommand::{name}"));
        }
    }
    assert!(
        bad.is_empty(),
        "这些发送应改走 session_output.rs 的 emit_*：\n{}",
        bad.join("\n")
    );
}

#[test]
fn update_candidates_only_after_session_branch() {
    let mut found: Vec<(String, usize)> = Vec::new();
    for (rel, src) in production_sources() {
        for (_, line) in sends_of(&src, &["UpdateCandidates"]) {
            found.push((rel.clone(), line));
        }
    }
    let mut files: Vec<&str> = found.iter().map(|(f, _)| f.as_str()).collect();
    files.sort();
    assert_eq!(
        files,
        vec!["coordinator.rs", "handle_addword.rs"],
        "UpdateCandidates 只许这两处各一次: {found:?}"
    );
    // 都得在 `session_tx.is_some()` 分支之后（分支以 return 收尾）
    for (rel, line) in &found {
        let full = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel))
            .unwrap();
        let before: String = full.lines().take(*line - 1).collect::<Vec<_>>().join("\n");
        let branch = before.rfind("self.session_tx.is_some()");
        let this_fn = before
            .rfind("\n    pub(crate) fn ")
            .max(before.rfind("\n    fn "));
        assert!(
            matches!((branch, this_fn), (Some(b), Some(f)) if b > f),
            "{rel}:{line} 的 UpdateCandidates 之前，同一函数内应有 session_tx 分支"
        );
    }
}
