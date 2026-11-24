use super::*;
use crate::wdict::WordIo;

/// 开关文件读不了（IO 错误）沿用上次读到的开关；格式坏才按全开并报问题。
#[test]
fn off_names_keeps_last_value_when_unreadable() {
    let s = store("off_io");
    let root = s.user_text_dir().to_path_buf();
    s.create_user_dict("wb", "a").unwrap();
    s.set_user_dict_enabled("wb", "wb", "a", false).unwrap();
    assert_eq!(off_names(&root, "wb", "wb"), ["a"]);
    let meta = meta_path(&root, "wb");
    std::fs::remove_file(&meta).unwrap();
    std::fs::create_dir(&meta).unwrap();
    assert_eq!(off_names(&root, "wb", "wb"), ["a"], "读不了沿用上次");
    std::fs::remove_dir(&meta).unwrap();
    std::fs::write(&meta, "not = [valid").unwrap();
    assert!(off_names(&root, "wb", "wb").is_empty(), "格式坏按全开");
    assert!(
        issues::list_under(&meta)
            .iter()
            .any(|i| i.kind == IssueKind::Isolated)
    );
}

/// Windows 保留设备名（不分大小写，带扩展名也算）与超长文件名拒绝。
#[test]
fn validate_name_rejects_reserved_and_too_long() {
    for bad in [
        "CON", "con", "Prn", "aux", "NUL", "com1", "COM9", "lpt1", "LPT9", "con.txt", "Nul.x.y",
        "con .txt",
    ] {
        assert!(validate_name(bad).is_err(), "{bad}");
    }
    for ok in ["console", "com0", "com10", "lpt", "我的 con", "a.con"] {
        assert!(validate_name(ok).is_ok(), "{ok}");
    }
    // 64 个四字节字符：字数在限内，但文件名（含 .dict.yaml）超过 255 字节
    let emoji = "😀".repeat(64);
    assert!(validate_name(&emoji).is_err());
    assert!(validate_name(&"😀".repeat(61)).is_ok());
}

/// 不分大小写的文件系统（Windows）上，后缀大小写不同也认得出是分库。
#[test]
fn lib_name_folds_suffix_case_when_asked() {
    assert_eq!(lib_name("成语.dict.yaml", false), Some("成语"));
    assert_eq!(lib_name("成语.DICT.YAML", true), Some("成语"));
    assert_eq!(lib_name("Ab.Dict.Yaml", true), Some("Ab"));
    assert_eq!(lib_name("成语.DICT.YAML", false), None);
    assert_eq!(lib_name("dicts.toml", true), None);
}

