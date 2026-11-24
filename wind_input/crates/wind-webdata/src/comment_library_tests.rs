//! `comment.library`：模板表带来源，总表每格给出实际生效的模板与所在层。
use super::*;
use std::sync::Arc;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_store::Store;

fn coord(tag: &str) -> Arc<Coordinator> {
    let base = std::env::temp_dir().join(format!("wind_cmtlib-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("schemas")).unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../data/system.comments.toml"
        ),
        base.join("system.comments.toml"),
    )
    .unwrap();
    let store = Arc::new(Store::open(base.join("s.redb")).unwrap());
    Coordinator::new_headless_with_store(Config::default(), Some(&base), store)
}

fn cell<'a>(v: &'a Value, scope: &str, state: &str, vertical: bool) -> &'a Value {
    v["effective"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["scope"] == scope && e["state"] == state && e["vertical"] == vertical)
        .unwrap_or_else(|| panic!("effective 缺 {scope}/{state}/{vertical}: {v}"))
}

#[test]
fn builtin_templates_and_global_effective() {
    let v = coord("base")
        .web_data_rpc("comment.library", &json!({}))
        .unwrap();
    let code = v["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "code")
        .expect("templates 应含 code");
    assert_eq!(code["origin"], "builtin");
    assert_eq!(code["name"], "编码（自动）");
    assert!(code["desc"].as_str().is_some_and(|d| !d.is_empty()));
    let g = cell(&v, "global", "default", true);
    assert_eq!(g["template_id"], "code");
    assert_eq!(g["from_scope"], "global");
    assert_eq!(
        v["system_assign"]["type"]["codetable"]["detail_horizontal"],
        "chaizi_pinyin"
    );
    for vertical in [false, true] {
        let e = cell(&v, "mode:temp_english", "default", vertical);
        assert_eq!(e["template_id"], "none");
        assert_eq!(e["from_scope"], "mode:temp_english");
    }
    let t = cell(&v, "type:codetable", "default", false);
    assert_eq!(t["template_id"], "code");
    assert_eq!(t["from_scope"], "global");
}

#[test]
fn draft_comment_param_drives_rows_and_origins() {
    let v = coord("draft")
        .web_data_rpc(
            "comment.library",
            &json!({"comment": {
                "templates": [
                    {"id": "code", "name": "改", "text": "X"},
                    {"id": "mine", "name": "我的", "text": "Y"}
                ],
                "assign": {"schema": {"wubi86": {"detail_horizontal": "mine"}}}
            }}),
        )
        .unwrap();
    let origin = |id: &str| {
        v["templates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .map(|t| t["origin"].clone())
    };
    assert_eq!(origin("code"), Some(json!("user_overrides_builtin")));
    let ch = v["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "code")
        .unwrap();
    assert_eq!(ch["builtin"]["name"], "编码（自动）");
    assert_eq!(origin("mine"), Some(json!("user")));
    for vertical in [false, true] {
        let s = cell(&v, "schema:wubi86", "detail", vertical);
        assert_eq!(s["template_id"], "mine", "竖排没写同本行横排");
        assert_eq!(s["from_scope"], "schema:wubi86");
    }
    let s = cell(&v, "schema:wubi86", "default", true);
    assert_eq!(s["from_scope"], "global");
}
