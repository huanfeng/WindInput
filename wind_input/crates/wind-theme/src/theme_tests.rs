use super::*;
use crate::schema::Dim;

/// 解析 → 归一化 → 类型化主文与竖排变体（同 `load_typed_dirs` 的后半段）。
fn typed_with_vertical(s: &str) -> Theme {
    let n = crate::normalize::normalize_theme(s.parse::<Value>().unwrap());
    let v = vertical_variant(&n);
    let mut t: Theme = n.try_into().unwrap();
    t.vertical = v.map(|v| Box::new(v.try_into().unwrap()));
    t
}

/// `[节点.vertical]` 深合并进节点：写了的字段竖排覆盖，没写的沿用横排；横排主文不变。
#[test]
fn vertical_patch_overlays_only_written_fields() {
    let t = typed_with_vertical(
        "[window]\nradius = 20\npadding = [6, 8]\n[window.vertical]\nradius = 6\n[item]\nradius = 4\n",
    );
    let h = t.views.as_ref().unwrap();
    assert_eq!(h.window.border.radius, Some(Dim::Dp(20.0)));
    let v = t.vertical.as_ref().unwrap().views.as_ref().unwrap();
    assert_eq!(v.window.border.radius, Some(Dim::Dp(6.0)));
    assert_eq!(v.window.padding.top, h.window.padding.top, "没写的字段沿用");
    assert_eq!(v.item.border.radius, Some(Dim::Dp(4.0)));
}

#[test]
fn no_vertical_patch_means_no_variant() {
    let t = typed_with_vertical("[window]\nradius = 20\n");
    assert!(t.vertical.is_none());
}

/// 只有候选窗自己的节点认 `vertical`；别的窗口（tooltip / status / toast）写了也不产生变体。
#[test]
fn vertical_on_other_windows_is_ignored() {
    let t = typed_with_vertical("[tooltip]\nradius = 4\n[tooltip.vertical]\nradius = 9\n");
    assert!(t.vertical.is_none());
}

/// 竖排里写翻页字符，横排的翻页图不能把它挡死（同 base 链「本层写了字符就让位」）。
#[test]
fn vertical_arrow_char_drops_horizontal_image() {
    let t = typed_with_vertical(
        "[footer_bar]\nprev_image = { ref = \"chevron.svg\" }\n[footer_bar.vertical]\nprev_char = \"^\"\n",
    );
    let h = t.views.as_ref().unwrap().footer_bar.prev_image.clone();
    let v = t.vertical.as_ref().unwrap().views.as_ref().unwrap();
    assert!(h.is_some(), "横排仍用图");
    assert!(v.footer_bar.prev_image.is_none(), "竖排的图应让位给字符");
    assert_eq!(v.footer_bar.prev_char.as_deref(), Some("^"));
}

#[test]
fn meta_from_text_extracts_name() {
    let s = "[meta]\nname = \"测试主题\"\nauthor = \"me\"\n";
    let m = meta_from_text(s).expect("应解析出 meta");
    assert_eq!(m.name, "测试主题");
    assert_eq!(m.author, "me");
    // 无 meta → None
    assert!(meta_from_text("foo = 1\n").is_none());
}

#[test]
fn validate_text_accepts_valid_rejects_garbage() {
    assert!(validate_text("[meta]\nname = \"ok\"\n").is_ok());
    // 非法 TOML
    assert!(validate_text("  : : :\n\t- bad").is_err());
}

#[test]
fn plain_slug_rejects_paths() {
    for ok in [
        "default",
        "_base",
        "my-theme.v2",
        "主题",
        "清风·浅",
        "console",
        "com0",
        "lpt",
        "a~b",
        "~",
        "conin",
        "x.wtheme",
        "a~12b",
        "Theme~2 Dark",
        "longname~1",
        "清风~1",
    ] {
        assert!(is_plain_slug(ok), "{ok}");
    }
    for bad in [
        "",
        ".",
        "..",
        "../x",
        "a/b",
        "a\\b",
        "C:x",
        "/etc",
        "a..b",
        "a\nb",
        "x.",
        "x ",
        "CON",
        "nul",
        "Aux.txt",
        "com1",
        "LPT9.toml",
        "CONIN$",
        "conout$.txt",
        "COM¹",
        "lpt³",
        "VERYLO~1",
        "AB~12.TOM",
        "theme~1",
        "x.wtheme-tmp",
        "x.WTHEME-OLD",
    ] {
        assert!(!is_plain_slug(bad), "{bad:?}");
    }
    assert!(validate_text("base = \"../../x\"\n").is_err());
    assert!(find_theme_dir(&[PathBuf::from("/")], "..").is_none());
}

