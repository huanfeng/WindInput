//! 资源层枚举点的**清单守门**：钉住「谁在自己拼资源目录」这件事。
//!
//! # 为什么需要它
//!
//! 单文件读取有收口点（`Config::resolve_data_file` / `resolve_schema_resource` /
//! `EngineManager::resolve_schema_file` / `resolve_dict_file`），加一层 `data_custom`
//! 自动继承。**目录枚举没有收口点**——方案列表、主题列表、双拼布局、opencc 数据目录
//! 各自维护一份自己的目录列表，一共七处，漏接一处的现象是「定制版里那个方案/主题
//! 静默不见了」，无任何日志，且在没有 `data_custom` 的开发机上永远复现不出来。
//!
//! P1d 已把这七处改成遍历 [`wind_config::Config::resource_layers`] 系列 API。本测试
//! 存在的理由是**下一个功能**：新写一处 `dir.join("schemas")` 是最自然不过的事，而它
//! 天然就是两层（或一层）的，漏接不会有任何编译期或运行期信号。
//!
//! # 这道测试能钉住什么、钉不住什么
//!
//! **能**：任何 `src/` 下的文件新增（或删除）一处 `join("schemas")` / `join("themes")` /
//! `join("opencc")`，本测试立刻变红，作者被迫回到下面的清单给它一个明确判定——是
//! 「已按层序展开」、还是「刻意只要某一层」（写用户目录、判定内置与否等）。
//!
//! **钉不住**（老实说清楚）：
//!
//! 1. 它只认这三个**字面量**。`join(SCHEMAS_SUBDIR)`、`join(format!("{sub}"))`、
//!    `push("schemas")`、`Path::new("…/schemas")` 一律扫不到。绕过它不需要恶意，只需要
//!    换个写法。
//! 2. 它不看调用的**上下文**：已登记的文件里，在已登记的那一处旁边再写一个只认两层的
//!    枚举，只要总数对不上就红——但如果作者顺手把清单数字改大（而不去想为什么），
//!    测试就白设了。判罪仍然靠人读下面那张表。
//! 3. 跳过测试模块的判据是**精确字面量** `#[cfg(test)]`。`#[cfg(all(test, windows))]`
//!    这一族（`wind-ui/src/candidate_window.rs`、`wind-bridge` 多处）**不会**被跳过 ⇒
//!    在那种块里写含 `join("schemas")` 的测试夹具会让本测试**假红**。假红最省事的处理
//!    方式恰恰是「顺手把清单数字改大」，那这道闸门就废了——遇到假红请扩这里的判据，
//!    不要改数字。
//! 4. 测试模块的闭合判据是「缩进精确相等的一行 `}`」（rustfmt 产物必然成立）。手写的、
//!    缩进对不齐的测试模块会让扫描从 `#[cfg(test)]` 一路吞到文件末尾，那之后的生产代码
//!    全部漏计。
//!
//! （曾在这里写过一条「写在测试模块之后的生产代码同样不计」——**那是错的**，已实测
//!  推翻：跳过测试模块后扫描会回到主循环继续，`log_rotate.rs` 的 `prune_stale_tsf_logs`
//!  这类写在测试模块之后的函数照样计入。一条假的「已知盲区」比没有更糟：它会让人相信
//!  一块其实有覆盖的区域没覆盖。）

use std::path::{Path, PathBuf};

