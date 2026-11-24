use super::*;

fn bytes(b: &[u8]) -> Arc<[u8]> {
    Arc::from(b)
}

fn table(path: &str, b: &[u8]) -> Arc<MemoryAssets> {
    let mut a = MemoryAssets::new();
    assert!(a.insert(path, bytes(b)));
    Arc::new(a)
}

#[test]
fn paths_normalize_on_insert_and_lookup() {
    let mut a = MemoryAssets::new();
    a.insert(".\\assets\\a.png", bytes(b"x"));
    assert_eq!(a.get("assets/a.png").map(|b| &b[..]), Some(&b"x"[..]));
    assert_eq!(a.get("./assets//a.png").map(|b| &b[..]), Some(&b"x"[..]));
    assert_eq!(
        a.get("img/../assets/./a.png").map(|b| &b[..]),
        Some(&b"x"[..])
    );
    assert!(a.get("assets/b.png").is_none());
    assert!(a.get("../assets/a.png").is_none(), "越过根查不到");
    assert!(!a.insert("../x.png", bytes(b"y")), "越过根不登记");
    assert!(!a.insert("./", bytes(b"y")));
    assert_eq!(a.len(), 1);
}

#[test]
fn source_key_only_for_registered_relative_refs() {
    uninstall();
    assert_eq!(source_key("a.png"), None, "没登记时一切照旧");
    let g = next_generation();
    install(g, &table("a.png", b"1"));
    let k = source_key("./a.png").expect("登记过的相对引用");
    assert_eq!(k, format!("{SCHEME}{g}/a.png"));
    assert_eq!(bytes_of(&k).flatten().as_deref(), Some(&b"1"[..]));
    assert_eq!(source_key("b.png"), None, "表里没有的走原路径");
    assert_eq!(source_key("../a.png"), None);
    assert_eq!(source_key("data:image/png;base64,AA=="), None);
    assert_eq!(bytes_of("/abs/a.png"), None, "不是内存源串");
    uninstall();
    assert_eq!(bytes_of(&k), Some(None));
}

/// 两个调用方各持一份表、各自用前登记：源串按代号区分，轮流登记后都还读得到自己的图。
#[test]
fn two_owners_take_turns_without_clobbering() {
    let (ga, a) = (next_generation(), table("a.png", b"A"));
    let (gb, b) = (next_generation(), table("a.png", b"B"));
    install(ga, &a);
    let ka = source_key("a.png").unwrap();
    install(gb, &b);
    let kb = source_key("a.png").unwrap();
    assert_ne!(ka, kb);
    assert_eq!(bytes_of(&kb).flatten().as_deref(), Some(&b"B"[..]));
    install(ga, &a);
    assert_eq!(bytes_of(&ka).flatten().as_deref(), Some(&b"A"[..]));
    uninstall();
}
