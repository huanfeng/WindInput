use super::*;
use std::collections::BTreeSet;

/// 注册表里的值域与 `NewlineStyle` 的变体集是两处独立事实，必须一致；
/// 否则设置端选的一档会被静默当成另一档（同 `EMPTY_CODE_BEHAVIOR_VALUES`）。
#[test]
fn newline_style_values_match_enum() {
    use crate::app_compat::NewlineStyle;
    for v in NEWLINE_STYLE_VALUES {
        assert!(
            NewlineStyle::from_config(v).is_some(),
            "注册表登记了 {v:?}，但 NewlineStyle 不认得它"
        );
    }
    // 反方向：枚举新增变体后，注册表不能漏登记。
    for style in [
        NewlineStyle::Keep,
        NewlineStyle::Cr,
        NewlineStyle::Lf,
        NewlineStyle::Crlf,
    ] {
        assert!(
            NEWLINE_STYLE_VALUES.contains(&style.as_config()),
            "NewlineStyle::{style:?} 没有登记进 NEWLINE_STYLE_VALUES"
        );
    }
}

/// 跨仓契约：`Float` 键必须吃得下 TOML 整数。
///
/// wind-setting 写回整数值的浮点时会落成 `72` 而非 `72.0`，两边没有编译期约束；
/// 不匹配的表现是「设置页填了值，重启后回到默认」的静默失效。
#[test]
fn float_fields_accept_integer_toml_values() {
    use crate::config::{CodetableFrequency, PinyinFrequency};
    let ct: CodetableFrequency =
        toml::from_str("half_life = 72").expect("码表 half_life 必须接受 TOML 整数");
    assert_eq!(ct.half_life, 72.0);
    let py: PinyinFrequency =
        toml::from_str("half_life = 72").expect("拼音 half_life 必须接受 TOML 整数");
    assert_eq!(py.half_life, 72.0);
    // 反向对照：小数形式当然也要能读，否则上面那条可能只是「两边都坏」。
    let ct2: CodetableFrequency = toml::from_str("half_life = 4.5").unwrap();
    assert_eq!(ct2.half_life, 4.5);
}

/// L1↔L2 同源：`ui.toolbar.items` 的出厂值在 Rust 默认值与 `data/config.toml` 里
/// 必须逐项相同（`config-design-rules.md` §R4）。顺序即渲染顺序，所以不能只比集合。
///
/// 放在本 crate 是因为只有这里读得到 L2（`data_config_toml()`）；协调器侧同名意图的
/// 测试只覆盖 L1。
#[test]
fn toolbar_items_l1_matches_l2() {
    let l1 = crate::Config::default().ui.toolbar.items;
    let l2: Vec<String> = data_config_toml()
        .get("ui")
        .and_then(|u| u.get("toolbar"))
        .and_then(|t| t.get("items"))
        .and_then(|v| v.as_array())
        .expect("data/config.toml 缺少 ui.toolbar.items")
        .iter()
        .map(|v| v.as_str().expect("items 元素须为字符串").to_string())
        .collect();
    assert_eq!(l1, l2, "ui.toolbar.items 的 L1 默认值与 L2 出厂文件不一致");
    // 两侧都必须是登记过的键。先剥 `-` 前缀：那是值域自带的语法（「在这个位置但不显示」），
    // 出厂的 `-s2t` 是合法条目，按裸键名比会误报。
    for k in &l2 {
        let key = k.strip_prefix('-').unwrap_or(k);
        assert!(
            crate::TOOLBAR_ITEM_KEYS.contains(&key),
            "出厂 items 含未登记条目 {k:?}"
        );
    }
}

/// L1↔L2 同源：`ui.tooltip.sections` 的出厂段列表在 Rust 默认值与 `data/config.toml` 里
/// 必须逐段、逐字段相同（`config-design-rules.md` §R4）。数组在预置文件里是整体覆盖，
/// 不一致即「恢复默认」与新装用户看到的气泡不同。
#[test]
fn tooltip_sections_l1_matches_l2() {
    let l1 = crate::Config::default().ui.tooltip.sections;
    let l2: Vec<crate::config::TooltipSection> = data_config_toml()
        .get("ui")
        .and_then(|u| u.get("tooltip"))
        .and_then(|t| t.get("sections"))
        .cloned()
        .expect("data/config.toml 缺少 ui.tooltip.sections")
        .try_into()
        .expect("ui.tooltip.sections 须能反序列化");
    assert_eq!(
        l1, l2,
        "ui.tooltip.sections 的 L1 默认值与 L2 出厂文件不一致"
    );
}

