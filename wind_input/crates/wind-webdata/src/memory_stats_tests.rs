//! `system.memoryStats`（只读诊断）的回包形态。设置端「高级 → 性能 → 内存占用」卡片与
//! 「复制诊断信息」按它取数：字段恒在（缺数给 `null` / 空数组），客户端可无条件渲染。
use super::*;
use std::sync::Arc;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_store::Store;

/// 一个自造码表方案 `zm1` 的数据目录 + store（不依赖 build_dev，不会静默跳过）。
/// `tag` 让并行的用例各用各的夹具目录——redb 是独占锁，两条用例同时开同一个 `s.redb`
/// 必有一条失败。
fn coord(tag: &str) -> Arc<Coordinator> {
    let base = std::env::temp_dir().join(format!("wind_memstats-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let schemas = base.join("schemas");
    std::fs::create_dir_all(schemas.join("zm1")).unwrap();
    std::fs::write(
        schemas.join("zm1.schema.toml"),
        "[schema]\nid = \"zm1\"\nname = \"zm1\"\n[engine]\ntype = \"codetable\"\n\
             [engine.codetable]\nmax_code_length = 4\n\
             [[dictionaries]]\nid = \"main\"\npath = \"zm1/zm1.dict.yaml\"\ndefault = true\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("zm1/zm1.dict.yaml"),
        "---\nname: zm1\nversion: \"1\"\n...\n甲\ta\n",
    )
    .unwrap();
    let store = Arc::new(Store::open(base.join("s.redb")).unwrap());
    let mut cfg = Config::default();
    cfg.schema.active = "zm1".into();
    cfg.schema.available = vec!["zm1".into()];
    Coordinator::new_headless_with_store(cfg, Some(&base), store)
}

/// storageStatus 报 meta.toml 里的实际格式版本，并带上写失败原因（没有时为 null）。
#[test]
fn storage_status_reports_actual_format_version_and_write_error() {
    let c = coord("storage_status_actual");
    let v = c.web_data_rpc("system.storageStatus", &json!({})).unwrap();
    assert_eq!(
        v["format_version"],
        wind_config::user_meta::KNOWN_FORMAT_VERSION
    );
    assert!(v["write_error"].is_null());
    let dir = c
        .user_store()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(
        dir.join("meta.toml"),
        "format_version = 3\ndir_id = \"x\"\n",
    )
    .unwrap();
    let v = c.web_data_rpc("system.storageStatus", &json!({})).unwrap();
    assert_eq!(v["format_version"], 3, "报文件里的实际值");
    assert_eq!(
        v["known_format_version"],
        wind_config::user_meta::KNOWN_FORMAT_VERSION
    );
}

/// §2.5：数据文件的问题（坏行 / 隔离 / 已重建 / 未迁移）经 storageStatus 交给设置页。
#[test]
fn storage_status_lists_data_file_issues() {
    let c = coord("storage_status_issues");
    let root = c.user_store().unwrap().user_text_dir().to_path_buf();
    let f = root.join("pinyin").join("freq.table.yaml");
    let g = root.join("stats").join("2026-10.toml");
    wind_store::issues::report(&f, wind_store::issues::IssueKind::BadLines(3));
    wind_store::issues::report(
        &g,
        wind_store::issues::IssueKind::Rebuilt {
            kept_as: "2026-10.toml.corrupt-x".into(),
        },
    );
    let v = c.web_data_rpc("system.storageStatus", &json!({})).unwrap();
    let issues = v["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 2, "{v}");
    assert_eq!(issues[0]["path"], "pinyin/freq.table.yaml");
    assert_eq!(issues[0]["kind"], "bad_lines");
    assert_eq!(issues[0]["lines"], 3);
    assert_eq!(issues[1]["path"], "stats/2026-10.toml");
    assert_eq!(issues[1]["kind"], "rebuilt");
    assert_eq!(issues[1]["kept_as"], "2026-10.toml.corrupt-x");
    wind_store::issues::clear(&f);
    wind_store::issues::clear(&g);
}

/// 用户数据设计 §7.1：`system.storageStatus` 报只读标志、原因与格式版本。正常构造的
/// 协调器不是只读；只读路径（meta.toml 版本更高）由 wind-coordinator 的
/// `tests/storage_readonly.rs` 验（trait 在那边不可调，这里验的是 RPC 的形状）。
#[test]
fn storage_status_reports_readonly_flag_and_versions() {
    let c = coord("storage_status_reports_r");
    let v = c.web_data_rpc("system.storageStatus", &json!({})).unwrap();
    assert_eq!(v["readonly"], false);
    assert!(v["reason"].is_null());
    assert_eq!(
        v["format_version"],
        wind_config::user_meta::KNOWN_FORMAT_VERSION
    );
    assert_eq!(
        v["known_format_version"],
        wind_config::user_meta::KNOWN_FORMAT_VERSION
    );
}

#[test]
fn memory_stats_reports_process_schemas_and_structures() {
    let c = coord("memory_stats_reports_pro");
    let v = c.web_data_rpc("system.memoryStats", &json!({})).unwrap();

    let p = &v["process"];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        assert_eq!(p["kind"], "rss_anon");
        assert!(p["bytes"].as_u64().unwrap() > 0);
    }
    #[cfg(windows)]
    {
        assert_eq!(p["kind"], "private");
        assert!(p["bytes"].as_u64().unwrap() > 0);
    }
    assert!(p.get("bytes").is_some() && p.get("kind").is_some());

    let schemas = v["schemas"].as_array().expect("schemas 是数组");
    assert_eq!(schemas.len(), 1, "只有当前方案已加载：{v}");
    assert_eq!(schemas[0]["id"], "zm1");
    assert_eq!(schemas[0]["type"], "CodeTable");
    assert!(schemas[0]["parts"].is_array());

    assert!(v["reverseIndex"].is_array());
    assert!(v["userText"].is_array());
    assert!(v.get("userAssoc").is_some());
    assert!(v.get("singleChar").is_some());
    for k in ["chaiziBytes", "pinyinBytes", "commentBytes"] {
        assert!(v["reverseLookup"][k].is_u64(), "reverseLookup.{k}：{v}");
    }
    assert!(
        v["redb"].is_null(),
        "全新用户目录不打开旧库，不该报它的缓存上限：{v}"
    );
    assert!(
        v["dictCache"]["policy"].is_string(),
        "dictCache.policy：{v}"
    );
    for k in [
        "lockedBytes",
        "lockFailedBytes",
        "preloadedBytes",
        "overBudgetBytes",
    ] {
        assert!(v["dictCache"][k].is_u64(), "dictCache.{k}：{v}");
    }
}
