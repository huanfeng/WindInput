//! macOS host-render forwarder：把协调器下发的 `UiCommand` 转成 push 帧。
//!
//! Windows 侧 `ui_thread` 直接驱动 LayeredWindow 呈现；macOS 侧无进程内窗口，
//! 候选/工具栏/提示统一光栅化进 POSIX SHM，再经 push 管道通知 .app 端取帧呈现。
//! 本模块在 `ext_presenter` 下编译：macOS，以及 Linux 的 `linux-host` 形态（宿主是 Fcitx5 addon）。
//! 全局热键是 macOS 专属（Carbon），Linux 不做，命令落 `other` 臂。
//!
//! 工具栏 / 软键盘两平台分叉：macOS 是菜单栏状态图标 + 服务进程自开的 NSPanel；Linux 与 Windows
//! 一样是悬浮条与面板，`toolbar.rs` / `soft_keyboard.rs` 原样驱动，窗口壳换成 `crate::ext_panel`
//! （光栅化进 SHM、推浮层帧，addon 报回指针）。它们的计时（显隐迟滞、自动隐藏、按住连发）
//! 由 [`forwarder_thread`] 按最近的 deadline 醒来推进，与 Windows `ui_thread` 的 `next_deadline`
//! 同一套，见 [`Panels`]。
//!
//! 状态气泡 / Toast / 悬停提示 / 自绘菜单两平台同构：这里按主题光栅化，走 `CMD_OVERLAY_FRAME`
//! （见 `crate::overlay_host` / `crate::menu_host`），宿主只贴图、落位、计时、转发鼠标。

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use crate::candidate_window::{CandidateWindow, CandidateWindowConfig};
use crate::manager::{UiCommand, UiEvent};
#[cfg(target_os = "macos")]
use crate::softkeyboard_host_macos as sk;
use crate::toast::{ToastKind, ToastPosition};
use wind_bridge::HostRenderSink;
use wind_bridge::shared_memory_posix::PosixSharedMemory;
use wind_ipc::codec::*;
use wind_ipc::protocol::*;

const SHM_MAX: usize = MAX_SHARED_RENDER_SIZE;

/// 把截图的慢活扔到后台线程。
///
/// **forwarder 线程绝不能在这里阻塞**：它按序处理**全部** `UiCommand`（候选更新、主题、
/// 工具栏…），卡住多久，输入法就有多久不响应。而截图这条路上的三件事都不快：
///   · PNG 编码（候选窗 Retina 下上千像素宽）；
///   · 写临时文件；
///   · `copy_bgra_to_clipboard` 在 macOS 要 **spawn 一个 `osascript` 进程**（几十到
///     几百毫秒）——服务进程不链接 AppKit，拿不到 NSPasteboard，只能走这条外部路。
/// 三者叠起来实测能让候选窗肉眼可见地冻住。
///
/// 每次截图起一个线程（而不是常驻工作线程）是刻意的：截图是用户偶发动作，频率以分钟计，
/// 一个线程的创建成本完全淹没在上面那几十毫秒里，不值得为它引入一条队列和它的生命周期。
fn spawn_screenshot_work(tag: &'static str, work: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new()
        .name(format!("windinput-screenshot-{tag}"))
        .spawn(work)
    {
        // 起不了线程（资源耗尽）→ 本次截图丢弃。**不能退回同步执行**：`Builder::spawn`
        // 已经把闭包吃掉了，这里拿不回来；何况系统连线程都起不出来时，更不该由 forwarder
        // 线程去扛一个几百毫秒的 osascript。记一条 warn，用户会发现"截了没反应"时有据可查。
        tracing::warn!("截图线程创建失败({tag}): {e}，本次截图已放弃");
    }
}

pub struct Forwarder {
    win: CandidateWindow,
    shm: Option<PosixSharedMemory>,
    sink: Arc<dyn HostRenderSink>,
    suffix: String,
    /// 回协调器的事件通道（全局热键触发、右键悬停提示等）。
    ev_tx: Sender<UiEvent>,
    /// 候选窗当前是否有帧在显示。外观类命令（主题/字号/布局…）只在**显示中**才重推帧。
    visible: bool,
    /// 状态气泡 / Toast / tooltip 的光栅浮层。
    overlays: crate::overlay_host::Overlays,
    /// 自绘菜单（主菜单 / 候选右键菜单）。
    menu: crate::menu_host::MenuHost,
    /// 悬浮工具栏 / 软键盘（Linux）。
    #[cfg(all(target_os = "linux", ext_presenter))]
    panels: Panels,
}

/// Linux 外部宿主形态的悬浮工具栏与软键盘。
///
/// 各命令的语义逐条照 Windows `manager.rs` 的 `ui_thread`：工具栏过 `ToolbarGate` 的显隐迟滞，
/// 软键盘惰性创建并补上最近一次主题。不同处只有两点：
/// - 工具栏也惰性创建（第一条工具栏命令时）：窗口壳要等 `ext_panel::install` 之后才能推帧，
///   且没开工具栏的用户不必为它建一段 SHM。
/// - 全屏判据来自宿主上报（`SetHostFullscreen`），而不是渲染端自己探前台。
#[cfg(all(target_os = "linux", ext_presenter))]
struct Panels {
    ev_tx: Sender<UiEvent>,
    toolbar: Option<crate::toolbar::Toolbar>,
    soft_keyboard: Option<crate::soft_keyboard::SoftKeyboard>,
    gate: crate::toolbar_gate::ToolbarGate,
    /// 迟滞期间待显示的状态（同 Windows 的 `toolbar_pending_state`）。
    pending: Option<crate::toolbar::ToolbarState>,
    /// 最近一次该画出来的状态。全屏期间被挡下的显示不调 `Toolbar::update`，退出全屏补画时用它。
    last_state: Option<crate::toolbar::ToolbarState>,
    /// 闸门此刻判定工具栏「该显示」：立即渲染或迟滞到期之后、隐藏到期或撤销之前。
    wanted: bool,
    /// 宿主报告焦点窗口全屏。
    fullscreen: bool,
    /// 退出全屏时要不要补画：进入全屏那一刻工具栏可见，或全屏期间闸门到期本该显示。
    /// 只看 `wanted` 不够——自动隐藏收起的工具栏闸门仍判「该显示」，补画就是凭空闪一下。
    show_on_exit: bool,
    /// 最近一次主题：惰性创建的两个窗口靠它补课（`SetTheme` 一般只在启动时发一次）。
    last_theme: Option<wind_theme::Resolved>,
}

#[cfg(all(target_os = "linux", ext_presenter))]
impl Panels {
    fn new(ev_tx: Sender<UiEvent>) -> Self {
        Self {
            ev_tx,
            toolbar: None,
            soft_keyboard: None,
            gate: crate::toolbar_gate::ToolbarGate::new(),
            pending: None,
            last_state: None,
            wanted: false,
            fullscreen: false,
            show_on_exit: false,
            last_theme: None,
        }
    }

    fn toolbar(&mut self) -> Option<&mut crate::toolbar::Toolbar> {
        if self.toolbar.is_none() {
            match crate::toolbar::Toolbar::new(self.ev_tx.clone()) {
                Ok(mut t) => {
                    if let Some(theme) = &self.last_theme {
                        t.set_theme(theme);
                    }
                    self.toolbar = Some(t);
                }
                Err(e) => tracing::error!("工具栏创建失败: {e}"),
            }
        }
        self.toolbar.as_mut()
    }

    /// 真正把工具栏画出来；全屏期间改为隐藏（对位 Windows 的 `blocked_by_exclusive_fullscreen`）。
    fn render_toolbar(&mut self, state: crate::toolbar::ToolbarState) {
        let blocked = self.fullscreen;
        if blocked {
            tracing::debug!("forwarder: 工具栏不显示——宿主全屏");
            self.show_on_exit = true;
        }
        self.last_state = Some(state.clone());
        if let Some(t) = self.toolbar() {
            if blocked {
                t.hide();
            } else {
                t.update(&state);
            }
        }
    }

    fn update_toolbar(&mut self, state: crate::toolbar::ToolbarState) {
        tracing::debug!("forwarder: UpdateToolbar {state:?}");
        // 内容更新与可见性提升是两件事：已显示时切中英不该再等一档迟滞（见 toolbar_gate）。
        let visible = self.toolbar().is_some_and(|t| t.is_visible());
        match self.gate.on_update(std::time::Instant::now(), visible) {
            crate::toolbar_gate::UpdateAction::RenderNow => {
                self.wanted = true;
                self.pending = None;
                self.render_toolbar(state);
            }
            crate::toolbar_gate::UpdateAction::Deferred => {
                self.wanted = false;
                self.pending = Some(state);
            }
        }
    }

    fn hide_toolbar(&mut self) {
        match self.gate.on_hide(std::time::Instant::now()) {
            crate::toolbar_gate::HideAction::CancelledPending => {
                self.wanted = false;
                self.pending = None;
                tracing::debug!("forwarder: HideToolbar 撤销待显示工具栏（迟滞窗口内）");
            }
            crate::toolbar_gate::HideAction::Scheduled => {}
        }
    }

    fn set_fullscreen(&mut self, on: bool) {
        if self.fullscreen == on {
            return;
        }
        self.fullscreen = on;
        if on {
            self.show_on_exit = false;
            if let Some(t) = &mut self.toolbar {
                self.show_on_exit = t.is_seen();
                t.hide();
            }
        } else if self.wanted
            && self.show_on_exit
            && let Some(state) = self.last_state.clone()
        {
            self.render_toolbar(state);
        }
    }

    /// addon 的 push 连接（重）建立：此前宿主报上来的状态一律作废。新 addon（fcitx5 重启）各项
    /// 初值都是默认，与默认相同的不会再报；旧 addon 崩溃时报到一半的（按住的键、指针在面板里）
    /// 也没人再来收尾。⚠ 以后新增「宿主上报的状态」，都要在这里复位，并配测试。
    ///
    /// - 全屏 → 不全屏：addon 只在变化时报，仍全屏会补报（`FullscreenReporter::resyncAfterRestart`）。
    /// - 两块面板各补一次离开：停连发、收按下态与悬停、清「指针在面板里」。
    /// - 输出名 / 工作区（协调器的 `host_output` / `host_work`）不清：每次获焦随 host.display 重报，
    ///   旧值在重报前仍是同一台机器上最好的估计，清掉反而让默认落点退回无工作区的兜底。
    fn reset_host_state(&mut self) {
        use wind_ipc::protocol::overlay::{OVERLAY_KIND_SOFTKEYBOARD, OVERLAY_KIND_TOOLBAR};
        use wind_ipc::protocol::panel_pointer::PANEL_POINTER_LEAVE;
        self.set_fullscreen(false);
        for kind in [OVERLAY_KIND_TOOLBAR, OVERLAY_KIND_SOFTKEYBOARD] {
            crate::ext_panel::deliver_pointer(kind, PANEL_POINTER_LEAVE, 0, 0, 0);
        }
    }

    fn set_theme(&mut self, theme: &wind_theme::Resolved) {
        self.last_theme = Some(theme.clone());
        if let Some(t) = &mut self.toolbar {
            t.set_theme(theme);
            // 仅在可见时重绘：repaint→render 末尾必 show，会绕过闸门的显示迟滞。
            if t.is_visible() {
                t.repaint();
            }
        }
        if let Some(k) = &mut self.soft_keyboard {
            k.set_theme(theme);
        }
    }

    fn soft_keyboard(&mut self) -> Option<&mut crate::soft_keyboard::SoftKeyboard> {
        if self.soft_keyboard.is_none() {
            match crate::soft_keyboard::SoftKeyboard::new(self.ev_tx.clone()) {
                Ok(mut k) => {
                    if let Some(theme) = &self.last_theme {
                        k.set_theme(theme);
                    }
                    self.soft_keyboard = Some(k);
                }
                Err(e) => tracing::error!("软键盘窗口创建失败: {e}"),
            }
        }
        self.soft_keyboard.as_mut()
    }

