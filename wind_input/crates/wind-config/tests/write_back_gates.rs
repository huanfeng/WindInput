//! 降级写盘闸的**清单守门**：钉住「谁在拿 `Config::load()` 的结果当种子」这件事。
//!
//! # 为什么需要它
//!
//! 段级降级（P0）把 `load()` 的 `Err` 变成了「成功但某段是出厂值」。凡是拿这个结果
//! 当**种子**再整表写回用户层 / 导出给用户的路径，降级时都会把用户数据抹掉。
//! 本仓已经逐条找出四条同形状的路径：
//!
//! | 路径 | 不加闸的后果 |
//! |---|---|
//! | `Config::materialize_key_actions` | 拿出厂残表整表覆盖 `keys.key_actions` 并打死版本标记 |
//! | `config_cli::cmd_export` | 导出一份「坏段已被出厂值替换」的 TOML，用户拿去备份/回写 |
//! | `patch::writes`（`config.applyPatch`） | Map 键的合并种子是出厂空表 ⇒ 用户已有条目整表消失 |
//! | `config.setItems` 的 Map 键 | 设置端发来的整表 = 出厂 base ⊕ 本次编辑 ⇒ 同上 |
//!
//! 找到第三条、第四条的过程说明一件事：**逐条打地鼠一定会漏下一条**。所以除了各自加闸，
//! 还要有一道机制，让「新开一条这样的路径」不能悄无声息地发生。
//!
//! # 这道测试能钉住什么、钉不住什么
//!
//! **能**：任何文件里新增（或删除）一处 `Config::load(` 调用，本测试立刻变红，作者被迫
//! 回到下面这张清单给它一个明确的判定——是只读消费，还是又一条写回路径。这正是唯一
//! 值得打断的时刻。同理，任何新的 `patch::preview(` 调用点若没有配套的
//! `mark_degraded_seeds(`，也会红。
//!
//! **钉不住**（老实说清楚）：它只认「拿到了可能降级的配置」这个**来源**，认不出调用方
//! 拿它去干了什么。一个人完全可以在已登记的文件里、已登记的那一处 `Config::load()`
//! 之后新写一段整表写回而本测试全绿。要挡住那个，得做真正的数据流分析，代价远超收益。
//! 本测试的定位是**强制分类**，不是自动判罪——判罪仍然靠人读上面那张表。
//!
//! 计数只看**非注释行**（`//` 开头的行不计），否则每改一句文档都要来改清单。

use std::path::{Path, PathBuf};

/// 允许出现 `Config::load(` 的文件及其**调用次数**，附每一处的判定。
///
/// 改动这张表时，请对每一处新增回答：「它的结果会不会成为写回用户层 / 导出给用户的
/// 种子？」是 ⇒ 必须过 `ConfigDegradation::blocks_write_back` / `taints`。
const CONFIG_LOAD_SITES: &[(&str, usize, &str)] = &[
    ("apps/repl/src/main.rs", 1, "只读：REPL 内存消费，不写盘"),
    (
        "apps/service/src/config_cli.rs",
        2,
        "cmd_export 已闸（is_degraded ⇒ 拒绝导出）；load_value 只读单键显示",
    ),
    (
        "apps/service/src/config_cli/custom_check.rs",
        1,
        "**不是调用点**：`config check --custom` 全程不加载本机配置（它体检的是别人的\
         定制包），那一处是 `check_never_touches_the_user_layer` 里的**禁用词字面量**，\
         正好用来钉住这一点。真新增调用点时次数会变成 2，本闸门照常拦",
    ),
    (
        "apps/service/src/main.rs",
        1,
        "只读：取日志级别等启动参数，不写盘",
    ),
    (
        "apps/wind-tools/src/bin/gen_dict/main.rs",
        1,
        "无关：那是 gen_dict 工具自己的 Config 类型，不是 wind_config::Config",
    ),
    (
        "crates/wind-coordinator/src/construct.rs",
        1,
        "只读：构造运行时配置，内存消费",
    ),
    (
        "crates/wind-coordinator/src/coordinator.rs",
        1,
        "只读：热重载，内存消费",
    ),
    (
        "crates/wind-coordinator/src/handle_cmdbar.rs",
        1,
        "load_value：get 只读放行；toggle 传 require_trustworthy=true ⇒ 已闸",
    ),
    (
        "crates/wind-mobile/src/lib.rs",
        1,
        "只读：移动端启动加载，内存消费",
    ),
    (
        "crates/wind-rpc/src/dispatch.rs",
        5,
        "config.get / getItem 只读；patch_entries 与 setItems 的 Map 键已闸；\
         config.degradation 只读且**只报告降级本身**（把 degradation 原样交给设置页显示，\
         不产生任何写回种子）",
    ),
    (
        "crates/wind-webdata/src/lib.rs",
        2,
        "config_patch_diff 已闸（mark_degraded_seeds）；keys_overview 只显示不写盘，\
         且降级时不列出不可信的那一层（见 keys_overview 的文档）",
    ),
];

