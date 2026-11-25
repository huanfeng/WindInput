use super::*;
use serde_json::json;

fn t(s: i64) -> String {
    format!("T{s}")
}

#[test]
fn now_requested() {
    assert_eq!(
        format_now(&json!({"outcome":"requested"})),
        ("已请求同步，完成后会提示".to_string(), 0)
    );
}

#[test]
fn now_disabled() {
    assert_eq!(
        format_now(&json!({"outcome":"disabled"})),
        ("数据同步未启用".to_string(), 2)
    );
}

#[test]
fn now_failed_with_and_without_error() {
    assert_eq!(
        format_now(&json!({"outcome":"failed","error":"断网"})),
        ("同步失败：断网".to_string(), 1)
    );
    assert_eq!(format_now(&json!({"outcome":"failed"})).1, 1);
}

#[test]
fn now_unknown_outcome() {
    assert_eq!(format_now(&json!({"outcome":"x"})).1, 1);
    assert_eq!(format_now(&json!({})).1, 1);
}

#[test]
fn status_full() {
    let v = json!({"phase":"idle","device":"d1","group":"g9","group_name":"家","last_ok":100,"last_error":"炸了"});
    assert_eq!(
        format_status(&v, t),
        "状态: 空闲\n设备: d1\n组: 家 (g9)\n上次成功: T100\n上次错误: 炸了"
    );
}

#[test]
fn status_no_group_name_no_last_ok_no_error() {
    let v = json!({"phase":"syncing","device":"d1","group":"g9","group_name":null,"last_ok":null,"last_error":null});
    assert_eq!(
        format_status(&v, t),
        "状态: 同步中\n设备: d1\n组: g9\n上次成功: —"
    );
}

#[test]
fn status_no_group_at_all() {
    let v = json!({"phase":"disabled"});
    assert_eq!(
        format_status(&v, t),
        "状态: 未启用\n设备: —\n组: —\n上次成功: —"
    );
}

#[test]
fn status_phase_labels() {
    for (p, zh) in [
        ("idle", "空闲"),
        ("syncing", "同步中"),
        ("error", "出错"),
        ("disabled", "未启用"),
        ("not_installed", "未安装"),
        ("starting", "启动中"),
        ("weird", "weird"),
    ] {
        assert!(format_status(&json!({"phase":p}), t).starts_with(&format!("状态: {zh}\n")));
    }
}

#[test]
fn run_unknown_or_empty_returns_2() {
    assert_eq!(run(&[]), 2);
    assert_eq!(run(&["bogus".to_string()]), 2);
}
