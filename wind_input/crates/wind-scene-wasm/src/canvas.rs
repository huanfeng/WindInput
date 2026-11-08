//! 浏览器 Canvas 文字后端：[`TextBackend`] 的测量与画字交给宿主（wasm 上是 `js/text.js` 的
//! OffscreenCanvas），回来的直通 RGBA 位图在这里按 source-over 合成进预乘 BGRA 缓冲。

use std::cell::RefCell;
use std::collections::HashMap;

use wind_scene::text::{ColorRun, TextBackend, TextMetrics, TextStyle, measure_key};

/// 一次画字的结果：直通 alpha 的 RGBA（`w × h × 4`），位图左上角相对文字盒左上角偏 `(x, y)`
/// （宿主四周留了边，字形伸出文字盒的部分不被裁掉）。
pub struct Glyphs {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// 宿主提供的画字能力。`font` 是 CSS `font` 简写（`700 16px "字族", 兜底…`）。
pub trait Canvas {
    /// `(宽, 行高)`；行高 = `fontBoundingBoxAscent + fontBoundingBoxDescent`。
    fn measure(&self, text: &str, font: &str) -> (f32, f32);
    /// 用 `rgba`（`[R, G, B, A]`）画整串文字；None = 宿主画不出。
    fn draw(&self, text: &str, font: &str, rgba: [u8; 4]) -> Option<Glyphs>;
}

/// 没设字体时的字族栈：与实机默认字体（Windows 的微软雅黑 UI）同一个优先，其余平台按常见中文
/// 字体兜底。
pub const DEFAULT_FAMILY: &str = "\"Microsoft YaHei UI\", \"Microsoft YaHei\", \"PingFang SC\", \
                                  \"Noto Sans CJK SC\", \"Noto Sans SC\", sans-serif";

/// 没设全局字重时的字重。
const DEFAULT_WEIGHT: i32 = 400;

/// 测量缓存条数上限，满了整表清空（同 wind-ui 各后端的做法）。
const MEASURE_CACHE_CAP: usize = 4096;

pub struct CanvasText<C> {
    canvas: C,
    /// 全局字族的 CSS 串（已含 [`DEFAULT_FAMILY`] 兜底）。
    family: String,
    /// 全局字重：`TextStyle::weight` 为 0（继承渲染器默认）时用它。
    weight: i32,
    base_size: f32,
    measures: RefCell<HashMap<u64, TextMetrics>>,
}

/// CSS 字符串字面量（字族名）：加引号，转义 `\` 与 `"`，控制字符换空格。
fn css_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.trim().chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl<C: Canvas> CanvasText<C> {
    pub fn new(canvas: C) -> Self {
        Self {
            canvas,
            family: DEFAULT_FAMILY.to_string(),
            weight: DEFAULT_WEIGHT,
            base_size: 16.0,
            measures: RefCell::default(),
        }
    }

    /// 全局字族（`ui.font.family`）；空 = [`DEFAULT_FAMILY`]。变了清测量缓存（键里的
    /// `family: None` 指的就是它）。
    pub fn set_family(&mut self, family: &str) {
        let css = if family.trim().is_empty() {
            DEFAULT_FAMILY.to_string()
        } else {
            format!("{}, {DEFAULT_FAMILY}", css_quote(family))
        };
        if css != self.family {
            self.family = css;
            self.measures.get_mut().clear();
        }
    }

    /// 全局字重（`ui.font.weight`）；0 = 常规。变了清测量缓存（键里的字重 0 指的就是它）。
    pub fn set_weight(&mut self, weight: i32) {
        let w = if weight > 0 { weight } else { DEFAULT_WEIGHT };
        if w != self.weight {
            self.weight = w;
            self.measures.get_mut().clear();
        }
    }

    /// `TextStyle` → CSS `font` 简写。
    pub fn font(&self, ts: &TextStyle) -> String {
        let weight = if ts.weight > 0 {
            ts.weight
        } else {
            self.weight
        };
        let size = if ts.size > 0.0 {
            ts.size
        } else {
            self.base_size
        };
        match ts.family {
            Some(f) => format!("{weight} {size}px {}, {}", css_quote(f), self.family),
            None => format!("{weight} {size}px {}", self.family),
        }
    }

    /// 在 `buf` 上画 `segs`：每段 `(颜色, 列起, 列止)` 各画一遍整串，只合成落在 `[列起, 列止)`
    /// 的像素——整串一次整形，颜色边界不让字距抖动（[`ColorRun`] 的约定）。
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        ox: i32,
        oy: i32,
        text: &str,
        font: &str,
        segs: &[([u8; 4], i32, i32)],
    ) -> Result<(), String> {
        for &(rgba, c0, c1) in segs {
            if rgba[3] == 0 || c0 >= c1 {
                continue;
            }
            let g = self
                .canvas
                .draw(text, font, rgba)
                .ok_or_else(|| "画字失败".to_string())?;
            composite(buf, buf_width, buf_height, ox + g.x, oy + g.y, &g, c0, c1)?;
        }
        Ok(())
    }
}

