use super::*;
use crate::text::MockText as TextRenderer;
use base64::Engine as _;

const RED: [u8; 4] = [255, 0, 0, 255];
const WHITE: [u8; 3] = [255, 255, 255];

fn data_uri(img: image::RgbaImage) -> String {
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("PNG 编码");
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png.into_inner())
    )
}

fn png_uri(rgba: [u8; 4], w: u32, h: u32) -> String {
    data_uri(image::RgbaImage::from_pixel(w, h, image::Rgba(rgba)))
}

/// 左半红、右半蓝：拿来对拍铺满结果，纯色图看不出拉伸方式变没变。
fn two_tone_uri() -> String {
    data_uri(image::RgbaImage::from_fn(8, 4, |x, _| {
        if x < 4 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 0, 255, 255])
        }
    }))
}

fn image(path: String, place: Option<ImagePlace>) -> ViewImage {
    ViewImage {
        path,
        mode: String::new(),
        slice: [0.0; 4],
        slice_repeat: [false; 2],
        opacity: 1.0,
        tint: None,
        place,
        scale: 1.0,
        flip: wind_theme::Flip::NONE,
    }
}

fn place(anchor: &str) -> ImagePlace {
    ImagePlace {
        anchor: anchor.to_string(),
        ..Default::default()
    }
}

/// 节点 `nw×nh` 置于 `bw×bh` 缓冲左上角，白底；返回 BGRA 缓冲。
fn paint(v: View, nw: f32, nh: f32, bw: usize, bh: usize) -> Vec<u8> {
    let tr = TextRenderer::new("test", 20.0).unwrap();
    let mut v = v.fixed_w(nw).fixed_h(nh);
    v.layout(0.0, 0.0, &tr);
    let mut buf = vec![0u8; bw * bh * 4];
    v.paint(&mut buf, bw as u32, bh as u32, &tr);
    buf
}

fn node() -> View {
    View::container(Layout::Row).bg([255, 255, 255, 255])
}

fn rgb(buf: &[u8], bw: usize, x: usize, y: usize) -> [u8; 3] {
    let i = (y * bw + x) * 4;
    [buf[i + 2], buf[i + 1], buf[i]]
}

fn alpha(buf: &[u8], bw: usize, x: usize, y: usize) -> u8 {
    buf[(y * bw + x) * 4 + 3]
}

/// 圆角裁剪：贴满窗口的直角子节点，四角被窗口弧线削掉；不开则方角会露出来。
#[test]
fn clip_corners_trims_square_children_to_rounded_outline() {
    let build = |clip: bool| {
        let mut root = View::container(Layout::Column)
            .bg([255, 255, 255, 255])
            .radius(12.0);
        if clip {
            root = root.clip_to_corners();
        }
        root.child(
            View::container(Layout::Row)
                .bg([255, 0, 0, 255])
                .fixed_w(60.0)
                .fixed_h(40.0),
        )
    };
    let off = paint(build(false), 60.0, 40.0, 60, 40);
    assert_eq!(rgb(&off, 60, 0, 0), [255, 0, 0], "未开裁剪：方角露出");
    let on = paint(build(true), 60.0, 40.0, 60, 40);
    assert_eq!(alpha(&on, 60, 0, 0), 0, "开裁剪：圆角外保持透明");
    assert_eq!(alpha(&on, 60, 59, 39), 0);
    assert_eq!(rgb(&on, 60, 30, 20), [255, 0, 0], "内部直角填充不受影响");
    assert_eq!(rgb(&on, 60, 30, 0), [255, 0, 0], "直边不受影响");
}

