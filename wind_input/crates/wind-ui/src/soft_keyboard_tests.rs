use super::*;

/// 默认位置落在协调器给的那块屏上，而不是主屏（`SPI_GETWORKAREA` 恒取主显示器工作区）。
///
/// 副屏在主屏左侧时工作区坐标为负，落点必须跟着为负；钳到 0 就会把面板推回主屏。
#[test]
fn default_origin_follows_given_work_area() {
    // 左侧副屏：虚拟桌面 x ∈ [-1920, 0)，工作区 (−1920, 0, 0, 1040)。
    let (x, y) = default_origin(700, 300, 1.0, Some((-1920, 0, 0, 1040)));
    assert_eq!(x, -1920 + (1920 - 700) / 2, "应在该屏水平居中（负坐标）");
    assert_eq!(y, 1040 - 300 - 16, "应贴该屏工作区底部，留 16px 边距");
    assert!(x < 0, "落点必须留在左侧副屏上，不得被推回主屏");
}

/// 副屏首次打开（还没有任何位置记忆）时，DPI 探针点必须落在那块副屏上。
///
/// 否则主副屏缩放不同时，首次在副屏打开按主屏缩放排版；有了 `LastOrigin` / `Anchor` 后
/// 才变对，表现成「偶发」。
#[test]
fn probe_point_follows_work_area_when_nothing_is_remembered() {
    // 左侧副屏：工作区 (−1920, 0, 0, 1040)，坐标为负。
    // 从入口条件（无锚点、无上一帧落点）走完整条链，而不是直接构造 `DefaultIn`——
    // 后者会跳过 `pick`，正好漏掉出问题的那一步。
    let at = Placement::pick(None, None, Some((-1920, 0, 0, 1040)));
    let (x, y) = at.probe_point();
    assert!(
        x < 0,
        "探针点必须落在左侧副屏上，取到 {x} 说明又按主屏查 DPI 了"
    );
    assert_eq!(
        (x, y),
        (-1, 1039),
        "工作区 right/bottom 是排他边界，须退 1px"
    );
}

/// 三级优先级：`Anchor` > `LastOrigin` > `DefaultIn`。
///
/// 这条钉的是 [`SoftKeyboard::placement`] 的分派本身。`render` 算落点与
/// `ensure_scale` 查 DPI 都基于它，改动优先级会同时影响两处。
#[test]
fn placement_priority_is_anchor_then_last_origin_then_default() {
    let work = Some((-1920, 0, 0, 1040));
    // 有锚点时压过上一帧落点——锚点是用户的**意图**，落点只是上一帧的结果。
    assert_eq!(
        Placement::pick(Some((800, 600)), Some((10, 20)), work),
        Placement::Anchor {
            right: 800,
            bottom: 600
        }
    );
    assert_eq!(
        Placement::pick(None, Some((10, 20)), work),
        Placement::LastOrigin { x: 10, y: 20 }
    );
    assert_eq!(
        Placement::pick(None, None, work),
        Placement::DefaultIn { work }
    );
}

/// 锚点/上一帧落点两级的探针取点：右下角退 1px、左上角不退。
#[test]
fn probe_point_respects_inclusive_and_exclusive_edges() {
    assert_eq!(
        Placement::Anchor {
            right: 1600,
            bottom: 1000
        }
        .probe_point(),
        (1599, 999),
        "锚点是面板右下角（排他），不退 1px 会查到相邻那块屏"
    );
    assert_eq!(
        Placement::LastOrigin { x: 100, y: 200 }.probe_point(),
        (100, 200),
        "左上角是包含边界，本身就在面板内"
    );
    assert_eq!(
        Placement::DefaultIn { work: None }.probe_point(),
        (0, 0),
        "查不到显示器时兜底主屏，与 default_origin 的 SPI_GETWORKAREA 兜底同屏"
    );
}

/// 面板比工作区还高时，纵向落点被夹到工作区顶部而不是变成负的屏外坐标。
///
/// 低分辨率屏 + 大字号缩放下真会发生（面板高度随 scale 线性增长）。
#[test]
fn default_origin_clamps_to_top_when_panel_is_taller_than_work_area() {
    let (_, y) = default_origin(700, 900, 1.0, Some((0, 0, 1280, 720)));
    assert_eq!(y, 0, "放不下时贴顶，而不是落到屏幕上方之外");
}

/// 锚点 → 落点：同一锚点下，尺寸不同（切面）算出的右下角恒等。
///
/// 软键盘各面键数不同 ⇒ 面板宽高随切面变，存左上角的话切一次面就朝右下长一截。
/// 与工具栏 `anchor_keeps_bottom_right_fixed_across_orientation` 是同一个不变量。
#[test]
fn anchor_keeps_bottom_right_fixed_across_pages() {
    const RIGHT: i32 = 1600;
    const BOTTOM: i32 = 1000;
    for (w, h) in [(700u32, 300u32), (900, 300), (700, 360)] {
        let (x, y) = origin_from_anchor(RIGHT, BOTTOM, w, h);
        assert_eq!(
            (x + w as i32, y + h as i32),
            (RIGHT, BOTTOM),
            "面板 {w}x{h} 的右下角跑了"
        );
    }
    // 尺寸变了左上角就该跟着动（朝屏幕内侧），否则说明压根没按当前尺寸现算。
    assert_ne!(
        origin_from_anchor(RIGHT, BOTTOM, 700, 300),
        origin_from_anchor(RIGHT, BOTTOM, 900, 300)
    );
}

