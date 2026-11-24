//! 缓存文件的**命名空间目录**：源文件在 `schemas/` 下的相对目录链。
//!
//! 词库缓存（`.wdat`/`.wridx`）与注释库缓存（`.wcmt`）不能把文件名干直接扔进缓存根，
//! 不同方案下的同名文件会撞在一起。命名空间取 `schemas` 之后的完整目录链而非直接父目录名：
//! 方案声明的 `rel` 可以多深都行（`ime_wubi86/word/x.dict.yaml`），只取父目录名会让两个方案的
//! `word/常用词` 撞到同一缓存、互相顶掉反复重建。扁平结构下两种算法结果相同。
//!
//! 从路径里回认 `schemas` 段而不是让调用方传 `rel`：缓存路径必须是源路径的纯函数，
//! `reader_pool` 的复用（同一文件必得同一 key）与各子引擎共用 `merged.wdat` 都依赖这一点。
//! 三层（data / user / custom）的词库根恒是 `<层根>/schemas`（见 `dict_layers`）。

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// 各层词库根的固定目录名。
const SCHEMAS_DIR_NAME: &str = "schemas";
/// 用户分库所在：`<用户目录>/data/<数据方案>/dicts/`。
const USER_DATA_DIR_NAME: &str = "data";
const USER_DICTS_DIR_NAME: &str = "dicts";

/// 源文件 → 缓存命名空间（相对目录链，可为空＝直接落缓存根）。
///
/// - `…/schemas/wubi86/x.dict.yaml` → `wubi86`
/// - `…/schemas/ime_wubi86/word/x.dict.yaml` → `ime_wubi86/word`
/// - `…/schemas/x.dict.yaml` → 空（词库直接躺在 schemas 根，无从分组）
/// - 路径里没有 `schemas` 段 → 回退到父目录名（测试夹具、便携版自定义根）
///
/// 只收 [`Component::Normal`] 段：`..`/盘符/根不会进入结果，缓存因此不可能被源路径里的
/// `../` 或 `X:name` 带出缓存根。
pub fn schema_namespace(source: &Path) -> PathBuf {
    let Some(dir) = source.parent() else {
        return PathBuf::new();
    };
    let segs: Vec<&OsStr> = dir
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();

    // 取最后一个 `schemas`：用户数据根本身可能也叫这个名字，靠后的那个才是词库根。
    match segs
        .iter()
        .rposition(|s| s.eq_ignore_ascii_case(SCHEMAS_DIR_NAME))
    {
        Some(i) => segs[i + 1..].iter().collect(),
        // 用户分库 `data/<数据方案>/dicts/`：带上数据方案，不同方案的同名分库不撞
        None if segs.len() >= 3
            && segs[segs.len() - 1] == USER_DICTS_DIR_NAME
            && segs[segs.len() - 3] == USER_DATA_DIR_NAME =>
        {
            segs[segs.len() - 3..].iter().collect()
        }
        // 没有 schemas 段：退回父目录名，免得测试夹具与便携版的缓存路径失去分组。
        None => segs.last().map(PathBuf::from).unwrap_or_default(),
    }
}

/// 命名空间 + 文件名 → 缓存路径。命名空间为空时直接落在 `cache_root` 下。
pub fn cache_path_in(cache_root: &Path, source: &Path, file_name: &str) -> PathBuf {
    let ns = schema_namespace(source);
    if ns.as_os_str().is_empty() {
        cache_root.join(file_name)
    } else {
        cache_root.join(ns).join(file_name)
    }
}

/// 文件名干：剥掉 `.dict.yaml` 里 `.dict` 这个冗余中缀（`rime_frost.dict` → `rime_frost`）。
pub fn cache_stem(source: &Path) -> String {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    stem.strip_suffix(".dict").unwrap_or(&stem).to_string()
}

/// 用例里的路径字面量一律写成 `/` 分隔的相对路径：反斜杠在 Linux / macOS 上不是分隔符，
/// 整串会成为一个 `Component::Normal`、切不出 `schemas` 段。Windows 独有的分隔语义由
/// `windows_backslash_layout` 单独兜住。
#[cfg(test)]
#[path = "cache_ns_tests.rs"]
mod tests;
