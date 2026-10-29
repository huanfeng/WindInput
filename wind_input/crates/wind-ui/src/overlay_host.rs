//! 外部宿主（Linux Fcitx5 addon / macOS `.app`）的光栅浮层：状态气泡 / Toast / 悬停提示。
//!
//! 宿主不排字，这三者与候选窗同构——在服务进程按主题光栅化（Linux 走 `text/linux`，macOS 走
//! CoreText），像素写进各层自己的 SHM 段，再推一帧 `CMD_OVERLAY_FRAME` 通知。宿主坐标单位
//! 在 Linux 是像素、macOS 是点，见 [`px_per_unit`]。
//!
//! **落位分工**：服务端拿不到屏幕几何（X11 根窗口 / 工作区只有 addon 知道），故只给出
//! 「首选点 + 备选点」或「锚点 + 留白」，addon 按工作区选定并夹回（`wind_linux` 的
//! `placeOverlay`）。几何公式与 Windows 各窗口的本地定位逐项对应，见各 `render_overlay`。
//!
//! **计时分工**：自动隐藏时长随帧下发、由宿主（addon / `.app`）计时。服务端的
//! forwarder 线程阻塞在命令通道上，没有到期唤醒机制，不在这边计时。

use std::sync::{Arc, Mutex};

use wind_bridge::HostRenderSink;
use wind_bridge::shared_memory_posix::PosixSharedMemory;
use wind_ipc::codec::{OverlayFrameMeta, encode_overlay_frame};
use wind_ipc::protocol::overlay::*;
use wind_ipc::protocol::{MAX_SHARED_RENDER_SIZE, SharedRenderHeader};
use wind_ui_types::{StatusTipAnchor, StatusTipPlacement, ToastKind, ToastPosition};

use crate::status_tip::StatusTip;
use crate::toast::Toast;

/// 浮层怎么落位（坐标均为**内容盒**左上，屏幕坐标）。语义见 `protocol::overlay` 的 `OVERLAY_PLACE_*`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    Absolute {
        x: i32,
        y: i32,
    },
    Flip {
        x: i32,
        y: i32,
        alt_x: i32,
        alt_y: i32,
    },
    FollowCandidate {
        x: i32,
        y: i32,
        alt_x: i32,
        alt_y: i32,
    },
    Anchor {
        anchor: u32,
        margin: i32,
    },
}

/// 宿主坐标单位对应多少位图像素。Linux 宿主（X11 / Wayland）的坐标就是像素：1.0；
/// macOS 的坐标是点，位图是 `scale` 倍的像素。落位里凡是把位图尺寸 / 像素偏移与宿主坐标混算
/// 的地方（留白、翻转的备选点、候选行矩形…）都要先过 [`to_host`]。
pub(crate) fn px_per_unit(scale: f32) -> f32 {
    px_per_unit_for(scale, cfg!(target_os = "macos"))
}

/// [`px_per_unit`] 的纯函数形态（`host_uses_points` 由平台决定），便于在 Linux 上测 macOS 的分支。
fn px_per_unit_for(scale: f32, host_uses_points: bool) -> f32 {
    if host_uses_points {
        scale.max(1.0)
    } else {
        1.0
    }
}

/// 位图像素长度 → 宿主坐标单位（四舍五入）。
pub(crate) fn to_host(px: f32, scale: f32) -> i32 {
    (px / px_per_unit(scale)).round() as i32
}

/// 一层浮层的一帧：位图 + 内容盒 + 落位规则。
pub(crate) struct Overlay {
    pub buf: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// 内容盒在位图内的偏移（软阴影左/上扩边）。
    pub content_x: u32,
    pub content_y: u32,
    pub content_w: u32,
    pub content_h: u32,
    pub software_shadow: bool,
    pub place: Place,
    /// 本帧的渲染缩放（位图像素 / 逻辑像素），随帧带给 macOS `.app` 折回点。
    pub scale: f32,
}

