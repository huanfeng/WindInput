use super::*;

/// 语法模型的平台覆盖只在取用时挑：桌面看基线（出厂关），移动端看 [mobile.grammar]
/// （出厂开随包小模型），且挑的时候不改动 Config 本身。
#[test]
fn pinyin_grammar_follows_platform_override() {
    let mut cfg = Config::default();
    let desktop = cfg.pinyin_effective(ConfigOverlay::Base).grammar;
    assert_eq!((desktop.weight, desktop.model.as_str()), (0.0, ""));
    let mobile = cfg.pinyin_effective(ConfigOverlay::Mobile).grammar;
    assert_eq!(
        (mobile.weight, mobile.model.as_str()),
        (0.5, "wanxiang-mini.gram")
    );

    cfg.mobile.grammar.weight = 0.0;
    assert_eq!(
        cfg.pinyin_effective(ConfigOverlay::Mobile).grammar.weight,
        0.0,
        "移动端可关"
    );
    assert_eq!(cfg.schema.pinyin.grammar.weight, 0.0, "基线不被覆盖值写穿");
}

/// 总开关的平台归属：移动端读 `[mobile.grammar].enabled`，不被桌面基线的 enabled 牵着走。
#[test]
fn pinyin_grammar_enabled_follows_platform_override() {
    let mut cfg = Config::default();
    cfg.schema.pinyin.grammar.weight = 0.5;
    cfg.schema.pinyin.grammar.model = "wanxiang-mini.gram".into();

    cfg.schema.pinyin.grammar.enabled = false;
    assert!(
        !cfg.pinyin_effective(ConfigOverlay::Base).grammar.active(),
        "桌面关"
    );
    assert!(
        cfg.pinyin_effective(ConfigOverlay::Mobile).grammar.active(),
        "桌面关不波及手机"
    );

    cfg.schema.pinyin.grammar.enabled = true;
    cfg.mobile.grammar.enabled = false;
    assert!(
        !cfg.pinyin_effective(ConfigOverlay::Mobile).grammar.active(),
        "手机关"
    );
    assert!(
        cfg.pinyin_effective(ConfigOverlay::Base).grammar.active(),
        "手机关不波及桌面"
    );
    assert!(cfg.schema.pinyin.grammar.enabled, "基线不被覆盖值写穿");
}

/// 真实分层：L1 默认 ⊕ L2 出厂 `data/config.toml` ⊕ 用户层，返回桌面生效的语法模型配置。
fn desktop_grammar_with_user_layer(user_toml: &str) -> PinyinGrammar {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/config.toml");
    let factory: toml::Value =
        toml::from_str(&std::fs::read_to_string(data).expect("读出厂 config.toml")).unwrap();
    let mut user: toml::Value = toml::from_str(user_toml).unwrap();
    Config::migrate_user_layer_value(&mut user);
    let mut merged = toml::Value::try_from(Config::default()).unwrap();
    merge_value(&mut merged, factory);
    merge_value(&mut merged, user);
    let cfg: Config = merged.try_into().expect("反序列化");
    cfg.pinyin_effective(ConfigOverlay::Base).grammar
}

/// 桌面出厂：开关关，模型不加载，但 weight / model 已备好。
#[test]
fn desktop_factory_grammar_is_off_but_ready() {
    let g = desktop_grammar_with_user_layer("");
    assert!(!g.enabled && !g.active());
    assert_eq!((g.weight, g.model.as_str()), (0.5, "wanxiang-mini.gram"));
}

/// 设置页勾上只写一个 enabled，就该生效。
#[test]
fn desktop_toggle_alone_activates_factory_model() {
    let g = desktop_grammar_with_user_layer("[schema.pinyin.grammar]\nenabled = true");
    assert!(g.active());
}

/// 老配置：只手写了 weight + model（没有 enabled）的桌面用户，叠在出厂 enabled=false 上
/// 也必须照旧生效——否则升级后模型被悄悄关掉。
#[test]
fn desktop_legacy_weight_and_model_stay_active_over_factory_false() {
    let g = desktop_grammar_with_user_layer(
        "[schema.pinyin.grammar]\nweight = 0.4\nmodel = \"wanxiang-lts-zh-hans.gram\"",
    );
    assert!(g.active(), "老配置被出厂 enabled=false 盖住了");
    assert_eq!(g.model, "wanxiang-lts-zh-hans.gram");
}

/// 迁移只认「两项都手写」：只写 weight 不算想开（与「不静默启用默认模型」同一原则），
/// 显式写了 enabled = false 的更不能被改回去。
#[test]
fn grammar_migration_is_conservative() {
    let only_weight = desktop_grammar_with_user_layer("[schema.pinyin.grammar]\nweight = 0.4");
    assert!(!only_weight.active());
    let explicit_off = desktop_grammar_with_user_layer(
        "[schema.pinyin.grammar]\nenabled = false\nweight = 0.4\nmodel = \"x.gram\"",
    );
    assert!(!explicit_off.active(), "显式 enabled=false 不能被迁移改回");
}

#[test]
fn codetable_wildcard_folds_from_schema_and_validates_key() {
    let g = CodetableGlobal::default();
    assert!(!g.wildcard, "出厂关闭");
    assert_eq!(g.wildcard_key, "z");
    assert_eq!(g.wildcard_char("t"), None, "关闭时恒 None，不看键");

    let spec = crate::schema::CodeTableSpec {
        wildcard: Some(true),
        wildcard_key: Some("?".into()),
        ..Default::default()
    };
    assert_eq!(g.resolved(Some(&spec)).wildcard_char("t"), Some('?'));

    let on = CodetableGlobal {
        wildcard: true,
        ..Default::default()
    };
    assert_eq!(
        on.resolved(Some(&crate::schema::CodeTableSpec::default()))
            .wildcard_char("t"),
        Some('z'),
        "方案没写 ⇒ 回落全局"
    );

    for bad in ["", "ab", "中", " ", "1", "\t"] {
        let c = CodetableGlobal {
            wildcard: true,
            wildcard_key: bad.into(),
            ..Default::default()
        };
        assert_eq!(c.wildcard_char("t"), None, "非法键 {bad:?} 应视为关闭");
    }
    assert_eq!(
        parse_wildcard_key("Z"),
        Some('z'),
        "字母归一小写：缓冲恒存小写"
    );
    assert_eq!(parse_wildcard_key("`"), Some('`'));
}

/// reverse-mode spec §2：仅单字是方案级开关，三态折叠同 `wildcard`，与主开关正交。
#[test]
fn codetable_wildcard_single_only_folds_from_schema() {
    let g = CodetableGlobal::default();
    assert!(!g.wildcard_single_only, "出厂关闭");
    let on = crate::schema::CodeTableSpec {
        wildcard_single_only: Some(true),
        ..Default::default()
    };
    assert!(
        g.resolved(Some(&on)).wildcard_single_only,
        "方案写了 ⇒ 覆盖"
    );
    let global_on = CodetableGlobal {
        wildcard_single_only: true,
        ..Default::default()
    };
    assert!(
        global_on
            .resolved(Some(&crate::schema::CodeTableSpec::default()))
            .wildcard_single_only,
        "方案没写 ⇒ 回落全局"
    );
    assert!(
        !global_on.resolved(Some(&on)).wildcard,
        "与主开关正交：开仅单字不连带开通配"
    );
}

/// 便携闸门只许挡便携这一侧：非便携下 `user_config_marker_path()` 必须仍给出路径。
///
/// 闸门误伤安装版时 `user_config_seen()` 恒 false，`probe_user_config` 会把「漫游还没挂上」
/// 当成默认用户直接按出厂配置起来；`tests/portable_no_seen_marker.rs` 只钉另一侧，拦不住它。
/// 本测试二进制不设 `WIND_INSTALL_ROOT`，`is_portable()` 恒 false，断言是确定性的。
#[test]
fn install_build_still_gets_a_marker_path() {
    assert!(
        !crate::variant::is_portable(),
        "前置：本测试二进制不该被识别成便携形态，否则下面这条恒空过"
    );
    assert!(
        Config::user_config_marker_path().is_some(),
        "便携闸门误伤了安装版：漫游仲裁会当场失效"
    );
}

/// `EmojiConfig` 的两条默认值来源（serde 缺键与 `Default::default()`）必须给出同一个结果，
/// 否则「用户没写这一段」与代码构造的空配置行为分叉。判整个结构相等，新增字段自动纳入。
#[test]
fn emoji_config_default_matches_empty_toml() {
    let from_empty: EmojiConfig = toml::from_str("").expect("空表应能反序列化");
    assert_eq!(
        from_empty,
        EmojiConfig::default(),
        "缺键时的 serde 默认值与 Default::default() 分叉了"
    );
}

/// 出厂必须是「什么都不发生」：功能关、分类表不加载、不学词频。
///
/// 与上一条互补——上一条只保证两条路径一致，一致地错了它也发现不了。
#[test]
fn emoji_config_ships_disabled() {
    let d = EmojiConfig::default();
    assert!(!d.enabled, "出厂必须关闭");
    assert!(!d.categories, "分类表干扰最大，出厂不加载");
    assert_eq!(d.max_hosts, 1, "只扩首选是控制候选序号漂移的主要手段");
}

/// 语法模型的默认值必须是「什么都不发生」：`weight = 0` 且 `model` 为空。
///
/// `model` 有默认值时，用户只调 weight 就会静默启用一个整句评测为负的模型（设计文档 §8）。
/// 要改成默认某个具体模型，先说明默认开启它为什么安全。
#[test]
fn grammar_defaults_to_disabled_and_no_model() {
    let g = PinyinGrammar::default();
    assert_eq!(g.weight, 0.0, "默认必须关闭");
    assert!(
        g.model.is_empty(),
        "默认模型必须为空，否则只调 weight 就会静默启用它，实得 {:?}",
        g.model
    );
    // 从空配置反序列化（用户没写这一段）也要落到同一处
    let from_empty: PinyinGrammar = toml::from_str("").expect("空表可反序列化");
    assert_eq!(from_empty, g, "缺省反序列化须与 Default 一致");
    assert!(!g.active(), "默认不加载模型");
}

/// 开关只做否决：老配置只写 weight + model（没有 enabled）照旧生效；
/// 开关关掉则不论 weight / model 写了什么都不加载；开着但少一项仍不加载。
#[test]
fn grammar_enabled_only_vetoes() {
    let legacy: PinyinGrammar =
        toml::from_str("weight = 0.5\nmodel = \"wanxiang-mini.gram\"").unwrap();
    assert!(
        legacy.enabled && legacy.active(),
        "老配置不写 enabled 须照旧生效"
    );

    let off: PinyinGrammar =
        toml::from_str("enabled = false\nweight = 0.5\nmodel = \"wanxiang-mini.gram\"").unwrap();
    assert!(!off.active(), "enabled=false 必须否决");

    let no_model: PinyinGrammar = toml::from_str("enabled = true\nweight = 0.5").unwrap();
    assert!(!no_model.active(), "只开开关不配模型仍不加载");
}

/// 注释库的方案限定：`schemas` 留空适用于全部方案，非空则只在列出的方案下加载。
/// 方向不能反，否则用户手写一条没写 `schemas` 就是「配了没反应」。
#[test]
fn comment_dict_schema_scoping() {
    let global = CommentDictSpec::default();
    assert!(global.applies_to("wubi86"), "留空适用于任意方案");
    assert!(global.applies_to(""), "空方案 id 也算适用");

    let scoped = CommentDictSpec {
        schemas: vec!["english".into(), "pinyin".into()],
        ..Default::default()
    };
    assert!(scoped.applies_to("english"));
    assert!(scoped.applies_to("pinyin"));
    assert!(
        !scoped.applies_to("wubi86"),
        "未列出的方案不得加载——挂大英汉词典在五笔下查是纯浪费，这正是本字段的目的"
    );
    assert!(
        !scoped.applies_to("English"),
        "方案 id 区分大小写，按精确匹配"
    );
}

#[test]
fn z_key_action_parses_value_domain() {
    assert_eq!(BoundAction::parse(""), BoundAction::None);
    assert_eq!(BoundAction::parse("none"), BoundAction::None);
    assert_eq!(BoundAction::parse(" TEMP_PINYIN "), BoundAction::TempPinyin);
    assert_eq!(BoundAction::parse("temp_english"), BoundAction::TempEnglish);
    assert_eq!(BoundAction::parse("AUX_CODE"), BoundAction::AuxCode);
    assert_eq!(
        BoundAction::parse("mix:quick_mix"),
        BoundAction::Mix("quick_mix".into())
    );
    assert_eq!(
        BoundAction::parse("special:rare"),
        BoundAction::Special("rare".into())
    );
    // 未知值绝不静默变成别的功能。
    assert_eq!(BoundAction::parse("enter_temp_pinyin"), BoundAction::None);
    assert_eq!(BoundAction::parse("quick_input"), BoundAction::None);
    // 空 id 无从定位目标，等同不启用（消费端也会被门卫挡下，此处提前收敛）。
    assert_eq!(BoundAction::parse("mix:"), BoundAction::None);
    assert_eq!(BoundAction::parse("special:  "), BoundAction::None);
    // id 大小写敏感（与 special_mode_idx / mix_mode_idx 的精确匹配同口径）。
    assert_eq!(
        BoundAction::parse("mix:Quick_Mix"),
        BoundAction::Mix("Quick_Mix".into())
    );
}

/// 出厂默认（`page_keys = [pageupdown, minus_equal]` / `highlight_keys = [arrows, tab]`）
/// 折算后必须逐键等价于旧 `NavKeys::from_config` 的产物——否则升级即回归。
#[test]
fn nav_keys_fold_into_session_actions_preserving_defaults() {
    let c = Config::default();
    let sa = c.keys.effective_session_actions();
    for (key, want) in [
        ("pageup", "page_prev"),
        ("pagedown", "page_next"),
        ("minus", "page_prev"),
        ("equal", "page_next"),
        ("up", "highlight_up"),
        ("down", "highlight_down"),
        ("tab", "highlight_down"),
        ("shift+tab", "highlight_up"),
    ] {
        assert_eq!(
            sa.get(key).map(String::as_str),
            Some(want),
            "默认绑定 {key} 折算后应为 {want}，实际 {:?}",
            sa.get(key)
        );
    }
    // 存储层必须保持原样：设置页的四个勾选框读的正是它，在 normalize 里清掉会让出厂默认
    // 在界面上全显示为未勾选。
    assert_eq!(
        c.keys.page_keys,
        default_page_keys(),
        "折算是消费层的视图，不得改写存储层"
    );
    assert_eq!(c.keys.highlight_keys, default_highlight_keys());
}

/// 出厂默认的 `select_key_groups = ["semicolon_quote"]` 折算成两条选词绑定。
#[test]
fn default_select_key_group_folds_into_session_actions() {
    let c = Config::default();
    let sa = c.keys.effective_session_actions();
    assert_eq!(
        sa.get("semicolon").map(String::as_str),
        Some("select_candidate:2"),
        "`;` 应折算成次选键"
    );
    assert_eq!(
        sa.get("quote").map(String::as_str),
        Some("select_candidate:3"),
        "`'` 应折算成三选键"
    );
    assert_eq!(
        c.keys.select_key_groups,
        default_select_key_groups(),
        "折算不得改写存储层"
    );
}

/// 修饰键选词组（走 keyup 轻敲）折算成四个纯修饰键名。这些键名必须同时被
/// `hotkey::session_key_to_vk` 与 `wind_keys::keymap::session_key_name_to_vk` 认得，
/// 后者由 wind-coordinator 的跨 crate 测试守。
#[test]
fn modifier_select_group_folds_to_modifier_key_names() {
    let mut c = Config::default();
    c.keys.select_key_groups = vec!["lrctrl".into()];
    let sa = c.keys.effective_session_actions();
    assert_eq!(
        sa.get("lctrl").map(String::as_str),
        Some("select_candidate:2")
    );
    assert_eq!(
        sa.get("rctrl").map(String::as_str),
        Some("select_candidate:3")
    );
    // 每个折算出的键名都必须解析得回来，否则绑定静默失效。
    for name in ["lctrl", "rctrl"] {
        assert!(
            crate::hotkey::session_key_to_vk(name).is_some(),
            "{name} 解析不出 VK，绑定会静默失效"
        );
    }
}

/// 以词定字组折算，含 `brackets`——它不在选词键组的值域里。
///
/// 回归点：两组曾被张冠李戴（用选词键组的解析器解以词定字配置），`brackets` 静默失效。
#[test]
fn select_char_group_folds_including_brackets() {
    let mut c = Config::default();
    c.keys.select_char_keys = vec!["brackets".into()];
    let sa = c.keys.effective_session_actions();
    assert_eq!(
        sa.get("lbracket").map(String::as_str),
        Some("select_char:1")
    );
    assert_eq!(
        sa.get("rbracket").map(String::as_str),
        Some("select_char:2")
    );
    assert_eq!(
        c.keys.select_char_keys,
        vec!["brackets".to_string()],
        "折算不得改写存储层"
    );
}

/// 撞键裁决：`comma_period` 同时配给以词定字与选词时，以词定字赢——复现主输入路径上
/// `select_char_index` 先于 `select_key_offset` 的判定顺序。
#[test]
fn select_char_wins_when_group_claimed_by_both() {
    let mut c = Config::default();
    c.keys.select_key_groups = vec!["comma_period".into()];
    c.keys.select_char_keys = vec!["comma_period".into()];
    assert_eq!(
        c.keys
            .effective_session_actions()
            .get("comma")
            .map(String::as_str),
        Some("select_char:1"),
        "撞键时以词定字应赢——它在消费链上更靠前"
    );
}

/// 序号越界 / 非数字的载荷落 `None`，不静默收下一个永不生效的绑定。
#[test]
fn select_ordinals_are_range_checked() {
    for bad in [
        "select_candidate:0",
        "select_candidate:11",
        "select_candidate:99",
        "select_candidate:x",
        "select_candidate:",
        "select_char:0",
    ] {
        assert_eq!(
            SessionAction::parse(bad),
            SessionAction::None,
            "{bad} 应被拒绝"
        );
        assert!(
            SessionAction::parse_checked(bad).is_none(),
            "{bad} 应被报成无法识别，而不是静默当成 none"
        );
    }
    assert_eq!(
        SessionAction::parse("select_candidate:2"),
        SessionAction::SelectCandidate(2)
    );
    assert_eq!(
        SessionAction::parse("select_candidate:10"),
        SessionAction::SelectCandidate(10),
        "候选窗最多开到 10 个，第 10 个候选键要选得进"
    );
    // Display 与 parse 互为逆运算——写回读不回来是「配置丢了」那类问题的根源。
    for a in [
        SessionAction::SelectCandidate(3),
        SessionAction::SelectCandidate(10),
        SessionAction::SelectChar(1),
    ] {
        assert_eq!(SessionAction::parse(&a.to_string()), a);
    }
}

/// page 组优先于 highlight 组，复现 `NavKeys::classify` 的 `.find()` 语义：两组都声明
/// `tab` 时 Tab 仍是翻页，否则用户什么都没改 Tab 就变成移高亮。
#[test]
fn nav_fold_gives_page_group_priority_over_highlight() {
    let mut c = Config::default();
    c.keys.page_keys = vec!["shift_tab".into()];
    c.keys.highlight_keys = vec!["tab".into()];
    let sa = c.keys.effective_session_actions();
    assert_eq!(
        sa.get("tab").map(String::as_str),
        Some("page_next"),
        "tab 两组都配时 page 组应赢"
    );
    assert_eq!(sa.get("shift+tab").map(String::as_str), Some("page_prev"));
}

/// 用户显式写在 `session_actions` 里的键不被折算覆盖：新配置优先于存量迁移。
/// 这是「用户要 Tab 清空、但 highlight_keys 默认还带着 tab」时唯一正确的裁决。
#[test]
fn explicit_session_action_survives_nav_fold() {
    let mut c = Config::default();
    c.keys
        .session_actions
        .insert("tab".into(), "page_next".into());
    assert_eq!(
        c.keys
            .effective_session_actions()
            .get("tab")
            .map(String::as_str),
        Some("page_next"),
        "显式配置应压过默认 highlight_keys 折算出的 highlight_down"
    );
}

