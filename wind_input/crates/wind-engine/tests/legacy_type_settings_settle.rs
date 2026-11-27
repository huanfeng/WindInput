//! 用户层旧的类型级全局段搬进方案设置后才从 config.toml 删掉：有方案没搬成就留着，下次启动重试。
//!
//! 全文件仅此一个 `#[test]`：它重定向用户目录并改写其中的 config.toml。

use std::path::{Path, PathBuf};

use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data")
}

const USER_CONFIG: &str = "[schema]\nactive = \"wubi86\"\n[schema.pinyin]\nsentence_count = 2\n";

/// 在 `tag` 目录下建用户目录（含带旧段的 config.toml）并起一个引擎；`prepare` 在起引擎前摆弄方案设置目录。
fn run(tag: &str, prepare: impl FnOnce(&Path)) -> PathBuf {
    let root = std::env::temp_dir().join(format!("wind_legacy_settle_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let user = root.join("user");
    let ov = user.join("schema_overrides");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(user.join("config.toml"), USER_CONFIG).unwrap();
    Config::set_dir_override(user.clone(), root.join("local"));
    prepare(&ov);

    let mut cfg = Config::default();
    cfg.schema.available = ["pinyin", "shuangpin", "wubi86"].map(String::from).to_vec();
    cfg.schema.active = "wubi86".into();
    cfg.legacy_type_settings = toml::from_str("[pinyin]\nsentence_count = 2\n").unwrap();
    let _m = EngineManager::with_store_override(&cfg, Some(&data_dir()), None, Some(ov));
    user
}

fn config_text(user: &Path) -> String {
    std::fs::read_to_string(user.join("config.toml")).unwrap()
}

#[test]
fn legacy_sections_are_cleared_only_after_every_scheme_took_them() {
    // 方案设置文件不是 UTF-8：原文件一个字节都不动，旧段留着。
    let bad: &[u8] = b"[engine.pinyin]\nsentence_count = 4 # \xff\xfe\n";
    let user = run("non_utf8", |ov| {
        std::fs::write(ov.join("shuangpin.toml"), bad).unwrap()
    });
    let ov = user.join("schema_overrides");
    assert_eq!(std::fs::read(ov.join("shuangpin.toml")).unwrap(), bad);
    assert!(config_text(&user).contains("[schema.pinyin]"));

    // 写盘失败（临时文件位置被目录占着）：旧段留着。
    let user = run("persist_fail", |ov| {
        std::fs::create_dir_all(ov.join("shuangpin.toml.tmp")).unwrap()
    });
    assert!(config_text(&user).contains("[schema.pinyin]"));
    assert!(
        std::fs::read_to_string(user.join("schema_overrides/pinyin.toml"))
            .unwrap()
            .contains("sentence_count = 2"),
        "能搬的方案照搬"
    );

    // 全部成功：旧段清掉，其余内容保留。
    let user = run("ok", |_| {});
    let text = config_text(&user);
    assert!(!text.contains("[schema.pinyin]"), "{text}");
    assert!(text.contains("active = \"wubi86\""), "{text}");
    for id in ["pinyin", "shuangpin"] {
        let t = std::fs::read_to_string(user.join(format!("schema_overrides/{id}.toml"))).unwrap();
        assert!(t.contains("sentence_count = 2"), "{id}: {t}");
    }
}
