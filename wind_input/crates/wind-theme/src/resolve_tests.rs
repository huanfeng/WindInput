use super::*;
use crate::schema::Dim;

fn data_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes")
}

fn testdata_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/themes")
}

fn load(name: &str, is_dark: bool) -> Resolved {
    load_resolved_dirs(&[data_dir()], name, is_dark).unwrap()
}

fn load_jidian(is_dark: bool) -> Resolved {
    load_resolved_dirs(&[testdata_dir(), data_dir()], "jidian-classic", is_dark).unwrap()
}

fn typed(text: &str) -> Theme {
    let value: toml::Value = toml::from_str(text).unwrap();
    crate::normalize::normalize_theme(value).try_into().unwrap()
}

/// 语言栏主字色的 light/dark 是任务栏明暗：候选窗无论按浅色还是深色求值，四格都齐。
#[test]
fn langbar_text_resolves_both_taskbar_tones() {
    let expect = LangbarTextColors {
        cn_light: Some([0, 0, 0, 255]),
        cn_dark: Some([255, 255, 255, 255]),
        en_light: Some([0, 0, 0, 255]),
        en_dark: Some([255, 255, 255, 255]),
    };
    assert_eq!(load("default", false).langbar_text, expect);
    assert_eq!(load("default", true).langbar_text, expect);
}

/// 子主题覆盖 `_base` 的 token，未覆盖的那个仍继承 `_base`；引用链可用。
#[test]
fn langbar_text_theme_overrides_base() {
    let root = std::env::temp_dir().join(format!("wt-langbar-{}", std::process::id()));
    let dir = root.join("lbtest");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("theme.toml"),
        "base = \"_base\"\n[colors]\nmy_red = \"#FF0000\"\n\
             langbar_text_cn = { light = \"${my_red}\", dark = \"#00FF0080\" }\n",
    )
    .unwrap();
    let r = load_resolved_dirs(&[root.clone(), data_dir()], "lbtest", false).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(r.langbar_text.cn_light, Some([255, 0, 0, 255]));
    assert_eq!(r.langbar_text.cn_dark, Some([0, 255, 0, 0x80]));
    assert_eq!(r.langbar_text.en_light, Some([0, 0, 0, 255]));
    assert_eq!(r.langbar_text.en_dark, Some([255, 255, 255, 255]));
}

/// 主题（含 base 链）都没写 token 时四格为 None，交给消费端回落内置色。
#[test]
fn langbar_text_absent_is_none() {
    let r = resolve(
        &typed("[colors]\ntext = \"#123456\"\n"),
        false,
        &[data_dir()],
    );
    assert_eq!(r.langbar_text, LangbarTextColors::default());
}

/// footer_bar 的禁用态 patch 要一路活到 RvNode —— 翻页箭头灰显色的唯一来源。
///
/// 少建这一份时，主题编辑器的「禁用态」面板配了没反应、预览与实机对不上。
#[test]
fn footer_bar_disabled_state_survives_resolve() {
    let text = "[footer_bar.disabled]\ncolor = \"#FF0000\"\n";
    let value: toml::Value = toml::from_str(text).unwrap();
    let normalized = crate::normalize::normalize_theme(value);
    let theme: Theme = normalized.try_into().unwrap();
    let r = resolve(&theme, false, &[data_dir()]);
    let d = r
        .views
        .footer_bar
        .disabled
        .as_ref()
        .expect("footer_bar.disabled 应被构建");
    assert_eq!(
        d.text_color,
        Some([255, 0, 0, 255]),
        "禁用态文字色应解析出来"
    );
}

/// 弹窗类节点（status / tooltip / toast / menu.root）的 `background.gradient` 必须求值进
/// RvNode——渲染层要消费它，这一层先不能丢。
#[test]
fn test_popup_nodes_carry_bg_gradient() {
    let text = r##"
[status]
background = { gradient = { stops = [{ color = "#FF0000", pos = 0.0 }, { color = "#0000FF", pos = 1.0 }] } }
[tooltip]
background = { gradient = { type = "radial", stops = [{ color = "#FF0000", pos = 0.0 }] } }
[toast]
background = { gradient = { angle = 90, stops = [{ color = "#00FF00", pos = 1.0 }] } }
[menu.root]
background = { gradient = { stops = [{ color = "#0000FF", pos = 0.5 }] } }
"##;
    let value: toml::Value = toml::from_str(text).unwrap();
    let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
    let r = resolve(&theme, false, &[data_dir()]);
    let g = |n: Option<&RvNode>, name: &str| {
        n.and_then(|n| n.bg_gradient.clone())
            .unwrap_or_else(|| panic!("{name} 的渐变没求值进 RvNode"))
    };
    let st = g(r.views.status.as_ref(), "status");
    assert_eq!(st.kind, "linear");
    assert_eq!(
        st.stops,
        vec![([255, 0, 0, 255], 0.0), ([0, 0, 255, 255], 1.0)]
    );
    assert_eq!(g(r.views.tooltip.as_ref(), "tooltip").kind, "radial");
    assert_eq!(g(r.views.toast.as_ref(), "toast").angle, 90.0);
    assert_eq!(
        g(r.views.menu_root.as_ref(), "menu.root").stops,
        vec![([0, 0, 255, 255], 0.5)]
    );
}

