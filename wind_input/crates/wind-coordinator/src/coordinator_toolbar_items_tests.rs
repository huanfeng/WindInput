//! `ui.toolbar.items` → 渲染项序列的解析（纯函数，不构造协调器）。

use super::parse_toolbar_items;
use wind_config::ToolbarButtonSpec;
use wind_ui_types::{DEFAULT_TOOLBAR_ITEMS, ToolbarItem};

fn parse(items: &[&str]) -> Vec<ToolbarItem> {
    parse_with(items, &[])
}

fn parse_with(items: &[&str], buttons: &[ToolbarButtonSpec]) -> Vec<ToolbarItem> {
    let owned: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    parse_toolbar_items(&owned, buttons)
}

/// 一个可用的自定义按钮（各字段可按需覆盖）。
fn btn(id: &str, label: &str) -> ToolbarButtonSpec {
    ToolbarButtonSpec {
        id: id.to_string(),
        label: label.to_string(),
        action: "proc.run(\"charmap.exe\")".to_string(),
        enabled: true,
    }
}

/// 留空 = 全部显示，按值域的声明顺序。
///
/// 断言写死字面序列而不是拿 `DEFAULT_TOOLBAR_ITEMS` 对拍（后者跟实现一起改，对拍恒绿）。
/// `l1_default_items_are_a_full_cover` 拿这条当基准。
#[test]
fn empty_means_every_item() {
    assert_eq!(
        parse(&[]),
        vec![
            ToolbarItem::Mode,
            ToolbarItem::Punct,
            ToolbarItem::FullWidth,
            ToolbarItem::S2t,
            ToolbarItem::T2s,
            ToolbarItem::SoftKeyboard,
            ToolbarItem::Settings,
        ]
    );
}

/// 数组顺序即渲染顺序——这正是本键与 `ui.status.items`（顺序无语义）的分界。
#[test]
fn order_is_preserved() {
    assert_eq!(
        parse(&["settings", "s2t", "mode"]),
        vec![ToolbarItem::Settings, ToolbarItem::S2t, ToolbarItem::Mode]
    );
}

/// 子集：没写的项不渲染。
#[test]
fn subset_drops_unlisted_items() {
    assert_eq!(
        parse(&["mode", "settings"]),
        vec![ToolbarItem::Mode, ToolbarItem::Settings]
    );
}

/// 未知项只丢它自己，其余照常——比整条回落默认更接近用户意图。
#[test]
fn unknown_key_is_skipped_not_fatal() {
    assert_eq!(
        parse(&["mode", "punkt", "settings"]),
        vec![ToolbarItem::Mode, ToolbarItem::Settings]
    );
}

/// 全非法 → 回落全集（而不是渲染出一条只剩拖动柄的空条）。
#[test]
fn all_invalid_falls_back_to_full_set() {
    assert_eq!(parse(&["nope", "nada"]), DEFAULT_TOOLBAR_ITEMS.to_vec());
}

/// 重复项保留：写两次就画两格。不静默去重——那会让用户以为自己没写对。
#[test]
fn duplicates_are_kept() {
    assert_eq!(
        parse(&["mode", "mode"]),
        vec![ToolbarItem::Mode, ToolbarItem::Mode]
    );
}

/// 前后空白容错（手写 TOML 常见），但空串不算非法项、不刷告警。
#[test]
fn whitespace_is_tolerated_and_blanks_are_silent() {
    assert_eq!(
        parse(&[" mode ", "", "settings"]),
        vec![ToolbarItem::Mode, ToolbarItem::Settings]
    );
}

/// L1（`Config::default()`）写死的那一行必须覆盖全集：把 `-` 前缀（「在这个位置，
/// 但不显示」）剥掉之后，它解析出的序列应与「留空」逐项相同。
///
/// 出厂排布刻意关掉了 `s2t`，但「关掉某几格」与「压根没写」不同：后者会让那几格在设置页
/// 跳回声明序位，也让加新格时忘了同步 `DEFAULT_TOOLBAR_SHOWN` 无人发现。剥前缀再比只放过前者。
///
/// 只管 L1（Rust 侧默认值）；L1↔L2 一致由 wind-config 的 `data_config_toml_*` 守门负责。
#[test]
fn l1_default_items_are_a_full_cover() {
    let factory = wind_config::Config::default().ui.toolbar.items;
    let all_shown: Vec<String> = factory
        .iter()
        .map(|k| k.strip_prefix('-').unwrap_or(k).to_string())
        .collect();
    assert_eq!(parse_toolbar_items(&all_shown, &[]), parse(&[]));
}

