//! `impl MessageHandler for Coordinator`：TSF 桥接层全部事件的入口
//!（按键/焦点/IME 激活/光标/菜单/候选交互/诊断），含失焦归属校验
//! `is_stale_focus_event` 与 ext 信封解码辅助。
//!（coordinator 子模块，自 coordinator.rs 平移，纯搬运。）

use super::*;

impl Coordinator {
    /// 失焦类事件的归属校验：`client_token` 不是当前活动客户端时判为**陈旧事件**并丢弃。
    ///
    /// 必要性来自 DLL 侧刻意安排的时序：DocMgr 级失焦是噪声信号（VSCode 实测一次应用切换
    /// 伴随 5 次），故 focus_lost 不在那里发，改由 `OnKillThreadFocus` 发出——实测**比
    /// DocMgr 级失焦晚约 100ms**（见 TextService.cpp 失焦分支注释）。而新宿主的
    /// focus_gained 在十几毫秒内就送达，于是跨宿主切换时到达顺序恒为
    /// 「新宿主 focus_gained → 旧宿主 focus_lost」。
    ///
    /// `ime_active` 是全局单例（不区分客户端），无校验时后者会把前者刚建立的激活态清掉：
    /// 工具栏闪一下即隐藏。服务端日志指纹＝`UpdateToolbar` 后约 90ms 紧跟一条 `HideToolbar`，
    /// 且此后长时间没有新的 `UpdateToolbar`。
    ///
    /// 两种放行情形：`client_token == 0`（旧 DLL 不带 token，保持既有行为）、
    /// `active == 0`（尚无任何客户端获焦，无从判定归属）。
    ///
    /// 注意本校验**只挡跨宿主**：同一进程内多个 DocMgr 共用一个 token，宿主自身在两个
    /// DocMgr 间抖动时 token 相同、一律放行——那条路径是 doc_changed 先发 focus_lost 紧接
    /// focus_gained，间隔 <10ms，由 UI 层 50ms 隐藏防抖吸收。
    pub(crate) fn is_stale_focus_event(&self, client_token: u64, what: &str) -> bool {
        let active = self.push_server.active_token();
        if client_token == 0 || active == 0 || client_token == active {
            return false;
        }
        tracing::debug!(
            "{}: 丢弃陈旧失焦 token={:#x} active={:#x}（旧宿主迟到的失焦，不动激活态与 UI）",
            what,
            client_token,
            active
        );
        true
    }
}

/// 解析扩展信封里的 `{"x":123,"y":456}` 落点 body。
///
/// 非法/缺字段/越界一律返回 `None` 交调用方忽略，而不是取 0 兜底：位置类消息拿默认值
/// 比丢掉一次拖动坏得多——`(0,0)` 会被当成合法坐标落盘，候选窗就此跑到屏幕左上角。
fn decode_ext_point(body: &[u8]) -> Option<(i32, i32)> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let x = v.get("x")?.as_i64()?;
    let y = v.get("y")?.as_i64()?;
    Some((i32::try_from(x).ok()?, i32::try_from(y).ok()?))
}

/// `shot.result` → Toast 文案。
///
/// 抽成纯函数是为了可测：这里全是措辞分支，而措辞正是**必须与 Windows 侧
/// `manager.rs` 逐字一致**的东西——两平台同一操作得到不同说法是最没必要的分叉，
/// 而这种分叉不会有任何编译或运行期信号。
fn shot_result_message(v: &serde_json::Value) -> (String, ToastKind) {
    let results = v.get("results").and_then(|r| r.as_array());
    let ok = |r: &serde_json::Value| r.get("ok").and_then(|b| b.as_bool()) == Some(true);
    if v.get("mode").and_then(|m| m.as_str()) == Some("all") {
        // 「截图所有窗口」：本进程截的候选窗数量由请求原样带回，与 `.app` 这边的成功数
        // 相加，合成**一条** Toast（各弹各的会连弹三四条）。
        let n = v.get("already").and_then(|n| n.as_u64()).unwrap_or(0) as usize
            + results.map_or(0, |a| a.iter().filter(|r| ok(r)).count());
        let dir = v.get("dir").and_then(|d| d.as_str()).unwrap_or("");
        if n == 0 {
            return ("没有可见窗口可截图".to_string(), ToastKind::Info);
        }
        return if v.get("already_clipboard").and_then(|b| b.as_bool()) == Some(true) {
            (
                format!("已保存 {n} 张截图（候选已复制到剪贴板）\n{dir}"),
                ToastKind::Success,
            )
        } else {
            (format!("已保存 {n} 张截图\n{dir}"), ToastKind::Success)
        };
    }
    // 单窗截图（气泡/提示自身右键菜单里的「截图此窗口」）。
    let Some(r) = results.and_then(|a| a.first()) else {
        return ("截图失败：无结果".to_string(), ToastKind::Error);
    };
    let label = match r.get("target").and_then(|t| t.as_str()) {
        Some("tooltip") => "悬停提示",
        _ => "状态提示气泡",
    };
    if ok(r) {
        let path = r.get("path").and_then(|p| p.as_str()).unwrap_or("");
        let suffix = if r.get("clipboard").and_then(|b| b.as_bool()) == Some(true) {
            "（已复制到剪贴板）"
        } else {
            ""
        };
        (format!("{label}已截图{suffix}\n{path}"), ToastKind::Success)
    } else {
        match r.get("reason").and_then(|x| x.as_str()) {
            // 不可见不是错误：用户在气泡消失之后才点的菜单，如实告知即可。
            Some("not_visible") | None => (format!("{label}未显示，无法截图"), ToastKind::Info),
            Some(e) => (format!("截图失败：{e}"), ToastKind::Error),
        }
    }
}

impl MessageHandler for Coordinator {
    /// 见 trait 文档：DLL/宿主新连接建立时的兜底刷新。真机复现（2026-08-17）：服务重启时
    /// alacritty.exe 早已在前台，管道重连只续发 `caret_update`，从没有新的 `FOCUS_GAINED`
    /// 促发 `update_active_compat`——`caret_offset_*` 等 per-app 规则整个会话都停在默认值，
    /// 用户得手动切一次焦点才生效。
    ///
    /// 只在该 pid 确认是当前前台窗口时才写 `active_compat`，避免后台宿主的无关重连
    /// 覆盖掉真正聚焦应用的规则（见 `foreground_pid` 文档）。非 Windows 平台本回调
    /// 也不会被 `wind-bridge` 调用（`handle_client` 本身是 `#[cfg(windows)]`），此处
    /// 仅为满足 trait 签名。
    fn handle_client_connected(&self, pid: u32) {
        #[cfg(windows)]
        {
            // ⚠ **必须先校正名字再刷规则**：下面那步是缓存优先的，缓存错了它照错的抄。
            // 新进程必然连一次，这是 pid_names 唯一的自愈时机（详见 revalidate_pid_name）。
            self.revalidate_pid_name(pid, &crate::coordinator::process_name(pid));
            self.apply_connected_pid_compat(pid, crate::foreground_pid());
        }
        #[cfg(not(windows))]
        let _ = pid;
    }

