use super::mock::{EMOJI_RGB, MockCanvas};
use super::*;

fn backend() -> (CanvasText<MockCanvas>, MockCanvas) {
    let c = MockCanvas::default();
    (CanvasText::new(c.clone()), c)
}

fn bgra_at(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

#[test]
fn font_string_from_style() {
    let (mut t, _) = backend();
    let ts = TextStyle::new(14.5);
    assert_eq!(t.font(&ts), format!("400 14.5px {DEFAULT_FAMILY}"));
    let ts = ts.with_weight(700).with_family(Some("A \"B\""));
    assert_eq!(
        t.font(&ts),
        format!("700 14.5px \"A \\\"B\\\"\", {DEFAULT_FAMILY}")
    );
    t.set_family("Noto Serif");
    t.set_weight(600);
    assert_eq!(
        t.font(&TextStyle::new(10.0)),
        format!("600 10px \"Noto Serif\", {DEFAULT_FAMILY}"),
        "字重 0 继承全局字重"
    );
}

/// 字重钳到 1..=1000，字号不合法（非有限、非正）用基准字号：CSS 里写不合法的值会被
/// `ctx.font` 静默忽略、沿用上一次的字体。
#[test]
fn font_string_clamps_weight_and_rejects_bad_size() {
    let (mut t, _) = backend();
    assert!(
        t.font(&TextStyle::new(10.0).with_weight(5000))
            .starts_with("1000 10px ")
    );
    t.set_weight(-3);
    assert!(t.font(&TextStyle::new(10.0)).starts_with("400 10px "));
    t.set_weight(9999);
    assert!(t.font(&TextStyle::new(10.0)).starts_with("1000 10px "));
    for bad in [f32::NAN, f32::INFINITY, 0.0, -5.0] {
        assert!(t.font(&TextStyle::new(bad)).contains(" 16px "), "{bad}");
    }
}

#[test]
fn measure_is_cached_per_key_and_family() {
    let (mut t, c) = backend();
    let ts = TextStyle::new(10.0);
    let m = t.measure("你好", &ts);
    assert_eq!((m.width, m.height), (12.0, 12.0));
    t.measure("你好", &ts);
    assert_eq!(c.measures.borrow().len(), 1, "同键命中缓存");
    t.measure("你好", &ts.with_weight(700));
    assert_eq!(c.measures.borrow().len(), 2);
    t.set_family("X");
    t.measure("你好", &ts);
    assert_eq!(c.measures.borrow().len(), 3, "换全局字族清缓存");
}

/// 对齐 Linux 后端：空串行高 = 字号 × 1.2；`\n` 分行，宽取最宽、高按行累加。
#[test]
fn empty_and_multiline_measure_like_linux() {
    let (t, c) = backend();
    let ts = TextStyle::new(10.0);
    let e = t.measure("", &ts);
    assert_eq!((e.width, e.height), (0.0, 12.0));
    assert!(c.measures.borrow().is_empty(), "空串不问宿主");
    let m = t.measure("ab\nabcd\n", &ts);
    assert_eq!((m.width, m.height), (24.0, 36.0));
}

/// 白色覆盖率着色 + source-over：不透明覆盖，半透明与底色混合；越界不 panic。
#[test]
fn draw_tints_coverage_and_composites() {
    let (t, _) = backend();
    let (w, h) = (20, 20);
    let mut buf = [0, 0, 255, 255].repeat((w * h) as usize); // 不透明红底（BGRA）
    let ts = TextStyle::new(10.0);
    t.draw(&mut buf, w, h, 2.4, 3.0, "a", &ts, [0, 255, 0, 255])
        .unwrap();
    assert_eq!(bgra_at(&buf, w, 2, 3), [0, 255, 0, 255], "不透明覆盖");
    assert_eq!(bgra_at(&buf, w, 1, 3), [0, 0, 255, 255], "字左边不动");
    t.draw(&mut buf, w, h, 10.0, 10.0, "a", &ts, [0, 0, 255, 128])
        .unwrap();
    assert_eq!(bgra_at(&buf, w, 10, 10), [128, 0, 127, 255], "半透明蓝叠红");
    t.draw(
        &mut buf,
        w,
        h,
        -50.0,
        18.0,
        "很长很长很长的字",
        &ts,
        [9, 9, 9, 255],
    )
    .unwrap();
}

/// 只栅格化落在缓冲里的横坐标段：极长的一行也只要缓冲宽那么大的位图。
#[test]
fn draw_requests_only_the_visible_columns() {
    let (t, c) = backend();
    let (w, h) = (30, 12);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let long = "字".repeat(10_000);
    t.draw(
        &mut buf,
        w,
        h,
        -100.0,
        0.0,
        &long,
        &TextStyle::new(10.0),
        [1, 1, 1, 255],
    )
    .unwrap();
    assert_eq!(c.draws.borrow()[0].2, (100, 130));
}

/// 分段色：一行只栅格化一次，按列区间着色；重叠时后写的赢；测量仍按前缀。
#[test]
fn draw_runs_colors_columns_once_per_line() {
    let (t, c) = backend();
    let (w, h) = (40, 12);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let ts = TextStyle::new(10.0); // 每字 6 列
    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];
    let runs = [
        ColorRun {
            start: 1,
            end: 3,
            rgba: red,
        },
        ColorRun {
            start: 2,
            end: 3,
            rgba: green,
        },
    ];
    t.draw_runs(
        &mut buf,
        w,
        h,
        0.0,
        0.0,
        "abcd",
        &ts,
        [0, 0, 255, 255],
        &runs,
    )
    .unwrap();
    assert_eq!(bgra_at(&buf, w, 5, 0), [255, 0, 0, 255], "a 是基色蓝");
    assert_eq!(bgra_at(&buf, w, 6, 0), [0, 0, 255, 255], "b 是红");
    assert_eq!(
        bgra_at(&buf, w, 12, 0),
        [0, 255, 0, 255],
        "c 被后写的绿盖住"
    );
    assert_eq!(bgra_at(&buf, w, 18, 0), [255, 0, 0, 255], "d 回到基色");
    assert_eq!(c.draws.borrow().len(), 1, "整行只栅格化一次");
}

