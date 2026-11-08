//! 浏览器 Canvas 文字后端：[`TextBackend`] 的测量与画字交给宿主（wasm 上是 `js/text.js` 的
//! OffscreenCanvas）。
//!
//! 画字只让宿主把**一行**文字用不透明白色栅格化一次（只栅格化落在缓冲里的那段横坐标），回来的
//! 位图在这里当覆盖率用：近白像素按所在区间的颜色着色，非白像素是彩色字形（emoji）的本色，只乘
//! 区间颜色的不透明度——与 wind-ui Linux 后端对彩色字形的处理一致。然后 source-over 合成进预乘
//! BGRA 缓冲。
//!
//! 与 Linux 后端对齐的口径：空串行高 = 字号 × 1.2；`\n` 分行（宽取最宽一行、高 = 行数 × 行高）；
//! 颜色区间重叠时后写的赢；字重钳到 1..=1000。

use std::cell::RefCell;
use std::collections::HashMap;

use wind_scene::text::{ColorRun, TextBackend, TextMetrics, TextStyle, measure_key};

/// 宿主画出的一行文字：直通 alpha 的 RGBA（`w × h × 4`），位图左上角在文字盒局部坐标 `(x, y)`。
pub struct Glyphs {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// 宿主提供的画字能力。`font` 是 CSS `font` 简写（`700 16px "字族", 兜底…`），`text` 不含换行。
pub trait Canvas {
    /// `(宽, 行高)`；行高 = `fontBoundingBoxAscent + fontBoundingBoxDescent`。
    fn measure(&self, text: &str, font: &str) -> Result<(f32, f32), String>;
    /// 用不透明白色画一行文字，只栅格化文字盒局部横坐标 `[x0, x1)` 那段。
    fn draw(&self, text: &str, font: &str, x0: i32, x1: i32) -> Result<Glyphs, String>;
}

/// 没设字体时的字族栈：与实机默认字体（Windows 的微软雅黑 UI）同一个优先，其余平台按常见中文
/// 字体兜底。
pub const DEFAULT_FAMILY: &str = "\"Microsoft YaHei UI\", \"Microsoft YaHei\", \"PingFang SC\", \
                                  \"Noto Sans CJK SC\", \"Noto Sans SC\", sans-serif";

/// 没设全局字重时的字重。
const DEFAULT_WEIGHT: i32 = 400;

/// 测量缓存条数上限，满了整表清空（同 wind-ui 各后端的做法）。
const MEASURE_CACHE_CAP: usize = 4096;

/// 回读像素三个通道都不低于它就当作白色覆盖率（getImageData 反预乘有舍入，低 alpha 处不是正 255）。
const WHITE_MIN: u8 = 250;

pub struct CanvasText<C> {
    canvas: C,
    /// 全局字族的 CSS 串（已含 [`DEFAULT_FAMILY`] 兜底）。
    family: String,
    /// 全局字重：`TextStyle::weight` 为 0（继承渲染器默认）时用它。
    weight: i32,
    base_size: f32,
    measures: RefCell<HashMap<u64, TextMetrics>>,
    /// 宿主调用失败的第一条原因（`measure` 没有返回错误的位置，先记下，由调用方在一帧后取走）。
    error: RefCell<Option<String>>,
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

/// 有效区间（落在字符边界、非空、不越界），保持原顺序（重叠时后写的赢）。
fn valid_runs<'a>(text: &str, runs: &'a [ColorRun]) -> Vec<&'a ColorRun> {
    runs.iter()
        .filter(|r| {
            let (s, e) = (r.start as usize, r.end as usize);
            s < e && e <= text.len() && text.is_char_boundary(s) && text.is_char_boundary(e)
        })
        .collect()
}

