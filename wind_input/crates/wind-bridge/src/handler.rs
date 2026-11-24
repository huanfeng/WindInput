//! MessageHandler trait：处理上游命令的接口
//!
//! 与 Go 版本 `bridge.MessageHandler` 对齐。

use wind_ipc::protocol::KeyPayload;

// re-export：FocusLostReason 是 MessageHandler 签名的一部分，实现方（含各处测试桩）
// 都经 `use crate::handler::*` 引入，不 re-export 会让它们各自去写 wind_ipc 路径。
pub use wind_ipc::protocol::FocusLostReason;
pub use wind_ipc::protocol::ModeSwitchSource;

// 同上：诊断快照载荷直接作签名类型，不在此复刻等价结构（改字段时最容易漏中间层）。
pub use wind_ipc::protocol::DiagSnapshotPayload;
pub use wind_ipc::protocol::UiElementPage;

/// 按键事件数据
#[derive(Debug, Clone)]
pub struct KeyEventData {
    pub key_code: u32,
    pub scan_code: u32,
    pub modifiers: u32,
    pub event_type: u8,
    pub toggles: u8,
    pub event_seq: u16,
    pub prev_char: u16,
    /// 输入序列里最近进文档的字符代表值（0 = 不可知）。客户端记账，取值规则见 C++
    /// `InputSequenceChar.h`；服务端只用它的**类别**（`wind_punct::CharClass`）。
    /// 与 `prev_char` 是两个事实：后者仍是数字后智能标点 / 智能符号 press2 守卫的输入。
    pub seq_char: u16,
}

impl From<&KeyPayload> for KeyEventData {
    fn from(p: &KeyPayload) -> Self {
        Self {
            key_code: p.key_code,
            scan_code: p.scan_code,
            modifiers: p.modifiers,
            event_type: p.event_type,
            toggles: p.toggles,
            event_seq: p.event_seq,
            prev_char: p.prev_char,
            seq_char: p.seq_char,
        }
    }
}

/// 状态更新数据（与 Go StatusUpdateData 对齐）
#[derive(Debug, Clone, Default)]
pub struct StatusUpdateData {
    pub chinese_mode: bool,
    pub full_width: bool,
    pub chinese_punct: bool,
    pub toolbar_visible: bool,
    pub caps_lock: bool,
    /// 软键盘面板是否开着。C++ 的吃键判定要用它——中文模式无 input session 时数字键
    /// 本是交还宿主的，软键盘的数字行需要它们被吃下来。
    pub soft_keyboard: bool,
    /// 当前面是**键盘面**（`send_keys`）：按键交还输入法，C++ 不启用软键盘总闸。
    /// 只在 `soft_keyboard` 为真时有意义。
    pub soft_keyboard_keys: bool,
    /// 热键激活的模式（加词 / 临拼 / 特殊 / 生僻字）当前活着。
    ///
    /// C++ 的 `_HasInputSession()` 要用它：这几个模式在 `input.caret.*_via_composition = false`
    /// 时不建 composition，否则 Backspace/Enter/Escape 会被判「无会话」而透传给宿主。
    ///
    /// level-triggered：每一条携带状态的响应都必须带上当前值，DLL 无条件镜像，漏带会清掉活着的会话。
    pub hotkey_session: bool,
    pub icon_label: String,
    pub key_down_hotkeys: Vec<u32>,
    pub key_up_hotkeys: Vec<u32>,
}

/// 提交请求数据（barrier 机制）
#[derive(Debug, Clone)]
pub struct CommitRequestData {
    pub barrier_seq: u16,
    pub trigger_key: u16,
    pub modifiers: u32,
    pub input_buffer: String,
}

/// 提交结果数据（barrier 机制）
#[derive(Debug, Clone)]
pub struct CommitResultData {
    pub barrier_seq: u16,
    pub text: String,
    pub new_composition: String,
    pub mode_changed: bool,
    pub chinese_mode: bool,
}

/// 宿主 composition 的默认占位内容：一个空格。跨语言约定，两侧取值必须一致。
///
/// 用在「有会话、但没有可见编码要放进宿主」的场合：非嵌入模式（编码由候选窗自绘）与联想态。
/// TSF 不接受空组合，故必须放点什么。约定的另一半是光标落在它前面（`caret_pos = 0`），
/// C++ 侧见 `TextService.cpp` 的 `_CompositionCaretFor`。
///
/// 占位字符按应用可配（compat.toml 的 `composition_placeholder = "space" | "zwsp" | "blank"`，出厂给
/// 浏览器配 zwsp：受控 `<input>` 会 trim 掉空格并终止组合）。协调器发占位时一律经
/// [`MessageHandler::composition_placeholder`] 取当前应用的字符，不要直接用本常量。C++ 侧三种字符
/// 都认作占位，见 `wind_tsf/include/CompositionPlaceholder.h`。
pub const COMPOSITION_PLACEHOLDER: &str = " ";