#[test]
fn row_slots_match_the_ansi_main_block() {
    // 13+13+11+10 = 47。与 wind-softkeyboard 的 KEY_ROWS 对不上时，键帽会整体错位，
    // 而错位在肉眼看来只是「某些符号跑到了别的键上」。
    assert_eq!(ROW_SLOTS.iter().sum::<usize>(), 47);
}

#[test]
fn page_cycling_wraps() {
    assert_eq!(next_page(0, 3), 1);
    assert_eq!(next_page(2, 3), 0);
    assert_eq!(prev_page(0, 3), 2);
    assert_eq!(prev_page(2, 3), 1);
    // 空表不该 panic（除零 / 下溢）
    assert_eq!(next_page(0, 0), 0);
    assert_eq!(prev_page(0, 0), 0);
}

#[test]
fn only_output_keys_repeat() {
    assert!(repeats(0), "符号键位重复");
    assert!(repeats(SOFT_TAG_FN_BASE), "退格等功能键重复");
    assert!(
        !repeats(SOFT_TAG_FN_BASE + SOFT_FN_CAPS_INDEX as i32),
        "大写锁定绝不重复——它是 toggle，按住会飞速开关，松手时是开是关全凭运气"
    );
    assert!(!repeats(SOFT_TAG_PAGE_BASE), "翻页不重复——按住会飞速乱切");
    assert!(!repeats(SOFT_TAG_CLOSE), "关闭不重复");
    assert!(!repeats(SOFT_TAG_CTRL), "粘滞 Ctrl 不重复");
}

/// tag 分段判据必须是闭区间——开区间会把后加的段整段吞掉。
///
/// 写成 `tag >= SOFT_TAG_PAGE_BASE` 时 `SOFT_TAG_TAB_VIEWPORT` 掉进翻页分支、被协调器越界
/// 检查挡下，点标签行留白什么都没发生。往 `SOFT_TAG_PAGE_BASE..SOFT_TAG_CLOSE` 之外再加
/// tag 段时照着补一条断言：编译器管不了整型常量的分段。
#[test]
fn tag_ranges_are_closed_so_later_segments_are_not_swallowed() {
    assert!(is_page_tag(SOFT_TAG_PAGE_BASE), "第 0 个面");
    assert!(
        is_page_tag(SOFT_TAG_CLOSE - 1),
        "面段的上界（闭区间右端前一个）"
    );
    assert!(!is_page_tag(SOFT_TAG_CLOSE), "关闭不是面");
    assert!(
        !is_page_tag(SOFT_TAG_TAB_VIEWPORT),
        "标签行视口是区域标记，不是某一个面——它被误判成面就是那个 bug 本身"
    );
    assert!(!is_page_tag(SOFT_TAG_TAB_LEFT) && !is_page_tag(SOFT_TAG_TAB_RIGHT));
    assert!(!is_page_tag(SOFT_TAG_FN_BASE), "功能键不是面");
    assert!(!is_page_tag(0), "键位不是面");
}

/// 导出的拖动区与按下时的判据逐像素一致：落在某块拖动区里 ⇔ `drags_panel(hit_at(..))`。
/// 覆盖键位、键间缝、标签行留白（视口）和盖在视口上的标签。
#[test]
fn drag_regions_match_press_hit_test_pixel_by_pixel() {
    let r = |x: f32, y: f32, w: f32, h: f32| Rect { x, y, w, h };
    let m = SoftMouse {
        hits: vec![
            (0, r(10.0, 10.0, 20.0, 20.0)),
            (1, r(40.0, 10.0, 20.0, 20.0)),
            (SOFT_TAG_TAB_VIEWPORT, r(0.0, 40.0, 100.0, 20.0)),
            (SOFT_TAG_PAGE_BASE, r(10.0, 40.0, 30.0, 20.0)),
        ],
        hover: -1,
        pressed: -1,
        ..Default::default()
    };
    let (w, h) = (100u32, 70u32);
    let regions = drag_regions_of(&m, w, h);
    for py in 0..h as i32 {
        for px in 0..w as i32 {
            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
            let inside = regions
                .iter()
                .filter(|[x, y, rw, rh]| px >= *x && px < x + rw && py >= *y && py < y + rh)
                .count();
            assert!(inside <= 1, "拖动区互不重叠 ({px},{py})");
            assert_eq!(
                inside == 1,
                drags_panel(m.hit_at(cx, cy)),
                "({px},{py}) 的拖动区归属与按下判据不一致"
            );
        }
    }
    assert!(
        regions.len() < 20,
        "相邻格应合并，实际 {} 块",
        regions.len()
    );
}

