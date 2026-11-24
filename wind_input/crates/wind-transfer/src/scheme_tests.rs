use super::*;
use std::fs;

/// 造一个最小用户 schemas 目录:my 方案引用 1 个用户码表 + 1 个系统码表 + 1 个缺失文件。
fn fixture(user: &std::path::Path, system: &std::path::Path) {
    fs::create_dir_all(user.join("my")).unwrap();
    fs::create_dir_all(system.join("sys")).unwrap();
    fs::write(
        user.join("my.schema.toml"),
        r#"
[schema]
id = "my"
version = "1.2"
[engine]
type = "codetable"
[engine.chaizi]
db_path = "my/chaizi.txt"
[[dictionaries]]
path = "my/main.dict.yaml"
[[dictionaries]]
path = "sys/shared.dict.yaml"
[[dictionaries]]
path = "my/ghost.dict.yaml"
"#,
    )
    .unwrap();
    fs::write(user.join("my/main.dict.yaml"), "d").unwrap();
    fs::write(user.join("my/chaizi.txt"), "c").unwrap();
    fs::write(system.join("sys/shared.dict.yaml"), "s").unwrap();
    // my/ghost.dict.yaml 故意不创建 → missing
}

/// 手工写一个 zip(测试守卫/识别用)。
fn write_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, data) in entries {
        w.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap();
}

/// wdat-only 词库：方案目录里只有编译好的 .wdat、没有 .dict.yaml 源。
/// 打包必须以 .wdat 命中并按 .wdat 的相对路径入包，否则导入端会还原出一个名为 yaml、
/// 内容却是二进制的文件。
#[test]
fn collect_packs_wdat_only_dict() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fs::create_dir_all(user.join("wb")).unwrap();
    fs::create_dir_all(&system).unwrap();
    fs::write(
        user.join("wb.schema.toml"),
        r#"
[schema]
id = "wb"
[engine]
type = "codetable"
[[dictionaries]]
path = "wb/main.dict.yaml"
"#,
    )
    .unwrap();
    // 只投放 wdat，不放 yaml
    fs::write(user.join("wb/main.wdat"), b"binary").unwrap();

    let plan =
        collect_package_files("wb", &user, std::slice::from_ref(&system), None, false).unwrap();
    let names: Vec<&str> = plan.pack.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        names.contains(&"wb/main.wdat"),
        "wdat-only 词库须以 .wdat 路径入包，实际: {names:?}"
    );
    assert!(
        !names.contains(&"wb/main.dict.yaml"),
        "不存在的 yaml 不应入包"
    );
    assert!(
        plan.missing.is_empty(),
        "wdat 在场就不算缺失，实际: {:?}",
        plan.missing
    );
    // 入包的源路径必须指向真实文件
    let src = plan
        .pack
        .iter()
        .find(|(n, _)| n == "wb/main.wdat")
        .map(|(_, p)| p.clone())
        .unwrap();
    assert!(src.is_file());
}

/// 多个系统目录（`data_custom` 在前、`data` 在后）：
///
/// 1. 只存在于 `data_custom` 的方案不得判成 `Missing`——只传 data 层时，定制版的方案
///    导出会得到一个空包/全进 missing，用户拿它装不回去；
/// 2. 它归 `System` 而不是 `User`：同一个 `locate` 也服务删除路径，判成 `User` 就等于
///    允许程序删 `data_custom` 里的文件（破不变量「程序永不写 data_custom」）；
/// 3. 自包含导出（`include_system = true`）照样把它读源打包，包在别的机器上可用；
/// 4. 同名文件靠前的层（custom）胜出。
#[test]
fn custom_layer_counts_as_system_not_missing() {
    let t = tempfile::tempdir().unwrap();
    let (user, custom, data) = (t.path().join("u"), t.path().join("dc"), t.path().join("d"));
    fs::create_dir_all(user.join("tiger")).unwrap();
    fs::create_dir_all(custom.join("tiger")).unwrap();
    fs::create_dir_all(data.join("tiger")).unwrap();
    // 方案文件与主词库只在定制层；另有一本词库两层都有（定制层应胜出）。
    fs::write(
        custom.join("tiger.schema.toml"),
        "[schema]\nid = \"tiger\"\n[engine]\ntype = \"codetable\"\n\
             [[dictionaries]]\npath = \"tiger/main.dict.yaml\"\n\
             [[dictionaries]]\npath = \"tiger/both.dict.yaml\"\n",
    )
    .unwrap();
    fs::write(custom.join("tiger/main.dict.yaml"), "custom-main").unwrap();
    fs::write(custom.join("tiger/both.dict.yaml"), "custom-both").unwrap();
    fs::write(data.join("tiger/both.dict.yaml"), "data-both").unwrap();

    let sys = vec![custom.clone(), data.clone()];

    // 自包含导出：定制层的方案文件与词库都读源打包，missing 为空。
    let plan = collect_package_files("tiger", &user, &sys, None, true).unwrap();
    let names: Vec<&str> = plan.pack.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        names.contains(&"tiger.schema.toml") && names.contains(&"tiger/main.dict.yaml"),
        "只存在于 data_custom 的方案与词库必须入包，实际: {names:?}"
    );
    assert!(
        plan.missing.is_empty(),
        "定制层命中不得记成缺失，实际: {:?}",
        plan.missing
    );
    let both = plan
        .pack
        .iter()
        .find(|(n, _)| n == "tiger/both.dict.yaml")
        .map(|(_, p)| p.clone())
        .expect("both 应入包");
    assert_eq!(
        fs::read_to_string(&both).unwrap(),
        "custom-both",
        "同名文件靠前的层（custom）胜出"
    );

    // 删除路径（include_system=false）：非根的系统命中只记引用；根方案本身仍进 pack，
    // 删除侧另有 user_dir 前缀守卫，这里只断言资源不会被当成可删的用户文件。
    let del = collect_package_files("tiger", &user, &sys, None, false).unwrap();
    assert!(
        del.system_refs
            .contains(&"tiger/main.dict.yaml".to_string()),
        "删除路径下定制层资源必须只记 system_refs，实际: {:?}",
        del.system_refs
    );
    assert!(
        !del.pack.iter().any(|(n, _)| n == "tiger/main.dict.yaml"),
        "定制层资源不得进入删除路径的 pack（那等于允许程序删 data_custom）"
    );
}