/// 直通 RGBA 位图按 source-over 合成进预乘 BGRA 缓冲，只动 `[c0, c1)` 列（缓冲坐标）。
#[allow(clippy::too_many_arguments)]
fn composite(
    buf: &mut [u8],
    bw: u32,
    bh: u32,
    x0: i32,
    y0: i32,
    g: &Glyphs,
    c0: i32,
    c1: i32,
) -> Result<(), String> {
    if g.rgba.len() != g.w as usize * g.h as usize * 4 {
        return Err(format!(
            "画字结果尺寸不符：{}×{} 却有 {} 字节",
            g.w,
            g.h,
            g.rgba.len()
        ));
    }
    if buf.len() < bw as usize * bh as usize * 4 {
        return Err("目标缓冲小于其宽高".to_string());
    }
    let gx0 = x0.max(0).max(c0);
    let gy0 = y0.max(0);
    let gx1 = x0.saturating_add(g.w as i32).min(bw as i32).min(c1);
    let gy1 = y0.saturating_add(g.h as i32).min(bh as i32);
    for y in gy0..gy1 {
        let src_row = (y - y0) as usize * g.w as usize;
        let dst_row = y as usize * bw as usize;
        for x in gx0..gx1 {
            let s = (src_row + (x - x0) as usize) * 4;
            let [r, gg, b, a] =
                [g.rgba[s], g.rgba[s + 1], g.rgba[s + 2], g.rgba[s + 3]].map(u32::from);
            if a == 0 {
                continue;
            }
            let inv = 255 - a;
            let d = &mut buf[(dst_row + x as usize) * 4..][..4];
            // BGRA 内存序；源色按 a 预乘，底色按 (1 - a) 衰减（同 wind-ui Linux 后端的 blit）。
            d[0] = ((b * a + d[0] as u32 * inv + 127) / 255) as u8;
            d[1] = ((gg * a + d[1] as u32 * inv + 127) / 255) as u8;
            d[2] = ((r * a + d[2] as u32 * inv + 127) / 255) as u8;
            d[3] = ((a * 255 + d[3] as u32 * inv + 127) / 255) as u8;
        }
    }
    Ok(())
}

/// 有效的颜色区间（落在字符边界、非空、不越界），按起点排序、去掉与前一段重叠的。
fn valid_runs(text: &str, runs: &[ColorRun]) -> Vec<ColorRun> {
    let mut v: Vec<ColorRun> = runs
        .iter()
        .copied()
        .filter(|r| {
            let (s, e) = (r.start as usize, r.end as usize);
            s < e && e <= text.len() && text.is_char_boundary(s) && text.is_char_boundary(e)
        })
        .collect();
    v.sort_by_key(|r| r.start);
    let mut end = 0;
    v.retain(|r| {
        let keep = r.start >= end;
        if keep {
            end = r.end;
        }
        keep
    });
    v
}

impl<C: Canvas> TextBackend for CanvasText<C> {
    fn base_size(&self) -> f32 {
        self.base_size
    }

    fn measure(&self, text: &str, ts: &TextStyle) -> TextMetrics {
        let key = measure_key(text, ts);
        if let Some(m) = self.measures.borrow().get(&key) {
            return m.clone();
        }
        let (width, height) = self.canvas.measure(text, &self.font(ts));
        let m = TextMetrics { width, height };
        let mut cache = self.measures.borrow_mut();
        if cache.len() >= MEASURE_CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, m.clone());
        m
    }

    fn draw(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        ts: &TextStyle,
        color: [u8; 4],
    ) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        let font = self.font(ts);
        let segs = [(color, i32::MIN, i32::MAX)];
        let (ox, oy) = (x.round() as i32, y.round() as i32);
        self.paint(buf, buf_width, buf_height, ox, oy, text, &font, &segs)
    }

    fn draw_runs(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        ts: &TextStyle,
        color: [u8; 4],
        runs: &[ColorRun],
    ) -> Result<(), String> {
        let runs = valid_runs(text, runs);
        if runs.is_empty() {
            return self.draw(buf, buf_width, buf_height, x, y, text, ts, color);
        }
        // 区间边界换成列：前缀宽度（整串排版里前缀与单量前缀在比例字体下一致到亚像素）。
        // 首段向左、末段向右不设限，伸出文字盒的字形不被切掉。
        let col = |b: usize| -> i32 {
            match b {
                0 => i32::MIN,
                b if b >= text.len() => i32::MAX,
                b => (x + self.measure(&text[..b], ts).width).round() as i32,
            }
        };
        let mut bounds: Vec<([u8; 4], usize, usize)> = Vec::new();
        let mut at = 0;
        for r in &runs {
            let (s, e) = (r.start as usize, r.end as usize);
            if s > at {
                bounds.push((color, at, s));
            }
            bounds.push((r.rgba, s, e));
            at = e;
        }
        if at < text.len() {
            bounds.push((color, at, text.len()));
        }
        let segs: Vec<_> = bounds
            .into_iter()
            .map(|(c, s, e)| (c, col(s), col(e)))
            .collect();
        let font = self.font(ts);
        let (ox, oy) = (x.round() as i32, y.round() as i32);
        self.paint(buf, buf_width, buf_height, ox, oy, text, &font, &segs)
    }
}