#[test]
fn slug_from_name_derives_plain_ids() {
    for (name, want) in [
        ("我的主题", "我的主题"),
        ("  mine  ", "mine"),
        ("Nord: Dark", "Nord- Dark"),
        ("黑/白", "黑-白"),
        ("a\\b", "a-b"),
        ("../../victim", "----victim"),
        ("v1.0...", "v1.0-"),
        ("尾巴. . ", "尾巴"),
        ("Aux", "theme-Aux"),
        ("com1.v2", "theme-com1.v2"),
        ("VERYLO~1", "VERYLO-1"),
        ("x.wtheme-tmp", "theme"),
        ("", "theme"),
        (" . ", "theme"),
    ] {
        let got = slug_from_name(name);
        assert_eq!(got, want, "{name:?}");
        assert!(is_plain_slug(&got), "{name:?} → {got:?}");
    }
}

#[test]
fn load_typed_dirs_rejects_missing_base() {
    // 派生主题引用不存在的 base：load_merged_dirs_at 的 find_theme_dir 应报错，
    // 供导入链路（web_theme_import_text）据此判定依赖校验失败并回滚。
    let dir = std::env::temp_dir().join(format!("wind_theme_missing_base_{}", std::process::id()));
    let theme_dir = dir.join("derived");
    std::fs::create_dir_all(&theme_dir).unwrap();
    std::fs::write(
        theme_dir.join(THEME_FILE),
        "base = \"no-such-base\"\n[meta]\nname = \"derived\"\n",
    )
    .unwrap();

    let err = load_typed_dirs(std::slice::from_ref(&dir), "derived").unwrap_err();
    assert!(
        err.to_string().contains("no-such-base"),
        "错误信息应指出缺失的 base 主题名: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_deep_overrides() {
    let base: Value = toml::from_str("[window]\npadding = 8\nradius = 4\n").unwrap();
    let over: Value = toml::from_str("[window]\nradius = 6\n").unwrap();
    let m = merge(base, over);
    let w = m.get("window").unwrap();
    assert_eq!(w.get("padding").unwrap().as_integer(), Some(8)); // base 保留
    assert_eq!(w.get("radius").unwrap().as_integer(), Some(6)); // over 覆盖
}

/// inline table 同样深合并——子主题只写其中一个键，base 的其余键必须留下。
///
/// 退化成整表替换时，子主题写 `prev_image = { tint = "…" }` 会顺手抹掉 `ref`，箭头静默
/// 退化成文字 ‹ ›、无报错无日志，故与表节点层分开各钉一次。
#[test]
fn merge_deep_overrides_inline_tables() {
    let base: Value = toml::from_str(
        "[footer_bar]\nprev_image = { ref = \"a.svg\", mode = \"center\", tint = \"#111111\" }\n",
    )
    .unwrap();
    let over: Value =
        toml::from_str("[footer_bar]\nprev_image = { tint = \"#222222\" }\n").unwrap();
    let img = merge(base, over);
    let img = img
        .get("footer_bar")
        .and_then(|f| f.get("prev_image"))
        .expect("prev_image");
    assert_eq!(img.get("ref").and_then(|v| v.as_str()), Some("a.svg"));
    assert_eq!(img.get("mode").and_then(|v| v.as_str()), Some("center"));
    assert_eq!(img.get("tint").and_then(|v| v.as_str()), Some("#222222"));
}

/// 走一遍真实调用序列的中段：两层各自归一化 → 互斥规则 → 深合并。
///
/// 归一化不能省：`load_merged_dirs_at` 就是这么排的，而 `drop_inherited_arrow_images`
/// 只认归一化后的 `views.footer_bar`。喂扁平形态给它等于测一条不存在的路径。
fn merged(base: &str, over: &str) -> Value {
    let mut b = normalize_theme(toml::from_str(base).unwrap());
    let o = normalize_theme(toml::from_str(over).unwrap());
    drop_inherited_arrow_images(&mut b, &o);
    merge(b, o)
}

/// 取归一化后 `views.footer_bar` 下某键（不存在则 None）。
fn footer<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get("views")
        .and_then(|x| x.get("footer_bar"))
        .and_then(|f| f.get(key))
}

