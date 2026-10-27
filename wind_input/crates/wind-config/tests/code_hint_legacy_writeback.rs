//! 旧客户端经 `set_user_value` 直接写退役的编码来源键：本次的值要覆盖已有的分配格，
//! 旧键不落盘（设计 candidate-comment-templates §13.6）。
//!
//! 集成测试（独立进程）：用户目录经 `WIND_DATADIR_CONF` 重定向，OnceLock 进程内只认一次。
//! ⚠️ 全文件仅此一个 `#[test]`。

use wind_config::Config;

fn cell(v: &toml::Value, scope: &[&str]) -> Option<String> {
    let mut t = v.get("ui")?.get("comment")?.get("assign")?;
    for k in scope {
        t = t.get(*k)?;
    }
    t.get("default_horizontal")?.as_str().map(String::from)
}

#[test]
fn legacy_code_hint_writes_override_existing_cells() {
    let tmp = std::env::temp_dir().join(format!("wind_code_hint_legacy_{}", std::process::id()));
    let root = tmp.join("install");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();
    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录已重定向"
    );
    let file = user.join("config.toml");
    let read =
        || -> toml::Value { toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap() };
    let s = |v: &str| toml::Value::String(v.into());

    std::fs::write(
        &file,
        "[ui.comment.assign.type.pinyin]\ndefault_horizontal = \"code\"\n\
         [ui.comment.assign.type.shuangpin]\ndefault_horizontal = \"code\"\n",
    )
    .unwrap();
    Config::set_user_value(&["schema", "pinyin", "code_hint_source"], s("shuangpin")).unwrap();
    let v = read();
    assert_eq!(cell(&v, &["type", "pinyin"]).as_deref(), Some("shuangpin"));
    assert_eq!(
        cell(&v, &["type", "shuangpin"]).as_deref(),
        Some("shuangpin")
    );
    assert!(
        v.get("schema")
            .and_then(|t| t.get("pinyin"))
            .and_then(|t| t.get("code_hint_source"))
            .is_none(),
        "旧键不落盘：{v}"
    );

    Config::set_user_value(&["schema", "pinyin", "code_hint_source"], s("off")).unwrap();
    let v = read();
    assert_eq!(cell(&v, &["type", "pinyin"]), None, "off = 回到出厂");
    assert_eq!(cell(&v, &["type", "shuangpin"]), None);

    Config::set_user_value(&["input", "temp_pinyin", "code_hint_source"], s("off")).unwrap();
    let v = read();
    assert_eq!(cell(&v, &["mode", "temp_pinyin"]).as_deref(), Some("none"));
    assert_eq!(cell(&v, &["mode", "mix"]).as_deref(), Some("none"));

    Config::set_user_value(
        &["schema", "mix", "pinyin_code_hint"],
        toml::Value::Boolean(false),
    )
    .unwrap();
    assert_eq!(
        cell(&read(), &["type", "mixed"]).as_deref(),
        Some("code_hint")
    );
    Config::set_user_value(
        &["schema", "mix", "pinyin_code_hint"],
        toml::Value::Boolean(true),
    )
    .unwrap();
    assert_eq!(cell(&read(), &["type", "mixed"]), None, "true = 回到出厂");

    let _ = std::fs::remove_dir_all(&tmp);
}
