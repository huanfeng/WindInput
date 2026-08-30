//! 软键盘：**没有定制层**时的两层合并必须与加层前逐字节一致。
//!
//! 这条防的是「为了加一层把原有两层的行为改坏」——绝大多数装机走的正是这条路径，
//! 而它在有定制层的用例里完全不被覆盖（那些用例每一条断言都靠 custom 层参与才成立）。
//!
//! 顺带钉住契约 2：判据是 `data_custom/custom.toml` **在场**，不是目录在场。
//! 只放 `system.softkeyboard.toml` 而没有清单的 `data_custom/` 必须被整层忽略——
//! 这一态在 `custom_layer_softkeyboard.rs` 里测不了（`custom_manifest()` 是 OnceLock，
//! 一个进程只能有一种层状态），故单开一个测试二进制。
//!
//! ⚠️ 本用例**不依赖 `build_dev/data`**：夹具全部自造，不会静默跳过。

use std::path::{Path, PathBuf};
use wind_config::Config;
use wind_coordinator::Coordinator;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn without_manifest_softkeyboard_keeps_the_old_two_layer_behavior() {
    // ⚠️ 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
    let tmp = std::env::temp_dir().join(format!(
        "wind_coord_softkeyboard_no_custom-{}",
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
        None,
        "前置条件：没有 custom.toml ⇒ 没有定制层（判据是文件在场，不是目录在场）"
    );

    write_at(
        &data,
        wind_softkeyboard::FILE_NAME,
        "[[pages]]\nid = \"p_data\"\nname = \"出厂\"\nkeys = { q = \"D_Q\", w = \"D_W\" }\n",
    );
    // 有目录、有软键盘文件，但**没有清单** ⇒ 整层忽略。
    write_at(
        &custom,
        wind_softkeyboard::FILE_NAME,
        "[[pages]]\nid = \"p_custom\"\nname = \"定制\"\nkeys = { q = \"C_Q\" }\n",
    );
    write_at(
        &user,
        wind_softkeyboard::FILE_NAME,
        "[[pages]]\nid = \"p_data\"\nkeys = { q = \"U_Q\" }\n\n\
         [[pages]]\nid = \"p_user\"\nname = \"用户\"\nkeys = { q = \"U_ONLY\" }\n",
    );

    let coord = Coordinator::new_headless(Config::default(), Some(&data));
    let table = coord.debug_softkeyboard();
    let ids: Vec<&str> = table.pages().iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["p_data", "p_user"],
        "无清单 ⇒ data_custom 里的面不得出现（契约 2：判据是 custom.toml 在场）"
    );

    let page = table.page("p_data").expect("出厂面必须在");
    assert_eq!(
        page.output("q", false),
        Some("U_Q"),
        "用户层的补丁照常叠加在出厂画布上（加层前后完全一致）"
    );
    assert_eq!(
        page.output("w", false),
        Some("D_W"),
        "用户层没碰的键保持出厂值——按面合并，不是整份取代"
    );
    assert_eq!(
        page.name, "出厂",
        "用户层只打补丁、没写 name ⇒ 保留出厂面名"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
