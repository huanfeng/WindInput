use super::*;
use crate::text::MockText as TextRenderer;

fn tr() -> TextRenderer {
    TextRenderer::new("test", 20.0).unwrap()
}

/// 构造一个固定尺寸的叶容器（无文本，尺寸跨平台确定）。
fn fixed(w: f32, h: f32, tag: i32) -> View {
    View::container(Layout::Row).fixed_w(w).fixed_h(h).tag(tag)
}

/// 取某 tag 节点 arrange 后的绝对矩形。
fn hit(root: &View, tag: i32) -> Rect {
    let mut out = Vec::new();
    root.collect_hits(&mut out);
    out.into_iter()
        .find(|(t, _)| *t == tag)
        .unwrap_or_else(|| panic!("tag {tag} 未命中"))
        .1
}

#[test]
fn measure_row_sums_main_axis_with_gap() {
    let mut v = View::container(Layout::Row)
        .gap(10.0)
        .child(fixed(50.0, 20.0, -1))
        .child(fixed(50.0, 20.0, -1));
    v.layout(0.0, 0.0, &tr());
    assert_eq!(v.measured_size(), (110.0, 20.0)); // 50+50+gap10, 交叉轴取 max
}

#[test]
fn measure_column_sums_main_axis_with_gap() {
    let mut v = View::container(Layout::Column)
        .gap(10.0)
        .child(fixed(50.0, 20.0, -1))
        .child(fixed(50.0, 20.0, -1));
    v.layout(0.0, 0.0, &tr());
    assert_eq!(v.measured_size(), (50.0, 50.0)); // 宽取 max50, 高 20+20+gap10
}

#[test]
fn measure_adds_padding() {
    let mut v = View::container(Layout::Row)
        .pad(Edges::all(8.0))
        .child(fixed(50.0, 20.0, -1));
    v.layout(0.0, 0.0, &tr());
    assert_eq!(v.measured_size(), (66.0, 36.0)); // +16 两侧 padding
}

#[test]
fn fixed_size_overrides_content() {
    let mut v = View::container(Layout::Row)
        .fixed_w(200.0)
        .fixed_h(30.0)
        .child(fixed(999.0, 999.0, -1));
    v.layout(0.0, 0.0, &tr());
    assert_eq!(v.measured_size(), (200.0, 30.0));
}

/// 宽度下限：内容不足时抬到下限。
#[test]
fn min_w_raises_narrow_content() {
    let mut v = View::container(Layout::Row)
        .min_w(200.0)
        .child(fixed(50.0, 20.0, -1));
    v.layout(0.0, 0.0, &tr());
    assert_eq!(v.measured_size(), (200.0, 20.0));
}

/// 反向对照：内容够宽时下限不得改变尺寸，否则 `min_w` 就退化成了 `fixed_w`。
#[test]
fn min_w_leaves_wide_content_untouched() {
    let mut v = View::container(Layout::Row)
        .min_w(200.0)
        .child(fixed(300.0, 20.0, -1));
    v.layout(0.0, 0.0, &tr());
    assert_eq!(v.measured_size(), (300.0, 20.0));
}

/// 候选窗最小宽度的核心机制：下限设在叶子上，宽度逐层冒到祖先，父级 padding 照常累加
/// —— 调用方不必自己换算序号列宽与内边距。
#[test]
fn min_w_propagates_from_leaf_to_ancestor() {
    let mut v = View::container(Layout::Column).pad(Edges::all(8.0)).child(
        View::container(Layout::Row)
            .child(fixed(10.0, 20.0, -1))
            // fixed_w=30 且 min_w=120：下限在 fixed 之后施加，故取 120。
            .child(fixed(30.0, 20.0, -1).min_w(120.0)),
    );
    v.layout(0.0, 0.0, &tr());
    // 内层 Row = 10 + 120 = 130；外层再加两侧 padding 16。
    assert_eq!(v.measured_size().0, 146.0);
}

