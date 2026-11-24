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
#[path = "host_regions_tests.rs"]
mod tests;