/// 按键事件结果类型
#[derive(Debug, Clone)]
pub enum KeyAction {
    /// 插入文本
    InsertText {
        text: String,
        new_composition: Option<String>,
        mode_changed: bool,
        chinese_mode: bool,
        has_new_composition: bool,
    },
    /// 更新组合
    UpdateComposition { text: String, caret_pos: u32 },
    /// 清除组合
    ClearComposition,
    /// 清除组合，并把当前这个键交还宿主（联想态回车/退格透传）。
    ///
    /// `ClearComposition` 会吃掉这个键，`PassThrough` 不收占位组合，两者各只做了一半。
    /// 本变体只声明意图，手段由宿主自选：Windows TSF 不能事后把 `pfEaten` 翻成 `FALSE`
    /// （不补发 `WM_KEYDOWN` 的宿主会丢键），故吃掉原键后 `SendInput` 重放；macOS
    /// `applyClearComposition` 后 `return false` 即可。
    ClearCompositionThenPassThrough,
    /// 透传给系统
    PassThrough,
    /// 状态更新（携带完整状态含 iconLabel）
    StatusUpdate(StatusUpdateData),
    /// 消费但不处理
    Consumed,
    /// 按键不处理（未匹配）
    NotHandled,
    /// 插入文本并定位光标
    InsertTextWithCursor { text: String, cursor_offset: u32 },
    /// 光标右移 `count` 格（配对跳出 / 智能跳过）。
    ///
    /// 标点配对恒为 1；直通 `ime.pair` 压入的多字符右段按其 `jump_steps` 取值。
    MoveCursorRight { count: u32 },
    /// 删除配对（智能删除）
    DeletePair,
    /// 删除光标前 count 个字符并插入文本（智能符号替换）
    ReplaceBackward { count: u32, text: String },
    /// 持有组合态（智能符号 HoldComposition 方案）：
    /// C++ 端开启组合显示 text，在 timeout_ms 毫秒后自动提交中文；
    /// press2 到来时用 `CommitReplacingHeld` 覆盖组合。
    HoldComposition { text: String, timeout_ms: u32 },
    /// 提交并替换掉 C++ 端 HoldComposition 里待定的中文符号（智能符号 press2 专用）。
    ///
    /// `InsertText` 在 hold 活跃时会把 held 符号并入一起提交（追加），press2 要的是替换；
    /// 两者 IPC 载荷同构，故用独立 action + flags 位显式声明。
    CommitReplacingHeld { text: String, chinese_mode: bool },
    /// 顶屏后开 HoldComposition（has_input + 智能符号 HoldComposition 组合路径）：
    /// 先提交 commit_text（候选/前缀），再将 hold_text（中文标点）放入 TSF 组合态，
    /// timeout_ms 后自动提交中文；press2 与普通 HoldComposition press2 路径一致。
    CommitAndHoldComposition {
        commit_text: String,
        hold_text: String,
        timeout_ms: u32,
    },
    /// 顶码 direct_commit：先真提交 commit_text（顶出文本），余码新组合 deferred_composition
    /// 延迟到 C++ 端触发键 keyup（或 timeout_ms 兜底定时器）才开——照抄真实输入法
    /// commit@keydown/restart@keyup 时序，靠隔一拍消息泵躲开 diff 式宿主整锁合并。
    CommitThenDeferComposition {
        commit_text: String,
        deferred_composition: String,
        timeout_ms: u32,
    },
}

