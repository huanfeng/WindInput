//! UI 管理器 + 消息循环
//!
//! 与 Go 版本 `wind_input/internal/ui/manager.go` 对齐。
//! 在独立线程中运行 Win32 消息循环，通过通道接收 UI 更新命令。

use crate::candidate_window::{CandidateWindow, CandidateWindowConfig};
use crate::toast::{ToastKind, ToastPosition};

/// re-export：使协调器以 `wind_ui::manager::InputDiagView` 统一引用。
pub use crate::input_diag_hud::{DiagSections, InputDiagView, WindowDiagView};
use std::sync::mpsc;
use tracing::{debug, error, info};
#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::*;

#[cfg(windows)]
pub use wind_ui_types::HostRenderArc;
/// 类型定义已下沉至 wind-ui-types（表现层协议 crate）；此处再导出以保持
/// `wind_ui::manager::*` 原路径成立（协调器与本 crate 内部均经此引用）。
/// 注意 DiagSections 三件套走上方 input_diag_hud 的链式转发，勿在此重复列出。
pub use wind_ui_types::{
    CandidateOp, GlobalHotkeyEntry, HOVER_PAGE_NEXT, HOVER_PAGE_PREV, MenuAnchor, MenuCmd,
    MenuItemSpec, MenuKind, MenuPlacement, ToolbarAction, UiCommand, UiEvent,
};

/// UI 管理器（在独立线程中运行）
pub struct UiManager {
    cmd_tx: mpsc::Sender<UiCommand>,
    waker: crate::wake::UiWaker,
    event_rx: Option<mpsc::Receiver<UiEvent>>,
    _thread: std::thread::JoinHandle<()>,
}

impl UiManager {
    pub fn new() -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<UiCommand>();
        let (ev_tx, ev_rx) = mpsc::channel::<UiEvent>();
        // UI 线程等的是「消息队列 ∪ 本唤醒事件 ∪ 最近的计时器到期」，见 `wake` 模块。
        let (waker, wait_port) = crate::wake::channel();

        let thread = std::thread::Builder::new()
            .name("ui-manager".into())
            .spawn(move || {
                Self::ui_thread(rx, ev_tx, wait_port);
            })?;