/// 缺 `pos` 的 stop 按序号均分（与主题编辑器同口径；全缺时同 CSS）；缺省成 0 的话全部 stop
/// 挤在起点，整块退化成最后一色的纯色。显式给了 pos 的照旧。
#[test]
fn test_gradient_missing_pos_spreads_evenly() {
    let text = r##"
[window]
background = { gradient = { stops = [{ color = "#FF0000" }, { color = "#00FF00" }, { color = "#0000FF" }] } }
[status]
background = { gradient = { stops = [{ color = "#FF0000" }, { color = "#0000FF", pos = 0.25 }] } }
[toast]
background = { gradient = { stops = [{ color = "#FF0000" }] } }
"##;
    let value: toml::Value = toml::from_str(text).unwrap();
    let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
    let r = resolve(&theme, false, &[data_dir()]);
    let pos = |n: Option<&RvNode>| -> Vec<f32> {
        n.and_then(|n| n.bg_gradient.clone())
            .expect("渐变应求值")
            .stops
            .iter()
            .map(|s| s.1)
            .collect()
    };
    assert_eq!(pos(Some(&r.views.window)), vec![0.0, 0.5, 1.0]);
    assert_eq!(pos(r.views.status.as_ref()), vec![0.0, 0.25]);
    assert_eq!(pos(r.views.toast.as_ref()), vec![0.0]);
}

#[test]
fn test_index_labels_survive_resolve() {
    // 主题 [index] labels 经 normalize→typed→resolve 全链路存活到 Resolved.views.index_labels。
    // 协调器据此裁决「用户 > 主题 > 默认」；此测只验证主题层承载不丢。
    let text = "[index]\nlabels = [\"①\", \"②\", \"③\"]\n";
    let value: toml::Value = toml::from_str(text).unwrap();
    let normalized = crate::normalize::normalize_theme(value);
    let theme: Theme = normalized.try_into().unwrap();
    let r = resolve(&theme, false, &[data_dir()]);
    assert_eq!(
        r.views.index_labels,
        vec!["①".to_string(), "②".to_string(), "③".to_string()],
    );
}

/// 菜单颜色语义：节点显式值优先，未配回退 palette token（与候选窗一致）。
/// 渲染层只读 token 时，主题里写的 `[menu.*]` 颜色全不生效。
#[test]
fn test_menu_node_colors_override_palette() {
    let text = "\
[colors]
menu_bg = \"#111111\"
menu_text = \"#222222\"
menu_hover_bg = \"#333333\"
menu_hover_text = \"#444444\"
menu_disabled = \"#555555\"
menu_separator = \"#666666\"

[menu.root]
background = \"#AA0000\"

[menu.item]
color = \"#BB0000\"
font_size = 2

[menu.item.hover]
background = \"#CC0000\"
color = \"#DD0000\"

[menu.item.disabled]
color = \"#EE0000\"

[menu.separator]
background = \"#FF0000\"
";
    let value: toml::Value = toml::from_str(text).unwrap();
    let normalized = crate::normalize::normalize_theme(value);
    let theme: Theme = normalized.try_into().unwrap();
    let r = resolve(&theme, false, &[data_dir()]);

    let root = r.views.menu_root.as_ref().expect("menu_root");
    assert_eq!(
        root.bg_color,
        Some([0xAA, 0, 0, 255]),
        "容器背景色应取节点值"
    );

    let item = r.views.menu_item.as_ref().expect("menu_item");
    assert_eq!(
        item.text_color,
        Some([0xBB, 0, 0, 255]),
        "项文字色应取节点值"
    );
    assert_eq!(item.font_size, 2.0, "字号偏移应透传");

    let hover = item.hover.as_ref().expect("menu.item.hover 应被构建");
    assert_eq!(hover.bg_color, Some([0xCC, 0, 0, 255]));
    assert_eq!(hover.text_color, Some([0xDD, 0, 0, 255]));

    let disabled = item.disabled.as_ref().expect("menu.item.disabled 应被构建");
    assert_eq!(disabled.text_color, Some([0xEE, 0, 0, 255]));

    let sep = r.views.menu_separator.as_ref().expect("menu_separator");
    assert_eq!(sep.bg_color, Some([0xFF, 0, 0, 255]), "分隔线色应取节点值");
}

/// 未写 [menu.*] 节点色时回退 palette token —— 保证既有主题行为不变。
#[test]
fn test_menu_falls_back_to_palette_tokens() {
    let text = "\
[colors]
menu_bg = \"#111111\"
menu_text = \"#222222\"
menu_hover_bg = \"#333333\"
menu_hover_text = \"#444444\"
menu_disabled = \"#555555\"
menu_separator = \"#666666\"

[menu.root]
radius = 6
";
    let value: toml::Value = toml::from_str(text).unwrap();
    let normalized = crate::normalize::normalize_theme(value);
    let theme: Theme = normalized.try_into().unwrap();
    let r = resolve(&theme, false, &[data_dir()]);

    let root = r.views.menu_root.as_ref().expect("menu_root");
    assert_eq!(root.bg_color, Some([0x11, 0x11, 0x11, 255]));
    let item = r.views.menu_item.as_ref().expect("menu_item");
    assert_eq!(item.text_color, Some([0x22, 0x22, 0x22, 255]));
    let hover = item.hover.as_ref().expect("hover 由 palette 默认色撑起");
    assert_eq!(hover.bg_color, Some([0x33, 0x33, 0x33, 255]));
    assert_eq!(hover.text_color, Some([0x44, 0x44, 0x44, 255]));
    let disabled = item
        .disabled
        .as_ref()
        .expect("disabled 由 palette 默认色撑起");
    assert_eq!(disabled.text_color, Some([0x55, 0x55, 0x55, 255]));
    let sep = r.views.menu_separator.as_ref().expect("menu_separator");
    assert_eq!(sep.bg_color, Some([0x66, 0x66, 0x66, 255]));
}

