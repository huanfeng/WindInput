//! 便携模式下的三层：`userdata` / `data_custom` / `data` **同处一棵目录树**。
//!
//! 为什么单列一个测试：安装版的三层分散在 `%APPDATA%` 与 Program Files 两处，
//! 便携版则全在 exe 同目录下互为兄弟——这是层序最容易串味的形态（三个目录只差一个名字，
//! 任何一处把根拼错都会落到隔壁层上），而它此前**完全测不到**：`is_portable()` 的判据
//! 是 `current_exe()` 同目录有无标记，测试进程改不了自己的 exe 位置。
//!
//! 补法是让 `is_portable()` / `portable_userdata_dir()` 与 `data_dir()` 共用
//! `variant::install_root()` 这一个根（含仅供测试的 `WIND_INSTALL_ROOT` 注入点）。
//! 两者必须同源：标记在注入根下找到、userdata 却回落真实 exe 目录的话，测出来的是一个
//! 生产中不存在的混合形态。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：`is_portable()` 与 `custom_manifest()` 都是 OnceLock。

use std::path::{Path, PathBuf};
use wind_config::Config;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn portable_layout_keeps_layer_order() {
    let tmp = std::env::temp_dir().join("wind_custom_layer_portable_e2e");
    let root = tmp.join("PortableApp");
    let data = root.join("data");
    let custom = root.join("data_custom");
    let user = root.join("userdata");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&root).unwrap();

    // 便携标记 + 定制版清单：两者都必须在任何 OnceLock 初始化之前就位。
    std::fs::write(root.join("portable_mode"), "portable=1\n").unwrap();
    write_at(&custom, "custom.toml", "[custom]\nid = \"huma-edition\"\n");

    // 一份指向别处的 datadir.conf：便携必须**忽略**它。
    // 这条决策此前只有纯函数单测（`decide_custom_dir`），端到端从未验证过——
    // 而它守的是「一台装过正式版的机器上，便携包把数据写回安装版目录」这个故障。
    let decoy = tmp.join("Decoy");
    let conf = tmp.join("datadir.conf");
    std::fs::write(&conf, decoy.to_string_lossy().as_bytes()).unwrap();

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_INSTALL_ROOT", &root);
        std::env::set_var("WIND_DATADIR_CONF", &conf);
    }

    assert!(
        wind_config::variant::is_portable(),
        "前置条件：标记在安装根下，必须被判为便携"
    );
    assert_eq!(
        Config::user_config_dir(),
        Some(user.clone()),
        "便携模式的用户层是 <安装根>/userdata，且不得被 datadir.conf 改道"
    );
    assert!(
        !decoy.exists(),
        "便携模式绝不能去建 datadir.conf 指定的目录"
    );
    assert_eq!(Config::data_dir(), Some(data.clone()));
    assert_eq!(Config::custom_data_dir(), Some(custom.clone()));

    // ★ 三个目录互为兄弟，层序仍须是 user > custom > data。
    assert_eq!(
        Config::resource_layers(),
        vec![user.clone(), custom.clone(), data.clone()],
        "便携形态下层序不得改变"
    );

    // 四层合并：用户层缺失的键落 L2.5，不穿透回 L2。
    write_at(
        &data,
        "config.toml",
        "[ui.candidate]\nper_page = 5\nper_page_extended = 5\n",
    );
    write_at(&custom, "config.toml", "[ui.candidate]\nper_page = 7\n");
    write_at(&user, "config.toml", "[schema]\nactive = \"pinyin\"\n");
    let cfg = Config::load(Some(&data)).unwrap();
    assert_eq!(cfg.schema.active, "pinyin");
    assert_eq!(cfg.ui.candidate.per_page, 7, "须回落 L2.5");
    assert_eq!(
        cfg.ui.candidate.per_page_extended, 5,
        "L2 未被定制层触及的键保留"
    );

    // 单文件解析：三个兄弟目录不得串味。
    write_at(&data, "shared.txt", "data");
    let c = write_at(&custom, "shared.txt", "custom");
    assert_eq!(
        Config::resolve_data_file(Some(&data), "shared.txt"),
        Some(c),
        "用户层缺失时须落定制层"
    );
    let u = write_at(&user, "shared.txt", "user");
    assert_eq!(
        Config::resolve_data_file(Some(&data), "shared.txt"),
        Some(u),
        "用户层在场时须胜出"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
