use super::*;
use std::path::{Path, PathBuf};

const COLS: &[&str] = &["code", "text", "action", "position", "cand_id"];

fn row(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wind_tbl_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 空首列（空编码）是数据行；省略的尾列补空串；注释与空行跳过；escape 反转义。
#[test]
fn parses_by_column_name_and_keeps_empty_code_rows() {
    let f = "---\ncolumns: [code, text, action, position, cand_id]\nwind: {kind: shadow, escape: true}\n...\n# 注释\n\naa\t恭\tpin\t0\n\t日期\tpin\t1\tphrase:rq:{date}\nbb\t见\\n了\tdelete\n";
    let d = parse_table(f.as_bytes(), Path::new("s")).unwrap();
    assert_eq!(d.kind.as_deref(), Some("shadow"));
    assert_eq!(col(&d, "action"), Some(2));
    let rows: Vec<Vec<&str>> = d
        .rows
        .iter()
        .map(|r| r.fields.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        rows,
        vec![
            vec!["aa", "恭", "pin", "0", ""],
            vec!["", "日期", "pin", "1", "phrase:rq:{date}"],
            vec!["bb", "见\n了", "delete", "", ""],
        ]
    );
}

/// 整写带按种类取的说明行（不认识的种类不写）；解析跳过它；补丁给直接以 `---` 开头的文件补上，第一行是注释的不动；空文件按新建写。
#[test]
fn banner_is_rendered_by_kind_and_kept_by_patch() {
    use crate::text::banner;
    let s = render_table(COLS, "shadow", &[row(&["aa", "恭", "pin", "0", ""])]);
    assert!(s.starts_with(&format!("{}\n---\n", banner::SHADOW)), "{s}");
    let d = parse_table(s.as_bytes(), Path::new("s")).unwrap();
    assert_eq!(d.kind.as_deref(), Some("shadow"));
    assert_eq!(d.rows.len(), 1);
    assert!(render_table(COLS, "other", &[]).starts_with("---\n"));
    let edit = [(row(&["bb"]), vec![row(&["bb", "八", "delete", "", ""])])];
    let out = |orig: &str| {
        String::from_utf8(
            patch_groups(
                orig.as_bytes(),
                Path::new("s"),
                &["code"],
                &edit,
                COLS,
                "shadow",
            )
            .unwrap(),
        )
        .unwrap()
    };
    let bare = "---\ncolumns: [code, text, action]\n...\naa\t恭\tpin\n";
    assert_eq!(
        out(bare),
        format!("{}\n{bare}bb\t八\tdelete\n", banner::SHADOW)
    );
    let crlf = bare.replace('\n', "\r\n");
    assert_eq!(
        out(&crlf),
        format!("{}\r\n{crlf}bb\t八\tdelete\r\n", banner::SHADOW)
    );
    let mine = format!("# 我的\n{bare}");
    assert_eq!(out(&mine), format!("{mine}bb\t八\tdelete\n"));
    assert!(out("").starts_with(&format!("{}\n---\n", banner::SHADOW)));
}

/// 按组替换：组内行整体换掉、位置不变；别的行、注释、CRLF、BOM 逐字节不变；新组追加。
#[test]
fn patch_groups_replaces_in_place_and_preserves_the_rest() {
    let orig = "\u{feff}---\r\ncolumns: [code, text, action, position, cand_id]\r\nwind: {kind: shadow, escape: true}\r\n...\r\n# 我的规则\r\naa\t恭\tpin\t0\t\r\naa\t工\tdelete\t\t\r\ncc\t从\tpin\t2\t\r\n";
    let out = patch_groups(
        orig.as_bytes(),
        Path::new("s"),
        &["code"],
        &[
            (row(&["aa"]), vec![row(&["aa", "共", "pin", "1", ""])]),
            (row(&["zz"]), vec![row(&["zz", "在", "delete", "", ""])]),
        ],
        COLS,
        "shadow",
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "\u{feff}---\r\ncolumns: [code, text, action, position, cand_id]\r\nwind: {kind: shadow, escape: true}\r\n...\r\n# 我的规则\r\naa\t共\tpin\t1\t\r\ncc\t从\tpin\t2\t\r\nzz\t在\tdelete\t\t\r\n"
    );
}

/// 新文件：渲染表头（含 `# no comment`），按给定列序输出；新行按文件列序（列序不同也对）。
#[test]
fn patch_into_empty_and_foreign_column_order() {
    let out = patch_groups(
        b"",
        Path::new("s"),
        &["code"],
        &[(row(&["#a"]), vec![row(&["#a", "井\t号", "pin", "0", ""])])],
        COLS,
        "shadow",
    )
    .unwrap();
    let d = parse_table(&out, Path::new("s")).unwrap();
    assert_eq!(d.rows[0].fields, row(&["#a", "井\t号", "pin", "0", ""]));
    let foreign = "---\ncolumns: [text, code, action]\nwind: {kind: shadow}\n...\n恭\taa\tpin\n";
    let out = patch_groups(
        foreign.as_bytes(),
        Path::new("s"),
        &["code"],
        &[(row(&["bb"]), vec![row(&["bb", "八", "delete", "", ""])])],
        COLS,
        "shadow",
    )
    .unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .ends_with("恭\taa\tpin\n八\tbb\tdelete\n")
    );
}

