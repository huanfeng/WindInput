use super::*;

fn sample() -> HashMap<char, String> {
    [('工', "aaaa"), ('王', "ggg"), ('㐀', "xyzw"), ('𠀀', "q")]
        .into_iter()
        .map(|(c, s)| (c, s.to_string()))
        .collect()
}

/// 基本盘：非 BMP 字符（`𠀀` U+20000）必须原样回来。
///
/// 用 `char as u32` 存标量值而不是 UTF-8 字节，正是为了让扩展区汉字与 BMP 同价——
/// 码表里的生僻字大量落在 Ext-B 以上，它们恰恰是最需要靠这张表取码的那批。
#[test]
fn roundtrip_preserves_every_entry_including_astral_chars() {
    let m = sample();
    let back = parse(&serialize(&m)).unwrap();
    assert_eq!(back, m);
    assert!(back.contains_key(&'𠀀'), "非 BMP 字符不能丢");
}

#[test]
fn empty_table_roundtrips() {
    let m = HashMap::new();
    assert_eq!(parse(&serialize(&m)).unwrap(), m);
}

/// 截断的文件必须整份拒绝，不能交出半张表。
#[test]
fn truncated_file_is_rejected_whole() {
    let img = serialize(&sample());
    for cut in [HEADER_LEN - 1, HEADER_LEN + 3, img.len() - 1] {
        assert!(
            parse(&img[..cut]).is_err(),
            "截到 {cut} B 仍被接受 —— 半份表会变成「某些字莫名取不到码」"
        );
    }
}

#[test]
fn foreign_or_future_files_are_rejected() {
    let mut img = serialize(&sample());
    let good = img.clone();
    img[0] = b'X';
    assert!(parse(&img).is_err(), "magic 不符必须拒绝");
    let mut img = good;
    img[4] = 0xFF;
    assert!(parse(&img).is_err(), "未来版本必须拒绝而不是照旧解析");
}

/// 空编码与超 255 字节的编码在写端就被丢掉，不写进文件。
#[test]
fn unrepresentable_codes_are_dropped_at_write_time() {
    let m: HashMap<char, String> = [
        ('空', String::new()),
        ('长', "a".repeat(MAX_CODE_LEN + 1)),
        ('好', "vb".to_string()),
    ]
    .into_iter()
    .collect();
    let back = parse(&serialize(&m)).unwrap();
    assert_eq!(back.len(), 1, "只有合法的那条该留下");
    assert_eq!(back.get(&'好').map(String::as_str), Some("vb"));
}

#[test]
fn write_then_read_from_disk() {
    let dir = std::env::temp_dir().join("wind_wscc_rt");
    let p = dir.join("t.wscc");
    let _ = std::fs::remove_file(&p);
    write(&p, &serialize(&sample())).unwrap();
    assert_eq!(read(&p).unwrap(), sample());
    let _ = std::fs::remove_file(&p);
}