    /// 宿主本地拖完：`ext_panel` 与窗口自己记下的位置一起改，下一帧不跳回旧处。
    fn moved(&mut self, kind: u32, x: i32, y: i32) {
        use wind_ipc::protocol::overlay::{OVERLAY_KIND_SOFTKEYBOARD, OVERLAY_KIND_TOOLBAR};
        crate::ext_panel::set_position(kind, x, y);
        match kind {
            OVERLAY_KIND_TOOLBAR => {
                if let Some(t) = &mut self.toolbar {
                    t.on_host_moved(x, y);
                }
            }
            OVERLAY_KIND_SOFTKEYBOARD => {
                if let Some(k) = &mut self.soft_keyboard {
                    k.on_host_moved(x, y);
                }
            }
            _ => tracing::debug!("forwarder: PanelMoved 未知 kind {kind}"),
        }
    }

    /// 推进计时与鼠标状态机产出的动作。同 Windows `ui_thread` 每轮做的那几步。
    fn tick(&mut self) {
        if self.gate.is_active() {
            match self.gate.tick_at(std::time::Instant::now()) {
                crate::toolbar_gate::GateTick::Show => {
                    tracing::debug!("forwarder: 工具栏显示（迟滞到期）");
                    self.wanted = true;
                    if let Some(state) = self.pending.take() {
                        self.render_toolbar(state);
                    }
                }
                crate::toolbar_gate::GateTick::Hide => {
                    self.wanted = false;
                    if let Some(t) = &mut self.toolbar {
                        t.hide();
                    }
                }
                crate::toolbar_gate::GateTick::None => {}
            }
        }
        if let Some(t) = &mut self.toolbar {
            t.tick();
        }
        if let Some(k) = &mut self.soft_keyboard {
            k.tick();
        }
    }

