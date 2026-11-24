use super::*;

#[test]
fn flat_layout_keeps_legacy_path() {
    // 出厂扁平结构：与旧的「父目录名」实现逐字节一致，存量缓存不失效。
    assert_eq!(
        schema_namespace(Path::new("d/data/schemas/wubi86/wubi86_jidian.dict.yaml")),
        PathBuf::from("wubi86")
    );
}

#[test]
fn nested_layout_keeps_schema_segment() {
    // 论坛 #115：二级子目录下，方案那一段必须留在命名空间里。
    let ns = schema_namespace(Path::new(
        "d/data/schemas/ime_wubi86/word/changyong.dict.yaml",
    ));
    assert_eq!(ns, PathBuf::from("ime_wubi86").join("word"));
}

#[test]
fn same_leaf_dir_across_schemas_does_not_collide() {
    // 用户真正担心的那个冲突：两个方案各有 word/ 且文件同名。
    let a = cache_path_in(
        Path::new("c"),
        Path::new("d/data/schemas/ime_wubi86/word/x.dict.yaml"),
        "x.wdat",
    );
    let b = cache_path_in(
        Path::new("c"),
        Path::new("d/data/schemas/ime_wubi98/word/x.dict.yaml"),
        "x.wdat",
    );
    assert_ne!(a, b);
}

#[test]
fn user_layer_and_data_layer_share_one_cache() {
    // 同一 rel 在不同层里只会有一个胜出者，共用一份缓存是有意设计（内容指纹判新鲜）。
    let user = schema_namespace(Path::new("Users/u/AppData/Roaming/W/schemas/p/a.dict.yaml"));
    let data = schema_namespace(Path::new("Program Files/W/data/schemas/p/a.dict.yaml"));
    assert_eq!(user, data);
}

#[test]
fn last_schemas_segment_wins() {
    // 数据根自己也叫 schemas 时，靠后的那个才是词库根。
    assert_eq!(
        schema_namespace(Path::new("schemas/data/schemas/pinyin/rime.dict.yaml")),
        PathBuf::from("pinyin")
    );
}

#[test]
fn dict_at_schemas_root_falls_to_cache_root() {
    assert_eq!(
        schema_namespace(Path::new("d/data/schemas/x.dict.yaml")),
        PathBuf::new()
    );
}

#[test]
fn without_schemas_segment_falls_back_to_parent_name() {
    // 测试夹具/便携版：没有 schemas 段时保持旧行为，仍按父目录名分组。
    assert_eq!(
        schema_namespace(Path::new("tmp/fixture/wubi86/x.dict.yaml")),
        PathBuf::from("wubi86")
    );
}

#[test]
fn user_dict_libs_are_grouped_by_data_schema() {
    // 用户分库 `<用户目录>/data/<数据方案>/dicts/`：不同数据方案的同名分库不撞
    let a = schema_namespace(Path::new("u/W/data/pinyin/dicts/成语.dict.yaml"));
    let b = schema_namespace(Path::new("u/W/data/wubi86/dicts/成语.dict.yaml"));
    assert_eq!(a, PathBuf::from("data").join("pinyin").join("dicts"));
    assert_ne!(a, b);
}

#[test]
fn parent_traversal_segments_are_dropped() {
    // `..` 不进命名空间：缓存写不出缓存根。
    let ns = schema_namespace(Path::new("d/data/schemas/p/../q/x.dict.yaml"));
    assert_eq!(ns, PathBuf::from("p").join("q"));
    assert!(!ns.to_string_lossy().contains(".."));
}

/// Windows 上反斜杠也是分隔符：盘符段被 `Component::Prefix` / `RootDir` 挡在外面，
/// 命名空间与正斜杠写法逐字节一致。
#[test]
#[cfg(windows)]
fn windows_backslash_layout() {
    assert_eq!(
        schema_namespace(Path::new(r"C:\d\data\schemas\ime_wubi86\word\x.dict.yaml")),
        PathBuf::from("ime_wubi86").join("word")
    );
}

#[test]
fn stem_strips_dict_infix() {
    assert_eq!(
        cache_stem(Path::new("a/rime_frost.dict.yaml")),
        "rime_frost"
    );
    assert_eq!(cache_stem(Path::new("a/en.yaml")), "en");
}