/// yaml 与 wdat 并存时走原路径，wdat 不得抢占（正常方案行为一步不变）。
#[test]
fn yaml_wins_over_sibling_wdat_when_packing() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fs::create_dir_all(user.join("wb")).unwrap();
    fs::create_dir_all(&system).unwrap();
    fs::write(
        user.join("wb.schema.toml"),
        "[schema]\nid = \"wb\"\n[engine]\ntype = \"codetable\"\n[[dictionaries]]\npath = \"wb/main.dict.yaml\"\n",
    )
    .unwrap();
    fs::write(user.join("wb/main.dict.yaml"), "src").unwrap();
    fs::write(user.join("wb/main.wdat"), b"binary").unwrap();

    let plan =
        collect_package_files("wb", &user, std::slice::from_ref(&system), None, false).unwrap();
    let names: Vec<&str> = plan.pack.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"wb/main.dict.yaml"));
    assert!(!names.contains(&"wb/main.wdat"), "yaml 在场时不应改打 wdat");
}

/// 非词库资源不受 wdat 探测影响：缺失仍按原相对路径上报。
#[test]
fn non_dict_resources_keep_original_missing_path() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    let plan =
        collect_package_files("my", &user, std::slice::from_ref(&system), None, false).unwrap();
    assert_eq!(
        plan.missing,
        vec!["my/ghost.dict.yaml"],
        "缺失路径须保持用户配置里写的原样"
    );
}

#[test]
fn collect_classifies_pack_ref_missing() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    let plan =
        collect_package_files("my", &user, std::slice::from_ref(&system), None, false).unwrap();
    let names: Vec<&str> = plan.pack.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"my.schema.toml"), "根方案文件必打包");
    assert!(names.contains(&"my/main.dict.yaml"));
    assert!(names.contains(&"my/chaizi.txt"));
    assert_eq!(plan.system_refs, vec!["sys/shared.dict.yaml"]);
    assert_eq!(plan.missing, vec!["my/ghost.dict.yaml"]);
    assert_eq!(plan.schema_ids, vec!["my"]);
    assert_eq!(plan.root_version, "1.2", "根方案版本从 schema.toml 提取");
}

#[test]
fn collect_recurses_mixed_user_schema() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    // mixed 方案引用用户方案 my + 系统方案 pinyin(引用方案文件在用户目录→打包递归;
    // 在系统目录→system_ref;均无→missing)
    fs::write(
        user.join("mix.schema.toml"),
        r#"
[schema]
id = "mix"
[engine]
type = "mixed"
[engine.mixed]
primary_schema = "my"
secondary_schema = "pinyin"
"#,
    )
    .unwrap();
    fs::write(
        system.join("pinyin.schema.toml"),
        "[schema]\nid=\"pinyin\"\n",
    )
    .unwrap();
    let plan =
        collect_package_files("mix", &user, std::slice::from_ref(&system), None, false).unwrap();
    let names: Vec<&str> = plan.pack.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"mix.schema.toml"));
    assert!(names.contains(&"my.schema.toml"), "用户引用方案递归打包");
    assert!(names.contains(&"my/main.dict.yaml"), "递归方案的资源也打包");
    assert!(
        plan.system_refs.contains(&"pinyin.schema.toml".to_string()),
        "系统引用方案只记引用"
    );
    assert_eq!(plan.schema_ids, vec!["mix", "my"]);
}

/// 自包含模式(include_system=true):系统目录命中的资源改为读源打包,system_refs 清空。
/// 这正是内置方案(如 wubi86)导出时词库能进包的关键。
#[test]
fn collect_self_contained_packs_system_resources() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    let plan =
        collect_package_files("my", &user, std::slice::from_ref(&system), None, true).unwrap();
    let names: Vec<&str> = plan.pack.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        names.contains(&"sys/shared.dict.yaml"),
        "系统词库自包含时须入包,实际: {names:?}"
    );
    assert!(
        plan.system_refs.is_empty(),
        "自包含时无系统引用,实际: {:?}",
        plan.system_refs
    );
    assert_eq!(plan.missing, vec!["my/ghost.dict.yaml"], "真缺失仍上报");
    // 入包的系统源路径必须指向真实文件
    let src = plan
        .pack
        .iter()
        .find(|(n, _)| n == "sys/shared.dict.yaml")
        .map(|(_, p)| p.clone())
        .unwrap();
    assert!(src.starts_with(&system) && src.is_file());
}

/// 自包含模式下,混输引用的系统子方案(及其资源)一并递归打包,而非只记引用。
#[test]
fn collect_self_contained_recurses_system_subschema() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    fs::create_dir_all(system.join("pinyin")).unwrap();
    fs::write(
        user.join("mix.schema.toml"),
        "[schema]\nid=\"mix\"\n[engine]\ntype=\"mixed\"\n[engine.mixed]\nprimary_schema=\"my\"\nsecondary_schema=\"pinyin\"\n",
    )
    .unwrap();
    fs::write(
        system.join("pinyin.schema.toml"),
        "[schema]\nid=\"pinyin\"\n[engine]\ntype=\"pinyin\"\n[[dictionaries]]\npath=\"pinyin/main.dict.yaml\"\n",
    )
    .unwrap();
    fs::write(system.join("pinyin/main.dict.yaml"), "py").unwrap();

    let plan =
        collect_package_files("mix", &user, std::slice::from_ref(&system), None, true).unwrap();
    let names: Vec<&str> = plan.pack.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"pinyin.schema.toml"), "系统子方案文件入包");
    assert!(
        names.contains(&"pinyin/main.dict.yaml"),
        "系统子方案的词库也入包,实际: {names:?}"
    );
    assert!(plan.system_refs.is_empty(), "自包含无系统引用");
    assert_eq!(plan.schema_ids, vec!["mix", "my", "pinyin"]);
}

