use super::*;
use crate::canvas::mock::MockCanvas;

fn preview() -> (Preview<MockCanvas>, MockCanvas) {
    let c = MockCanvas::default();
    (Preview::new(c.clone()), c)
}

fn opaque_px(f: &Frame) -> usize {
    f.rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] == 255)
        .count()
}

fn count_px(f: &Frame, rgba: [u8; 4]) -> usize {
    f.rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| **p == rgba)
        .count()
}

/// 编码成 PNG 的纯色图。
fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    ::image::RgbaImage::from_pixel(w, h, ::image::Rgba(rgba))
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            ::image::ImageFormat::Png,
        )
        .unwrap();
    out
}

/// 某段文字被画时用的 CSS 字体串。
fn font_of(c: &MockCanvas, text: &str) -> String {
    let draws = c.draws.borrow();
    draws
        .iter()
        .find(|(t, _, _)| t == text)
        .unwrap_or_else(|| panic!("没画 {text}：{draws:?}"))
        .1
        .clone()
}

#[test]
fn default_theme_renders_sample_candidates() {
    let (mut p, c) = preview();
    let f = p.render(&RenderOpts::default()).unwrap();
    assert_eq!(f.rgba.len(), (f.width * f.height * 4) as usize);
    assert!(f.width > f.height, "横排是宽条：{}x{}", f.width, f.height);
    assert!(opaque_px(&f) > 0, "窗口底色应画出");
    for want in ["你好", "拟好", "wqiy", "ni'hao"] {
        font_of(&c, want);
    }
    assert!(
        !c.draws.borrow().iter().any(|(t, _, _)| t == "匿"),
        "出厂 7 条，不画第 10 条"
    );
    assert_eq!(f.next_ms, None, "无皮肤无动画");
}