impl Overlay {
    fn meta(&self, kind: u32, seq: u32, duration_ms: i32) -> OverlayFrameMeta {
        let mut flags = SharedRenderHeader::FLAG_VISIBLE | SharedRenderHeader::FLAG_CONTENT_READY;
        if self.software_shadow {
            flags |= SharedRenderHeader::FLAG_SOFTWARE_SHADOW;
        }
        let mut m = OverlayFrameMeta {
            kind,
            seq,
            width: self.width,
            height: self.height,
            flags,
            content_x: self.content_x as i32,
            content_y: self.content_y as i32,
            content_w: self.content_w,
            content_h: self.content_h,
            duration_ms,
            scale: self.scale,
            ..Default::default()
        };
        match self.place {
            Place::Absolute { x, y } => {
                m.place = OVERLAY_PLACE_ABSOLUTE;
                (m.x, m.y, m.alt_x, m.alt_y) = (x, y, x, y);
            }
            Place::Flip { x, y, alt_x, alt_y } => {
                m.place = OVERLAY_PLACE_FLIP;
                (m.x, m.y, m.alt_x, m.alt_y) = (x, y, alt_x, alt_y);
            }
            Place::FollowCandidate { x, y, alt_x, alt_y } => {
                m.place = OVERLAY_PLACE_FOLLOW_CANDIDATE;
                (m.x, m.y, m.alt_x, m.alt_y) = (x, y, alt_x, alt_y);
            }
            Place::Anchor { anchor, margin } => {
                m.place = OVERLAY_PLACE_ANCHOR;
                m.anchor = anchor;
                m.margin = margin;
            }
        }
        m
    }
}

/// 状态气泡锚点 → wire 锚点。窗口锚点在 Linux 拿不到前台窗口边框（`anchor_frames` 非
/// Windows 恒 `None`），与 Windows「拿不到边框」同一降级：换成同位置的屏幕锚点。
pub(crate) fn status_anchor_code(a: StatusTipAnchor) -> u32 {
    use StatusTipAnchor as A;
    match a.screen_equivalent() {
        A::ScreenCenter | A::WindowCenter => OVERLAY_ANCHOR_CENTER,
        A::ScreenTopLeft => OVERLAY_ANCHOR_TOP_LEFT,
        A::ScreenTopRight => OVERLAY_ANCHOR_TOP_RIGHT,
        A::ScreenBottomLeft | A::WindowBottomLeft => OVERLAY_ANCHOR_BOTTOM_LEFT,
        A::ScreenBottomRight => OVERLAY_ANCHOR_BOTTOM_RIGHT,
    }
}

/// Toast 位置 → wire 锚点（Windows `toast::place_on_work_area` 的七个位置）。
pub(crate) fn toast_anchor_code(p: ToastPosition) -> u32 {
    match p {
        ToastPosition::Center => OVERLAY_ANCHOR_CENTER,
        ToastPosition::TopCenter => OVERLAY_ANCHOR_TOP_CENTER,
        ToastPosition::BottomCenter => OVERLAY_ANCHOR_BOTTOM_CENTER,
        ToastPosition::TopLeft => OVERLAY_ANCHOR_TOP_LEFT,
        ToastPosition::TopRight => OVERLAY_ANCHOR_TOP_RIGHT,
        ToastPosition::BottomLeft => OVERLAY_ANCHOR_BOTTOM_LEFT,
        ToastPosition::BottomRight => OVERLAY_ANCHOR_BOTTOM_RIGHT,
    }
}

/// 状态气泡跟随光标的落位（纯几何）。与 Windows `StatusTip::show` 同一公式：左对齐于
/// 光标、首选侧（`above` 时光标顶端上方，否则底端下方）`gap` 处叠加用户偏移；首选侧放不下时
/// 翻到另一侧（不叠偏移）。`caret_bottom` 是光标底端，`caret_h` 光标高。
///
/// ⚠ 宿主的 `FLIP` 只在**下溢**时改用备选点：首选上方而上方越出工作区时，宿主只会把它夹到上沿。
pub(crate) fn status_caret_place(
    caret_x: i32,
    caret_bottom: i32,
    caret_h: i32,
    offset: (i32, i32),
    content_h: u32,
    gap: i32,
    above: bool,
) -> Place {
    let x = caret_x + offset.0;
    let below = caret_bottom + gap;
    let top = caret_bottom - caret_h.max(0) - content_h as i32 - gap;
    let (y, alt_y) = if above {
        (top + offset.1, below)
    } else {
        (below + offset.1, top)
    };
    Place::Flip {
        x,
        y,
        alt_x: x,
        alt_y,
    }
}