#[test]
fn export_package_writes_flat_layout_and_meta() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    let out = t.path().join("my.zip");
    let r = export_package(
        "my",
        &user,
        std::slice::from_ref(&system),
        None,
        &out,
        "1.0.0",
        "windows",
        "t",
    )
    .unwrap();
    assert_eq!(r.path, out);
    // 自包含导出:用户资源 3 个 + 系统 sys/shared.dict.yaml 一并打包 = 4;系统引用清空。
    assert_eq!(r.packed.len(), 4);
    assert!(r.system_refs.is_empty(), "自包含导出无系统引用");
    assert_eq!(r.missing.len(), 1);
    // 零层级布局:条目名即 schemas 相对路径
    let bytes = crate::bundle::extract_entry(&out, "my/main.dict.yaml").unwrap();
    assert_eq!(bytes, b"d");
    assert!(crate::bundle::extract_entry(&out, "my.schema.toml").is_ok());
    // 系统词库内容确实进了包
    assert_eq!(
        crate::bundle::extract_entry(&out, "sys/shared.dict.yaml").unwrap(),
        b"s"
    );
    // package.toml 元信息完备
    let meta = read_package_meta(&out).unwrap();
    assert_eq!(meta.package.format_version, PACKAGE_FORMAT_VERSION);
    assert_eq!(meta.package.app_version, "1.0.0");
    assert_eq!(meta.schema.id, "my");
    assert_eq!(meta.schema.version, "1.2");
    assert!(meta.refs.system.is_empty(), "自包含导出无系统引用");
    assert_eq!(meta.refs.missing, vec!["my/ghost.dict.yaml"]);
}

/// 有 override 且换了双拼布局:导出按折叠视图收集(新布局文件入包),
/// 包内方案文件写合并结果——导入方无需 override 机制即得到定制后行为。
#[test]
fn export_folds_override_and_packs_new_layout() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fs::create_dir_all(user.join("shuangpin")).unwrap();
    fs::create_dir_all(&system).unwrap();
    fs::write(
        user.join("sp.schema.toml"),
        r#"
[schema]
id = "sp"
[engine]
type = "pinyin"
[engine.pinyin]
scheme = "shuangpin"
[engine.pinyin.shuangpin]
layout = "old"
"#,
    )
    .unwrap();
    fs::write(user.join("shuangpin/old.toml"), "old-layout").unwrap();
    fs::write(user.join("shuangpin/new.toml"), "new-layout").unwrap();
    let ov_dir = t.path().join("schema_overrides");
    fs::create_dir_all(&ov_dir).unwrap();
    fs::write(
        ov_dir.join("sp.toml"),
        "[engine.pinyin.shuangpin]\nlayout = \"new\"\n",
    )
    .unwrap();

    let out = t.path().join("sp.zip");
    let r = export_package(
        "sp",
        &user,
        std::slice::from_ref(&system),
        Some(&ov_dir),
        &out,
        "1.0.0",
        "windows",
        "t",
    )
    .unwrap();
    assert!(
        r.packed.contains(&"shuangpin/new.toml".to_string()),
        "override 新指向的布局须入包,实际: {:?}",
        r.packed
    );
    assert!(
        !r.packed.contains(&"shuangpin/old.toml".to_string()),
        "折叠后不再引用旧布局,不应入包"
    );
    // 包内方案文件 = 折叠结果,而非源文件字节
    let bytes = crate::bundle::extract_entry(&out, "sp.schema.toml").unwrap();
    let schema: wind_config::schema::Schema =
        toml::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(
        schema.engine.pinyin.shuangpin.layout, "new",
        "包内方案文件须含 override 值"
    );
}

/// 无 override(目录给了但没有该方案的文件):方案文件字节原样入包,
/// 打包清单与不传 override 目录时逐条目一致。
#[test]
fn export_without_override_keeps_schema_bytes() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    let ov_dir = t.path().join("schema_overrides");
    fs::create_dir_all(&ov_dir).unwrap(); // 目录在场,但无 my.toml
    let out = t.path().join("my.zip");
    let r = export_package(
        "my",
        &user,
        std::slice::from_ref(&system),
        Some(&ov_dir),
        &out,
        "1.0.0",
        "windows",
        "t",
    )
    .unwrap();
    let baseline =
        collect_package_files("my", &user, std::slice::from_ref(&system), None, true).unwrap();
    let baseline_rels: Vec<String> = baseline.pack.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(r.packed, baseline_rels, "无 override 时打包清单不变");
    // 字节级:包内方案文件 == 源文件原字节(不走 Value 序列化往返)
    let in_zip = crate::bundle::extract_entry(&out, "my.schema.toml").unwrap();
    let on_disk = fs::read(user.join("my.schema.toml")).unwrap();
    assert_eq!(in_zip, on_disk, "无 override 的方案文件须字节级不变");
}

/// override 在场但解析失败 → 导出硬失败。静默按无 override 处理 = 用户以为
/// 定制随包带走了,实际导出的是未定制方案。
#[test]
fn export_fails_on_corrupt_override() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    let ov_dir = t.path().join("schema_overrides");
    fs::create_dir_all(&ov_dir).unwrap();
    fs::write(ov_dir.join("my.toml"), "not toml at all {{{").unwrap();
    let out = t.path().join("my.zip");
    let err = export_package(
        "my",
        &user,
        std::slice::from_ref(&system),
        Some(&ov_dir),
        &out,
        "1.0.0",
        "windows",
        "t",
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        err.contains("override"),
        "错误须指明 override 层问题: {err}"
    );
}

/// mixed 子方案的 override 同样折叠:子方案 override 新指向的资源入包,
/// 包内子方案文件写合并结果;无 override 的根方案文件字节不变。
#[test]
fn export_folds_mixed_subschema_override() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    fs::write(user.join("my/chaizi2.txt"), "c2").unwrap();
    fs::write(
        user.join("mix.schema.toml"),
        "[schema]\nid=\"mix\"\n[engine]\ntype=\"mixed\"\n[engine.mixed]\nprimary_schema=\"my\"\n",
    )
    .unwrap();
    let ov_dir = t.path().join("schema_overrides");
    fs::create_dir_all(&ov_dir).unwrap();
    fs::write(
        ov_dir.join("my.toml"),
        "[engine.chaizi]\ndb_path = \"my/chaizi2.txt\"\n",
    )
    .unwrap();

    let out = t.path().join("mix.zip");
    let r = export_package(
        "mix",
        &user,
        std::slice::from_ref(&system),
        Some(&ov_dir),
        &out,
        "1.0.0",
        "windows",
        "t",
    )
    .unwrap();
    assert!(
        r.packed.contains(&"my/chaizi2.txt".to_string()),
        "子方案 override 新指向的资源须入包,实际: {:?}",
        r.packed
    );
    let bytes = crate::bundle::extract_entry(&out, "my.schema.toml").unwrap();
    let sub: wind_config::schema::Schema =
        toml::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(
        sub.engine.chaizi.db_path, "my/chaizi2.txt",
        "包内子方案文件须含 override 值"
    );
    // 根方案无 override → 字节不变
    let root_in_zip = crate::bundle::extract_entry(&out, "mix.schema.toml").unwrap();
    assert_eq!(root_in_zip, fs::read(user.join("mix.schema.toml")).unwrap());
}

