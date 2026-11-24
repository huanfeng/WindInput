//! 热重载基础：验证 ConfigBundle 能从 Config 正确重建轻量派生缓存。
//! （reload_user_config 走磁盘 IO 不在此测；这里测其核心——从配置重建派生状态。）
use super::*;

/// 方案级 `[session_actions]` 的键**必须**进 TSF 转发表。
///
/// 不进表的后果不是「优先级低」而是**服务端根本收不到这个键**——C++ 按转发表决定送不
/// 送过来。表现是「方案里配了完全没反应」，与配错了同形，用户无从分辨。
#[test]
fn schema_session_keys_enter_the_forward_table() {
    let cfg = Config::default();
    let bare = ConfigBundle::build(cfg.clone(), &Default::default());
    // `home` 是功能键（非可打印）⇒ 走 key_down + FORWARD_ONLY 那条，且不在出厂的任何
    // 键组里（page_keys = pageupdown/minus_equal、highlight_keys = arrows/tab、
    // select_key_groups = semicolon_quote）。
    let vk = keymap::session_key_name_to_vk("home").unwrap().vk;
    assert!(
        !bare
            .compiled_hotkeys
            .key_down
            .iter()
            .any(|e| e.match_hash == vk),
        "前置条件：被测键须是出厂默认未登记的，否则测不到「新增登记」这件事"
    );
    let union = SchemaKeyUnion {
        session_key_names: ["home".to_string()].into_iter().collect(),
        ..Default::default()
    };
    let with_schema = ConfigBundle::build(cfg, &union);
    assert!(
        with_schema
            .compiled_hotkeys
            .key_down
            .iter()
            .any(|e| e.match_hash == vk),
        "方案级会话态键必须进 key_down 转发表，否则服务端根本收不到"
    );
    assert!(
        with_schema.schema_session_vks.contains(&vk),
        "VK 形式也要留一份，供 capslock_bound 那类「任一方案绑过没有」的判定"
    );
}

/// ★ 去重：方案绑的键若全局已登记，不再追加第二条。
///
/// 重复条目不改变行为（`.find()` 先到先得且两条一模一样），但会让推给 C++ 的表随方案
/// 数量膨胀，也让「切方案前后推送字节不变」这条验证手段失去意义——那正是并集策略是否
/// 生效的唯一凭据。
#[test]
fn schema_session_key_already_registered_globally_is_not_duplicated() {
    let mut cfg = Config::default();
    cfg.keys
        .session_actions
        .insert("home".to_string(), "page_prev".to_string());
    let global_only = ConfigBundle::build(cfg.clone(), &Default::default());
    let union = SchemaKeyUnion {
        session_key_names: ["home".to_string()].into_iter().collect(),
        ..Default::default()
    };
    let both = ConfigBundle::build(cfg, &union);
    assert_eq!(
        both.compiled_hotkeys.key_down.len(),
        global_only.compiled_hotkeys.key_down.len(),
        "全局已登记的键，方案级不该再追加一条"
    );
}

/// 空格列掩码 = 全局表 ∪ 方案表：方案表整表替换，只看全局那份，方案里配的空格在
/// Notepad++ 这类宿主上就静默失效（t337）。
#[test]
fn space_custom_cols_unions_global_and_schema_tables() {
    let mut cfg = Config::default();
    assert_eq!(
        ConfigBundle::build(cfg.clone(), &Default::default()).space_custom_cols,
        0
    );
    cfg.input.punct.custom_enabled = true;
    cfg.input.punct.custom_mappings.insert(
        " ".into(),
        vec!["\u{3000}".into(), "".into(), "".into(), "".into()],
    );
    let union = SchemaKeyUnion {
        space_custom_cols: 0b1000,
        ..Default::default()
    };
    assert_eq!(ConfigBundle::build(cfg, &union).space_custom_cols, 0b1001);
}

#[test]
fn config_bundle_rebuilds_pairs_from_config() {
    let mut cfg = Config::default();
    cfg.input.auto_pair.chinese_pairs = vec!["（）".to_string(), "【】".to_string()];
    cfg.input.auto_pair.english_pairs = vec!["()".to_string()];
    let b = ConfigBundle::build(cfg, &Default::default());
    assert_eq!(b.cn_pairs, vec![('（', '）'), ('【', '】')]);
    assert_eq!(b.en_pairs, vec![('(', ')')]);
}

#[test]
fn parse_jump_out_keys_maps_names_to_vk() {
    // 支持的键名（大小写/空白不敏感），未知名忽略。
    let set = parse_jump_out_keys(&[
        " Tab ".into(),
        "ENTER".into(),
        "space".into(),
        "esc".into(),
        "unknown".into(),
    ]);
    assert!(set.contains(&keymap::VK_TAB));
    assert!(set.contains(&keymap::VK_RETURN)); // enter → VK_RETURN
    assert!(set.contains(&keymap::VK_SPACE));
    assert!(set.contains(&keymap::VK_ESCAPE)); // esc → VK_ESCAPE
    assert_eq!(set.len(), 4); // "unknown" 被忽略
    // "return" 别名等价 enter
    assert!(parse_jump_out_keys(&["return".into()]).contains(&keymap::VK_RETURN));
    // 空配置 → 空集（不启用）
    assert!(parse_jump_out_keys(&[]).is_empty());
}

#[test]
fn config_bundle_parses_jump_out_keys() {
    let mut cfg = Config::default();
    cfg.input.auto_pair.jump_out_keys = vec!["tab".into(), "enter".into()];
    let b = ConfigBundle::build(cfg, &Default::default());
    assert!(b.jump_out_keys.contains(&keymap::VK_TAB));
    assert!(b.jump_out_keys.contains(&keymap::VK_RETURN));
    assert_eq!(b.jump_out_keys.len(), 2);
}

#[test]
fn config_bundle_carries_config_values() {
    // 改配置 → 重建 bundle → bundle.config 反映新值（热重载替换后读取生效的基础）。
    let mut cfg = Config::default();
    cfg.input.symbol.smart_mode = true;
    cfg.ui.candidate.per_page = 9;
    let b = ConfigBundle::build(cfg, &Default::default());
    assert!(b.config.input.symbol.smart_mode);
    assert_eq!(b.config.ui.candidate.per_page, 9);
}