/// 把整串切成同色的字节区间 `(起, 止, 颜色)`：未被覆盖的是基色，重叠处取最后一个覆盖它的区间。
fn color_spans(text: &str, runs: &[&ColorRun], base: [u8; 4]) -> Vec<(usize, usize, [u8; 4])> {
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for r in runs {
        cuts.push(r.start as usize);
        cuts.push(r.end as usize);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<(usize, usize, [u8; 4])> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let c = runs
            .iter()
            .rev()
            .find(|r| r.start as usize <= a && b <= r.end as usize)
            .map_or(base, |r| r.rgba);
        match out.last_mut() {
            Some(last) if last.2 == c && last.1 == a => last.1 = b,
            _ => out.push((a, b, c)),
        }
    }
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
            error: RefCell::default(),
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
        let w = if weight > 0 {
            weight.clamp(1, 1000)
        } else {
            DEFAULT_WEIGHT
        };
        if w != self.weight {
            self.weight = w;
            self.measures.get_mut().clear();
        }
    }

    /// 取走宿主调用失败的原因（没有失败为 None）。
    pub fn take_error(&self) -> Option<String> {
        self.error.borrow_mut().take()
    }

    fn fail(&self, e: String) {
        self.error.borrow_mut().get_or_insert(e);
    }

    /// 生效字号：有限且为正，否则基准字号（CSS 里写不合法的字号，`ctx.font` 会静默保留旧值）。
    fn size_of(&self, ts: &TextStyle) -> f32 {
        if ts.size.is_finite() && ts.size > 0.0 {
            ts.size
        } else {
            self.base_size
        }
    }

    /// `TextStyle` → CSS `font` 简写。
    pub fn font(&self, ts: &TextStyle) -> String {
        let weight = if ts.weight > 0 {
            ts.weight.clamp(1, 1000)
        } else {
            self.weight
        };
        let size = self.size_of(ts);
        match ts.family {
            Some(f) => format!("{weight} {size}px {}, {}", css_quote(f), self.family),
            None => format!("{weight} {size}px {}", self.family),
        }
    }

    /// 一行（不含 `\n`）的度量，缓存。宿主失败时记下错误、按 `字数 × 字号 × 0.6` 估。
    fn measure_line(&self, line: &str, ts: &TextStyle) -> TextMetrics {
        let key = measure_key(line, ts);
        if let Some(m) = self.measures.borrow().get(&key) {
            return m.clone();
        }
        let size = self.size_of(ts);
        let m = match self.canvas.measure(line, &self.font(ts)) {
            Ok((width, height)) if width.is_finite() && height.is_finite() => TextMetrics {
                width: width.max(0.0),
                height: if height > 0.0 { height } else { size * 1.2 },
            },
            Ok((w, h)) => {
                self.fail(format!("测量结果不是有限数：{w} × {h}"));
                return TextMetrics {
                    width: 0.0,
                    height: size * 1.2,
                };
            }
            Err(e) => {
                self.fail(format!("测量文字失败：{e}"));
                return TextMetrics {
                    width: line.chars().count() as f32 * size * 0.6,
                    height: size * 1.2,
                };
            }
        };
        let mut cache = self.measures.borrow_mut();
        if cache.len() >= MEASURE_CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, m.clone());
        m
    }

    /// 逐行画，`spans` 是整串的同色字节区间。
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        buf: &mut [u8],
        bw: u32,
        bh: u32,
        x: f32,
        y: f32,
        text: &str,
        ts: &TextStyle,
        spans: &[(usize, usize, [u8; 4])],
    ) -> Result<(), String> {
        let need = u64::from(bw) * u64::from(bh) * 4;
        if (buf.len() as u64) < need {
            return Err("目标缓冲小于其宽高".to_string());
        }
        let font = self.font(ts);
        let ox = x.round() as i32;
        let mut line_y = y;
        let mut off = 0;
        for line in text.split('\n') {
            let (start, end) = (off, off + line.len());
            off = end + 1;
            let lm = self.measure_line(line, ts);
            let oy = line_y.round() as i32;
            line_y += lm.height;
            if line.is_empty() || oy >= bh as i32 || oy.saturating_add(lm.height.ceil() as i32) < 0
            {
                continue;
            }
            // 本行里的同色区间，边界换成缓冲列：前缀宽度；行首向左、行尾向右不设限（伸出文字盒
            // 的字形不被切掉）。
            let col = |b: usize| -> i32 {
                match b - start {
                    0 => i32::MIN,
                    l if l >= line.len() => i32::MAX,
                    l => (x + self.measure_line(&line[..l], ts).width).round() as i32,
                }
            };
            let segs: Vec<(i32, i32, [u8; 4])> = spans
                .iter()
                .filter(|(s, e, c)| *s < end && *e > start && c[3] > 0)
                .map(|&(s, e, c)| (col(s.max(start)), col(e.min(end)), c))
                .filter(|(a, b, _)| a < b)
                .collect();
            if segs.is_empty() {
                continue;
            }
            // 只栅格化落在缓冲里的那段（文字盒局部坐标）。
            let g = self
                .canvas
                .draw(line, &font, -ox, (bw as i32).saturating_sub(ox))
                .map_err(|e| format!("画字失败：{e}"))?;
            composite(buf, bw, bh, ox + g.x, oy + g.y, &g, &segs)?;
        }
        Ok(())
    }

    fn paint_or_record(&self, r: Result<(), String>) -> Result<(), String> {
        if let Err(e) = &r {
            self.fail(e.clone());
        }
        r
    }
}

