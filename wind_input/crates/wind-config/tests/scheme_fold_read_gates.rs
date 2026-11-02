//! 通用折叠键的**读取点守门**：登记为 `fold = true` 的 `ui.candidate.*` 键，生产代码不得
//! 绕过 `Coordinator::eff_candidate()` 直接读全局值。
//!
//! # 为什么需要它
//!
//! 方案里配了 `per_page`，只有读取点走折叠结果才会生效。哪怕只有一处仍写
//! `config.ui.candidate.per_page`，那一处就「配了不生效」——本仓最高频的报障形态，
//! 且在没有声明方案级覆盖的开发机上永远复现不出来。
//!
//! # 能钉住什么、钉不住什么
//!
//! **能**：`wind-config` 之外的生产代码里出现 `.ui.candidate.<折叠键>` 字面链式访问即红。
//! 新增折叠登记后，既有的绕行读取立刻现形，作者被迫改走 `eff_candidate()`。
//!
//! **钉不住**（老实说清楚）：
//! 1. 先 `let c = &cfg.ui.candidate;` 再 `c.per_page`——只认完整的链式字面量；
//! 2. 测试模块的跳过判据是首个 `#[cfg(test)]` 行，其后的内容一律不计；
//! 3. `wind-config` 自身整个豁免（定义、归一化、折叠本体都在那里）。

use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // tests/ 里是夹具，写配置不是读取点；target 是产物。
            if matches!(name, "target" | "tests" | "wind-config") {
                continue;
            }
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 生产段里直接读折叠键的行（`文件:行号: 内容`）。
fn offenders(src: &str, leaves: &[&str], file: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (i, line) in src.lines().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        for leaf in leaves {
            let needle = format!(".ui.candidate.{leaf}");
            if let Some(pos) = line.find(&needle) {
                let after = &line[pos + needle.len()..];
                // 叶子名完整匹配（`per_page` 不应误伤 `per_page_extended`）。
                if !after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                    hits.push(format!("{file}:{}: {}", i + 1, line.trim()));
                }
            }
        }
    }
    hits
}

fn fold_leaves() -> Vec<&'static str> {
    wind_config::config_schema::SCHEMA_OVERRIDES
        .iter()
        .filter(|o| o.fold)
        .filter_map(|o| o.key.strip_prefix("ui.candidate."))
        .collect()
}

#[test]
fn no_production_code_reads_folded_keys_directly() {
    let leaves = fold_leaves();
    assert!(!leaves.is_empty(), "没有任何折叠登记，门禁空转");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    rs_files(&root.join("crates"), &mut files);
    rs_files(&root.join("apps"), &mut files);
    assert!(
        files.len() > 50,
        "只扫到 {} 个文件，路径可能不对",
        files.len()
    );
    let mut hits = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        hits.extend(offenders(&src, &leaves, &f.display().to_string()));
    }
    assert!(
        hits.is_empty(),
        "以下生产代码绕过 Coordinator::eff_candidate() 直接读了可被方案覆盖的键，\
         方案里配了会不生效：\n{}",
        hits.join("\n")
    );
}

/// 扫描器自检：会抓真读取、不抓注释 / 字符串里的键名 / 同前缀兄弟键 / 测试段。
#[test]
fn scanner_flags_direct_reads_only() {
    let leaves = ["per_page"];
    let bad = "let n = self.rt().config.ui.candidate.per_page;\n";
    assert_eq!(offenders(bad, &leaves, "x.rs").len(), 1);
    assert!(offenders("// config.ui.candidate.per_page\n", &leaves, "x.rs").is_empty());
    assert!(offenders("let k = \"ui.candidate.per_page\";\n", &leaves, "x.rs").is_empty());
    assert!(
        offenders(
            "let n = cfg.ui.candidate.per_page_extended;\n",
            &leaves,
            "x.rs"
        )
        .is_empty(),
        "兄弟键 per_page_extended 不该被误伤"
    );
    let tail = "fn a() {}\n#[cfg(test)]\nmod t { fn b() { cfg.ui.candidate.per_page = 5; } }\n";
    assert!(offenders(tail, &leaves, "x.rs").is_empty());
}