/// 参照实现：整窗备份 + 整窗掩码。角块版必须与它逐字节一致，
/// 含奇数尺寸、胶囊形（半径被钳）、半径大于半边长。
#[test]
fn corner_save_matches_full_window_reference() {
    for &(w, h, rad) in &[
        (60u32, 40u32, 12.0f32),
        (61, 41, 12.0),
        (50, 21, 999.0),
        (51, 20, 999.0),
        (30, 30, 40.0),
        (40, 40, 0.4),
    ] {
        let (bw, bh) = (w + 6, h + 6);
        let mut base = vec![0u8; (bw * bh * 4) as usize];
        fill_rounded(
            &mut base,
            bw,
            bh,
            3.0,
            3.0,
            w as f32,
            h as f32,
            [40, 80, 160, 255],
            rad,
        );
        let rect = Rect {
            x: 3.0,
            y: 3.0,
            w: w as f32,
            h: h as f32,
        };
        // 模拟子节点：斜向渐变风格的不规则内容铺满整个窗口矩形。
        let mut after = base.clone();
        for y in 3..3 + h {
            for x in 3..3 + w {
                let o = ((y * bw + x) * 4) as usize;
                let v = ((x * 7 + y * 13) % 251) as u8;
                after[o..o + 4].copy_from_slice(&[v, 255 - v, v / 2, 255]);
            }
        }
        // 新实现。
        let mut got = after.clone();
        CornerSave::take(&base, bw, bh, rect, rad)
            .unwrap()
            .restore(&mut got, bw, bh);
        // 参照：整窗掩码。
        let mut mask = vec![0u8; (w * h * 4) as usize];
        fill_rounded(&mut mask, w, h, 0.0, 0.0, w as f32, h as f32, [255; 4], rad);
        let mut want = after.clone();
        for y in 0..h {
            for x in 0..w {
                let cov = mask[((y * w + x) * 4 + 3) as usize] as i32;
                let d = (((y + 3) * bw + x + 3) * 4) as usize;
                for c in 0..4 {
                    let (cur, old) = (after[d + c] as i32, base[d + c] as i32);
                    want[d + c] = (old + ((cur - old) * cov + 127) / 255) as u8;
                }
            }
        }
        // tiny-skia 对「部分在画布外」的路径走裁剪光栅化，覆盖率与整窗掩码可差 1 级。
        if let Some(i) = (0..got.len()).find(|&i| got[i].abs_diff(want[i]) > 1) {
            let px = (i / 4) as u32;
            panic!(
                "{w}x{h} r={rad}: ({},{}) ch{} got {} want {}",
                px % bw,
                px / bw,
                i % 4,
                got[i],
                want[i]
            );
        }
    }
}

/// 窗口只有一部分在缓冲内（被屏幕边缘截断）时不越界、不 panic。
#[test]
fn corner_save_survives_rect_partly_outside_buffer() {
    let mut buf = vec![255u8; 20 * 20 * 4];
    let rect = Rect {
        x: -5.0,
        y: 10.0,
        w: 40.0,
        h: 30.0,
    };
    let saved = CornerSave::take(&buf, 20, 20, rect, 8.0).unwrap();
    saved.restore(&mut buf, 20, 20);
}

/// z>=0 覆盖图也要被圆角轮廓裁掉（还原放在它之后）。
#[test]
fn clip_corners_also_trims_front_layers() {
    let mut root = View::container(Layout::Column)
        .bg([255, 255, 255, 255])
        .radius(12.0)
        .clip_to_corners();
    root.layers.push(ViewLayer {
        path: png_uri(RED, 4, 4),
        z: 1,
        place: ImagePlace {
            w: 60.0,
            h: 40.0,
            ..place("top-left")
        },
        tint: [0, 0, 0, 0],
        opacity: 1.0,
        flip: wind_theme::Flip::NONE,
    });
    let buf = paint(root.fixed_w(60.0).fixed_h(40.0), 60.0, 40.0, 60, 40);
    assert_eq!(alpha(&buf, 60, 0, 0), 0, "前景层不得露出圆角外");
    assert_eq!(rgb(&buf, 60, 30, 20), [255, 0, 0]);
}

/// z<0 覆盖图同样不得露出圆角外：圆角备份要取在它之前。
#[test]
fn clip_corners_also_trims_back_layers() {
    let mut root = View::container(Layout::Column)
        .bg([255, 255, 255, 255])
        .radius(12.0)
        .clip_to_corners();
    root.layers.push(ViewLayer {
        path: png_uri(RED, 4, 4),
        z: -1,
        place: ImagePlace {
            w: 60.0,
            h: 40.0,
            ..place("top-left")
        },
        tint: [0, 0, 0, 0],
        opacity: 1.0,
        flip: wind_theme::Flip::NONE,
    });
    let buf = paint(root.fixed_w(60.0).fixed_h(40.0), 60.0, 40.0, 60, 40);
    assert_eq!(alpha(&buf, 60, 59, 39), 0, "底层图不得露出圆角外");
    assert_eq!(rgb(&buf, 60, 30, 20), [255, 0, 0]);
}