        Ok(Self {
            cmd_tx: tx,
            waker,
            event_rx: Some(ev_rx),
            _thread: thread,
        })
    }

    pub fn sender(&self) -> mpsc::Sender<UiCommand> {
        self.cmd_tx.clone()
    }

    /// 唤醒 UI 线程的句柄。
    ///
    /// ⚠ **必须与 [`Self::sender`] 配对**：投递命令后要调用 `wake()`，否则 UI 线程会一直
    /// 睡到下一个计时器到期（可能永不到期）才看见那条命令。裸用这两者容易漏，故生产路径
    /// 一律经 `wind_coordinator::UiSender` —— 它把「先投递、再唤醒」固化进一次 `send`，
    /// 调用方无从写错。
    pub fn waker(&self) -> crate::wake::UiWaker {
        self.waker.clone()
    }

    /// 取出 UI 事件接收端（仅可取一次）；协调器据此处理鼠标交互。
    pub fn take_event_rx(&mut self) -> Option<mpsc::Receiver<UiEvent>> {
        self.event_rx.take()
    }

    /// UI 线程主循环
    ///
    /// 注意 [`UiManager::new`] 只负责 spawn 本线程即返回 `Ok`——窗口创建成功与否**不影响**
    /// 它的返回值。因此本函数一旦提前 `return`，主线程毫不知情：服务照常启动、输入照常工作，
    /// 而候选窗/工具栏/托盘/状态气泡**全部消失**。开机早期窗口站尚未就绪时
    /// `CreateWindowExW` 失败正是这种场景，且唯一痕迹是下面那条 `error!`——
    /// 偏偏主日志的 non_blocking worker 也可能已经死了。故这些分支同时写启动轨迹。
    fn ui_thread(
        rx: mpsc::Receiver<UiCommand>,
        event_tx: mpsc::Sender<UiEvent>,
        wait_port: crate::wake::UiWaitPort,
    ) {
        wind_config::startup_trace::stage("ui-thread-begin");

        // 创建候选窗口
        let config = CandidateWindowConfig::default();
        let mut candidate_window = match CandidateWindow::new(config, event_tx.clone()) {
            Ok(w) => {
                info!("Candidate window created");
                wind_config::startup_trace::stage("ui-candidate-window-ok");
                w
            }
            Err(e) => {
                error!("Failed to create candidate window: {}", e);
                // UI 线程就此退出 = 全部 GUI 消失，这是最需要留痕的一步。
                wind_config::startup_trace::stage(&format!("ui-candidate-window-FAILED: {e}"));
                return;
            }
        };

        // 状态提示气泡（best-effort，失败不影响候选窗口）
        let mut status_tip = match crate::status_tip::StatusTip::new(event_tx.clone()) {
            Ok(t) => Some(t),
            Err(e) => {
                error!("Failed to create status tip: {}", e);
                None
            }
        };
        let mut tip_hide_at: Option<std::time::Instant> = None;
        // 最近一次显示所用的自动隐藏时长（毫秒），交互结束后据此重新计时。
        let mut tip_duration_ms: u64 = 0;
        // 上一轮气泡是否处于交互中，用于识别「交互刚结束」这一沿——理由同
        // `auto_hide` 的 `was_engaged` 字段，见下方使用处。
        let mut tip_was_interacting = false;

        // 输入诊断 HUD（惰性创建：首次 ShowInputDiag 时构造，best-effort）
        let mut input_diag_hud: Option<crate::input_diag_hud::InputDiagHud> = None;

        // 一次性通知 toast（best-effort）
        let mut toast = match crate::toast::Toast::new() {
            Ok(t) => Some(t),
            Err(e) => {
                error!("Failed to create toast: {}", e);
                None
            }
        };
        let mut toast_hide_at: Option<std::time::Instant> = None;
        // 软键盘面板（惰性创建：首次 ShowSoftKeyboard 时构造，best-effort）
        let mut soft_keyboard: Option<crate::soft_keyboard::SoftKeyboard> = None;
        // 最近一次主题。**惰性创建的窗口靠它补课**——`SetTheme` 一般只在启动时发一次，
        // 而软键盘可能在那之后很久才第一次打开，不缓存就会永远停在内置默认配色。
        let mut last_theme: Option<wind_theme::Resolved> = None;
        // 状态提示防抖：合并快速连续的提示（如连按切换），避免气泡闪烁
        // 载荷：(text, x, y, caret_height, offset_x, offset_y)
        // payload: (text, x, y, caret_h, off_x, off_y, duration_ms, fixed, fixed_x, fixed_y)
        let mut tip_debounce = crate::debounce::Debouncer::<(
            String,
            i32,
            i32,
            i32,
            i32,
            i32,
            u64,
            bool,
            i32,
            i32,
        )>::new(60);
        // 工具栏显隐迟滞闸门（两侧都有迟滞，理由不同，见 toolbar_gate 模块文档）。
        let mut toolbar_gate = crate::toolbar_gate::ToolbarGate::new();
        // 待显示的状态：迟滞期间工具栏不可见，只需保留最后一份，到期时一次渲染。
        let mut toolbar_pending_state: Option<crate::toolbar::ToolbarState> = None;

        // 右键候选弹出菜单（best-effort）
        let mut popup_menu = match crate::popup_menu::PopupMenu::new(event_tx.clone()) {
            Ok(m) => Some(m),
            Err(e) => {
                error!("Failed to create popup menu: {}", e);
                None
            }
        };

        // 常驻工具栏（best-effort，失败不影响其它窗口）
        let mut toolbar = match crate::toolbar::Toolbar::new(event_tx.clone()) {
            Ok(t) => Some(t),
            Err(e) => {
                error!("Failed to create toolbar: {}", e);
                None
            }
        };

        // 已注册的全局热键（RegisterHotKey hwnd=NULL 绑定本线程；WM_HOTKEY 落线程消息队列，
        // 无目标窗口，DispatchMessage 不路由，须在下方消息泵中直接截获）。
        #[cfg(windows)]
        let mut global_hotkeys: Vec<GlobalHotkeyEntry> = Vec::new();

        // host-render 管理器（Windows）：由 SetHostRender 命令注入；None = 本地 LayeredWindow 路径。
        #[cfg(windows)]
        let mut host_render: Option<
            std::sync::Arc<wind_bridge::host_render_windows::HostRenderManager>,
        > = None;

        // 所有窗口构造完毕、即将进入消息泵。走到这里说明 GUI 该有的都建起来了；
        // 若客户仍报「无 GUI」，问题就在消息泵或显示逻辑，而非创建失败。
        wind_config::startup_trace::stage("ui-thread-loop");

        // Win32 消息循环 + 通道接收
        // 待处理命令队列：每轮排空通道并合并连续候选更新（只渲染最新一帧），
        // 避免长按翻页/连按方向键时 UpdateCandidates 堆积、松键后仍继续刷新。
        let mut pending: std::collections::VecDeque<UiCommand> = std::collections::VecDeque::new();
        'main: loop {
            // 状态提示气泡到期自动隐藏。
            // 用户正在与气泡交互（拖动 / 悬停其上 / 右键菜单打开）时**顺延**而非隐藏：
            // 否则气泡会在被操作的过程中凭空消失。交互结束后重新获得完整一份时长。
            if let Some(deadline) = tip_hide_at {
                let interacting = status_tip.as_ref().is_some_and(|t| t.interacting());
                // 「交互刚结束」这一沿：也要重新给满一份时长。轮询年代两次 tick 只差 ~8ms，
                // 「交互期间每轮顺延」自然就让结束时刻成了计时起点；事件驱动下 tick 变稀疏，
                // 不显式处理就会按**最后一次唤醒**的时刻算，气泡提前消失。
                // 与 `auto_hide` 的 `was_engaged` 同构。
                let just_ended = tip_was_interacting && !interacting;
                tip_was_interacting = interacting;
                if interacting || just_ended {
                    tip_hide_at = Some(
                        std::time::Instant::now()
                            + std::time::Duration::from_millis(tip_duration_ms.max(1)),
                    );
                } else if std::time::Instant::now() >= deadline {
                    if let Some(t) = &status_tip {
                        t.hide();
                    }
                    #[cfg(windows)]
                    if let Some(hr) = &host_render {
                        use wind_ipc::protocol::HOST_WINDOW_STATUS;
                        hr.hide_kind(HOST_WINDOW_STATUS);
                    }
                    tip_hide_at = None;
                }
            }
            // toast 到期自动隐藏
            if let Some(deadline) = toast_hide_at
                && std::time::Instant::now() >= deadline
            {
                if let Some(t) = &toast {
                    t.hide();
                }
                toast_hide_at = None;
            }
            // 工具栏显隐迟滞推进。无待定项时 is_active()=false 直接跳过（不取时间）。
            if toolbar_gate.is_active() {
                match toolbar_gate.tick_at(std::time::Instant::now()) {
                    crate::toolbar_gate::GateTick::Show => {
                        // 熬过窗口期没被 HideToolbar 撤销 → 真正显示。
                        // Toolbar::update → render 是所有显示路径的单点，末尾必 show。
                        //
                        // 这条日志不可省：`UI: UpdateToolbar` 打在闸门判定**之前**，命令到达
                        // 不等于工具栏出现（Deferred 的那些可能被撤销）。少了本行，日志里就
                        // 只剩「命令到了」和「撤销了」，唯独看不到「到底显示了没有」——排查
                        // 闪烁时最需要的恰恰是这个时刻。
                        debug!("UI: 工具栏显示（迟滞到期）");
                        if let (Some(t), Some(st)) = (&mut toolbar, &toolbar_pending_state) {
                            t.update(st);
                        }
                        toolbar_pending_state = None;
                    }
                    crate::toolbar_gate::GateTick::Hide => {
                        if let Some(t) = &mut toolbar {
                            t.hide();
                        }
                    }
                    crate::toolbar_gate::GateTick::None => {}
                }
            }
            // 非阻塞处理 Win32 消息（仅 Windows 有消息泵；非 Windows 为 mock，跳过）
            #[cfg(windows)]
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                    // 线程级全局热键：WM_HOTKEY 无目标窗口，须在泵中截获并回送协调器
                    if msg.message == WM_HOTKEY {
                        let id = msg.wParam.0 as i32;
                        if let Some(e) = global_hotkeys.iter().find(|e| e.id == id) {
                            debug!("UI: global hotkey triggered: {}", e.action);
                            let _ = event_tx.send(UiEvent::GlobalHotkey(e.action.clone()));
                        }
                        continue;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                // 系统明暗切换：wnd_proc 在上面 PeekMessage 期间被系统回调置标记（该消息是
                // SendMessage 广播，不入队列，泵里截不到），故在泵之后取走并回送协调器。
                if crate::window::take_system_color_changed() {
                    debug!("UI: 系统明暗设置变更 → 通知协调器");
                    let _ = event_tx.send(UiEvent::SystemThemeChanged);
                }
            }

            // 推进鼠标悬停防抖（稳定后才发出 Hover）
            candidate_window.tick();
            // 推进工具栏悬停高亮（按光标位置本地重绘）
            if let Some(t) = &mut toolbar {
                t.tick();
            }
            // 推进菜单（脏重绘 / 关闭）
            if let Some(m) = &mut popup_menu {
                m.tick();
            }
            // 推进软键盘（点击派发 / 长按重复 / 悬停重绘）
            if let Some(k) = &mut soft_keyboard {
                k.tick();
            }

            // 推进状态提示防抖（稳定后才真正显示气泡）
            if let Some((text, x, y, ch, ox, oy, dur, fixed, fx, fy)) = tip_debounce.poll()
                && let Some(t) = &mut status_tip
            {
                // host-render 分流：有活跃目标且写帧成功 → SHM + 本地隐藏；否则本地显示。
                #[cfg_attr(not(windows), allow(unused_mut))] // 仅 Windows 分支会改写它
                let mut host_ok = false;
                #[cfg(windows)]
                if let Some(hr) = &host_render
                    && let Some(target) = hr.active_target()
                {
                    use wind_bridge::shared_render_frame::FrameParams;
                    use wind_ipc::protocol::HOST_WINDOW_STATUS;
                    let fo = if fixed {
                        t.render_frame_fixed(&text, fx, fy, x, y)
                    } else {
                        t.render_frame(&text, x, y, ch, ox, oy)
                    };
                    if let Some((bgra, w, h, sx, sy, sw)) = fo {
                        let p = FrameParams {
                            sequence: 0,
                            x: sx,
                            y: sy,
                            width: w,
                            height: h,
                            bgra: &bgra,
                            rects: &[],
                            rendered_hover_index: -1,
                            target_instance_id: 0,
                            software_shadow: sw,
                        };
                        match hr.write_frame_for_kind(HOST_WINDOW_STATUS, &target, &p) {
                            Ok(()) => {
                                t.hide();
                                host_ok = true;
                            }
                            Err(e) => {
                                tracing::warn!("host render 写 status 帧失败，回退本地: {}", e);
                            }
                        }
                    }
                }
                if !host_ok {
                    if fixed {
                        t.show_fixed(&text, fx, fy, x, y);
                    } else {
                        t.show(&text, x, y, ch, ox, oy);
                    }
                }
                // dur==0 → 常驻(always):不设隐藏时刻;否则按配置时长自动隐藏。
                tip_duration_ms = dur;
                tip_hide_at = if dur == 0 {
                    None
                } else {
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(dur))
                };
            }

            // 排空通道：合并连续候选更新（只保留最新一条），其它命令保序
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok(cmd) => {
                        // 新候选更新若紧跟在另一候选更新之后，丢弃旧的（只渲染最新帧）
                        if matches!(cmd, UiCommand::UpdateCandidates { .. })
                            && matches!(pending.back(), Some(UiCommand::UpdateCandidates { .. }))
                        {
                            pending.pop_back();
                        }
                        pending.push_back(cmd);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            let had_cmd = !pending.is_empty();
            // 一轮处理完所有待办（候选更新已合并为至多一条），不留积压到下一轮
            while let Some(cmd) = pending.pop_front() {
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
                        caret_x,
                        caret_y,
                        caret_height,
                        caret_valid,
                        fixed,
                        fixed_x,
                        fixed_y,
                    } => {
                        debug!(
                            "UI: UpdateCandidates ({} items, selected={}, hover={}, page={}/{}, pos={},{})",
                            candidates.len(),
                            selected,
                            hover,
                            page,
                            total_pages,
                            caret_x,
                            caret_y
                        );
                        // 编码区归宿主画时，候选窗拿到数据也不显示——数据恒下发，
                        // 显示与否由这个标志决定（数据/渲染解耦，见 UiCommand 注释）。
                        let preedit = if preedit_host_owned {
                            String::new()
                        } else {
                            preedit
                        };
                        candidate_window.update(
                            &preedit,
                            preedit_caret,
                            &mode_label,
                            candidates,
                            selected,
                            hover,
                            page,
                            total_pages,
                        );
                        candidate_window.set_position(caret_x, caret_y, caret_height, caret_valid);
                        candidate_window.set_fixed_position(fixed.then_some((fixed_x, fixed_y)));
                        // host-render 分流：有活跃目标时渲染到 SHM，本地窗口互斥隐藏。
                        // 无目标或 host-render 未注入时落本地 LayeredWindow 路径（零改动）。
                        #[cfg(windows)]
                        if let Some(hr) = &host_render
                            && try_host_render_candidates(hr, &mut candidate_window)
                        {
                            continue; // 跳过本地 show()，分流完成
                        }
                        candidate_window.show();
                    }
                    UiCommand::HideCandidates => {
                        debug!("UI: HideCandidates");
                        // host-render 侧先 hide（hide 必达，幂等双发）
                        #[cfg(windows)]
                        if let Some(hr) = &host_render {
                            use wind_ipc::protocol::{HOST_WINDOW_CANDIDATE, HOST_WINDOW_TOOLTIP};
                            hr.hide_kind(HOST_WINDOW_CANDIDATE);
                            hr.hide_kind(HOST_WINDOW_TOOLTIP);
                        }
                        candidate_window.hide();
                        if let Some(m) = &mut popup_menu {
                            m.hide();
                        }
                    }
                    UiCommand::ShowCandidateMenu { items, anchor } => {
                        debug!(
                            "UI: ShowMenu ({} items) at ({},{}) {:?}",
                            items.len(),
                            anchor.x,
                            anchor.y,
                            anchor.placement
                        );
                        if let Some(m) = &mut popup_menu {
                            m.show(items, anchor);
                        }
                    }
                    UiCommand::MenuKey(key) => {
                        if let Some(m) = &mut popup_menu {
                            m.on_key(key);
                        }
                    }
                    UiCommand::HideMenu => {
                        if let Some(m) = &mut popup_menu {
                            m.hide();
                        }
                    }
                    UiCommand::CopyToClipboard(text) => {
                        crate::popup_menu::set_clipboard_text(&text);
                    }
                    UiCommand::OpenPath(path) => {
                        open_path(&path);
                    }
                    UiCommand::OpenApp { path, args } => {
                        open_app(&path, &args);
                    }
                    UiCommand::TakeScreenshot { dir } => {
                        let ts = crate::screenshot::timestamp();
                        let dir = std::path::PathBuf::from(&dir);
                        let mut saved = 0usize;
                        let mut candidate_to_clipboard = false;

                        // 候选窗口：保存文件 + 同时复制到剪贴板（与 Go 对齐）
                        if candidate_window.is_visible() {
                            let path = dir.join(format!("candidate_{ts}.png"));
                            match candidate_window.capture_to_file(&path) {
                                Ok(_) => {
                                    saved += 1;
                                    info!("Screenshot saved: {:?}", path);
                                    match candidate_window.capture_to_clipboard() {
                                        Ok(_) => candidate_to_clipboard = true,
                                        Err(e) => tracing::warn!("Screenshot clipboard: {}", e),
                                    }
                                }
                                Err(e) => tracing::warn!("Screenshot candidate: {}", e),
                            }
                        }
                        // 工具栏
                        if let Some(tb) = &toolbar
                            && tb.is_visible()
                        {
                            let path = dir.join(format!("toolbar_{ts}.png"));
                            match tb.capture_to_file(&path) {
                                Ok(_) => {
                                    saved += 1;
                                    info!("Screenshot saved: {:?}", path);
                                }
                                Err(e) => tracing::warn!("Screenshot toolbar: {}", e),
                            }
                        }
                        // 状态提示
                        if let Some(st) = &status_tip
                            && st.is_visible()
                        {
                            let path = dir.join(format!("status_tip_{ts}.png"));
                            match st.capture_to_file(&path) {
                                Ok(_) => {
                                    saved += 1;
                                    info!("Screenshot saved: {:?}", path);
                                }
                                Err(e) => tracing::warn!("Screenshot status_tip: {}", e),
                            }
                        }
                        // 悬停提示（编码反查气泡）
                        if candidate_window.tooltip_is_visible() {
                            let path = dir.join(format!("tooltip_{ts}.png"));
                            match candidate_window.tooltip_capture_to_file(&path) {
                                Ok(_) => {
                                    saved += 1;
                                    info!("Screenshot saved: {:?}", path);
                                }
                                Err(e) => tracing::warn!("Screenshot tooltip: {}", e),
                            }
                        }
                        // 右键菜单
                        if let Some(pm) = &popup_menu
                            && pm.is_visible()
                        {
                            let path = dir.join(format!("popup_menu_{ts}.png"));
                            match pm.capture_to_file(&path) {
                                Ok(_) => {
                                    saved += 1;
                                    info!("Screenshot saved: {:?}", path);
                                }
                                Err(e) => tracing::warn!("Screenshot popup_menu: {}", e),
                            }
                        }
                        // Toast（通常不可见，有则顺带保存）
                        if let Some(t) = &toast
                            && t.is_visible()
                        {
                            let path = dir.join(format!("toast_{ts}.png"));
                            match t.capture_to_file(&path) {
                                Ok(_) => {
                                    saved += 1;
                                    info!("Screenshot saved: {:?}", path);
                                }
                                Err(e) => tracing::warn!("Screenshot toast: {}", e),
                            }
                        }
                        info!("UI screenshots taken: {}, dir: {:?}", saved, dir);
                        // 结果 toast
                        if let Some(t) = &mut toast {
                            let msg = if saved > 0 {
                                if candidate_to_clipboard {
                                    format!(
                                        "已保存 {} 张截图（候选已复制到剪贴板）\n{}",
                                        saved,
                                        dir.display()
                                    )
                                } else {
                                    format!("已保存 {} 张截图\n{}", saved, dir.display())
                                }
                            } else {
                                "没有可见的 UI 窗口可截图".to_string()
                            };
                            let kind = if saved > 0 {
                                ToastKind::Success
                            } else {
                                ToastKind::Info
                            };
                            t.show(&msg, ToastPosition::BottomRight, kind);
                            toast_hide_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(4000),
                            );
                        }
                    }
                    UiCommand::ScreenshotCandidateToClipboard => {
                        let (msg, kind) = if candidate_window.is_visible() {
                            match candidate_window.capture_to_clipboard() {
                                Ok(_) => {
                                    info!("Candidate screenshot copied to clipboard");
                                    ("候选窗口已截图到剪贴板".to_string(), ToastKind::Success)
                                }
                                Err(e) => {
                                    tracing::warn!("Screenshot to clipboard failed: {}", e);
                                    (format!("截图到剪贴板失败：{}", e), ToastKind::Error)
                                }
                            }
                        } else {
                            ("候选窗口未显示，无法截图".to_string(), ToastKind::Info)
                        };
                        if let Some(t) = &mut toast {
                            t.show(&msg, ToastPosition::BottomRight, kind);
                            toast_hide_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(3000),
                            );
                        }
                    }
                    UiCommand::ScreenshotStatusTip { dir } => {
                        let ts = crate::screenshot::timestamp();
                        let (msg, kind) = match &status_tip {
                            Some(st) if st.is_visible() => {
                                let path = dir.join(format!("status_tip_{ts}.png"));
                                match st.capture_to_file(&path) {
                                    Ok(_) => {
                                        info!("Screenshot saved: {:?}", path);
                                        // 存盘的同时进剪贴板：截完就能直接粘贴，省去翻目录。
                                        let clip = st.capture_to_clipboard();
                                        if let Err(e) = &clip {
                                            tracing::warn!(
                                                "Screenshot status_tip clipboard: {}",
                                                e
                                            );
                                        }
                                        let suffix = if clip.is_ok() {
                                            "（已复制到剪贴板）"
                                        } else {
                                            ""
                                        };
                                        (
                                            format!(
                                                "状态提示气泡已截图{}\n{}",
                                                suffix,
                                                path.display()
                                            ),
                                            ToastKind::Success,
                                        )
                                    }
                                    Err(e) => {
                                        tracing::warn!("Screenshot status_tip: {}", e);
                                        (format!("截图失败：{}", e), ToastKind::Error)
                                    }
                                }
                            }
                            _ => ("状态提示气泡未显示，无法截图".to_string(), ToastKind::Info),
                        };
                        if let Some(t) = &mut toast {
                            t.show(&msg, ToastPosition::BottomRight, kind);
                            toast_hide_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(3000),
                            );
                        }
                    }
                    UiCommand::CopyTooltipText => {
                        let text = candidate_window.tooltip_text().to_string();
                        let (msg, kind) = if !text.is_empty() {
                            crate::popup_menu::set_clipboard_text(&text);
                            ("提示内容已复制".to_string(), ToastKind::Success)
                        } else {
                            ("提示内容为空，无法复制".to_string(), ToastKind::Info)
                        };
                        if let Some(t) = &mut toast {
                            t.show(&msg, ToastPosition::BottomRight, kind);
                            toast_hide_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(3000),
                            );
                        }
                    }
                    UiCommand::ScreenshotTooltip { dir } => {
                        let ts = crate::screenshot::timestamp();
                        let (msg, kind) = if candidate_window.tooltip_is_visible() {
                            let path = dir.join(format!("tooltip_{ts}.png"));
                            match candidate_window.tooltip_capture_to_file(&path) {
                                Ok(_) => {
                                    info!("Screenshot saved: {:?}", path);
                                    // 存盘的同时进剪贴板：截完就能直接粘贴，省去翻目录。
                                    let clip = candidate_window.tooltip_capture_to_clipboard();
                                    if let Err(e) = &clip {
                                        tracing::warn!("Screenshot tooltip clipboard: {}", e);
                                    }
                                    let suffix = if clip.is_ok() {
                                        "（已复制到剪贴板）"
                                    } else {
                                        ""
                                    };
                                    (
                                        format!("提示气泡已截图{}\n{}", suffix, path.display()),
                                        ToastKind::Success,
                                    )
                                }
                                Err(e) => {
                                    tracing::warn!("Screenshot tooltip: {}", e);
                                    (format!("截图失败：{}", e), ToastKind::Error)
                                }
                            }
                        } else {
                            ("提示气泡未显示，无法截图".to_string(), ToastKind::Info)
                        };
                        if let Some(t) = &mut toast {
                            t.show(&msg, ToastPosition::BottomRight, kind);
                            toast_hide_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(3000),
                            );
                        }
                    }
                    UiCommand::SetStatusMenuOpen(open) => {
                        if let Some(st) = &status_tip {
                            st.set_menu_open(open);
                        }
                    }
                    UiCommand::ReportStatusTipPos => {
                        if let Some(st) = &status_tip
                            && st.is_visible()
                        {
                            let (x, y) = st.content_origin();
                            let _ = event_tx.send(UiEvent::StatusTipMoved { x, y });
                        }
                    }
                    UiCommand::ReportCandidatePos => {
                        if candidate_window.is_visible() {
                            let (x, y) = candidate_window.content_origin();
                            let _ = event_tx.send(UiEvent::CandidateWindowMoved { x, y });
                        }
                    }
                    UiCommand::SetTooltipMenuOpen(open) => {
                        candidate_window.tooltip_set_menu_open(open);
                    }
                    UiCommand::ShowStatusTip {
                        text,
                        x,
                        y,
                        caret_height,
                        offset_x,
                        offset_y,
                        duration_ms,
                        fixed,
                        fixed_x,
                        fixed_y,
                    } => {
                        debug!("UI: ShowStatusTip '{}' at ({},{})", text, x, y);
                        // 经防抖：合并快速连续提示，避免气泡闪烁
                        tip_debounce.trigger((
                            text,
                            x,
                            y,
                            caret_height,
                            offset_x,
                            offset_y,
                            duration_ms,
                            fixed,
                            fixed_x,
                            fixed_y,
                        ));
                    }
                    UiCommand::HideStatusTip => {
                        // 取消待显示的防抖项 + 立即隐藏 + 清隐藏计时(常驻模式失焦)。
                        tip_debounce.cancel();
                        if let Some(t) = &status_tip {
                            t.hide();
                        }
                        #[cfg(windows)]
                        if let Some(hr) = &host_render {
                            use wind_ipc::protocol::HOST_WINDOW_STATUS;
                            hr.hide_kind(HOST_WINDOW_STATUS);
                        }
                        tip_hide_at = None;
                    }
                    UiCommand::ShowInputDiag(v) => {
                        // 惰性创建：失败仅记 error，不影响其它窗口。
                        if input_diag_hud.is_none() {
                            match crate::input_diag_hud::InputDiagHud::new(event_tx.clone()) {
                                Ok(h) => input_diag_hud = Some(h),
                                Err(e) => error!("Failed to create input diag HUD: {}", e),
                            }
                        }
                        if let Some(h) = input_diag_hud.as_mut() {
                            h.show_or_update(&v);
                        }
                    }
                    UiCommand::HideInputDiag => {
                        if let Some(h) = input_diag_hud.as_mut() {
                            h.hide();
                        }
                    }
                    UiCommand::CopyInputDiagText => {
                        if let Some(h) = input_diag_hud.as_ref() {
                            h.copy_text();
                        }
                    }
                    UiCommand::ShowToast {
                        text,
                        position,
                        kind,
                        duration_ms,
                    } => {
                        debug!("UI: ShowToast '{}' ({:?},{:?})", text, position, kind);
                        if let Some(t) = &mut toast {
                            t.show(&text, position, kind);
                            toast_hide_at = Some(
                                std::time::Instant::now()
                                    + std::time::Duration::from_millis(duration_ms.max(1)),
                            );
                        }
                    }
                    UiCommand::UpdateToolbar(tb_state) => {
                        debug!("UI: UpdateToolbar {:?}", tb_state);
                        // 传当前可见性：**内容更新与可见性提升是两件事**。本命令同时承担
                        // 「更新中英·标点·全半角」与「让工具栏出现」，整条延迟会让已显示
                        // 时按 Shift 切中英也慢一档，手感明显退化。
                        let visible = toolbar.as_ref().is_some_and(|t| t.is_visible());
                        match toolbar_gate.on_update(std::time::Instant::now(), visible) {
                            crate::toolbar_gate::UpdateAction::RenderNow => {
                                if let Some(t) = &mut toolbar {
                                    t.update(&tb_state);
                                }
                                toolbar_pending_state = None;
                            }
                            crate::toolbar_gate::UpdateAction::Deferred => {
                                debug!(
                                    "UI: UpdateToolbar 待显示（迟滞 {}ms）",
                                    crate::toolbar_gate::SHOW_DEBOUNCE.as_millis()
                                );
                                toolbar_pending_state = Some(tb_state);
                            }
                        }
                    }
                    UiCommand::HideToolbar => {
                        match toolbar_gate.on_hide(std::time::Instant::now()) {
                            crate::toolbar_gate::HideAction::CancelledPending => {
                                // 撤销待显示——消除 DocMgr churn 闪烁的关键一步。
                                // 工具栏本就不可见，无需再排隐藏。
                                toolbar_pending_state = None;
                                debug!("UI: HideToolbar 撤销待显示工具栏（迟滞窗口内）");
                            }
                            crate::toolbar_gate::HideAction::Scheduled => {
                                debug!(
                                    "UI: HideToolbar (debounced {}ms)",
                                    crate::toolbar_gate::HIDE_DEBOUNCE.as_millis()
                                );
                            }
                        }
                    }
                    UiCommand::SetToolbarPos { x, y } => {
                        debug!("UI: SetToolbarPos ({},{})", x, y);
                        if let Some(t) = &mut toolbar {
                            t.set_pos(x, y);
                        }
                    }
                    UiCommand::SetToolbarCorner {
                        work_right,
                        work_bottom,
                    } => {
                        debug!("UI: SetToolbarCorner work=({},{})", work_right, work_bottom);
                        if let Some(t) = &mut toolbar {
                            t.set_corner(work_right, work_bottom);
                        }
                    }
                    UiCommand::SetToolbarAutoHide { enabled, delay_ms } => {
                        debug!(
                            "UI: SetToolbarAutoHide enabled={} delay={}ms",
                            enabled, delay_ms
                        );
                        if let Some(t) = &mut toolbar {
                            t.set_auto_hide(enabled, delay_ms);
                        }
                    }
                    UiCommand::SetToolbarVertical(v) => {
                        debug!("UI: SetToolbarVertical {}", v);
                        if let Some(t) = &mut toolbar {
                            t.set_vertical(v);
                        }
                    }
                    UiCommand::SetToolbarLayout(items) => {
                        debug!("UI: SetToolbarLayout n={}", items.len());
                        if let Some(t) = &mut toolbar {
                            t.set_layout(items);
                        }
                    }
                    UiCommand::SetTheme(theme) => {
                        debug!("UI: SetTheme (dark={})", theme.is_dark);
                        let t = *theme;
                        last_theme = Some(t.clone());
                        if let Some(k) = &mut soft_keyboard {
                            k.set_theme(&t);
                        }
                        if let Some(tb) = &mut toolbar {
                            tb.set_theme(&t);
                            // 仅在可见时重绘：repaint→render 末尾无条件 show，对隐藏中的
                            // 工具栏调用会把它显形，绕过 toolbar_gate 的显示迟滞。
                            // （启动时 last_state 为 None，repaint 本就是 no-op，故此前不显形。）
                            if tb.is_visible() {
                                tb.repaint();
                            }
                        }
                        if let Some(m) = &mut popup_menu {
                            m.set_theme(&t);
                        }
                        if let Some(st) = &mut status_tip {
                            st.set_theme(&t);
                        }
                        if let Some(to) = &mut toast {
                            to.set_theme(&t);
                        }
                        candidate_window.set_theme(t); // 同时更新其 tooltip
                        if candidate_window.is_visible() {
                            // host 模式下 visible=true 表示「内容在 host 窗口可见」，重绘须走
                            // host 分流重写 SHM 帧，不得弹本地窗口（否则与 host 窗双显）。
                            #[cfg(windows)]
                            let host_handled = match &host_render {
                                Some(hr) => try_host_render_candidates(hr, &mut candidate_window),
                                None => false,
                            };
                            #[cfg(not(windows))]
                            let host_handled = false;
                            if !host_handled {
                                candidate_window.show();
                            }
                        }
                    }
                    UiCommand::SetCandidateTextFamily(family) => {
                        candidate_window.set_text_family_override(&family);
                    }
                    UiCommand::SetCandidateLayout {
                        vertical,
                        rotated,
                        upright,
                    } => {
                        candidate_window.set_orientation(vertical, rotated, upright);
                    }
                    UiCommand::SetPreeditEmbedded(embedded) => {
                        candidate_window.set_preedit_embedded(embedded);
                    }
                    UiCommand::SetCandidateFontSize(size) => {
                        candidate_window.set_font_size_override(size);
                    }
                    UiCommand::SetCandidateFont {
                        family,
                        fallback,
                        scripts,
                    } => {
                        // 两步顺序承重：`set_font_family` 换的是 TextFormat 的全局字族，
                        // `set_font_plan` 换的是链与指派，后者的链首必须是前者刚设进去的
                        // 那个字族（空字族回落内置默认的判定在 `resolve_family` 一处）。
                        candidate_window.set_font_family(&family);
                        candidate_window.set_font_plan(&family, &fallback, &scripts);
                    }
                    UiCommand::SetCandidateMinSize {
                        width_horizontal,
                        width_vertical,
                        height_horizontal,
                        height_vertical,
                        rows,
                    } => {
                        candidate_window.set_min_size(
                            width_horizontal,
                            width_vertical,
                            height_horizontal,
                            height_vertical,
                            rows,
                        );
                    }
                    UiCommand::SetTooltipDelay(delay) => {
                        candidate_window.set_tooltip_delay(delay);
                    }
                    UiCommand::SetCandidateFlipWhenAbove(flip) => {
                        candidate_window.set_flip_when_above(flip);
                    }
                    UiCommand::SetCandidateSwapWhenAbove(swap) => {
                        candidate_window.set_swap_preedit_when_above(swap);
                    }
                    UiCommand::SetPagerInPreedit(on) => {
                        candidate_window.set_pager_in_preedit(on);
                    }
                    UiCommand::SetPagerDisplay(mode) => {
                        candidate_window.set_pager_display(mode);
                    }
                    UiCommand::SetPageNumberDisplay(mode) => {
                        candidate_window.set_page_number_display(mode);
                    }
                    UiCommand::SetTooltipChaiziFont { path, family } => {
                        candidate_window.set_chaizi_font(&path, &family);
                    }
                    UiCommand::RegisterGlobalHotkeys(entries) => {
                        #[cfg(windows)]
                        {
                            use windows::Win32::UI::Input::KeyboardAndMouse::{
                                HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
                            };
                            // 覆盖式：先反注册旧列表（配置重载可能改键/删项），再注册新列表
                            for e in &global_hotkeys {
                                let _ = unsafe { UnregisterHotKey(HWND::default(), e.id) };
                            }
                            for e in &entries {
                                let mods = HOT_KEY_MODIFIERS(e.modifiers | MOD_NOREPEAT.0);
                                match unsafe { RegisterHotKey(HWND::default(), e.id, mods, e.vk) } {
                                    Ok(()) => debug!(
                                        "UI: registered global hotkey {} (mods=0x{:X} vk=0x{:02X})",
                                        e.action, e.modifiers, e.vk
                                    ),
                                    // 失败（组合被其它程序占用等）仅告警，不影响其余热键
                                    Err(err) => tracing::warn!(
                                        "UI: register global hotkey {} failed: {}",
                                        e.action,
                                        err
                                    ),
                                }
                            }
                            global_hotkeys = entries;
                        }
                        #[cfg(not(windows))]
                        {
                            let _ = entries;
                        }
                    }
                    #[cfg(windows)]
                    UiCommand::SetHostRender(hr) => {
                        debug!("UI: SetHostRender");
                        host_render = Some(hr.0);
                    }
                    UiCommand::ShowSoftKeyboard {
                        pages,
                        current,
                        keys,
                    } => {
                        debug!("UI: ShowSoftKeyboard (page={current}, keys={})", keys.len());
                        if soft_keyboard.is_none() {
                            match crate::soft_keyboard::SoftKeyboard::new(event_tx.clone()) {
                                Ok(mut k) => {
                                    if let Some(t) = &last_theme {
                                        k.set_theme(t);
                                    }
                                    soft_keyboard = Some(k);
                                }
                                Err(e) => error!("软键盘窗口创建失败: {e}"),
                            }
                        }
                        if let Some(k) = &mut soft_keyboard {
                            k.show(pages, current, keys);
                        }
                    }
                    UiCommand::HideSoftKeyboard => {
                        debug!("UI: HideSoftKeyboard");
                        if let Some(k) = &mut soft_keyboard {
                            k.hide();
                        }
                    }
                    UiCommand::SoftKeyboardKeyState { slot, down } => {
                        if let Some(k) = &mut soft_keyboard {
                            k.set_key_down(&slot, down);
                        }
                    }
                    UiCommand::SoftKeyboardLayer { shift } => {
                        if let Some(k) = &mut soft_keyboard {
                            k.set_layer(shift);
                        }
                    }
                    UiCommand::Shutdown => {
                        info!("UI: Shutdown");
                        // host-render 全部隐藏（Shutdown 必达）
                        #[cfg(windows)]
                        if let Some(hr) = &host_render {
                            hr.hide_all();
                        }
                        candidate_window.hide();
                        if let Some(t) = &status_tip {
                            t.hide();
                        }
                        if let Some(t) = &toast {
                            t.hide();
                        }
                        if let Some(t) = &mut toolbar {
                            t.hide();
                        }
                        break 'main;
                    }
                }
            }
            if disconnected {
                info!("UI: Channel disconnected, shutting down");
                break 'main;
            }
            if !had_cmd {
                // 本循环唯一的休眠点：睡到「最近的计时器到期」，或被唤醒（新命令 /
                // Win32 消息）提前叫醒。
                //
                // 取代的是原先无条件的 8ms 休眠。那一版让 UI 线程在完全空闲时也每秒醒
                // 64~125 次（`Sleep` 被对齐到系统时钟粒度，而本进程不调 `timeBeginPeriod`，
                // 故实际频率还随别的进程改动全局粒度而浮动），是服务静态 CPU 占用的唯一
                // 来源，也让 CPU 无法进入深度 C-state。
                //
                // ⚠ **新增任何「靠每轮被调用才能推进」的状态，都必须在下面登记它的到期
                // 时刻**。漏登记的后果不是变慢，而是它在这里睡下去就再也不会被推进——
                // 除非碰巧有别的事把线程叫醒。这类 bug 表现为「偶尔不生效」，极难复现。
                let now = std::time::Instant::now();
                let next_deadline = [
                    // 状态气泡自动隐藏
                    tip_hide_at,
                    // toast 自动隐藏
                    toast_hide_at,
                    // 工具栏显隐迟滞（50 / 120ms 两侧）
                    toolbar_gate.deadline(),
                    // 状态提示防抖（60ms 尾沿）
                    tip_debounce.deadline(),
                    // 候选窗悬停激活闸门
                    candidate_window.next_deadline(),
                    // 工具栏自动隐藏（含淡出动画的逐帧推进）
                    toolbar.as_ref().and_then(|t| t.next_deadline(now)),
                    // 菜单外点击轮询——唯一仍需定期唤醒者，且仅在菜单可见时
                    popup_menu.as_ref().and_then(|m| m.next_deadline(now)),
                    // 软键盘键帽的长按重复
                    soft_keyboard.as_ref().and_then(|k| k.next_deadline()),
                ]
                .into_iter()
                .flatten()
                .min();
                // 全为 None ⇒ 无限等待，线程真正零开销地停下，直到有命令或消息到来。
                wait_port.wait(next_deadline.map(|d| d.saturating_duration_since(now)));
            }
        }

        // 消息泵退出 = GUI 全部失效，而主线程同样不会察觉（见函数文档）。
        wind_config::startup_trace::stage("ui-thread-EXIT");
    }
}

