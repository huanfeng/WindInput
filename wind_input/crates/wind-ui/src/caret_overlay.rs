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

        // ── 组合矩形：空心框 ──
        if let Some((l, t, r, b)) = v.comp_rect {
            stroke_rect(
                self.window.buffer_mut(),
                bw,
                bh,
                l - ox,
                t - oy,
                r - ox,
                b - oy,
                2,
                C_RECT,
            );
            // 左下角额外画一个实心小方块：那正是「采信矩形」时锚点该落的位置，
            // 与绿色十字是否重合，一眼就能判断判据算对没有。
            fill_rect(
                self.window.buffer_mut(),
                bw,
                bh,
                l - ox - 3,
                b - oy - 3,
                l - ox + 3,
                b - oy + 3,
                C_RECT,
            );
        }

        // ── 插入点：竖线（长度取上报的 height，能顺带看出行高是否退化）──
        let (cx, cy, chh) = v.caret;
        let h = chh.max(4);
        fill_rect(
            self.window.buffer_mut(),
            bw,
            bh,
            cx - ox - 1,
            cy - oy - h,
            cx - ox + 1,
            cy - oy,
            C_CARET,
        );

        // ── 组合起点：空心小方块 ──
        if let Some((sx, sy)) = v.comp_start {
            stroke_rect(
                self.window.buffer_mut(),
                bw,
                bh,
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
        cross(
            self.window.buffer_mut(),
            bw,
            bh,
            ax - ox,
            ay - oy,
            10,
            2,
            C_ANCHOR,
        );

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

/// 写一个像素（BGRA，预乘 alpha）。越界静默丢弃——几何可能部分落在画布外。
fn put(buf: &mut [u8], bw: u32, bh: u32, x: i32, y: i32, c: Rgba) {
    if x < 0 || y < 0 || x >= bw as i32 || y >= bh as i32 {
        return;
    }
    let idx = ((y as u32 * bw + x as u32) * 4) as usize;
    if idx + 3 >= buf.len() {
        return;
    }
    let a = c.3 as u32;
    // 预乘：LayeredWindow 的 UpdateLayeredWindow 走 AC_SRC_ALPHA，缓冲必须是预乘的。
    buf[idx] = ((c.2 as u32 * a) / 255) as u8;
    buf[idx + 1] = ((c.1 as u32 * a) / 255) as u8;
    buf[idx + 2] = ((c.0 as u32 * a) / 255) as u8;
    buf[idx + 3] = c.3;
}

fn fill_rect(buf: &mut [u8], bw: u32, bh: u32, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgba) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            put(buf, bw, bh, x, y, c);
        }
    }
}

fn stroke_rect(
    buf: &mut [u8],
    bw: u32,
    bh: u32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    t: i32,
    c: Rgba,
) {
    for i in 0..t {
        for x in x0..=x1 {
            put(buf, bw, bh, x, y0 + i, c);
            put(buf, bw, bh, x, y1 - i, c);
        }
        for y in y0..=y1 {
            put(buf, bw, bh, x0 + i, y, c);
            put(buf, bw, bh, x1 - i, y, c);
        }
    }
}

fn cross(buf: &mut [u8], bw: u32, bh: u32, x: i32, y: i32, arm: i32, t: i32, c: Rgba) {
    fill_rect(buf, bw, bh, x - arm, y - t / 2, x + arm, y + t / 2, c);
    fill_rect(buf, bw, bh, x - t / 2, y - arm, x + t / 2, y + arm, c);
}

#[cfg(test)]
mod tests {
    use super::*;
    // 只有测试构造样例时用到；放在顶层会让非测试构建报未使用导入（CI 的 clippy 按 -D warnings 跑）。
    use wind_ui_types::diag::RectVerdict;

    fn view() -> CaretOverlayView {
        CaretOverlayView {
            caret: (2710, 792, 43),
            comp_start: Some((1699, 745)),
            comp_rect: Some((1699, 701, 2710, 792)),
            anchor: (1699, 792),
            anchor_source: "composition_rect",
            caret_source: "tsf_selection",
            rect_verdict: RectVerdict::Trusted,
            process: "WINWORD.EXE".into(),
        }
    }

