//! 外部宿主（macOS `.app` / Linux addon）要的皮肤区域：装饰矩形与鼠标策略、输入区。
//! 协议见 `wind-ipc` 的 `CMD_CANDIDATE_SKIN`，设计见 theme-skin-animation.md §17.2。

use wind_theme::skin::DecorHit;

/// 输入区矩形上限：超了退化为「窗体 ∪ 非穿透装饰」的矩形并集（§17.2）。
pub const MAX_INPUT_RECTS: usize = 1024;

/// 一块装饰在缓冲里的矩形与鼠标策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostDecor {
    /// 变体 `decor` 里的下标（与装饰状态机同号）。
    pub index: usize,
    pub hit: DecorHit,
    /// 在窗体下面（z < 0）。
    pub under: bool,
    /// 缓冲内 `x, y, w, h`（设备像素）。
    pub rect: [i32; 4],
}

/// `alpha > 0` 的像素合成矩形：逐行取不透明游程，与上一行游程完全相同的并进上一行的矩形。
/// 超过 `max` 个返回 `None`（调用方退化）。`bgra` 是 `w × h` 的紧凑 BGRA。
pub fn opaque_rects(bgra: &[u8], w: u32, h: u32, max: usize) -> Option<Vec<[i32; 4]>> {
    let (w, h) = (w as usize, h as usize);
    let mut out: Vec<[i32; 4]> = Vec::new();
    // 上一行的游程，以及它们各自在 `out` 里的下标（纵向合并时把高度加一）。
    let mut prev: Vec<(i32, i32)> = Vec::new();
    let mut prev_idx: Vec<usize> = Vec::new();
    let mut runs: Vec<(i32, i32)> = Vec::new();
    for y in 0..h {
        runs.clear();
        let row = &bgra[y * w * 4..(y + 1) * w * 4];
        let mut x = 0;
        while x < w {
            while x < w && row[x * 4 + 3] == 0 {
                x += 1;
            }
            let start = x;
            while x < w && row[x * 4 + 3] != 0 {
                x += 1;
            }
            if x > start {
                runs.push((start as i32, x as i32));
            }
        }
        if runs == prev {
            for &i in &prev_idx {
                out[i][3] += 1;
            }
            continue;
        }
        prev_idx.clear();
        for &(a, b) in &runs {
            if out.len() == max {
                return None;
            }
            prev_idx.push(out.len());
            out.push([a, y as i32, b - a, 1]);
        }
        std::mem::swap(&mut prev, &mut runs);
    }
    Some(out)
}

/// 预乘 alpha 的 over：`top` 盖在 `dst` 上，结果写回 `dst`。
pub fn composite_over(dst: &mut [u8], top: &[u8]) {
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(top.as_chunks::<4>().0)
    {
        blend(d, s);
    }
}

/// 预乘 alpha 的 over：`under` 垫在 `dst` 下面，结果写回 `dst`。
pub fn composite_under(dst: &mut [u8], under: &[u8]) {
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(under.as_chunks::<4>().0)
    {
        let top = *d;
        *d = *s;
        blend(d, &top);
    }
}

fn blend(d: &mut [u8; 4], s: &[u8; 4]) {
    match s[3] {
        0 => {}
        255 => *d = *s,
        a => {
            let inv = 255 - a as u32;
            for c in 0..4 {
                d[c] = (s[c] as u32 + (d[c] as u32 * inv + 127) / 255).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
}