    fn handle_menu_command(&self, command: &str) -> Option<StatusUpdateData> {
        info!("Menu command: {}", command);
        match command {
            "toggle_mode" => self.handle_toggle_mode().0,
            "toggle_width" => {
                {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.full_width = !s.full_width;
                }
                self.record_last_state();
                self.push_state_update();
                self.show_status();
                Some(self.build_status())
            }
            "toggle_punct" => {
                let effective_chinese = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.chinese_mode && !s.caps_lock
                };
                if effective_chinese {
                    {
                        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        s.chinese_punct = !s.chinese_punct;
                    }
                    self.record_last_state();
                    self.push_state_update();
                    self.show_status();
                }
                Some(self.build_status())
            }
            "switch_engine" => {
                self.cycle_schema();
                Some(self.build_status())
            }
            "toggle_s2t" => {
                let on = {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.s2t_enabled = !s.s2t_enabled;
                    s.s2t_enabled
                };
                self.persist_s2t_enabled(on);
                self.show_status();
                Some(self.build_status())
            }
            _ => None,
        }
    }

    /// macOS `.app` 查询功能主菜单：构建菜单树并编码为 `CmdMenuShow` 帧字节。
    /// Windows 走进程内 `show_main_menu` 渲染，不用此路径（返回空帧亦无害）。
    fn query_menu_encoded(&self, simplified: bool) -> Vec<u8> {
        #[cfg(target_os = "macos")]
        {
            // IMK 输入源菜单用精简树(无子菜单)；候选框右键/菜单栏指示器用完整树(带子菜单，
            // 经 inProcess 直接投递，AppKit 能正确处理嵌套子菜单)。
            let items = if simplified {
                self.build_menu_items_macos()
            } else {
                self.build_main_menu_items()
            };
            let nodes = Self::menu_items_to_nodes(&items);
            wind_ipc::codec::encode_menu_show(&nodes)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = simplified;
            Vec::new()
        }
    }

    /// macOS `.app` 回传统一菜单选择：由菜单 id 还原动作并派发。
    fn handle_menu_action_id(&self, id: i32) {
        if let Some(kind) = wind_ui_types::MenuKind::from_menu_id(id) {
            self.menu_action(kind);
        } else {
            tracing::debug!("handle_menu_action_id: 未知菜单 id {}", id);
        }
    }

    /// macOS `.app` 上报前台上下文（聚焦时快照）：缓存 app/title/sel 供命令直通车取值。
    fn handle_front_context(&self, app: &str, title: &str, sel: &str) {
        let mut fc = self.front_ctx.lock().unwrap_or_else(|e| e.into_inner());
        *fc = (app.to_string(), title.to_string(), sel.to_string());
    }

    /// 鼠标左键点选候选（macOS `.app` / Windows host-render DLL）：
    /// ≥0 复用 `mouse_select`（提交页内第 N 个候选）；负值为翻页按钮
    /// （-1 上页 / -2 下页，对齐 Go HandleCandidateSelect 的分流），复用本地窗口
    /// 点击翻页的 `mouse_page` 路径（翻页后经 notify_ui_update 重推帧）。
    fn handle_candidate_select(&self, page_local_index: i32) {
        match page_local_index {
            -1 => self.mouse_page(-1),
            -2 => self.mouse_page(1),
            v if v >= 0 => self.mouse_select(v as usize),
            _ => {}
        }
    }

    /// host 候选框的鼠标滚轮。
    ///
    /// 语义 = **上下方向键调整高亮项**（`move_up`/`move_down`），到页边界自然翻到相邻页，
    /// 不是整页翻动。这是 Windows 上既有的行为，两平台共用本实现。
    ///
    /// 此前是 trait 上的空实现（"统一接入点便于后续按配置实现"），于是 Windows 的
    /// host-render DLL 一直在发这个帧、服务端收下什么也不做——滚轮在**两个平台**都无效。
    /// 不加配置项：滚动候选框就是要动高亮，没有第二种合理解释。
    ///
    /// `delta` 是 `WHEEL_DELTA`(120) 的倍数、正=上滚（Win32 约定，macOS 侧按同一约定折算）。
    /// 一次事件可能跨多格（高速滚轮/触控板惯性），故按格数循环。上限 `MAX_NOTCHES` 防
    /// 惯性滚动一次跳过几十项——那既不是用户意图，也会让候选窗疯狂重绘。
    fn handle_candidate_scroll(&self, delta: i32) {
        const WHEEL_DELTA: i32 = 120;
        const MAX_NOTCHES: i32 = 5;
        if delta == 0 {
            return;
        }
        // 不足一格也算一格：触控板的单次轻扫 delta 可能小于 120，直接整除会得 0（滚不动）。
        let notches = (delta.abs() / WHEEL_DELTA).clamp(1, MAX_NOTCHES);
        let up = delta > 0;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.candidates.is_empty() {
            return;
        }
        let mut changed = false;
        for _ in 0..notches {
            let moved = if up {
                self.move_up(&mut state)
            } else {
                self.move_down(&mut state)
            };
            if !moved {
                break; // 已到首/末项，继续滚也没有更多可动
            }
            changed = true;
        }
        if changed {
            self.notify_ui_update(&state);
        }
    }

    /// 鼠标 hover 候选/翻页器：复用进程内路径的 `mouse_hover`（置 hover_index + 重绘高亮帧）。
    /// 两端线约定不同（按编译平台分支，事件源平台互斥）：
    /// - macOS `.app`：候选 ≥0；翻页器 -1(上页)/-2(下页)；无悬停 i32::MIN 哨兵。
    /// - Windows host DLL（HostWindow.cpp `_OnMouseMove`）：候选 ≥0；无悬停 -1；
    ///   翻页器 -2(上页)/-3(下页)——rect 表的 -1/-2 因 hover 需要独立的「无」被平移一位。
    fn handle_candidate_hover(&self, page_local_index: i32) {
        #[cfg(windows)]
        let target = match page_local_index {
            -2 => wind_ui_types::HOVER_PAGE_PREV,
            -3 => wind_ui_types::HOVER_PAGE_NEXT,
            v if v >= 0 => v,
            _ => -1,
        };
        #[cfg(not(windows))]
        let target = match page_local_index {
            -1 => wind_ui_types::HOVER_PAGE_PREV,
            -2 => wind_ui_types::HOVER_PAGE_NEXT,
            v if v >= 0 => v,
            _ => -1,
        };
        self.mouse_hover(target);
    }

    /// 扩展信封（`CMD_EXT`）：低频消息统一入口。**未知 kind 安静忽略**——旧服务收到新
    /// `.app` 发的新 kind 只当没看见，而不是解析失败把连接搞坏（见 `ext_kind` 的演进约定）。
    fn handle_ext(&self, kind: &str, body: &[u8]) {
        use wind_ipc::protocol::ext_kind;
        match kind {
            // 拖动落点回报。落不落盘由 save_* 按当前定位方式自行判定：固定位置=重新摆放，
            // 跟随光标=只是临时挪开，不写配置。
            ext_kind::POS_CANDIDATE | ext_kind::POS_STATUS_TIP => {
                let Some((x, y)) = decode_ext_point(body) else {
                    tracing::warn!("扩展消息 {kind} 的 body 不是 {{x,y}}，忽略");
                    return;
                };
                if kind == ext_kind::POS_CANDIDATE {
                    self.save_candidate_pos(x, y);
                } else {
                    self.save_status_tip_pos(x, y);
                }
            }
            // 原生浮窗截图的结果（`.app` 动手，服务端只管文案）。
            ext_kind::SHOT_RESULT => match serde_json::from_slice(body) {
                Ok(v) => {
                    let (msg, kind) = shot_result_message(&v);
                    self.show_toast(&msg, ToastPosition::BottomRight, kind);
                }
                Err(e) => tracing::warn!("shot.result 载荷无法解析：{e}"),
            },
            _ => tracing::debug!("未处理的扩展消息 kind={kind}"),
        }
    }

    /// macOS `.app` 候选右键动作：动作串 → 词条操作/复制，作用于页内下标候选。
    fn handle_candidate_context_menu(&self, page_local_index: i32, action: &str) {
        use wind_ui_types::{CandidateOp, UiCommand};
        if page_local_index < 0 {
            return;
        }
        let page_local = page_local_index as usize;
        let op = match action {
            "move_top" => CandidateOp::MoveTop,
            "move_up" => CandidateOp::MoveUp,
            "move_down" => CandidateOp::MoveDown,
            "delete" => CandidateOp::Delete,
            "reset_default" => CandidateOp::Reset,
            "copy" => {
                // 解析页内下标对应候选文本，交 UI 侧写剪贴板（macOS 走 popup_menu::set_clipboard_text）。
                let text = {
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    let (start, end) = self.page_range(&state);
                    let idx = start + page_local;
                    if idx < end && idx < state.candidates.len() {
                        state.candidates[idx].text.clone()
                    } else {
                        String::new()
                    }
                };
                if !text.is_empty() {
                    let _ = self.ui_tx.send(UiCommand::CopyToClipboard(text));
                }
                return;
            }
            other => {
                tracing::debug!("handle_candidate_context_menu: 未知动作 {}", other);
                return;
            }
        };
        self.candidate_op(op, page_local);
    }

    fn handle_show_context_menu(&self, x: i32, y: i32) {
        // 弹出菜单窗口 (popup_menu.rs / ShowCandidateMenu·MenuKey·HideMenu UiCommand) 是
        // Windows 专有；macOS 由 IMK 原生 NSMenu 渲染菜单 (InputController.menu())。
        // macOS 上 IMK 频繁调 menu() → Swift 发 CMD_SHOW_CONTEXT_MENU 仅为「查询菜单项」，
        // 若在此调 show_main_menu 会把协调器置 menu_open=true 并经 forward_menu_key 吞掉后续
        // 所有按键，而 macOS 无弹窗、永不回 MenuClose → 输入被永久卡死 (打字无响应)。
        #[cfg(not(target_os = "macos"))]
        self.show_main_menu(wind_ui_types::MenuAnchor::at_point(x, y));
        #[cfg(target_os = "macos")]
        let _ = (x, y);
    }

    fn handle_english_stats(&self, chars: u32, digits: u32, puncts: u32, spaces: u32) {
        // TSF 侧英文模式统计（对齐 Go RecordTSFEnglish）。
        // chars→english, digits+spaces→other（对齐 classify_chars_full 行为）, puncts→punct。
        let collector = match self.stat_collector.as_ref() {
            Some(c) => c,
            None => return,
        };
        let cfg = &self.rt().config.stats;
        if !cfg.enabled || !cfg.track_english {
            return;
        }
        if chars == 0 && digits == 0 && puncts == 0 && spaces == 0 {
            return;
        }
        collector.record(StatEvent {
            timestamp: chrono::Local::now(),
            chinese: 0,
            english: chars,
            punct: puncts,
            other: digits.saturating_add(spaces),
            code_len: 0,
            candidate_pos: -1,
            schema_id: self.active_schema_id(),
            source: CommitSource::TsfDirect,
        });
    }

    fn preedit_uses_placeholder(&self) -> bool {
        // 非 app_inline（候选窗自显 preedit）→ 应用侧用占位空格，不重复显示编码。
        self.preedit_display
            .lock()
            .map(|m| !m.in_app())
            .unwrap_or(false)
    }

    /// bridge 真正入口：在按键处理之上统一埋点输入统计（上屏文本字符数），
    /// 再做 preedit 占位后处理。集中在此避免修改 40+ 个 commit 返回点（对齐旧 Go
    /// HandleKeyEvent 末尾的 recordCommitFallback 思路）。
    fn handle_key_event_policed(&self, data: &KeyEventData) -> KeyAction {
        let action = self.handle_key_event(data);
        self.record_input_stats(&action);
        // 自提交打点 + 码表自动造词投喂。与 record_input_stats 同一收口理由：上屏路径有
        // 40+ 个返回点，且约 10 处绕过 commit_action 直接构造 InsertText，散点接线必漏。
        self.note_commit_action(&action);
        // PassThrough / UpdateComposition 时 C++ 侧会调 FlushHoldCompositionIfActive 提交旧符号；
        // coordinator 需同步清除 held_text，防止后续标点的 pre_held_text 捡到已提交的旧值
        // 而造成二次提交（"。。="）。仅在 held_text 非空时操作，避免干扰无 Hold 状态的武装态。
        match &action {
            KeyAction::PassThrough
            | KeyAction::NotHandled
            | KeyAction::UpdateComposition { .. } => {
                let mut arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
                if arm.held_text.is_some() {
                    arm.held_text = None;
                    arm.armed = false;
                    arm.hold_pending_commit = false;
                }
            }
            _ => {}
        }
        // 检索范围临时放宽的失效：本次组合结束（缓冲已空）即恢复配置档位。
        // 与 record_input_stats / note_commit_action 同一收口理由——`input_buffer.clear()`
        // 有十几个调用点（上屏/取消/切焦点/模式切换），散点接线必漏。放在按键处理的唯一出口，
        // 天然覆盖全部结束路径。用户选字上屏后下一次输入即回到智能档；而放宽期间继续敲字母、
        // 退格改码、翻页都不会丢状态（缓冲非空），符合「找生僻字常要改几次编码」的实际。
        self.expire_scope_override();
        // 配对状态保活：须在 handle_key_event **之后**刷新，否则本次按键的陈旧判定
        // 会先被自己刷新掉，TTL 永不触发。栈空时是空操作。
        self.touch_pair_state();
        if self.preedit_uses_placeholder() {
            action.with_composition_placeholder()
        } else {
            action
        }
    }

    fn handle_key_event(&self, data: &KeyEventData) -> KeyAction {
        // 每次按键开始重置统计标志：具体上屏路径调 record_commit 置位，
        // 顶层 record_input_stats 仅在未置位时兜底（对齐 Go handle_key_event 开头 reset）。
        self.stat_recorded
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // ── 小键盘归一化（numpad_behavior = follow_main）──
        // 「同主键盘区数字」的语义 = 小键盘键就是主键盘键，故在此改写键码后交由既有主键盘
        // 逻辑接管，一处生效于所有模式。置于最前（仅晚于统计复位）：模式分派、热键、英文
        // 直通等所有后续判断都应看到归一化后的键。direct 时不改写，各模式走自己的 numpad 臂。
        let normalized;
        let data = match numpad_to_main(data.key_code) {
            Some((vk, need_shift)) if self.rt().config.input.numpad_behavior == "follow_main" => {
                normalized = KeyEventData {
                    key_code: vk,
                    modifiers: if need_shift {
                        data.modifiers | MOD_SHIFT
                    } else {
                        data.modifiers
                    },
                    ..data.clone()
                };
                &normalized
            }
            _ => data,
        };
        debug!(
            "handle_key_event: type={} code=0x{:02X} mods=0x{:04X}",
            data.event_type, data.key_code, data.modifiers
        );
        // 记录按键时刻：fast 档据此判断「连续快速输入」（见 handle_caret_probe）。
        // 记录打字节奏：算出**相邻两次按键**的间隔，供 fast 档判断连续输入（见 handle_caret_probe）。
        {
            let now = std::time::Instant::now();
            let prev = self
                .last_key_at
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace(now);
            if let Some(p) = prev {
                *self
                    .last_key_interval_ms
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(now.duration_since(p).as_millis() as u64);
            }
        }

        // 用每键携带的 toggles 快照（C++ 前台线程 GetKeyState 实时采集）校准 CapsLock 镜像。
        // 专门的 VK_CAPITAL key_up 状态通知在英文模式（TSF 不吃该键）或用户于其它应用/
        // 输入法期间切换大写时不会到达，镜像会陈旧——表现为 cancel_on_mode_switch 在
        // "英文+大写"场景读到 caps_lock=false 而跳过取消。服务进程自身 GetKeyState 的
        // toggle 位跨线程不可靠，故以事件快照为权威。
        {
            let caps_now = (data.toggles & 0x01) != 0;
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if s.caps_lock != caps_now {
                debug!(
                    "CapsLock mirror recalibrated from key toggles: {}",
                    caps_now
                );
                s.caps_lock = caps_now;
            }
        }

        // ── key_up：toggle 模式键（Shift/Ctrl/CapsLock）直接切换 ──
        // 关键：TSF 对 toggle 键会"吃掉 keydown 不转发"，仅在 C++ 侧判定为干净单击后
        // 于 keyUp 转发该键事件（_SendKeyToService(..., KEY_EVENT_UP)）。因此服务端
        // 收到 toggle 键的 keyUp 即应直接切换，无需 keydown/pending（对齐 Go HandleKeyEvent）。
        if data.event_type == EVENT_KEY_UP {
            // 修饰键作二三候选键（select_key_groups 含 lrshift / lrctrl）：**先于**下面一切。
            // 同一个键可能多个身份都配了（设置页会提示冲突，但配置文件里拦不住），既有裁决是
            // 「有候选选词、无候选切换」——输入到一半按 Ctrl 想选词的意图远比切中英文常见，而
            // 空闲时按 Ctrl 除了切换也没别的可做。无候选/越界时返回 None 落到下面各分支。
            //
            // ⚠ 2026-08-10 从 CapsLock 分支**之后**上移到这里。CapsLock 永远不在
            // `select_key_vks` 的值域里（那边只有 semicolon/quote/comma/period/lrshift/lrctrl），
            // 故这次上移对 CapsLock 是无副作用的空转；上移的目的是让下面新增的会话态绑定
            // 也排在选词之后，保住「选词优先」这条既有裁决。
            if let Some(act) = self.handle_select_key_up(data) {
                return act;
            }
            // 会话态绑定里的 keyup-only 键（`capslock = "page_prev"` 那类）。
            //
            // ★ **必须先于**下面 CapsLock 的状态同步分支：那条会调 `take_input_on_mode_switch`
            // 把正在打的编码上屏或丢弃。配了 CapsLock 翻页的用户每翻一页就毁一次输入，
            // 现象是「翻页时编码莫名没了」——极难联想到是大小写同步干的。
            //
            // 无候选时本函数返回 None，键照常落到下面的原有处理（CapsLock 仍切大小写、
            // 修饰键仍切中英文）。「有会话归绑定、无会话归原语义」正是两张表的分野。
            if let Some(act) = self.handle_session_action_key_up(data) {
                return act;
            }
            // CapsLock 单独处理：C++ 侧总是发送此 key_up（不经 key_up_tsf_hashes 过滤），
            // 故须先于 is_toggle_mode_keycode 检查。同步真实大写锁定状态，不翻转 chinese_mode
            // （对齐 Go handleCapsLockStateNoLock：capsLockOn 跟随 data.toggles & 0x01）。
            if data.key_code == 0x14
            /* VK_CAPITAL */
            {
                let caps_lock_on = (data.toggles & 0x01) != 0;
                debug!("CapsLock state notification: on={}", caps_lock_on);
                let had_pending = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    !s.input_buffer.is_empty()
                        || !s.committed_text.is_empty()
                        || !s.candidates.is_empty()
                };
                let commit_text = {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    // 切大写时按"切英文"语义处理待输入（commit_on_switch）；切回小写时直接丢弃。
                    let text = self.take_input_on_mode_switch(&mut s, !caps_lock_on);
                    s.caps_lock = caps_lock_on;
                    text
                };
                self.punct.lock().unwrap_or_else(|e| e.into_inner()).reset();
                self.push_state_update();
                self.show_status();
                self.notify_toolbar();
                self.notify_ui_hide();
                if !commit_text.is_empty() || had_pending {
                    let chinese_mode = self
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .chinese_mode;
                    return KeyAction::InsertText {
                        text: commit_text,
                        new_composition: None,
                        mode_changed: false,
                        chinese_mode,
                        has_new_composition: false,
                    };
                }
                return KeyAction::StatusUpdate(self.build_status());
            }
            // 方案级 `[key_actions]` 绑在修饰键上的功能（`rshift = "toggle_schema:english"`）。
            // **先于** is_toggle_mode_keycode：同一个键两处都配时，方案级是更具体的声明，
            // 与 keydown 侧「方案表命中即跳过全局链」同一裁决方向。
            //
            // 只处理纯修饰键：有字符的键归 keydown 的 try_activate_mode 管（英文模式下
            // 必须让它出字），两条路各管一半、不重叠。判据是键的形态而非动词类别，
            // 见 docs/design/schema-key-actions.md §4.1。
            if keymap::is_pure_modifier_vk(data.key_code)
                && let Some(act) = self.handle_bound_modifier_key_up(data.key_code)
            {
                return act;
            }
            if self.is_toggle_mode_keycode(data.key_code) {
                debug!("toggle_mode key_up: code=0x{:02X}", data.key_code);
                // 切换前是否有未上屏的编码/候选（决定是否需要结束应用 composition）。
                let had_pending = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    !s.input_buffer.is_empty()
                        || !s.committed_text.is_empty()
                        || !s.candidates.is_empty()
                };
                let (status, commit_text) = self.handle_toggle_mode();
                let chinese_after = status.as_ref().map(|s| s.chinese_mode).unwrap_or(false);
                // 切英文（中→英）有待输入：commit_on_switch=true 上屏原始编码，否则空 commit。
                // 两种都返回 InsertText：空文本 + 有 composition 时 C++ CommitText 仍会
                // EndComposition，清掉应用里残留的编码（StatusUpdate 分支不结束 composition，
                // 是“切英文后编码不清空”的根因）；mode_changed 同时更新中英图标。
                if !commit_text.is_empty() || had_pending {
                    return KeyAction::InsertText {
                        text: commit_text,
                        new_composition: None,
                        mode_changed: true,
                        chinese_mode: chinese_after,
                        has_new_composition: false,
                    };
                }
                if let Some(status) = status {
                    return KeyAction::StatusUpdate(status);
                }
            }
            return KeyAction::PassThrough;
        }
        if data.event_type != EVENT_KEY_DOWN {
            return KeyAction::PassThrough;
        }

        // ── 右键菜单打开时：方向键/回车/ESC 由菜单消费（优先于一切）──
        // 仅非 macOS：弹出菜单窗口是 Windows 专有，macOS 用 IMK 原生菜单自行消费键，
        // 协调器不应吞键 (否则 menu_open 一旦被置真会永久卡死输入，见 handle_show_context_menu)。
        #[cfg(not(target_os = "macos"))]
        if self.is_menu_open() && self.forward_menu_key(data.key_code) {
            return KeyAction::Consumed;
        }

        // ── key_down 热键匹配 ──
        // 规范化修饰位：TSF 转发的 modifiers 可能含 L/R 具体位，而 key_down 热键以
        // 通用位（ctrl/shift/alt/win）注册，故先掩掉具体位再比对 match_hash。
        let norm_mods = data.modifiers & hotkey::MOD_GENERIC_MASK;
        let norm_hash = calc_key_hash(norm_mods, data.key_code);
        if let Some(action) = self.rt().compiled_hotkeys.match_key_down(norm_hash)
            && !action.is_empty()
        {
            debug!(
                "Hotkey matched (key_down): {} (0x{:08X})",
                action, norm_hash
            );
            let action = action.to_string();
            // 加词热键需返回占位 composition（激活 C++ 转发全部按键），不符 dispatch_hotkey
            // 的「bool→StatusUpdate」契约，故在此特判直接返回 KeyAction。仅中文模式响应。
            if action == "add_word" {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.chinese_mode {
                    return self.enter_add_word_mode(&mut state);
                }
            } else if action == "open_add_word_dialog" {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.chinese_mode {
                    return self.open_add_word_from_history(&mut state);
                }
            } else if action == "enter_temp_pinyin" {
                // 临拼直达热键：进入前先上屏半成品（commit_and_enter_temp_pinyin 内含），
                // 传 key_code=0 → 组合区无引导符。已在临拼态则幂等；中文模式下一律吞键
                // （不放行，避免把该组合键泄漏给宿主）。
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.chinese_mode {
                    if state.active != Some(ModeKind::TempPinyin)
                        && let Some(target) = self.engine_mgr.temp_pinyin_target()
                    {
                        return self.commit_and_enter_temp_pinyin(&mut state, 0, target);
                    }
                    return KeyAction::Consumed;
                }
            } else if let Some(id) = action.strip_prefix("enter_special:") {
                // 特殊模式直达热键：按 id 定位配置序 idx（与 match_special_trigger 下标语义一致）。
                // 已在该模式则幂等；未知 id / 方案不可加载均安全吞键（不放行以免误触）。
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.chinese_mode {
                    if let Some(idx) = self.special_mode_idx(id)
                        && state.active != Some(ModeKind::Special(idx))
                        && let Some(schema) = self.special_schema(idx)
                        && self.engine_mgr.ensure_schema(&schema)
                    {
                        // key_code=0 哨兵：热键进入不写引导符。
                        return self.commit_and_enter_special_mode(&mut state, idx, 0);
                    }
                    return KeyAction::Consumed;
                }
            } else if let Some(id) = action.strip_prefix("toggle_schema:") {
                // 方案往返热键（keys.key_actions）：切过去，再按一次回来源。
                // 与 switch_schema 同样**不判 chinese_mode**——回程尤其要在英文态按得动。
                //
                // trigger_vk 传 0：全局热键在所有方案里都生效，不需要「回程键临时授权」
                // 那套（那是方案级绑定专有的问题，见 `schema_return_key_action`）。
                self.toggle_schema_by_id(id, 0);
                return KeyAction::StatusUpdate(self.build_status());
            } else if let Some(id) = action.strip_prefix("switch_schema:") {
                // 方案直达热键：切 active 方案。**不判 chinese_mode**——与循环键
                // (`switch_engine`) 同策略。切方案在英文态下同样该生效，否则切到英文方案后
                // 这条路径就失效了，用户回不到中文方案。
                self.switch_schema_by_id(id);
                return KeyAction::StatusUpdate(self.build_status());
            } else if self.dispatch_hotkey(&action) {
                return KeyAction::StatusUpdate(self.build_status());
            }
        }

        // ── 候选词操作热键（Ctrl+数字 置顶/删除）──
        // 这两组在编译期仅注册转发（action 为空，上方匹配不触发），实际语义在此分派。
        // 须先于下方 Ctrl/Alt 组合「清空隐藏候选」分支，否则 Ctrl+数字 会被当作普通组合吞掉。
        if let Some(act) = self.handle_candidate_action_hotkey(data) {
            return act;
        }

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        // 快捷加词模式：消费全部按键（↑↓调词长/Enter确认/Esc退出），先于英文透传与单点分派。
        if state.add_word_active {
            return self.handle_add_word_key(&mut state, data);
        }

        // 密码框强制英文抑制：透传（不改 chinese_mode 持久值）。图标另有呈现（显 "英"），
        // 走 ToolbarState/语言栏的独立字段，与本判据无耦合——详见 password_suppress 字段注释。
        // 须先于下方全角分支——密码框里不该出全角字符，一律半角透传。
        // 注：透传要真生效，C++ 侧必须也没吃这个键，否则「吃了再吐」丢键（见 TSF 待办）。
        if self
            .password_suppress
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return KeyAction::PassThrough;
        }

        // 配对跳出键：**全模式统一前置判定**，必须早于下面的英文模式分支——英文模式对普通键
        // 直接 PassThrough，判定放在中文路径里就永远跑不到（旧实现即如此，是「英文模式跳不出
        // 中文里打的配对」的根因之一）。守卫与失效方向见 try_jump_out。
        if let Some(act) = self.try_jump_out(&state, data) {
            return act;
        }

        // 英文模式
        if !state.chinese_mode {
            // 全角：键已被 TSF 的 `english_fullwidth` 分支吃下等 Rust 出字，此处必须转换，
            // 否则 PassThrough 会形成「吃了再吐」→ 严格 TSF 宿主丢键（见 handle_english_full_width）。
            // Ctrl/Alt 组合不参与：C++ 的 ClassifyInputKey 对其返回 None，本就不吃。
            if state.full_width
                && data.modifiers & MOD_SHORTCUT == 0
                && let Some(act) = self.handle_english_full_width(&mut state, data)
            {
                return act;
            }
            // 半角英文 + 该标点键配了「英半」列：DLL 已按 core 推送的字符集合吃下此键
            // （`english_custom_punct` 分支），此处必须出字，否则同样「吃了再吐」丢键。
            // 未配的键 handle 返回 None → 落到下方透传，行为与历史完全一致。
            if data.modifiers & MOD_SHORTCUT == 0
                && let Some(act) = self.handle_english_custom_punct(&mut state, data)
            {
                return act;
            }
            // 半角英文：透传，宿主自然出字（保留 WM_KEYDOWN 原生语义）。
            return KeyAction::PassThrough;
        }

        // CapsLock 开：大写语义，不进中文输入流。
        // 全角开：将按键转为正确大小写的英文字符再做全角转换后上屏。
        // 全角关：TSF 层在无 session 时已透传；有 session（切换前残留）时由此兜底 PassThrough。
        // Ctrl/Alt 组合不拦截（让下方热键/清空逻辑处理）。
        if state.caps_lock && data.modifiers & MOD_SHORTCUT == 0 {
            if state.full_width {
                let shift = data.modifiers & MOD_SHIFT != 0;
                let is_letter = (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code);
                // CapsLock 对字母大小写取反：CapsLock ON + no Shift → 大写；Shift → 小写。
                // printable_char 以 shift=true 产生大写，故字母键时翻转 shift。
                let effective_shift = if is_letter { !shift } else { shift };
                // 用 full_width_source_char 而非 printable_char：C++ 在中文全角下也吃
                // 空格(chinese_fullwidth_space)与小键盘(chinese_fullwidth_number)，
                // 而这两者都不在 printable_char 覆盖内 → 曾落下方 PassThrough → 丢键。
                if let Some(ch) = full_width_source_char(data.key_code, effective_shift) {
                    // 经完整标点转换流水线（自定义映射"英全"列 → 全半角），
                    // 而非直接 to_full_width，确保用户自定义映射生效。
                    // 临时置 chinese_punct=false 对应"英全"状态（不走中文标点转换）。
                    let saved_punct = state.chinese_punct;
                    state.chinese_punct = false;
                    let text = self.convert_punct_char(&state, ch);
                    state.chinese_punct = saved_punct;
                    return Self::commit_action(text, true);
                }
            }
            return KeyAction::PassThrough;
        }

        // 统一夺取回退：夺取式模式（URL/后续 z 临拼）中，退到夺取边界再按退格 →
        // 撤销夺取、把快照回放回正常码表输入流（而非停在无候选的独占模式）。
        // 须先于下方单点分派，否则退格会被模式处理器按普通删字符消费。
        if data.key_code == keymap::VK_BACK && self.can_rewind(&state) {
            return self.rewind_hijack(&mut state);
        }

        // 已激活独占模式：单点分派到专用处理器（唯一入口，见 pipeline.rs）。
        match state.active {
            Some(ModeKind::TempPinyin) => return self.handle_temp_pinyin_key(&mut state, data),
            Some(ModeKind::TempEnglish) => return self.handle_temp_english_key(&mut state, data),
            Some(ModeKind::Url) => return self.handle_url_key(&mut state, data),
            Some(ModeKind::Special(_)) => return self.handle_special_key(&mut state, data),
            Some(ModeKind::Mix(_)) => return self.handle_mix_key(&mut state, data),
            Some(ModeKind::AuxCode) => return self.handle_aux_code_key(&mut state, data),
            None => {}
        }

        // 方案级表的 A 类状态切换（`backslash = "toggle_punct"` 这类）。
        //
        // 与紧随其后的 `try_activate_mode` 分属两半：B 类建 overlay、要 `&mut State`，
        // 故在锁内；A/C 类只改全局状态，目标函数（dispatch_hotkey / toggle_schema_by_id）
        // 各自加锁，**必须锁外执行**——判定在这里做完，guard 就地 drop 掉。
        //
        // 位置在英文模式分水岭之后，与 B 类同：有字符的键在英文态必须能出字。代价是
        // `toggle_mode` 那类「用来离开英文态」的动作在此不可达，故它们限修饰键（keyup 路径），
        // 见 `BoundAction::requires_modifier_key`。
        if let Some(action) = self.bound_lock_free_action_for_keydown(&state, data) {
            drop(state);
            if let Some(act) = self.run_lock_free_bound_action(&action, data.key_code) {
                return act;
            }
            // 门卫没过：不吞键，重新取锁走原有链路（与各模式门卫同策略）。
            state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        }

        // 空缓冲模式激活：单一入口，优先级链见 try_activate_mode（对齐 key-pipeline.md §2.1）。
        if let Some(act) = self.try_activate_mode(&mut state, data) {
            return act;
        }

        // Ctrl/Alt/Cmd 组合（非热键）：有输入则清空并隐藏候选窗，否则透传。
        // 必须 notify_ui_hide：否则候选窗残留（如 Ctrl+A 时卡死，需再输入才复位）。
        //
        // ⚠ 这里返回 `ClearComposition` 的语义是「清掉组合」，**不是**「这个键归我了」。
        // 宿主必须照旧执行它的快捷键——TSF 靠 `OnTestKeyDown` 压根不转发这类键来保证，
        // macOS 无那层前置闸门，故由 `BridgeResponseRouter` 对快捷键组合把这一帧判为
        // 「不消费」（见 Swift 侧 `hostShortcut` 参数）。改动本分支的返回值前先读那里。
        if data.modifiers & MOD_SHORTCUT != 0 {
            if !state.input_buffer.is_empty() || !state.committed_text.is_empty() {
                self.reset_pinyin_composition(&mut state);
                self.notify_ui_hide();
                return KeyAction::ClearComposition;
            }
            return KeyAction::PassThrough;
        }

        // ── 网址模式激活（夺取式）──
        // 普通输入累积时，若 input_buffer + 当前键字符 恰好等于某前缀（如 "www."/"http"），
        // 则夺取进入网址模式。置于主分派前，确保「补全前缀的那一键」（字母或 '.'）先被截获，
        // 不落入普通码表/标点处理。前缀按惯例小写，故探针用小写字母对齐 input_buffer。
        if self.rt().config.input.url.enabled {
            let shift = data.modifiers & MOD_SHIFT != 0;
            if let Some(ch) = printable_char(data.key_code, shift) {
                let probe = format!("{}{}", state.input_buffer, ch.to_ascii_lowercase());
                if self.is_url_prefix(&probe) {
                    return self.enter_url_mode(&mut state, probe);
                }
            }
        }

        debug!(
            "key_event: code=0x{:02X} mods=0x{:04X} chinese={} full={} caps={} buf='{}'",
            data.key_code,
            data.modifiers,
            state.chinese_mode,
            state.full_width,
            state.caps_lock,
            state.input_buffer
        );

        // 非字母码元闸门：本方案把某个数字/符号配成了码元（如 `a-z0-9` 要打 `Win10`、
        // `a-x/` 要打含 `/` 的词条）→ 进缓冲，抢在以词定字/翻页/数字选词/标点流水线之前。
        //
        // 位置即契约（见 docs/design/codetable-input-chars.md「组码中码元优先，空缓冲让位」）：
        // 置于模式激活与 URL 夺取**之后**，故空缓冲下的引导键、临拼/临英触发键、URL 前缀
        // 一概不受影响；置于下方各闸门**之前**，故组码中这些键归码表而非选词/翻页。
        //
        // 空缓冲时闸门查的是**首码集**：数字默认不在其中 ⇒ 不接管 ⇒ 数字键照常选词/透传，
        // 用户不会失去「选第 1 个候选」和原生数字输入。
        //
        // ⚠️ 默认码元集 a-z 不含任何非字母字符 ⇒ 恒不命中，与历史逐键等价（零回归）。
        if let Some(act) = self.try_code_char_gate(&mut state, data) {
            return act;
        }

        // 以词定字（select_char）：配置的成对标点键从当前高亮候选词逐字上屏（对齐 Go
        // handleEngineDefault——select_char 优先于翻页键，故置于 apply_session_action 之前）。默认
        // `select_char_keys` 为空 → select_char_index 恒 None → 跳过（零回归）。仅在缓冲非空或
        // 有候选时拦截；空缓冲且无候选时放行，让 `,`/`.` 作普通标点（对齐 Go 空缓冲回退标点）。
        if data.modifiers & MOD_SHIFT == 0
            && (!state.input_buffer.is_empty() || !state.candidates.is_empty())
            && let Some(char_index) = self.select_char_index(data.key_code)
        {
            return self.handle_select_char_with_overflow(
                &mut state,
                char_index,
                data.key_code,
                data.prev_char,
            );
        }

        // 候选翻页/高亮：配置驱动统一处理（普通模式为码表型，`-`/`=` 可作翻页）。
        // 仅有候选时生效；无候选时下方 match 的回退臂负责透传方向/翻页键。
        if let Some(act) = self.apply_session_action(&mut state, data, true) {
            // 共键（`page_next_aux_code`）的「翻页 + 进辅助码」已在 `apply_session_action`
            // 的 `PageNextAuxCode` 臂一并处理；此处无需再判定。
            return act;
        }

        // 数字小键盘 —— direct（默认）：IME 不把该键解释为选词，但**已打的码不丢**：先顶屏当前
        // 高亮候选（含逐步转换的已转换前缀），再接着输出该小键盘字符。
        // follow_main 时键已在 handle_key_event 入口归一化为主键盘等价键，永不到达此处。
        if let Some(npc) = numpad_char(data.key_code) {
            // 命令候选顶屏 → 执行命令（与按空格一致），不上屏 display 标签、不追加该字符。
            if let Some(act) = self.top_commit_command_guard(&mut state) {
                return act;
            }
            let has_comp = !state.input_buffer.is_empty()
                || !state.committed_text.is_empty()
                || !state.candidates.is_empty();
            return self.commit_highlight_then_char(&mut state, npc, has_comp);
        }

        // ── z-fallback 夺取：**必须早于下面的按键分派** ──
        //
        // 缓冲以 z 开头、加上这一键后 `z…` 破活码前缀 ⇒ 首 z 实为引导键，抛弃它、
        // 残余码切进目标模式（见 `try_z_fallback`，内含全部门禁：码表引擎 / z 有绑定 /
        // 目标接得住这个字符 / 破前缀）。
        //
        // ★ 放在 match **之前**而不是各臂里：数字键在缓冲非空时是选词键、符号走标点
        // 流水线，两条都会当场把键消费掉——夺取判定挂在臂里就永远轮不到。原先只挂在
        // 字母臂上，于是 `z = "mix:quick_mix"` 的用户「进了快捷输入却算不了数」，而同一个
        // mix 用 `;` 进就正常（`;` 首键直接进模式，之后所有键都归 mix 处理）。
        //
        // 单点而非三处各接一次：这仓已多次栽在「N 条通路只接了 N-1 条」上
        // （见 project_mixed_overflow_vs_topcode）。
        if data.modifiers & MOD_SHORTCUT == 0 {
            let probe = if (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code) {
                Some((b'a' + (data.key_code - keymap::VK_A) as u8) as char)
            } else if (keymap::VK_0..=keymap::VK_9).contains(&data.key_code) {
                Some((b'0' + (data.key_code - keymap::VK_0) as u8) as char)
            } else {
                punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
            };
            if let Some(ch) = probe
                && let Some(act) = self.try_z_fallback(&mut state, ch)
            {
                return act;
            }
        }

        match data.key_code {
            // Escape：取消整个组合（含已转换前缀），不上屏。实现收口在 `cancel_session`
            // ——`keys.session_actions` 里绑 `cancel` 的键走的是同一个函数，两条通路
            // 行为必然一致。
            keymap::VK_ESCAPE => self.cancel_session(&mut state),
            keymap::VK_BACK => {
                // 联想态：收掉候选并结束占位组合。**必须先于下面的既有分支**——那些分支
                // 在「缓冲空 + 无已转换段」时给 `PassThrough`，而联想态挂着占位组合
                // （见 `handle_assoc::ASSOC_COMPOSITION`），裸透传会把组合悬在宿主里。
                //
                // 这一键是吃掉还是连同收窗一起交还宿主，由 `backspace_cancels_only` 定
                // （默认吃掉，与回车相反的理由见 `assoc_backspace`）。
                //
                // 联想只需单独接这两个键：其余（翻页/上下移高亮/二三候选/数字选词/
                // 空格选高亮/Esc 取消/鼠标点选）的既有分支门槛都只是「候选非空」，
                // 联想候选就住在 `candidates` 里，天然全部适用。
                if state.assoc_active() {
                    return self.assoc_backspace(&mut state);
                }
                // Backspace：分步撤销——有已转换段则先把最后一段退回拼音（你→ni，码并回剩余
                // 缓冲前部、重转），否则删光标前一个字符。
                // 段回退**优先于光标**（不看光标位置，对齐 Go handleBackspace 的分支顺序）。
                if !state.committed_segs.is_empty() {
                    self.pop_committed_seg(&mut state)
                } else if !state.input_buffer.is_empty() {
                    let st = &mut *state;
                    let deleted = preedit_cursor::BufEdit::new_cased(
                        &mut st.input_buffer,
                        &mut st.input_cursor_pos,
                        &mut st.input_buffer_cased,
                    )
                    .backspace();
                    if !deleted {
                        // 缓冲非空但光标已在最左：吃掉不透传，否则宿主会删到组合区之前的正文。
                        KeyAction::Consumed
                    } else {
                        self.update_candidates(&mut state);
                        if state.input_buffer.is_empty() {
                            self.notify_ui_hide();
                            KeyAction::ClearComposition
                        } else {
                            let display = state.preedit.clone();
                            let caret_pos = self.composition_caret(&state);
                            self.notify_ui_update(&state);
                            KeyAction::UpdateComposition {
                                caret_pos,
                                text: display,
                            }
                        }
                    }
                } else {
                    KeyAction::PassThrough
                }
            }
            keymap::VK_SPACE => {
                // 联想态 + `space_commits = false`：空格不选联想，收窗后照常出空格。
                //
                // 联想态下「高亮」是输入法猜的，不是用户选的——有人希望空格顺手选中
                // （主流做法，故默认开），也有人希望空格就是空格。这一项没有更对的答案，
                // 所以是个配置；但**它只在联想态有意义**，正常输入的空格恒是选高亮。
                if state.assoc_active() && !self.assoc_config().space_commits {
                    self.exit_assoc(&mut state, crate::handle_assoc::AssocExit::NonSelectKey);
                    self.notify_ui_hide();
                    let text = self.convert_punct(&state, ' ', data.prev_char);
                    self.record_commit(&text, 0, -1, CommitSource::Punctuation);
                    return Self::commit_action(text, true);
                }
                // Space：选当前高亮候选 / 上屏编码
                if !state.candidates.is_empty() {
                    let (start, _) = self.page_range(&state);
                    let idx = (start + state.selected_index).min(state.candidates.len() - 1);
                    let cand = state.candidates[idx].clone();
                    self.commit_selected(&mut state, &cand, (idx - start) as i32)
                } else if !state.input_buffer.is_empty() || !state.committed_text.is_empty() {
                    // 空码空格：按 space_on_empty_behavior（对齐 Go handleSpace 空码分支）——
                    // "clear" 清空编码；否则上屏「已转换前缀 + 剩余拼音原码」。
                    if self.rt().config.input.space_on_empty_behavior == "clear" {
                        state.committed_text.clear();
                        state.committed_segs.clear();
                        state.input_buffer.clear();
                        state.candidates.clear();
                        self.notify_ui_hide();
                        return KeyAction::ClearComposition;
                    }
                    let prefix = self.take_committed(&mut state);
                    // 上屏的是**用户所打的形态**：Shift+字母的大写存在影子串里，缓冲恒小写。
                    let raw_code = preedit_cursor::cased_or_buffer(
                        &state.input_buffer,
                        &state.input_buffer_cased,
                    )
                    .to_string();
                    // 上屏剩余拼音原码：prefix(committed) 段已在选词时记过，此处只记 input_buffer 避免重复。
                    self.record_commit(
                        &raw_code,
                        raw_code.len() as u32,
                        -1,
                        CommitSource::RawInput,
                    );
                    let mut text = self.maybe_s2t(&state, &format!("{}{}", prefix, raw_code));
                    // 英文补空格（`schema.english.commit_space`）：本分支上屏的是**输入缓冲
                    // 原码**（词库里没有的自造词），无候选可依，故用方案口径
                    // `english_space_enabled` 而非候选口径。与选中候选补空格一致——两者都是
                    // 「一个英文词打完了」，行为分叉才是意外。
                    //
                    // ⚠️ 下方 VK_RETURN 分支代码与本块**逐行同形**，但**刻意不补**：回车是
                    // 终结性动作（多伴随换行/提交意图），语义与「接着打下一个词」相反。改这里
                    // 时别顺手把那边也改了。
                    if self.english_space_enabled() {
                        text.push(' ');
                    }
                    state.input_buffer.clear();
                    state.input_buffer_cased.clear();
                    state.candidates.clear();
                    self.notify_ui_hide();
                    Self::commit_action(text, true)
                } else {
                    // 空缓冲空格：经标点流水线转换（自定义映射「空格」行四态可覆盖；
                    // 内建默认仅全角态转全角空格 U+3000，对齐设置端展示基线与微软拼音）。
                    // 流水线原样返回 " " 时（半角态无自定义）维持透传，保留宿主对
                    // 空格键的原生语义（如网页滚动）。
                    let text = self.convert_punct(&state, ' ', data.prev_char);
                    if text == " " {
                        return KeyAction::PassThrough;
                    }
                    self.record_commit(&text, 0, -1, CommitSource::Punctuation);
                    Self::commit_action(text, true)
                }
            }
            keymap::VK_RETURN => {
                // 联想态：收窗并结束占位组合，**默认连同把回车交还宿主**
                // （`enter_cancels_only`，见 `assoc_enter`）。
                //
                // 下方各分支的门槛都是「缓冲或已转换前缀非空」，联想两者皆空 ⇒ 会落到最后的
                // `PassThrough`，而那只交还键、不收组合，占位组合会悬在宿主里（同退格）。
                //
                // 刻意**不**上屏高亮联想：回车是终结性动作，用户按它是要换行/发送，
                // 不是「就选高亮那条吧」。
                if state.assoc_active() {
                    return self.assoc_enter(&mut state);
                }
                // Enter：按 enter_behavior 配置（对齐 Go handleEnter）——"clear" 清空编码
                // (不上屏)；否则(commit)上屏「已转换前缀 + 剩余原码」。
                if !state.input_buffer.is_empty() || !state.committed_text.is_empty() {
                    if self.enter_clears_composition() {
                        state.committed_text.clear();
                        state.committed_segs.clear();
                        state.input_buffer.clear();
                        state.candidates.clear();
                        self.notify_ui_hide();
                        return KeyAction::ClearComposition;
                    }
                    let prefix = self.take_committed(&mut state);
                    // 上屏的是**用户所打的形态**：Shift+字母的大写存在影子串里，缓冲恒小写。
                    let raw_code = preedit_cursor::cased_or_buffer(
                        &state.input_buffer,
                        &state.input_buffer_cased,
                    )
                    .to_string();
                    // 上屏剩余拼音原码：prefix(committed) 段已在选词时记过，此处只记 input_buffer 避免重复。
                    self.record_commit(
                        &raw_code,
                        raw_code.len() as u32,
                        -1,
                        CommitSource::RawInput,
                    );
                    // ⚠️ 本块与上方 VK_SPACE 空码分支逐行同形，唯一差别是**不补英文空格**
                    // （`schema.english.commit_space`）：回车是终结性动作，多伴随换行/提交
                    // 意图，与空格「接着打下一个词」的语义相反。这是刻意的不对称，不是漏接。
                    let text = self.maybe_s2t(&state, &format!("{}{}", prefix, raw_code));
                    state.input_buffer.clear();
                    state.input_buffer_cased.clear();
                    state.candidates.clear();
                    self.notify_ui_hide();
                    Self::commit_action(text, true)
                } else {
                    KeyAction::PassThrough
                }
            }
            keymap::VK_1..=keymap::VK_9 if data.modifiers & MOD_SHIFT == 0 => {
                // 数字键 1-9 选当前页第 N 个候选；越界按 input.overflow.number_key 处理
                // （ignore 吞键 / commit 上屏高亮 / commit_and_input 顶字+数字，对齐 Go）。
                let num = (data.key_code - 0x31) as usize + 1; // 1..=9
                if state.candidates.is_empty()
                    && state.input_buffer.is_empty()
                    && state.committed_text.is_empty()
                {
                    let digit = (b'0' + num as u8) as char;
                    // 全角：C++ 为此专门在无 session 时也吃数字（`chinese_fullwidth_number`
                    // 分支），故必须出字——透传会「吃了再吐」→ 严格 TSF 宿主丢键、宽松宿主出
                    // 半角（旧行为：1-9 各应用表现不一，而 `0` 因无此臂落标点流水线反而正常）。
                    // 走完整流水线而非裸 to_full_width，与 `0`/小键盘/CapsLock 各路径一致。
                    if state.full_width {
                        let text = self.convert_punct(&state, digit, data.prev_char);
                        self.record_commit(&text, 0, -1, CommitSource::Punctuation);
                        return Self::commit_action(text, true);
                    }
                    // 半角无候选：透传，纯数字键由宿主出字（保留原生按键语义）。
                    // 对齐 Go：recordCommit(key, 0, -1, SourcePunctuation) 后再 return nil。
                    self.record_commit(&digit.to_string(), 0, -1, CommitSource::Punctuation);
                    return KeyAction::PassThrough;
                }
                self.handle_number_key_select(&mut state, num)
            }
            keymap::VK_0
                if data.modifiers & MOD_SHIFT == 0
                    && !(state.candidates.is_empty()
                        && state.input_buffer.is_empty()
                        && state.committed_text.is_empty()) =>
            {
                // 数字键 0 选当前页第 10 个候选（对齐通行约定 0=第10；越界按
                // overflow.number_key 处理）。follow_main 归一化后小键盘 0 走此臂，与主键盘一致。
                // 空缓冲下的 0 不进此臂（guard 排除）→ 落兜底标点流水线，保持全角态输出全角 ０
                // 及自定义标点映射——0 曾靠「不在数字选词臂、落兜底」才正确，见 fullwidth 修复。
                self.handle_number_key_select(&mut state, 10)
            }
            keymap::VK_A..=keymap::VK_Z => {
                // ★ 会话态选词键里的**字母**（当前值域只有 z）：有候选时先选词，再谈组码。
                //
                // 为什么必须拦在这里、而不是像符号键那样交给下方的选词消费点：那一段在
                // `decideBufferedTrigger` 分支里（本 match 的符号/数字臂之后），而字母走
                // 本臂、当场进缓冲，**永远流不到那里**。`apply_session_action` 也接不住——
                // 它对 `SelectCandidate` 刻意返回 `None`（选词带 overflow 语义，执行路径另在别处）。
                //
                // ⚠️ 候选不足时**落回本臂的正常组码**，不套 `keys.overflow.select_key`：
                // 那三档是为符号键设计的（符号本身不是编码，越界了才要决定它怎么办），
                // 而字母键的「输出该键字符」恰恰就是当编码打。套过来的话，`commit` 档会
                // 在候选不足时吞掉字母并上屏高亮候选，用户按 z 想接着打码却上了别的字。
                // 判据与 `handle_select_key_up` 同源、结论相反：那里修饰键**没有**字符可
                // 输出所以吞键，这里字母的字符就是编码所以落回。
                if !state.candidates.is_empty()
                    && let Some(offset) = self.select_key_offset(data.key_code)
                {
                    let (start, end) = self.page_range(&state);
                    let idx = start + offset;
                    if idx < end {
                        let cand = state.candidates[idx].clone();
                        return self.commit_selected(&mut state, &cand, offset as i32);
                    }
                }
                // A-Z 字母累积。缓冲恒存小写：z-fallback 探针、顶码判定、引擎查询、词频记账
                // 全部只看它，大小写对匹配零影响。
                let ch = (b'a' + (data.key_code - 0x41) as u8) as char;
                // Shift+字母的大写只进影子串，供组合区显示与「上屏原码」还原用户所打的形态
                // （打 `aBC` 回车得 `aBC`）。CapsLock 在中文输入流里到不了这一步——上面
                // `state.caps_lock` 分支已整段接管，故此处只需判 Shift。
                let raw = if data.modifiers & MOD_SHIFT != 0 {
                    ch.to_ascii_uppercase()
                } else {
                    ch
                };
                // 注：z-fallback 夺取已上移到 match **之前**统一处理（数字/符号臂同样需要它，
                // 而那两条会当场消费掉按键）。故此处不再调用。
                //
                // 非码元字母（如 `input_chars = "a-x"` 下的 y/z）：不进缓冲，终结组合并出字。
                //
                // ★ **必须在 z-fallback 之后**。z 常同时是「非码元」（a-x 方案）与
                // 「临时拼音触发键」，若先判非码元，z 会被当成普通字符顶上屏，临拼永远
                // 进不去——同理，空缓冲下的模式激活在更上游的 try_activate_mode 已处理完。
                // 上移之后这条顺序仍然成立（夺取在 match 前，更早）。
                //
                // 默认码元集 a-z 下本判定恒不命中，与历史逐键等价（零回归）。
                if !self.can_enter_buffer(&state, ch) {
                    return self.reject_non_code_char(&mut state, raw);
                }
                self.accumulate_code_char(&mut state, ch, raw)
            }
            keymap::VK_LEFT | keymap::VK_RIGHT | keymap::VK_HOME | keymap::VK_END => {
                // 编码区光标移动（对齐 Go handleCursorLeft/Right/Home/End 的三态语义）：
                // ① 无组合 → 透传，宿主照常移动文档光标；② 有剩余编码 → 编码区内移动；
                // ③ 已在边界 / 只剩只读的已转换前缀 → 吃掉不透传（否则宿主光标会跳出组合区）。
                // 左右键若被用户配成翻页/高亮键，上面的 apply_session_action 已先行拦截，走不到这里
                // ——「配了别的功能」即等价于放弃光标移动。
                if state.input_buffer.is_empty() {
                    if state.committed_text.is_empty() {
                        KeyAction::PassThrough
                    } else {
                        KeyAction::Consumed
                    }
                } else {
                    let st = &mut *state;
                    let mut ed = preedit_cursor::BufEdit::new(
                        &mut st.input_buffer,
                        &mut st.input_cursor_pos,
                    );
                    let moved = match data.key_code {
                        keymap::VK_LEFT => ed.move_left(),
                        keymap::VK_RIGHT => ed.move_right(),
                        keymap::VK_HOME => ed.home(),
                        _ => ed.end(),
                    };
                    if moved {
                        // 光标移动**不重算候选**（不调 update_candidates）：光标不参与引擎查询，
                        // 候选与 preedit 文本均不变，只是 caret 位置变了。但仍须 notify_ui_update
                        // ——自绘编码栏要据新 caret 重画插入符（"不重算候选" ≠ "不刷新 UI"）。
                        let display = state.preedit.clone();
                        let caret_pos = self.composition_caret(&state);
                        self.notify_ui_update(&state);
                        KeyAction::UpdateComposition {
                            caret_pos,
                            text: display,
                        }
                    } else {
                        KeyAction::Consumed
                    }
                }
            }
            keymap::VK_DELETE => {
                // 前删（删光标后一个字符，光标不动）。与 Backspace 刻意不对称：Backspace 一上来
                // 就回退已转换段，Delete 只删剩余编码、不碰前缀（对齐 Go handleDelete）。
                if state.input_buffer.is_empty() {
                    if state.committed_text.is_empty() {
                        KeyAction::PassThrough
                    } else {
                        KeyAction::Consumed
                    }
                } else {
                    let st = &mut *state;
                    let deleted = preedit_cursor::BufEdit::new_cased(
                        &mut st.input_buffer,
                        &mut st.input_cursor_pos,
                        &mut st.input_buffer_cased,
                    )
                    .delete();
                    if !deleted {
                        // 光标已在末尾，前方无字符可删。
                        KeyAction::Consumed
                    } else if state.input_buffer.is_empty() && !state.committed_segs.is_empty() {
                        // 剩余编码被删空但仍有已转换段：回退最后一段（对齐 Go handleDelete）。
                        self.pop_committed_seg(&mut state)
                    } else {
                        self.update_candidates(&mut state);
                        if state.input_buffer.is_empty() {
                            self.notify_ui_hide();
                            KeyAction::ClearComposition
                        } else {
                            let display = state.preedit.clone();
                            let caret_pos = self.composition_caret(&state);
                            self.notify_ui_update(&state);
                            KeyAction::UpdateComposition {
                                caret_pos,
                                text: display,
                            }
                        }
                    }
                }
            }
            keymap::VK_UP | keymap::VK_DOWN | keymap::VK_PRIOR | keymap::VK_NEXT => {
                // 方向/翻页键回退臂：有候选时翻页/高亮已由上面的 apply_session_action（配置驱动）处理，
                // 这里只剩"无候选"情形——无组合则透传给应用，有组合则消费。
                if state.input_buffer.is_empty() && state.committed_text.is_empty() {
                    KeyAction::PassThrough
                } else {
                    KeyAction::Consumed
                }
            }
            keymap::VK_QUOTE | keymap::VK_BACKTICK
                if data.modifiers & MOD_SHIFT == 0
                    && !state.input_buffer.is_empty()
                    && self.pinyin_separator_key(data.key_code) =>
            {
                // 拼音手动音节分隔符：把 `'` 压入缓冲作硬边界（引擎按 `'` 强制切分、查询前剥除、
                // preedit 原样保留含末尾 `'`）。走与字母键一致的候选刷新路径。
                // 置于选词/标点分派（`_` 臂）之前：分隔符模式下该键优先作分隔符而非三选键——
                // auto 模式仅在 `'` 未被占作选择键时才拦截 `'`（见 pinyin_separator_key）。
                {
                    let st = &mut *state;
                    preedit_cursor::BufEdit::new(&mut st.input_buffer, &mut st.input_cursor_pos)
                        .insert('\'');
                }
                match self.update_candidates(&mut state) {
                    InputOutcome::AutoCommit(text) => {
                        // 记账码取首候选（按来源分流，见 `freq_code`），与上一处 AutoCommit 同口径。
                        let (source, code) = state
                            .candidates
                            .first()
                            .map(|c| (c.source, self.freq_code(&state.input_buffer, c)))
                            .unwrap_or_else(|| {
                                (CandidateSource::default(), state.input_buffer.clone())
                            });
                        let out = self.commit_candidate(&mut state, &text, None, source, &code);
                        self.notify_ui_hide();
                        return Self::commit_action(out, true);
                    }
                    // 含副作用命令自动命中：与空格选中命令同路（清组合 + 异步执行）。
                    InputOutcome::AutoCommand(cand) => {
                        return self.commit_command(&mut state, &cand);
                    }
                    InputOutcome::Clear => {
                        state.input_buffer.clear();
                        state.candidates.clear();
                        self.notify_ui_hide();
                        return KeyAction::ClearComposition;
                    }
                    InputOutcome::Normal => {}
                }
                let display = state.preedit.clone();
                let caret_pos = self.composition_caret(&state);
                self.notify_ui_update(&state);
                KeyAction::UpdateComposition {
                    caret_pos,
                    text: display,
                }
            }
            _ => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                // 触发键优先级链（对齐 Go decideBufferedTrigger，缓冲非空/有候选时）：
                if !shift {
                    // B/C. 二/三候选键 + 候选足够 → 选候选
                    //
                    // ★ 双拼韵母键（微软/搜狗/紫光的 `;` = ing）**到不了这里**：它们已由
                    // 上游的非字母码元闸门 `try_code_char_gate` 接管进缓冲。此处原有一段
                    // `is_shuangpin_final` 局部避让，只做到「跳过选词」而没人接住那个键——
                    // 它接着流到 D0 的模式引导键（`;` 出厂绑 quick_mix）和下方标点流水线，
                    // 于是 `ing` 韵母仍旧打不出。三条拦截通路只挡了一条，是典型的半截修复。
                    // 现由码元集单点仲裁（拼音引擎的 `input_chars` 从双拼布局推导）。
                    let mut select_overflow: Option<char> = None;
                    if let Some(offset) = self.select_key_offset(data.key_code) {
                        let (start, end) = self.page_range(&state);
                        let idx = start + offset;
                        if idx < end {
                            let cand = state.candidates[idx].clone();
                            return self.commit_selected(&mut state, &cand, offset as i32);
                        }
                        // E. 越界：记下触发键字符，延后到模式触发判定之后再按 overflow 策略处理
                        // （对齐 Go decideBufferedTrigger——次/三选键越界时 overflow 排在
                        // 模式激活之后，故 `;` 候选不足时优先进快捷输入而非 overflow）。
                        // 仅在有 input session 时才标记越界；空缓冲+空候选（完全空闲态）
                        // 应回落到下方普通标点流程，否则 ' / ; 在中文空闲模式下永远被吞。
                        if !state.input_buffer.is_empty() || !state.candidates.is_empty() {
                            select_overflow = punct_char(data.key_code, false);
                        }
                    }
                    // D0. 方案级按键功能表（`[key_actions]`）先于全局引导键裁决。
                    //
                    // ★ 这是进模式的**第二条通路**（顶字 + 进模式），与空缓冲的
                    // `try_activate_mode` 并列。两条都必须接同一个裁决，否则方案里写的
                    // `none` 只挡得住一条——空码按 `;` 会被这里接管，表现为「禁用没生效」。
                    // 本臂的模式触发判定不要求缓冲非空，故空码同样走到这里。
                    match self.bound_key_decision(data.key_code) {
                        crate::handle_lifecycle::BoundKeyDecision::Act(action) => {
                            if let Some(act) = self.commit_and_enter_bound_action(
                                &mut state,
                                &action,
                                data.key_code,
                            ) {
                                return act;
                            }
                            // 门卫没过：不吞键，落普通流程（与空缓冲进入同策略）。
                        }
                        // 让位：跳过下面全部模式触发判定，落普通流程。
                        crate::handle_lifecycle::BoundKeyDecision::Yield => {}
                        crate::handle_lifecycle::BoundKeyDecision::NotBound => {
                            // D. 模式触发键 → 顶屏高亮候选 + 进模式。
                            // 特殊模式引导键（判定顺序对齐空缓冲时 handle_lifecycle：special 先于
                            // mix）——方案不可加载则不拦截，落普通流程（与空缓冲进入同守卫）。
                            // 传真实 key_code → 组合区写引导符，与空缓冲进入一致。
                            if let Some(act) =
                                self.try_global_trigger_commit_enter(&mut state, data)
                            {
                                return act;
                            }
                        }
                    }
                    // E. 次/三选键越界且非模式触发键 → 按 input.overflow.select_key 处理
                    if let Some(ch) = select_overflow {
                        return self.handle_overflow_select_key(&mut state, ch, data.prev_char);
                    }
                }
                if let Some(ch) = punct_char(data.key_code, shift) {
                    // 快照 held_text：非参与集合的标点会在 try_smart_symbol_replace 中解除武装
                    // 并清空 held_text，须在此前保存，以便下方普通标点流程将旧符号纳入 CommitText。
                    // 加超时防护：若 arm.at 已超出 timeout，说明 C++ timer 已自然触发提交，
                    // held_text 已过期——不再使用，防止二次提交（"。" → 等待 >500ms → "=" → "。。="）。
                    let pre_held_text = {
                        let arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
                        let timeout = self.smart_symbol_timeout();
                        let still_in_window =
                            arm.at.map(|t| t.elapsed() < timeout).unwrap_or(false);
                        if still_in_window {
                            arm.held_text.clone()
                        } else {
                            None
                        }
                    };
                    // 智能符号模式：同键连按删中文标点改英文（press2 短路返回）。
                    // 须在候选提交逻辑之前：press2 时无待输入，依赖光标前字符匹配武装态。
                    if let Some(act) = self.try_smart_symbol_replace(&state, ch, data.prev_char) {
                        return act;
                    }
                    // 标点顶码上屏开关：有编码/已确认前缀时，码表/混输按方案
                    // engine.codetable.punct_commit 决定是否顶字上屏。
                    // 关闭时标点「直接无效」——吞掉该键、保留编码继续输入（不顶字、不透传上屏
                    // 英文标点）。该功能少用，吞键比 Go 的 `return nil` 透传更符合预期。
                    // TODO(拼音标点顶码)：拼音引擎也应有独立 punct_commit 配置（默认开），
                    // 待相关引擎配置重构落定后接入；当前拼音恒顶字上屏（等价默认开）。
                    let has_input =
                        !state.input_buffer.is_empty() || !state.committed_text.is_empty();
                    if has_input {
                        let punct_commit = match self.engine_mgr.current_engine_type() {
                            Some(wind_engine::EngineType::Pinyin) => true,
                            // 码表/混输：读有效码表配置（全局 schema.codetable + 方案 override）。
                            _ => self.engine_mgr.codetable_settings().punct_commit,
                        };
                        if !punct_commit {
                            return KeyAction::Consumed;
                        }
                        // HoldComposition + has_input：arm 已设 hold_pending_commit，
                        // 顶屏上屏候选后开 HoldComposition 放入中文标点。
                        let hold_info = {
                            let arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
                            if arm.armed && arm.hold_pending_commit {
                                Some((
                                    arm.str.clone(),
                                    self.smart_symbol_timeout().as_millis() as u32,
                                ))
                            } else {
                                None
                            }
                        };
                        if let Some((hold_text, timeout_ms)) = hold_info {
                            // 命令候选顶屏 → 执行命令（与按空格一致），不走智能符号 Hold。
                            if let Some(act) = self.top_commit_command_guard(&mut state) {
                                return act;
                            }
                            // 空码丢弃（`punct_on_empty_behavior = "clear"`）：与下方普通标点
                            // 出口同判据、同语义。本分支是智能符号专属的**独立**上屏通路，
                            // 只改那边会得到「开了智能符号的宿主上开关不生效」的间歇性不一致。
                            let discard_empty_code = state.candidates.is_empty()
                                && !state.input_buffer.is_empty()
                                && self.punct_clears_on_empty();
                            let committed = self.take_committed(&mut state);
                            let mut commit_text = if discard_empty_code {
                                String::new()
                            } else {
                                self.maybe_s2t(&state, &committed)
                            };
                            if !state.candidates.is_empty() {
                                let (start, _) = self.page_range(&state);
                                let idx =
                                    (start + state.selected_index).min(state.candidates.len() - 1);
                                let cand = state.candidates[idx].clone();
                                // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
                                let freq_code = self.freq_code(&state.input_buffer, &cand);
                                self.record_selection(&freq_code, &cand.text, cand.source);
                                self.record_commit(
                                    &cand.text,
                                    state.input_buffer.len() as u32,
                                    (idx - start) as i32,
                                    CommitSource::Candidate,
                                );
                                commit_text.push_str(&self.cand_s2t_text(&state, &cand));
                            } else if !state.input_buffer.is_empty() && !discard_empty_code {
                                // 无候选顶屏的是原码 → 同回车，用用户所打的大小写形态。
                                commit_text.push_str(preedit_cursor::cased_or_buffer(
                                    &state.input_buffer,
                                    &state.input_buffer_cased,
                                ));
                            }
                            state.input_buffer.clear();
                            state.candidates.clear();
                            {
                                let mut arm =
                                    self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
                                arm.held_text = Some(hold_text.clone());
                                arm.hold_pending_commit = false;
                            }
                            self.record_commit(&hold_text, 0, -1, CommitSource::Punctuation);
                            self.notify_ui_hide();
                            return KeyAction::CommitAndHoldComposition {
                                commit_text,
                                hold_text,
                                timeout_ms,
                            };
                        }
                    }
                    // 命令候选顶屏 → 执行命令（与按空格一致），不上屏 display 标签、不追加标点。
                    if let Some(act) = self.top_commit_command_guard(&mut state) {
                        return act;
                    }
                    // 标点/符号键：先上屏已转换前缀 + 首选候选（若有输入），再追加（转换后的）标点
                    //
                    // 空码（缓冲非空但一个候选都没有）+ `punct_on_empty_behavior = "clear"`：
                    // 废码与已转换前缀都不上屏，只出标点本身。丢 `committed_text` 是与
                    // `enter_behavior` 的 clear 对齐的既定决策——「清空编码」就是清空全部，
                    // 不让用户记忆「哪部分会保留」（见 enter-behavior-clear-semantics.md）。
                    // 码表下 `committed_text` 恒为空串，实际影响面只在拼音逐步转换。
                    //
                    // ⚠️ 判据必须算在 `take_committed` **之前**：那一步会把 committed_text 取空，
                    // 之后再问就恒为假。
                    let discard_empty_code = state.candidates.is_empty()
                        && !state.input_buffer.is_empty()
                        && self.punct_clears_on_empty();
                    let committed = self.take_committed(&mut state);
                    let mut out = if discard_empty_code {
                        String::new()
                    } else {
                        self.maybe_s2t(&state, &committed)
                    };
                    // 若此前有 HoldComposition 残留（非参与集合标点令 arm 解除武装），
                    // 将旧符号纳入 out 首部：CommitText 原子替换 TSF 组合态，timer 被 CancelHoldTimer
                    // 取消，旧符号不会二次提交，也不会因组合态被覆盖而丢失。
                    if let Some(ref held) = pre_held_text {
                        out = format!("{}{}", held, out);
                    }
                    // ★ 联想态**不顶屏**（见 `commit_highlight_then_char` 里的同款守卫）。
                    //
                    // 顶屏的语义前提是「用户打了码、还没选词，按标点意味着『就选高亮那条吧』」。
                    // 联想态没有码——高亮那条是输入法猜的，此刻按「。」的意图就是打个句号。
                    //
                    // 真机现象（2026-08-16）：打「我」上屏、联想首条「我们」、按「。」得到
                    // 「我我们。」——既顶了不该顶的，又用了整词而非该补的那半截。
                    //
                    // ⚠️ 注意这一段**不受上面 `has_input` 守卫**：那个只挡住了「顶码上屏开关」
                    // 那条分支，本段是标点的通用出口，联想态照样走得到。判据必须自己带。
                    if !state.candidates.is_empty() && !state.assoc_active() {
                        let (start, _) = self.page_range(&state);
                        let idx = (start + state.selected_index).min(state.candidates.len() - 1);
                        let cand = state.candidates[idx].clone();
                        // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
                        let freq_code = self.freq_code(&state.input_buffer, &cand);
                        self.record_selection(&freq_code, &cand.text, cand.source);
                        // 标点上屏前先记被顶出的高亮候选（来源候选）。
                        self.record_commit(
                            &cand.text,
                            state.input_buffer.len() as u32,
                            (idx - start) as i32,
                            CommitSource::Candidate,
                        );
                        out.push_str(&self.cand_s2t_text(&state, &cand));
                    } else if !state.input_buffer.is_empty() && !discard_empty_code {
                        // 无候选顶屏的是原码 → 同回车，用用户所打的大小写形态。
                        out.push_str(preedit_cursor::cased_or_buffer(
                            &state.input_buffer,
                            &state.input_buffer_cased,
                        ));
                    }
                    let had_input = !state.input_buffer.is_empty()
                        || !state.candidates.is_empty()
                        || !committed.is_empty();
                    state.input_buffer.clear();
                    state.candidates.clear();

                    // CapsLock + 无待提交内容：TSF 层应已透传此键，coordinator 不应收到；
                    // 防御性兜底——直接透传让系统产生原始 WM_KEYDOWN + WM_CHAR。
                    if state.caps_lock && !had_input {
                        return KeyAction::PassThrough;
                    }

                    // 标点单点流水线：自定义映射 > 数字后智能 > 中文标点 > 全半角。
                    // CapsLock 开时大写语义等同英文模式，临时关闭中文标点转换。
                    let saved_chinese_punct = state.chinese_punct;
                    if state.caps_lock {
                        state.chinese_punct = false;
                    }
                    // 引号交替态钉左：开了配对后一次按键即产出完整一对，交替开关不参与决策。
                    let quote_paired = self.pin_quote_left_if_paired(&state, ch);
                    let piece = self.convert_punct(&state, ch, data.prev_char);
                    state.chinese_punct = saved_chinese_punct;
                    out.push_str(&piece);
                    // 标点字符（候选部分已在标点前顶屏候选处记 Candidate；标点候选已 set
                    // stat_recorded，故此处必须显式记标点，否则顶层 fallback 会跳过它）。
                    self.record_commit(&piece, 0, -1, CommitSource::Punctuation);
                    if had_input {
                        self.notify_ui_hide();
                    }
                    // 标点配对（对齐 Go）：插入配对 + 智能跳过
                    let pch = piece.chars().last().unwrap_or(' ');
                    if let Some(pairs) = self.active_pairs(state.chinese_punct) {
                        // 智能跳过：仅无候选前缀（out 即标点本身）时，输右括号→光标右移。
                        // 引号一律不走此路（`quote_paired` 中文引号 / `*l != *r` 对称英文引号）：
                        // 对称配对的按键不携带开/闭这一位，
                        // 无从判断用户想跳出还是想嵌套新的一对，故取消右符号处理、跳出交给跳出键。
                        // 非对称配对（括号类）则由 `right_symbol` 开关决定是否跳出。
                        if out == piece
                            && !quote_paired
                            && self.rt().jump_out_on_right_symbol
                            && pairs.iter().any(|(l, r)| *r == pch && *l != *r)
                        {
                            let mut tr =
                                self.pair_tracker.lock().unwrap_or_else(|e| e.into_inner());
                            // 同 handle_punct：多字符右段配不上单个标点按键，只能 Tab/Enter 跳出。
                            if tr.peek().is_some_and(|e| e.right_is_char(pch)) {
                                tr.pop();
                                return KeyAction::MoveCursorRight { count: 1 };
                            }
                            tr.clear();
                        }
                        // 插入配对：左括号 → 补右括号，光标置于其间
                        if let Some((_, right)) = pairs.iter().find(|(l, _)| *l == pch).copied() {
                            self.push_pair(pch, right);
                            // 右引号已由本次配对补出，交替开关不该停在「右」——否则一旦中途
                            // 关掉配对，遗留的右态会让下一个引号直接出闭引号。
                            if quote_paired {
                                self.punct
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .pin_quote_left(ch);
                            }
                            let cursor_offset = out.encode_utf16().count() as u32;
                            let text = format!("{}{}", out, right);
                            return KeyAction::InsertTextWithCursor {
                                text,
                                cursor_offset,
                            };
                        }
                    }
                    Self::commit_action(out, true)
                } else if !state.input_buffer.is_empty() {
                    KeyAction::Consumed
                } else {
                    KeyAction::PassThrough
                }
            }
        }
    }

    fn handle_focus_gained(&self, data: &FocusData) -> Option<StatusUpdateData> {
        // 与 handle_focus_lost 的 token 日志配对：只有两边都记 token，才能从日志算出
        // 「同一实例 gained 后多久自己 lost」——区分 DocMgr 抖动与真实离开就靠这个间隔。
        tracing::debug!(
            "handle_focus_gained: token={:#x} scope={:#x}",
            data.client_token,
            data.input_scope_mask
        );
        // 切进新的可编辑上下文同样是「用户动了别处」。⚠️focus_gained **没有任何去重**
        // （每次 DocMgr 获焦都发一条，Excel 同一 DocMgr 6ms 抖动、VSCode 一次切换 5 次都会
        // 各发一条），全靠 menu_close_on_focus_change 的守卫期挡住刚弹出的菜单。
        self.menu_close_on_focus_change("focus_gained");
        // 解析焦点进程的 caret 兼容态（微信 caret_use_top、per-app caret_offset_* 等）。
        // ★★ 必须在下面的 `apply_focus_caret` **之前**跑：那一步会读 `active_compat` 做
        // `caret_use_top`/`caret_offset_*` 变换，若仍在这之后调用，本次焦点事件带来的第一份
        // 坐标就会拿**上一个进程**的规则去变换——同步段此刻还没来得及切，症状是「刚切到这个
        // 应用第一次候选框/状态气泡位置不对，之后才对」，很容易被误判成 DPI 换算没生效。
        // 本段为 FOCUS_GAINED 的重型后置段（DLL 阻塞响应已写出），同步 OpenProcess 不影响
        // 首键延迟，提前到这里跑没有性能代价。
        //
        // ⚠ 必须在覆写 active_compat **之前**取旧值：`update_active_compat` 会整体覆写它，
        // 跑完之后读到的已是新进程的规则，「切换前那个应用有没有初始规则」就永远取不到了。
        // 漏掉这点不会编译报错、不会 panic，只表现为「从规则应用切出去后模式不恢复」。
        let new_pid = (data.client_token >> 32) as u32;
        // macOS：宿主名只能由 `.app` 告知（服务进程的 `process_name` 恒空）。必须**先于**
        // update_active_compat 落进缓存，否则那边读到空名 → compat 规则匹配不上、per-app
        // 记忆表查不到，整条按应用链路静默退化成全局行为。Windows 恒为空串，不进此分支。
        if !data.bundle_id.is_empty() && new_pid != 0 {
            self.pid_names
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(new_pid, data.bundle_id.to_lowercase());
        }
        // ⚠ 取自 `mode_scope` 而非 `active_compat`：后者会被过渡窗口（任务栏）更新，
        // 拿它当「上一个模式归属宿主」会让紧随其后的桌面焦点被判成同进程、规则不再生效。
        // 详见 `mode_scope` 字段注释。
        let (old_pid, old_has_rule) = *self.mode_scope.lock().unwrap_or_else(|e| e.into_inner());
        self.update_active_compat(data.client_token);
        let new_has_rule = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_initial_rule;
        // 焦点 caret 走与同步段同一个入口。**不要在这里直写 `state.caret_*`**——重型段晚于
        // 同步段执行，直写会把同步段的 height 守卫与 caret_use_top 变换整个覆盖掉。
        // 详见 apply_focus_caret 的文档注释。
        self.apply_focus_caret(
            &CaretData {
                x: data.x,
                y: data.y,
                height: data.height,
                composition_start_x: data.composition_start_x,
                composition_start_y: data.composition_start_y,
                source: data.caret_source,
            },
            "handle_focus_gained",
        );
        // 组合起点锚定作废：焦点事件意味着**换了 docMgr**。组合本身可能还在（buffer 未清），
        // 但它的宿主位置可能整体迁移——Excel 输入时会在「单元格」与「公式编辑栏」两个 docMgr
        // 之间来回切，实测组合从 (593,572) 迁到 (1457,959)。而锚定「同一组合只锁一次、之后
        // 不再更新」的隐含前提正是**起点不会移动**，这里恰好证伪。
        //
        // 不作废的后果是候选窗钉死在旧 docMgr 上：协调器拿 state.caret_* 判出 reshow，下发时
        // 却用锁死的组合起点，日志上表现为「reshow: dx=1297 说要重定位，UI pos 却纹丝不动」。
        // 清掉后由下一帧 caret_update 就地重锁，候选窗跟到新位置。
        *self
            .composition_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        // 坐标缓存作废（同上一段的理由，只是作用在另一个消费者上）：刚写进 state 的那份
        // 是**焦点事件随包携带**的坐标，宿主此刻多半还没 reflow，甚至根本还没建好新文档的
        // 编辑上下文（Excel 实测 454ms）。它够格当"没有更好选择时的兜底显示位置"，但不够格
        // 让 fast 档判定"可以跳过等待了"。
        self.caret_cache_verified
            .store(false, std::sync::atomic::Ordering::Relaxed);
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            // 焦点进入文本框 = 本输入法激活（对齐 Go HandleFocusGained → SetIMEActivated(true)）。
            // 不依赖 IME_ACTIVATED 的到达时机，确保工具栏在焦点到达时即可显示。
            state.ime_active = true;
            // DLL 只对「有可编辑上下文」的 DocMgr 发 focus_gained（无上下文走 NoEditCtx
            // 分支），故收到本命令即等价于"焦点在可编辑控件里"。这是 has_edit_context
            // 唯一的置真路径之一，另一处是 handle_ime_activated 的兜底。
            state.has_edit_context = true;
            // 权威信号：DLL 只在确有可编辑上下文时才发 focus_gained（没有则改发
            // focus_lost(NoEditCtx)），故这里可以放心清掉「不可输入」判定。
            state.focus_no_edit_ctx = false;
        }
        // 撤销上屏计数复位：进入新文本框，光标前是新上下文，下次 undo 退化删 1
        // （首次聚焦无配对 focus_lost 时，本处兜底）。
        self.last_commit_len
            .store(1, std::sync::atomic::Ordering::Relaxed);
        // 配对状态归属校验（防御性）：配对栈是全局单栈、不分宿主，栈顶有可能是别的宿主压的。
        // 真实失焦已在 handle_focus_lost 清过栈，能活到这里的只有 CtxLost 噪声，故本校验
        // 正常不触发；留着是因为成本为零，且「全局单栈」这个事实没变。
        self.clear_pair_tracker_if_foreign(data.client_token);
        // 记录活动客户端：鼠标点击的 commit 只推给它，避免广播多发
        if data.client_token != 0 {
            self.push_server.set_active_token(data.client_token);
            // 与上一行分开记：`active_token` 也可能由 `ime_activated` 设置（每进程仅一次），
            // 两者分叉即意味着某宿主的 focus_gained 被上游吃掉了。判据见 `gained_token`，
            // 消费点在 handle_focus_lost 的 WARN。
            self.push_server.note_focus_gained(data.client_token);
        }
        // per-app 状态：进程名已入缓存，按规则表/记忆表/默认值切换本应用中英状态。若与同步段
        // get_current_mode 回传值不同（该进程首次聚焦），随后的 push_activation_status 推送修正。
        //
        // 两个条件的分工：
        //   crossed      焦点**跨进程**切入才重算。同应用内的焦点跳转（Everything 的搜索框
        //                ↔ 结果列表）不重算，否则用户手切的模式会被反复拉回初始值——这正是
        //                「初始值」与「锁定」的分界线。
        //   per_app / has_rule
        //                per_app_scope 是既有的按应用记忆语义；has_rule 把 compat.toml 规则的
        //                影响严格限制在**进出规则应用**这一步。判据若退化成「规则表非空」，
        //                则任意两个应用之间的切换都会重算，global+remember=false（出厂默认）下
        //                会把用户在 Word 手切的英文在切到 Chrome 时重置掉，与规则应用无关。
        //
        // 取舍：per_app_scope 下同进程重复 focus_gained 不再重算（此前每次都算）。记忆表由
        // record_app_mode 与当前状态保持同步，重算结果恒等于现值，故语义无变化；代价是失去了
        // 一条隐式的 compartment 脏事件自愈路径，该自愈在 IME_ACTIVATED 路径仍然保留。
        let crossed = new_pid != 0 && old_pid != new_pid;
        // 作用域一票否决：任务栏 / Alt+Tab 切换器与桌面同属 explorer.exe，仅凭进程名
        // 分不开，判据只能来自窗口类。名字取 update_active_compat 刚填好的缓存（此刻必已
        // 就绪）。未配作用域的进程恒放行 ⇒ 绝大多数应用零变化。
        // 详见 `InitialModeScopeRule` 与 should_reapply_initial 注释。
        let proc_name = self.cached_proc_name(data.client_token);
        let out_of_scope = !self
            .app_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .initial_mode_applies_to_window(&proc_name, &data.window_class);
        // ★ 无条件打印窗口类：此前只在命中时打，于是「没打日志」同时意味着
        //   「没配作用域」「窗口类为空」「类不在清单里」三种情况，而它们的排查方向不同。
        //   本轮缺陷（空窗口类被放行）之所以要靠旁证推断，就是因为这行当时只打一半。
        tracing::debug!(
            "focus_gained: proc={proc_name} class={:?} 作用域内={}",
            data.window_class,
            !out_of_scope
        );
        if out_of_scope {
            // 独立日志行：不打的话「模式没跟着切」在日志里与「压根没发生跨进程切换」
            // 完全同形，而两者的排查方向相反。
            tracing::debug!(
                "focus_gained: 窗口在初始模式作用域外 class={:?} → 跳过重算（mode_scope 不推进）",
                data.window_class
            );
        } else if new_pid != 0 {
            // 只有**真正参与决策**的焦点才推进模式归属。过渡窗口跳过这一步，是为了不把
            // 「跨进程切入」这个一次性事件提前消费掉——否则点任务栏再回桌面时，桌面就成了
            // 「同进程」，它配的 initial_mode 永远不会生效（实测缺陷，见字段注释）。
            *self.mode_scope.lock().unwrap_or_else(|e| e.into_inner()) = (new_pid, new_has_rule);
        }
        if should_reapply_initial(
            crossed,
            self.rt().config.input.default.per_app_scope(),
            old_has_rule,
            new_has_rule,
            out_of_scope,
        ) {
            self.apply_initial_mode(data.client_token, false);
        }
        let status = self.build_status();
        self.push_activation_status(data.client_token);
        self.notify_toolbar_async(); // 激活态 → 工具栏显示（异步，避免 is_foreground_fullscreen 阻塞 bridge 线程）
        self.show_persistent_status_if_always(); // 常驻模式:获焦即显示状态
        // ui.status.show_on_focus：切到新宿主时提示一次。按 client_token 去重——同一宿主内换
        // docMgr（Excel 单元格 ↔ 公式栏）不重复弹，见 last_focus_tip_token。
        self.show_focus_status_if_enabled(data.client_token);
        let pid = (data.client_token >> 32) as u32;
        self.apply_input_diag(pid, data.disabled, data.reason, data.input_scope_mask);
        Some(status)
    }

    fn handle_focus_lost(&self, client_token: u64, reason: FocusLostReason) {
        // 独立日志行：失焦此前在服务端日志里完全不可见，只能靠 TSF 日志反推 HideToolbar
        // 的来源（2026-07-26 工具栏闪隐排查即因此多绕一圈）。token 便于与 DLL 日志的
        // `Sending focus_lost token=…` 对齐到具体宿主实例。
        tracing::debug!(
            "handle_focus_lost: token={:#x} reason={:?}",
            client_token,
            reason
        );
        // ★★ CapsLock 钩子闸门兜底归零，**先于 stale 判定**：钩子是全局的，闸门若因任何
        // 疏漏滞留在 true，用户切到别的应用后按 CapsLock 就完全失灵——这是本功能唯一
        // 会伤到「没在用输入法的时刻」的故障方向，必须在最宽的路径上归零。
        // 与 menu_close 同理放在 stale 判定之前：陈旧失焦同样证明用户动了别处。
        wind_keys::capslock_hook::set_should_eat(false);
        // 关菜单**先于** stale 判定与 reason 分流：菜单的生命周期与输入态无关，
        // 陈旧失焦/噪声层失焦同样证明用户动了别处。详见 menu_close_on_focus_change。
        self.menu_close_on_focus_change("focus_lost");
        if self.is_stale_focus_event(client_token, "handle_focus_lost") {
            return;
        }
        // ★ 探针：放行了一条「从未上报过 focus_gained 的 token」的失焦。
        //
        // `is_stale_focus_event` 挡不住它——`active_token` 有两个来源，`ime_activated`
        // 那条每进程只发一次，于是「宿主的 focus_gained 全被上游吃掉」时 token 恰好
        // **等于** active，照常放行，把 ime_active / has_edit_context 清掉后再没有任何
        // 东西能置回来（focus_gained 才置，而它正是被吃掉的那个）。2026-08-18 任务管理器
        // 就是这样：DLL 的 locked/transient 守卫把 WinUI 3 宿主判成 transient，症状是
        // 「首次启动正常、切走再切回工具栏不显示」，排查时只能靠翻 DLL 日志反推。
        //
        // 用 WARN 而非 DEBUG：这不是可以正常发生的事，出现一次就说明上游有事件被吞。
        // 不在此处做任何补救（照常执行清理）——补救等于对一个未知成因猜后果，先把它
        // 变成可见的信号，成因由日志定位。
        //
        // ⚠ 附加 `active != 0` 一项不是可有可无的：`is_stale_focus_event` 对 `active == 0`
        // （尚无任何客户端获焦）无条件放行，那种失焦压根没有归属可清，警告它纯属噪音。
        // 能走到这里且 `active != 0`，则由 stale 校验反推必有 `client_token == active`
        // ——正是「它就是当前活动客户端，却从未 gained 过」这一种。
        let gained = self.push_server.gained_token();
        if client_token != 0 && client_token != gained && self.push_server.active_token() != 0 {
            tracing::warn!(
                "handle_focus_lost: token={client_token:#x} 从未上报过 focus_gained（最近 gained={gained:#x}）——上游可能吞掉了它的 focus_gained，激活态被清后将无人恢复"
            );
        }
        // 三项后果彼此独立，由 reason 决定各自是否发生（矩阵见 FocusLostReason）。
        // 一刀切地全做，就是 CtxLost 清输入态复发「首字符直接上屏」的由来；
        // 一刀切地全不做，就是应用内点到非文本框工具栏永不隐藏的由来。
        let clears_input = reason.clears_input();
        // 词频已即时写入 redb（事务持久），失焦无需再落盘。
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if reason.clears_ime_active() {
                // 整个应用失去前台。用户开启系统“为每个应用窗口使用不同输入法”时，切到用
                // 别的输入法的应用不会触发 IME_DEACTIVATED，只有 FocusLost。工具栏隐藏经
                // UI 层 50ms 防抖——紧接着若有 FocusGained 会取消隐藏，无闪烁。
                s.ime_active = false;
                // 真正离开了这个宿主 ⇒ 焦点气泡的去重记录作废，下次再进来该重新提示一次。
                // **只在这一档清**：CtxLost/DocChanged 是宿主内部换 docMgr 的噪声，清了就等于
                // 按 docMgr 计数，Excel 下又会变回「输入一次闪两下」。
                *self
                    .last_focus_tip_token
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = 0;
            }
            if reason.clears_edit_context() {
                // 焦点不在可编辑控件里了 → 工具栏隐藏。DocChanged 不走这里：换文档后
                // 由随后的 focus_gained（可编辑）或 NoEditCtx（不可编辑）重新定夺。
                s.has_edit_context = false;
            }
            // ⚠ 语言栏图标只认 **NoEditCtx** 这一档：它才表示「新文档确实没有可编辑上下文」。
            // CtxLost 是 DocMgr 级失焦的噪声（"DocMgr 走了"≠"进了不可输入的地方"），
            // 拿它驱动图标就是 2026-08-18 实测到的误显「英」。详见 State::focus_no_edit_ctx。
            if matches!(reason, FocusLostReason::NoEditCtx) {
                s.focus_no_edit_ctx = true;
            }
            if clears_input {
                // 焦点切换后旧 composition 上下文已失效，清理输入态，避免候选残留到新焦点。
                s.input_buffer.clear();
                s.preedit.clear();
                // 联想候选就住在 `candidates` 里，故上面那句已经把它一并清掉了——
                // 联想的依据是「刚上屏的那段文本就在光标前面」，焦点一走这个前提就不成立。
                s.candidates.clear();
                // 复位菜单态，否则下一个键被 forward_menu_key 吞掉。
                // **本处刻意不受 MENU_FOCUS_GUARD 保护**：下面的 notify_ui_hide 会经
                // HideCandidates 无条件隐藏菜单窗口，此时若把 menu_open 留成 true，就成了
                // 「窗口没了、键还被吞」的状态不一致——比守卫失效更糟。
                s.menu_open = false;
                s.menu_opened_at = None;
                self.reset_exclusive_modes(&mut s); // 失焦丢弃临时英文/拼音/快捷输入残留
            }
        }
        if clears_input {
            // 失焦即清配对状态。**曾尝试按 reason 细分保留**（弹框夺走前台时光标其实还在
            // 括号中间），2026-07-29 真机后放弃：配对状态存在 core 全局单栈与**每个宿主进程
            // 各自一份**的 DLL 计数两处，而开启「为每个应用配置不同输入法」后切换应用会让
            // 整个 IME 上下文重建；更根本的是焦点离开期间用户做了什么（点走光标、删掉括号）
            // 输入法完全无法感知，保留状态本质上是猜测。实测「大部分情况不行」——
            // 一个大部分情况下失效的功能比没有更糟，用户拍板放弃。
            //
            // 注意「同一焦点内」的陈旧风险与本项无关，仍由 state_ttl_secs 兜底。
            self.clear_pair_tracker();
            // 撤销上屏计数复位：换窗/换文本框后光标前已非「刚上屏那段」，下次 undo 退化删 1。
            self.last_commit_len
                .store(1, std::sync::atomic::Ordering::Relaxed);
            // 失焦即清抑制态：密码框失焦到下次 focus_gained 之间无控件收键，suppress 残留虽不
            // 可利用，但属状态卫生隐患——独立 atomic，无锁依赖，不与上面的 state 锁冲突。
            self.password_suppress
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
        // 工具栏可见性无论哪种 reason 都要重算：ime_active 与 has_edit_context 任一变化都影响它。
        self.notify_toolbar_async(); // 防抖，异步避免阻塞 bridge 线程
        if clears_input {
            self.notify_ui_hide(); // 隐藏候选窗 + 弹出菜单（HideCandidates 连带关菜单）
            self.hide_tip(); // 失焦隐藏状态提示（常驻模式尤需）
            self.terminate_auto_phrase("focus_lost"); // 换窗口 = 一段输入结束
        }
        // CtxLost 刻意不碰候选窗：输入态还在（Excel 抖动保护），候选窗应跟随输入态而非
        // 焦点。真正离开时随后的 DocChanged / Thread 会收口。
    }

    fn get_current_mode(&self, client_token: u64, window_class: &str) -> (bool, bool) {
        // FocusGained 同步路径回传 ModePush：DLL 正同步阻塞等本值，仅允许锁+HashMap 查询，
        // 严禁 OpenProcess 等跨进程调用。`should_reapply_initial` / `apply_initial_mode` /
        // `cached_proc_name` / `rule_initial_*` 全部满足该约束（纯锁 + 表查询）。
        //
        // ★★ 判据与落地**必须与重型段 `handle_focus_gained` 逐字同源**——就是下面这两个
        // 函数调用，不再另写一份。
        //
        // 此处曾手抄过一份简化版：只处理「规则表 / 记忆表命中」，两者都没命中就**保持现状**，
        // 留给重型段修正。那个"保持现状"在上一个应用被规则强制成英文时是错的——现状就是
        // 那个英文。实测（2026-08-18，桌面配 initial_mode=english）：
        //     ModePush (focus sync) chineseMode=0   ← 同步段回传上一个应用的「英」
        //     ActivationStatusPush  mode=1 label=中 ← 3~5ms 后重型段纠正
        // 三个宿主逐一复现（notepad ×2、EverEdit ×1）。后果有两条：DLL 据此写
        // OPENCLOSE compartment，系统语言指示器跟着翻一下（用户看到的「闪」）；且这几毫秒里
        // 若首键已到，会按英文处理——而同步回传的**全部意义**就是消除这个首键竞态。
        //
        // 旧注释只叮嘱两处「必须同序」，没料到差异会出在**兜底层级**上：重型段的
        // `initial_chinese_mode_for` 在规则/记忆之外还有 remember_last_state 与配置默认两层。
        // 同源调用之后，这类漂移在结构上不可能再发生。
        let new_pid = (client_token >> 32) as u32;
        let (old_pid, old_has_rule) = *self.mode_scope.lock().unwrap_or_else(|e| e.into_inner());
        let crossed = new_pid != 0 && old_pid != new_pid;
        if crossed {
            let proc = self.cached_proc_name(client_token);
            // 作用域一票否决，判据与重型段完全同源。
            // ⚠ **两处都要有**：本方法先跑且 DLL 正阻塞等它的回传值，只挡住重型段的话，
            // 状态早在这里就被改掉了，日志上却显示「已跳过」——实测就栽在这一步。
            let out_of_scope = !proc.is_empty()
                && !self
                    .app_compat
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .initial_mode_applies_to_window(&proc, window_class);
            if out_of_scope {
                tracing::debug!(
                    "get_current_mode: 窗口在初始模式作用域外 proc={proc} class={window_class:?} → 保持现状"
                );
            } else if !proc.is_empty() {
                let new_has_rule = self.rule_initial_mode(&proc).is_some()
                    || self.rule_initial_punct(&proc).is_some();
                let per_app = self.rt().config.input.default.per_app_scope();
                if crate::coordinator::should_reapply_initial(
                    crossed,
                    per_app,
                    old_has_rule,
                    new_has_rule,
                    out_of_scope,
                ) {
                    // reset_aux=false：与重型段的调用逐字一致。随后重型段会用同样的入参
                    // 再调一次，`apply_initial_mode` 是幂等的（每次都按当前表重算目标）。
                    self.apply_initial_mode(client_token, false);
                }
                let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                return (s.chinese_mode, s.full_width);
            }
        }
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (s.chinese_mode, s.full_width)
    }

    fn handle_ime_activated(&self, client_token: u64) -> Option<StatusUpdateData> {
        if client_token != 0 {
            self.push_server.set_active_token(client_token);
        }
        // 切回本输入法时同样刷新焦点进程的 caret 兼容态（异步段，不阻塞 DLL）。
        self.update_active_compat(client_token);
        // 激活初始状态矩阵：remember=false 重置为配置默认（含全半角/标点）；
        // remember=true 保持全局记忆；state_scope="app" 恢复该应用的会话记忆。
        // 同时构成对 compartment 脏事件污染的自愈兜底（详见 TextService.cpp 门卫修复）。
        self.apply_initial_mode(client_token, true);
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.ime_active = true;
            // 兜底置真：宿主主动激活本输入法，通常意味着焦点已进入输入框。
            // 若某些宿主 IME_ACTIVATED 之后不补发 focus_gained，而这里不置位，
            // has_edit_context 将永远停在 false —— 工具栏再也不显示。
            // 该字段的失效方向不对称：多显示只是碍眼，永不显示是功能失效，故取宽松侧。
            s.has_edit_context = true;
            s.focus_no_edit_ctx = false;
        }
        let status = self.build_status();
        self.push_activation_status(client_token);
        self.notify_toolbar_async(); // 激活态 → 工具栏显示（异步，避免 is_foreground_fullscreen 阻塞 bridge 线程）
        self.show_persistent_status_if_always(); // 常驻模式:激活即显示状态
        Some(status)
    }

    fn handle_ime_deactivated(&self, client_token: u64) {
        tracing::debug!("handle_ime_deactivated: token={:#x}", client_token);
        // 同 handle_focus_lost：关菜单先于 stale 判定。下面清 menu_open 的那段仍保留
        // （非陈旧路径的完整清理），两处幂等叠加无副作用。
        self.menu_close_on_focus_change("ime_deactivated");
        // 与 focus_lost 同源的乱序风险：切走本输入法时旧宿主的 IME_DEACTIVATED 同样可能
        // 晚于新宿主的 focus_gained 到达（两者都是 fire-and-forget 异步写）。
        if self.is_stale_focus_event(client_token, "handle_ime_deactivated") {
            return;
        }
        // 切走本输入法（换到别的 IME / 非输入法应用）：清激活态、清输入、隐藏全部 UI。
        // 对齐 Go SetIMEActivated(false)（隐藏工具栏 + hideUI），根治“切走仍残留显示”。
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.ime_active = false;
            s.has_edit_context = false; // 切走本输入法：谈不上焦点在不在可编辑控件里
            s.focus_no_edit_ctx = false; // 同上：不表态（input_block 也会因 ime_active 早退）
            s.input_buffer.clear();
            s.preedit.clear();
            s.candidates.clear();
            s.menu_open = false;
            s.menu_opened_at = None;
            self.reset_exclusive_modes(&mut s); // 切走本输入法时丢弃独占模式残留
        }
        self.notify_toolbar_async(); // 非激活态 → notify_toolbar 内部下发 HideToolbar（异步）
        self.notify_ui_hide(); // 隐藏候选窗 + 弹出菜单
        self.hide_tip(); // 切走本输入法隐藏状态提示
        self.terminate_auto_phrase("ime_deactivated"); // 切走输入法 = 一段输入结束
    }

    fn handle_mode_notify(&self, flags: u32) {
        let chinese_mode = (flags & wind_ipc::protocol::STATUS_CHINESE_MODE) != 0;
        let clear_input = (flags & wind_ipc::protocol::STATUS_MODE_CHANGED) != 0;
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.chinese_mode = chinese_mode;
            if clear_input {
                state.input_buffer.clear();
                state.candidates.clear();
                self.reset_exclusive_modes(&mut state); // 系统模式切换时丢弃独占模式残留
            }
        }
        self.record_app_mode(chinese_mode);
        self.record_last_state();
    }

    fn handle_toggle_mode(&self) -> (Option<StatusUpdateData>, String) {
        // 「切换模式时取消大小写锁定」：CapsLock 开时按切换键，语义是"回到可输入中文
        // 的状态"（对齐搜狗）——取消锁定并归位中文，而非翻转 chinese_mode；否则
        // chinese_mode 原本为 true（被 CapsLock 压制）时翻转反而落到英文，切换仍然无效。
        let caps_cancelled = self.cancel_caps_on_switch();
        // 中英切换 = 一段输入结束。须在取 state 锁之前调用：terminate_auto_phrase 内部
        // 走词库 IO，不可在持 state 锁时进行。
        self.terminate_auto_phrase("toggle_mode");
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.chinese_mode = if caps_cancelled {
            true
        } else {
            !state.chinese_mode
        };
        let chinese = state.chinese_mode;
        // 标点随中英文切换（对齐 Go）：开启 punct_follow_mode 时，标点中/英跟随当前模式。
        if self.rt().config.input.punct.follow_mode {
            state.chinese_punct = chinese;
        }
        let commit_text = self.take_input_on_mode_switch(&mut state, chinese);
        drop(state);
        self.record_app_mode(chinese);
        self.record_last_state();
        self.punct.lock().unwrap_or_else(|e| e.into_inner()).reset();
        self.disarm_smart_symbol();
        // 配对栈**刻意不清**：中英切换既不移动光标也不消除已插入的右符号，「光标紧贴右符号」
        // 这个前提仍然成立，清掉只会让用户切走再切回后 Tab/Enter 跳不出去。真正让前提失效的
        // 是失焦与组合被终止，那两处仍清（见 clear_pair_tracker 的其余调用点）。
        // C++ 侧同源：模式切换路径调 ResetComposingState(TRUE) 保留 _pairPendingDepth，
        // 否则中文模式下 Enter 会被会话门控挡在 DLL 里，根本到不了这里。
        self.push_state_update();
        self.show_status();
        self.notify_toolbar();
        self.notify_ui_hide(); // 取消输入：隐藏候选窗
        (Some(self.build_status()), commit_text)
    }

    fn handle_system_mode_switch(&self, chinese_mode: bool) -> (Option<StatusUpdateData>, String) {
        // 「切换模式时取消大小写锁定」：目标模式由外部指定（Ctrl+Space/KBLSwitch），
        // 仅取消 CapsLock 让目标模式真正生效，不改写目标。
        let _ = self.cancel_caps_on_switch();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.chinese_mode = chinese_mode;
        // 标点随中英文切换（对齐 Go）：开启 punct_follow_mode 时，标点跟随模式。
        if self.rt().config.input.punct.follow_mode {
            state.chinese_punct = chinese_mode;
        }
        let commit_text = self.take_input_on_mode_switch(&mut state, chinese_mode);
        drop(state);
        self.record_app_mode(chinese_mode);
        self.record_last_state();
        self.punct.lock().unwrap_or_else(|e| e.into_inner()).reset();
        self.disarm_smart_symbol();
        // 配对栈刻意不清，理由同 handle_toggle_mode。
        self.push_state_update();
        self.show_status(); // 与 Shift 切换（handle_toggle_mode）统一：Ctrl+Space/外部切换也显示中/英提示
        self.notify_toolbar();
        self.notify_ui_hide(); // 取消输入：隐藏候选窗
        (Some(self.build_status()), commit_text)
    }

    fn handle_composition_terminated(&self) {
        // SearchHost.exe / 开始菜单等受限宿主：搜索框不支持 TSF composition，
        // DLL 每次设置 composition 后宿主立即终止，属伪终止事件。
        // Rust 版无 last_key_time 竞态窗口（对照 Go handle_lifecycle.go:559-572），
        // host-render 激活时直接忽略清缓冲动作以保留输入状态与候选，
        // 下一按键的 UpdateComposition 会自动重建 composition。
        // （host_render_active() 仅在 active 连接已通过白名单 setup 时为 true，
        //   不会误伤白名单外的普通宿主。）
        if self.host_render_active() {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // 必须整体复位（含 active/temp_pinyin_*/mix_* 等 overlay 状态），不能只清 input_buffer：
        // 临时拼音/快捷输入的缓冲与前缀不在 input_buffer 里，只清后者会让模式残留——
        // 真机现象：` 进临拼后点鼠标移光标，候选窗随 notify_ui_hide 消失但模式还在，
        // 再按 d 仍走 handle_temp_pinyin_key，组合区诡异地显示 `d。
        // reset_exclusive_modes 内含 disarm_smart_symbol 与强制竖排布局恢复。
        // 此回调仅在 TSF 意外终止组合时触发（焦点切换、宿主强制 EndComposition 等）；
        // 我们自己的 CommitText 不触发（_pComposition 已提前置 nullptr，走"Already released"分支）。
        // 因此在此 disarm 是安全的：意外中断必然使 HoldComposition 失效，旧 held_text 不可再用。
        self.reset_exclusive_modes(&mut state);
        // 复位菜单状态：点击别处会终止 composition 并经 notify_ui_hide 隐藏菜单窗口，
        // 但若不清 menu_open，下一个键会被 forward_menu_key 当作菜单键吞掉（首字符失效）。
        state.menu_open = false;
        drop(state);
        self.clear_pair_tracker(); // 组合意外终止：配对上下文失效，清栈防跳出键误判
        self.notify_ui_hide();
    }

    fn handle_caret_update(&self, data: &CaretData) {
        // compStart 必须打：它是「本轮 composition 的 reflow 坐标是否已到」的唯一判据
        // （compStart=(0,0) ⇒ 该帧来自 idle 更新，组合还没建立/还没 reflow），也是
        // coords_ready 逃生口与嵌入模式定位锚点的来源。此前只打 x/y/h，查候选窗定位问题时
        // 必须去翻 TSF 日志对时间戳才能补上这一维。
        tracing::debug!(
            "handle_caret_update: x={} y={} h={} compStart=({},{}) src={}",
            data.x,
            data.y,
            data.height,
            data.composition_start_x,
            data.composition_start_y,
            wind_ipc::protocol::caret_source::name(data.source)
        );
        // height==0：宿主尚未 reflow，GetTextExt 返回退化矩形，坐标不可靠 → 跳过（不更新缓存、
        // 不触发显示），等 OnLayoutChange 后的有效坐标（对齐 Go HandleCaretUpdate）。
        if data.height == 0 {
            return;
        }
        // 应用兼容规则 caret_use_top（对齐 Go HandleCaretUpdate 的 rect.bottom→rect.top）：
        // 微信等 WebView 的 GetTextExt 返回 height 不稳定（1↔20px），rect.bottom 随之漂移 ~20px，
        // 但 rect.top 始终稳定（≤1px，≈正文底端）。改用 top 定位：Y -= height，使候选窗下方显示
        // 锚在稳定的 top（wind-ui 下方公式 = caret_y + gap，不读 height，故下方不受 height 影响）。
        //
        // 关键：height 不能压成 1。上方显示时 wind-ui 用 caret_top = caret_y - height 推算正文顶端
        // （above 底边 = caret_y - height - gap）；若 height=1 则正文顶端被当成 top-1（≈正文底端），
        // 候选窗会整条压住正文/光标。故保留真实行高 raw_h，并对退化帧（raw_h=1）取下限兜底，
        // 让上方显示正确避让正文（偏大只是多留空隙，偏小才会遮挡——宁大勿小）。
        // 组合起点 Y 同步上移以保持锚点一致。后续逻辑全部基于变换后的本地副本。
        let mut data = *data;
        self.apply_caret_compat(&mut data);
        let data = &data;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (prev_x, prev_y) = (state.caret_x, state.caret_y);
        state.caret_x = data.x;
        state.caret_y = data.y;
        state.caret_height = data.height;
        state.caret_source = data.source;
        let now_valid =
            !(data.x == 0 && data.y == 0) && data.x.abs() < 32000 && data.y.abs() < 32000;
        if !now_valid {
            debug!("caret_update → 丢弃: 坐标无效（(0,0) 哨兵或越界）");
            return;
        }
        // 消费焦点气泡的挂起：DLL 在焦点路径拿不到同步锁时会异步补一条权威坐标，这就是它。
        // **必须在下面的 `composing` 闸门之前**——焦点刚到达时用户还没输入，`composing` 恒 false，
        // 放在闸门之后等于永远不执行（而且完全静默）。
        //
        // 只认 TSF 域：本闸门存在的全部意义就是不拿 GUI 回退坐标定位气泡。
        if self
            .pending_focus_tip
            .load(std::sync::atomic::Ordering::Relaxed)
            && wind_ipc::protocol::caret_source::is_tsf(data.source)
        {
            self.pending_focus_tip
                .store(false, std::sync::atomic::Ordering::Relaxed);
            debug!(
                "focus_tip → 补显示: 等到权威坐标 ({},{}) src={}",
                data.x,
                data.y,
                wind_ipc::protocol::caret_source::name(data.source)
            );
            // 先放锁再显示：show_tip 内部要重新取 state 锁读坐标，持锁调用会自死锁。
            drop(state);
            self.show_tip(&self.status_indicator_text());
            state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        }
        let composing = !state.candidates.is_empty() || !state.input_buffer.is_empty();
        if !composing {
            // 常态、非异常：上屏后到下一键之间宿主仍会上报 caret。注意坐标**已在上面写入
            // state.caret_x/y**，只是不做显示决策——这一条解释了「按键前明明收到过正确坐标，
            // 候选窗却还在等 reflow」，是排查首显延迟时最容易看漏的一环。
            debug!("caret_update → 仅更新缓存: 无组合（无候选且缓冲空），不做显示决策");
            return;
        }
        // 组合起点锚定：同一组合只接受首个有效 compStart，后续即便携带新值也不覆盖（防部分控件
        // GetRange 让起点随输入漂移，致候选窗随输入右移）。500px 校验排除 logical/physical 坐标系不一致。
        {
            let mut cs = self
                .composition_start
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !cs.2 && (data.composition_start_x != 0 || data.composition_start_y != 0) {
                let dx = (data.composition_start_x - data.x).abs();
                let dy = (data.composition_start_y - data.y).abs();
                // ⚠ 500px 校验的前提是**两者同源**——它想抓的是「同一个 context 报出的两个坐标
                // 却相差离谱」这种坐标系不一致。当 caret 本身来自 GUI 回退等非 TSF 通道时，
                // 它和组合起点压根不是一个语义域，比较毫无意义。桌面输入实测：caret=(0,1388)
                // 是任务栏残留的 Win32 光标、compStart=(473,217) 才是真实组合位置，dy=1171
                // 让这道闸门把**唯一正确的数据**当异常丢弃了。
                // 故非 TSF 源直接采信组合起点——此时它比 caret 可信得多。
                if !wind_ipc::protocol::caret_source::is_tsf(data.source)
                    && data.source != wind_ipc::protocol::caret_source::UNKNOWN
                {
                    *cs = (data.composition_start_x, data.composition_start_y, true);
                    debug!(
                        "组合起点锁定: ({},{})（跳过距离校验：caret 源={} 非 TSF，与组合起点不同源）",
                        data.composition_start_x,
                        data.composition_start_y,
                        wind_ipc::protocol::caret_source::name(data.source)
                    );
                } else if dx < 500 && dy < 500 {
                    *cs = (data.composition_start_x, data.composition_start_y, true);
                    debug!(
                        "组合起点锁定: ({},{})（本组合内不再更新；coords_ready 逃生口据此成立）",
                        data.composition_start_x, data.composition_start_y
                    );
                } else {
                    debug!(
                        "组合起点丢弃: ({},{}) 距 caret dx={dx} dy={dy} ≥500px（疑 logical/physical 坐标系不一致，caret 源={}）",
                        data.composition_start_x,
                        data.composition_start_y,
                        wind_ipc::protocol::caret_source::name(data.source)
                    );
                }
            }
        }
        // 记录本帧为「上一轮权威坐标」，供下一轮组合的试探采样做判据。
        // 放在这里（已过有效性与 composing 守卫）而非函数入口：只有真正被采纳为定位依据的
        // 坐标才有资格当基准，否则会把 idle 帧、退化帧混进来，判据立刻失真。
        *self
            .last_authoritative_caret
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (data.x, data.y, true);
        // 同一条「够格」判据的第二个消费者：坐标缓存自此对应当前插入点，fast 档的短兜底
        // 可以放心拿它首显（见 caret_cache_verified 的字段注释）。
        self.caret_cache_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // 消费首显等待：本次为 reflow 后权威坐标。
        let was_pending = {
            let mut pfs = self
                .pending_first_show
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let w = *pfs;
            *pfs = false;
            w
        };
        if was_pending {
            // 延迟的首次显示：用本权威坐标无条件首显（不过滤）。
            debug!("caret_update → 首显: 消费 pending_first_show，本帧作权威坐标");
            self.show_authorized
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.notify_ui_update(&state);
        } else if *self
            .candidate_shown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            // 已显示后的坐标更新：≤3px 微移跳过 reshow（吞掉宿主 caret 微调，如 WPS 的 2px 偏移）；
            // 显著变化（换行 / reflow 修正）才 reshow，由 UI 层 4px 位置阈值再次过滤微移。
            let dx = (data.x - prev_x).abs();
            let dy = (data.y - prev_y).abs();
            // 首显用过非权威坐标时，本轮**第一次**权威坐标改用放宽的容差：偏差在
            // 「行高 × settle_ratio」以内就不校正。抖动的观感来自校正动作本身而非坐标偏差
            // ——十几像素的偏移用户根本不会注意，跳一下却很显眼（多数输入法也这么处理）。
            // 换行/重排的偏差通常 ≥2 个行高，远超此阈值，仍会正常校正。
            let settle = if self
                .first_show_was_provisional
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                let ratio = self.rt().config.ui.candidate.first_show_settle_ratio;
                let h = data.height.max(state.caret_height).max(1) as f32;
                (h * ratio.max(0.0)) as i32
            } else {
                0
            };
            let tol = settle.max(3); // 常规微移过滤下限保持 3px 不变
            if dx <= tol && dy <= tol {
                debug!("caret_update → 忽略: 微移 dx={dx} dy={dy}（≤{tol}px，不 reshow）");
                return;
            }
            debug!("caret_update → reshow: dx={dx} dy={dy}");
            self.show_authorized
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.notify_ui_update(&state);
        } else {
            // 隐式出口：既没在等首显、候选窗也没显示着。此前这里静默结束，日志上与
            // 「首显」「reshow」无从区分——查「候选窗为什么没出现」时这一条最要紧，
            // 因为它说明本帧坐标到了但没有任何一方消费它。
            debug!("caret_update → 无动作: 未等待首显且候选窗未显示，本帧仅落缓存");
        }
    }

    /// focus_gained 随包携带的 caret：只更新坐标缓存，**不做任何显示决策**。
    ///
    /// 焦点事件带来的坐标是「切换发生的那一刻」的值，宿主可能还没 reflow。若把它交给
    /// [`Self::handle_caret_update`]，会被当成 reflow 后的权威坐标消费掉首显等待，候选窗
    /// 就在中间位置先显示一次再跳到最终位置。Excel 单元格激活实测三段坐标：
    /// 1025,687（选中态）→ **1369,1036（焦点事件，非权威）** → 1590,1092（reflow 后）。
    ///
    /// caret_use_top 变换要照做——坐标缓存本身必须与 handle_caret_update 写入的口径一致，
    /// 否则首键前的兜底坐标会和后续更新差一个行高。
    fn handle_focus_gained_caret(&self, data: &CaretData) {
        self.apply_focus_caret(data, "handle_focus_gained_caret");
    }

    fn handle_caret_probe(&self, data: &CaretData) {
        // 首帧 reflow 期间 DLL 逐次采样上报（CMD_CARET_PROBE）。默认**完全忽略**——
        // 不开 fast_first_show 的宿主必须保持「等 reflow 权威坐标」的原行为，一字不差。
        let compat = *self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
        if compat.first_show_mode != wind_config::app_compat::FirstShowMode::Fast {
            debug!(
                "caret_probe → 忽略: 当前档位={} 非 fast",
                compat.first_show_mode.as_config()
            );
            return;
        }
        // 只在正等首显时有意义：已显示 / 未 arm 的帧交给常规 caret_update 路径。
        if !*self
            .pending_first_show
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            debug!("caret_probe → 忽略: 未在等待首显（已首显过 / 未 arm）");
            return;
        }
        // 退化 rect（无高度）一律不采信：实测 WPS 首帧曾采到 top==bottom 的样本，
        // 其 x 与真实位置差 1687px，采信即大幅错位。
        if data.height <= 0 {
            debug!("caret_probe → 丢弃: 退化 rect（h<=0）");
            return;
        }
        // ★ 首帧信任门（第二条通路）：坐标缓存未经当前插入点验证时，本函数下面两条判据
        // **全都失去判断力**，必须一起让位给长兜底。
        //
        // 判据 1 靠「≠ 上一轮权威坐标」推断「宿主已 reflow」，其成立前提是那个基准与当前
        // 插入点**可比**。焦点刚切换时基准属于另一个单元格/文档/应用，probe 值当然不等于
        // 它 ⇒ 判据恒成立 ⇒ 必然采信一个还没 reflow 的坐标。判据 2（连打快路径）同理：
        // 跨焦点的"上一次按键间隔"说明不了当前这一帧的坐标可信。
        //
        // ⚠ 实测（2026-08-03 Excel）：闸门刚 arm 了 600ms 长兜底，6ms 后 probe 就用
        // (1299,535) 抢先首显，而 200ms 后真坐标是 (1344,744) ⇒ 显示后跳一次。
        // **信任门只接在兜底 timer 上是不够的——首显有多条通路，否决判据必须每条都接。**
        if self.first_show_needs_long_wait() {
            debug!(
                "caret_probe → 继续等待: 坐标缓存未经当前插入点验证，本轮判据无基准可比（x={} y={}）",
                data.x, data.y
            );
            return;
        }
        // 快路径：连续快速输入时直接采信首条采样，不再比对上一轮权威坐标。
        // 依据是连打时光标沿同一行顺序前移、不发生重排，坐标本来就八九不离十；而这种节奏下
        // 用户对「跟手」的敏感度远高于十几像素的偏差。窗口可经
        // ui.candidate.fast_typing_window_ms 调整，0 = 关闭本快路径。
        let fast_window = self.rt().config.ui.candidate.fast_typing_window_ms;
        if fast_window > 0 {
            let interval = *self
                .last_key_interval_ms
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(ms) = interval
                && ms <= fast_window
            {
                debug!(
                    "caret_probe → 提前首显(按键间隔 {ms}ms≤{fast_window}ms): x={} y={}",
                    data.x, data.y
                );
                self.first_show_was_provisional
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.handle_caret_update(data);
                return;
            }
        }
        // 判据：与上一轮权威坐标不同 ⇒ 宿主已 reflow ⇒ 本帧可信。
        // 尚无上一轮基准时（焦点刚到达的首次输入）直接采信：此时没有「旧值」可疑。
        let (lx, ly, has_base) = *self
            .last_authoritative_caret
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if has_base && data.x == lx && data.y == ly {
            debug!("caret_probe → 继续等待: 坐标仍等于上一轮权威 ({lx},{ly})，宿主尚未 reflow");
            return;
        }
        debug!(
            "caret_probe → 提前首显: x={} y={} h={}（基准 ({lx},{ly}) has_base={has_base}）",
            data.x, data.y, data.height
        );
        // 复用权威路径：更新坐标缓存 + 消费等待 + 首显。若判错，随后到达的真权威坐标
        // 会经 handle_caret_update 按放宽后的容差决定是否校正。
        self.first_show_was_provisional
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.handle_caret_update(data);
    }

    fn handle_caret_pending(&self) {
        // DLL 新组合在 reflow 完成前发来的"坐标待定"握手（_compositionJustStarted）：
        // 仅当正等待首显时，延长兜底超时到 600ms，避免 OnLayoutChange burst 慢的应用（如 EverEdit）
        // 在真实坐标到达前被 150ms 兜底用旧坐标抢先显示。
        if !*self
            .pending_first_show
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            return;
        }
        // fast 档刻意不接受这次延长：它的短兜底就是为「坐标要 60~190ms 才到」的宿主设计的，
        // 延到 600ms 等于把 fast 重新变回 wait（而组合往往活不到 100ms，兜底根本不会到期）。
        //
        // ⚠ 唯独坐标缓存不可信时**不能**在这里提前放弃延长：那种情况下短兜底会走
        // `fire_pending_first_show` 的首帧信任门自行延长，两处口径必须一致，否则表现为
        // 「握手到得早就短兜底、到得晚反而正确」这种随 IPC 时序摇摆的行为。
        //
        // ⚠ 坐标缓存不可信时 fast 档同样**不在这里**延长：那种情况的等待时长已由
        // `arm_pending_first_show` 的首帧信任门决定（且刻意不因后续事件重置）。握手若也插
        // 一脚就成了第二个真相源，表现为「握手到得早就长等、到得晚就短兜底」这种随 IPC
        // 时序摇摆的行为。
        if self.first_show_mode_is_fast() {
            debug!("caret_pending → 忽略延长: fast 档兜底时长在 arm 时已按坐标可信度定");
            return;
        }
        self.arm_pending_first_show_with_timeout(FIRST_SHOW_LONG_FALLBACK_MS);
    }

    /// 宿主报告「光标移动且当前无 composition」（C++ `TextService::OnEndEdit`，守卫
    /// `selChanged && _pComposition == nullptr`）。
    ///
    /// 这是码表自动造词**唯一能感知到「用户敲了空格/回车结束一句」的途径**：码表每选一字
    /// 就上屏并关闭 composition，此后 Space/Enter 被 TSF 直接透传给宿主，协调器根本收不到
    /// 按键（`KeyEventSink.cpp:398/966/1024` —— Backspace/Enter/Escape 仅在有 composition
    /// 或 input session 时才拦截）。
    ///
    /// # 自提交宽限期
    ///
    /// 本输入法自己提交文字后，宿主插入文本同样导致光标移动 → 同样回送本事件，且在协议层
    /// **与用户真实光标移动完全无法区分**，只能靠时间判别。若不区分，每上屏一个字就被自己
    /// 的回声判成「用户移动光标」→ flush → 缓冲永远只有 1 个字 → 造词恒不触发。
    ///
    /// 宽限值取 [`SELF_COMMIT_GRACE`]，已由真机日志校准（见该常量注释的实测分布）。
    ///
    /// 回声分支**不做任何动作**，故只记 TRACE：它的频率恒等于上屏频率（每上屏一个字必有
    /// 一条），放在 DEBUG 会把真正有信息量的「用户移动光标 → 终止序列」淹掉。需要重新
    /// 校准 `SELF_COMMIT_GRACE` 时开 TRACE 即可拿回完整分布。
    fn handle_selection_changed(&self, _prev_char: u16) {
        let since = self
            .last_self_commit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|t| t.elapsed());
        let is_echo = since.is_some_and(|d| d < SELF_COMMIT_GRACE);
        if is_echo {
            trace!("selection_changed: since_self_commit={since:?} → 自提交回声，忽略");
            return;
        }
        debug!("selection_changed: since_self_commit={since:?} → 用户移动光标");
        // 坐标缓存随之过期：用户在同一 DocMgr 内点到了别处（不发 focus_gained），而宿主
        // 只在有 composition 时才回送 caret_update，所以缓存里仍是上次输入的位置。fast 档
        // 若拿它给下一次输入的首帧定位，候选窗会先出现在旧位置再跳过来。
        // ★ 复用本判据是安全的：它的两个误判方向对本用途都不致命——误判成回声只是维持
        //   现状（不比现在差），误判成移动只是让下一次首显多等一程（慢而不错）。
        self.caret_cache_verified
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.terminate_auto_phrase("selection_changed");
    }

    fn handle_commit_request(&self, data: &CommitRequestData) -> Option<CommitResultData> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.input_buffer.is_empty() {
            return None;
        }
        let tk = data.trigger_key as u32; // 协议为 u16，统一按 VK(u32) 比对
        // 取上屏文本、来源与记账码：命中候选取候选 source，退回原码分支为 None（不可归因）。
        // 记账码按来源分流（见 `freq_code`）——码表按输入码、拼音/英文按候选码；退回原码的
        // 分支上屏的就是缓冲本身，无候选可依，用输入码。
        let cand_meta = |c: &Candidate| {
            (
                c.text.clone(),
                c.source,
                self.freq_code(&state.input_buffer, c),
            )
        };
        let raw = || {
            (
                state.input_buffer.clone(),
                CandidateSource::None,
                state.input_buffer.clone(),
            )
        };
        // ⚠️ 这是一条**独立于按键路径的上屏通路**（DLL 侧 TSF 排水 / 顶码延迟提交发起），
        // 补空格必须在此单独接线——只改 `commit_selected` 会得到「键盘空格补了、排水路径没补」
        // 的间歇性不一致。第四元 `append_space` 按分支显式给出，不在末尾统一判断：四个分支
        // 的答案各不相同，统一判断迟早把它们抹平。
        let (text, source, freq_code, append_space) = if tk == keymap::VK_SPACE {
            match state.candidates.first() {
                // 空格选首选：候选口径（与 `commit_selected` 同）。
                Some(c) => {
                    let (t, s, f) = cand_meta(c);
                    let ap = self.english_appends_space(s);
                    (t, s, f, ap)
                }
                // 空格退回原码：无候选可依，方案口径（与 VK_SPACE 空码分支同）。
                //
                // ⚠️ 这一支**没有接空码丢弃开关**（`input.space_on_empty_behavior`），按键路径
                // 那边接了。眼下不是缺陷——整条 barrier 通路是**死代码**：C++ 侧
                // `_SendCommitRequest` 只有定义没有调用点（见 wind_tsf/src/AGENTS.md「Barrier
                // mechanism 预留，尚未激活」）。哪天真把它接上，这里连同下方 VK_RETURN 分支
                // （`enter_behavior`）都得补判据，否则表现为「开关只在部分宿主/部分时机生效」。
                None => {
                    let (t, s, f) = raw();
                    (t, s, f, self.english_space_enabled())
                }
            }
        } else if tk == keymap::VK_RETURN {
            // 回车恒不补：终结性动作，与按键路径的 VK_RETURN 分支同口径。
            let (t, s, f) = raw();
            (t, s, f, false)
        } else if (keymap::VK_1..=keymap::VK_9).contains(&tk) {
            match state.candidates.get((tk - keymap::VK_1) as usize) {
                // 数字键选词：候选口径（「所有选中方式一律补」）。
                Some(c) => {
                    let (t, s, f) = cand_meta(c);
                    let ap = self.english_appends_space(s);
                    (t, s, f, ap)
                }
                // 数字键越界退回原码：**不补**。按键路径下此情形走
                // `handle_overflow_number_key`，候选为空时直接吞键不上屏——本分支是 DLL 侧
                // 独有的兜底，没有对应的键盘行为可对齐，保守不补。
                None => {
                    let (t, s, f) = raw();
                    (t, s, f, false)
                }
            }
        } else {
            // 未知触发键：不可归因，不补。
            let (t, s, f) = raw();
            (t, s, f, false)
        };
        state.input_buffer.clear();
        state.candidates.clear();
        // 与 handle_key_event 的选词路径保持一致：记录词频用于学习排序
        self.record_selection(&freq_code, &text, source);
        // 补空格**必须在记账之后**：`record_selection` 记的是词本身，带上尾空格会写出
        // 「hello 」这种与读取端（`apply_freq_rerank` 按候选文本查）永远对不上的词频键。
        let text = if append_space {
            format!("{text} ")
        } else {
            text
        };
        // 上屏即组合结束：复位首显延迟状态，使下一组合首帧重新延迟到 reflow 后的权威坐标，
        // 避免其锁定到本组合旧坐标（"上屏后立即输入候选窗错位"主场景）。
        self.reset_first_show();
        Some(CommitResultData {
            barrier_seq: data.barrier_seq,
            text,
            new_composition: String::new(),
            mode_changed: false,
            chinese_mode: state.chinese_mode,
        })
    }

    fn handle_host_render_failed(&self, reason: u32) {
        // DLL 侧 host-render 初始化/映射失败：记录告警。后续（Task 6/7）可据此回退渲染路径。
        warn!("host-render 失败上报 reason={reason}（DLL 退回进程内渲染）");
    }

    fn handle_input_state_report(&self, pid: u32, disabled: bool, reason: u8, mask: u64) {
        self.apply_input_diag(pid, disabled, reason, mask);
    }

    fn handle_diag_snapshot(&self, snap: &wind_ipc::protocol::DiagSnapshotPayload) {
        self.apply_diag_snapshot(snap);
    }
}