/// `Self::load(` 在 wind-config 内部的调用点（上表按 `Config::load(` 计，扫不到它）。
const SELF_LOAD_SITES: &[(&str, usize, &str)] = &[(
    "crates/wind-config/src/config.rs",
    2,
    "① materialize_key_actions 的权威加载，已闸（blocks_write_back(\"keys\", …)）；\
     ② key_origin 的生效值取样——纯只读（结果只进 CLI/RPC 的呈现），\
     且它反过来把 degradation.taints 作为 KeyOrigin.degraded 报出去，\
     正是这张闸门表想要的方向",
)];

/// 工作区根（`wind_input/`）：本 crate 在 `wind_input/crates/wind-config`。
fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("wind-config 应位于 <root>/crates/wind-config")
        .to_path_buf();
    // 硬失败而不是静默跳过：一个「找不到源码就当通过」的守门测试等于没有守门，
    // 而它的绿灯还会让人以为查过了。
    assert!(
        root.join("crates").is_dir() && root.join("apps").is_dir(),
        "扫不到工作区源码（{}），本测试无法成立",
        root.display()
    );
    root
}

/// 递归收集 `crates/*/src` 与 `apps/*/src` 下的 `.rs`（相对工作区根的 `/` 分隔路径）。
///
/// 只扫 `src/`：`tests/` 里的 `Config::load()` 是测试自己的夹具，不构成生产写盘路径，
/// 计进来只会让这张清单被测试改动反复惊动。
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
            } else if p.extension().is_some_and(|x| x == "rs") {
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
    // `apps/wind-tools/src/bin/**` 这类嵌套 bin 也在 src 下，已被 walk 覆盖。
    out.sort();
    out
}

/// 数某个片段在非注释行上的出现次数。
fn count_code(text: &str, needle: &str) -> usize {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .filter(|l| l.contains(needle))
        .count()
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

/// 把清单与实测对齐；不一致时把差异逐条打出来，而不是只报一句「不相等」。
fn assert_inventory(actual: &[(String, usize)], expected: &[(&str, usize, &str)], needle: &str) {
    let mut problems: Vec<String> = Vec::new();
    for (path, n) in actual {
        match expected.iter().find(|(p, _, _)| p == path) {
            None => problems.push(format!(
                "新增未登记的 `{needle}` 调用点：{path}（{n} 处）\n\
                 → 它的结果会不会成为写回用户层/导出给用户的种子？\n\
                   会：过 `ConfigDegradation::blocks_write_back` / `taints` 再登记；\n\
                   不会：登记到 tests/write_back_gates.rs 的清单并写明「只读」的理由。"
            )),
            Some((_, want, _)) if want != n => problems.push(format!(
                "`{needle}` 调用次数变了：{path} 清单 {want} 处、实测 {n} 处\n\
                 → 新增的那处是只读还是写回？判完再改清单。"
            )),
            Some(_) => {}
        }
    }
    for (path, want, why) in expected {
        if !actual.iter().any(|(p, _)| p == path) {
            problems.push(format!(
                "清单里的 `{needle}` 调用点已消失：{path}（登记 {want} 处，判定「{why}」）\n\
                 → 确认是删掉了就把这一行也删掉，别留着当摆设。"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "降级写盘闸清单与源码不符：\n\n{}\n\n\
         这张清单存在的理由见本文件头部——已经有四条路径栽在同一个坑里了。",
        problems.join("\n\n")
    );
}

#[test]
fn config_load_sites_are_all_classified() {
    let root = workspace_root();
    assert_inventory(
        &scan(&root, "Config::load("),
        CONFIG_LOAD_SITES,
        "Config::load(",
    );
    assert_inventory(&scan(&root, "Self::load("), SELF_LOAD_SITES, "Self::load(");
}

/// `patch::preview()` 产出的条目要拿去当 Map 合并种子，故每个调用点都必须紧接着跑
/// [`wind_config::patch::mark_degraded_seeds`]——漏一处，那条路上的降级就没人拦。
///
/// 这一条比上面的清单**强**：它不只要求分类，而是要求同一文件里出现配套的调用。
/// 弱点同样说清楚：它只看「同一文件里有没有」，不保证接在正确的那一处 preview 后面。
#[test]
fn every_patch_preview_is_followed_by_the_degradation_gate() {
    let root = workspace_root();
    let sites = scan(&root, "patch::preview(");
    assert!(
        !sites.is_empty(),
        "一处 `patch::preview(` 都扫不到，说明扫描失效了（而不是真的没有）"
    );
    for (path, n) in &sites {
        let text = std::fs::read_to_string(root.join(path)).expect("读源码");
        let gates = count_code(&text, "mark_degraded_seeds(");
        assert!(
            gates >= *n,
            "{path}：{n} 处 `patch::preview(` 却只有 {gates} 处 `mark_degraded_seeds(`。\n\
             preview 的条目会被 `patch::writes` 当 Map 合并种子，降级时那张表是出厂空表，\n\
             合并写回等于把用户已有条目整表抹掉。"
        );
    }
}
