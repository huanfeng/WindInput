use super::*;

/// `w × h` 的缓冲，`on(x, y)` 处不透明。
fn img(w: u32, h: u32, on: impl Fn(u32, u32) -> bool) -> Vec<u8> {
    let mut b = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            if on(x, y) {
                b[((y * w + x) * 4 + 3) as usize] = 1;
            }
        }
    }
    b
}

#[test]
fn identical_rows_merge_into_one_rect() {
    let b = img(10, 6, |x, y| (2..8).contains(&x) && (1..5).contains(&y));
    assert_eq!(opaque_rects(&b, 10, 6, 16), Some(vec![[2, 1, 6, 4]]));
}

#[test]
fn rounded_corner_rows_stay_separate() {
    // 第 0 行缩进一格（圆角），其余满宽；右侧另有一块两行高的装饰。
    let b = img(8, 4, |x, y| {
        (y == 0 && (1..5).contains(&x)) || (y > 0 && x < 6) || (x == 7 && y < 2)
    });
    assert_eq!(
        opaque_rects(&b, 8, 4, 16),
        Some(vec![
            [1, 0, 4, 1],
            [7, 0, 1, 1],
            [0, 1, 6, 1],
            [7, 1, 1, 1],
            [0, 2, 6, 2]
        ])
    );
}

#[test]
fn transparent_buffer_has_no_rects() {
    assert_eq!(
        opaque_rects(&img(5, 5, |_, _| false), 5, 5, 4),
        Some(vec![])
    );
}

#[test]
fn too_many_rects_gives_up() {
    // 棋盘格：每行 3 段、行行不同。
    let b = img(6, 4, |x, y| (x + y) % 2 == 0);
    assert_eq!(opaque_rects(&b, 6, 4, 12).map(|v| v.len()), Some(12));
    assert_eq!(opaque_rects(&b, 6, 4, 11), None);
}

#[test]
fn premultiplied_over_and_under() {
    let half = [0u8, 0, 100, 128];
    let solid = [200u8, 0, 0, 255];
    let mut d = solid.to_vec();
    composite_over(&mut d, &half);
    assert_eq!(d, vec![100, 0, 100, 255]);
    let mut d = half.to_vec();
    composite_under(&mut d, &solid);
    assert_eq!(d, vec![100, 0, 100, 255]);
    // 透明的上层不改下层；下层透明时结果就是上层。
    let mut d = solid.to_vec();
    composite_over(&mut d, &[0, 0, 0, 0]);
    assert_eq!(d, solid.to_vec());
    let mut d = half.to_vec();
    composite_under(&mut d, &[0, 0, 0, 0]);
    assert_eq!(d, half.to_vec());
}