/// 派生主题写了翻页字符 → 继承来的箭头图让位，字符才是作者要的东西。
///
/// 不成立的话 `base = "_base"` 的主题写 `prev_char` 都石沉大海，而补救写法
/// （`prev_image = { ref = "" }`）无从猜起。
#[test]
fn explicit_char_drops_inherited_arrow_image() {
    let m = merged(
        "[footer_bar]\nprev_image = { ref = \"chevron.svg\" }\nnext_image = { ref = \"chevron2.svg\" }\n",
        "[footer_bar]\nprev_char = \"$\"\nnext_char = \")\"\n",
    );
    assert!(footer(&m, "prev_image").is_none(), "继承的上一页图应让位");
    assert!(footer(&m, "next_image").is_none(), "继承的下一页图应让位");
    assert_eq!(footer(&m, "prev_char").and_then(|v| v.as_str()), Some("$"));
}

/// 让位只按「本层写没写」判，与写的是什么值无关：
/// 上一页写了字符 → 上一页的图让位；下一页没写 → 下一页的图原样继承。
/// 两侧互不牵连，否则「只想改一边」的主题会莫名丢掉另一边的图。
#[test]
fn inherited_image_survives_on_the_side_without_char() {
    let m = merged(
        "[footer_bar]\nprev_image = { ref = \"a.svg\" }\nnext_image = { ref = \"b.svg\" }\n",
        "[footer_bar]\nprev_char = \"$\"\n",
    );
    assert!(footer(&m, "prev_image").is_none());
    assert_eq!(
        footer(&m, "next_image")
            .and_then(|i| i.get("ref"))
            .and_then(|v| v.as_str()),
        Some("b.svg"),
        "没写 next_char 的那侧不受影响"
    );
}

/// 同层仍是图优先：一个主题自己把图和字符都写上，图留着
/// （字符退为图 `ref` 解析不出时的兜底）。让位只针对**继承来的**图。
#[test]
fn same_layer_image_and_char_both_kept() {
    let m = merged(
        "[footer_bar]\nprev_image = { ref = \"inherited.svg\", mode = \"center\" }\n",
        "[footer_bar]\nprev_char = \"$\"\nprev_image = { ref = \"own.svg\" }\n",
    );
    let img = footer(&m, "prev_image").expect("prev_image");
    assert_eq!(
        img.get("ref").and_then(|v| v.as_str()),
        Some("own.svg"),
        "本层自己写的图不该被本层自己写的字符挤掉"
    );
    // mode 是判别点：本层没写它, 只有「没让位、照常深合并」才留得下。
    // 只断言 ref 的话, 让位与否都得到 own.svg, 这条测试就永远不会红。
    assert_eq!(
        img.get("mode").and_then(|v| v.as_str()),
        Some("center"),
        "同层写了图 → 不该触发让位, 继承的图应照常深合并"
    );
    assert_eq!(footer(&m, "prev_char").and_then(|v| v.as_str()), Some("$"));
}

/// `ref = ""`（_qingfeng / msime 清掉继承图的写法）仍是**本层写的图**，
/// 照常保留 —— 它一路传到渲染层才解析成空、回退字符档，与本规则不冲突。
#[test]
fn explicit_empty_ref_is_still_an_own_image() {
    let m = merged(
        "[footer_bar]\nprev_image = { ref = \"chevron.svg\", mode = \"center\" }\n",
        "[footer_bar]\nprev_char = \"$\"\nprev_image = { ref = \"\" }\n",
    );
    let img = footer(&m, "prev_image").expect("prev_image");
    assert_eq!(
        img.get("ref").and_then(|v| v.as_str()),
        Some(""),
        "显式清空是本层的意思, 该原样留着"
    );
    // 同上：ref 两条分支都得空串, mode 才是判别点。
    assert_eq!(
        img.get("mode").and_then(|v| v.as_str()),
        Some("center"),
        "本层写了图(哪怕是空 ref) → 不触发让位"
    );
}