#[cfg(test)]
mod ext_envelope_tests {
    //! 扩展信封 `pos.*` / `shot.*` 的 body 解析与文案，以及滚轮的高亮移动。
    use super::*;

    fn coord() -> Arc<Coordinator> {
        Coordinator::new_headless(Config::default(), None)
    }

    #[test]
    fn decodes_well_formed_point() {
        assert_eq!(
            decode_ext_point(br#"{"x":123,"y":-456}"#),
            Some((123, -456))
        );
        // 多余字段照常忽略——JSON body 的向前兼容就靠这条。
        assert_eq!(
            decode_ext_point(br#"{"x":1,"y":2,"screen":"builtin"}"#),
            Some((1, 2))
        );
    }

    /// 滚轮 = 上下键调整高亮项，到页边界翻到相邻页。
    ///
    /// 回归意义：`handle_candidate_scroll` 长期是 trait 上的空实现，Windows 的
    /// host-render DLL 一直在发这个帧、服务端收下什么也不做——滚轮在两个平台都无效。
    #[test]
    fn scroll_moves_highlight_and_crosses_pages() {
        use wind_candidate::Candidate;
        let c = coord();
        let per_page = {
            let mut s = c.state.lock().unwrap();
            s.candidates = (0..12)
                .map(|i| Candidate {
                    text: i.to_string(),
                    ..Default::default()
                })
                .collect();
            s.selected_index = 0;
            s.current_page = 0;
            drop(s);
            c.per_page(None)
        };
        assert!((2..12).contains(&per_page), "本用例要求每页 2..12 项");

        // 下滚一格 → 高亮下移一项（不是翻一页）
        c.handle_candidate_scroll(-120);
        assert_eq!(c.state.lock().unwrap().selected_index, 1);

        // 一路滚到页尾再一格 → 跨到下一页首项
        for _ in 0..(per_page - 1) {
            c.handle_candidate_scroll(-120);
        }
        {
            let s = c.state.lock().unwrap();
            assert_eq!(s.current_page, 1, "页尾再下滚应翻到下一页");
            assert_eq!(s.selected_index, 0, "跨页后高亮落在首项");
        }

        // 上滚回卷到上一页末项
        c.handle_candidate_scroll(120);
        {
            let s = c.state.lock().unwrap();
            assert_eq!(s.current_page, 0);
            assert_eq!(s.selected_index, per_page - 1);
        }
    }

    /// 触控板一次轻扫的 delta 可能不足一格（<120）——整除会得 0，滚轮就"滚不动"。
    #[test]
    fn scroll_with_sub_notch_delta_still_moves_one() {
        use wind_candidate::Candidate;
        let c = coord();
        {
            let mut s = c.state.lock().unwrap();
            s.candidates = (0..5)
                .map(|i| Candidate {
                    text: i.to_string(),
                    ..Default::default()
                })
                .collect();
        }
        c.handle_candidate_scroll(-13);
        assert_eq!(c.state.lock().unwrap().selected_index, 1);
    }

    /// 惯性滚动一次可能带来极大的 delta；不设上限会一口气跳过几十项并疯狂重绘。
    #[test]
    fn scroll_is_capped_per_event() {
        use wind_candidate::Candidate;
        let c = coord();
        {
            let mut s = c.state.lock().unwrap();
            s.candidates = (0..200)
                .map(|i| Candidate {
                    text: i.to_string(),
                    ..Default::default()
                })
                .collect();
        }
        c.handle_candidate_scroll(-120 * 50);
        let s = c.state.lock().unwrap();
        let moved = s.current_page * c.per_page(None) + s.selected_index;
        assert_eq!(moved, 5, "单次事件最多移动 MAX_NOTCHES 项");
    }

    /// 无候选时不得有任何动作（也不该 panic）。
    #[test]
    fn scroll_without_candidates_is_noop() {
        let c = coord();
        c.handle_candidate_scroll(-120);
        assert_eq!(c.state.lock().unwrap().selected_index, 0);
    }

    /// 「截图所有窗口」：两侧数量相加，合成一条 Toast。
    ///
    /// 分开弹是最容易写出来的实现，也是最烦人的——候选窗 + 气泡 + 提示 + Toast 全可见时
    /// 会连弹四条通知。`already` 由服务端放进请求、`.app` 原样带回，就是为了不为这一次
    /// 往返在任何一边留状态。
    #[test]
    fn shot_all_sums_both_sides_into_one_message() {
        let v = serde_json::json!({
            "mode": "all",
            "dir": "/tmp/shots",
            "already": 1,                    // 候选窗（服务进程截的）
            "already_clipboard": true,
            "results": [
                {"target": "status_tip", "ok": true},
                {"target": "tooltip", "ok": false, "reason": "not_visible"},
                {"target": "toast", "ok": true},
            ],
        });
        let (msg, kind) = super::shot_result_message(&v);
        assert_eq!(msg, "已保存 3 张截图（候选已复制到剪贴板）\n/tmp/shots");
        assert!(matches!(kind, ToastKind::Success));
    }

    /// 一个都没截到不是错误：用户可能就是在没有任何浮窗时点的菜单。
    #[test]
    fn shot_all_with_nothing_visible_is_info() {
        let v = serde_json::json!({
            "mode": "all", "already": 0, "dir": "/tmp",
            "results": [{"target": "status_tip", "ok": false, "reason": "not_visible"}],
        });
        let (msg, kind) = super::shot_result_message(&v);
        assert_eq!(msg, "没有可见窗口可截图");
        assert!(matches!(kind, ToastKind::Info));
    }

    /// 单窗截图的三种结局：成功带路径、不可见（Info 不是 Error）、真失败。
    #[test]
    fn shot_single_wording_by_outcome() {
        let mk = |r: serde_json::Value| {
            super::shot_result_message(&serde_json::json!({ "results": [r] }))
        };
        let (msg, kind) = mk(serde_json::json!({
            "target": "tooltip", "ok": true, "clipboard": true, "path": "/tmp/t.png"
        }));
        assert_eq!(msg, "悬停提示已截图（已复制到剪贴板）\n/tmp/t.png");
        assert!(matches!(kind, ToastKind::Success));

        let (msg, kind) = mk(serde_json::json!({
            "target": "status_tip", "ok": false, "reason": "not_visible"
        }));
        assert_eq!(msg, "状态提示气泡未显示，无法截图");
        assert!(matches!(kind, ToastKind::Info), "不可见不该报成错误");

        let (msg, kind) = mk(serde_json::json!({
            "target": "status_tip", "ok": false, "reason": "render_failed"
        }));
        assert_eq!(msg, "截图失败：render_failed");
        assert!(matches!(kind, ToastKind::Error));
    }

    /// 缺字段 / 非整数 / 越界 / 不是 JSON —— 一律 None。
    ///
    /// 关键在于**不能取 0 兜底**：`(0, 0)` 会被当成合法坐标落盘成 custom_x/y，
    /// 候选窗下次就跑到屏幕左上角，而用户只是拖了一下。
    #[test]
    fn rejects_malformed_bodies() {
        for bad in [
            &br#"{"x":1}"#[..],            // 缺 y
            br#"{"y":1}"#,                 // 缺 x
            br#"{"x":1.5,"y":2}"#,         // 非整数
            br#"{"x":"1","y":"2"}"#,       // 字符串
            br#"{"x":99999999999,"y":0}"#, // 越出 i32
            br#"[1,2]"#,                   // 不是对象
            b"not json",
            b"",
        ] {
            assert_eq!(decode_ext_point(bad), None, "body={:?} 应被拒", bad);
        }
    }
}
