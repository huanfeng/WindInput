use super::*;

// 典型值：主题 [toolbar] 默认（条厚 30dp、拖动柄 12dp、格 30dp）× scale 1.0，
// 4 格（模式/标点/全半角/设置）。
const THICK: f32 = 30.0;
const GRIP: f32 = 12.0;
const CELL: f32 = 30.0;
const N: usize = 4;

/// 横条：回归基线。重构成 `bar_layout` 之前，render 内联算的就是这组值——
/// 这条测试的作用是「纵向功能没有偷偷改掉横向的既有排布」。
#[test]
fn horizontal_layout_matches_legacy_geometry() {
    let l = bar_layout(false, THICK, GRIP, CELL, N);
    assert_eq!((l.w, l.h), (12.0 + 30.0 * 4.0, 30.0));
    assert_eq!(
        (l.grip.x, l.grip.y, l.grip.w, l.grip.h),
        (0.0, 0.0, 12.0, 30.0)
    );
    // 格自拖动柄之后起，沿 x 依次排开，各格占满条厚。
    for (i, c) in l.cells.iter().enumerate() {
        assert_eq!(c.x, 12.0 + 30.0 * i as f32, "第 {i} 格 x");
        assert_eq!((c.y, c.w, c.h), (0.0, 30.0, 30.0), "第 {i} 格");
    }
}

/// 拖动区 = 拖动柄：横条在左端、纵条在顶端；格子不能落进去（否则 addon 会把点格子
/// 当成拖动吃掉）。首帧之前的空矩形不导出。
#[test]
fn drag_region_is_the_grip_and_excludes_cells() {
    for vertical in [false, true] {
        let l = bar_layout(vertical, THICK, GRIP, CELL, N);
        let [x, y, w, h] = grip_region(&l.grip).expect("拖动柄非空");
        let want = if vertical {
            [0, 0, 30, 12]
        } else {
            [0, 0, 12, 30]
        };
        assert_eq!([x, y, w, h], want, "vertical={vertical}");
        for c in &l.cells {
            let inside = (c.x + c.w * 0.5) < (x + w) as f32 && (c.y + c.h * 0.5) < (y + h) as f32;
            assert!(!inside, "格子中心落进了拖动区 vertical={vertical}");
        }
    }
    assert_eq!(grip_region(&Rect::default()), None);
}

/// 右下角落点：从工作区右/下边界各退去工具栏尺寸再留 12px 边距。
#[test]
fn corner_backs_off_from_work_area_edges() {
    // 1920×1080 主屏，任务栏 40px：工作区右下 (1920, 1040)；工具栏 132×30。
    let (x, y) = Toolbar::corner_in_work_area(1920, 1040, 132, 30);
    assert_eq!((x, y), (1920 - 132 - 12, 1040 - 30 - 12));
}

/// 副屏在主屏**左侧**时工作区坐标为负，落点必须跟着为负。
///
/// 所以此处不能 `max(0)`：钳到 0 会把工具栏推回主屏。越界回收由 `render` 里的
/// `clamp_to_work_area` 负责，它按落点反查显示器，不预设桌面原点在 (0,0)。
#[test]
fn corner_allows_negative_coords_on_left_side_monitor() {
    // 左侧副屏：虚拟桌面 x ∈ [-1920, 0)，工作区右下 (0, 1080)。
    let (x, y) = Toolbar::corner_in_work_area(0, 1080, 132, 30);
    assert_eq!(x, -144, "落点应留在左侧副屏上（负坐标）");
    assert_eq!(y, 1038);
}

/// 落点是尺寸的函数：同一块屏上横条与纵条的右下角必然落在不同位置。
///
/// 窗口以 `create(160, 40)` 的占位尺寸起步，`set_vertical` 在隐藏期间不重排，首次 `render`
/// 之前 `window.size()` 不是真值；用它算/钳落点会让重启后凭空左移。故 `set_anchor`/`set_corner`
/// 隐藏期间只登记意图，落点与钳制统一由 `render` 用刚 `resize` 出的尺寸计算。
#[test]
fn corner_depends_on_bar_orientation() {
    // 1920×1080 屏、底部 40px 任务栏 → 工作区右下 (1920, 1040)。
    // 默认几何：横条 132×30，转置后纵条 30×132。
    let horizontal = Toolbar::corner_in_work_area(1920, 1040, 132, 30);
    let vertical = Toolbar::corner_in_work_area(1920, 1040, 30, 132);
    assert_eq!(vertical.0 - horizontal.0, 102, "纵条更窄，落点更靠右");
    assert_eq!(horizontal.1 - vertical.1, 102, "纵条更高，落点更靠上");

    // 占位尺寸算出的落点比纵条真值靠左 130px——「重启后位置左移」的正是这个量。
    // 若哪天改了 create 的占位尺寸，这条会红，提醒回来确认推迟计算仍然成立。
    let placeholder = Toolbar::corner_in_work_area(1920, 1040, 160, 40);
    assert_eq!(vertical.0 - placeholder.0, 130);
}