impl KeyAction {
    /// 把应用侧组合串里的拼音音节分隔符换成 `sep`。只改组合串：已承诺上屏的
    /// 正文不动；`UpdateComposition` 的光标位置（字符数）按替换后的长度换算。
    pub fn with_syllable_separator(self, sep: &str) -> KeyAction {
        match self {
            KeyAction::UpdateComposition { text, caret_pos } if text.contains('\'') => {
                KeyAction::UpdateComposition {
                    caret_pos: map_caret_utf16(&text, caret_pos as usize, sep) as _,
                    text: apply_syllable_separator(&text, sep),
                }
            }
            KeyAction::InsertText {
                text,
                new_composition: Some(c),
                mode_changed,
                chinese_mode,
                has_new_composition,
            } if c.contains('\'') => KeyAction::InsertText {
                text,
                new_composition: Some(apply_syllable_separator(&c, sep)),
                mode_changed,
                chinese_mode,
                has_new_composition,
            },
            KeyAction::CommitThenDeferComposition {
                commit_text,
                deferred_composition,
                timeout_ms,
            } if deferred_composition.contains('\'') => KeyAction::CommitThenDeferComposition {
                commit_text,
                deferred_composition: apply_syllable_separator(&deferred_composition, sep),
                timeout_ms,
            },
            other => other,
        }
    }

    /// 非 app_inline（候选窗自行显示 preedit）时，应用侧组合串替换为单个占位字符、光标置前。
    /// 目的：保留一段组合串供应用上报 caret 坐标（候选窗定位），但不在应用内显示真实编码
    /// （避免与候选窗 preedit 重复）。对齐 Go 版"模拟空格 + 光标移前"。
    ///
    /// `placeholder` 由调用方按当前应用给出（空格 / ZWSP / U+2800，见 [`COMPOSITION_PLACEHOLDER`]）。
    pub fn with_composition_placeholder(self, placeholder: &str) -> KeyAction {
        match self {
            // `!text.is_empty()` 守卫必须留着：空组合区意味着这一刻不该有组合区。本函数只是
            // preedit 的显示策略，不决定谁需要占位；热键模式的占位由各自 `enter_*` 显式发出
            // （见 `enter_add_word_mode`），出厂 app_inline 下它们也走不到这里。
            KeyAction::UpdateComposition { text, .. } if !text.is_empty() => {
                KeyAction::UpdateComposition {
                    text: placeholder.to_string(),
                    caret_pos: 0,
                }
            }
            KeyAction::InsertText {
                text,
                new_composition: Some(c),
                mode_changed,
                chinese_mode,
                has_new_composition,
            } if !c.is_empty() => KeyAction::InsertText {
                text,
                new_composition: Some(placeholder.to_string()),
                mode_changed,
                chinese_mode,
                has_new_composition,
            },
            // direct_commit 顶码的余码组合同样是待重开的编码串，要换占位；commit_text 是正文，不能动。
            KeyAction::CommitThenDeferComposition {
                commit_text,
                deferred_composition,
                timeout_ms,
            } if !deferred_composition.is_empty() => KeyAction::CommitThenDeferComposition {
                commit_text,
                deferred_composition: placeholder.to_string(),
                timeout_ms,
            },
            // HoldComposition 系刻意不在此列：组合内容是要显示的中文符号本身，不是编码串。
            other => other,
        }
    }
}

/// 焦点数据
///
/// `Debug` 手写：`window_title` 是用户数据，调试输出只给长度（`{:?}` 打进日志也不泄露原文）。
#[derive(Clone)]
pub struct FocusData {
    pub x: i32,
    pub y: i32,
    pub height: i32,
    pub composition_start_x: i32,
    pub composition_start_y: i32,
    pub client_token: u64,
    pub input_scope_mask: u64,
    pub disabled: bool,
    pub reason: u8,
    /// 上面那组坐标的来源（`wind_ipc::protocol::caret_source::*`）。
    ///
    /// `OnSetFocus` 不是按键上下文，同步 edit session 必被宿主拒绝，回退链交出的是**跨窗口的**
    /// Win32 光标。焦点气泡就锚在这组坐标上，故必须能分辨来源，详见 `FocusGainedPayload`。
    pub caret_source: i32,
    /// 宿主 app 的 bundle id（仅 macOS，由 `.app` 随焦点事件上报；Windows 恒空串）。
    ///
    /// macOS 服务进程无法按 pid 反查进程名，服务端小写后填进 `pid_names` 缓存，之后两平台路径一致。
    pub bundle_id: String,
    /// 焦点所在顶层窗口的类名；拿不到时为空串（旧 DLL、macOS 暂未上报）。
    ///
    /// 同一进程名（如 `explorer.exe`）可承载语义相反的焦点，只有窗口类能分开，见
    /// `AppCompat::initial_mode_applies_to_window`。空串 = 不知道焦点在哪，消费端保持现状。
    pub window_class: String,
    /// 同一个顶层窗口的标题，供 compat.toml 的 `title` 条件匹配；拿不到时为空串（旧 DLL、服务端
    /// 未推开采集——没有标题规则时 DLL 不取标题，见 `CONFIG_KEY_COMPAT_TITLE_MATCH`）。
    ///
    /// 用户数据（网页标题、文件名）：任何级别的日志只记长度，不记原文。
    pub window_title: String,
}

