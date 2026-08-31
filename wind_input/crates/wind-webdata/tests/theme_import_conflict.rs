//! 同名主题导入：冲突走 **result 字段**（`conflict: true`）而非 error 通道，`force=true` 覆盖。
//!
//! 为什么这一条值一个用例：设置端要靠「认出这是冲突」才能弹「是否覆盖」并以 `force=true`
//! 重推。RPC 的 error 通道只有一个 `String`（`wind_ipc::rpc::Response::error`），客户端
//! 只能靠匹配文案来认——改一次报错文案，覆盖确认就静默退化成一条普通报错，没有任何编译期
//! 检查或既有测试看得见。本用例把 `conflict` 字段钉成契约。
//!
//! 为什么必须是集成测试（独立进程）：`Config::user_config_dir()` 走 `WIND_DATADIR_CONF`
//! 且内部 OnceLock 缓存，env 必须在任何初始化之前就位。
//! ⚠️ 全文件仅此一个 `#[test]`，理由同 `custom_layer_hide_theme_import.rs`。
//! ⚠️ 不依赖 `build_dev/data`：主题夹具全自造，故不会静默跳过。

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_webdata::WebDataRpc;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

fn theme_toml(name: &str, bg: &str) -> String {
    format!("[meta]\nname = \"{name}\"\n\n[colors]\nbg = \"{bg}\"\n")
}

#[test]
fn same_slug_import_reports_conflict_in_result_and_force_overwrites() {
    // ⚠️ 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
    let tmp = std::env::temp_dir().join(format!(
        "wind_webdata_theme_import_conflict-{}",
        std::process::id()
    ));
    let root = tmp.join("install");
    let data = root.join("data");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录须已重定向，否则本测试会写真实 %APPDATA%\\WindInput\\themes"
    );

    write_at(
        &data,
        "themes/default/theme.toml",
        &theme_toml("默认", "#000000"),
    );
    let coord = Coordinator::new_headless(Config::default(), Some(&data));

    let import = |bg: &str, force: bool| -> anyhow::Result<Value> {
        coord.web_data_rpc(
            "theme.importFromText",
            &json!({ "yaml": theme_toml("我的主题", bg), "slug": "mine", "force": force }),
        )
    };
    let landed = || std::fs::read_to_string(user.join("themes/mine/theme.toml")).unwrap();

    // ① 首次导入：正常成功。没有这条正向对照，②在「导入整体坏掉」时也会绿。
    let first = import("#111111", false).expect("首次导入不该失败");
    assert_eq!(first["ok"], json!(true), "首次导入回执应 ok，实际={first}");
    assert!(landed().contains("#111111"), "首次导入应真的落盘");

    // ② 同 slug 再导入且 force=false：**不是 RPC 错误**，而是带 conflict 标记的正常回执。
    //    设置端据此弹「是否覆盖」——判据是 conflict 字段，不是报错文案。
    let again = import("#222222", false).expect("冲突是业务性失败，不该走 error 通道");
    assert_eq!(
        again["conflict"],
        json!(true),
        "同名冲突必须带机器可读的 conflict 标记（客户端据此弹覆盖确认），实际={again}"
    );
    assert_eq!(
        again["ok"],
        json!(false),
        "冲突时不许给成功回执，实际={again}"
    );
    assert_eq!(
        again["slug"],
        json!("mine"),
        "回执要点名撞上的是哪个主题 id，实际={again}"
    );
    assert_eq!(
        again["display_name"],
        json!("我的主题"),
        "回执要带显示名，覆盖确认框要拿它写正文，实际={again}"
    );
    assert!(
        landed().contains("#111111"),
        "force=false 撞名时一个字节都不许落盘，实际={}",
        landed()
    );

    // ③ force=true 重推：覆盖生效，且回执是成功而非 conflict。
    let forced = import("#333333", true).expect("force=true 应能覆盖");
    assert_eq!(
        forced["ok"],
        json!(true),
        "覆盖导入回执应 ok，实际={forced}"
    );
    assert!(
        forced.get("conflict").is_none(),
        "覆盖成功后不该再带 conflict 标记，实际={forced}"
    );
    assert!(
        landed().contains("#333333"),
        "force=true 必须真的写入新内容，实际={}",
        landed()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
