use super::*;

#[test]
fn classify_covers_program_files_only() {
    assert!(classify(&["pinyin", "user.dict.yaml"]).is_some());
    assert!(classify(&["pinyin", "dicts.toml"]).is_some());
    assert!(classify(&["stats", "2026-10.toml"]).is_some());
    assert!(classify(&["charsets", "rare.yaml"]).is_some());
    assert!(classify(&["pinyin", "dicts", "a.dict.yaml"]).is_none());
    assert!(classify(&[".backup", "user.dict.yaml"]).is_none());
    assert!(classify(&["pinyin", "notes.txt"]).is_none());
}

/// 文案改过的旧说明行换成当前的，其余字节不动。
#[test]
fn outdated_banner_is_replaced() {
    let d = std::env::temp_dir().join(format!("wind_bf_old-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("pinyin")).unwrap();
    let f = d.join("pinyin/temp.dict.yaml");
    let body = "---\ncolumns: [text, code, weight, count, created_at]\nwind:\n  kind: temp\n...\n";
    std::fs::write(&f, format!("# 清风输入法 · 旧的说明。\n{body}")).unwrap();
    assert_eq!(backfill_banners(&d), 1);
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        format!("{}\n{body}", banner::TEMP)
    );
    assert_eq!(backfill_banners(&d), 0);
    let _ = std::fs::remove_dir_all(&d);
}

/// 第一行很长（旧说明行超过一次读的块、多字节字符跨块）也整行替换，不切在行中间；整个文件
/// 没有换行 ⇒ 不动。
#[test]
fn long_first_line_is_replaced_whole() {
    let d = std::env::temp_dir().join(format!("wind_bf_long-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("quick_format.toml");
    std::fs::write(&f, format!("# 清风输入法 · {}\na = 1\n", "长".repeat(3000))).unwrap();
    assert_eq!(backfill_banners(&d), 1);
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        format!("{}\na = 1\n", banner::QUICK_FORMAT)
    );
    std::fs::write(&f, "a = 1").unwrap();
    assert_eq!(backfill_banners(&d), 0);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "a = 1");
    let _ = std::fs::remove_dir_all(&d);
}

/// CRLF 文件补的行也用 CRLF。
#[test]
fn crlf_file_gets_crlf_banner() {
    let d = std::env::temp_dir().join(format!("wind_bf_crlf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("quick_format.toml"), "a = 1\r\n").unwrap();
    assert_eq!(backfill_banners(&d), 1);
    assert_eq!(
        std::fs::read_to_string(d.join("quick_format.toml")).unwrap(),
        format!("{}\r\na = 1\r\n", banner::QUICK_FORMAT)
    );
    assert_eq!(backfill_banners(&d), 0);
    let _ = std::fs::remove_dir_all(&d);
}
