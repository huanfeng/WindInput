//! 类型级全局设置（`schema.codetable` / `pinyin` / `mix` / `english`）只从出厂层生效：
//! 用户层与定制层写了不参与合并，用户层那份摘进 `Config::legacy_type_settings` 等着搬进方案。
//! 设计见 `docs/design/scheme-settings-flatten.md` §4.2。
//!
//! 全文件仅此一个 `#[test]`：它重定向用户目录并改写其中的 config.toml。

use std::path::{Path, PathBuf};
use wind_config::Config;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn user_and_custom_layers_no_longer_feed_type_level_settings() {
    let tmp = std::env::temp_dir().join("wind_legacy_type_settings_e2e");
    let root = tmp.join("install");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 出厂层：这里的值就是类型内置默认。
    write_at(
        &root.join("data"),
        "config.toml",
        "[schema]\nactive = \"wubi86\"\n[schema.codetable]\ntop_code_commit = true\n",
    );
    write_at(
        &root.join("data_custom"),
        "custom.toml",
        "[custom]\nid = \"t\"\nname = \"t\"\n",
    );
    write_at(
        &root.join("data_custom"),
        "config.toml",
        "[schema.codetable]\nwildcard = true\n[ui.candidate]\nper_page = 5\n",
    );
    let user_file = write_at(
        &user,
        "config.toml",
        "[schema]\nactive = \"pinyin\"\n[schema.codetable]\npunct_commit = true\n\
         [schema.pinyin]\nsentence_count = 3\n[schema.pinyin.fuzzy]\nenabled = true\n",
    );

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        Config::user_config_dir(),
        Some(user.clone()),
        "用户目录须已重定向"
    );

    let cfg = Config::load(Config::data_dir().as_deref()).unwrap();
    // 出厂层的值在；定制层、用户层写的不生效。
    assert!(cfg.schema.codetable.top_code_commit);
    assert_eq!(cfg.ui.candidate.per_page, 5, "前置条件：定制层确实加载了");
    assert!(!cfg.schema.codetable.wildcard, "定制层的类型段不读");
    assert!(!cfg.schema.codetable.punct_commit, "用户层的类型段不读");
    assert_eq!(cfg.schema.pinyin.sentence_count, 1);
    // 同一份文件里的其它内容照常生效。
    assert_eq!(cfg.schema.active, "pinyin");
    assert!(cfg.schema.pinyin.fuzzy.enabled, "模糊音仍是全局项");
    // 用户层那份原样留着待搬。
    let legacy = &cfg.legacy_type_settings;
    assert_eq!(legacy["codetable"]["punct_commit"].as_bool(), Some(true));
    assert_eq!(legacy["pinyin"]["sentence_count"].as_integer(), Some(3));
    assert!(legacy["pinyin"].get("fuzzy").is_none());

    // 清掉之后文件里只剩不相干的内容，再加载没有残留。
    assert!(Config::clear_legacy_type_settings().unwrap());
    let text = std::fs::read_to_string(&user_file).unwrap();
    assert!(
        !text.contains("punct_commit") && !text.contains("sentence_count"),
        "{text}"
    );
    assert!(
        text.contains("active = \"pinyin\"") && text.contains("[schema.pinyin.fuzzy]"),
        "{text}"
    );
    let cfg = Config::load(Config::data_dir().as_deref()).unwrap();
    assert!(cfg.legacy_type_settings.is_empty());
    assert!(cfg.schema.pinyin.fuzzy.enabled);
    assert!(
        !Config::clear_legacy_type_settings().unwrap(),
        "第二次无事可做"
    );
}