/// 候选窗最右侧钉一张小图。anchor=right + 向左 4px，落点之外不得被涂。
#[test]
fn anchored_bg_image_lands_at_anchor_plus_offset() {
    let p = ImagePlace {
        off_x: -4.0,
        w: 10.0,
        h: 10.0,
        ..place("right")
    };
    let buf = paint(
        node().bg_image(image(png_uri(RED, 10, 10), Some(p))),
        100.0,
        40.0,
        100,
        40,
    );
    // 期望 x∈[86,96)、y∈[15,25)。
    assert_eq!(
        rgb(&buf, 100, 86, 15),
        [255, 0, 0],
        "左上角没落在锚点+偏移处"
    );
    assert_eq!(
        rgb(&buf, 100, 95, 24),
        [255, 0, 0],
        "右下角没落在锚点+偏移处"
    );
    assert_eq!(rgb(&buf, 100, 5, 20), WHITE, "背景图仍铺满了整个节点");
    assert_eq!(rgb(&buf, 100, 85, 20), WHITE, "图左侧被涂");
    assert_eq!(rgb(&buf, 100, 97, 20), WHITE, "偏移没生效：右缘留白被涂");
    assert_eq!(rgb(&buf, 100, 90, 13), WHITE, "图上方被涂");
    assert_eq!(rgb(&buf, 100, 90, 26), WHITE, "图下方被涂");
}

/// 百分比偏移相对节点宽高：50% × 100 = 50，25% × 40 = 10。
#[test]
fn bg_image_pct_offset_is_relative_to_node() {
    let p = ImagePlace {
        off_x_pct: 50.0,
        off_y_pct: 25.0,
        w: 10.0,
        h: 10.0,
        ..place("top-left")
    };
    let buf = paint(
        node().bg_image(image(png_uri(RED, 10, 10), Some(p))),
        100.0,
        40.0,
        100,
        40,
    );
    assert_eq!(rgb(&buf, 100, 50, 10), [255, 0, 0]);
    assert_eq!(rgb(&buf, 100, 59, 19), [255, 0, 0]);
    assert_eq!(rgb(&buf, 100, 49, 15), WHITE);
    assert_eq!(rgb(&buf, 100, 55, 9), WHITE);
    assert_eq!(rgb(&buf, 100, 60, 15), WHITE);
}

/// 不给 size = 原图像素尺寸（与覆盖图一致）。
#[test]
fn bg_image_without_size_uses_source_pixels() {
    let buf = paint(
        node().bg_image(image(png_uri(RED, 6, 4), Some(place("bottom-right")))),
        40.0,
        20.0,
        40,
        20,
    );
    assert_eq!(rgb(&buf, 40, 34, 16), [255, 0, 0]);
    assert_eq!(rgb(&buf, 40, 39, 19), [255, 0, 0]);
    assert_eq!(rgb(&buf, 40, 33, 18), WHITE);
    assert_eq!(rgb(&buf, 40, 36, 15), WHITE);
}

/// 尺寸规则的纯几何：只给一维 → 另一维按原图宽高比推算；两维都不给 → 原图 × DPI。
/// 两维都给时按给的来，不看原图（`src` 不该被调用——位图白解码一次不便宜）。
#[test]
fn rect_size_rules() {
    let host = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let size = |p: ImagePlace, src: (u32, u32)| {
        let (_, _, w, h) = p.rect(host, || Some(src)).expect("有原图尺寸就该出矩形");
        (w, h)
    };
    let tl = place("top-left");

    // 只给 w：h = w × src_h / src_w。
    let only_w = ImagePlace {
        w: 20.0,
        ..tl.clone()
    };
    assert_eq!(
        size(only_w, (10, 6)),
        (20.0, 12.0),
        "只给 w 时 h 没按原图比例"
    );
    // 只给 h：w = h × src_w / src_h。
    let only_h = ImagePlace {
        h: 12.0,
        ..tl.clone()
    };
    assert_eq!(
        size(only_h, (10, 6)),
        (20.0, 12.0),
        "只给 h 时 w 没按原图比例"
    );
    // 都不给：原图 × scale。
    let none = ImagePlace {
        scale: 2.0,
        ..tl.clone()
    };
    assert_eq!(size(none, (6, 4)), (12.0, 8.0), "缺省尺寸没乘 DPI");
    // 都给：不问原图。
    let both = ImagePlace {
        w: 7.0,
        h: 9.0,
        scale: 2.0,
        ..tl.clone()
    };
    let (_, _, w, h) = both
        .rect(host, || panic!("两维都给了还去取原图尺寸"))
        .unwrap();
    assert_eq!((w, h), (7.0, 9.0));
    // 只给一维时 scale 不再叠乘（给定维已 ×scale，比例无量纲）。
    let only_w_hidpi = ImagePlace {
        w: 40.0,
        scale: 2.0,
        ..tl
    };
    assert_eq!(size(only_w_hidpi, (10, 6)), (40.0, 24.0));
}