/// 白色覆盖率位图按列区间着色、source-over 合成进预乘 BGRA 缓冲（缓冲坐标）。
fn composite(
    buf: &mut [u8],
    bw: u32,
    bh: u32,
    x0: i32,
    y0: i32,
    g: &Glyphs,
    segs: &[(i32, i32, [u8; 4])],
) -> Result<(), String> {
    if g.rgba.len() as u64 != u64::from(g.w) * u64::from(g.h) * 4 {
        return Err(format!(
            "画字结果尺寸不符：{}×{} 却有 {} 字节",
            g.w,
            g.h,
            g.rgba.len()
        ));
    }
    let gx1 = x0.saturating_add(g.w.min(i32::MAX as u32) as i32);
    let gy1 = y0.saturating_add(g.h.min(i32::MAX as u32) as i32);
    let (gy0, gy1) = (y0.max(0), gy1.min(bh as i32));
    for &(c0, c1, [cr, cg, cb, ca]) in segs {
        let (xa, xb) = (x0.max(0).max(c0), gx1.min(bw as i32).min(c1));
        let ca = u32::from(ca);
        for y in gy0..gy1 {
            let src_row = (y - y0) as usize * g.w as usize;
            let dst_row = y as usize * bw as usize;
            for x in xa..xb {
                let s = (src_row + (x - x0) as usize) * 4;
                let px = &g.rgba[s..s + 4];
                let sa = (u32::from(px[3]) * ca + 127) / 255;
                if sa == 0 {
                    continue;
                }
                // 近白 = 覆盖率，取区间色；否则是彩色字形的本色。
                let [r, gg, b] = if px[..3].iter().all(|&v| v >= WHITE_MIN) {
                    [cr, cg, cb]
                } else {
                    [px[0], px[1], px[2]]
                }
                .map(u32::from);
                let inv = 255 - sa;
                let d = &mut buf[(dst_row + x as usize) * 4..][..4];
                // BGRA 内存序；源色按 sa 预乘，底色按 (1 - sa) 衰减（同 wind-ui Linux 后端的 blit）。
                d[0] = ((b * sa + u32::from(d[0]) * inv + 127) / 255) as u8;
                d[1] = ((gg * sa + u32::from(d[1]) * inv + 127) / 255) as u8;
                d[2] = ((r * sa + u32::from(d[2]) * inv + 127) / 255) as u8;
                d[3] = ((sa * 255 + u32::from(d[3]) * inv + 127) / 255) as u8;
            }
        }
    }
    Ok(())
}

impl<C: Canvas> TextBackend for CanvasText<C> {
    fn base_size(&self) -> f32 {
        self.base_size
    }

    fn measure(&self, text: &str, ts: &TextStyle) -> TextMetrics {
        if text.is_empty() {
            return TextMetrics {
                width: 0.0,
                height: self.size_of(ts) * 1.2,
            };
        }
        if !text.contains('\n') {
            return self.measure_line(text, ts);
        }
        let (mut width, mut height) = (0.0f32, 0.0f32);
        for line in text.split('\n') {
            let m = self.measure_line(line, ts);
            width = width.max(m.width);
            height += m.height;
        }
        TextMetrics { width, height }
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
        let spans = [(0, text.len(), color)];
        let r = self.paint(buf, buf_width, buf_height, x, y, text, ts, &spans);
        self.paint_or_record(r)
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
        let spans = color_spans(text, &valid_runs(text, runs), color);
        let r = self.paint(buf, buf_width, buf_height, x, y, text, ts, &spans);
        self.paint_or_record(r)
    }
}

