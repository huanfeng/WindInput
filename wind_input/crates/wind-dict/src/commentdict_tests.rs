use super::*;

fn rows(v: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    v.iter()
        .map(|(t, c, k)| (t.to_string(), c.to_string(), k.to_string()))
        .collect()
}

fn build(tag: &str, v: &[(&str, &str, &str)]) -> (std::path::PathBuf, CommentReader) {
    let p = std::env::temp_dir().join(format!("wind_wcmt_{}_{}.wcmt", std::process::id(), tag));
    write_comment_wcmt(&p, &rows(v)).unwrap();
    let r = CommentReader::open(&p).unwrap();
    (p, r)
}

#[test]
fn roundtrip_and_binary_search() {
    // 乱序输入，验证写入端排序 + 读取端二分
    let (p, r) = build(
        "basic",
        &[
            ("龘", "dá 龙飞的样子", ""),
            ("一", "yī 数词", ""),
            ("中国", "China", ""),
            ("啊", "叹词", ""),
        ],
    );
    assert_eq!(r.entry_count(), 4);
    assert_eq!(r.lookup_first("中国"), Some("China"));
    assert_eq!(r.lookup_first("一"), Some("yī 数词"));
    assert_eq!(r.lookup_first("龘"), Some("dá 龙飞的样子"));
    assert_eq!(r.lookup_first("不存在"), None);
    // 边界：小于最小键 / 大于最大键
    assert_eq!(r.lookup_first("\u{1}"), None);
    assert_eq!(r.lookup_first("\u{10FFFF}"), None);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn code_disambiguation_and_fallback() {
    let (p, r) = build(
        "code",
        &[
            ("行", "háng 行列", "tfhh"),
            ("行", "xíng 行走", "hang"),
            ("好", "hǎo", ""),
        ],
    );
    assert_eq!(r.lookup_by_code("行", "tfhh"), Some("háng 行列"));
    assert_eq!(r.lookup_by_code("行", "hang"), Some("xíng 行走"));
    // code 对不上 → 由调用方回落 lookup_first（跨方案挂同一份库是常态）
    assert_eq!(r.lookup_by_code("行", "wubi_no_such"), None);
    assert_eq!(r.lookup_first("行"), Some("háng 行列"), "回落取组内首条");
    // 空 code 不参与消歧
    assert_eq!(r.lookup_by_code("好", ""), None);
    let _ = std::fs::remove_file(&p);
}

/// 「先到先得」：同 (text, code) 重复时保留首次出现者，且必须在**大数组**上也成立
/// （小数组下 sort_unstable 恰好稳定，只测三五条抓不到排序不稳定的 bug）。
#[test]
fn duplicate_keeps_first_at_scale() {
    let mut v: Vec<(String, String, String)> = Vec::new();
    for i in 0..2000 {
        v.push((format!("词{i:04}"), format!("第{i}条"), String::new()));
    }
    // 在末尾追加与前面全部重复的行（后到者），它们不应覆盖已有注释
    for i in 0..2000 {
        v.push((format!("词{i:04}"), "后到的".into(), String::new()));
    }
    let p = std::env::temp_dir().join(format!("wind_wcmt_dup_{}.wcmt", std::process::id()));
    write_comment_wcmt(&p, &v).unwrap();
    let r = CommentReader::open(&p).unwrap();
    assert_eq!(r.entry_count(), 2000, "同词同码只留一条");
    assert_eq!(r.lookup_first("词0000"), Some("第0条"));
    assert_eq!(r.lookup_first("词1999"), Some("第1999条"));
    let _ = std::fs::remove_file(&p);
}

/// 多字节键的二分：比较按 UTF-8 字节序，与写入端 `String::cmp` 必须一致。
/// 混入扩展区汉字（4 字节）——若哪一端按 char 数或按 UTF-16 比较，这里会错位。
#[test]
fn multibyte_keys_compare_consistently() {
    let (p, r) = build(
        "multibyte",
        &[
            ("𠮷", "土 + 口（扩展 B）", ""),
            ("吉", "吉利", ""),
            ("a", "拉丁", ""),
            ("Ω", "欧米伽", ""),
            ("🀄", "麻将", ""),
        ],
    );
    for (k, want) in [
        ("𠮷", "土 + 口（扩展 B）"),
        ("吉", "吉利"),
        ("a", "拉丁"),
        ("Ω", "欧米伽"),
        ("🀄", "麻将"),
    ] {
        assert_eq!(r.lookup_first(k), Some(want), "键 {k} 应可查到");
    }
    let _ = std::fs::remove_file(&p);
}

#[test]
fn empty_dict_is_valid() {
    let (p, r) = build("empty", &[]);
    assert!(r.is_empty());
    assert_eq!(r.lookup_first("任意"), None);
    let _ = std::fs::remove_file(&p);
}

/// 非 wcmt 文件必须被拒绝而非当成空表——否则用户把路径写错时毫无提示。
#[test]
fn rejects_foreign_file() {
    let p = std::env::temp_dir().join(format!("wind_wcmt_bad_{}.wcmt", std::process::id()));
    std::fs::write(&p, b"not a wcmt file at all, padding to exceed header size").unwrap();
    assert!(CommentReader::open(&p).is_err());
    // 过短文件同样拒绝
    std::fs::write(&p, b"WCMT").unwrap();
    assert!(CommentReader::open(&p).is_err());
    let _ = std::fs::remove_file(&p);
}
