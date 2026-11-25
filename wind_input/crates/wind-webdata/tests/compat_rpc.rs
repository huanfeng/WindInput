//! `compat.*` 路由：必须走到 `wind_config::compat_admin`，而不是落进 `unknown method`。
//!
//! 只测「路由通了 + 坏请求是 Err」。规则的合并 / 写时复制 / 导入导出语义在
//! `wind-config` 的 `compat_admin` 单测里，写入后的重载在 `wind-coordinator` 的
//! `compat_reload_tests` 里——这里刻意不碰真实用户目录的写入路径。
use serde_json::json;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_webdata::WebDataRpc;

/// `compat.schema` 是纯函数（不读目录），可以放心在测试里直接调。
#[test]
fn compat_schema_is_routed_and_lists_fields() {
    let c = Coordinator::new_headless(Config::default(), None);
    let v = c.web_data_rpc("compat.schema", &json!({})).unwrap();
    let fields = v["fields"].as_array().expect("fields 应是数组");
    assert!(
        fields.iter().any(|f| f["key"] == "stale_probe_guard"),
        "元数据应含 stale_probe_guard"
    );
    assert!(v["groups"].is_array());
}

/// 坏请求只会得到 Err，不会 panic，也不会被当成别的命名空间的方法吞掉。
#[test]
fn compat_bad_requests_come_back_as_errors() {
    let c = Coordinator::new_headless(Config::default(), None);
    assert!(
        c.web_data_rpc("compat.list", &json!({"section": "x"}))
            .is_err()
    );
    assert!(c.web_data_rpc("compat.bogus", &json!({})).is_err());
}

/// 外部改了文件后的重载入口都在分派表里：`compat.reload`（不能被 `compat.*` 通配吞成未知方法）、
/// `schema.reloadFiles`、`theme.reloadFiles`。规则表重载的实际效果在 coordinator 的
/// `reload_compat_and_refresh_*` 测试里验。
#[test]
fn reload_entries_are_routed() {
    let c = Coordinator::new_headless(Config::default(), None);
    assert_eq!(
        c.web_data_rpc("compat.reload", &json!({})).unwrap(),
        json!({"ok": true})
    );
    let r = c
        .web_data_rpc("schema.reloadFiles", &json!({"ids": ["nope"]}))
        .unwrap();
    assert_eq!(r["reloaded"], json!(["nope"]));
    let r = c.web_data_rpc("schema.reloadFiles", &json!({})).unwrap();
    assert_eq!(r["ok"], true);
    let r = c
        .web_data_rpc("theme.reloadFiles", &json!({"ids": ["not-current"]}))
        .unwrap();
    assert_eq!(r["reloaded"], json!(["not-current"]));
    assert_eq!(r["applied"], false, "非当前主题不重新下发");
    assert_eq!(
        c.web_data_rpc("softkeyboard.reload", &json!({})).unwrap(),
        json!({"ok": true})
    );
}

#[test]
fn reload_entries_reject_bad_ids() {
    let c = Coordinator::new_headless(Config::default(), None);
    for m in ["schema.reloadFiles", "theme.reloadFiles"] {
        assert!(c.web_data_rpc(m, &json!({"ids": "x"})).is_err(), "{m}");
        assert!(c.web_data_rpc(m, &json!({"ids": [1]})).is_err(), "{m}");
    }
    assert!(
        c.web_data_rpc("theme.reloadFiles", &json!({"ids": ["../x"]}))
            .is_err()
    );
}
