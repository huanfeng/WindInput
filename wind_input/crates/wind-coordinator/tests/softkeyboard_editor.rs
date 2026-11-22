//! 软键盘编辑器（`softkeyboard.get` / `softkeyboard.apply`）的端到端：写用户层 → 热重载 →
//! 运行时那张表当场换新，不必重启服务。
//!
//! 单开一个测试二进制：用户目录经 `WIND_DATADIR_CONF` 重定向，那是进程级 OnceLock。
//!
//! ⚠️ 本用例**不依赖 `build_dev/data`**：夹具全部自造，不会静默跳过。

use std::path::Path;

use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_coordinator::web_host::WebDataHost;
use wind_softkeyboard::admin::Op;

fn write_at(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
}

#[test]
fn editor_writes_the_user_layer_and_hot_reloads() {
    let tmp =
        std::env::temp_dir().join(format!("wind_coord_softkbd_editor-{}", std::process::id()));
    let root = tmp.join("install");
    let data = root.join("data");
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
        "前置条件：用户目录须已重定向，否则本测试会读写真实用户目录"
    );
    write_at(
        &data,
        wind_softkeyboard::FILE_NAME,
        "[[pages]]\nid = \"p\"\nname = \"出厂\"\nkeys = { q = \"D_Q\" }\n",
    );

    let coord = Coordinator::new_headless(Config::default(), Some(&data));
    assert_eq!(
        coord
            .debug_softkeyboard()
            .page("p")
            .unwrap()
            .output("q", false),
        Some("D_Q")
    );

    let view = coord
        .softkeyboard_apply(&[
            Op::SetKey {
                page: "p".into(),
                slot: "q".into(),
                shift: false,
                value: Some("U_Q".into()),
            },
            Op::CreatePage {
                id: "mine".into(),
                name: "我的".into(),
                copy_from: None,
            },
            Op::SetKey {
                page: "mine".into(),
                slot: "a".into(),
                shift: true,
                value: Some("Α".into()),
            },
        ])
        .expect("apply");

    // 1) 落到了用户层，且只有补丁。
    let written = std::fs::read_to_string(user.join(wind_softkeyboard::FILE_NAME)).unwrap();
    assert!(
        written.contains("U_Q") && !written.contains("rows"),
        "{written}"
    );
    // 2) 运行时那张表当场换新（不重启）。
    let table = coord.debug_softkeyboard();
    assert_eq!(table.page("p").unwrap().output("q", false), Some("U_Q"));
    assert_eq!(table.page("mine").unwrap().output("a", true), Some("Α"));
    // 3) 返回的就是新视图，带默认值供「恢复默认」。
    let pages = view["pages"].as_array().unwrap();
    let p = pages.iter().find(|p| p["id"] == "p").unwrap();
    let q = p["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["slot"] == "q")
        .unwrap();
    assert_eq!(q["base"], "U_Q");
    assert_eq!(q["defaultBase"], "D_Q");
    assert_eq!(q["basePatched"], true);
    assert_eq!(coord.softkeyboard_view().unwrap(), view);

    // 4) 失败的批次不动文件。
    assert!(
        coord
            .softkeyboard_apply(&[Op::DeletePage { page: "p".into() }])
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(user.join(wind_softkeyboard::FILE_NAME)).unwrap(),
        written
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
