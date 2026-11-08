//! 主题导入的 id 不能跳出用户主题目录。
//!
//! id 来自调用方的 slug 或主题自己的 meta.name，都不可信。曾经 `meta.name = "../../victim"`
//! 会让目标落到主题目录外，依赖校验失败后的回滚再对它 `remove_dir_all`——整个目录被删。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：`Config::user_config_dir()` 走 `WIND_DATADIR_CONF` 且内部
//! OnceLock 缓存，env 必须在任何初始化之前就位（同 `theme_import_conflict.rs`）。

use serde_json::json;
use std::path::Path;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_webdata::WebDataRpc;

fn write_at(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
}

#[test]
fn import_ids_cannot_escape_user_theme_dir() {
    let tmp = std::env::temp_dir().join(format!(
        "wind_webdata_theme_import_path_ids-{}",
        std::process::id()
    ));
    let root = tmp.join("install");
    let data = root.join("data");
    let user = tmp.join("a").join("b").join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
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
        "前置：用户目录须已重定向"
    );

    write_at(
        &data,
        "themes/default/theme.toml",
        "[meta]\nname = \"默认\"\n",
    );
    // 受害目录：与用户目录同级的 victim/（themes/../../victim 正好落到这里），里面有文件和子目录。
    let victim = tmp.join("a").join("b").join("victim");
    write_at(&victim, "keep.txt", "keep");
    write_at(&victim, "sub/deep.txt", "deep");
    let coord = Coordinator::new_headless(Config::default(), Some(&data));

    // base 指向不存在的主题：依赖校验必失败，走回滚分支。
    let bad_base = |name: &str| format!("base = \"no-such-base\"\n[meta]\nname = \"{name}\"\n");
    let import = |text: String, slug: Option<&str>| {
        let mut p = json!({ "yaml": text, "force": true });
        if let Some(s) = slug {
            p["slug"] = json!(s);
        }
        coord.web_data_rpc("theme.importFromText", &p)
    };

    // ① meta.name 穿越、不给 slug：必须报错，受害目录一个字节不动。
    for name in ["../../victim", "../../../b/victim", "..\\..\\victim"] {
        let r = import(bad_base(name), None);
        assert!(r.is_err(), "穿越的 meta.name 必须被拒：{name} → {r:?}");
    }
    // ② 不合法的 slug 不再静默退回显示名，直接报错。
    for slug in ["../victim", "C:x", "a:b", "x.", "CON", "/abs"] {
        let r = import(bad_base("正常名字"), Some(slug));
        assert!(r.is_err(), "不合法的 slug 必须被拒：{slug} → {r:?}");
    }
    assert!(victim.join("keep.txt").exists(), "受害目录的文件被删了");
    assert!(
        victim.join("sub/deep.txt").exists(),
        "受害目录的子目录被删了"
    );

    // ③ 用户主题目录里已有同名目录（没有 theme.toml，但有别的文件）：回滚只撤写入的
    //    theme.toml，不删整个目录。
    let existing = user.join("themes").join("plain");
    write_at(&existing, "notes.txt", "mine");
    let r = import(bad_base("plain"), Some("plain"));
    assert!(r.is_err(), "base 不存在应报依赖校验失败：{r:?}");
    assert!(
        existing.join("notes.txt").exists(),
        "回滚删掉了原本就在的目录"
    );
    assert!(
        !existing.join("theme.toml").exists(),
        "回滚应撤掉写入的 theme.toml"
    );

    // ④ 正向对照：合法 id 照常导入；新建目录在校验失败时整个撤掉。
    let ok = import("[meta]\nname = \"我的\"\n".into(), Some("mine")).expect("合法导入");
    assert_eq!(ok["ok"], json!(true), "{ok}");
    assert!(user.join("themes/mine/theme.toml").exists());
    let r = import(bad_base("新的"), Some("fresh"));
    assert!(r.is_err());
    assert!(
        !user.join("themes/fresh").exists(),
        "本次新建的目录在回滚时应删掉"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
