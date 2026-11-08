//! 渲染与按行补丁写回（设计 §3.5 文本目标第 1 步）。
//!
//! 补丁只改被操作的行，其余字节（BOM、换行风格、注释、空行、行尾空白、自定义顺序）原样保留；
//! 文件直接以 `---` 开头时最前面补一行说明（[`super::banner::prepend_if_bare`]）。
//! 操作按给出顺序依次作用到「文件行 + 本轮新增行」的内存模型上，最后统一输出：
//! - `Upsert`：已有同键行 ⇒ 第一行的权重取 `max(旧, 新)`（无权重列则不动），拼音文件里该行码
//!   无空格（边界未知）而操作带边界时把码改写成带空格的；没有 ⇒ 追加到末尾；
//! - `Insert`：没有同键行才追加，权重列留空（用户分库：没给权重就按方案默认）；有 ⇒ 空操作；
//! - `Delete`：删所有同键行（含本轮新增的）；
//! - `SetWeight`：只改第一行；早于文件修改时间的视为被手改覆盖，跳过；没有该行 ⇒ 空操作。
//!
//! 匹配键只经 [`normalize_key`]。

use super::format::{ParseError, normalize_key, parse};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum PatchOp {
    Upsert {
        code: String,
        text: String,
        boundary: u64,
        weight: i32,
    },
    Insert {
        code: String,
        text: String,
        boundary: u64,
    },
    Delete {
        code: String,
        text: String,
    },
    SetWeight {
        code: String,
        text: String,
        weight: i32,
        ts_secs: i64,
    },
}

fn header(banner: &str, name: &str, syllable_codes: bool) -> String {
    let mut h = format!(
        "{banner}\n---\nname: {name}\ncolumns: [code, text, weight]\nwind:\n  kind: words\n"
    );
    if syllable_codes {
        h.push_str("  code_spacing: syllable\n");
    }
    // 码表方案的码可能以 `#` 开头，不能被当成注释，所以正文一开头就关掉注释
    h.push_str("  escape: true\n...\n# no comment\n");
    h
}

/// 整文件渲染（迁移 / 清空 / 导入 / 新建）：说明行、LF、无 BOM、`columns: [code, text, weight]`。
pub fn render(name: &str, syllable_codes: bool, rows: &[(String, String, u64, i32)]) -> String {
    render_with(super::banner::USER_DICT, name, syllable_codes, rows)
}

/// 同 [`render`]，说明行由调用方给（用户分库）。
pub fn render_with(
    banner: &str,
    name: &str,
    syllable_codes: bool,
    rows: &[(String, String, u64, i32)],
) -> String {
    let mut s = header(banner, name, syllable_codes);
    for (code, text, boundary, weight) in rows {
        s.push_str(&wind_wdict::join_code_by_boundary(code, *boundary));
        s.push('\t');
        s.push_str(&wind_wdict::escape_text_field(text));
        s.push('\t');
        s.push_str(&weight.to_string());
        s.push('\n');
    }
    s
}

/// 本轮新增的一行（结构化保存，输出时按文件列序拼）。
struct Added {
    key: (String, String),
    code: String,
    boundary: u64,
    /// None：权重列留空（`Insert`）。
    weight: Option<i32>,
    alive: bool,
}

/// 改写一行的第 `col` 列；`\r` 结尾的行保持 `\r`。
fn with_col(line: &str, col: usize, value: &str) -> String {
    let (core, cr) = match line.strip_suffix('\r') {
        Some(c) => (c, "\r"),
        None => (line, ""),
    };
    let mut parts: Vec<&str> = core.split('\t').collect();
    if parts.len() <= col {
        parts.resize(col + 1, "");
    }
    parts[col] = value;
    format!("{}{cr}", parts.join("\t"))
}

fn with_weight(line: &str, weight_col: usize, w: i32) -> String {
    with_col(line, weight_col, &w.to_string())
}