/// 条目的值域散在几处：`TOOLBAR_ITEM_KEYS`（配置层值域）、本函数的 match 臂、
/// `DEFAULT_TOOLBAR_ITEMS`（协议层默认项）、`DEFAULT_TOOLBAR_SHOWN` 与
/// `data/config.toml` 那一行（配置层默认项）。
///
/// 「出厂不显示某格」不归这条管：它由 `DEFAULT_TOOLBAR_SHOWN` 的 `-` 前缀表达，值域与
/// 协议层默认项仍是全集，这样往值域加一格不会改变任何人的工具栏外观。
///
/// 钉住两条：每个登记的键都被解析认识（只改常量没改 match 会静默跳过新键），以及默认项
/// 按同样的相对顺序出现在值域里。
#[test]
fn every_registered_key_is_parsable_and_keeps_default_order() {
    let keys: Vec<String> = wind_config::TOOLBAR_ITEM_KEYS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let parsed = parse_toolbar_items(&keys, &[]);
    assert_eq!(
        parsed.len(),
        keys.len(),
        "TOOLBAR_ITEM_KEYS 里有 parse_toolbar_items 不认识的键（被静默跳过了）：{keys:?} → {parsed:?}"
    );
    let mut rest = parsed.iter();
    for d in DEFAULT_TOOLBAR_ITEMS.iter() {
        assert!(
            rest.any(|p| p == d),
            "默认项 {d:?} 不在值域里，或与值域的声明顺序不一致：{parsed:?}"
        );
    }
}

// ── `-` 前缀：关着但占位 ─────────────────────────────────────

/// 带前缀的项不渲染，其余照常——这是「关掉一格」的表达。
#[test]
fn dash_prefix_hides_the_item() {
    assert_eq!(
        parse(&["mode", "-full_width", "punct"]),
        vec![ToolbarItem::Mode, ToolbarItem::Punct]
    );
}

/// 前缀存在的全部理由：关掉再打开，位置不变。
///
/// 模拟设置页一个来回：full_width 拖到第 2 位、关掉、再打开。若关闭时从数组删掉，
/// 重新打开只能补在声明序位（第 3 位）。
#[test]
fn toggling_off_and_on_preserves_position() {
    let off = ["mode", "-full_width", "punct"];
    let on = ["mode", "full_width", "punct"];
    // 关着时不渲染它。
    assert_eq!(parse(&off), vec![ToolbarItem::Mode, ToolbarItem::Punct]);
    // 打开后回到原位（第 2 位），而不是声明序里的位置。
    assert_eq!(
        parse(&on),
        vec![
            ToolbarItem::Mode,
            ToolbarItem::FullWidth,
            ToolbarItem::Punct
        ]
    );
}

/// 自定义按钮同样可以被关着占位。
#[test]
fn dash_prefix_works_on_custom_items() {
    let buttons = [btn("sym", "符")];
    assert_eq!(
        parse_with(&["mode", "-custom:sym"], &buttons),
        vec![ToolbarItem::Mode]
    );
    assert_eq!(
        parse_with(&["mode", "custom:sym"], &buttons),
        vec![
            ToolbarItem::Mode,
            ToolbarItem::Custom {
                index: 0,
                label: "符".to_string(),
            }
        ]
    );
}

/// 前缀不掩盖拼写错误：`-punkt` 与 `punkt` 是同一个错，都该跳过。
///
/// 钉的是不因带前缀就被当成合法关闭项；实现改成「见 `-` 就 continue」时本条仍绿、下一条会红。
#[test]
fn dash_prefix_does_not_legitimize_typos() {
    assert_eq!(
        parse(&["mode", "-punkt", "punct"]),
        vec![ToolbarItem::Mode, ToolbarItem::Punct]
    );
}

/// `-` 只剥一层：`--mode` 不是「关掉的关掉的 mode」，是手滑。
#[test]
fn double_dash_is_a_typo_not_a_double_negative() {
    assert_eq!(parse(&["--mode", "punct"]), vec![ToolbarItem::Punct]);
}