/// 用户把 `page_keys` 清成 `[]` 的意图不能丢。
///
/// 这正是默认值必须留在被折算一侧的理由：出厂绑定若直接写进 `session_actions`，空数组就与
/// 「从没配过」同形，用户关掉的翻页键重启后又回来。
#[test]
fn cleared_page_keys_yield_no_paging_binds() {
    let mut c = Config::default();
    c.keys.page_keys = vec![];
    c.keys.highlight_keys = vec![];
    // 选词键组默认非空（semicolon_quote），一并清掉才测得出「清空即无绑定」。
    c.keys.select_key_groups = vec![];
    c.keys.select_char_keys = vec![];
    let sa = c.keys.effective_session_actions();
    assert!(sa.is_empty(), "全部清空时不应折算出任何绑定，实际 {sa:?}");
}

/// `normalize` 不得碰这两处——折算是消费层的视图，不是存储层的改写。
///
/// 在 normalize 里折算并清空原字段会让设置页读到的四项恒为空、删掉的折算绑定下次又被
/// 折算回来。判据见 `effective_session_actions` 的文档。
#[test]
fn normalize_does_not_rewrite_key_group_storage() {
    let mut c = Config::default();
    let before_groups = (
        c.keys.page_keys.clone(),
        c.keys.highlight_keys.clone(),
        c.keys.select_key_groups.clone(),
        c.keys.select_char_keys.clone(),
    );
    let before_explicit = c.keys.session_actions.clone();
    c.normalize();
    assert_eq!(
        (
            c.keys.page_keys.clone(),
            c.keys.highlight_keys.clone(),
            c.keys.select_key_groups.clone(),
            c.keys.select_char_keys.clone(),
        ),
        before_groups,
        "四组键组配置是存储层，normalize 不得清空"
    );
    assert_eq!(
        c.keys.session_actions, before_explicit,
        "session_actions 只存用户显式配的，normalize 不得往里塞折算结果——             否则高级表里会冒出一堆用户没配过的绑定，且删了下次又回来"
    );
}

/// 有效视图是纯函数：多次调用结果一致，且与调用前后的 normalize 无关。
/// 注释切换键选功能键时补进会话按键表；按档位选动词，切换键为无、注释关着或用户自己绑了这个键就不补。
#[test]
fn function_switch_key_joins_session_actions() {
    use crate::comment_lib::{SwitchKey, SwitchMode};
    let f1 = |c: &Config| c.effective_session_actions().get("f1").cloned();
    let mut c = Config::default();
    assert_eq!(c.ui.comment.switch_key, SwitchKey::F1);
    assert_eq!(f1(&c).as_deref(), Some("comment_state:session"));
    c.ui.comment.switch_mode = SwitchMode::Hold;
    assert_eq!(f1(&c).as_deref(), Some("comment_state:session"));
    c.ui.comment.switch_mode = SwitchMode::Persist;
    assert_eq!(f1(&c).as_deref(), Some("comment_state"));
    c.ui.comment.switch_mode = SwitchMode::Session;
    c.ui.comment.switch_key = SwitchKey::None;
    assert_eq!(f1(&c), None);
    c.ui.comment.switch_key = SwitchKey::F5;
    assert_eq!(f1(&c), None);
    assert_eq!(
        c.effective_session_actions().get("f5").map(String::as_str),
        Some("comment_state:session")
    );
    c.ui.comment.switch_key = SwitchKey::F1;
    c.keys.session_actions.insert("f1".into(), "none".into());
    assert_eq!(f1(&c).as_deref(), Some("none"), "用户的绑定优先");
    c.keys.session_actions.clear();
    c.ui.comment.switch_key = SwitchKey::Ctrl;
    assert_eq!(f1(&c), None);
    c.ui.comment.switch_key = SwitchKey::F1;
    c.ui.comment.enabled = false;
    assert_eq!(f1(&c), None);
}

#[test]
fn effective_session_actions_is_pure() {
    let mut c = Config::default();
    let a = c.keys.effective_session_actions();
    c.normalize();
    let b = c.keys.effective_session_actions();
    assert_eq!(a, b);
}

/// 组名展开表里的键名是跨 crate 拼写契约（`wind-keys` 与 `hotkey.rs` 各有一份解析）。
/// 这里只守「本 crate 认得」这一半，写错表现为翻页键静默失效；两份解析表一致由
/// `wind-coordinator` 的跨 crate 测试守。
#[test]
fn nav_group_names_resolve() {
    for g in [
        "pageupdown",
        "minus_equal",
        "brackets",
        "comma_period",
        "shift_tab",
    ] {
        assert_eq!(
            page_key_group_binds(g).len(),
            2,
            "page 组 {g} 应展开出两个键"
        );
    }
    for g in ["arrows", "tab"] {
        assert_eq!(
            highlight_key_group_binds(g).len(),
            2,
            "highlight 组 {g} 应展开出两个键"
        );
    }
    assert!(page_key_group_binds("nonexistent").is_empty());
    // 每个折算出的动词都必须解析得回来——写错动词字面量会静默落 None。
    for (_, a) in page_key_group_binds("shift_tab") {
        assert!(SessionAction::parse(&a.to_string()).is_enabled());
    }
}

/// `clear` 是 `cancel` 的别名，内核只有一种行为，回写只用规范名 `cancel`，
/// 避免同一份配置两次保存后出现两种写法。
#[test]
fn clear_parses_as_cancel_alias() {
    assert_eq!(SessionAction::parse("cancel"), SessionAction::Cancel);
    assert_eq!(SessionAction::parse("clear"), SessionAction::Cancel);
    assert_eq!(SessionAction::parse("CLEAR"), SessionAction::Cancel);
    assert_eq!(SessionAction::Cancel.to_string(), "cancel");
    // 别名也要能通过「值认不认识」的校验，否则加载期会误报成拼写错误。
    assert_eq!(
        SessionAction::parse_checked("clear"),
        Some(SessionAction::Cancel)
    );
}

/// 「要不要有候选」的判据挂在动作上，不在消费点。
///
/// 导航类没有候选时无事可做；`cancel` 在「打了码还没出候选」（如网址模式）时恰恰必须生效。
/// 判据放到消费点上就要维护多份一致的守卫。
#[test]
fn only_navigation_actions_require_candidates() {
    for a in [
        SessionAction::PagePrev(PageOverflow::Global),
        SessionAction::PageNext(PageOverflow::Global),
        SessionAction::HighlightUp,
        SessionAction::HighlightDown,
    ] {
        assert!(a.requires_candidates(), "{a:?} 是导航类，无候选时无事可做");
    }
    assert!(
        !SessionAction::Cancel.requires_candidates(),
        "cancel 在有编码无候选时必须生效，否则网址模式里按了没反应"
    );
}

/// `commit_highlighted`：解析与回写互逆，且要候选——没有高亮可上屏时按键回落原语义。
#[test]
fn commit_highlighted_round_trips_and_requires_candidates() {
    let a = SessionAction::parse(" Commit_Highlighted ");
    assert_eq!(a, SessionAction::CommitHighlighted);
    assert_eq!(
        SessionAction::parse(&a.to_string()),
        a,
        "Display 与 parse 须互逆"
    );
    assert_eq!(
        SessionAction::parse_checked("commit_highlighted"),
        Some(SessionAction::CommitHighlighted)
    );
    assert!(a.requires_candidates(), "无候选时无高亮可上屏，须放行按键");
}

/// `commit_code`：解析与回写互逆，且不要候选——空码上屏原码正是它的用处。
#[test]
fn commit_code_round_trips_and_works_without_candidates() {
    let a = SessionAction::parse(" Commit_Code ");
    assert_eq!(a, SessionAction::CommitCode);
    assert_eq!(
        SessionAction::parse(&a.to_string()),
        a,
        "Display 与 parse 须互逆"
    );
    assert!(!a.requires_candidates(), "空码无候选时也要能上屏原码");
}

/// `command:<表达式>`：两张表同一套解析，表达式原样保留大小写与内部空白。
///
/// `SessionAction::parse` 一上来就整串小写化，command 必须在那之前分出来，否则网址与
/// `key.tap("Ctrl+Shift+P")` 会被改坏；`parse_checked` 也不能把小写化的副本传给 `parse`。
#[test]
fn command_verb_keeps_expression_verbatim_in_both_tables() {
    let raw = r#"  Command:proc.run("C:\\Tools\\Char Map.exe")  "#;
    let want = r#"proc.run("C:\\Tools\\Char Map.exe")"#.to_string();
    assert_eq!(BoundAction::parse(raw), BoundAction::Command(want.clone()));
    assert_eq!(
        SessionAction::parse(raw),
        SessionAction::Command(want.clone())
    );
    assert_eq!(
        SessionAction::parse_checked(raw),
        Some(SessionAction::Command(want.clone())),
        "parse_checked 不得在交给 parse 之前小写化"
    );
    let a = SessionAction::Command(want);
    assert_eq!(
        SessionAction::parse(&a.to_string()),
        a,
        "Display 与 parse 须互逆"
    );

    // 网址区分大小写，且内部连续空白要原样保留（设置端核对项）。
    let url = r#"command:open("https://Example.com/A  B")"#;
    let want = r#"open("https://Example.com/A  B")"#.to_string();
    assert_eq!(BoundAction::parse(url), BoundAction::Command(want.clone()));
    assert_eq!(
        SessionAction::parse(url),
        SessionAction::Command(want.clone())
    );
    assert_eq!(
        SessionAction::parse_checked(url),
        Some(SessionAction::Command(want))
    );
}

/// 表达式为空不是合法绑定：落 `None`，`parse_checked` 报「不认识」供加载期告警。
#[test]
fn command_verb_with_empty_expression_is_rejected() {
    for bad in ["command:", "command:   ", " COMMAND: "] {
        assert_eq!(BoundAction::parse(bad), BoundAction::None, "{bad:?}");
        assert_eq!(SessionAction::parse(bad), SessionAction::None, "{bad:?}");
        assert_eq!(SessionAction::parse_checked(bad), None, "{bad:?}");
    }
}

