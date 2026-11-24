//! `sysDict.categories / header / entries`：一级表的形状与先后（主库最先、按 base_order），
//! 停用库也有条数，发现目录登记的库 source 为 dir，开关后再取即反映新状态；二级头部卡片与
//! 词条分页 / 搜索。
use super::*;
use std::sync::Arc;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_store::Store;

/// 夹具：协调器 + 临时数据根；离开作用域（含断言失败）即删临时目录。
struct Fixture {
    c: Arc<Coordinator>,
    base: std::path::PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// 码表 `zm2`：主库 + 停用的分组库 + 文件缺失的库 + `zm2/ext/` 下一张未登记的库；
/// 混输 `mx2` 没有自己的附加库。
fn coord(tag: &str) -> Fixture {
    let base = std::env::temp_dir().join(format!("wind_syscats-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let schemas = base.join("schemas");
    std::fs::create_dir_all(schemas.join("zm2/ext")).unwrap();
    std::fs::write(
        schemas.join("zm2.schema.toml"),
        "[schema]\nid = \"zm2\"\nname = \"码二\"\n[engine]\ntype = \"codetable\"\n\
             [engine.codetable]\nmax_code_length = 4\n\
             [[dictionaries]]\nid = \"main\"\npath = \"zm2/zm2.dict.yaml\"\ndefault = true\n\
             [[dictionaries]]\nid = \"term\"\nlabel = \"术语\"\ndescription = \"专业词\"\n\
             group = \"专业术语\"\npath = \"zm2/term.dict.yaml\"\ndefault_enabled = false\nbase_order = 1\n\
             [[dictionaries]]\nid = \"ghost\"\nlabel = \"缺文件\"\npath = \"zm2/ghost.dict.yaml\"\n\
             default_enabled = true\n",
    )
    .unwrap();
    let head = "---\nname: x\nversion: \"1\"\n...\n";
    std::fs::write(
        schemas.join("zm2/zm2.dict.yaml"),
        "---\nname: x\nversion: \"1\"\nimport_tables:\n  - sub/more\n...\n甲\ta\t9\n乙\tb\t5\n",
    )
    .unwrap();
    std::fs::create_dir_all(schemas.join("zm2/sub")).unwrap();
    std::fs::write(
        schemas.join("zm2/sub/more.dict.yaml"),
        format!("{head}丁\taa\t3\n戊\tab\t2\n己\tc\t1\n"),
    )
    .unwrap();
    std::fs::write(
        schemas.join("zm2/term.dict.yaml"),
        format!("{head}算法\taaaa\t1\n# 注释\n\n编译\tbbbb\t1\n"),
    )
    .unwrap();
    std::fs::write(
        schemas.join("zm2/ext/mine.dict.yaml"),
        "---\nname: 我的词\n...\n丙\tc\t1\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("mx2.schema.toml"),
        "[schema]\nid = \"mx2\"\nname = \"混\"\n[engine]\ntype = \"mixed\"\n\
             [engine.mixed]\nprimary_schema = \"zm2\"\n",
    )
    .unwrap();
    let store = Arc::new(Store::open(base.join("s.redb")).unwrap());
    let mut cfg = Config::default();
    cfg.schema.active = "zm2".into();
    cfg.schema.available = vec!["zm2".into(), "mx2".into()];
    // override 落临时目录：开关测试会写它，不能碰本机配置。
    let c = Coordinator::new_headless_with_store_override(
        cfg,
        Some(&base),
        store,
        Some(base.join("overrides")),
    );
    Fixture { c, base }
}

fn item<'a>(v: &'a Value, id: &str) -> &'a Value {
    v.as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == id)
        .unwrap_or_else(|| panic!("清单里没有 {id}: {v}"))
}

#[test]
fn lists_main_first_with_order_sources_and_counts() {
    let f = coord("shape");
    let c = &f.c;
    let v = c
        .web_data_rpc("sysDict.categories", &json!({"schema": "zm2"}))
        .unwrap();
    let ids: Vec<(&str, u64)> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["id"].as_str().unwrap(), d["order"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        ids,
        [
            ("main", 1),
            ("ghost", 2),
            ("term", 3),
            ("dir:zm2/ext/mine", 4)
        ],
        "主库最先，其余按 base_order（ghost 0 < term 1 < 发现目录），同值按方案块序"
    );
    assert_eq!(
        item(&v, "main"),
        &json!({
            "id": "main", "label": "zm2", "description": "", "group": null,
            "enabled": true, "source": "main", "count": 2, "order": 1, "togglable": false,
        }),
        "主库：没写 label 取文件名，关不掉；码表库的 import_tables 加载器不读、不计"
    );
    assert_eq!(
        item(&v, "term"),
        &json!({
            "id": "term", "label": "术语", "description": "专业词",
            "group": "专业术语", "enabled": false, "source": "bundled", "count": 2,
            "order": 3, "togglable": true,
        }),
        "停用的库也有条数"
    );
    let ghost = item(&v, "ghost");
    assert_eq!(
        (&ghost["enabled"], &ghost["count"]),
        (&json!(true), &Value::Null)
    );
    assert_eq!(ghost["group"], Value::Null, "没写 group 给 null");
    let mine = item(&v, "dir:zm2/ext/mine");
    assert_eq!(
        (
            &mine["source"],
            &mine["label"],
            &mine["count"],
            &mine["group"]
        ),
        (&json!("dir"), &json!("我的词"), &json!(1), &Value::Null)
    );
}