#[test]
fn candidates_take_comment_above_and_pinyin_hint() {
    let (mut p, c) = preview();
    let o: RenderOpts = serde_json::from_str(
        r#"{"candidates":[{"text":"中(zhong)文","hint":[3,10],"comment_above":"zhōng wén"},{"text":"众"}]}"#,
    )
    .unwrap();
    let f = p.render(&o).unwrap();
    font_of(&c, "zhōng wén");
    assert!(f.nodes.iter().any(|n| n.key == "comment_above"));
    let bad: RenderOpts =
        serde_json::from_str(r#"{"candidates":[{"text":"中文","hint":[1,3]}]}"#).unwrap();
    let e = p.render(&bad).err().unwrap();
    assert!(e.contains("字符边界"), "{e}");
}

#[test]
fn frame_carries_node_rects_in_device_pixels() {
    let (mut p, _) = preview();
    let one = p.render(&RenderOpts::default()).unwrap();
    let two = p
        .render(&RenderOpts {
            scale: 2.0,
            ..Default::default()
        })
        .unwrap();
    let keys: Vec<&str> = one.nodes.iter().map(|n| n.key).collect();
    for want in [
        "window",
        "preedit_bar",
        "candidate_list",
        "item",
        "index",
        "footer_bar",
    ] {
        assert!(keys.contains(&want), "缺 {want}：{keys:?}");
    }
    assert_eq!(keys.iter().filter(|k| **k == "item").count(), 7);
    let item = |f: &Frame| f.nodes.iter().find(|n| n.key == "item").unwrap().clone();
    let (a, b) = (item(&one), item(&two));
    assert!(
        (b.w - a.w * 2.0).abs() <= 2.0,
        "设备像素随 scale：{a:?} / {b:?}"
    );
    for n in &two.nodes {
        assert!(n.x >= 0.0 && n.y >= 0.0, "{n:?}");
        assert!(n.x + n.w <= two.width as f32 + 0.5, "{n:?}");
        assert!(n.y + n.h <= two.height as f32 + 0.5, "{n:?}");
    }
    let json = serde_json::to_string(&one.nodes[0]).unwrap();
    assert!(json.starts_with(r#"{"key":"window","x":"#), "{json}");
}

#[test]
fn opts_parse_from_partial_json_and_change_layout() {
    let (mut p, c) = preview();
    let o: RenderOpts = serde_json::from_str(
        r#"{"vertical":true,"scale":2,"font_size":15,"candidates":[{"text":"甲"},{"text":"乙","comment":"注","label":"b"}],"preedit":"jy"}"#,
    )
    .unwrap();
    let v = p.render(&o).unwrap();
    assert!(v.height > v.width, "竖排是高条：{}x{}", v.width, v.height);
    for want in ["乙", "注", "b", "jy"] {
        font_of(&c, want);
    }
    // 候选正文按生效字号 × 缩放画。
    assert!(
        font_of(&c, "甲").contains(" 30px "),
        "{}",
        font_of(&c, "甲")
    );
}

#[test]
fn dark_and_shadow_change_pixels() {
    let (mut p, _) = preview();
    let light = p.render(&RenderOpts::default()).unwrap();
    let dark = p
        .render(&RenderOpts {
            dark: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!((light.width, light.height), (dark.width, dark.height));
    assert_ne!(light.rgba, dark.rgba, "明暗换色");
    let bare = p
        .render(&RenderOpts {
            soft_shadow: false,
            ..Default::default()
        })
        .unwrap();
    assert!(bare.width < light.width && bare.height < light.height);
}

#[test]
fn user_font_family_reaches_css_font() {
    let (mut p, c) = preview();
    p.render(&RenderOpts {
        font_family: "Noto Serif SC".into(),
        font_weight: 700,
        ..Default::default()
    })
    .unwrap();
    let f = font_of(&c, "你好");
    assert!(f.contains("\"Noto Serif SC\""), "{f}");
    assert!(f.starts_with("700 "), "{f}");
}

#[test]
fn set_theme_errors_verbatim_and_keeps_current_theme() {
    let (mut p, _) = preview();
    let before = p.render(&RenderOpts::default()).unwrap().rgba;
    let e = p.set_theme("[meta\nname = 1", "", Vec::new()).unwrap_err();
    assert!(e.contains("parse theme"), "{e}");
    let e = p
        .set_theme("base = \"nope\"\n", "", Vec::new())
        .unwrap_err();
    assert!(e.contains("\"nope\"") && e.contains("只认内置主题"), "{e}");
    assert_eq!(p.render(&RenderOpts::default()).unwrap().rgba, before);
    // 编辑的就是 default 本身：base 指向自己视同没写。
    p.set_theme(
        "base = \"mine\"\n[meta]\nname = \"m\"\n",
        "mine",
        Vec::new(),
    )
    .unwrap();
}

/// 参数上限：每一条都报错、不 panic、不卡死，且实例之后照常可用。
#[test]
fn oversized_params_are_rejected() {
    let (mut p, _) = preview();
    let d = RenderOpts::default;
    let many = vec![
        CandidateOpt {
            text: "字".into(),
            ..Default::default()
        };
        MAX_CANDIDATES + 1
    ];
    let long = CandidateOpt {
        text: "字".repeat(MAX_TEXT_CHARS + 1),
        ..Default::default()
    };
    let cases = [
        RenderOpts { scale: 0.0, ..d() },
        RenderOpts { scale: 9.0, ..d() },
        RenderOpts {
            scale: f32::NAN,
            ..d()
        },
        RenderOpts {
            font_size: 100_000.0,
            ..d()
        },
        RenderOpts {
            font_size: f32::INFINITY,
            ..d()
        },
        RenderOpts {
            font_size: -1.0,
            ..d()
        },
        RenderOpts {
            candidates: many,
            ..d()
        },
        RenderOpts {
            candidates: vec![long],
            ..d()
        },
        RenderOpts {
            preedit: Some("a".repeat(MAX_TEXT_CHARS + 1)),
            ..d()
        },
        RenderOpts {
            total_pages: MAX_PAGES + 1,
            ..d()
        },
        RenderOpts {
            selected: 1 << 40,
            ..d()
        },
    ];
    for (i, o) in cases.iter().enumerate() {
        assert!(p.render(o).is_err(), "第 {i} 条应报错");
    }
    // 字号在上限内、缩放顶格：帧超限，在分配缓冲之前报错。
    let e = p
        .render(&RenderOpts {
            font_size: MAX_FONT_SIZE,
            scale: MAX_SCALE,
            ..d()
        })
        .err()
        .expect("帧超限");
    assert!(e.contains("超出上限"), "{e}");
    // 时刻钳住而不是 panic。
    for t in [1e300, f64::INFINITY, f64::NAN, -5.0] {
        p.render(&RenderOpts {
            t_ms: t,
            animate: true,
            ..d()
        })
        .unwrap();
    }
    assert!(p.render(&d()).is_ok(), "报错之后照常可用");
}

/// 长注释在高缩放下像实机一样被钳宽，能出图，不报「超出上限」。
#[test]
fn long_comment_is_clamped_in_width_not_rejected() {
    let (mut p, _) = preview();
    let f = p
        .render(&RenderOpts {
            scale: 2.0,
            candidates: vec![CandidateOpt {
                text: "长".into(),
                comment: "注".repeat(MAX_TEXT_CHARS),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    assert!(f.width <= MAX_SIDE, "{}", f.width);
    assert!(f.clamped, "过了预览屏宽");
}

/// 预览屏按逻辑像素定：换缩放不改截断（宽度随缩放等比），放大到撞尺寸闸报 FRAME_TOO_LARGE。
#[test]
fn screen_is_logical_so_zoom_keeps_content() {
    let (mut p, _) = preview();
    let o = |scale: f32| RenderOpts {
        scale,
        candidates: vec![CandidateOpt {
            text: "长".into(),
            comment: "注".repeat(150),
            ..Default::default()
        }],
        ..Default::default()
    };
    let one = p.render(&o(1.0)).unwrap();
    let two = p.render(&o(2.0)).unwrap();
    assert!(!one.clamped && !two.clamped);
    // 等比（差的是各处取整）。
    assert!(
        (two.width as f32 - one.width as f32 * 2.0).abs() <= one.width as f32 * 0.02,
        "{} vs {}",
        one.width,
        two.width
    );
    let e = p.render(&o(4.0)).err().expect("4 倍撞 4096");
    assert!(e.starts_with(FRAME_TOO_LARGE), "{e}");
    assert!(p.render(&o(1.0)).is_ok());
}

/// 主题里的字号走同一道闸。
#[test]
fn theme_font_size_is_bounded_too() {
    let (mut p, _) = preview();
    p.set_theme(
        "base = \"default\"\n[behavior]\nfont_size = 100000\n",
        "",
        Vec::new(),
    )
    .unwrap();
    let e = p
        .render(&RenderOpts::default())
        .err()
        .expect("主题字号超限");
    assert!(e.contains("生效字号"), "{e}");
    assert!(
        p.render(&RenderOpts {
            font_size: 16.0,
            ..Default::default()
        })
        .is_ok(),
        "用户字号覆盖后可画"
    );
}

/// 宿主（JS）调用失败：这一帧报错，实例不报废，恢复后照常画。
#[test]
fn host_failure_fails_the_frame_only() {
    let (mut p, c) = preview();
    let good = p.render(&RenderOpts::default()).unwrap().rgba;
    c.fail.set(true);
    let e = p
        .render(&RenderOpts {
            font_family: "X".into(), // 换字族清测量缓存，迫使再问宿主
            ..Default::default()
        })
        .err()
        .expect("宿主失败");
    assert!(e.contains("mock"), "{e}");
    c.fail.set(false);
    assert_eq!(p.render(&RenderOpts::default()).unwrap().rgba, good);
}

/// 继承内置主题、背景图来自调用方的资源表：底图的颜色出现在帧里。
#[test]
fn theme_with_base_and_memory_asset() {
    let (mut p, _) = preview();
    p.set_theme(
        &bg_theme("bg.png"),
        "",
        vec![("bg.png".into(), png(4, 4, [0, 200, 0, 255]))],
    )
    .unwrap();
    let f = p.render(&RenderOpts::default()).unwrap();
    assert!(count_px(&f, [0, 200, 0, 255]) > 100, "底图应铺开");
}

fn bg_theme(r: &str) -> String {
    format!(
        "base = \"default\"\n[meta]\nname = \"t\"\n\
             [window]\nbackground = {{ image = {{ ref = \"{r}\", mode = \"stretch\" }} }}\n"
    )
}

/// 同线程两个实例：各持资源表与缓存，一方换主题 / 资源不让另一方的图失效。
#[test]
fn two_previews_do_not_clobber_each_other() {
    let (mut a, _) = preview();
    let (mut b, _) = preview();
    let green = [0, 200, 0, 255];
    let blue = [0, 0, 200, 255];
    a.set_theme(
        &bg_theme("bg.png"),
        "",
        vec![("bg.png".into(), png(4, 4, green))],
    )
    .unwrap();
    let fa = a.render(&RenderOpts::default()).unwrap();
    b.set_theme(
        &bg_theme("bg.png"),
        "",
        vec![("bg.png".into(), png(4, 4, blue))],
    )
    .unwrap();
    let fb = b.render(&RenderOpts::default()).unwrap();
    assert!(count_px(&fb, blue) > 100);
    assert_eq!(
        a.render(&RenderOpts::default()).unwrap().rgba,
        fa.rgba,
        "A 不受 B 影响"
    );
    b.set_theme(&bg_theme("bg.png"), "", Vec::new()).unwrap();
    assert_eq!(
        a.render(&RenderOpts::default()).unwrap().rgba,
        fa.rgba,
        "B 换资源也不影响 A"
    );
}

/// 资源没变时沿用代号与缓存（只改主题文字不重新解码）；变了换代号。
#[test]
fn unchanged_assets_keep_generation() {
    let (mut p, _) = preview();
    let assets = || vec![("bg.png".into(), png(4, 4, [0, 200, 0, 255]))];
    p.set_theme(&bg_theme("bg.png"), "", assets()).unwrap();
    let g = p.assets_gen;
    p.set_theme(
        &format!("{}\n[behavior]\nfont_size = 18\n", bg_theme("bg.png")),
        "",
        assets(),
    )
    .unwrap();
    assert_eq!(p.assets_gen, g, "资源没变沿用代号");
    p.set_theme(
        &bg_theme("bg.png"),
        "",
        vec![("bg.png".into(), png(4, 4, [1, 2, 3, 255]))],
    )
    .unwrap();
    assert_ne!(p.assets_gen, g, "内容变了换代号");
}

/// 两帧 GIF 当装饰：`idle` 停第 0 帧（红）、`typing` 转 `busy`（第 1 帧，蓝）。
fn skin_theme(anim: &str) -> (String, Vec<(String, Vec<u8>)>) {
    let toml = format!(
        "[meta]\nname = \"s\"\n[skin]\ndensity = 1\n\
             [skin.horizontal.frame]\nimage = \"frame.png\"\n\
             stretch_x = [[4, 12]]\nstretch_y = [[4, 12]]\n\
             [[skin.horizontal.decor]]\nimage = \"dot.gif\"\nanchor = \"frame.top_left\"\n{anim}\n"
    );
    let assets = vec![
        ("frame.png".into(), png(16, 16, [200, 200, 200, 255])),
        ("dot.gif".into(), gif2([255, 0, 0, 255], [0, 0, 255, 255])),
    ];
    (toml, assets)
}

fn at(p: &mut Preview<MockCanvas>, t: f64) -> Frame {
    p.render(&RenderOpts {
        animate: true,
        t_ms: t,
        ..Default::default()
    })
    .unwrap()
}

/// 皮肤动画：装饰按 `t_ms` 换帧，`next_ms` 报下一次换帧；不播动画停在首帧。
#[test]
fn skin_animation_follows_t_ms() {
    let (mut p, _) = preview();
    let (toml, assets) = skin_theme("");
    p.set_theme(&toml, "", assets).unwrap();
    let f0 = at(&mut p, 0.0);
    assert_eq!(f0.next_ms, Some(100.0), "两帧各 100ms");
    let f1 = at(&mut p, 150.0);
    assert_ne!(f0.rgba, f1.rgba, "第二帧换了颜色");
    let f2 = at(&mut p, 250.0);
    assert_eq!(f0.rgba, f2.rgba, "循环回第一帧");
    let still = p.render(&RenderOpts::default()).unwrap();
    assert_eq!(still.next_ms, None);
}

/// `signal` 真的驱动状态机：`typing` 之后装饰换到 `busy` 的帧。
#[test]
fn signal_drives_decor_state_machine() {
    let (mut p, _) = preview();
    let (toml, assets) = skin_theme(
        "states = { idle = { frames = \"0\" }, busy = { frames = \"1\" } }\n\
             on = { typing = { state = \"busy\" } }",
    );
    p.set_theme(&toml, "", assets).unwrap();
    let red = [255, 0, 0, 255];
    let blue = [0, 0, 255, 255];
    let f0 = at(&mut p, 0.0);
    assert!(
        count_px(&f0, red) > 0 && count_px(&f0, blue) == 0,
        "初始 idle = 红"
    );
    assert_eq!(at(&mut p, 500.0).rgba, f0.rgba, "没事件就停在 idle");
    assert!(p.signal("typing", Some(600.0)).unwrap(), "typing 改了画面");
    let f1 = at(&mut p, 650.0);
    assert!(
        count_px(&f1, blue) > 0 && count_px(&f1, red) == 0,
        "typing 后 = 蓝"
    );
    // show 只让常驻状态从头播，不回 idle；换主题才回初始状态。
    assert!(p.signal("show", None).unwrap());
    assert_eq!(at(&mut p, 700.0).rgba, f1.rgba);
    let (toml, assets) = skin_theme(
        "states = { idle = { frames = \"0\" }, busy = { frames = \"1\" } }\n\
             on = { typing = { state = \"busy\" } }",
    );
    p.set_theme(&toml, "", assets).unwrap();
    assert_eq!(at(&mut p, 800.0).rgba, f0.rgba, "换主题回到 idle");
    assert!(p.signal("click:0", Some(10.0)).is_ok());
    assert!(p.signal("nonsense", None).is_err());
    assert!(p.signal("click:x", None).is_err());
}

/// 皮肤回执夹具：主坐标 density 2，窗体 32×32 两轴各一条带（y 轴 tile），四边内缩 6，
/// 一块伸出窗体右上角的装饰（z 120、block）；`free` 时再分槽位（候选锚内容框左上、翻页锚窗体右下）。
fn geom_theme(free: bool, extra: &str) -> (String, Vec<(String, Vec<u8>)>) {
    let slots = if free {
        "[skin.horizontal.slots.candidates]\nanchor = \"content.top_left\"\n\
             [skin.horizontal.slots.footer]\nanchor = \"frame.bottom_right\"\noffset = [-8, -4]\n"
    } else {
        ""
    };
    let toml = format!(
        "[meta]\nname = \"g\"\n[skin]\ndensity = 2\n\
             [skin.horizontal.frame]\nimage = \"assets/frame.png\"\n\
             stretch_x = [[8, 24]]\nstretch_y = [{{ from = 8, to = 24, fill = \"tile\" }}]\n\
             [skin.horizontal.content]\nleft = 6\ntop = 6\nright = 6\nbottom = 6\n\
             [skin.horizontal.placement]\ncaret = {{ anchor = \"content.top_left\", offset = [2, 4] }}\n\
             {slots}{extra}\
             [[skin.horizontal.decor]]\nimage = \"assets/dot.png\"\nanchor = \"frame.top_right\"\n\
             pivot = \"center\"\noffset = [-4, 0]\nz = 120\nhit = \"block\"\n"
    );
    let assets = vec![
        ("assets/frame.png".into(), png(32, 32, [200, 200, 200, 255])),
        ("assets/dot.png".into(), png(12, 12, [255, 0, 0, 255])),
    ];
    (toml, assets)
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> RectInfo {
    RectInfo { x, y, w, h }
}

fn rect_i((x, y, w, h): (i32, i32, u32, u32)) -> RectInfo {
    rect(x as f32, y as f32, w as f32, h as f32)
}

/// 回执的皮肤几何就是引擎这一帧求出的那份（stack / free × 1x / 2x），与 `nodes` 同坐标。
#[test]
fn skin_receipt_matches_engine_geometry() {
    for free in [false, true] {
        let (mut p, _) = preview();
        let (toml, assets) = geom_theme(free, "");
        let w = p.set_theme(&toml, "", assets).unwrap();
        assert!(w.is_empty(), "{w:?}");
        for scale in [1.0f32, 2.0] {
            let f = p
                .render(&RenderOpts {
                    scale,
                    ..Default::default()
                })
                .unwrap();
            let s = f.skin.clone().expect("启用了皮肤");
            let g = p.scene.skin_geom.as_ref().unwrap();
            let k = scale / 2.0;
            let ctx = format!("free={free} scale={scale}");
            assert_eq!(s.variant, "horizontal", "{ctx}");
            assert_eq!((s.k, s.render_scale), (k, scale), "{ctx}");
            assert_eq!(s.frame_source, "assets/frame.png");
            assert_eq!(s.min_size, [g.min_w, g.min_h], "{ctx}");
            let fr = rect_i(g.frame_for_test());
            assert_eq!(s.frame, fr, "{ctx}");
            let window = f.nodes.iter().find(|n| n.key == "window").unwrap();
            // 窗体是根容器盒向上取整。
            assert!(
                (window.x, window.y) == (fr.x, fr.y)
                    && (0.0..1.0).contains(&(fr.w - window.w))
                    && (0.0..1.0).contains(&(fr.h - window.h)),
                "窗体即根容器盒，与 nodes 同坐标（{ctx}）：{window:?} / {fr:?}"
            );
            let i = 6.0 * k;
            assert_eq!(
                s.content,
                rect(fr.x + i, fr.y + i, fr.w - 2.0 * i, fr.h - 2.0 * i),
                "{ctx}"
            );
            let rects: Vec<RectInfo> = g
                .decor_rects_for_test()
                .iter()
                .map(|&r| rect_i(r))
                .collect();
            assert_eq!(
                s.decor.iter().map(|d| d.rect).collect::<Vec<_>>(),
                rects,
                "{ctx}"
            );
            let d = &s.decor[0];
            assert!(
                d.rect.y < fr.y && d.rect.x + d.rect.w > fr.x + fr.w,
                "外伸：{d:?}"
            );
            assert_eq!((d.index, d.z, d.hit), (0, 120, "block"));
            assert_eq!(
                d.clip,
                ClipInfo {
                    state: None,
                    frame: 0,
                    raw: Some(0)
                }
            );
            // 切片（Pair 带缺省 round）：源像素按 frame.image 写，目标覆盖整个窗体。
            for (axis, origin, len, tiled) in [
                (&s.axis_x, fr.x, fr.w, "round"),
                (&s.axis_y, fr.y, fr.h, "tile"),
            ] {
                let kinds: Vec<&str> = axis.iter().map(|a| a.kind).collect();
                assert_eq!(kinds, ["fixed", tiled, "fixed"], "{ctx}");
                assert_eq!(axis[1].src, [8.0, 24.0], "{ctx}");
                assert_eq!(axis[0].dst[0], origin);
                assert_eq!(axis[2].dst[1], origin + len);
                assert_eq!(axis[0].dst[1] - axis[0].dst[0], 8.0 * scale / 2.0);
            }
            assert_eq!(
                s.caret.below,
                [fr.x + i + 2.0 * k, fr.y + i + 4.0 * k],
                "{ctx}"
            );
            assert_eq!(s.caret.above, [fr.x, fr.y + fr.h], "缺省对齐窗体左下");
            if free {
                let names: Vec<&str> = s.slots.iter().map(|x| x.name).collect();
                assert_eq!(names, ["candidates", "footer"], "{ctx}");
                assert_eq!(
                    s.slots.iter().map(|x| x.on).collect::<Vec<_>>(),
                    ["content", "frame"]
                );
                let sizes = g.parts_for_test();
                let pos = g.part_positions_for_test(fr.w, fr.h);
                for ((slot, (_, w, h)), (x, y)) in s.slots.iter().zip(sizes).zip(pos) {
                    assert_eq!(slot.rect, rect(fr.x + x, fr.y + y, w, h), "{ctx}");
                }
                let footer = s.slots[1].rect;
                assert_eq!(
                    (footer.x + footer.w, footer.y + footer.h),
                    (fr.x + fr.w - 8.0 * k, fr.y + fr.h - 4.0 * k),
                    "{ctx}"
                );
            } else {
                assert!(s.slots.is_empty());
            }
            let json = serde_json::to_string(&s).unwrap();
            for key in ["renderScale", "frameSource", "minSize", "axisX", "axisY"] {
                assert!(json.contains(&format!("\"{key}\":")), "{key}：{json}");
            }
            assert_eq!(json.contains("\"box\":"), free, "{json}");
        }
    }
}

/// 回执报实际所用的变体：嵌入没配回退非嵌入；配了用嵌入；竖排没配不启用皮肤。
#[test]
fn skin_receipt_reports_variant_after_fallback() {
    let (mut p, _) = preview();
    let (toml, assets) = geom_theme(false, "");
    p.set_theme(&toml, "", assets.clone()).unwrap();
    let embedded = RenderOpts {
        embedded: true,
        ..Default::default()
    };
    assert_eq!(
        p.render(&embedded).unwrap().skin.unwrap().variant,
        "horizontal"
    );
    let vertical = RenderOpts {
        vertical: true,
        ..Default::default()
    };
    assert!(
        p.render(&vertical).unwrap().skin.is_none(),
        "竖排不回退横排"
    );
    let own = format!("{toml}[skin.horizontal_embedded.frame]\nimage = \"assets/frame.png\"\n");
    p.set_theme(&own, "", assets).unwrap();
    assert_eq!(
        p.render(&embedded).unwrap().skin.unwrap().variant,
        "horizontal_embedded"
    );
    assert!(p.render(&RenderOpts::default()).unwrap().skin.is_some());
    p.set_theme(&bg_theme("bg.png"), "", Vec::new()).unwrap();
    assert!(
        p.render(&RenderOpts::default()).unwrap().skin.is_none(),
        "无皮肤"
    );
}

/// 引擎的 `skin:` 告警原文随回执带出：解析期的随 `set_theme`，出图期的随帧；非 `skin:` 的不进来。
#[test]
fn skin_warnings_come_back_verbatim() {
    let (mut p, _) = preview();
    // 窗体图密度 0.001：倍率 1000，32 px 的图要缩放成 32000 px，引擎整套回退普通窗口。
    // 窗口底图 9000×1 超出位图上限：引擎也告警，但不是 `skin:` 前缀。
    let toml = "[meta]\nname = \"w\"\n\
                    [window]\nbackground = { image = { ref = \"assets/wide.png\", mode = \"stretch\" } }\n\
                    [skin]\ndensity = 1\n\
                    [skin.horizontal.frame]\nimage = \"assets/frame.png\"\ndensity = 0.001\n\
                    stretch_x = [{ from = 8, to = 24, fill = \"zigzag\" }]\n";
    let assets = vec![
        ("assets/frame.png".into(), png(32, 32, [200, 200, 200, 255])),
        ("assets/wide.png".into(), png(9000, 1, [0, 0, 0, 255])),
    ];
    let w = p.set_theme(toml, "", assets).unwrap();
    assert!(
        w.len() == 1 && w[0].starts_with("skin: 未知拉伸带填法"),
        "解析期：{w:?}"
    );
    let f = p.render(&RenderOpts::default()).unwrap();
    assert!(f.skin.is_none(), "回退普通窗口");
    assert!(
        f.warnings.len() == 1
            && f.warnings[0].starts_with("skin: 窗体图按倍率 ")
            && f.warnings[0].ends_with(" 缩放后超出上限，回退普通窗口"),
        "只收 skin: 前缀：{:?}",
        f.warnings
    );
    // 同一场景按场景去重，下一帧不再报；换主题（新场景）后首帧再报。
    assert!(
        p.render(&RenderOpts::default())
            .unwrap()
            .warnings
            .is_empty()
    );
}

/// 装饰的片段与帧号：`idle` 第 0 帧；`typing` 后 `busy`（原始第 1 帧）。
#[test]
fn skin_receipt_reports_decor_clip() {
    let (mut p, _) = preview();
    let (toml, assets) = skin_theme(
        "states = { idle = { frames = \"0\" }, busy = { frames = \"1\" } }\n\
             on = { typing = { state = \"busy\" } }",
    );
    p.set_theme(&toml, "", assets).unwrap();
    let clip = |f: Frame| f.skin.unwrap().decor[0].clip.clone();
    assert_eq!(
        clip(at(&mut p, 0.0)),
        ClipInfo {
            state: Some("idle".into()),
            frame: 0,
            raw: Some(0)
        }
    );
    p.signal("typing", Some(10.0)).unwrap();
    assert_eq!(
        clip(at(&mut p, 20.0)),
        ClipInfo {
            state: Some("busy".into()),
            frame: 0,
            raw: Some(1)
        }
    );
}

/// 帧多、目标尺寸大的动图装饰：12 帧 × 800² 超 24 MiB 帧预算，引擎隔帧丢弃（stride 2）。
/// `idle` 取原始 4–5 帧。
fn dropping_theme(extra: &str) -> (String, Vec<(String, Vec<u8>)>) {
    let toml = format!(
        "[meta]\nname = \"d\"\n{extra}[skin]\ndensity = 1\n\
             [skin.horizontal.frame]\nimage = \"assets/frame.png\"\n\
             [[skin.horizontal.decor]]\nimage = \"assets/anim.gif\"\nsize = [800, 800]\n\
             states = {{ idle = {{ frames = \"4-5\" }} }}\n"
    );
    let frames: Vec<([u8; 4], u32)> = (0..12).map(|i| ([i * 20, 0, 0, 255], 100)).collect();
    let assets = vec![
        ("assets/frame.png".into(), png(16, 16, [200, 200, 200, 255])),
        (
            "assets/anim.gif".into(),
            wind_scene::image_cache::anim_fixtures::gif(4, 4, &frames),
        ),
    ];
    (toml, assets)
}

fn is_drop(w: &str) -> bool {
    w.starts_with("skin: 动画帧超出 ") && w.contains("隔帧丢弃到 6 帧")
}

/// 资源层告警（降帧）只在建缓存时报：资源没变、只改 TOML 再 `set_theme`，首帧由命中的缓存重放；
/// 改小装饰尺寸（换了缓存键、不再降帧）后不再报。降帧后 `raw` = 保留帧 × 步长。
#[test]
fn resource_warnings_survive_toml_only_set_theme() {
    let (mut p, _) = preview();
    let (toml, assets) = dropping_theme("");
    assert!(p.set_theme(&toml, "", assets.clone()).unwrap().is_empty());
    let f = at(&mut p, 0.0);
    assert!(f.warnings.iter().any(|w| is_drop(w)), "{:?}", f.warnings);
    let clip = &f.skin.as_ref().unwrap().decor[0].clip;
    assert_eq!(
        (clip.state.as_deref(), clip.raw),
        (Some("idle"), Some(4)),
        "原始 4–5 帧隔帧丢弃后落在保留的第 2 帧：2 × 2 = 4"
    );
    assert!(at(&mut p, 10.0).warnings.is_empty(), "同场景不重复报");
    // 只改 TOML、资源不变：缓存沿用、引擎不会再报，首帧由命中的缓存条目重放。
    let (toml2, _) = dropping_theme("[behavior]\nfont_size = 18\n");
    assert!(p.set_theme(&toml2, "", assets.clone()).unwrap().is_empty());
    let f = at(&mut p, 20.0);
    assert!(
        f.warnings.iter().any(|w| is_drop(w)),
        "首帧重放：{:?}",
        f.warnings
    );
    assert!(at(&mut p, 30.0).warnings.is_empty(), "其后不重复");
    // 改小尺寸：12 帧 × 200² 在预算内，不再降帧；旧条目没被这一帧用到，不重放。
    let smaller = toml2.replace("size = [800, 800]", "size = [200, 200]");
    assert!(
        p.set_theme(&smaller, "", assets.clone())
            .unwrap()
            .is_empty()
    );
    let f = at(&mut p, 40.0);
    assert!(!f.warnings.iter().any(|w| is_drop(w)), "{:?}", f.warnings);
    assert_eq!(
        f.skin.unwrap().decor[0].clip.raw,
        Some(4),
        "不降帧：原始第 4 帧"
    );
    // 改回大尺寸：旧条目又被用到，重放。
    p.set_theme(&toml2, "", assets).unwrap();
    assert!(at(&mut p, 50.0).warnings.iter().any(|w| is_drop(w)));
}

/// `index` 是求值后（z 排序、丢掉无效项）的下标，`src_index` 是 TOML 里的原始下标。
#[test]
fn decor_receipt_carries_toml_index() {
    let (mut p, _) = preview();
    let toml = "[meta]\nname = \"z\"\n[skin]\ndensity = 1\n\
                    [skin.horizontal.frame]\nimage = \"assets/frame.png\"\n\
                    [[skin.horizontal.decor]]\nimage = \"\"\n\
                    [[skin.horizontal.decor]]\nimage = \"assets/dot.png\"\nz = 5\n\
                    [[skin.horizontal.decor]]\nimage = \"assets/dot.png\"\nz = -3\n\
                    [[skin.horizontal.decor]]\nimage = \"assets/missing.png\"\n";
    let assets = vec![
        ("assets/frame.png".into(), png(16, 16, [200, 200, 200, 255])),
        ("assets/dot.png".into(), png(4, 4, [255, 0, 0, 255])),
    ];
    p.set_theme(toml, "", assets).unwrap();
    let s = p.render(&RenderOpts::default()).unwrap().skin.unwrap();
    let got: Vec<(usize, usize, i32)> = s
        .decor
        .iter()
        .map(|d| (d.index, d.src_index, d.z))
        .collect();
    // 求值后：[0] z -3（TOML 2）、[1] 缺图（TOML 3，渲染时丢）、[2] z 5（TOML 1）；TOML 0 空图解析时丢。
    assert_eq!(got, [(0, 2, -3), (2, 1, 5)]);
    let json = serde_json::to_string(&s.decor[0]).unwrap();
    assert!(json.contains("\"srcIndex\":2"), "{json}");
}

/// 装饰（本身或片段引用的附加源）是 SVG：告警、不画，回执里也没有这块装饰。
#[test]
fn svg_decor_warns_instead_of_silently_vanishing() {
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="#f00"/></svg>"##.to_vec();
    let (toml, mut assets) = geom_theme(false, "");
    assets.push(("assets/dot.svg".into(), svg));
    let (mut p, _) = preview();
    let main = toml.replace("assets/dot.png", "assets/dot.svg");
    assert_ne!(main, toml);
    p.set_theme(&main, "", assets.clone()).unwrap();
    let f = p.render(&RenderOpts::default()).unwrap();
    assert!(
        f.warnings
            .iter()
            .any(|w| w == "skin: decor 不支持 SVG（assets/dot.svg），不画"),
        "{:?}",
        f.warnings
    );
    assert!(f.skin.unwrap().decor.is_empty(), "SVG 装饰不该出现在回执里");
    // 附加源是 SVG：装饰本身照画，片段不可用。
    let extra = toml.replace(
        "hit = \"block\"\n",
        "hit = \"block\"\nstates = { idle = { image = \"assets/dot.svg\" } }\n",
    );
    assert_ne!(extra, toml);
    p.set_theme(&extra, "", assets).unwrap();
    let f = p.render(&RenderOpts::default()).unwrap();
    assert!(
        f.warnings
            .iter()
            .any(|w| w == "skin: decor 不支持 SVG（assets/dot.svg），不画"),
        "{:?}",
        f.warnings
    );
    assert_eq!(f.skin.unwrap().decor.len(), 1, "主图是位图，装饰照画");
}

/// 回执带装饰的 `flip`（不翻为 null），外接框与不翻时一致；窗体写 flip 报告警、照常出图。
#[test]
fn decor_receipt_carries_flip_and_frame_flip_warns() {
    let (mut p, _) = preview();
    let (toml, assets) = geom_theme(false, "");
    assert!(p.set_theme(&toml, "", assets.clone()).unwrap().is_empty());
    let plain = p.render(&RenderOpts::default()).unwrap().skin.unwrap();
    assert_eq!(plain.decor[0].flip, None);
    let json = serde_json::to_string(&plain.decor[0]).unwrap();
    assert!(json.contains("\"flip\":null"), "{json}");
    let flipped = toml.replace("hit = \"block\"\n", "hit = \"block\"\nflip = \"xy\"\n");
    assert_ne!(flipped, toml);
    assert!(
        p.set_theme(&flipped, "", assets.clone())
            .unwrap()
            .is_empty()
    );
    let s = p.render(&RenderOpts::default()).unwrap().skin.unwrap();
    assert_eq!(s.decor[0].flip, Some("xy"));
    assert_eq!(s.decor[0].rect, plain.decor[0].rect, "只翻像素，外接框不变");
    let json = serde_json::to_string(&s.decor[0]).unwrap();
    assert!(json.contains("\"flip\":\"xy\""), "{json}");
    // 非法值：告警、按不翻。
    let bad = toml.replace("hit = \"block\"\n", "hit = \"block\"\nflip = \"left\"\n");
    let w = p.set_theme(&bad, "", assets.clone()).unwrap();
    assert!(w.iter().any(|w| w.contains("decor.flip")), "{w:?}");
    let s = p.render(&RenderOpts::default()).unwrap().skin.unwrap();
    assert_eq!(s.decor[0].flip, None);
    // 窗体不开放 flip。
    let frame = toml.replace(
        "image = \"assets/frame.png\"\n",
        "image = \"assets/frame.png\"\nflip = \"x\"\n",
    );
    assert_ne!(frame, toml);
    let w = p.set_theme(&frame, "", assets).unwrap();
    assert!(w.iter().any(|w| w.contains("frame 不支持 flip")), "{w:?}");
    assert!(p.render(&RenderOpts::default()).unwrap().skin.is_some());
}

/// 选到 srcset 里的另一份源图：`frameSource` 报它，`axis*.src` 仍按 `frame.image` 的像素。
#[test]
fn srcset_pick_keeps_axis_in_main_image_pixels() {
    let (mut p, _) = preview();
    let toml = "[meta]\nname = \"x\"\n[skin]\ndensity = 1\n\
                    [skin.horizontal.frame]\nimage = \"assets/f1.png\"\nstretch_x = [[8, 24]]\n\
                    srcset = [{ image = \"assets/f2.png\", density = 2 }]\n";
    let assets = vec![
        ("assets/f1.png".into(), png(32, 32, [200, 200, 200, 255])),
        ("assets/f2.png".into(), png(64, 64, [100, 100, 100, 255])),
    ];
    p.set_theme(toml, "", assets).unwrap();
    let at_scale = |p: &mut Preview<MockCanvas>, scale| {
        p.render(&RenderOpts {
            scale,
            ..Default::default()
        })
        .unwrap()
        .skin
        .unwrap()
    };
    let one = at_scale(&mut p, 1.0);
    assert_eq!(one.frame_source, "assets/f1.png");
    let two = at_scale(&mut p, 2.0);
    assert_eq!(two.frame_source, "assets/f2.png");
    for s in [&one, &two] {
        assert_eq!(s.axis_x[1].src, [8.0, 24.0], "{}", s.frame_source);
        assert_eq!(s.axis_x[2].src, [24.0, 32.0]);
        let fixed = s.axis_x[0].dst[1] - s.axis_x[0].dst[0];
        assert_eq!(fixed, 8.0 * s.render_scale, "{}", s.frame_source);
    }
}

/// 竖排变体与上方布局：回执几何同样就是引擎这一帧的那份。
#[test]
fn vertical_and_above_receipts_match_engine_geometry() {
    let (mut p, _) = preview();
    let (toml, assets) = geom_theme(false, "");
    let toml = toml.replace("skin.horizontal", "skin.vertical");
    p.set_theme(&toml, "", assets.clone()).unwrap();
    let o = RenderOpts {
        vertical: true,
        scale: 2.0,
        ..Default::default()
    };
    let f = p.render(&o).unwrap();
    let s = f.skin.unwrap();
    let g = p.scene.skin_geom.as_ref().unwrap();
    assert_eq!(s.variant, "vertical");
    assert!(f.height > f.width, "{}x{}", f.width, f.height);
    assert_eq!(s.frame, rect_i(g.frame_for_test()));
    let rects: Vec<RectInfo> = g
        .decor_rects_for_test()
        .iter()
        .map(|&r| rect_i(r))
        .collect();
    assert_eq!(s.decor.iter().map(|d| d.rect).collect::<Vec<_>>(), rects);
    assert!(
        p.render(&RenderOpts::default()).unwrap().skin.is_none(),
        "横排没配"
    );

    let (toml, _) = geom_theme(false, "");
    p.set_theme(&toml, "", assets).unwrap();
    let above = p
        .render(&RenderOpts {
            above: true,
            ..Default::default()
        })
        .unwrap();
    let s = above.skin.unwrap();
    let g = p.scene.skin_geom.as_ref().unwrap();
    let fr = rect_i(g.frame_for_test());
    assert_eq!(s.frame, fr);
    let window = above.nodes.iter().find(|n| n.key == "window").unwrap();
    assert_eq!((window.x, window.y), (fr.x, fr.y), "上方重排不挪窗体");
    assert_eq!(s.caret.above, [fr.x, fr.y + fr.h]);
}

/// 亮暗皮肤夹具：窗体、装饰、装饰 `states` 的附加源都经 `[resources]` 的 `{ light, dark }`
/// 引用，亮暗两套各一种颜色；`dark_size` 改暗色窗体的边长（同尺寸是 16）。
const LIGHT: [[u8; 4]; 3] = [[201, 202, 203, 255], [250, 10, 10, 255], [10, 250, 10, 255]];
const DARK: [[u8; 4]; 3] = [[31, 32, 33, 255], [10, 10, 250, 255], [250, 250, 10, 255]];
fn light_dark_theme(dark_size: u32) -> (String, Vec<(String, Vec<u8>)>) {
    let toml = "[meta]\nname = \"ld\"\n\
             [resources]\n\
             frame = { light = \"assets/frame-light.png\", dark = \"assets/frame-dark.png\" }\n\
             dot = { light = \"assets/dot-light.png\", dark = \"assets/dot-dark.png\" }\n\
             busy = { light = \"assets/busy-light.png\", dark = \"assets/busy-dark.png\" }\n\
             [skin]\ndensity = 1\n\
             [skin.horizontal.frame]\nimage = \"frame\"\n\
             stretch_x = [[4, 12]]\nstretch_y = [[4, 12]]\n\
             [[skin.horizontal.decor]]\nimage = \"dot\"\nanchor = \"frame.top_left\"\n\
             [[skin.horizontal.decor]]\nimage = \"dot\"\nanchor = \"frame.top_right\"\n\
             states = { idle = { image = \"busy\" } }\n"
        .to_string();
    let assets = vec![
        ("assets/frame-light.png".into(), png(16, 16, LIGHT[0])),
        (
            "assets/frame-dark.png".into(),
            png(dark_size, dark_size, DARK[0]),
        ),
        ("assets/dot-light.png".into(), png(6, 6, LIGHT[1])),
        ("assets/dot-dark.png".into(), png(6, 6, DARK[1])),
        ("assets/busy-light.png".into(), png(6, 6, LIGHT[2])),
        ("assets/busy-dark.png".into(), png(6, 6, DARK[2])),
    ];
    (toml, assets)
}

fn dark(on: bool) -> RenderOpts {
    RenderOpts {
        dark: on,
        ..Default::default()
    }
}

/// 窗体、装饰、附加源按预览明暗取 `[resources]` 的对应变体；亮暗交替画不串缓存。
#[test]
fn skin_images_follow_light_dark_resources() {
    let (mut p, _) = preview();
    let (toml, assets) = light_dark_theme(16);
    let w = p.set_theme(&toml, "", assets).unwrap();
    assert!(w.is_empty(), "{w:?}");
    let light = p.render(&dark(false)).unwrap();
    let dark_f = p.render(&dark(true)).unwrap();
    for (f, want, other) in [(&light, LIGHT, DARK), (&dark_f, DARK, LIGHT)] {
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        for (w, o) in want.iter().zip(other) {
            assert!(count_px(f, *w) > 0, "缺 {w:?}");
            assert_eq!(count_px(f, o), 0, "混进了另一侧的 {o:?}");
        }
    }
    assert_eq!(light.skin.as_ref().unwrap().frame_source, "frame");
    assert_eq!(
        light.skin.as_ref().unwrap().frame,
        dark_f.skin.as_ref().unwrap().frame,
        "同尺寸：几何不随明暗变"
    );
    // 来回切：每次都画回各自那套（图片缓存按解析后的源区分）。
    for _ in 0..2 {
        assert_eq!(p.render(&dark(false)).unwrap().rgba, light.rgba);
        assert_eq!(p.render(&dark(true)).unwrap().rgba, dark_f.rgba);
    }
}

/// 暗色窗体图与亮色尺寸不同：告警、窗体改用亮色图（几何同亮色），尺寸一致的装饰照换暗色。
#[test]
fn mismatched_dark_frame_falls_back_to_light_image() {
    let (mut p, _) = preview();
    let (toml, assets) = light_dark_theme(20);
    p.set_theme(&toml, "", assets).unwrap();
    let light = p.render(&dark(false)).unwrap();
    assert!(
        light.warnings.is_empty(),
        "亮色不校验：{:?}",
        light.warnings
    );
    let f = p.render(&dark(true)).unwrap();
    assert_eq!(
        f.warnings,
        ["skin: frame 的暗色图 20x20 与亮色图 16x16 尺寸不同，改用亮色图"]
    );
    assert!(count_px(&f, LIGHT[0]) > 0 && count_px(&f, DARK[0]) == 0);
    assert!(count_px(&f, DARK[1]) > 0 && count_px(&f, DARK[2]) > 0);
    let (s, ls) = (f.skin.unwrap(), light.skin.unwrap());
    assert_eq!((s.min_size, s.axis_x), (ls.min_size, ls.axis_x));
    // 装饰主图同理；附加源本就按装饰尺寸铺，不受此限。
    let (toml, mut assets) = light_dark_theme(16);
    assets[3].1 = png(8, 8, DARK[1]);
    assets[5].1 = png(9, 9, DARK[2]);
    p.set_theme(&toml, "", assets).unwrap();
    let f = p.render(&dark(true)).unwrap();
    assert_eq!(
        f.warnings,
        ["skin: dot 的暗色图 8x8 与亮色图 6x6 尺寸不同，改用亮色图"]
    );
    assert!(count_px(&f, LIGHT[1]) > 0 && count_px(&f, DARK[1]) == 0);
    assert!(count_px(&f, DARK[0]) > 0 && count_px(&f, DARK[2]) > 0);
    let d = f.skin.unwrap().decor;
    assert!(
        d.iter().all(|d| (d.rect.w, d.rect.h) == (6.0, 6.0)),
        "{d:?}"
    );
}

/// 暗色窗体图读不出、亮色读得出：告警、改用亮色图，皮肤照常（不退回普通窗口）。
#[test]
fn unreadable_dark_frame_falls_back_to_light_image() {
    let (mut p, _) = preview();
    let (toml, mut assets) = light_dark_theme(16);
    assets.remove(1);
    p.set_theme(&toml, "", assets).unwrap();
    let f = p.render(&dark(true)).unwrap();
    assert_eq!(f.warnings, ["skin: frame 的暗色图读不出，改用亮色图"]);
    assert!(f.skin.is_some(), "皮肤照常");
    assert!(count_px(&f, LIGHT[0]) > 0 && count_px(&f, DARK[1]) > 0);
}

/// 暗色窗体图是 SVG：不拿亮色顶替，交给窗体的 SVG 告警（暗色下回退普通窗口）。
#[test]
fn svg_dark_frame_keeps_svg_warning() {
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><rect width="20" height="20" fill="#f00"/></svg>"##.to_vec();
    let (toml, mut assets) = light_dark_theme(16);
    assets[1] = ("assets/frame-dark.svg".into(), svg);
    let toml = toml.replace("assets/frame-dark.png", "assets/frame-dark.svg");
    let (mut p, _) = preview();
    p.set_theme(&toml, "", assets).unwrap();
    let f = p.render(&dark(true)).unwrap();
    assert!(f.skin.is_none(), "暗色回退普通窗口");
    assert!(
        f.warnings.iter().any(|w| w.contains("SVG")),
        "{:?}",
        f.warnings
    );
    assert!(!f.warnings.iter().any(|w| w.contains("改用亮色图")));
    assert!(p.render(&dark(false)).unwrap().skin.is_some(), "亮色照常");
}

/// 两帧 GIF（各 100ms），4×4 纯色。
fn gif2(a: [u8; 4], b: [u8; 4]) -> Vec<u8> {
    use ::image::codecs::gif::{GifEncoder, Repeat};
    use ::image::{Delay, Frame as ImgFrame, RgbaImage};
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        enc.set_repeat(Repeat::Infinite).unwrap();
        for c in [a, b] {
            let img = RgbaImage::from_pixel(4, 4, ::image::Rgba(c));
            enc.encode_frame(ImgFrame::from_parts(
                img,
                0,
                0,
                Delay::from_numer_denom_ms(100, 1),
            ))
            .unwrap();
        }
    }
    out
}