#[test]
fn import_roundtrip_into_fresh_dir() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    let out = t.path().join("my.zip");
    export_package(
        "my",
        &user,
        std::slice::from_ref(&system),
        None,
        &out,
        "1.0.0",
        "windows",
        "t",
    )
    .unwrap();

    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let prev = preview_import(&out, &dest).unwrap();
    // 自包含包含系统词库 → 4 个条目全部待新增,无系统引用。
    assert_eq!(prev.will_add.len(), 4);
    assert!(prev.conflicts.is_empty());
    assert!(prev.system_refs.is_empty());
    assert_eq!(prev.meta.schema.id, "my");

    let r = import_package(&out, &dest, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.imported.len(), 4);
    assert!(r.conflicts.is_empty());
    assert_eq!(r.schema_ids, vec!["my"]);
    assert!(dest.join("my.schema.toml").is_file());
    // 系统词库随包落进目标用户目录
    assert!(dest.join("sys/shared.dict.yaml").is_file());
    assert!(
        !dest.join(PACKAGE_META_NAME).exists(),
        "package.toml 是元信息,不落盘"
    );
    assert_eq!(std::fs::read(dest.join("my/main.dict.yaml")).unwrap(), b"d");
}

#[test]
fn import_merge_skips_existing_replace_overwrites() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    let out = t.path().join("my.zip");
    export_package(
        "my",
        &user,
        std::slice::from_ref(&system),
        None,
        &out,
        "1.0.0",
        "windows",
        "t",
    )
    .unwrap();

    let dest = t.path().join("dest");
    std::fs::create_dir_all(dest.join("my")).unwrap();
    std::fs::write(dest.join("my/main.dict.yaml"), b"OLD").unwrap();

    // preview 报冲突
    let prev = preview_import(&out, &dest).unwrap();
    assert_eq!(prev.conflicts, vec!["my/main.dict.yaml"]);

    // Merge:跳过已存在,内容保持 OLD(自包含包 4 条,冲突 1 条 → 导入 3 条)
    let r = import_package(&out, &dest, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.conflicts, vec!["my/main.dict.yaml"]);
    assert_eq!(r.imported.len(), 3);
    assert_eq!(
        std::fs::read(dest.join("my/main.dict.yaml")).unwrap(),
        b"OLD"
    );

    // Replace:覆盖为包内内容(4 条全写)
    let r2 = import_package(&out, &dest, crate::merge::Strategy::Replace).unwrap();
    assert_eq!(r2.imported.len(), 4);
    assert!(r2.conflicts.is_empty());
    assert_eq!(std::fs::read(dest.join("my/main.dict.yaml")).unwrap(), b"d");
}