#[test]
fn leaf_text_measured_via_mock() {
    let mut v = View::leaf("abc", [0, 0, 0, 255]).font_size(20.0);
    v.layout(0.0, 0.0, &tr());
    // mock: 3 字符 × 20 × 0.6 = 36 宽；20 × 1.2 = 24 高
    assert_eq!(v.measured_size(), (36.0, 24.0));
}

#[test]
fn arrange_cross_center_offsets_child() {
    let mut root = View::container(Layout::Row)
        .cross(Align::Center)
        .fixed_h(40.0)
        .child(fixed(50.0, 20.0, 0));
    root.layout(0.0, 0.0, &tr());
    let r = hit(&root, 0);
    assert_eq!(r.x, 0.0);
    assert_eq!(r.y, 10.0); // (40-20)/2 居中
}

#[test]
fn arrange_grow_spacer_absorbs_remaining() {
    let mut root = View::container(Layout::Row)
        .fixed_w(200.0)
        .child(fixed(50.0, 10.0, 0))
        .child(View::spacer())
        .child(fixed(50.0, 10.0, 2));
    root.layout(0.0, 0.0, &tr());
    assert_eq!(hit(&root, 0).x, 0.0);
    assert_eq!(hit(&root, 2).x, 150.0); // spacer 吸收 200-100=100，把末项推到 150
}

/// 「左固定 + 中间自由 + 右固定」——**不需要新的布局原语**，现有两件就够：
/// 窄行加 `fill_cross` 撑到列的内容宽（= 最宽那行的宽度），内部 `spacer().grow()`
/// 吃掉富余，末项就恒贴右缘。
///
/// ★ 少了 `fill_cross`，行宽只等于自身内容宽，spacer 分不到一个像素，右组会跟着
/// 左组浮动——软键盘的关闭按钮与翻页键都栽在这里，而且**面板越宽错得越明显**，
/// 窄面板上看起来还挺对。
/// 命中区必须与**看得见的部分**一致，否则鼠标会在容器外的空白处「点到」
/// 一个不可见的控件。
#[test]
fn clip_trims_hits_to_the_visible_part() {
    let mut root = View::container(Layout::Row)
        .fixed_w(100.0)
        .fixed_h(20.0)
        .clipped()
        .child(fixed(60.0, 20.0, 0)) // 0..60 全露
        .child(fixed(60.0, 20.0, 1)) // 60..120，只露出 40
        .child(fixed(60.0, 20.0, 2)); // 120..180，全在框外
    root.layout(0.0, 0.0, &tr());
    assert_eq!(hit(&root, 0).w, 60.0, "完全可见的不受影响");
    assert_eq!(hit(&root, 1).w, 40.0, "跨边界的裁到露出的那段");
    let mut out = Vec::new();
    root.collect_hits(&mut out);
    assert!(
        !out.iter().any(|(t, _)| *t == 2),
        "完全在框外的控件不该进命中表"
    );
}