/// 在临时目录里摆一条 `themebase ← derived` 的主题链，走真实加载链取回类型化结果。
///
/// `tag` 只为让并发跑的用例各用各的目录。
fn load_chain(tag: &str, base_toml: &str, derived_toml: &str) -> crate::schema::Theme {
    let dir = std::env::temp_dir().join(format!("wind_theme_{}_{}", tag, std::process::id()));
    // 断言失败时下面的 remove 不会执行 —— 开头先清一次，免得上一轮的残留影响这一轮。
    let _ = std::fs::remove_dir_all(&dir);
    let base_dir = dir.join("themebase");
    let derived_dir = dir.join("derived");
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::create_dir_all(&derived_dir).unwrap();
    std::fs::write(base_dir.join(THEME_FILE), base_toml).unwrap();
    std::fs::write(derived_dir.join(THEME_FILE), derived_toml).unwrap();
    let t = load_typed_dirs(std::slice::from_ref(&dir), "derived").expect("load derived");
    let _ = std::fs::remove_dir_all(&dir);
    t
}

/// base 写简写、派生只覆盖其中一边时，其余三边必须还在。
///
/// 归一化若排在整条链合并之后，`padding = [6, 8]` 会被 `{ left = 20 }` 整个顶掉、上右下凭空
/// 消失，且主题照常加载、无报错无日志。
#[test]
fn shorthand_in_base_survives_partial_override() {
    let t = load_chain(
        "shorthand",
        "[meta]\nname = \"themebase\"\n[window]\npadding = [6, 8]\n",
        "base = \"themebase\"\n[meta]\nname = \"derived\"\n[window]\npadding = { left = 20 }\n",
    );
    let pad = &t.views.as_ref().expect("views").window.padding;
    let dp = |d: Option<Dim>| d.map(|d| d.resolve(1.0, 0.0));
    assert_eq!(dp(pad.left), Some(20.0), "派生覆盖的那边");
    assert_eq!(dp(pad.top), Some(6.0), "base 简写的上边不该丢");
    assert_eq!(dp(pad.right), Some(8.0), "右边不该丢");
    assert_eq!(dp(pad.bottom), Some(6.0), "下边不该丢");
}

/// 派生主题写一个纯色背景，就该把 base 那层的背景图盖掉 —— 整体重置，不是只换底色。
///
/// 绘制顺序是底色 → 渐变 → 边框 → 背景图（`wind-ui/src/view.rs`），继承来的图留着就盖在
/// 派生的底色上。标量写法靠合并时「标量 vs 表」的整体替换语义——归一化若在合并前把它展成
/// `{ color = … }`，这条就没了。
#[test]
fn scalar_background_in_derived_resets_inherited_image() {
    let t = load_chain(
        "bgreset",
        "[meta]\nname = \"themebase\"\n[window]\nbackground = { color = \"#FF0000\", image = { ref = \"p.png\" } }\n",
        "base = \"themebase\"\n[meta]\nname = \"derived\"\n[window]\nbackground = \"#00FF00\"\n",
    );
    let bg = &t.views.as_ref().expect("views").window.background;
    assert!(
        bg.image.is_none(),
        "派生写了纯色 → 继承来的背景图应整体让位, 实得 {:?}",
        bg.image
    );
}

/// 派生主题把 slice_repeat 写错时，要退回拉伸，而不是静默继承 base 的 repeat。
///
/// `expand_axes` 承诺「认不出的形态一律丢弃 ⇒ 两轴拉伸」：若产出空表，进 base 深合并后
/// 什么也盖不住，继承的 repeat 照旧生效。prev_image/layers 这类字段的归一化排在合并之前，
/// 所以只在这几个字段上现形。
#[test]
fn bad_slice_repeat_in_derived_falls_back_to_stretch() {
    let t = load_chain(
        "sliceaxes",
        "[meta]\nname = \"themebase\"\n[footer_bar]\n\
             prev_image = { ref = \"a.svg\", mode = \"nine_slice\", slice_repeat = \"repeat\" }\n",
        "base = \"themebase\"\n[meta]\nname = \"derived\"\n[footer_bar]\n\
             prev_image = { ref = \"a.svg\", mode = \"nine_slice\", slice_repeat = 42 }\n",
    );
    let im = t
        .views
        .as_ref()
        .expect("views")
        .footer_bar
        .prev_image
        .as_ref()
        .expect("prev_image");
    assert_ne!(
        im.slice_repeat.x.as_deref(),
        Some("repeat"),
        "写错的值该退回拉伸, 不该继承 base 的 repeat"
    );
    assert_ne!(im.slice_repeat.y.as_deref(), Some("repeat"));
}