/// L1↔L2 同源：气泡的单行上限与折行宽度（取值守门在 config.rs `test_tooltip_defaults`，
/// 行为守门在协调器 `tooltip` 模块的出厂配置用例）。
#[test]
fn tooltip_limits_l1_matches_l2() {
    let l1 = crate::Config::default().ui.tooltip;
    let tip = data_config_toml()
        .get("ui")
        .and_then(|u| u.get("tooltip"))
        .cloned()
        .expect("data/config.toml 缺少 [ui.tooltip]");
    let int = |k: &str| {
        tip.get(k)
            .and_then(toml::Value::as_integer)
            .unwrap_or_else(|| panic!("data/config.toml 缺少 ui.tooltip.{k}"))
    };
    assert_eq!(int("max_chars"), l1.max_chars as i64);
    assert_eq!(int("wrap_width"), l1.wrap_width as i64);
}

/// `[ui.<table>]` 下某个布尔键在 L2 里的值；缺键直接红。
fn l2_bool(table: &str, key: &str) -> bool {
    data_config_toml()
        .get("ui")
        .and_then(|u| u.get(table))
        .and_then(|t| t.get(key))
        .and_then(toml::Value::as_bool)
        .unwrap_or_else(|| panic!("data/config.toml 缺少布尔键 ui.{table}.{key}"))
}

/// L1↔L2 同源：悬停提示总开关（出厂开，设计 memory-footprint.md §9 D2「零回归」）。
#[test]
fn tooltip_enabled_l1_matches_l2() {
    let l1 = crate::Config::default().ui.tooltip.enabled;
    assert!(l1, "ui.tooltip.enabled 出厂应为 true");
    assert_eq!(l2_bool("tooltip", "enabled"), l1);
}

/// L1↔L2 同源：`[ui.comment]` 的总开关、当前态、切换键与切换方式。
#[test]
fn ui_comment_l1_matches_l2() {
    let l1 = crate::Config::default().ui.comment;
    assert!(l1.enabled, "ui.comment.enabled 出厂应为 true");
    assert_eq!(l2_bool("comment", "enabled"), l1.enabled);
    let comment = data_config_toml()
        .get("ui")
        .and_then(|u| u.get("comment"))
        .cloned()
        .expect("data/config.toml 缺少 [ui.comment]");
    let l2: crate::comment_lib::UiCommentConfig =
        comment.try_into().expect("[ui.comment] 须能反序列化");
    assert_eq!(l2, l1, "[ui.comment] 的 L1 默认值与 L2 出厂文件不一致");
}

/// L1↔L2 同源：方案常驻策略（出厂关，设计 memory-footprint.md §9 D1）。
#[test]
fn keep_all_loaded_l1_matches_l2() {
    let l1 = crate::Config::default().schema.keep_all_loaded;
    assert!(!l1, "schema.keep_all_loaded 出厂应为 false");
    let l2 = data_config_toml()
        .get("schema")
        .and_then(|t| t.get("keep_all_loaded"))
        .and_then(toml::Value::as_bool)
        .expect("data/config.toml 缺少布尔键 schema.keep_all_loaded");
    assert_eq!(l2, l1);
}

