//! 三方合并（设计 §8）。`base` 为上次同步版本（可能缺失），`local` 为本地当前，`remote` 为远端最新。
//! 本地一方恒为「后同步者」，同键冲突时本地胜出，被丢弃的值进冲突报告。

use std::collections::BTreeMap;

/// 同键冲突：保留 `kept`，丢弃 `dropped`（文本形式，仅供提示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub key: String,
    pub kept: String,
    pub dropped: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub content: Vec<u8>,
    pub conflicts: Vec<Conflict>,
}

/// 该路径是否有内容级合并（否则走「取新者 + 冲突副本」）。
pub fn is_mergeable(path: &str) -> bool {
    path.ends_with(".dict.yaml") || path.ends_with(".toml")
}

/// 合并；不可合并的类型或内容解析失败返回 None，由调用方退化为冲突副本。
pub fn merge(path: &str, base: Option<&[u8]>, local: &[u8], remote: &[u8]) -> Option<Merged> {
    let (l, r) = (
        std::str::from_utf8(local).ok()?,
        std::str::from_utf8(remote).ok()?,
    );
    let b = match base {
        Some(b) => Some(std::str::from_utf8(b).ok()?),
        None => None,
    };
    if path.ends_with(".dict.yaml") {
        Some(merge_dict(b, l, r))
    } else if path.ends_with(".toml") {
        merge_toml(b, l, r)
    } else {
        None
    }
}

// ───────────────────────── 逐行表 ─────────────────────────

struct Dict<'a> {
    header: Vec<&'a str>,
    body: Vec<&'a str>,
    trailing_nl: bool,
}

fn split_dict(s: &str) -> Dict<'_> {
    let trailing_nl = s.ends_with('\n');
    let lines: Vec<&str> = s.strip_suffix('\n').unwrap_or(s).split('\n').collect();
    let first = lines
        .first()
        .map(|l| l.trim_start_matches('\u{feff}').trim_end());
    if first == Some("---")
        && let Some(i) = lines.iter().skip(1).position(|l| l.trim_end() == "...")
    {
        let cut = i + 2;
        return Dict {
            header: lines[..cut].to_vec(),
            body: lines[cut..].to_vec(),
            trailing_nl,
        };
    }
    Dict {
        header: vec![],
        body: lines,
        trailing_nl,
    }
}

/// 头部 `columns: [code, text, weight]` 里权重列的位置，默认 2。
fn weight_col(header: &[&str]) -> usize {
    header
        .iter()
        .find_map(|l| l.trim().strip_prefix("columns:"))
        .and_then(|v| {
            v.trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .position(|c| c.trim() == "weight")
        })
        .unwrap_or(2)
}

/// 行的匹配键：「编码（去空白）+ 词」，与 `wind-userdata` 的 `text::format::normalize_key`
/// 同源但简化（不按方案小写：大小写不同的行保守地视为不同键）。注释/空行以整行为键。
fn row_key(line: &str) -> String {
    let core = line.trim_end_matches('\r');
    if core.trim().is_empty() || core.trim_start().starts_with('#') {
        return format!("\u{0}raw:{core}");
    }
    let mut f = core.split('\t');
    match (f.next(), f.next()) {
        (Some(code), Some(text)) => {
            let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
            format!("{code}\t{}", text.trim())
        }
        _ => format!("\u{0}raw:{core}"),
    }
}

type Rows = (Vec<String>, BTreeMap<String, Vec<String>>);

fn rows(body: &[&str]) -> Rows {
    let (mut order, mut map) = (Vec::new(), BTreeMap::<String, Vec<String>>::new());
    for l in body {
        let k = row_key(l);
        if !map.contains_key(&k) {
            order.push(k.clone());
        }
        map.entry(k).or_default().push((*l).to_string());
    }
    (order, map)
}

fn weight_of(lines: &[String], col: usize) -> i64 {
    lines
        .first()
        .and_then(|l| l.trim_end_matches('\r').split('\t').nth(col))
        .and_then(|w| w.trim().parse().ok())
        .unwrap_or(0)
}