#[test]
fn import_without_package_meta_still_works() {
    // 手工打包(无 package.toml)也能导入——方案文件本身即信息源。
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("hand.zip");
    write_zip(
        &pkg,
        &[
            ("hand.schema.toml", b"[schema]\nid=\"hand\"\n".as_slice()),
            ("hand/d.dict.yaml", b"d".as_slice()),
        ],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let prev = preview_import(&pkg, &dest).unwrap();
    assert_eq!(prev.will_add.len(), 2);
    assert!(prev.meta.package.created_at.is_empty(), "无元信息回落默认");
    assert_eq!(
        prev.meta.package.format_version, 1,
        "无元信息视为 legacy v1"
    );
    let r = import_package(&pkg, &dest, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.schema_ids, vec!["hand"]);
    assert!(dest.join("hand.schema.toml").is_file());
}

/// package.toml 在场但无 format_version 字段 → legacy v1,照常导入。
#[test]
fn import_meta_without_format_version_is_legacy() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("legacy.zip");
    write_zip(
        &pkg,
        &[
            (
                PACKAGE_META_NAME,
                b"[package]\napp_version = \"0.9.0\"\n".as_slice(),
            ),
            (
                "legacy.schema.toml",
                b"[schema]\nid=\"legacy\"\n".as_slice(),
            ),
        ],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let prev = preview_import(&pkg, &dest).unwrap();
    assert_eq!(prev.meta.package.format_version, 1, "缺字段即 legacy v1");
    assert_eq!(prev.meta.package.app_version, "0.9.0");
    let r = import_package(&pkg, &dest, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.schema_ids, vec!["legacy"]);
}

/// 当前规格版本(=2)的包正常通过门禁。
#[test]
fn import_accepts_current_format_version() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("v2.zip");
    write_zip(
        &pkg,
        &[
            (
                PACKAGE_META_NAME,
                b"[package]\nformat_version = 2\n".as_slice(),
            ),
            ("v2.schema.toml", b"[schema]\nid=\"v2\"\n".as_slice()),
        ],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let prev = preview_import(&pkg, &dest).unwrap();
    assert_eq!(prev.meta.package.format_version, 2);
    let r = import_package(&pkg, &dest, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.schema_ids, vec!["v2"]);
}

/// 更高 format_version → preview 与 import 都硬拒绝,并提示升级应用。
/// 宽容只给过去,不给未来。
#[test]
fn import_rejects_future_format_version() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("v3.zip");
    write_zip(
        &pkg,
        &[
            (
                PACKAGE_META_NAME,
                b"[package]\nformat_version = 3\n".as_slice(),
            ),
            ("v3.schema.toml", b"[schema]\nid=\"v3\"\n".as_slice()),
        ],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let err = preview_import(&pkg, &dest).unwrap_err().to_string();
    assert!(err.contains("升级"), "拒绝信息须提示升级应用: {err}");
    let err2 = import_package(&pkg, &dest, crate::merge::Strategy::Merge)
        .err()
        .unwrap()
        .to_string();
    assert!(err2.contains("升级"), "import 同样拒绝: {err2}");
    assert!(
        !dest.join("v3.schema.toml").exists(),
        "拒绝的包不得落盘任何文件"
    );
}

/// package.toml 声明了 format_version 却解析损坏(类型错)→ 硬报错,不得静默
/// 回落 legacy 绕过门禁。
#[test]
fn import_rejects_corrupt_meta_declaring_format_version() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("corrupt.zip");
    write_zip(
        &pkg,
        &[
            (
                PACKAGE_META_NAME,
                b"[package]\nformat_version = \"not-a-number\"\n".as_slice(),
            ),
            ("c.schema.toml", b"[schema]\nid=\"c\"\n".as_slice()),
        ],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let err = preview_import(&pkg, &dest).unwrap_err().to_string();
    assert!(err.contains("无法解析"), "损坏元信息须报解析错误: {err}");
    assert!(import_package(&pkg, &dest, crate::merge::Strategy::Merge).is_err());

    // 对照:同样解析失败但未声明 format_version → 维持 legacy 宽容,照常导入。
    let pkg2 = t.path().join("garbage-meta.zip");
    write_zip(
        &pkg2,
        &[
            (PACKAGE_META_NAME, b"not toml at all {{{".as_slice()),
            ("g.schema.toml", b"[schema]\nid=\"g\"\n".as_slice()),
        ],
    );
    let prev = preview_import(&pkg2, &dest).unwrap();
    assert_eq!(prev.meta.package.format_version, 1, "无声明回落 legacy");
    let r = import_package(&pkg2, &dest, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.schema_ids, vec!["g"]);
}

// ── 说明元信息(title / description)──

/// 包级说明正常读出:title 单行、description 可多行,首尾空白 trim。
#[test]
fn import_reads_package_title_and_description() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("info.zip");
    write_zip(
        &pkg,
        &[
            (
                PACKAGE_META_NAME,
                "[package]\nformat_version = 2\ntitle = \"快符方案\"\n\
                     description = \"\"\"\n分号引导的符号表。\n含常用标点。\n\"\"\"\n"
                    .as_bytes(),
            ),
            ("info.schema.toml", b"[schema]\nid=\"info\"\n".as_slice()),
        ],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let prev = preview_import(&pkg, &dest).unwrap();
    assert_eq!(prev.meta.package.title, "快符方案");
    assert_eq!(
        prev.meta.package.description,
        "分号引导的符号表。\n含常用标点。"
    );
    // 缺省即空串(既有风格),不报错。
    let prev2 = {
        let pkg2 = t.path().join("noinfo.zip");
        write_zip(
            &pkg2,
            &[
                (
                    PACKAGE_META_NAME,
                    b"[package]\nformat_version = 2\n".as_slice(),
                ),
                (
                    "noinfo.schema.toml",
                    b"[schema]\nid=\"noinfo\"\n".as_slice(),
                ),
            ],
        );
        preview_import(&pkg2, &dest).unwrap()
    };
    assert!(prev2.meta.package.title.is_empty());
    assert!(prev2.meta.package.description.is_empty());
}

/// 说明非法(换行 / 超长 / 控制字符 / 类型不对)→ 与 format_version 门禁同层的硬错误,
/// preview 与 import 都拒绝,且零落盘。校验复用 wind-config 的同一份实现。
#[test]
fn import_rejects_invalid_package_info() {
    let t = tempfile::tempdir().unwrap();
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let long_title = "字".repeat(wind_config::patch::INFO_TITLE_MAX_CHARS + 1);
    let long_desc = "说".repeat(wind_config::patch::INFO_DESCRIPTION_MAX_CHARS + 1);
    let cases: Vec<String> = vec![
        "[package]\nformat_version = 2\ntitle = \"第一行\\n第二行\"\n".to_string(),
        format!("[package]\nformat_version = 2\ntitle = \"{long_title}\"\n"),
        format!("[package]\nformat_version = 2\ndescription = \"{long_desc}\"\n"),
        "[package]\nformat_version = 2\ndescription = \"说明\\u0007\"\n".to_string(),
        "[package]\nformat_version = 2\ntitle = 5\n".to_string(),
    ];
    for (i, meta) in cases.iter().enumerate() {
        let pkg = t.path().join(format!("bad{i}.zip"));
        write_zip(
            &pkg,
            &[
                (PACKAGE_META_NAME, meta.as_bytes()),
                ("bad.schema.toml", b"[schema]\nid=\"bad\"\n".as_slice()),
            ],
        );
        assert!(preview_import(&pkg, &dest).is_err(), "应拒绝: {meta}");
        assert!(
            import_package(&pkg, &dest, crate::merge::Strategy::Merge).is_err(),
            "import 同样拒绝: {meta}"
        );
    }
    assert!(
        !dest.join("bad.schema.toml").exists(),
        "拒绝的包不得落盘任何文件"
    );
}

/// 版本门禁**先于**说明校验:未来版本很可能放宽说明限额,拿当前规则去判更高版本的包
/// 本身就是错的,且"请升级"才对得上真实原因。同一个包若两处都不合法,必须报版本过高。
#[test]
fn future_format_version_reported_before_invalid_info() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("v3-badinfo.zip");
    let long_title = "字".repeat(wind_config::patch::INFO_TITLE_MAX_CHARS + 1);
    write_zip(
        &pkg,
        &[
            (
                PACKAGE_META_NAME,
                format!("[package]\nformat_version = 3\ntitle = \"{long_title}\"\n").as_bytes(),
            ),
            ("v3.schema.toml", b"[schema]\nid=\"v3\"\n".as_slice()),
        ],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    for err in [
        preview_import(&pkg, &dest).unwrap_err().to_string(),
        import_package(&pkg, &dest, crate::merge::Strategy::Merge)
            .err()
            .expect("import 同样应拒绝")
            .to_string(),
    ] {
        assert!(err.contains("升级"), "应先报版本过高: {err}");
        assert!(
            !err.contains("过长"),
            "不该拿当前规则去判更高版本的说明: {err}"
        );
    }
}

/// 包级说明与包内 config_patch 自带的说明**互不干扰**:两级说的不是同一件事,
/// 不合并、不做优先级判定。片段原文原样上浮(含它自己的 `[package]` 段)。
#[test]
fn package_info_and_config_patch_info_stay_separate() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("twolevel.zip");
    let patch = "[package]\ntitle = \"片段级说明\"\n[ui.candidate]\nper_page = 9\n";
    write_zip(
        &pkg,
        &[
            (
                PACKAGE_META_NAME,
                "[package]\nformat_version = 2\ntitle = \"包级说明\"\n".as_bytes(),
            ),
            ("two.schema.toml", b"[schema]\nid=\"two\"\n".as_slice()),
            (CONFIG_PATCH_NAME, patch.as_bytes()),
        ],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let prev = preview_import(&pkg, &dest).unwrap();
    assert_eq!(prev.meta.package.title, "包级说明");
    assert_eq!(
        prev.config_patch.as_deref(),
        Some(patch),
        "片段原文原样上浮,包级说明不并入也不覆盖"
    );
}

// ── config_patch.toml(配置包)──

/// v2 包里的 config_patch.toml:不进 will_add/conflicts、不落盘,原文随预览返回。
#[test]
fn config_patch_is_returned_not_written() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("cfg.zip");
    write_zip(
        &pkg,
        &[
            (
                PACKAGE_META_NAME,
                b"[package]\nformat_version = 2\n".as_slice(),
            ),
            ("cfg.schema.toml", b"[schema]\nid=\"cfg\"\n".as_slice()),
            (
                CONFIG_PATCH_NAME,
                "[ui.candidate]\nper_page = 9\n".as_bytes(),
            ),
        ],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();

    let prev = preview_import(&pkg, &dest).unwrap();
    assert_eq!(
        prev.will_add,
        vec!["cfg.schema.toml"],
        "config_patch 不进文件清单,实际: {:?}",
        prev.will_add
    );
    assert!(prev.conflicts.is_empty());
    assert_eq!(
        prev.config_patch.as_deref(),
        Some("[ui.candidate]\nper_page = 9\n"),
        "片段原文须随预览返回"
    );

    let r = import_package(&pkg, &dest, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.imported, vec!["cfg.schema.toml"]);
    assert!(
        !dest.join(CONFIG_PATCH_NAME).exists(),
        "config_patch 不是方案资源,不得落进 schemas 目录"
    );
}

/// 不含 config_patch 的普通包:字段为 None(不是空串),消费端据此区分「没有片段」。
#[test]
fn plain_package_has_no_config_patch() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("plain.zip");
    write_zip(
        &pkg,
        &[("p.schema.toml", b"[schema]\nid=\"p\"\n".as_slice())],
    );
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    assert!(preview_import(&pkg, &dest).unwrap().config_patch.is_none());
}

