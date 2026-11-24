//! 候选窗定位调试浮窗：把宿主上报的几何与协调器算出的锚点**叠在屏幕上画出来**。
//!
//! ★ 为什么需要它：定位缺陷本质是**空间**问题——「宿主说组合占了这块区域」与「候选窗
//! 画在了那个点」对不对得上。日志只能给出离散数值，人得在脑子里把它们还原成屏幕上的
//! 位置关系；宿主一多（QQ / 记事本 / 飞书 / WPS 文字 / WPS 表格 / Word / Excel）、操作
//! 序列一长（打字、换行、逐字删除交替），这个还原就不可靠了——2026-09-05 那轮排查里，
//! 几千条帧混在一起、要靠时间戳反推用户当时在做什么，已经到了方法的极限。
//!
//! 直接画出来，错位一眼可见：矩形框在哪、caret 在哪、锚点落在哪，是否对齐无需推理。
//!
//! 复用 [`crate::window::LayeredWindow`]（自带 `WS_EX_NOACTIVATE | WS_EX_TOPMOST |
//! `WS_EX_TOOLWINDOW | WS_EX_LAYERED`），另加 `WS_EX_TRANSPARENT` 让鼠标穿透——
//! 它盖在宿主上方，绝不能吃掉点击。
//!
//! ⚠ **只在 Dev 变体的菜单里可开**，默认关闭：它是排查工具，不是功能。

// 非 Windows 下本模块的实现体全体没有使用者——`CaretOverlay` 与 `manager.rs` 里
// 唯一的调用点都在 `#[cfg(windows)]` 内，于是 `Canvas`、配色常量、`content_bounds`
// 等私有项在 darwin 构建下全变孤儿，撞上 macOS CI 的 `-D warnings`。
//
// 不给整个 `pub mod` 加 `#[cfg(windows)]` 收场，是为了**保住文件末尾那两个单测**：
// 它们测的是越界丢弃与「画了就得看得见」这类纯像素逻辑，与平台无关；模块一旦被
// cfg 掉，Linux 侧 `cargo test`（`dev.sh ci` 在本机跑）就再也不碰它们，测试变成
// 只在 Windows 开发机上偶尔跑一次——「测试静默不跑」比留一条 allow 更坏。
//
// 用 `cfg_attr(not(windows), ...)` 而不是无条件 `allow`：Windows 侧（含 CI 的
// msvc target clippy）仍然照常查死代码，真长出无用实现照样会被点名。
#![cfg_attr(not(windows), allow(dead_code))]

use wind_ui_types::diag::CaretOverlayView;

#[cfg(windows)]
use crate::text::dwrite::TextRenderer;

/// 画布相对于内容包围盒的外扩，留出画十字与文字的余量。
const PADDING: i32 = 96;
/// 文字行高（物理像素，按 1.0 缩放基准；高 DPI 下由调用方的 scale 放大）。
///
/// 刻意偏小：这块文字是**辅助**，主角是几何。首版用 16px，实测在 150% DPI 下字块
/// 高达 5 行 × 24px，直接把组合区盖住了——调试工具挡住被调试的对象，等于没有。
const LINE_H: f32 = 11.0;

/// BGRA（LayeredWindow 的缓冲是预乘 alpha 的 BGRA）。
#[derive(Clone, Copy)]
struct Rgba(u8, u8, u8, u8);

const C_RECT: Rgba = Rgba(0, 200, 255, 255); // 组合矩形：青
const C_CARET: Rgba = Rgba(255, 64, 64, 255); // 插入点：红
const C_START: Rgba = Rgba(255, 200, 0, 255); // 组合起点：黄
const C_ANCHOR: Rgba = Rgba(0, 255, 120, 255); // 实际锚点：绿
const C_TEXT_BG: Rgba = Rgba(0, 0, 0, 205);

#[cfg(windows)]
pub struct CaretOverlay {
    window: crate::window::LayeredWindow,
    renderer: TextRenderer,
    scale: f32,
    /// 画布左上角在屏幕上的位置——所有几何都要减去它换成画布内坐标。
    origin: (i32, i32),
    visible: bool,
}

