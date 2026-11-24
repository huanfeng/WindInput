use super::*;
use wind_config::Config;

/// 设置端写完规则后，前台进程必须立即按新规则重算，而不是等切走再切回。
/// 右键菜单与设置端「应用兼容」的候选窗首显文案必须一致（设置端取的是核心登记的
/// `compat_schema::OPTION_LABELS`）：两边各写一份，改一边漏一边用户就会看到两套说法。
#[test]
fn menu_titles_and_option_names_match_the_settings_labels() {
    use wind_config::compat_schema::{COMPAT_FIELDS, option_label_for};
    // 字段键 → 右键菜单「应用独立配置」里对应子菜单的标题。设置端的字段名取核心元数据，
    // 两边必须是同一个词，否则用户在菜单里见到的叫法在设置里找不到。
    let src = include_str!("handle_menu.rs");
    for (key, title) in [
        ("first_show_mode", "候选窗首显"),
        ("candidate_position_mode", "候选窗定位"),
        ("initial_mode", "初始输入模式"),
        ("initial_punct", "初始标点模式"),
        ("schema", "方案"),
        ("status_position_mode", "状态提示位置"),
        ("status_fallback_position", "坐标不可用时"),
        ("auto_pair", "符号自动配对"),
        ("ignore_host_ime_switch", "软件切换中英"),
        ("password_force_english", "密码框强制英文"),
    ] {
        let label = COMPAT_FIELDS
            .iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("元数据里没有 {key}"))
            .label;
        assert!(
            label.contains(title),
            "{key}: 设置端字段名 {label:?} 应包含菜单标题 {title:?}"
        );
        assert!(
            src.contains(&format!("\"{title}\"")),
            "菜单里没有标题 {title:?}（改了菜单要同步核心元数据）"
        );
    }
    // 状态提示位置的锚点名（菜单与设置端共用同一批取值）。
    for a in wind_config::app_compat::StatusAnchor::ALL {
        for key in ["status_position_mode", "status_fallback_position"] {
            assert_eq!(
                option_label_for(key, a.as_config()),
                Some(status_anchor_label(a)),
                "{key} 的 {} 与菜单不一致",
                a.as_config()
            );
        }
    }
    assert_eq!(
        option_label_for("status_fallback_position", "last"),
        Some("上次位置")
    );
    assert_eq!(
        option_label_for("status_fallback_position", "hide"),
        Some("不显示")
    );
}

#[test]
fn first_show_menu_labels_match_the_settings_option_labels() {
    for (_, mode, label) in Coordinator::FIRST_SHOW_MENU {
        let Some(mode) = mode else { continue };
        assert_eq!(
            wind_config::compat_schema::option_label(mode.as_config()),
            Some(label),
            "{} 的菜单文案与设置端不一致",
            mode.as_config()
        );
    }
}

#[test]
fn reload_compat_and_refresh_applies_new_rule_to_the_focused_process_immediately() {
    let user = std::env::temp_dir().join(format!("wind_compat_reload_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    let (c, _rx) = Coordinator::new_headless_with_ui_at(Config::default(), None, Some(&user));

    // 前台是 feishu.exe（pid 4242），此时还没有任何规则。
    c.active_compat.lock().unwrap().pid = 4242;
    c.pid_names
        .lock()
        .unwrap()
        .insert(4242, "feishu.exe".into());
    assert!(!c.active_compat.lock().unwrap().stale_probe_guard);

    std::fs::write(
        user.join("compat.toml"),
        "[[apps]]\nprocess = \"Feishu.exe\"\nstale_probe_guard = true\npin_anchor_when_start_drifts = true\n",
    )
    .unwrap();

    // 对照：只重载规则表，缓存仍是旧的——这正是需要 refresh 的原因。
    c.reload_app_compat();
    assert!(
        !c.active_compat.lock().unwrap().stale_probe_guard,
        "对照失效：只重载就已经生效，说明 refresh 这一步没有存在的理由"
    );

    c.reload_compat_and_refresh();
    let ac = *c.active_compat.lock().unwrap();
    assert!(ac.stale_probe_guard, "前台进程应立即拿到新规则");
    assert!(ac.pin_anchor_when_start_drifts, "协议字段也要一并刷新");
    assert_eq!(ac.pid, 4242, "不得改动 pid：它同时是上一次真实焦点的身份");
    let _ = std::fs::remove_dir_all(&user);
}