/// 允许出现 `join("schemas")` 的文件及**出现次数**，附每一处的判定。
///
/// 新增一处时请回答：「这里枚举/解析资源，要不要看见 `data_custom` 层？」
/// 要 ⇒ 改用 `Config::resource_layers*`；不要 ⇒ 登记并写明只认哪一层、为什么。
const SCHEMAS_SITES: &[(&str, usize, &str)] = &[
    (
        "apps/service/src/config_cli/custom_check.rs",
        1,
        "`config check --custom` 刻意**只看命令行指定的那两个目录**（`--custom` / `--data`），\
         不走 resource_layers：它体检的是一份还没安装的定制包，混进本机安装的层会让同一个\
         包在两台机器上体检出两种结论。同 `dict weight-check --data` 的取舍",
    ),
    (
        "apps/service/src/dict_cli.rs",
        2,
        "`dict weight-check` 的扫描层序：不带 `--data` 时按 resource_layers 展开（第 2 处），\
         带 `--data` 时刻意只看指定的那一个目录（第 1 处）——那个标志的语义是「体检这个目录」",
    ),
    (
        "crates/wind-engine/src/manager.rs",
        10,
        "4 处（scan_chars_in_range / 两个索引构建 / build_engine）把 data 层根交给\
         resolve_dict_file，层序在它内部展开；1 处 installed_schemas 的 scan_dirs 已按层序；\
         2 处 is_user_schema / delete_user_schema **刻意只查用户目录**——它们问的是\
         「这个方案文件是不是用户自己装的」，故 data / data_custom 的方案本来就已经是\
         `is_user_schema()==false`、`delete_user_schema()` 直接 bail，无需加层；\
         3 处是 resolve_schema_file 的层内拼接与兜底",
    ),
    (
        "crates/wind-mobile/src/lib.rs",
        1,
        "scan_installed_schemas 已按层序（在 resource_layers_with 的循环体内拼 schemas）",
    ),
    (
        "crates/wind-webdata/src/lib.rs",
        4,
        "3 处**用户层**写入侧：user_schemas_dir（导入方案包）与两处备份源；\
         1 处 system_schemas_dirs 已按层序展开（非 user 的每一层各一个 schemas/，\
         喂给 wind_transfer 的方案包导出/删除）",
    ),
];

/// 允许出现 `join("themes")` 的文件及**出现次数**。
const THEMES_SITES: &[(&str, usize, &str)] = &[
    (
        "apps/service/src/config_cli/custom_check.rs",
        1,
        "同 SCHEMAS_SITES 里那条：`config check --custom` 只看命令行指定的两个目录",
    ),
    (
        "crates/wind-webdata/src/lib.rs",
        3,
        "全是**用户层**：user_themes_dir（导入/删除的落点）与两处备份源。\
         主题搜索链走 theme_search_dirs（已按层序）",
    ),
];

/// 允许出现 `join("opencc")` 的文件及**出现次数**。
///
/// **空表是刻意的**：简繁数据已改为「链里每本 octrie 各自走 `resolve_data_file`
/// （rel = `opencc/<名>.octrie`）」，全仓不该再有任何一处把 opencc 当成一个目录去拼——
/// 那种写法一定意味着「先选中一个目录再整份加载」，而那正是「定制层只放一本
/// `STPhrases.octrie` 就一个字都不转」的成因（见 `Converter::load_variant_resolved`）。
///
/// ⚠️ 上面那句「空表是刻意的」约束的是**加载**侧。下面这一条是**诊断**侧的例外：
/// `config check --custom` 要把定制层 `opencc/` 里的文件名与出厂目录逐个比对，好报出
/// 「名字对不上 ⇒ 这本永远不会被任何一条链取到」。它只列目录名、不建 `Converter`，
/// 成立的前提恰恰就是「链按文件名跨层取」这条性质——与上面要禁的写法方向相反。
const OPENCC_SITES: &[(&str, usize, &str)] = &[(
    "apps/service/src/config_cli/custom_check.rs",
    2,
    "诊断而非加载：列定制层与出厂层的 opencc 目录做文件名比对，不构建 Converter",
)];

