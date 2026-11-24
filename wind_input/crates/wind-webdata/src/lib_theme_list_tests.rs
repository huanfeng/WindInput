use super::*;

fn data_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data")
}

/// 内置主题不给目录，`base` 取主题文件自己写的那一级。
#[test]
fn list_reports_base_and_hides_builtin_dir() {
    let c = wind_coordinator::Coordinator::new_headless(
        wind_config::Config::default(),
        Some(&data_dir()),
    );
    let list = c.web_theme_list(&json!({})).unwrap();
    let def = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "default")
        .expect("列出 default");
    assert_eq!(def["builtin"], true);
    assert_eq!(def["dir"], "");
    assert_eq!(def["base"], "_qingfeng");
    // 默认不算皮肤概要（要整份解析主题，拖慢设置端启动）：键不在。
    assert!(def.get("skin").is_none() && def.get("animated").is_none());
}

/// `skin:true` 补皮肤字段；`id` 只回那一项。
#[test]
fn list_skin_flag_and_id_filter() {
    let c = wind_coordinator::Coordinator::new_headless(
        wind_config::Config::default(),
        Some(&data_dir()),
    );
    let one = c
        .web_theme_list(&json!({ "skin": true, "id": "default" }))
        .unwrap();
    let arr = one.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "default");
    assert_eq!(arr[0]["skin"], false);
    assert_eq!(arr[0]["animated"], false);
    let all = c.web_theme_list(&json!({})).unwrap();
    assert!(all.as_array().unwrap().len() > 1);
}

#[test]
fn skin_summary_flags_animated_decor() {
    let root = std::env::temp_dir().join(format!("wind_skin_summary_{}", std::process::id()));
    let write = |slug: &str, decor: &str| {
        let d = root.join(slug);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join(wind_transfer::theme::THEME_ENTRY_NAME),
            format!(
                "[skin.horizontal.frame]\nimage = \"f.png\"\n[[skin.horizontal.decor]]\nimage = \"{decor}\"\n"
            ),
        )
        .unwrap();
    };
    write("still", "d.png");
    write("moving", "d.gif");
    // 扩展名不算数，看文件头：静态的 .webp 不是动图，GIF 签名才是。
    std::fs::write(root.join("still/d.png"), b"\x89PNG\r\n\x1a\n\0\0\0\0IDAT").unwrap();
    std::fs::write(root.join("moving/d.gif"), b"GIF89a").unwrap();
    write("webp", "d.webp");
    std::fs::write(root.join("webp/d.webp"), b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0").unwrap();
    let dirs = vec![root.clone()];
    let sum = |slug: &str| {
        theme_skin_summary(&wind_theme::load_resolved_dirs(&dirs, slug, false).unwrap())
    };
    assert_eq!(sum("still"), (true, false));
    assert_eq!(sum("moving"), (true, true));
    assert_eq!(sum("webp"), (true, false));
    assert_eq!(
        theme_skin_summary(&wind_theme::Resolved::default()),
        (false, false)
    );
    let _ = std::fs::remove_dir_all(root);
}