/// 状态气泡固定位置：`(0,0)` 是「从未摆过」的哨兵，落到光标处（同 Windows `fixed_anchor`）。
pub(crate) fn status_fixed_place(fx: i32, fy: i32, caret_x: i32, caret_bottom: i32) -> Place {
    let (x, y) = if (fx, fy) == (0, 0) {
        (caret_x, caret_bottom)
    } else {
        (fx, fy)
    };
    Place::Absolute { x, y }
}

/// 悬停提示的落位（纯几何），与 Windows `tooltip::clamp_to_work_area` / `clamp_beside` 对应。
/// `row` 是悬停候选行在屏幕上的 `(left, top, right, bottom)`（按候选窗**建议**落点算，
/// addon 再平移到候选窗实际位置）。
/// - 横排：行下方 2px，下方放不下翻到行上方；
/// - 竖排 / 旋转：窗口右侧 4px 对齐行顶，右侧放不下改左侧。
pub(crate) fn tooltip_place(row: (i32, i32, i32, i32), content: (u32, u32), beside: bool) -> Place {
    let (l, t, r, b) = row;
    let (cw, ch) = (content.0 as i32, content.1 as i32);
    if beside {
        let gap = 4;
        Place::FollowCandidate {
            x: r + gap,
            y: t,
            alt_x: l - gap - cw,
            alt_y: t,
        }
    } else {
        let gap = 2;
        Place::FollowCandidate {
            x: l,
            y: b + gap,
            alt_x: l,
            alt_y: t - gap - ch,
        }
    }
}

const LAYERS: [u32; 3] = [
    OVERLAY_KIND_TOOLTIP,
    OVERLAY_KIND_STATUS,
    OVERLAY_KIND_TOAST,
];

/// 三层浮层的 SHM 段与显示态。候选窗那段仍归 `Forwarder` 自己管。
pub(crate) struct OverlayShms {
    suffix: String,
    shms: [Option<PosixSharedMemory>; 3],
    shown: [bool; 3],
}

/// 建某一层的 SHM 段（段名见 `endpoint::overlay_shm_name`）。失败记 warn 返回 `None`，
/// 调用方整帧放弃（不推无像素的通知）。
pub(crate) fn create_layer_shm(suffix: &str, kind: u32) -> Option<PosixSharedMemory> {
    let name = wind_bridge::endpoint::overlay_shm_name(suffix, kind)?;
    PosixSharedMemory::create(&name, MAX_SHARED_RENDER_SIZE)
        .map_err(|e| tracing::warn!("create overlay SHM {name} failed: {e}"))
        .ok()
}

fn slot(kind: u32) -> usize {
    LAYERS
        .iter()
        .position(|k| *k == kind)
        .expect("未知浮层 kind")
}

impl OverlayShms {
    pub(crate) fn new(suffix: String) -> Self {
        Self {
            suffix,
            shms: [None, None, None],
            shown: [false; 3],
        }
    }

    fn ensure(&mut self, kind: u32) -> Option<&mut PosixSharedMemory> {
        let i = slot(kind);
        if self.shms[i].is_none() {
            self.shms[i] = Some(create_layer_shm(&self.suffix, kind)?);
        }
        self.shms[i].as_mut()
    }

    /// 写像素 + 推通知。SHM 建不起来整帧放弃（不推无像素的通知）。
    pub(crate) fn show(
        &mut self,
        sink: &dyn HostRenderSink,
        kind: u32,
        ov: &Overlay,
        duration_ms: i32,
    ) {
        if ov.buf.len() > MAX_SHARED_RENDER_SIZE - SharedRenderHeader::SIZE {
            tracing::warn!("浮层 kind={kind} 位图过大 {}x{}，丢弃", ov.width, ov.height);
            return;
        }
        let Some(shm) = self.ensure(kind) else {
            return;
        };
        let seq = shm.write_frame(0, 0, ov.width, ov.height, &ov.buf);
        sink.push_frame(&encode_overlay_frame(&ov.meta(kind, seq, duration_ms)));
        self.shown[slot(kind)] = true;
    }