#[test]
fn resource_dir_join_sites_are_all_classified() {
    let root = workspace_root();
    assert_inventory(&scan(&root, r#"join("schemas")"#), SCHEMAS_SITES);
    assert_inventory(&scan(&root, r#"join("themes")"#), THEMES_SITES);
    assert_inventory(&scan(&root, r#"join("opencc")"#), OPENCC_SITES);
}

/// 工作区根（`wind_input/`）：本 crate 在 `wind_input/crates/wind-config`。
fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("wind-config 应位于 <root>/crates/wind-config")
        .to_path_buf();
    // 硬失败而不是静默跳过：一个「找不到源码就当通过」的守门测试等于没有守门。
    assert!(
        root.join("crates").is_dir() && root.join("apps").is_dir(),
        "扫不到工作区源码（{}），本测试无法成立",
        root.display()
    );
    root
}

/// 递归收集 `crates/*/src` 与 `apps/*/src` 下的 `.rs`（相对工作区根的 `/` 分隔路径）。
/// 跳过 `*_tests.rs`：整文件都是测试夹具。
fn source_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(&p, root, out);
            } else if p.extension().is_some_and(|x| x == "rs")
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with("_tests.rs"))
            {
                let rel = p
                    .strip_prefix(root)
                    .expect("在 root 下")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    for base in ["crates", "apps"] {
        let Ok(rd) = std::fs::read_dir(root.join(base)) else {
            continue;
        };
        for e in rd.flatten() {
            let src = e.path().join("src");
            if src.is_dir() {
                walk(&src, root, &mut out);
            }
        }
    }
    out.sort();
    out
}

/// 数某个片段在**非注释、非 `#[cfg(test)] mod` 块**的行上出现的次数。
///
/// 测试模块的判据是 rustfmt 的缩进闭合：`#[cfg(test)]` 之后紧跟 `mod X {`，
/// 则跳到同缩进的 `}` 为止。不做真正的语法分析——那对一道 grep 式守门来说过重。
fn count_code(text: &str, needle: &str) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0usize;
    let mut n = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed == "#[cfg(test)]" {
            let indent = &line[..line.len() - trimmed.len()];
            let next = lines.get(i + 1).map(|l| l.trim_start()).unwrap_or("");
            if next.starts_with("mod ") && next.ends_with('{') {
                let close = format!("{indent}}}");
                i += 2;
                while i < lines.len() && lines[i] != close {
                    i += 1;
                }
                i += 1;
                continue;
            }
        }
        if !trimmed.starts_with("//") && line.contains(needle) {
            n += 1;
        }
        i += 1;
    }
    n
}

/// 扫全仓，返回 `(相对路径, 次数)`，按路径排序。
fn scan(root: &Path, needle: &str) -> Vec<(String, usize)> {
    let mut found: Vec<(String, usize)> = Vec::new();
    for rel in source_files(root) {
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        let n = count_code(&text, needle);
        if n > 0 {
            found.push((rel, n));
        }
    }
    found
}

/// 把清单与实测对齐；不一致时把差异逐条打出来。
fn assert_inventory(actual: &[(String, usize)], expected: &[(&str, usize, &str)]) {
    let mut problems: Vec<String> = Vec::new();
    for (path, n) in actual {
        match expected.iter().find(|(p, _, _)| p == path) {
            None => problems.push(format!(
                "新增未登记的资源目录拼接：{path}（{n} 处）\n\
                 → 这里要不要看见 data_custom 层？\n\
                   要：改用 `Config::resource_layers` / `resource_layers_named*`；\n\
                   不要：登记到 tests/resource_layer_gates.rs 并写明只认哪一层、为什么。"
            )),
            Some((_, want, _)) if want != n => problems.push(format!(
                "拼接次数变了：{path} 清单 {want} 处、实测 {n} 处\n\
                 → 新增的那处按层序展开了吗？判完再改清单。"
            )),
            Some(_) => {}
        }
    }
    for (path, want, why) in expected {
        if !actual.iter().any(|(p, _)| p == path) {
            problems.push(format!(
                "清单里的拼接点已消失：{path}（登记 {want} 处，判定「{why}」）\n\
                 → 确认是删掉了就把这一行也删掉，别留着当摆设。"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "资源层枚举点清单与源码不符：\n\n{}\n\n\
         这张清单存在的理由见本文件头部——漏接一处 = 定制版里那份资源静默不见。",
        problems.join("\n\n")
    );
}