/// `[views.toolbar.button.mode.chinese]` 里的简写同样要展开。
///
/// 归一化对已存在的 `mode` 表也要递归，否则 `padding = 5` 到 serde 是
/// `invalid type: integer 5`，整份主题加载失败。
#[test]
fn shorthand_inside_nested_toolbar_mode_is_expanded() {
    let t = load_chain(
        "toolbarmode",
        "[meta]\nname = \"themebase\"\n",
        "base = \"themebase\"\n[meta]\nname = \"derived\"\n\
             [views.toolbar.button.mode.chinese]\npadding = 5\nradius = 2\n",
    );
    let cn = t
        .views
        .as_ref()
        .expect("views")
        .toolbar
        .as_ref()
        .expect("toolbar")
        .button
        .mode
        .as_ref()
        .map(|m| &m.chinese)
        .expect("mode.chinese");
    assert_eq!(
        cn.padding.top.map(|d| d.resolve(1.0, 0.0)),
        Some(5.0),
        "标量简写应已展开成四边"
    );
    assert_eq!(
        cn.border.radius.map(|d| d.resolve(1.0, 0.0)),
        Some(2.0),
        "radius → border.radius 的搬迁在这一层同样要做"
    );
}

/// 两层用不同书写形态（base 规范嵌套 / 派生扁平）时，两边的节点都要留下。
///
/// 归一化排在合并之后的话，两种形态在合并阶段是互不相干的两个键，最后 `views` 被整块
/// 替换，base 那份连同 `views.item` 这类节点一起蒸发。
#[test]
fn mixed_writing_forms_keep_both_sides() {
    let t = load_chain(
        "mixedform",
        "[meta]\nname = \"themebase\"\n[views.item]\npadding = 4\n[views.window]\npadding = 6\n",
        "base = \"themebase\"\n[meta]\nname = \"derived\"\n[window]\npadding = 12\n",
    );
    let v = t.views.as_ref().expect("views");
    assert_eq!(
        v.window.padding.top.map(|d| d.resolve(1.0, 0.0)),
        Some(12.0),
        "派生（扁平）覆盖 base（嵌套）的同名节点"
    );
    assert_eq!(
        v.item.padding.top.map(|d| d.resolve(1.0, 0.0)),
        Some(4.0),
        "base 那份 views 里没被派生碰过的节点必须留下 —— 整块替换正是从这里丢东西的"
    );
}

/// 规范嵌套形态里写简写不该炸：`normalize_node` 也要作用于已存在的 `views` 表，否则
/// `[views.window] padding = 6` 到 serde 是 `invalid type: integer 6`，整份主题加载失败。
#[test]
fn shorthand_inside_nested_views_is_expanded() {
    let t = load_chain(
        "nestedshorthand",
        "[meta]\nname = \"themebase\"\n",
        "base = \"themebase\"\n[meta]\nname = \"derived\"\n[views.window]\npadding = 6\nradius = 3\n",
    );
    let w = &t.views.as_ref().expect("views").window;
    let dp = |d: Option<Dim>| d.map(|d| d.resolve(1.0, 0.0));
    assert_eq!(dp(w.padding.top), Some(6.0), "标量简写应已展开成四边");
    assert_eq!(dp(w.padding.left), Some(6.0));
    assert_eq!(
        dp(w.border.radius),
        Some(3.0),
        "radius → border.radius 的搬迁在嵌套形态里同样要做"
    );
}