impl Drop for UiManager {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(UiCommand::Shutdown);
    }
}

/// host-render 候选分流：有活跃目标时渲染候选帧（含悬停 tooltip 帧联动）写 SHM，
/// 本地窗口互斥隐藏（hide_local_window_only，保留跨帧防抖/粘滞状态）。
/// 返回 true = 已由 host 路径处理（调用方跳过本地 show）；false = 无目标/写帧失败 → 走本地路径。
#[cfg(windows)]
fn try_host_render_candidates(
    hr: &std::sync::Arc<wind_bridge::host_render_windows::HostRenderManager>,
    candidate_window: &mut CandidateWindow,
) -> bool {
    use wind_bridge::shared_render_frame::FrameParams;
    use wind_ipc::protocol::{HOST_WINDOW_CANDIDATE, HOST_WINDOW_TOOLTIP, HostRenderHitRect};
    let Some(target) = hr.active_target() else {
        return false;
    };
    match candidate_window.render_frame() {
        Some(frame) => {
            let rects: Vec<HostRenderHitRect> = frame
                .hit_rects
                .iter()
                .map(|(idx, r)| HostRenderHitRect {
                    // 翻页按钮的内部 tag（HOVER_PAGE_PREV/NEXT = 100000/100001）重映射为
                    // SHM/C++ 线约定（-1 上页 / -2 下页，HostWindow.cpp _HitTest）——与
                    // manager_macos.rs 的 darwin 重映射对齐。正数 tag 会被 C++ 当候选索引，
                    // 点击翻页变成 mouse_select(100000) 被丢弃（真机踩坑：翻页点击无效）。
                    index: match *idx {
                        i if i == HOVER_PAGE_PREV => -1,
                        i if i == HOVER_PAGE_NEXT => -2,
                        i => i,
                    },
                    x: r.x as i32,
                    y: r.y as i32,
                    w: r.w as i32,
                    h: r.h as i32,
                })
                .collect();
            let params = FrameParams {
                sequence: 0,
                x: frame.screen_x,
                y: frame.screen_y,
                width: frame.width,
                height: frame.height,
                bgra: &frame.buf,
                rects: &rects,
                // C++ 以此为 hover 去重基线（_UpdateHitRects → _lastHoverIndex），值域是
                // C++ hover 约定（-1 无 / -2 上页 / -3 下页）——内部 tag 须同步重映射。
                rendered_hover_index: match candidate_window.hover() {
                    i if i == HOVER_PAGE_PREV => -2,
                    i if i == HOVER_PAGE_NEXT => -3,
                    i => i,
                },
                target_instance_id: 0,
                software_shadow: frame.software_shadow,
            };
            match hr.write_frame_for_kind(HOST_WINDOW_CANDIDATE, &target, &params) {
                Ok(()) => {
                    candidate_window.hide_local_window_only();
                    // 悬停 tooltip 帧联动：有悬停写帧，无悬停隐藏（幂等）。
                    match candidate_window.render_tooltip_frame(frame.screen_x, frame.screen_y) {
                        Some((tt_buf, tt_w, tt_h, tt_x, tt_y, tt_shadow)) => {
                            let tt_params = FrameParams {
                                sequence: 0,
                                x: tt_x,
                                y: tt_y,
                                width: tt_w,
                                height: tt_h,
                                bgra: &tt_buf,
                                rects: &[],
                                rendered_hover_index: -1,
                                target_instance_id: 0,
                                software_shadow: tt_shadow,
                            };
                            if let Err(e) =
                                hr.write_frame_for_kind(HOST_WINDOW_TOOLTIP, &target, &tt_params)
                            {
                                tracing::warn!("host render 写 tooltip 帧失败: {}", e);
                                hr.hide_kind(HOST_WINDOW_TOOLTIP);
                            }
                        }
                        None => hr.hide_kind(HOST_WINDOW_TOOLTIP),
                    }
                    true
                }
                Err(e) => {
                    // 写帧失败必须回退本地窗口，不得静默丢帧
                    tracing::warn!("host render 写帧失败，回退本地窗口: {}", e);
                    false
                }
            }
        }
        None => {
            // 无内容可渲染：隐藏 host 侧 + 本地侧，幂等
            hr.hide_kind(HOST_WINDOW_CANDIDATE);
            hr.hide_kind(HOST_WINDOW_TOOLTIP);
            candidate_window.hide();
            true
        }
    }
}

