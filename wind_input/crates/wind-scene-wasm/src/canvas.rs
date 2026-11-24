//! 浏览器 Canvas 文字后端：[`TextBackend`] 的测量与画字交给宿主（wasm 上是 `js/text.js` 的
//! OffscreenCanvas）。
//!
//! 画字让宿主把**一行**文字用不透明白色栅格化（只栅格化落在缓冲里的那段横坐标），回来的位图在
//! 这里当覆盖率用，按所在区间的颜色着色。本行可能含彩色字形（emoji，见 [`may_have_color_glyph`]）
//! 时再用黑色画一遍对比：两遍颜色差得多的像素是覆盖率，相同的是彩色字形的本色——后者不随区间色
//! 着色、只乘区间色的不透明度，与 wind-ui Linux 后端对彩色字形的处理一致（emoji 里的白色也保持白）。
//! 然后 source-over 合成进预乘 BGRA 缓冲。
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
    /// 用不透明的灰度 `ink`（255 = 白、0 = 黑）画一行文字，只栅格化文字盒局部横坐标 `[x0, x1)`
    /// 那段。彩色字形不随 `ink` 变色。
    fn draw(&self, text: &str, font: &str, x0: i32, x1: i32, ink: u8) -> Result<Glyphs, String>;
}

/// 没设字体时的字族栈：与实机默认字体（Windows 的微软雅黑 UI）同一个优先，其余平台按常见中文
/// 字体兜底。
pub const DEFAULT_FAMILY: &str = "\"Microsoft YaHei UI\", \"Microsoft YaHei\", \"PingFang SC\", \
                                  \"Noto Sans CJK SC\", \"Noto Sans SC\", sans-serif";

/// 没设全局字重时的字重。
const DEFAULT_WEIGHT: i32 = 400;

/// 测量缓存条数上限，满了整表清空（同 wind-ui 各后端的做法）。
const MEASURE_CACHE_CAP: usize = 4096;

/// 白、黑两遍的同一像素，三个通道都至少差这么多才算覆盖率（彩色字形两遍相同，差 0）。
const INK_DIFF_MIN: u8 = 128;

/// 这一行可能含彩色字形（emoji）：只有这时才画第二遍（黑色）去分辨，纯中英文行省一次宿主调用。
/// 宁多勿漏——多判只是多画一遍，漏判则 emoji 被染成文字色。
pub(crate) fn may_have_color_glyph(line: &str) -> bool {
    line.chars().any(|c| {
        matches!(u32::from(c),
            0xA9 | 0xAE | 0x203C | 0x2049 | 0x2122 | 0x2139
            | 0x2190..=0x21FF   // 箭头（↔ ↩ 等有 emoji 呈现）
            | 0x2300..=0x23FF   // ⌚ ⏰ 等
            | 0x24C2 | 0x25A0..=0x27BF // 几何图形、杂项符号、装饰符号（☀ ✅ ❤）
            | 0x2900..=0x297F | 0x2B00..=0x2BFF
            | 0x3030 | 0x303D | 0x3297 | 0x3299
            | 0xFE0F | 0x200D | 0x20E3 // 呈现选择符、ZWJ、键帽
            | 0x1F000..=0x1FAFF // 麻将牌 … 符号与象形文字扩展
            | 0xE0020..=0xE007F) // 旗帜标签
    })
}

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
            let (x0, x1) = (-ox, (bw as i32).saturating_sub(ox));
            let draw = |ink| {
                self.canvas
                    .draw(line, &font, x0, x1, ink)
                    .map_err(|e| format!("画字失败：{e}"))
            };
            let g = draw(255)?;
            let dark = if may_have_color_glyph(line) {
                Some(draw(0)?)
            } else {
                None
            };
            composite(buf, bw, bh, ox + g.x, oy + g.y, &g, dark.as_ref(), &segs)?;
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

/// 白色位图按列区间着色、source-over 合成进预乘 BGRA 缓冲（缓冲坐标）。`dark` 是同一行用黑色
/// 画的那遍：有它时两遍颜色相同的像素是彩色字形本色；没有时全当覆盖率。
#[allow(clippy::too_many_arguments)]
fn composite(
    buf: &mut [u8],
    bw: u32,
    bh: u32,
    x0: i32,
    y0: i32,
    g: &Glyphs,
    dark: Option<&Glyphs>,
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
    if let Some(d) = dark
        && (d.x, d.y, d.w, d.h, d.rgba.len()) != (g.x, g.y, g.w, g.h, g.rgba.len())
    {
        return Err("黑白两遍画字的位图不一致".to_string());
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
                // 黑白两遍差得多 = 覆盖率，取区间色；相同 = 彩色字形的本色。
                let coverage = dark.is_none_or(|d| {
                    (0..3).all(|i| px[i].saturating_sub(d.rgba[s + i]) >= INK_DIFF_MIN)
                });
                let [r, gg, b] = if coverage {
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
#[path = "canvas_mock_tests.rs"]
pub(crate) mod mock;

#[cfg(test)]
#[path = "canvas_tests.rs"]
mod tests;