    /// 该层当前显示着的一帧 `(BGRA, 宽, 高)`，从 SHM 读回（截图用）；没在显示为 `None`。
    ///
    /// 「在显示」是服务端这边的认识：addon 自己计时 / 被点击收掉的浮层不回报。截图只从
    /// 气泡自己的右键菜单进来，菜单开着期间 addon 不让它消失，两边在这里是一致的。
    pub(crate) fn snapshot(&self, kind: u32) -> Option<(Vec<u8>, u32, u32)> {
        let i = slot(kind);
        if !self.shown[i] {
            return None;
        }
        let shm = self.shms[i].as_ref()?;
        let hdr = shm.read_header();
        let (w, h) = (hdr.width, hdr.height);
        let n = w as usize * h as usize * 4;
        let px = shm.pixels();
        (n > 0 && px.len() >= n).then(|| (px[..n].to_vec(), w, h))
    }

    /// 隐藏一层。本就没显示过则什么都不发（候选窗每帧都会问一次 tooltip 该不该藏）。
    pub(crate) fn hide(&mut self, sink: &dyn HostRenderSink, kind: u32) {
        let i = slot(kind);
        if !self.shown[i] {
            return;
        }
        self.shown[i] = false;
        let seq = self.shms[i].as_mut().map(|s| s.write_hidden()).unwrap_or(0);
        sink.push_frame(&encode_overlay_frame(&OverlayFrameMeta {
            kind,
            seq,
            ..Default::default()
        }));
    }
}

/// 状态气泡与 Toast 的渲染器 + 三层 SHM。tooltip 的渲染器挂在候选窗里（它要候选行几何），
/// 这里只替它管 SHM。
///
/// SHM 放在锁后：截图类命令在后台线程收尾（见 `manager_macos` 的 `spawn_screenshot_work`），
/// 结果 Toast 得在那边弹，碰不到 forwarder 的 `&mut self`。渲染器（内含 `Rc`）不能过线程，
/// 那边按主题快照现建一个，见 [`ToastHandle`]。
pub(crate) struct Overlays {
    shms: Arc<Mutex<OverlayShms>>,
    status: Option<StatusTip>,
    toast: Option<Toast>,
    theme: Option<Box<wind_theme::Resolved>>,
    /// 候选窗建议落点的内容盒（宿主坐标单位），没显示为 `None`。跟随光标的状态泡避让它。
    cand_rect: Option<crate::status_tip::Rect4>,
    /// 最近一次状态泡的参数与到期时刻（`None` = 常驻）：候选窗出现 / 挪动时按它重新落位。
    last_status: Option<LastStatus>,
}

/// 最近一次状态泡：`(文本, 光标 x, 光标行顶, 光标高, 定位, 到期时刻)`。
type LastStatus = (
    String,
    i32,
    i32,
    i32,
    StatusTipPlacement,
    Option<std::time::Instant>,
);

