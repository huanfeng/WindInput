//! 启动时给 `data/` 下程序写出、却还没有说明行的数据文件补上第一行（设计
//! `user-dict-libraries-and-external-edit.md` §5）。文案与整文件重写时生成的那行相同；文案改过的旧
//! 说明行换成当前的。
//!
//! 在绑定 / 载入任何数据之前做：之后各处按新内容取指纹，引擎侧的文本目标按「文件变了」由文本
//! 重建段、未折回的操作留在 delta 里照常叠加，不丢。只读第一行判断，补过的文件再次启动不重读全文。
//! 不动：用户自放的分库（`dicts/` 下）、带 BOM 的、已有说明行的、词库 / 表格文本第一行是别的注释的
//! （用户自己写的）、解析不了的。

use std::io::BufRead;
use std::path::{Path, PathBuf};
use wind_userdata::text::{banner, fold};

/// 说明行的统一开头（见 `banner.rs` 的格式测试）。
const BANNER_PREFIX: &str = "# 清风输入法 · ";

/// 补说明行之前的格式检查（解析不了的文件原样留给用户修，§2.5 坏文件不被改动）。
#[derive(Clone, Copy)]
enum Check {
    /// `user.dict.yaml`（Rime 词库格式）。
    Words,
    /// 表格文本（Shadow / 短语 / 学到的数据 / 补全）。
    Table,
    Toml,
    Yaml,
    /// 字符类用户层：`---` 头部是 YAML，`...` 之后是成员列表。
    Charset,
    /// 只要是 UTF-8。
    Text,
}

/// 数据目录下 `rel`（相对路径分量）这个文件的说明行与检查方式；不归程序写的 ⇒ None。
fn classify(rel: &[&str]) -> Option<(&'static str, Check)> {
    Some(match rel {
        ["phrases.dict.yaml"] => (banner::PHRASES, Check::Table),
        ["completion.table.yaml"] => (banner::COMPLETION, Check::Table),
        ["common_chars.yaml"] => (banner::COMMON_CHARS, Check::Yaml),
        ["quick_format.toml"] => (banner::QUICK_FORMAT, Check::Toml),
        ["phrases.sys_hash"] => (banner::PHRASE_SYS_HASH, Check::Text),
        ["stats", f] if f.ends_with(".toml") => (banner::STATS, Check::Toml),
        ["charsets", f] if f.ends_with(".yaml") => (banner::CHARSETS, Check::Charset),
        [d, f] if !d.starts_with('.') => match *f {
            "user.dict.yaml" => (banner::USER_DICT, Check::Words),
            "shadow.dict.yaml" => (banner::SHADOW, Check::Table),
            "temp.dict.yaml" => (banner::TEMP, Check::Table),
            "draft.dict.yaml" => (banner::DRAFT, Check::Table),
            "freq.table.yaml" => (banner::FREQ, Check::Table),
            "assoc.table.yaml" => (banner::ASSOC, Check::Table),
            crate::user_dicts::META_FILE => (banner::USER_DICTS_META, Check::Toml),
            _ => return None,
        },
        _ => return None,
    })
}

/// 数据目录下 `rel` 是不是程序的数据文件（有说明行的那些，加上用户分库 `<方案>/dicts/*.dict.yaml`）。
pub(crate) fn is_data_file(rel: &[&str]) -> bool {
    classify(rel).is_some()
        || matches!(rel, [d, "dicts", f] if !d.starts_with('.') && f.ends_with(".dict.yaml"))
}

fn parses(bytes: &[u8], path: &Path, check: Check) -> bool {
    let Ok(s) = std::str::from_utf8(bytes) else {
        return false;
    };
    match check {
        Check::Words => wind_userdata::text::parse(bytes, path, false).is_ok(),
        Check::Table => wind_userdata::text::table::parse_table(bytes, path).is_ok(),
        Check::Toml => s.parse::<toml::Value>().is_ok(),
        Check::Yaml => serde_yaml::from_str::<serde_yaml::Value>(s).is_ok(),
        Check::Charset => {
            let head = s.split("\n...").next().unwrap_or("");
            let head = head.trim_start().trim_start_matches("---");
            serde_yaml::from_str::<serde_yaml::Value>(head).is_ok()
        }
        Check::Text => true,
    }
}

/// 第一行（含行尾 `\n`）：读不了 / 整个文件没有换行 ⇒ None。按行读到换行为止（长行不截断）。
fn first_line(path: &Path) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::BufReader::new(std::fs::File::open(path).ok()?)
        .read_until(b'\n', &mut buf)
        .ok()?;
    buf.ends_with(b"\n").then_some(buf)
}

/// 给一个文件补说明行；补了返回 true。
fn backfill_one(path: &Path, line: &str, check: Check) -> anyhow::Result<bool> {
    let Some(first) = first_line(path) else {
        return Ok(false);
    };
    // 已有说明行；词库 / 表格文本第一行是别的注释 ⇒ 用户自己写的，同按行补丁的规则不动（小表的
    // 旧文件第一行是程序写的格式说明，照补）
    let has_banner = first.starts_with(BANNER_PREFIX.as_bytes());
    let user_comment =
        !has_banner && first.starts_with(b"#") && matches!(check, Check::Words | Check::Table);
    let current = first.strip_suffix(b"\n").unwrap_or(&first);
    let current = current.strip_suffix(b"\r").unwrap_or(current);
    if first.starts_with("\u{feff}".as_bytes()) || user_comment || current == line.as_bytes() {
        return Ok(false);
    }
    let Some((bytes, _)) = fold::read_text(path)? else {
        return Ok(false);
    };
    if !parses(&bytes, path, check) {
        return Ok(false);
    }
    let nl: &[u8] = if first.ends_with(b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    // 文案改过的旧说明行换成当前的；没有说明行的补在最前面
    let rest = if has_banner {
        &bytes[first.len().min(bytes.len())..]
    } else {
        &bytes[..]
    };
    let out = [line.as_bytes(), nl, rest].concat();
    fold::commit_text(path, &out, Some(&fold::fingerprint(Some(&bytes))))?;
    Ok(true)
}

/// 扫一遍数据目录（`.backup/` 与分库目录不进），补齐说明行。单个文件失败只 WARN。返回补了几个。
pub(crate) fn backfill_banners(root: &Path) -> usize {
    let mut files: Vec<PathBuf> = Vec::new();
    for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            for f in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                if f.file_type().is_ok_and(|t| t.is_file()) {
                    files.push(f.path());
                }
            }
        } else {
            files.push(p);
        }
    }
    files.sort();
    let mut n = 0;
    for p in files {
        let Ok(rel) = p.strip_prefix(root) else {
            continue;
        };
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        let Some((line, check)) = classify(&parts) else {
            continue;
        };
        match backfill_one(&p, line, check) {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!("{} 补说明行失败：{e:#}", p.display()),
        }
    }
    if n > 0 {
        tracing::info!("已给 {n} 个数据文件补上说明行");
    }
    n
}

#[cfg(test)]
#[path = "banner_backfill_tests.rs"]
mod tests;
