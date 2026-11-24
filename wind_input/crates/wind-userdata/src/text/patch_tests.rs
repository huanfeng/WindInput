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
    let orig =
        "---\nwind:\n  code_spacing: syllable\n...\n你好\tni hao\t3\n你好\tnihao\t4\n好\thao\t1\n";
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
    assert!(s.contains("columns: [code, text, weight]") && s.contains("code_spacing: syllable"));
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
