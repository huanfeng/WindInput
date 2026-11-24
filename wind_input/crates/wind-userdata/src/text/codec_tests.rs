use super::*;

fn e(
    code: &str,
    text: &str,
    w: i32,
    count: u32,
    created: i64,
    last: i64,
    boundary: u64,
) -> SnapshotEntry {
    SnapshotEntry {
        code: code.into(),
        text: text.into(),
        weight: w,
        boundary,
        count,
        created_at: created,
        last_used: last,
        flags: 0,
        order: 0,
    }
}

fn p(
    src: &str,
    spec: &TableSpec,
    edits: &BTreeMap<(String, String), Option<SnapshotEntry>>,
) -> String {
    String::from_utf8(patch(src.as_bytes(), Path::new("f"), spec, false, false, edits).unwrap())
        .unwrap()
}

#[test]
fn temp_round_trips_through_render_and_decode() {
    let v = vec![e("nihao", "你好", 3, 2, 1700000000, 1700000000, 0b101)];
    let s = render(&TEMP, true, &v);
    assert!(s.contains("ni hao\t你好\t3\t2\t1700000000\n"), "{s}");
    let d = decode(
        s.as_bytes(),
        Path::new("temp.dict.yaml"),
        &TEMP,
        false,
        true,
    )
    .unwrap();
    assert_eq!(d.bad_lines, 0);
    assert_eq!(d.entries.len(), 1);
    let got = &d.entries[0];
    assert_eq!((got.code.as_str(), got.text.as_str()), ("nihao", "你好"));
    assert_eq!((got.weight, got.count, got.created_at), (3, 2, 1700000000));
    assert_eq!(got.boundary, 0b101);
}

/// 整写带各自的说明行，解码照常；补丁给直接以 `---` 开头的文件补上说明行，第一行是注释的不动，空文件按新建写。
#[test]
fn banner_is_rendered_and_patch_keeps_the_first_line() {
    use crate::text::banner;
    for (spec, line) in [
        (&TEMP, banner::TEMP),
        (&DRAFT, banner::DRAFT),
        (&FREQ, banner::FREQ),
        (&ASSOC, banner::ASSOC),
    ] {
        let s = render(spec, false, &[]);
        assert!(s.starts_with(&format!("{line}\n---\n")), "{s}");
    }
    let v = vec![e("a", "啊", 0, 3, 0, 10, 0)];
    let s = render(&FREQ, false, &v);
    let d = decode(s.as_bytes(), Path::new("f"), &FREQ, false, false).unwrap();
    assert_eq!((d.entries.len(), d.bad_lines), (1, 0));
    let mut edits = BTreeMap::new();
    edits.insert(
        ("b".into(), "不".into()),
        Some(e("b", "不", 0, 1, 0, 11, 0)),
    );
    let old = render(&FREQ, false, &v).replacen(&format!("{}\n", banner::FREQ), "", 1);
    assert!(
        p(&old, &FREQ, &edits).starts_with(&format!("{}\n---\n", banner::FREQ)),
        "直接以 --- 开头的旧文件补上说明行"
    );
    let crlf = old.replace('\n', "\r\n");
    assert!(p(&crlf, &FREQ, &edits).starts_with(&format!("{}\r\n---\r\n", banner::FREQ)));
    let (_, d) =
        patch_decoded(old.as_bytes(), Path::new("f"), &FREQ, false, false, &edits).unwrap();
    assert_eq!(d.entries.len(), 2);
    let mine = format!("# 我的\n{old}");
    assert!(p(&mine, &FREQ, &edits).starts_with("# 我的\n---\n"));
    assert!(p("", &FREQ, &edits).starts_with(&format!("{}\n---\n", banner::FREQ)));
}

#[test]
fn assoc_context_is_kept_verbatim() {
    let v = vec![e("我们 的", "我们的事", 0, 5, 0, 1700000001, 0)];
    let s = render(&ASSOC, false, &v);
    let d = decode(
        s.as_bytes(),
        Path::new("assoc.table.yaml"),
        &ASSOC,
        true,
        false,
    )
    .unwrap();
    assert_eq!(d.entries[0].code, "我们 的", "上文不去空格、不小写");
    assert_eq!(
        (d.entries[0].count, d.entries[0].last_used),
        (5, 1700000001)
    );
}

#[test]
fn text_with_tab_and_hash_round_trips() {
    let v = vec![e("a", "#x\ty", 0, 1, 0, 1, 0)];
    let s = render(&FREQ, false, &v);
    let d = decode(s.as_bytes(), Path::new("f"), &FREQ, false, false).unwrap();
    assert_eq!(d.entries[0].text, "#x\ty");
}

#[test]
fn bad_rows_are_skipped_counted_and_left_untouched_by_patch() {
    let head = render(&FREQ, false, &[]);
    let src = format!("{head}a\t啊\t3\t10\nb\t不\tx\t10\n\t空码\t1\t1\n");
    let d = decode(
        src.as_bytes(),
        Path::new("freq.table.yaml"),
        &FREQ,
        false,
        false,
    )
    .unwrap();
    assert_eq!(d.entries.len(), 1);
    assert_eq!(d.bad_lines, 2);
    let mut edits = BTreeMap::new();
    edits.insert(
        ("a".to_string(), "啊".to_string()),
        Some(e("a", "啊", 0, 4, 0, 20, 0)),
    );
    let out = p(&src, &FREQ, &edits);
    assert!(out.contains("a\t啊\t4\t20\n"), "{out}");
    assert!(out.contains("b\t不\tx\t10\n"), "坏行原样保留");
    assert!(out.contains("\t空码\t1\t1\n"));
}

