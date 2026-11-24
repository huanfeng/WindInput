use super::*;

#[test]
fn civil_date_conversion_matches_known_days() {
    assert_eq!(civil_from_days(0), (1970, 1, 1), "epoch");
    assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    assert_eq!(civil_from_days(19_782), (2024, 2, 29), "闰年 2 月 29 日");
    assert_eq!(civil_from_days(-1), (1969, 12, 31), "epoch 之前");
}

#[test]
fn today_is_well_formed() {
    let t = today_utc();
    assert_eq!(t.len(), 10, "YYYY-MM-DD: {t}");
    assert_eq!(t.matches('-').count(), 2);
    assert!(t.starts_with("20"), "本世纪内: {t}");
}

#[test]
fn main_dict_header_declares_column_order() {
    let dir = std::env::temp_dir().join("gen_dict_writer_test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("out.dict.yaml");
    let cfg = Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        output_name: "test_dict".into(),
        ..Default::default()
    };
    let entries = vec![Entry::new("中".into(), "khk".into(), 500, 0)];
    write_main_dict(&p, &entries, &cfg, "2026-01-01").unwrap();

    let s = std::fs::read_to_string(&p).unwrap();
    let (header, body) = s.split_once("\n...\n").expect("须有 ... 分隔");
    assert!(header.contains("name: test_dict"));
    assert!(header.contains("version: \"2026-01-01\""));
    assert!(!header.contains("sort:"), "不得写出 librime 的 sort: 键");
    assert!(
        header.contains("columns:\n  - code\n  - text\n  - weight"),
        "列序声明须与数据行一致"
    );
    assert_eq!(body, "khk\t中\t500\n", "数据行为 code\\ttext\\tweight");
}

#[test]
fn extra_dict_sorted_by_code_then_weight_desc() {
    let dir = std::env::temp_dir().join("gen_dict_writer_test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("extra.dict.yaml");
    let mut entries = vec![
        Entry::new("低".into(), "emoj".into(), 100, 0),
        Entry::new("高".into(), "emoj".into(), 200, 1),
        Entry::new("甲".into(), "aaaa".into(), 50, 2),
    ];
    write_extra_dict(&p, &mut entries, "t", Category::Emoji, "2026-01-01").unwrap();

    let s = std::fs::read_to_string(&p).unwrap();
    let body = s.split_once("\n...\n").unwrap().1;
    assert_eq!(
        body, "aaaa\t甲\t50\nemoj\t高\t200\nemoj\t低\t100\n",
        "编码升序，同码权重降序"
    );
}

#[test]
fn passthrough_strips_sort_key_and_its_comment() {
    let dir = std::env::temp_dir().join("gen_dict_writer_test");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("district_src.yaml");
    let dst = dir.join("district_dst.yaml");
    // 复刻上游 district 的头部形状
    std::fs::write(
        &src,
        "---\nname: d\nversion: \"2022-12-16\"\ndict_grouped: true\n\
             sort: original\n# 码表的排序方式: by_weight 权重，original 原始顺序\n\
             \n# 内容格式说明\n...\n甲\taaaa\t1\n乙\tbbbb\t2\n",
    )
    .unwrap();

    assert!(passthrough_stripping_sort(&src, &dst).unwrap());
    let s = std::fs::read_to_string(&dst).unwrap();
    assert!(!s.contains("sort: original"), "sort: 键须被清掉");
    assert!(
        !s.contains("码表的排序方式"),
        "其解释注释须一并清掉，不留孤儿注释"
    );
    assert!(s.contains("base_order 定库间分档"), "须换上统一的说明注释");
    assert!(s.contains("# 内容格式说明"), "其余头部注释保持原样");
    assert_eq!(
        s.split_once("\n...\n").unwrap().1,
        "甲\taaaa\t1\n乙\tbbbb\t2\n",
        "数据行必须一字不改"
    );
}

#[test]
fn passthrough_preserves_missing_trailing_newline() {
    // 上游 district 末尾没有换行；补一个就不再是"一字不改"
    let dir = std::env::temp_dir().join("gen_dict_writer_test");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("noeol_src.yaml");
    let dst = dir.join("noeol_dst.yaml");
    std::fs::write(&src, "---\nname: d\nsort: original\n...\n甲\taaaa\t1").unwrap();
    passthrough_stripping_sort(&src, &dst).unwrap();
    let s = std::fs::read_to_string(&dst).unwrap();
    assert!(s.ends_with("甲\taaaa\t1"), "末尾不得凭空多出换行: {s:?}");
}

#[test]
fn passthrough_preserves_crlf_line_endings() {
    let dir = std::env::temp_dir().join("gen_dict_writer_test");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("crlf_src.yaml");
    let dst = dir.join("crlf_dst.yaml");
    std::fs::write(
        &src,
        "---\r\nname: d\r\nsort: original\r\n...\r\n甲\taaaa\t1\r\n",
    )
    .unwrap();
    passthrough_stripping_sort(&src, &dst).unwrap();
    let s = std::fs::read_to_string(&dst).unwrap();
    assert!(s.contains("甲\taaaa\t1\r\n"), "CRLF 须保留，不得静默转 LF");
    assert!(!s.contains("sort: original"));
}

#[test]
fn passthrough_is_idempotent_when_no_sort_key() {
    let dir = std::env::temp_dir().join("gen_dict_writer_test");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("nosort_src.yaml");
    let dst = dir.join("nosort_dst.yaml");
    std::fs::write(&src, "---\nname: d\n...\n甲\taaaa\t1\n").unwrap();
    assert!(
        !passthrough_stripping_sort(&src, &dst).unwrap(),
        "本就没有 sort: 键"
    );
    assert_eq!(
        std::fs::read_to_string(&dst).unwrap(),
        "---\nname: d\n...\n甲\taaaa\t1\n"
    );
}

#[test]
fn passthrough_ignores_sort_like_lines_in_body() {
    // 数据区里恰好有以 sort: 开头的词条时不得被当成头部键删掉
    let dir = std::env::temp_dir().join("gen_dict_writer_test");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("body_src.yaml");
    let dst = dir.join("body_dst.yaml");
    std::fs::write(&src, "---\nname: d\n...\nsort:x\tabcd\t1\n").unwrap();
    assert!(!passthrough_stripping_sort(&src, &dst).unwrap());
    assert!(
        std::fs::read_to_string(&dst)
            .unwrap()
            .contains("sort:x\tabcd\t1")
    );
}

#[test]
fn atomic_write_leaves_no_tmp_behind() {
    let dir = std::env::temp_dir().join("gen_dict_writer_test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("atomic.dict.yaml");
    let cfg = Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        ..Default::default()
    };
    write_main_dict(&p, &[], &cfg, "2026-01-01").unwrap();
    assert!(p.exists());
    assert!(
        !p.with_extension("tmp").exists(),
        "临时文件须已被 rename 消费"
    );
}
