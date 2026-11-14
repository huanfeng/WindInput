//! 主题导入的 id 不能跳出用户主题目录。
//!
//! id 来自调用方的 slug 或主题自己的 meta.name / 包名，都不可信。曾经 `meta.name = "../../victim"`
//! 会让目标落到主题目录外，依赖校验失败后的回滚再对它 `remove_dir_all`——整个目录被删。
//! 没给 slug 时 id 从显示名派生成普通名字；给了不合法的 slug 直接报错。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：`Config::user_config_dir()` 走 `WIND_DATADIR_CONF` 且内部
//! OnceLock 缓存，env 必须在任何初始化之前就位（同 `theme_import_conflict.rs`）。

use serde_json::json;
use std::io::Write;
use std::path::Path;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_webdata::WebDataRpc;

fn write_at(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
}

/// 手工打一个主题包（package.toml 的 name 可以随便写，export_package 写不出穿越名）。
fn write_package(path: &Path, package_name: &str, theme_text: &str) {
    let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    let meta =
        format!("[package]\nformat_version = 1\nkind = \"theme\"\nname = \"{package_name}\"\n");
    w.start_file("package.toml", opts).unwrap();
    w.write_all(meta.as_bytes()).unwrap();
    w.start_file("theme.toml", opts).unwrap();
    w.write_all(theme_text.as_bytes()).unwrap();
    w.finish().unwrap();
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
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

    let themes = user.join("themes");
    let err = |r: anyhow::Result<serde_json::Value>| format!("{:#}", r.expect_err("应报错"));

    // ① meta.name 穿越、不给 slug：id 派生成普通名字留在主题目录内，依赖校验失败的回滚
    //    只删这个新建的派生目录；受害目录一个字节不动。
    // 反斜杠在 TOML 基本字符串里要转义，`..\\\\..` 落到 meta.name 里是 `..\..`。
    for name in ["../../victim", "../../../b/victim", "..\\\\..\\\\victim"] {
        let e = err(import(bad_base(name), None));
        assert!(e.contains("依赖校验失败"), "{name} → {e}");
    }
    assert!(
        names_in(&themes).is_empty(),
        "派生目录回滚后不该留下：{:?}",
        names_in(&themes)
    );
    // ② 不合法的 slug 不再静默退回显示名，直接报错。
    for slug in [
        "../victim",
        "C:x",
        "a:b",
        "x.",
        "CON",
        "/abs",
        "VERYLO~1",
        "x.wtheme-old",
    ] {
        let e = err(import(bad_base("正常名字"), Some(slug)));
        assert!(e.contains("不合法"), "不合法的 slug 必须被拒：{slug} → {e}");
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
    //    slug 两端的空白不算 id 的一部分。
    let ok = import("[meta]\nname = \"我的\"\n".into(), Some("  mine  ")).expect("合法导入");
    assert_eq!(ok["ok"], json!(true), "{ok}");
    assert_eq!(ok["slug"], json!("mine"), "{ok}");
    assert!(user.join("themes/mine/theme.toml").exists());
    let r = import(bad_base("新的"), Some("fresh"));
    assert!(r.is_err());
    assert!(
        !user.join("themes/fresh").exists(),
        "本次新建的目录在回滚时应删掉"
    );

    // ⑤ 不给 slug、显示名含 `:`（设置端的粘贴 / 文件 / 链接导入都这样调）：派生 id 导入成功。
    let ok = import("[meta]\nname = \"Nord: Dark\"\n".into(), None).expect("显示名派生 id");
    assert_eq!(ok["slug"], json!("Nord- Dark"), "{ok}");
    assert_eq!(ok["display_name"], json!("Nord: Dark"), "{ok}");
    assert!(themes.join("Nord- Dark/theme.toml").exists());

    // ⑤b 两个显示名派生到同一 id（`Nord: Dark` 与 `Nord/ Dark` 都成 `Nord- Dark`）、不强制：
    //     报冲突不覆盖，文案点出被占目录里**已有**主题的名字。
    let p = json!({ "yaml": "[meta]\nname = \"Nord/ Dark\"\n", "force": false });
    let again = coord
        .web_data_rpc("theme.importFromText", &p)
        .expect("冲突走回执");
    assert_eq!(again["conflict"], json!(true), "{again}");
    assert_eq!(again["slug"], json!("Nord- Dark"), "{again}");
    assert_eq!(again["existing_name"], json!("Nord: Dark"), "{again}");
    assert!(
        again["error"].as_str().unwrap().contains("Nord: Dark"),
        "{again}"
    );
    assert!(
        std::fs::read_to_string(themes.join("Nord- Dark/theme.toml"))
            .unwrap()
            .contains("Nord: Dark"),
        "不强制时不该覆盖"
    );

    // ⑥ 包导入：package.toml 的 name 穿越、base 合法（依赖校验通过，走成功分支）。
    //    落到主题目录内的派生 id；受害目录不被挪走（不留 victim.wtheme-old），内容原样。
    let pkg = tmp.join("evil.wtheme");
    write_package(
        &pkg,
        "../../victim",
        "base = \"default\"\n[meta]\nname = \"x\"\n",
    );
    let p = json!({ "path": pkg.to_string_lossy(), "force": true });
    let ok = coord
        .web_data_rpc("theme.importPackage", &p)
        .expect("穿越的包名派生成普通 id 后照常导入");
    assert_eq!(ok["ok"], json!(true), "{ok}");
    let id = ok["slug"].as_str().unwrap().to_string();
    assert!(wind_theme::is_plain_slug(&id), "{id:?}");
    assert!(themes.join(&id).join("theme.toml").exists(), "{id:?}");
    let victim_parent = victim.parent().unwrap();
    assert!(!victim_parent.join("victim.wtheme-old").exists());
    assert!(!victim_parent.join("victim.wtheme-tmp").exists());
    assert_eq!(names_in(&victim), ["keep.txt", "sub"], "受害目录被改动");
    assert_eq!(
        std::fs::read_to_string(victim.join("keep.txt")).unwrap(),
        "keep"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