    /// 最近一次需要 [`Self::tick`] 的时刻。清单照 `manager.rs` 的 `next_deadline` 同类项，
    /// 漏登记一项，那件事就只在碰巧有命令到来时才推进。
    fn next_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        [
            // 工具栏显隐迟滞（50 / 120ms 两侧）
            self.gate.deadline(),
            // 工具栏自动隐藏（含淡出动画的逐帧推进）
            self.toolbar.as_ref().and_then(|t| t.next_deadline(now)),
            // 软键盘键帽的长按重复
            self.soft_keyboard.as_ref().and_then(|k| k.next_deadline()),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

impl Forwarder {
    pub fn new(ev_tx: Sender<UiEvent>, sink: Arc<dyn HostRenderSink>, suffix: String) -> Self {
        // CandidateWindow 在非 Windows 是纯光栅 mock，不产生鼠标事件；共用同一 tx 即可。
        let win = CandidateWindow::new(CandidateWindowConfig::default(), ev_tx.clone())
            .expect("create candidate window (mock/raster host)");
        let overlays = crate::overlay_host::Overlays::new(ev_tx.clone(), suffix.clone());
        let menu = crate::menu_host::MenuHost::new(ev_tx.clone(), suffix.clone());
        // 工具栏 / 软键盘的窗口壳在本线程推帧，先装上（它们惰性创建，见 `Panels`）。
        #[cfg(all(target_os = "linux", ext_presenter))]
        crate::ext_panel::install(Arc::clone(&sink), &suffix);
        #[cfg(all(target_os = "linux", ext_presenter))]
        let panels = Panels::new(ev_tx.clone());
        Self {
            win,
            shm: None,
            sink,
            suffix,
            ev_tx,
            visible: false,
            overlays,
            #[cfg(all(target_os = "linux", ext_presenter))]
            panels,
            menu,
        }
    }

    fn ensure_shm(&mut self) -> Option<&mut PosixSharedMemory> {
        if self.shm.is_none() {
            match PosixSharedMemory::create(&wind_bridge::endpoint::shm_name(&self.suffix), SHM_MAX)
            {
                Ok(s) => self.shm = Some(s),
                Err(e) => {
                    tracing::warn!("create SHM failed: {}", e);
                    return None;
                }
            }
        }
        self.shm.as_mut()
    }

    /// 该命令是否只改**外观**而不改内容。
    ///
    /// 这类命令在 Windows 上由窗口自己重绘，macOS 却是「渲染在服务进程、像素经 SHM 推给
    /// `.app`」——不主动重推一帧，已经显示着的候选窗就停在旧样子，直到下一次按键 / 鼠标
    /// 悬停触发 `UpdateCandidates` 才更新。表现为「菜单里换了主题要把鼠标移到候选项上才生效」。
    fn affects_appearance(cmd: &UiCommand) -> bool {
        matches!(
            cmd,
            UiCommand::SetTheme(_)
                | UiCommand::SetCandidateLayout { .. }
                | UiCommand::SetCandidateTextFamily(_)
                | UiCommand::SetPreeditEmbedded(_)
                | UiCommand::SetCandidateFontSize(_)
                | UiCommand::SetCandidateFont { .. }
                | UiCommand::SetCandidateMinSize { .. }
                | UiCommand::SetCandidateFlipWhenAbove(_)
                | UiCommand::SetCandidateSwapWhenAbove(_)
                | UiCommand::SetPagerInPreedit(_)
                | UiCommand::SetCandidateOffset { .. }
                | UiCommand::SetCandidateShadow(_)
                | UiCommand::SetPagerDisplay { .. }
                | UiCommand::SetPageNumberDisplay { .. }
        )
    }

    pub fn handle(&mut self, cmd: UiCommand) {
        let repaint = Self::affects_appearance(&cmd);
        self.handle_inner(cmd);
        // 只在显示中才重推：不可见时重推会把一个空窗口推上屏。
        if repaint && self.visible {
            self.push_current_frame();
        }
    }

    fn handle_inner(&mut self, cmd: UiCommand) {
        match cmd {
            UiCommand::UpdateCandidates {
                preedit,
                preedit_caret,
                preedit_host_owned,
                mode_label,
                candidates,
                selected,
                hover,
                page,
                total_pages,
                grid,
                caret_x,
                caret_y,
                caret_height,
                caret_valid,
                fixed,
                fixed_x,
                fixed_y,
            } => {
                tracing::debug!(
                    "forwarder UpdateCandidates: n={} preedit={:?} caret=({},{},{}) valid={}",
                    candidates.len(),
                    preedit,
                    caret_x,
                    caret_y,
                    caret_height,
                    caret_valid
                );
                // 编码区归宿主自绘时（`preedit_display = app_inline`），候选窗不重复画一遍
                // ——数据恒下发、显示与否看这个标志，与 Windows 侧 `manager.rs` 同一判据。
                let preedit = if preedit_host_owned {
                    String::new()
                } else {
                    preedit
                };
                self.win.set_grid(grid);
                self.win.update(
                    &preedit,
                    preedit_caret,
                    &mode_label,
                    candidates,
                    selected,
                    hover,
                    page,
                    total_pages,
                );
                self.win
                    .set_position(caret_x, caret_y, caret_height, caret_valid);
                // 固定位置：坐标由服务进程算定（`render_frame` 走 place_fixed 分支），
                // 帧里带 FLAG_ABSOLUTE_POS 告诉 `.app` 照搬、别再按光标翻转。
                self.win
                    .set_fixed_position(fixed.then_some((fixed_x, fixed_y)));
                self.push_current_frame();
            }
            UiCommand::HideCandidates => self.hide_frame(),
            // macOS 没有悬浮工具栏，状态进菜单栏图标；Linux 的悬浮工具栏见下方 `panels` 那组臂。
            #[cfg(target_os = "macos")]
            UiCommand::UpdateToolbar(s) => {
                let mut flags = STATUS_TOOLBAR_VISIBLE;
                if s.chinese_mode {
                    flags |= STATUS_CHINESE_MODE;
                }
                if s.full_width {
                    flags |= STATUS_FULL_WIDTH;
                }
                if s.chinese_punct {
                    flags |= STATUS_CHINESE_PUNCT;
                }
                let mode = if s.chinese_mode { 1 } else { 0 };
                self.sink
                    .push_frame(&encode_mode_status(flags, mode, &s.icon_label));
            }
            #[cfg(target_os = "macos")]
            UiCommand::HideToolbar => {
                self.sink.push_frame(&encode_mode_status(0, 0, ""));
            }
            // ── 悬浮工具栏（Linux）：语义逐条照 Windows `ui_thread`，见 `Panels` ──
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::UpdateToolbar(s) => self.panels.update_toolbar(s),
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::HideToolbar => self.panels.hide_toolbar(),
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::SetToolbarAnchor { right, bottom } => {
                if let Some(t) = self.panels.toolbar() {
                    t.set_anchor(right, bottom);
                }
            }
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::SetToolbarCorner {
                work_right,
                work_bottom,
            } => {
                if let Some(t) = self.panels.toolbar() {
                    t.set_corner(work_right, work_bottom);
                }
            }
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::SetToolbarAutoHide {
                enabled,
                delay_ms,
                hover_reveal,
            } => {
                if let Some(t) = self.panels.toolbar() {
                    t.set_auto_hide(enabled, delay_ms, hover_reveal);
                }
            }
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::SetToolbarVertical(v) => {
                if let Some(t) = self.panels.toolbar() {
                    t.set_vertical(v);
                }
            }
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::SetToolbarLayout(items) => {
                if let Some(t) = self.panels.toolbar() {
                    t.set_layout(items);
                }
            }
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::SetHostFullscreen(on) => self.panels.set_fullscreen(on),
            // addon 报上来的指针：翻译成 `WM_*` 交给该窗口的鼠标状态机；产出的点击 / 连发由
            // 主循环紧接着的 `tick` 消费。
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::PanelPointer {
                kind,
                event,
                button,
                x,
                y,
            } => crate::ext_panel::deliver_pointer(kind, event, button, x, y),
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::PanelMoved { kind, x, y } => self.panels.moved(kind, x, y),
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::HostReconnected => {
                self.panels.reset_host_state();
                crate::ext_panel::resend_visible();
            }
            UiCommand::ShowStatusTip {
                text,
                x,
                y,
                caret_height,
                duration_ms,
                placement,
            } => {
                // 光栅化成浮层帧，落位（含锚点）交宿主按屏幕几何做。
                self.overlays.show_status(
                    &*self.sink,
                    &text,
                    x,
                    y,
                    caret_height,
                    duration_ms,
                    placement,
                );
            }
            UiCommand::HideStatusTip => {
                self.overlays.hide(
                    &*self.sink,
                    wind_ipc::protocol::overlay::OVERLAY_KIND_STATUS,
                );
            }
            UiCommand::ShowToast {
                text,
                position,
                kind,
                duration_ms,
                accent,
            } => self.push_toast(&text, position, kind, duration_ms as i32, accent),
            UiCommand::SetTheme(t) => {
                // 面板是惰性建窗的，主题得在本模块之外留一份底，理由见 `SkCmd::Theme`。
                #[cfg(target_os = "macos")]
                self.push_softkeyboard(sk::SkCmd::Theme(t.clone()));
                #[cfg(all(target_os = "linux", ext_presenter))]
                self.panels.set_theme(&t);
                self.overlays.set_theme(&t);
                self.menu.set_theme(&*self.sink, &t);
                self.win.set_theme(*t);
            }
            UiCommand::SetCandidateTextFamily(f) => self.win.set_text_family_override(&f),
            UiCommand::SetCandidateLayout {
                vertical,
                rotated,
                upright,
            } => self.win.set_orientation(vertical, rotated, upright),
            UiCommand::SetPreeditEmbedded(v) => self.win.set_preedit_embedded(v),
            UiCommand::SetCandidateFontSize(s) => self.win.set_font_size_override(s),
            // 两步顺序同 Windows 侧 manager：链首必须是刚设进去的那个字族。
            // ⚠️ CoreText 后端只实现了回退链那一半，脚本指派尚未实现（见 coretext.rs 的
            // `plan` 字段说明）——这里照常下发，是为了让两平台的**配置通路**一致，
            // 补上 macOS 侧渲染时不必再改接线。
            UiCommand::SetCandidateFont {
                family,
                fallback,
                scripts,
                weight,
            } => {
                self.win.set_font_family(&family);
                self.win.set_font_plan(&family, &fallback, &scripts);
                self.win.set_font_weight(weight);
            }
            UiCommand::SetCandidateMinSize {
                width_horizontal,
                width_vertical,
                height_horizontal,
                height_vertical,
                rows,
            } => self.win.set_min_size(
                width_horizontal,
                width_vertical,
                height_horizontal,
                height_vertical,
                rows,
            ),
            UiCommand::SetTooltipDelay(d) => self.win.set_tooltip_delay(d),
            UiCommand::SetCandidateFlipWhenAbove(v) => self.win.set_flip_when_above(v),
            UiCommand::SetCandidateSwapWhenAbove(v) => self.win.set_swap_preedit_when_above(v),
            UiCommand::SetCandidateOffset { x, y } => self.win.set_user_offset(x, y),
            UiCommand::SetCandidateShadow(on) => self.win.set_theme_shadow(on),
            UiCommand::SetPagerInPreedit(v) => self.win.set_pager_in_preedit(v),
            UiCommand::SetPagerDisplay { h, v } => self.win.set_pager_display(h, v),
            UiCommand::SetPageNumberDisplay { h, v } => self.win.set_page_number_display(h, v),
            UiCommand::SetTooltipChaiziFont { path, family } => {
                self.win.set_chaizi_font(&path, &family)
            }
            // 全局热键：macOS 走 Carbon；Linux 无对应通路（Wayland 无标准全局热键），落 `other` 臂。
            #[cfg(target_os = "macos")]
            UiCommand::RegisterGlobalHotkeys(entries) => {
                // 只入队 + 唤醒主线程；真正的 Carbon 注册在主线程做（见该模块头「线程约定」）。
                crate::global_hotkey_macos::apply(entries, self.ev_tx.clone());
            }
            // 「截图所有窗口」：截**我们自己渲染的**每一个可见浮窗——候选窗与各浮层的像素都在
            // 本进程（后者在各自的 SHM 段里），就地编码存盘。
            //
            // 菜单不截：菜单开着时用户正点着菜单项，而「截图所有窗口」本身就是菜单项。
            UiCommand::TakeScreenshot { dir } => {
                use wind_ipc::protocol::overlay::*;
                let dir = std::path::PathBuf::from(&dir);
                let ts = crate::screenshot::timestamp();
                // 取像素要 `&self`（读候选窗当前帧 / 浮层 SHM），必须在本线程；**其余全部挪走**，
                // 理由见 `spawn_screenshot_work`。
                let candidate = self.capture_candidate();
                let overlays = [
                    ("status_tip", OVERLAY_KIND_STATUS),
                    ("tooltip", OVERLAY_KIND_TOOLTIP),
                    ("toast", OVERLAY_KIND_TOAST),
                ]
                .map(|(tag, kind)| (tag, self.overlays.snapshot(kind)));
                let sink = Arc::clone(&self.sink);
                let toast = self.overlays.toast_handle();
                spawn_screenshot_work("take", move || {
                    let (mut saved, mut clipboard) = (0usize, false);
                    if let Some((buf, w, h)) = candidate {
                        let path = dir.join(format!("candidate_{ts}.png"));
                        match crate::screenshot::save_bgra_to_png(&buf, w, h, &path) {
                            Ok(()) => {
                                tracing::info!("Screenshot saved: {:?}", path);
                                saved += 1;
                                // 存盘同时进剪贴板（对齐 Windows）：截完直接能粘贴，省去翻目录。
                                // 剪贴板失败不影响"已存盘"这个既成事实，只在文案里说明。
                                match crate::screenshot::copy_bgra_to_clipboard(&buf, w, h) {
                                    Ok(()) => clipboard = true,
                                    Err(e) => tracing::warn!("截图进剪贴板失败: {e}"),
                                }
                            }
                            Err(e) => tracing::warn!("截图存盘失败: {e}"),
                        }
                    }
                    for (tag, shot) in overlays {
                        let Some((buf, w, h)) = shot else { continue };
                        let path = dir.join(format!("{tag}_{ts}.png"));
                        match crate::screenshot::save_bgra_to_png(&buf, w, h, &path) {
                            Ok(()) => {
                                tracing::info!("Screenshot saved: {:?}", path);
                                saved += 1;
                            }
                            Err(e) => tracing::warn!("截图存盘失败（{tag}）: {e}"),
                        }
                    }
                    // 文案与 Windows `manager.rs` 的对应分支一致，两平台同一操作不该有不同措辞。
                    let (msg, kind) = match (saved, clipboard) {
                        (0, _) => ("没有可见窗口可截图".to_string(), ToastKind::Info),
                        (n, true) => (
                            format!("已保存 {n} 张截图（候选已复制到剪贴板）\n{}", dir.display()),
                            ToastKind::Success,
                        ),
                        (n, false) => (
                            format!("已保存 {n} 张截图\n{}", dir.display()),
                            ToastKind::Success,
                        ),
                    };
                    toast.show(&*sink, &msg, ToastPosition::BottomRight, kind, 4000, None);
                });
            }
            UiCommand::ScreenshotCandidateToClipboard => {
                let shot = self.capture_candidate();
                let sink = Arc::clone(&self.sink);
                let toast = self.overlays.toast_handle();
                spawn_screenshot_work("clip", move || {
                    let (msg, kind) = match shot {
                        Some((buf, w, h)) => {
                            match crate::screenshot::copy_bgra_to_clipboard(&buf, w, h) {
                                Ok(()) => {
                                    tracing::info!("Candidate screenshot copied to clipboard");
                                    ("候选窗口已截图到剪贴板".to_string(), ToastKind::Success)
                                }
                                Err(e) => {
                                    tracing::warn!("Screenshot to clipboard failed: {e}");
                                    (format!("截图到剪贴板失败：{e}"), ToastKind::Error)
                                }
                            }
                        }
                        None => ("候选窗口未显示，无法截图".to_string(), ToastKind::Info),
                    };
                    toast.show(&*sink, &msg, ToastPosition::BottomRight, kind, 3000, None);
                });
            }
            UiCommand::CopyTooltipText(text) => {
                // 文本由协调器按原始行拼好随命令下发，复制无需 .app 参与。
                let (msg, kind) = match Some(text.as_str()).filter(|s| !s.is_empty()) {
                    Some(t) => {
                        crate::popup_menu::set_clipboard_text(t);
                        ("提示内容已复制".to_string(), ToastKind::Success)
                    }
                    None => ("提示内容为空，无法复制".to_string(), ToastKind::Info),
                };
                self.push_result_toast(&msg, kind);
            }
            // 状态气泡 / 悬停提示的截图：像素在本进程（该层 SHM），就地截（同候选窗）。
            UiCommand::ScreenshotStatusTip { dir } => self.shoot_overlay(
                wind_ipc::protocol::overlay::OVERLAY_KIND_STATUS,
                "status_tip",
                "状态提示气泡",
                &dir,
            ),
            UiCommand::ScreenshotTooltip { dir } => self.shoot_overlay(
                wind_ipc::protocol::overlay::OVERLAY_KIND_TOOLTIP,
                "tooltip",
                "提示气泡",
                &dir,
            ),
            // 协调器把定位方式切到 fixed 时问「你现在在哪」，好把当前位置落盘成 custom_x/y
            // ——否则窗口会跳到上次保存（往往是 0,0）的坐标。
            //
            // 两者都转成一次**下行询问**而不是在本进程记账：浮窗的落位在 `.app` 侧，
            // 服务进程推下去的只是建议落点，实际位置还会被那边的屏幕钳制 / 下方放不下时上翻 /
            // 用户本次组合内的拖动落位改掉。答案经上行 `pos.*` 回来（见 `handle_ext`）。
            UiCommand::ReportCandidatePos => {
                self.sink
                    .push_frame(&encode_ext(ext_kind::POS_CANDIDATE_QUERY, b""));
            }
            UiCommand::ReportStatusTipPos => {
                self.sink
                    .push_frame(&encode_ext(ext_kind::POS_STATUS_TIP_QUERY, b""));
            }
            // ── 软键盘面板 ──
            //
            // 与其余浮窗相反，它**不走 SHM + `.app` 那条路**：面板由服务进程自己开窗
            // （macOS 上唯一一处），理由见 `crate::mac_panel` 模块头。这里只负责把命令
            // 从本工作线程转运到主线程——AppKit 碰不得工作线程。
            // Linux 的软键盘是 `ext_panel` 窗口壳，见下面那组 Linux 臂。
            #[cfg(target_os = "macos")]
            UiCommand::ShowSoftKeyboard {
                pages,
                current,
                keys,
                send_keys,
                // 多屏摆放依据,macOS 上用不到:协调器的 `focus_monitor()` 在
                // `#[cfg(not(target_os = "windows"))]` 下恒返回 `None`,于是
                // `softkeyboard_placement()` 走早返回分支,这两个字段恒是 `None`。
                // 面板位置由 `mac_panel` 自己定,不经这条路。
                //
                // ⚠️ 刻意逐个写出而不用 `..`:用 `..` 的话,将来 `ShowSoftKeyboard`
                //    再加 macOS 也该处理的字段会被**静默忽略**——而本行当初漏掉这
                //    两个字段正是同一类事故(编译期报错才把它暴露出来)。显式列出,
                //    下次加字段时这里会再次编译失败,强制过一遍 review。
                anchor: _,
                work_area: _,
            } => self.push_softkeyboard(sk::SkCmd::Show {
                pages,
                current,
                keys,
                send_keys,
            }),
            #[cfg(target_os = "macos")]
            UiCommand::HideSoftKeyboard => self.push_softkeyboard(sk::SkCmd::Hide),
            #[cfg(target_os = "macos")]
            UiCommand::SoftKeyboardKeyState { slot, down } => {
                self.push_softkeyboard(sk::SkCmd::KeyState { slot, down })
            }
            #[cfg(target_os = "macos")]
            UiCommand::SoftKeyboardLayer { shift } => {
                self.push_softkeyboard(sk::SkCmd::Layer { shift })
            }
            // ── 软键盘（Linux）：同 Windows `ui_thread`，惰性创建 ──
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::ShowSoftKeyboard {
                pages,
                current,
                keys,
                send_keys,
                anchor,
                work_area,
            } => {
                tracing::debug!(
                    "forwarder: ShowSoftKeyboard (page={current}, keys={})",
                    keys.len()
                );
                if let Some(k) = self.panels.soft_keyboard() {
                    k.set_placement(anchor, work_area);
                    k.show(pages, current, keys, send_keys);
                }
            }
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::HideSoftKeyboard => {
                if let Some(k) = &mut self.panels.soft_keyboard {
                    k.hide();
                }
            }
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::SoftKeyboardKeyState { slot, down } => {
                if let Some(k) = &mut self.panels.soft_keyboard {
                    k.set_key_down(&slot, down);
                }
            }
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::SoftKeyboardLayer { shift } => {
                if let Some(k) = &mut self.panels.soft_keyboard {
                    k.set_layer(shift);
                }
            }
            UiCommand::OpenPath(path) => crate::manager::open_path(&path),
            UiCommand::OpenApp { path, args } => crate::manager::open_app(&path, &args),
            UiCommand::Shutdown => {}
            #[cfg(target_os = "macos")]
            UiCommand::CopyToClipboard(text) => crate::popup_menu::set_clipboard_text(&text),
            // Linux 的剪贴板靠外部命令（wl-copy / xclip / xsel），缺工具是常态而不是意外：
            // 静默失败的话用户点了「复制」毫无反应，故失败时弹 Toast 说清楚缺什么。
            #[cfg(all(target_os = "linux", ext_presenter))]
            UiCommand::CopyToClipboard(text) => {
                if let Err(e) = crate::popup_menu::try_set_clipboard_text(&text) {
                    tracing::warn!("写剪贴板失败: {e}");
                    self.push_result_toast(&format!("复制失败：{e}"), ToastKind::Error);
                }
            }
            // ── 自绘菜单（见 `crate::menu_host`）──
            UiCommand::ShowCandidateMenu { items, anchor } => {
                self.menu.show(&*self.sink, items, anchor)
            }
            UiCommand::MenuKey(vk) => self.menu.key(&*self.sink, vk),
            UiCommand::HideMenu => self.menu.hide(&*self.sink),
            UiCommand::MenuPointer { event, x, y } => self.menu.pointer(&*self.sink, event, x, y),
            UiCommand::SetWorkArea {
                left,
                top,
                right,
                bottom,
            } => self.menu.set_work_area(left, top, right, bottom),
            // 右键悬停提示：命中按本进程最近一次画的那一帧算（与 Windows 气泡自己收右键同一套），
            // 回 `RequestTooltipMenu` 让协调器核对指纹、弹菜单。提示已不在屏上（addon 报上来
            // 的途中被收了）就不弹——菜单会指着一个看不见的东西。
            UiCommand::TooltipMenuAt {
                x,
                y,
                local_x,
                local_y,
            } => {
                let shown = self
                    .overlays
                    .is_shown(wind_ipc::protocol::overlay::OVERLAY_KIND_TOOLTIP);
                match self.win.tooltip_menu_request_at(local_x, local_y) {
                    Some((candidate, hit, doc_fingerprint)) if shown => {
                        let _ = self.ev_tx.send(UiEvent::RequestTooltipMenu {
                            x,
                            y,
                            candidate,
                            hit,
                            doc_fingerprint,
                        });
                    }
                    _ => tracing::debug!("右键悬停提示时提示已不在屏上，不弹菜单"),
                }
            }
            // 气泡 / 提示「菜单开着时别自动消失」：Linux 上这份保持由 addon 自己做（它知道菜单
            // 开没开、指针在哪，自动隐藏的计时也在它那边），服务端没有要跟着动的状态。
            UiCommand::SetTooltipMenuOpen(_) | UiCommand::SetStatusMenuOpen(_) => {}
            // 其余未接的变体（截图族 / 输入诊断 HUD / 拖动落点回报 / 候选右键菜单键盘
            // 导航 / 工具栏位置）见 wind_macos/AGENTS.md「与 Windows 的功能差距」表。
            // 新接一个就从那张表里划掉一行。
            other => {
                tracing::debug!("forwarder: 暂未处理 {:?}", std::mem::discriminant(&other));
            }
        }
    }

    /// 推进工具栏 / 软键盘的计时与鼠标动作（主循环每轮一次）。
    #[cfg(all(target_os = "linux", ext_presenter))]
    fn tick(&mut self) {
        self.panels.tick();
    }

    /// 主循环最多睡到这一刻；`None` = 只等命令。
    #[cfg(all(target_os = "linux", ext_presenter))]
    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.panels.next_deadline(std::time::Instant::now())
    }

    /// 把一条软键盘命令转运给主线程宿主。
    #[cfg(target_os = "macos")]
    fn push_softkeyboard(&self, cmd: sk::SkCmd) {
        sk::apply(cmd, &self.ev_tx);
    }

    /// 推一条 toast：光栅化成浮层帧。
    fn push_toast(
        &mut self,
        text: &str,
        position: ToastPosition,
        kind: ToastKind,
        duration_ms: i32,
        accent: Option<[u8; 4]>,
    ) {
        self.overlays
            .show_toast(&*self.sink, text, position, kind, duration_ms, accent);
    }

    /// 截图/复制类操作的结果反馈：右下角 toast，3 秒（与 Windows 侧同一形态）。
    fn push_result_toast(&mut self, text: &str, kind: ToastKind) {
        self.push_toast(text, ToastPosition::BottomRight, kind, 3000, None);
    }

    /// 取候选窗当前帧的像素（BGRA + 尺寸）。窗口未显示时返回 `None`。
    ///
    /// macOS 的候选窗像素本来就在服务进程里（我们光栅化后经 SHM 推给 `.app`），故截图
    /// 无需 `.app` 参与——直接把同一份 buffer 编码存盘即可。状态气泡 / 悬停
    /// 提示 / Toast / 菜单的像素同样在本进程（各层 SHM），见 `shoot_overlay`。
    fn capture_candidate(&mut self) -> Option<(Vec<u8>, u32, u32)> {
        if !self.visible {
            return None;
        }
        let f = self.win.render_frame()?;
        Some((f.buf, f.width, f.height))
    }

    /// 状态气泡 / 悬停提示右键菜单里的「截图此窗口」。像素在本进程（该层 SHM），与
    /// 候选窗截图同一路数：存盘 + 进剪贴板在后台线程做（外部命令可能卡），文件名与 Toast
    /// 文案与 Windows `manager.rs` 的同名分支逐字一致。
    fn shoot_overlay(
        &self,
        kind: u32,
        file_tag: &'static str,
        label: &'static str,
        dir: &std::path::Path,
    ) {
        let shot = self.overlays.snapshot(kind);
        let path = dir.join(format!("{file_tag}_{}.png", crate::screenshot::timestamp()));
        let sink = Arc::clone(&self.sink);
        let toast = self.overlays.toast_handle();
        spawn_screenshot_work(file_tag, move || {
            let (msg, kind) = match shot {
                Some((buf, w, h)) => match crate::screenshot::save_bgra_to_png(&buf, w, h, &path) {
                    Ok(()) => {
                        tracing::info!("Screenshot saved: {:?}", path);
                        let clip = crate::screenshot::copy_bgra_to_clipboard(&buf, w, h);
                        if let Err(e) = &clip {
                            tracing::warn!("Screenshot {file_tag} clipboard: {e}");
                        }
                        let suffix = if clip.is_ok() {
                            "（已复制到剪贴板）"
                        } else {
                            ""
                        };
                        (
                            format!("{label}已截图{suffix}\n{}", path.display()),
                            ToastKind::Success,
                        )
                    }
                    Err(e) => {
                        tracing::warn!("Screenshot {file_tag}: {e}");
                        (format!("截图失败：{e}"), ToastKind::Error)
                    }
                },
                None => (format!("{label}未显示，无法截图"), ToastKind::Info),
            };
            toast.show(&*sink, &msg, ToastPosition::BottomRight, kind, 3000, None);
        });
    }

    /// 按 `win` 的当前状态渲染一帧并推给 `.app`（像素走 SHM，元数据走 push 管道）。
    ///
    /// 内容更新（`UpdateCandidates`）与纯外观变更（换主题/字号…）共用此路径——后者若不
    /// 走这里重推一帧，显示中的候选窗就会停在旧样子。
    fn push_current_frame(&mut self) {
        match self.win.render_frame() {
            Some(f) => {
                let (sx, sy, w, h, scale, soft, absolute) = (
                    f.screen_x,
                    f.screen_y,
                    f.width,
                    f.height,
                    f.scale,
                    f.software_shadow,
                    f.absolute_pos,
                );
                // 翻页器命中矩形的内部 tag(HOVER_PAGE_PREV/NEXT=100000/100001)重映射为
                // Swift/Go 约定的 -1(上页)/-2(下页)，否则 100000>=0 会被 .app 误当候选选中
                // (index 100000) → 翻页失效；候选 tag(>=0)原样。对齐 Go forwarder_darwin。
                let rects: Vec<(i32, i32, i32, i32, i32)> = f
                    .hit_rects
                    .iter()
                    .map(|(i, r)| {
                        let wire = if *i == crate::manager::HOVER_PAGE_PREV {
                            -1
                        } else if *i == crate::manager::HOVER_PAGE_NEXT {
                            -2
                        } else {
                            *i
                        };
                        (wire, r.x as i32, r.y as i32, r.w as i32, r.h as i32)
                    })
                    .collect();
                let buf = f.buf;
                // 先写 SHM 像素并取 seq；shm 建失败则整帧放弃——
                // 不能只推命中矩形/tooltip 而无底帧，否则 .app 拿到无像素的命中区（不一致）。
                let seq = match self.ensure_shm() {
                    Some(shm) => shm.write_frame(sx, sy, w, h, &buf),
                    None => return,
                };
                let mut flags =
                    SharedRenderHeader::FLAG_VISIBLE | SharedRenderHeader::FLAG_CONTENT_READY;
                if soft {
                    flags |= SharedRenderHeader::FLAG_SOFTWARE_SHADOW;
                }
                if absolute {
                    flags |= SharedRenderHeader::FLAG_ABSOLUTE_POS;
                }
                self.sink.push_frame(&encode_host_render_frame(
                    seq,
                    sx,
                    sy,
                    w,
                    h,
                    flags,
                    scale.round().max(1.0) as u32,
                ));
                self.sink.push_frame(&encode_candidate_rects(&rects));
                // tooltip 在本进程光栅化，坐标按候选窗建议落点算（宿主平移到实际位置）。
                // 悬停目标取自候选窗自己的 hover 状态。
                let ov = self.win.render_tooltip_overlay(sx, sy);
                self.overlays.show_tooltip(&*self.sink, ov);
                self.visible = true;
                let rect = self.win.screen_rect();
                self.overlays.set_candidate_rect(&*self.sink, rect);
                tracing::debug!(
                    "forwarder pushed host-render frame seq={} {}x{} at ({},{}) scale={}",
                    seq,
                    w,
                    h,
                    sx,
                    sy,
                    scale
                );
            }
            None => {
                tracing::debug!("forwarder render_frame=None → hide");
                self.hide_frame();
            }
        }
    }

    fn hide_frame(&mut self) {
        // 无论 SHM 建没建起来都得落 visible=false：否则外观类命令会对着一个已经隐藏的
        // 候选窗重推帧，把它又推回屏幕上。
        self.visible = false;
        self.overlays.hide(
            &*self.sink,
            wind_ipc::protocol::overlay::OVERLAY_KIND_TOOLTIP,
        );
        self.overlays.set_candidate_rect(&*self.sink, None);
        if let Some(shm) = self.shm.as_mut() {
            let seq = shm.write_hidden();
            self.sink
                .push_frame(&encode_host_render_frame(seq, 0, 0, 0, 0, 0, 1));
        }
    }
}

pub fn forwarder_thread(
    rx: Receiver<UiCommand>,
    ev_tx: Sender<UiEvent>,
    sink: Arc<dyn HostRenderSink>,
    suffix: String,
) {
    let mut fwd = Forwarder::new(ev_tx, sink, suffix);
    tracing::info!("macOS host-render forwarder started");
    run_loop(&mut fwd, &rx);
}

/// forwarder 主循环，收到 `Shutdown` 或发送端全部断开即返回。
///
/// 不是裸 `for cmd in rx`：那样线程永远阻塞在 recv 上，背景图缓存的闲置回收
/// （`manager.rs` 的消息循环负责的那件事）就一次都不会发生。
#[cfg(target_os = "macos")]
fn run_loop(fwd: &mut Forwarder, rx: &Receiver<UiCommand>) {
    while let Some(cmd) = crate::view::recv_evicting_idle(rx) {
        if matches!(cmd, UiCommand::Shutdown) {
            break;
        }
        fwd.handle(cmd);
    }
}

/// Linux 多一件事：工具栏 / 软键盘有计时器，等命令时最多睡到最近的 deadline，醒来 `tick`。
/// 闲置回收照旧挂在等待上（`recv_evicting_idle_until`）。
#[cfg(all(target_os = "linux", ext_presenter))]
fn run_loop(fwd: &mut Forwarder, rx: &Receiver<UiCommand>) {
    use crate::view::Recv;
    loop {
        fwd.tick();
        match crate::view::recv_evicting_idle_until(rx, fwd.next_deadline()) {
            Recv::Cmd(UiCommand::Shutdown) | Recv::Disconnected => break,
            Recv::Cmd(cmd) => fwd.handle(cmd),
            Recv::Timeout => {}
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::candidate_window::CandidateItem;
    use std::sync::{Arc, Mutex};

    struct CapSink(Arc<Mutex<Vec<Vec<u8>>>>);
    impl wind_bridge::HostRenderSink for CapSink {
        fn push_frame(&self, f: &[u8]) {
            self.0.lock().unwrap().push(f.to_vec());
        }
    }
    fn cmd_of(f: &[u8]) -> u16 {
        u16::from_le_bytes([f[2], f[3]])
    }
    fn item(t: &str) -> CandidateItem {
        CandidateItem {
            text_styled: Default::default(),
            text: t.into(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: Default::default(),
            comment_above: Default::default(),
            no_index: false,
        }
    }
    /// 事件通道的接收端在测试里不消费，但必须**持有**——drop 掉会让 forwarder 里的
    /// `ev_tx.send` 立刻报错。故连同 Forwarder 一起返回。
    fn mk(
        cap: Arc<Mutex<Vec<Vec<u8>>>>,
        suffix: &str,
    ) -> (Forwarder, std::sync::mpsc::Receiver<UiEvent>) {
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        (
            Forwarder::new(ev_tx, Arc::new(CapSink(cap)), suffix.into()),
            ev_rx,
        )
    }

    #[test]
    fn update_candidates_emits_frame_and_rects() {
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t1");
        f.handle(UiCommand::UpdateCandidates {
            preedit: "a".into(),
            preedit_caret: 1,
            preedit_host_owned: false,
            mode_label: "".into(),
            candidates: vec![item("中"), item("国")],
            selected: 0,
            hover: -1,
            page: 1,
            total_pages: 1,
            grid: None,
            caret_x: 100,
            caret_y: 200,
            caret_height: 20,
            caret_valid: true,
            fixed: false,
            fixed_x: 0,
            fixed_y: 0,
        });
        let v = cap.lock().unwrap();
        assert!(
            v.iter()
                .any(|x| cmd_of(x) == wind_ipc::protocol::CMD_HOST_RENDER_FRAME)
        );
        assert!(
            v.iter()
                .any(|x| cmd_of(x) == wind_ipc::protocol::CMD_CANDIDATE_RECTS)
        );
    }

    /// 造一条最小的 UpdateCandidates，供需要「先显示一帧」的用例复用。
    /// 取最后一帧 `CMD_HOST_RENDER_FRAME` 的 `(x, y, flags)`。
    fn last_render_frame(cap: &Arc<Mutex<Vec<Vec<u8>>>>) -> Option<(i32, i32, u32)> {
        let v = cap.lock().unwrap();
        let f = v
            .iter()
            .rev()
            .find(|f| cmd_of(f) == CMD_HOST_RENDER_FRAME)?;
        let p = &f[8..];
        Some((
            i32::from_le_bytes(p[4..8].try_into().unwrap()),
            i32::from_le_bytes(p[8..12].try_into().unwrap()),
            u32::from_le_bytes(p[20..24].try_into().unwrap()),
        ))
    }

    fn show_two_fixed(f: &mut Forwarder, fixed: bool, fx: i32, fy: i32) {
        f.handle(UiCommand::UpdateCandidates {
            preedit: "a".into(),
            preedit_caret: 1,
            preedit_host_owned: false,
            mode_label: "".into(),
            candidates: vec![item("中"), item("国")],
            selected: 0,
            hover: -1,
            page: 1,
            total_pages: 1,
            grid: None,
            caret_x: 100,
            caret_y: 200,
            caret_height: 20,
            caret_valid: true,
            fixed,
            fixed_x: fx,
            fixed_y: fy,
        });
    }

    /// 固定位置模式：帧坐标就是配置里的 custom_x/y，且带 FLAG_ABSOLUTE_POS。
    ///
    /// 回归：这条路径此前被 `fixed: _` 显式丢弃——「候选窗固定位置」在 macOS 上整个是死的，
    /// 设置里改了没有任何反应。标志位不能省：`.app` 收到普通帧会自作主张做「下方放不下就
    /// 翻到光标上方」，固定点靠近屏幕底边时窗口会被莫名弹到顶上。
    #[test]
    fn fixed_position_frame_carries_absolute_flag_and_coords() {
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t_fixed");
        show_two_fixed(&mut f, true, 640, 480);

        let (x, y, flags) = last_render_frame(&cap).expect("应推出 host render frame");
        assert_eq!((x, y), (640, 480), "固定位置必须原样下发，不再按光标推算");
        assert_ne!(
            flags & SharedRenderHeader::FLAG_ABSOLUTE_POS,
            0,
            "缺 FLAG_ABSOLUTE_POS，.app 会对固定位置套用光标翻转逻辑"
        );
    }

    /// 跟随光标模式不得置 FLAG_ABSOLUTE_POS——否则 `.app` 不再做上翻，光标贴屏幕底边时
    /// 候选窗被钳在底边糊住输入位。
    #[test]
    fn follow_caret_frame_has_no_absolute_flag() {
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t_follow");
        show_two_fixed(&mut f, false, 640, 480);

        let (_, _, flags) = last_render_frame(&cap).expect("应推出 host render frame");
        assert_eq!(flags & SharedRenderHeader::FLAG_ABSOLUTE_POS, 0);
    }

    /// `ReportCandidatePos` / `ReportStatusTipPos`（协调器把定位方式切到 fixed 时问「你现在
    /// 在哪」）在 macOS 只能转成一次下行询问 —— 浮窗是 `.app` 侧的 NSPanel，服务进程推下去
    /// 的只是建议落点，实际位置还会被那边的屏幕钳制 / 上翻 / 拖动落位改掉。
    ///
    /// 回归：这里一度改用「记住最后一帧推的坐标」直接回答，省掉一次往返。那个值在窗口被
    /// 上翻或拖动过之后就是错的，落盘后窗口会摆到一个它从没出现过的地方。
    #[test]
    fn report_pos_sends_query_downstream() {
        for (cmd, want) in [
            (UiCommand::ReportCandidatePos, ext_kind::POS_CANDIDATE_QUERY),
            (
                UiCommand::ReportStatusTipPos,
                ext_kind::POS_STATUS_TIP_QUERY,
            ),
        ] {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, ev) = mk(cap.clone(), "_t_query");
            show_two_fixed(&mut f, true, 300, 400);
            cap.lock().unwrap().clear();
            while ev.try_recv().is_ok() {}

            f.handle(cmd);
            let v = cap.lock().unwrap();
            let ext = v
                .iter()
                .find(|f| cmd_of(f) == CMD_EXT)
                .expect("应发扩展信封问询");
            let (kind, body) = decode_ext(&ext[8..]).expect("信封应可解");
            assert_eq!(kind, want);
            assert!(body.is_empty(), "问询不带 body");
            assert!(
                ev.try_recv().is_err(),
                "不该就地编一个位置事件——答案要等 .app 回"
            );
        }
    }

    fn show_two(f: &mut Forwarder) {
        f.handle(UiCommand::UpdateCandidates {
            preedit: "a".into(),
            preedit_caret: 1,
            preedit_host_owned: false,
            mode_label: "".into(),
            candidates: vec![item("中"), item("国")],
            selected: 0,
            hover: -1,
            page: 1,
            total_pages: 1,
            grid: None,
            caret_x: 100,
            caret_y: 200,
            caret_height: 20,
            caret_valid: true,
            fixed: false,
            fixed_x: 0,
            fixed_y: 0,
        });
    }

    #[test]
    fn theme_change_repaints_visible_candidates() {
        // 回归：换主题只改了 win 的配色却不重推帧，显示中的候选窗停在旧样子，
        // 要等下一次按键/鼠标悬停才更新（用户可见症状：菜单里换主题「不生效」）。
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t_theme");
        show_two(&mut f);
        cap.lock().unwrap().clear();

        f.handle(UiCommand::SetTheme(Box::default()));
        let v = cap.lock().unwrap();
        assert!(
            v.iter()
                .any(|x| cmd_of(x) == wind_ipc::protocol::CMD_HOST_RENDER_FRAME),
            "换主题后必须重推一帧"
        );
    }

    #[test]
    fn theme_change_does_not_resurrect_hidden_candidates() {
        // 反向：候选窗已隐藏时换主题不得把它推回屏幕上。
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t_theme2");
        show_two(&mut f);
        f.handle(UiCommand::HideCandidates);
        cap.lock().unwrap().clear();

        f.handle(UiCommand::SetTheme(Box::default()));
        assert!(
            cap.lock().unwrap().is_empty(),
            "隐藏状态下换主题不该推任何帧"
        );
    }

    /// 没有任何可见窗口时不能悄悄存一张空图：不建目录、不落文件，只弹一条「没有可见窗口」的 Toast。
    #[test]
    fn screenshot_without_visible_windows_saves_nothing_and_toasts() {
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t_shot0");
        let dir = std::env::temp_dir().join("windinput_test_shots_none");
        f.handle(UiCommand::TakeScreenshot {
            dir: dir.display().to_string(),
        });

        // Toast 由后台线程推出，等它到达。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let toasts = || {
            cap.lock()
                .unwrap()
                .iter()
                .filter(|f| cmd_of(f) == CMD_OVERLAY_FRAME)
                .count()
        };
        while toasts() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(toasts(), 1, "应恰好弹一条结果 Toast");
        assert!(!dir.exists(), "不该建目录/落文件");
    }

    /// 等目录里出现 `want` 个文件（截图已改为后台线程完成）。最多等 5 秒。
    fn wait_for_files(dir: &std::path::Path, want: usize) -> Vec<std::ffi::OsString> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let files: Vec<_> = std::fs::read_dir(dir)
                .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
                .unwrap_or_default();
            if files.len() >= want || std::time::Instant::now() >= deadline {
                return files;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn screenshot_saves_png_when_visible() {
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t_shot1");
        show_two(&mut f);
        let dir = std::env::temp_dir().join(format!("windinput_test_shots_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        f.handle(UiCommand::TakeScreenshot {
            dir: dir.display().to_string(),
        });

        // 存盘已挪到后台线程（forwarder 线程不能被 PNG 编码 + osascript 卡住），
        // 故等它落盘；超时即判失败，不放过"永远不落盘"这种回归。
        let files = wait_for_files(&dir, 1);
        assert_eq!(files.len(), 1, "应存出一张 PNG，实际 {files:?}");
        let name = files[0].to_string_lossy().to_string();
        assert!(
            name.starts_with("candidate_") && name.ends_with(".png"),
            "{name}"
        );
        // 回归：timestamp() 曾在非 Windows 恒返回 "00000000_000000"，多张截图互相覆盖。
        assert!(
            !name.contains("00000000_000000"),
            "文件名须含真实时间戳，实际 {name}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hide_emits_hidden_frame() {
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t2");
        // 先显示一帧建 shm。
        f.handle(UiCommand::UpdateCandidates {
            preedit: "a".into(),
            preedit_caret: 1,
            preedit_host_owned: false,
            mode_label: "".into(),
            candidates: vec![item("中")],
            selected: 0,
            hover: -1,
            page: 1,
            total_pages: 1,
            grid: None,
            caret_x: 10,
            caret_y: 20,
            caret_height: 20,
            caret_valid: true,
            fixed: false,
            fixed_x: 0,
            fixed_y: 0,
        });
        cap.lock().unwrap().clear();
        f.handle(UiCommand::HideCandidates);
        let v = cap.lock().unwrap();
        let hr = v
            .iter()
            .find(|x| cmd_of(x) == wind_ipc::protocol::CMD_HOST_RENDER_FRAME)
            .expect("hidden frame");
        // payload flags @ 帧 offset 8+20=28，VISIBLE 位应为 0。
        assert_eq!(u32::from_le_bytes(hr[28..32].try_into().unwrap()) & 0x1, 0);
    }

    /// 工具栏命令在 macOS 转成菜单栏状态图标（`CMD_MODE_STATUS`）；Linux 驱动的是悬浮工具栏，见 `panels`。
    #[cfg(target_os = "macos")]
    #[test]
    fn hide_toolbar_emits_mode_status() {
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t6");
        f.handle(UiCommand::HideToolbar);
        assert!(
            cap.lock()
                .unwrap()
                .iter()
                .any(|x| cmd_of(x) == wind_ipc::protocol::CMD_MODE_STATUS)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn update_toolbar_emits_mode_status() {
        let cap = Arc::new(Mutex::new(Vec::new()));
        let (mut f, _ev) = mk(cap.clone(), "_t3");
        f.handle(UiCommand::UpdateToolbar(
            crate::toolbar::ToolbarState::default(),
        ));
        assert!(
            cap.lock()
                .unwrap()
                .iter()
                .any(|x| cmd_of(x) == wind_ipc::protocol::CMD_MODE_STATUS)
        );
    }

    /// Linux 外部宿主形态：工具栏 / 软键盘经 `ext_panel` 推 kind 10 / 11 的浮层帧，
    /// 指针经 `PanelPointer` 回到它们自己的鼠标状态机，计时由 forwarder 按 deadline 推进。
    #[cfg(all(target_os = "linux", ext_presenter))]
    mod panels {
        use super::*;
        use crate::toolbar::ToolbarState;
        use crate::toolbar_gate::SHOW_DEBOUNCE;
        use std::time::Duration;
        use wind_ipc::protocol::overlay::{OVERLAY_KIND_SOFTKEYBOARD, OVERLAY_KIND_TOOLBAR};
        use wind_ipc::protocol::panel_pointer::*;
        use wind_ui_types::{SOFT_TAG_CLOSE, SoftKeyCap, ToolbarAction};

        /// 计时类断言的余量：只用来盖过线程调度抖动，不参与语义。
        const SLACK: Duration = Duration::from_millis(60);

        /// 一帧 kind 10 / 11 浮层的元数据 + 拖动区。
        #[derive(Debug)]
        struct Panel {
            visible: bool,
            w: u32,
            h: u32,
            x: i32,
            y: i32,
            drag: Vec<[i32; 4]>,
        }

        fn panel_frames(cap: &Arc<Mutex<Vec<Vec<u8>>>>, kind: u32) -> Vec<Panel> {
            cap.lock()
                .unwrap()
                .iter()
                .filter(|f| cmd_of(f) == CMD_OVERLAY_FRAME)
                .map(|f| &f[8..])
                .filter(|p| u32::from_le_bytes(p[0..4].try_into().unwrap()) == kind)
                .map(|p| {
                    let u = |o: usize| u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
                    let n = u(72) as usize;
                    let drag = (0..n)
                        .map(|k| {
                            let at = 76 + 16 * k;
                            [0, 4, 8, 12].map(|d| u(at + d) as i32)
                        })
                        .collect();
                    Panel {
                        visible: u(16) & SharedRenderHeader::FLAG_VISIBLE != 0,
                        w: u(8),
                        h: u(12),
                        x: u(24) as i32,
                        y: u(28) as i32,
                        drag,
                    }
                })
                .collect()
        }

        /// 段名带进程号与用例名：并发跑的测试进程、同进程的各用例互不撞段。
        fn mk_panels(
            cap: Arc<Mutex<Vec<Vec<u8>>>>,
            tag: &str,
        ) -> (Forwarder, std::sync::mpsc::Receiver<UiEvent>) {
            mk(cap, &format!("_p{tag}{}", std::process::id()))
        }

        fn pointer(f: &mut Forwarder, kind: u32, event: u32, button: u32, (x, y): (i32, i32)) {
            f.handle(UiCommand::PanelPointer {
                kind,
                event,
                button,
                x,
                y,
            });
            f.tick();
        }

        /// 显示工具栏：发状态、熬过显示迟滞、推进一次。返回最后一帧。
        fn show_toolbar(f: &mut Forwarder, cap: &Arc<Mutex<Vec<Vec<u8>>>>) -> Panel {
            f.handle(UiCommand::UpdateToolbar(ToolbarState::default()));
            std::thread::sleep(SHOW_DEBOUNCE + SLACK);
            f.tick();
            panel_frames(cap, OVERLAY_KIND_TOOLBAR)
                .pop()
                .expect("迟滞到期后应推出工具栏帧")
        }

        /// 47 个键位都给符号，任何一个点下去都会上屏。
        fn show_softkeyboard(f: &mut Forwarder) {
            let keys = (0..47)
                .map(|i| SoftKeyCap {
                    slot: format!("k{i}"),
                    base: "，".into(),
                    shift: "《".into(),
                })
                .collect();
            f.handle(UiCommand::ShowSoftKeyboard {
                pages: vec!["标点".into(), "数学".into()],
                current: 0,
                keys,
                send_keys: false,
                anchor: None,
                work_area: Some((0, 0, 1920, 1080)),
            });
        }

        fn center_of(f: &Forwarder, tag: i32) -> (i32, i32) {
            let r = f
                .panels
                .soft_keyboard
                .as_ref()
                .expect("软键盘已建")
                .hit_rect(tag)
                .unwrap_or_else(|| panic!("tag {tag} 没有命中区"));
            ((r.x + r.w * 0.5) as i32, (r.y + r.h * 0.5) as i32)
        }

        fn key_events(ev: &std::sync::mpsc::Receiver<UiEvent>) -> usize {
            ev.try_iter()
                .filter(|e| matches!(e, UiEvent::SoftKeyboardKey { .. }))
                .count()
        }

        #[test]
        fn toolbar_update_shows_after_gate_debounce() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk_panels(cap.clone(), "tb1");
            f.handle(UiCommand::UpdateToolbar(ToolbarState::default()));
            f.tick();
            assert!(
                panel_frames(&cap, OVERLAY_KIND_TOOLBAR).is_empty(),
                "显示迟滞期内不出帧"
            );
            let due = f.next_deadline().expect("迟滞要登记唤醒时刻");
            assert!(due <= std::time::Instant::now() + SHOW_DEBOUNCE);

            std::thread::sleep(SHOW_DEBOUNCE + SLACK);
            f.tick();
            let fr = panel_frames(&cap, OVERLAY_KIND_TOOLBAR);
            let last = fr.last().expect("迟滞到期应出帧");
            assert!(last.visible && last.w > 0 && last.h > 0);
            assert_eq!(last.drag.len(), 1, "工具栏的拖动区就是拖动柄一块");
        }

        #[test]
        fn toolbar_hidden_while_host_fullscreen() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk_panels(cap.clone(), "tb2");
            f.handle(UiCommand::SetHostFullscreen(true));
            f.handle(UiCommand::UpdateToolbar(ToolbarState::default()));
            std::thread::sleep(SHOW_DEBOUNCE + SLACK);
            f.tick();
            assert!(
                !panel_frames(&cap, OVERLAY_KIND_TOOLBAR)
                    .iter()
                    .any(|p| p.visible),
                "全屏期间迟滞到期也不显示"
            );

            f.handle(UiCommand::SetHostFullscreen(false));
            let fr = panel_frames(&cap, OVERLAY_KIND_TOOLBAR);
            assert!(
                fr.last().is_some_and(|p| p.visible),
                "退出全屏：闸门处于显示态就补画"
            );

            f.handle(UiCommand::SetHostFullscreen(true));
            let fr = panel_frames(&cap, OVERLAY_KIND_TOOLBAR);
            assert!(
                !fr.last().unwrap().visible,
                "进入全屏：显示中的工具栏立即隐藏"
            );

            // 全屏期间的内容更新也不能把它推回屏上。
            f.handle(UiCommand::UpdateToolbar(ToolbarState::default()));
            std::thread::sleep(SHOW_DEBOUNCE + SLACK);
            f.tick();
            assert!(
                !panel_frames(&cap, OVERLAY_KIND_TOOLBAR)
                    .last()
                    .unwrap()
                    .visible
            );

            // 闸门已收起（HideToolbar 生效）时退出全屏不补画。
            f.handle(UiCommand::HideToolbar);
            std::thread::sleep(crate::toolbar_gate::HIDE_DEBOUNCE + SLACK);
            f.tick();
            let n = panel_frames(&cap, OVERLAY_KIND_TOOLBAR).len();
            f.handle(UiCommand::SetHostFullscreen(false));
            assert!(
                panel_frames(&cap, OVERLAY_KIND_TOOLBAR)[n..]
                    .iter()
                    .all(|p| !p.visible),
                "工具栏本就该藏着，退出全屏不能把它变出来"
            );
        }

        #[test]
        fn softkeyboard_show_pushes_kind_11_with_drag_regions() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk_panels(cap.clone(), "sk1");
            show_softkeyboard(&mut f);
            let fr = panel_frames(&cap, OVERLAY_KIND_SOFTKEYBOARD);
            let p = fr.last().expect("打开软键盘应推 kind 11 的帧");
            assert!(p.visible);
            assert!(!p.drag.is_empty(), "要带拖动区");
            let inside = |(x, y): (i32, i32)| {
                p.drag
                    .iter()
                    .any(|[dx, dy, dw, dh]| x >= *dx && x < dx + dw && y >= *dy && y < dy + dh)
            };
            for [x, y, w, h] in &p.drag {
                assert!(*x >= 0 && *y >= 0 && x + w <= p.w as i32 && y + h <= p.h as i32);
            }
            assert!(inside((1, 1)), "面板内边距（键位之外的留白）可拖");
            assert!(!inside(center_of(&f, 0)), "键帽不能落进拖动区");
            assert!(
                !inside(center_of(&f, SOFT_TAG_CLOSE)),
                "关闭按钮不能落进拖动区"
            );
        }

        #[test]
        fn panel_pointer_click_on_toolbar_cell_emits_toolbar_event() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, ev) = mk_panels(cap.clone(), "tb3");
            let p = show_toolbar(&mut f, &cap);
            // 拖动柄之后第一格是中英格（默认项序列打头的是 Mode）。
            let [gx, _, gw, _] = p.drag[0];
            let cells = wind_ui_types::DEFAULT_TOOLBAR_ITEMS.len() as i32;
            let cell = (p.w as i32 - gw) / cells;
            let at = (gx + gw + cell / 2, p.h as i32 / 2);
            pointer(&mut f, OVERLAY_KIND_TOOLBAR, PANEL_POINTER_PRESS, 1, at);
            pointer(&mut f, OVERLAY_KIND_TOOLBAR, PANEL_POINTER_RELEASE, 1, at);
            let got: Vec<_> = ev
                .try_iter()
                .filter_map(|e| match e {
                    UiEvent::Toolbar(a) => Some(a),
                    _ => None,
                })
                .collect();
            assert_eq!(got, vec![ToolbarAction::ToggleMode]);
        }

        /// 按 `next_deadline` 睡、醒来 `tick`，直到 `until`——等同主循环在没有命令时的行为。
        fn drive_until(f: &mut Forwarder, until: std::time::Instant) {
            loop {
                let now = std::time::Instant::now();
                if now >= until {
                    return;
                }
                let wake = f.next_deadline().map_or(until, |d| d.min(until));
                std::thread::sleep(wake.saturating_duration_since(now));
                f.tick();
            }
        }

        /// 自动隐藏已把工具栏收起：进出一次全屏不能把它补画出来（否则每次退出全屏闪一下）。
        #[test]
        fn fullscreen_exit_keeps_auto_hidden_toolbar_hidden() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk_panels(cap.clone(), "tb5");
            // 1000ms 是 `AutoHide::configure` 的下限。
            let delay = Duration::from_millis(1000);
            f.handle(UiCommand::SetToolbarAutoHide {
                enabled: true,
                delay_ms: delay.as_millis() as u64,
                hover_reveal: false,
            });
            show_toolbar(&mut f, &cap);
            let until = std::time::Instant::now() + delay + crate::auto_hide::FADE_DURATION + SLACK;
            drive_until(&mut f, until);
            assert!(
                !panel_frames(&cap, OVERLAY_KIND_TOOLBAR)
                    .last()
                    .unwrap()
                    .visible,
                "自动隐藏到期应已收起"
            );
            let n = panel_frames(&cap, OVERLAY_KIND_TOOLBAR).len();
            f.handle(UiCommand::SetHostFullscreen(true));
            f.handle(UiCommand::SetHostFullscreen(false));
            assert!(
                panel_frames(&cap, OVERLAY_KIND_TOOLBAR)[n..]
                    .iter()
                    .all(|p| !p.visible),
                "退出全屏不该把自动隐藏收起的工具栏补画出来"
            );
        }

        /// 内容在指针底下变了（切面重建面板）：指针还在面板里就按它重算高亮；LEAVE 之后
        /// 「最后指针」虽然还停在那个键上，也不能把它点亮。
        #[test]
        fn softkeyboard_hover_follows_content_only_while_pointer_inside() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk_panels(cap.clone(), "sk4");
            show_softkeyboard(&mut f);
            let key = center_of(&f, 0);
            let k = OVERLAY_KIND_SOFTKEYBOARD;
            let hover = |f: &Forwarder| f.panels.soft_keyboard.as_ref().unwrap().hover();
            pointer(&mut f, k, PANEL_POINTER_MOTION, 0, key);
            assert_eq!(hover(&f), 0);

            // 切面：`show` 先清掉悬停，重排后按指针位置补回来。
            show_softkeyboard(&mut f);
            assert_eq!(hover(&f), 0, "指针还在键上，切面后高亮应跟上");

            pointer(&mut f, k, PANEL_POINTER_LEAVE, 0, (0, 0));
            assert_eq!(hover(&f), -1);
            show_softkeyboard(&mut f);
            assert_eq!(hover(&f), -1, "指针已离开，切面不能点亮任何键");
        }