/// L1↔L2 同源：词库缓存策略（出厂锁定）。
#[test]
fn dict_cache_policy_l1_matches_l2() {
    let l1 = crate::Config::default().schema.dict_cache_policy;
    assert_eq!(l1, "lock", "schema.dict_cache_policy 出厂应为 lock");
    let l2 = data_config_toml()
        .get("schema")
        .and_then(|t| t.get("dict_cache_policy"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .expect("data/config.toml 缺少字符串键 schema.dict_cache_policy");
    assert_eq!(l2, l1);
}

/// 出厂 `keys.key_actions` 里的动词必须真的编译得进热键表。
///
/// 动词值域散在三处：`BoundAction::parse`、`hotkey::hotkey_action_entry`（组合键白名单）、
/// 协调器的分派臂。只加第一处时出厂热键按下无反应，与「没绑」同形，只在日志留一行
/// `组合键不支持动词 ... 忽略`。
#[test]
fn factory_combo_key_actions_are_accepted_by_the_hotkey_compiler() {
    let Some(actions) = data_config_toml()
        .get("keys")
        .and_then(|k| k.get("key_actions"))
        .and_then(|v| v.as_table())
        .cloned()
    else {
        return; // 出厂没配 key_actions 时无可校验
    };
    for (key, action) in &actions {
        let action = action.as_str().expect("key_actions 的值须为字符串").trim();
        if action.is_empty() {
            continue;
        }
        // 只校验走组合键通路的那些；单键与纯修饰键有各自的消费者。
        if crate::hotkey::route_of_key_action(key) != Some(crate::hotkey::KeyActionRoute::Hotkey) {
            continue;
        }
        assert!(
            crate::hotkey::hotkey_action_entry(action).is_some(),
            "出厂 keys.key_actions 的 {key:?} = {action:?} 不被组合键白名单接受，                 按下去会静默无反应（见 hotkey_action_entry）"
        );
    }
}

/// 出厂 `[keys]` 段里的热键串必须真的解析得动。
///
/// 写错一个字时 `parse_hotkey` 返回 `None`，那条静默不进热键表，日志无任何痕迹。
/// 判据自动排除模板与非热键串：L1 默认值解析得动的字段，L2 也必须解析得动
/// （`pin_candidate = "ctrl+number"` 这类模板在 L1 就解析不动，自动跳过），新字段自动纳入。
#[test]
fn factory_keys_hotkeys_are_parsable() {
    let l1 = toml::Value::try_from(Config::default()).expect("默认配置应可序列化");
    let l2 = data_config_toml();
    let (Some(l1k), Some(l2k)) = (
        l1.get("keys").and_then(toml::Value::as_table),
        l2.get("keys").and_then(toml::Value::as_table),
    ) else {
        panic!("L1 与 data/config.toml 都该有 [keys] 段");
    };
    let mut checked = 0usize;
    for (name, l2v) in l2k {
        let (Some(s2), Some(s1)) = (l2v.as_str(), l1k.get(name).and_then(toml::Value::as_str))
        else {
            continue;
        };
        if crate::hotkey::parse_hotkey(s1).is_none() {
            continue; // L1 那份就不是可解析的热键串（模板 / 非热键字段）
        }
        checked += 1;
        assert!(
            crate::hotkey::parse_hotkey(s2).is_some(),
            "data/config.toml 的 keys.{name} = {s2:?} 解析不动，出厂绑定会静默失效"
        );
    }
    // 一条都没校验到 = 判据本身失效（字段改名 / 段结构变了），那时它会假绿。
    assert!(
        checked >= 5,
        "只校验到 {checked} 个热键字段，判据可能已失效"
    );
}

/// 解析仓库内系统预置 `data/config.toml`。
fn data_config_toml() -> toml::Value {
    // CARGO_MANIFEST_DIR = <repo>/wind_input/crates/wind-config
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../data/config.toml");
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("读取 {path} 失败（Stage1 注册表对照需要）: {e}"));
    toml::from_str(&content).expect("data/config.toml 解析失败")
}

fn leaf_keys_of(value: &toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_leaf_keys("", value, &mut out);
    out.sort();
    out.dedup();
    out
}

#[test]
fn leaf_keys_drills_tables_and_keeps_arrays_maps_as_leaves() {
    let keys: BTreeSet<String> = config_leaf_keys().into_iter().collect();
    // 标量叶子
    assert!(keys.contains("ui.candidate.per_page"), "应含标量叶子");
    // 嵌套标量
    assert!(
        keys.contains("keys.overflow.number_key"),
        "应含嵌套标量叶子"
    );
    // 空 HashMap 作叶子保留
    assert!(
        keys.contains("input.punct.custom_mappings"),
        "空表(custom_mappings)应作叶子保留"
    );
    // 数组作叶子（不下钻元素）
    assert!(keys.contains("schema.available"), "数组应作叶子");
    assert!(
        keys.contains("schema.mix_modes"),
        "结构体数组应作单一叶子，不展开元素"
    );
    // 中间表不应作为叶子出现
    assert!(!keys.contains("ui.candidate"), "中间表不应是叶子");
    assert!(!keys.contains("ui"), "顶层表不应是叶子");
}

#[test]
fn leaf_entries_flattens_table_to_key_value_pairs() {
    let v: toml::Value = toml::from_str(
        "[ui.candidate]\nper_page = 9\nlayout = \"vertical\"\n[input.auto_pair]\nchinese = false\n",
    )
    .unwrap();
    let entries = leaf_entries(&v);
    assert!(
        entries
            .iter()
            .any(|(k, val)| k == "ui.candidate.per_page" && val.as_integer() == Some(9))
    );
    assert!(
        entries
            .iter()
            .any(|(k, val)| k == "ui.candidate.layout" && val.as_str() == Some("vertical"))
    );
    assert!(
        entries
            .iter()
            .any(|(k, val)| k == "input.auto_pair.chinese" && val.as_bool() == Some(false))
    );
    // 不应出现中间表键
    assert!(!entries.iter().any(|(k, _)| k == "ui.candidate"));
}

#[test]
fn registry_covers_every_config_key() {
    let struct_keys: BTreeSet<String> = config_leaf_keys().into_iter().collect();
    let registry_keys: BTreeSet<String> = registry().iter().map(|f| f.key.to_string()).collect();

    let missing: Vec<&String> = struct_keys.difference(&registry_keys).collect();
    let extra: Vec<&String> = registry_keys.difference(&struct_keys).collect();

    assert!(
        missing.is_empty() && extra.is_empty(),
        "注册表与 Config 不一致：\n  注册表缺失({}): {:?}\n  注册表多余({}): {:?}",
        missing.len(),
        missing,
        extra.len(),
        extra
    );
}

/// 刻意不写进 `data/config.toml` 的键（配套 [`data_config_toml_covers_registry`]）。
///
/// 二者均为结构体数组（`StructList`），写进预置文件会整体覆盖、把当时的定义冻结成快照，
/// 日后改代码侧默认值全被静默遮蔽；故真相源留在 `config.rs` 的 `Default`。
/// 只有「写进去会造成实际危害」才够格加进这里。
const ABSENT_FROM_DATA_CONFIG: &[&str] = &[
    "schema.mix_modes",
    // 注释词库挂载列表：出厂为空数组（词典多有版权，由用户自行放置），写出来没有信息量，
    // 却会与预置层的整表覆盖语义纠缠。格式见文档站 customize/candidate-comment。
    "ui.comment_dicts",
    // 工具栏自定义按钮：同上，出厂为空数组。格式与示例写在 `[ui.toolbar]` 段的注释里。
    "ui.toolbar.buttons",
    // 候选注释的用户模板与分配：用户结构化内容，出厂为空。出厂模板与分配放在
    // `system.comments.toml`——config.toml 的表逐键深合并，写进这里的出厂行会在用户
    // 删掉后下次加载又被合并回来。
    "ui.comment.templates",
    "ui.comment.assign.global",
    "ui.comment.assign.type",
    "ui.comment.assign.schema",
    "ui.comment.assign.mode",
    // 扩展列表：同上，出厂为空数组，条目随用户安装的扩展增减。
    "extensions.list",
];

/// `data/config.toml` 必须显式列出注册表里的每一个键（豁免名单除外）。
///
/// 与 [`data_config_toml_has_no_orphan_keys`] 互为反向：那个管「不许多」，这个管「不许少」；
/// 缺了这一侧，新增配置项时漏写预置文件不会被发现。
#[test]
fn data_config_toml_covers_registry() {
    let toml_keys: BTreeSet<String> = leaf_keys_of(&data_config_toml()).into_iter().collect();
    let missing: Vec<&str> = registry()
        .iter()
        .map(|f| f.key)
        .filter(|k| !toml_keys.contains(*k) && !ABSENT_FROM_DATA_CONFIG.contains(k))
        .collect();
    assert!(
        missing.is_empty(),
        "data/config.toml 缺少 {} 个已登记键（预置文件应完整列出全部可配置项，\
             既是出厂默认值也是说明书）: {:?}",
        missing.len(),
        missing
    );
}

/// 豁免名单自身不能腐烂：里面的键必须仍在注册表中，且确实没写进预置文件。
/// 否则删掉某个字段后，名单会留下一条永远为真的死条目。
#[test]
fn absent_allowlist_stays_accurate() {
    let toml_keys: BTreeSet<String> = leaf_keys_of(&data_config_toml()).into_iter().collect();
    for key in ABSENT_FROM_DATA_CONFIG {
        assert!(is_known_key(key), "豁免名单含未登记键（已删字段？）: {key}");
        assert!(
            !toml_keys.contains(*key),
            "{key} 已写进 data/config.toml，应从豁免名单移除"
        );
    }
}

#[test]
fn data_config_toml_has_no_orphan_keys() {
    let struct_keys: BTreeSet<String> = config_leaf_keys().into_iter().collect();
    let toml_keys = leaf_keys_of(&data_config_toml());
    let orphans: Vec<&String> = toml_keys
        .iter()
        .filter(|k| !struct_keys.contains(*k))
        .collect();
    assert!(
        orphans.is_empty(),
        "data/config.toml 含 {} 个孤立键（struct 无对应字段，会被静默丢弃）: {:?}",
        orphans.len(),
        orphans
    );
}

#[test]
fn field_lookup_finds_registered_key_and_rejects_unknown() {
    let f = field("ui.candidate.layout").expect("已登记键应查到");
    assert_eq!(f.key, "ui.candidate.layout");
    assert!(matches!(f.ty, FieldType::Enum(_)));
    assert!(is_known_key("keys.overflow.number_key"));
    assert!(!is_known_key("ui.candidate.bogus"), "未登记键应返回 None");
    assert!(!is_known_key("totally.made.up"));
}

/// 守卫：core 内部 setter 硬编码的 key 路径必须都在注册表中（防拼写/漂移）。
/// 新增 `Config::set_user_*` 调用点时，把其路径加到这里。
#[test]
fn internal_setter_paths_are_registered() {
    const INTERNAL_PATHS: &[&str] = &[
        "schema.active",
        "ui.theme.style",
        "ui.theme.name",
        "ui.candidate.preedit_display",
        "ui.toolbar.visible",
    ];
    for p in INTERNAL_PATHS {
        assert!(is_known_key(p), "内部 setter 路径未在注册表登记: {p}");
    }
}

#[test]
fn validate_accepts_correct_types() {
    assert!(validate("ui.candidate.per_page", &toml::Value::Integer(9)).is_ok());
    assert!(validate("ui.candidate.hide_window", &toml::Value::Boolean(true)).is_ok());
    assert!(
        validate(
            "ui.candidate.layout",
            &toml::Value::String("vertical".into())
        )
        .is_ok()
    );
    // Float 字段接受整数值（宽松）
    assert!(validate("ui.candidate.font_size", &toml::Value::Integer(18)).is_ok());
    assert!(validate("ui.candidate.font_size", &toml::Value::Float(18.0)).is_ok());
    // Enum 允许空串成员（pager_bar_display 含 ""）
    assert!(
        validate(
            "ui.candidate.pager_bar_display",
            &toml::Value::String("".into())
        )
        .is_ok()
    );
    // 数组 / 表
    assert!(
        validate(
            "schema.available",
            &toml::Value::Array(vec![toml::Value::String("wubi86".into())])
        )
        .is_ok()
    );
}

#[test]
fn validate_rejects_unknown_key() {
    assert_eq!(
        validate("ui.candidate.bogus", &toml::Value::Integer(1)),
        Err(ValidateError::UnknownKey)
    );
}

#[test]
fn validate_rejects_type_mismatch() {
    let r = validate(
        "ui.candidate.per_page",
        &toml::Value::String("seven".into()),
    );
    assert!(
        matches!(r, Err(ValidateError::TypeMismatch { .. })),
        "{r:?}"
    );
    let r2 = validate("ui.candidate.hide_window", &toml::Value::Integer(1));
    assert!(
        matches!(r2, Err(ValidateError::TypeMismatch { .. })),
        "{r2:?}"
    );
}

#[test]
fn validate_rejects_enum_out_of_range() {
    let r = validate(
        "ui.candidate.layout",
        &toml::Value::String("diagonal".into()),
    );
    assert!(
        matches!(r, Err(ValidateError::EnumOutOfRange { .. })),
        "{r:?}"
    );
}

#[test]
fn validate_rejects_strlist_with_non_string_element() {
    let r = validate(
        "schema.available",
        &toml::Value::Array(vec![toml::Value::Integer(1)]),
    );
    assert!(
        matches!(r, Err(ValidateError::TypeMismatch { .. })),
        "{r:?}"
    );
}

#[test]
fn registry_types_match_default_values() {
    let default = toml::Value::try_from(Config::default()).unwrap();
    for field in registry() {
        let value = navigate(&default, field.key)
            .unwrap_or_else(|| panic!("默认配置缺少注册表声明的键: {}", field.key));
        assert!(
            type_matches(field.ty, value),
            "键 {} 声明类型 {:?} 与默认值实际类型不符: {:?}",
            field.key,
            field.ty,
            value
        );
    }
}

/// 按点分路径在 TOML 表中导航取值。
fn navigate<'a>(root: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    let mut cur = root;
    for part in key.split('.') {
        cur = cur.as_table()?.get(part)?;
    }
    Some(cur)
}