/// 强调条条高比：显式值透传；未配 / 越界一律落默认 0.6。
///
/// 钉住「未配 ≠ 0.0」：`RvViews` derive 的 Default 是 0.0，resolve 不显式写默认值时，
/// 所有未配该项的主题强调条会塌成 2px 细线。
#[test]
fn test_accent_bar_height_ratio_defaults_and_clamps() {
    let build = |accent: &str| {
        let text = format!("[colors]\naccent = \"#3B82F6\"\n\n[accent_bar]\n{accent}");
        let value: toml::Value = toml::from_str(&text).unwrap();
        let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
        resolve(&theme, false, &[data_dir()])
            .views
            .accent_bar_height_ratio
    };

    // 未配 → 默认
    assert_eq!(build("enabled = true\n"), DEFAULT_ACCENT_BAR_HEIGHT_RATIO);
    // 显式值透传
    assert_eq!(build("enabled = true\nheight_ratio = 1.0\n"), 1.0);
    assert_eq!(build("enabled = true\nheight_ratio = 0.35\n"), 0.35);
    // 越界（≤0 / >1）→ 回退默认，不 fail
    assert_eq!(
        build("enabled = true\nheight_ratio = 0.0\n"),
        DEFAULT_ACCENT_BAR_HEIGHT_RATIO
    );
    assert_eq!(
        build("enabled = true\nheight_ratio = -0.5\n"),
        DEFAULT_ACCENT_BAR_HEIGHT_RATIO
    );
    assert_eq!(
        build("enabled = true\nheight_ratio = 1.5\n"),
        DEFAULT_ACCENT_BAR_HEIGHT_RATIO
    );
}

/// 内置主题真实加载出的条高比必须**可用**（0 < r ≤ 1），不会塌成 2px 细线。
///
/// 只钉「值可用」这个与主题选值无关的性质：内置主题可能显式配条高比。
/// 「未配→0.6」由 `..._defaults_and_clamps` 与下面的 default 用例正面覆盖。
#[test]
fn test_builtin_themes_get_usable_accent_ratio() {
    for name in ["default", "msime"] {
        let r = load(name, false).views.accent_bar_height_ratio;
        assert!(
            r > 0.0 && r <= 1.0,
            "{name} 条高比 {r} 不可用（0 会被渲染钳成 2px 细线）"
        );
    }
    assert_eq!(
        load("default", false).views.accent_bar_height_ratio,
        DEFAULT_ACCENT_BAR_HEIGHT_RATIO,
        "default 未配 height_ratio，应落默认值而非 0"
    );
}