/// 四级优先级：`PendingCorner` > `Anchor` > `LastPos` > `PrimaryCorner`。
///
/// 第一级压过记忆锚点是有意的：协调器跟随焦点换屏时下发目标屏工作区，若让 `Anchor` 赢，
/// 工具栏会被上一块屏的记忆位置顶回去。
#[test]
fn placement_priority_puts_pending_corner_above_remembered_anchor() {
    assert_eq!(
        Placement::pick(Some((2560, 1392)), Some((800, 600)), Some((10, 20))),
        Placement::PendingCorner {
            work_right: 2560,
            work_bottom: 1392
        },
        "换屏请求必须压过记忆锚点，否则跟随焦点换屏会被旧屏位置顶回去"
    );
    assert_eq!(
        Placement::pick(None, Some((800, 600)), Some((10, 20))),
        Placement::Anchor {
            right: 800,
            bottom: 600
        },
        "锚点是用户的意图，压过上一帧落点"
    );
    assert_eq!(
        Placement::pick(None, None, Some((10, 20))),
        Placement::LastPos { x: 10, y: 20 }
    );
    assert_eq!(Placement::pick(None, None, None), Placement::PrimaryCorner);
}

/// DPI 探针点逐级都落在**目标屏**内。
///
/// 与软键盘 `probe_point_follows_work_area_when_nothing_is_remembered` 同一个不变量：
/// `ensure_scale` 与 `render` 必须认定同一块屏，否则按 A 屏 DPI 排出的尺寸落到 B 屏。
#[test]
fn probe_point_stays_on_the_target_screen_at_every_level() {
    // 左侧副屏：工作区 (−1920, 0, 0, 1040)，坐标为负。
    let at = Placement::pick(Some((0, 1040)), None, None);
    assert_eq!(
        at.probe_point(),
        (-1, 1039),
        "工作区 right/bottom 排他，须退 1px；不退会查到右邻那块屏"
    );
    assert_eq!(
        Placement::pick(None, Some((0, 1040)), None).probe_point(),
        (-1, 1039),
        "锚点是窗口右下角，同样排他"
    );
    assert_eq!(
        Placement::pick(None, None, Some((-800, 300))).probe_point(),
        (-800, 300),
        "上一帧落点是左上角，包含边界，不退"
    );
    assert_eq!(
        Placement::pick(None, None, None).probe_point(),
        (0, 0),
        "无任何记录时兜底主屏，与 corner_position 的 SPI_GETWORKAREA 同屏"
    );
}

/// 横纵切换时右下角纹丝不动。
///
/// `bar_layout` 让纵条与横条互为转置（132×30 ↔ 30×132）；若固定左上角，贴屏幕右下角摆放
/// （出厂默认位置）时多出的长度会顶出工作区、再被 `clamp_to_work_area` 拉回，每切一次挪一次。
/// 不变量：同一锚点下，两个朝向算出的落点 + 各自尺寸 = 同一个右下角。
#[test]
fn anchor_keeps_bottom_right_fixed_across_orientation() {
    // 用户把工具栏摆在这里（窗口右下角）。
    const RIGHT: i32 = 1908;
    const BOTTOM: i32 = 1028;
    // 默认几何：横条 132×30，转置后纵条 30×132。
    let (hx, hy) = Toolbar::origin_from_anchor(RIGHT, BOTTOM, 132, 30);
    let (vx, vy) = Toolbar::origin_from_anchor(RIGHT, BOTTOM, 30, 132);

    assert_eq!(
        (hx + 132, hy + 30),
        (RIGHT, BOTTOM),
        "横条右下角应落在锚点上"
    );
    assert_eq!(
        (vx + 30, vy + 132),
        (RIGHT, BOTTOM),
        "纵条右下角应落在锚点上"
    );

    // 左上角**应该**动（尺寸变了），动的方向是朝屏幕内侧——这才是想要的。
    assert_eq!(vx - hx, 102, "纵条更窄，左上角右移");
    assert_eq!(vy - hy, -102, "纵条更高，左上角上移");
}

