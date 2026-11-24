use super::*;
use crate::text::MockText as TextRenderer;

fn tr() -> TextRenderer {
    TextRenderer::new("test", 20.0).unwrap()
}

fn fixed(w: f32, h: f32, tag: i32) -> View {
    View::container(Layout::Row).fixed_w(w).fixed_h(h).tag(tag)
}

fn hit(root: &View, tag: i32) -> Rect {
    let mut out = Vec::new();
    root.collect_hits(&mut out);
    out.into_iter()
        .find(|(t, _)| *t == tag)
        .unwrap_or_else(|| panic!("tag {tag} 未命中"))
        .1
}

/// 旋转节点对外交换宽高——整个旋转能力的支点：子树仍在未旋转空间里排版。
#[test]
fn rotation_swaps_the_measured_size() {
    let mut root = View::rotated_cw(fixed(40.0, 10.0, 0));
    root.layout(0.0, 0.0, &tr());
    assert_eq!(root.measured_size(), (10.0, 40.0));
}

/// 方向判据：局部左上角必须落到屏幕右上角。落到左下角就是逆时针，
/// 蒙古文会变成自下而上读。
#[test]
fn local_top_left_maps_to_screen_top_right() {
    let host = Rect {
        x: 100.0,
        y: 200.0,
        w: 10.0,
        h: 40.0,
    };
    // 局部 (0,0) 处 2×3 的小块。
    let got = rotate_rect(
        Rot::Cw,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 2.0,
            h: 3.0,
        },
        host,
    );
    assert_eq!(got.x, 100.0 + 10.0 - 3.0, "没贴到右缘 ⇒ 转反了");
    assert_eq!(got.y, 200.0);
    assert_eq!((got.w, got.h), (3.0, 2.0), "宽高没交换");
}

/// 命中矩形必须跟着转，漏掉时画面正常、只有鼠标点错候选。反向对照：不带旋转时矩形不得被映射。
#[test]
fn hit_rects_follow_the_rotation() {
    let inner = View::container(Layout::Column)
        .child(fixed(40.0, 10.0, 1))
        .child(fixed(40.0, 10.0, 2));
    let mut root = View::rotated_cw(inner);
    root.layout(0.0, 0.0, &tr());
    // 局部：1 在上 (y=0)、2 在下 (y=10)；屏幕宽 20（=局部高）。
    // 顺时针后：1 贴右（x=10）、2 在左（x=0）。
    assert_eq!(hit(&root, 1).x, 10.0);
    assert_eq!(hit(&root, 2).x, 0.0);
    assert_eq!(hit(&root, 1).w, 10.0, "屏幕宽应为局部高");
    assert_eq!(hit(&root, 1).h, 40.0, "屏幕高应为局部宽");

    let mut plain = View::container(Layout::Column)
        .child(fixed(40.0, 10.0, 1))
        .child(fixed(40.0, 10.0, 2));
    plain.layout(0.0, 0.0, &tr());
    assert_eq!(hit(&plain, 1).y, 0.0, "无旋转时不得映射");
    assert_eq!(hit(&plain, 2).y, 10.0);
}

/// 像素方向：局部缓冲左上角那一点，搬到屏幕后必须在右上角。与
/// `local_top_left_maps_to_screen_top_right` 是同一约定的矩形 / 像素两面，必须一起钉。
#[test]
fn pixel_blit_puts_local_origin_at_screen_top_right() {
    let (cw, ch) = (4u32, 3u32); // 局部 4×3 ⇒ 屏幕 3×4
    let mut tmp = vec![0u8; (cw * ch * 4) as usize];
    tmp[0..4].copy_from_slice(&[1, 2, 3, 4]); // 局部 (0,0)
    let (bw, bh) = (3u32, 4u32);
    let mut buf = vec![0u8; (bw * bh * 4) as usize];
    blit_rotate(Rot::Cw, &tmp, cw, ch, &mut buf, bw, bh, 0, 0);
    let top_right = ((bw - 1) * 4) as usize; // 第 0 行、最后一列
    assert_eq!(&buf[top_right..top_right + 4], &[1, 2, 3, 4]);
    assert_eq!(&buf[0..4], &[0, 0, 0, 0], "左上角不该有东西");
}

/// 搬进来再搬回去必须是恒等——底子（窗口圆角/渐变/背景图）要原样保留，
/// 且文本后端的差分法合成拿它当抗锯齿的背景基准，错一位文字边缘就带错误混色。
#[test]
fn blit_round_trip_is_identity() {
    let (cw, ch) = (5u32, 7u32);
    let (bw, bh) = (ch, cw);
    let orig: Vec<u8> = (0..(bw * bh * 4)).map(|i| (i % 251) as u8).collect();
    let mut buf = orig.clone();
    let mut tmp = vec![0u8; (cw * ch * 4) as usize];
    blit_unrotate(Rot::Cw, &buf, bw, bh, 0, 0, &mut tmp, cw, ch);
    blit_rotate(Rot::Cw, &tmp, cw, ch, &mut buf, bw, bh, 0, 0);
    assert_eq!(buf, orig, "往返不是恒等：底子会被搬歪");
}