fn store(tag: &str) -> Store {
    let p = std::env::temp_dir().join(format!("wind_udl_{tag}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_dir_all(crate::store::derived_text_dir(&p));
    Store::open(&p).unwrap()
}

fn row(code: &str, text: &str, weight: i32) -> WordIo {
    WordIo {
        code: code.into(),
        text: text.into(),
        weight,
        count: 0,
        boundary: None,
    }
}

#[test]
fn name_rules() {
    for ok in ["成语", "行业 词", "a-b_c", &"字".repeat(64)] {
        assert!(validate_name(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        ".hidden",
        "a/b",
        "a\\b",
        "a:b",
        "a*b",
        "a?b",
        "a\"b",
        "a<b",
        "a>b",
        "a|b",
        "a\tb",
        " a",
        "a ",
        "a.",
        &"字".repeat(65),
    ] {
        assert!(validate_name(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn scan_lists_libs_sorted_non_recursive() {
    let s = store("scan");
    let root = s.user_text_dir();
    let d = libs_dir(root, "wb");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    for f in [
        "b.dict.yaml",
        "a.dict.yaml",
        "c.txt",
        "sub/x.dict.yaml",
        ".h.dict.yaml",
    ] {
        std::fs::write(d.join(f), "---\n...\n").unwrap();
    }
    std::fs::write(d.join("a.dict.yaml.bak"), "").unwrap();
    let names: Vec<String> = scan(root, "wb").into_iter().map(|l| l.name).collect();
    assert_eq!(names, ["a", "b"]);
    assert!(scan(root, "none").is_empty());
}

fn lib_rows(s: &Store, ds: &str, name: &str) -> Vec<(String, String, i32)> {
    let mut v: Vec<_> = s
        .user_dict_entries(ds, name)
        .unwrap()
        .into_iter()
        .map(|e| (e.text, e.code, e.weight))
        .collect();
    v.sort();
    v
}

/// 加词按 (词, 码) 去重：已有的改成给的权重（可调低），没给权重就不动；新词没给权重时权重列留空。
#[test]
fn add_entry_upserts_by_text_and_code() {
    let s = store("add");
    s.import_user_dict("wb", "a", &[row("a", "工", 5)]).unwrap();
    assert!(
        s.add_user_dict_entry("wb", "a", "aa", "式", 0, Some(3))
            .unwrap()
    );
    assert!(
        s.add_user_dict_entry("wb", "a", "a", "工", 0, Some(9))
            .unwrap()
    );
    assert!(
        !s.add_user_dict_entry("wb", "a", "a", "工", 0, None)
            .unwrap()
    );
    assert!(
        !s.add_user_dict_entry("wb", "a", "a", "工", 0, Some(9))
            .unwrap()
    );
    assert!(
        s.add_user_dict_entry("wb", "a", "ab", "戈", 0, None)
            .unwrap()
    );
    assert!(
        s.add_user_dict_entry("wb", "a", "aa", "式", 0, Some(1))
            .unwrap()
    );
    assert_eq!(
        lib_rows(&s, "wb", "a"),
        [
            ("工".into(), "a".into(), 9),
            ("式".into(), "aa".into(), 1),
            ("戈".into(), "ab".into(), 0)
        ]
    );
    let p = lib_path(s.user_text_dir(), "wb", "a");
    assert!(std::fs::read_to_string(&p).unwrap().ends_with("ab\t戈\n"));
    for (code, text) in [
        ("", "x"),
        ("a", ""),
        ("a", "x\ty"),
        ("a\nb", "x"),
        ("a", "x\r"),
    ] {
        assert!(
            s.add_user_dict_entry("wb", "a", code, text, 0, Some(1))
                .is_err(),
            "{code:?} {text:?}"
        );
    }
    assert!(
        s.add_user_dict_entry("wb", "无", "a", "x", 0, None)
            .is_err(),
        "分库不存在"
    );
}

/// 拼音分库：带空格的码按音节写。
#[test]
fn add_entry_keeps_syllables_in_pinyin_lib() {
    let s = store("add_py");
    s.create_user_dict("pinyin", "c").unwrap();
    s.add_user_dict_entry("pinyin", "c", "ni hao", "你好", 0, Some(2))
        .unwrap();
    let p = lib_path(s.user_text_dir(), "pinyin", "c");
    assert!(
        std::fs::read_to_string(&p)
            .unwrap()
            .contains("ni hao\t你好\t2\n")
    );
}

/// 清空：留文件头与说明行，删光词条；再清一次是 0。
#[test]
fn clear_keeps_header_and_banner() {
    let s = store("clear");
    s.import_user_dict(
        "pinyin",
        "c",
        &[row("ni hao", "你好", 1), row("a", "啊", 1)],
    )
    .unwrap();
    assert_eq!(s.clear_user_dict("pinyin", "c").unwrap(), 2);
    let text = std::fs::read_to_string(lib_path(s.user_text_dir(), "pinyin", "c")).unwrap();
    assert!(text.starts_with(banner::USER_DICT_LIB), "{text}");
    assert!(text.contains("code_spacing: syllable"), "{text}");
    assert!(lib_rows(&s, "pinyin", "c").is_empty());
    assert_eq!(s.clear_user_dict("pinyin", "c").unwrap(), 0);
    assert!(s.clear_user_dict("pinyin", "无").is_err());
}

/// 清空按解析器的注释区算：`# no comment` 之后 `#` 开头的是词条，一并删掉；返回数与删掉的一致。
#[test]
fn clear_treats_hash_lines_after_no_comment_as_entries() {
    let s = store("clear_nc");
    let p = lib_path(s.user_text_dir(), "wb", "c");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        "---\nname: c\ncolumns: [text, code, weight]\n...\n# 注释\n# no comment\n#井\tjing\t2\n你\tni\t1\n",
    )
    .unwrap();
    assert_eq!(s.clear_user_dict("wb", "c").unwrap(), 2);
    assert!(lib_rows(&s, "wb", "c").is_empty());
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "---\nname: c\ncolumns: [text, code, weight]\n...\n# 注释\n# no comment\n"
    );
}

/// 没有权重列的分库：给了权重 ⇒ 报错、文件不动；不给照常加。
#[test]
fn add_entry_with_weight_needs_weight_column() {
    let s = store("add_now");
    let p = lib_path(s.user_text_dir(), "wb", "n");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "---\nname: n\ncolumns: [text, code]\n...\n工\ta\n").unwrap();
    let before = std::fs::read(&p).unwrap();
    let e = s
        .add_user_dict_entry("wb", "n", "b", "了", 0, Some(5))
        .unwrap_err()
        .to_string();
    assert!(e.contains("没有权重列"), "{e}");
    assert!(
        s.add_user_dict_entry("wb", "n", "a", "工", 0, Some(5))
            .is_err()
    );
    assert_eq!(std::fs::read(&p).unwrap(), before);
    assert!(
        s.add_user_dict_entry("wb", "n", "b", "了", 0, None)
            .unwrap()
    );
}

/// 外部编辑中：加词、清空都挡住，文件不动。
#[test]
fn add_and_clear_rejected_while_editing() {
    let s = store("add_edit");
    s.import_user_dict("wb", "a", &[row("a", "工", 5)]).unwrap();
    let sess = s.begin_edit("wb/dicts/a.dict.yaml").unwrap();
    let before = std::fs::read(&sess.path).unwrap();
    let e = s
        .add_user_dict_entry("wb", "a", "b", "x", 0, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("正在外部编辑"), "{e}");
    assert!(
        s.clear_user_dict("wb", "a")
            .unwrap_err()
            .to_string()
            .contains("正在外部编辑")
    );
    assert_eq!(std::fs::read(&sess.path).unwrap(), before);
    s.end_edit("wb/dicts/a.dict.yaml").unwrap();
    assert_eq!(s.clear_user_dict("wb", "a").unwrap(), 1);
}

/// 分库的上一版在 `.backup/` 里：改名跟着走，删除一并删掉；数据文件旁边没有 `.bak`。
#[test]
fn lib_backup_follows_rename_and_delete() {
    let s = store("bakmv");
    let root = s.user_text_dir().to_path_buf();
    s.import_user_dict("wb", "a", &[row("a", "工", 1)]).unwrap();
    let p = lib_path(&root, "wb", "a");
    assert!(s.remove_user_dict_entry("wb", "a", "a", "工").unwrap());
    let bak = root.join(".backup/wb/dicts/a.dict.yaml");
    assert!(bak.is_file());
    assert!(!root.join("wb/dicts/a.dict.yaml.bak").exists());
    s.rename_user_dict("wb", "a", "b").unwrap();
    assert!(!bak.exists());
    let bak_b = root.join(".backup/wb/dicts/b.dict.yaml");
    assert!(bak_b.is_file());
    assert!(!p.exists());
    s.delete_user_dict("wb", "b").unwrap();
    assert!(!bak_b.exists());
}

#[test]
fn create_writes_banner_and_header_and_rejects_dup() {
    let s = store("create");
    let p = s.create_user_dict("pinyin", "成语 大全").unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.starts_with(banner::USER_DICT_LIB), "{text}");
    assert!(text.contains("code_spacing: syllable"), "{text}");
    assert_eq!(read_label(&p), "成语 大全");
    assert!(s.create_user_dict("pinyin", "成语 大全").is_err(), "重名");
    assert!(s.create_user_dict("pinyin", "a/b").is_err(), "非法名");
    let p = s.create_user_dict("wb", "x").unwrap();
    assert!(!std::fs::read_to_string(p).unwrap().contains("code_spacing"));
}

#[test]
fn switches_default_on_and_persist_per_schema() {
    let s = store("switch");
    let root = s.user_text_dir().to_path_buf();
    s.create_user_dict("pinyin", "a").unwrap();
    s.create_user_dict("pinyin", "b").unwrap();
    let on = |schema: &str| -> Vec<bool> {
        s.list_user_dicts("pinyin", schema)
            .unwrap()
            .into_iter()
            .map(|l| l.enabled)
            .collect()
    };
    assert_eq!(on("pinyin"), [true, true], "出厂全开");
    s.set_user_dict_enabled("pinyin", "shuangpin", "b", false)
        .unwrap();
    assert_eq!(on("pinyin"), [true, true], "开关按方案");
    assert_eq!(on("shuangpin"), [true, false]);
    assert_eq!(off_names(&root, "pinyin", "shuangpin"), ["b"]);
    let meta = std::fs::read_to_string(meta_path(&root, "pinyin")).unwrap();
    assert!(meta.starts_with(banner::USER_DICTS_META), "{meta}");
    s.set_user_dict_enabled("pinyin", "shuangpin", "b", true)
        .unwrap();
    assert!(off_names(&root, "pinyin", "shuangpin").is_empty());
}

#[test]
fn corrupt_meta_reads_all_on_and_is_kept_aside_on_write() {
    let s = store("corrupt");
    let root = s.user_text_dir().to_path_buf();
    s.create_user_dict("wb", "a").unwrap();
    let mp = meta_path(&root, "wb");
    std::fs::write(&mp, "[schemas.wb\noff = [").unwrap();
    assert!(off_names(&root, "wb", "wb").is_empty(), "坏文件按全开");
    assert_eq!(issues::list_under(&mp)[0].kind, IssueKind::Isolated);
    s.set_user_dict_enabled("wb", "wb", "a", false).unwrap();
    assert_eq!(off_names(&root, "wb", "wb"), ["a"]);
    let kept = std::fs::read_dir(root.join(".backup/wb"))
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("dicts.toml.corrupt-")
        })
        .count();
    assert_eq!(kept, 1, "坏文件留存");
    assert!(matches!(
        issues::list_under(&mp)[0].kind,
        IssueKind::Rebuilt { .. }
    ));
    issues::clear(&mp);
}

#[test]
fn import_rename_delete_and_remove_entry() {
    let s = store("ops");
    let root = s.user_text_dir().to_path_buf();
    let n = s
        .import_user_dict(
            "pinyin",
            "成语",
            &[
                row("hou ji bo fa", "厚积薄发", 5),
                row("ni hao", "你好", 0),
                row("ni hao", "你好", 3),
            ],
        )
        .unwrap();
    assert_eq!(n, 2, "同码同词只留一条");
    assert!(s.import_user_dict("pinyin", "成语", &[]).is_err(), "已存在");
    let e = s.user_dict_entries("pinyin", "成语").unwrap();
    assert_eq!(e.len(), 2);
    assert_eq!(e[0].code, "houjibofa");
    assert_ne!(e[0].boundary, 0, "音节边界随空格保留");
    assert_eq!(e[1].weight, 3, "取较高权重");

    s.set_user_dict_enabled("pinyin", "pinyin", "成语", false)
        .unwrap();
    s.rename_user_dict("pinyin", "成语", "四字").unwrap();
    assert!(!lib_path(&root, "pinyin", "成语").exists());
    assert_eq!(read_label(&lib_path(&root, "pinyin", "四字")), "四字");
    assert_eq!(off_names(&root, "pinyin", "pinyin"), ["四字"]);
    assert!(s.rename_user_dict("pinyin", "无", "x").is_err(), "不存在");
    s.create_user_dict("pinyin", "别的").unwrap();
    assert!(
        s.rename_user_dict("pinyin", "四字", "别的").is_err(),
        "重名"
    );

    assert!(
        s.remove_user_dict_entry("pinyin", "四字", "ni hao", "你好")
            .unwrap()
    );
    assert!(
        !s.remove_user_dict_entry("pinyin", "四字", "ni hao", "你好")
            .unwrap()
    );
    assert_eq!(s.user_dict_entries("pinyin", "四字").unwrap().len(), 1);

    s.delete_user_dict("pinyin", "四字").unwrap();
    assert!(!lib_path(&root, "pinyin", "四字").exists());
    assert!(off_names(&root, "pinyin", "pinyin").is_empty());
    assert!(s.delete_user_dict("pinyin", "四字").is_err());
}

#[test]
fn user_file_without_banner_stays_without_banner_after_remove() {
    let s = store("ownfile");
    let root = s.user_text_dir().to_path_buf();
    let p = lib_path(&root, "wb", "mine");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "---\nname: 我的\n...\n工\ta\n式\taa\t2\n").unwrap();
    let l = s.list_user_dicts("wb", "wb").unwrap();
    assert_eq!(
        (l[0].label.as_str(), l[0].entries, l[0].file.as_str()),
        ("我的", 2, "wb/dicts/mine.dict.yaml")
    );
    assert!(s.remove_user_dict_entry("wb", "mine", "a", "工").unwrap());
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "---\nname: 我的\n...\n式\taa\t2\n"
    );
}

#[test]
fn fingerprint_changes_with_files_and_switches() {
    let s = store("fp");
    let root = s.user_text_dir().to_path_buf();
    let a = fingerprint(&root, "wb");
    s.create_user_dict("wb", "x").unwrap();
    let b = fingerprint(&root, "wb");
    assert_ne!(a, b);
    assert_eq!(b, fingerprint(&root, "wb"), "没变就相同");
    s.set_user_dict_enabled("wb", "wb", "x", false).unwrap();
    assert_ne!(b, fingerprint(&root, "wb"));
}