fn merge_dict(base: Option<&str>, local: &str, remote: &str) -> Merged {
    let (l, r) = (split_dict(local), split_dict(remote));
    let b = base.map(split_dict);
    let col = weight_col(&l.header);
    let empty: Rows = (vec![], BTreeMap::new());
    let (lo, lm) = rows(&l.body);
    let (ro, rm) = rows(&r.body);
    let bm = b.as_ref().map(|d| rows(&d.body)).unwrap_or(empty);
    let bm = &bm.1;

    let mut conflicts = Vec::new();
    let header: &Vec<&str> = match &b {
        None => &l.header,
        Some(bd) if l.header == bd.header => &r.header,
        _ => &l.header,
    };
    // 头部双方都改且不同：取本地，但要报告，不能静默丢掉远端的头部修改。
    let base_header = b.as_ref().map(|d| &d.header);
    if l.header != r.header && base_header != Some(&r.header) && base_header != Some(&l.header) {
        conflicts.push(Conflict {
            key: "(词库头部)".into(),
            kept: l.header.join("\n"),
            dropped: r.header.join("\n"),
        });
    }

    let mut body: Vec<String> = Vec::new();
    for k in lo.iter().chain(ro.iter().filter(|k| !lm.contains_key(*k))) {
        let (lv, rv, bv) = (lm.get(k), rm.get(k), bm.get(k));
        let pick: Option<&Vec<String>> = if lv == rv || rv == bv {
            lv
        } else if lv == bv {
            rv
        } else {
            match (lv, rv) {
                (Some(a), Some(b)) => Some(if weight_of(b, col) > weight_of(a, col) {
                    b
                } else {
                    a
                }),
                (a, b) => a.or(b), // 一删一改：保留改
            }
        };
        if let Some(p) = pick {
            body.extend(p.iter().cloned());
        }
    }
    let mut text = header.join("\n");
    if !body.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&body.join("\n"));
    }
    if l.trailing_nl || r.trailing_nl {
        text.push('\n');
    }
    Merged {
        content: text.into_bytes(),
        conflicts,
    }
}

// ───────────────────────── TOML ─────────────────────────

fn merge_toml(base: Option<&str>, local: &str, remote: &str) -> Option<Merged> {
    let l: toml::Table = local.parse().ok()?;
    let r: toml::Table = remote.parse().ok()?;
    let b: toml::Table = match base {
        Some(s) => s.parse().ok()?,
        None => toml::Table::new(),
    };
    // 一方已包含另一方的全部修改时保留其原字节（含注释与排版）。
    let mut conflicts = Vec::new();
    let merged = merge_table(&b, &l, &r, "", &mut conflicts);
    let content = if merged == l {
        local.as_bytes().to_vec()
    } else if merged == r {
        remote.as_bytes().to_vec()
    } else {
        toml::to_string(&merged).ok()?.into_bytes()
    };
    Some(Merged { content, conflicts })
}

fn show(v: &toml::Value) -> String {
    v.to_string()
}

fn merge_table(
    b: &toml::Table,
    l: &toml::Table,
    r: &toml::Table,
    prefix: &str,
    conflicts: &mut Vec<Conflict>,
) -> toml::Table {
    let mut out = toml::Table::new();
    let keys: std::collections::BTreeSet<&String> =
        l.keys().chain(r.keys()).chain(b.keys()).collect();
    for k in keys {
        let (lv, rv, bv) = (l.get(k), r.get(k), b.get(k));
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        let pick: Option<toml::Value> = if lv == rv || rv == bv {
            lv.cloned()
        } else if lv == bv {
            rv.cloned()
        } else {
            match (lv, rv) {
                (Some(toml::Value::Table(lt)), Some(toml::Value::Table(rt))) => {
                    let empty = toml::Table::new();
                    let bt = match bv {
                        Some(toml::Value::Table(t)) => t,
                        _ => &empty,
                    };
                    Some(toml::Value::Table(merge_table(
                        bt, lt, rt, &path, conflicts,
                    )))
                }
                (Some(a), Some(c)) => {
                    conflicts.push(Conflict {
                        key: path,
                        kept: show(a),
                        dropped: show(c),
                    });
                    Some(a.clone())
                }
                (a, c) => a.or(c).cloned(), // 一删一改：保留改
            }
        };
        if let Some(v) = pick {
            out.insert(k.clone(), v);
        }
    }
    out
}

// ───────────────────────── 冲突副本命名 ─────────────────────────

/// `<name>.conflict-<设备>-<日期>.<ext>`；无扩展名则追加在末尾。
pub fn conflict_copy_name(path: &str, device: &str, date: &str) -> String {
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let tag = format!(".conflict-{device}-{date}");
    match file.rfind('.') {
        Some(i) if i > 0 => format!("{dir}{}{tag}{}", &file[..i], &file[i..]),
        _ => format!("{dir}{file}{tag}"),
    }
}

