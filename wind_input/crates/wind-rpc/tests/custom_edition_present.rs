//! 定制版身份的对外暴露 —— **清单在场**的一半：启动摘要有内容、`system.info` 带出四个字段。
//!
//! 「不在场」那一半在 `custom_edition_absent.rs`。
//!
//! # 为什么必须是集成测试，且两态各占一个测试二进制
//!
//! 判据 `Config::custom_manifest()` 用 OnceLock 缓存：同一进程里只解析一次盘上状态。
//! 把两态写进同一个二进制的话，先跑的那个把层定死，后跑的静默测到错误的目标——而两个
//! 断言都还是绿的（「有清单」测到 None 时会红，但「无清单」测到 Some 时**恰好**也能写成绿）。
//! 重定向靠 `WIND_INSTALL_ROOT`（安装根）与 `WIND_DATADIR_CONF`（用户目录）。
//!
//! ⚠️ 两个环境变量都必须设：漏掉 `WIND_DATADIR_CONF` 时 `Config::load()` 会去读用户真实的
//! `%APPDATA%\WindInput\config.toml`（本仓已有前科）。

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

fn call(state: &DispatchState, method: &str) -> Value {
    let r = dispatch(
        state,
        Request {
            version: 1,
            id: 1,
            method: method.to_string(),
            params: json!({}),
        },
    );
    assert!(r.error.is_none(), "{method} 不该失败: {:?}", r.error);
    r.result.expect("应有 result")
}

#[test]
fn custom_edition_identity_is_visible_in_log_summary_and_system_info() {
    // ⚠️ 临时目录名带 pid：并发跑测试（或上一轮残留）时共用一个目录会互相删对方的夹具。
    let tmp = std::env::temp_dir().join(format!(
        "wind_custom_edition_present_{}",
        std::process::id()
    ));
    let root = tmp.join("install");
    let data = root.join("data");
    let custom = root.join("data_custom");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();
    write_at(&data, "config.toml", "[schema]\nactive = \"wubi86\"\n");

    // 清单必须在**任何** OnceLock 初始化之前就位，否则本次进程里定制层恒为关闭。
    write_at(
        &custom,
        "custom.toml",
        "[custom]\n\
         id = \"huma-edition\"\n\
         name = \"虎码定制版\"\n\
         version = \"1.2\"\n\
         base_version = \"0.9.30\"\n",
    );

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
        wind_config::Config::custom_manifest().is_some(),
        "前置条件：本用例须真的被判为定制版，否则下面全是「不在场」那一半的断言"
    );

    // ── 启动摘要（service 启动时打的那一行）────────────────────────────────────

    let summary = wind_rpc::custom_edition::startup_summary()
        .expect("★ 清单在场时必须有摘要——没有它，报障日志里看不出这是不是定制版");
    // 逐项断言而不是整串比对：id / 显示名 / 版本 / 基线版本少任何一项，报障时就少一条线索。
    for expect in ["huma-edition", "虎码定制版", "1.2", "0.9.30"] {
        assert!(
            summary.contains(expect),
            "启动摘要须含 {expect}，实得: {summary}"
        );
    }

    // ── system.info 的 customEdition ──────────────────────────────────────────

    let state = DispatchState::new(Arc::new(StubCore), "dev").expect("DispatchState");
    let info = call(&state, "system.info");
    assert_eq!(
        info["customEdition"],
        json!({
            "id": "huma-edition",
            "name": "虎码定制版",
            "version": "1.2",
            "baseVersion": "0.9.30",
        }),
        "★ 关于页据此显示定制版身份；字段名是跨仓契约，改名等于设置页那侧静默显示空白\n{info}"
    );
    // 与日志同源：两处分叉（日志说 1.2、关于页说 1.3）是最难查的一类不一致。
    assert!(
        summary.contains(info["customEdition"]["version"].as_str().unwrap()),
        "日志摘要与 RPC 字段的版本必须来自同一份清单"
    );

    // 原有字段不受牵连——加字段时最容易顺手写坏的就是它们。
    assert_eq!(info["running"], json!(true));
    assert_eq!(info["variant"], json!("dev"));
    assert!(info["version"].is_string());

    let _ = std::fs::remove_dir_all(&tmp);
}
