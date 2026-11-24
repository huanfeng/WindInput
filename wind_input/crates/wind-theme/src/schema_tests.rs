use super::*;
use crate::theme::{load_typed, load_typed_dirs};
use std::path::Path;

fn data_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes")
}

fn testdata_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/themes")
}

#[test]
fn test_dim_parse() {
    assert_eq!(parse_dim("8"), Ok(Dim::Dp(8.0)));
    assert_eq!(parse_dim("8dp"), Ok(Dim::Dp(8.0)));
    assert_eq!(parse_dim("1px"), Ok(Dim::Px(1.0)));
    assert_eq!(parse_dim("50%"), Ok(Dim::Pct(50.0)));
    assert!(parse_dim("nope").is_err());
    // 求值语义
    assert_eq!(Dim::Dp(8.0).resolve(2.0, 100.0), 16.0); // dp×scale
    assert_eq!(Dim::Px(1.0).resolve(2.0, 100.0), 1.0); // px 直给
    assert_eq!(Dim::Pct(50.0).resolve(2.0, 100.0), 50.0); // pct×host
}

#[test]
fn test_ld_select() {
    let s = Ld::Scalar("#FFF".into());
    assert_eq!(s.select(false), Some("#FFF"));
    assert_eq!(s.select(true), Some("#FFF"));
    let v = Ld::Variant {
        light: Some("#FFF".into()),
        dark: Some("#000".into()),
    };
    assert_eq!(v.select(false), Some("#FFF"));
    assert_eq!(v.select(true), Some("#000"));
    // 缺侧回退另一侧
    let only_light = Ld::Variant {
        light: Some("#FFF".into()),
        dark: None,
    };
    assert_eq!(only_light.select(true), Some("#FFF"));
    // derive {enabled, algorithm} 形态 → 皆 None
    let derive: Ld = toml::from_str("enabled = true\nalgorithm = \"hsl-shift\"").unwrap();
    assert_eq!(derive.select(false), None);
}

#[test]
fn test_all_builtin_themes_typecheck() {
    let dir = data_dir();
    for name in ["_base", "default", "msime"] {
        let t = load_typed(&dir, name).unwrap_or_else(|e| panic!("load {name}: {e}"));
        assert!(t.views.is_some(), "{name} 应有 views");
        assert!(t.colors.is_some(), "{name} 应有 colors");
    }
}

#[test]
fn test_jidian_classic_rich_features() {
    let t = load_typed_dirs(&[testdata_dir(), data_dir()], "jidian-classic")
        .expect("load jidian-classic");
    // base 继承 _base → colors 合并进来
    assert!(t.colors.is_some(), "应从 _base 继承 colors");
    // resources：panel/sel 双变体 + mark 标量
    let panel = t.resources.get("panel").expect("panel resource");
    assert_eq!(panel.select(false), Some("panel.png"));
    assert_eq!(panel.select(true), Some("panel-dark.png"));
    assert_eq!(
        t.resources.get("mark").and_then(|m| m.select(false)),
        Some("mark.png")
    );

    let views = t.views.as_ref().expect("views");
    // window 九宫格背景图
    let bg = views
        .window
        .background
        .image
        .as_ref()
        .expect("window bg image");
    assert_eq!(bg.reference, "panel");
    assert_eq!(bg.mode, "nine_slice");
    assert_eq!(bg.slice.top, Some(Dim::Dp(8.0)));
    // window 阴影 blur
    let sh = views.window.shadow.as_ref().expect("window shadow");
    assert_eq!(sh.blur, Some(Dim::Dp(8.0)));
    // window layers：右下角水印 z=1
    assert_eq!(views.window.layers.len(), 1);
    let mark = &views.window.layers[0];
    assert_eq!(mark.reference, "mark");
    assert_eq!(mark.z, 1);
    assert_eq!(mark.anchor, "bottom-right");
    assert_eq!(mark.offset.x, Some(Dim::Dp(-8.0))); // 负偏移内缩
    assert_eq!(mark.opacity, Some(0.9));
    // item 选中态：背景图 + 字重（状态 patch）
    let sel = views.item.selected.as_ref().expect("item.selected");
    assert_eq!(sel.font_weight, Some(600));
    assert_eq!(
        sel.background.image.as_ref().map(|i| i.reference.as_str()),
        Some("sel")
    );
    // accent_bar 启用 + 宽度
    assert_eq!(views.accent_bar.enabled, Some(true));
    assert_eq!(views.accent_bar.width, Some(Dim::Dp(3.0)));
    // 其它窗口也吃九宫格 + 水印
    let status = views.status.as_ref().expect("status view");
    assert!(status.background.image.is_some());
    assert_eq!(status.layers.len(), 1);
    assert!(views.menu.is_some());
    assert!(views.toast.is_some());
}