/// 推算 / 缺省尺寸有上限（[`MAX_PLACED_EDGE`]）：市场主题给个 1×100000 的图再写 w=20，
/// 按比例推出 h=2,000,000，填充位图就是几 GB。双向钉：超了必须截，没超一点不能动。
#[test]
fn rect_size_is_capped() {
    let host = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let size = |p: ImagePlace, src: (u32, u32)| {
        let (_, _, w, h) = p.rect(host, || Some(src)).unwrap();
        (w, h)
    };
    let tl = place("top-left");
    let only_w = ImagePlace {
        w: 20.0,
        ..tl.clone()
    };
    assert_eq!(size(only_w.clone(), (1, 100_000)), (20.0, MAX_PLACED_EDGE));
    let hidpi = ImagePlace {
        scale: 2.0,
        ..tl.clone()
    };
    assert_eq!(size(hidpi, (100_000, 1)), (MAX_PLACED_EDGE, 2.0));
    let both = ImagePlace {
        w: 99_999.0,
        h: 3.0,
        ..tl
    };
    assert_eq!(size(both, (1, 1)), (MAX_PLACED_EDGE, 3.0));
    // 反向：贴着上限以下的合法大图原样保留。
    assert_eq!(size(only_w, (1, 204)), (20.0, 4080.0));
}

/// 定位背景图（place=Some）+ SVG center：单元（文档尺寸）居中于定位块内，块内其余透明。
#[test]
fn positioned_svg_bg_image_centers_unit_in_its_block() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="#FF0000"/></svg>"##;
    let uri = format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    );
    let p = ImagePlace {
        off_x: 2.0,
        off_y: 2.0,
        w: 12.0,
        h: 12.0,
        ..place("top-left")
    };
    let img = ViewImage {
        mode: "center".to_string(),
        ..image(uri, Some(p))
    };
    let buf = paint(node().bg_image(img), 20.0, 20.0, 20, 20);
    // 定位块 [2,14)，单元 4×4 居中 → [6,10)。
    assert_eq!(rgb(&buf, 20, 6, 6), [255, 0, 0]);
    assert_eq!(rgb(&buf, 20, 9, 9), [255, 0, 0]);
    assert_eq!(rgb(&buf, 20, 5, 7), WHITE, "单元被拉伸到整块");
    assert_eq!(rgb(&buf, 20, 10, 7), WHITE, "单元被拉伸到整块");
    assert_eq!(rgb(&buf, 20, 7, 2), WHITE);
}

/// SVG 与位图同口径：原图尺寸取 SVG 文档尺寸，只给一维按文档宽高比推算。
#[test]
fn svg_layer_single_axis_keeps_document_aspect() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="6"><rect width="10" height="6" fill="#FF0000"/></svg>"##;
    let uri = format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    );
    let p = ImagePlace {
        w: 20.0,
        ..place("top-left")
    };
    let buf = paint(node().layers(vec![layer(uri, 1, p)]), 40.0, 20.0, 40, 20);
    assert_eq!(
        rgb(&buf, 40, 19, 11),
        [255, 0, 0],
        "SVG 高没按文档比例推到 12"
    );
    assert_eq!(rgb(&buf, 40, 5, 12), WHITE, "SVG 高超出 12");
    assert_eq!(rgb(&buf, 40, 20, 3), WHITE, "SVG 宽超出 size.w");
}