    /// 包围盒必须覆盖**所有**要画的点——漏掉任何一个，那个元素就画在画布外、静默消失，
    /// 而「元素没出现」在调试工具里会被误读成「宿主没上报」。
    #[test]
    fn bounds_cover_every_drawn_element() {
        let v = view();
        let (x0, y0, x1, y1) = content_bounds(&v).expect("有内容时必须给出包围盒");
        let (l, t, r, b) = v.comp_rect.unwrap();
        let (sx, sy) = v.comp_start.unwrap();
        for (px, py, what) in [
            (l, t, "rect 左上"),
            (r, b, "rect 右下"),
            (sx, sy, "组合起点"),
            (v.caret.0, v.caret.1, "caret 底"),
            (v.caret.0, v.caret.1 - v.caret.2, "caret 顶"),
            (v.anchor.0, v.anchor.1, "锚点"),
        ] {
            assert!(
                px > x0 && px < x1 && py > y0 && py < y1,
                "{what} ({px},{py}) 落在画布外 ({x0},{y0})-({x1},{y1})"
            );
        }
    }

    /// 只有 caret 的退化情形（宿主什么都没给）仍要能画出来——那恰恰是要观察的状态。
    #[test]
    fn bounds_work_with_caret_only() {
        let v = CaretOverlayView {
            comp_start: None,
            comp_rect: None,
            ..view()
        };
        assert!(content_bounds(&v).is_some());
    }

    /// ★ 文字标注必须能完整表达四类信息——缺任何一类，看图的人就得回头翻日志，
    /// 那正是这个工具要消灭的动作。
    #[test]
    fn legend_carries_every_thing_needed_to_judge() {
        let v = view();
        let text = legend_lines(&v).join(
            "
",
        );
        for needle in [
            "WINWORD.EXE",       // 哪个宿主——per-app 开关作用在谁身上
            "tsf_selection",     // 坐标来源
            "采信矩形",          // 判据走了哪条分支
            "1699,701,2710,792", // 宿主给的矩形
            "2710,792",          // 插入点
            "1699,745",          // 组合起点
            "composition_rect",  // 锚点取自哪里
        ] {
            assert!(
                text.contains(needle),
                "文字标注缺少 {needle}：
{text}"
            );
        }
    }

    /// ★★★ 文字条带**不得压住几何**——调试工具挡住被调试的对象等于没有。
    ///
    /// 首版把文字画在画布左上角 (4,4)，而画布原点 = 内容包围盒 - PADDING，于是文字
    /// 必然落在组合区正上方 96px 内、几乎注定重叠：记事本实测直接盖住了组合区。
    /// 修法是让文字**占用画布之外新增的空间**，而不是借用给几何留的余量。
    #[test]
    fn legend_band_does_not_overlap_geometry() {
        let v = view();
        let (_, y0, _, _) = content_bounds(&v).unwrap();
        // 条带高度由 measure_legend 给出（此处以保守下界代入：至少一行）。
        let band_h = 1u32;
        // 几何的最高点是 rect.top；画布原点上移 band_h 后，它在画布内的 y 应当 ≥ band_h。
        let origin_y = y0 - band_h as i32;
        let rect_top_in_canvas = v.comp_rect.unwrap().1 - origin_y;
        assert!(
            rect_top_in_canvas >= band_h as i32,
            "几何最高点落进了文字条带内（{rect_top_in_canvas} < {band_h}）"
        );
    }

    /// 越界写入必须静默丢弃，不得 panic 也不得写坏相邻像素。
    #[test]
    fn out_of_bounds_pixels_are_dropped() {
        let (bw, bh) = (8u32, 8u32);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        put(&mut buf, bw, bh, -1, 0, C_CARET);
        put(&mut buf, bw, bh, 0, -1, C_CARET);
        put(&mut buf, bw, bh, 8, 0, C_CARET);
        put(&mut buf, bw, bh, 0, 8, C_CARET);
        assert!(buf.iter().all(|&b| b == 0), "越界写入污染了缓冲");
    }

    /// 画出来的每个元素都要能被看见：填充后对应像素的 alpha 必须非零。
    #[test]
    fn drawing_actually_marks_pixels() {
        let (bw, bh) = (16u32, 16u32);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        cross(&mut buf, bw, bh, 8, 8, 4, 2, C_ANCHOR);
        let idx = ((8 * bw + 8) * 4) as usize;
        assert_ne!(buf[idx + 3], 0, "十字中心应当被画上");
    }
}
