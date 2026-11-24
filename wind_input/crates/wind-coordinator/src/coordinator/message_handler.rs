//! `impl MessageHandler for Coordinator`：TSF 桥接层全部事件的入口
//!（按键/焦点/IME 激活/光标/菜单/候选交互/诊断），含失焦归属校验
//! `is_stale_focus_event` 与 ext 信封解码辅助。
//!（coordinator 子模块，自 coordinator.rs 平移，纯搬运。）

use super::*;
// 本模块自己导入，不挂到 `coordinator.rs` 的 use 上：那个文件是多方共改的热点，
// 搭在它的 import 行会让提交与别人的改动落进同一个 hunk。
use wind_config::app_compat::NewlineStyle;
use wind_ipc::protocol::{EVENT_MOD_DOWN, EVENT_MOD_UP, TOGGLE_PASSTHROUGH_KEY};

impl Coordinator {
    /// 光标前最后一段已不是我们上屏的那段（宿主自己出了字、重放了键，或用户挪了光标）：撤销上屏
    /// 计数退化为 1（否则「你好」后打个 0 再撤销删的是「好0」），草稿滑窗断流（否则「你」「0」
    /// 「好」连打会登记「你好」），下一词学习断开（前后两次上屏已不相邻）。
    pub(crate) fn note_host_wrote_text(&self, reason: &str) {
        self.last_commit_len
            .store(1, std::sync::atomic::Ordering::Relaxed);
        self.terminate_auto_phrase(reason);
        self.note_word_boundary();
    }

    /// 仍挂在组合里的智能符号（HoldComposition）。超出时限的视为没有：C++ 定时器到点已自行提交，
    /// 这边的 `held_text` 不会随之清掉，拿它当「还在组合」会二次提交（"。" → 等 >500ms → "=" → "。。="）。
    pub(crate) fn live_held_text(&self) -> Option<String> {
        let arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
        let timeout = self.smart_symbol_timeout();
        if arm.at.is_some_and(|t| t.elapsed() < timeout) {
            arm.held_text.clone()
        } else {
            None
        }
    }

    /// 当前焦点应用的上屏换行档位：per-app（compat `[[commit_newline]]`）→ 全局
    /// （`input.commit_newline`）→ 出厂 [`NewlineStyle::Keep`]。
    ///
    /// 只有 Windows 消费这一项：macOS 的 IMKit 用 LF，跨平台的协调器不该背 Windows 的
    /// 文本约定；Android 同理。非 Windows 上本函数是编译期常量，`apply` 随即走
    /// `Cow::Borrowed` 快路径，运行时零成本。
    #[cfg(windows)]
    pub(crate) fn commit_newline_style(&self) -> NewlineStyle {
        let proc_name = self.active_process_name();
        if let Some(style) = self
            .app_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .commit_newline_for(&proc_name)
        {
            return style;
        }
        // 全局层是唯一做回落的地方：认不出的值按出厂档处理，per-app 层认不出的值
        // 已在 compat 侧退化为「没配」（见 NewlineStyle::from_config 的取舍）。
        NewlineStyle::from_config(&self.rt().config.input.commit_newline).unwrap_or_default()
    }

    /// 见 Windows 版的说明：非 Windows 平台不参与换行改写。
    #[cfg(not(windows))]
    pub(crate) fn commit_newline_style(&self) -> NewlineStyle {
        NewlineStyle::Keep
    }

    /// 按当前档位改写一段最终写入宿主文档的文本。
    ///
    /// 先做「压根没有换行」的短路：逐字上屏是绝对多数，那条路不查表、不加锁、不分配。
    pub(crate) fn convert_commit_newline(&self, text: String) -> String {
        if !text.contains(['\r', '\n']) {
            return text;
        }
        match self.commit_newline_style().apply(&text) {
            std::borrow::Cow::Borrowed(_) => text,
            std::borrow::Cow::Owned(s) => s,
        }
    }

    /// 改写 action 里所有最终写入宿主文档的文本字段。
    ///
    /// 先做「这个 action 压根没有带换行的正文」的短路，与 [`Self::convert_commit_newline`]
    /// 同一理由：本函数挂在每一次按键上（`handle_key_event_policed`），而绝大多数按键
    /// （透传 / 消费 / 方向键 / 逐字上屏）都不带换行。档位求值要取两把锁、做几次字符串
    /// 分配，不能让它们白跑一遍——`apply_newline_style` 里的 `Keep` 短路发生在档位求出
    /// 来之后，挡不住这个开销。
    pub(crate) fn apply_commit_newline(&self, action: KeyAction) -> KeyAction {
        if !commit_text_has_newline(&action) {
            return action;
        }
        apply_newline_style(action, self.commit_newline_style())
    }
}

/// `action` 里是否有带换行的正文。判据必须与 [`apply_newline_style`] 改写的字段集
/// 保持一致：这里少看一个字段，那个字段就永远不会被改写（而且是静默的）。
fn commit_text_has_newline(action: &KeyAction) -> bool {
    let has = |t: &str| t.contains(['\r', '\n']);
    match action {
        KeyAction::InsertText { text, .. }
        | KeyAction::InsertTextWithCursor { text, .. }
        | KeyAction::ReplaceBackward { text, .. }
        | KeyAction::CommitReplacingHeld { text, .. } => has(text),
        KeyAction::CommitAndHoldComposition { commit_text, .. }
        | KeyAction::CommitThenDeferComposition { commit_text, .. } => has(commit_text),
        _ => false,
    }
}

/// 按档位改写 `action` 里所有最终写入宿主文档的文本字段。
///
/// 组合区文本（`UpdateComposition` / `HoldComposition` 的 text、`new_composition`、
/// `deferred_composition`）一律不动：它们不是正文，改了只会让编码栏显示错乱。
///
/// 写成自由函数是为了能在任何平台单测：`commit_newline_style` 在非 Windows 上恒为 `Keep`，
/// 挂在它上面的测试在 Linux / macOS 上全绿却什么都没验到。
pub(crate) fn apply_newline_style(action: KeyAction, style: NewlineStyle) -> KeyAction {
    // 档位是 Keep 时整棵树都不必走：`Keep.apply` 虽然也零拷贝，但这里还能省下逐字段的
    // 解构与重建。
    if style == NewlineStyle::Keep {
        return action;
    }
    let conv = |text: String| -> String {
        if !text.contains(['\r', '\n']) {
            return text;
        }
        match style.apply(&text) {
            std::borrow::Cow::Borrowed(_) => text,
            std::borrow::Cow::Owned(s) => s,
        }
    };
    match action {
        KeyAction::InsertText {
            text,
            new_composition,
            mode_changed,
            chinese_mode,
            has_new_composition,
        } => KeyAction::InsertText {
            text: conv(text),
            new_composition,
            mode_changed,
            chinese_mode,
            has_new_composition,
        },
        KeyAction::InsertTextWithCursor {
            text,
            cursor_offset,
        } => KeyAction::InsertTextWithCursor {
            text: conv(text),
            cursor_offset,
        },
        KeyAction::ReplaceBackward { count, text } => KeyAction::ReplaceBackward {
            count,
            text: conv(text),
        },
        KeyAction::CommitReplacingHeld { text, chinese_mode } => KeyAction::CommitReplacingHeld {
            text: conv(text),
            chinese_mode,
        },
        KeyAction::CommitAndHoldComposition {
            commit_text,
            hold_text,
            timeout_ms,
        } => KeyAction::CommitAndHoldComposition {
            commit_text: conv(commit_text),
            hold_text,
            timeout_ms,
        },
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            timeout_ms,
        } => KeyAction::CommitThenDeferComposition {
            commit_text: conv(commit_text),
            deferred_composition,
            timeout_ms,
        },
        other => other,
    }
}

impl Coordinator {
    /// 失焦类事件的归属校验：`client_token` 不是当前活动客户端时判为陈旧事件并丢弃。
    ///
    /// DLL 的 focus_lost 由 `OnKillThreadFocus` 发出（DocMgr 级失焦噪声太多），比新宿主的
    /// focus_gained 晚到，跨宿主切换时顺序恒为「新宿主 focus_gained → 旧宿主 focus_lost」。
    /// `ime_active` 是全局单例，不校验就会被后者清掉，表现为工具栏闪一下即隐藏
    /// （日志：`UpdateToolbar` 后紧跟 `HideToolbar`）。
    ///
    /// 放行：`client_token == 0`（旧 DLL 不带 token）、`active == 0`（尚无客户端获焦）。
    /// 只挡跨宿主：同进程多个 DocMgr 共用 token，其间抖动由 UI 层的隐藏防抖吸收。
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

/// 宿主报来的屏幕坐标的钳制范围。±2^24 远超任何真实屏幕，又给菜单定位里的「坐标 + 宽高」
/// 留足 i32 余量：任意 i32 原样进去，`popup_menu` 的定位加法在 dev 构建溢出 panic、release 回绕。
#[cfg(ext_presenter)]
const HOST_COORD_LIMIT: i32 = 1 << 24;

#[cfg(ext_presenter)]
fn clamp_host_coord(v: i32) -> i32 {
    v.clamp(-HOST_COORD_LIMIT, HOST_COORD_LIMIT)
}

/// `menu.open` 的 body：`{"target":i32,"x":i32,"y":i32,"work":[左,上,右,下],"lx":i32,"ly":i32}`。
/// 缺 `target` / 坐标即整条不认；`work` 缺省或不成形（含钳制后右 ≤ 左、下 ≤ 上）按「没有
/// 工作区」（全 0）处理——菜单仍能弹，只是不做翻转，越界交给 addon 那边看得见的溢出。
/// `lx`/`ly`（右键点在悬停提示位图内的坐标）只有悬停提示菜单带，缺了按 `None`。
/// 坐标一律钳到 [`HOST_COORD_LIMIT`] 以内。
#[cfg(ext_presenter)]
fn decode_menu_open(body: &[u8]) -> Option<crate::handle_menu::MenuOpenRequest> {
    use crate::handle_menu::MenuOpenRequest;
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let int = |v: &serde_json::Value| v.as_i64().and_then(|n| i32::try_from(n).ok());
    let coord = |v: &serde_json::Value| int(v).map(clamp_host_coord);
    let target = int(v.get("target")?)?;
    let x = coord(v.get("x")?)?;
    let y = coord(v.get("y")?)?;
    let mut work = [0i32; 4];
    if let Some(arr) = v.get("work").and_then(|w| w.as_array())
        && arr.len() == 4
    {
        for (slot, n) in work.iter_mut().zip(arr) {
            *slot = coord(n).unwrap_or(0);
        }
        let [left, top, right, bottom] = work;
        if right <= left || bottom <= top {
            work = [0; 4];
        }
    }
    let local = v.get("lx").and_then(coord).zip(v.get("ly").and_then(coord));
    Some(MenuOpenRequest {
        target,
        x,
        y,
        work,
        local,
    })
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

/// 扩展信封 body 里的 `output`（输出 / 显示器名）。缺字段、非字符串、空串都返回 `None`：
/// 位置记忆按它分桶，空串当键只会多出一个读不回来的桶。
#[cfg(all(target_os = "linux", ext_presenter))]
fn decode_ext_output(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let s = v.get("output")?.as_str()?;
    (!s.is_empty()).then(|| s.to_owned())
}

/// 扩展信封 body 里的 `work`（`[左,上,右,下]`，屏幕坐标）。缺字段、不是四个整数、空矩形都返回 `None`。
#[cfg(all(target_os = "linux", ext_presenter))]
fn decode_ext_work(body: &[u8]) -> Option<(i32, i32, i32, i32)> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let arr = v.get("work")?.as_array()?;
    let mut w = [0i32; 4];
    if arr.len() != 4 {
        return None;
    }
    for (slot, n) in w.iter_mut().zip(arr) {
        *slot = clamp_host_coord(i32::try_from(n.as_i64()?).ok()?);
    }
    let [left, top, right, bottom] = w;
    (right > left && bottom > top).then_some((left, top, right, bottom))
}

/// `host.fullscreen` 的 body `{"fullscreen":bool}`。字段不是布尔就整条不认：把坏值当成「退出全屏」
/// 会让工具栏冒到全屏应用上面。
#[cfg(all(target_os = "linux", ext_presenter))]
fn decode_ext_fullscreen(body: &[u8]) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("fullscreen")?.as_bool()
}

/// `host.display` 的 body：两个字段各自可缺省（缺省 = 不改现值）。
#[derive(Debug, PartialEq)]
struct HostDisplay {
    caret_free: Option<bool>,
    caret_trusted: Option<bool>,
    inline_preedit: Option<bool>,
    scale: Option<f32>,
}

/// 解析 `host.display`。字段类型不对（`"caret_free":1`、`"scale":"2"`）按「缺省」处理而不是整条拒：
/// 两个字段互相独立，一个坏了不该连累另一个；而缺省 = 不改现值，坏值不会把已有状态改回默认
/// （那会重新引入 Wayland 下的 1~2 秒首显延迟）。整个 body 不是 JSON 对象才返回 `None`。
fn decode_ext_host_display(body: &[u8]) -> Option<HostDisplay> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = v.as_object()?;
    Some(HostDisplay {
        caret_free: obj.get("caret_free").and_then(|x| x.as_bool()),
        caret_trusted: obj.get("caret_trusted").and_then(|x| x.as_bool()),
        inline_preedit: obj.get("inline_preedit").and_then(|x| x.as_bool()),
        scale: obj
            .get("scale")
            .and_then(|x| x.as_f64())
            .map(|x| x as f32)
            .filter(|x| x.is_finite() && *x > 0.0),
    })
}

/// `diag.host_render_placed` → (是否升 WARN, 日志行)。body 不是 JSON 对象返回 `None`。
///
/// 只有 recheck 且旧窗 `requested` 与新窗不同才升 WARN：那才是「建窗时 band 判错、候选
/// 曾被宿主界面盖住」。只缺 owner / 窗口被连带销毁也会 recheck，但 band 本来是对的，
/// 打成 WARN 会稀释检索价值。
fn host_render_placed_log(body: &[u8]) -> Option<(bool, String)> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = v.as_object()?;
    let num = |k: &str| obj.get(k).and_then(|x| x.as_u64());
    let flag = |k: &str| obj.get(k).and_then(|x| x.as_bool());
    let show = |x: Option<u64>| x.map_or_else(|| "?".to_owned(), |n| n.to_string());
    let trigger = obj.get("trigger").and_then(|x| x.as_str()).unwrap_or("?");
    let mut line = format!(
        "host_render placed: pid={} trigger={trigger} probed={} requested={} actual={} owner={}",
        show(num("pid")),
        show(num("probed")),
        show(num("requested")),
        show(num("actual")),
        flag("owner").map_or("?", |b| if b { "true" } else { "false" }),
    );
    // 候选窗的皮肤穿透装饰窗口建没建出来（旧 DLL 不报这一项）。
    if let Some(pass) = flag("pass") {
        line.push_str(if pass {
            " pass=true"
        } else {
            " pass=false（穿透装饰合回主层、不能点穿）"
        });
    }
    if trigger != "recheck" {
        return Some((false, line));
    }
    let band_was_wrong = num("prev_requested") != num("requested");
    line.push_str(&format!(
        " prev_probed={} prev_requested={} prev_actual={} prev_owner={}",
        show(num("prev_probed")),
        show(num("prev_requested")),
        show(num("prev_actual")),
        flag("prev_owner").map_or("?", |b| if b { "true" } else { "false" }),
    ));
    line.push_str(if band_was_wrong {
        "（建窗时 band 判错，候选曾被宿主界面盖住，已按前台重建）"
    } else if flag("prev_owner") == Some(false) {
        "（band 无误，建窗时缺 owner，已补）"
    } else {
        "（候选窗句柄已失效，已重建）"
    });
    Some((band_was_wrong, line))
}

impl MessageHandler for Coordinator {
    /// 见 trait 文档：新连接建立时的兜底刷新。服务重启时宿主若已在前台，管道重连只续发
    /// `caret_update`、不会再有 `FOCUS_GAINED` 触发 `update_active_compat`，per-app 规则
    /// 会整个会话停在默认值。
    ///
    /// 只在该 pid 是当前前台窗口时才写 `active_compat`，免得后台宿主重连覆盖聚焦应用的规则
    /// （见 `foreground_pid` 文档）。非 Windows 上本回调不会被调用，仅为满足 trait 签名。
    fn handle_client_connected(&self, pid: u32) {
        #[cfg(windows)]
        {
            // 必须先校正名字再刷规则：下一步是缓存优先的，缓存错了它照抄。新进程必连一次，
            // 这是 pid_names 唯一的自愈时机（见 revalidate_pid_name）。
            self.revalidate_pid_name(pid, &crate::coordinator::process_name(pid));
            self.apply_connected_pid_compat(pid, crate::foreground_pid());
        }
        // 新连接 = 新的 DLL 实例，它会在首次 BeginUIElement 时重报接管状态；
        // 先把 pid 复用可能残留的旧账清掉。
        self.clear_uielement_host_pid(pid);
        #[cfg(not(windows))]
        let _ = pid;
    }

