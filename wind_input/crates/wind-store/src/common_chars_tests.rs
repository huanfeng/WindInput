use super::*;

/// 审查 M-2：还原前的「清空」遇到改坏的文件 ⇒ 另存留存，不直接删掉。
#[test]
fn clearing_broken_file_keeps_it_aside() {
    let s = store("clear_broken");
    s.set_common_char_override("龘", true).unwrap();
    let f = s.common_chars_path();
    std::fs::write(&f, "common: [\n").unwrap();
    s.common_doc().sync();
    s.clear_common_char_overrides().unwrap();
    let kept: Vec<_> = std::fs::read_dir(s.user_text_dir().join(".backup"))
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("common_chars.yaml.corrupt-")
        })
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        std::fs::read_to_string(kept[0].path()).unwrap(),
        "common: [\n"
    );
    s.set_common_char_override("靐", false).unwrap();
    wind_userdata::issues::clear(&f);
}

/// M-1：手改坏了按最后一次好数据服务、拒写、不重写文件。
#[test]
fn common_chars_broken_edit_keeps_serving_and_is_not_rewritten() {
    let s = store("broken");
    s.set_common_char_override("龘", true).unwrap();
    let f = s.common_chars_path();
    std::fs::write(&f, "common: [\n").unwrap();
    s.common_doc().sync();
    assert_eq!(s.get_common_char_override("龘").unwrap(), Some(true));
    assert!(s.set_common_char_override("靐", false).is_err());
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "common: [\n");
    wind_userdata::issues::clear(&f);
}

/// 每个测试独立文件：redb 是单写者，共用文件会让并发测试互相阻塞。
/// `tag` 区分同一测试里需要的第二个库（导入导出对拷）。
fn store(tag: &str) -> Store {
    let p = std::env::temp_dir().join(format!(
        "wind_common_chars_test_{}_{}_{:?}.redb",
        std::process::id(),
        tag,
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&p);
    Store::open(&p).unwrap()
}

#[test]
fn set_get_remove_roundtrip() {
    let s = store("a");
    assert_eq!(s.get_common_char_override("槮").unwrap(), None);

    s.set_common_char_override("槮", true).unwrap();
    assert_eq!(s.get_common_char_override("槮").unwrap(), Some(true));

    // 同字改写方向，不是追加第二条。
    s.set_common_char_override("槮", false).unwrap();
    assert_eq!(s.get_common_char_override("槮").unwrap(), Some(false));
    assert_eq!(s.list_common_char_overrides().unwrap().len(), 1);

    assert!(s.remove_common_char_override("槮").unwrap());
    assert_eq!(s.get_common_char_override("槮").unwrap(), None);
    // 再删一次：没有这条，返回 false 而非报错。
    assert!(!s.remove_common_char_override("槮").unwrap());
}

#[test]
fn both_directions_are_storable() {
    let s = store("b");
    // 生僻字升级为常用
    s.set_common_char_override("槮", true).unwrap();
    // 常用字降级为生僻
    s.set_common_char_override("的", false).unwrap();
    let all = s.list_common_char_overrides().unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.contains(&CommonCharOverride {
        ch: "槮".into(),
        common: true
    }));
    assert!(all.contains(&CommonCharOverride {
        ch: "的".into(),
        common: false
    }));
}

#[test]
fn clear_removes_all() {
    let s = store("c");
    s.set_common_char_override("槮", true).unwrap();
    s.set_common_char_override("的", false).unwrap();
    assert_eq!(s.clear_common_char_overrides().unwrap(), 2);
    assert!(s.list_common_char_overrides().unwrap().is_empty());
}

#[test]
fn jsonl_roundtrip() {
    let s = store("d");
    s.set_common_char_override("槮", true).unwrap();
    s.set_common_char_override("的", false).unwrap();
    let text = s.export_common_chars_jsonl().unwrap();
    assert_eq!(text.lines().count(), 2);

    let s2 = store("d2");
    assert_eq!(s2.import_common_chars_jsonl(&text).unwrap(), 2);
    assert_eq!(s2.get_common_char_override("槮").unwrap(), Some(true));
    assert_eq!(s2.get_common_char_override("的").unwrap(), Some(false));
}

#[test]
fn import_skips_bad_lines() {
    let s = store("e");
    let text = "{\"ch\":\"槮\",\"common\":true}\n不是 json\n\n{\"ch\":\"的\",\"common\":false}\n";
    assert_eq!(s.import_common_chars_jsonl(text).unwrap(), 2);
    assert_eq!(s.list_common_char_overrides().unwrap().len(), 2);
}

/// 常用字覆盖落用户目录文件。
#[test]
fn common_chars_live_in_user_dir() {
    let s = store("file");
    s.set_common_char_override("槮", true).unwrap();
    s.set_common_char_override("的", false).unwrap();
    let f = std::fs::read_to_string(s.common_chars_path()).unwrap();
    assert!(
        f.contains("common:") && f.contains("槮") && f.contains("uncommon:"),
        "{f}"
    );
    assert_eq!(s.get_common_char_override("的").unwrap(), Some(false));
}

/// 编辑器正在改文件（复核前被改）⇒ 程序这次写入报错、不覆盖；sync 后再写成功。
#[test]
fn override_write_refuses_when_file_changed_underneath() {
    let s = store("race");
    s.set_common_char_override("槮", true).unwrap();
    let f = s.common_chars_path();
    let f2 = f.clone();
    let me = std::thread::current().id();
    let hook = wind_userdata::text::fold::add_before_rename_hook(Box::new(move || {
        if std::thread::current().id() == me {
            std::fs::write(&f2, "common: [甲]\nuncommon: []\n").unwrap();
        }
    }));
    let r = s.set_common_char_override("乙", true);
    drop(hook);
    assert!(r.is_err());
    assert!(std::fs::read_to_string(&f).unwrap().contains("甲"));
    s.sync_user_texts();
    s.set_common_char_override("乙", true).unwrap();
    let f = std::fs::read_to_string(&f).unwrap();
    assert!(f.contains("甲") && f.contains("乙"), "{f}");
}