        /// 宿主本地拖完回报落点：之后的重绘落在新位置，不跳回旧处。
        #[test]
        fn panel_moved_keeps_toolbar_at_dropped_position() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk_panels(cap.clone(), "tb4");
            show_toolbar(&mut f, &cap);
            f.handle(UiCommand::PanelMoved {
                kind: OVERLAY_KIND_TOOLBAR,
                x: 300,
                y: 400,
            });
            f.handle(UiCommand::UpdateToolbar(ToolbarState {
                chinese_mode: true,
                ..Default::default()
            }));
            let p = panel_frames(&cap, OVERLAY_KIND_TOOLBAR).pop().unwrap();
            assert!(p.visible);
            assert_eq!((p.x, p.y), (300, 400));
        }

        /// addon 收到 SERVICE_READY 会丢掉手里的工具栏 / 软键盘（它分不清服务是重启了还是只换了
        /// 连接）。同一服务换了 push 连接时状态都还在，不重推的话面板要等下一次状态变化才回来。
        #[test]
        fn reconnect_resends_visible_panels() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk_panels(cap.clone(), "rc1");
            let tb = show_toolbar(&mut f, &cap);
            show_softkeyboard(&mut f);
            let skb = panel_frames(&cap, OVERLAY_KIND_SOFTKEYBOARD)
                .pop()
                .expect("软键盘应出帧");
            assert!(skb.visible);

            let geo = |p: &Panel| (p.visible, p.x, p.y, p.w, p.h, p.drag.clone());
            cap.lock().unwrap().clear();
            f.handle(UiCommand::HostReconnected);
            let again = panel_frames(&cap, OVERLAY_KIND_TOOLBAR);
            assert_eq!(again.len(), 1, "可见的工具栏原样补推一帧：{again:?}");
            assert_eq!(geo(&again[0]), geo(&tb));
            let again = panel_frames(&cap, OVERLAY_KIND_SOFTKEYBOARD);
            assert_eq!(again.len(), 1, "可见的软键盘同样补推：{again:?}");
            assert_eq!(geo(&again[0]), geo(&skb));

            // 收起的面板不补：补了就是把一个已经关掉的窗口推回屏幕上。
            f.handle(UiCommand::HideSoftKeyboard);
            cap.lock().unwrap().clear();
            f.handle(UiCommand::HostReconnected);
            assert_eq!(panel_frames(&cap, OVERLAY_KIND_TOOLBAR).len(), 1);
            assert!(panel_frames(&cap, OVERLAY_KIND_SOFTKEYBOARD).is_empty());
        }

        /// addon 换了实例（fcitx5 重启）：新 addon 的全屏初值是「不全屏」，状态没变就不会报——
        /// 旧 addon 报的「全屏」要在这里作废，否则工具栏再也不出来。
        #[test]
        fn reconnect_clears_host_fullscreen() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk_panels(cap.clone(), "rc2");
            show_toolbar(&mut f, &cap);
            f.handle(UiCommand::SetHostFullscreen(true));
            assert!(
                !panel_frames(&cap, OVERLAY_KIND_TOOLBAR)
                    .last()
                    .unwrap()
                    .visible
            );
            cap.lock().unwrap().clear();
            f.handle(UiCommand::HostReconnected);
            assert!(
                panel_frames(&cap, OVERLAY_KIND_TOOLBAR)
                    .last()
                    .is_some_and(|p| p.visible),
                "重连后按「不全屏」重新放出工具栏"
            );
        }

        /// addon 崩溃时正按住软键盘的键：它再也报不来松开 / 离开。重连时补一次离开——连发停下、
        /// 按下态与「指针在面板里」一并清掉，免得新 addon 连上后还在往焦点应用里打字。
        #[test]
        fn reconnect_releases_held_softkey_and_pointer() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, ev) = mk_panels(cap.clone(), "rc3");
            show_softkeyboard(&mut f);
            show_toolbar(&mut f, &cap);
            let k = OVERLAY_KIND_SOFTKEYBOARD;
            let key = center_of(&f, 0);
            pointer(&mut f, k, PANEL_POINTER_PRESS, 1, key);
            pointer(
                &mut f,
                OVERLAY_KIND_TOOLBAR,
                PANEL_POINTER_MOTION,
                0,
                (20, 10),
            );
            assert!(crate::ext_panel::pointer_inside(crate::sys::HWND(
                OVERLAY_KIND_TOOLBAR as isize
            )));
            assert_eq!(key_events(&ev), 1, "按下即出字一次");

            f.handle(UiCommand::HostReconnected);
            f.tick();
            let (delay, rate) = crate::soft_keyboard::repeat_params();
            std::thread::sleep(Duration::from_millis(delay + rate * 2) + SLACK);
            f.tick();
            assert_eq!(key_events(&ev), 0, "重连后不再连发");
            assert_eq!(f.panels.soft_keyboard.as_ref().unwrap().hover(), -1);
            for kind in [OVERLAY_KIND_TOOLBAR, k] {
                assert!(
                    !crate::ext_panel::pointer_inside(crate::sys::HWND(kind as isize)),
                    "kind {kind}：指针不再算在面板里"
                );
            }
        }

        /// 宿主只报左上角；换算成右下角锚点要用面板尺寸，由渲染端算好回报协调器落盘（同 Windows
        /// 拖动结束时的 `ToolbarMoved` / `SoftKeyboardMoved`）。
        #[test]
        fn panel_moved_reports_bottom_right_anchor() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, ev) = mk_panels(cap.clone(), "mv");
            let tb = show_toolbar(&mut f, &cap);
            show_softkeyboard(&mut f);
            let skb = panel_frames(&cap, OVERLAY_KIND_SOFTKEYBOARD)
                .pop()
                .expect("软键盘应推出帧");
            while ev.try_recv().is_ok() {}
            f.handle(UiCommand::PanelMoved {
                kind: OVERLAY_KIND_TOOLBAR,
                x: 300,
                y: 400,
            });
            f.handle(UiCommand::PanelMoved {
                kind: OVERLAY_KIND_SOFTKEYBOARD,
                x: 50,
                y: 60,
            });
            let got: Vec<UiEvent> = ev.try_iter().collect();
            let (tw, th) = (tb.w as i32, tb.h as i32);
            let (sw, sh) = (skb.w as i32, skb.h as i32);
            assert!(
                got.iter()
                    .any(|e| matches!(e, UiEvent::ToolbarMoved { right, bottom }
                    if (*right, *bottom) == (300 + tw, 400 + th))),
                "{got:?}"
            );
            assert!(
                got.iter()
                    .any(|e| matches!(e, UiEvent::SoftKeyboardMoved { right, bottom }
                    if (*right, *bottom) == (50 + sw, 60 + sh))),
                "{got:?}"
            );
        }

        /// 按住键帽、此后不再有任何命令：连发靠 forwarder 自己按 deadline 醒来推进。
        #[test]
        fn softkeyboard_hold_repeats_via_deadline() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, ev) = mk_panels(cap.clone(), "sk2");
            show_softkeyboard(&mut f);
            let at = center_of(&f, 0);
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(UiCommand::PanelPointer {
                kind: OVERLAY_KIND_SOFTKEYBOARD,
                event: PANEL_POINTER_PRESS,
                button: 1,
                x: at.0,
                y: at.1,
            })
            .unwrap();
            let (delay, rate) = crate::soft_keyboard::repeat_params();
            let hold = Duration::from_millis(delay + rate * 3) + SLACK;
            let stopper = std::thread::spawn(move || {
                std::thread::sleep(hold);
                let _ = tx.send(UiCommand::Shutdown);
            });
            run_loop(&mut f, &rx);
            stopper.join().unwrap();
            let n = key_events(&ev);
            assert!(n >= 2, "按下一次 + 至少一次连发，实际 {n}");
        }

        /// 按下后挪出再松手 = 反悔：抬起才触发的控件（关闭）不触发；按下即出字的键帽
        /// 只出按下那一次，不再连发。
        #[test]
        fn press_drag_out_release_cancels() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, ev) = mk_panels(cap.clone(), "sk3");
            show_softkeyboard(&mut f);
            let close = center_of(&f, SOFT_TAG_CLOSE);
            let key = center_of(&f, 0);
            let k = OVERLAY_KIND_SOFTKEYBOARD;

            pointer(&mut f, k, PANEL_POINTER_PRESS, 1, close);
            pointer(&mut f, k, PANEL_POINTER_MOTION, 0, key);
            pointer(&mut f, k, PANEL_POINTER_RELEASE, 1, key);
            let closed = |ev: &std::sync::mpsc::Receiver<UiEvent>| {
                ev.try_iter()
                    .filter(|e| matches!(e, UiEvent::SoftKeyboardClose))
                    .count()
            };
            assert_eq!(closed(&ev), 0, "挪出关闭按钮再松手不该关");

            // 挪出后先**按住不放**熬过连发间隔再松手：抬起本身也会停连发，松手放在等待
            // 之前就测不到「移出即停」。
            pointer(&mut f, k, PANEL_POINTER_PRESS, 1, key);
            pointer(&mut f, k, PANEL_POINTER_MOTION, 0, close);
            let (delay, rate) = crate::soft_keyboard::repeat_params();
            std::thread::sleep(Duration::from_millis(delay + rate * 2) + SLACK);
            f.tick();
            assert_eq!(key_events(&ev), 1, "键帽按下即出字一次，挪出后不再连发");
            pointer(&mut f, k, PANEL_POINTER_RELEASE, 1, close);

            // 对照：原地按下松开，关闭照常触发。
            pointer(&mut f, k, PANEL_POINTER_PRESS, 1, close);
            pointer(&mut f, k, PANEL_POINTER_RELEASE, 1, close);
            assert_eq!(closed(&ev), 1);
        }
    }

    /// 状态气泡 / Toast / tooltip 光栅化成 `CMD_OVERLAY_FRAME`。
    mod overlays {
        use super::*;
        use wind_bridge::shared_memory_posix::PosixSharedMemory;
        use wind_ipc::protocol::overlay::*;
        use wind_ui_types::{StatusTipAnchor, StatusTipPlacement};

        /// 解出一帧浮层元数据（按 `encode_overlay_frame` 的偏移）。
        struct Meta {
            kind: u32,
            w: u32,
            h: u32,
            flags: u32,
            place: u32,
            x: i32,
            y: i32,
            alt_y: i32,
            anchor: u32,
            margin: i32,
            duration: i32,
        }

        fn overlays(cap: &Arc<Mutex<Vec<Vec<u8>>>>, kind: u32) -> Vec<Meta> {
            cap.lock()
                .unwrap()
                .iter()
                .filter(|f| cmd_of(f) == CMD_OVERLAY_FRAME)
                .map(|f| {
                    let p = &f[8..];
                    let u = |o: usize| u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
                    let i = |o: usize| i32::from_le_bytes(p[o..o + 4].try_into().unwrap());
                    Meta {
                        kind: u(0),
                        w: u(8),
                        h: u(12),
                        flags: u(16),
                        place: u(20),
                        x: i(24),
                        y: i(28),
                        alt_y: i(36),
                        anchor: u(40),
                        margin: i(44),
                        duration: i(64),
                    }
                })
                .filter(|m| m.kind == kind)
                .collect()
        }

        fn visible(m: &Meta) -> bool {
            m.flags & SharedRenderHeader::FLAG_VISIBLE != 0
        }

        fn status(f: &mut Forwarder, placement: StatusTipPlacement) {
            f.handle(UiCommand::ShowStatusTip {
                text: "中".into(),
                x: 10,
                y: 20,
                caret_height: 18,
                duration_ms: 1500,
                placement,
            });
        }

        /// 跟随光标：首选点 = 光标底端 + 4 + 偏移，备选点 = 光标顶端上方；像素真在 SHM 里。
        #[test]
        fn status_tip_caret_is_rasterized_with_flip_points() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk(cap.clone(), "_lo1");
            status(
                &mut f,
                StatusTipPlacement::Caret {
                    offset_x: 3,
                    offset_y: 4,
                    above: false,
                },
            );
            let m = overlays(&cap, OVERLAY_KIND_STATUS);
            assert_eq!(m.len(), 1);
            let m = &m[0];
            assert!(visible(m));
            assert_eq!(m.place, OVERLAY_PLACE_FLIP);
            assert_eq!((m.x, m.y), (13, 20 + 18 + 4 + 4));
            assert!(m.alt_y < 20, "备选点在光标顶端之上");
            assert_eq!(m.duration, 1500, "自动隐藏时长随帧下发、归 addon 计时");
            let name =
                wind_bridge::endpoint::overlay_shm_name("_lo1", OVERLAY_KIND_STATUS).unwrap();
            let shm = PosixSharedMemory::open_readonly(&name, MAX_SHARED_RENDER_SIZE).unwrap();
            let hdr = shm.read_header();
            assert_eq!((hdr.width, hdr.height), (m.w, m.h));
            let px = &shm.pixels()[..(m.w * m.h * 4) as usize];
            assert!(px.chunks(4).any(|p| p[3] > 0), "位图不能是全透明");
        }

        fn caret_status(f: &mut Forwarder) {
            f.handle(UiCommand::ShowStatusTip {
                text: "注释:详细".into(),
                x: 100,
                y: 200,
                caret_height: 20,
                duration_ms: 1500,
                placement: StatusTipPlacement::Caret {
                    offset_x: 0,
                    offset_y: 0,
                    above: false,
                },
            });
        }

        /// 候选窗显示中弹跟随光标的状态泡：避让到光标上方，交宿主随候选窗实际落点平移。
        #[test]
        fn status_tip_avoids_a_visible_candidate_window() {
            use wind_ipc::protocol::overlay::OVERLAY_PLACE_FOLLOW_CANDIDATE;
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk(cap.clone(), "_lo_av1");
            show_two_fixed(&mut f, false, 0, 0);
            caret_status(&mut f);
            let m = overlays(&cap, OVERLAY_KIND_STATUS);
            let m = m.last().expect("应推出状态泡");
            assert_eq!(m.place, OVERLAY_PLACE_FOLLOW_CANDIDATE);
            assert!(
                m.y + m.h as i32 <= 200,
                "应在光标行顶之上：y={} h={}",
                m.y,
                m.h
            );
        }

        /// 状态泡先弹、候选窗后出现：按原参数重新落位一次，剩余时长不超过原时长。
        #[test]
        fn status_tip_replaces_when_candidates_appear_later() {
            use wind_ipc::protocol::overlay::OVERLAY_PLACE_FOLLOW_CANDIDATE;
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk(cap.clone(), "_lo_av2");
            caret_status(&mut f);
            assert_eq!(
                overlays(&cap, OVERLAY_KIND_STATUS)[0].place,
                OVERLAY_PLACE_FLIP
            );
            show_two_fixed(&mut f, false, 0, 0);
            let m = overlays(&cap, OVERLAY_KIND_STATUS);
            assert_eq!(m.len(), 2, "候选窗出现后重推一帧");
            assert_eq!(m[1].place, OVERLAY_PLACE_FOLLOW_CANDIDATE);
            assert!(m[1].duration > 0 && m[1].duration <= 1500);
        }

        #[test]
        fn status_tip_fixed_and_anchor_placements() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk(cap.clone(), "_lo2");
            status(&mut f, StatusTipPlacement::Fixed { x: 500, y: 600 });
            status(
                &mut f,
                StatusTipPlacement::Anchor(StatusTipAnchor::WindowBottomLeft),
            );
            let m = overlays(&cap, OVERLAY_KIND_STATUS);
            assert_eq!(m.len(), 2);
            assert_eq!(m[0].place, OVERLAY_PLACE_ABSOLUTE);
            assert_eq!((m[0].x, m[0].y), (500, 600));
            assert_eq!(m[1].place, OVERLAY_PLACE_ANCHOR);
            assert_eq!(
                m[1].anchor, OVERLAY_ANCHOR_BOTTOM_LEFT,
                "窗口锚点拿不到窗口边框，降级为屏幕锚点"
            );
            assert_eq!(m[1].margin, 16);
        }

        #[test]
        fn hide_status_tip_after_show_emits_hidden_overlay() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk(cap.clone(), "_lo3");
            f.handle(UiCommand::HideStatusTip);
            assert!(
                overlays(&cap, OVERLAY_KIND_STATUS).is_empty(),
                "没显示过不发"
            );
            status(&mut f, StatusTipPlacement::Fixed { x: 1, y: 1 });
            f.handle(UiCommand::HideStatusTip);
            let m = overlays(&cap, OVERLAY_KIND_STATUS);
            assert_eq!(m.len(), 2);
            assert!(visible(&m[0]) && !visible(&m[1]));
        }

        #[test]
        fn toast_is_rasterized_with_anchor() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk(cap.clone(), "_lo4");
            f.handle(UiCommand::ShowToast {
                text: "ok".into(),
                position: crate::toast::ToastPosition::BottomCenter,
                kind: crate::toast::ToastKind::Success,
                duration_ms: 2000,
                accent: None,
            });
            let m = overlays(&cap, OVERLAY_KIND_TOAST);
            assert_eq!(m.len(), 1);
            assert!(visible(&m[0]));
            assert_eq!(m[0].place, OVERLAY_PLACE_ANCHOR);
            assert_eq!(m[0].anchor, OVERLAY_ANCHOR_BOTTOM_CENTER);
            assert_eq!((m[0].margin, m[0].duration), (12, 2000));
        }

        /// 复制类操作的结果反馈也走浮层（此前 Linux 上这条 Toast 被 addon 丢弃）。
        #[test]
        fn copy_tooltip_text_reports_empty_when_no_tip() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk(cap.clone(), "_lo5");
            f.handle(UiCommand::CopyTooltipText(String::new()));
            let m = overlays(&cap, OVERLAY_KIND_TOAST);
            assert_eq!(m.len(), 1);
            assert_eq!(m[0].anchor, OVERLAY_ANCHOR_BOTTOM_RIGHT);
        }

        /// 一个带单行反查内容（「nǐ」）的候选。
        fn tip_cand() -> CandidateItem {
            use wind_ui_types::{StyledText, TooltipDoc, TooltipLine, TooltipSection};
            let mut cand = item("你");
            cand.tooltip = Arc::new(TooltipDoc {
                sections: vec![TooltipSection {
                    title: None,
                    inline: false,
                    lines: vec![TooltipLine {
                        text: StyledText::from("nǐ"),
                        raw: 0,
                    }],
                }],
            });
            cand
        }

        /// 显示一页只含 `cand` 的候选，悬停在 `hover`。
        fn update(f: &mut Forwarder, cand: &CandidateItem, hover: i32) {
            f.handle(UiCommand::UpdateCandidates {
                preedit: "ni".into(),
                preedit_caret: 2,
                preedit_host_owned: false,
                mode_label: "".into(),
                candidates: vec![cand.clone()],
                selected: 0,
                hover,
                page: 1,
                total_pages: 1,
                grid: None,
                caret_x: 100,
                caret_y: 200,
                caret_height: 20,
                caret_valid: true,
                fixed: false,
                fixed_x: 0,
                fixed_y: 0,
            })
        }

        /// 悬停候选有反查内容 → tooltip 层跟着候选窗走；候选窗隐藏时 tooltip 一起藏。
        #[test]
        fn hover_tooltip_follows_candidate_and_hides_with_it() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk(cap.clone(), "_lo6");
            let cand = tip_cand();
            let update = |f: &mut Forwarder, hover: i32| update(f, &cand, hover);
            update(&mut f, -1);
            assert!(
                overlays(&cap, OVERLAY_KIND_TOOLTIP).is_empty(),
                "无悬停不发"
            );
            update(&mut f, 0);
            let (sx, sy, _) = last_render_frame(&cap).unwrap();
            let m = overlays(&cap, OVERLAY_KIND_TOOLTIP);
            assert_eq!(m.len(), 1);
            assert!(visible(&m[0]));
            assert_eq!(m[0].place, OVERLAY_PLACE_FOLLOW_CANDIDATE);
            assert_eq!(m[0].duration, 0, "tooltip 常驻到下一帧");
            assert!(
                m[0].x >= sx && m[0].y > sy,
                "横排在候选行下方：({},{}) vs 候选窗 ({sx},{sy})",
                m[0].x,
                m[0].y
            );
            f.handle(UiCommand::HideCandidates);
            let m = overlays(&cap, OVERLAY_KIND_TOOLTIP);
            assert!(!visible(m.last().unwrap()), "候选窗藏了 tooltip 也得藏");
        }

        fn tooltip_menu_requests(
            ev: &std::sync::mpsc::Receiver<UiEvent>,
        ) -> Vec<(i32, i32, i32, Option<wind_ui_types::TooltipHit>, u64)> {
            ev.try_iter()
                .filter_map(|e| match e {
                    UiEvent::RequestTooltipMenu {
                        x,
                        y,
                        candidate,
                        hit,
                        doc_fingerprint,
                    } => Some((x, y, candidate, hit, doc_fingerprint)),
                    _ => None,
                })
                .collect()
        }

        /// addon 报「右键点在提示位图内某处」→ 按本进程画的那一帧做命中，回送与 Windows 气泡
        /// 自己收右键时同一个事件（带候选下标与指纹，协调器据此核对）；点在文字上命中那一行，
        /// 点在内边距上没有命中。提示不在屏上时不请求菜单。
        #[test]
        fn tooltip_menu_at_hits_the_drawn_frame_only_while_shown() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, ev) = mk(cap.clone(), "_lo7");
            let cand = tip_cand();
            update(&mut f, &cand, -1);
            f.handle(UiCommand::TooltipMenuAt {
                x: 5,
                y: 6,
                local_x: 10,
                local_y: 10,
            });
            assert!(tooltip_menu_requests(&ev).is_empty(), "提示没显示不弹菜单");

            update(&mut f, &cand, 0);
            let m = overlays(&cap, OVERLAY_KIND_TOOLTIP);
            let m = m.last().unwrap();
            let center = ((m.w / 2) as i32, (m.h / 2) as i32);
            f.handle(UiCommand::TooltipMenuAt {
                x: 300,
                y: 400,
                local_x: center.0,
                local_y: center.1,
            });
            f.handle(UiCommand::TooltipMenuAt {
                x: 300,
                y: 400,
                local_x: 0,
                local_y: 0,
            });
            let fp = cand.tooltip.fingerprint();
            let hit = Some(wind_ui_types::TooltipHit {
                section: 0,
                raw_line: Some(0),
            });
            assert_eq!(
                tooltip_menu_requests(&ev),
                vec![(300, 400, 0, hit, fp), (300, 400, 0, None, fp)],
                "位图中心是那一行文字，左上角是内边距"
            );

            f.handle(UiCommand::HideCandidates);
            f.handle(UiCommand::TooltipMenuAt {
                x: 300,
                y: 400,
                local_x: center.0,
                local_y: center.1,
            });
            assert!(tooltip_menu_requests(&ev).is_empty(), "候选窗收了就不弹");
        }

        /// 气泡菜单的「截图此窗口」：像素在本进程，就地存 PNG 并弹 Windows 同款文案的 Toast；
        /// 没在显示时只弹「未显示」。
        #[test]
        fn overlay_screenshot_saves_png_or_reports_not_visible() {
            let cap = Arc::new(Mutex::new(Vec::new()));
            let (mut f, _ev) = mk(cap.clone(), "_lo8");
            let dir =
                std::env::temp_dir().join(format!("windinput_test_ovshot_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            f.handle(UiCommand::ScreenshotStatusTip { dir: dir.clone() });
            let toast = || overlays(&cap, OVERLAY_KIND_TOAST).len();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while toast() == 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert_eq!(toast(), 1, "未显示也要告诉用户");
            assert!(!dir.exists(), "没显示就不落文件");

            status(&mut f, StatusTipPlacement::Fixed { x: 1, y: 1 });
            f.handle(UiCommand::ScreenshotStatusTip { dir: dir.clone() });
            let files = wait_for_files(&dir, 1);
            assert_eq!(files.len(), 1, "应存出一张 PNG，实际 {files:?}");
            let name = files[0].to_string_lossy().to_string();
            assert!(
                name.starts_with("status_tip_") && name.ends_with(".png"),
                "{name}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