#[cfg(windows)]
impl CaretOverlay {
    pub fn new() -> Result<Self, String> {
        let window = crate::window::LayeredWindow::create(None, 16, 16, "WindCaretOverlay")?;
        // 鼠标穿透：浮窗盖在宿主编辑区上方，绝不能吃掉点击。
        // LayeredWindow 默认没有这一位，创建后补上。
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_TRANSPARENT,
            };
            let hwnd = window.hwnd();
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_TRANSPARENT.0 as isize);
        }
        let scale = dpi_scale();
        let renderer = TextRenderer::new("Microsoft YaHei UI", LINE_H * scale)?;
        Ok(Self {
            window,
            renderer,
            scale,
            origin: (0, 0),
            visible: false,
        })
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.window.hide();
            self.visible = false;
        }
    }

    pub fn show_or_update(&mut self, v: &CaretOverlayView) {
        // 画布覆盖「所有要画的点」的包围盒 + 外扩。不整屏铺满：整屏的 layered 缓冲在
        // 4K 下是 30MB+，每帧重画会明显吃 CPU，而我们要画的东西只集中在插入点附近。
        let Some((x0, y0, x1, y1)) = content_bounds(v) else {
            self.hide();
            return;
        };
        // 文字占画布顶部一条独立条带，几何区在其下方——**两者不重叠**是硬要求：
        // 首版把文字压在几何上，实测直接盖住记事本的组合区（用户截图），
        // 调试工具挡住被调试的对象等于没有。
        let lines = legend_lines(v);
        let band = self.measure_legend(&lines);
        // 画布还要能装下最长的一行，否则文字被右侧裁掉（首版实测 rect= 那行被切断）。
        let cw = ((x1 - x0) as u32).max(band.0 + 8);
        let ch = (y1 - y0) as u32 + band.1;
        self.window.resize(cw, ch);
        // 几何整体下移 band.1：origin 上移同样多，绘制时的 `- oy` 就自然把图形推到条带下方。
        self.origin = (x0, y0 - band.1 as i32);
        let band_h = band.1;
        {
            let buf = self.window.buffer_mut();
            buf.fill(0);
        }

        let (bw, bh) = self.window.size();
        let ox = self.origin.0;
        let oy = self.origin.1;

        // 缓冲连同尺寸只取一次:下面每一笔都画在同一张画布上,借用在最后一笔之后结束
        // (NLL),故其后的 paint_legend 仍能拿到 &mut self。
        let mut canvas = Canvas {
            buf: self.window.buffer_mut(),
            w: bw,
            h: bh,
        };

        // ── 组合矩形：空心框 ──
        if let Some((l, t, r, b)) = v.comp_rect {
            canvas.stroke_rect(l - ox, t - oy, r - ox, b - oy, 2, C_RECT);
            // 左下角额外画一个实心小方块：那正是「采信矩形」时锚点该落的位置，
            // 与绿色十字是否重合，一眼就能判断判据算对没有。
            canvas.fill_rect(l - ox - 3, b - oy - 3, l - ox + 3, b - oy + 3, C_RECT);
        }

        // ── 插入点：竖线（长度取上报的 height，能顺带看出行高是否退化）──
        let (cx, cy, chh) = v.caret;
        let h = chh.max(4);
        canvas.fill_rect(cx - ox - 1, cy - oy - h, cx - ox + 1, cy - oy, C_CARET);

        // ── 组合起点：空心小方块 ──
        if let Some((sx, sy)) = v.comp_start {
            canvas.stroke_rect(
                sx - ox - 5,
                sy - oy - 5,
                sx - ox + 5,
                sy - oy + 5,
                2,
                C_START,
            );
        }

        // ── 实际锚点：十字（最关键的一个——候选窗就画在这里）──
        let (ax, ay) = v.anchor;
        canvas.cross(ax - ox, ay - oy, 10, 2, C_ANCHOR);

        // ── 文字标注（顶部条带，不与几何重叠）──
        self.paint_legend(&lines, bw, bh, band_h);

        if let Err(e) = self.window.update() {
            tracing::warn!("CaretOverlay update failed: {}", e);
            return;
        }
        self.window.show(self.origin.0, self.origin.1);
        self.visible = true;
    }

    /// 组装文字块（与绘制分开：尺寸要先量出来才能定画布大小）。
    fn build_legend(&self, lines: &[String]) -> crate::view::View {
        use crate::view::{Align, Edges, Layout, View};
        let s = self.scale;
        let mut col = View::container(Layout::Column)
            .bg([0, 0, 0, C_TEXT_BG.3])
            .pad(Edges::xy(6.0 * s, 3.0 * s))
            .gap(1.0 * s);
        for line in lines {
            col =
                col.child(View::leaf(line.clone(), [225, 225, 225, 255]).text_align(Align::Start));
        }
        col
    }

    /// 量出文字条带的 (宽, 高)，画布据此预留空间。
    fn measure_legend(&self, lines: &[String]) -> (u32, u32) {
        let mut col = self.build_legend(lines);
        col.layout(0.0, 0.0, &self.renderer);
        let (w, h) = col.measured_size();
        (w.ceil() as u32, h.ceil() as u32)
    }

    fn paint_legend(&mut self, lines: &[String], bw: u32, bh: u32, _band_h: u32) {
        let mut col = self.build_legend(lines);
        col.layout(0.0, 0.0, &self.renderer);
        col.paint(self.window.buffer_mut(), bw, bh, &self.renderer);
    }
}

