//! 清单解析失败 ⇒ **整个 custom 层不启用**（回落原版行为），不做「半启用」。
//!
//! 为什么这条要单独占一个测试二进制：判据 `Config::custom_manifest()` 用 OnceLock 缓存，
//! 同一进程里只解析一次盘上状态，与 `custom_layer.rs` 的「清单有效」互斥。
//!
//! 为什么不能半启用：半解析出来的清单可能丢掉 `hide` 名单，于是定制版里本该被删掉的
//! 方案又冒出来——诡异且难以归因。而「完全变回原版」现象足够明显，用户会立刻报障。

use std::path::{Path, PathBuf};
use wind_config::Config;
use wind_config::app_compat::AppCompat;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn unparsable_manifest_disables_the_whole_custom_layer() {
    let tmp = std::env::temp_dir().join("wind_custom_manifest_invalid_e2e");
    let root = tmp.join("install");
    let data = root.join("data");
    let custom = root.join("data_custom");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 语法就坏的清单：`hide` 缺右方括号。走的是 `toml::from_str` 失败那条路。
    write_at(
        &custom,
        "custom.toml",
        "[custom]\nid = \"huma-edition\"\n\n[schemas]\nhide = [\"wubi86\"\n",
    );
    // 定制层的其余内容一应俱全——正是要证明它们**全部**不生效，而不是只丢了清单本身。
    write_at(
        &custom,
        "config.toml",
        "[ui.candidate]\nper_page = 7\n[schema]\nactive = \"tigercode\"\n",
    );
    write_at(&custom, "only_custom.txt", "custom");
    write_at(&custom, "shared.txt", "custom");
    write_at(
        &custom,
        "compat.toml",
        "[[apps]]\nprocess = \"b.exe\"\ncaret_use_top = true\n",
    );

    write_at(
        &data,
        "config.toml",
        "[ui.candidate]\nper_page = 5\n[schema]\nactive = \"wubi86\"\n",
    );
    write_at(&data, "shared.txt", "data");
    write_at(&data, "compat.toml", "[[apps]]\nprocess = \"b.exe\"\n");

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

    assert!(
        Config::custom_manifest().is_none(),
        "清单解析失败必须判为「不是定制版」"
    );
    assert_eq!(
        Config::custom_data_dir(),
        None,
        "清单不可解析时定制层目录不得暴露给任何解析点"
    );
    assert_eq!(
        Config::resource_layers(),
        vec![user.clone(), data.clone()],
        "层序里不得出现 custom 层"
    );

    // config.toml：定制层不参与合并，四层退化为三层。
    let cfg = Config::load(Some(&data)).unwrap();
    assert_eq!(cfg.ui.candidate.per_page, 5, "须取 L2，不得取 L2.5");
    assert_eq!(cfg.schema.active, "wubi86", "须取 L2，不得取 L2.5");
    let preset = Config::system_preset_value(Some(&data)).unwrap();
    assert_eq!(preset["ui"]["candidate"]["per_page"].as_integer(), Some(5));

    // 单文件解析：定制层既不覆盖，也不提供独有文件。
    assert_eq!(
        Config::resolve_data_file(Some(&data), "shared.txt"),
        Some(data.join("shared.txt")),
        "定制层不得覆盖 data 层"
    );
    assert_eq!(
        Config::resolve_data_file(Some(&data), "only_custom.txt"),
        None,
        "定制层独有的文件也须不可见（这就是「整层退场」）"
    );

    // compat.toml：定制层那条规则不得生效。
    let compat = AppCompat::load(Some(&data), Some(&user));
    assert!(
        !compat.get_rule("b.exe").unwrap().caret_use_top,
        "定制层的 compat 规则不得生效"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