fn type_matches(ty: FieldType, value: &toml::Value) -> bool {
    match ty {
        FieldType::Bool => value.is_bool(),
        FieldType::Int => value.is_integer(),
        FieldType::Float => value.is_float(),
        FieldType::Str | FieldType::Enum(_) => value.is_str(),
        // 可分档枚举：标签式是字符串，表写法是表，两种都合法。
        FieldType::LayoutEnum(_) => value.is_str() || value.is_table(),
        FieldType::StrList | FieldType::StructList => value.is_array(),
        FieldType::Map(_) => value.is_table(),
    }
}

/// data/config.toml 每个已登记叶子键的值，必须通过 registry 校验（类型 / enum 合法）。
/// 注意：config.toml 作为系统预置可合法覆盖 code default，故此处只校验「合法」而非「等于默认」。
#[test]
fn data_config_toml_values_pass_validation() {
    let toml_val = data_config_toml();
    let mut bad = Vec::new();
    for (key, value) in leaf_entries(&toml_val) {
        // 未登记键由 data_config_toml_has_no_orphan_keys 守护，这里只校验已登记项的值
        if field(&key).is_none() {
            continue;
        }
        if let Err(e) = validate(&key, &value) {
            bad.push(format!("{key}: {e}"));
        }
    }
    assert!(
        bad.is_empty(),
        "data/config.toml 含非法值（类型/enum 不符 registry）:\n{}",
        bad.join("\n")
    );
}