#[test]
fn patch_deletes_all_duplicates_and_appends_new_keys() {
    let head = render(&FREQ, false, &[]);
    let src = format!("{head}# 我的注释\na\t啊\t1\t1\na\t啊\t2\t2\n");
    let mut edits = BTreeMap::new();
    edits.insert(("a".to_string(), "啊".to_string()), None);
    edits.insert(
        ("b".to_string(), "不".to_string()),
        Some(e("b", "不", 0, 1, 0, 5, 0)),
    );
    let out = p(&src, &FREQ, &edits);
    assert!(!out.contains("啊"), "{out}");
    assert!(out.contains("# 我的注释\n"));
    assert!(out.ends_with("b\t不\t1\t5\n"), "{out}");
}

#[test]
fn patch_follows_file_column_order() {
    let src =
        "---\ncolumns: [text, code, last_used, count]\nwind: {kind: freq}\n...\n啊\ta\t1\t1\n";
    let mut edits = BTreeMap::new();
    edits.insert(
        ("a".to_string(), "啊".to_string()),
        Some(e("a", "啊", 0, 7, 0, 9, 0)),
    );
    assert!(p(src, &FREQ, &edits).ends_with("啊\ta\t9\t7\n"));
}

#[test]
fn patch_keeps_crlf_and_bom() {
    let head = render(&FREQ, false, &[]).replace('\n', "\r\n");
    let src = format!("\u{feff}{head}a\t啊\t1\t1\r\n");
    let mut edits = BTreeMap::new();
    edits.insert(
        ("a".to_string(), "啊".to_string()),
        Some(e("a", "啊", 0, 2, 0, 2, 0)),
    );
    edits.insert(
        ("b".to_string(), "不".to_string()),
        Some(e("b", "不", 0, 1, 0, 1, 0)),
    );
    let out = p(&src, &FREQ, &edits);
    assert!(out.starts_with('\u{feff}'));
    assert!(out.contains("a\t啊\t2\t2\r\n"), "{out:?}");
    assert!(out.ends_with("b\t不\t1\t1\r\n"), "{out:?}");
}

#[test]
fn patch_into_empty_file_writes_header() {
    let mut edits = BTreeMap::new();
    edits.insert(
        ("a".to_string(), "啊".to_string()),
        Some(e("a", "啊", 0, 1, 0, 1, 0)),
    );
    let out = p("", &FREQ, &edits);
    let d = decode(out.as_bytes(), Path::new("f"), &FREQ, false, false).unwrap();
    assert_eq!(d.entries.len(), 1);
    assert!(out.contains("kind: freq"));
}

/// 补丁顺带给出的条目必须与重新解析补丁结果逐项相同（含行号、坏行数）。
#[test]
fn patched_entries_equal_reparsing_the_output() {
    let head = render(&TEMP, true, &[]);
    let crlf_head = head.replace('\n', "\r\n");
    let cases = [
        format!("{head}你好\tni hao\t3\t2\t9\n坏\n# 注\n你好\tni hao\t1\t1\t1\n先\txian\t1\t1\t1"),
        format!("\u{feff}{crlf_head}啊\ta\t1\t1\t1\r\n"),
        "---\ncolumns: [text, code, weight, count, created_at]\nwind: {kind: temp}\n...\n啊\ta\t1\t1\t1\n".to_string(),
        String::new(),
    ];
    let mut edits = BTreeMap::new();
    edits.insert(
        ("nihao".to_string(), "你好".to_string()),
        Some(e("nihao", "你好", 9, 9, 9, 9, 0b101)),
    );
    edits.insert(("a".to_string(), "啊".to_string()), None);
    edits.insert(
        ("xin".to_string(), "#新".to_string()),
        Some(e("xin", "#新", 1, 1, 1, 1, 0b1)),
    );
    for src in &cases {
        let (out, got) =
            patch_decoded(src.as_bytes(), Path::new("t"), &TEMP, false, true, &edits).unwrap();
        let want = decode(&out, Path::new("t"), &TEMP, false, true).unwrap();
        assert_eq!(got.entries, want.entries, "{src:?}");
        assert_eq!(got.bad_lines, want.bad_lines, "{src:?}");
    }
}

#[test]
fn patch_on_200k_rows_is_linear() {
    let v: Vec<SnapshotEntry> = (0..200_000)
        .map(|i| e(&format!("c{i}"), &format!("t{i}"), 0, 1, 0, 1, 0))
        .collect();
    let src = render(&FREQ, false, &v);
    let mut edits = BTreeMap::new();
    for i in (0..200_000).step_by(100) {
        edits.insert(
            (format!("c{i}"), format!("t{i}")),
            Some(e(&format!("c{i}"), &format!("t{i}"), 0, 9, 0, 9, 0)),
        );
    }
    let t = std::time::Instant::now();
    patch(src.as_bytes(), Path::new("f"), &FREQ, false, false, &edits).unwrap();
    assert!(
        t.elapsed() < std::time::Duration::from_secs(3),
        "{:?}",
        t.elapsed()
    );
}