#[test]
fn codetable_imports_do_not_affect_main_count() {
    let f = coord("subgone");
    std::fs::remove_file(f.base.join("schemas/zm2/sub/more.dict.yaml")).unwrap();
    let v =
        f.c.web_data_rpc("sysDict.categories", &json!({"schema": "zm2"}))
            .unwrap();
    assert_eq!(item(&v, "main")["count"], 2, "子表在不在都只算本文件");
}

#[test]
fn header_card_and_errors() {
    let f = coord("header");
    let c = &f.c;
    let h = c
        .web_data_rpc(
            "sysDict.header",
            &json!({"schema": "zm2", "dictId": "main"}),
        )
        .unwrap();
    assert_eq!(
        (
            &h["id"],
            &h["label"],
            &h["source"],
            &h["name"],
            &h["version"]
        ),
        (
            &json!("main"),
            &json!("zm2"),
            &json!("main"),
            &json!("x"),
            &json!("1")
        )
    );
    assert_eq!(h["importTables"], json!(["sub/more"]), "声明原样列");
    assert_eq!(h["count"], 2);
    assert_eq!(h["tables"], json!([]), "码表库不加载子表");
    assert!(h["path"].as_str().unwrap().ends_with("zm2.dict.yaml"));
    assert!(h["mtimeMs"].as_u64().is_some());
    let t = c
        .web_data_rpc(
            "sysDict.header",
            &json!({"schema": "zm2", "dictId": "term"}),
        )
        .unwrap();
    assert_eq!(
        (&t["source"], &t["tables"]),
        (&json!("bundled"), &json!([]))
    );
    // 未知 id / 文件缺失 / 缺参
    for p in [
        json!({"schema": "zm2", "dictId": "nope"}),
        json!({"schema": "zm2", "dictId": "ghost"}),
        json!({"schema": "nope", "dictId": "main"}),
        json!({"schema": "zm2"}),
    ] {
        assert!(c.web_data_rpc("sysDict.header", &p).is_err(), "{p}");
    }
}

#[test]
fn entries_pages_and_searches() {
    let f = coord("entries");
    let c = &f.c;
    let get = |p: Value| c.web_data_rpc("sysDict.entries", &p).unwrap();
    let v = get(json!({"schema": "zm2", "dictId": "main", "offset": 1, "limit": 2}));
    assert_eq!(v["total"], 2);
    assert_eq!(
        v["rows"],
        json!([{"code": "b", "text": "乙", "weight": 5, "table": "zm2"}]),
        "码表库不混入 import_tables 子表"
    );
    assert_eq!(v["note"], Value::Null);
    let v = get(json!({"schema": "zm2", "dictId": "main", "query": "a", "mode": "code"}));
    let texts: Vec<&str> = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap())
        .collect();
    assert_eq!((v["total"].as_u64(), texts), (Some(1), vec!["甲"]));
    let v = get(json!({"schema": "zm2", "dictId": "term", "query": "编", "mode": "text"}));
    assert_eq!(v["total"], 1);
    let v = get(json!({"schema": "zm2", "dictId": "term", "offset": 9}));
    assert_eq!(
        (&v["total"], &v["rows"]),
        (&json!(2), &json!([])),
        "越界给空页"
    );
    // 不给 mode：编码前缀或词子串
    let v = get(json!({"schema": "zm2", "dictId": "term", "query": "b"}));
    assert_eq!(v["rows"][0]["text"], "编译");
    let v = get(json!({"schema": "zm2", "dictId": "term", "query": "算"}));
    assert_eq!(v["rows"][0]["text"], "算法");
    // sort / order：排序后分页
    let texts_of = |v: &Value| -> Vec<String> {
        v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["text"].as_str().unwrap().to_string())
            .collect()
    };
    let v = get(json!({"schema": "zm2", "dictId": "main", "sort": "weight"}));
    assert_eq!(texts_of(&v), ["乙", "甲"]);
    let v = get(json!({
        "schema": "zm2", "dictId": "main", "sort": "weight", "order": "desc",
        "offset": 1, "limit": 1
    }));
    assert_eq!(
        (v["total"].as_u64(), texts_of(&v)),
        (Some(2), vec!["乙".into()])
    );
    let v = get(json!({"schema": "zm2", "dictId": "term", "sort": "code", "order": "desc"}));
    assert_eq!(texts_of(&v), ["编译", "算法"]);
    for p in [
        json!({"schema": "zm2", "dictId": "nope"}),
        json!({"schema": "zm2", "dictId": "main", "mode": "pinyin"}),
        json!({"schema": "zm2", "dictId": "main", "sort": "count"}),
        json!({"schema": "zm2", "dictId": "main", "sort": "code", "order": "up"}),
        json!({"schema": "zm2", "dictId": "ghost"}),
    ] {
        assert!(c.web_data_rpc("sysDict.entries", &p).is_err(), "{p}");
    }
}

#[test]
fn reflects_toggle_and_rejects_bad_input() {
    let f = coord("toggle");
    let c = &f.c;
    c.web_data_rpc(
        "schema.setDictEnabled",
        &json!({"id": "zm2", "dictId": "term", "enabled": true}),
    )
    .unwrap();
    let v = c
        .web_data_rpc("sysDict.categories", &json!({"schema": "zm2"}))
        .unwrap();
    assert_eq!(item(&v, "term")["enabled"], true);
    let mx = c
        .web_data_rpc("sysDict.categories", &json!({"schema": "mx2"}))
        .unwrap();
    assert_eq!(mx, json!([]), "混输没有自己的附加库");
    assert!(
        c.web_data_rpc("sysDict.categories", &json!({"schema": "nope"}))
            .is_err()
    );
    assert!(c.web_data_rpc("sysDict.categories", &json!({})).is_err());
}