impl std::fmt::Debug for FocusData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FocusData")
            .field("x", &self.x)
            .field("y", &self.y)
            .field("height", &self.height)
            .field("composition_start_x", &self.composition_start_x)
            .field("composition_start_y", &self.composition_start_y)
            .field("client_token", &self.client_token)
            .field("input_scope_mask", &self.input_scope_mask)
            .field("disabled", &self.disabled)
            .field("reason", &self.reason)
            .field("caret_source", &self.caret_source)
            .field("bundle_id", &self.bundle_id)
            .field("window_class", &self.window_class)
            .field("window_title_len", &self.window_title.chars().count())
            .finish()
    }
}

/// 光标位置数据
#[derive(Debug, Clone, Copy)]
pub struct CaretData {
    pub x: i32,
    pub y: i32,
    pub height: i32,
    pub composition_start_x: i32,
    pub composition_start_y: i32,
    /// 坐标来源（`wind_ipc::protocol::caret_source::*`）。
    ///
    /// TSF 域的坐标出自当前 context，GUI 域的是跨窗口的 Win32 光标。旧 DLL 与 macOS 短包落 `UNKNOWN`。
    pub source: i32,
    /// 整个组合 range 的包围矩形 `(left, top, right, bottom)`，屏幕物理坐标。
    ///
    /// 它答「组合占了多大一块」，`composition_start_*` 答「组合从哪开始」；组合换行后只有本字段
    /// 能看出跨行（见 docs/architecture/caret-position-host-compat-notes.md 的「二·六」）。
    ///
    /// `None` = 本帧没有：旧 DLL / macOS 短包，或 `GetTextExt` 取不到（含 `TS_E_NOLAYOUT`，即布局未算完）。
    pub composition_rect: Option<(i32, i32, i32, i32)>,
}

/// 把拼音编码显示串里的音节分隔符 `'` 换成 `sep`（`ui.candidate.syllable_separator`，可为空串）。
/// 只用于**显示**：引擎内部缓冲与切分不动。
pub fn apply_syllable_separator(text: &str, sep: &str) -> String {
    text.replace('\'', sep)
}

/// [`apply_syllable_separator`] 之后，原串光标位置换算到新串。
///
/// 单位是 **UTF-16 码元**（与 `KeyAction::UpdateComposition::caret_pos` 一致）：前缀里有
/// 扩展 B 区汉字（2 码元）、分隔符是非 BMP 字符（如 emoji）时，按字符数算会偏。
pub fn map_caret_utf16(text: &str, caret: usize, sep: &str) -> usize {
    let (mut units, mut quotes) = (0usize, 0usize);
    for c in text.chars() {
        if units >= caret {
            break;
        }
        units += c.len_utf16();
        if c == '\'' {
            quotes += 1;
        }
    }
    caret - quotes + quotes * sep.encode_utf16().count()
}

/// MessageHandler trait：协调器实现此接口处理各种事件
pub trait MessageHandler: Send + Sync {
    /// 处理按键事件
    fn handle_key_event(&self, data: &KeyEventData) -> KeyAction;

    /// 应用侧组合串是否使用占位空格（候选窗显示 preedit 的非 app_inline 模式）。默认否（app_inline）。
    fn preedit_uses_placeholder(&self) -> bool {
        false
    }