/// 只给一维 size：另一维按原图宽高比推算（背景图与覆盖图同此口径）。
#[test]
fn single_axis_size_keeps_source_aspect() {
    let p = ImagePlace {
        w: 20.0,
        ..place("top-left")
    };
    let bg = paint(
        node().bg_image(image(png_uri(RED, 10, 6), Some(p.clone()))),
        40.0,
        20.0,
        40,
        20,
    );
    let layer = paint(
        node().layers(vec![ViewLayer {
            path: png_uri(RED, 10, 6),
            z: 1,
            place: p,
            opacity: 1.0,
            tint: [0, 0, 0, 0],
            flip: wind_theme::Flip::NONE,
        }]),
        40.0,
        20.0,
        40,
        20,
    );
    for (name, buf) in [("背景图", &bg), ("覆盖图", &layer)] {
        assert_eq!(rgb(buf, 40, 19, 5), [255, 0, 0], "{name}：宽没取 size.w");
        assert_eq!(rgb(buf, 40, 20, 3), WHITE, "{name}：宽超出 size.w");
        // 原图 10×6、w=20 → h=12。
        assert_eq!(
            rgb(buf, 40, 5, 11),
            [255, 0, 0],
            "{name}：高没按比例推到 12"
        );
        assert_eq!(rgb(buf, 40, 5, 12), WHITE, "{name}：高超出 12");
    }
}

/// 定位图超出节点的部分裁掉：圆角外、节点右缘外都不能有图。
#[test]
fn positioned_bg_image_is_clipped_to_rounded_node() {
    // 节点 40×40（圆角 12）放在 60×40 缓冲里；图 16×16 贴右上角再向右挪 6px，一半出界。
    let p = ImagePlace {
        off_x: 6.0,
        ..place("top-right")
    };
    let buf = paint(
        node()
            .radius(12.0)
            .bg_image(image(png_uri(RED, 16, 16), Some(p))),
        40.0,
        40.0,
        60,
        40,
    );
    assert_eq!(rgb(&buf, 60, 32, 8), [255, 0, 0], "盒内那半没画");
    assert_eq!(alpha(&buf, 60, 42, 8), 0, "节点右缘外被涂");
    assert_eq!(alpha(&buf, 60, 39, 0), 0, "圆角外被涂");
}

fn layer(path: String, z: i32, place: ImagePlace) -> ViewLayer {
    ViewLayer {
        path,
        z,
        place,
        opacity: 1.0,
        tint: [0, 0, 0, 0],
        flip: wind_theme::Flip::NONE,
    }
}

/// 图层 tint：语义同背景图（把图当 alpha 蒙版、RGB 换成 tint 色），`paint_layer` 不得丢掉它。
#[test]
fn layer_tint_recolors_like_bg_image() {
    let p = ImagePlace {
        w: 10.0,
        h: 10.0,
        ..place("top-left")
    };
    let tinted = ViewLayer {
        tint: [0, 0, 255, 255],
        ..layer(png_uri(RED, 10, 10), 1, p.clone())
    };
    let buf = paint(node().layers(vec![tinted]), 20.0, 20.0, 20, 20);
    assert_eq!(rgb(&buf, 20, 5, 5), [0, 0, 255], "图层 tint 没生效");
    assert_eq!(rgb(&buf, 20, 15, 15), WHITE, "图层外被涂");
    // 反向对照：未配 tint 原样出图。
    let plain = paint(
        node().layers(vec![layer(png_uri(RED, 10, 10), 1, p)]),
        20.0,
        20.0,
        20,
        20,
    );
    assert_eq!(rgb(&plain, 20, 5, 5), [255, 0, 0]);
}

/// 覆盖图超出节点的部分裁掉，裁到节点矩形（不带圆角）——对齐编辑器预览
/// `paintLayers` 的 `ctx.rect` 硬裁；内容上下两层同一口径。
#[test]
fn layer_overflow_is_clipped_to_node_rect() {
    for z in [-1, 1] {
        // 节点 40×40（圆角 12）放在 60×40 缓冲里；图 16×16 贴右上角再向右挪 6px，一半出界。
        let p = ImagePlace {
            off_x: 6.0,
            ..place("top-right")
        };
        let buf = paint(
            View::container(Layout::Row).radius(12.0).layers(vec![layer(
                png_uri(RED, 16, 16),
                z,
                p,
            )]),
            40.0,
            40.0,
            60,
            40,
        );
        assert_eq!(rgb(&buf, 60, 32, 8), [255, 0, 0], "z={z}：盒内那半没画");
        assert_eq!(alpha(&buf, 60, 40, 8), 0, "z={z}：节点右缘外被涂");
        assert_eq!(alpha(&buf, 60, 45, 8), 0, "z={z}：节点右缘外被涂");
        assert_eq!(
            rgb(&buf, 60, 39, 0),
            [255, 0, 0],
            "z={z}：覆盖图只裁矩形，圆角处照画（编辑器口径）"
        );
    }
}