    fn handle_menu_command(&self, command: &str) -> Option<StatusUpdateData> {
        info!("Menu command: {}", command);
        match command {
            // 菜单/托盘无按键上下文：切换前那串编码经 push 出口交还宿主（有文本则上屏，
            // 否则清掉宿主里残留的组合），与热键路径同一策略。
            "toggle_mode" => {
                let (status, commit) = self.toggle_mode_with_commit();
                self.push_switch_commit(&commit);
                status
            }
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
            // 用户手动切标点：不需要额外记「手动值」——schema_scope_gen 守卫本身就实现了
            // 「本代际内手动值胜出」（代际未变时 sync_schema_scope 直接返回，不会来覆盖）。
            // 见 crate::schema_scope 模块文档最后一节。
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
                let commit = self.cycle_schema();
                self.push_switch_commit(&commit);
                Some(self.build_status())
            }
            // 两个方向共用一条路：互斥保证住在 `toggle_conversion_direction` 里，
            // 各写一份必然漂移（其中一份忘了关掉对面就是两个方向同时开）。
            "toggle_s2t" | "toggle_t2s" => {
                if self.try_toggle_conversion(command == "toggle_s2t") {
                    self.show_status();
                }
                Some(self.build_status())
            }
            _ => None,
        }
    }

    /// macOS `.app` 查询功能主菜单：构建菜单树并编码为 `CmdMenuShow` 帧字节。
    /// Windows 走进程内 `show_main_menu` 渲染，不用此路径（返回空帧亦无害）。
    fn query_menu_encoded(&self, simplified: bool) -> Vec<u8> {
        #[cfg(ext_presenter)]
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
        #[cfg(not(ext_presenter))]
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
    /// 语义 = 上下方向键调整高亮项（`move_up`/`move_down`），到页边界自然翻到相邻页，
    /// 不是整页翻动；两平台共用。不加配置项：滚动候选框就是要动高亮，没有第二种合理解释。
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
            // 展开态滚一行（同 `mouse_page`）：网格里逐个移动高亮，滚一格只挪一个字，太慢。
            let moved = if Self::grid_active(&state) {
                self.grid_scroll(&mut state, !up)
            } else if up {
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

    /// 扩展信封（`CMD_EXT`）：低频消息统一入口。未知 kind 安静忽略——旧服务收到新
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
            // Linux addon：工具栏 / 软键盘本地拖完的落点（内容盒左上，屏幕坐标）与落点输出名。
            #[cfg(all(target_os = "linux", ext_presenter))]
            ext_kind::POS_TOOLBAR | ext_kind::POS_SOFTKEYBOARD => {
                use wind_ipc::protocol::overlay::{
                    OVERLAY_KIND_SOFTKEYBOARD, OVERLAY_KIND_TOOLBAR,
                };
                let Some((x, y)) = decode_ext_point(body) else {
                    tracing::warn!("扩展消息 {kind} 的 body 不是 {{x,y}}，忽略");
                    return;
                };
                let panel = if kind == ext_kind::POS_TOOLBAR {
                    OVERLAY_KIND_TOOLBAR
                } else {
                    OVERLAY_KIND_SOFTKEYBOARD
                };
                self.on_host_panel_dropped(panel, x, y, decode_ext_output(body));
            }
            // Linux addon：当前焦点宿主的显示环境（光标坐标是否可用、界面缩放）。
            ext_kind::HOST_DISPLAY => match decode_ext_host_display(body) {
                Some(d) => {
                    if let Some(free) = d.caret_free {
                        self.set_caret_independent(free);
                    }
                    if let Some(trusted) = d.caret_trusted {
                        self.set_host_caret_trusted(trusted);
                    }
                    if let Some(inline) = d.inline_preedit {
                        self.set_host_inline_preedit(inline);
                    }
                    // wind-ui 只在 desktop-ui（桌面形态）下才是依赖；headless / Android 没有它
                    #[cfg(all(feature = "desktop-ui", not(windows), not(target_os = "macos")))]
                    if let Some(scale) = d.scale {
                        wind_ui::dpi::set_host_scale(scale);
                    }
                    // 能力声明 `panels`：能贴工具栏 / 软键盘浮层（旧 addon 不报，见字段 `host_panels`）。
                    // 缺省同其它字段 = 不改现值；复位靠请求连接断开。
                    #[cfg(all(target_os = "linux", ext_presenter))]
                    if let Some(panels) = serde_json::from_slice::<serde_json::Value>(body)
                        .ok()
                        .and_then(|v| v.get("panels")?.as_bool())
                    {
                        self.host_panels
                            .store(panels, std::sync::atomic::Ordering::Relaxed);
                    }
                    // 焦点输出名与其工作区：工具栏 / 软键盘位置记忆的分桶键与默认落点。两者成对：
                    // 报了其一，工作区就跟着这次报告换（只报输出名 = 工作区未知），免得新输出配上旧输出的
                    // 工作区；输出名只在报了时更新；都没报 = 不改现值。
                    #[cfg(all(target_os = "linux", ext_presenter))]
                    {
                        let output = decode_ext_output(body);
                        let work = decode_ext_work(body);
                        if output.is_some() || work.is_some() {
                            tracing::debug!("host.display: output={output:?} work={work:?}");
                            if output.is_some() {
                                *self.host_output.lock().unwrap_or_else(|e| e.into_inner()) =
                                    output;
                            }
                            *self.host_work.lock().unwrap_or_else(|e| e.into_inner()) = work;
                        }
                    }
                    tracing::debug!("host.display: {d:?}");
                }
                None => tracing::warn!("host.display 载荷无法解析，忽略"),
            },
            // Linux addon：焦点窗口进出全屏。全屏期间不显示工具栏，判据在渲染端（对位 Windows 的
            // `blocked_by_exclusive_fullscreen`）。
            #[cfg(all(target_os = "linux", ext_presenter))]
            ext_kind::HOST_FULLSCREEN => match decode_ext_fullscreen(body) {
                Some(on) => {
                    tracing::debug!("host.fullscreen: {on}");
                    let _ = self
                        .ui_tx
                        .send(wind_ui_types::UiCommand::SetHostFullscreen(on));
                }
                None => tracing::warn!("host.fullscreen 载荷无法解析，忽略"),
            },
            // macOS `.app` / Linux addon：系统「减少动态效果」，连上时与系统设置变化时报。
            ext_kind::HOST_REDUCE_MOTION => {
                let reduce = serde_json::from_slice::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| v.get("reduce")?.as_bool());
                match reduce {
                    #[cfg(all(feature = "desktop-ui", ext_presenter))]
                    Some(r) => {
                        tracing::debug!("host.reduce_motion: {r}");
                        wind_ui::candidate_window::set_host_reduce_motion(r);
                    }
                    #[cfg(not(all(feature = "desktop-ui", ext_presenter)))]
                    Some(_) => {}
                    None => tracing::warn!("host.reduce_motion 载荷无法解析，忽略"),
                }
            }
            // Linux addon / macOS `.app`：候选窗皮肤装饰上点了一下（判定在宿主，同 Windows）。
            #[cfg(ext_presenter)]
            ext_kind::SKIN_DECOR_CLICK => match serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .and_then(|v| v.get("index")?.as_u64())
            {
                Some(i) => {
                    let _ = self
                        .ui_tx
                        .send(wind_ui_types::UiCommand::SkinDecorClick(i as usize));
                }
                None => tracing::warn!("skin.decor_click 载荷无法解析，忽略"),
            },
            // Windows TSF：host render 候选窗落位诊断（SearchHost 写不了 tsf_log，只能看这条）。
            ext_kind::DIAG_HOST_RENDER_PLACED => match host_render_placed_log(body) {
                Some((true, line)) => tracing::warn!("{line}"),
                Some((false, line)) => tracing::info!("{line}"),
                None => tracing::warn!("diag.host_render_placed 载荷无法解析，忽略"),
            },
            // Linux addon：右键候选 / 候选窗空白处 / Fcitx5 状态区入口请求打开自绘菜单。
            #[cfg(ext_presenter)]
            ext_kind::MENU_OPEN => match decode_menu_open(body) {
                Some(req) => self.open_menu_from_host(req),
                None => tracing::warn!("menu.open 载荷无法解析，忽略"),
            },
            // Linux addon：菜单被 addon 自己收掉了（超时 / 失焦 / 服务重启 / 抓不住指针…）。
            #[cfg(ext_presenter)]
            ext_kind::MENU_DISMISS => {
                let why = serde_json::from_slice::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| v.get("reason")?.as_str().map(str::to_owned))
                    .unwrap_or_default();
                self.menu_dismissed_by_host(&why);
            }
            _ => tracing::debug!("未处理的扩展消息 kind={kind}"),
        }
    }

    /// Linux addon 报来的菜单指针事件（按键号：1 左 / 2 中 / 3 右）。菜单已关还收到事件 =
    /// 两端认识错开（addon 还挂着上一帧），让 UI 收菜单、补推隐藏帧。
    fn handle_menu_pointer(&self, event: u32, button: u32, x: i32, y: i32) {
        #[cfg(ext_presenter)]
        {
            use wind_ipc::protocol::menu_pointer::*;
            use wind_ui_types::{MenuPointerEvent as E, UiCommand};
            if !self.is_menu_open() {
                let _ = self.ui_tx.send(UiCommand::HideMenu);
                return;
            }
            let ev = match (event, button) {
                (MENU_POINTER_MOTION, _) => E::Move,
                (MENU_POINTER_PRESS, 1) => E::LeftPress,
                (MENU_POINTER_PRESS, 3) => E::RightPress,
                (MENU_POINTER_PRESS, _) => E::OtherPress,
                _ => return,
            };
            self.touch_menu();
            // 坐标同 `menu.open` 钳制：命中测试里也有「坐标 + 宽高」。
            let _ = self.ui_tx.send(UiCommand::MenuPointer {
                event: ev,
                x: clamp_host_coord(x),
                y: clamp_host_coord(y),
            });
        }
        #[cfg(not(ext_presenter))]
        let _ = (event, button, x, y);
    }

    /// Linux addon 报来的工具栏 / 软键盘指针事件：原样交给渲染端，由它翻译成 `WM_*` 喂给窗口
    /// 自己的鼠标状态机（点格子、右键菜单、按住连发都在那一侧）。其它平台没有这条消息，沿用默认实现。
    #[cfg(all(target_os = "linux", ext_presenter))]
    fn on_panel_pointer(&self, kind: u32, event: u32, button: u32, x: i32, y: i32) {
        let _ = self.ui_tx.send(wind_ui_types::UiCommand::PanelPointer {
            kind,
            event,
            button,
            x,
            y,
        });
    }

    /// Linux：addon 的请求连接断了。菜单若开着，没人会再报关闭——就地复位，免得下一个
    /// addon 实例（fcitx5 重启）送来的方向键 / 回车 / Esc 被一个不存在的菜单吞掉。
    ///
    /// 宿主上报状态的统一复位在渲染端的 `HostReconnected`（push 连接重建，见 manager_macos
    /// `reset_host_state`）。这里只先补一次面板离开，让 addon 崩溃时按住的软键盘键当场停止连发，
    /// 不必等新 addon 连上；面板能力声明（`host_panels`）也在这里复位。全屏不在这里复位——请求连接
    /// 超时熔断也会断开，addon 其实还在。
    fn handle_client_disconnected(&self) {
        #[cfg(ext_presenter)]
        if self.is_menu_open() {
            self.menu_dismissed_by_host("addon 连接断开");
        }
        #[cfg(all(target_os = "linux", ext_presenter))]
        {
            // 面板能力随 addon 实例走：下一个连上来的可能是不认面板的旧 addon（重启 fcitx5 换了
            // 版本），得它自己重新声明。还活着的 addon 在恢复连接时会重报焦点（含 host.display）。
            // 不挂在 push 重连钩子上：服务启动时钩子补跑晚于 addon 首报，会把刚声明的冲掉。
            self.host_panels
                .store(false, std::sync::atomic::Ordering::Relaxed);
            use wind_ipc::protocol::overlay::{OVERLAY_KIND_SOFTKEYBOARD, OVERLAY_KIND_TOOLBAR};
            use wind_ipc::protocol::panel_pointer::PANEL_POINTER_LEAVE;
            for kind in [OVERLAY_KIND_TOOLBAR, OVERLAY_KIND_SOFTKEYBOARD] {
                self.on_panel_pointer(kind, PANEL_POINTER_LEAVE, 0, 0, 0);
            }
        }
    }

    fn handle_show_context_menu(&self, x: i32, y: i32) {
        // 自绘菜单（popup_menu.rs / ShowCandidateMenu·MenuKey·HideMenu UiCommand）在 Windows 由本
        // 命令弹出；外部宿主（Linux addon / macOS `.app`）走 `menu.open` 扩展信封。macOS 上 IMK
        // 频繁调 `menu()` → Swift 发 CMD_SHOW_CONTEXT_MENU 仅为「查询菜单项」（系统输入源菜单是原生
        // NSMenu），若在此调 show_main_menu 会把协调器置 menu_open=true 并经 forward_menu_key 吞掉
        // 后续按键，而此时没有任何自绘菜单窗口、永不回 MenuClose → 输入被卡死 (打字无响应)。
        #[cfg(not(ext_presenter))]
        self.show_main_menu(wind_ui_types::MenuAnchor::at_point(x, y));
        #[cfg(ext_presenter)]
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
        // 暂停学习同样不记（理由见 `record_commit_ks`）。
        if !cfg.enabled || !cfg.track_english || self.learning_suspended() {
            return;
        }
        if chars == 0 && digits == 0 && puncts == 0 && spaces == 0 {
            return;
        }
        let total = chars
            .saturating_add(digits)
            .saturating_add(puncts)
            .saturating_add(spaces);
        collector.record(StatEvent {
            timestamp: chrono::Local::now(),
            chinese: 0,
            english: chars,
            punct: puncts,
            other: digits.saturating_add(spaces),
            code_len: 0,
            candidate_pos: -1,
            // 英文直输是 1 字符 1 击键。这里必须显式给出，否则一批 ≤4 字符的上报会被
            // 「短码出长词」规则误判封顶——那规则针对的是中文短码，与英文无关。
            keystrokes: total,
            schema_id: self.active_schema_id(),
            source: CommitSource::TsfDirect,
        });
    }

    fn assoc_session_hint(&self) -> bool {
        self.assoc_bare_session_now()
    }

    fn composition_placeholder(&self) -> &'static str {
        // 按焦点应用的 compat 规则取（空格 / ZWSP / U+2800），唯一实现在 push_config.rs。
        Coordinator::composition_placeholder(self)
    }

    fn syllable_separator(&self) -> Option<String> {
        let active_none = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .is_none();
        self.syllable_separator_for(active_none)
    }

    fn preedit_uses_placeholder(&self) -> bool {
        // 非 app_inline（候选窗自显 preedit）→ 应用侧用占位空格，不重复显示编码。
        //
        // 取有效归属（[`Coordinator::preedit_in_app_effective`]）而非配置原值：候选窗被宿主压住
        // （UI-less 全屏游戏 / D3D 独占）时它不画，宿主自绘的 `UiElementPage` 又不带编码串，
        // 再占位编码就完全看不见。见 `handle_uielement.rs::a_suppressed_host_gets_the_real_code_inline`。
        //
        // `host_render` 与 `hide_candidate_window` 刻意不算压制：前者候选窗照画，嵌入会重复；
        // 后者是用户第二次显式选择，「关窗 + 编码归候选窗」本身就是盲打语境。后果虽与压制态
        // 相同也别并进来，见 `handle_uielement.rs::the_user_hiding_the_window_keeps_the_placeholder`。
        //
        // 只看 preedit 显示模式，别叠 `[input.caret]` 的逐模式开关：出厂 app_inline 下那些开关
        // 会全部失效，且这是全局出口，会改写所有路径的 preedit 形态。开关在 `enter_*` 里各自
        // 决定发什么（见 `enter_add_word_mode`）。
        !self.preedit_in_app_effective()
    }

    /// bridge 真正入口：在按键处理之上统一埋点输入统计（上屏文本字符数），
    /// 再做 preedit 占位后处理。集中在此避免修改 40+ 个 commit 返回点（对齐旧 Go
    /// HandleKeyEvent 末尾的 recordCommitFallback 思路）。
    fn handle_key_event_policed(&self, data: &KeyEventData) -> KeyAction {
        // 上一按之后、这一按之前有键被透传给宿主 ⇒ 智能符号武装态失效。英文半角下 TSF 只吃
        // 标点键，中间的字母数字不产生按键事件，出口的「非同键解除武装」与 `smart_symbol_press2`
        // 的「缓冲非空」都看不见，只有 DLL 知道，经 `TOGGLE_PASSTHROUGH_KEY` 上报
        // （见 C++ `_NotePassthroughKeyDown`）。
        //
        // 必须在 `handle_key_event` 之前处理：放到出口就晚了一拍，press2 已误触发。
        //
        // 同一事实也说明光标前那段已不是我们上屏的，据此复位撤销计数与草稿滑窗（见
        // `note_host_wrote_text`）。中文半角空闲的数字键 DLL 不转发（NumberKeyPolicy.h），
        // 这是服务端唯一知道它们的途径。位为假时连锁都不取。
        if data.event_type == EVENT_KEY_DOWN && data.toggles & TOGGLE_PASSTHROUGH_KEY != 0 {
            {
                let mut arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
                if arm.armed {
                    debug!("SmartSymbol: 上一按之后有键被透传给宿主，解除武装");
                    arm.armed = false;
                    arm.hold_pending_commit = false;
                }
            }
            self.note_host_wrote_text("宿主自行出字");
        }
        // 中英 / 中数间补空格的「插入点前一字符」：只在组合开始前锁存（见 `crate::auto_spacing`）。
        // 联想态不重新锁存：联想是紧接着上一笔上屏的，服务端已把 `spacing_prev` 推进到那一笔
        // 的末字；客户端那份在某些上屏路径上会慢一拍（上屏后立刻起组合的那几条），拿来覆盖反而错。
        if data.event_type == EVENT_KEY_DOWN {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !Self::has_input_session(&st) {
                self.latch_spacing_boundary(&mut st, data);
            }
        }
        let action = self.handle_key_event(data);
        let action = self.redispatch_commit_code(data, action);
        // 平台中立的收尾：补空格、换行改写、输入统计、自提交打点与造词投喂、检索放宽复原、
        // 配对保活。收口在这里而不是各上屏点，理由相同：上屏路径 40+ 个返回点，约 10 处绕过
        // `commit_action` 直接构造 InsertText，散点接线必漏；换行形式是平台/宿主的表达约定，
        // 属服务端职责（A3-3）。各步的取舍与其余来源对照见 `session_exit::exit_steps`。
        //
        // 它与下面两段智能符号收尾互不读写，先后无关。配对保活须在 `handle_key_event` 之后
        // 刷新，否则本次按键的陈旧判定会先被自己刷新掉。
        let action = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.finish_session_action(st, crate::session_exit::ActionOrigin::Key, action)
        };
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
        // 中间按了别的键 ⇒ 智能符号武装态失效：press1 之后只该等同一个键的第二次按下。
        // 上面那段按 `held_text.is_some()` 开门，对出厂 `DeleteReplace`（`held_text` 恒 None）不起作用。
        //
        // 比的是本次按键产出的标点字符而非虚拟键码：武装侧记的是字符
        // （`arm_smart_symbol_after_commit` 拿不到键码）。
        //
        // 只解除 `armed`、不碰 `held_text`：后者归上面那段按 action 清，这里清掉会让下一个标点的
        // `pre_held_text` 捡不到仍在组合态的旧符号，造成二次提交。标点分支里的
        // `disarm_smart_symbol()` 连 `held_text` 一起清不矛盾：那些点上 `held_text` 已为空。
        //
        // 只在 keydown 上做：修饰键的 keyup 会转发到服务端（C++ `_DispatchPendingToggleKeyUp`），
        // 在 keyup 上解除会误伤「按住 Shift 连按两次 `？`」。
        //
        // 只覆盖桌面宿主；`wind-mobile` 不经这里，由 `smart_symbol_press2` 的「press1 后又打了编码」
        // 兜住。两边各有用例，见 `tests/smart_symbol.rs` 的同名注释。
        if data.event_type == EVENT_KEY_DOWN {
            let mut arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
            if arm.armed {
                // `punct_char` 只认主键盘；英文全角经 `full_width_source_char` 连小键盘符号也能武装，
                // 不补 `numpad_char` 会在 press1 当场自解武装。
                // 带 Ctrl/Alt/Win 一律算别的键：`Ctrl+.` 不是在打标点，宿主可能已挪走光标。
                // 与 C++ `_IsCustomEnglishPunctKey` 的同款守卫对齐。
                let same_key = data.modifiers & MOD_SHORTCUT == 0
                    && punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
                        .or_else(|| numpad_char(data.key_code))
                        .is_some_and(|ch| ch == arm.key);
                if !same_key {
                    arm.armed = false;
                    arm.hold_pending_commit = false;
                }
            }
        }
        // 联想占位组合的两道收口，都挂在这个唯一出口上：
        // ① `assoc_release_on_passthrough`：联想还活着、这一键落到透传（Del / Home / 左右…），
        //    收组合并交还按键；
        // ② `adopt_orphaned_placeholder`：联想已因自动隐藏退出，组合成了孤儿。
        //
        // 先①后②：①返回的 `ClearCompositionThenPassThrough` 属 `Fate::Absorbs`，②会顺手撤掉残留的
        // 孤儿标记；反过来①就看不到透传。目前两者互斥，对调测试不会红，顺序是防将来的第三条路径。
        //
        // 须在占位后处理之前：会话层判定先于显示层加工。只在 keydown 改判：交还按键靠 C++ 在
        // `OnKeyDown` / `DispatchHotkey` 重放，keyup 上改判会消费掉标记却不重放。
        let action = if data.event_type == EVENT_KEY_DOWN {
            let action = self.assoc_release_on_passthrough(data, action);
            self.adopt_orphaned_placeholder(action)
        } else {
            action
        };
        // 本键交还宿主重放（联想态的退格 / 回车 / 方向键等）：同开头透传位那段，光标前已不是我们
        // 上屏的那段。普通宿主的重放键在 Test 里被 Suppress、不置透传位，不能等下一按的位。
        // 必须排在上面两道改判之后：Del / Home / 左右这些是在 `assoc_release_on_passthrough`
        // 里才改成重放的。
        if matches!(action, KeyAction::ClearCompositionThenPassThrough) {
            self.note_host_wrote_text("交还宿主重放");
        }
        // 无占位联想（`none`）的会话位对账。内层 `handle_key_event` 返回时的推送早于上面两道改判，
        // 看不到改判里退出联想（Del / 方向键 / Ctrl+C 落透传）的结果，缓存会停在 TRUE。
        // 联想活着的每个 keydown 还强推一次：DLL 对「迟到的 FALSE」只置待收，后到的 TRUE
        // 把它撤销，服务端的真实状态才收敛得回来。只对配了 `none` 的应用，别处零开销。
        if data.event_type == EVENT_KEY_DOWN && self.assoc_placeholder_is_none() {
            if self.assoc_bare_session_now() {
                self.hotkey_session_force_push
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.push_hotkey_session_if_changed();
        }
        // 扩展事件：取最终结果（上面两道改判之后），占位替换只是显示层加工，不影响它。
        self.hook_after_key(data, &action);
        if self.preedit_uses_placeholder() {
            action.with_composition_placeholder(self.composition_placeholder())
        } else if let Some(sep) = self.syllable_separator() {
            action.with_syllable_separator(&sep)
        } else {
            action
        }
    }

    fn handle_key_event(&self, data: &KeyEventData) -> KeyAction {
        // 〔K1 修饰键原始上报〕
        // 候选显示时如实上报的 Ctrl 按下 / 松开：服务端判单击，判出来再以原样的松开重进本函数
        // （所以排在一切之前，也不经下面的统计 / 打字节奏 / 软键盘推送）。
        if matches!(data.event_type, EVENT_MOD_DOWN | EVENT_MOD_UP) {
            return self.handle_ctrl_raw(data);
        }
        // 软键盘状态若在本次按键中变了，返回前推给 C++（吃键判定要用）。声明在最开头
        // 是必需的：局部变量逆序析构 ⇒ 它最后走 ⇒ 那时 `state` 的 MutexGuard 已经还了。
        let _sk_push = crate::handle_softkeyboard::SoftKeyboardPushOnDrop(self);
        // 〔K2 组字中 Alt：注释态〕
        // 组字中的 Alt 按下 / 松开（平台只在组字中转发）：只管注释态，吃不吃都在这里定。
        if keymap::is_alt_vk(data.key_code)
            && matches!(data.event_type, EVENT_KEY_DOWN | EVENT_KEY_UP)
        {
            if data.event_type == EVENT_KEY_DOWN {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                crate::comment_switch::on_other_ctrl_key(&mut st.ctrl_track);
            }
            return self.handle_comment_alt(data);
        }
        // 〔K3 注释功能键按住〕
        // 按住档的功能键切换键：按下（含连发）与松开都在这里接走，排在下面「按住期间的别的键」之前。
        if let Some(act) = self.handle_comment_fn_hold(data) {
            return act;
        }
        // 按住切换键期间的别的键：照常处理完再收起详细态。声明位置的理由同 `_sk_push`。
        // 同时弄脏 Alt / Ctrl 的单击判定（含 `WM_HOTKEY` 送来的 Ctrl+数字）。
        let _hold_off = if data.event_type == EVENT_KEY_DOWN {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            crate::comment_switch::on_other_key(&mut st.alt_track);
            crate::comment_switch::on_other_ctrl_key(&mut st.ctrl_track);
            st.comment_hold_flip
                .then(|| crate::comment::HoldOffOnDrop(self))
        } else {
            None
        };
        // 〔K4 每键复位与观测：统计/会话标志复位、方案行为同步、小键盘归一、打字节奏、CapsLock 校准（不返回）〕
        // 每次按键开始重置统计标志：具体上屏路径调 record_commit 置位，
        // 顶层 record_input_stats 仅在未置位时兜底（对齐 Go handle_key_event 开头 reset）。
        self.stat_recorded
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // 不经 `handle_key_event_policed` 的调用方（移动端）没有重新分派这一步，
        // 别让上一键的登记漏到这一键。
        self.commit_code_pending
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // 按下时已没有输入会话（或只剩联想）= 上一次组合已经结束（出口有几十处），「本次展开过」
        // 随之作废。不看「候选为空」：码表空码、拼音分步上屏后删空余码时组合还在。
        if data.event_type == EVENT_KEY_DOWN {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !Self::has_input_session(&st) || st.assoc_active() {
                st.grid_keep = false;
            }
            // 注释翻转只跟候选窗走：联想窗里翻的要留着（联想入口另行清掉上一次的）。
            if !Self::has_input_session(&st) {
                st.clear_comment_flips();
                st.alt_track = None;
            }
        }

        // 方案级行为覆盖的兜底同步点：代际未变时是一次 atomic load + 比较，可忽略。
        // 它保证「切方案后 [punct] 没落地」最迟在下一次按键收敛——其余三个调用点
        // （push_state_update / show_status / apply_ui_config）只是让它当场可见。
        // 见 crate::schema_scope 模块文档。
        self.sync_schema_scope_locked();

        // ── 小键盘归一化（numpad_behavior = follow_main）──
        // 「同主键盘区数字」的语义 = 小键盘键就是主键盘键，故在此改写键码后交由既有主键盘
        // 逻辑接管，一处生效于所有模式。置于最前（仅晚于统计复位）：模式分派、热键、英文
        // 直通等所有后续判断都应看到归一化后的键。direct 时不改写，各模式走自己的 numpad 臂。
        //
        // 来源标记必须在改写之前取：follow_main 改完键码后就再也分不出这键来自小键盘，
        // 而 `input.numpad_half_width` 要在出字那一步知道。写进 `state` 见下方加锁处
        // （此处还没拿到锁）。
        let numpad_origin = numpad_to_main(data.key_code).is_some();
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
        // 记录相邻两次按键的间隔，供 fast 档判断连续输入（见 handle_caret_probe）。
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

        // 〔K5 key_up：切换键与修饰键选词〕
        // ── key_up：toggle 模式键（Shift/Ctrl/CapsLock）直接切换 ──
        // 关键：TSF 对 toggle 键会"吃掉 keydown 不转发"，仅在 C++ 侧判定为干净单击后
        // 于 keyUp 转发该键事件（_SendKeyToService(..., KEY_EVENT_UP)）。因此服务端
        // 收到 toggle 键的 keyUp 即应直接切换，无需 keydown/pending（对齐 Go HandleKeyEvent）。
        if data.event_type == EVENT_KEY_UP {
            // 修饰键作二三候选键（select_key_groups 含 lrshift / lrctrl）先于下面一切，会话态绑定
            // 也排在选词之后。同一键多身份都配了时裁决是「有候选选词、无候选切换」。无候选/越界
            // 时返回 None 落到下面各分支。`capslock = "select_candidate:3"` 也是合法配置。
            //
            // CapsLock 的闸门：钩子已装时 keyup 能走到这里，说明钩子那一刻没吃这个键，系统已翻转
            // 锁定态且撤不回，再执行会话动作就是和系统抢（翻页与大写同时发生）。`SHOULD_EAT` 与
            // 按键之间隔着 IPC 往返，窗口无法消除，故吃与不吃只由钩子裁决，服务端服从。
            // 钩子未装时保留 keyup 这条退路。判据是纯函数 `Coordinator::key_up_session_action_allowed`，
            // 以便在 Linux CI 上测。
            if Coordinator::key_up_session_action_allowed(
                data.key_code,
                self.capslock_hook_installed(),
            ) {
                if let Some(act) = self.handle_select_key_up(data) {
                    return act;
                }
                // 会话态绑定里的 keyup-only 键（`capslock = "page_prev"` 那类）。
                //
                // 必须先于下面 CapsLock 的状态同步分支：那条会调 `take_input_on_mode_switch`
                // 把正在打的编码上屏或丢弃，配了 CapsLock 翻页的用户每翻一页就丢一次输入。
                //
                // 无候选时本函数返回 None，键照常落到下面的原有处理（CapsLock 仍切大小写、
                // 修饰键仍切中英文）。「有会话归绑定、无会话归原语义」正是两张表的分野。
                if let Some(act) = self.handle_session_action_key_up(data) {
                    return act;
                }
            }
            // CapsLock 单独处理：C++ 侧总是发送此 key_up（不经 key_up_tsf_hashes 过滤），
            // 故须先于 is_toggle_mode_keycode 检查。同步真实大写锁定状态，不翻转 chinese_mode
            // （对齐 Go handleCapsLockStateNoLock：capsLockOn 跟随 data.toggles & 0x01）。
            if data.key_code == 0x14
            /* VK_CAPITAL */
            {
                let caps_lock_on = (data.toggles & 0x01) != 0;
                debug!("CapsLock state notification: on={}", caps_lock_on);
                // 配了会话绑定的用户按 CapsLock 不是为了切大写。走到这里说明钩子没拦住、系统已翻了
                // 锁定态，但不该再连带把正在打的编码上屏/丢弃；只同步镜像（`cancel_on_mode_switch`
                // 等判据依赖它）。判据复用装钩子用的 `capslock_bound()`，不另写一份以免漂移。
                if self.capslock_bound() {
                    {
                        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        s.caps_lock = caps_lock_on;
                    }
                    self.push_state_update();
                    self.show_status();
                    self.notify_toolbar();
                    return KeyAction::StatusUpdate(self.build_status());
                }
                let had_pending = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    !s.input_buffer.is_empty()
                        || !s.committed_text.is_empty()
                        || !s.candidates.is_empty()
                };
                let commit_text = {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    // 切大写时按"切英文"语义处理待输入（commit_on_switch）；切回小写时直接丢弃。
                    let upper = caps_lock_on && self.rt().config.input.capslock.commit_upper;
                    let text = self.take_input_on_mode_switch_cased(&mut s, !caps_lock_on, upper);
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
            // 先于 is_toggle_mode_keycode：同一个键两处都配时，方案级是更具体的声明，
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
                // EndComposition，清掉应用里残留的编码（StatusUpdate 不结束 composition）；
                // mode_changed 同时更新中英图标。
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
            } else if let Some(act) = self.handle_comment_ctrl_up(data) {
                // 注释切换键（Ctrl 单击）排在以上各功能之后：撞键时它们优先。
                return act;
            }
            return KeyAction::PassThrough;
        }
        if data.event_type != EVENT_KEY_DOWN {
            return KeyAction::PassThrough;
        }

        // 〔K6 右键菜单接键〕
        // ── 右键菜单打开时：方向键/回车/ESC 由菜单消费（优先于一切）──
        // 菜单由服务自绘的两种形态：Windows 进程内窗口、光栅帧交宿主（macOS `.app` / Linux
        // addon）贴图（同一份 `popup_menu`）。宿主形态下 menu_open 一旦被置真而窗口没了，输入会被
        // 永久吞键卡死，各条关闭路径与自愈见 docs/design/linux-port.md §7。
        if self.is_menu_open() && self.forward_menu_key(data.key_code) {
            return KeyAction::Consumed;
        }

        // ── key_down 热键匹配 ──
        // 规范化修饰位：TSF 转发的 modifiers 可能含 L/R 具体位，而 key_down 热键以
        // 通用位（ctrl/shift/alt/win）注册，故先掩掉具体位再比对 match_hash。
        let norm_mods = data.modifiers & hotkey::MOD_GENERIC_MASK;
        let norm_hash = calc_key_hash(norm_mods, data.key_code);
        // 〔K7 key_down 全局热键〕
        if let Some(action) = self.rt().compiled_hotkeys.match_key_down(norm_hash)
            && !action.is_empty()
            // 裸 F 键在英文模式归宿主（TSF 靠策略位不吃；mac / Linux 整体转发，在这里挡）。
            && !(hotkey::bare_function_key_needs_chinese(data.key_code, norm_mods, action)
                && !self.is_chinese_mode())
            // 组字中，会话表里绑了同一个裸 F 键（如出厂注释切换键 F1）时，会话优先：
            // 全局 `key_actions` 里的 F 键绑定只管空闲态。
            && !(hotkey::is_bare_function_key(data.key_code, norm_mods)
                && self.has_active_session()
                && self
                    .session_action_for(data.key_code, data.modifiers & MOD_SHIFT != 0, true)
                    .is_some())
        {
            debug!(
                "Hotkey matched (key_down): {} (0x{:08X})",
                action, norm_hash
            );
            let action = action.to_string();
            // 与 DLL 侧「乐观置位」对称的记账点：C++ 在 `WM_HOTKEY` 收到任何 GLOBAL 热键都把
            // `_hotkeyModeSession` 置 TRUE（分不出动作），协调器不知道这一位。若协调器自己的
            // 会话态前后都是 false，`push_hotkey_session_if_changed` 判为没变不推，DLL 停在 TRUE，
            // 此后 Enter/Esc/Backspace 被吃了再吐而丢键（如 `open_add_word_dialog`）。
            // 故命中过就无条件推一次，别指望别的推送顺带纠正。
            self.hotkey_session_force_push
                .store(true, std::sync::atomic::Ordering::Relaxed);
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
            } else if let Some(act) = self.dispatch_bound_action_hotkey(&action) {
                // 其余动词统一按 `BoundAction` 分派——组合键与单键、修饰键同一个值域。
                // 热键上下文专有的三条（key_code=0 哨兵 / chinese_mode 守卫 / 幂等）
                // 都在那个函数里，见其文档。
                return act;
            }
        }

        // 〔K8 候选期快捷键组（守卫不成立返回 None）〕
        // ── 候选词操作热键（Ctrl+数字 置顶/删除）──
        // 这两组在编译期仅注册转发（action 为空，上方匹配不触发），实际语义在此分派。
        // 须先于下方 Ctrl/Alt 组合「清空隐藏候选」分支，否则 Ctrl+数字 会被当作普通组合吞掉。
        // 快捷输入的选词键排在置顶/删除前面：出厂同为 Ctrl+数字，快捷输入里归选词。
        if let Some(act) = self.try_quick_input_select(data) {
            return act;
        }
        if let Some(act) = self.handle_candidate_action_hotkey(data) {
            return act;
        }

        // ── 上屏注释 / 拼音（`input.alt_commit`：Alt+数字 / Alt+空格）──
        // 与上一段同层、同理由：候选热键，须先于下方 Ctrl/Alt 组合的兜底清组合分支。
        if let Some(act) = self.try_alt_commit(data, numpad_origin) {
            return act;
        }

        // ── 英文候选大小写档位循环（`input.english_case_cycle_key`）──
        // 与上一段同处「候选窗显示期间生效的快捷键」这一层，理由也相同：五个模式一次接通。
        // 守卫（配了键 / 按的就是那个键 / 英文语境 / 有候选）都在函数内部，任一不成立即返回
        // None，按键原样落回它本来的语义。CapsLock 那份走全局钩子，不经此处。
        if let Some(act) = self.try_english_case_cycle_key(data) {
            return act;
        }

        // ── 整句切换（`schema.pinyin.sentence_cycle_key`）──
        // 同上一段的层级与理由。守卫（配了键 / 普通拼音输入 / 整句池 ≥ 2 条）都在函数内部，
        // 任一不成立即返回 None —— Tab 出厂是高亮下移键，池子不足两条时必须原样落回。
        if let Some(act) = self.try_sentence_cycle_key(data) {
            return act;
        }

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // 无条件写入（含 false）：只在为真时置位会把上一次按键的来源留给这一次，
        // 表现为「先按一下小键盘、再按主键盘同一个键也出半角」。见 `State::numpad_origin`。
        state.numpad_origin = numpad_origin;

        // 〔K9 快捷加词模式〕
        // 快捷加词模式：消费全部按键（↑↓调词长/Enter确认/Esc退出），先于英文透传与单点分派。
        if state.add_word_active {
            return self.handle_add_word_key(&mut state, data);
        }

        // 〔K10 密码框透传〕
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

        // 〔K11 软键盘〕
        // 软键盘：接管主键区的符号键位（查表直接上屏）。特殊键与布局外的键在那里返回
        // None，自然落回下面的常规链路 → 透传给宿主，于是「退格还能删、回车还换行」不需
        // 要单独写一条规则。
        //
        // 位置在密码框抑制之后：密码框里一律透传，软键盘也不例外。
        if let Some(act) = self.handle_softkeyboard_key(&state, data) {
            return act;
        }

        // 〔K12 配对跳出〕
        // 配对跳出键：全模式统一前置判定，必须早于下面的英文模式分支——英文模式对普通键
        // 直接 PassThrough，放在中文路径里就永远跑不到。守卫与失效方向见 try_jump_out。
        if let Some(act) = self.try_jump_out(&state, data) {
            return act;
        }

        // 〔K13 英文模式分水岭〕
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
            // 未配的键 handle 返回 None → 落到下方透传。
            if data.modifiers & MOD_SHORTCUT == 0
                && let Some(act) = self.handle_english_custom_punct(&mut state, data)
            {
                return act;
            }
            // 半角英文：透传，宿主自然出字（保留 WM_KEYDOWN 原生语义）。
            return KeyAction::PassThrough;
        }

        // 〔K14 CapsLock 大写〕
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
                // 它们不在 printable_char 覆盖内，落到下方 PassThrough 就丢键。
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
            } else if state.input_buffer.is_empty()
                && state.candidates.is_empty()
                && let Some(act) = self.handle_english_custom_punct(&mut state, data)
            {
                // 半角：与英文半角同一出口。DLL 对英文配对键 / 英半自定义键开了透传例外
                // （`_IsEnglishHalfEatenPunctKey`）吃下转发，此处必须出字 + 配对，否则
                // 「中文 + 大写」下配对静默失效。
                return act;
            }
            return KeyAction::PassThrough;
        }

        // 〔K15 夺取回退〕
        // 统一夺取回退：夺取式模式（URL/后续 z 临拼）中，退到夺取边界再按退格 →
        // 撤销夺取、把快照回放回正常码表输入流（而非停在无候选的独占模式）。
        // 须先于下方单点分派，否则退格会被模式处理器按普通删字符消费。
        if data.key_code == keymap::VK_BACK && self.can_rewind(&state) {
            return self.rewind_hijack(&mut state);
        }

        // 〔K16 独占模式分派〕
        // 已激活独占模式：单点分派到专用处理器（唯一入口，见 pipeline.rs）。
        match state.active {
            Some(ModeKind::TempPinyin) => return self.handle_temp_pinyin_key(&mut state, data),
            Some(ModeKind::TempEnglish) => return self.handle_temp_english_key(&mut state, data),
            Some(ModeKind::Url) => return self.handle_url_key(&mut state, data),
            Some(ModeKind::Email) => return self.handle_email_key(&mut state, data),
            Some(ModeKind::Unicode) => return self.handle_unicode_key(&mut state, data),
            // 生僻字模式复用 special 的整套按键处理，别另写一份（两份迟早分叉）。两者只差
            // 引擎方案与生僻准入，分别在 `overlay_engine_schema` 与 `update_special_candidates`
            // 里分流。反查模式同族：候选构建在 `update_special_candidates` 开头分流到 `handle_reverse`。
            Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) | Some(ModeKind::Reverse) => {
                return self.handle_special_key(&mut state, data);
            }
            Some(ModeKind::Mix(_)) => return self.handle_mix_key(&mut state, data),
            Some(ModeKind::AuxCode) => return self.handle_aux_code_key(&mut state, data),
            None => {}
        }

        // 〔K17 方案级锁外状态切换〕
        // 方案级表的 A 类状态切换（`backslash = "toggle_punct"` 这类）。
        //
        // 与紧随其后的 `try_activate_mode` 分属两半：B 类建 overlay、要 `&mut State`，
        // 故在锁内；A/C 类只改全局状态，目标函数（dispatch_hotkey / toggle_schema_by_id）
        // 各自加锁，必须锁外执行——判定在这里做完，guard 就地 drop 掉。
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

        // 〔K18 空缓冲模式激活〕
        // 空缓冲模式激活：单一入口，优先级链见 try_activate_mode（对齐 key-pipeline.md §2.1）。
        if let Some(act) = self.try_activate_mode(&mut state, data) {
            return act;
        }

        // Ctrl+←/→：整音节跳（rime `left_by_syllable` / `right_by_syllable`）。
        // 会话键表只认 vk+shift、不带 Ctrl，所以这里单独认；用户要关就写
        // `session_actions = { "ctrl+left" = "none" }`（同时也不再向宿主转发该键）。
        // 只在组码中（有编码）吃：无编码时 Ctrl+←/→ 是宿主的按词移动。
        if data.modifiers & (MOD_SHORTCUT | MOD_SHIFT) == wind_ipc::protocol::MOD_CTRL
            && matches!(data.key_code, keymap::VK_LEFT | keymap::VK_RIGHT)
            && !state.input_buffer.is_empty()
            && self.ctrl_syllable_enabled(data.key_code)
        {
            let moved =
                Self::move_caret_syllable(&mut state, data.key_code == keymap::VK_RIGHT, false);
            return if moved {
                self.caret_moved_action(&mut state)
            } else {
                KeyAction::Consumed
            };
        }

        // Ctrl/Alt/Cmd 组合（非热键）：有输入则清空并隐藏候选窗，否则透传。
        // 必须 notify_ui_hide：否则候选窗残留（如 Ctrl+A 时卡死，需再输入才复位）。
        //
        // 显式声明「收组合后交还当前键」，避免宿主靠组合状态猜测归属。
        if data.modifiers & MOD_SHORTCUT != 0 {
            if !state.input_buffer.is_empty() || !state.committed_text.is_empty() {
                self.reset_pinyin_composition(&mut state);
                self.notify_ui_hide();
                return KeyAction::ClearCompositionThenPassThrough;
            }
            return KeyAction::PassThrough;
        }

        // 〔K19 前缀夺取〕
        // ── 前缀夺取式模式激活 ──
        // 普通输入累积时，若 input_buffer + 当前键字符 恰好等于某前缀，则夺取进入对应模式。
        // 置于主分派前，确保「补全前缀的那一键」先被截获，不落入普通码表/标点处理。
        if let Some(act) = self.try_prefix_hijack(&mut state, data) {
            return act;
        }

        debug!(
            // prev_char 必须在这里露出来：它是「数字后智能标点」与智能符号 press2 判定的
            // 唯一文档侧输入，客户端不填时两处都静默走另一条分支，别处日志看不出来。
            "key_event: code=0x{:02X} mods=0x{:04X} chinese={} full={} caps={} prev_char=0x{:04X} buf='{}'",
            data.key_code,
            data.modifiers,
            state.chinese_mode,
            state.full_width,
            state.caps_lock,
            data.prev_char,
            state.input_buffer
        );

        // 〔K20 编码字符闸门 / 符号缓冲闸门〕
        // 非字母码元闸门：本方案把某个数字/符号配成了码元（如 `a-z0-9` 要打 `Win10`、
        // `a-x/` 要打含 `/` 的词条）→ 进缓冲，抢在以词定字/翻页/数字选词/标点流水线之前。
        //
        // 位置即契约（见 docs/design/codetable-input-chars.md「组码中码元优先，空缓冲让位」）：
        // 置于模式激活与 URL 夺取之后，故空缓冲下的引导键、临拼/临英触发键、URL 前缀
        // 一概不受影响；置于下方各闸门之前，故组码中这些键归码表而非选词/翻页。
        //
        // 空缓冲时闸门查的是首码集：数字默认不在其中，数字键照常选词/透传。
        // 默认码元集 a-z 不含非字母字符，恒不命中。
        if let Some(act) = self.try_code_char_gate(&mut state, data) {
            return act;
        }

        // 组码中符号入缓冲（`input.buffer_symbol_chars`，出厂只有 `-`）：让 `sun-panel`
        // 这类带连字符的英文打得出来。码元闸门无条件夺取，本闸门只捡该键此刻空着的那一格
        // （判据在 `symbol_buffer_key_free`），故必须排在它之后：同一字符两边都配时真码元优先。
        //
        // 出厂 `-` 只接管「有候选 + 没翻过页」那一格；无候选那格刻意不碰，那里 `-` 要落标点臂
        // 甩掉废码并吃全角与自定义标点映射（见 `symbol_buffer_key_free` 的最后一问）。
        if let Some(act) = self.try_symbol_buffer_gate(&mut state, data) {
            return act;
        }

        // 〔K21 以词定字〕
        // 以词定字（select_char）：配置的成对标点键从当前高亮候选词逐字上屏（对齐 Go
        // handleEngineDefault——select_char 优先于翻页键，故置于 apply_session_action 之前）。默认
        // `select_char_keys` 为空 → select_char_index 恒 None → 跳过。仅在缓冲非空或
        // 有候选时拦截；空缓冲且无候选时放行，让 `,`/`.` 作普通标点（对齐 Go 空缓冲回退标点）。
        //
        // 这是标点键的第三条通路，在标点臂之前就 return。拦截条件只用以词定字自己的适用条件
        // （有候选才有字源）；没候选就放行到标点臂，由 `input.punct_on_empty_behavior` 处置。
        // 别把标点策略写进这个条件：那会让空码下 `,` `.` 的行为取决于以词定字开没开。
        // `keys.overflow.select_char_key` 只管真正的越界（字数不够、联想态无 `input_buffer`）。
        //
        // 放行安全的依据（改前请重核）：到标点臂之间的 `apply_session_action`（这几个键只查到
        // `select_char:N`，返回 None）、`numpad_char`、`try_z_fallback`（要求 z 开头且破前缀）
        // 在空码下都拦不到这几个键。
        if data.modifiers & MOD_SHIFT == 0
            && !state.candidates.is_empty()
            && let Some(char_index) = self.select_char_index(data.key_code)
        {
            return self.handle_select_char_with_overflow(
                &mut state,
                char_index,
                data.key_code,
                data.prev_char,
            );
        }

        // 〔K22 会话动作（导航/翻页/取消…）〕
        // 候选翻页/高亮：配置驱动统一处理（普通模式为码表型，`-`/`=` 可作翻页）。
        // 仅有候选时生效；无候选时下方 match 的回退臂负责透传方向/翻页键。
        if let Some(act) = self.apply_session_action(&mut state, data, true) {
            return act;
        }

        // 〔K23 小键盘 direct〕
        // 数字小键盘 —— direct（默认）：IME 不把该键解释为选词，但已打的码不丢：先顶屏当前
        // 高亮候选（含逐步转换的已转换前缀），再接着输出该小键盘字符。
        // follow_main 时键已在 handle_key_event 入口归一化为主键盘等价键，永不到达此处。
        if let Some(npc) = numpad_char(data.key_code) {
            let has_comp = !state.input_buffer.is_empty()
                || !state.committed_text.is_empty()
                || !state.candidates.is_empty();
            // 空闲 + 半角：透传，由宿主自己出字——与主键盘数字空缓冲臂（`VK_1..=VK_9`）同一语义，
            // 判据也同源（联想态挂在 `candidates` 上，算有组合）。
            //
            // 不能由我们 InsertText 出字：C++ OnTestKeyDown 判「不吃」，而 Chrome 类宿主无视 test
            // 结论照调 OnKeyDown，宿主出一次、我们再插一次即双重上屏。新 DLL 共用
            // `NumberKeyPolicy.h` 不再转发，本臂与下面 follow_main 那条留给旧 DLL 与这类宿主。
            //
            // 全角态必须照旧出字：C++ 的 `chinese_fullwidth_number` 分支那时会吃这批键，
            // 透传就成了「吃了再吐」。`numpad_half_width` 开着的全角态也一样（吃键与否 C++
            // 只看全角，不看这个开关），出的半角字由下方 `commit_highlight_then_char` 负责。
            if !has_comp && !state.full_width {
                self.record_commit(&npc.to_string(), 0, -1, CommitSource::Punctuation);
                // 汉字后的小键盘数字、开了中数间空格：同主键盘那条臂，出字补空格。
                if npc.is_ascii_digit() && self.digit_after_cjk_outputs(&state, data) {
                    return Self::commit_action(npc.to_string(), true);
                }
                return KeyAction::PassThrough;
            }
            // 命令候选顶屏 → 执行命令（与按空格一致），不上屏 display 标签、不追加该字符。
            if let Some(act) = self.top_commit_command_guard(&mut state) {
                return act;
            }
            return self.commit_highlight_then_char(&mut state, npc, has_comp);
        }
        // 同一条闸的 follow_main 版：键已归一成主键盘键，上面那臂认不出来，运算符会落进标点臂
        // 出「。」等（数字有主键盘空缓冲臂兜着）。C++ 的 Test 只按小键盘 Number 类判、不知道
        // 本档位，空闲半角照样放行 ⇒ 普通宿主只出 `.`，Chrome 类宿主再加我们插的「。」。
        // 这里与 Test 对齐，空闲半角一律透传。
        //
        // 比上面那臂多判一个挂着的智能符号：HoldComposition 挂着组合时 C++ 视作有会话、已吃下
        // 本键，透传就是「吃了再吐」；交给标点臂，它会把挂着的符号一并上屏。超时的不算——C++ 定时器
        // 已自行提交、没有会话了，而 `held_text` 在这边不会随之清掉。
        if state.numpad_origin
            && !state.full_width
            && state.input_buffer.is_empty()
            && state.committed_text.is_empty()
            && state.candidates.is_empty()
            && self.live_held_text().is_none()
            && let Some(ch) = punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
        {
            self.record_commit(&ch.to_string(), 0, -1, CommitSource::Punctuation);
            // 汉字后的小键盘数字、开了中数间空格：同主键盘数字那条臂出字补空格（DLL 已吃下）。
            if ch.is_ascii_digit() && self.digit_after_cjk_outputs(&state, data) {
                return Self::commit_action(ch.to_string(), true);
            }
            return KeyAction::PassThrough;
        }

        // 〔K24 z-fallback 夺取〕
        // ── z-fallback 夺取：必须早于下面的按键分派 ──
        //
        // 缓冲以 z 开头、加上这一键后 `z…` 破活码前缀 ⇒ 首 z 实为引导键，抛弃它、
        // 残余码切进目标模式（见 `try_z_fallback`，内含全部门禁：码表引擎 / z 有绑定 /
        // 目标接得住这个字符 / 破前缀）。
        //
        // 放在 match 之前而不是各臂里：数字键在缓冲非空时是选词键、符号走标点流水线，都会当场
        // 消费掉键，挂在臂里就轮不到（如 `z = "mix:quick_mix"` 进了快捷输入却打不了数字）。
        // 单点接入，免得三条通路漏接一条。
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

        // 〔K25 默认按键臂〕
        match data.key_code {
            // Escape：取消整个组合（含已转换前缀），不上屏。实现收口在 `cancel_session`
            // ——`keys.session_actions` 里绑 `cancel` 的键走的是同一个函数，两条通路
            // 行为必然一致。
            //
            // 没有会话（也没挂着待定标点）时交还宿主：Linux / macOS 没有 TSF 那道前置闸门，
            // 每个键都会到这里，空闲 Esc 若照样回 ClearComposition 就被吞掉。
            keymap::VK_ESCAPE
                if !Self::has_input_session(&state) && self.live_held_text().is_none() =>
            {
                KeyAction::PassThrough
            }
            keymap::VK_ESCAPE => self.cancel_session(&mut state),
            keymap::VK_BACK => {
                // 联想态：收掉候选并结束占位组合。必须先于下面的既有分支——那些分支
                // 在「缓冲空 + 无已转换段」时给 `PassThrough`，而联想态挂着占位组合
                // （见 `Coordinator::assoc_composition`），裸透传会把组合悬在宿主里。
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
                // 段回退优先于光标（不看光标位置，对齐 Go handleBackspace 的分支顺序）。
                if let Some(act) = self.collapse_expanded_group(&mut state) {
                    act
                } else if !state.committed_segs.is_empty() {
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
                // 所以是个配置；但它只在联想态有意义，正常输入的空格恒是选高亮。
                if state.assoc_active() && !self.assoc_config().space_commits {
                    self.exit_assoc(&mut state, crate::handle_assoc::AssocExit::NonSelectKey);
                    self.notify_ui_hide();
                    let text = self.convert_punct(&state, ' ', data.prev_char);
                    self.record_commit(&text, 0, -1, CommitSource::Punctuation);
                    return Self::commit_action(text, true);
                }
                // Space：选当前高亮候选 / 上屏编码。有候选那一支与会话态动词
                // `commit_highlighted` 同一个出口（见 `commit_highlighted`）。
                if let Some(act) = self.commit_highlighted(&mut state) {
                    act
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
                    // 上屏的是用户所打的形态：Shift+字母的大写存在影子串里，缓冲恒小写。
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
                    let raw_text = format!("{}{}", prefix, raw_code);
                    let mut text = self.maybe_convert(&state, &raw_text);
                    // 原码类上屏也进上屏历史（转换前形态、不含下面补的空格，同回车）。
                    self.push_commit_history(&raw_text);
                    // 英文方案上屏原码 = 词库里没有的原文：英文自动造词的信号。
                    if prefix.is_empty() && self.engine_mgr.active_is_english() {
                        self.learn_english_raw(&self.engine_mgr.active_schema_id(), &raw_code);
                    }
                    // 英文补空格（`schema.english.commit_space`）：上屏的是输入缓冲原码，无候选可依，
                    // 故用语境口径 `english_space_enabled_in`（临英与英文方案是两份开关）。与选中候选
                    // 补空格一致。下方 VK_RETURN 分支与本块同形但刻意不补，改这里别顺手改那边。
                    if self.english_space_enabled_in(&state) {
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
                    //
                    // DLL 据 `space_custom_cols` 吃下的空格则必须出字：并集掩码可能比当前方案
                    // 宽，产物仍是 " " 时透传就成了吃了再吐。
                    let text = self.convert_punct(&state, ' ', data.prev_char);
                    if text == " " && !self.idle_space_eaten(&state) {
                        return KeyAction::PassThrough;
                    }
                    self.record_commit(&text, 0, -1, CommitSource::Punctuation);
                    Self::commit_action(text, true)
                }
            }
            keymap::VK_RETURN => {
                // 联想态：收窗并结束占位组合，默认连同把回车交还宿主
                // （`enter_cancels_only`，见 `assoc_enter`）。
                //
                // 下方各分支的门槛都是「缓冲或已转换前缀非空」，联想两者皆空 ⇒ 会落到最后的
                // `PassThrough`，而那只交还键、不收组合，占位组合会悬在宿主里（同退格）。
                //
                // 刻意不上屏高亮联想：回车是终结性动作，用户按它是要换行/发送，
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
                    // 上屏的是用户所打的形态：Shift+字母的大写存在影子串里，缓冲恒小写。
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
                    // 本块与上方 VK_SPACE 空码分支同形，唯一差别是不补英文空格
                    // （`schema.english.commit_space`）：回车是终结性动作，刻意不对称。
                    let raw_text = format!("{}{}", prefix, raw_code);
                    let text = self.maybe_convert(&state, &raw_text);
                    // 原码类上屏也进上屏历史（`;` 重复上屏取得到）。记转换前形态（与选词
                    // 出口一致）：重复上屏时会再过一次简繁转换。原码不是词库词，不记词频。
                    self.push_commit_history(&raw_text);
                    // 英文方案回车上屏原码：英文自动造词的信号（回车是英文里最常用的「上屏原文」）。
                    if prefix.is_empty() && self.engine_mgr.active_is_english() {
                        self.learn_english_raw(&self.engine_mgr.active_schema_id(), &raw_code);
                    }
                    state.input_buffer.clear();
                    state.input_buffer_cased.clear();
                    state.candidates.clear();
                    self.notify_ui_hide();
                    Self::commit_action(text, true)
                } else {
                    KeyAction::PassThrough
                }
            }
            // 移动端英文方案组码中：数字是词的一部分（mp3、h2o、win10），进编码而不是选词。
            // 判据见 `handle_assoc::digit_enters_english_buffer`；桌面与中文方案不进此臂，数字照旧选词。
            keymap::VK_0..=keymap::VK_9
                if data.modifiers & MOD_SHIFT == 0
                    && crate::handle_assoc::digit_enters_english_buffer(
                        self.profile.digit_enters_english_buffer,
                        !state.input_buffer.is_empty(),
                        self.engine_mgr.active_is_english(),
                    ) =>
            {
                let digit = (b'0' + (data.key_code - keymap::VK_0) as u8) as char;
                self.accumulate_code_char(&mut state, digit, digit)
            }
            // 汉字后打数字、开了中数间空格：出字，前缀空格由统一出口补（判据见
            // `digit_after_cjk_outputs`；小键盘 direct 那条臂共用）。
            keymap::VK_0..=keymap::VK_9 if self.digit_after_cjk_outputs(&state, data) => {
                let digit = (b'0' + (data.key_code - keymap::VK_0) as u8) as char;
                let text = digit.to_string();
                self.record_commit(&text, 0, -1, CommitSource::Punctuation);
                Self::commit_action(text, true)
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
                    // 分支），故必须出字——透传会「吃了再吐」→ 严格 TSF 宿主丢键、宽松宿主出半角。
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
            // 空闲半角的 0：与上面 1-9 同一透传。C++ Test 对无会话半角数字不吃，若由我们 InsertText，
            // 照调 OnKeyDown 的 Chrome 类宿主会出两个 0。全角不进此臂，落兜底流水线出 ０（C++ 会吃键）。
            keymap::VK_0
                if data.modifiers & MOD_SHIFT == 0
                    && !state.full_width
                    && state.candidates.is_empty()
                    && state.input_buffer.is_empty()
                    && state.committed_text.is_empty() =>
            {
                self.record_commit("0", 0, -1, CommitSource::Punctuation);
                KeyAction::PassThrough
            }
            keymap::VK_0
                if data.modifiers & MOD_SHIFT == 0
                    && !(state.candidates.is_empty()
                        && state.input_buffer.is_empty()
                        && state.committed_text.is_empty()) =>
            {
                // 数字键 0 选当前页第 10 个候选（对齐通行约定 0=第10；越界按
                // overflow.number_key 处理）。follow_main 归一化后小键盘 0 走此臂，与主键盘一致。
                // 空闲的 0 不进此臂（guard 排除）：半角由上一臂透传，全角落兜底标点流水线出全角 ０。
                self.handle_number_key_select(&mut state, 10)
            }
            keymap::VK_A..=keymap::VK_Z => {
                // 会话态选词键里的字母（当前值域只有 z）：有候选时先选词，再谈组码。必须拦在这里：
                // 字母当场进缓冲，流不到 `_` 臂的选词消费点；`apply_session_action` 对
                // `SelectCandidate` 刻意返回 `None`。
                //
                // 候选不足时落回正常组码，不套 `keys.overflow.select_key`：字母的字符就是编码，
                // 套 `commit` 档会吞掉字母上屏别的字（与 `handle_select_key_up` 结论相反：修饰键
                // 没有字符可输出）。联想态让出选词键（`select_keys_commit = false`）时也落回组码。
                if !state.candidates.is_empty()
                    && !self.assoc_yields_select_key(&state)
                    && let Some(offset) = self.select_key_offset(data.key_code)
                {
                    let (start, end) = self.select_range(&state);
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
                // 非码元字母（如 `input_chars = "a-x"` 下的 y/z）：不进缓冲，终结组合并出字。
                // 必须在 z-fallback（match 之前）与 try_activate_mode 之后：z 常同时是非码元与
                // 临拼触发键，先判非码元临拼就进不去。默认码元集 a-z 下恒不命中。
                //
                // 字母通配键（spec §3.3）放在码元判定之前：五笔配 `a-y` 时 `z` 不是码元，组码中
                // 仍要能作通配。让位时落回下面的码元判定。
                if self.wildcard_enters(&state, ch) {
                    return self.accumulate_code_char(&mut state, ch, raw);
                }
                if !self.can_enter_buffer(&state, ch) {
                    return self.reject_non_code_char(&mut state, raw);
                }
                self.accumulate_code_char(&mut state, ch, raw)
            }
            keymap::VK_LEFT | keymap::VK_RIGHT | keymap::VK_HOME | keymap::VK_END => {
                // 编码区光标移动逻辑。←/→ 按 rime 导航器语义：若光标在音节边界上则逐音节移动，
                // 在音节中间按则单字符；无切分情况（双拼 / 单音节 / 码表单码）退回单字符。
                //
                // 三态对齐 Go handleCursorLeft/Right/Home/End：无组合时透传，宿主照常
                // 移动文档光标；有剩余编码时在编码区内移动；已在边界或只剩只读的已转换
                // 前缀时吞键，避免宿主光标跳出组合区。
                //
                // 左右键若被配成翻页/高亮等会话动作，或被展开网格接管，上面的
                // `apply_session_action` 已先行拦截，走不到这里。
                //
                // 移动后默认只刷新 caret：光标不参与引擎查询，候选与 preedit 文本均不变，
                // 但自绘编码栏要据新 caret 重画插入符，所以仍须 `notify_ui_update`。
                // `cursor_by_syllable` 开关打开时例外：光标在中间时候选只查光标前一段
                // （`query_cut`），移动后必须重算。
                if state.input_buffer.is_empty() {
                    if state.committed_text.is_empty() {
                        KeyAction::PassThrough
                    } else {
                        KeyAction::Consumed
                    }
                } else {
                    let st = &mut *state;
                    let smart = (data.modifiers & MOD_SHIFT == 0
                        && matches!(data.key_code, keymap::VK_LEFT | keymap::VK_RIGHT))
                    .then(|| {
                        Self::move_caret_syllable(st, data.key_code == keymap::VK_RIGHT, true)
                    });
                    let moved = if let Some(moved) = smart {
                        moved
                    } else {
                        let mut ed = preedit_cursor::BufEdit::new(
                            &mut st.input_buffer,
                            &mut st.input_cursor_pos,
                        );
                        match data.key_code {
                            keymap::VK_LEFT => ed.move_left(),
                            keymap::VK_RIGHT => ed.move_right(),
                            keymap::VK_HOME => ed.home(),
                            _ => ed.end(),
                        }
                    };
                    if moved {
                        if crate::handle_candidate::cursor_by_syllable() {
                            self.update_candidates(&mut state);
                        }
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
                    && (self.manual_separator_key(data.key_code)
                        || self.english_phrase_separator_key(data.key_code)) =>
            {
                // 拼音手动音节分隔符：把 `'` 压入缓冲作硬边界（引擎按 `'` 强制切分、查询前剥除、
                // preedit 原样保留含末尾 `'`）。走与字母键一致的候选刷新路径。
                // 置于选词/标点分派（`_` 臂）之前：分隔符模式下该键优先作分隔符而非三选键——
                // auto 模式仅在 `'` 未被占作选择键时才拦截 `'`（见 manual_separator_key）。
                //
                // 英文词组分词符走同一条路：同样是「把 `'` 压进缓冲、照常刷候选」，
                // 区别只在谁来放行。两个谓词或在一起而不是合成一个，理由见
                // `english_phrase_separator_key` 的文档（避让 vs 夺取是两条相反策略）。
                // 反引号不参与英文分词——该谓词只认 VK_QUOTE。
                {
                    let st = &mut *state;
                    preedit_cursor::BufEdit::new(&mut st.input_buffer, &mut st.input_cursor_pos)
                        .insert('\'');
                }
                match self.update_candidates(&mut state) {
                    InputOutcome::AutoCommit(text) => {
                        // 记账码取首候选（按来源分流，见 `freq_code`），与上一处 AutoCommit 同口径。
                        let (source, code, freq_text) = state
                            .candidates
                            .first()
                            .map(|c| {
                                let code = self.freq_code(&state.input_buffer, c);
                                (c.source, code, c.freq_text().to_string())
                            })
                            .unwrap_or_else(|| {
                                let buf = state.input_buffer.clone();
                                (CandidateSource::default(), buf, text.clone())
                            });
                        // `6a-draft`：满码自动上屏 / 空码顶出同样算「用过」→ 草稿转正。
                        // 与 `coordinator.rs` 那处 AutoCommit 同口径，判据见 `commit_top_text`。
                        self.promote_draft_on_auto_commit(&state, &text, &code);
                        let out = self
                            .commit_candidate(&mut state, &text, &freq_text, None, source, &code);
                        // 满码自动上屏同样要接联想，出口与手动选词一致。
                        return self.auto_commit_then_assoc(&mut state, out, &text);
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
                    // 双拼韵母键（如 `;` = ing）到不了这里：已由上游码元闸门 `try_code_char_gate`
                    // 接管进缓冲（拼音引擎的 `input_chars` 从双拼布局推导）。别在这里加局部避让，
                    // 它后面还有模式引导键与标点流水线两条通路。
                    //
                    // 联想态让出选词键（`input.association.select_keys_commit = false`）：
                    // 整段跳过（不选、也不记越界），该键按「联想窗不在」往下走——模式引导照常
                    // 进模式，否则落下方标点流水线，那里先收联想再出标点。
                    let mut select_overflow: Option<char> = None;
                    if !self.assoc_yields_select_key(&state)
                        && let Some(offset) = self.select_key_offset(data.key_code)
                    {
                        let (start, end) = self.select_range(&state);
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
                    // 这是进模式的第二条通路（顶字 + 进模式），与空缓冲的 `try_activate_mode` 并列。
                    // 两条必须接同一个裁决，否则方案里写的 `none` 只挡得住一条。
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
                    // 联想态：先把联想整个收掉，再按空闲态出标点（联想不顶屏）。
                    //
                    // 必须早于下面的智能符号：其 press1 在「无输入」时短路返回 `HoldComposition`，
                    // 联想候选会留在 `candidates` 里，此后光标上报被当成组合期间而锁住组合起点。
                    // 普通出口虽清候选，也漏清编码栏标识与自动隐藏计时。
                    let was_assoc =
                        self.exit_assoc(&mut state, crate::handle_assoc::AssocExit::TopCommitKey);
                    if was_assoc {
                        self.notify_ui_hide();
                    }
                    // 快照 held_text：非参与集合的标点会在 try_smart_symbol_replace 中解除武装
                    // 并清空 held_text，须在此前保存，以便下方普通标点流程将旧符号纳入 CommitText。
                    // 加超时防护：若 arm.at 已超出 timeout，说明 C++ timer 已自然触发提交，
                    // held_text 已过期——不再使用，防止二次提交（"。" → 等待 >500ms → "=" → "。。="）。
                    let pre_held_text = self.live_held_text();
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
                            // 英文方案恒允许：`punct_commit` 是码表「标点顶字」的方案属性，
                            // 英文词后接标点是最基本的用法，不该被码表的出厂 false 吞掉。
                            Some(
                                wind_engine::EngineType::Pinyin | wind_engine::EngineType::English,
                            ) => true,
                            // 码表/混输：读有效码表配置（全局 schema.codetable + 方案 override）。
                            _ => self.engine_mgr.codetable_settings().punct_commit,
                        };
                        if !punct_commit {
                            // 标点被吞、符号从未上屏，而本次按键已在 `try_smart_symbol_replace` 里武装成
                            // press1；不解除，下次按同键会去删一个从未上屏的符号。本行目前没有独立会红的
                            // 用例（其他解除点先兜住了），留着是为维持「武装态不得指向从未上屏的符号」。
                            self.disarm_smart_symbol();
                            return KeyAction::Consumed;
                        }
                        // 空码 + `punct_on_empty_behavior = "clear_no_input"`：废码丢弃，标点
                        // 本身也不产出——整个按键当没按过。
                        //
                        // 短路点刻意放在 `hold_info` 之前：标点有两条独立的上屏通路（智能符号
                        // `CommitAndHoldComposition` 与普通标点出口），放这里一处覆盖两条。
                        // 也必须早于标点流水线：走到那里再丢弃，会记一次从未上屏的标点并压进配对栈。
                        if self.punct_empty_code_policy(&state)
                            == PunctEmptyCodePolicy::ClearNoInput
                        {
                            self.reset_pinyin_composition(&mut state);
                            // 标点不上屏，而本次按键可能已被武装成 press1，须解除（理由同上）。
                            self.disarm_smart_symbol();
                            self.notify_ui_hide();
                            // 上一个智能符号若还挂在组合态（`pre_held_text`），直接 ClearComposition
                            // 会连它一起收掉，故用一次提交把它落实。提交文本必须是空串：符号由宿主端
                            // 吸收进前缀（理由见下方普通标点出口），服务端再填就是双写。记账仍要记。
                            if let Some(held) = pre_held_text {
                                self.record_commit(&held, 0, -1, CommitSource::Punctuation);
                                return Self::commit_action(String::new(), true);
                            }
                            return KeyAction::ClearComposition;
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
                                // 标点被命令吃掉、从未上屏，而本次按键已武装成 press1，命令又清空了
                                // 缓冲，press2 的两道判据都会放行。同 `!punct_commit` / `clear_no_input`。
                                self.disarm_smart_symbol();
                                return act;
                            }
                            // 空码丢弃（`punct_on_empty_behavior = "clear"`）：与下方普通标点
                            // 出口同判据、同语义。本分支是智能符号专属的独立上屏通路，
                            // 只改那边会让开了智能符号的宿主上开关不生效。
                            //
                            // `clear_no_input` 已在上方单点短路，走不到这里；此处只区分
                            // 「丢废码但出标点」与「照常上屏」。
                            let discard_empty_code =
                                self.punct_empty_code_policy(&state) == PunctEmptyCodePolicy::Clear;
                            self.learn_on_main_punct_top_commit(&mut state);
                            let committed = self.take_committed(&mut state);
                            let mut commit_text = if discard_empty_code {
                                String::new()
                            } else {
                                self.maybe_convert(&state, &committed)
                            };
                            if !state.candidates.is_empty() {
                                let (start, _) = self.page_range(&state);
                                let idx =
                                    (start + state.selected_index).min(state.candidates.len() - 1);
                                let cand = state.candidates[idx].clone();
                                // 记账码：码表按输入码（码位独立），拼音/英文按候选码；
                                // 通配组码记全码。见 `main_freq_code`。
                                let freq_code = self.main_freq_code(&state.input_buffer, &cand);
                                self.record_selection_cand(&freq_code, &cand);
                                self.record_commit(
                                    &cand.text,
                                    state.input_buffer.len() as u32,
                                    (idx - start) as i32,
                                    CommitSource::Candidate,
                                );
                                commit_text.push_str(&self.cand_convert_text(&state, &cand));
                            } else if !state.input_buffer.is_empty() && !discard_empty_code {
                                // 无候选顶屏的是原码 → 同回车，用用户所打的大小写形态。
                                let raw = preedit_cursor::cased_or_buffer(
                                    &state.input_buffer,
                                    &state.input_buffer_cased,
                                );
                                // 原码类上屏进上屏历史（转换前形态、不含标点，同回车）。
                                self.push_commit_history(&format!("{committed}{raw}"));
                                if committed.is_empty() && self.engine_mgr.active_is_english() {
                                    self.learn_english_raw(
                                        &self.engine_mgr.active_schema_id(),
                                        raw,
                                    );
                                }
                                // 输入统计同回车上屏原码：只记本次上屏的原码（已转换前缀选词时已记过），
                                // 标点在下面另记一笔。
                                self.record_commit(
                                    raw,
                                    raw.len() as u32,
                                    -1,
                                    CommitSource::RawInput,
                                );
                                commit_text.push_str(raw);
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
                        // 同上一处 command guard。凡「本次按键已武装、但标点最终没上屏」的提前返回
                        // 都要解除（同族：`!punct_commit` / `clear_no_input` / CapsLock 透传）。
                        self.disarm_smart_symbol();
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
                    // 判据必须算在 `take_committed` 之前：那一步会把 committed_text 取空，
                    // 之后再问就恒为假。
                    //
                    // `clear_no_input`（丢废码且标点也不出）已在上方 `has_input` 块内单点短路，
                    // 走不到这里——那一态必须早于标点流水线返回，见该处注释。
                    let discard_empty_code =
                        self.punct_empty_code_policy(&state) == PunctEmptyCodePolicy::Clear;
                    self.learn_on_main_punct_top_commit(&mut state);
                    let committed = self.take_committed(&mut state);
                    let mut out = if discard_empty_code {
                        String::new()
                    } else {
                        self.maybe_convert(&state, &committed)
                    };
                    // 别把仍挂在组合态的旧符号（HoldComposition）拼进 out：宿主端会把它吸收进前缀
                    // （TSF `AbsorbHeldIntoPrefix`、macOS `absorbHeldIntoPrefix`、薄宿主
                    // `edit_ops::to_outcome` 直接 `EditOp::Commit`），服务端再拼就是双写「。。%」。
                    //
                    // 联想态不顶屏（同 `commit_highlight_then_char`）：联想没有码，高亮是猜的，按「。」
                    // 就是要个句号。本段是标点的通用出口、不受上面 `has_input` 守卫，判据须自己带。
                    if !state.candidates.is_empty() && !state.assoc_active() {
                        let (start, _) = self.page_range(&state);
                        let idx = (start + state.selected_index).min(state.candidates.len() - 1);
                        let cand = state.candidates[idx].clone();
                        // 记账码：码表按输入码（码位独立），拼音/英文按候选码；通配组码记全码。
                        // 见 `main_freq_code`。
                        let freq_code = self.main_freq_code(&state.input_buffer, &cand);
                        self.record_selection_cand(&freq_code, &cand);
                        // 标点上屏前先记被顶出的高亮候选（来源候选）。
                        self.record_commit(
                            &cand.text,
                            state.input_buffer.len() as u32,
                            (idx - start) as i32,
                            CommitSource::Candidate,
                        );
                        out.push_str(&self.cand_convert_text(&state, &cand));
                    } else if !state.input_buffer.is_empty() && !discard_empty_code {
                        // 无候选顶屏的是原码 → 同回车，用用户所打的大小写形态。
                        let raw = preedit_cursor::cased_or_buffer(
                            &state.input_buffer,
                            &state.input_buffer_cased,
                        );
                        // 原码类上屏进上屏历史（转换前形态、不含标点，同回车）。
                        self.push_commit_history(&format!("{committed}{raw}"));
                        if committed.is_empty() && self.engine_mgr.active_is_english() {
                            self.learn_english_raw(&self.engine_mgr.active_schema_id(), raw);
                        }
                        // 输入统计同回车上屏原码：只记本次上屏的原码（已转换前缀选词时已记过），
                        // 标点在下面另记一笔。
                        self.record_commit(raw, raw.len() as u32, -1, CommitSource::RawInput);
                        out.push_str(raw);
                    }
                    // 联想态计入「有输入」：宿主里挂着占位组合，这个标点须由服务端出、
                    // 不能落下面 CapsLock 的透传（透传不碰组合，占位组合会悬着）。
                    let had_input = was_assoc
                        || !state.input_buffer.is_empty()
                        || !state.candidates.is_empty()
                        || !committed.is_empty();
                    state.input_buffer.clear();
                    state.candidates.clear();

                    // CapsLock + 无待提交内容：TSF 层应已透传此键，coordinator 不应收到；
                    // 防御性兜底——直接透传让系统产生原始 WM_KEYDOWN + WM_CHAR。
                    if state.caps_lock && !had_input {
                        // 键交还宿主，标点不由服务端出，而本次按键已按中文列武装成 press1，须解除
                        // （同 `!punct_commit` / `clear_no_input` / 两处 command guard）。
                        self.disarm_smart_symbol();
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
                    // 「英文后」看紧挨着标点的那段：被顶出的候选 / 原码，没有则是光标前（锁存值）。
                    let piece = self.convert_punct_after(&state, ch, data.prev_char, &out);
                    state.chinese_punct = saved_chinese_punct;
                    // 智能符号 press1 在上面（顶屏之前）就按当时能看到的上下文武装了；英文后智能
                    // 标点看的是被顶出的那段，可能让实际上屏的是另一形态（`_` 而非 `——`）。武装串与
                    // 光标前的实际内容对不上时 press2 会按武装串的长度删字——删到用户打的字母，
                    // 故不一致即解除。只在英文后智能标点开着时检查：关着时两者同源。
                    if self.rt().config.input.punct.smart_after_latin {
                        let mut arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
                        if arm.armed && arm.key == ch && arm.str != piece {
                            arm.armed = false;
                        }
                    }
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
                                // 联想态：宿主里还挂着占位组合，而 `MoveCursorRight` 不碰组合。
                                // 跳出语义保留（不改成再上屏一个右括号——那是「））」），占位组合
                                // 按超时孤儿那套两路收口（见 `fire_assoc_hide`）：主动 push 结束
                                // 组合；push 没落地时，下一次透传键由 `adopt_orphaned_placeholder`
                                // 收掉——跳出合成的那个 VK_RIGHT 若被宿主会话转发回来，正是那一键。
                                if was_assoc {
                                    self.assoc_placeholder_orphaned
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
                                    self.push_end_composition();
                                }
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
                            // 从完整文本末尾回退右符号的宽度，顶出的候选前缀不参与定位。
                            let cursor_offset = right.len_utf16() as u32;
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
        self.note_word_boundary();
        // 与 handle_focus_lost 的 token 日志配对：只有两边都记 token，才能从日志算出
        // 「同一实例 gained 后多久自己 lost」——区分 DocMgr 抖动与真实离开就靠这个间隔。
        tracing::debug!(
            "handle_focus_gained: token={:#x} scope={:#x}",
            data.client_token,
            data.input_scope_mask
        );
        // 切进新的可编辑上下文同样是「用户动了别处」。focus_gained 没有去重（每次 DocMgr
        // 获焦都发，宿主抖动时一次切换会来好几条），全靠 menu_close_on_focus_change 的守卫期
        // 挡住刚弹出的菜单。
        self.menu_close_on_focus_change("focus_gained");
        self.close_softkeyboard_on_focus_change(
            crate::handle_softkeyboard::SoftKbFocusSignal::Gained {
                pid: (data.client_token >> 32) as u32,
            },
        );
        // 解析焦点进程的 caret 兼容态（微信 caret_use_top、per-app caret_offset_* 等）。
        // 必须在下面的 `apply_focus_caret` 之前跑：那一步按 `active_compat` 做
        // `caret_use_top`/`caret_offset_*` 变换，顺序反了第一份坐标会用上一个进程的规则
        // （刚切进来的第一次候选窗位置不对）。本段是 FOCUS_GAINED 的重型后置段，DLL 已拿到
        // 响应，同步 OpenProcess 不影响首键延迟。
        //
        // 旧值必须在 `update_active_compat` 整体覆写之前取，否则「切出规则应用后模式恢复」
        // 会静默失效。
        let new_pid = (data.client_token >> 32) as u32;
        // macOS：宿主名只能由 `.app` 告知（服务进程的 `process_name` 恒空）。必须先于
        // update_active_compat 落进缓存，否则那边读到空名 → compat 规则匹配不上、per-app
        // 记忆表查不到，整条按应用链路静默退化成全局行为。Windows 恒为空串，不进此分支。
        if !data.bundle_id.is_empty() && new_pid != 0 {
            let name = data.bundle_id.to_lowercase();
            let prev = self
                .pid_names
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(new_pid, name.clone());
            // 名字变了（含首次落缓存）：按键路径预提取的值按新名字重算。下面的
            // update_active_compat 在同 pid 同窗口时会早退，不会替它刷。
            if prev.as_deref() != Some(name.as_str()) {
                self.refresh_active_compat_lookups();
            }
        }
        // 取自 `mode_scope` 而非 `active_compat`：后者会被过渡窗口（任务栏）更新，
        // 拿它当「上一个模式归属宿主」会让紧随其后的桌面焦点被判成同进程、规则不再生效。
        // 详见 `mode_scope` 字段注释。
        let old_scope = self
            .mode_scope
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // 窗口上下文记账先于 update_active_compat：后者按该 token 记下的窗口解析规则
        // （同进程换窗口时持续类字段即时刷新，见 `coordinator/window_ctx.rs`）。
        // 留不留标题按进程判（`focus_window_of`）。重型段可以反查进程名（通常 bridge 连接时已缓存）。
        let window = self.focus_window_of(
            &self.proc_name_or_lookup(new_pid),
            &data.window_class,
            &data.window_title,
        );
        // DLL 手里那份密码门控是按该 token 旧窗口算的（没记录 = 握手时的空窗口），换窗口时
        // 下面的 `apply_input_diag` 只降不升，见 `apply_input_diag_capped`。
        let dll_pfe = self.password_force_english_for_token(data.client_token);
        let window_changed = self.note_focus_window(data.client_token, window.clone());
        self.update_active_compat(data.client_token);
        // 同一 token 换了窗口 ⇒ DLL 手里按 token 持有的那几项配置（占位字符、密码抑制、英文配对）
        // 可能已不对，重推。放在下面 `apply_input_diag` 之前：服务端按新窗口算密码抑制时，DLL
        // 那份尽量已在路上（不变量 core.suppress ⊆ C++.suppress，见 `apply_input_diag`）。
        if window_changed {
            self.repush_window_scoped_dll_config(data.client_token);
        }
        let new_has_rule = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_initial_rule;
        // 焦点 caret 走与同步段同一个入口。不要在这里直写 `state.caret_*`——重型段晚于
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
                composition_rect: None,
            },
            "handle_focus_gained",
        );
        // 组合起点锚定作废：焦点事件意味着换了 docMgr，组合还在但宿主位置可能整体迁移（表格宿主
        // 在单元格与编辑栏间切换，见 docs/architecture/caret-position-host-compat-notes.md 的
        // 「A 类」）。不作废则 reshow 仍下发锁死的旧起点，候选窗钉在旧位置；清掉后由下一帧重锁。
        *self
            .composition_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        self.tsf_frame_seen_in_composition
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // 矩形锚点与 pre_reflow 基准同步作废（理由同上）。两个都要清：它们的清位点都是
        // 「组合结束」与「焦点换 docMgr」两处。
        *self
            .locked_rect_anchor
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        *self
            .last_pre_reflow_probe
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        *self
            .pre_reflow_comp_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        // `shown_anchor` 刻意不清：焦点切换不会抹掉屏幕上的候选窗，清掉会让 reshow 判据失去
        // 唯一基准。它只在 `reset_first_show` 作废（见 docs/architecture/caret-position-host-compat-notes.md
        // 的「核心模型」）。
        // 坐标缓存作废：焦点事件随包携带的坐标多半在宿主 reflow 之前，可作兜底显示位置，
        // 但不够格让 fast 档跳过等待。
        self.caret_cache_verified
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // 同一件事的另一面：焦点后的空闲上报可以解除信任门（走 25ms 而非 600ms），但不足以
        // 让候选窗立即显示——宿主此刻报的往往是上一处的位置。挡住 idle_anchor 逃生口，直到本次
        // 焦点下拿到过一帧组合期权威坐标。见 `awaiting_first_authority_after_focus`。
        self.awaiting_first_authority_after_focus
            .store(true, std::sync::atomic::Ordering::Relaxed);
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
        // 空闲吃 CapsLock 的闸门按活动客户端的 pid 下发，故在 set_active_token 之后同步。
        {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.sync_capslock_idle_gate(&s);
        }
        // per-app 状态：进程名已入缓存，按规则表/记忆表/默认值切换本应用中英状态。若与同步段
        // get_current_mode 回传值不同（该进程首次聚焦），随后的 push_activation_status 推送修正。
        //
        // 两个条件的分工：
        //   crossed      焦点切入才重算。同应用内的焦点跳转（搜索框 ↔ 结果列表）不重算，
        //                否则用户手切的模式会被反复拉回初始值。
        //   per_app / has_rule
        //                per_app_scope 是按应用记忆；has_rule 把 compat.toml 规则的影响限制在
        //                进出规则应用这一步。退化成「规则表非空」会让任意两个应用间切换都重算，
        //                把用户手切的模式重置掉。
        //
        // 取舍：per_app_scope 下同进程重复 focus_gained 不重算（记忆表与现值同步，语义不变）；
        // 代价是少一条 compartment 脏事件的隐式自愈，该自愈在 IME_ACTIVATED 路径仍保留。
        //
        // 「切入」（`focus_crossed`）还包括同进程内命中的带窗口条件的进入类规则集合变了（两个
        // GUI 窗口各配各的 initial_mode）。仅标题变化不重算，唯一例外是「标题迟到」，见 `focus_crossed`。
        //
        // 作用域一票否决：任务栏 / Alt+Tab 切换器与桌面同属 explorer.exe，只能靠窗口类区分。
        // 未配作用域的进程恒放行。详见 `InitialModeScopeRule` 与 should_reapply_initial 注释。
        let proc_name = self.cached_proc_name(data.client_token);
        let resolved = self.resolve_compat(&proc_name, &window);
        let new_entry = entry_window_key(&resolved);
        let crossed = focus_crossed(&old_scope, new_pid, &resolved, &window);
        let title_blind = self.focus_is_title_blind(&proc_name, &window);
        let out_of_scope = !self
            .app_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .initial_mode_applies_to_window(&proc_name, &data.window_class);
        // 无条件打印窗口类：只在命中时打，「没打日志」就分不清「没配作用域」「窗口类为空」
        // 「类不在清单里」三种情况。
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
            // 只有真正参与决策的焦点才推进模式归属。过渡窗口跳过这一步，是为了不把
            // 「跨进程切入」这个一次性事件提前消费掉——否则点任务栏再回桌面时，桌面就成了
            // 「同进程」，它配的 initial_mode 永远不会生效（见字段注释）。
            *self.mode_scope.lock().unwrap_or_else(|e| e.into_inner()) = ModeScope {
                pid: new_pid,
                has_rule: new_has_rule,
                entry_windows: new_entry,
                title_blind,
                window: window.clone(),
            };
        }
        // 按应用方案（compat.toml `schema`）：与 initial_mode 同一个判据——跨进程切入才重算，
        // 同进程内焦点跳转不动，尊重用户在应用内的手切；作用域外的过渡窗口（任务栏）也不动，
        // 否则「点任务栏再回来」就把 mode_scope 之外的方案切走了。
        // 放在 apply_initial_mode 之前：切方案会让方案级标点意图在下一次状态推送时落地，
        // 显式 `initial_punct` 规则要在它之后再落一次才压得住。
        // 不得挪进 get_current_mode（DLL 同步阻塞路径），见 `coordinator/app_schema.rs`。
        if crossed && !out_of_scope {
            self.apply_app_schema_on_focus(&proc_name, &window);
        }
        if should_reapply_initial(
            crossed,
            self.rt().config.input.default.per_app_scope(),
            old_scope.has_rule,
            new_has_rule,
            out_of_scope,
        ) {
            self.apply_initial_mode(data.client_token, false);
        }
        let status = self.build_status();
        self.push_activation_status(data.client_token);
        self.notify_toolbar_async(); // 激活态 → 工具栏显示（异步，避免 foreground_fullscreen_kind 阻塞 bridge 线程）
        self.show_persistent_status_if_always(); // 常驻模式:获焦即显示状态
        // ui.status.show_on_focus：切到新宿主时提示一次。按 client_token 去重——同一宿主内换
        // docMgr（Excel 单元格 ↔ 公式栏）不重复弹，见 last_focus_tip_token。
        self.show_focus_status_if_enabled(data.client_token);
        let pid = (data.client_token >> 32) as u32;
        self.apply_input_diag_capped(
            pid,
            data.disabled,
            data.reason,
            data.input_scope_mask,
            !window_changed || dll_pfe,
        );
        Some(status)
    }

    fn handle_focus_lost(&self, client_token: u64, reason: FocusLostReason) {
        self.note_word_boundary();
        // 独立日志行，否则只能靠 TSF 日志反推 HideToolbar 的来源。token 便于与 DLL 日志的
        // `Sending focus_lost token=…` 对齐到具体宿主实例。
        tracing::debug!(
            "handle_focus_lost: token={:#x} reason={:?}",
            client_token,
            reason
        );
        // CapsLock 钩子闸门兜底归零，先于 stale 判定：钩子是全局的，闸门滞留在 true 会让
        // 用户在别的应用里 CapsLock 失灵，必须在最宽的路径上归零。陈旧失焦同样证明用户动了别处。
        wind_keys::capslock_hook::set_should_eat(false);
        wind_keys::capslock_hook::set_alt_gate(0);
        wind_keys::capslock_hook::set_alt_pid(0);
        // 关菜单先于 stale 判定与 reason 分流：菜单的生命周期与输入态无关，
        // 陈旧失焦/噪声层失焦同样证明用户动了别处。详见 menu_close_on_focus_change。
        self.menu_close_on_focus_change("focus_lost");
        self.close_softkeyboard_on_focus_change(
            crate::handle_softkeyboard::SoftKbFocusSignal::Lost(reason),
        );
        self.reset_comment_alt();
        if self.is_stale_focus_event(client_token, "handle_focus_lost") {
            return;
        }
        // 探针：放行了一条从未上报过 focus_gained 的 token 的失焦。`active_token` 也可能由
        // `ime_activated`（每进程一次）设置，宿主的 focus_gained 被上游吞掉时 token 恰等于 active，
        // 激活态被清后再无人置回（症状：切走再切回工具栏不显示）。
        //
        // 用 WARN：出现一次就说明上游有事件被吞。不做补救（成因未知），只让它可见。
        // `active != 0` 不可省：那时 stale 校验无条件放行，警告纯属噪音。`NoEditCtx` 除外：
        // DLL 对不可编辑的 DocMgr 本就不发 focus_gained、改发这一条。
        let gained = self.push_server.gained_token();
        if client_token != 0
            && client_token != gained
            && self.push_server.active_token() != 0
            && !matches!(reason, FocusLostReason::NoEditCtx)
        {
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
                self.sync_capslock_idle_gate(&s);
                // 真正离开了这个宿主 ⇒ 焦点气泡的去重记录作废，下次再进来该重新提示一次。
                // 只在这一档清：CtxLost/DocChanged 是宿主内部换 docMgr 的噪声，清了就等于
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
            // 语言栏图标只认 NoEditCtx 这一档：它才表示「新文档确实没有可编辑上下文」。
            // CtxLost 是 DocMgr 级失焦的噪声，拿它驱动图标会误显「英」。详见 State::focus_no_edit_ctx。
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
                // 本处刻意不受 MENU_FOCUS_GUARD 保护：下面的 notify_ui_hide 会经
                // HideCandidates 无条件隐藏菜单窗口，此时若把 menu_open 留成 true，就成了
                // 「窗口没了、键还被吞」的状态不一致——比守卫失效更糟。
                s.menu_open = false;
                #[cfg(ext_presenter)]
                self.hide_menu_ui_unlocked();
                s.menu_opened_at = None;
                self.reset_exclusive_modes(&mut s); // 失焦丢弃临时英文/拼音/快捷输入残留
            }
        }
        if clears_input {
            // 失焦即清配对状态，别按 reason 细分保留：焦点离开期间用户做了什么（点走光标、删掉
            // 括号）输入法无从感知，且 DLL 侧计数按宿主进程各一份，保留只是猜测。
            // 同一焦点内的陈旧风险由 state_ttl_secs 兜底。
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
            // 只收菜单、不解除气泡 / 状态气泡的隐藏抑制会让它残留（气泡从此移出不隐藏、右键被当成
            // 「菜单开着」）。这里之后没有菜单命令要派发，不受 clear_tooltip_menu_flag 的截图时序
            // 约束，可以立即解除。UI 侧另有兜底（Tooltip::on_menu_dismissed），两处幂等叠加。
            self.clear_tooltip_menu_flag();
            self.hide_tip(); // 失焦隐藏状态提示（常驻模式尤需）
            self.terminate_auto_phrase("focus_lost"); // 换窗口 = 一段输入结束
        }
        // CtxLost 刻意不碰候选窗：输入态还在（Excel 抖动保护），候选窗应跟随输入态而非
        // 焦点。真正离开时随后的 DocChanged / Thread 会收口。
    }

    fn get_current_mode(
        &self,
        client_token: u64,
        window_class: &str,
        window_title: &str,
    ) -> (bool, bool, bool) {
        // 回传三元组（中英 / 全半角 / 中英标点）。标点态不可省：DLL 的标点透传判据要按
        // 它在两份集合间二选一，漏了就会在焦点切换后的竞态窗口里误用英文态超集
        // （`,` `.` 被透传成半角）。而 per-app 的 `initial_punct` 规则正是在本方法里落地的。
        //
        // FocusGained 同步路径回传 ModePush：DLL 正同步阻塞等本值，仅允许锁+HashMap 查询，
        // 严禁 OpenProcess 等跨进程调用。`should_reapply_initial` / `apply_initial_mode` /
        // `cached_proc_name` / `rule_initial_*` 全部满足该约束（纯锁 + 表查询）。
        //
        // 判据与落地必须与重型段 `handle_focus_gained` 同源——就是下面这两个函数调用，别另写
        // 简化版：兜底层级（remember_last_state、配置默认）一漏，同步段就会回传上一个应用的
        // 模式，系统语言指示器闪一下，首键也按错的模式处理，而同步回传正是为消除这个竞态。
        let new_pid = (client_token >> 32) as u32;
        let old_scope = self
            .mode_scope
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (old_pid, old_has_rule) = (old_scope.pid, old_scope.has_rule);
        // 窗口用 DLL 随本请求带来的类名 + 标题：重型段的记账（`note_focus_window`）还没跑，token
        // 表里是上一个窗口。跨越判据与重型段同源（`focus_crossed`），窗口也同源（`focus_window_of`）。
        // 进程名只查缓存（同步段禁止 OpenProcess）；查不到时下面本就跳过 per-app 重算。
        let proc = self.cached_proc_name(client_token);
        let window = self.focus_window_of(&proc, window_class, window_title);
        let resolved = self.resolve_compat(&proc, &window);
        let crossed = focus_crossed(&old_scope, new_pid, &resolved, &window);
        if crossed {
            // 作用域一票否决，判据与重型段完全同源。
            // 两处都要有：本方法先跑且 DLL 正阻塞等它的回传值，只挡住重型段的话，
            // 状态早在这里就被改掉了，日志上却显示「已跳过」。
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
                let new_has_rule = resolved
                    .rule
                    .as_ref()
                    .is_some_and(|r| r.initial_mode.is_some() || r.initial_punct.is_some());
                let per_app = self.rt().config.input.default.per_app_scope();
                let reapply = crate::coordinator::should_reapply_initial(
                    crossed,
                    per_app,
                    old_has_rule,
                    new_has_rule,
                    out_of_scope,
                );
                if reapply {
                    // reset_aux=false：与重型段的调用逐字一致。随后重型段会用同样的入参
                    // 再调一次，`apply_initial_mode` 是幂等的（每次都按当前表重算目标）。
                    self.apply_initial_mode_in(client_token, &window, false);
                }
                // 锁先释放再打日志：本方法在 DLL 的同步阻塞路径上，不在持锁期间做格式化。
                let (chinese, full, punct) = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    (s.chinese_mode, s.full_width, s.chinese_punct)
                };
                tracing::debug!(
                    "get_current_mode: proc={proc} class={window_class:?} old_pid={old_pid} \
                     old_has_rule={old_has_rule} new_has_rule={new_has_rule} per_app={per_app} \
                     reapply={reapply} → 回传 chinese={chinese} full={full} punct={punct}"
                );
                return (chinese, full, punct);
            } else {
                // 这条出路跳过 per-app 重算、回传全局现状，首键可能按上一个应用的模式处理。
                // `cached_proc_name` 只查 `pid_names`（同步段禁止 OpenProcess），服务重启或
                // PID 复用后可能查不到。排「切过来首字符是英文」看本行。
                tracing::debug!(
                    "get_current_mode: pid={new_pid} 进程名未知（pid_names 未命中） \
                     class={window_class:?} → 跳过 per-app 重算，回传全局现状"
                );
            }
        }
        // crossed=false（同进程内换焦点）与上面两条 fall-through 共用本出口；
        // 三者靠 crossed= 与各自那条前置日志区分。
        let (chinese, full, punct) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.chinese_mode, s.full_width, s.chinese_punct)
        };
        tracing::debug!(
            "get_current_mode: pid={new_pid} old_pid={old_pid} crossed={crossed} \
             → 回传现状 chinese={chinese} full={full} punct={punct}"
        );
        (chinese, full, punct)
    }

    fn handle_ime_activated(&self, client_token: u64) -> Option<StatusUpdateData> {
        if client_token != 0 {
            self.push_server.set_active_token(client_token);
        }
        // 切回本输入法时同样刷新焦点进程的 caret 兼容态（异步段，不阻塞 DLL）。
        // 本事件不带窗口信息：规则按该 token 最近一次 focus_gained 记下的窗口解析（没有则只按进程名）。
        self.update_active_compat(client_token);
        // 激活初始状态矩阵：remember=false 重置为配置默认（含全半角/标点）；
        // remember=true 保持全局记忆；state_scope="app" 恢复该应用的会话记忆。
        // 同时构成对 compartment 脏事件污染的自愈兜底（详见 TextService.cpp 门卫修复）。
        self.apply_initial_mode(client_token, true);
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.ime_active = true;
            self.sync_capslock_idle_gate(&s);
            // 兜底置真：宿主主动激活本输入法，通常意味着焦点已进入输入框。
            // 若某些宿主 IME_ACTIVATED 之后不补发 focus_gained，而这里不置位，
            // has_edit_context 将永远停在 false —— 工具栏再也不显示。
            // 该字段的失效方向不对称：多显示只是碍眼，永不显示是功能失效，故取宽松侧。
            s.has_edit_context = true;
            s.focus_no_edit_ctx = false;
        }
        // 输入诊断态（密码抑制 / 禁用）是按焦点采的读数，切走本输入法后不会有人更新它；
        // 不清就会让新实例沿用上一实例留下的密码态（切回后图标恒显「英」）。
        // 清零后由 DLL 随后补发的 input_state_report 覆盖（同一 bridge 线程按序处理）。
        self.apply_input_diag((client_token >> 32) as u32, false, 0, 0);
        let status = self.build_status();
        self.push_activation_status(client_token);
        self.notify_toolbar_async(); // 激活态 → 工具栏显示（异步，避免 foreground_fullscreen_kind 阻塞 bridge 线程）
        self.show_persistent_status_if_always(); // 常驻模式:激活即显示状态
        Some(status)
    }

    fn is_background_client(&self, pid: u32) -> bool {
        let active_pid = (self.push_server.active_token() >> 32) as u32;
        // 先挡掉绝大多数情况（发送方就是活动客户端），焦点查询只在跨进程时才做。
        if pid == 0 || active_pid == 0 || pid == active_pid {
            return false;
        }
        // 最近一次按键就是它发的 ⇒ 它在打字，不是后台。误判一次后的自愈口：活动 token
        // 只有 focus_gained / ime_activated 会改，而 SearchHost 这类宿主不发 focus_gained、
        // ime_activated 每进程只发一次，没有这道口子，误判会让它整段使用期间的光标都被丢掉。
        if pid == self.focus_pid.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let focus = crate::focus_owner_pid();
        let background = crate::is_background_sender(pid, active_pid, focus);
        // 跨进程才会走到这里，量很小；三个判据 pid 全打出来，靶机上才能复核判得对不对。
        tracing::debug!(
            "后台客户端判定: sender={pid} active={active_pid} focus={focus} → {}",
            if background { "后台" } else { "放行" }
        );
        background
    }

    fn handle_background_ime_activated(&self, client_token: u64) {
        self.push_activation_status(client_token);
    }

    fn handle_ime_deactivated(&self, client_token: u64) {
        tracing::debug!("handle_ime_deactivated: token={:#x}", client_token);
        // 同 handle_focus_lost：关菜单先于 stale 判定。下面清 menu_open 的那段仍保留
        // （非陈旧路径的完整清理），两处幂等叠加无副作用。
        self.menu_close_on_focus_change("ime_deactivated");
        self.close_softkeyboard_on_focus_change(
            crate::handle_softkeyboard::SoftKbFocusSignal::ImeDeactivated,
        );
        self.reset_comment_alt();
        // 与 focus_lost 同源的乱序风险：切走本输入法时旧宿主的 IME_DEACTIVATED 同样可能
        // 晚于新宿主的 focus_gained 到达（两者都是 fire-and-forget 异步写）。
        if self.is_stale_focus_event(client_token, "handle_ime_deactivated") {
            return;
        }
        // 切走本输入法（换到别的 IME / 非输入法应用）：清激活态、清输入、隐藏全部 UI。
        // 对齐 Go SetIMEActivated(false)（隐藏工具栏 + hideUI），根治“切走仍残留显示”。
        // 宿主接管候选绘制的记账随之清掉（DLL 重新激活时会再报，见 ActivateEx）。
        self.clear_uielement_host_pid((client_token >> 32) as u32);
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.ime_active = false;
            self.sync_capslock_idle_gate(&s);
            s.has_edit_context = false; // 切走本输入法：谈不上焦点在不在可编辑控件里
            s.focus_no_edit_ctx = false; // 同上：不表态（input_block 也会因 ime_active 早退）
            s.input_buffer.clear();
            s.preedit.clear();
            s.candidates.clear();
            s.menu_open = false;
            #[cfg(ext_presenter)]
            self.hide_menu_ui_unlocked();
            s.menu_opened_at = None;
            self.reset_exclusive_modes(&mut s); // 切走本输入法时丢弃独占模式残留
        }
        self.notify_toolbar_async(); // 非激活态 → notify_toolbar 内部下发 HideToolbar（异步）
        self.notify_ui_hide(); // 隐藏候选窗 + 弹出菜单
        // 同 handle_focus_lost：只收菜单不解除抑制会残留；此后无菜单命令派发，可立即解除。
        self.clear_tooltip_menu_flag();
        self.hide_tip(); // 切走本输入法隐藏状态提示
        self.terminate_auto_phrase("ime_deactivated"); // 切走输入法 = 一段输入结束
    }

    fn handle_mode_notify(&self, flags: u32) {
        let chinese_mode = (flags & wind_ipc::protocol::STATUS_CHINESE_MODE) != 0;
        let clear_input = (flags & wind_ipc::protocol::STATUS_MODE_CHANGED) != 0;
        // 模式变更的四个入口都打一条同形日志（见 handle_system_mode_switch 的说明）。
        // 本入口当前在 C++ 侧无调用点（SendModeNotify 无人调用），日志出现即说明有新调用方。
        tracing::debug!(
            "mode_notify: {} -> {} clear_input={}",
            if self.is_chinese_mode() { "中" } else { "英" },
            if chinese_mode { "中" } else { "英" },
            clear_input
        );
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
        self.handle_toggle_mode_cased(false)
    }

    fn handle_system_mode_switch(
        &self,
        chinese_mode: bool,
        source: wind_ipc::protocol::ModeSwitchSource,
        ctrl_held: bool,
    ) -> (Option<StatusUpdateData>, String) {
        // 用户关掉了 Ctrl+空格 切换（`keys.ctrl_space_toggle` 或自定义按键里绑 none），
        // 而这次翻转是用户按出来的（系统输入法开关热键 / 按键侧兜底）⇒ 拒绝，回包仍是当前
        // 模式，DLL 据此把 compartment 拉回。与下面 ignore_host_ime_switch 同一条仲裁回路，
        // 「必须再异步推一次状态」的理由也相同。判据见 `ctrl_space` 模块。
        if self.ctrl_space_switch_rejected(chinese_mode, source, ctrl_held) {
            tracing::debug!(
                "system_mode_switch: source={} ctrl_held={} 请求 {}，Ctrl+空格 切换已关闭，拒绝",
                source.as_str(),
                ctrl_held,
                if chinese_mode { "中" } else { "英" }
            );
            self.push_state_update();
            return (Some(self.build_status()), String::new());
        }
        // 「忽略软件切换中英」（per-app 优先，未配跟随全局）：拒绝后不改模式，回包仍是
        // 当前模式。DLL 侧 `_ApplyModeSwitch` 见到 `newChineseMode != requestedMode` 会把
        // compartment 拉回真实模式——这条仲裁回路早就存在（密码框强制英文用的就是它），
        // 不必新开通道。开和关两个方向同一处理。
        if self.host_ime_switch_ignored(source, ctrl_held) {
            let cur = self.is_chinese_mode();
            tracing::debug!(
                "system_mode_switch: source={} 请求 {}，已按 ignore_host_ime_switch 拒绝（保持{}）",
                source.as_str(),
                if chinese_mode { "中" } else { "英" },
                if cur { "中" } else { "英" }
            );
            // 必须再异步推一次状态，不能只靠同步回包：同步回包的回写发生在 `OnChange` 调用栈里，
            // 会被「值相同就不写」的守卫跳过（见 docs/architecture/tsf-openclose-compartment-semantics.md
            // §3.2），compartment 与真实模式脱节，下一次 Ctrl+Space 就成了 no-op。
            // `UpdateFullStatus` 在 WM_UPDATE_STATUS 上下文里无条件回写，读到的是落定值。
            self.push_state_update();
            return (Some(self.build_status()), String::new());
        }
        // 模式变更的四个入口（本入口 / handle_toggle_mode / handle_mode_notify /
        // per-app initial_mode 重算）都必须留一条同形日志，否则「自动变成英文」无从溯源。
        // source 把宿主写 compartment（WPF/游戏主动关 IME）与我们自己的按键兜底分开。
        tracing::debug!(
            "system_mode_switch: source={} {} -> {}",
            source.as_str(),
            if self.is_chinese_mode() { "中" } else { "英" },
            if chinese_mode { "中" } else { "英" }
        );
        // 「切换模式时取消大小写锁定」：目标模式由外部指定（Ctrl+Space/KBLSwitch），
        // 出厂只取消 CapsLock 让目标模式真正生效，不改写目标；`mode_after_cancel = english`
        // 时改落英文，判定与 Shift 那条同一函数（`mode_after_caps_cancel`）。
        // 改写只对用户按出来的切换生效：宿主写 compartment / 菜单按请求落地，来源分派见
        // `CapsCancelSwitch::from_system`。
        //
        // 改写了目标时回包的模式就与请求不同——DLL 的 `_ApplyModeSwitch` 见到
        // `newChineseMode != requestedMode` 会把 compartment 拉回真实模式，与 per-app
        // 「忽略软件切换中英」拒绝请求走的是同一条仲裁回路。
        let chinese_mode = if self.cancel_caps_on_switch() {
            Self::mode_after_caps_cancel(
                &self.rt().config.input.capslock.mode_after_cancel,
                CapsCancelSwitch::from_system(chinese_mode, source, ctrl_held),
                self.is_chinese_mode(),
            )
        } else {
            chinese_mode
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.chinese_mode = chinese_mode;
        // 标点随中英文切换（对齐 Go）：开启 punct_follow_mode 时，标点跟随模式。
        if self.rt().config.input.punct.follow_mode {
            self.set_punct_below_schema_intent(&mut state, chinese_mode);
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
        // 宿主亲口说组合没了 ⇒ 联想孤儿占位组合也随之不存在，撤标记。
        //
        // 只在这一处撤，不在 `handle_focus_lost` 撤：那里的 CtxLost / DocChanged 是
        // 宿主内部换 docMgr 的噪声，组合可能还挂着，误撤会留下悬空的占位组合（危险方向）；
        // 不撤的代价只是下一次透传多发一次收口，`EndComposition` 空跑、键照常重放（安全方向）。
        self.clear_orphaned_placeholder();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // 必须整体复位（含 active/temp_pinyin_*/mix_* 等 overlay 状态），不能只清 input_buffer：
        // 临时拼音/快捷输入的缓冲与前缀不在 input_buffer 里，只清后者会让模式残留
        // （点鼠标移光标后候选窗消失，再按键仍走临拼）。
        // reset_exclusive_modes 内含 disarm_smart_symbol 与强制竖排布局恢复。
        // 此回调仅在 TSF 意外终止组合时触发（焦点切换、宿主强制 EndComposition 等）；
        // 我们自己的 CommitText 不触发（_pComposition 已提前置 nullptr，走"Already released"分支）。
        // 因此在此 disarm 是安全的：意外中断必然使 HoldComposition 失效，旧 held_text 不可再用。
        self.reset_exclusive_modes(&mut state);
        // 复位菜单状态：点击别处会终止 composition 并经 notify_ui_hide 隐藏菜单窗口，
        // 但若不清 menu_open，下一个键会被 forward_menu_key 当作菜单键吞掉（首字符失效）。
        state.menu_open = false;
        #[cfg(ext_presenter)]
        self.hide_menu_ui_unlocked();
        drop(state);
        self.clear_pair_tracker(); // 组合意外终止：配对上下文失效，清栈防跳出键误判
        self.notify_ui_hide();
        // 同 handle_focus_lost：只收菜单不解除抑制会残留；此后无菜单命令派发，可立即解除。
        self.clear_tooltip_menu_flag();
    }

    fn handle_caret_update(&self, data: &CaretData) {
        // compStart 必须打：它是「本轮 composition 的 reflow 坐标是否已到」的唯一判据
        // （compStart=(0,0) ⇒ 该帧来自 idle 更新，组合还没建立/还没 reflow），也是
        // coords_ready 逃生口与嵌入模式定位锚点的来源。
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
        // 应用兼容规则 caret_use_top（见 docs/architecture/caret-position-host-compat-notes.md
        // 的「B 类」）：改用稳定的 rect.top 定位，Y -= height，组合起点 Y 同步上移。
        // height 不能压成 1：上方显示时 wind-ui 用 caret_y - height 推正文顶端，压成 1 会让候选窗
        // 压住正文；故保留真实行高并对退化帧取下限（宁大勿小）。后续逻辑都基于变换后的本地副本。
        let mut data = *data;
        let active_compat = self.apply_caret_compat(&mut data);
        let data = &data;
        let composition_start_pair_guard = active_compat.composition_start_pair_guard;
        // 本组合已有 TSF 坐标时，GUI 回退帧是噪声（见字段注释），在写缓存之前就挡掉。
        if data.source == wind_ipc::protocol::caret_source::GUI_CARET
            && self
                .tsf_frame_seen_in_composition
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            debug!(
                "caret_update → 忽略: 本组合已有 TSF 坐标，gui_caret ({},{}) 是跨窗口的陈旧位置",
                data.x, data.y
            );
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (prev_x, prev_y, prev_height, prev_source) = (
            state.caret_x,
            state.caret_y,
            state.caret_height,
            state.caret_source,
        );
        state.caret_x = data.x;
        state.caret_y = data.y;
        state.caret_height = data.height;
        // 行高估计只吃非退化的帧：微信等 WebView 的 height 在 1↔20px 跳变，1 是宿主的
        // 退化值而不是真的行高变了。见 `last_sane_caret_height` 的字段注释。
        if data.height > 1 {
            self.last_sane_caret_height
                .store(data.height, std::sync::atomic::Ordering::Relaxed);
        }
        state.caret_source = data.source;
        let now_valid = Self::caret_is_valid(data.x, data.y, data.height);
        if !now_valid {
            debug!("caret_update → 丢弃: caret 无效（(0,0) 哨兵、越界或高度非正）");
            return;
        }
        // 消费焦点气泡的挂起：DLL 在焦点路径拿不到同步锁时会异步补一条权威坐标，这就是它。
        // 必须在下面的 `composing` 闸门之前：焦点刚到达时 `composing` 恒 false。
        //
        // 只认 TSF 域：本闸门存在的全部意义就是不拿 GUI 回退坐标定位气泡。
        // compare_exchange 而非 load + store：与锚点超时（`fire_focus_tip_timeout`）同时到达时
        // 只让一方显示。
        if wind_ipc::protocol::caret_source::is_tsf(data.source)
            && self
                .pending_focus_tip
                .compare_exchange(
                    true,
                    false,
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
        {
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
        // 无占位的联想（`assoc_placeholder = "none"`）：宿主里没有组合，WPS 等会在上屏后
        // 上报一个与联想窗无关的远处光标，照常当「组合中」处理会把窗口重新锚过去而飘走。
        // 此时只更新缓存，窗口留在联想出现的位置。
        let bare_assoc = state.input_buffer.is_empty()
            && state.assoc_active()
            && self.assoc_placeholder_is_none();
        let composing =
            (!state.candidates.is_empty() && !bare_assoc) || !state.input_buffer.is_empty();
        if !composing {
            // 常态：上屏后到下一键之间宿主仍会上报 caret。坐标已写入 state，只是不做显示决策。
            // 这一帧是组合开始前对当前插入点的直接测量，记下这个出身：下一次组合的 probe 若与它
            // 显著不符，陈旧的是 probe（见 `caret_cache_is_idle_report` 与 `handle_caret_probe`）。
            self.caret_cache_is_idle_report
                .store(true, std::sync::atomic::Ordering::Relaxed);
            // 同一帧也够格解除首帧信任门。不在这里置位就是单向陷阱：清位有切窗口、点击移光标
            // 两个日常入口，置位却只有组合期权威 caret_update 一个，而 fast 档的组合常等不到它，
            // 一旦被清每个组合都会 arm 600ms 长兜底。
            //
            // 限 TSF 通道：`gui_caret` 不够格让 fast 档跳过等待（见 docs/architecture/
            // caret-position-host-compat-notes.md 的「非 TSF 坐标来源」）。与上面无条件置位的
            // `caret_cache_is_idle_report` 口径刻意不同：那个问出身，本字段问能否直接拿来定位。
            if wind_ipc::protocol::caret_source::is_tsf(data.source) {
                self.caret_cache_verified
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            debug!(
                "caret_update → 仅更新缓存: 无组合（无候选且缓冲空），不做显示决策；\
                 记为组合前空闲上报 ({},{}) tsf={}",
                data.x,
                data.y,
                wind_ipc::protocol::caret_source::is_tsf(data.source)
            );
            return;
        }
        // `stale_probe_guard` 宿主的组合期坐标整条来源都不可信（见 `handle_caret_probe` 的同名
        // 判据），普通 caret_update 同源，必须一并挡掉并还原缓存，空闲坐标继续当锚点（见
        // docs/architecture/caret-position-host-compat-notes.md 的「B 类」）。没有空闲上报时不生效。
        if wind_ipc::protocol::caret_source::is_tsf(data.source)
            && active_compat.stale_probe_guard
            && self
                .caret_cache_is_idle_report
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            debug!(
                "caret_update → 忽略: 该宿主组合期坐标恒不可信，沿用组合前空闲上报 ({},{})（本帧 ({},{})）",
                prev_x, prev_y, data.x, data.y
            );
            state.caret_x = prev_x;
            state.caret_y = prev_y;
            state.caret_height = prev_height;
            state.caret_source = prev_source;
            return;
        }
        if wind_ipc::protocol::caret_source::is_tsf(data.source) {
            self.tsf_frame_seen_in_composition
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // 先记下「宿主上一帧如实上报的组合起点」，供后面的大偏移逃生阀判据使用。
        //
        // 必须在下面的锚定块之前取旧值：判据问的是宿主自己前后两帧是否改口，与我方锁了什么无关。
        // 零值不更新：compStart=(0,0) 表示「宿主这帧没报」，覆盖会让空闲帧把记录清掉。
        let prev_reported_comp_start = {
            let mut r = self
                .last_reported_comp_start
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prev = *r;
            if data.composition_start_x != 0 || data.composition_start_y != 0 {
                *r = (data.composition_start_x, data.composition_start_y, true);
            }
            prev
        };
        // 组合矩形的用法、可信判据与几种形态见 docs/architecture/caret-position-host-compat-notes.md
        // 的「组合矩形的三种形态」。可信时锚点直接取左下角，跨行时自动落到最后一行行首；
        // compStart 跨行后仍停在第一行。
        //
        // 「矩形只是一个插入点」的宽度上界：光标宽 0~2px，真包围盒最窄一个字符宽（16px 起），
        // 4 落在两者之间的空带里。
        const INSERTION_POINT_MAX_WIDTH: i32 = 4;

        // 本帧布局查询整个退化（宽或高为 0）：同帧的 caret 与 compStart 出自同一次查询，一并
        // 不可信，下面据此抑制大偏移逃生阀。与宽 1~2px 的「插入点形态」严格区分，后者 caret 可信。
        let frame_layout_degenerate = data
            .composition_rect
            .is_some_and(|(l, t, r, b)| r <= l || b <= t);
        // 插入点形态且宿主声明了 `pin_anchor_when_start_drifts`：compStart 跟着插入点走，必须同时
        // 抑制大偏移逃生阀，否则锚点被一路推远、删除时小步回退永远回不来。必须按宿主开启：
        // Excel / WPS 表格形态相同而期望相反（候选窗要跟着单元格走）。
        let rect_is_insertion_point = active_compat.pin_anchor_when_start_drifts
            && data
                .composition_rect
                .is_some_and(|(l, t, r, b)| r > l && b > t && r - l <= INSERTION_POINT_MAX_WIDTH);
        let rect_anchor = data.composition_rect.and_then(|(l, t, r, b)| {
            // 判据一：矩形要真的是个范围，按宽度判。别改成「left >= caret.x」：换行后 caret 回到
            // 行首恰好等于 left，真范围会被误判（见文档「已否定的方向」）。挡住后走既有锚点逻辑，
            // 那条路对插入点形态的宿主本就是调好的。
            if b <= t || r - l <= INSERTION_POINT_MAX_WIDTH {
                self.note_overlay_verdict(if b <= t || r <= l {
                    wind_ui_types::diag::RectVerdict::Degenerate
                } else {
                    wind_ui_types::diag::RectVerdict::InsertionPoint
                });
                debug!(
                    "组合矩形不可信（是插入点不是范围）: rect=({l},{t},{r},{b}) w={} caret.x={}——回退既有锚点逻辑",
                    r - l,
                    data.x
                );
                return None;
            }
            // 判据二：覆盖当前插入行。若某宿主只返回首行，caret 走到下一行时 y 会
            // 大于 bottom，此时该矩形答的不是当前行的位置。
            if data.y > b {
                self.note_overlay_verdict(
                    wind_ui_types::diag::RectVerdict::NotCoveringCaretLine,
                );
                debug!(
                    "组合矩形不可信（不覆盖插入行）: rect=({l},{t},{r},{b}) caret=({},{})——疑该宿主只返回首行",
                    data.x, data.y
                );
                return None;
            }
            self.note_overlay_verdict(wind_ui_types::diag::RectVerdict::Trusted);
            Some((l, b))
        });
        // 暂存本帧几何，锚点在 notify_ui_update 里补齐后一并推给浮窗。
        self.stash_overlay_frame(data);
        if let Some((ax, ay)) = rect_anchor {
            // 同一组合、同一行里，矩形只给一次锚点：有的宿主 `left` 随组合变长而右移（H 类 Tabby，
            // 见 docs/architecture/caret-position-host-compat-notes.md）。对 `left` 稳定的宿主这是
            // no-op，故做成全局。跨行（`ay` 变了）必须放行，那正是矩形路径的价值。
            //
            // 锁用独立的 `locked_rect_anchor` 而不是 `cs.2`：新组合第一帧的 compStart 常是陈旧值，
            // 要靠本路径修正，绑在 `cs.2` 上会把这次修正挡掉。两把锁先取值再上锁，不嵌套
            // （同 `effective_first_show_mode` 的注释）。
            let locked = *self
                .locked_rect_anchor
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut cs = self
                .composition_start
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if locked.2 && ay == locked.1 {
                // 不是什么都不做，而是把 cs 写回本轮锁定的锚点：`shown_anchor` 回灌与大偏移逃生阀
                // 可能在无矩形的那一帧改过 `cs`，它们依赖「矩形每帧如实到达」，只挡不写会让那次
                // 改写永久生效。
                if ax != locked.0 {
                    debug!(
                        "组合矩形起点漂移，不采信: ({ax},{ay}) → 保持本轮锚点 ({},{})（同组合同行内\
                         起点按定义不动；宿主的 left 随组合变长而移）",
                        locked.0, locked.1
                    );
                }
                *cs = (locked.0, locked.1, true);
            } else {
                if (cs.0, cs.1) != (ax, ay) {
                    debug!(
                        "组合起点取自组合矩形: ({},{}) → ({ax},{ay})（宿主对整个组合范围的测量，跨行时自动落到最后一行行首）",
                        cs.0, cs.1
                    );
                }
                *cs = (ax, ay, true);
                drop(cs); // 先放掉 cs 再上另一把锁，避免引入新的持有序
                *self
                    .locked_rect_anchor
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = (ax, ay, true);
            }
        }
        // 钉住的是列不是行：`pin_anchor_when_start_drifts` 的宿主没有组合矩形、不知道最后一行
        // 行首在哪，但 `caret.y` 给出了当前行。跨行时 x 保持钉住、y 跟随 caret 的行，否则候选窗
        // 会压住第二行。单行时本条自动无效；删回上一行也跟着回去。有矩形时由 `rect_anchor` 处理。
        if active_compat.pin_anchor_when_start_drifts
            && rect_anchor.is_none()
            && Self::caret_is_valid(data.x, data.y, data.height)
        {
            let mut cs = self
                .composition_start
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cs.2 && cs.1 != data.y {
                debug!(
                    "组合起点跟行: ({},{}) → ({},{})（x 钉住、y 跟随 caret 所在行；该宿主无组合矩形）",
                    cs.0, cs.1, cs.0, data.y
                );
                cs.1 = data.y;
            }
        }
        // 组合起点锚定：同一组合只接受首个有效 compStart，后续即便携带新值也不覆盖（防部分控件
        // GetRange 让起点随输入漂移，致候选窗随输入右移）。500px 校验排除 logical/physical 坐标系不一致。
        {
            let mut cs = self
                .composition_start
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !cs.2 && (data.composition_start_x != 0 || data.composition_start_y != 0) {
                // 先扩到 i64 再减：宿主未初始化的 compStart 可能是 i32::MIN，
                // 直接 i32 相减 / abs 会在 debug 构建中溢出 panic，连“丢弃脏数据”都走不到。
                let dx = (i64::from(data.composition_start_x) - i64::from(data.x)).abs();
                let dy = (i64::from(data.composition_start_y) - i64::from(data.y)).abs();
                // 500px 校验的前提是两者同源（抓同一 context 的坐标系不一致）。caret 来自 GUI 回退
                // 等非 TSF 通道时与组合起点不同源，比较无意义，直接采信组合起点。
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
        //
        // `TSF_DEFAULT_POS` 例外：它按定义是「宿主不知道插入点在哪」，不能标记缓存对应当前插入点，
        // 否则下一次组合走短兜底，先在旧位置闪一下再跳（见 docs/architecture/
        // caret-position-host-compat-notes.md 的「非 TSF 坐标来源」）。必须双向赋值：state 已被
        // 本帧改写成默认位置，上一帧留下的 true 也不再成立。`GUI_CARET` 在本站点仍置真。
        //
        // 另两个标志照常置位，尤其 `awaiting_first_authority_after_focus` 必须清，否则默认位置
        // 宿主永远解禁不了 `idle_anchor` 逃生口。
        //
        // 代价：该宿主恒 arm 600ms 长兜底，`IsHostDefaultPosition` 失败关闭时会等满 600ms。
        // 接受是因为短兜底那 25ms 显示的也是错位置。
        self.caret_cache_verified.store(
            data.source != wind_ipc::protocol::caret_source::TSF_DEFAULT_POS,
            std::sync::atomic::Ordering::Relaxed,
        );
        // 缓存自此装的是本次组合的位置，不再是「组合前的空闲上报」——那条判据的前提
        // 到此为止。清位必须和上面的置位同处：两者都以「这一帧够格当基准」为条件，分开
        // 写就会在某条 early return 上分叉（本函数上游有五处提前返回）。
        self.caret_cache_is_idle_report
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // 本次焦点下已经拿到过一帧组合期权威坐标 ⇒ 宿主已 reflow、坐标体系稳定，此后的空闲
        // 上报可以放心当锚点用（idle_anchor 逃生口解禁）。同上两个标志一起放在这个「够格」
        // 判据下面，三者共享同一个前提，分开写必然在某条 early return 上分叉。
        self.awaiting_first_authority_after_focus
            .store(false, std::sync::atomic::Ordering::Relaxed);
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
            // 基准用 `caret_baseline`（上一次被认可的插入点），既不用坐标缓存也不用 `shown_anchor`：
            // 缓存会被 probe 抢先刷新，比较恒得「没变化」，错位无法自愈；`shown_anchor` 会把 settle
            // 吸收的偏差永久留下，下一帧 tol 掉回 3px 时候选窗自己挪一格。三个量的分工见
            // docs/architecture/caret-position-host-compat-notes.md 的「核心模型」。缓存仅在基准
            // 未建立时兜底（本分支前提是候选窗已显示，理论上走不到）。
            let (base_x, base_y) = {
                let b = self
                    .caret_baseline
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if b.2 { (b.0, b.1) } else { (prev_x, prev_y) }
            };
            let dx = (data.x - base_x).abs();
            let dy = (data.y - base_y).abs();
            // 首显用过非权威坐标时，本轮第一次权威坐标改用放宽的容差：偏差在
            // 「行高 × settle_ratio」以内就不校正。抖动的观感来自校正动作本身而非坐标偏差
            // ——十几像素的偏移用户根本不会注意，跳一下却很显眼（多数输入法也这么处理）。
            // 换行/重排的偏差通常 ≥2 个行高，远超此阈值，仍会正常校正。
            let settle = if self
                .first_show_was_provisional
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                let ratio = self.rt().config.ui.candidate.first_show_settle_ratio;
                // 带上 `last_sane_caret_height`：微信等 WebView 的 height 在 1↔20px 跳变，
                // 只取当前帧会让容差塌缩到下限 3px（`(1×0.8) as i32 == 0`），放宽多少倍都没用。
                // 行高是宿主的属性、不是单帧的属性，见该字段的注释。
                let h = data
                    .height
                    .max(state.caret_height)
                    .max(
                        self.last_sane_caret_height
                            .load(std::sync::atomic::Ordering::Relaxed),
                    )
                    .max(1) as f32;
                (h * ratio.max(0.0)) as i32
            } else {
                0
            };
            // 水平与垂直分别定容差：垂直要分辨换行（偏差恰好一个行高），ratio 须 < 1.0（出厂 0.8）；
            // 水平要吸收「上屏文本比编码窄」（满码自动上屏后立刻开新组合，差约一个字符宽），
            // 0.8×行高恰好卡在门外，故取 2 倍，仍远小于真实错位（数百像素）。
            // 只在本轮第一帧权威坐标上生效（`first_show_was_provisional` 被 swap 消费），之后都回到 3px。
            let tol_y = settle.max(3); // 常规微移过滤下限保持 3px 不变
            let tol_x = settle.saturating_mul(2).max(3);
            if dx <= tol_x && dy <= tol_y {
                // 组合起点也得跟着不动：reshow 时位置取的是 `cs`，若它锁成这一帧的新坐标，settle
                // 只是把那一跳推迟到下一个字母。「不值得校正」与「记住位置」分处两地，两处都要表达。
                // 有权威组合矩形时不做这次回灌：矩形每帧如实到达，回灌会把 `cs` 从「宿主说的起点」
                // 污染成「显示位置」。
                if rect_anchor.is_none() {
                    let anchor = *self.shown_anchor.lock().unwrap_or_else(|e| e.into_inner());
                    if anchor.2 {
                        let mut cs = self
                            .composition_start
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if cs.2 {
                            *cs = (anchor.0, anchor.1, true);
                        }
                    }
                }
                // 候选窗不动，但这一帧的坐标已被认可——基准必须跟上，否则同样的偏差
                // 下一帧会被重新算一遍，而 settle 的放宽容差已被 swap 消费掉，tol 掉回 3px。
                *self
                    .caret_baseline
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = (data.x, data.y, true);
                debug!(
                    "caret_update → 忽略: 微移 dx={dx} dy={dy}（≤{tol_x}/{tol_y}px，不 reshow）"
                );
                return;
            }
            // 基准记的是上一次被认可的插入点，不是候选窗画在哪。漏掉这里，组合起点锁定后
            // 同一个 dx 会每帧反复触发 reshow。
            *self
                .caret_baseline
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = (data.x, data.y, true);
            // 大偏移逃生阀：「组合起点本组合内不再更新」的出口。首显位置整个是错的（陈旧的空闲
            // 上报、上一个单元格）时，不重锁就永远回不来。判据是偏差的量级（> 3 倍行高），与方向
            // 无关；不能改成「reported compStart 非零就信它」，多个宿主的 compStart 陈旧或坐标系混用。
            // 已知盲区与 QQNT 的 pair guard 例外见 docs/architecture/caret-position-host-compat-notes.md
            // 的「组合矩形的三种形态」与「E 类」。
            //
            // 有可信组合矩形、本帧整个退化、或插入点形态宿主时整段跳过：矩形本身就是起点的信息来源，
            // 而后两者的 caret 会把逃生阀一路骗走。
            if rect_anchor.is_none() && !frame_layout_degenerate && !rect_is_insertion_point {
                let line_h = self
                    .last_sane_caret_height
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .max(state.caret_height)
                    .max(1);
                if dx > line_h * 3 || dy > line_h * 3 {
                    let mut cs = self
                        .composition_start
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if cs.2 {
                        // pair guard 的判据：宿主自己前后两帧报的组合起点没有改口，caret 离它多远都只是
                        // 组合宽度。不要拿 `cs` 与 reported compStart 比（`cs` 有三个写入者，另两个写的
                        // 是显示位置），也不要退回只认某种 source 帧对的写法（理由见文档「E 类」）。
                        // 宿主换了插入上下文时起点必然改口，逃生阀照常生效；组合第一帧无前值同样不成立。
                        // 见 `a_grossly_wrong_first_show_can_relock_the_composition_start`。
                        let guarded_pair = composition_start_pair_guard
                            && Self::caret_is_valid(
                                data.composition_start_x,
                                data.composition_start_y,
                                data.height,
                            )
                            && prev_reported_comp_start.2
                            && (prev_reported_comp_start.0, prev_reported_comp_start.1)
                                == (data.composition_start_x, data.composition_start_y);
                        if guarded_pair {
                            debug!(
                                "组合起点保持: ({},{})（命中 composition_start_pair_guard：\
                                 prev=({prev_x},{prev_y}) h={prev_height} src={}，current=({},{}) src={}，\
                                 compStart=({},{})（上一帧同值）；caret 跨度 {dx}/{dy} 不是起点错误）",
                                cs.0,
                                cs.1,
                                wind_ipc::protocol::caret_source::name(prev_source),
                                data.x,
                                data.y,
                                wind_ipc::protocol::caret_source::name(data.source),
                                data.composition_start_x,
                                data.composition_start_y
                            );
                        } else {
                            // 重锁目标：默认取当前 caret（多数宿主的 compStart 不可信）；开了
                            // `composition_start_pair_guard` 的宿主已确认如实上报，取它本帧报的
                            // compStart，否则输入框整体移动时会锁到组合末端、再被 guard 钉住。
                            // 这只对显式声明的宿主成立，不等于「compStart 非零就信它」。
                            let reported_is_trustworthy = composition_start_pair_guard
                                && Self::caret_is_valid(
                                    data.composition_start_x,
                                    data.composition_start_y,
                                    data.height,
                                );
                            let (to_x, to_y) = if reported_is_trustworthy {
                                (data.composition_start_x, data.composition_start_y)
                            } else {
                                (data.x, data.y)
                            };
                            if (cs.0, cs.1) != (to_x, to_y) {
                                debug!(
                                    "组合起点重锁: ({},{}) → ({to_x},{to_y})（caret 偏移 {dx}/{dy} \
                                     远超字宽量级，首显位置整个是错的，不能再钉住；目标取自{}）",
                                    cs.0,
                                    cs.1,
                                    if reported_is_trustworthy {
                                        "宿主本帧上报的 compStart"
                                    } else {
                                        "当前 caret"
                                    }
                                );
                                *cs = (to_x, to_y, true);
                            }
                        }
                    }
                }
            }
            debug!("caret_update → reshow: dx={dx} dy={dy}");
            self.show_authorized
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.notify_ui_update(&state);
        } else {
            // 隐式出口：既没在等首显、候选窗也没显示着。打日志以与「首显」「reshow」区分，
            // 查「候选窗为什么没出现」时最要紧。
            debug!("caret_update → 无动作: 未等待首显且候选窗未显示，本帧仅落缓存");
        }
    }

    /// focus_gained 随包携带的 caret：只更新坐标缓存，不做任何显示决策。
    ///
    /// 焦点事件带来的坐标是「切换发生的那一刻」的值，宿主可能还没 reflow。若把它交给
    /// [`Self::handle_caret_update`]，会被当成 reflow 后的权威坐标消费掉首显等待，候选窗
    /// 就在中间位置先显示一次再跳到最终位置。
    ///
    /// caret_use_top 变换要照做——坐标缓存本身必须与 handle_caret_update 写入的口径一致，
    /// 否则首键前的兜底坐标会和后续更新差一个行高。
    fn handle_focus_gained_caret(&self, data: &CaretData) {
        self.apply_focus_caret(data, "handle_focus_gained_caret");
    }

    fn handle_caret_probe(&self, data: &CaretData) {
        // reflow 前探测（组合刚启动时 C++ 的异步取值）：绝不作首显的位置来源，只刷新缓存 +
        // 记下「本轮重排前在哪」当否决基准（见 `last_pre_reflow_probe`）。两者都不改变首显时机，
        // 故放在档位门之前：`wait` 档宿主同样需要（见 docs/architecture/
        // caret-position-host-compat-notes.md 的「C 类」）。
        if data.source == wind_ipc::protocol::caret_source::PRE_REFLOW {
            let absorbed = self.absorb_probe_coords(data);
            // 记下本轮的「重排前坐标」，供下面那条 reflow 判据当基准（见字段注释）。
            // 记的是本帧原值，与 absorb 收没收它无关：判据问的是「宿主还停在重排前吗」，
            // 而退化帧/不可信帧同样能回答这个问题——按 absorbed 过滤会让恰恰最需要它的宿主
            // （probe 不可信那些）拿不到基准。
            *self
                .last_pre_reflow_probe
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = (data.x, data.y, true);
            // 组合起点只在本帧被收入时记（退化帧 / probe 不可信的宿主不给它当兜底位置），
            // 500px 同源校验与 handle_caret_update 锁组合起点那处一致。
            let (csx, csy) = (data.composition_start_x, data.composition_start_y);
            if absorbed
                && (csx != 0 || csy != 0)
                && (i64::from(csx) - i64::from(data.x)).abs() < 500
                && (i64::from(csy) - i64::from(data.y)).abs() < 500
            {
                *self
                    .pre_reflow_comp_start
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = (csx, csy, true);
            }
            debug!(
                "caret_probe(pre_reflow) → 刷新缓存 + 记为本轮重排前基准: ({},{}) h={} compStart=({},{}) {}",
                data.x,
                data.y,
                data.height,
                data.composition_start_x,
                data.composition_start_y,
                if absorbed {
                    "已收入"
                } else {
                    "未收入（退化帧或该宿主 probe 不可信）"
                }
            );
            return;
        }
        // 首帧 reflow 期间 DLL 逐次采样上报（CMD_CARET_PROBE）。非 fast 档完全忽略，
        // 保持「等 reflow 权威坐标」。
        let mode = self.effective_first_show_mode();
        if mode != wind_config::app_compat::FirstShowMode::Fast {
            debug!("caret_probe → 忽略: 当前档位={} 非 fast", mode.as_config());
            return;
        }
        // 只在正等首显时有意义：已显示 / 未 arm 的帧交给常规 caret_update 路径。
        if !*self
            .pending_first_show
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            // 不做显示决策，但把坐标收进缓存：连续快速上屏时 probe 常是唯一的位置来源（C 类宿主
            // 长按时一条权威 caret_update 都不来）。只影响下一轮首显的起点，本轮候选窗不随输入移动。
            // 退化帧与 `stale_probe_guard` 宿主的 probe 不收（见 `absorb_probe_coords`）。
            let absorb = self.absorb_probe_coords(data);
            debug!(
                "caret_probe → 不做显示决策: 未在等待首显（已首显过 / 未 arm）；\
                 坐标 ({},{}) h={} {}",
                data.x,
                data.y,
                data.height,
                if absorb {
                    "已收入缓存"
                } else {
                    "未收入（退化帧或该宿主 probe 不可信）"
                }
            );
            return;
        }
        // 退化 rect（无高度）一律不采信：其 x 可能与真实位置差上千像素。
        if data.height <= 0 {
            debug!("caret_probe → 丢弃: 退化 rect（h<=0）");
            return;
        }
        // 首帧信任门（第二条通路）：坐标缓存未经当前插入点验证时，下面的判据都失去判断力
        // （基准属于上一个文档，「≠ 基准」恒成立；跨焦点的按键间隔也说明不了什么），必须一起
        // 让位给长兜底。首显有多条通路，否决判据必须每条都接。
        if self.first_show_needs_long_wait() {
            // 否决它做显示决策，但不否决它当坐标来源：收进缓存后兜底到期用的就是这份试探坐标，
            // 可能有几十像素偏差，但远好过上一个文档留下的那份。
            let absorbed = self.absorb_probe_coords(data);
            debug!(
                "caret_probe → 继续等待: 坐标缓存未经当前插入点验证，本轮判据无基准可比（x={} y={}，{}）",
                data.x,
                data.y,
                if absorbed {
                    "坐标已收入缓存供兜底用"
                } else {
                    "坐标未收入（退化帧或该宿主 probe 恒陈旧）"
                }
            );
            return;
        }
        // 第三道判据：`stale_probe_guard` 宿主的 probe 恒停在上一次组合的位置（B 类），手上有
        // 组合前空闲上报时拒绝 probe，由 25ms 短兜底用空闲坐标显示。必须放在连打快路径之前，
        // 否则按键间隔够短就整条绕过。逐宿主开启、不做全局：微信该信缓存、WindTerm 该信 probe，
        // 位置关系分不出该信谁。详见 `AppCompatRule::stale_probe_guard`。
        let guard_on = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stale_probe_guard;
        if guard_on
            && self
                .caret_cache_is_idle_report
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            let (sx, sy) = {
                let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                (s.caret_x, s.caret_y)
            };
            // 判据是布尔的：手上有宿主刚报的空闲坐标就用它、忽略所有 probe。不要改回位置关系判据
            // （连续四版被真机推翻，见文档「已否定的方向」）。代价是放弃 probe 提前首显（4ms → 25ms），
            // 换来位置正确。没有空闲上报时本判据不生效。
            debug!(
                "caret_probe → 拒绝: 该宿主 composition rect 恒陈旧，改用组合前空闲上报 \
                 ({sx},{sy})（probe 报的是 ({},{})）",
                data.x, data.y
            );
            return;
        }
        // 第四道判据：与本轮 `pre_reflow` 帧逐位相同 ⇒ 宿主还没重排。判据 1 的「上一轮权威坐标」
        // 在终端行满回绕时失效（见文档「已否定的方向」），本轮的 pre_reflow 帧才是贴切的基准。
        // 必须放在连打快路径之前：它要救的行满回绕正是连打到行尾才发生的。判据是相等，不是距离/方向。
        //
        // 代价：组合期间光标本就不动的宿主每轮都会命中，probe 抢跑关闭、恒退回 25ms 兜底，
        // 首显慢约 20ms 而位置不变；换来的是换行时不跳一屏。
        let (px, py, has_pre) = *self
            .last_pre_reflow_probe
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if has_pre && data.x == px && data.y == py {
            debug!(
                "caret_probe → 继续等待: 坐标仍等于本轮 pre_reflow 帧 ({px},{py})，宿主尚未 reflow"
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
            // 等待不变，但缓存若是非 TSF 域（GUI caret / last_known）就先换成这份 probe：兜底到期时
            // 用的是缓存，TSF 域的旧值再旧也比跨窗口的 GUI caret 可信（Java 被动客户端即此，见
            // docs/architecture/caret-position-host-compat-notes.md 的「非 TSF 坐标来源」）。
            // 只换非 TSF 缓存：TSF 域的空闲上报才是对的那个，本分支的前提正是 probe 可能停在上一轮。
            let cache_is_tsf = {
                let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                wind_ipc::protocol::caret_source::is_tsf(s.caret_source)
            };
            if wind_ipc::protocol::caret_source::is_tsf(data.source)
                && !cache_is_tsf
                && self.absorb_probe_coords(data)
            {
                debug!(
                    "caret_probe → 继续等待: 坐标仍等于上一轮权威 ({lx},{ly})，宿主尚未 reflow；\
                     缓存原为非 TSF 来源，已换成本帧供兜底用"
                );
                return;
            }
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
        // 延到 600ms 等于把 fast 重新变回 wait。坐标缓存不可信时也不在这里延长：那时的等待时长
        // 已由 `arm_pending_first_show` 的首帧信任门决定，握手再插一脚就成了第二个真相源，
        // 行为会随 IPC 时序摇摆。
        if self.first_show_mode_is_fast() {
            debug!("caret_pending → 忽略延长: fast 档兜底时长在 arm 时已按坐标可信度定");
            return;
        }
        self.arm_pending_first_show_with_timeout(FIRST_SHOW_LONG_FALLBACK_MS);
    }

    /// 宿主报告「光标移动且当前无 composition」（C++ `TextService::OnEndEdit`，守卫
    /// `selChanged && _pComposition == nullptr`）。
    ///
    /// 这是码表自动造词唯一能感知到「用户敲了空格/回车结束一句」的途径：码表每选一字
    /// 就上屏并关闭 composition，此后 Space/Enter 被 TSF 直接透传给宿主，协调器根本收不到
    /// 按键（`KeyEventSink.cpp:398/966/1024` —— Backspace/Enter/Escape 仅在有 composition
    /// 或 input session 时才拦截）。
    ///
    /// # 自提交宽限期
    ///
    /// 本输入法自己提交文字后，宿主插入文本同样导致光标移动 → 同样回送本事件，且在协议层
    /// 与用户真实光标移动无法区分，只能靠时间判别。若不区分，每上屏一个字就被自己
    /// 的回声判成「用户移动光标」→ flush → 缓冲永远只有 1 个字 → 造词恒不触发。
    ///
    /// 宽限值取 [`SELF_COMMIT_GRACE`]，已由真机日志校准（见该常量注释的实测分布）。
    ///
    /// 回声分支不做任何动作，故只记 TRACE：它的频率恒等于上屏频率（每上屏一个字必有
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
        // 复用本判据是安全的：误判成回声只是维持现状，误判成移动只是让下一次首显多等一程。
        self.caret_cache_verified
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.note_host_wrote_text("selection_changed");
        // 无占位联想：光标被用户挪走，联想不再有意义，立刻收窗（否则要等自动隐藏计时）。
        // 宿主只在「光标移动且当前无组合」时回报本事件，恰是无占位联想的形状。
        if self.assoc_placeholder_is_none() {
            let exited = {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                self.exit_assoc(&mut st, crate::handle_assoc::AssocExit::CaretMoved)
            };
            if exited {
                self.push_hotkey_session_if_changed();
                self.notify_ui_hide();
            }
        }
    }

    fn handle_commit_request(&self, data: &CommitRequestData) -> Option<CommitResultData> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.input_buffer.is_empty() {
            return None;
        }
        let tk = data.trigger_key as u32; // 协议为 u16，统一按 VK(u32) 比对
        // 取上屏文本、来源与记账码：命中候选取候选 source，退回原码分支为 None（不可归因）。
        // 记账码按来源分流（见 `freq_code`）——码表按输入码、拼音/英文按候选码；退回原码的
        // 分支上屏的就是缓冲本身，无候选可依，用输入码。通配组码记全码，见 `main_freq_code`。
        let cand_meta = |c: &Candidate| {
            (
                c.text.clone(),
                c.source,
                self.main_freq_code(&state.input_buffer, c),
            )
        };
        let raw = || {
            (
                state.input_buffer.clone(),
                CandidateSource::None,
                state.input_buffer.clone(),
            )
        };
        // 这是一条独立于按键路径的上屏通路（DLL 侧 TSF 排水 / 顶码延迟提交发起），补空格必须
        // 在此单独接线。第四元 `append_space` 按分支显式给出：四个分支的答案各不相同。
        let (text, source, freq_code, append_space) = if tk == keymap::VK_SPACE {
            match state.candidates.first() {
                // 空格选首选：候选口径（与 `commit_selected` 同）。
                Some(c) => {
                    let (t, s, f) = cand_meta(c);
                    // 与按键路径同口径：头部候选（输入原文）不带 source，只认 source 会漏补。
                    let ap = self.english_appends_space(
                        &state,
                        s,
                        &t,
                        crate::preedit_cursor::cased_or_buffer(
                            &state.input_buffer,
                            &state.input_buffer_cased,
                        ),
                    );
                    (t, s, f, ap)
                }
                // 空格退回原码：无候选可依，方案口径（与 VK_SPACE 空码分支同）。
                //
                // 这一支没有接空码丢弃开关（`input.space_on_empty_behavior`），下方 VK_RETURN 也没接
                // `enter_behavior`。目前整条 barrier 通路未激活（C++ `_SendCommitRequest` 无调用点，
                // 见 wind_tsf/src/AGENTS.md），接上时这两处都要补判据。
                None => {
                    let (t, s, f) = raw();
                    // 手上有 `state`（本函数开头就取了锁），故按语境取两份开关中的一份。
                    (t, s, f, self.english_space_enabled_in(&state))
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
                    // 与按键路径同口径：头部候选（输入原文）不带 source，只认 source 会漏补。
                    let ap = self.english_appends_space(
                        &state,
                        s,
                        &t,
                        crate::preedit_cursor::cased_or_buffer(
                            &state.input_buffer,
                            &state.input_buffer_cased,
                        ),
                    );
                    (t, s, f, ap)
                }
                // 数字键越界退回原码：不补。按键路径下此情形走
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
        // 补空格必须在记账之后：`record_selection` 记的是词本身，带上尾空格会写出
        // 「hello 」这种与读取端（`apply_freq_rerank` 按候选文本查）永远对不上的词频键。
        let mut text = if append_space {
            format!("{text} ")
        } else {
            text
        };
        // 中英交界空格同按键出口一样补在本笔前面（`crate::auto_spacing`）；这条通路不产
        // `KeyAction`，故直接调文本级判据。
        let mut prev = state.spacing_prev;
        self.space_committed_text(&mut prev, &mut text);
        state.spacing_prev = prev;
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
        // DLL 侧 host-render 初始化/映射失败：记录告警。
        warn!("host-render 失败上报 reason={reason}（DLL 退回进程内渲染）");
    }

    fn handle_input_state_report(&self, pid: u32, disabled: bool, reason: u8, mask: u64) {
        self.apply_input_diag(pid, disabled, reason, mask);
    }

    fn handle_key_observed(&self, key_code: u32, modifiers: u32) {
        self.hook_observed_key(key_code, modifiers);
    }

    fn handle_diag_snapshot(&self, snap: &wind_ipc::protocol::DiagSnapshotPayload) {
        self.apply_diag_snapshot(snap);
    }

    // ── TSF UI-less（宿主自绘候选）三件套，实现见 handle_uielement.rs ──
    fn handle_uielement_state(&self, pid: u32, host_draws: bool, host_reads: bool) {
        self.apply_uielement_state(pid, host_draws, host_reads);
    }

    fn uielement_page(&self) -> wind_ipc::protocol::UiElementPage {
        self.uielement_page_snapshot()
    }

    fn handle_uielement_action(&self, action: u32, arg: u32) {
        self.apply_uielement_action(action, arg);
    }

    fn note_key_source_pid(&self, pid: u32) {
        if pid != 0 {
            self.focus_pid
                .store(pid, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
#[path = "message_handler_ext_envelope_tests.rs"]
mod ext_envelope_tests;

impl Coordinator {
    /// 同 `MessageHandler::handle_toggle_mode`，`upper` 为真时切英文上屏的原码转大写
    /// （CapsLock 作切换键且开了 `input.capslock.commit_upper`）。
    pub(crate) fn handle_toggle_mode_cased(
        &self,
        upper: bool,
    ) -> (Option<StatusUpdateData>, String) {
        self.handle_switch_mode_cased(None, upper)
    }

    /// 中英切换内核：`target` 给定终态（`Some(true)`＝置中文 / `Some(false)`＝置英文，
    /// `ime.set("cn-en", …)` 走这里），`None`＝翻转（Shift 等切换键）。
    pub(crate) fn handle_switch_mode_cased(
        &self,
        target: Option<bool>,
        upper: bool,
    ) -> (Option<StatusUpdateData>, String) {
        // 模式变更的四个入口都打一条同形日志（见 handle_system_mode_switch 的说明）。
        // 本入口 = 本地切换键（Shift 等），按键侧另有 `toggle_mode key_up` 记录键码，
        // 但那条只在 policed 路径上，菜单/热键触发的 toggle 走不到，故这里仍要打。
        tracing::debug!(
            "switch_mode: {} -> {}",
            if self.is_chinese_mode() { "中" } else { "英" },
            match target {
                Some(true) => "置中文",
                Some(false) => "置英文",
                None => "翻转",
            }
        );
        // 「切换模式时取消大小写锁定」：CapsLock 开时按切换键，出厂语义是"回到可输入中文
        // 的状态"（对齐搜狗）——取消锁定并归位中文，而非翻转 chinese_mode；否则
        // chinese_mode 原本为 true（被 CapsLock 压制）时翻转反而落到英文，切换仍然无效。
        // 落点可配（`input.capslock.mode_after_cancel`），判定见 `mode_after_caps_cancel`。
        let caps_cancelled = self.cancel_caps_on_switch();
        // 中英切换 = 一段输入结束。须在取 state 锁之前调用：terminate_auto_phrase 内部
        // 走词库 IO，不可在持 state 锁时进行。
        self.terminate_auto_phrase("toggle_mode");
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.chinese_mode = if caps_cancelled {
            Self::mode_after_caps_cancel(
                &self.rt().config.input.capslock.mode_after_cancel,
                target.map_or(CapsCancelSwitch::Toggle, CapsCancelSwitch::Directed),
                state.chinese_mode,
            )
        } else {
            target.unwrap_or(!state.chinese_mode)
        };
        let chinese = state.chinese_mode;
        // 标点随中英文切换（对齐 Go）：开启 punct_follow_mode 时，标点中/英跟随当前模式。
        if self.rt().config.input.punct.follow_mode {
            self.set_punct_below_schema_intent(&mut state, chinese);
        }
        let commit_text = self.take_input_on_mode_switch_cased(&mut state, chinese, upper);
        drop(state);
        self.record_app_mode(chinese);
        self.record_last_state();
        self.punct.lock().unwrap_or_else(|e| e.into_inner()).reset();
        self.disarm_smart_symbol();
        // 配对栈刻意不清：中英切换既不移动光标也不消除已插入的右符号，「光标紧贴右符号」
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

    /// 热键进入的 overlay 模式（临拼 / 特殊 / 生僻字）该发什么组合区。
    ///
    /// # 为什么只在 `display` 为空时看开关
    ///
    /// 这几个模式有两条进入方式，组合区形态完全不同：
    /// - 引导键（`\`、`z` 等）⇒ `display` 含引导符，是真实内容，必须建 composition
    ///   才显示得出来。此刻替换选中内容本就是标准行为，开关不该、也不能干预。
    /// - 直达热键（`key_code == 0` ⇒ 不写引导符）⇒ `display` 为空。这时建 composition
    ///   的唯一目的是撑开 range 取坐标（空组合区由 C++ 侧补一个占位空格），而那个占位
    ///   建在当前 selection 上，用户选中着文字时会被替换掉。开关管的就是这一种。
    ///
    /// 关掉时返回 `Consumed`：根本不发 `UpdateComposition`，DLL 收不到东西也就不会建
    /// composition，选中内容原样保留；坐标退到 `GetCaretPosition` 的回退链。代价是那条回退
    /// 链在很多宿主上不准——这正是开关按模式分设、默认全开的原因（见 `CaretPlacementConfig`）。
    fn hotkey_entry_composition(
        &self,
        key_code: u32,
        display: String,
        via_composition: bool,
    ) -> KeyAction {
        // 判据是 `key_code == 0` 这个显式哨兵，不是派生的 `display.is_empty()`：引导键映射不出
        // 字符时 display 也为空（会误吞引导键）；进入瞬间 preedit 一旦带上任何东西，开关又会
        // 静默失效。
        // 哨兵的出处见 `commit_and_enter_temp_pinyin` 与 `enter_special_mode`：直达热键
        // 分派时一律传 0，「不写引导符」正是靠它。
        debug_assert!(
            key_code != 0 || display.is_empty(),
            "直达热键（key_code==0）不写引导符，组合区理应为空，实际 display={display:?}"
        );
        if key_code == 0 && !via_composition {
            return KeyAction::Consumed;
        }
        let caret_pos = display.chars().count() as u32;
        KeyAction::UpdateComposition {
            text: display,
            caret_pos,
        }
    }

    /// 临拼直达热键进入时是否用占位 composition 取坐标。
    pub(crate) fn temp_pinyin_entry_composition(
        &self,
        key_code: u32,
        display: String,
    ) -> KeyAction {
        let on = self.rt().config.input.caret.temp_pinyin_via_composition;
        self.hotkey_entry_composition(key_code, display, on)
    }

    /// 特殊模式直达热键进入时是否用占位 composition 取坐标。
    pub(crate) fn special_entry_composition(&self, key_code: u32, display: String) -> KeyAction {
        let on = self.rt().config.input.caret.special_via_composition;
        self.hotkey_entry_composition(key_code, display, on)
    }

    /// 生僻字直达热键进入时是否用占位 composition 取坐标。
    pub(crate) fn rare_char_entry_composition(&self, key_code: u32, display: String) -> KeyAction {
        let on = self.rt().config.input.caret.rare_char_via_composition;
        self.hotkey_entry_composition(key_code, display, on)
    }

    /// 反查模式直达热键进入：不设开关、固定走占位 composition（与
    /// `rare_char_via_composition` 出厂值一致，让直达热键与生僻字模式行为同构）。
    pub(crate) fn reverse_entry_composition(&self, key_code: u32, display: String) -> KeyAction {
        self.hotkey_entry_composition(key_code, display, true)
    }
}

#[cfg(test)]
#[path = "message_handler_commit_newline_action_tests.rs"]
mod commit_newline_action_tests;

#[cfg(all(test, ext_presenter))]
mod menu_open_decode_tests {
    use super::*;
    use crate::handle_menu::MenuOpenRequest;

    fn decode(body: &str) -> MenuOpenRequest {
        decode_menu_open(body.as_bytes()).unwrap_or_else(|| panic!("应能解析：{body}"))
    }

    #[test]
    fn ordinary_request_passes_through_unchanged() {
        let r = decode(r#"{"target":2,"x":100,"y":-20,"work":[0,0,1920,1080],"lx":5,"ly":6}"#);
        assert_eq!(
            r,
            MenuOpenRequest {
                target: 2,
                x: 100,
                y: -20,
                work: [0, 0, 1920, 1080],
                local: Some((5, 6)),
            }
        );
    }

    /// 极值坐标钳到 ±2^24：原样放进去，菜单定位的「坐标 + 宽高」在 dev 构建溢出 panic。
    #[test]
    fn extreme_coordinates_are_clamped() {
        let (max, min) = (i32::MAX, i32::MIN);
        let r = decode(&format!(
            r#"{{"target":-1,"x":{max},"y":{min},"work":[{min},{min},{max},{max}],"lx":{max},"ly":{min}}}"#
        ));
        let l = HOST_COORD_LIMIT;
        assert_eq!((r.x, r.y), (l, -l));
        assert_eq!(r.work, [-l, -l, l, l]);
        assert_eq!(r.local, Some((l, -l)));
    }

    /// 菜单指针事件的坐标同样钳制（命中测试里也有「坐标 + 宽高」）。
    #[test]
    fn menu_pointer_coordinates_are_clamped() {
        use wind_bridge::handler::MessageHandler;
        use wind_ipc::protocol::menu_pointer::MENU_POINTER_MOTION;
        use wind_ui_types::{MenuAnchor, MenuPointerEvent, UiCommand};
        let (c, rx) = Coordinator::new_headless_with_ui(wind_config::Config::default(), None);
        c.show_main_menu(MenuAnchor::at_point(0, 0));
        let _ = rx.try_iter().count();
        c.handle_menu_pointer(MENU_POINTER_MOTION, 0, i32::MAX, i32::MIN);
        let l = HOST_COORD_LIMIT;
        let cmds: Vec<_> = rx.try_iter().collect();
        assert!(
            cmds.iter().any(|m| matches!(
                m,
                UiCommand::MenuPointer { event: MenuPointerEvent::Move, x, y } if (*x, *y) == (l, -l)
            )),
            "{cmds:?}"
        );
    }

    /// 工作区不成形（右 ≤ 左 / 下 ≤ 上，包括钳制后才塌掉的）按「没有工作区」。
    #[test]
    fn inverted_or_collapsed_work_area_means_none() {
        for work in [
            "[100,0,50,800]",
            "[0,800,1280,0]",
            "[0,0,0,800]",
            "[2147483647,0,2147483646,800]",
        ] {
            let r = decode(&format!(r#"{{"target":-1,"x":1,"y":2,"work":{work}}}"#));
            assert_eq!(r.work, [0; 4], "work={work}");
        }
    }
}

#[cfg(test)]
#[path = "message_handler_shortcut_ownership_tests.rs"]
mod shortcut_ownership_tests;