/// 写之前文件被手改过 ⇒ 先按文件重新解析，手改（注释、顺序、新增行）不丢；`# no comment` 之后
/// 不含 tab 的 `#` 行仍是注释。
#[test]
fn write_reparses_hand_edited_file_first() {
    let dir = tmpdir("handedit");
    let path = dir.join("t.dict.yaml");
    let t = TextTable::open(path.clone(), COLS, "shadow");
    t.edit(
        &[(row(&["aa"]), vec![row(&["aa", "恭", "pin", "0", ""])])],
        &["code"],
    )
    .unwrap();
    let mut s = std::fs::read_to_string(&path).unwrap();
    s.push_str("# 手写\nbb\t八\tdelete\t\t\n");
    std::fs::write(&path, &s).unwrap();
    t.edit(
        &[(row(&["cc"]), vec![row(&["cc", "从", "pin", "1", ""])])],
        &["code"],
    )
    .unwrap();
    let s = std::fs::read_to_string(&path).unwrap();
    assert!(
        s.contains("# 手写\nbb\t八\tdelete\t\t\ncc\t从\tpin\t1\t\n"),
        "{s}"
    );
    assert_eq!(t.doc().rows.len(), 3);
}

/// 头部坏 ⇒ 写入报错不覆盖；读按最后一次成功解析的内容；整表替换照样成功。
#[test]
fn isolated_table_refuses_writes_keeps_last_good() {
    let dir = tmpdir("isolated");
    let path = dir.join("t.dict.yaml");
    let t = TextTable::open(path.clone(), COLS, "shadow");
    t.edit(
        &[(row(&["aa"]), vec![row(&["aa", "恭", "pin", "0", ""])])],
        &["code"],
    )
    .unwrap();
    std::fs::write(&path, "---\ncolumns: [\n...\n").unwrap();
    assert!(!t.sync());
    assert_eq!(t.doc().rows.len(), 1, "保留上次成功解析的内容");
    assert!(t.edit(&[(row(&["bb"]), vec![])], &["code"]).is_err());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "---\ncolumns: [\n...\n",
        "不覆盖"
    );
    t.replace_all(&[row(&["cc", "从", "pin", "0", ""])])
        .unwrap();
    assert_eq!(t.doc().rows[0].fields[0], "cc");
    assert!(!t.sync(), "替换后指纹已记，无变化");
}

/// 命令栏文本（`$CC(...)` 等）里的 `\n` / `\t` 是字面序列，往返必须逐字不变——第一次保存就把
/// `C:\new` 读成「C: + 换行 + ew」是写坏用户数据（P3 审查 M1）。
#[test]
fn cmdbar_text_roundtrips_byte_exact() {
    let dir = tmpdir("cmdbar");
    let path = dir.join("t.dict.yaml");
    let t = TextTable::open(path.clone(), COLS, "shadow");
    let txt = "$CC(C:\\new\\test.txt)";
    t.edit(
        &[(row(&["cmd"]), vec![row(&["cmd", txt, "pin", "0", ""])])],
        &["code"],
    )
    .unwrap();
    assert_eq!(t.doc().rows[0].fields[1], txt);
    assert!(std::fs::read_to_string(&path).unwrap().contains(txt));
    let t2 = TextTable::open(path, COLS, "shadow");
    assert_eq!(t2.doc().rows[0].fields[1], txt);
}