/// [`SCHEMA_OVERRIDES`] 里的每条登记都必须指到真实存在的键（或非空的段）。
///
/// 拦的是键名漂移：全局键改名或删掉后，设置页那一行静默不再提示可被方案覆盖。
/// 拦不住反方向（core 新增方案级字段却没登记），那层对应只存在于 `resolved()` 函数体里。
#[test]
fn schema_overrides_point_at_real_keys() {
    for o in SCHEMA_OVERRIDES {
        match o.key.strip_suffix('.') {
            // 段前缀：该段下至少要有一个已登记的叶子键。
            Some(prefix) => {
                let n = registry()
                    .iter()
                    .filter(|f| {
                        f.key
                            .strip_prefix(prefix)
                            .is_some_and(|r| r.starts_with('.'))
                    })
                    .count();
                assert!(
                    n > 0,
                    "SCHEMA_OVERRIDES 的段前缀 `{}` 在注册表里一个键都没匹配到——\
                         多半是那一段被改名或删了",
                    o.key
                );
            }
            None => assert!(
                is_known_key(o.key),
                "SCHEMA_OVERRIDES 登记了未知键 `{}`（改名或已删除？）",
                o.key
            ),
        }
    }
}

/// 前缀匹配不能误伤同前缀的兄弟段，也不能把段名本身当成叶子键。
#[test]
fn schema_override_prefix_matches_only_that_section() {
    // 段内叶子命中。
    assert!(schema_override_of("schema.codetable.top_code_commit").is_some());
    // 段名本身不是叶子键，不该命中。
    assert!(schema_override_of("schema.codetable").is_none());
    // 同前缀的兄弟段不该被误伤（`schema.codetableX` 与 `schema.codetable` 只差一个字符）。
    assert!(schema_override_of("schema.codetableX.foo").is_none());
    // 精确登记项。
    assert!(schema_override_of("input.punct.custom_enabled").is_some());
    // 同段但没登记的键不该命中（`smart_list` 不在方案级下放范围）。
    assert!(schema_override_of("input.punct.smart_list").is_none());
}