    /// 当前应用的组合占位字符。默认空格 [`COMPOSITION_PLACEHOLDER`]；协调器按焦点应用
    /// 的 compat 规则 `composition_placeholder` 覆盖。
    fn composition_placeholder(&self) -> &'static str {
        COMPOSITION_PLACEHOLDER
    }

    /// 刚处理完的这次按键之后，是否处于「无占位的联想会话」（`assoc_placeholder = "none"`）。
    /// 为真时 bridge 在上屏应答里置 `COMMIT_FLAG_ASSOC_SESSION`，让 DLL **同步**获知
    /// 该继续转发按键（宿主里没有组合可供 `HasActiveComposition()` 判断）。默认否。
    fn assoc_session_hint(&self) -> bool {
        false
    }

    /// 处理按键并按 preedit 显示策略后处理组合串（bridge 入口应调用此方法）。
    fn handle_key_event_policed(&self, data: &KeyEventData) -> KeyAction {
        let action = self.handle_key_event(data);
        if self.preedit_uses_placeholder() {
            action.with_composition_placeholder(self.composition_placeholder())
        } else if let Some(sep) = self.syllable_separator() {
            action.with_syllable_separator(&sep)
        } else {
            action
        }
    }

    /// 当前该用什么字符显示拼音音节分隔符；`None` = 保持默认 `'`（含非拼音场合）。
    /// 占位模式下组合串已换成占位符，无需再换。
    fn syllable_separator(&self) -> Option<String> {
        None
    }

    /// 新客户端连接建立时调用（仅桥接主通道，`pid` 取自 `GetNamedPipeClientProcessId`
    /// ——即对端宿主进程，因为 TSF DLL 与宿主同进程）。默认 no-op。
    ///
    /// 用途：服务重启或管道重连时宿主可能早已在前台、不会再来 `FOCUS_GAINED`，这是唯一能拿到
    /// 其 pid 去解析 per-app 兼容规则的时机。
    fn handle_client_connected(&self, _pid: u32) {}

    /// UDS 请求连接断开（macOS / Linux，`server_unix` 在连接线程收尾时调）。默认 no-op。
    ///
    /// Linux 用它兜「addon 断线」：自绘菜单开着时 addon 进程没了，没人会再报关闭，
    /// `menu_open` 留成 true 就会吞掉下一个 addon 实例送来的方向键 / 回车 / Esc。
    fn handle_client_disconnected(&self) {}

    /// Linux addon：自绘菜单打开期间的指针事件（[`wind_ipc::protocol::CMD_MENU_POINTER`]）。
    /// `event` 见 `protocol::menu_pointer`；`button` 为 X11 按键号；`(x, y)` 为屏幕坐标。默认空。
    fn handle_menu_pointer(&self, _event: u32, _button: u32, _x: i32, _y: i32) {}

    /// Linux addon：工具栏 / 软键盘上的指针事件（[`wind_ipc::protocol::CMD_PANEL_POINTER`]）。
    /// `kind` 为浮层 kind，`event` 见 `protocol::panel_pointer`，`button` 为 X11 按键号，
    /// `(x, y)` 为位图内坐标。给默认实现是刻意的：测试夹具等不必各写一遍空函数。
    fn on_panel_pointer(&self, kind: u32, event: u32, button: u32, x: i32, y: i32) {
        tracing::debug!(
            "未处理的面板指针事件 kind={kind} event={event} button={button} x={x} y={y}"
        );
    }

    /// 处理焦点获取（返回状态用于 ActivationStatusPush）
    fn handle_focus_gained(&self, data: &FocusData) -> Option<StatusUpdateData>;

    /// 处理焦点丢失。
    ///
    /// `client_token` 为发出该失焦的 TSF 实例（0 = 旧 DLL 未携带，实现方应保守放行）。
    /// 实现方必须据此做归属校验：跨宿主切换时本命令会晚于新宿主的 focus_gained 到达。
    ///
    /// `reason` 区分四种语义不同的失焦，不可一刀切按「离开输入法」处理（[`FocusLostReason::CtxLost`]
    /// 来自 DocMgr 噪声层，清输入态会导致首字符直接上屏）。后果矩阵见 [`FocusLostReason`]。
    fn handle_focus_lost(&self, client_token: u64, reason: FocusLostReason);

    /// 处理 IME 激活（返回状态用于 ActivationStatusPush）
    fn handle_ime_activated(&self, client_token: u64) -> Option<StatusUpdateData>;

    /// 发送方进程是否是「后台客户端」：别的进程正持有键盘焦点、且它就是当前活动客户端。
    ///
    /// 这类客户端的 `IME_ACTIVATED` / 光标上报是初始化噪声，不是焦点转移（如 SearchHost 的
    /// WebView2 子进程首字时才初始化 TSF，会抢走活动客户端与 host render 写帧目标）。
    /// 默认 `false` = 不判。只拦 `IME_ACTIVATED` 与 `CARET_UPDATE`；`FOCUS_GAINED` 照常处理。
    fn is_background_client(&self, _pid: u32) -> bool {
        false
    }

    /// 后台客户端的 `IME_ACTIVATED`：只给它回推一次激活态（让它的 DLL 状态对齐），
    /// **不**改活动客户端、不重置输入模式。判据见 [`Self::is_background_client`]。
    fn handle_background_ime_activated(&self, _client_token: u64) {}

    /// 处理 IME 停用。`client_token` 语义同 [`Self::handle_focus_lost`]。
    fn handle_ime_deactivated(&self, client_token: u64);

    /// 处理模式通知
    fn handle_mode_notify(&self, flags: u32);

    /// 处理模式切换（返回状态和可选的待提交文本）
    fn handle_toggle_mode(&self) -> (Option<StatusUpdateData>, String);

    /// 处理系统模式切换（返回状态和可选的待提交文本）
    ///
    /// `source` 是发起方（宿主写 compartment / 按键兜底 / 功能菜单）；`ctrl_held` 是这次
    /// compartment 变化发生时 Ctrl 是否被按住，由 DLL 现场采样。
    ///
    /// 两者构成 `ignore_host_ime_switch` 的判据：只有「compartment 来源 + Ctrl 未按住」才是宿主
    /// 自作主张，其余一律照办。除这一条外不要按 source 分支。
    fn handle_system_mode_switch(
        &self,
        chinese_mode: bool,
        source: ModeSwitchSource,
        ctrl_held: bool,
    ) -> (Option<StatusUpdateData>, String);

    /// 处理菜单命令（返回状态更新）
    fn handle_menu_command(&self, command: &str) -> Option<StatusUpdateData>;

    /// 处理组合终止
    fn handle_composition_terminated(&self);

    /// 处理光标位置更新
    fn handle_caret_update(&self, data: &CaretData);

    /// focus_gained 随包携带的 caret：只更新坐标缓存，不得触发任何显示决策。
    ///
    /// 焦点事件的坐标未必是宿主 reflow 后的权威值，拿它满足首显闸门会让候选窗先闪一下再跳走；
    /// 权威坐标由 OnLayoutChange 之后的 caret_update 送达。
    ///
    /// 刻意不给默认实现：`DeferredHandler` 这类装饰器逐个方法转发，有默认实现时装饰器不重写也能
    /// 编译，真正的实现就收不到消息。
    fn handle_focus_gained_caret(&self, data: &CaretData);

    /// 处理光标待定（composition 刚启动，真正 caret 在 reflow 后到达）
    fn handle_caret_pending(&self);

    /// 处理首显试探采样（`CMD_CARET_PROBE`）。
    ///
    /// 首帧 reflow 期间 DLL 每次 layout change 采一次坐标发来，这些采样未必可信（WPS 前两条
    /// 仍是旧坐标）。实现方自行判定哪条可采纳，且默认应当忽略：不启用快速首显的宿主必须保持
    /// 「等 reflow 权威坐标」的行为。
    ///
    /// 必需方法，不给默认实现，理由同 [`Self::handle_focus_gained_caret`]。
    fn handle_caret_probe(&self, data: &CaretData);

    /// 处理选区变化
    fn handle_selection_changed(&self, prev_char: u16);

    /// 处理提交请求（barrier 机制）
    fn handle_commit_request(&self, data: &CommitRequestData) -> Option<CommitResultData>;

    /// 处理 Host Render 失败上报（DLL 侧初始化/SHM 映射失败等，reason 为失败码）。
    /// setup 应答与断线清理由 bridge 服务器直接经 HostRenderManager 完成，不经此 trait；
    /// 此回调仅用于让协调器记录/告警。默认空实现。
    fn handle_host_render_failed(&self, _reason: u32) {}

    /// 显示功能主菜单（任务栏输入法指示右键）。x/y 为屏幕坐标；
    /// i32::MIN 表示坐标缺失（由 UI 取光标位置）。默认空实现。
    fn handle_show_context_menu(&self, _x: i32, _y: i32) {}

    /// 处理 TSF 侧上报的英文模式输入统计（CMD_INPUT_STATS，异步，无响应）。
    /// chars = a-z/A-Z 字符数; digits = 数字键(0-9/numpad); puncts = 符号键; spaces = 空格键。
    /// 对齐 Go `RecordTSFEnglish`。默认空实现（无统计的 handler 静默忽略）。
    fn handle_english_stats(&self, _chars: u32, _digits: u32, _puncts: u32, _spaces: u32) {}

    /// 鼠标点选候选（darwin .app / Windows host-render DLL，页内下标）。
    /// 负值为翻页按钮：-1 上页 / -2 下页（与 SHM 命中矩形及 C++ _OnMouseClick 约定一致）。默认空。
    fn handle_candidate_select(&self, _page_local_index: i32) {}

    /// host 候选框鼠标滚轮（delta 为 WHEEL_DELTA 倍数，正=上滚）。
    ///
    /// 协调器把它实现为「上下键调整高亮项」（到页边界翻到相邻页），两平台同一实现，
    /// 见 `Coordinator::handle_candidate_scroll`。此处的空实现只服务于测试夹具。
    fn handle_candidate_scroll(&self, _delta: i32) {}

    /// darwin: .app 鼠标 hover 候选（页内下标，-1=无）。默认空。
    fn handle_candidate_hover(&self, _page_local_index: i32) {}

    /// 扩展信封（`CMD_EXT`）：低频消息的统一入口。`kind` 见 `wind_ipc::protocol::ext_kind`，
    /// `body` 是不透明字节（通常是 JSON），由实现方按 kind 解析。
    ///
    /// 给默认实现是刻意的：信封的前提是「未知 kind 安静忽略」，没实现与不认识 kind 语义相同。
    fn handle_ext(&self, kind: &str, _body: &[u8]) {
        tracing::debug!("未处理的扩展消息 kind={kind}");
    }

    /// darwin: 查询功能菜单，返回已编码的 `CmdMenuShow` 帧字节（响应 `CmdShowContextMenu`）。
    /// `simplified=true` 为 IMK 输入源菜单用的精简树(无子菜单)，false 为候选框右键/菜单栏
    /// 指示器用的完整树(带子菜单)。`.app` 端解码为原生 NSMenu。默认返回空 `Vec`。
    fn query_menu_encoded(&self, _simplified: bool) -> Vec<u8> {
        Vec::new()
    }

    /// darwin: .app 回传统一菜单项选择（菜单 id）。默认空。
    fn handle_menu_action_id(&self, _id: i32) {}

    /// darwin: .app 上报前台上下文（app bundle id / 窗口标题 / 选中文本），供命令直通车
    /// app()/title()/sel() 取值。聚焦时快照，缺 AX/IMKit 支持的字段为空串。默认空实现。
    fn handle_front_context(&self, _app: &str, _title: &str, _sel: &str) {}

    /// 返回当前权威模式 (chinese_mode, full_width)，供 FocusGained 同步路径回传 ModePush。
    /// `client_token`（PID<<32|instance）标识焦点进程：state_scope="app" 时按进程切换记忆状态。
    /// 必须极轻量（仅锁+内存查询），不得有任何阻塞/跨进程调用——DLL 正同步阻塞等本值。
    /// 与 Go `MessageHandler.GetCurrentMode` 对齐。默认返回中文模式（安全默认）。
    ///
    /// `window_class`：焦点顶层窗口类，语义同 [`FocusData::window_class`]，用于跳过 shell
    /// 过渡窗口的初始模式套用。`window_title`：同 [`FocusData::window_title`]——规则按
    /// (进程, 类名, 标题) 解析，同步段与重型段必须拿同一个窗口算，否则带标题的规则会让两段的
    /// 初始模式各算各的（先回一个、再被推送改回来，即「闪」）。
    ///
    /// 这是「按应用套用初始模式」的第二个落点，与重型段 `handle_focus_gained` 各算各的（本方法
    /// 先执行）。两处的门控条件必须同步改。
    fn get_current_mode(
        &self,
        _client_token: u64,
        _window_class: &str,
        _window_title: &str,
    ) -> (bool, bool, bool) {
        (true, false, true)
    }

    /// compartment 禁用态变更（不换焦点）上报。默认空实现。
    fn handle_input_state_report(&self, _pid: u32, _disabled: bool, _reason: u8, _mask: u64) {}

    /// DLL 放行给宿主的用户按键（`CMD_KEY_OBSERVED`），只为扩展程序上报。**不得**参与任何
    /// 输入决策——它只在有扩展订阅时才到达。默认空实现。
    fn handle_key_observed(&self, _key_code: u32, _modifiers: u32) {}

    /// 诊断快照上报（焦点窗口链 / 前台窗口 / TSF 上下文实例 id）。
    ///
    /// 仅在服务端经 `CONFIG_KEY_DIAG_SNAPSHOT` 推开采集后才会到达；HUD 关闭时 DLL 一条不发。
    /// 纯观测数据，不得参与任何输入决策（否则会出现「诊断开着才正常」）。默认空实现。
    fn handle_diag_snapshot(&self, _snap: &DiagSnapshotPayload) {}

    // ── TSF UI-less（宿主自绘候选）三件套，见 wind-ipc `CMD_UIELEMENT_*` 注释 ──

    /// DLL 报告某进程的 UIElement 状态。两个判据分开传，因为消费端对它们的态度不同：
    /// - `host_draws`：宿主**声明**接管候选绘制（`pbShow=FALSE` / UI-less 线程）——事实，无覆盖；
    /// - `host_reads`：宿主没声明却把候选串读走了——推断，**默认不据此收窗**，
    ///   compat 写 `host_drawn_candidates = true` 才生效（opt-in）。
    ///
    /// 消费方按 pid 记账，该进程聚焦期间不弹自己的候选窗。默认空实现。
    fn handle_uielement_state(&self, _pid: u32, _host_draws: bool, _host_reads: bool) {}

    /// 每个按键事件的**发送进程** pid（管道对端）。按键是「谁在输入」的最强证据：
    /// 游戏这类宿主常常没有可编辑 TSF 上下文，`focus_gained` 一次都不会来，光靠焦点事件
    /// 记的 pid 会停在上一个进程上。默认空实现；实现方只做一次原子写，不得在此做重活。
    fn note_key_source_pid(&self, _pid: u32) {}

    /// DLL 拉取候选快照（同步）。默认空快照：宿主看到「0 条候选」而不是解析失败。
    fn uielement_page(&self) -> UiElementPage {
        UiElementPage::default()
    }

    /// 宿主经 `ITfCandidateListUIElementBehavior` 发来的候选操作（`UIELEMENT_ACTION_*`）。默认空实现。
    fn handle_uielement_action(&self, _action: u32, _arg: u32) {}
}

