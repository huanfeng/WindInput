//! 定制版身份的对外暴露 —— **清单不在场**的一半（绝大多数装机走这一条）。
//!
//! 钉两件事：
//!
//! 1. 启动摘要**一行都不打**。非定制版每次启动都打一行「本机不是定制版」就是纯噪音，
//!    而日志是报障时唯一的线索来源，噪音会把真正的线索挤出滚动窗口。
//! 2. `system.info` 的 `customEdition` 是**显式 `null`，字段仍在**。跨仓契约无编译期
//!    约束：「字段不存在」与「这版 core 还没实现这个字段」在设置端看来完全一样，于是
//!    关于页只能靠猜；显式 `null` 才明确表示「问过了，本机不是定制版」。
//!
//! 独立成一个测试二进制的理由见 `custom_edition_present.rs`（`custom_manifest()` 是 OnceLock）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use wind_ipc::rpc::Request;
use wind_rpc::{CoreRpc, DispatchState, dispatch};

struct StubCore;

impl CoreRpc for StubCore {
    fn is_chinese_mode(&self) -> bool {
        true
    }
    fn active_schema_id(&self) -> String {
        "wubi86".into()
    }
}

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn without_manifest_summary_is_silent_and_rpc_field_is_explicit_null() {
    let tmp =
        std::env::temp_dir().join(format!("wind_custom_edition_absent_{}", std::process::id()));
    let root = tmp.join("install");
    let data = root.join("data");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();
    write_at(&data, "config.toml", "[schema]\nactive = \"wubi86\"\n");
    // 刻意**不**建 data_custom/custom.toml。

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        wind_config::Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录须已重定向，否则本测试会读写真实 %APPDATA%"
    );
    assert!(
        wind_config::Config::custom_manifest().is_none(),
        "前置条件：本用例须判为非定制版；判成定制版的话下面两条会以相反的理由变绿"
    );

    assert!(
        wind_rpc::custom_edition::startup_summary().is_none(),
        "★ 非定制版不该打摘要行"
    );
    assert_eq!(
        wind_rpc::custom_edition::identity_json(),
        Value::Null,
        "非定制版的身份是显式 null"
    );

    let state = DispatchState::new(Arc::new(StubCore), "dev").expect("DispatchState");
    let r = dispatch(
        &state,
        Request {
            version: 1,
            id: 1,
            method: "system.info".into(),
            params: json!({}),
        },
    );
    let info = r.result.expect("system.info 应成功");
    let obj = info.as_object().expect("system.info 应为对象");
    assert!(
        obj.contains_key("customEdition"),
        "★ 字段必须在（值为 null），不是「没有就不给」\n{info}"
    );
    assert_eq!(info["customEdition"], Value::Null, "{info}");

    let _ = std::fs::remove_dir_all(&tmp);
}
