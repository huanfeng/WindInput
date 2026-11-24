use super::*;

fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 以 `root` 下的 redb 与 override 目录拼 [`MobileCore`]；同一个 `root` 再调 = 重启。
/// 不走 `MobileCore::new`：它改进程环境变量，与并行测试互相踩。
fn core_at(root: &Path) -> MobileCore {
    let store = Arc::new(wind_store::Store::open(root.join("s.redb")).unwrap());
    let coord = Coordinator::new_headless_with_store_override(
        Config::default(),
        Some(&data_dir()),
        store,
        Some(root.join("overrides")),
    );
    MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: data_dir(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    }
}

#[test]
fn list_toggle_persist_and_errors() {
    if !data_dir().join("schemas/pinyin.schema.toml").exists() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let root = std::env::temp_dir().join(format!("wind_mobile_sd_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let idiom = |m: &MobileCore| {
        m.schema_dicts("pinyin")
            .into_iter()
            .find(|d| d.id == "cell_idiom")
    };

    let m = core_at(&root);
    assert_eq!(
        idiom(&m),
        Some(SchemaDictInfo {
            id: "cell_idiom".into(),
            label: "成语".into(),
            description: "成语、俗语与古诗文名句".into(),
            enabled: false,
        })
    );
    assert!(
        m.schema_dicts("pinyin")
            .iter()
            .all(|d| d.id != "pinyin_main"),
        "不列主库"
    );
    m.set_schema_dict_enabled("pinyin", "cell_idiom", true)
        .unwrap();
    assert!(idiom(&m).unwrap().enabled);
    drop(m);
    let m = core_at(&root);
    assert!(idiom(&m).unwrap().enabled, "重建 MobileCore 后仍开");

    let err = m
        .set_schema_dict_enabled("pinyin", "no_such_dict", true)
        .unwrap_err();
    assert!(err.contains("no_such_dict"), "原因要点名: {err}");
    assert!(
        m.set_schema_dict_enabled("nope", "cell_idiom", true)
            .is_err()
    );
    assert!(m.schema_dicts("nope").is_empty(), "未知方案列空、不 panic");
    let _ = std::fs::remove_dir_all(&root);
}

/// 词库统计门面：回包是桌面 `dict.systemStats` 同形态的 JSON，第二次命中缓存；
/// 未知方案带原因报错。统计口径在 wind-engine / wind-webdata 的用例里验。
#[test]
fn dict_stats_returns_json_and_caches() {
    if !data_dir().join("schemas/wubi86.schema.toml").exists() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let root = std::env::temp_dir().join(format!("wind_mobile_ds_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let m = core_at(&root);
    let first = m.dict_stats("wubi86", 5, false).unwrap();
    assert!(first.contains(r#""schemaId":"wubi86""#), "{first}");
    assert!(first.contains(r#""engineType":"codetable""#), "{first}");
    assert!(first.contains(r#""total":{"#), "{first}");
    let again = m.dict_stats("wubi86", 5, false).unwrap();
    assert!(again.contains(r#""cached":true"#), "第二次走缓存: {again}");
    let fresh = m.dict_stats("wubi86", 5, true).unwrap();
    assert!(fresh.contains(r#""cached":false"#), "refresh 绕过缓存");
    let err = m.dict_stats("no_such_schema", 5, false).unwrap_err();
    assert!(err.contains("no_such_schema"), "原因要点名: {err}");
    let _ = std::fs::remove_dir_all(&root);
}

/// 联想学习数据：计数跨方案合计，清空后归零、返回删除条数。
#[test]
fn assoc_history_count_and_clear() {
    if !data_dir().join("schemas/wubi86.schema.toml").exists() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    use wind_coordinator::web_host::WebDataHost;
    let root = std::env::temp_dir().join(format!("wind_mobile_ah_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let m = core_at(&root);
    assert_eq!(m.assoc_history_count(), 0);
    let store = m.coord.user_store().expect("有持久化存储");
    // 一个启用的方案、一个没启用的（停用了的方案下学到的也要清得掉）
    store.record_assoc_pick("wubi86", "我们", "一起").unwrap();
    store.record_assoc_pick("wubi86", "我们", "一起").unwrap();
    store.record_assoc_pick("pinyin", "谢谢", "大家").unwrap();
    assert_eq!(m.assoc_history_count(), 2, "按条计、跨方案合计");
    assert_eq!(m.assoc_history_clear().unwrap(), 2);
    assert_eq!(m.assoc_history_count(), 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// 只读模式：带着原因拒绝，且先于方案校验（不论方案在不在）。
#[test]
fn readonly_rejects_with_reason() {
    let user = std::env::temp_dir().join(format!("wind_mobile_sd_ro_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(
        user.join("meta.toml"),
        "format_version = 99\ndir_id = \"x\"\n",
    )
    .unwrap();
    let (coord, _rx) =
        Coordinator::new_session(Config::default(), None, Some(&user), InputProfile::mobile());
    let m = MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: user.clone(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    };
    let err = m
        .set_schema_dict_enabled("pinyin", "cell_idiom", true)
        .unwrap_err();
    assert!(err.contains("99"), "要把只读原因交给用户: {err}");
    let _ = std::fs::remove_dir_all(&user);
}