/// 工具栏整体背景色/边框色：节点优先，未配回退 toolbar_* token。
#[test]
fn test_toolbar_node_colors_override_palette() {
    let text = "\
[colors]
toolbar_background = \"#111111\"
toolbar_border = \"#222222\"

[toolbar]
background = \"#AA0000\"
border = { color = \"#BB0000\" }
";
    let value: toml::Value = toml::from_str(text).unwrap();
    let normalized = crate::normalize::normalize_theme(value);
    let theme: Theme = normalized.try_into().unwrap();
    let r = resolve(&theme, false, &[data_dir()]);
    assert_eq!(r.views.toolbar_bg_color, Some([0xAA, 0, 0, 255]));
    assert_eq!(r.views.toolbar_border_color, Some([0xBB, 0, 0, 255]));

    // 未配节点色 → 回退 token
    let text2 = "\
[colors]
toolbar_background = \"#111111\"
toolbar_border = \"#222222\"

[toolbar]
height = 30
";
    let v2: toml::Value = toml::from_str(text2).unwrap();
    let t2: Theme = crate::normalize::normalize_theme(v2).try_into().unwrap();
    let r2 = resolve(&t2, false, &[data_dir()]);
    assert_eq!(r2.views.toolbar_bg_color, Some([0x11, 0x11, 0x11, 255]));
    assert_eq!(r2.views.toolbar_border_color, Some([0x22, 0x22, 0x22, 255]));
}

/// 候选窗位置偏移：内联表与 `[x, y]` 简写等价，未配为 None（=0，旧行为）。
#[test]
fn test_window_position_offset() {
    let load_offset = |body: &str| {
        let value: toml::Value = toml::from_str(body).unwrap();
        let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
        let v = resolve(&theme, false, &[data_dir()]).views;
        (v.window_offset_x, v.window_offset_y)
    };

    let inline = load_offset("[window]\nposition_offset = { x = 3, y = 4 }\n");
    assert_eq!(inline.0, Some(crate::schema::Dim::Dp(3.0)));
    assert_eq!(inline.1, Some(crate::schema::Dim::Dp(4.0)));
    // 数组简写（与 shadow.offset 一致的书写体验）→ 归一化成同一形态
    assert_eq!(load_offset("[window]\nposition_offset = [3, 4]\n"), inline);
    // px 单位不随 DPI 缩放
    assert_eq!(
        load_offset("[window]\nposition_offset = { y = \"2px\" }\n").1,
        Some(crate::schema::Dim::Px(2.0))
    );
    // 未配 → None，place_window 拿到 0，定位与旧版逐像素一致
    assert_eq!(load_offset("[window]\nradius = 8\n"), (None, None));
}

/// `slice_repeat` 要一路走到渲染消费形态：字符串在求值层坍缩成两轴布尔。
///
/// 这条守的是数据通路——渲染层再怎么实现平铺，这里断了就永远是拉伸。
#[test]
fn slice_repeat_reaches_rv_image_per_axis() {
    let text = "\
[preedit_bar]
background = { image = { ref = \"p.png\", mode = \"nine_slice\", slice_repeat = [\"repeat\", \"stretch\"] } }

[item]
background = { image = { ref = \"q.png\", mode = \"nine_slice\" } }
";
    let value: toml::Value = toml::from_str(text).unwrap();
    let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
    let v = resolve(&theme, false, &[data_dir()]).views;

    let bar = v.preedit_bar.bg_image.expect("preedit_bar 背景图");
    assert_eq!(bar.slice_repeat, [true, false], "两轴分别求值，不能串");

    // 不写的主题保持原样：全部拉伸，既有观感不被新字段改掉。
    let item = v.item.bg_image.expect("item 背景图");
    assert_eq!(item.slice_repeat, [false, false]);
}

/// `flip` 走到背景图、图层与翻页箭头；非法值按不翻，不连累整份主题。
#[test]
fn flip_reaches_bg_image_layers_and_arrows() {
    let text = "\
[window]
background = { image = { ref = \"p.png\", flip = \"x\" } }
layers = [{ ref = \"a.png\", flip = \"y\" }, { ref = \"b.png\", flip = \"xy\" }, { ref = \"c.png\" }]

[item]
background = { image = { ref = \"q.png\", flip = \"z\" } }

[footer_bar]
prev_image = { ref = \"prev.svg\", flip = \"x\" }
";
    let value: toml::Value = toml::from_str(text).unwrap();
    let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
    let v = resolve(&theme, false, &[data_dir()]).views;
    let x = Flip { x: true, y: false };
    assert_eq!(v.window.bg_image.expect("窗口背景图").flip, x);
    let layers: Vec<Flip> = v.window.layers.iter().map(|l| l.flip).collect();
    assert_eq!(
        layers,
        [
            Flip { x: false, y: true },
            Flip { x: true, y: true },
            Flip::NONE
        ]
    );
    assert_eq!(
        v.item.bg_image.expect("item 背景图").flip,
        Flip::NONE,
        "非法值按不翻"
    );
    assert_eq!(v.footer_bar.prev_image.expect("翻页箭头").flip, x);
}

#[test]
fn flip_names_round_trip() {
    for s in ["x", "y", "xy"] {
        assert_eq!(Flip::parse(s).and_then(Flip::name), Some(s));
    }
    assert_eq!(Flip::NONE.name(), None);
    for bad in ["", "X", "yx", "both", "none"] {
        assert_eq!(Flip::parse(bad), None, "{bad:?}");
    }
}

/// 分隔线节点：颜色缺省取调色板 `separator`，`[divider]` 可覆盖颜色 / 线粗 / 开关。
#[test]
fn divider_defaults_to_separator_and_can_be_overridden() {
    let resolve_of = |text: &str| {
        let value: toml::Value = toml::from_str(text).unwrap();
        let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
        resolve(&theme, false, &[data_dir()]).views
    };
    let v = resolve_of("[colors]\nseparator = \"#112233\"\n[window]\nradius = 1\n");
    assert_eq!(
        v.divider.bg_color,
        Some([0x11, 0x22, 0x33, 255]),
        "缺省取 separator"
    );
    assert_eq!(v.divider_enabled, None);
    assert_eq!(v.divider_height, None);

    let v = resolve_of(
        "[colors]\nseparator = \"#112233\"\n[divider]\nbackground = \"#445566\"\nheight = 3\nenabled = false\nmargin = [0, 10, 6, 10]\n",
    );
    assert_eq!(
        v.divider.bg_color,
        Some([0x44, 0x55, 0x66, 255]),
        "显式色优先"
    );
    assert_eq!(v.divider_height, Some(crate::schema::Dim::Dp(3.0)));
    assert_eq!(v.divider_enabled, Some(false));
    assert!(v.divider.margin.left.is_some() && v.divider.margin.bottom.is_some());
}

/// 四个自带盒模型的节点：背景与边框须完整进 RvNode。
///
/// 防止日后在 resolve 里加节点白名单时把它们漏掉：漏掉时圆角会借用 `item.border_radius`
/// 或背景边框直接丢失。
#[test]
fn test_container_nodes_carry_own_box_model() {
    let text = "\
[colors]
accent = \"#3B82F6\"

[preedit_bar]
background = \"#111111\"
radius = 7
border = { color = \"#222222\", width = 2 }

[mode_label]
background = \"#333333\"
radius = 9
border = { color = \"#444444\" }

[candidate_list]
background = \"#555555\"
radius = 11
border = { color = \"#666666\" }

[footer_bar]
background = \"#777777\"
radius = 13
border = { color = \"#888888\" }
";
    let value: toml::Value = toml::from_str(text).unwrap();
    let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
    let v = resolve(&theme, false, &[data_dir()]).views;

    let cases: [(&str, &RvNode, u8, u8, f32); 4] = [
        ("preedit_bar", &v.preedit_bar, 0x11, 0x22, 7.0),
        ("mode_label", &v.mode_label, 0x33, 0x44, 9.0),
        ("candidate_list", &v.candidate_list, 0x55, 0x66, 11.0),
        ("footer_bar", &v.footer_bar, 0x77, 0x88, 13.0),
    ];
    for (name, node, bg, border, radius) in cases {
        assert_eq!(node.bg_color, Some([bg, bg, bg, 255]), "{name} 背景色");
        assert_eq!(
            node.border_color,
            Some([border, border, border, 255]),
            "{name} 边框色"
        );
        assert_eq!(
            node.border_radius,
            Some(crate::schema::Dim::Dp(radius)),
            "{name} 圆角（自身的，不是借 item 的）"
        );
    }
    // preedit_bar 显式边框宽也要透传（渲染层 `dim(border_width, 0).max(1)`）。
    assert_eq!(
        v.preedit_bar.border_width,
        Some(crate::schema::Dim::Dp(2.0))
    );
    // 未配圆角的节点保持 None —— 渲染层据此回退（preedit_bar/mode_label 回退 item）。
    let bare: toml::Value = toml::from_str("[colors]\naccent = \"#3B82F6\"\n").unwrap();
    let t2: Theme = crate::normalize::normalize_theme(bare).try_into().unwrap();
    let v2 = resolve(&t2, false, &[data_dir()]).views;
    assert_eq!(v2.preedit_bar.border_radius, None);
    assert_eq!(v2.mode_label.border_radius, None);
}

/// 工具栏整条外框圆角/线宽要解析进 RvNode：只取 border.color 时，编辑器开放的
/// 两个控件在实机上毫无变化。
#[test]
fn test_toolbar_border_radius_and_width_extracted() {
    let text = "\
[toolbar]
border = { color = \"#BB0000\", radius = 0, width = \"2px\" }
";
    let value: toml::Value = toml::from_str(text).unwrap();
    let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
    let r = resolve(&theme, false, &[data_dir()]);
    // radius=0 必须是「显式直角」而非「未配」——硬边缘风格靠它。
    assert_eq!(
        r.views.toolbar_border_radius,
        Some(crate::schema::Dim::Dp(0.0))
    );
    // px 单位不随 DPI 加粗（发丝线）。
    assert_eq!(
        r.views.toolbar_border_width,
        Some(crate::schema::Dim::Px(2.0))
    );

    // 未配 → None，渲染层落内置派生值（条高×0.30 / 1dp），零回归。
    let bare: toml::Value = toml::from_str("[toolbar]\nheight = 30\n").unwrap();
    let t2: Theme = crate::normalize::normalize_theme(bare).try_into().unwrap();
    let r2 = resolve(&t2, false, &[data_dir()]);
    assert_eq!(r2.views.toolbar_border_radius, None);
    assert_eq!(r2.views.toolbar_border_width, None);
}

#[test]
fn test_default_theme_resolve() {
    let r = load("default", false);
    // window bg = bg light = 白
    assert_eq!(r.window_bg(), Some([255, 255, 255, 255]));
    // v4 设计基线：序号为淡色纯数字。shape="none" → 不画圆圈（bg_color 兜底 accent 但不渲染）。
    assert_eq!(r.views.index.bg_shape, "none");
    assert_eq!(r.views.index.bg_color, Some([0x3b, 0x82, 0xf6, 255])); // accent 兜底(因 shape=none 不绘制)
    assert_eq!(r.views.index.text_color, Some([0x9A, 0xA0, 0xAE, 255])); // text_hint light
    // 选中候选文字用 accent 高亮（[text.selected].color = accent_text light）。
    let sel = r.views.text.selected.as_ref().expect("text.selected patch");
    assert_eq!(sel.text_color, Some([0x25, 0x63, 0xeb, 255]));
    // 行为：default 单页不显翻页区；清风设计默认不显页码数字（保持整洁）。
    assert!(!r.behavior.always_show_pager);
    assert!(!r.behavior.show_page_number);
    assert_eq!(r.behavior.font_size, 18);
    // 暗色 bg = 深蓝灰（设计稿暗色面板基调 #121826）
    let d = load("default", true);
    assert_eq!(d.window_bg(), Some([0x12, 0x18, 0x26, 255]));
}

#[test]
fn test_jidian_classic_rvnode() {
    let r = load_jidian(false);
    // window 九宫格背景图
    let bg = r.views.window.bg_image.as_ref().expect("window bg image");
    assert_eq!(bg.reference, "panel");
    assert_eq!(bg.mode, "nine_slice");
    assert_eq!(bg.slice, [8.0, 8.0, 8.0, 8.0]);
    // window 投影 blur 进顶层 shadow
    assert_eq!(r.views.shadow_blur, Some(Dim::Dp(8.0)));
    // layers 水印 z=1 + 透明度
    assert_eq!(r.views.window.layers.len(), 1);
    let mark = &r.views.window.layers[0];
    assert_eq!(mark.reference, "mark");
    assert_eq!(mark.z, 1);
    assert_eq!(mark.opacity, 0.9);
    assert_eq!(mark.offset_x, Some(Dim::Dp(-8.0)));
    // item 选中态 patch：背景图 + 字重（几何不进）
    let sel = r.views.item.selected.as_ref().expect("item.selected patch");
    assert_eq!(sel.font_weight, 600);
    assert!(sel.bg_image.is_some());
    // accent_bar 几何
    assert_eq!(r.views.accent_bar_width, Some(Dim::Dp(3.0)));
    // resources 解析为绝对路径（按 is_dark 选 light 变体 panel.png）
    let panel = r.resources.get("panel").expect("panel resource");
    assert!(panel.ends_with("panel.png"), "got {panel}");
    assert!(Path::new(panel).is_absolute(), "应为绝对路径: {panel}");
    // 暗色选 dark 变体
    let d = load_jidian(true);
    assert!(
        d.resources
            .get("panel")
            .unwrap()
            .ends_with("panel-dark.png")
    );
    // 暗色另记亮色那侧（皮肤校验亮暗尺寸用）；亮色求值不记，亮暗同图的键也不记。
    assert!(d.light_resources["panel"].ends_with("panel.png"));
    assert!(!d.light_resources.contains_key("mark"));
    assert!(r.light_resources.is_empty());
    // 其它窗口也吃九宫格
    let status = r.views.status.as_ref().expect("status rvnode");
    assert!(status.bg_image.is_some());
    assert_eq!(status.layers.len(), 1);
}

#[test]
fn test_read_meta_and_multidir_base() {
    let dirs = vec![testdata_dir(), data_dir()];
    // 显示名 / order 取自主题自身 meta（不 base 合并）。
    let m = crate::theme::read_meta(&dirs, "jidian-classic").expect("jidian meta");
    assert_eq!(m.name, "极点经典(位图)");
    assert_eq!(m.order, 2);
    let dm = crate::theme::read_meta(&[data_dir()], "default").expect("default meta");
    assert_eq!(dm.name, "清风·蓝");
    // 多目录加载：jidian `base: _base` 跨目录解析正常（resources/views 出来）。
    let r = load_resolved_dirs(&dirs, "jidian-classic", false).expect("resolve jidian");
    assert!(
        r.views.window.bg_image.is_some(),
        "应解析出 _base+jidian 合并后的 window 背景图"
    );
    assert!(r.resources.contains_key("panel"));

    // 资产链：default 经 _qingfeng → _base，链中应含 _base 目录（_base 的 chevron.svg 可被继承解析）。
    let d = load_resolved_dirs(&[data_dir()], "default", false).expect("resolve default");
    assert!(
        d.asset_dirs.iter().any(|p| p.ends_with("_base")),
        "default 资产链应含 _base 目录"
    );
    // 清风设计把 _base 的 chevron 覆盖为空 ref（回退细小文字 ‹ › 翻页箭头）。
    let prev = d
        .views
        .footer_bar
        .prev_image
        .as_ref()
        .expect("footer prev_image");
    assert_eq!(prev.reference, "", "清风主题应覆盖 chevron 为空 → 文字箭头");
}

#[test]
fn test_state_gating_drops_geometry_only_patch() {
    // hover 只改 padding（几何）应被视为空 patch 丢弃；改色则保留。
    let palette = HashMap::new();
    let geo_only = ViewNode {
        padding: crate::schema::ViewEdges {
            top: Some(Dim::Dp(99.0)),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(
        resolve_state(Some(&geo_only), &palette, false, None, None).is_none(),
        "纯几何 patch 应丢弃"
    );
    let color_patch = ViewNode {
        color: Some(Ld::Scalar("#FF0000".into())),
        ..Default::default()
    };
    assert!(
        resolve_state(Some(&color_patch), &palette, false, None, None).is_some(),
        "改色 patch 应保留"
    );
}

// ───────────────────── 文字角色色（分段着色 §5）─────────────────────

fn roles_theme(name: &str, dark: bool) -> Resolved {
    crate::load_resolved_dirs(&[testdata_dir(), data_dir_for_roles()], name, dark)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn data_dir_for_roles() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes")
}

fn hex(s: &str) -> Rgba {
    parse_hex(s).unwrap()
}

#[test]
fn comment_roles_resolve_tokens_and_light_dark() {
    let l = roles_theme("span-roles", false);
    let d = roles_theme("span-roles", true);
    assert_eq!(l.views.comment.roles["code_rev"], hex("#C00000"));
    assert_eq!(d.views.comment.roles["code_rev"], hex("#FF8080"));
    assert_eq!(l.views.comment.roles["literal"], hex("#B0B0B0"));
    assert_eq!(d.views.comment.roles["literal"], hex("#606060"));
    assert_eq!(l.views.comment.roles["pinyin"], hex("#008000"));
    let tip = l.views.tooltip.as_ref().unwrap();
    assert_eq!(tip.roles["title"], l.palette["accent"]);
    assert_eq!(tip.roles["readings"], hex("#9AD0FF"));
}

/// transparent 与 alpha 为 0 的角色色拒收（按未设置）。
#[test]
fn transparent_roles_are_rejected() {
    let t = roles_theme("span-roles", false);
    assert!(!t.views.comment.roles.contains_key("chaizi"));
    assert!(!t.views.comment.roles.contains_key("dict"));
}

/// 状态 patch：选中态有正文色 + 单列角色；悬停态**只**写了 roles 也不能被丢。
#[test]
fn state_roles_survive_nil_gating() {
    let t = roles_theme("span-roles", false);
    let sel = t.views.comment.selected.as_ref().expect("selected");
    assert_eq!(sel.text_color, Some(hex("#FFFFFF")));
    assert_eq!(sel.roles["code_rev"], hex("#FFE08A"));
    let hov = t
        .views
        .comment
        .hover
        .as_ref()
        .expect("只写 roles 的 hover patch 不能被丢");
    assert_eq!(hov.roles["pinyin"], hex("#0000FF"));
    assert_eq!(hov.text_color, None);
}

/// 派生主题逐键深合并：改一个、`""` 撤销一个、其余继承。
#[test]
fn derived_theme_merges_roles_per_key() {
    let t = roles_theme("span-roles-child", false);
    let r = &t.views.comment.roles;
    assert_eq!(r["pinyin"], hex("#00AA00"));
    assert!(!r.contains_key("literal"), "`\"\"` 撤销 base 的角色");
    assert_eq!(r["code_rev"], hex("#C00000"), "未提及的角色继承 base");
    assert_eq!(
        t.views.comment.selected.as_ref().unwrap().roles["code_rev"],
        hex("#FFE08A")
    );
}

// ───────────────── comment_above 回退（candidate-comment-above-line §3.4）─────────────────

fn edges_unset(e: &crate::rvnode::RvEdges) -> bool {
    e.top.is_none() && e.right.is_none() && e.bottom.is_none() && e.left.is_none()
}

/// 全空 ⇒ 字体 / 颜色 / roles / 选中态回退 comment；margin / padding 不继承。light/dark 各取各的。
#[test]
fn comment_above_inherits_font_color_roles_but_not_edges() {
    for dark in [false, true] {
        let t = roles_theme("above-inherit", dark);
        let (c, a) = (&t.views.comment, &t.views.comment_above);
        assert_eq!(a.font_size, -2.0);
        assert_eq!(a.font_weight, 300);
        assert_eq!(a.text_color, c.text_color);
        assert_eq!(
            a.text_color,
            Some(hex(if dark { "#AABBCC" } else { "#112233" }))
        );
        assert_eq!(
            a.roles["code_rev"],
            hex(if dark { "#FF8080" } else { "#C00000" })
        );
        assert_eq!(a.roles["pinyin"], hex("#008000"));
        let sel = a.selected.as_ref().expect("selected 回退 comment 的");
        assert_eq!(sel.text_color, Some(hex("#FFFFFF")));
        assert_eq!(sel.roles["code_rev"], hex("#FFE08A"));
        // 状态 patch 同样只继承字体 / 颜色 / roles：comment.selected 的底色、边框不带过来。
        let csel = c.selected.as_ref().unwrap();
        assert_eq!(csel.bg_color, Some(hex("#334455")));
        assert_eq!(csel.border_color, Some(hex("#556677")));
        assert_eq!(sel.bg_color, None, "选中态底色不继承");
        assert_eq!(sel.border_color, None, "选中态边框不继承");
        // 继承来的字族：缺字体告警按字族名去重取第一个路径，须指向用户写了的 views.comment。
        assert_eq!(a.font_family.as_deref(), Some("InheritFam"));
        let declared = t.views.declared_font_families();
        let first = declared.iter().find(|(_, f)| f == "InheritFam").unwrap();
        assert_eq!(
            first.0, "views.comment",
            "告警不该指向没写过的 comment_above"
        );
        assert!(c.margin.left.is_some() && c.padding.bottom.is_some());
        assert!(edges_unset(&a.margin), "margin 不继承");
        assert!(edges_unset(&a.padding), "padding 不继承");
    }
}

/// 自己写了的字段用自己的，没写的仍继承；roles 里 `""` 撤销继承；状态 roles 逐键覆盖。
#[test]
fn comment_above_own_values_win_and_empty_role_revokes() {
    for dark in [false, true] {
        let t = roles_theme("above-own", dark);
        let (c, a) = (&t.views.comment, &t.views.comment_above);
        assert_eq!(a.font_size, -6.0, "自己写的字号");
        assert_eq!(c.font_size, -2.0);
        assert_eq!(a.text_color, c.text_color, "没写的颜色仍继承");
        assert_eq!(a.padding.bottom, Some(Dim::Dp(3.0)));
        assert!(edges_unset(&a.margin));
        assert!(!a.roles.contains_key("pinyin"), "\"\" 撤销、不再回退");
        assert_eq!(a.roles["code_rev"], hex("#C00000"), "未提及的角色继承");
        assert!(c.roles.contains_key("pinyin"), "comment 自身不受影响");
        let sel = a.selected.as_ref().unwrap();
        assert_eq!(sel.roles["code_rev"], hex("#00FF00"), "选中态自己写的胜出");
    }
}

/// 出厂角色色表（设计 §18）：`_base` 配、全部出厂主题继承且不改——只有气泡配，取本主题的
/// `tooltip_*`（主题 roles 里的 `${token}` 按字面解析，不做 `tooltip_` 作用域查找，§6.2，
/// 故必须显式写 `tooltip_*`）。注释不配（用户要求保持原外观），状态 patch 与其余节点也一律不配。
#[test]
fn factory_theme_roles_are_the_base_table() {
    let ids = crate::list_theme_ids(&data_dir_for_roles());
    for must in ["_base", "_qingfeng", "default", "msime"] {
        assert!(ids.iter().any(|i| i == must), "枚举漏了 {must}：{ids:?}");
    }
    for name in &ids {
        for dark in [false, true] {
            let t = roles_theme(name, dark);
            let v = &t.views;
            let mut nodes = vec![
                ("window", &v.window),
                ("preedit_bar", &v.preedit_bar),
                ("candidate_list", &v.candidate_list),
                ("item", &v.item),
                ("index", &v.index),
                ("text", &v.text),
                ("comment", &v.comment),
                ("accent_bar", &v.accent_bar),
                ("footer_bar", &v.footer_bar),
                ("mode_label", &v.mode_label),
            ];
            for (k, n) in [
                ("status", &v.status),
                ("tooltip", &v.tooltip),
                ("toast", &v.toast),
                ("menu.root", &v.menu_root),
                ("menu.item", &v.menu_item),
                ("menu.separator", &v.menu_separator),
            ] {
                if let Some(n) = n {
                    nodes.push((k, n));
                }
            }
            let pal = |k: &str| t.palette[k];
            let want_tooltip: std::collections::HashMap<String, Rgba> = [
                ("full_text", "tooltip_accent_text"),
                ("readings", "tooltip_accent_text"),
                ("word_code", "tooltip_success"),
                ("code_source", "tooltip_info"),
                ("chaizi", "tooltip_warning"),
                ("chaizi_all", "tooltip_warning"),
                ("chaizi_code", "tooltip_info"),
                ("chaizi_code_all", "tooltip_info"),
                ("unicode", "tooltip_error"),
                ("unicode_all", "tooltip_error"),
            ]
            .map(|(r, k)| (r.to_string(), pal(k)))
            .into();
            for (k, n) in nodes {
                let states = [
                    Some(n),
                    n.selected.as_deref(),
                    n.hover.as_deref(),
                    n.disabled.as_deref(),
                ];
                for (i, st) in states.into_iter().enumerate() {
                    let Some(st) = st else { continue };
                    let want = match (k, i) {
                        ("tooltip", 0) => &want_tooltip,
                        ("text", 0) => {
                            // text 常态只允许正确拼音提示段的角色色（候选正文里的 `(ping)`）。
                            assert!(
                                st.roles.keys().all(|r| r == "pinyin_hint"),
                                "{name} dark={dark} text.roles 只允许 pinyin_hint"
                            );
                            continue;
                        }
                        ("comment", 0) => {
                            // comment 常态只允许 `${correct_pinyin}` 的角色色（与 pinyin_hint 同色）。
                            assert!(
                                st.roles.keys().all(|r| r == "correct_pinyin"),
                                "{name} dark={dark} comment.roles 只允许 correct_pinyin"
                            );
                            continue;
                        }
                        _ => {
                            assert!(
                                st.roles.is_empty(),
                                "{name} dark={dark} {k}[{i}]：出厂只在注释 / 气泡常态配 roles"
                            );
                            continue;
                        }
                    };
                    assert_eq!(&st.roles, want, "{name} dark={dark} {k}");
                }
            }
        }
    }
}

/// 出厂主题的 `pinyin_hint` 与窗口底色对比度 >= 3:1，且不与候选正文色 / 注释色相同。
#[test]
fn factory_pinyin_hint_is_readable_and_distinct() {
    use crate::palette::contrast_ratio as contrast;
    let ids = crate::list_theme_ids(&data_dir_for_roles());
    for name in ids
        .iter()
        .filter(|n| ["_base", "_qingfeng", "default"].contains(&n.as_str()))
    {
        for dark in [false, true] {
            let t = roles_theme(name, dark);
            let v = &t.views;
            let hint = crate::pinyin_hint_color(
                &v.text,
                &v.comment,
                crate::TextState::Normal,
                [30, 30, 30, 255],
                [150, 150, 150, 255],
                None,
            );
            assert_eq!(hint, v.text.roles["pinyin_hint"], "{name} dark={dark}");
            assert_eq!(
                v.comment.roles.get("correct_pinyin"),
                Some(&hint),
                "{name} dark={dark}：${{correct_pinyin}} 与内嵌提示同色"
            );
            let bg = v.window.bg_color.expect("window 底色");
            assert!(
                contrast(hint, bg) >= 3.0,
                "{name} dark={dark} 对比度 {}",
                contrast(hint, bg)
            );
            assert_ne!(Some(hint), v.text.text_color);
            assert_ne!(Some(hint), v.comment.text_color);
        }
    }
}
