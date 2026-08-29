//! 用被定制版 `[themes] hide` 掉的 slug 导入主题 ⇒ **当场拒掉，不给成功回执**。
//!
//! 为什么这一条单独值一个用例：hide 是绝对的（用户层同名主题也不复活，见
//! `Config::custom_hides_theme` 的取舍说明），而**主题导入 RPC 是用户唯一能主动撞上
//! 那个 id 的入口**。放行的话文件确实写下去了、回执是 `ok: true`，但它永远不进列表
//! （`list_themes_full` 滤掉）、选它也会被 `push_theme` 兜底掉——用户看到的是
//! 「导入成功了却哪儿都找不到」，一个自相矛盾的回执。
//!
//! 为什么必须是集成测试（独立进程）：`Config::custom_manifest()` 用 OnceLock 缓存。
//! ⚠️ 全文件仅此一个 `#[test]`，理由同 `wind-engine/tests/custom_layer_hide.rs`。
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

fn theme_toml(name: &str) -> String {
    format!("[meta]\nname = \"{name}\"\n\n[colors]\nbg = \"#123456\"\n")
}

#[test]
fn importing_a_hidden_theme_slug_is_refused_not_silently_orphaned() {
    // ⚠️ 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
    let tmp = std::env::temp_dir().join(format!(
        "wind_webdata_custom_hide_import-{}",
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

    // 清单必须在**任何** OnceLock 初始化之前就位。
    write_at(
        &custom,
        "custom.toml",
        "[custom]\nid = \"huma-edition\"\nversion = \"1.0\"\n\n\
         [themes]\nhide = [\"msime\"]\n",
    );

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
    assert_eq!(
        Config::custom_data_dir(),
        Some(custom.clone()),
        "前置条件：清单在场时 custom 层必须启用"
    );

    write_at(&data, "themes/default/theme.toml", &theme_toml("默认"));
    let coord = Coordinator::new_headless(Config::default(), Some(&data));

    let import = |slug: &str| -> anyhow::Result<Value> {
        coord.web_data_rpc(
            "theme.importFromText",
            &json!({ "yaml": theme_toml("我的主题"), "slug": slug }),
        )
    };

    // 正向对照：普通 slug 照常导入成功。没有这条，下面那条在「导入整体坏掉」时也会绿。
    let ok = import("mine").expect("普通 slug 必须能导入成功");
    assert_eq!(ok["ok"], json!(true), "正常导入的回执应是 ok，实际={ok}");
    assert!(
        user.join("themes/mine/theme.toml").is_file(),
        "正常导入应真的落盘"
    );

    // ★ 被 hide 的 slug：必须报错，且**一个字节都不许落盘**。
    let err = import("msime").expect_err("被 hide 的 slug 必须被拒，不能给成功回执");
    let msg = format!("{err}");
    assert!(
        msg.contains("msime"),
        "报错要点名是哪个 id 不可用（用户得知道改哪儿），实际={msg}"
    );
    assert!(
        !user.join("themes/msime").exists(),
        "拒掉就不该留下任何目录——留下的话用户下次导入会撞上「已存在（force=false）」，\
         而那个提示指向一个他根本看不见的主题"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
