//! 定制层（`data_custom`）在 **wind-engine 的枚举点**上的端到端接线：
//! 方案枚举（`installed_schemas`）、方案文件解析（`resolve_schema_file`，经
//! `schema_supported` 间接验证）、双拼布局枚举（`shuangpin_layouts`）。
//!
//! 为什么必须是集成测试（独立进程）：`Config::custom_manifest()` 用 OnceLock 缓存，
//! 同一进程里只解析一次盘上状态。用 `WIND_INSTALL_ROOT` 把安装根重定向到临时目录
//! （`data/` 与 `data_custom/` 同级），用 `WIND_DATADIR_CONF` 把用户目录重定向。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：多个测试在同一二进制里并行会争抢这两个环境变量与
//! OnceLock，先跑的那个会把层定死，后跑的静默测到错误的目标。
//! ⚠️ 两个环境变量都**必须**设：漏掉 `WIND_DATADIR_CONF` 时用户层会指向真实的
//! `%APPDATA%\WindInput`（本仓已有把测试写进真实用户目录的前科）。

use std::path::{Path, PathBuf};
use wind_config::Config;
use wind_engine::EngineManager;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

/// 最小可用的码表方案文件（`is_supported()` 为真即可进 `installed_schemas`）。
fn schema_toml(id: &str) -> String {
    format!("[schema]\nid = \"{id}\"\n\n[engine]\ntype = \"codetable\"\n")
}

/// 最小可用的双拼布局（缺 `[finals]` 会被枚举跳过）。
fn layout_toml(id: &str, name: &str) -> String {
    format!("[meta]\nid = \"{id}\"\nname = \"{name}\"\n\n[finals]\na = [\"a\"]\n")
}

#[test]
fn custom_layer_is_visible_to_engine_enumerations() {
    // ⚠️ 目录名带 pid：本仓常态是多 worktree / 多会话并行跑测试，固定名 + 开头的
    // `remove_dir_all` 会让两个 cargo test 互删夹具，失败现象与真实缺陷难以区分。
    let tmp = std::env::temp_dir().join(format!(
        "wind_engine_custom_layer_e2e-{}",
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

    // 清单必须在**任何** OnceLock 初始化之前就位，否则本次进程里定制层恒为关闭。
    write_at(
        &custom,
        "custom.toml",
        "[custom]\nid = \"huma-edition\"\nname = \"虎码定制版\"\nversion = \"1.0\"\n",
    );

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录须已重定向，否则本测试会读写真实 %APPDATA%"
    );
    assert_eq!(
        Config::custom_data_dir(),
        Some(custom.clone()),
        "前置条件：清单在场时 custom 层必须启用"
    );

    // data 层：内置方案 + 一个双拼布局
    write_at(
        &data,
        "schemas/builtin.schema.toml",
        &schema_toml("builtin"),
    );
    write_at(&data, "schemas/shared.schema.toml", &schema_toml("shared"));
    write_at(
        &data,
        "schemas/shuangpin/xiaohe.toml",
        &layout_toml("xiaohe", "小鹤双拼"),
    );
    // custom 层：定制版自带方案（加法不需要声明）+ 同名覆盖 + 自带布局 + 同名覆盖布局
    write_at(&custom, "schemas/tiger.schema.toml", &schema_toml("tiger"));
    write_at(
        &custom,
        "schemas/shared.schema.toml",
        &schema_toml("shared"),
    );
    write_at(
        &custom,
        "schemas/shuangpin/huma.toml",
        &layout_toml("huma", "虎码双拼"),
    );
    write_at(
        &custom,
        "schemas/shuangpin/xiaohe.toml",
        &layout_toml("xiaohe", "小鹤(定制版)"),
    );
    // 用户层：第三方方案（层序最靠前的那一层不能因为加了 custom 就失效）
    write_at(&user, "schemas/mine.schema.toml", &schema_toml("mine"));

    let mut cfg = Config::default();
    cfg.schema.available = vec!["builtin".to_string()];
    cfg.schema.active = "builtin".to_string();
    let mgr = EngineManager::new(&cfg, Some(&data));

    // ── 方案枚举：合并去重，各层都贡献 id ────────────────────────────────────
    let ids = mgr.installed_schemas();
    for want in ["builtin", "shared", "tiger", "mine"] {
        assert!(
            ids.contains(&want.to_string()),
            "installed_schemas 应含 {want}（各层合并去重），实际={ids:?}"
        );
    }
    // ★ 最要紧的一条：`tiger` 只存在于 data_custom。它能进列表，说明 scan_dirs 扫到了
    // custom 层，**并且** schema_supported → read_schema → resolve_schema_file 也解析到了
    // custom 层——后者是 P1d 之前完全缺失的一环（`resolve_schema_file` 是另一份两层实现），
    // 缺了它的现象是「我把虎码方案放进 data_custom 了，程序当没看见」，无任何日志。
    assert_eq!(
        ids.iter().filter(|i| *i == "shared").count(),
        1,
        "同名方案在多层出现时只应列一次，实际={ids:?}"
    );

    // ── 双拼布局枚举：靠前的层胜出（与方案枚举的「合并去重」语义不同）──────────
    let layouts = mgr.shuangpin_layouts();
    assert!(
        layouts.contains(&("huma".to_string(), "虎码双拼".to_string())),
        "定制层独有的布局必须能枚举到，实际={layouts:?}"
    );
    assert!(
        layouts.contains(&("xiaohe".to_string(), "小鹤(定制版)".to_string())),
        "同名布局须由靠前的层（custom > data）胜出，实际={layouts:?}"
    );
    assert!(
        !layouts.iter().any(|(_, name)| name == "小鹤双拼"),
        "被遮蔽的 data 层布局不得同时出现，实际={layouts:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