/// 走**真实加载链**（写盘 → load_typed_dirs → 合并 → normalize → 类型化）验一次让位。
///
/// 上面几条直接调 `drop_inherited_arrow_images`，删掉 `load_merged_dirs_at` 里那行调用
/// 它们不会红；这条从磁盘上两份 toml 出发，挂住调用点、normalize 与类型化整条链。
#[test]
fn inherited_image_yields_through_the_real_load_chain() {
    let dir = std::env::temp_dir().join(format!("wind_theme_arrow_chain_{}", std::process::id()));
    let base_dir = dir.join("themebase");
    let derived_dir = dir.join("derived");
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::create_dir_all(&derived_dir).unwrap();
    std::fs::write(
        base_dir.join(THEME_FILE),
        "[meta]\nname = \"themebase\"\n[footer_bar]\n\
             prev_image = { ref = \"chevron_prev.svg\" }\n\
             next_image = { ref = \"chevron_next.svg\" }\n",
    )
    .unwrap();
    std::fs::write(
        derived_dir.join(THEME_FILE),
        "base = \"themebase\"\n[meta]\nname = \"derived\"\n[footer_bar]\nprev_char = \"$\"\n",
    )
    .unwrap();

    let t = load_typed_dirs(std::slice::from_ref(&dir), "derived").expect("load derived");
    let footer = &t.views.as_ref().expect("views").footer_bar;
    assert_eq!(
        footer.prev_char.as_deref(),
        Some("$"),
        "本层写的字符要活到类型化之后"
    );
    assert!(
        footer.prev_image.is_none(),
        "写了 prev_char 的那侧, 继承来的图应在合并层就让位"
    );
    assert!(
        footer.next_image.is_some(),
        "没写 next_char 的那侧, 继承来的图原样保留"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 规范嵌套形态（`[views.footer_bar]`）同样受规则约束 —— 合并虽跑在 normalize
/// 之前, 但主题文件本就可以直接写 views 表, 只认扁平形态会留下静默盲区。
#[test]
fn rule_applies_to_nested_views_form() {
    let m = merged(
        "[views.footer_bar]\nprev_image = { ref = \"chevron.svg\" }\n",
        "[views.footer_bar]\nprev_char = \"$\"\n",
    );
    assert!(footer(&m, "prev_image").is_none(), "嵌套形态也应让位");
    assert_eq!(footer(&m, "prev_char").and_then(|v| v.as_str()), Some("$"));
}

/// 文本加载与目录加载同一条合并链：内置主题逐个比对合并结果（类型化那步两边共用
/// `type_merged`；`Theme` 里有 HashMap，Debug 串不能直接比）。
#[test]
fn text_loading_matches_directory_loading_for_builtins() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes");
    let dirs = vec![root.clone()];
    let read = |id: &str| Ok(std::fs::read_to_string(root.join(id).join(THEME_FILE))?);
    let ids = list_theme_ids(&root);
    assert!(ids.iter().any(|i| i == "default"), "{ids:?}");
    for id in ids {
        let from_dir = load_merged_dirs(&dirs, &id, 0).unwrap();
        let text = read(&id).unwrap();
        let from_text = load_merged_text_at(&text, &id, 0, &read).unwrap();
        assert_eq!(from_dir, from_text, "{id}");
        load_typed_text(&text, &id, &read).unwrap();
    }
}

#[test]
fn text_loading_reports_errors() {
    let none = |b: &str| -> anyhow::Result<String> { anyhow::bail!("没有 {b}") };
    let e = load_typed_text("[meta\nname = 1", "", &none).unwrap_err();
    assert!(e.to_string().starts_with("parse theme:"), "{e}");
    let e = load_typed_text("base = \"missing\"\n", "", &none).unwrap_err();
    assert_eq!(
        e.to_string(),
        "没有 missing",
        "取不到 base 的说明由调用方给"
    );
    // base 只认普通 slug：路径形态连查都不查。
    let asked = std::cell::Cell::new(false);
    let spy = |_: &str| {
        asked.set(true);
        Ok(String::new())
    };
    assert!(load_typed_text("base = \"../x\"\n", "", &spy).is_err());
    assert!(!asked.get(), "非 slug 的 base 不该去取");
    // base 写成自己的 id 视同没写（同目录加载）。
    load_typed_text("base = \"mine\"\n", "mine", &spy).unwrap();
    assert!(!asked.get(), "指向自己的 base 不该去取");
    // 循环继承止于深度上限。
    let cyc = |id: &str| {
        Ok(format!(
            "base = \"{}\"\n",
            if id == "a" { "b" } else { "a" }
        ))
    };
    let e = load_typed_text("base = \"a\"\n", "", &cyc).unwrap_err();
    assert!(e.to_string().contains("too deep"), "{e}");
}
