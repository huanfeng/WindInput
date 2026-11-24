use super::*;
use wind_theme::Resolved;

/// 本模块是主题求值形态到渲染形态的**最后一环**：这里漏传一个字段，上游求值
/// 测得再全也没用——画出来仍是旧行为，而且一声不响。
///
/// `slice_repeat` 是第一个用这条护栏钉住的字段：漏传它的后果是九宫中段永远拉伸，
/// 也就是候选窗变宽时纹理跟着「呼吸」，正是它要修的那个毛病。
#[test]
fn rv_image_carries_fill_shape_through() {
    let theme = Resolved::default();
    let im = RvImage {
        reference: "panel.png".to_string(),
        mode: "nine_slice".to_string(),
        slice: [1.0, 2.0, 3.0, 4.0],
        slice_repeat: [true, false],
        opacity: 0.5,
        ..Default::default()
    };

    let out = rv_image(&theme, Some(&im), 1.0).expect("有 ref 就该出图");

    assert_eq!(out.mode, "nine_slice");
    assert_eq!(out.slice, [1.0, 2.0, 3.0, 4.0]);
    assert_eq!(out.slice_repeat, [true, false], "中段重复没传到渲染层");
    assert_eq!(out.opacity, 0.5);
}

/// 背景图何时走定位，逐条对齐编辑器 `bgPositioned()`：anchor / 任一非零偏移 / 非零尺寸。
/// 什么都没配（或偏移写成 0）必须仍是铺满——既有主题一个像素都不能变。
#[test]
fn rv_image_places_only_when_positioning_is_configured() {
    use wind_theme::schema::Dim;
    let theme = Resolved::default();
    let base = RvImage {
        reference: "girl.png".to_string(),
        ..Default::default()
    };
    let placed = |im: RvImage| rv_image(&theme, Some(&im), 2.0).unwrap().place;

    assert_eq!(placed(base.clone()), None, "没配定位却不再铺满");
    let zero_off = RvImage {
        offset_x: Some(Dim::Dp(0.0)),
        offset_y: Some(Dim::Pct(0.0)),
        ..base.clone()
    };
    assert_eq!(placed(zero_off), None, "零偏移不算配了定位");
    for im in [
        RvImage {
            anchor: "right".to_string(),
            ..base.clone()
        },
        RvImage {
            offset_y: Some(Dim::Pct(-3.0)),
            ..base.clone()
        },
        RvImage {
            h: 5,
            ..base.clone()
        },
    ] {
        assert!(placed(im).is_some(), "配了定位字段却仍铺满");
    }

    // 换算与覆盖图同一套：dp×scale、百分比原样、尺寸×scale。
    let full = RvImage {
        anchor: "bottom-right".to_string(),
        offset_x: Some(Dim::Dp(-4.0)),
        offset_y: Some(Dim::Pct(10.0)),
        w: 24,
        ..base.clone()
    };
    let p = placed(full.clone()).unwrap();
    assert_eq!(
        p,
        ImagePlace {
            anchor: "bottom-right".to_string(),
            off_x: -8.0,
            off_y: 0.0,
            off_x_pct: 0.0,
            off_y_pct: 10.0,
            w: 48.0,
            h: 0.0,
            scale: 2.0,
        }
    );
    assert_eq!(rv_layers(&theme, &[full], 2.0)[0].place, p);
}

/// 图层 tint 透传：口径同背景图 `rv_image`（未配 = 不染色 `[0,0,0,0]`）。
/// 曾经 schema 有、resolve 有、到这一环被丢——主题配了图层 tint 毫无变化。
#[test]
fn rv_layers_carries_tint() {
    let theme = Resolved::default();
    let tinted = RvImage {
        reference: "mark.svg".to_string(),
        tint: Some([10, 20, 30, 200]),
        ..Default::default()
    };
    let plain = RvImage {
        reference: "mark.svg".to_string(),
        ..Default::default()
    };
    let out = rv_layers(&theme, &[tinted.clone(), plain], 1.0);
    assert_eq!(out[0].tint, [10, 20, 30, 200], "图层 tint 没传到渲染层");
    assert_eq!(out[1].tint, [0, 0, 0, 0], "未配 tint 应原样出图");
    // 与背景图同口径。
    let bg = rv_image(&theme, Some(&tinted), 1.0).unwrap();
    assert_eq!(bg.tint, Some(out[0].tint));
}

/// flip 透传到背景图与图层（同 tint：一环漏掉就是「配了不生效」）。
#[test]
fn rv_image_and_layers_carry_flip() {
    let theme = Resolved::default();
    let flip = wind_theme::Flip { x: true, y: false };
    let im = RvImage {
        reference: "mark.png".to_string(),
        flip,
        ..Default::default()
    };
    assert_eq!(rv_image(&theme, Some(&im), 1.0).unwrap().flip, flip);
    assert_eq!(rv_layers(&theme, &[im], 1.0)[0].flip, flip);
}

/// 内存资源表：登记过的相对引用（字面 ref 与 resources 解析出的相对路径）改写成内存源串，
/// 图片缓存能按它解码；没登记的、data: 与绝对路径照旧。
#[test]
fn memory_assets_take_relative_refs_first() {
    let png = {
        let mut out = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let mut mem = crate::assets::MemoryAssets::new();
    mem.insert("assets/panel.png", png);
    crate::assets::install(crate::assets::next_generation(), &std::sync::Arc::new(mem));
    let mut theme = Resolved::default();
    theme
        .resources
        .insert("panel".into(), "./assets/panel.png".into());

    let lit = asset_path(&theme, "assets/panel.png").unwrap();
    let res = asset_path(&theme, "panel").unwrap();
    assert!(lit.starts_with("mem:"), "{lit}");
    assert_eq!(lit, res, "两种写法指向同一个源");
    let mut cache = crate::image_cache::ImageCache::new();
    assert_eq!(cache.src_size(&lit), Some((3, 2)), "图片缓存能读内存源");

    assert_eq!(
        asset_path(&theme, "other.png").as_deref(),
        Some("other.png")
    );
    assert_eq!(
        asset_path(&theme, "data:image/png;base64,AA==").as_deref(),
        Some("data:image/png;base64,AA==")
    );
    crate::assets::uninstall();
    assert_eq!(
        asset_path(&theme, "panel").as_deref(),
        Some("./assets/panel.png"),
        "撤表后回到原有解析"
    );
}

/// 空 ref = 没有图，不能凭空造一个（调用方据此跳过整层绘制）。
#[test]
fn rv_image_rejects_empty_reference() {
    assert!(rv_image(&Resolved::default(), Some(&RvImage::default()), 1.0).is_none());
    assert!(rv_image(&Resolved::default(), None, 1.0).is_none());
}