/// 标签行留白按下 → 拖动整块面板，与那条分支自己的注释一致。
#[test]
fn tab_row_blank_drags_the_panel() {
    assert!(drags_panel(-1), "没命中任何命中区（键位之间的缝）");
    assert!(
        drags_panel(SOFT_TAG_TAB_VIEWPORT),
        "标签行留白——命中的是视口这个区域标记，它 >= 0，判 `h < 0` 会漏掉"
    );
    // 反向对照：真控件绝不能被当成拖动，否则点了就只会拖窗口。
    assert!(!drags_panel(0), "键位");
    assert!(!drags_panel(SOFT_TAG_PAGE_BASE), "标签");
    assert!(!drags_panel(SOFT_TAG_TAB_LEFT), "左箭头");
    assert!(!drags_panel(SOFT_TAG_CLOSE), "关闭");
    assert!(!drags_panel(SOFT_TAG_PIN), "钉住");
    assert!(!is_page_tag(SOFT_TAG_PIN), "钉住不是面");
}

/// 两张钉住图标必须能解析且有墨：`rasterize_svg_str_tinted` 解析失败返回 `None`，
/// 绘制侧见 `None` 就静默不画——SVG 写错的表现是按钮上什么都没有，其余测试照样绿。
#[test]
fn pin_icons_rasterize_with_ink() {
    for (name, svg) in [
        ("pin_off", include_str!("../res/icons/pin_off.svg")),
        ("pin_on", include_str!("../res/icons/pin_on.svg")),
    ] {
        let pm = crate::image_cache::rasterize_svg_str_tinted(svg, 13, 13, [0, 0, 0, 255])
            .unwrap_or_else(|| panic!("{name}.svg 解析失败"));
        let inked = pm.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(inked >= 15, "{name} 在 13px 下只有 {inked} 个像素有墨");
    }
}

/// 钉住按钮真正画出图：按 `build` 里同样的节点形状（定宽定高 + 圆角 + 负 margin）绘制，
/// 中心一带必须有墨：图挂在空子容器上时高度塌成 0、只剩顶上一条线，上一条测试验不出。
#[test]
fn pin_button_paints_its_icon() {
    use crate::text::dwrite::TextRenderer;
    use crate::view::{Align, Edges, Layout, View};
    let tr = TextRenderer::new("test", 20.0).unwrap();
    for pinned in [false, true] {
        let mut v = View::container(Layout::Row)
            .fixed_w(60.0)
            .fixed_h(40.0)
            .cross(Align::Center)
            .child(
                View::container(Layout::Row)
                    .fixed_w(32.0)
                    .fixed_h(24.0)
                    .margin(Edges {
                        t: -4.0,
                        l: -4.0,
                        ..Default::default()
                    })
                    .radius(12.0)
                    .bg_image(pin_icon(pinned, [0, 0, 0, 255], 13.0)),
            );
        v.layout(0.0, 0.0, &tr);
        let (w, h) = (60usize, 40usize);
        let mut buf = vec![0u8; w * h * 4];
        v.paint(&mut buf, w as u32, h as u32, &tr);
        let inked = buf.chunks(4).filter(|p| p[3] > 0).count();
        // 13×13 的图钉至少几十个像素；塌成一条线只有十几个。
        assert!(inked >= 30, "pinned={pinned}: 只画出 {inked} 个像素");
    }
}

#[test]
fn slot_labels_use_the_printed_symbol() {
    assert_eq!(slot_label("grave"), "`");
    assert_eq!(slot_label("lbracket"), "[");
    assert_eq!(slot_label("q"), "Q");
    assert_eq!(slot_label("1"), "1");
}

/// 标签滚动的算术：贴左露出、贴右露出、已可见则不动。
#[test]
fn scroll_only_moves_when_the_tab_is_off_screen() {
    let w = [80.0f32; 8]; // 每项 80，间隙 0 ⇒ 内容 640
    let view = 200.0;
    // 已完整可见 ⇒ 一动不动。不这样的话
    // 每帧都会把滚动量拽回去，用户滚不动标签行。
    assert_eq!(scroll_to_show(&w, 0.0, 1, 0.0, view), 0.0);
    // 在右边界之外 ⇒ 贴右露出：第 3 项右缘 4*80=320，减去视口 200
    assert_eq!(scroll_to_show(&w, 0.0, 3, 0.0, view), 120.0);
    // 在左边界之外 ⇒ 贴左露出
    assert_eq!(scroll_to_show(&w, 0.0, 1, 300.0, view), 80.0);
    // 不会滚过头：末项也不超过 content-view
    assert_eq!(scroll_to_show(&w, 0.0, 7, 0.0, view), 640.0 - 200.0);
    // 内容装得下 ⇒ 恒为 0
    assert_eq!(scroll_to_show(&[50.0, 50.0], 0.0, 1, 0.0, view), 0.0);
    // 空表不 panic
    assert_eq!(scroll_to_show(&[], 0.0, 3, 0.0, view), 0.0);
}
