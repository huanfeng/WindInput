//! `dict.systemStats` 的回包形态：码表方案出各库分行 / 码长 / 重码，混输按成员并排。
use super::*;
use std::sync::Arc;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_store::Store;

/// 码表 `zm1`（主库 + 停用扩展）+ 混输 `mx`（主码表 zm1，无拼音成员）。
fn coord(tag: &str) -> Arc<Coordinator> {
    let base = std::env::temp_dir().join(format!("wind_sysstats-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let schemas = base.join("schemas");
    std::fs::create_dir_all(schemas.join("zm1")).unwrap();
    std::fs::write(
        schemas.join("zm1.schema.toml"),
        "[schema]\nid = \"zm1\"\nname = \"码一\"\n[engine]\ntype = \"codetable\"\n\
             [engine.codetable]\nmax_code_length = 4\n\
             [[dictionaries]]\nid = \"main\"\npath = \"zm1/zm1.dict.yaml\"\ndefault = true\n\
             [[dictionaries]]\nid = \"ext\"\npath = \"zm1/ext.dict.yaml\"\ndefault_enabled = false\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("zm1/zm1.dict.yaml"),
        "---\nname: zm1\nversion: \"1\"\n...\n甲\ta\t9\n乙\ta\t5\n甲乙\taaaa\t1\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("mx.schema.toml"),
        "[schema]\nid = \"mx\"\nname = \"混\"\n[engine]\ntype = \"mixed\"\n\
             [engine.mixed]\nprimary_schema = \"zm1\"\n",
    )
    .unwrap();
    let store = Arc::new(Store::open(base.join("s.redb")).unwrap());
    let mut cfg = Config::default();
    cfg.schema.active = "zm1".into();
    cfg.schema.available = vec!["zm1".into(), "mx".into()];
    Coordinator::new_headless_with_store(cfg, Some(&base), store)
}

#[test]
fn codetable_shape_and_cache_flag() {
    let c = coord("ct");
    let v = c
        .web_data_rpc("dict.systemStats", &json!({"schemaId": "zm1"}))
        .unwrap();
    assert_eq!(v["engineType"], "codetable");
    assert_eq!(v["schemaName"], "码一");
    assert_eq!(v["cached"], false);
    assert_eq!(v["layers"][0]["kind"], "main");
    assert_eq!(v["layers"][0]["entries"], 3);
    assert_eq!(v["layers"][1]["enabled"], false);
    assert!(v["layers"][1].get("entries").is_none(), "停用库只列不计");
    assert_eq!(v["total"]["byLen"]["2"], 1);
    assert_eq!(v["dup"]["groups"], 1);
    assert_eq!(v["dup"]["top"][0]["texts"], json!(["甲", "乙"]));
    assert_eq!(v["codeLen"]["char"]["1"], 2);
    assert_eq!(v["user"], json!({"userWords": 0, "tempWords": 0}));
    let v = c
        .web_data_rpc("dict.systemStats", &json!({"schemaId": "zm1"}))
        .unwrap();
    assert_eq!(v["cached"], true);
    let v = c
        .web_data_rpc(
            "dict.systemStats",
            &json!({"schemaId": "zm1", "refresh": true}),
        )
        .unwrap();
    assert_eq!(v["cached"], false);
}

#[test]
fn mixed_lists_members_and_unknown_schema_errors() {
    let c = coord("mx");
    let v = c
        .web_data_rpc("dict.systemStats", &json!({"schemaId": "mx"}))
        .unwrap();
    assert_eq!(v["engineType"], "mixed");
    let members = v["members"].as_array().unwrap();
    assert_eq!(members.len(), 1, "没配拼音成员就只有主码表");
    assert_eq!(members[0]["schemaId"], "zm1");
    assert!(
        c.web_data_rpc("dict.systemStats", &json!({"schemaId": "nope"}))
            .is_err()
    );
    assert!(c.web_data_rpc("dict.systemStats", &json!({})).is_err());
}
