//! 快捷输入的**用户设置文件**：设置页「导入 / 导出」读写的那份 TOML。
//!
//! ## 与 `system.quick.toml` 的分工
//!
//! ```text
//! system.quick.toml   基表：格式模板与出厂顺序（出厂，或高级用户整份覆盖）
//! 本文件              用户改动：调序 / 停用（+ 将来的自定义条目）
//! ```
//!
//! **只导出改动、不导出完整表**是刻意的：换机导入后，出厂新增的格式仍会自然生效；
//! 若导出完整表，就把当前版本的出厂内容固化进了文件，日后升级新增的格式在导入后
//! **永远不会出现**，而用户完全无从察觉（他只会觉得新版本没带来新写法）。
//!
//! 这与 [`crate::FormatAdjust`] 存稀疏 `(id, position)` 而非完整 id 序列是同一条理由。
//!
//! ## 序列化手写而非 `toml::to_string`
//!
//! 这份文件是**给人看、也能手改**的（与出厂文件同族），需要头部注释与稳定的引号风格，
//! 而 serde 的 TOML 输出带不了注释。解析反过来走 serde（容错与错误信息都更好）。
//! 与 `wind-store` 的 wdict 一样：导出手写、解析用解析器。

use crate::{FormatAdjust, FormatEntry, FormatKind, validate_format_text};

/// 一份用户设置（导入/导出的内存形态）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserSettings {
    /// 按类别的调整。同一 kind 只出现一次。
    pub adjust: Vec<(FormatKind, FormatAdjust)>,
    /// 用户自定义条目。
    ///
    /// P1 阶段尚无存储落点，导入时**如实报告为「已忽略 N 条」**而不是静默丢弃——
    /// 静默丢弃的话，用户从新版本导出、在旧版本导入，会以为自定义格式带过来了。
    pub formats: Vec<FormatEntry>,
}

impl UserSettings {
    pub fn is_empty(&self) -> bool {
        self.formats.is_empty() && self.adjust.iter().all(|(_, a)| a.is_empty())
    }
}

/// 解析结果：设置本体 + **被跳过的条目及原因**。
///
/// 跳过必须能报到用户眼前（导入预览会列出来）。「静默截断」在本仓是明令禁止的——
/// 用户看到「导入成功」却少了两条规则，无从判断是文件坏了还是程序吃了。
#[derive(Debug, Clone, Default)]
pub struct ParseOutcome {
    pub settings: UserSettings,
    pub skipped: Vec<String>,
}

/// 文件头注释。导出的文件应当自解释：用户过几个月打开它还能看懂。
const HEADER: &str = "\
# WindInput 快捷输入 · 用户设置
#
# 本文件只记录**你的改动**（调序 / 停用 / 自定义条目），不含出厂格式表本身。
# 因此升级到新版本后，出厂新增的写法照样会出现——导出完整表就没有这个好处了。
#
# 格式模板与出厂顺序在 system.quick.toml，与本文件各管一段，互不覆盖。
# 可在设置页「词库管理 → 快捷输入」里导入本文件，也可手工编辑后再导入。
";

/// 序列化为 TOML 文本。
pub fn serialize_user_settings(s: &UserSettings) -> String {
    let mut out = String::from(HEADER);
    if !s.formats.is_empty() {
        out.push_str("\n# 自定义条目\n");
        for e in &s.formats {
            out.push_str("\n[[formats]]\n");
            out.push_str(&format!("id = {}\n", toml_string(&e.id)));
            out.push_str(&format!("kind = {}\n", toml_string(e.kind.as_str())));
            out.push_str(&format!("text = {}\n", toml_string(&e.text)));
            out.push_str(&format!("position = {}\n", e.position));
        }
    }
    // 只写非空的类别：空记录写出去除了占地方没有别的作用。
    let adjust: Vec<_> = s.adjust.iter().filter(|(_, a)| !a.is_empty()).collect();
    if !adjust.is_empty() {
        out.push_str("\n# 调序与停用（position 是该类候选内的 0-based 下标）\n");
        for (kind, a) in adjust {
            out.push_str("\n[[adjust]]\n");
            out.push_str(&format!("kind = {}\n", toml_string(kind.as_str())));
            if !a.moved.is_empty() {
                // 顺序即 LIFO：第一条是最新的，导入时必须逆序重放（见 `apply` 侧注释）。
                let items: Vec<String> = a
                    .moved
                    .iter()
                    .map(|(id, pos)| format!("{{ id = {}, position = {} }}", toml_string(id), pos))
                    .collect();
                out.push_str(&format!("moved = [{}]\n", items.join(", ")));
            }
            if !a.disabled.is_empty() {
                let items: Vec<String> = a.disabled.iter().map(|d| toml_string(d)).collect();
                out.push_str(&format!("disabled = [{}]\n", items.join(", ")));
            }
        }
    }
    out
}

