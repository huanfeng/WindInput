//! 定制版减法（`custom.toml` 的 `[themes] hide`）在主题两侧的端到端接线。
//!
//! 主题的 hide 有两个消费侧，只做一侧各有现象：只做列表侧时，存量用户 `ui.theme.name` 指着的
//! 主题照常生效（设置页里找不到它、界面上就是它）；只做解析侧时，列表里还列着一个选了会变成
//! 别的样子的主题。故两侧各测一条：
//!
//! 1. 列表侧 `list_themes_full`：右键菜单与设置页 `theme.list` 共用的唯一实现（`wind-webdata`
//!    的 `web_theme_list` 复用它）；
//! 2. 解析侧 `theme_palette`（`theme_query.rs`，移动端拉取式色表，刻意不走 `push_theme` 那条链，
//!    与它共用 `Coordinator::theme_id_honoring_hide`）：断言拿到的是兜底主题的色表而不是被 hide
//!    那个的，且两者事先确实不同（否则断言恒真）。
//!
//! 另钉 hide 是绝对的：用户在 `%APPDATA%\WindInput\themes\` 放一份同名主题，它仍不出现在列表里。
//!
//! 必须是集成测试：`Config::custom_manifest()` 用 OnceLock 缓存，全文件仅此一个 `#[test]`，理由同
//! `wind-engine/tests/custom_layer_hide.rs`。不依赖 `build_dev/data`（主题夹具全部自造），不会静默跳过。

use std::path::{Path, PathBuf};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_coordinator::web_host::WebDataHost;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

/// 最小主题：`meta` 够列表显示，`colors.bg` 够 `theme_palette` 求值出一个可辨认的值。
fn theme_toml(name: &str, bg: &str) -> String {
    format!("[meta]\nname = \"{name}\"\norder = 1\n\n[colors]\nbg = \"{bg}\"\n")
}

/// `#RRGGBB` → `0xAARRGGBB`（`theme_palette` 的输出布局，A 补 FF）。
fn argb(rgb: u32) -> u32 {
    0xFF00_0000 | rgb
}

#[test]
fn hidden_themes_vanish_from_list_and_from_resolution() {
    // 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
    let tmp = std::env::temp_dir().join(format!(
        "wind_coord_custom_hide_theme_e2e-{}",
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

    // 清单必须在任何 OnceLock 初始化之前就位。定制者要删掉出厂的 msime 主题。
    write_at(
        &custom,
        "custom.toml",
        "[custom]\nid = \"huma-edition\"\nversion = \"1.0\"\n\n\
         [themes]\nhide = [\"msime\", \"user_named\"]\n",
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

    // data 层：兜底主题 default + 要被删掉的 msime + 一个留着的 violet（正向对照）
    write_at(
        &data,
        "themes/default/theme.toml",
        &theme_toml("默认", "#00BB00"),
    );
    write_at(
        &data,
        "themes/msime/theme.toml",
        &theme_toml("微软", "#AA0000"),
    );
    write_at(
        &data,
        "themes/violet/theme.toml",
        &theme_toml("紫", "#0000CC"),
    );
    // 用户层：用户拿被 hide 的 id 命名了自己的主题——仍然不可见（hide 是绝对的）。
    write_at(
        &user,
        "themes/user_named/theme.toml",
        &theme_toml("我的", "#123456"),
    );
    // 用户层独有主题（正向对照：用户层这条枚举路径本身没坏）
    write_at(
        &user,
        "themes/mine/theme.toml",
        &theme_toml("我的2", "#654321"),
    );

    // 活跃主题就是被 hide 的那个（存量用户升级到定制版的必然状态）。
    let mut cfg = Config::default();
    cfg.ui.theme.name = "msime".to_string();
    let coord = Coordinator::new_headless(cfg, Some(&data));

    // ── 1. 列表侧 ─────────────────────────────────────────────────────────────
    let rows = WebDataHost::list_themes_full(&*coord);
    let ids: Vec<&str> = rows.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(
        !ids.contains(&"msime"),
        "被 hide 的主题不得出现在列表里（右键菜单与设置页 theme.list 共用它），实际={ids:?}"
    );
    assert!(
        ids.contains(&"default") && ids.contains(&"violet"),
        "其余随包分发的主题必须照常列出，实际={ids:?}"
    );
    assert!(
        ids.contains(&"mine"),
        "用户层独有主题照常列出（证明用户层那条枚举路径没被顺手关掉），实际={ids:?}"
    );

    // ── 2. hide 是绝对的：用户层同名主题也不复活 ─────────────────────────────
    assert!(
        user.join("themes/user_named/theme.toml").is_file(),
        "前置条件：用户层那份同名主题确实在盘上，否则下面这条断言恒真"
    );
    assert!(
        !ids.contains(&"user_named"),
        "被 hide 的 id 在**任何层**都不存在，用户层放一份同名主题也不例外，实际={ids:?}"
    );

    // ── 3. 解析侧：active 指着被 hide 的主题 ⇒ 拿到的是兜底主题的色表 ─────────
    assert_eq!(
        coord.active_theme_id(),
        "msime",
        "前置条件：活跃主题**仍是**被 hide 的那个——降级只在解析侧发生，\
         不改写 ui.theme.name（用户卸掉定制包即恢复原样）"
    );
    let palette = coord.theme_palette(false);
    let bg = palette
        .iter()
        .find(|(k, _)| k == "bg")
        .map(|(_, v)| *v)
        .expect("色表里应有 bg");
    assert_eq!(
        bg,
        argb(0x00BB00),
        "解析侧必须落到兜底主题 default 的色表；拿到 msime 的色 = 只做了列表侧过滤"
    );
    assert_ne!(
        argb(0x00BB00),
        argb(0xAA0000),
        "前置条件：两个主题的 bg 必须不同，否则上一条断言恒真"
    );
    // 顺带钉住拉取面的标准色契约兜底（与桌面 `wind_theme::resolve` 同一口径）：夹具主题没写
    // base、也没写 error，照样拉得到 `_base` 的亮档 error。
    assert_eq!(
        palette.iter().find(|(k, _)| k == "error").map(|(_, v)| *v),
        Some(argb(0xD93025)),
        "无 base 主题拉调色板也应补上契约名"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