/// 用资源管理器打开路径（best-effort）
#[cfg(windows)]
pub(crate) fn open_path(path: &str) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// macOS：交给 LaunchServices（`/usr/bin/open`）。目录→Finder、URL→默认浏览器，
/// 与 Windows 的 `ShellExecuteW("open", ...)` 语义对齐。
#[cfg(target_os = "macos")]
pub(crate) fn open_path(path: &str) {
    match std::process::Command::new("/usr/bin/open")
        .arg(path)
        .spawn()
    {
        Ok(_) => debug!("open_path: {path}"),
        Err(e) => tracing::warn!("open_path 失败 {path}: {e}"),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn open_path(_path: &str) {}

/// 启动可执行程序并传参（ShellExecute open + params）；args 为空时等价 open_path。
#[cfg(windows)]
pub(crate) fn open_app(path: &str, args: &str) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let params: Vec<u16> = args.encode_utf16().chain(std::iter::once(0)).collect();
    let params_ptr = if args.is_empty() {
        PCWSTR::null()
    } else {
        PCWSTR(params.as_ptr())
    };
    unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            params_ptr,
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// macOS：`.app` 包走 `open -a … --args …`（LaunchServices 负责激活已在跑的实例），
/// 裸可执行文件直接 spawn。args 按空白切分——Windows 侧那串是拼给 ShellExecute 的
/// 单一参数串，macOS 需要切成 argv；含空格的取值请在构造端加引号（`build_settings_args`
/// 已如此），此处按引号成对保留。
#[cfg(target_os = "macos")]
pub(crate) fn open_app(path: &str, args: &str) {
    let argv = split_args(args);
    let spawned = if path.ends_with(".app") {
        let mut c = std::process::Command::new("/usr/bin/open");
        c.arg("-a").arg(path);
        if !argv.is_empty() {
            c.arg("--args").args(&argv);
        }
        c.spawn()
    } else {
        std::process::Command::new(path).args(&argv).spawn()
    };
    match spawned {
        Ok(_) => debug!("open_app: {path} {args}"),
        Err(e) => tracing::warn!("open_app 失败 {path}: {e}"),
    }
}

/// 把 Windows 口径的单一参数串切成 argv：按空白切分，成对的 `"` 内空白不切。
/// 不做转义处理（`\"` 之类）——构造端只会产出简单的 `--k=v` 与带引号的取值。
#[cfg(target_os = "macos")]
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for ch in s.chars() {
        match ch {
            '"' => in_quote = !in_quote,
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn open_app(_path: &str, _args: &str) {}

#[cfg(test)]
mod menu_id_tests {
    use super::*;

    /// `to_menu_id` 与 `from_menu_id` 是**两份手写的 match**，新增 MenuCmd 变体必须同时改两处。
    /// 漏掉 `from_menu_id` 那侧的表现是「点了菜单毫无反应」且不留任何日志——极难联想到 id 映射。
    /// 本测试锁住双向一致性，顺带也能抓出两个变体撞同一 id 的情况（撞号时 round-trip 必然不等）。
    ///
    /// ⚠ 编译器抓不到下面这张列表的遗漏：新增 MenuCmd 变体时请手动补一行。
    #[test]
    fn menu_cmd_id_roundtrip() {
        let all = [
            MenuCmd::SchemaEnglish,
            MenuCmd::TogglePunct,
            MenuCmd::ToggleWidth,
            MenuCmd::ToggleS2t,
            MenuCmd::ToggleToolbar,
            MenuCmd::ReloadConfig,
            MenuCmd::RestartService,
            MenuCmd::OpenConfigDir,
            MenuCmd::OpenAppDir,
            MenuCmd::OpenLogDir,
            MenuCmd::OpenDictionary,
            MenuCmd::OpenSettings,
            MenuCmd::OpenAbout,
            MenuCmd::TakeScreenshot,
            MenuCmd::ScreenshotCandidateToClipboard,
            MenuCmd::ToggleInputDiagnostics,
            MenuCmd::TogglePasswordSuppress,
            MenuCmd::FirstShowMode(0),
            MenuCmd::FirstShowMode(1),
            MenuCmd::FirstShowMode(2),
            MenuCmd::InitialMode(0),
            MenuCmd::InitialMode(1),
            MenuCmd::InitialMode(2),
            MenuCmd::InitialPunct(0),
            MenuCmd::InitialPunct(1),
            MenuCmd::InitialPunct(2),
            MenuCmd::StatusToggleAlways,
            MenuCmd::StatusResetPosition,
            MenuCmd::StatusScreenshot,
            MenuCmd::StatusTogglePinned,
            MenuCmd::StatusToggleShowOnFocus,
            MenuCmd::TooltipCopy,
            MenuCmd::TooltipScreenshot,
            MenuCmd::InputDiagCopy,
            MenuCmd::InputDiagToggleFreeze,
            MenuCmd::InputDiagToggleTopmost,
            MenuCmd::InputDiagToggleSection(0),
            MenuCmd::InputDiagToggleSection(3),
            MenuCmd::AutoPairRule(0),
            MenuCmd::AutoPairRule(2),
            MenuCmd::IconToggleColors,
            MenuCmd::IconToggleSizeMarks,
            MenuCmd::IconBadgeShape(0),
            MenuCmd::IconBadgeShape(5),
            MenuCmd::SchemaSelect(0),
            MenuCmd::SchemaSelect(7),
            MenuCmd::ThemeSelect(3),
            MenuCmd::FilterMode(2),
            MenuCmd::ThemeStyle(1),
        ];
        for cmd in all {
            let id = MenuKind::Command(cmd).to_menu_id();
            let back = MenuKind::from_menu_id(id).unwrap_or_else(|| {
                panic!("{cmd:?} → id={id} 无法反解析（from_menu_id 漏了该 id）")
            });
            assert_eq!(
                back,
                MenuKind::Command(cmd),
                "{cmd:?} → id={id} → {back:?}：双向映射不一致（多为 id 撞号）"
            );
        }
    }

    /// 不可点击项（分隔符 / 子菜单 / 展示文本行）恒为 0，且 0 不得反解析成任何
    /// 动作——否则点到分隔符/标题行会误触发某个命令。
    #[test]
    fn non_clickable_ids_are_inert() {
        assert_eq!(MenuKind::Separator.to_menu_id(), 0);
        assert_eq!(MenuKind::Submenu.to_menu_id(), 0);
        assert_eq!(MenuKind::Label.to_menu_id(), 0);
        assert!(MenuKind::from_menu_id(0).is_none());
    }
}