/// TOML 字符串字面量。
///
/// ⚠️ 优先用单引号字面量（与出厂文件风格一致、无需转义反斜杠），但模板里**确实会出现
/// 单引号**——`{amt(unit='圆')}` 就是出厂就有的写法，而 TOML 的单引号字面量里
/// 无法转义单引号。故含单引号时改用双引号并转义。
///
/// 少了这道分支，含函数参数的自定义条目导出后就是一份语法错误的文件，且导出时不报错，
/// 只在导入时炸。
fn toml_string(s: &str) -> String {
    if s.contains('\'') {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        format!("'{s}'")
    }
}

/// 解析用户设置文件。整份语法错误返回 `Err`；单条非法只跳过该条并记入
/// [`ParseOutcome::skipped`]。
pub fn parse_user_settings(text: &str) -> Result<ParseOutcome, toml::de::Error> {
    let raw: RawFile = toml::from_str(text)?;
    let mut out = ParseOutcome::default();

    for r in raw.formats {
        // id 先查：它是后续所有报错信息里的定位手段，缺了的话那些报错会写成
        // 「条目 ：未知类别」这种没头没尾的样子。
        if r.id.trim().is_empty() {
            out.skipped.push("有一条自定义条目缺 id".to_string());
            continue;
        }
        let Some(kind) = FormatKind::parse(&r.kind) else {
            out.skipped
                .push(format!("条目 {}：未知类别 {}", r.id, r.kind));
            continue;
        };
        if let Err(e) = validate_format_text(kind, &r.text) {
            out.skipped.push(format!("条目 {}：{}", r.id, e));
            continue;
        }
        out.settings.formats.push(FormatEntry {
            id: r.id,
            kind,
            text: r.text,
            position: r.position.unwrap_or(0),
        });
    }

    for r in raw.adjust {
        let Some(kind) = FormatKind::parse(&r.kind) else {
            out.skipped.push(format!("调整段：未知类别 {}", r.kind));
            continue;
        };
        // 同一类别写了两段：合并而不是后者顶替前者——两段都是用户的意图，
        // 丢掉一段属于静默截断。
        let entry = match out.settings.adjust.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, a)) => a,
            None => {
                out.settings.adjust.push((kind, FormatAdjust::default()));
                &mut out.settings.adjust.last_mut().expect("刚 push").1
            }
        };
        for m in r.moved {
            if m.id.trim().is_empty() {
                out.skipped
                    .push(format!("{} 类的一条移动规则缺 id", kind.as_str()));
                continue;
            }
            entry.moved.push((m.id, m.position));
        }
        for d in r.disabled {
            if d.trim().is_empty() {
                continue;
            }
            entry.disabled.push(d);
        }
    }
    Ok(out)
}

#[derive(serde::Deserialize)]
struct RawFile {
    #[serde(default)]
    formats: Vec<RawFormat>,
    #[serde(default)]
    adjust: Vec<RawAdjust>,
}

#[derive(serde::Deserialize)]
struct RawFormat {
    #[serde(default)]
    id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    position: Option<i32>,
}

#[derive(serde::Deserialize)]
struct RawAdjust {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    moved: Vec<RawMove>,
    #[serde(default)]
    disabled: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RawMove {
    #[serde(default)]
    id: String,
    #[serde(default)]
    position: usize,
}

#[cfg(test)]
#[path = "user_file_tests.rs"]
mod tests;
