//! Toast 通知窗口
//!
//! 与 Go 版本 `wind_input/internal/ui/toast_window.go` 对齐。
//! 一次性通知（方案切换/词库就绪/错误等）：独立 layered 窗口，按屏幕位置摆放，
//! 类型区分配色，约数秒后由 UI 循环自动隐藏。尺寸跟随主题（theme.views.toast）。

use crate::text::dwrite::TextRenderer;
use crate::view::{Align, Edges, Layout, View, ViewImage, ViewLayer};
use crate::window::LayeredWindow;

/// 位置/类型枚举已下沉至 wind-ui-types；再导出保持 `wind_ui::toast::*` 原路径成立。
pub use wind_ui_types::{ToastKind, ToastPosition};

/// 左侧强调条颜色（RGBA）。渲染色表属于渲染端，不随类型下沉；
/// 类型来自外部 crate 无法加私有固有方法，故为模块私有自由函数。
fn accent(kind: ToastKind) -> [u8; 4] {
    match kind {
        ToastKind::Info => [64, 158, 255, 255],
        ToastKind::Success => [82, 196, 110, 255],
        ToastKind::Error => [245, 108, 108, 255],
    }
}

/// Toast 通知窗口
pub struct Toast {
    window: LayeredWindow,
    renderer: TextRenderer,
    scale: f32,
    bg: [u8; 4],
    fg: [u8; 4],
    bg_image: Option<ViewImage>,
    layers: Vec<ViewLayer>,
    shadow: Option<crate::view::SoftShadow>,
    radius: Option<f32>,
    /// 主题配的边框（色, 宽 dp）。None=未配，回退按提示等级取色的内置默认。
    border: Option<([u8; 4], Option<f32>)>,
    theme: Option<wind_theme::Resolved>,
    /// 基准字号（逻辑像素）：跟随主题 behavior.font_size（+ toast 节点偏移）。
    base_logical: f32,
}

impl Toast {
    /// 无主题时的兜底字号（逻辑像素）。
    const DEFAULT_FONT_PX: f32 = 15.0;

    pub fn new() -> Result<Self, String> {
        let scale = crate::dpi::scale_for_point(0, 0);
        let window = LayeredWindow::create(None, 240, 60, "WindInputToast")?;
        let renderer = TextRenderer::new("Microsoft YaHei UI", Self::DEFAULT_FONT_PX * scale)?;
        Ok(Self {
            window,
            renderer,
            scale,
            bg: [44, 44, 48, 240],
            fg: [240, 240, 245, 255],
            bg_image: None,
            layers: Vec::new(),
            shadow: None,
            radius: None,
            border: None,
            theme: None,
            base_logical: Self::DEFAULT_FONT_PX,
        })
    }

    /// 应用主题（toast 底色/文字色 + 位图背景/层；尺寸跟随主题）。
    pub fn set_theme(&mut self, theme: &wind_theme::Resolved) {
        self.theme = Some(theme.clone());
        self.bg = theme.color("toast_bg", self.bg);
        self.fg = theme.color("toast_text", self.fg);
        let node_off = theme
            .views
            .toast
            .as_ref()
            .map(|n| n.font_size)
            .unwrap_or(0.0);
        self.base_logical = (theme.behavior.font_size as f32 + node_off).max(8.0);
        self.renderer.set_base_size(self.base_logical * self.scale);
        if let Some(node) = &theme.views.toast {
            let s = self.scale;
            self.bg_image = crate::theme_assets::rv_image(theme, node.bg_image.as_ref());
            self.layers = crate::theme_assets::rv_layers(theme, &node.layers, s);
            self.shadow = crate::view::SoftShadow::build(
                node.shadow_offset_x,
                node.shadow_offset_y,
                node.shadow_blur,
                node.shadow_spread,
                node.shadow_spread_offset_x,
                node.shadow_spread_offset_y,
                node.shadow_color,
                s,
            );
            self.radius = node.border_radius.map(|d| d.resolve(s, 0.0));
            // 底色/文字色/边框改由节点覆盖 palette 兜底（节点色已在 resolve 阶段
            // 合成 toast_bg / toast_text 默认）。边框此前是 accent + 2px 字面量，
            // 主题写什么都没用，现在未配才回退到那套按等级取色的默认。
            if let Some(c) = node.bg_color {
                self.bg = c;
            }
            if let Some(c) = node.text_color {
                self.fg = c;
            }
            self.border = node
                .border_color
                .map(|c| (c, node.border_width.map(|d| d.resolve(s, 0.0))));
        } else {
            self.bg_image = None;
            self.layers = Vec::new();
            self.shadow = None;
            self.border = None;
            self.radius = None;
        }
    }