/// 命令与中英态无关、不限修饰键、会话态下不要候选。
#[test]
fn command_verb_policy() {
    let b = BoundAction::parse(r#"command:open("https://example.com")"#);
    assert!(!b.only_in_chinese_mode(), "英文态下也该按得动");
    assert!(!b.requires_modifier_key(), "有字符的键上也能绑");
    let s = SessionAction::parse(r#"command:open("https://example.com")"#);
    assert!(!s.requires_candidates(), "打了码没候选时也能按");
}

/// 存量迁移：`trigger_keys` 里的 z → `z_key_action`，其余字母丢弃。
/// 不迁移的话解析端的 `filter_map` 会无声吃掉字母，功能静默失效。
#[test]
fn migrate_letter_trigger_keys_moves_z_to_action() {
    let mut c = Config::default();
    c.input.temp_pinyin.trigger_keys = vec!["backtick".into(), "z".into(), "q".into()];
    c.normalize();

    assert_eq!(
        c.schema.codetable.z_key_action, "temp_pinyin",
        "z 应折算成 z_key_action"
    );
    // 字母项被本步摘除；符号项随后由五c 的收编迁移移进 keys.key_actions
    // （`normalize` 里两步依次跑）。断言它去了哪而不是「还在原地」——
    // 后者在收编后是个必然失败的观察点，但字母摘除这条契约本身没变。
    assert!(
        c.input.temp_pinyin.trigger_keys.is_empty(),
        "符号项应已被收编迁移取走"
    );
    assert_eq!(
        c.keys.key_actions.get("backtick").map(String::as_str),
        Some("temp_pinyin"),
        "符号引导键应完整折算进 keys.key_actions，不能在两步之间丢失"
    );
    assert!(
        !c.keys.key_actions.contains_key("q"),
        "被摘除的字母不该跟着进新表"
    );
}

/// 归属优先级与模式激活链一致：临拼 > mix（`schema.special_modes` 已废弃，z 进 overlay
/// 方案改写 `z_key_action = "special:<方案id>"`）。
#[test]
fn migrate_letter_trigger_keys_follows_activation_priority() {
    let mut c = Config::default();
    c.input.temp_pinyin.trigger_keys = vec!["z".into()];
    c.schema.mix_modes = vec![MixModeConfig {
        id: "mx".into(),
        trigger_keys: vec!["z".into()],
        ..Default::default()
    }];
    c.normalize();

    assert_eq!(
        c.schema.codetable.z_key_action, "temp_pinyin",
        "同时配在多处时按激活链取临拼（老实现也是临拼先匹配）"
    );
    assert!(
        c.schema.mix_modes[0].trigger_keys.is_empty(),
        "未中选的字母项同样要摘除，否则留在配置里也是静默失效"
    );
}

/// 三处 `trigger_keys` 折算进 `keys.key_actions`（`schema.special_modes[]` 已废弃，
/// 不再是来源）。
#[test]
fn migrate_trigger_keys_folds_all_four_sources() {
    let mut c = Config::default();
    c.input.temp_pinyin.trigger_keys = vec!["backtick".into()];
    c.input.temp_english.trigger_keys = vec!["quote".into()];
    c.schema.mix_modes = vec![MixModeConfig {
        id: "quick_mix".into(),
        trigger_keys: vec!["semicolon".into()],
        ..Default::default()
    }];
    c.normalize();

    let ka = &c.keys.key_actions;
    assert_eq!(ka.get("backtick").map(String::as_str), Some("temp_pinyin"));
    assert_eq!(ka.get("quote").map(String::as_str), Some("temp_english"));
    assert_eq!(
        ka.get("semicolon").map(String::as_str),
        Some("mix:quick_mix")
    );

    // 旧字段清空——留着也是静默失效，而用户会以为它还生效。
    assert!(c.input.temp_pinyin.trigger_keys.is_empty());
    assert!(c.input.temp_english.trigger_keys.is_empty());
    assert!(c.schema.mix_modes[0].trigger_keys.is_empty());
}

/// 争用同一个键时，归属必须复现 `try_activate_mode` 的真实调用顺序：
/// 临英 > 临拼 > special > mix。以代码为准，不以设计文档为准；
/// 顺序错了用户升级后会进错模式。
#[test]
fn migrate_trigger_keys_follows_real_activation_order() {
    let mut c = Config::default();
    // 同一个 `;` 配给四处，按真实顺序应归临英。
    c.input.temp_english.trigger_keys = vec!["semicolon".into()];
    c.input.temp_pinyin.trigger_keys = vec!["semicolon".into()];
    c.schema.mix_modes = vec![MixModeConfig {
        id: "mx".into(),
        trigger_keys: vec!["semicolon".into()],
        ..Default::default()
    }];
    c.normalize();
    assert_eq!(
        c.keys.key_actions.get("semicolon").map(String::as_str),
        Some("temp_english"),
        "临英在激活链最前，应赢下争用"
    );

    // 单独验临拼 > mix：去掉临英那一处。
    let mut c2 = Config::default();
    c2.input.temp_pinyin.trigger_keys = vec!["grave".into()];
    c2.schema.mix_modes = vec![MixModeConfig {
        id: "mx".into(),
        trigger_keys: vec!["grave".into()],
        ..Default::default()
    }];
    c2.normalize();
    assert_eq!(
        c2.keys.key_actions.get("grave").map(String::as_str),
        Some("temp_pinyin"),
        "临拼排在 mix 之前"
    );
}

/// 用户已在 `keys.key_actions` 显式配过的键不被存量迁移覆盖。
#[test]
fn migrate_trigger_keys_does_not_override_explicit() {
    let mut c = Config::default();
    c.keys
        .key_actions
        .insert("backtick".into(), "temp_english".into());
    c.input.temp_pinyin.trigger_keys = vec!["backtick".into()];
    c.normalize();
    assert_eq!(
        c.keys.key_actions.get("backtick").map(String::as_str),
        Some("temp_english"),
        "用户的新配置优先于存量迁移"
    );
    // 旧字段仍要清空：它已经不可能生效了。
    assert!(c.input.temp_pinyin.trigger_keys.is_empty());
}

/// 空串项与解析不了的键名都不该进新表——前者是历史占位，后者是脏数据。
#[test]
fn migrate_trigger_keys_skips_empty_and_unparsable() {
    let mut c = Config::default();
    c.schema.mix_modes = vec![MixModeConfig {
        id: "mx".into(),
        trigger_keys: vec!["".into(), "  ".into(), "根本不是键".into()],
        ..Default::default()
    }];
    c.normalize();
    assert!(
        !c.keys.key_actions.values().any(|v| v == "mix:mx"),
        "空串/无法解析的键名不该进表，实际 {:?}",
        c.keys.key_actions
    );
}

/// 残留的 `schema.special_modes` 能被读出来（供告警），且 `normalize` 后清空。
///
/// 字段若直接删掉，serde 会静默丢弃整段，用户以为仍然生效；故用 `Vec<toml::Value>`
/// 接住，只为发一条 warn。
#[test]
fn legacy_special_modes_is_read_then_cleared_with_warning() {
    let toml_src = r#"
[schema]
active = "wubi86"

[[schema.special_modes]]
id = "kf"
schema = "kf"
show_all_on_enter = true
"#;
    let mut c: Config = toml::from_str(toml_src).expect("含废弃段的配置仍须解析成功");
    assert_eq!(
        c.schema.legacy_special_modes.len(),
        1,
        "废弃段必须读得出来，否则无法告警"
    );
    c.normalize();
    assert!(
        c.schema.legacy_special_modes.is_empty(),
        "告警后须清空——留着会让后续「有没有配过」的判断读到假信号"
    );
    // 且不做任何迁移：不得凭空往 key_actions 里塞绑定（见设计文档 §5）。
    assert!(
        !c.keys
            .key_actions
            .values()
            .any(|v| v.starts_with("special:")),
        "本轮刻意不迁移，实际 {:?}",
        c.keys.key_actions
    );
}

/// 废弃字段不再写出：`skip_serializing_if` 保证保存后 config.toml 里不会又冒出来。
#[test]
fn legacy_special_modes_is_not_serialized_back() {
    let c = Config::default();
    let out = toml::to_string(&c).expect("序列化");
    assert!(!out.contains("special_modes"), "废弃字段不该写回配置文件");
}

/// 已显式配过 `z_key_action` 时，存量迁移不得覆盖用户的新配置。
#[test]
fn migrate_letter_trigger_keys_does_not_override_explicit() {
    let mut c = Config::default();
    c.schema.codetable.z_key_action = "temp_english".into();
    c.input.temp_pinyin.trigger_keys = vec!["z".into()];
    c.normalize();

    assert_eq!(
        c.schema.codetable.z_key_action, "temp_english",
        "显式配置优先于存量迁移"
    );
    assert!(
        c.input.temp_pinyin.trigger_keys.is_empty(),
        "旧字母项无论是否中选都要摘除"
    );
}

/// 内置「快捷」默认成员用占位符，使快捷输入的拼音跟随主拼音方案（而非恒为全拼）。
#[test]
fn quick_mix_default_members_use_primary_pinyin_placeholder() {
    let modes = default_mix_modes();
    let quick = modes
        .iter()
        .find(|m| m.id == QUICK_MIX_ID)
        .expect("应有内置 quick_mix");
    assert!(
        quick
            .members
            .contains(&MIX_MEMBER_PRIMARY_PINYIN.to_string()),
        "默认成员应为占位符，实际 {:?}",
        quick.members
    );
    assert!(
        !quick.members.contains(&DEFAULT_PINYIN_SCHEMA.to_string()),
        "不应再硬编码字面 pinyin，实际 {:?}",
        quick.members
    );
}

/// 存量迁移：改过「快捷输入激活键」的用户配置里，members 被整体写回为字面 pinyin，
/// 加载期须迁成占位符，否则这些用户的快捷输入永远是全拼。
#[test]
fn normalize_migrates_quick_mix_literal_pinyin() {
    let mut cfg = Config::default();
    cfg.schema.mix_modes = vec![
        MixModeConfig {
            id: QUICK_MIX_ID.to_string(),
            members: vec![
                "quick_input".to_string(),
                "pinyin".to_string(),
                "english".to_string(),
            ],
            ..Default::default()
        },
        // 用户自定义 mix：字面 id 精确解释，不迁移。
        MixModeConfig {
            id: "my_mix".to_string(),
            members: vec!["pinyin".to_string()],
            ..Default::default()
        },
    ];
    cfg.normalize();
    assert!(
        cfg.schema.mix_modes[0]
            .members
            .contains(&MIX_MEMBER_PRIMARY_PINYIN.to_string()),
        "内置 quick_mix 的字面 pinyin 应迁为占位符，实际 {:?}",
        cfg.schema.mix_modes[0].members
    );
    assert_eq!(
        cfg.schema.mix_modes[1].members,
        vec!["pinyin"],
        "自定义 mix 的字面 pinyin 应原样保留"
    );
}

/// 在合并值里塞一个存量用户配置残留的 `schema.quick_input.force_vertical`。
fn merged_with_force_vertical(v: Option<bool>) -> toml::Value {
    let mut merged = toml::Value::try_from(Config::default()).expect("默认配置应可序列化");
    if let Some(v) = v {
        merged
            .get_mut("schema")
            .and_then(|s| s.get_mut("quick_input"))
            .and_then(|q| q.as_table_mut())
            .expect("schema.quick_input 应存在")
            .insert("force_vertical".to_string(), toml::Value::Boolean(v));
    }
    merged
}

fn quick_mix_layout(merged: toml::Value) -> LayoutIntent {
    let cfg: Config = merged.try_into().expect("迁移后应可反序列化");
    cfg.schema
        .mix_modes
        .iter()
        .find(|m| m.id == QUICK_MIX_ID)
        .expect("内置 quick_mix 应存在")
        .candidate_layout
}

/// 废弃键 `force_vertical` → `mix_modes[quick_mix].candidate_layout`。
///
/// 映射刻意不对称：`false` 迁成 Follow 而非 Horizontal。旧 false 语义是「不强制」，
/// 迁成 Horizontal 会把全局设成竖排的用户钉在横排上。
#[test]
fn force_vertical_migrates_into_quick_mix_candidate_layout() {
    for (old, want) in [
        (true, LayoutIntent::Vertical),
        (false, LayoutIntent::Follow),
    ] {
        let mut merged = merged_with_force_vertical(Some(old));
        Config::migrate_force_vertical_value(&mut merged);
        assert_eq!(
            quick_mix_layout(merged),
            want,
            "force_vertical={old} 应迁为 {want:?}"
        );
    }
}

/// 旧键缺席（全新安装 / 新版预置文件已删该行）时不动，保留出厂竖排。
/// 守的是「未改过配置的用户升级后行为不变」。
#[test]
fn absent_force_vertical_keeps_factory_vertical() {
    let mut merged = merged_with_force_vertical(None);
    Config::migrate_force_vertical_value(&mut merged);
    assert_eq!(
        quick_mix_layout(merged),
        LayoutIntent::Vertical,
        "无旧键时应保留 default_mix_modes() 的出厂竖排"
    );
}

/// 存量迁移：合并成员 `quick_input` 就地展开为四个细分来源，其余成员的相对序不变。
#[test]
fn normalize_expands_legacy_quick_input_member() {
    let mut cfg = Config::default();
    cfg.schema.mix_modes = vec![MixModeConfig {
        id: QUICK_MIX_ID.to_string(),
        members: vec![
            "quick_input".to_string(),
            MIX_MEMBER_PRIMARY_PINYIN.to_string(),
            "english".to_string(),
        ],
        ..Default::default()
    }];
    cfg.normalize();
    let mut expected: Vec<String> = wind_quick_input::LEGACY_EXPANSION
        .iter()
        .map(|s| s.to_string())
        .collect();
    expected.push(MIX_MEMBER_PRIMARY_PINYIN.to_string());
    expected.push("english".to_string());
    assert_eq!(
        cfg.schema.mix_modes[0].members, expected,
        "旧值应在原位展开，展开序 = 默认成员序"
    );
    // 幂等：再跑一次不重复展开
    let once = cfg.schema.mix_modes[0].members.clone();
    cfg.normalize();
    assert_eq!(cfg.schema.mix_modes[0].members, once, "迁移应幂等");
}

/// 展开时跳过用户已单独写出的细分来源，不产生重复成员。
#[test]
fn legacy_expansion_skips_already_present_sources() {
    let mut cfg = Config::default();
    cfg.schema.mix_modes = vec![MixModeConfig {
        id: QUICK_MIX_ID.to_string(),
        members: vec![
            wind_quick_input::MEMBER_NUMBER.to_string(),
            "quick_input".to_string(),
        ],
        ..Default::default()
    }];
    cfg.normalize();
    let m = &cfg.schema.mix_modes[0].members;
    assert_eq!(
        m.iter()
            .filter(|s| *s == wind_quick_input::MEMBER_NUMBER)
            .count(),
        1,
        "细分来源不应重复，实际 {:?}",
        m
    );
    assert_eq!(
        m,
        &vec![
            wind_quick_input::MEMBER_NUMBER.to_string(),
            wind_quick_input::MEMBER_CALC.to_string(),
            wind_quick_input::MEMBER_DATE.to_string(),
            wind_quick_input::MEMBER_REPEAT.to_string(),
        ],
        "用户显式写出的来源保持其位置"
    );
}

/// 存量迁移：废弃键 `enable_english = false` 落成 members 里的 english 删除。
/// 该迁移在反序列化前作用于 TOML 值，故直接验证 `migrate_enable_english_value`。
#[test]
fn migrates_disabled_enable_english_into_members() {
    let mut v = toml::Value::try_from(Config::default()).unwrap();
    // 模拟存量用户配置：关掉了英文候选
    v.get_mut("schema")
        .unwrap()
        .get_mut("quick_input")
        .unwrap()
        .as_table_mut()
        .unwrap()
        .insert("enable_english".to_string(), toml::Value::Boolean(false));
    Config::migrate_enable_english_value(&mut v);
    let cfg: Config = v.try_into().unwrap();
    assert!(
        !cfg.schema.mix_modes[0]
            .members
            .contains(&"english".to_string()),
        "关过英文候选的存量用户，升级后英文不应冒回来：{:?}",
        cfg.schema.mix_modes[0].members
    );
    assert!(
        cfg.schema.mix_modes[0]
            .members
            .contains(&MIX_MEMBER_PRIMARY_PINYIN.to_string()),
        "只应移除 english，其余成员不动"
    );
}

/// 默认值（enable_english 缺省或为 true）不触发迁移。
#[test]
fn default_keeps_english_member() {
    let mut v = toml::Value::try_from(Config::default()).unwrap();
    Config::migrate_enable_english_value(&mut v);
    let cfg: Config = v.try_into().unwrap();
    assert!(
        cfg.schema.mix_modes[0]
            .members
            .contains(&"english".to_string()),
        "无废弃键时 english 成员应保留"
    );
}

/// input.default 新增项与既有语义：state_scope 默认 global、remember 默认 false。
#[test]
fn input_default_state_scope_defaults() {
    let d = InputDefaultConfig::default();
    assert_eq!(d.state_scope, "global");
    assert!(!d.per_app_scope());
    assert!(!d.remember_last_state);
    // 缺字段的旧 config.toml 反序列化与 Default 一致。
    let parsed: InputDefaultConfig = toml::from_str("").unwrap();
    assert_eq!(parsed.state_scope, "global");
    assert!(!parsed.remember_last_state);
    assert!(parsed.chinese_mode);
    // scope 解析大小写不敏感。
    let parsed: InputDefaultConfig = toml::from_str("state_scope = \"App\"").unwrap();
    assert!(parsed.per_app_scope());
}

/// `is_settled` 的语义边界：只有「再等也不会变」的两态算就绪。
/// 尤其 `Ready { file_exists: false }` 必须算就绪——全新安装本就没有
/// config.toml（只在用户首次改设置时创建），把它当未就绪会让每个新用户白等整个超时。
#[test]
fn probe_settled_semantics() {
    let dir = PathBuf::from("x");
    assert!(UserConfigProbe::Portable(dir.clone()).is_settled());
    // 自定义数据目录是本机固定盘上的普通目录，不存在「等漫游挂载」一说；
    // 漏判会让每次启动都白等一个完整超时，然后退回系统预置方案。
    assert!(UserConfigProbe::CustomDir(dir.clone()).is_settled());
    assert!(
        UserConfigProbe::Ready {
            dir: dir.clone(),
            dir_exists: true,
            file_exists: true,
        }
        .is_settled()
    );
    assert!(
        UserConfigProbe::Ready {
            dir: dir.clone(),
            dir_exists: false,
            file_exists: false,
        }
        .is_settled(),
        "漫游根就绪后，配置在不在是确定性事实，不该继续等待"
    );
    assert!(
        !UserConfigProbe::ConfigPending { dir: dir.clone() }.is_settled(),
        "本地标记说该用户本有 config.toml，但此刻看不到 → 竞态，须继续等而非就绪"
    );
    // 这两态才是「系统尚未就绪」，等待有意义。
    assert!(!UserConfigProbe::RoamingUnavailable.is_settled());
    assert!(!UserConfigProbe::RoamingMissing(dir).is_settled());
}

/// 等待的返回值必须与探测结论一致，且未就绪时不得超出 timeout 太多
/// （防止把服务启动无限期卡住——超时后要降级继续启动，不是死等）。
#[test]
fn wait_respects_probe_and_timeout() {
    let settled = Config::probe_user_config().is_settled();
    let start = std::time::Instant::now();
    let ready = Config::wait_user_config_ready(std::time::Duration::from_millis(50));
    let elapsed = start.elapsed();

    assert_eq!(ready, settled, "返回值应与探测结论一致");
    if settled {
        // 开发机/CI 上漫游根通常存在：必须立即返回，一次 sleep 都不能有。
        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "已就绪却等待了 {elapsed:?}"
        );
    } else {
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "超时后应降级返回而非死等，实际 {elapsed:?}"
        );
    }
}

/// 重试路径：前几次未就绪，之后转就绪 → 必须等到就绪再返回 true。
/// 这是本修复的核心分支，开发机上探测恒就绪走不到，只能靠注入。
#[test]
fn wait_retries_until_ready() {
    let mut calls = 0u32;
    let ready = Config::wait_until_settled(
        || {
            calls += 1;
            if calls < 3 {
                UserConfigProbe::RoamingUnavailable
            } else {
                UserConfigProbe::Ready {
                    dir: PathBuf::from("x"),
                    dir_exists: true,
                    file_exists: true,
                }
            }
        },
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(1),
    );
    assert!(ready, "转为就绪后应返回 true");
    assert_eq!(calls, 3, "应恰好重试到就绪那次为止");
}

/// 始终未就绪 → 必须在 timeout 后降级返回 false，而不是死等把服务卡住。
#[test]
fn wait_gives_up_after_timeout() {
    let mut calls = 0u32;
    let start = std::time::Instant::now();
    let ready = Config::wait_until_settled(
        || {
            calls += 1;
            UserConfigProbe::RoamingMissing(PathBuf::from("x"))
        },
        std::time::Duration::from_millis(60),
        std::time::Duration::from_millis(10),
    );
    assert!(!ready, "始终未就绪应返回 false");
    assert!(calls > 1, "应至少重试过，实际 {calls} 次");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "应及时放弃，实际 {:?}",
        start.elapsed()
    );
}

fn tv(s: &str) -> toml::Value {
    toml::from_str::<toml::Value>(s).expect("测试 TOML 应可解析")
}

