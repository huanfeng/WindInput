//! `theme.delete` 顺带删掉该主题的预览缓存图（`theme_render::forget_theme`），别的主题的不动。
//!
//! 为什么必须是集成测试（独立进程）：用户目录与缓存目录都在进程内首次取用时定下（OnceLock /
//! 环境变量），env 必须在任何初始化之前就位。
//! ⚠️ 全文件仅此一个 `#[test]`，理由同 `theme_import_conflict.rs`。
//! ⚠️ 只在 Linux 上跑：缓存目录靠 `XDG_DATA_HOME` 重定向，别的平台会碰到真实的本机目录。

#[cfg(target_os = "linux")]
#[test]
fn delete_removes_only_that_themes_previews() {
    use serde_json::json;
    use wind_config::Config;
    use wind_coordinator::Coordinator;
    use wind_webdata::WebDataRpc;

    let tmp = std::env::temp_dir().join(format!(
        "wind_webdata_theme_delete_forget-{}",
        std::process::id()
    ));
    let root = tmp.join("install");
    let data = root.join("data");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let local = tmp.join("local");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
        std::env::set_var("XDG_DATA_HOME", &local);
    }
    assert_eq!(Config::user_config_dir(), Some(user.clone()));
    let cache = Config::cache_dir()
        .expect("缓存目录")
        .join("theme_previews");
    assert!(
        cache.starts_with(&tmp),
        "前置条件：缓存目录须已重定向，否则本测试会删真实缓存：{}",
        cache.display()
    );

    let theme = "[meta]\nname = \"X\"\n";
    for slug in ["mine", "other"] {
        let d = user.join("themes").join(slug);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("theme.toml"), theme).unwrap();
    }
    std::fs::create_dir_all(data.join("themes")).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let key = "0123456789abcdef0123456789abcdef";
    let mine = cache.join(format!("mine.{key}.png"));
    let other = cache.join(format!("other.{key}.png"));
    std::fs::write(&mine, b"x").unwrap();
    std::fs::write(&other, b"x").unwrap();

    let coord = Coordinator::new_headless(Config::default(), Some(&data));
    let r = coord
        .web_data_rpc("theme.delete", &json!({ "name": "mine" }))
        .expect("删除用户主题");
    assert_eq!(r["ok"], json!(true));
    assert!(!user.join("themes/mine").exists());
    assert!(!mine.exists(), "删主题顺带删它的预览图");
    assert!(other.exists(), "别的主题的图不动");

    // 非普通 slug 一律拒绝，不拼路径、不删任何东西。
    assert!(
        coord
            .web_data_rpc(
                "theme.delete",
                &json!({ "name": "../UserData/themes/other" })
            )
            .is_err()
    );
    assert!(user.join("themes/other").exists());

    let _ = std::fs::remove_dir_all(&tmp);
}