/// 可带进后台线程的 Toast 入口：SHM 共享，渲染器在使用线程上现建（截图是偶发动作，
/// 建一个渲染器的成本淹没在剪贴板外部命令里）。
pub(crate) struct ToastHandle {
    shms: Arc<Mutex<OverlayShms>>,
    theme: Option<Box<wind_theme::Resolved>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 渲染一条 Toast 并推上去（`toast` 为 None = 渲染器没建起来，只能藏掉旧的）。
#[allow(clippy::too_many_arguments)]
fn push_toast(
    shms: &Mutex<OverlayShms>,
    toast: Option<&mut Toast>,
    sink: &dyn HostRenderSink,
    text: &str,
    position: ToastPosition,
    kind: ToastKind,
    duration_ms: i32,
    accent: Option<[u8; 4]>,
) {
    let ov = toast.and_then(|t| t.render_overlay(text, position, kind, accent));
    let mut shms = lock(shms);
    match ov {
        // Toast 恒有时长：0 在 Windows 侧等于「立即到期」，这里别让它变成常驻。
        Some(ov) => shms.show(sink, OVERLAY_KIND_TOAST, &ov, duration_ms.max(1)),
        None => shms.hide(sink, OVERLAY_KIND_TOAST),
    }
}

impl ToastHandle {
    pub(crate) fn show(
        self,
        sink: &dyn HostRenderSink,
        text: &str,
        position: ToastPosition,
        kind: ToastKind,
        duration_ms: i32,
        accent: Option<[u8; 4]>,
    ) {
        let mut toast = Toast::new()
            .map_err(|e| tracing::warn!("Toast 渲染器创建失败: {e}"))
            .ok();
        if let (Some(t), Some(theme)) = (&mut toast, &self.theme) {
            t.set_theme(theme);
        }
        push_toast(
            &self.shms,
            toast.as_mut(),
            sink,
            text,
            position,
            kind,
            duration_ms,
            accent,
        );
    }
}

impl Overlays {
    pub(crate) fn new(
        ev_tx: std::sync::mpsc::Sender<crate::manager::UiEvent>,
        suffix: String,
    ) -> Self {
        let status = StatusTip::new(ev_tx)
            .map_err(|e| tracing::warn!("状态气泡渲染器创建失败: {e}"))
            .ok();
        let toast = Toast::new()
            .map_err(|e| tracing::warn!("Toast 渲染器创建失败: {e}"))
            .ok();
        Self {
            shms: Arc::new(Mutex::new(OverlayShms::new(suffix))),
            status,
            toast,
            theme: None,
            cand_rect: None,
            last_status: None,
        }
    }

    /// 候选窗内容盒（`None` = 收起）。变了而跟随光标的状态泡还在显示，就按原参数重新落位，
    /// 到期时刻不变（见 docs/design/status-tip-avoid-candidates.md）。
    pub(crate) fn set_candidate_rect(
        &mut self,
        sink: &dyn HostRenderSink,
        r: Option<crate::status_tip::Rect4>,
    ) {
        if self.cand_rect == r {
            return;
        }
        self.cand_rect = r;
        if let Some(s) = &mut self.status {
            s.set_candidate_rect(r);
        }
        let Some((text, x, top, h, placement, until)) = self.last_status.clone() else {
            return;
        };
        if !matches!(placement, StatusTipPlacement::Caret { .. })
            || !self.is_shown(OVERLAY_KIND_STATUS)
        {
            return;
        }
        let dur = match until {
            None => 0,
            Some(t) => {
                let left = t.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return;
                }
                left.as_millis().max(1) as u64
            }
        };
        self.show_status(sink, &text, x, top, h, dur, placement);
    }

    pub(crate) fn toast_handle(&self) -> ToastHandle {
        ToastHandle {
            shms: Arc::clone(&self.shms),
            theme: self.theme.clone(),
        }
    }

    pub(crate) fn set_theme(&mut self, t: &wind_theme::Resolved) {
        if let Some(s) = &mut self.status {
            s.set_theme(t);
        }
        if let Some(s) = &mut self.toast {
            s.set_theme(t);
        }
        self.theme = Some(Box::new(t.clone()));
    }

    pub(crate) fn show_toast(
        &mut self,
        sink: &dyn HostRenderSink,
        text: &str,
        position: ToastPosition,
        kind: ToastKind,
        duration_ms: i32,
        accent: Option<[u8; 4]>,
    ) {
        push_toast(
            &self.shms,
            self.toast.as_mut(),
            sink,
            text,
            position,
            kind,
            duration_ms,
            accent,
        );
    }

