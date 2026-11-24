//! 资源层枚举点的清单守门：钉住「谁在自己拼资源目录」。
//!
//! 单文件读取有收口点（`Config::resolve_data_file` / `resolve_schema_resource` /
//! `EngineManager::resolve_schema_file` / `resolve_dict_file`），自动继承 `data_custom` 层；
//! 目录枚举没有，须遍历 [`wind_config::Config::resource_layers`] 系列 API。漏接一处时
//! 定制版里的方案/主题静默不见，且在没有 `data_custom` 的开发机上复现不出来。
//!
//! 能钉住：`src/` 下新增或删除一处 `join("schemas")` / `join("themes")` / `join("opencc")`
//! 即红，作者须在下面的清单里判定「已按层序展开」还是「刻意只要某一层」。
//!
//! 钉不住：
//!
//! 1. 只认这三个字面量；`join(SCHEMAS_SUBDIR)`、`push("schemas")`、`Path::new("…/schemas")`
//!    等写法扫不到。
//! 2. 不看上下文：只数次数，作者顺手改大清单数字就白设了，判定仍靠人读清单。
//! 3. 只跳过精确字面量 `#[cfg(test)]` 的测试模块；`#[cfg(all(test, windows))]` 块里的夹具
//!    会假红。遇到假红请扩判据，不要改数字。
//! 4. 测试模块按「缩进相等的 `}`」闭合；缩进对不齐的手写模块会让扫描吞到文件末尾，
//!    之后的生产代码漏计。测试模块之后的生产代码正常计入。

use std::path::{Path, PathBuf};

/// 允许出现 `join("schemas")` 的文件及出现次数，附每一处的判定。
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
        13,
        "4 处（scan_chars_in_range / 两个索引构建 / build_engine）把 data 层根交给\
         resolve_dict_file，层序在它内部展开；1 处 installed_schemas 的 scan_dirs 已按层序；\
         2 处 is_user_schema / delete_user_schema **刻意只查用户目录**——它们问的是\
         「这个方案文件是不是用户自己装的」，故 data / data_custom 的方案本来就已经是\
         `is_user_schema()==false`、`delete_user_schema()` 直接 bail，无需加层；\
         3 处是 resolve_schema_file 的层内拼接与兜底；         1 处 warn_stale_common_chars **刻意只查用户目录**——它问的是「用户自己那份         已经失效的 common_chars.txt 还在不在」，data / data_custom 层的同名文件是我们         自己发的（且已随常用字表搬进 charsets/ 而删除），不该提醒用户去迁移它；         1 处 declared_dict_files 同前 4 处——只是把 data 层根交给 resolve_dict_file，         层序在它内部展开；1 处 read_schema 给发现目录注入读已声明词库的头部（查 import_tables），         同样只是把 data 层根交给 resolve_dict_file；发现目录本身的列举走 list_schema_resource_dir",
    ),
    (
        "crates/wind-engine/src/manager/dict_stats.rs",
        1,
        "词库统计只把 data 层根交给 resolve_dict_file（经 load_dict_individually），层序在它\
         内部展开，与 manager.rs 那 4 处同理",
    ),
    (
        "crates/wind-engine/src/manager/dict_view.rs",
        1,
        "系统词库查看（条数 / 头部 / 词条）只把 data 层根交给 resolve_dict_file 定位主文件，\
         层序在它内部展开；import_tables 子表相对主文件所在目录、不走分层，同 rime_source_paths",
    ),
    (
        "crates/wind-config/src/config.rs",
        1,
        "list_schema_resource_dir 已按层序展开（在 resource_layers_named_with 的循环体内         拼 schemas/<sub>）——它是 resolve_schema_resource 的「有哪些名字」那一面，         设置页据此列出可选的注释词库文件，故与解析侧看见的层必须是同一批",
    ),
    (
        "crates/wind-engine/src/pinyin/generate/legacy_equiv.rs",
        1,
        "仅测试夹具（整文件 `#[cfg(test)]`）：对拍紧凑索引与旧实现，直接读 `build_dev/data` \
         这一个目录的拼音词库，不是运行期资源解析，不需要看见 data_custom 层",
    ),
    (
        "crates/wind-mobile/src/lib.rs",
        2,
        "scan_installed_schemas 已按层序（在 resource_layers_with 的循环体内拼 schemas）；\
         scheme_dirs 同样在 resource_layers_named_with 的循环体内逐层拼 schemas（方案包导入/删除的目标）",
    ),
    (
        "crates/wind-webdata/src/lib.rs",
        4,
        "3 处**用户层**写入侧：user_schemas_dir（导入方案包）与两处备份源；\
         1 处 system_schemas_dirs 已按层序展开（非 user 的每一层各一个 schemas/，\
         喂给 wind_transfer 的方案包导出/删除）",
    ),
];

/// 允许出现 `join("themes")` 的文件及出现次数。
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

/// 允许出现 `join("opencc")` 的文件及出现次数。
///
/// 加载侧一处都不该有：每本 octrie 各自走 `resolve_data_file`（rel = `opencc/<名>.octrie`），
/// 把 opencc 当目录整份加载会让定制层只放一本时整条链失效（见 `Converter::load_variant_resolved`）。
///
/// 下面一条是诊断侧例外：`config check --custom` 逐个比对定制层 `opencc/` 的文件名，
/// 只列目录名、不建 `Converter`。
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

/// 数某个片段在非注释、非 `#[cfg(test)] mod` 块的行上出现的次数。
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