/// 系统 DPI 缩放因子（与 `input_diag_hud` / `status_tip` 同源——本 crate 没有共享入口）。
#[cfg(windows)]
fn dpi_scale() -> f32 {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGPIXELSY, ReleaseDC};
    unsafe {
        let hdc = GetDC(HWND::default());
        let dpi = GetDeviceCaps(hdc, LOGPIXELSY);
        ReleaseDC(HWND::default(), hdc);
        if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 }
    }
}

/// 文字标注的各行内容。
fn legend_lines(v: &CaretOverlayView) -> Vec<String> {
    let (cx, cy, chh) = v.caret;
    let rect_txt = match v.comp_rect {
        Some((l, t, r, b)) => format!("rect=({l},{t},{r},{b}) {}x{}", r - l, b - t),
        None => "rect=—".to_string(),
    };
    let start_txt = match v.comp_start {
        Some((x, y)) => format!("start=({x},{y})"),
        None => "start=—".to_string(),
    };
    vec![
        format!(
            "{}  src={}  判据: {}",
            v.process,
            v.caret_source,
            v.rect_verdict.label()
        ),
        rect_txt,
        format!("{start_txt}  caret=({cx},{cy}) h={chh}"),
        format!(
            "anchor=({},{}) ← {}",
            v.anchor.0, v.anchor.1, v.anchor_source
        ),
    ]
}

/// 内容包围盒（含外扩）。全 `None` 且 caret 也无效时返回 `None`——没东西可画。
fn content_bounds(v: &CaretOverlayView) -> Option<(i32, i32, i32, i32)> {
    let (cx, cy, ch) = v.caret;
    let mut pts: Vec<(i32, i32)> = vec![(cx, cy), (cx, cy - ch.max(4)), v.anchor];
    if let Some((x, y)) = v.comp_start {
        pts.push((x, y));
    }
    if let Some((l, t, r, b)) = v.comp_rect {
        pts.push((l, t));
        pts.push((r, b));
    }
    let x0 = pts.iter().map(|p| p.0).min()? - PADDING;
    let y0 = pts.iter().map(|p| p.1).min()? - PADDING;
    let x1 = pts.iter().map(|p| p.0).max()? + PADDING;
    let y1 = pts.iter().map(|p| p.1).max()? + PADDING;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0, y0, x1, y1))
}

/// 绘制目标：像素缓冲连同它的尺寸。
///
/// 三者恒一起传递（缓冲离开 `w`/`h` 就无法定位像素），打包成一体后各绘制函数的参数
/// 数量落回可读范围，也让「`buf` 的长度必须与 `w`×`h`×4 自洽」这个前提有了归属。
struct Canvas<'a> {
    buf: &'a mut [u8],
    w: u32,
    h: u32,
}

impl Canvas<'_> {
    /// 写一个像素（BGRA，预乘 alpha）。越界静默丢弃——几何可能部分落在画布外。
    fn put(&mut self, x: i32, y: i32, c: Rgba) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let idx = ((y as u32 * self.w + x as u32) * 4) as usize;
        if idx + 3 >= self.buf.len() {
            return;
        }
        let a = c.3 as u32;
        // 预乘：LayeredWindow 的 UpdateLayeredWindow 走 AC_SRC_ALPHA，缓冲必须是预乘的。
        self.buf[idx] = ((c.2 as u32 * a) / 255) as u8;
        self.buf[idx + 1] = ((c.1 as u32 * a) / 255) as u8;
        self.buf[idx + 2] = ((c.0 as u32 * a) / 255) as u8;
        self.buf[idx + 3] = c.3;
    }

    fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgba) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.put(x, y, c);
            }
        }
    }

    fn stroke_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, t: i32, c: Rgba) {
        for i in 0..t {
            for x in x0..=x1 {
                self.put(x, y0 + i, c);
                self.put(x, y1 - i, c);
            }
            for y in y0..=y1 {
                self.put(x0 + i, y, c);
                self.put(x1 - i, y, c);
            }
        }
    }

    fn cross(&mut self, x: i32, y: i32, arm: i32, t: i32, c: Rgba) {
        self.fill_rect(x - arm, y - t / 2, x + arm, y + t / 2, c);
        self.fill_rect(x - t / 2, y - arm, x + t / 2, y + arm, c);
    }
}

#[cfg(test)]
#[path = "caret_overlay_tests.rs"]
mod tests;