/// 增删工具栏格子（`ui.toolbar.items`）同样不得挪动位置。
///
/// 与换向是同一个不变量的另一面：`bar_layout` 的条长 = `grip + cell * n`，格数一变条长就变。
#[test]
fn anchor_keeps_bottom_right_fixed_across_cell_count() {
    const RIGHT: i32 = 1908;
    const BOTTOM: i32 = 1028;
    for w in [80u32, 132, 210] {
        let (x, y) = Toolbar::origin_from_anchor(RIGHT, BOTTOM, w, 30);
        assert_eq!(
            (x + w as i32, y + 30),
            (RIGHT, BOTTOM),
            "条宽 {w} 时右下角跑了"
        );
    }
}

/// 纵条：整条与横条互为转置——宽高对调、格沿 y 排开、各格占满条宽。
#[test]
fn vertical_layout_is_transpose_of_horizontal() {
    let h = bar_layout(false, THICK, GRIP, CELL, N);
    let v = bar_layout(true, THICK, GRIP, CELL, N);
    assert_eq!((v.w, v.h), (h.h, h.w), "整条宽高对调");
    assert_eq!((v.grip.w, v.grip.h), (h.grip.h, h.grip.w), "拖动柄区对调");
    assert_eq!(v.cells.len(), h.cells.len());
    for (i, (cv, ch)) in v.cells.iter().zip(h.cells.iter()).enumerate() {
        assert_eq!((cv.x, cv.y), (ch.y, ch.x), "第 {i} 格坐标对调");
        assert_eq!((cv.w, cv.h), (ch.h, ch.w), "第 {i} 格尺寸对调");
    }
}

/// 主题几何在两个朝向下同源：条厚恒取 `height`、格长恒取 `button_width`。
/// 若哪天有人为纵向另引一套尺寸，这条会红——那正是要挡的改动。
#[test]
fn vertical_reuses_same_theme_dimensions() {
    let v = bar_layout(true, THICK, GRIP, CELL, N);
    assert_eq!(v.w, THICK, "纵条宽 = 主题 height");
    for c in &v.cells {
        assert_eq!(c.h, CELL, "纵条每格高 = 主题 button_width");
        assert_eq!(c.w, THICK, "纵条每格宽 = 条宽");
    }
    assert_eq!(v.h, GRIP + CELL * N as f32, "纵条总高 = 拖动柄 + 各格");
}

/// 兜底外框圆角在两个朝向下必须相等：它描述的是条的厚度，与条有多长无关。
///
/// 按 `h` 取比例时，纵条下 `h` 是整条长度，圆角会被 `push_round_rect` 钳成满胶囊、
/// 格数越多越必然触顶。按钮高亮圆角那里用 `hw.min(hh)`，外框须同样按朝向取厚度。
#[test]
fn border_radius_is_orientation_invariant() {
    let h = bar_layout(false, THICK, GRIP, CELL, N);
    let v = bar_layout(true, THICK, GRIP, CELL, N);
    let rh = default_border_radius(h.w, h.h);
    let rv = default_border_radius(v.w, v.h);
    assert_eq!(rh, rv, "同一套几何转 90° 后圆角不得改变");
    assert_eq!(rh, THICK * 0.30, "圆角派生自条厚，与条长无关");

    // 钳制上限 = 短边一半；兜底值必须留在其下，否则会被静默压成满胶囊。
    for (tag, l) in [("横", &h), ("纵", &v)] {
        let r = default_border_radius(l.w, l.h);
        assert!(
            r < l.w.min(l.h) * 0.5,
            "{tag}条圆角 {r} 触顶（短边半值 {}）",
            l.w.min(l.h) * 0.5
        );
    }

    // 条越长圆角越不该跟着变——纵条加 5 格，半径原地不动。
    let longer = bar_layout(true, THICK, GRIP, CELL, N + 5);
    assert_eq!(default_border_radius(longer.w, longer.h), rv);
}