/// 测试用画布：宽 = 字符数 × 字号 × 0.6、行高 = 字号 × 1.2（同 wind-scene 的 `MockText`），
/// 画字出一块白色实心矩形（`😀` 那一格画成橙色，模拟彩色字形），按请求的横坐标段裁；记下每次
/// 调用。`fail` 置位时两种调用都报错（模拟 JS 抛异常）。
#[cfg(test)]
pub(crate) mod mock {
    use super::{Canvas, Glyphs};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// 一次画字调用：(文字, 字体串, 横坐标段)。
    pub(crate) type DrawCall = (String, String, (i32, i32));

    pub(crate) const EMOJI_RGB: [u8; 3] = [255, 160, 0];

    #[derive(Clone, Default)]
    pub(crate) struct MockCanvas {
        pub(crate) measures: Rc<RefCell<Vec<String>>>,
        pub(crate) draws: Rc<RefCell<Vec<DrawCall>>>,
        pub(crate) fail: Rc<Cell<bool>>,
    }

    /// CSS 字体串里的字号。
    fn size_of(font: &str) -> f32 {
        font.split_whitespace()
            .find_map(|t| t.strip_suffix("px")?.parse().ok())
            .unwrap_or(16.0)
    }

    impl Canvas for MockCanvas {
        fn measure(&self, text: &str, font: &str) -> Result<(f32, f32), String> {
            if self.fail.get() {
                return Err("mock measure 抛异常".into());
            }
            self.measures.borrow_mut().push(font.to_string());
            let s = size_of(font);
            Ok((text.chars().count() as f32 * s * 0.6, s * 1.2))
        }

        fn draw(&self, text: &str, font: &str, x0: i32, x1: i32) -> Result<Glyphs, String> {
            if self.fail.get() {
                return Err("mock draw 抛异常".into());
            }
            self.draws
                .borrow_mut()
                .push((text.to_string(), font.to_string(), (x0, x1)));
            let s = size_of(font);
            let cell = s * 0.6;
            let full = (text.chars().count() as f32 * cell).ceil() as i32;
            let (a, b) = (x0.max(0), x1.min(full));
            let (w, h) = ((b - a).max(0) as u32, (s * 1.2).ceil() as u32);
            let chars: Vec<char> = text.chars().collect();
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for _ in 0..h {
                for x in a..a + w as i32 {
                    let c = chars.get((x as f32 / cell) as usize).copied();
                    match c {
                        Some('😀') => {
                            rgba.extend_from_slice(&[EMOJI_RGB[0], EMOJI_RGB[1], EMOJI_RGB[2], 255])
                        }
                        _ => rgba.extend_from_slice(&[255, 255, 255, 255]),
                    }
                }
            }
            Ok(Glyphs {
                x: a,
                y: 0,
                w,
                h,
                rgba,
            })
        }
    }
}

#[cfg(test)]
mod tests {
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

    /// 彩色字形（emoji）保持本色，只乘区间颜色的不透明度。
    #[test]
    fn color_glyph_keeps_its_own_color() {
        let (t, _) = backend();
        let (w, h) = (20, 12);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let runs = [ColorRun {
            start: 0,
            end: 5,
            rgba: [0, 0, 255, 255],
        }];
        t.draw_runs(
            &mut buf,
            w,
            h,
            0.0,
            0.0,
            "a😀",
            &TextStyle::new(10.0),
            [9, 9, 9, 255],
            &runs,
        )
        .unwrap();
        let [r, g, b] = EMOJI_RGB;
        assert_eq!(bgra_at(&buf, w, 7, 0), [b, g, r, 255]);
        assert_eq!(bgra_at(&buf, w, 1, 0), [255, 0, 0, 255], "普通字按区间色");
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
        assert!(composite(&mut buf, 2, 2, 0, 0, &g, &[(i32::MIN, i32::MAX, [1; 4])]).is_err());
    }
}