    /// 显示 toast：`pos` 决定屏幕位置，`kind` 决定强调色，`accent_override`
    /// （`ui.toast(color=…)`）给了则压过 kind 与主题边框色。
    pub fn show(
        &mut self,
        text: &str,
        pos: ToastPosition,
        kind: ToastKind,
        accent_override: Option<[u8; 4]>,
    ) {
        if text.is_empty() {
            self.hide();
            return;
        }
        self.ensure_scale();
        let s = self.scale;
        // 深色圆角底 + 居中文本；类型用边框色区分（稳健、跨主题可见）。
        let label = View::leaf(text, self.fg).text_align(Align::Center);
        // 边框取色三级：显式 color > 主题 toast.border > 按提示等级取色（+2dp 宽）。
        // 显式色排在主题之前——它是这一条提示的语义，不是外观偏好。
        let (border_color, border_width) = match (accent_override, self.border) {
            (Some(c), b) => (c, b.and_then(|(_, w)| w).unwrap_or(2.0 * s).max(1.0)),
            (None, Some((c, w))) => (c, w.unwrap_or(2.0 * s).max(1.0)),
            (None, None) => (accent(kind), (2.0 * s).max(1.0)),
        };
        let mut card = View::container(Layout::Row)
            .cross(Align::Center)
            .bg(self.bg)
            .pad(Edges::xy(14.0 * s, 10.0 * s))
            .child(label)
            .border(border_color, border_width);
        card.corner_radius = self.radius.unwrap_or(6.0 * s);
        if let Some(img) = &self.bg_image {
            card = card.bg_image(img.clone());
        }
        if !self.layers.is_empty() {
            card = card.layers(self.layers.clone());
        }

        let (ml, mt, mr, mb) = self
            .shadow
            .as_ref()
            .map(|sh| sh.margins())
            .unwrap_or((0, 0, 0, 0));
        card.layout(ml as f32, mt as f32, &self.renderer);
        let (w_f, h_f) = card.measured_size();
        let cw = (w_f.ceil() as u32).max(48);
        let ch = (h_f.ceil() as u32).max(28);
        let w = cw + ml + mr;
        let h = ch + mt + mb;

        self.window.resize(w, h);
        {
            let buf = self.window.buffer_mut();
            let n = (w * h * 4) as usize;
            buf[..n].fill(0);
            if let Some(sh) = &self.shadow {
                sh.paint(
                    buf,
                    w,
                    h,
                    ml as f32,
                    mt as f32,
                    cw as f32,
                    ch as f32,
                    card.corner_radius,
                );
            }
            card.paint(buf, w, h, &self.renderer);
        }
        if let Err(e) = self.window.update() {
            tracing::warn!("Toast update failed: {}", e);
        }

        let (px, py) = place_on_work_area(pos, cw, ch, (12.0 * s).round() as i32);
        self.window.show(px - ml as i32, py - mt as i32);
    }

    /// 将当前渲染帧保存为 PNG 文件（截图用）。
    pub fn capture_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        self.window.capture_to_file(path)
    }

    /// 窗口当前是否可见（查询 Win32 IsWindowVisible）。
    pub fn is_visible(&self) -> bool {
        #[cfg(windows)]
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(self.window.hwnd()).as_bool()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// 返回 Toast 窗口句柄（截图用）。
    #[cfg(windows)]
    pub fn hwnd(&self) -> windows::Win32::Foundation::HWND {
        self.window.hwnd()
    }

    pub fn hide(&self) {
        self.window.hide();
    }

    fn ensure_scale(&mut self) {
        let sc = crate::dpi::scale_for_point(0, 0);
        if (sc - self.scale).abs() > 0.01 {
            self.scale = sc;
            self.renderer.set_base_size(self.base_logical * sc);
            if let Some(t) = self.theme.clone() {
                self.set_theme(&t);
            }
        }
    }
}

/// 按 `pos` 在光标所在显示器工作区内摆放内容盒，返回内容盒左上屏幕坐标。`margin` 为离边距。
#[cfg_attr(not(windows), allow(unused_variables))]
fn place_on_work_area(pos: ToastPosition, w: u32, h: u32, margin: i32) -> (i32, i32) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
        };
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        unsafe {
            let mut cur = POINT::default();
            let _ = GetCursorPos(&mut cur);
            let mon = MonitorFromPoint(cur, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let wa = mi.rcWork;
                let (wi, hi) = (w as i32, h as i32);
                let cx = (wa.left + wa.right) / 2 - wi / 2;
                let cy = (wa.top + wa.bottom) / 2 - hi / 2;
                let left = wa.left + margin;
                let right = wa.right - wi - margin;
                let top = wa.top + margin;
                let bottom = wa.bottom - hi - margin;
                let (x, y) = match pos {
                    ToastPosition::Center => (cx, cy),
                    ToastPosition::TopCenter => (cx, top),
                    ToastPosition::BottomCenter => (cx, bottom),
                    ToastPosition::TopLeft => (left, top),
                    ToastPosition::TopRight => (right, top),
                    ToastPosition::BottomLeft => (left, bottom),
                    ToastPosition::BottomRight => (right, bottom),
                };
                return (x.max(wa.left), y.max(wa.top));
            }
        }
    }
    (0, 0)
}