/// 逆时针的方向判据：局部左上角落到屏幕左下角。只测宽高交换的话，把 Ccw 实现成顺时针
/// 也能全绿，对联式竖排的每个字会转 180°。
#[test]
fn ccw_puts_local_origin_at_screen_bottom_left() {
    let host = Rect {
        x: 100.0,
        y: 200.0,
        w: 10.0,
        h: 40.0,
    };
    let got = rotate_rect(
        Rot::Ccw,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 2.0,
            h: 3.0,
        },
        host,
    );
    assert_eq!(got.x, 100.0, "没贴到左缘 ⇒ 转反了");
    assert_eq!(got.y, 200.0 + 40.0 - 2.0, "没贴到下缘 ⇒ 转反了");
    assert_eq!((got.w, got.h), (3.0, 2.0), "宽高没交换");

    // 像素层同一约定（连续/离散两处最容易错开一格）。
    let (cw, ch) = (4u32, 3u32); // 局部 4×3 ⇒ 屏幕 3×4
    let mut tmp = vec![0u8; (cw * ch * 4) as usize];
    tmp[0..4].copy_from_slice(&[1, 2, 3, 4]); // 局部 (0,0)
    let (bw, bh) = (3u32, 4u32);
    let mut buf = vec![0u8; (bw * bh * 4) as usize];
    blit_rotate(Rot::Ccw, &tmp, cw, ch, &mut buf, bw, bh, 0, 0);
    let bottom_left = ((bh - 1) * bw * 4) as usize;
    assert_eq!(&buf[bottom_left..bottom_left + 4], &[1, 2, 3, 4]);
    assert_eq!(&buf[0..4], &[0, 0, 0, 0], "左上角不该有东西");
}

/// 对联式竖排的数学：外层顺时针套内层逆时针 ≡ 什么都没做。同时钉住两向映射互为逆、
/// 嵌套时内层拿到的底子正确、宽高交换两次回到原值。用左右异色的两块，单色转 180° 测不出方向。
#[test]
fn cw_around_ccw_is_the_identity() {
    const RED: [u8; 4] = [0, 0, 255, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];
    let leaf = || {
        View::container(Layout::Row)
            .child(
                View::container(Layout::Row)
                    .fixed_w(2.0)
                    .fixed_h(2.0)
                    .bg(RED),
            )
            .child(
                View::container(Layout::Row)
                    .fixed_w(2.0)
                    .fixed_h(2.0)
                    .bg(BLUE),
            )
    };
    let (bw, bh) = (4u32, 2u32);

    let mut plain = leaf();
    plain.layout(0.0, 0.0, &tr());
    assert_eq!(plain.measured_size(), (4.0, 2.0));
    let mut direct = vec![0u8; (bw * bh * 4) as usize];
    plain.paint(&mut direct, bw, bh, &tr());

    let mut nested = View::rotated_cw(View::rotated_ccw(leaf()));
    nested.layout(0.0, 0.0, &tr());
    assert_eq!(nested.measured_size(), (4.0, 2.0), "两次交换应回到原尺寸");
    let mut got = vec![0u8; (bw * bh * 4) as usize];
    nested.paint(&mut got, bw, bh, &tr());

    assert_eq!(got, direct, "cw∘ccw 不是恒等 ⇒ 竖排的字会歪或倒");
}

/// 嵌套下的命中矩形也必须还原——画面对而点击错是这类 bug 的典型形态。
#[test]
fn cw_around_ccw_restores_hit_rects() {
    let build = || {
        View::container(Layout::Row)
            .child(fixed(4.0, 2.0, 1))
            .child(fixed(6.0, 2.0, 2))
    };
    let mut plain = build();
    plain.layout(0.0, 0.0, &tr());
    let mut nested = View::rotated_cw(View::rotated_ccw(build()));
    nested.layout(0.0, 0.0, &tr());
    for tag in [1, 2] {
        let (n, p) = (hit(&nested, tag), hit(&plain, tag));
        assert_eq!(
            (n.x, n.y, n.w, n.h),
            (p.x, p.y, p.w, p.h),
            "tag {tag} 的命中区没还原"
        );
    }
}

/// 旋转块部分落在缓冲外时不得 panic，也不得写坏界内像素。
#[test]
fn out_of_bounds_blit_is_clipped_not_panicking() {
    let (cw, ch) = (4u32, 4u32);
    let tmp = vec![9u8; (cw * ch * 4) as usize];
    let (bw, bh) = (4u32, 4u32);
    let mut buf = vec![0u8; (bw * bh * 4) as usize];
    // 原点在负象限：只有右下角一部分落在界内。
    blit_rotate(Rot::Cw, &tmp, cw, ch, &mut buf, bw, bh, -2, -2);
    assert_eq!(&buf[0..4], &[9, 9, 9, 9], "界内部分应被写到");
    // 完全在界外：一个字节都不该动。
    let mut buf2 = vec![0u8; (bw * bh * 4) as usize];
    blit_rotate(Rot::Cw, &tmp, cw, ch, &mut buf2, bw, bh, 100, 100);
    assert!(buf2.iter().all(|&b| b == 0));
}
