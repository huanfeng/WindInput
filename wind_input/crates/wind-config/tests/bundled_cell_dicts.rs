//! 随包附带的 rime-frost 细胞词库：构建脚本的文件清单与出厂方案里的 `cell_*` 声明必须一致。
//!
//! 清单写在四处——`scripts/dev.sh` / `scripts/mac/dev.sh` 的 `FROST_CELLS`、`scripts/dev.ps1` 的
//! `$FrostCells`、全拼 / 双拼方案的 `[[dictionaries]]`。漏一处的后果都是静默的：
//! - 脚本多发、方案没声明：靠发现目录照样出现，但名称是英文文件名；
//! - 方案声明了、脚本没发：设置里列着一张勾上也没词的库。
//!
//! 另钉「出厂不勾选」：细胞库是用户主动开的（docs/design/schema-dict-discovery.md §7.1 / §8）。
//!
//! ⚠️ 读仓库里的真实出厂文件，不是夹具。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use wind_config::schema::Schema;

const CELL_DIR: &str = "pinyin/cn_dicts_cell";

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/wind_input/crates/wind-config
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("仓库根")
        .to_path_buf()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("读 {rel}: {e}"))
}

/// `FROST_CELLS="a b c"`（bash）
fn bash_cells(rel: &str) -> BTreeSet<String> {
    let src = read(rel);
    let line = src
        .lines()
        .find(|l| l.starts_with("FROST_CELLS=\""))
        .unwrap_or_else(|| panic!("{rel} 里找不到 FROST_CELLS"));
    line["FROST_CELLS=\"".len()..]
        .trim_end_matches('"')
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// `$FrostCells = @('a', 'b',\n 'c')`（PowerShell，可跨行）
fn ps_cells(rel: &str) -> BTreeSet<String> {
    let src = read(rel);
    let start = src
        .find("$FrostCells = @(")
        .unwrap_or_else(|| panic!("{rel} 里找不到 $FrostCells"));
    let body = &src[start + "$FrostCells = @(".len()..];
    let body = &body[..body.find(')').expect("$FrostCells 缺右括号")];
    body.split(',')
        .map(|s| s.trim().trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn schema(rel: &str) -> Schema {
    toml::from_str(&read(rel)).unwrap_or_else(|e| panic!("解析 {rel}: {e}"))
}

#[test]
fn build_scripts_ship_the_same_cells() {
    let sh = bash_cells("scripts/dev.sh");
    assert!(!sh.is_empty());
    assert_eq!(
        bash_cells("scripts/mac/dev.sh"),
        sh,
        "mac/dev.sh 与 dev.sh 不一致"
    );
    assert_eq!(ps_cells("scripts/dev.ps1"), sh, "dev.ps1 与 dev.sh 不一致");
}

#[test]
fn pinyin_schemas_declare_every_shipped_cell_disabled_with_a_label() {
    let shipped = bash_cells("scripts/dev.sh");
    for rel in [
        "data/schemas/pinyin.schema.toml",
        "data/schemas/shuangpin.schema.toml",
    ] {
        let s = schema(rel);
        assert!(
            s.dictionary_dirs.iter().any(|d| d.path == CELL_DIR),
            "{rel} 没声明发现目录 {CELL_DIR}"
        );
        let cells: Vec<_> = s
            .dictionaries
            .iter()
            .filter(|d| d.path.starts_with(&format!("{CELL_DIR}/")))
            .collect();
        let declared: BTreeSet<String> = cells
            .iter()
            .map(|d| {
                d.path[CELL_DIR.len() + 1..]
                    .strip_suffix(".dict.yaml")
                    .unwrap_or_else(|| panic!("{rel}: {} 不是 .dict.yaml", d.path))
                    .to_string()
            })
            .collect();
        assert_eq!(declared, shipped, "{rel} 的 cell_* 声明与随包清单不一致");
        for d in cells {
            assert!(!d.label.is_empty(), "{rel}: {} 没写名称", d.id);
            assert_eq!(d.dict_type, "rime_pinyin", "{rel}: {}", d.id);
            assert!(!d.default, "{rel}: {} 不能是主库", d.id);
            assert_ne!(
                d.default_enabled,
                Some(true),
                "{rel}: {} 出厂应不勾选",
                d.id
            );
            assert_eq!(d.enabled, None, "{rel}: {} 出厂不该写 enabled", d.id);
        }
    }
}
