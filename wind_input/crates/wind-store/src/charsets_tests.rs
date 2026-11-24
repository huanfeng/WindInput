use super::*;

/// 每个测试独立文件：redb 是单写者，共用文件会让并发测试互相阻塞。
fn store(tag: &str) -> Store {
    let p = std::env::temp_dir().join(format!(
        "wind_charsets_test_{}_{}_{:?}.redb",
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
    assert_eq!(s.get_charset_doc("emoji").unwrap(), None);

    s.set_charset_doc("emoji", "key: emoji\ndefault: rare\n")
        .unwrap();
    assert_eq!(
        s.get_charset_doc("emoji").unwrap().as_deref(),
        Some("key: emoji\ndefault: rare\n")
    );

    // 同 key 覆盖，不是追加。
    s.set_charset_doc("emoji", "key: emoji\n").unwrap();
    assert_eq!(
        s.get_charset_doc("emoji").unwrap().as_deref(),
        Some("key: emoji\n")
    );

    assert!(s.remove_charset_doc("emoji").unwrap());
    assert!(
        !s.remove_charset_doc("emoji").unwrap(),
        "二次删除返回 false"
    );
    assert_eq!(s.get_charset_doc("emoji").unwrap(), None);
}

/// 文本原样进出——多行、中文、`...` 分隔符、行首 `-`，一个字节都不能动。
/// 备份和编辑态都靠它：这里若做任何规范化，回读时 diff 就对不上出厂。
#[test]
fn text_is_stored_verbatim() {
    let s = store("verbatim");
    let text = "---\nkey: 我的符号\nname: 我的符号\nranges: [U+2600-U+26FF]\n...\n★\n-☯\n\n";
    s.set_charset_doc("我的符号", text).unwrap();
    assert_eq!(
        s.get_charset_doc("我的符号").unwrap().as_deref(),
        Some(text)
    );
}

#[test]
fn list_is_sorted_by_key_and_clear_empties_it() {
    let s = store("list");
    s.set_charset_doc("zeta", "key: zeta\n").unwrap();
    s.set_charset_doc("alpha", "key: alpha\n").unwrap();
    s.set_charset_doc("emoji", "key: emoji\n").unwrap();

    let keys: Vec<String> = s
        .list_charset_docs()
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(keys, ["alpha", "emoji", "zeta"]);

    assert_eq!(s.clear_charset_docs().unwrap(), 3);
    assert!(s.list_charset_docs().unwrap().is_empty());
    assert_eq!(s.clear_charset_docs().unwrap(), 0, "清空空表不报错");
}

/// 字符集用户层落用户目录文件；key 不能越出目录。
#[test]
fn charset_docs_live_in_user_dir() {
    let s = store("file");
    s.set_charset_doc("rare", "key: rare\nname: 生僻\n")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(s.charset_path("rare").unwrap()).unwrap(),
        "key: rare\nname: 生僻\n"
    );
    assert_eq!(s.list_charset_docs().unwrap().len(), 1);
    assert!(s.remove_charset_doc("rare").unwrap());
    assert!(!s.charset_path("rare").unwrap().exists());
    assert!(s.set_charset_doc("../x", "k").is_err());
}

/// 审查 I-3：启动后第一次手改就要被 sync 发现。
#[test]
fn first_hand_edit_after_open_is_detected() {
    let p = std::env::temp_dir().join(format!("wind_cs_first_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    {
        let s = Store::open(&p).unwrap();
        s.set_charset_doc("rare", "key: rare\n").unwrap();
    }
    let s = Store::open(&p).unwrap();
    std::fs::write(s.charset_path("rare").unwrap(), "key: rare\nname: 改\n").unwrap();
    assert!(s.sync_charsets(), "第一次手改就要报变化");
    assert!(!s.sync_charsets());
}