/// unix 秒 → `YYYY-MM-DD`（UTC，公历）。
pub fn date_string(unix: u64) -> String {
    let z = (unix / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "---\nname: u\ncolumns: [code, text, weight]\n...\n";

    fn dict(rows: &[&str]) -> String {
        format!("{H}{}\n", rows.join("\n"))
    }
    fn m(base: Option<&str>, l: &str, r: &str) -> String {
        String::from_utf8(
            merge(
                "x.dict.yaml",
                base.map(str::as_bytes),
                l.as_bytes(),
                r.as_bytes(),
            )
            .unwrap()
            .content,
        )
        .unwrap()
    }

    #[test]
    fn dict_header_edited_on_both_sides_is_reported() {
        let b = dict(&["a\t啊\t1"]);
        let l = b.replace("name: u", "name: local");
        let r = b.replace("name: u", "name: remote");
        let out = merge(
            "x.dict.yaml",
            Some(b.as_bytes()),
            l.as_bytes(),
            r.as_bytes(),
        )
        .unwrap();
        assert!(
            String::from_utf8(out.content)
                .unwrap()
                .contains("name: local")
        );
        assert_eq!(out.conflicts.len(), 1, "头部双改要记入冲突");
    }

    #[test]
    fn dict_union_of_adds() {
        let b = dict(&["a\t啊\t1"]);
        let l = dict(&["a\t啊\t1", "b\t吧\t1"]);
        let r = dict(&["a\t啊\t1", "c\t从\t1"]);
        let out = m(Some(&b), &l, &r);
        assert_eq!(out, dict(&["a\t啊\t1", "b\t吧\t1", "c\t从\t1"]));
    }

    #[test]
    fn dict_same_key_takes_larger_weight() {
        let b = dict(&["a\t啊\t1"]);
        let l = dict(&["a\t啊\t5"]);
        let r = dict(&["a\t啊\t9"]);
        assert_eq!(m(Some(&b), &l, &r), dict(&["a\t啊\t9"]));
        assert_eq!(m(Some(&b), &r, &l), dict(&["a\t啊\t9"]));
    }

    #[test]
    fn dict_delete_vs_edit_keeps_edit() {
        let b = dict(&["a\t啊\t1", "b\t吧\t1"]);
        let l = dict(&["b\t吧\t1"]); // 删 a
        let r = dict(&["a\t啊\t7", "b\t吧\t1"]); // 改 a
        assert_eq!(m(Some(&b), &l, &r), dict(&["b\t吧\t1", "a\t啊\t7"]));
    }

    #[test]
    fn dict_plain_delete_propagates() {
        let b = dict(&["a\t啊\t1", "b\t吧\t1"]);
        let l = dict(&["b\t吧\t1"]);
        assert_eq!(m(Some(&b), &l, &b), dict(&["b\t吧\t1"]));
    }

    #[test]
    fn dict_key_ignores_code_spaces() {
        let b = dict(&["ni hao\t你好\t1"]);
        let l = dict(&["ni hao\t你好\t1"]);
        let r = dict(&["nihao\t你好\t3"]);
        assert_eq!(m(Some(&b), &l, &r), dict(&["nihao\t你好\t3"]));
    }

    #[test]
    fn dict_header_follows_changed_side() {
        let b = dict(&["a\t啊\t1"]);
        let r = b.replace("name: u", "name: v");
        let l = dict(&["a\t啊\t1", "b\t吧\t1"]);
        let out = m(Some(&b), &l, &r);
        assert!(out.contains("name: v") && out.contains("b\t吧"));
    }

    #[test]
    fn dict_no_base_is_union() {
        let l = dict(&["a\t啊\t1"]);
        let r = dict(&["b\t吧\t1"]);
        assert_eq!(m(None, &l, &r), dict(&["a\t啊\t1", "b\t吧\t1"]));
    }

    #[test]
    fn toml_key_level_merge_and_conflict() {
        let b = "[a]\nx = 1\ny = 1\n[b]\nz = 1\n";
        let l = "[a]\nx = 2\ny = 1\n[b]\nz = 1\n";
        let r = "[a]\nx = 3\ny = 9\n[b]\nz = 1\nw = 4\n";
        let o = merge(
            "config.toml",
            Some(b.as_bytes()),
            l.as_bytes(),
            r.as_bytes(),
        )
        .unwrap();
        let t: toml::Table = std::str::from_utf8(&o.content).unwrap().parse().unwrap();
        assert_eq!(t["a"]["x"].as_integer(), Some(2)); // 本地胜
        assert_eq!(t["a"]["y"].as_integer(), Some(9));
        assert_eq!(t["b"]["w"].as_integer(), Some(4));
        assert_eq!(o.conflicts.len(), 1);
        assert_eq!(o.conflicts[0].key, "a.x");
        assert_eq!(o.conflicts[0].dropped, "3");
    }

    #[test]
    fn toml_subsumed_side_keeps_original_bytes() {
        let b = "x = 1\n";
        let l = "# 我的注释\nx = 1\ny = 2\n";
        let r = "x = 1\n";
        let o = merge("c.toml", Some(b.as_bytes()), l.as_bytes(), r.as_bytes()).unwrap();
        assert_eq!(o.content, l.as_bytes());
    }

    #[test]
    fn toml_bad_input_is_not_merged() {
        assert!(merge("c.toml", None, b"x = 1", b"= =").is_none());
        assert!(merge("themes/a.json", None, b"{}", b"{}").is_none());
    }

    #[test]
    fn copy_name_and_date() {
        assert_eq!(
            conflict_copy_name("themes/a/b.json", "dev1", "2026-11-24"),
            "themes/a/b.conflict-dev1-2026-11-24.json"
        );
        assert_eq!(conflict_copy_name("x", "d", "t"), "x.conflict-d-t");
        assert_eq!(date_string(0), "1970-01-01");
        assert_eq!(date_string(1_795_000_000), "2026-11-18");
    }
}