/// 格数随简繁格增减（`cells()` 的既有行为），布局须跟着长短，不能越界。
#[test]
fn layout_tracks_cell_count() {
    let four = bar_layout(true, THICK, GRIP, CELL, 4);
    let five = bar_layout(true, THICK, GRIP, CELL, 5);
    assert_eq!(five.h - four.h, CELL, "多一格恰长一格");
    assert_eq!(five.cells.len(), 5);
    // 末格不得超出整条（渲染越界会被静默裁掉，看着像"最后一格没画出来"）。
    let last = five.cells.last().unwrap();
    assert!(
        last.y + last.h <= five.h,
        "末格越界：{} > {}",
        last.y + last.h,
        five.h
    );
}

/// n=1（`items` 只留一格）与 n=0 的排布。
///
/// `ui.toolbar.items` 让两者可达。n=0 已由 `expand_cells` 的兜底挡住，仍钉一条：`bar_layout`
/// 全程无除法，就算兜底被绕过也不会 panic、不会算出负尺寸。
#[test]
fn layout_handles_one_and_zero_cells() {
    let one = bar_layout(false, THICK, GRIP, CELL, 1);
    assert_eq!(one.w, GRIP + CELL);
    assert_eq!(one.cells.len(), 1);
    assert_eq!(one.cells[0].x, GRIP);

    let zero = bar_layout(false, THICK, GRIP, CELL, 0);
    assert!(zero.cells.is_empty());
    // 只剩拖动柄：尺寸仍为正（负尺寸会让 resize/缓冲区计算炸掉）。
    assert_eq!(zero.w, GRIP);
    assert!(zero.w > 0.0 && zero.h > 0.0);
}

/// `s2t_on` = 简入繁出当前是否开着。**它只该影响格里画什么，不该影响有没有这一格**
/// ——参数名从早先的 `s2t_shown` 改过来，正是因为那层「开着才显示」的合取已删。
fn tb_state(s2t_on: bool) -> ToolbarState {
    ToolbarState {
        icon_label: "拼".to_string(),
        s2t_enabled: s2t_on,
        // 桌面工具栏已不读这个字段（它留给 macOS / 移动端），这里跟着 s2t_enabled 填，
        // 避免读者误以为两者还有关系。
        s2t_shown: s2t_on,
        ..Default::default()
    }
}

/// 同 [`tb_state`]，但开的是繁入简出那一档。两个方向互斥，故不做「两个都开」的组合。
fn tb_state_t2s(t2s_on: bool) -> ToolbarState {
    ToolbarState {
        icon_label: "拼".to_string(),
        t2s_enabled: t2s_on,
        ..Default::default()
    }
}

fn actions(cells: &[Cell]) -> Vec<ToolbarAction> {
    cells.iter().map(|c| c.action).collect()
}

/// 默认项序列展开出全部七格，且**与任何运行时状态无关**。
#[test]
fn default_layout_expands_to_every_item() {
    let expected = vec![
        ToolbarAction::ToggleMode,
        ToolbarAction::TogglePunct,
        ToolbarAction::ToggleWidth,
        ToolbarAction::ToggleS2t,
        ToolbarAction::ToggleT2s,
        ToolbarAction::ToggleSoftKeyboard,
        ToolbarAction::OpenSettings,
    ];
    for on in [true, false] {
        assert_eq!(
            actions(&expand_cells(
                &wind_ui_types::DEFAULT_TOOLBAR_ITEMS,
                &tb_state(on)
            )),
            expected,
            "格的有无不该随运行时状态变（s2t_on={on}）"
        );
        assert_eq!(
            actions(&expand_cells(
                &wind_ui_types::DEFAULT_TOOLBAR_ITEMS,
                &tb_state_t2s(on)
            )),
            expected,
            "格的有无不该随运行时状态变（t2s_on={on}）"
        );
    }
}

/// 简繁格恒在，开关态只体现在格内的字与高亮上。
///
/// 它是简繁的唯一鼠标入口，若与「简繁当前开着」合取，关掉后自己消失就再也开不回来。
/// 显隐归 `ui.toolbar.items` 一处管，运行时不许再插一脚。
#[test]
fn s2t_cell_stays_when_conversion_is_off() {
    let layout = [ToolbarItem::S2t];
    for (s2t_on, text, hl) in [(true, "繁", true), (false, "简", false)] {
        let cells = expand_cells(&layout, &tb_state(s2t_on));
        assert_eq!(actions(&cells), vec![ToolbarAction::ToggleS2t]);
        assert_eq!(cells[0].text, text);
        assert_eq!(cells[0].highlight, hl);
    }
}