/// 全部关闭 → 展开为空 → 回落全集。
///
/// 只在手写配置时可达（设置页有「至少留一格」的闸门）；兜底是因为只剩拖动柄的空条看着像 bug。
#[test]
fn all_disabled_falls_back_to_full_set() {
    assert_eq!(
        parse(&["-mode", "-punct", "-full_width", "-s2t", "-settings"]),
        DEFAULT_TOOLBAR_ITEMS.to_vec()
    );
}

// ── 自定义按钮 ──────────────────────────────────────────────

/// `custom:<id>` 按 id 找到定义，可排在内置项中间。
#[test]
fn custom_button_resolves_by_id_at_any_position() {
    let buttons = [btn("sym", "符")];
    assert_eq!(
        parse_with(&["mode", "custom:sym", "settings"], &buttons),
        vec![
            ToolbarItem::Mode,
            ToolbarItem::Custom {
                index: 0,
                label: "符".to_string(),
            },
            ToolbarItem::Settings,
        ]
    );
}

/// index 是在 buttons 数组里的下标，不是在 items 里的位置——点击回指靠它。
#[test]
fn custom_index_points_into_buttons_array() {
    let buttons = [btn("a", "甲"), btn("b", "乙")];
    let got = parse_with(&["custom:b", "custom:a"], &buttons);
    let idx: Vec<u8> = got
        .iter()
        .filter_map(|i| match i {
            ToolbarItem::Custom { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(idx, vec![1, 0], "下标须指向 buttons，与 items 里的顺序无关");
}

/// 四种「配了不出现」必须各自可分辨（都只表现为"我的按钮没了"）：
/// 悬空引用 / 被 enabled 关掉 / label 为空 / 前缀写错。
#[test]
fn custom_button_vanishes_only_for_these_reasons() {
    let buttons = [
        btn("ok", "符"),
        ToolbarButtonSpec {
            enabled: false,
            ..btn("off", "关")
        },
        btn("blank", "   "),
    ];
    // 每种非法情形都只丢它自己，其余照常。
    let got = parse_with(
        &[
            "custom:ghost", // 没有对应定义
            "custom:off",   // enabled = false
            "custom:blank", // label 全空白
            "custom-ok",    // 前缀写错（连字符）
            "custom:ok",    // 唯一合法的
        ],
        &buttons,
    );
    assert_eq!(
        got,
        vec![ToolbarItem::Custom {
            index: 0,
            label: "符".to_string(),
        }]
    );
}

/// label 按显示宽度截断：一个汉字或两个 ASCII。
///
/// 与语言栏图标主字的 `icon_label_trunc` 同一条规则，只是上限常量各留一个。理由见 `toolbar_label_trunc` 的文档。
#[test]
fn custom_label_truncates_by_display_width() {
    let cases = [
        ("符", "符"),   // 1 个汉字，宽 2
        ("符号", "符"), // 2 个汉字，宽 4 → 截
        ("En", "En"),   // 2 个 ASCII，宽 2
        ("Eng", "En"),  // 3 个 ASCII，宽 3 → 截
        (" 符 ", "符"), // 首尾空白先去掉
        ("符A", "符"),  // 混排：汉字已占满 2
        ("A符", "A"),   // 混排：A 占 1，再加汉字就超
    ];
    for (raw, want) in cases {
        let buttons = [btn("x", raw)];
        let got = parse_with(&["custom:x"], &buttons);
        let label = match got.first() {
            Some(ToolbarItem::Custom { label, .. }) => label.clone(),
            other => panic!("{raw:?} 未解析出自定义项：{other:?}"),
        };
        assert_eq!(label, want, "label {raw:?} 应截成 {want:?}");
    }
}

/// 光在 `buttons` 里定义不等于要显示：`items` 是显示与顺序的唯一真相源。
///
/// 反过来说，`items` 留空时回落的是内置全集，不含任何自定义按钮——否则用户
/// 没法"先定义好、暂时不放上去"。
#[test]
fn defining_a_button_does_not_show_it() {
    let buttons = [btn("sym", "符")];
    assert_eq!(parse_with(&[], &buttons), DEFAULT_TOOLBAR_ITEMS.to_vec());
}