/// 出厂默认（L1⊕L2⊕L2.5）样本：覆盖标量 / 数组 / 多级嵌套。
fn preset_sample() -> toml::Value {
    tv(r#"
[schema]
active = "wubi86"

[schema.mix]
auto_commit_block_on_pinyin = true
pinyin_only_overflow = true
show_source_hint = false

[schema.codetable]
top_code_commit = true

[keys]
page_keys = ["pageupdown", "minus_equal"]
"#)
}

#[test]
fn prune_removes_redundant_keeps_overrides() {
    let preset = preset_sample();
    let mut user = tv(r#"
[schema]
active = "wubi86_pinyin"

[schema.mix]
auto_commit_block_on_pinyin = false
pinyin_only_overflow = true
show_source_hint = false

[keys]
page_keys = ["pageupdown", "minus_equal"]
"#);
    let removed = prune_redundant(&mut user, &preset);
    assert_eq!(
        removed, 3,
        "pinyin_only_overflow / show_source_hint / page_keys 三个与默认相同"
    );
    assert_eq!(
        get_nested(&user, &["schema", "mix", "auto_commit_block_on_pinyin"]),
        Some(&toml::Value::Boolean(false)),
        "真实覆盖（与默认相反）必须保留"
    );
    assert_eq!(
        get_nested(&user, &["schema", "active"]).and_then(|v| v.as_str()),
        Some("wubi86_pinyin"),
        "真实覆盖必须保留"
    );
    assert!(
        get_nested(&user, &["keys"]).is_none(),
        "唯一子键被删后，空的 [keys] 段应一并回收"
    );
}

#[test]
fn prune_preserves_merged_result() {
    // 本轮修复的核心保证：被删的键都会从 L1⊕L2⊕L2.5 回落到同一个值，故清理对当前行为
    // 零影响，只影响将来默认值变更能否到达该用户。这条不变量成立，清理才是安全的。
    let preset = preset_sample();
    let user = tv(r#"
[schema]
active = "wubi86_pinyin"

[schema.mix]
auto_commit_block_on_pinyin = false
pinyin_only_overflow = true
show_source_hint = false

[schema.codetable]
top_code_commit = true

[keys]
page_keys = ["pageupdown", "minus_equal"]

[input.punct.custom_mappings]
"'1" = ["1", "＇"]
"#);
    let mut before = preset.clone();
    merge_value(&mut before, user.clone());

    let mut pruned = user.clone();
    let removed = prune_redundant(&mut pruned, &preset);
    assert!(removed > 0, "样本应含冗余键，否则本测试证明不了任何事");
    let mut after = preset.clone();
    merge_value(&mut after, pruned);

    assert_eq!(before, after, "清理前后合并结果必须逐键相同");
}

#[test]
fn prune_is_idempotent() {
    let preset = preset_sample();
    let mut user = tv(r#"
[schema.mix]
pinyin_only_overflow = true
show_source_hint = false
"#);
    assert_eq!(prune_redundant(&mut user, &preset), 2);
    assert_eq!(prune_redundant(&mut user, &preset), 0, "二次清理应无事可做");
}

#[test]
fn prune_keeps_keys_absent_from_preset() {
    // 出厂默认里没有的键一律保留：用户自定义标点映射这类动态键（键名由用户输入决定，
    // 不可能出现在 preset 里）若被当成冗余删掉就是丢用户数据。废弃键的清理是另一件事，
    // 必须走显式名单，绝不能靠「preset 里没有」来推断。
    let preset = preset_sample();
    let mut user = tv(r#"
[input.punct.custom_mappings]
"'1" = ["1", "＇"]
"#);
    assert_eq!(prune_redundant(&mut user, &preset), 0);
    assert!(get_nested(&user, &["input", "punct", "custom_mappings", "'1"]).is_some());
}

#[test]
fn prune_keeps_unregistered_keys_even_when_matching_preset() {
    // 注册表未登记的键即使与 preset 完全相同也不得删——此处用真实存在过的废弃键
    // `input.code_commit.*`（已迁到 schema.codetable.*，注册表里查不到）。
    // 废弃键清理是另一件事，必须走显式名单：靠「等于 preset」去推断会把语义搞反。
    assert!(
        !crate::config_schema::is_known_key("input.code_commit.auto_commit_at_full"),
        "前提：该键确未登记，否则本测试证明不了 registry 这道保险"
    );
    let preset = tv(r#"
[input.code_commit]
auto_commit_at_full = false
"#);
    let mut user = preset.clone();
    assert_eq!(prune_redundant(&mut user, &preset), 0);
    assert!(
        get_nested(&user, &["input", "code_commit", "auto_commit_at_full"]).is_some(),
        "未登记键必须原样保留"
    );
}

// ── 引导键物化（`Config::materialize_key_actions`）──

#[test]
fn materialize_into_writes_bindings_and_drops_legacy_fields() {
    let mut root = tv(r#"
[input.temp_pinyin]
enabled = true
trigger_keys = ["backtick"]

[input.temp_english]
trigger_keys = ["semicolon"]

[[schema.mix_modes]]
id = "quick_mix"
members = ["date"]
trigger_keys = ["backslash"]
"#);
    let bindings = BTreeMap::from([
        ("backtick".to_string(), "temp_pinyin".to_string()),
        ("semicolon".to_string(), "temp_english".to_string()),
    ]);
    let dropped = materialize_into(&mut root, &bindings).unwrap();

    assert_eq!(dropped, 3, "三处 trigger_keys 都应被摘掉");
    assert_eq!(
        get_nested(&root, &["keys", "key_actions", "backtick"]).and_then(toml::Value::as_str),
        Some("temp_pinyin")
    );
    assert!(already_materialized(&root), "版本标记应已写入");
    assert!(
        get_nested(&root, &["input", "temp_pinyin", "trigger_keys"]).is_none(),
        "旧字段必须删除，否则仍是第二真相源"
    );
    assert!(
        get_nested(&root, &["input", "temp_pinyin", "enabled"]).is_some(),
        "同段内的其它键不得受牵连"
    );
    assert!(
        get_nested(&root, &["input", "temp_english"]).is_none(),
        "该段只剩 trigger_keys 时应被 remove_nested 整段回收"
    );
    // mix_modes 只摘 trigger_keys，元素本身（id/members）必须留着。
    let modes = get_nested(&root, &["schema", "mix_modes"])
        .and_then(toml::Value::as_array)
        .expect("mix_modes 应仍在");
    assert_eq!(modes.len(), 1);
    assert!(modes[0].get("id").is_some(), "元素本身不可删");
    assert!(modes[0].get("trigger_keys").is_none());
}

#[test]
fn already_materialized_only_trusts_explicit_version() {
    // 「key_actions 非空」不等于「迁移过」——用户手工配过但从没迁移时会猜错，
    // 一猜错就是出厂绑定永久丢失，故判据只认显式版本号。
    let hand_written = tv("[keys.key_actions]\nbacktick = \"temp_pinyin\"\n");
    assert!(!already_materialized(&hand_written));
    assert!(!already_materialized(&tv("")));
    assert!(already_materialized(&tv(
        "[keys]\nkey_actions_materialized = 1\n"
    )));
}

/// 回归闸：物化标记绝不能被 `prune_user_config` 清掉，否则下次加载重新折算，
/// 用户删掉的出厂绑定又被灌回去。
#[test]
fn prune_keeps_materialize_marker() {
    let preset = toml::Value::try_from(Config::default()).unwrap();
    assert!(
        get_nested(&preset, &["keys", "key_actions_materialized"]).is_none(),
        "标记不得出现在序列化产物里——否则 registry_covers_every_config_key 会红，\
             且它会被当成用户可配项参与 prune 比对"
    );
    assert!(
        !crate::config_schema::is_known_key("keys.key_actions_materialized"),
        "标记是迁移账本、不是配置项，不该登记进注册表"
    );
    let mut user = tv("[keys]\nkey_actions_materialized = 1\n");
    prune_redundant(&mut user, &preset);
    prune_retired(&mut user);
    assert!(already_materialized(&user), "标记必须在清理后仍然存活");
}

/// 行为闸：已物化时，折算不得复活用户删掉的绑定。
///
/// 这正是报障现场——用户在设置页取消勾选反引号，保存后下次加载又被折算灌回去。
#[test]
fn materialized_config_stops_reviving_deleted_binding() {
    let mut c = Config::default();
    // L2 出厂声明仍在（磁盘上那份我们不删），而用户已把绑定删光。
    c.input.temp_pinyin.trigger_keys = vec!["backtick".into()];
    c.keys.key_actions.clear();
    c.keys.key_actions_materialized = KEY_ACTIONS_MATERIALIZE_VERSION;

    c.normalize();

    assert!(
        !c.keys.key_actions.contains_key("backtick"),
        "已物化时 trigger_keys 不得再折算——否则用户的删除永远不生效"
    );
    assert!(
        c.input.temp_pinyin.trigger_keys.is_empty(),
        "normalize 的后置条件：trigger_keys 恒为空，消费端一律读 key_actions"
    );
}

/// 反事实对照：未物化时折算照旧（Android 等不跑物化的宿主依赖这条路径）。
#[test]
fn unmaterialized_config_still_folds_trigger_keys() {
    let mut c = Config::default();
    c.input.temp_pinyin.trigger_keys = vec!["backtick".into()];
    c.keys.key_actions.clear();
    assert_eq!(c.keys.key_actions_materialized, 0, "前提：未物化");

    c.normalize();

    assert_eq!(
        c.keys.key_actions.get("backtick").map(String::as_str),
        Some("temp_pinyin"),
        "未物化时必须保持原有折算行为，否则 Android 侧引导键当场失效"
    );
}

#[test]
fn prune_keeps_map_subpaths() {
    // `input.punct.custom_mappings` 在注册表里是 Map 类型——整体才是一个配置项。
    // collect_leaf_paths 会把它下钻成 `...custom_mappings."'1"` 这种伪键，删单条是错的语义
    // （等于悄悄改写用户的标点映射表）。registry 保险必须拦住。
    assert!(
        crate::config_schema::is_known_key("input.punct.custom_mappings"),
        "前提：Map 整体是登记键"
    );
    assert!(
        !crate::config_schema::is_known_key("input.punct.custom_mappings.'1"),
        "前提：其子路径不是登记键"
    );
    let preset = tv(r#"
[input.punct.custom_mappings]
"'1" = ["1", "＇"]
"#);
    let mut user = preset.clone();
    assert_eq!(
        prune_redundant(&mut user, &preset),
        0,
        "Map 子路径不得被当叶子删除"
    );
    assert!(get_nested(&user, &["input", "punct", "custom_mappings", "'1"]).is_some());
}

#[test]
fn remove_nested_reclaims_empty_parents_only() {
    let mut root = tv(r#"
[a.b]
x = 1
y = 2
"#);
    let toml::Value::Table(t) = &mut root else {
        unreachable!()
    };
    assert!(remove_nested(t, &["a", "b", "x"]));
    assert!(
        get_nested(&root, &["a", "b", "y"]).is_some(),
        "兄弟键还在时不得回收父表"
    );
    let toml::Value::Table(t) = &mut root else {
        unreachable!()
    };
    assert!(remove_nested(t, &["a", "b", "y"]));
    assert!(get_nested(&root, &["a"]).is_none(), "父表变空应逐级回收");
}

/// `MixModeConfig` 的两条默认值路径（serde 缺省与 `Default::default()`）必须一致。
///
/// `free_input_takes_select_keys` 的 serde 缺省是 `true`，而 `bool::default()` 是 `false`，
/// 所以 `Default` 是手写的；再加带非零默认值的字段而忘了改它，这里会红。
#[test]
fn mix_mode_config_serde_default_matches_default_impl() {
    let from_serde: MixModeConfig =
        toml::from_str("").expect("空表应能反序列化出全默认的 MixModeConfig");
    assert_eq!(
        from_serde,
        MixModeConfig::default(),
        "serde 缺省与 Default::default() 必须逐字段一致"
    );
    assert!(
        MixModeConfig::default().free_input_takes_select_keys,
        "夺取二三候选键默认应为开"
    );
    assert!(
        MixModeConfig::default().numeric_letter_select,
        "数字透镜下字母选词默认应为开"
    );
}

/// 退役键走显式名单清除，且三类不得误伤：同段里还活着的键、名字相似但仍在使用的
/// 另一个键、以及未登记的 Map 子路径。
#[test]
fn prune_retired_removes_dead_keys_only() {
    let mut root = tv(r#"
[schema.quick_input]
enable_english = true
enabled = true
decimal_places = 6

[schema.mix]
enable_english = true

[input.punct.custom_mappings]
"/" = ["、", "／", "、", "/"]
"#);
    assert_eq!(prune_retired(&mut root), 2, "两个退役键都应删除");
    assert!(get_nested(&root, &["schema", "quick_input", "enable_english"]).is_none());
    assert!(get_nested(&root, &["schema", "quick_input", "enabled"]).is_none());
    assert!(
        get_nested(&root, &["schema", "quick_input", "decimal_places"]).is_some(),
        "同段里还活着的键不得误删"
    );
    // `schema.mix.enable_english` 是另一个仍在使用的键（混输引擎混入英文词库
    //   候选的开关，manager.rs 实读）。它与退役的 `schema.quick_input.enable_english`
    //   只是叶子名相同，按整条路径匹配才不会误伤。
    assert!(
        get_nested(&root, &["schema", "mix", "enable_english"]).is_some(),
        "schema.mix.enable_english 仍在使用，不得误删"
    );
    // Map 子路径同样不在注册表里，靠「未登记就删」会把用户的自定义标点映射删光。
    assert!(
        get_nested(&root, &["input", "punct", "custom_mappings", "/"]).is_some(),
        "Map 子路径不得误删"
    );
    assert_eq!(prune_retired(&mut root), 0, "幂等：再跑一次删 0 个");
}

/// 两个旧键进退役清单：清掉它们，同段活键不动；幂等。
#[test]
fn retired_reverse_keys_are_pruned() {
    let mut root = tv(r#"
[input.reverse]
enabled = true
candidate_layout = "vertical"
lookup_disabled_dicts = true

[schema.codetable]
lookup_disabled_dicts = true
wildcard = true
"#);
    assert_eq!(prune_retired(&mut root), 2);
    assert!(get_nested(&root, &["input", "reverse", "enabled"]).is_none());
    assert!(get_nested(&root, &["schema", "codetable", "lookup_disabled_dicts"]).is_none());
    assert!(
        get_nested(&root, &["input", "reverse", "lookup_disabled_dicts"]).is_some(),
        "新键不得误删"
    );
    assert!(get_nested(&root, &["input", "reverse", "candidate_layout"]).is_some());
    assert!(get_nested(&root, &["schema", "codetable", "wildcard"]).is_some());
    assert_eq!(prune_retired(&mut root), 0);
}

/// 父表被清空时应整段回收——用户配置里 `[schema.quick_input]` 常常只有这两个退役键。
#[test]
fn prune_retired_reclaims_emptied_parent_table() {
    let mut root = tv(r#"
[schema.quick_input]
enable_english = true
enabled = true
"#);
    assert_eq!(prune_retired(&mut root), 2);
    assert!(
        get_nested(&root, &["schema", "quick_input"]).is_none(),
        "两个键都删完后空的 [schema.quick_input] 段应一并回收"
    );
}

#[test]
fn collect_leaf_paths_treats_arrays_as_leaves() {
    // 数组整体是一个配置项：下钻进元素会切出无法用 path 表达、也无法与 preset 比对的伪键。
    let v = tv(r#"
[keys]
page_keys = ["a", "b"]

[schema]
active = "x"
"#);
    let mut out = Vec::new();
    collect_leaf_paths(&v, &mut Vec::new(), &mut out);
    out.sort();
    assert_eq!(
        out,
        vec![
            vec!["keys".to_string(), "page_keys".to_string()],
            vec!["schema".to_string(), "active".to_string()],
        ]
    );
}

#[test]
fn comment_dyn_key_writes_cell_and_template_and_blank_deletes() {
    use crate::config_schema::comment_dyn_key;
    let mut root = toml::Table::new();
    let mut put = |k: &str, v: &str| {
        Config::apply_comment_dyn_key(&mut root, comment_dyn_key(k).unwrap(), v).unwrap()
    };
    put("ui.comment.assign.mode.url.default_vertical", "t1");
    put("ui.comment.assign.global.detail_horizontal", "none");
    put("ui.comment.template.t1", "${code}");
    put("ui.comment.template.t1", "${dict}");
    put("ui.comment.template.t2", "x");
    let v = toml::Value::Table(root.clone());
    let c: crate::comment_lib::UiCommentConfig = get_nested(&v, &["ui", "comment"])
        .unwrap()
        .clone()
        .try_into()
        .unwrap();
    assert_eq!(c.assign.mode["url"].default_vertical.as_deref(), Some("t1"));
    assert_eq!(c.assign.global.detail_horizontal.as_deref(), Some("none"));
    assert_eq!(c.templates.len(), 2);
    assert_eq!(c.templates[0].text, "${dict}");
    let mut put = |k: &str, v: &str| {
        Config::apply_comment_dyn_key(&mut root, comment_dyn_key(k).unwrap(), v).unwrap()
    };
    put("ui.comment.assign.mode.url.default_vertical", "");
    put("ui.comment.assign.global.detail_horizontal", "");
    put("ui.comment.template.t1", "");
    put("ui.comment.template.t2", "");
    assert!(root.is_empty(), "全删后应回收空段: {root:?}");
}

#[test]
fn comment_dyn_key_keeps_builtin_name_reports_real_path_and_refuses_bad_shapes() {
    use crate::config_schema::comment_dyn_key;
    let sys = crate::comment_lib::SystemComments::embedded();
    let b = sys.templates.first().expect("内置模板非空").clone();
    let mut root = toml::Table::new();
    let tkey = format!("ui.comment.template.{}", b.id);
    let (p, v) =
        Config::apply_comment_dyn_key(&mut root, comment_dyn_key(&tkey).unwrap(), "${code}")
            .unwrap();
    assert_eq!(p, ["ui", "comment", "templates"]);
    assert_eq!(v[0]["name"].as_str(), Some(b.name.as_str()), "名称沿用内置");
    let (p, v) = Config::apply_comment_dyn_key(
        &mut root,
        comment_dyn_key("ui.comment.assign.type.pinyin.default_vertical").unwrap(),
        "x",
    )
    .unwrap();
    assert_eq!(p, ["ui", "comment", "assign", "type"]);
    assert_eq!(v["pinyin"]["default_vertical"].as_str(), Some("x"));
    // 坏形状：拒写且不动原值。
    let mut bad = toml::Table::new();
    bad.insert(
        "ui".into(),
        toml::Value::Table({
            let mut c = toml::Table::new();
            c.insert(
                "comment".into(),
                toml::Value::Table({
                    let mut t = toml::Table::new();
                    t.insert("templates".into(), toml::Value::String("x".into()));
                    t.insert("assign".into(), toml::Value::String("y".into()));
                    t
                }),
            );
            c
        }),
    );
    let before = bad.clone();
    for k in [
        "ui.comment.template.t1",
        "ui.comment.assign.global.default_vertical",
    ] {
        assert!(
            Config::apply_comment_dyn_key(&mut bad, comment_dyn_key(k).unwrap(), "z").is_err(),
            "{k}"
        );
    }
    assert_eq!(bad, before);
}

#[test]
fn set_nested_creates_overwrites_and_preserves() {
    let mut t = toml::Table::new();
    t.insert("keep".into(), toml::Value::String("x".into()));
    set_nested(
        &mut t,
        &["ui", "candidate", "preedit_display"],
        toml::Value::String("candidate_inline".into()),
    );
    set_nested(
        &mut t,
        &["schema", "active"],
        toml::Value::String("pinyin".into()),
    );
    // 原有项保留
    assert_eq!(t.get("keep").unwrap().as_str(), Some("x"));
    // 嵌套创建
    assert_eq!(
        t.get("ui")
            .unwrap()
            .get("candidate")
            .unwrap()
            .get("preedit_display")
            .unwrap()
            .as_str(),
        Some("candidate_inline")
    );
    assert_eq!(
        t.get("schema").unwrap().get("active").unwrap().as_str(),
        Some("pinyin")
    );
    // 同路径覆盖
    set_nested(
        &mut t,
        &["ui", "candidate", "preedit_display"],
        toml::Value::String("candidate_top".into()),
    );
    assert_eq!(
        t.get("ui")
            .unwrap()
            .get("candidate")
            .unwrap()
            .get("preedit_display")
            .unwrap()
            .as_str(),
        Some("candidate_top")
    );
    // 其它兄弟键不受影响
    assert_eq!(
        t.get("schema").unwrap().get("active").unwrap().as_str(),
        Some("pinyin")
    );
}

/// 模拟 load 的合并：默认 Value ← overlay 深合并 → 反序列化 + normalize。
fn merged_with(overlay_toml: &str) -> Config {
    let mut base = toml::Value::try_from(Config::default()).unwrap();
    let overlay: toml::Value = toml::from_str(overlay_toml).unwrap();
    merge_value(&mut base, overlay);
    let mut cfg: Config = base.try_into().unwrap();
    cfg.normalize();
    cfg
}

/// 补丁里的键覆盖同名叶子，其余字段（含全局改过的）原样保留。
#[test]
fn candidate_folded_with_overrides_only_patched_leaves() {
    let base = UiCandidateConfig {
        min_rows: 4,
        per_page_extended: 6,
        ..Default::default()
    };
    let mut patch = toml::Table::new();
    patch.insert("per_page".into(), toml::Value::Integer(9));
    let f = base.folded_with(&patch);
    assert_eq!(f.per_page, 9);
    assert_eq!(f.min_rows, 4, "未覆盖的项跟随全局");
    assert_eq!(f.per_page_extended, 6);
    assert_eq!(base.per_page, 7, "原值不被改动");
}

#[test]
fn candidate_folded_with_empty_patch_is_identity() {
    let base = UiCandidateConfig::default();
    let f = base.folded_with(&toml::Table::new());
    assert_eq!(f.per_page, base.per_page);
}

/// 补丁里混进了反序列化不了的值时退回原值，而不是带出半份配置。
#[test]
fn candidate_folded_with_unparsable_patch_falls_back() {
    let base = UiCandidateConfig::default();
    let mut patch = toml::Table::new();
    patch.insert("per_page".into(), toml::Value::String("x".into()));
    assert_eq!(base.folded_with(&patch).per_page, base.per_page);
}

#[test]
fn test_default_per_page_is_7() {
    assert_eq!(
        Config::default().ui.candidate.per_page,
        7,
        "候选每页默认应为 7"
    );
}

#[test]
fn test_merge_reads_per_page_from_ui_candidate() {
    let cfg = merged_with("[ui.candidate]\nper_page = 9\n");
    assert_eq!(
        cfg.ui.candidate.per_page, 9,
        "应从 [ui.candidate] 读取 per_page=9"
    );
}

#[test]
fn test_merge_per_page_zero_keeps_default() {
    // per_page=0 视为无效，normalize 回退默认，避免每页只显示 1 个
    let cfg = merged_with("[ui.candidate]\nper_page = 0\n");
    assert_eq!(cfg.ui.candidate.per_page, 7, "per_page=0 应保留默认 7");
}

/// 简繁两方向的生效口径：四种组合各一条。只有「两个都开 ⇒ s2t 赢」那格能分辨实现
/// 是否真的做了取舍。
#[test]
fn conversion_directions_resolve_conflict_toward_s2t() {
    let mut c = InputConfig::default();
    for (s2t, t2s, want) in [
        (false, false, (false, false)),
        (true, false, (true, false)),
        (false, true, (false, true)),
        (true, true, (true, false)), // ★ 冲突：s2t 赢，t2s 被压掉
    ] {
        c.s2t.enabled = s2t;
        c.t2s.enabled = t2s;
        assert_eq!(
            c.conversion_directions(),
            want,
            "s2t={s2t} t2s={t2s} 的生效方向不对"
        );
        assert_eq!(
            c.has_conversion_conflict(),
            s2t && t2s,
            "s2t={s2t} t2s={t2s} 的冲突判定不对"
        );
    }
}

#[test]
fn test_merge_keeps_input_s2t_debug() {
    // 回归：deep-merge 必须保留各段（features 拆解后 s2t 归 input）
    let cfg = merged_with(
        "[input.s2t]\nenabled = true\nvariant = \"s2tw\"\n\
             [debug]\nlog_level = \"trace\"\n",
    );
    assert!(cfg.input.s2t.enabled, "input.s2t.enabled 应被合并");
    assert_eq!(cfg.input.s2t.variant, "s2tw");
    assert_eq!(cfg.debug.log_level, "trace", "debug 段应被合并");
}

#[test]
fn test_merge_partial_keeps_unspecified_default() {
    // overlay 只覆盖单个字段，同段其它字段保留默认（不被清空）
    let cfg = merged_with("[input]\nenter_behavior = \"clear\"\n");
    assert_eq!(cfg.input.enter_behavior, "clear");
    assert_eq!(cfg.input.filter_mode, "smart", "同段未指定字段应保留默认");
}

#[test]
fn test_merge_input_subtable_fields() {
    // 旧合并漏掉的 input 子表字段（如 auto_pair）现应合并
    let cfg = merged_with("[input.auto_pair]\nchinese = false\nenglish = false\n");
    assert!(!cfg.input.auto_pair.chinese);
    assert!(!cfg.input.auto_pair.english);
}

#[test]
fn test_tooltip_defaults() {
    let t = Config::default().ui.tooltip;
    assert_eq!(
        t.delay, 200,
        "delay 按 data/config.toml 预置默认 200（偏离 Go 的 100）"
    );
    let on: Vec<&str> = t
        .sections
        .iter()
        .filter(|s| s.enabled)
        .map(|s| s.label.as_str())
        .collect();
    assert_eq!(
        on,
        ["完整原文", "编码{(${code_source})}", "拼音", "释义"],
        "出厂开完整原文、编码、拼音、释义"
    );
    assert_eq!(t.max_chars, 200, "单行显示上限默认 200 字");
    assert_eq!(t.wrap_width, 40, "折行宽度默认 40 列");
    // 旧出厂开关（全缺省）迁出来必须恰是出厂段列表：否则从没碰过气泡设置、却在用户层
    // 留着旧键的老用户，升级后会拿到一份与新装用户不同的段列表。
    assert_eq!(
        tooltip_sections_from_legacy(LegacyTooltipFlags::default()),
        default_tooltip_sections()
    );
}

/// 只写过「与旧出厂相同」的旧开关的老用户：迁移不给本层写段列表，这批用户继续跟随出厂，
/// 而不是被钉在一份自定义段列表上。反向：真改过开关的用户，迁出的段列表留在用户层。
#[test]
fn default_legacy_switches_leave_no_sections_custom_ones_kept() {
    let all_default = "[ui.tooltip]\ncode_enabled = true\npinyin_enabled = true\n\
                           pinyin_heteronyms = true\npinyin_max_readings = 0\n\
                           chaizi_enabled = false\ndebug_enabled = false\n";
    // 迁移本身（三条写盘 / 加载路径共用）就不写段列表：set_user_value 只剪本次那个键，
    // 迁移若写了，整份出厂段列表会原样落盘。
    let mut root: toml::Value = toml::from_str(all_default).unwrap();
    Config::migrate_tooltip_sections_value(&mut root);
    assert!(
        get_nested(&root, &["ui", "tooltip", "sections"]).is_none(),
        "全缺省旧开关不得在本层留下段列表：{root:?}"
    );
    assert!(
        get_nested(&root, &["ui", "tooltip", "code_enabled"]).is_none(),
        "旧键照样摘掉"
    );
    // 真改过开关的用户，迁出的段列表留在本层，prune 也不删（与出厂不等）。
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data");
    let preset = Config::system_preset_value(Some(&data)).unwrap();
    let mut root: toml::Value = toml::from_str("[ui.tooltip]\nchaizi_enabled = true\n").unwrap();
    Config::migrate_tooltip_sections_value(&mut root);
    prune_redundant(&mut root, &preset);
    assert!(
        get_nested(&root, &["ui", "tooltip", "sections"]).is_some(),
        "开过拆字的用户，合并段必须留在用户层"
    );
    // 用户层里原样躺着一份出厂段列表的，按冗余删——preset 取真实的 L1⊕L2（Value 层合并），
    // 不只是 L1：L2 少写一个字段（如 `promote = ""`）会让两边在 Value 层对不上。
    let mut user = toml::Value::Table(Default::default());
    if let toml::Value::Table(t) = &mut user {
        set_nested(
            t,
            &["ui", "tooltip", "sections"],
            toml::Value::try_from(default_tooltip_sections()).unwrap(),
        );
    }
    assert_eq!(prune_redundant(&mut user, &preset), 1);
}

/// 旧开关迁移：跑真实链路（用户层迁移 → ⊕ L1 默认 → 反序列化），返回生效段列表与迁移后的本层。
fn tooltip_after_migration(user_toml: &str) -> (Vec<TooltipSection>, toml::Value) {
    let mut user: toml::Value = toml::from_str(user_toml).unwrap();
    Config::migrate_user_layer_value(&mut user);
    let mut merged = toml::Value::try_from(Config::default()).unwrap();
    merge_value(&mut merged, user.clone());
    let cfg: Config = merged.try_into().expect("反序列化");
    (cfg.ui.tooltip.sections, user)
}

fn enabled_labels(sections: &[TooltipSection]) -> Vec<&str> {
    sections
        .iter()
        .filter(|s| s.enabled)
        .map(|s| s.label.as_str())
        .collect()
}

/// 拆字 + 拼音同开 → 一个合并段（`promote` 复现旧行序），不再有独立的拼音、拆字段。
#[test]
fn migrate_tooltip_chaizi_with_pinyin_becomes_merged_section() {
    let (secs, user) = tooltip_after_migration("[ui.tooltip]\nchaizi_enabled = true\n");
    assert_eq!(
        enabled_labels(&secs),
        ["完整原文", "编码{(${code_source})}", "拆字 / 拼音", "释义"]
    );
    let merged = secs.iter().find(|s| s.label == "拆字 / 拼音").unwrap();
    assert_eq!(merged.each, "han");
    assert_eq!(merged.promote, "chaizi");
    assert_eq!(
        merged.template,
        "${char}：{${chaizi}{ [${chaizi_code}]}\t}${readings}"
    );
    assert!(
        !secs.iter().any(|s| s.label == "拆字" || s.label == "拼音"),
        "被合并的两段不留关着的残壳"
    );
    assert!(
        get_nested(&user, &["ui", "tooltip", "chaizi_enabled"]).is_none(),
        "旧键须从本层移除"
    );
}

#[test]
fn migrate_tooltip_chaizi_without_pinyin_enables_chaizi_section() {
    let (secs, _) =
        tooltip_after_migration("[ui.tooltip]\nchaizi_enabled = true\npinyin_enabled = false\n");
    assert_eq!(
        enabled_labels(&secs),
        ["完整原文", "编码{(${code_source})}", "拆字", "释义"]
    );
}

/// `heteronyms = false` 先于 `max_readings` 判定（旧实现即如此），迁成 `${readings:1}`。
#[test]
fn migrate_tooltip_reading_count_switches() {
    let tpl = |toml: &str| {
        let (secs, _) = tooltip_after_migration(toml);
        secs.into_iter()
            .find(|s| s.label == "拼音")
            .unwrap()
            .template
    };
    assert_eq!(
        tpl("[ui.tooltip]\npinyin_heteronyms = false\npinyin_max_readings = 3\n"),
        "${char}：${readings:1}"
    );
    assert_eq!(
        tpl("[ui.tooltip]\npinyin_max_readings = 2\n"),
        "${char}：${readings:2}"
    );
    assert_eq!(
        tpl("[ui.tooltip]\npinyin_max_readings = 0\n"),
        "${char}：${readings}"
    );
}

#[test]
fn migrate_tooltip_code_and_debug_switches() {
    let (secs, _) =
        tooltip_after_migration("[ui.tooltip]\ncode_enabled = false\ndebug_enabled = true\n");
    assert_eq!(enabled_labels(&secs), ["完整原文", "拼音", "释义", "调试"]);
}

/// 旧出厂 6 段（加「释义」之前）原样躺在用户层的：视为冗余剪掉，跟随新出厂；否则出厂一变它
/// 就成了删不掉的「自定义」。
/// 字段写得不全（serde 缺省补齐后相等）同样认；改过任何一处的不动。
#[test]
fn stale_factory_tooltip_sections_follow_new_factory() {
    let old6 = r#"
[[ui.tooltip.sections]]
label = "完整原文"
template = "${full_text}"
[[ui.tooltip.sections]]
label = "编码{(${code_source})}"
template = "${word_code}"
[[ui.tooltip.sections]]
label = "拼音"
each = "han"
template = "${char}：${readings}"
[[ui.tooltip.sections]]
enabled = false
label = "拆字"
each = "han"
template = "${char}：${chaizi}{ [${chaizi_code}]}"
[[ui.tooltip.sections]]
enabled = false
label = "Unicode"
each = "char"
template = "${char}：${unicode}"
[[ui.tooltip.sections]]
enabled = false
label = "调试"
template = "${debug}"
"#;
    let (secs, user) = tooltip_after_migration(old6);
    assert_eq!(secs, default_tooltip_sections(), "跟随新出厂（含「释义」）");
    assert!(
        get_nested(&user, &["ui", "tooltip", "sections"]).is_none(),
        "本层的旧出厂副本被剪掉：{user:?}"
    );
    // 落盘那条路（prune_user_config 用的同一个迁移函数）同样剪。
    let mut root: toml::Value = toml::from_str(old6).unwrap();
    Config::migrate_stale_factory_tooltip_sections_value(&mut root);
    assert!(get_nested(&root, &["ui", "tooltip", "sections"]).is_none());

    // 改过一处（打开了调试段）= 用户自定义，原样保留。
    let custom = old6.replacen("enabled = false\nlabel = \"调试\"", "label = \"调试\"", 1);
    assert_ne!(custom, old6, "前提：确实改到了");
    let (secs, user) = tooltip_after_migration(&custom);
    assert!(get_nested(&user, &["ui", "tooltip", "sections"]).is_some());
    assert_eq!(secs.len(), 6, "自定义列表不被加段");
}

/// 用户已写 `sections` 时以它为准，旧键只清不迁。
#[test]
fn migrate_tooltip_existing_sections_win_over_legacy_switches() {
    let (secs, user) = tooltip_after_migration(
        "[ui.tooltip]\nchaizi_enabled = true\n\n[[ui.tooltip.sections]]\nlabel = \"只要这段\"\ntemplate = \"${word_code}\"\n",
    );
    assert_eq!(enabled_labels(&secs), ["只要这段"]);
    assert!(get_nested(&user, &["ui", "tooltip", "chaizi_enabled"]).is_none());
}

/// 没写旧键的层原样不动（不凭空写出一份 `sections`，否则出厂段列表再也送达不了该用户）。
#[test]
fn migrate_tooltip_leaves_untouched_layer_alone() {
    let (_, user) = tooltip_after_migration("[ui.tooltip]\ndelay = 300\n");
    assert_eq!(
        user,
        toml::from_str::<toml::Value>("[ui.tooltip]\ndelay = 300\n").unwrap()
    );
}

/// 迁移落盘后再清退役键，生效值不变（`prune_user_config` 的顺序）；幂等。
#[test]
fn migrate_tooltip_then_prune_retired_keeps_effective_sections() {
    let src = "[ui.tooltip]\nchaizi_enabled = true\npinyin_max_readings = 2\n";
    let (before, _) = tooltip_after_migration(src);
    let mut root: toml::Value = toml::from_str(src).unwrap();
    Config::migrate_tooltip_sections_value(&mut root);
    assert_eq!(prune_retired(&mut root), 0, "迁移已摘掉旧键，清单无事可做");
    let (after, _) = tooltip_after_migration(&toml::to_string(&root).unwrap());
    assert_eq!(before, after);
    let again = root.clone();
    Config::migrate_tooltip_sections_value(&mut root);
    assert_eq!(root, again, "幂等");
    // 反向：只清不迁会丢设置——这正是必须先落盘迁移的理由。
    let mut raw: toml::Value = toml::from_str(src).unwrap();
    assert_eq!(prune_retired(&mut raw), 2);
    let (lost, _) = tooltip_after_migration(&toml::to_string(&raw).unwrap());
    assert_ne!(lost, before);
}

#[test]
fn test_candidate_tuning_defaults_and_methods() {
    let c = Config::default().ui.candidate;
    assert_eq!(c.font_size, 0.0, "字号默认 0 = 跟随主题");
    assert_eq!(c.max_chars, 16, "默认最大 16 字");
    assert!(c.index_labels.is_empty() && !c.flip_when_above);
    // 未配置 → 全部让位（主题/默认数字由协调器裁决）
    assert_eq!(c.user_index_label(0), None);
    // truncate：0=不限
    assert_eq!(
        c.truncate_display("这是一个很长的候选"),
        "这是一个很长的候选"
    );
}

#[test]
fn test_candidate_index_labels_and_truncate() {
    let cfg =
        merged_with("[ui.candidate]\nindex_labels = [\"a\", \"s\", \"d\", \"f\"]\nmax_chars = 4\n");
    let c = cfg.ui.candidate;
    assert_eq!(c.user_index_label(0), Some("a".to_string()));
    assert_eq!(c.user_index_label(2), Some("d".to_string()));
    assert_eq!(
        c.user_index_label(9),
        None,
        "槽位不足→None（让位主题/默认）"
    );
    assert_eq!(
        c.truncate_display("一二三四五六"),
        "一二三四…",
        "截断到 4 字并加省略号"
    );
    assert_eq!(c.truncate_display("一二"), "一二", "不足不截");
}

/// 截断按字素簇计（口径同悬停提示），不把 ZWJ 序列 / 组合符 / 变体选择符拦腰切开。
#[test]
fn test_truncate_display_counts_graphemes() {
    let cfg = merged_with("[ui.candidate]\nmax_chars = 3\n");
    let c = cfg.ui.candidate;
    let family = "👨\u{200D}👩\u{200D}👧";
    // ZWJ 序列算一个字：恰好 3 簇不截；4 簇截到前 3 簇，序列完整保留。
    let abf = format!("ab{family}");
    assert_eq!(c.truncate_display(&abf), abf, "ZWJ 序列整簇计 1");
    assert_eq!(
        c.truncate_display(&format!("{family}{family}{family}{family}")),
        format!("{family}{family}{family}…")
    );
    // 组合符：e + U+0301 是一簇。
    let e = "e\u{301}";
    assert_eq!(
        c.truncate_display(&e.repeat(3)),
        e.repeat(3),
        "组合符不另计"
    );
    assert_eq!(
        c.truncate_display(&e.repeat(4)),
        format!("{}…", e.repeat(3))
    );
    // 变体选择符：⚽ + U+FE0F 是一簇。
    let ball = "⚽\u{FE0F}";
    assert_eq!(c.truncate_display(&ball.repeat(3)), ball.repeat(3));
    assert_eq!(
        c.truncate_display(&format!("ab{ball}{ball}")),
        format!("ab{ball}…"),
        "截断点落在簇边界，不留孤立的 U+FE0F"
    );
    // 普通中英文与恰好等长。
    assert_eq!(c.truncate_display("中a文b"), "中a文…");
    assert_eq!(c.truncate_display("中a文"), "中a文", "恰好等长不截");
}

#[test]
fn test_user_index_label_optional() {
    // 用户显式设置：已配槽位返回 Some，越界返回 None（主题层可接手）。
    let cfg = merged_with("[ui.candidate]\nindex_labels = [\"a\", \"s\", \"d\", \"f\"]\n");
    let c = cfg.ui.candidate;
    assert_eq!(c.user_index_label(0), Some("a".to_string()));
    assert_eq!(c.user_index_label(3), Some("f".to_string()));
    assert_eq!(c.user_index_label(4), None, "越界→None（让位主题/默认）");
    // 未配置：全 None，优先级完全交给主题/默认。
    let d = merged_with("").ui.candidate;
    assert_eq!(d.user_index_label(0), None);
}

#[test]
fn test_index_label_slot_holds_multiple_chars() {
    // 本次重构的核心判据：一槽一串，不再按 char 切。
    // 三种旧形态下必被拆散的标签：括号数字、罗马数字、带 ZWJ 的组合 emoji。
    let cfg =
        merged_with("[ui.candidate]\nindex_labels = [\"(1)\", \"Ⅱ\", \"👨\\u200D👩\\u200D👧\"]\n");
    let c = cfg.ui.candidate;
    assert_eq!(c.user_index_label(0), Some("(1)".to_string()));
    assert_eq!(c.user_index_label(1), Some("Ⅱ".to_string()));
    assert_eq!(
        c.user_index_label(2),
        Some("👨\u{200D}👩\u{200D}👧".to_string()),
        "ZWJ 组合序列整体占一槽，不被拆成 5 个 char"
    );
}

#[test]
fn test_index_label_empty_slot_yields_to_theme() {
    // 第二项恢复的能力：中间空槽 = 该槽让位主题（旧的字符串形态表达不出来）。
    // 判据是 None 而非 Some("")——协调器只在 None 时才去问主题层。
    let cfg = merged_with("[ui.candidate]\nindex_labels = [\"a\", \"\", \"\", \"f\"]\n");
    let c = cfg.ui.candidate;
    assert_eq!(c.user_index_label(0), Some("a".to_string()));
    assert_eq!(c.user_index_label(1), None, "空槽让位，不是 Some(\"\")");
    assert_eq!(c.user_index_label(2), None);
    assert_eq!(
        c.user_index_label(3),
        Some("f".to_string()),
        "空槽之后仍生效"
    );
}

#[test]
fn test_migrate_empty_code_behavior_normalizes_illegal_values() {
    // 设置端 manifest 曾把 keys.overflow 的四个选项抄给了这三个键，而消费点只认
    // commit/clear。归一是零行为变更（那两个值本就走 commit 分支），目的是让存量配置
    // 落回合法值域，设置页下拉才能「按当前值恢复选中项」而不是静默弹回首项。
    let mut v: toml::Value = toml::from_str(
        "[input]\nenter_behavior = \"ignore\"\nspace_on_empty_behavior = \"commit_and_input\"\npunct_on_empty_behavior = \"clear\"\n",
    )
    .unwrap();
    Config::migrate_empty_code_behavior_value(&mut v);
    assert_eq!(v["input"]["enter_behavior"].as_str(), Some("commit"));
    assert_eq!(
        v["input"]["space_on_empty_behavior"].as_str(),
        Some("commit")
    );
    assert_eq!(
        v["input"]["punct_on_empty_behavior"].as_str(),
        Some("clear"),
        "合法值不得被动——否则这条迁移会把用户设的 clear 抹成 commit"
    );
    // 标点独有的第三态必须活着穿过迁移。这条断言的存在意义：迁移一旦把合法值域
    // 内联抄成 ["commit","clear"]（最自然的写法），存量配置里的 clear_no_input 会被
    // 静默抹回 commit——只在升过级的机器上复现，新装机器怎么测都是绿的。
    let mut v3: toml::Value =
        toml::from_str("[input]\npunct_on_empty_behavior = \"clear_no_input\"\n").unwrap();
    Config::migrate_empty_code_behavior_value(&mut v3);
    assert_eq!(
        v3["input"]["punct_on_empty_behavior"].as_str(),
        Some("clear_no_input"),
        "标点第三态被迁移抹掉了——合法值域须取自注册表，不能在迁移里另抄一份"
    );
    // 反向夹逼：同一个值在回车上不合法，须归一为 commit。两条一起才拦得住
    // 「图省事让三键共用一份值域」。
    let mut v4: toml::Value =
        toml::from_str("[input]\nenter_behavior = \"clear_no_input\"\n").unwrap();
    Config::migrate_empty_code_behavior_value(&mut v4);
    assert_eq!(
        v4["input"]["enter_behavior"].as_str(),
        Some("commit"),
        "回车没有 clear_no_input 这一态，须被归一"
    );
    // 幂等：每次启动都会跑一遍。
    let before = v.clone();
    Config::migrate_empty_code_behavior_value(&mut v);
    assert_eq!(v, before, "对已归一的值二次迁移须无变化");
}

#[test]
fn test_empty_code_behavior_registry_values_match_impl() {
    // 注册表的值域必须与消费点实际认得的值一致。三键值域不同：标点多一个
    // `clear_no_input`（丢废码且吞标点），回车/空格的 `clear` 本就吞键。分开断言是为了拦
    // 「顺手统一成一样」，共用一个期望值的话那种改动照样绿。
    let values_of = |key: &str| -> &'static [&'static str] {
        let f = crate::config_schema::field(key).expect("键须已登记");
        match f.ty {
            crate::config_schema::FieldType::Enum(vals) => vals,
            other => panic!("{key} 须登记为 Enum 而非 {other:?}"),
        }
    };
    for key in ["input.enter_behavior", "input.space_on_empty_behavior"] {
        assert_eq!(
            values_of(key),
            ["commit", "clear"],
            "{key} 的值域须恰为 commit/clear——它的 clear 已是吞键态，别把标点那个新值扩散过来"
        );
    }
    assert_eq!(
        values_of("input.punct_on_empty_behavior"),
        ["commit", "clear", "clear_no_input"],
        "标点值域须为三态——加值前先确认 Coordinator::punct_empty_code_policy 真的分了那一支"
    );
}

#[test]
fn test_migrate_index_labels_string_to_array() {
    // 存量迁移：旧的字符串形态按 char 拆成数组。不迁移的话 `try_into` 整体失败，
    // 调用方 `unwrap_or_default()` 会把全部配置回落出厂值。
    let mut v: toml::Value = toml::from_str("[ui.candidate]\nindex_labels = \"asdf\"\n").unwrap();
    Config::migrate_index_labels_value(&mut v);
    assert_eq!(
        v["ui"]["candidate"]["index_labels"],
        toml::Value::try_from(["a", "s", "d", "f"]).unwrap()
    );
    // 幂等：已是数组则原样不动（每次启动都会跑一遍）。
    let before = v.clone();
    Config::migrate_index_labels_value(&mut v);
    assert_eq!(v, before, "对新格式二次迁移须无变化");
    // 迁移后能落进结构体，且旧值语义不变。
    let cfg = merged_with("[ui.candidate]\nindex_labels = [\"a\", \"s\", \"d\", \"f\"]\n");
    assert_eq!(cfg.ui.candidate.user_index_label(1), Some("s".to_string()));
}

#[test]
fn test_load_survives_legacy_string_index_labels() {
    // 迁移的真正把关点在 `Config::load` 的 `merged.try_into()?` 上：
    // 这里复刻那条链路（默认值 ⊕ 旧格式用户层 → 迁移 → 反序列化）。
    // 没有迁移这一步，try_into 会 Err，整个配置无声回落出厂值。
    let mut merged = toml::Value::try_from(Config::default()).unwrap();
    let legacy: toml::Value = toml::from_str("[ui.candidate]\nindex_labels = \"asdf\"\n").unwrap();
    merge_value(&mut merged, legacy);
    Config::migrate_index_labels_value(&mut merged);
    let cfg: Config = merged
        .try_into()
        .expect("旧字符串经迁移后须能反序列化，否则全盘配置丢失");
    assert_eq!(
        cfg.ui.candidate.index_labels,
        vec!["a", "s", "d", "f"],
        "旧的单字符标签原样保留，用户无需重配"
    );
}

// ─────────────────────── 段级降级（section fallback）───────────────────────

/// 复刻 [`Config::load`] 的「L1 默认 ⊕ 用户层 → 反序列化」链路。
///
/// 刻意不调 `Config::load`：它会读真实用户目录下的 config.toml，测试输入就取决于本机。
/// 段级降级的判定都在纯函数 `deserialize_with_section_fallback` 里，测试只喂 `toml::Value`。
fn merged_user_value(user_toml: &str) -> toml::Value {
    let mut merged = toml::Value::try_from(Config::default()).expect("默认配置应可序列化");
    merge_value(
        &mut merged,
        toml::from_str(user_toml).expect("用例 TOML 应可解析"),
    );
    merged
}

/// 未发布即退役的两个键残留在用户配置里——不报错、不触发段级降级，同段的活键逐个完好。
#[test]
fn stale_reverse_keys_load_without_section_fallback() {
    let merged = merged_user_value(
        "[input.reverse]\nenabled = true\ncandidate_layout = \"vertical\"\nlookup_disabled_dicts = true\n\
             [schema.codetable]\nlookup_disabled_dicts = true\nwildcard = true\n",
    );
    let cfg = Config::deserialize_with_section_fallback(merged);
    assert_eq!(cfg.input.reverse.candidate_layout, LayoutIntent::Vertical);
    assert!(cfg.input.reverse.lookup_disabled_dicts);
    assert!(cfg.schema.codetable.wildcard, "同段活键不受旧键牵连");
}

/// `ui` 段的毒：`ui.font.scripts` 是 `BTreeMap<String, Vec<String>>`。
///
/// 必须用 Map 型字段构造：全仓无 `deny_unknown_fields`/`flatten`/`untagged`，普通 struct
/// 的未知键被静默丢弃，拿它构造会得到恒绿的假测试；Map 字段的残留项才会被当真数据反序列化。
const POISON_UI: &str = "[ui.font]\nscripts = { latin = 42 }\n";
/// `keys` 段的毒：`keys.key_actions` 是 `BTreeMap<String, String>`，值给整型。
const POISON_KEYS: &str = "[keys]\nkey_actions = { F7 = 42 }\n";
/// 分散在各段、必须活下来的用户设置。`ui.candidate.per_page` 与 [`POISON_UI`] 同属 `ui`
/// 段但不同子表，用来钉住「降级只降到 `ui.font`」。
const HEALTHY_USER: &str = "\
[schema]
active = \"section_fallback_probe\"
[input.default]
chinese_mode = false
[ui.candidate]
per_page = 9
[stats]
enabled = false
[debug]
log_level = \"trace\"
";

/// 坏段回落默认，其余段的用户值逐键完好。
///
/// 摘掉整个降级逻辑或只摘掉子表细化（`narrow_bad_section` 恒返回空），本组用例都会红；
/// [`section_fallback_is_transparent_when_healthy`]、[`degradation_affects_matches_subpaths`]、
/// [`section_fallback_falls_back_to_whole_section_when_not_narrowable`] 在两种摘法下应保持绿。
#[test]
fn section_fallback_keeps_healthy_sections() {
    let merged = merged_user_value(&format!("{HEALTHY_USER}{POISON_UI}"));
    // 前置：这份输入在没有降级的老链路上确实是 Err。若哪天 `scripts` 换了类型、
    // 这条毒不再是毒，这里会先红——避免下面的断言退化成恒绿。
    assert!(
        merged.clone().try_into::<Config>().is_err(),
        "用例前提失效：该输入已不再触发整体反序列化失败，请另造毒键"
    );

    let cfg = Config::deserialize_with_section_fallback(merged);

    // 好段逐键保留。这组断言故意排在降级记录之前：它们才是用户实际丢掉的东西，
    // 摘掉降级逻辑时应该由它们先红，指向「用户的方案设置没了」而不是「元信息对不上」。
    assert_eq!(
        cfg.schema.active, "section_fallback_probe",
        "schema 段无毒，用户值必须原样保留"
    );
    assert!(!cfg.input.default.chinese_mode, "input 段的用户值必须保留");
    assert!(!cfg.stats.enabled, "stats 段的用户值必须保留");
    assert_eq!(cfg.debug.log_level, "trace", "debug 段的用户值必须保留");
    assert_eq!(
        cfg.ui.candidate.per_page, 9,
        "毒在 ui.font，ui.candidate 是同段的**另一个子表**，用户值必须保留"
    );

    assert_eq!(cfg.degradation.sections, vec!["ui.font".to_string()]);
    assert!(!cfg.degradation.total_fallback);

    // 坏段回落 L1 默认（不是「保留了半截毒值」，也不是「整份归零」）
    assert!(
        cfg.ui.font.scripts.is_empty(),
        "有毒的 ui.font 子表须回落出厂默认"
    );
}

/// 降级粒度收到子表这一层：`ui.font.scripts` 有毒时只降 `ui.font`，`ui` 段其余子表的
/// 用户值一个不少。只降到段一级的话，一个坏字体映射就会把候选窗、主题等一起打回出厂。
#[test]
fn section_fallback_narrows_to_subtable_within_section() {
    // 同一个 `ui` 段里铺开 4 个子表的用户值，只有 `ui.font` 那个有毒。
    let merged = merged_user_value(
        "\
[ui.candidate]
per_page = 9
max_chars = 33
[ui.theme]
name = \"section_fallback_theme\"
[ui.toolbar]
visible = false
[ui.font]
family = \"SectionFallbackFont\"
scripts = { latin = 42 }
",
    );
    assert!(merged.clone().try_into::<Config>().is_err(), "用例前提");

    let cfg = Config::deserialize_with_section_fallback(merged);

    // 同段的其他子表：完好
    assert_eq!(cfg.ui.candidate.per_page, 9, "ui.candidate 不该被牵连");
    assert_eq!(cfg.ui.candidate.max_chars, 33, "ui.candidate 不该被牵连");
    assert_eq!(
        cfg.ui.theme.name, "section_fallback_theme",
        "ui.theme 不该被牵连"
    );
    assert!(!cfg.ui.toolbar.visible, "ui.toolbar 不该被牵连");

    // 有毒的那个子表：整个回落（`family` 是同子表内的附带损失，粒度只到这一层）
    assert_eq!(cfg.degradation.sections, vec!["ui.font".to_string()]);
    assert!(cfg.ui.font.scripts.is_empty());
    assert_eq!(
        cfg.ui.font.family,
        Config::default().ui.font.family,
        "同一子表内的键一并回落——这是「只递归一层」的已知代价"
    );
}

/// 细化不到子键时退回整段降级：段本身就不是表（`[input]` 位置写了个标量）。
#[test]
fn section_fallback_falls_back_to_whole_section_when_not_narrowable() {
    let mut merged = merged_user_value(HEALTHY_USER);
    merged
        .as_table_mut()
        .unwrap()
        .insert("input".to_string(), toml::Value::Integer(42));
    assert!(merged.clone().try_into::<Config>().is_err(), "用例前提");

    let cfg = Config::deserialize_with_section_fallback(merged);

    assert_eq!(
        cfg.degradation.sections,
        vec!["input".to_string()],
        "探不出更细的粒度就记整段，不能假装细化成功"
    );
    assert_eq!(
        cfg.schema.active, "section_fallback_probe",
        "其余段照常保留"
    );
    assert_eq!(cfg.ui.candidate.per_page, 9);
    assert!(
        cfg.input.default.chinese_mode,
        "input 整段回落出厂默认（默认为 true）"
    );
}

/// 一个重复键不该让整份配置失效——救回其余内容，并如实报出被跳过的行号。
#[test]
fn lenient_parse_salvages_around_duplicate_key() {
    let src = "[ui]\nx = 1\n\n[input]\nfoo = true\nfoo = false\nbar = 3\n";
    let r = parse_toml_lenient(src);
    assert!(!r.is_clean(), "重复键必须被记成语法故障");
    // 行号是用户在编辑器里看到的那个（1-based），不是内部索引。
    assert_eq!(r.skipped_lines, vec![6], "只该丢重复的那一行");

    let v = r.value.expect("其余内容必须救回来");
    assert_eq!(v["ui"]["x"].as_integer(), Some(1), "无关段不受影响");
    assert_eq!(
        v["input"]["bar"].as_integer(),
        Some(3),
        "同段里出错行之后的键也要保住——按段丢弃就会丢掉它"
    );
    assert_eq!(
        v["input"]["foo"].as_bool(),
        Some(true),
        "保留的是首次出现的那个值"
    );
}

/// 段头坏掉时必须丢弃整段，而不是只删那一行。
///
/// 只删段头，它下面的键会被 TOML 归给上一个段并可能真的生效；污染比丢失危险，丢失看得见。
#[test]
fn lenient_parse_drops_whole_section_when_header_is_broken() {
    // `[b` 缺右括号 ⇒ 段头本身非法。
    let src = "[a]\nx = 1\n\n[b\ny = 2\nz = 9\n\n[c]\nw = 3\n";
    let r = parse_toml_lenient(src);
    let v = r.value.expect("应能救回 a 与 c");

    assert_eq!(v["a"]["x"].as_integer(), Some(1));
    assert_eq!(v["c"]["w"].as_integer(), Some(3), "坏段之后的段要保住");
    assert!(v.get("b").is_none(), "坏掉的段整个不该出现");
    // 本测试的核心断言：坏段里的键绝不能被吸进上一个段。
    assert!(
        v["a"].get("y").is_none() && v["a"].get("z").is_none(),
        "段头被删后其下的键漂到了上一段——这正是本函数要防的污染"
    );
}

/// 完全不是 TOML 的内容不该被「啃」成残骸：剔到上限仍不成立就整份作废，
/// 面目全非却能解析的配置比明确失败的更危险。
#[test]
fn lenient_parse_gives_up_on_non_toml() {
    let src = "<!DOCTYPE html>\n<html><body>\n".repeat(40);
    let r = parse_toml_lenient(&src);
    assert!(r.value.is_none(), "啃不动就该整份作废，不能返回残骸");
    assert!(r.first_error.is_some());
}

/// 「跳过了几行」不等于「救回来了」：啃到上限仍失败时一个字都没加载，不能说「已加载其余内容」。
///
/// 判据必须是 `is_salvaged()`，不是 `skipped_lines` 非空。这条文案是 toast / CLI describe /
/// CLI export 的共同来源。
#[test]
fn describe_never_claims_success_when_nothing_was_salvaged() {
    let mut u = UnparsableLayer {
        layer: "user",
        path: PathBuf::from("config.toml"),
        error: "invalid".into(),
        // 试着跳了三行……
        skipped_lines: vec![1, 2, 3],
        // ……但最终什么都没解析出来。
        salvaged_keys: 0,
        not_utf8: false,
    };
    let s = u.describe();
    assert!(
        !s.contains("加载其余内容"),
        "一个键都没救回来时不许说「加载其余内容」：{s}"
    );
    assert!(s.contains("未能加载"), "必须说清本次没加载成功：{s}");
    // 尝试过的行号仍要留着——用户拿它去看那几行到底怎么了。
    assert!(
        s.contains('1') && s.contains('3'),
        "尝试跳过的行号不该丢：{s}"
    );

    // 对照：救回了内容才能说「加载其余内容」，否则上面的断言恒真也能全绿。
    u.salvaged_keys = 2;
    let ok = u.describe();
    assert!(ok.contains("加载其余内容"), "救回来了就该这么说：{ok}");
    assert!(!ok.contains("未能加载"), "{ok}");
}

/// 行号多到一定程度必须折叠：整份不是 TOML 时会啃到 `LENIENT_MAX_SKIPS`，全列出来会撑爆
/// toast 和设置页横幅，而那时具体行号本就没意义。
#[test]
fn lines_phrase_folds_long_lists() {
    let mk = |n: usize| UnparsableLayer {
        layer: "user",
        path: PathBuf::from("config.toml"),
        error: String::new(),
        skipped_lines: (1..=n).collect(),
        salvaged_keys: 1,
        not_utf8: false,
    };
    assert_eq!(mk(0).lines_phrase(), "", "没有行号就返回空串");

    // 不超阈值：逐个列出，不折叠。
    let few = mk(LINES_LISTED_MAX).lines_phrase();
    assert!(!few.contains('等'), "刚好到阈值不该折叠：{few}");
    assert!(few.contains(&LINES_LISTED_MAX.to_string()), "{few}");

    // 超阈值：折叠，并给出总数——量级信息比逐个行号有用。
    let many = mk(32).lines_phrase();
    assert!(many.contains("等 32 处"), "超阈值须折叠并给总数：{many}");
    assert!(!many.contains("30"), "不该把第 30 个行号也列出来：{many}");
    assert!(
        many.chars().count() < 40,
        "折叠后仍然很长就等于没折叠：{many}"
    );
    // 量词自带：调用方直接嵌入，不再拼「第 {} 行」——那样折叠形态会拼出
    // 「第 …等 32 处 行」的语病（实跑撞到过）。
    assert!(
        many.starts_with("第 ") && many.contains(" 行"),
        "短语须自带「第…行」：{many}"
    );
}

/// 非 UTF-8 的配置文件要进 `unparsable`（关写盘闸），不能当「文件不存在」跳过。
#[test]
fn non_utf8_config_is_reported_as_unparsable() {
    let dir = std::env::temp_dir().join(format!("wind_cfg_gbk_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    // 「# 中文」的 GBK 字节
    std::fs::write(&path, b"# \xd6\xd0\xce\xc4\n[ui]\nx = 1\n").unwrap();
    let mut un = Vec::new();
    let v = Config::read_toml_value_reporting(&path, "user", &mut un);
    assert!(v.is_none());
    assert_eq!(un.len(), 1, "编码错误必须上报");
    assert!(un[0].not_utf8);
    assert!(un[0].describe().contains("UTF-8"), "{}", un[0].describe());

    // 对照：带 BOM 的 UTF-8 照常加载，不上报。
    std::fs::write(&path, "\u{FEFF}# 中文\n[ui]\nx = 1\n").unwrap();
    let mut un = Vec::new();
    let v = Config::read_toml_value_reporting(&path, "user", &mut un);
    assert!(un.is_empty(), "{un:?}");
    assert_eq!(v.unwrap()["ui"]["x"].as_integer(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 全仓约定用 `text.parse::<toml::Value>()` 解析整篇文档（见 AGENTS.md）。这依赖
/// toml 0.8 的 `FromStr for Value` 按文档解析；升级 toml 后语义若变成「单个值」，这里先红。
#[test]
fn toml_value_from_str_parses_whole_documents() {
    let v: toml::Value = "a = 1\n[b]\nc = 2\n".parse().expect("整篇文档须能解析");
    assert_eq!(v["a"].as_integer(), Some(1));
    assert_eq!(v["b"]["c"].as_integer(), Some(2));
}

/// 合法文件走的是零改动的快路径：`is_clean` 为真、一行都没跳过。
#[test]
fn lenient_parse_leaves_valid_toml_untouched() {
    let r = parse_toml_lenient("[ui]\nx = 1\n");
    assert!(r.is_clean());
    assert!(r.skipped_lines.is_empty());
    assert_eq!(r.value.unwrap()["ui"]["x"].as_integer(), Some(1));
}

/// 任一层语法不合法 ⇒ `taints` 对任何路径为真，四道写盘闸一并关上。
///
/// 不做路径匹配是刻意的：救回的 Value 里「被跳过的键」与「从未写过的键」同形，
/// 「只影响这几段」的判断都是猜，猜错就是把残表写回磁盘。
#[test]
fn unparsable_layer_taints_every_path() {
    let deg = ConfigDegradation {
        sections: Vec::new(),
        total_fallback: false,
        unparsable: vec![UnparsableLayer {
            layer: "user",
            path: PathBuf::from("config.toml"),
            error: "duplicate key".into(),
            skipped_lines: vec![6],
            salvaged_keys: 2,
            not_utf8: false,
        }],
    };
    assert!(deg.is_degraded());
    assert!(deg.has_unparsable());
    for p in ["keys", "ui.font", "input.punct.custom_mappings", "schema"] {
        assert!(deg.taints(p), "语法故障必须让 {p} 也不可信");
    }
    // 对照：干净的降级记录不该恒为真，否则上面的断言全是废话。
    assert!(!ConfigDegradation::default().taints("keys"));
}

/// 写盘闸的判据必须两个方向都判：降级段是待写路径的祖先（坏在大段、写小格，如
/// `input.punct` 整段降级时写 `input.punct.custom_mappings`），待写路径是降级段的祖先
/// （写大表、坏在小格，如 `materialize_key_actions` 整表写 `keys`）。
/// 每个方向各配一条不该命中的对照，否则「恒为 true」也能全绿。
#[test]
fn taints_judges_both_ancestor_directions() {
    let deg = ConfigDegradation {
        sections: vec!["input.punct".into(), "keys.key_actions".into()],
        total_fallback: false,
        unparsable: Vec::new(),
    };
    // 相等
    assert!(deg.taints("input.punct"));
    // 降级段是待写路径的祖先（坏在大段、写小格）
    assert!(deg.taints("input.punct.custom_mappings"));
    // 待写路径是降级段的祖先（写大表、坏在小格）
    assert!(
        deg.taints("keys"),
        "整表写 keys 时 keys.key_actions 降级必须拦下"
    );
    // 对照：同段的兄弟子路径不受牵连，否则一个坏键会把整段的写盘全堵死
    assert!(!deg.taints("input.symbol"));
    assert!(!deg.taints("keys.session_actions"));
    assert!(!deg.taints("ui"));
    // 前缀相同但不是路径祖先：`keys.key_actions_materialized` 不该被
    // `keys.key_actions` 命中（字符串 starts_with 会误判，故判据必须看 `.`）
    assert!(!deg.taints("keys.key_actions_materialized"));

    // 整份回落 ⇒ 一切不可信
    let total = ConfigDegradation {
        sections: Vec::new(),
        total_fallback: true,
        unparsable: Vec::new(),
    };
    assert!(total.taints("anything"));
    // 未降级 ⇒ 一律放行（正常路径不能被闸误伤）
    let clean = ConfigDegradation::default();
    assert!(!clean.taints("keys"));
    assert!(!clean.blocks_write_back("keys", "测试"));
}

/// `affects` 与 `taints` 对顶层段名必须给出同一答案——两处判据漂移正是本仓
/// 反复栽的那类，而这两个函数守的是同一批写盘路径。
#[test]
fn affects_agrees_with_taints_on_top_level_sections() {
    for deg in [
        ConfigDegradation {
            sections: vec!["keys.key_actions".into()],
            total_fallback: false,
            unparsable: Vec::new(),
        },
        ConfigDegradation {
            sections: vec!["ui".into()],
            total_fallback: false,
            unparsable: Vec::new(),
        },
        ConfigDegradation {
            sections: Vec::new(),
            total_fallback: true,
            unparsable: Vec::new(),
        },
        ConfigDegradation::default(),
    ] {
        for section in ["input", "keys", "schema", "ui", "stats", "debug", "mobile"] {
            assert_eq!(
                deg.affects(section),
                deg.taints(section),
                "{deg:?} 在 [{section}] 上两个判据分叉了"
            );
        }
    }
}

/// 每条降级记录必须带自己那一段的错误，不能共用整份 `try_into` 的错误——那个错误只点得到
/// 其中一段，拼进每行 WARN 会把排查的人带到无关的段。
#[test]
fn section_fallback_reports_per_section_error() {
    let merged = merged_user_value(&format!("{HEALTHY_USER}{POISON_UI}{POISON_KEYS}"));
    let default_v = toml::Value::try_from(Config::default()).unwrap();
    let sections = merged.as_table().unwrap();

    use crate::section_fallback::narrow_bad_section;
    let ui_err = narrow_bad_section::<Config>(&default_v, "ui", sections.get("ui").unwrap());
    let keys_err = narrow_bad_section::<Config>(&default_v, "keys", sections.get("keys").unwrap());

    assert_eq!(ui_err.len(), 1);
    assert_eq!(keys_err.len(), 1);
    assert_eq!(ui_err[0].0, "ui.font");
    assert_eq!(keys_err[0].0, "keys.key_actions");

    assert!(
        ui_err[0].1.contains("scripts"),
        "ui 那条要讲自己的字段，实得 {:?}",
        ui_err[0].1
    );
    assert!(
        keys_err[0].1.contains("key_actions"),
        "keys 那条要讲自己的字段，实得 {:?}",
        keys_err[0].1
    );
    assert!(
        !keys_err[0].1.contains("scripts"),
        "keys 那条绝不能讲 ui 的字段，实得 {:?}",
        keys_err[0].1
    );
}

/// [`ConfigDegradation::affects`] 的判据必须覆盖子路径。这是
/// [`Config::materialize_key_actions`] 闸三的判据，写成精确相等会漏判 `keys.key_actions`，
/// 让可恢复的降级变成磁盘上的永久数据丢失。
#[test]
fn degradation_affects_matches_subpaths() {
    let d = ConfigDegradation {
        sections: vec!["keys.key_actions".to_string()],
        total_fallback: false,
        unparsable: Vec::new(),
    };
    assert!(d.affects("keys"), "子路径必须算作该段受影响");
    assert!(!d.affects("ui"), "别的段不能被误判");
    // 前缀相同但不是同一段：`key` 不是 `keys` 的父段，不能靠裸 `starts_with` 匹配上。
    assert!(!d.affects("key"), "只认以 '.' 分隔的真父段");

    let whole = ConfigDegradation {
        sections: vec!["keys".to_string()],
        total_fallback: false,
        unparsable: Vec::new(),
    };
    assert!(whole.affects("keys"));

    let total = ConfigDegradation {
        sections: Vec::new(),
        total_fallback: true,
        unparsable: Vec::new(),
    };
    assert!(total.affects("keys"), "整份回落时任何段都受影响");
    assert!(total.affects("ui"));
}

/// 多段同时有毒：两段都降级，好段不受牵连。拦的是「逐段替换直到成功」的实现，
/// 它分不清无辜段，会连带降级；探针法对每段独立判定。
#[test]
fn section_fallback_isolates_multiple_bad_sections() {
    let merged = merged_user_value(&format!("{HEALTHY_USER}{POISON_UI}{POISON_KEYS}"));
    assert!(merged.clone().try_into::<Config>().is_err(), "用例前提");

    let cfg = Config::deserialize_with_section_fallback(merged);

    // 无辜段一个都不能少（同上，先断言用户可见的损失）
    assert_eq!(cfg.schema.active, "section_fallback_probe");
    assert!(!cfg.input.default.chinese_mode);
    assert!(!cfg.stats.enabled);
    assert_eq!(cfg.debug.log_level, "trace");
    assert_eq!(cfg.ui.candidate.per_page, 9);

    assert_eq!(
        cfg.degradation.sections,
        vec!["keys.key_actions".to_string(), "ui.font".to_string()],
        "两处毒各自被定位到子表这一层"
    );
    assert!(cfg.ui.font.scripts.is_empty());
    assert!(cfg.keys.key_actions.is_empty());
}

/// 幂等：同一输入连跑两次结果一致；降级后的产物再喂回去不再降级、值也不再变。
/// 后半条对应设置页「加载→写回→再加载」的实际链路。
#[test]
fn section_fallback_is_idempotent() {
    let input = merged_user_value(&format!("{HEALTHY_USER}{POISON_UI}"));

    let first = Config::deserialize_with_section_fallback(input.clone());
    let second = Config::deserialize_with_section_fallback(input);
    // 先钉住「第一轮确实降了级、且好段还在」——否则本用例在「压根没有降级逻辑」的
    // 世界里也是绿的（两轮都得到出厂默认，当然一致），成为一个恒绿的假守门。
    assert_eq!(first.schema.active, "section_fallback_probe");
    assert_eq!(first.degradation.sections, vec!["ui.font".to_string()]);
    assert_eq!(first.degradation, second.degradation);
    assert_eq!(
        toml::Value::try_from(&first).unwrap(),
        toml::Value::try_from(&second).unwrap(),
        "同一输入两次加载须逐键相同"
    );

    // 第二轮：拿第一轮的产物当输入（等价于降级后写回再加载）
    let round_trip = Config::deserialize_with_section_fallback(
        toml::Value::try_from(&first).expect("降级后的配置应可序列化"),
    );
    assert_eq!(
        round_trip.degradation,
        ConfigDegradation::default(),
        "毒已被清掉，第二轮不该再降级"
    );
    assert_eq!(
        toml::Value::try_from(&round_trip).unwrap(),
        toml::Value::try_from(&first).unwrap(),
        "第二轮不得再改动任何键"
    );
}

/// 毒不在任何单段（顶层压根不是表）：整份回落 L1 默认，并把这件事标出来。
/// 段级降级只在「能定位到段」时成立，定位不到时不能假装成功。
#[test]
fn section_fallback_reports_total_failure() {
    let cfg = Config::deserialize_with_section_fallback(toml::Value::Integer(42));
    assert!(cfg.degradation.total_fallback, "定位不到有毒段须如实标记");
    assert!(cfg.degradation.sections.is_empty());
    assert_eq!(cfg.schema.active, Config::default().schema.active);
}

/// 健康配置零副作用：不降级、不留记录，且与直接 `try_into` 逐键相同。
#[test]
fn section_fallback_is_transparent_when_healthy() {
    let merged = merged_user_value(HEALTHY_USER);
    let direct: Config = merged.clone().try_into().expect("健康配置须直接可反序列化");
    let cfg = Config::deserialize_with_section_fallback(merged);
    assert_eq!(cfg.degradation, ConfigDegradation::default());
    assert_eq!(
        toml::Value::try_from(&cfg).unwrap(),
        toml::Value::try_from(&direct).unwrap(),
        "成功路径必须与老行为完全一致"
    );
}

/// `degradation` 是 `#[serde(skip)]` 的运行期元信息，不是配置键：不进注册表叶子集合，
/// 也不会被 `config.get` 序列化。一旦被序列化，`prune`/`set_user_value` 会把它当用户配置项
/// 写进用户 config.toml。
#[test]
fn degradation_is_not_a_config_key() {
    let v = toml::Value::try_from(Config::default()).unwrap();
    assert!(
        v.get("degradation").is_none(),
        "降级记录不得出现在配置的序列化产物里"
    );
    assert!(
        !crate::config_schema::config_leaf_keys()
            .iter()
            .any(|k| k.starts_with("degradation")),
        "降级记录不得进入配置叶子路径集合"
    );
}

/// 旧字号开关迁移：跑真实链路（用户层迁移 → ⊕ L1 默认 → 反序列化），返回生效字号。
///
/// 必须带 L1 默认层：生产里合并结果必然含全部字段，只喂裸 Value 的话迁移即便一次都
/// 不执行，测试也照样绿。
fn font_size_after_migration(user_toml: &str) -> (f32, toml::Value) {
    let mut user: toml::Value = toml::from_str(user_toml).unwrap();
    Config::migrate_user_layer_value(&mut user);
    let mut merged = toml::Value::try_from(Config::default()).unwrap();
    merge_value(&mut merged, user.clone());
    let cfg: Config = merged.try_into().expect("反序列化");
    (cfg.ui.candidate.font_size, user)
}

#[test]
fn migrate_font_size_follow_true_means_follow_theme() {
    let (fs, user) = font_size_after_migration(
        "[ui.candidate]\nfont_size = 22\nfont_size_follow_theme = true\n",
    );
    assert_eq!(
        fs, 0.0,
        "follow=true 时旧字号从未生效，迁移为 0（跟随主题）"
    );
    assert!(
        get_nested(&user, &["ui", "candidate", "font_size_follow_theme"]).is_none(),
        "旧键须从本层移除"
    );
}

/// 没写 `follow` 键、但写了 `font_size`：这正是新格式下用户自定义字号后落盘的形状，
/// 不能当「旧格式、从未生效」清零——`prune_user_config` 每次启动都跑迁移，清零会让
/// 用户设的字号重启后变回跟随主题。
#[test]
fn migrate_font_size_missing_follow_keeps_new_format_value() {
    let (fs, _user) = font_size_after_migration("[ui.candidate]\nfont_size = 22\n");
    assert_eq!(fs, 22.0, "没有旧键就不是旧格式，字号原样保留");
}

#[test]
fn migrate_font_size_follow_false_keeps_user_size() {
    let (fs, _) = font_size_after_migration(
        "[ui.candidate]\nfont_size = 22\nfont_size_follow_theme = false\n",
    );
    assert_eq!(fs, 22.0);
}

/// 旧版写回会剪掉等于出厂值的键：「关跟随、字号保持 18」的文件里只剩 follow=false。
/// 不补 18 的话会落到新出厂值 0，用户被悄悄切回跟随主题。
#[test]
fn migrate_font_size_follow_false_without_size_restores_legacy_default() {
    let (fs, _) = font_size_after_migration("[ui.candidate]\nfont_size_follow_theme = false\n");
    assert_eq!(fs, 18.0);
}

#[test]
fn migrate_font_size_untouched_layer_follows_theme_by_default() {
    let (fs, user) = font_size_after_migration("[ui.candidate]\nmax_chars = 9\n");
    assert_eq!(fs, 0.0);
    assert!(get_nested(&user, &["ui", "candidate", "font_size"]).is_none());
}

fn badges_after_migration(user_toml: &str) -> Vec<LangBarBadge> {
    let mut user: toml::Value = toml::from_str(user_toml).unwrap();
    Config::migrate_user_layer_value(&mut user);
    let mut merged = toml::Value::try_from(Config::default()).unwrap();
    merge_value(&mut merged, user);
    let cfg: Config = merged.try_into().expect("反序列化");
    cfg.ui.langbar.badges
}

#[test]
fn migrate_badge_auto_becomes_empty() {
    let b = badges_after_migration(
        "[[ui.langbar.badges]]\nstate = \"punct_cn\"\ncolor_light = \"auto\"\ncolor_dark = \"auto\"\n",
    );
    assert_eq!(b[0].color_light, "");
    assert_eq!(b[0].color_dark, "");
    assert_eq!(b[0].alpha_light, None);
}

#[test]
fn migrate_badge_rgb6_keeps_color_and_follows_global_alpha() {
    let b = badges_after_migration(
        "[[ui.langbar.badges]]\nstate = \"punct_cn\"\ncolor_light = \"#2288E0\"\ncolor_dark = \"#2288E0\"\n",
    );
    assert_eq!(b[0].color_light, "#2288E0");
    assert_eq!(b[0].alpha_light, None);
}

/// 浅、深两侧 8 位色值的 alpha 不同，迁移后各自保留。
#[test]
fn migrate_badge_rgba8_splits_alpha_per_side() {
    let b = badges_after_migration(
        "[[ui.langbar.badges]]\nstate = \"punct_cn\"\ncolor_light = \"#2288E080\"\ncolor_dark = \"#2288E0FF\"\n",
    );
    assert_eq!(b[0].color_light, "#2288E0");
    assert_eq!(b[0].color_dark, "#2288E0");
    assert!((b[0].alpha_light.unwrap() - 128.0 / 255.0).abs() < 1e-3);
    assert_eq!(b[0].alpha_dark, Some(1.0));
}

/// 旧写法 8 位色值的 `#` 可省（渲染侧 `parse_hex` 两种都认）：不带 `#` 的同样拆，
/// 输出统一带 `#`。
#[test]
fn migrate_badge_rgba8_without_hash_prefix_also_splits() {
    let b = badges_after_migration(
        "[[ui.langbar.badges]]\nstate = \"punct_cn\"\ncolor_light = \"2288E080\"\ncolor_dark = \"2288e0ff\"\n",
    );
    assert_eq!(b[0].color_light, "#2288E0");
    assert_eq!(b[0].color_dark, "#2288e0");
    assert!((b[0].alpha_light.unwrap() - 128.0 / 255.0).abs() < 1e-3);
    assert_eq!(b[0].alpha_dark, Some(1.0));
}

/// 缺色值键时 serde 默认即「与主字同色」—— 旧版缺键的生效值是 `auto`，新版是 `""`，
/// 两者同义，故缺键不需要迁移。
#[test]
fn migrate_badge_missing_color_defaults_to_follow() {
    let b = badges_after_migration("[[ui.langbar.badges]]\nstate = \"punct_cn\"\n");
    assert_eq!(
        (b[0].color_light.as_str(), b[0].color_dark.as_str()),
        ("", "")
    );
    assert_eq!((b[0].alpha_light, b[0].alpha_dark), (None, None));
}

#[test]
fn factory_badges_have_no_auto_and_no_alpha() {
    for b in Config::default().ui.langbar.badges {
        assert!(b.color_light != "auto" && b.color_dark != "auto");
        assert_eq!((b.alpha_light, b.alpha_dark), (None, None));
    }
}

#[test]
fn serde_default_of_font_size_matches_default_impl() {
    let c: UiCandidateConfig = toml::from_str("").unwrap();
    assert_eq!(c.font_size, UiCandidateConfig::default().font_size);
}

#[test]
fn grid_expand_defaults_off_and_cell_truncation_takes_the_tighter_limit() {
    let cfg = Config::default();
    assert!(
        !cfg.ui.candidate.grid_expand,
        "出厂关：只有设置里打开的用户才会展开"
    );
    assert_eq!(cfg.ui.candidate.grid_rows(), 5);
    assert!(cfg.ui.candidate.grid_pageupdown_screen);
    let mut c = cfg.ui.candidate.clone();
    c.max_chars = 16;
    c.grid_cell_max_chars = 3;
    assert_eq!(c.truncate_grid_cell("一二三四"), "一二三…");
    assert_eq!(c.truncate_grid_cell("一二三"), "一二三");
    c.grid_cell_max_chars = 0;
    c.max_chars = 2;
    assert_eq!(
        c.truncate_grid_cell("一二三"),
        "一二…",
        "0 = 只受 max_chars 约束"
    );
    c.grid_rows = 99;
    assert_eq!(c.grid_rows(), 10, "可见行数夹进合法范围");
}

#[test]
fn comment_above_defaults_off_and_round_trips() {
    let cfg = Config::default();
    assert!(!cfg.ui.candidate.comment_above);
    let v: Config = toml::from_str("[ui.candidate]\ncomment_above = true\n").unwrap();
    assert!(v.ui.candidate.comment_above);
}

/// `comment_max_chars`（横竖共用）→ 两个方向各一份。配过非 0 值的用户升级后
/// 注释必须仍按原长度截断，否则表现是「候选栏莫名变宽」，无人会报 bug。
#[test]
fn migrate_comment_max_chars_copies_into_both_directions() {
    let mut v: toml::Value = toml::from_str("[ui.candidate]\ncomment_max_chars = 12\n").unwrap();
    Config::migrate_comment_max_chars_value(&mut v);
    let cand = v.get("ui").unwrap().get("candidate").unwrap();
    assert_eq!(
        cand.get("comment_max_chars_vertical").unwrap().as_integer(),
        Some(12)
    );
    assert_eq!(
        cand.get("comment_max_chars_horizontal")
            .unwrap()
            .as_integer(),
        Some(12)
    );

    // 幂等：每次启动都跑一遍，第二次不得再改。
    let before = v.clone();
    Config::migrate_comment_max_chars_value(&mut v);
    assert_eq!(v, before, "二次迁移须无变化");
}

/// 用户已显式写了新键时，旧键不得覆盖它——新键是更明确的意图。
///
/// 同时钉住「只补缺失的那一侧」：只写了竖排的用户，横排仍该从旧键拿到值。
#[test]
fn migrate_comment_max_chars_keeps_explicit_new_keys() {
    let mut v: toml::Value =
        toml::from_str("[ui.candidate]\ncomment_max_chars = 12\ncomment_max_chars_vertical = 30\n")
            .unwrap();
    Config::migrate_comment_max_chars_value(&mut v);
    let cand = v.get("ui").unwrap().get("candidate").unwrap();
    assert_eq!(
        cand.get("comment_max_chars_vertical").unwrap().as_integer(),
        Some(30),
        "显式新键不被旧键盖掉"
    );
    assert_eq!(
        cand.get("comment_max_chars_horizontal")
            .unwrap()
            .as_integer(),
        Some(12),
        "缺失的那一侧仍从旧键补齐"
    );
}

/// 迁移摘掉旧键：写盘路径先迁再写，把一侧改回 0（出厂值 ⇒ 新键被删）后旧键不会再把它补回。
#[test]
fn migrate_comment_max_chars_drops_old_key_so_reset_to_zero_sticks() {
    let mut root: toml::Value = toml::from_str("[ui.candidate]\ncomment_max_chars = 12\n").unwrap();
    // set_user_value 的顺序：先迁移，再按「等于出厂即删」写入竖排 0。
    Config::migrate_comment_max_chars_value(&mut root);
    assert_eq!(prune_retired(&mut root), 0, "迁移已摘掉旧键，清单无事可做");
    let toml::Value::Table(t) = &mut root else {
        unreachable!()
    };
    remove_nested(t, &["ui", "candidate", "comment_max_chars_vertical"]);
    let cfg = merged_with(&toml::to_string(&root).unwrap());
    assert_eq!(
        cfg.ui.candidate.comment_max_chars_vertical, 0,
        "改回 0 不被打回"
    );
    assert_eq!(cfg.ui.candidate.comment_max_chars_horizontal, 12);
    // 反向：只清不迁会丢设置——这正是 prune 必须先落盘迁移的理由。
    let mut raw: toml::Value = toml::from_str("[ui.candidate]\ncomment_max_chars = 12\n").unwrap();
    assert_eq!(prune_retired(&mut raw), 1);
    let lost = merged_with(&toml::to_string(&raw).unwrap());
    assert_eq!(lost.ui.candidate.comment_max_chars_horizontal, 0);
}

#[test]
fn pinyin_global_config_defaults() {
    let c = Config::default();
    assert!(c.schema.pinyin.use_smart_compose);
    assert_eq!(c.schema.pinyin.separator, "auto");
    assert!(!c.schema.pinyin.fuzzy.enabled);
    assert!(!c.schema.pinyin.fuzzy.zh_z);
    assert!(!c.schema.pinyin.fuzzy.un_ong, "un_ong 出厂关");
    assert!(!c.schema.pinyin.fuzzy.eng_ong, "eng_ong 出厂关");
    // 单写一项只开那一项，另一项保持出厂关。
    let c = merged_with("[schema.pinyin.fuzzy]\nun_ong = true\n");
    assert!(c.schema.pinyin.fuzzy.un_ong);
    assert!(!c.schema.pinyin.fuzzy.eng_ong);
}

#[test]
fn correction_defaults() {
    let c = Config::default().input.correction;
    assert!(c.enabled);
    assert_eq!(c.marker, "*");
    assert!(c.rules.on_ong && c.rules.vowel_swap);
    assert!(
        !c.rules.gn_ng
            && !c.rules.legacy_spelling
            && !c.rules.h_misplaced
            && !c.rules.mg_ng
            && !c.rules.n_before_g
            && !c.rules.ai_ia
    );
    assert!(c.custom.is_empty());
    assert!(!c.hint.enabled && c.hint.fuzzy && c.hint.misread && !c.hint.correction);
    assert_eq!(c.hint.scope, HintScope::First);
    assert!(!c.hint.tone);
}

#[test]
fn bad_hint_scope_falls_back_to_first() {
    let c: Config = toml::from_str("[input.correction.hint]\nscope = \"bogus\"").unwrap();
    assert_eq!(c.input.correction.hint.scope, HintScope::First);
}

#[test]
fn correction_hint_parses_scope_and_tone() {
    let c = merged_with("[input.correction.hint]\nenabled = true\nscope = \"all\"\ntone = true\n");
    let h = c.input.correction.hint;
    assert!(h.enabled && h.tone);
    assert_eq!(h.scope, HintScope::All);
    assert!(h.fuzzy, "未写的键保持出厂值");
}

/// `auto_learn.max_word_length` 的默认值两条路（`AutoLearnConfig::default()` 与 serde 缺键）
/// 都得是 10，所以 `AutoLearnConfig` 不能 `derive(Default)`。
///
/// 拼音造词分支在协调器的 headless 测试里执行不到（`is_pinyin()` 恒 false），这条只能在本 crate 钉。
#[test]
fn auto_learn_max_word_length_default_is_consistent() {
    assert_eq!(
        AutoLearnConfig::default().max_word_length,
        10,
        "代码默认（Default impl）"
    );
    assert_eq!(
        Config::default().schema.pinyin.auto_learn.max_word_length,
        10,
        "经 PinyinGlobal::default() 传导后仍是 10"
    );
    // 配置里写了 [auto_learn] 段但没写本键 → serde default 兜底，仍是 10。
    let c = merged_with("[schema.pinyin.auto_learn]\nenabled = true\n");
    assert_eq!(
        c.schema.pinyin.auto_learn.max_word_length, 10,
        "缺键应走 serde default，而非零值"
    );
    assert!(c.schema.pinyin.auto_learn.enabled, "同段其它键正常覆盖");
    // 显式配置可覆盖；0 = 不限（保留旧行为的逃生舱）。
    let c = merged_with("[schema.pinyin.auto_learn]\nmax_word_length = 0\n");
    assert_eq!(c.schema.pinyin.auto_learn.max_word_length, 0);
}

#[test]
fn pinyin_global_merge_partial() {
    // 仅覆盖 [schema.pinyin.fuzzy] 的 enabled 和 zh_z，其余字段应保留默认值（深合并验证）
    let c = merged_with("[schema.pinyin.fuzzy]\nenabled = true\nzh_z = true\n");
    // 被覆盖字段：变为 true
    assert!(c.schema.pinyin.fuzzy.enabled, "enabled 应被覆盖为 true");
    assert!(c.schema.pinyin.fuzzy.zh_z, "zh_z 应被覆盖为 true");
    // 未覆盖的 fuzzy 字段：保留默认 false
    assert!(!c.schema.pinyin.fuzzy.ch_c, "ch_c 未覆盖，应保留默认 false");
    assert!(!c.schema.pinyin.fuzzy.sh_s, "sh_s 未覆盖，应保留默认 false");
    // 未覆盖的 pinyin 顶层字段：保留默认值
    assert!(
        c.schema.pinyin.use_smart_compose,
        "use_smart_compose 未覆盖，应保留默认 true"
    );
    assert_eq!(
        c.schema.pinyin.separator, "auto",
        "separator 未覆盖，应保留默认 auto"
    );
}

#[test]
fn test_keys_defaults() {
    // keys 合并 hotkeys + 选择键，默认值需保留（[keys] 整表缺失走 Default）
    let k = Config::default().keys;
    assert_eq!(k.toggle_mode_keys, vec!["lshift", "rshift"]);
    assert_eq!(k.switch_engine, "ctrl+shift+e");
    assert_eq!(k.select_key_groups, vec!["semicolon_quote"]);
    assert_eq!(k.page_keys, vec!["pageupdown", "minus_equal"]);
    assert_eq!(k.overflow.number_key, "ignore");
}

#[test]
fn test_schema_modes_and_input_groups() {
    let c = Config::default();
    // 模式三件套归 schema
    assert_eq!(c.schema.mix_modes.len(), 1, "默认一个快捷 mix");
    // 引导键的默认值仍挂在实例上（由 normalize 折算进 keys.key_actions）——
    // 放在被折算的一侧，「用户清空」才折算得出空。理由见 default_mix_modes。
    assert_eq!(c.schema.mix_modes[0].trigger_keys, vec!["semicolon"]);
    assert_eq!(c.schema.quick_input.decimal_places, 6);
    // input 子组
    assert!(
        c.input.punct.smart_after_digit,
        "punct.smart_after_digit 默认开"
    );
    assert_eq!(c.input.symbol.smart_timeout_ms, 500);
    assert!(c.input.temp_english.enabled && c.input.temp_english.show_candidates);
    assert_eq!(c.input.url.prefixes.len(), 5);
    // 网址历史是独立于 url.enabled 的第二道开关，且出厂关闭：网址模式本身不落盘，
    // 开了历史才开始记用户打过的网址原文。
    assert!(!c.input.url.history_enabled, "网址历史出厂关闭");
    assert_eq!(c.input.url.history_max, 200);
    assert!(!c.input.email.enabled, "邮箱模式出厂关闭");
    assert!(!c.input.unicode.enabled, "Unicode 码点输入出厂关闭");
    assert_eq!(
        c.input.unicode.prefixes,
        vec!["u+".to_string(), "U+".to_string()],
        "两条前缀各对应一个入口（码表缓冲 / 临英转交），少一条就有一个入口进不去"
    );
    // input.phrase / stats
    assert_eq!(c.input.phrase.min_prefix, 2);
    assert!(c.stats.enabled && c.stats.track_english);
}

#[test]
fn system_preset_without_data_dir_equals_default() {
    let preset = Config::system_preset_value(None).unwrap();
    assert_eq!(preset, toml::Value::try_from(Config::default()).unwrap());
}

#[test]
fn system_preset_applies_config_toml_overrides() {
    let data_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../data"));
    let preset = Config::system_preset_value(Some(data_dir)).unwrap();
    let cfg: Config = preset.try_into().unwrap();
    // config.toml 作为 L2 预置覆盖了空的 code default
    assert_eq!(cfg.schema.active, "wubi86");
}

#[test]
fn test_smart_method_default() {
    let cfg = SymbolConfig::default();
    assert_eq!(cfg.smart_method, SmartMethod::DeleteReplace);
}

#[test]
fn test_smart_method_serde_round_trip() {
    let toml = r#"
smart_mode = true
smart_method = "delete_replace"
"#;
    let cfg: SymbolConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.smart_method, SmartMethod::DeleteReplace);
    assert!(cfg.smart_mode);
}

#[test]
fn test_smart_method_default_when_absent() {
    let toml = r#"smart_mode = true"#;
    let cfg: SymbolConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.smart_method, SmartMethod::DeleteReplace);
}

/// 混输简拼出厂关。这是产品决策：简拼候选「消费整串」会让归属恒判给拼音，
/// `pinyin_only_overflow` 随即拦下超码长顶码。本条是它唯一的钉子。
#[test]
fn mix_pinyin_abbrev_defaults_off() {
    assert!(
        !MixGlobal::default().enable_pinyin_abbrev,
        "简拼出厂应为关；改它前先读 data/config.toml 同名项的注释"
    );
}

/// 第三处同源：serde 字段默认值（写了 `[schema.mix]` 段、没写这一项时取它）必须也是 `false`。
/// `Config::load` 以 `Config::default()` 打底触发不到，但直接反序列化部分配置的调用方会拿到它。
#[test]
fn mix_pinyin_abbrev_serde_default_matches_default() {
    let v: Config = toml::from_str("[schema.mix]\nshow_source_hint = true\n").unwrap();
    assert_eq!(
        v.schema.mix.enable_pinyin_abbrev,
        MixGlobal::default().enable_pinyin_abbrev,
        "serde 默认值与 Default 漂移了"
    );
}

/// 同源守门：L1（`MixGlobal::default()`）与 L2（`data/config.toml`）必须给出同一个值。
/// 漂移时引擎单测跑在现实中不存在的配置下，且 `preset_for_pruning` 判「用户值是否等于默认」
/// 会开始吃用户配置。缺 `data/` 时静默跳过（全仓惯例）。
#[test]
fn mix_pinyin_abbrev_l1_and_l2_agree() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data");
    let path = dir.join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "跳过 mix_pinyin_abbrev_l1_and_l2_agree：{} 不存在",
            path.display()
        );
        return;
    };
    let v: toml::Value = toml::from_str(&text).expect("data/config.toml 应能解析");
    let l2 = v
        .get("schema")
        .and_then(|s| s.get("mix"))
        .and_then(|m| m.get("enable_pinyin_abbrev"))
        .and_then(toml::Value::as_bool)
        .expect("data/config.toml 应显式写出 schema.mix.enable_pinyin_abbrev");
    assert_eq!(
        l2,
        MixGlobal::default().enable_pinyin_abbrev,
        "L1 与 L2 的简拼默认值漂移了"
    );
}

/// 取值守门：出简让全的全局出厂值是关。
///
/// 全局基线是所有码表方案共用的，而「短码首选 = 简码」只对五笔这类前缀式简码成立；
/// 由方案在 `[engine.codetable]` 里自己声明。本条是这项产品决策唯一的钉子。
#[test]
fn short_code_yield_defaults_off_globally() {
    assert_eq!(
        CodetableGlobal::default().short_code_yield_level,
        0,
        "出简让全的全局出厂应为关；要改先读 default_short_code_yield_level 的注释"
    );
}

/// 同源守门：L1（`CodetableGlobal::default()`）与 L2（`data/config.toml`）必须一致，
/// 理由同上面那条简拼守门。缺 `data/` 时静默跳过（全仓惯例）。
#[test]
fn short_code_yield_l1_and_l2_agree() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../data")
        .join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "跳过 short_code_yield_l1_and_l2_agree：{} 不存在",
            path.display()
        );
        return;
    };
    let v: toml::Value = toml::from_str(&text).expect("data/config.toml 应能解析");
    let l2 = v
        .get("schema")
        .and_then(|s| s.get("codetable"))
        .and_then(|c| c.get("short_code_yield_level"))
        .and_then(toml::Value::as_integer)
        .expect("data/config.toml 应显式写出 schema.codetable.short_code_yield_level");
    assert_eq!(
        l2 as usize,
        CodetableGlobal::default().short_code_yield_level,
        "出简让全的 L1 与 L2 默认值漂移了"
    );
}

/// 出厂方案守门：wubi86 不声明出简让全，与第三方码表一样跟随全局。
///
/// 方案级 `Some(_)` 恒覆盖全局，内置方案自带特例会让全局页那一项对它变成摆设，用户却不知道。
/// 论证见 `docs/design/codetable-short-code-yields-full.md` §6.3；方案文件里刻意不留说明。
///
/// 断言走真实反序列化：「删掉 / 注释掉 / 键名写错」都等价于 `None`；判 `is_none()`
/// 是为了让补回任何档位都变红。
#[test]
fn wubi86_schema_does_not_declare_short_code_yield() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../data/schemas")
        .join("wubi86.schema.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "跳过 wubi86_schema_does_not_declare_short_code_yield：{} 不存在",
            path.display()
        );
        return;
    };
    let schema: crate::schema::Schema =
        toml::from_str(&text).expect("wubi86.schema.toml 应能解析成 Schema");
    assert!(
        schema.engine.codetable.short_code_yield_level.is_none(),
        "出厂方案不该自带出简让全档位（实际 {:?}）——它会让全局页那一项对五笔失效。\
             理由见 docs/design/codetable-short-code-yields-full.md §6.3",
        schema.engine.codetable.short_code_yield_level
    );
}

/// 取值守门：单字输入的全局出厂值是关。
///
/// 翻这个值不会有任何别的测试变红——它是产品决策，本条是它唯一的钉子。
#[test]
fn single_char_defaults_off_globally() {
    assert!(
        !CodetableGlobal::default().single_char,
        "码表侧单字输入出厂应为关；要改先读 CodetableGlobal::single_char 的「出厂关」一节"
    );
    assert!(
        !PinyinGlobalConfig::default().single_char,
        "拼音侧同上。★ 两侧刻意是两个字段，但出厂关是同一条产品决策"
    );
}

/// 同源守门：L1（`InputConfig::default()`）与 L2（`data/config.toml`）必须一致。
/// 漂移的后果同上面那条：单测跑在现实中不存在的配置下，且 `preset_for_pruning`
/// 会把用户显式设的值误判成默认而删掉。缺 `data/` 时静默跳过（全仓惯例）。
#[test]
fn single_char_l1_and_l2_agree() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../data")
        .join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "跳过 single_char_l1_and_l2_agree：{} 不存在",
            path.display()
        );
        return;
    };
    let v: toml::Value = toml::from_str(&text).expect("data/config.toml 应能解析");
    let ct = v
        .get("schema")
        .and_then(|s| s.get("codetable"))
        .and_then(|c| c.get("single_char"))
        .and_then(toml::Value::as_bool)
        .expect("data/config.toml 应显式写出 schema.codetable.single_char");
    assert_eq!(
        ct,
        CodetableGlobal::default().single_char,
        "码表侧单字输入的 L1 与 L2 默认值漂移了"
    );
    let py = v
        .get("schema")
        .and_then(|s| s.get("pinyin"))
        .and_then(|c| c.get("single_char"))
        .and_then(toml::Value::as_bool)
        .expect("data/config.toml 应显式写出 schema.pinyin.single_char");
    assert_eq!(
        py,
        PinyinGlobalConfig::default().single_char,
        "拼音侧单字输入的 L1 与 L2 默认值漂移了"
    );
}

/// 出厂方案守门：所有内置方案都不声明单字输入，一律跟随全局（判据同
/// `wubi86_schema_does_not_declare_short_code_yield`）。
///
/// 「五笔常开、拼音不开」是文档与设置页引导的事，不是出厂值的事；想给 wubi86 补 `true`
/// 之前先读 `CodeTableSpec::single_char` 的文档。本字段与引擎类型无关，故遍历全部方案；
/// 判 `is_none()` 让补上任何取值都变红。
#[test]
fn factory_schemas_do_not_declare_single_char() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!(
            "跳过 factory_schemas_do_not_declare_single_char：{} 不存在",
            dir.display()
        );
        return;
    };
    let mut checked = 0usize;
    for e in entries.flatten() {
        let path = e.path();
        if !path.to_string_lossy().ends_with(".schema.toml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let schema: crate::schema::Schema = match toml::from_str(&text) {
            Ok(s) => s,
            // 解析不了是别的测试的地盘（`read_schema` 有段级降级），本条只管字段取值。
            Err(_) => continue,
        };
        assert!(
            schema.engine.codetable.single_char.is_none(),
            "出厂方案 {} 不该自带单字输入（实际 {:?}）——它会让全局页那一项对该方案失效。                 理由见 CodeTableSpec::single_char 的文档",
            path.display(),
            schema.engine.codetable.single_char
        );
        checked += 1;
    }
    // 反向保证：目录改名或后缀判据失效时，本测试不得退化成空跑而静默变绿。
    assert!(checked >= 3, "只扫到 {checked} 个方案文件，判据多半失效了");
}

/// 出厂守门：内置拼音方案不得自带 `[engine.pinyin].separator`。
///
/// 理由同 `wubi86_schema_does_not_declare_short_code_yield`：方案级 `Some(_)` 恒压过全局。
/// 这一项本就是让用户按自己的键位预算覆盖的（全拼用反引号、双拼把反引号留给辅助码）。
#[test]
fn builtin_pinyin_schemas_do_not_declare_separator() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas");
    for id in ["pinyin", "shuangpin"] {
        let path = dir.join(format!("{id}.schema.toml"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("跳过 {id}：{} 不存在", path.display());
            continue;
        };
        let schema: crate::schema::Schema =
            toml::from_str(&text).unwrap_or_else(|e| panic!("{id}.schema.toml 应能解析: {e}"));
        assert!(
            schema.engine.pinyin.separator.is_none(),
            "出厂方案 {id} 不该自带 separator（实际 {:?}）——它会让全局页那一项对该方案失效",
            schema.engine.pinyin.separator
        );
    }
}

/// 取值守门：空码时按标点出厂即丢弃废码，而同族的回车/空格仍是 `commit`。
///
/// 这组不一致是产品决策（见 `default_punct_on_empty_behavior`）。三值一起断言是为了拦
/// 「顺手把三个统一了」，分开写三条的话那种改动只会红一条，看起来像只动了一个值。
#[test]
fn empty_code_behavior_defaults_are_intentionally_asymmetric() {
    let c = InputConfig::default();
    assert_eq!(
        c.punct_on_empty_behavior, "clear",
        "标点出厂应丢弃废码；要改先读 default_punct_on_empty_behavior 的注释"
    );
    assert_eq!(
        c.enter_behavior, "commit",
        "回车须保留上屏原码——它是「我就要这串原码」的唯一出口，别为了统一把它一起改了"
    );
    assert_eq!(
        c.space_on_empty_behavior, "commit",
        "空格暂随回车；改它会静默改写「曾把它设成当时默认值」的用户，须先写 changelog"
    );
}

/// 同源守门：L1（`InputConfig::default()`）与 L2（`data/config.toml`）必须给出同一个值，
/// 理由同 `MixGlobal` 那条。缺 `data/` 时静默跳过（全仓惯例）。
#[test]
fn empty_code_behavior_l1_and_l2_agree() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../data")
        .join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "跳过 empty_code_behavior_l1_and_l2_agree：{} 不存在",
            path.display()
        );
        return;
    };
    let v: toml::Value = toml::from_str(&text).expect("data/config.toml 应能解析");
    let l1 = InputConfig::default();
    for (key, expected) in [
        ("enter_behavior", l1.enter_behavior.as_str()),
        (
            "space_on_empty_behavior",
            l1.space_on_empty_behavior.as_str(),
        ),
        (
            "punct_on_empty_behavior",
            l1.punct_on_empty_behavior.as_str(),
        ),
    ] {
        let l2 = v
            .get("input")
            .and_then(|i| i.get(key))
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("data/config.toml 应显式写出 input.{key}"));
        assert_eq!(l2, expected, "input.{key} 的 L1 与 L2 默认值漂移了");
    }
}