fn weight_in(line: &str, weight_col: Option<usize>) -> i32 {
    weight_col
        .and_then(|i| line.trim_end_matches('\r').split('\t').nth(i))
        .and_then(|w| w.trim().parse().ok())
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
pub fn patch(
    original: &[u8],
    path: &Path,
    lowercase: bool,
    ops: &[PatchOp],
    file_mtime_secs: i64,
    new_file_name: &str,
    syllable_codes: bool,
) -> Result<Vec<u8>, ParseError> {
    patch_with_banner(
        original,
        path,
        lowercase,
        ops,
        file_mtime_secs,
        new_file_name,
        syllable_codes,
        Some(super::banner::USER_DICT),
    )
}

/// 同 [`patch`]；`banner` 为 None 时不给直接以 `---` 开头的文件补说明行（用户分库：用户自己
/// 放进来的文件不动）。
#[allow(clippy::too_many_arguments)]
pub fn patch_with_banner(
    original: &[u8],
    path: &Path,
    lowercase: bool,
    ops: &[PatchOp],
    file_mtime_secs: i64,
    new_file_name: &str,
    syllable_codes: bool,
    banner: Option<&str>,
) -> Result<Vec<u8>, ParseError> {
    let fresh: std::borrow::Cow<[u8]>;
    let original: &[u8] = if original.is_empty() {
        let b = banner.unwrap_or(super::banner::USER_DICT);
        fresh = header(b, new_file_name, syllable_codes).into_bytes().into();
        &fresh
    } else {
        fresh = super::banner::prepend_if_bare(original, banner);
        &fresh
    };
    let dict = parse(original, path, lowercase)?;
    let s = std::str::from_utf8(original).map_err(|_| ParseError::NotUtf8)?;
    let (bom, body) = match s.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", s),
    };
    let layout = wind_dict::rime_layout(body, path).ok_or(ParseError::NoBody)?;
    let ends_with_nl = body.ends_with('\n');
    let mut lines: Vec<Option<String>> = body.split('\n').map(|l| Some(l.to_string())).collect();
    if ends_with_nl {
        lines.pop();
    }
    let mut by_key: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for e in &dict.entries {
        by_key
            .entry((e.code.clone(), e.text.clone()))
            .or_default()
            .push(e.line);
    }
    let mut added: Vec<Added> = Vec::new();
    // 同键在一轮内至多一条活的新增行（Upsert 命中就合并），按键索引即可，免得大批追加成平方级
    let mut added_at: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for op in ops {
        let (code, text) = match op {
            PatchOp::Upsert { code, text, .. }
            | PatchOp::Insert { code, text, .. }
            | PatchOp::Delete { code, text }
            | PatchOp::SetWeight { code, text, .. } => (code, text),
        };
        let (flat, _, norm_text) = normalize_key(code, text, lowercase);
        let key = (flat.clone(), norm_text.clone());
        let first_line = by_key
            .get(&key)
            .and_then(|v| v.iter().copied().find(|i| lines[*i].is_some()));
        let first_added = added_at.get(&key).copied().filter(|&i| added[i].alive);
        match op {
            PatchOp::Upsert {
                boundary, weight, ..
            } => {
                if let Some(i) = first_line {
                    let mut line = lines[i].take().unwrap();
                    if let Some(wc) = layout.weight_col
                        && *weight > weight_in(&line, Some(wc))
                    {
                        line = with_weight(&line, wc, *weight);
                    }
                    let unknown_boundary = line
                        .trim_end_matches('\r')
                        .split('\t')
                        .nth(layout.code_col)
                        .is_some_and(|c| !c.trim().contains(' '));
                    if dict.header.syllable_codes && *boundary != 0 && unknown_boundary {
                        let spaced = wind_wdict::join_code_by_boundary(&flat, *boundary);
                        line = with_col(&line, layout.code_col, &spaced);
                    }
                    lines[i] = Some(line);
                } else if let Some(a) = first_added {
                    added[a].weight = Some(added[a].weight.map_or(*weight, |w| w.max(*weight)));
                    if added[a].boundary == 0 {
                        added[a].boundary = *boundary;
                    }
                } else {
                    added_at.insert(key.clone(), added.len());
                    added.push(Added {
                        key,
                        code: flat,
                        boundary: *boundary,
                        weight: Some(*weight),
                        alive: true,
                    });
                }
            }
            PatchOp::Insert { boundary, .. } => {
                if first_line.is_none() && first_added.is_none() {
                    added_at.insert(key.clone(), added.len());
                    added.push(Added {
                        key,
                        code: flat,
                        boundary: *boundary,
                        weight: None,
                        alive: true,
                    });
                }
            }
            PatchOp::Delete { .. } => {
                if let Some(v) = by_key.get(&key) {
                    for &i in v {
                        lines[i] = None;
                    }
                }
                if let Some(&i) = added_at.get(&key) {
                    added[i].alive = false;
                }
            }
            PatchOp::SetWeight {
                weight, ts_secs, ..
            } => {
                if *ts_secs < file_mtime_secs {
                    continue;
                }
                if let Some(i) = first_line {
                    if let Some(wc) = layout.weight_col {
                        let line = lines[i].as_ref().unwrap();
                        lines[i] = Some(with_weight(line, wc, *weight));
                    }
                } else if let Some(a) = first_added {
                    added[a].weight = Some(*weight);
                }
            }
        }
    }
    let nl = if dict.crlf { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(original.len() + added.len() * 32);
    out.push_str(bom);
    let kept: Vec<&String> = lines.iter().flatten().collect();
    for (i, l) in kept.iter().enumerate() {
        out.push_str(l);
        if i + 1 < kept.len() || ends_with_nl {
            out.push('\n');
        }
    }
    let alive: Vec<&Added> = added.iter().filter(|a| a.alive).collect();
    if !alive.is_empty() && !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    let ncols = layout
        .text_col
        .max(layout.code_col)
        .max(layout.weight_col.unwrap_or(0))
        + 1;
    let mut rows = Vec::with_capacity(alive.len());
    for a in alive {
        let mut cols = vec![String::new(); ncols];
        cols[layout.text_col] = if dict.header.escape {
            wind_wdict::escape_text_field(&a.key.1)
        } else {
            a.key.1.clone()
        };
        cols[layout.code_col] = wind_wdict::join_code_by_boundary(&a.code, a.boundary);
        if let (Some(wc), Some(w)) = (layout.weight_col, a.weight) {
            cols[wc] = w.to_string();
        }
        // 权重留空且在末列：不留行尾的空列
        let keep = layout.text_col.max(layout.code_col) + 1;
        while cols.len() > keep && cols.last().is_some_and(String::is_empty) {
            cols.pop();
        }
        rows.push(cols.join("\t"));
    }
    // 没关注释的旧文件：追加的行以 `#` 开头时先补一行 `# no comment`，否则它会被读成注释
    if layout.comment_cutoff.is_none() && rows.iter().any(|r| r.starts_with('#')) {
        out.push_str("# no comment");
        out.push_str(nl);
    }
    for r in rows {
        out.push_str(&r);
        out.push_str(nl);
    }
    Ok(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const HEAD_CRLF: &str = "\u{feff}# 我的词库\r\n---\r\nname: pinyin.user\r\ncolumns: [text, code, weight]\r\nwind:\r\n  kind: words\r\n  code_spacing: syllable\r\n  escape: true\r\n...\r\n";

    fn up(code: &str, text: &str, b: u64, w: i32) -> PatchOp {
        PatchOp::Upsert {
            code: code.into(),
            text: text.into(),
            boundary: b,
            weight: w,
        }
    }

    /// 直接以 `---` 开头的文件补丁后多出的说明行。
    fn banned(s: &str) -> String {
        format!("{}\n{s}", crate::text::banner::USER_DICT)
    }

    /// `Insert`：没有同键行才追加且权重列留空；已有就不动（权重不改）。
    #[test]
    fn insert_appends_blank_weight_and_keeps_existing() {
        let head = "---\nname: x\ncolumns: [text, code, weight]\n...\n工\ta\t5\n";
        let ins = |code: &str, text: &str| PatchOp::Insert {
            code: code.into(),
            text: text.into(),
            boundary: 0,
        };
        let out = |ops: &[PatchOp]| {
            String::from_utf8(
                patch_with_banner(
                    head.as_bytes(),
                    Path::new("x.dict.yaml"),
                    false,
                    ops,
                    0,
                    "x",
                    false,
                    None,
                )
                .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(out(&[ins("a", "工")]), head);
        assert_eq!(out(&[ins("b", "式")]), format!("{head}式\tb\n"));
        assert_eq!(
            out(&[ins("b", "式"), up("b", "式", 0, 3)]),
            format!("{head}式\tb\t3\n"),
            "同轮再 Upsert 给了权重"
        );
    }

    fn run(orig: &str, ops: &[PatchOp]) -> String {
        String::from_utf8(
            patch(
                orig.as_bytes(),
                Path::new("user.dict.yaml"),
                false,
                ops,
                1_000,
                "pinyin.user",
                true,
            )
            .unwrap(),
        )
        .unwrap()
    }

    /// Review Focus 1：除被改行外逐字节不变（BOM、CRLF、注释、空行、自定义顺序、行尾空白）。
    #[test]
    fn patch_preserves_everything_but_touched_lines() {
        let orig = format!(
            "{HEAD_CRLF}# 常用\r\n再见\tzai jian\t5  \r\n\r\n你好\tni hao\t3\r\n先\txian\t1\r\n"
        );
        let out = run(
            &orig,
            &[
                up("nihao", "你好", 0b101, 9),
                PatchOp::Delete {
                    code: "xian".into(),
                    text: "先".into(),
                },
                up("xinci", "新词", 0b1001, 7),
            ],
        );
        let want = format!(
            "{HEAD_CRLF}# 常用\r\n再见\tzai jian\t5  \r\n\r\n你好\tni hao\t9\r\n新词\txin ci\t7\r\n"
        );
        assert_eq!(out, want);
    }

    /// Review Focus 2：`ni hao` 行被扁平键 `nihao` 命中；重复行 Delete 全删、SetWeight 只改第一行。
    #[test]
    fn delete_hits_spaced_line_by_flat_key() {
        let orig = "---\nwind:\n  code_spacing: syllable\n...\n你好\tni hao\t3\n你好\tnihao\t4\n好\thao\t1\n";
        let out = run(
            orig,
            &[PatchOp::Delete {
                code: "nihao".into(),
                text: "你好".into(),
            }],
        );
        assert_eq!(
            out,
            banned("---\nwind:\n  code_spacing: syllable\n...\n好\thao\t1\n")
        );
        let out = run(
            "---\n...\n好\thao\t1\n好\thao\t1\n",
            &[PatchOp::SetWeight {
                code: "hao".into(),
                text: "好".into(),
                weight: 8,
                ts_secs: 2_000,
            }],
        );
        assert_eq!(out, banned("---\n...\n好\thao\t8\n好\thao\t1\n"));
    }

    #[test]
    fn upsert_takes_max_and_setweight_older_than_file_is_skipped() {
        let orig = "---\n...\n好\thao\t10\n";
        assert_eq!(
            run(orig, &[up("hao", "好", 0, 3)]),
            banned(orig),
            "Upsert 取 max：旧 10 > 新 3 不变"
        );
        let stale = PatchOp::SetWeight {
            code: "hao".into(),
            text: "好".into(),
            weight: 1,
            ts_secs: 500,
        };
        assert_eq!(
            run(orig, &[stale]),
            banned(orig),
            "早于文件 mtime 的 SetWeight 视为被手改覆盖"
        );
    }

    /// Review Focus 5：边界未知的词写回无空格码，再解析边界仍为 0（单个汉字除外，见
    /// `format::single_han_char_without_spaces_is_one_syllable`）。
    #[test]
    fn unknown_boundary_roundtrips_as_zero() {
        let out = run(
            "",
            &[up("xianning", "咸宁", 0, 1), up("xian", "先", 0b1, 1)],
        );
        let d = crate::text::parse(out.as_bytes(), Path::new("u"), false).unwrap();
        let b: Vec<(String, u64)> = d
            .entries
            .iter()
            .map(|e| (e.code.clone(), e.boundary))
            .collect();
        assert_eq!(
            b,
            vec![("xianning".into(), 0), ("xian".into(), 0b1)],
            "多音节未知仍为 0；单个汉字由解析推断为单音节"
        );
        assert!(
            out.starts_with(&format!(
                "{}\n---\nname: pinyin.user\n",
                crate::text::banner::USER_DICT
            )),
            "空文件先渲染说明行与头"
        );
    }

    #[test]
    fn escape_and_missing_trailing_newline() {
        let out = run(
            "---\nwind:\n  escape: true\n...\n好\thao\t1",
            &[up("cn", "C:\\note", 0, 1)],
        );
        assert_eq!(
            out,
            banned("---\nwind:\n  escape: true\n...\n好\thao\t1\nC:\\\\note\tcn\t1\n")
        );
    }

    #[test]
    fn render_writes_a_complete_file() {
        let s = render(
            "pinyin.user",
            true,
            &[("nihao".into(), "你好".into(), 0b101, 3)],
        );
        assert!(
            s.contains("columns: [code, text, weight]") && s.contains("code_spacing: syllable")
        );
        assert!(s.ends_with("...\n# no comment\nni hao\t你好\t3\n"));
        assert!(s.starts_with(&format!("{}\n---\n", crate::text::banner::USER_DICT)));
    }

    /// 说明行：直接以 `---` 开头的文件补上（行尾随文件）；第一行是注释（含用户自己的）或带 BOM ⇒ 原样不动。
    #[test]
    fn patch_keeps_the_first_line_as_is() {
        let custom = "# 我自己的说明\n---\n...\n好\thao\t1\n";
        assert_eq!(
            run(custom, &[up("ni", "你", 0b1, 2)]),
            "# 我自己的说明\n---\n...\n好\thao\t1\n你\tni\t2\n"
        );
        let bare = "---\n...\n好\thao\t1\n";
        assert_eq!(
            run(bare, &[up("ni", "你", 0b1, 2)]),
            format!(
                "{}\n---\n...\n好\thao\t1\n你\tni\t2\n",
                crate::text::banner::USER_DICT
            ),
            "直接以 --- 开头的文件补上说明行"
        );
        let crlf = "---\r\n...\r\n好\thao\t1\r\n";
        assert_eq!(
            run(crlf, &[up("ni", "你", 0b1, 2)]),
            format!(
                "{}\r\n---\r\n...\r\n好\thao\t1\r\n你\tni\t2\r\n",
                crate::text::banner::USER_DICT
            ),
            "CRLF 文件的说明行也用 CRLF"
        );
        let bom = "\u{feff}---\n...\n好\thao\t1\n";
        assert!(
            run(bom, &[up("ni", "你", 0b1, 2)]).starts_with("\u{feff}---\n"),
            "带 BOM 不补"
        );
    }

    /// 大批追加线性：6 万条新词一轮补丁（导入 / 迁移走这条）不能是平方级。
    #[test]
    fn appending_many_rows_is_linear() {
        let ops: Vec<PatchOp> = (0..60_000)
            .map(|i| up(&format!("c{i:05}"), &format!("词{i:05}"), 0, 1))
            .collect();
        let t = std::time::Instant::now();
        let out = run("", &ops);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(3),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(out.lines().filter(|l| l.contains('\t')).count(), 60_000);
    }

    /// 用户分库：整写用分库的说明行；删行补丁不给用户自己放进来的文件补说明行。
    #[test]
    fn lib_banner_on_render_and_untouched_on_patch() {
        let banner = crate::text::banner::USER_DICT_LIB;
        let out = render_with(
            banner,
            "\"成语\"",
            false,
            &[("a".into(), "工".into(), 0, 3)],
        );
        assert!(
            out.starts_with(&format!("{banner}\n---\nname: \"成语\"\n")),
            "{out}"
        );
        let orig = "---\n...\n工\ta\t3\n式\taa\t1\n";
        let del = PatchOp::Delete {
            code: "a".into(),
            text: "工".into(),
        };
        let got = patch_with_banner(
            orig.as_bytes(),
            Path::new("x.dict.yaml"),
            false,
            &[del],
            0,
            "x",
            false,
            None,
        )
        .unwrap();
        assert_eq!(String::from_utf8(got).unwrap(), "---\n...\n式\taa\t1\n");
    }
}
