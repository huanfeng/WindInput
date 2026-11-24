//! 辅助码 txt 文件 / 文本加载
//!
//! 行格式与 rime-lua-aux-code `aux_code` 目录一致用 **`=` 分隔**（UTF-8，每行一条
//! `字=码`，同一汉字多编码分列多行，如 `阿=ek` / `厑=ib` / `厑=ii`）；空行与 `#`
//! 注释行跳过。`=` 右侧也可**按空白写多个码**（`七=a p`，手心表的写法），与分列多行等价。
//! 文件必须是 UTF-8：非 UTF-8（如 GBK）整张忽略并告警「请转存为 UTF-8」。
//!
//! ## 名称（唯一解析的元数据）
//! 只从**文件第 1 行**读取方法名（`# name: 笔画` 或 `#name: 笔画`，`#` 后空格可有可无、
//! value 两侧去空白）。第 1 行不是名字就不解析元数据，`load_from_file` 回落文件
//! 主干名（`stroke.txt` → `stroke`）。version / source 等一律当普通注释。
//!
//! 本模块只负责把外部文本 / 文件变成 [`AuxCodeTable`]，不接触表的内存布局——
//! 表的三段式存储、去重、合并语义全在 [`crate::table`]。

use crate::table::AuxCodeTable;

/// 从辅助码 txt 文件构建**单张**码表。
///
/// 路径由调用方解析（用户目录同名文件优先），本 crate 不负责定位——与 `wind-reverse`
/// 同一约定。文件读不出来（路径错、无权限、非 UTF-8）返回空表并告警，不 panic。
///
/// 表很小，整体读入，不缓存 / mmap；懒加载时机由调用方控制。
pub fn load_from_file(path: &std::path::Path) -> AuxCodeTable {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let mut t = parse_str(&content);
            if t.name.is_empty() {
                t.name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
            t
        }
        // 非 UTF-8（手心等表常见 GBK）：全仓不带 GBK 解码依赖，不为一张表引入 `encoding_rs`，
        // 明确告诉用户怎么办，产出空表（筛选随之 passthrough）。
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            tracing::warn!(
                "辅助码文件不是 UTF-8 编码，已忽略，请转存为 UTF-8: {}",
                path.display()
            );
            AuxCodeTable::new()
        }
        Err(e) => {
            tracing::warn!("读取辅助码文件失败 {}: {}", path.display(), e);
            AuxCodeTable::new()
        }
    }
}

/// **合并加载多张码表**：`merge(paths.iter().map(load_from_file))`（先出现 = 高优）。
///
/// 供协调器懒加载时把相邻的文件来源坍缩成一张表（`aux_code_source.rs`；方案来源
/// `schema:<id>` 不经此处，见 [`crate::AuxCodeLookup`]）。空文件经 `merge` 自动跳过。
pub fn load_merged(paths: &[std::path::PathBuf]) -> AuxCodeTable {
    AuxCodeTable::merge(paths.iter().map(|p| load_from_file(p)))
}

/// 只读首行取码表名（`# name: 小鹤`）。设置页列可选码表时用：为了一个名字整张读入四万行不值。
pub fn read_name(path: &std::path::Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(f).read_line(&mut line).ok()?;
    parse_name_from_first_line(line.trim_start_matches('\u{feff}').trim_end())
}

/// 从首行提取 `# name: X` / `#name: X`（`#` 后空格可有可无，value 两侧去空白）。
fn parse_name_from_first_line(first_line: &str) -> Option<String> {
    let rest = first_line.trim_start().strip_prefix('#')?.trim_start();
    let (key, value) = rest.split_once(':')?;
    if key.trim().eq_ignore_ascii_case("name") {
        let v = value.trim();
        (!v.is_empty()).then(|| v.to_string())
    } else {
        None
    }
}

/// 解析辅助码文本（**`=` 分隔**，与 rime-lua-aux-code 行格式一致）：
///
/// 每行一条 `字=码`（字符在前、码在后；同一汉字多种编码分列多行），如
/// `阿=ek`、`厑=ib`、`厑=ii`。
///
/// - 空行与 `#` 注释行跳过；无 `=`、左侧非单字或右侧为空码的行整行跳过
/// - 右侧按空白拆成多个码（`七=a p` = `七=a` + `七=p`），行内 `#` 起的注释先剥掉
/// - 开头剥掉 UTF-8 BOM、孤立 `\r` 行尾折成 `\n`（见 [`wind_utils::text::normalize_input`]）：
///   否则首行字会被当成非单字跳掉，或整份文件被当成一行注释、静默得到 0 条
/// - **第 1 行**提取方法名（`# name: 笔画`，见模块文档）；其余元数据一律当注释
///
/// 纯解析，不接触文件系统。
pub(crate) fn parse_str(content: &str) -> AuxCodeTable {
    let normalized = wind_utils::text::normalize_input(content);
    let content = normalized.as_ref();
    let mut name = String::new();
    let mut rows: Vec<(char, &str)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if i == 0
            && let Some(n) = parse_name_from_first_line(line)
        {
            name = n;
        }
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((head, code)) = line.split_once('=') else {
            continue; // 无 =：不是 `字=码` 形态
        };
        let head = head.trim();
        // 行内 `#` 注释先剥掉再拆码（码只有字母，`#` 不会是码的一部分）：否则 `七=a # 注`
        // 按空白一拆，`#`、`注` 都成了码。
        let code = code.split('#').next().unwrap_or_default().trim();
        if code.is_empty() {
            continue; // 空码 = 没码
        }
        let mut chars = head.chars();
        let Some(c) = chars.next() else {
            continue;
        };
        if chars.next().is_some() {
            continue; // 等号左侧必须是单个汉字，多字行跳过
        }
        // 一行多码：`七=a p`（手心表的写法）按空白拆开，与分列多行等价。
        rows.extend(code.split_whitespace().map(|code| (c, code)));
    }
    AuxCodeTable::from_rows(rows).with_name(name)
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