/// 测试用画布：宽 = 字符数 × 字号 × 0.6、行高 = 字号 × 1.2（同 wind-scene 的 `MockText`），
/// 画字出一块实心矩形（四周无留边），并记下每次调用的字体串与颜色。
#[cfg(test)]
pub(crate) mod mock {
    use super::{Canvas, Glyphs};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 一次画字调用：(文字, 字体串, 颜色)。
    pub(crate) type DrawCall = (String, String, [u8; 4]);

    #[derive(Clone, Default)]
    pub(crate) struct MockCanvas {
        pub(crate) measures: Rc<RefCell<Vec<String>>>,
        pub(crate) draws: Rc<RefCell<Vec<DrawCall>>>,
    }

    /// CSS 字体串里的字号。
    fn size_of(font: &str) -> f32 {
        font.split_whitespace()
            .find_map(|t| t.strip_suffix("px")?.parse().ok())
            .unwrap_or(16.0)
    }

    impl Canvas for MockCanvas {
        fn measure(&self, text: &str, font: &str) -> (f32, f32) {
            self.measures.borrow_mut().push(font.to_string());
            let s = size_of(font);
            (text.chars().count() as f32 * s * 0.6, s * 1.2)
        }

        fn draw(&self, text: &str, font: &str, rgba: [u8; 4]) -> Option<Glyphs> {
            self.draws
                .borrow_mut()
                .push((text.to_string(), font.to_string(), rgba));
            let s = size_of(font);
            let w = (text.chars().count() as f32 * s * 0.6).ceil() as u32;
            let h = (s * 1.2).ceil() as u32;
            Some(Glyphs {
                x: 0,
                y: 0,
                w,
                h,
                rgba: rgba.repeat((w * h) as usize),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockCanvas;
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

    /// 直通 RGBA 按 source-over 合成成预乘 BGRA：不透明直接覆盖，半透明与底色混合。
    #[test]
    fn draw_composites_straight_rgba_into_premultiplied_bgra() {
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
        // 越界不 panic。
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

    /// 分段色：每段整串画一遍、只合成自己那几列；测量仍是整串。
    #[test]
    fn draw_runs_colors_columns_by_prefix_width() {
        let (t, c) = backend();
        let (w, h) = (40, 12);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let ts = TextStyle::new(10.0); // 每字 6 列
        let runs = [ColorRun {
            start: 1,
            end: 2,
            rgba: [255, 0, 0, 255],
        }];
        t.draw_runs(
            &mut buf,
            w,
            h,
            0.0,
            0.0,
            "abc",
            &ts,
            [0, 0, 255, 255],
            &runs,
        )
        .unwrap();
        assert_eq!(bgra_at(&buf, w, 5, 0), [255, 0, 0, 255], "a 是基色蓝");
        assert_eq!(bgra_at(&buf, w, 6, 0), [0, 0, 255, 255], "b 是红");
        assert_eq!(bgra_at(&buf, w, 12, 0), [255, 0, 0, 255], "c 回到基色");
        let draws = c.draws.borrow();
        assert_eq!(draws.len(), 3);
        assert!(draws.iter().all(|(s, _, _)| s == "abc"), "每段都画整串");
        // 坏区间丢弃后退回单色画。
        drop(draws);
        let bad = [ColorRun {
            start: 1,
            end: 9,
            rgba: [1, 2, 3, 255],
        }];
        t.draw_runs(&mut buf, w, h, 0.0, 0.0, "abc", &ts, [0, 0, 255, 255], &bad)
            .unwrap();
        assert_eq!(c.draws.borrow().len(), 4);
    }

    #[test]
    fn valid_runs_drops_bad_and_overlapping() {
        let r = |s, e| ColorRun {
            start: s,
            end: e,
            rgba: [0; 4],
        };
        let got = valid_runs("你好ab", &[r(7, 8), r(0, 3), r(1, 3), r(2, 7), r(3, 3)]);
        assert_eq!(got, vec![r(0, 3), r(7, 8)]);
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
        assert!(composite(&mut buf, 2, 2, 0, 0, &g, i32::MIN, i32::MAX).is_err());
    }
}