/// 同 [`s2t_cell_stays_when_conversion_is_off`]，钉住繁简格的同一条不变量。
///
/// 另外钉住格里画什么：本格恒显「简」、开合只由高亮表达，与上面那格的「关简 / 开繁」轮换
/// 不同构。若图对称改成轮换，两格都关时会一起显「简」，用户分不出哪格是哪格。
#[test]
fn t2s_cell_stays_and_never_swaps_its_glyph() {
    let layout = [ToolbarItem::T2s];
    for (t2s_on, hl) in [(true, true), (false, false)] {
        let cells = expand_cells(&layout, &tb_state_t2s(t2s_on));
        assert_eq!(actions(&cells), vec![ToolbarAction::ToggleT2s]);
        assert_eq!(cells[0].text, "简", "本格的字不随开关变");
        assert_eq!(cells[0].highlight, hl);
    }
}

/// 顺序照配置走，没配的项不出现。
#[test]
fn expand_follows_configured_order_and_subset() {
    let layout = [ToolbarItem::Settings, ToolbarItem::Mode];
    assert_eq!(
        actions(&expand_cells(&layout, &tb_state(true))),
        vec![ToolbarAction::OpenSettings, ToolbarAction::ToggleMode]
    );
}

/// 空展开必须回落全量条，而不是渲染出一条只剩拖动柄的空窄条。
///
/// 如今内置项全部无条件产格，只有空 layout 还能走到。兜底照留：闸门要装在产出最终结果的
/// 那一环，而不是靠上游恰好不送空进来。
#[test]
fn empty_expansion_falls_back_instead_of_rendering_nothing() {
    let cells = expand_cells(&[], &tb_state(false));
    assert!(!cells.is_empty(), "空展开必须回落，否则工具栏只剩拖动柄");
    assert_eq!(
        actions(&cells),
        actions(&expand_cells_raw(
            &wind_ui_types::DEFAULT_TOOLBAR_ITEMS,
            &tb_state(false)
        )),
        "回落的应当是全量条"
    );
    // 内核（无兜底）确实会给出空——证明上面那条断言测的是兜底本身，不是恒真。
    assert!(expand_cells_raw(&[], &tb_state(false)).is_empty());
}

/// 每张工具栏图标都必须**真的能光栅化出可见形状**。
///
/// 守一个全程无声的失效：`rasterize_svg_str_tinted` 解析失败返回 `None`，`draw_svg_icon`
/// 见 `None` 直接 return，SVG 写错的表现只是格子空白，没有日志、没有 panic。
///
/// 按 `FONT_PX * 0.80` 的实际渲染尺寸（12px）测，顺带钉住「12px 上还剩多少墨」：
/// 低于阈值多半是形状太细或跑到画布外了。
#[test]
fn every_toolbar_icon_rasterizes_to_visible_ink() {
    let size = (Toolbar::FONT_PX * 0.80).round() as u32;
    for (name, svg) in [
        ("punct_full", PUNCT_FULL_SVG),
        ("punct_half", PUNCT_HALF_SVG),
        ("width_full", WIDTH_FULL_SVG),
        ("width_half", WIDTH_HALF_SVG),
        ("setting", SETTING_SVG),
        ("soft_keyboard", SOFT_KEYBOARD_SVG),
    ] {
        let pm = crate::image_cache::rasterize_svg_str_tinted(svg, size, size, [0, 0, 0, 255])
            .unwrap_or_else(|| panic!("{name}.svg 解析失败——渲染时会静默画不出来"));
        let inked = pm.pixels().iter().filter(|p| p.alpha() > 0).count();
        let total = (size * size) as usize;
        assert!(
            inked * 100 / total >= 5,
            "{name}.svg 在 {size}px 下只有 {inked}/{total} 像素有墨——形状太细或落在画布外"
        );
    }
}

/// 缩放只改绝对值、不改结构：dp→设备像素由调用方（render 的 dim 闭包）算好再传入。
#[test]
fn layout_scales_uniformly() {
    let one = bar_layout(true, THICK, GRIP, CELL, N);
    let two = bar_layout(true, THICK * 2.0, GRIP * 2.0, CELL * 2.0, N);
    assert_eq!((two.w, two.h), (one.w * 2.0, one.h * 2.0));
}
