//! 用户 emoji 文件内容变了（如同步落地）后，配置重载要重建该表，而不是因判据未变直接跳过。
//!
//! 便携标记把整棵目录树重定向到临时目录，数据根为空、只放一张用户表，不依赖 `build_dev/data`。
//! 一个进程只能重定向一次，故单开测试二进制。

use std::sync::Arc;
use wind_config::Config;
use wind_coordinator::Coordinator;

#[test]
fn user_emoji_file_edit_takes_effect_on_reload() {
    let root = std::env::temp_dir().join(format!("wind_emoji_user_reload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("userdata/emoji")).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join(wind_config::variant::PORTABLE_MARKER_NAME), "").unwrap();
    // SAFETY: 在任何路径缓存初始化之前设置；本二进制只有这一个用例。
    unsafe {
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(Config::user_config_dir(), Some(root.join("userdata")));
    let table = root.join("userdata/emoji/mine.txt");
    std::fs::write(&table, "苹果\t苹果 🍎\n").unwrap();

    let store = Arc::new(wind_store::Store::open(root.join("s.redb")).unwrap());
    let mut cfg = Config::default();
    cfg.input.emoji.enabled = true;
    cfg.input.emoji.modules.insert("mine".into(), true);
    let coord = Coordinator::new_headless_with_store(cfg, Some(&root.join("data")), store);
    coord.debug_sync_emoji_dict();
    assert_eq!(coord.debug_emoji_lookup("苹果").as_deref(), Some("🍎"));

    std::fs::write(&table, "苹果\t苹果 🍏 🍐\n").unwrap();
    coord.debug_sync_emoji_dict();
    assert_eq!(coord.debug_emoji_lookup("苹果").as_deref(), Some("🍏 🍐"));
    let _ = std::fs::remove_dir_all(&root);
}