/// 裁剪的实现是「画完还原界外像素」，这条钉住它真的还原了。
#[test]
fn clip_keeps_paint_inside_the_box() {
    let mut root = View::container(Layout::Row)
        .fixed_w(10.0)
        .fixed_h(10.0)
        .clipped()
        .child(
            View::container(Layout::Row)
                .fixed_w(40.0)
                .fixed_h(10.0)
                .bg([200, 30, 30, 255]),
        );
    root.layout(0.0, 0.0, &tr());
    let (w, h) = (40u32, 10u32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    root.paint(&mut buf, w, h, &tr());
    let px = |x: u32, y: u32| -> [u8; 4] {
        let o = ((y * w + x) * 4) as usize;
        [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
    };
    assert_ne!(px(5, 5), [0, 0, 0, 0], "框内该被画到");
    assert_eq!(px(20, 5), [0, 0, 0, 0], "框外必须保持原样");
    assert_eq!(px(39, 5), [0, 0, 0, 0], "远处同样不能被涂到");
}

#[test]
fn fill_cross_row_lets_spacer_pin_the_last_child_right() {
    let mut root = View::container(Layout::Column)
        .child(fixed(200.0, 10.0, 9)) // 最宽的行，决定列宽
        .child(
            View::container(Layout::Row)
                .fill_cross()
                .child(fixed(20.0, 10.0, 0))
                .child(View::spacer().grow())
                .child(fixed(30.0, 10.0, 2)),
        );
    root.layout(0.0, 0.0, &tr());
    assert_eq!(hit(&root, 0).x, 0.0, "左组贴左缘");
    assert_eq!(hit(&root, 2).x, 170.0, "右组贴右缘 200-30");
}

#[test]
fn arrange_fill_cross_stretches_width() {
    let mut root = View::container(Layout::Column).fixed_w(100.0).child(
        View::container(Layout::Column)
            .fixed_h(20.0)
            .fill_cross()
            .tag(0),
    );
    root.layout(0.0, 0.0, &tr());
    assert_eq!(hit(&root, 0).w, 100.0); // 跨轴撑满列内容宽
}

#[test]
fn arrange_applies_child_margin() {
    let mut root = View::container(Layout::Row).child(fixed(10.0, 10.0, 0).margin(Edges {
        l: 5.0,
        t: 3.0,
        r: 0.0,
        b: 0.0,
    }));
    root.layout(0.0, 0.0, &tr());
    let r = hit(&root, 0);
    assert_eq!((r.x, r.y), (5.0, 3.0)); // margin 偏移子节点原点
}

/// 插入符是**覆盖层**：改变 caret 位置不得改变文本的布局尺寸。
/// 这是回归测试——曾把 preedit 拆成「前半+竖线+后半」三节点，因
/// `measure(a+b) != measure(a)+measure(b)` 且拆分点随光标而变，导致移动光标时编码位移。
#[test]
fn caret_never_affects_measured_size() {
    let tr = tr();
    let mut base = View::leaf("nihao", [0, 0, 0, 255]).font_size(20.0);
    base.layout(0.0, 0.0, &tr);
    let want = base.measured_size();
    for caret in 0..=5 {
        let mut v = View::leaf("nihao", [0, 0, 0, 255])
            .font_size(20.0)
            .caret_at(caret, 1.0);
        v.layout(0.0, 0.0, &tr);
        assert_eq!(
            v.measured_size(),
            want,
            "caret={caret} 不应改变文本布局尺寸"
        );
    }
}

/// caret_at 自夹到字符边界——paint 里 `&t[..cp]` 落在字符中间会 panic。
#[test]
fn caret_at_clamps_to_char_boundary() {
    // 「你」占 3 字节：1/2 退回 0，3 是边界；越界截到末尾（"你hao" = 6 字节）
    assert_eq!(
        View::leaf("你hao", [0; 4]).caret_at(1, 1.0).caret_at,
        Some(0)
    );
    assert_eq!(
        View::leaf("你hao", [0; 4]).caret_at(2, 1.0).caret_at,
        Some(0)
    );
    assert_eq!(
        View::leaf("你hao", [0; 4]).caret_at(3, 1.0).caret_at,
        Some(3)
    );
    assert_eq!(
        View::leaf("你hao", [0; 4]).caret_at(99, 1.0).caret_at,
        Some(6)
    );
}

#[test]
fn collect_hits_depth_first_skips_untagged() {
    let mut root = View::container(Layout::Column)
        .child(fixed(10.0, 10.0, 0))
        .child(fixed(10.0, 10.0, 1).child(fixed(5.0, 5.0, 2)));
    root.layout(0.0, 0.0, &tr());
    let mut out = Vec::new();
    root.collect_hits(&mut out);
    let tags: Vec<i32> = out.iter().map(|(t, _)| *t).collect();
    assert_eq!(tags, vec![0, 1, 2]); // 先序遍历；root 默认 tag=-1 不收集
}
