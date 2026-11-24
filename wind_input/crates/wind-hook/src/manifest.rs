// SPDX-License-Identifier: MPL-2.0
//! 扩展清单 `<extensions>/<id>/extension.toml`。
//!
//! ```toml
//! name = "按键音"
//! exe = "keysound"                       # 目录内相对路径；或按平台分写：
//! # exe = { windows = "keysound.exe", linux = "keysound", macos = "keysound" }
//! args = []
//! want = ["key.detail", "mode"]          # 只是申请，实际以核心配置里的授权为准
//! protocol = [1]
//!
//! [[option]]                             # 可选：扩展自己的选项，设置端照此渲染，见 crate::options
//! key = "volume"
//! type = "number"
//! default = 0.6
//! min = 0.0
//! max = 1.0
//! ```
//!
//! 扩展 id 就是目录名，清单里不写——目录即身份，删目录即卸载。可执行文件必须落在扩展目录内。

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::options::{self, OptionSpec};

pub const MANIFEST_FILE: &str = "extension.toml";

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    /// 已校验、落在 `dir` 内的可执行文件绝对路径。
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub want: Vec<String>,
    pub protocol: Vec<u32>,
    pub options: Vec<OptionSpec>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ExeSpec {
    One(String),
    PerOs(BTreeMap<String, String>),
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    name: String,
    exe: ExeSpec,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    want: Vec<String>,
    #[serde(default)]
    protocol: Vec<u32>,
    #[serde(default)]
    option: Vec<OptionSpec>,
}

/// 扩展 id 只许小写字母、数字、`-`、`_`：它会进日志文件名与配置键。
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// 读取并校验一个扩展目录。
pub fn load(dir: &Path) -> Result<Manifest, String> {
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("目录名不是合法 UTF-8")?
        .to_string();
    if !valid_id(&id) {
        return Err(format!("扩展 id `{id}` 不合法（只许小写字母、数字、-、_）"));
    }
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE))
        .map_err(|e| format!("读 {MANIFEST_FILE} 失败: {e}"))?;
    let raw: Raw = toml::from_str(&text).map_err(|e| format!("{MANIFEST_FILE} 格式错误: {e}"))?;
    let rel = match raw.exe {
        ExeSpec::One(s) => s,
        ExeSpec::PerOs(m) => m
            .get(std::env::consts::OS)
            .cloned()
            .ok_or_else(|| format!("没有 {} 平台的 exe", std::env::consts::OS))?,
    };
    let exe = resolve_inside(dir, &rel)?;
    options::validate(&raw.option)?;
    Ok(Manifest {
        name: if raw.name.is_empty() {
            id.clone()
        } else {
            raw.name
        },
        id,
        dir: dir.to_path_buf(),
        exe,
        args: raw.args,
        want: raw.want,
        protocol: raw.protocol,
        options: raw.option,
    })
}

/// 把清单里的相对路径解析到扩展目录内；绝对路径、`..`、软链出界一律拒绝。
fn resolve_inside(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if rel.is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("exe `{rel}` 必须是扩展目录内的相对路径"));
    }
    let base = dir
        .canonicalize()
        .map_err(|e| format!("扩展目录不可访问: {e}"))?;
    let full = base
        .join(p)
        .canonicalize()
        .map_err(|e| format!("exe `{rel}` 不存在: {e}"))?;
    if !full.starts_with(&base) || !full.is_file() {
        return Err(format!("exe `{rel}` 不在扩展目录内或不是文件"));
    }
    Ok(full)
}

/// 扫描扩展根目录下的全部子目录。目录不存在视为没有扩展。
/// 返回 (id, 结果)，按 id 排序；目录名不合法的也列出来，便于设置端显示原因。
pub fn scan(root: &Path) -> Vec<(String, Result<Manifest, String>)> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<_> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            (id, load(&e.path()))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