/// legacy(v1 / 无 package.toml)包里出现 config_patch → 硬拒绝并提示重新打包。
/// v1 语义下旧客户端会把它当死文件落盘,生成端必须显式声明版本。
#[test]
fn config_patch_in_legacy_package_is_rejected() {
    let t = tempfile::tempdir().unwrap();
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();

    // 无 package.toml
    let pkg = t.path().join("v1-nometa.zip");
    write_zip(
        &pkg,
        &[
            ("v1.schema.toml", b"[schema]\nid=\"v1\"\n".as_slice()),
            (CONFIG_PATCH_NAME, b"ui.candidate.per_page = 9\n".as_slice()),
        ],
    );
    let err = preview_import(&pkg, &dest).unwrap_err().to_string();
    assert!(err.contains("重新打包"), "须提示按新格式重新打包: {err}");
    assert!(import_package(&pkg, &dest, crate::merge::Strategy::Merge).is_err());
    assert!(
        !dest.join("v1.schema.toml").exists(),
        "拒绝的包不得落盘任何文件"
    );

    // package.toml 在场但声明 format_version = 1
    let pkg2 = t.path().join("v1-meta.zip");
    write_zip(
        &pkg2,
        &[
            (
                PACKAGE_META_NAME,
                b"[package]\nformat_version = 1\n".as_slice(),
            ),
            ("v1b.schema.toml", b"[schema]\nid=\"v1b\"\n".as_slice()),
            (CONFIG_PATCH_NAME, b"ui.candidate.per_page = 9\n".as_slice()),
        ],
    );
    assert!(preview_import(&pkg2, &dest).is_err());
}

#[test]
fn delete_package_removes_exclusive_keeps_shared() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system); // my: my/main.dict.yaml + my/chaizi.txt 独享
    // other 方案与 my 共享一个码表文件
    fs::create_dir_all(user.join("shared")).unwrap();
    fs::write(user.join("shared/common.dict.yaml"), "common").unwrap();
    fs::write(
        user.join("other.schema.toml"),
        "[schema]\nid=\"other\"\n[engine]\ntype=\"codetable\"\n[[dictionaries]]\npath=\"shared/common.dict.yaml\"\n",
    )
    .unwrap();
    // my 也引用 shared/common:追加进 my 的 dictionaries
    let my_toml = fs::read_to_string(user.join("my.schema.toml")).unwrap();
    fs::write(
        user.join("my.schema.toml"),
        format!("{my_toml}[[dictionaries]]\npath = \"shared/common.dict.yaml\"\n"),
    )
    .unwrap();

    let keep = vec!["other".to_string()];
    let r = delete_package("my", &user, std::slice::from_ref(&system), &keep).unwrap();
    assert!(r.deleted.contains(&"my.schema.toml".to_string()));
    assert!(r.deleted.contains(&"my/main.dict.yaml".to_string()));
    assert!(
        r.kept_shared
            .contains(&"shared/common.dict.yaml".to_string()),
        "共享文件保留"
    );
    assert_eq!(r.schema_ids, vec!["my"]);
    assert!(!user.join("my.schema.toml").exists());
    assert!(!user.join("my").exists(), "独享资源目录删空后应移除");
    assert!(
        user.join("shared/common.dict.yaml").is_file(),
        "共享文件仍在"
    );
    assert!(user.join("other.schema.toml").is_file(), "其它方案不受影响");
}