#[cfg(test)]
#[path = "handler_placeholder_tests.rs"]
mod placeholder_tests;

#[cfg(test)]
mod focus_data_debug_tests {
    use super::FocusData;

    /// 窗口标题是用户数据：`{:?}` 只输出长度，不输出原文。
    #[test]
    fn debug_output_never_contains_the_window_title() {
        let d = FocusData {
            x: 0,
            y: 0,
            height: 0,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: 1,
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: 0,
            bundle_id: String::new(),
            window_class: "Chrome_WidgetWin_1".into(),
            window_title: "机密文件 - 网页".into(),
        };
        let s = format!("{d:?}");
        assert!(!s.contains("机密"), "{s}");
        assert!(s.contains("window_title_len: 9"), "{s}");
        assert!(s.contains("Chrome_WidgetWin_1"), "类名照常输出");
    }
}

#[cfg(test)]
mod syllable_separator_tests {
    use super::*;

    /// 光标是 UTF-16 码元：分隔符换成更长 / 更短 / 非 BMP，光标都要落在同一个音节边界上。
    #[test]
    fn caret_follows_separator_in_utf16_units() {
        let t = "xiao'peng";
        // 光标在第一个 ' 之后（5 个码元）
        assert_eq!(map_caret_utf16(t, 5, ""), 4, "去掉分隔符少 1");
        assert_eq!(map_caret_utf16(t, 5, "'"), 5);
        assert_eq!(map_caret_utf16(t, 5, "·"), 5, "BMP 单字符与 ' 同长");
        assert_eq!(map_caret_utf16(t, 5, " "), 5);
        assert_eq!(map_caret_utf16(t, 5, "😀"), 6, "非 BMP 分隔符占 2 个码元");
        // 光标在分隔符之前：不受影响
        assert_eq!(map_caret_utf16(t, 4, "😀"), 4);
        // 前缀里有扩展 B 区字（2 码元）：按码元数而不是字符数
        let t = "𠀀xiao'peng";
        assert_eq!(map_caret_utf16(t, 7, ""), 6);
        assert_eq!(map_caret_utf16(t, 7, "😀"), 8);
    }

    #[test]
    fn with_syllable_separator_maps_caret_and_text() {
        let act = KeyAction::UpdateComposition {
            text: "xiao'peng".into(),
            caret_pos: 9,
        };
        match act.with_syllable_separator("") {
            KeyAction::UpdateComposition { text, caret_pos } => {
                assert_eq!(text, "xiaopeng");
                assert_eq!(caret_pos, 8, "光标在末尾仍在末尾");
            }
            other => panic!("{other:?}"),
        }
    }
}