    /// `(x, caret_top, caret_h)` 为 wire 光标（y 取行顶，同 macOS），跟随光标时换算到底端。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn show_status(
        &mut self,
        sink: &dyn HostRenderSink,
        text: &str,
        x: i32,
        caret_top: i32,
        caret_h: i32,
        duration_ms: u64,
        placement: StatusTipPlacement,
    ) {
        self.last_status = Some((
            text.to_string(),
            x,
            caret_top,
            caret_h,
            placement,
            (duration_ms > 0)
                .then(|| std::time::Instant::now() + std::time::Duration::from_millis(duration_ms)),
        ));
        let ov = self
            .status
            .as_mut()
            .and_then(|s| s.render_overlay(text, x, caret_top + caret_h, caret_h, placement));
        let mut shms = lock(&self.shms);
        match ov {
            Some(ov) => shms.show(
                sink,
                OVERLAY_KIND_STATUS,
                &ov,
                duration_ms.min(i32::MAX as u64) as i32,
            ),
            None => shms.hide(sink, OVERLAY_KIND_STATUS),
        }
    }

    pub(crate) fn hide(&self, sink: &dyn HostRenderSink, kind: u32) {
        lock(&self.shms).hide(sink, kind);
    }

    /// 见 [`OverlayShms::snapshot`]。
    pub(crate) fn snapshot(&self, kind: u32) -> Option<(Vec<u8>, u32, u32)> {
        lock(&self.shms).snapshot(kind)
    }

    /// 服务端认为该层此刻在显示（最近推的是可见帧）。
    pub(crate) fn is_shown(&self, kind: u32) -> bool {
        lock(&self.shms).shown[slot(kind)]
    }

    /// tooltip 一帧（渲染在候选窗里做）；`None` = 该藏。常驻到下一帧，不自动隐藏。
    pub(crate) fn show_tooltip(&self, sink: &dyn HostRenderSink, ov: Option<Overlay>) {
        let mut shms = lock(&self.shms);
        match ov {
            Some(ov) => shms.show(sink, OVERLAY_KIND_TOOLTIP, &ov, 0),
            None => shms.hide(sink, OVERLAY_KIND_TOOLTIP),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS 宿主坐标是点、位图是 scale 倍像素：像素长度折成点，反向取整互逆；Linux 恒不换算。
    #[test]
    fn host_units_follow_platform() {
        assert_eq!(px_per_unit_for(2.0, true), 2.0);
        assert_eq!(px_per_unit_for(0.5, true), 1.0, "缩放不会小于 1");
        assert_eq!(px_per_unit_for(2.0, false), 1.0);
        let k = px_per_unit_for(2.0, true);
        assert_eq!((61.0_f32 / k).round() as i32, 31);
        assert_eq!((30.0_f32 * k).round() as i32, 60);
    }

    #[test]
    fn caret_place_below_with_offset_and_flip_above_without() {
        // 光标底 120、高 20；偏移 (5, 7)；内容高 30；gap 4。
        let p = status_caret_place(100, 120, 20, (5, 7), 30, 4, false);
        assert_eq!(
            p,
            Place::Flip {
                x: 105,
                y: 131,
                alt_x: 105,
                alt_y: 120 - 20 - 30 - 4
            }
        );
    }

    #[test]
    fn caret_place_above_puts_offset_on_top_and_flips_below() {
        let p = status_caret_place(100, 120, 20, (5, 7), 30, 4, true);
        assert_eq!(
            p,
            Place::Flip {
                x: 105,
                y: 120 - 20 - 30 - 4 + 7,
                alt_x: 105,
                alt_y: 124
            }
        );
    }

    #[test]
    fn fixed_unset_sentinel_falls_back_to_caret() {
        assert_eq!(
            status_fixed_place(0, 0, 300, 400),
            Place::Absolute { x: 300, y: 400 }
        );
        assert_eq!(
            status_fixed_place(-800, 10, 300, 400),
            Place::Absolute { x: -800, y: 10 },
            "负坐标（左侧副屏）是合法落点，不是哨兵"
        );
    }

    #[test]
    fn window_anchors_degrade_to_screen_anchors() {
        assert_eq!(
            status_anchor_code(StatusTipAnchor::WindowCenter),
            status_anchor_code(StatusTipAnchor::ScreenCenter)
        );
        assert_eq!(
            status_anchor_code(StatusTipAnchor::WindowBottomLeft),
            OVERLAY_ANCHOR_BOTTOM_LEFT
        );
        assert_eq!(
            status_anchor_code(StatusTipAnchor::ScreenTopRight),
            OVERLAY_ANCHOR_TOP_RIGHT
        );
    }

    #[test]
    fn toast_positions_map_to_distinct_anchors() {
        let all = [
            ToastPosition::Center,
            ToastPosition::TopCenter,
            ToastPosition::BottomCenter,
            ToastPosition::TopLeft,
            ToastPosition::TopRight,
            ToastPosition::BottomLeft,
            ToastPosition::BottomRight,
        ];
        let mut codes: Vec<u32> = all.iter().map(|p| toast_anchor_code(*p)).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), all.len());
    }

    #[test]
    fn tooltip_below_row_flips_above_row() {
        let p = tooltip_place((50, 100, 150, 130), (80, 24), false);
        assert_eq!(
            p,
            Place::FollowCandidate {
                x: 50,
                y: 132,
                alt_x: 50,
                alt_y: 100 - 2 - 24
            }
        );
    }

    #[test]
    fn tooltip_beside_prefers_right_then_left() {
        let p = tooltip_place((50, 100, 150, 130), (80, 24), true);
        assert_eq!(
            p,
            Place::FollowCandidate {
                x: 154,
                y: 100,
                alt_x: 50 - 4 - 80,
                alt_y: 100
            }
        );
    }

    struct Cap(std::sync::Mutex<Vec<Vec<u8>>>);
    impl HostRenderSink for Cap {
        fn push_frame(&self, f: &[u8]) {
            self.0.lock().unwrap().push(f.to_vec());
        }
    }

    #[test]
    fn hide_without_show_sends_nothing_and_show_then_hide_sends_hidden_frame() {
        let cap = Cap(std::sync::Mutex::new(Vec::new()));
        // 名字带进程号，并发测试不撞段。
        let mut shms = OverlayShms::new(format!("_t{}", std::process::id()));
        shms.hide(&cap, OVERLAY_KIND_STATUS);
        assert!(cap.0.lock().unwrap().is_empty(), "没显示过就不该发隐藏帧");
        let ov = Overlay {
            buf: vec![255; 4 * 4 * 2],
            width: 4,
            height: 2,
            content_x: 0,
            content_y: 0,
            content_w: 4,
            content_h: 2,
            software_shadow: false,
            place: Place::Anchor {
                anchor: OVERLAY_ANCHOR_CENTER,
                margin: 16,
            },
            scale: 1.0,
        };
        shms.show(&cap, OVERLAY_KIND_STATUS, &ov, 1200);
        shms.hide(&cap, OVERLAY_KIND_STATUS);
        let frames = cap.0.lock().unwrap().clone();
        assert_eq!(frames.len(), 2);
        let u = |f: &[u8], o: usize| u32::from_le_bytes(f[8 + o..12 + o].try_into().unwrap());
        assert_eq!(u(&frames[0], 0), OVERLAY_KIND_STATUS);
        assert_ne!(u(&frames[0], 16) & SharedRenderHeader::FLAG_VISIBLE, 0);
        assert_eq!(u(&frames[0], 20), OVERLAY_PLACE_ANCHOR);
        assert_eq!(u(&frames[1], 16) & SharedRenderHeader::FLAG_VISIBLE, 0);
        assert!(u(&frames[1], 4) > u(&frames[0], 4), "隐藏帧也推进 seq");
    }

    /// 截图从 SHM 读回的就是推下去的那一帧；藏了就没有。
    #[test]
    fn snapshot_reads_back_shown_frame_only() {
        let cap = Cap(std::sync::Mutex::new(Vec::new()));
        let mut shms = OverlayShms::new(format!("_ts{}", std::process::id()));
        assert!(shms.snapshot(OVERLAY_KIND_TOOLTIP).is_none(), "没显示过");
        let buf: Vec<u8> = (0..3 * 2 * 4).map(|i| i as u8).collect();
        let ov = Overlay {
            buf: buf.clone(),
            width: 3,
            height: 2,
            content_x: 0,
            content_y: 0,
            content_w: 3,
            content_h: 2,
            software_shadow: false,
            place: Place::Absolute { x: 0, y: 0 },
            scale: 1.0,
        };
        shms.show(&cap, OVERLAY_KIND_TOOLTIP, &ov, 0);
        assert_eq!(shms.snapshot(OVERLAY_KIND_TOOLTIP), Some((buf, 3, 2)));
        assert!(shms.snapshot(OVERLAY_KIND_STATUS).is_none(), "各层各管各的");
        shms.hide(&cap, OVERLAY_KIND_TOOLTIP);
        assert!(shms.snapshot(OVERLAY_KIND_TOOLTIP).is_none(), "藏了就不给");
    }
}