/// 完全落在节点内的覆盖图不受裁剪影响：同一位置、节点大小不同，画出来逐像素一致。
#[test]
fn layer_inside_node_is_not_trimmed() {
    let p = ImagePlace {
        off_x: 5.0,
        off_y: 5.0,
        ..place("top-left")
    };
    let at = |n: f32| {
        paint(
            View::container(Layout::Row).layers(vec![layer(two_tone_uri(), 1, p.clone())]),
            n,
            n,
            60,
            60,
        )
    };
    let small = at(13.0);
    assert_eq!(rgb(&small, 60, 5, 5), [255, 0, 0]);
    assert_eq!(
        rgb(&small, 60, 12, 8),
        [0, 0, 255],
        "贴着节点边的那列被裁了"
    );
    assert!(small == at(60.0), "节点内的覆盖图被裁剪改了像素");
}

/// 不配定位 = 铺满节点。对拍：铺满结果与「左上锚点、尺寸恰为节点」的定位结果逐像素一致，
/// 且圆角外保持透明。
#[test]
fn unpositioned_bg_image_fills_node_as_before() {
    let (w, h) = (30usize, 12usize);
    let full = paint(
        node().radius(4.0).bg_image(image(two_tone_uri(), None)),
        w as f32,
        h as f32,
        w,
        h,
    );
    let exact = ImagePlace {
        w: w as f32,
        h: h as f32,
        ..place("top-left")
    };
    let placed = paint(
        node()
            .radius(4.0)
            .bg_image(image(two_tone_uri(), Some(exact))),
        w as f32,
        h as f32,
        w,
        h,
    );
    assert_eq!(rgb(&full, w, 2, 6), [255, 0, 0]);
    assert_eq!(rgb(&full, w, 27, 6), [0, 0, 255], "没有拉伸铺满");
    assert_eq!(alpha(&full, w, 0, 0), 0);
    assert!(full == placed, "铺满与等尺寸定位不一致");
}

/// flip 接到背景图（铺满 / 定位）与图层的绘制：左红右蓝翻成左蓝右红，摆放位置不变。
#[test]
fn flip_reaches_bg_image_and_layer_paint() {
    let x = wind_theme::Flip { x: true, y: false };
    let (w, h) = (30usize, 12usize);
    let flipped = |place: Option<ImagePlace>| ViewImage {
        flip: x,
        ..image(two_tone_uri(), place)
    };
    let full = paint(node().bg_image(flipped(None)), w as f32, h as f32, w, h);
    assert_eq!(rgb(&full, w, 2, 6), [0, 0, 255], "铺满背景图没翻");
    assert_eq!(rgb(&full, w, 27, 6), [255, 0, 0]);
    let p = ImagePlace {
        w: 10.0,
        h: 4.0,
        off_x: 5.0,
        ..place("top-left")
    };
    let placed = paint(
        node().bg_image(flipped(Some(p.clone()))),
        w as f32,
        h as f32,
        w,
        h,
    );
    assert_eq!(rgb(&placed, w, 5, 1), [0, 0, 255], "定位背景图没翻");
    assert_eq!(rgb(&placed, w, 14, 1), [255, 0, 0]);
    assert_eq!(rgb(&placed, w, 4, 1), WHITE, "只翻像素，摆放位置不动");
    assert_eq!(rgb(&placed, w, 15, 1), WHITE);
    let lay = ViewLayer {
        flip: x,
        ..layer(two_tone_uri(), 1, p)
    };
    let buf = paint(node().layers(vec![lay]), w as f32, h as f32, w, h);
    assert_eq!(rgb(&buf, w, 5, 1), [0, 0, 255], "图层没翻");
    assert_eq!(rgb(&buf, w, 14, 1), [255, 0, 0]);
    assert_eq!(rgb(&buf, w, 4, 1), WHITE);
}