#[test]
fn delete_package_mixed_recurses_and_never_touches_system() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    fs::write(
        user.join("mix.schema.toml"),
        "[schema]\nid=\"mix\"\n[engine]\ntype=\"mixed\"\n[engine.mixed]\nprimary_schema=\"my\"\nsecondary_schema=\"pinyin\"\n",
    )
    .unwrap();
    fs::write(
        system.join("pinyin.schema.toml"),
        "[schema]\nid=\"pinyin\"\n",
    )
    .unwrap();

    // 无其它方案共享 → mix 及其递归引用的用户方案 my 一并删除
    let r = delete_package("mix", &user, std::slice::from_ref(&system), &[]).unwrap();
    assert!(r.deleted.contains(&"mix.schema.toml".to_string()));
    assert!(
        r.deleted.contains(&"my.schema.toml".to_string()),
        "递归引用的用户方案一并删"
    );
    assert_eq!(r.schema_ids, vec!["mix", "my"]);
    assert!(!user.join("my").exists());
    assert!(
        system.join("pinyin.schema.toml").is_file(),
        "系统目录文件永不删"
    );
    assert!(
        system.join("sys/shared.dict.yaml").is_file(),
        "系统资源永不删"
    );
}

#[test]
fn delete_package_keeps_subschema_shared_by_other_mixed() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    // 两个混输方案都引用用户方案 my:删 mix1 时 my 因被 mix2 共享而保留
    for m in ["mix1", "mix2"] {
        fs::write(
            user.join(format!("{m}.schema.toml")),
            format!(
                "[schema]\nid=\"{m}\"\n[engine]\ntype=\"mixed\"\n[engine.mixed]\nprimary_schema=\"my\"\n"
            ),
        )
        .unwrap();
    }
    let keep = vec!["mix2".to_string()];
    let r = delete_package("mix1", &user, std::slice::from_ref(&system), &keep).unwrap();
    assert!(r.deleted.contains(&"mix1.schema.toml".to_string()));
    assert!(
        r.kept_shared.contains(&"my.schema.toml".to_string()),
        "被 mix2 共享的子方案保留"
    );
    assert_eq!(r.schema_ids, vec!["mix1"], "只有 mix1 计入被删方案");
    assert!(user.join("my.schema.toml").is_file());
    assert!(
        user.join("my/main.dict.yaml").is_file(),
        "共享子方案的资源保留"
    );
}

#[test]
fn import_rejects_path_traversal_and_wrong_kind() {
    let t = tempfile::tempdir().unwrap();
    let dest = t.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();

    // 含 .. 条目应拒绝
    let bad = t.path().join("bad.zip");
    write_zip(
        &bad,
        &[
            ("my.schema.toml", b"x".as_slice()),
            ("../evil.toml", b"x".as_slice()),
        ],
    );
    assert!(preview_import(&bad, &dest).is_err(), "含 .. 条目应拒绝");
    assert!(import_package(&bad, &dest, crate::merge::Strategy::Merge).is_err());

    // Windows 盘符相对路径(C:foo):is_absolute()==false 但 join 会丢 base,必须拦。
    let bad2 = t.path().join("bad2.zip");
    write_zip(
        &bad2,
        &[
            ("my.schema.toml", b"x".as_slice()),
            ("C:evil.toml", b"x".as_slice()),
        ],
    );
    assert!(preview_import(&bad2, &dest).is_err(), "盘符相对路径应拒绝");
    assert!(import_package(&bad2, &dest, crate::merge::Strategy::Merge).is_err());

    // 根下无 *.schema.toml → 不是方案包
    let notpkg = t.path().join("not.zip");
    write_zip(&notpkg, &[("readme.txt", b"x".as_slice())]);
    assert!(preview_import(&notpkg, &dest).is_err());

    // 备份包(含 manifest.toml)→ 针对性报错
    let backup = t.path().join("backup.zip");
    write_zip(
        &backup,
        &[
            (
                "manifest.toml",
                b"format = \"windinput-bundle\"\n".as_slice(),
            ),
            ("config/config.toml", b"x".as_slice()),
        ],
    );
    let err = preview_import(&backup, &dest).unwrap_err().to_string();
    assert!(err.contains("备份包"), "误选备份包应有针对性提示: {err}");
}

// ───────── 方案文件内容里的越界路径 ─────────
//
// 归档条目名的守卫（`import_rejects_path_traversal_and_wrong_kind`）拦不住这一类：越界路径
// 可以藏在 `evil.schema.toml` 的内容里。方案文件落盘后仍是同一个包带进来的外来数据。

/// 造一个条目名干净、内容越界的用户方案；同时在 user_dir 之外放一个受害文件。
fn evil_schema_fixture(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let user = root.join("schemas");
    fs::create_dir_all(&user).unwrap();
    let victim = root.join("victim.txt");
    fs::write(&victim, "SECRET-CONTENT").unwrap();
    fs::write(
        user.join("evil.schema.toml"),
        "[schema]\nid = \"evil\"\nversion = \"1.0\"\n[engine]\ntype = \"codetable\"\n\
             [[dictionaries]]\npath = \"../victim.txt\"\n",
    )
    .unwrap();
    (user, victim)
}

/// 删除方案不得删掉 user_dir 之外的文件。
///
/// 两道守卫各挡一层：`is_safe_rel` 让它进不了 plan，`delete_package` 里规范化后的
/// `starts_with` 是纵深第二道（词法比较对 `..` 恒为 true，不能用）。
#[test]
fn delete_never_escapes_user_dir_via_schema_content() {
    let t = tempfile::tempdir().unwrap();
    let (user, victim) = evil_schema_fixture(t.path());

    let r = delete_package("evil", &user, &[], &[]).unwrap();
    assert!(victim.exists(), "越界文件被删了：{:?}", r.deleted);
    assert_eq!(
        r.deleted,
        vec!["evil.schema.toml"],
        "只该删方案文件本身；越界引用不进删除清单"
    );
}

/// 导出不得把 user_dir 之外的文件打进包——导出的包正是要发给别人的。
#[test]
fn export_never_packs_files_outside_user_dir() {
    let t = tempfile::tempdir().unwrap();
    let (user, _victim) = evil_schema_fixture(t.path());
    let out = t.path().join("evil.wpkg");

    let r = export_package("evil", &user, &[], None, &out, "0.0.0", "windows", "t").unwrap();
    assert_eq!(r.packed, vec!["evil.schema.toml"], "越界文件不得入包");
    // 直接查 zip：`packed` 是我们自己填的，光看它等于自证。
    let a = zip::ZipArchive::new(fs::File::open(&out).unwrap()).unwrap();
    let names: Vec<String> = a.file_names().map(String::from).collect();
    assert!(
        !names.iter().any(|n| n.contains("victim")),
        "zip 里出现了越界文件: {names:?}"
    );
    // 越界引用要能被看见（否则「某个词库莫名其妙不在」无从查起）。
    assert_eq!(r.missing, vec!["../victim.txt"], "越界引用应记进 missing");
}