/// 登记的段名必须是方案文件里真实存在的段——它原样进设置页提示，用户照着写；
/// 写错段名的配置会被静默忽略（`Schema` 不拒未知字段）。
#[test]
fn schema_override_sections_exist_in_schema() {
    // 可选子段默认是 None、序列化时不出现，先填上才能在树里找到它们。
    let mut schema = crate::schema::Schema::default();
    schema.engine.codetable.frequency = Some(Default::default());
    schema.engine.codetable.english_merge = Some(Default::default());
    let tree = toml::Value::try_from(schema).expect("序列化 Schema");
    for o in SCHEMA_OVERRIDES {
        let path = o
            .section
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or_else(|| panic!("{} 的段名 {:?} 不是 [a.b] 形式", o.key, o.section));
        let mut node = &tree;
        for seg in path.split('.') {
            node = node.get(seg).unwrap_or_else(|| {
                panic!(
                    "{} 登记的段 {} 在 Schema 里不存在（断在 {seg}）",
                    o.key, o.section
                )
            });
        }
        assert!(node.is_table(), "{} 登记的段 {} 不是表", o.key, o.section);
    }
}

/// 段前缀不递归：子段要么自己登记，要么不该被标记。
///
/// 递归前缀会把 `schema.codetable.auto_phrase.*` 一并标成可覆盖，而
/// `CodetableGlobal::resolved` 根本不读它；标错比漏标更糟，用户会写一段不会被读的配置。
#[test]
fn section_prefix_does_not_leak_into_subsections() {
    // 登记了的子段：命中，且命中的是子段那一条（提示文案要说 [engine.codetable.frequency]）。
    let hit = schema_override_of("schema.codetable.frequency.half_life")
        .expect("frequency 子段已单独登记，应命中");
    assert_eq!(hit.section, "[engine.codetable.frequency]");
    // 没登记的子段：绝不能因为父段登记了就跟着被标。
    for k in [
        "schema.codetable.auto_phrase.enabled",
        "schema.codetable.auto_phrase.min_phrase_len",
        "schema.codetable.auto_phrase.temp_max_entries",
    ] {
        assert!(
            schema_override_of(k).is_none(),
            "{k} 没有方案级形态（resolved 不折叠 auto_phrase），不该被标记"
        );
    }
}
/// 写盘闸的判据必须按「整值覆盖」而不是按 Map 分类。
///
/// 设置端把数组也整份发送，只认 Map 会漏掉 `ui.toolbar.items` 等数组键：段降级时 base 是
/// 出厂值，用户改一格保存，原有内容永久消失。逐变体断言是为了加新变体时
/// `is_whole_value_leaf` 的穷举 match 逼作者回答「整体换掉还是只动一个值」。
#[test]
fn whole_value_leaf_covers_arrays_not_just_maps() {
    for ty in [
        FieldType::Map(&[]),
        FieldType::StrList,
        FieldType::StructList,
    ] {
        assert!(ty.is_whole_value_leaf(), "{ty:?} 是整值覆盖，必须过写盘闸");
    }
    for ty in [
        FieldType::Bool,
        FieldType::Int,
        FieldType::Float,
        FieldType::Str,
        FieldType::Enum(&[]),
    ] {
        assert!(
            !ty.is_whole_value_leaf(),
            "{ty:?} 是标量，拦它只会让降级期间整个设置页存不了盘"
        );
    }
}

/// 那几个真实的数组型键确实登记着数组类型——否则上一条测试只是在自说自话。
#[test]
fn the_array_keys_that_bit_us_are_registered_as_whole_value() {
    for key in [
        "ui.toolbar.items",
        "ui.toolbar.buttons",
        "ui.langbar.badges",
        "schema.mix_modes",
        "input.punct.custom_mappings",
    ] {
        let f = field(key).unwrap_or_else(|| panic!("{key} 未登记"));
        assert!(
            f.ty.is_whole_value_leaf(),
            "{key}（{:?}）应被写盘闸拦下",
            f.ty
        );
    }
}