#[test]
fn top_commit_mode_default_is_direct_commit() {
    let c = InputConfig::default();
    assert_eq!(c.top_commit_mode, TopCommitMode::DirectCommit);
}

#[test]
fn top_commit_mode_serde_round_trip() {
    let toml = r#"top_commit_mode = "direct_commit""#;
    let cfg: InputConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.top_commit_mode, TopCommitMode::DirectCommit);
}

#[test]
fn top_commit_mode_absent_defaults_direct_commit() {
    let cfg: InputConfig = toml::from_str("").unwrap();
    assert_eq!(cfg.top_commit_mode, TopCommitMode::DirectCommit);
}

/// 双击候选窗截图：出厂关，空表反序列化与 Default 一致。
#[test]
fn candidate_double_click_screenshot_defaults_off() {
    let c: UiCandidateConfig = toml::from_str("").unwrap();
    assert!(!c.double_click_screenshot);
    assert!(!UiCandidateConfig::default().double_click_screenshot);
}

/// 工具栏自动隐藏：默认关、超时 5 秒；空表反序列化与 Default 一致。
#[test]
fn toolbar_auto_hide_defaults() {
    let tb: ToolbarConfig = toml::from_str("").unwrap();
    assert!(!tb.auto_hide);
    assert_eq!(tb.auto_hide_delay, 5);
    let d = ToolbarConfig::default();
    assert!(!d.auto_hide);
    assert_eq!(d.auto_hide_delay, 5);
}

/// 「英文状态隐藏工具栏」出厂关、「悬停唤回」出厂关（自动隐藏的附加选项）；空表反序列化与 Default 一致。
#[test]
fn toolbar_english_and_hover_reveal_defaults() {
    let tb: ToolbarConfig = toml::from_str("").unwrap();
    assert!(!tb.hide_in_english);
    assert!(!tb.auto_hide_hover_reveal);
    let d = ToolbarConfig::default();
    assert!(!d.hide_in_english);
    assert!(!d.auto_hide_hover_reveal);
}

/// `rare_char` 必须解析成生僻字模式，且未知动词仍回落 None——`BoundAction::parse` 的契约
/// 是未知值不静默变成别的功能（例如被写成前缀匹配后 `rare_char_xxx` 也命中）。
#[test]
fn rare_char_bound_action_parses() {
    assert_eq!(BoundAction::parse("rare_char"), BoundAction::RareChar);
    assert_eq!(BoundAction::parse("rare_charx"), BoundAction::None);
    assert_eq!(BoundAction::parse("rare"), BoundAction::None);
    assert_eq!(BoundAction::parse(""), BoundAction::None);
}
