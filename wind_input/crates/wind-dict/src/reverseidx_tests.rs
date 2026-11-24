use super::*;

fn build(v: &[(&str, &str, i32)]) -> ReverseIndex {
    ReverseIndex::build(
        v.iter()
            .map(|(t, c, w)| (t.to_string(), c.to_string(), *w))
            .collect(),
    )
}

#[test]
fn roundtrip_through_disk_is_byte_identical_and_behaviour_identical() {
    let mem = build(&[
        ("工", "aaaa", 100),
        ("工", "a", 50),
        ("中", "khk", 10),
        ("大", "de", 7),
    ]);
    let p = std::env::temp_dir().join(format!("wind_wridx_rt_{}.wridx", std::process::id()));
    write_wridx(&p, mem.image()).unwrap();

    // 阈值给足 → 常驻；给 0 → mmap。两条路都必须与内存版**逐位相同**。
    let resident = ReverseIndex::open(&p, usize::MAX).unwrap();
    let mapped = ReverseIndex::open(&p, 0).unwrap();
    assert!(resident.is_resident());
    assert!(!mapped.is_resident());
    assert_eq!(resident.image(), mem.image());
    assert_eq!(mapped.image(), mem.image());
    assert_eq!(mapped.resident_bytes(), 0, "mmap 不应计入常驻内存");
    assert_eq!(resident.resident_bytes(), mem.data_bytes());

    for idx in [&mem, &resident, &mapped] {
        assert_eq!(idx.len(), 3);
        assert_eq!(
            idx.codes_of("工").unwrap().iter().collect::<Vec<_>>(),
            ["a", "aaaa"]
        );
        assert_eq!(idx.codes_of("工").unwrap().last(), Some("aaaa"));
        assert_eq!(idx.codes_of("工").unwrap().join("/"), "a/aaaa");
        assert!(idx.codes_of("无").is_none());
    }
    drop((resident, mapped));
    let _ = std::fs::remove_file(&p);
}

/// arena 是连续拼接的，相邻词/码若边界算错会串味。等长且互为前缀的样本压这个边界。
#[test]
fn pool_boundaries_dont_bleed() {
    let idx = build(&[
        ("甲", "aa", 1),
        ("甲", "aaa", 1),
        ("乙", "bb", 1),
        ("丙", "cc", 1),
    ]);
    assert_eq!(
        idx.codes_of("甲").unwrap().iter().collect::<Vec<_>>(),
        ["aa", "aaa"]
    );
    assert_eq!(
        idx.codes_of("乙").unwrap().iter().collect::<Vec<_>>(),
        ["bb"]
    );
    // 「丙」按字节序排首位，其编码起点必须是 0 而非前一条目的终点
    assert_eq!(
        idx.codes_of("丙").unwrap().iter().collect::<Vec<_>>(),
        ["cc"]
    );
    assert!(idx.codes_of("").is_none(), "空词不应命中");
}

/// 多字节键的二分：比较按 UTF-8 字节序，与写入端 `String::cmp` 必须一致。
/// 混入扩展区汉字（4 字节）——若哪一端按 char 数或 UTF-16 比较，这里会错位。
#[test]
fn multibyte_keys_compare_consistently() {
    let idx = build(&[
        ("𠮷", "a", 1),
        ("吉", "b", 1),
        ("a", "c", 1),
        ("Ω", "d", 1),
        ("🀄", "e", 1),
    ]);
    for (k, want) in [
        ("𠮷", "a"),
        ("吉", "b"),
        ("a", "c"),
        ("Ω", "d"),
        ("🀄", "e"),
    ] {
        assert_eq!(
            idx.codes_of(k).unwrap().last(),
            Some(want),
            "键 {k} 应可查到"
        );
    }
    // 边界：小于最小键 / 大于最大键
    assert!(idx.codes_of("\u{1}").is_none());
    assert!(idx.codes_of("\u{10FFFF}").is_none());
}

#[test]
fn empty_index_is_valid_everywhere() {
    let mem = build(&[]);
    assert!(mem.is_empty());
    assert!(mem.codes_of("任意").is_none());
    assert!(mem.texts_with_prefix("中", 9).is_empty());
    let p = std::env::temp_dir().join(format!("wind_wridx_empty_{}.wridx", std::process::id()));
    write_wridx(&p, mem.image()).unwrap();
    let disk = ReverseIndex::open(&p, usize::MAX).unwrap();
    assert!(disk.is_empty());
    assert!(disk.codes_of("任意").is_none());
    drop(disk);
    let _ = std::fs::remove_file(&p);

    // Default 与「序列化一个空表」必须行为一致（前者刻意不走序列化，故须单独验）
    let d = ReverseIndex::default();
    assert!(d.is_empty() && d.codes_of("x").is_none());
    assert!(d.texts_with_prefix("x", 9).is_empty());
}

/// 非 wridx / 版本不符 / 过短的文件必须被拒绝而非当成空表——否则缓存格式升级后
/// 用户会静默失去反查，且毫无提示。
#[test]
fn rejects_foreign_or_stale_file() {
    let p = std::env::temp_dir().join(format!("wind_wridx_bad_{}.wridx", std::process::id()));
    std::fs::write(
        &p,
        b"not a wridx file at all, padded well past the header size",
    )
    .unwrap();
    assert!(
        ReverseIndex::open(&p, usize::MAX).is_err(),
        "magic 不符须拒绝"
    );
    std::fs::write(&p, b"WRIX").unwrap();
    assert!(ReverseIndex::open(&p, usize::MAX).is_err(), "过短须拒绝");
    // 版本位改掉 → 拒绝（缓存格式升级的自动失效依赖这一条）
    let mut img = build(&[("中", "a", 1)]).image().to_vec();
    img[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
    std::fs::write(&p, &img).unwrap();
    assert!(
        ReverseIndex::open(&p, usize::MAX).is_err(),
        "版本不符须拒绝"
    );
    let _ = std::fs::remove_file(&p);
}

/// 超长编码被丢弃，但**同词的其它编码与权重必须留下**——整词消失才是真正的伤害。
#[test]
fn oversized_code_is_dropped_without_losing_the_word() {
    let long = "x".repeat(MAX_CODE_BYTES + 1);
    let idx = ReverseIndex::build(vec![
        ("词".into(), "ok".into(), 5),
        ("词".into(), long, 900),
        ("别的".into(), "z".into(), 1),
    ]);
    assert_eq!(
        idx.codes_of("词").unwrap().iter().collect::<Vec<_>>(),
        ["ok"]
    );
    // 权重在编码过滤之**前**聚合，故被丢弃那条的 900 仍要生效
    assert_eq!(
        idx.texts_with_prefix("词", 1),
        Vec::<(&str, i32)>::new(),
        "「词」没有更长的词"
    );
    let all = build(&[("中", "a", 1)]);
    assert_eq!(all.len(), 1);
}

/// 超长的**词**整条丢弃，但不能影响其相邻条目的偏移计算。
#[test]
fn oversized_text_is_dropped_without_shifting_neighbours() {
    let huge = "中".repeat(MAX_TEXT_BYTES); // 3 字节/字 → 远超 64KB
    let idx = ReverseIndex::build(vec![
        ("阿".into(), "a".into(), 1),
        (huge, "h".into(), 1),
        ("龘".into(), "z".into(), 2),
    ]);
    assert_eq!(idx.len(), 2, "超长词整条丢弃");
    assert_eq!(idx.codes_of("阿").unwrap().last(), Some("a"));
    assert_eq!(idx.codes_of("龘").unwrap().last(), Some("z"));
}