/// 子方案 id 走的是同一道守卫：`engine.mixed.*_schema` 也是包作者写的字符串。
#[test]
fn mixed_subschema_id_cannot_escape() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    let user = root.join("schemas");
    fs::create_dir_all(&user).unwrap();
    // user_dir 之外放一个「方案文件」，看 mixed 引用能不能把它拽进来
    fs::write(
        root.join("outside.schema.toml"),
        "[schema]\nid = \"outside\"\n[engine]\ntype = \"codetable\"\n",
    )
    .unwrap();
    fs::write(
        user.join("mix.schema.toml"),
        "[schema]\nid = \"mix\"\nversion = \"1.0\"\n[engine]\ntype = \"mixed\"\n\
             [engine.mixed]\nprimary_schema = \"../outside\"\n",
    )
    .unwrap();

    let plan = collect_package_files("mix", &user, &[], None, true).unwrap();
    let packed: Vec<&str> = plan.pack.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(packed, vec!["mix.schema.toml"], "越界子方案不得被收集");
    assert!(
        plan.missing.iter().any(|m| m.contains("outside")),
        "越界子方案应记进 missing: {:?}",
        plan.missing
    );
}

// ───────── 导入规模限额 ─────────

/// 条目数上限。中央目录里塞满空条目就能把逐条校验拖死，而这一步在解压之前。
#[test]
fn import_rejects_too_many_entries() {
    let t = tempfile::tempdir().unwrap();
    let dest = t.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    let pkg = t.path().join("many.zip");
    let mut w = zip::ZipWriter::new(fs::File::create(&pkg).unwrap());
    for i in 0..=MAX_PACKAGE_ENTRIES {
        w.start_file(
            format!("f{i}.txt"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    }
    w.finish().unwrap();
    let err = preview_import(&pkg, &dest).unwrap_err().to_string();
    assert!(err.contains("条目过多"), "{err}");
}

/// 可调上限版：低于包的实际大小即拒绝，且零落盘；默认版（256 MiB）对同一个包照常导入。
#[test]
fn import_with_limit_enforces_lower_cap_and_default_is_unchanged() {
    let t = tempfile::tempdir().unwrap();
    let dest = t.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    let pkg = t.path().join("p.zip");
    let mut w = zip::ZipWriter::new(fs::File::create(&pkg).unwrap());
    let o = zip::write::SimpleFileOptions::default();
    w.start_file("a.schema.toml", o).unwrap();
    w.write_all(b"[schema]\nid = \"a\"\n").unwrap();
    w.start_file("a/big.dict.yaml", o).unwrap();
    w.write_all(&vec![b'x'; 4096]).unwrap();
    w.finish().unwrap();
    let err = import_package_with_limit(&pkg, &dest, crate::merge::Strategy::Merge, 1024)
        .err()
        .expect("低于实际大小的上限必须拒绝")
        .to_string();
    assert!(err.contains("过大"), "{err}");
    assert!(!dest.join("a.schema.toml").exists());
    let ok = import_package(&pkg, &dest, crate::merge::Strategy::Merge).expect("默认上限放行");
    assert_eq!(ok.schema_ids, ["a"]);
}

/// 高压缩比条目（zip 炸弹）超上限即报错。
#[test]
fn limited_read_rejects_oversized_entry() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("bomb.zip");
    let mut w = zip::ZipWriter::new(fs::File::create(&pkg).unwrap());
    w.start_file("big.bin", zip::write::SimpleFileOptions::default())
        .unwrap();
    let chunk = vec![0u8; 1024 * 1024];
    for _ in 0..200 {
        w.write_all(&chunk).unwrap();
    }
    w.finish().unwrap();
    let err = crate::bundle::extract_entry_limited(&pkg, "big.bin", 1024 * 1024)
        .unwrap_err()
        .to_string();
    assert!(err.contains("超过上限"), "{err}");
}

/// 上一条测不出「有没有把超额部分读进内存」：先 `read_to_end` 再判长度同样报错、同样绿。
/// 读了多少在运行时观察不到，计时也区分不开（解压太快），故用源码扫描守卫钉住
/// `Read::take` 还在。判据弱，但比一条永远绿的计时断言强。
#[test]
fn limited_read_bounds_the_read_itself() {
    const SRC: &str = include_str!("bundle.rs");
    let body = SRC
        .split("pub fn extract_entry_limited")
        .nth(1)
        .expect("extract_entry_limited 应存在")
        .split("\n}")
        .next()
        .unwrap();
    assert!(
        body.contains(".take(max + 1)"),
        "extract_entry_limited 必须用 Read::take 把解压停在上限处，\
             而不是读完再判长度——后者一样报错，但内存已经被撑爆了"
    );
}

/// 反向对照：没超限的包照常导入。少了它，把上限写成 0 也能让上面两条全绿。
#[test]
fn normal_sized_package_still_imports() {
    let t = tempfile::tempdir().unwrap();
    let dest = t.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    let pkg = t.path().join("ok.zip");
    write_zip(
        &pkg,
        &[
            ("my.schema.toml", b"[schema]\nid=\"my\"\n".as_slice()),
            ("my/main.dict.yaml", b"a\t\xe5\xb7\xa5\n".as_slice()),
        ],
    );
    let r = import_package(&pkg, &dest, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.imported.len(), 2);
}

/// 反向对照：合法的子目录 rel（`my/main.dict.yaml`）必须照常收集。
/// 少了这条，把守卫写成「一律拒绝」也能让上面三条全绿。
#[test]
fn safe_rels_are_still_collected() {
    let t = tempfile::tempdir().unwrap();
    let (user, system) = (t.path().join("u"), t.path().join("s"));
    fixture(&user, &system);
    let plan =
        collect_package_files("my", &user, std::slice::from_ref(&system), None, false).unwrap();
    let packed: Vec<&str> = plan.pack.iter().map(|(r, _)| r.as_str()).collect();
    assert!(
        packed.contains(&"my/main.dict.yaml") && packed.contains(&"my/chaizi.txt"),
        "合法子目录资源被误拦: {packed:?}"
    );
}