/// 彩色字形（emoji）保持本色——包括它的白色部分——只乘区间颜色的不透明度；只有可能含彩色
/// 字形的行才画第二遍。
#[test]
fn color_glyph_keeps_its_own_color_including_white() {
    let (t, c) = backend();
    let (w, h) = (20, 12);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let runs = [ColorRun {
        start: 0,
        end: 5,
        rgba: [0, 0, 255, 255],
    }];
    let ts = TextStyle::new(10.0); // 每格 6 列：a = 0..6，😀 = 6..12（左半橙、右半白）
    t.draw_runs(&mut buf, w, h, 0.0, 0.0, "a😀", &ts, [9, 9, 9, 255], &runs)
        .unwrap();
    let [r, g, b] = EMOJI_RGB;
    assert_eq!(bgra_at(&buf, w, 7, 0), [b, g, r, 255], "橙色部分保持本色");
    assert_eq!(
        bgra_at(&buf, w, 10, 0),
        [255, 255, 255, 255],
        "白色部分不被染成蓝"
    );
    assert_eq!(bgra_at(&buf, w, 1, 0), [255, 0, 0, 255], "普通字按区间色");
    assert_eq!(c.draws.borrow().len(), 2, "含 emoji 的行黑白各画一遍");
    let mut buf = vec![0u8; (w * h * 4) as usize];
    t.draw(&mut buf, w, h, 0.0, 0.0, "你a", &ts, [0, 0, 255, 255])
        .unwrap();
    assert_eq!(c.draws.borrow().len(), 3, "纯中英文行只画一遍");
}

#[test]
fn color_glyph_detection() {
    for yes in ["😀", "a✅", "❤\u{FE0F}", "🇨🇳", "©", "1\u{FE0F}\u{20E3}"] {
        assert!(may_have_color_glyph(yes), "{yes:?}");
    }
    for no in ["你好", "abc", "（全角）", "ㄅ", "…"] {
        assert!(!may_have_color_glyph(no), "{no:?}");
    }
}

#[test]
fn multiline_draw_stacks_lines() {
    let (t, c) = backend();
    let (w, h) = (20, 30);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    t.draw(
        &mut buf,
        w,
        h,
        0.0,
        0.0,
        "a\nb",
        &TextStyle::new(10.0),
        [0, 255, 0, 255],
    )
    .unwrap();
    assert_eq!(
        bgra_at(&buf, w, 1, 13),
        [0, 255, 0, 255],
        "第二行在行高之下"
    );
    assert_eq!(c.draws.borrow().len(), 2);
}

/// 宿主失败（JS 抛异常）：画字返回错误，测量按估值继续，错误都记下供调用方取走。
#[test]
fn host_failures_are_errors_not_panics() {
    let (t, c) = backend();
    c.fail.set(true);
    let mut buf = vec![0u8; 16 * 16 * 4];
    let m = t.measure("ab", &TextStyle::new(10.0));
    assert_eq!(m.width, 12.0);
    assert!(t.take_error().unwrap().contains("mock measure"));
    assert!(
        t.draw(
            &mut buf,
            16,
            16,
            0.0,
            0.0,
            "a",
            &TextStyle::new(10.0),
            [1, 1, 1, 255]
        )
        .is_err()
    );
    assert!(t.take_error().is_some());
    assert!(t.take_error().is_none(), "取走即清");
    c.fail.set(false);
    assert!(
        t.draw(
            &mut buf,
            16,
            16,
            0.0,
            0.0,
            "a",
            &TextStyle::new(10.0),
            [1, 1, 1, 255]
        )
        .is_ok()
    );
}

#[test]
fn color_spans_last_writer_wins_and_merges() {
    let r = |s, e, c: u8| ColorRun {
        start: s,
        end: e,
        rgba: [c, 0, 0, 255],
    };
    let text = "你好ab";
    let runs = [r(0, 6, 1), r(3, 7, 2), r(7, 8, 3), r(1, 3, 9), r(7, 9, 9)];
    let v = valid_runs(text, &runs);
    assert_eq!(v.len(), 3, "切在字中间的、越界的丢掉");
    let base = [0, 0, 0, 255];
    assert_eq!(
        color_spans(text, &v, base),
        vec![
            (0, 3, [1, 0, 0, 255]),
            (3, 7, [2, 0, 0, 255]),
            (7, 8, [3, 0, 0, 255])
        ]
    );
    assert_eq!(color_spans("ab", &[], base), vec![(0, 2, base)]);
}

#[test]
fn mismatched_bitmap_is_an_error() {
    let g = Glyphs {
        x: 0,
        y: 0,
        w: 2,
        h: 2,
        rgba: vec![0; 3],
    };
    let mut buf = vec![0u8; 16];
    let segs = [(i32::MIN, i32::MAX, [1; 4])];
    assert!(composite(&mut buf, 2, 2, 0, 0, &g, None, &segs).is_err());
    let ok = Glyphs {
        x: 0,
        y: 0,
        w: 1,
        h: 1,
        rgba: vec![255; 4],
    };
    let other = Glyphs {
        x: 1,
        y: 0,
        w: 1,
        h: 1,
        rgba: vec![0, 0, 0, 255],
    };
    assert!(
        composite(&mut buf, 2, 2, 0, 0, &ok, Some(&other), &segs).is_err(),
        "两遍位图不一致"
    );
}
