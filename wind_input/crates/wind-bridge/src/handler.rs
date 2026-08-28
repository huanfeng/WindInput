//! MessageHandler trait：处理上游命令的接口
//!
//! 与 Go 版本 `bridge.MessageHandler` 对齐。

use wind_ipc::protocol::KeyPayload;

// re-export：FocusLostReason 是 MessageHandler 签名的一部分，实现方（含各处测试桩）
// 都经 `use crate::handler::*` 引入，不 re-export 会让它们各自去写 wind_ipc 路径。
pub use wind_ipc::protocol::FocusLostReason;

// 同上：诊断快照载荷直接作签名类型，不再在此复刻一份等价结构——它有 14 个字段，
// 复刻等于给「两处独立事实」再开一个入口，而这类结构改动时最容易漏的就是中间层。
pub use wind_ipc::protocol::DiagSnapshotPayload;

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

/// 宿主 composition 的**占位内容**：一个空格。
///
/// # 它是一条跨语言的约定，不是随便挑的字符
///
/// 用在两种「输入法有会话、但没有可见编码要放进宿主」的场合：
///   - 非嵌入模式（编码由候选窗自绘，宿主里不该重复显示一遍）
///   - 联想态（压根没有编码，只是要让宿主继续转发按键）
///
/// TSF 不接受空组合，故必须放点什么；一个空格是最不打扰的选择。
///
/// ★ **约定的另一半是「光标落在它前面」**（`caret_pos = 0`）。少了这一半，用户看到的
/// 插入点会跳到那个空格**之后**——在兼容良好的宿主里表现为光标凭空右移一格，很突兀。
/// 正常打字时下一键的 `UpdateComposition` 会立刻把光标拉回 0，所以这个缺陷长期被掩盖；
/// 联想态没有「下一键」，组合就那么挂着，才把它暴露出来（2026-08-16 用户反馈）。
///
/// C++ 侧据此把「组合内容恰为本值」的情形一律按 `caret_pos = 0` 开组合，见
/// `TextService.cpp` 的 `_CompositionCaretFor`。**两侧取值必须一致**，改这里要同步改那里。
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
    /// 清除组合，**并把当前这个键交还宿主**（联想态回车/退格透传）。
    ///
    /// # 为什么不能用 `ClearComposition` 或 `PassThrough` 表达
    ///
    /// 两者各只做了一半，而这里两件事都要：
    /// - `ClearComposition` 收了组合，但**吃掉**这个键（Windows/Android 语义如此；
    ///   macOS 只在 `hostShortcut` 时才交还）。用户按回车要的是换行，不是「关个窗」。
    /// - `PassThrough` 交还了键，却**不收组合**——联想态挂着占位组合
    ///   （`handle_assoc::ASSOC_COMPOSITION`），组合会悬在宿主里，编码栏留着「联想输入」。
    ///
    /// # 实现手段由各宿主自己选，本变体只声明意图
    ///
    /// - **Windows TSF**：`EndComposition()` 后**不能**把 `pfEaten` 吐成 `FALSE`——
    ///   `OnTestKeyDown` 已按「有会话」吃了这个键，翻转会让不补发 `WM_KEYDOWN` 的宿主
    ///   （EverEdit 等）直接丢键。故沿用 hold / 配对跳出那条已验证的路：吃掉原键，
    ///   `SendInput` 重放一个干净的按键，宿主先看到收口后的文档、再看到普通按键。
    /// - **macOS IMKit**：无前置闸门，`applyClearComposition` 后 `return false` 即可。
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
    /// 提交并**替换**掉 C++ 端 HoldComposition 里待定的中文符号（智能符号 press2 专用）。
    ///
    /// 与 `InsertText` 的唯一区别就是这个替换语义：`InsertText` 在 hold 活跃时会把 held
    /// 符号并入前缀一起提交（追加），press2 要的却是把「。」换成「.」。两者在 IPC 载荷上
    /// 本来完全同构，C++ 端无从分辨，故用独立 action + flags 位显式声明。
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
    /// 非 app_inline（候选窗自行显示 preedit）时，应用侧组合串替换为单个占位空格、光标置前。
    /// 目的：保留一段组合串供应用上报 caret 坐标（候选窗定位），但不在应用内显示真实编码
    /// （避免与候选窗 preedit 重复）。对齐 Go 版"模拟空格 + 光标移前"。
    pub fn with_composition_placeholder(self) -> KeyAction {
        match self {
            KeyAction::UpdateComposition { text, .. } if !text.is_empty() => {
                KeyAction::UpdateComposition {
                    text: COMPOSITION_PLACEHOLDER.to_string(),
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
                new_composition: Some(COMPOSITION_PLACEHOLDER.to_string()),
                mode_changed,
                chinese_mode,
                has_new_composition,
            },
            // direct_commit 顶码的余码组合与上面 InsertText 的 new_composition 同性质（都是
            // 待重开的编码串），只是延迟到 keyup 才开，故同样要换占位空格——漏了会让真实编码
            // 直接写进宿主 composition，在独立编码栏（candidate_top）下与候选窗 preedit 重复显示。
            // commit_text 是已承诺上屏的正文，不能动。
            KeyAction::CommitThenDeferComposition {
                commit_text,
                deferred_composition,
                timeout_ms,
            } if !deferred_composition.is_empty() => KeyAction::CommitThenDeferComposition {
                commit_text,
                deferred_composition: COMPOSITION_PLACEHOLDER.to_string(),
                timeout_ms,
            },
            // CommitAndHoldComposition / HoldComposition 刻意不在此列：它们的组合内容是中文符号
            // 本身（承诺要在宿主显示的正文），不是编码串，换成占位空格会把符号弄丢。
            other => other,
        }
    }
}

/// 焦点数据
#[derive(Debug, Clone)]
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
    /// 宿主 app 的 bundle id（**仅 macOS**，由 `.app` 随焦点事件上报；Windows 恒空串）。
    ///
    /// macOS 的服务进程无法像 Windows 那样按 pid 反查进程名，「当前是哪个应用」只能由
    /// `.app` 告知。服务端小写后填进 `pid_names` 缓存，compat.toml 规则匹配与 per-app
    /// 中英记忆都从那里取名——两平台在缓存之后的路径完全一致。
    pub bundle_id: String,
    /// 焦点所在**顶层窗口**的类名；拿不到时为空串（旧 DLL、macOS 暂未上报）。
    ///
    /// 存在的理由：per-app 规则的身份是进程映像名，而 `explorer.exe` 一个名字同时承载
    /// 桌面与任务栏 / Alt+Tab / 溢出区两类语义相反的焦点。只有窗口类能把它们分开，
    /// 见 `AppCompat::initial_mode_applies_to_window`。
    ///
    /// ⚠ 空串的语义是「不知道焦点在哪」。消费端据此**保持现状**（不重算初始模式）；
    /// 未配作用域的进程不受影响，故旧 DLL / macOS 上一切照旧。
    pub window_class: String,
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
    /// **不同来源不是同一件东西**：TSF 域的坐标出自当前 context，GUI 域的是跨窗口的 Win32 光标。
    /// 旧 DLL 与 macOS 短包给不出该值，落 `UNKNOWN`，此时按既有行为处理即可。
    pub source: i32,
}

/// MessageHandler trait：协调器实现此接口处理各种事件
pub trait MessageHandler: Send + Sync {
    /// 处理按键事件
    fn handle_key_event(&self, data: &KeyEventData) -> KeyAction;

    /// 应用侧组合串是否使用占位空格（候选窗显示 preedit 的非 app_inline 模式）。默认否（app_inline）。
    fn preedit_uses_placeholder(&self) -> bool {
        false
    }

    /// 处理按键并按 preedit 显示策略后处理组合串（bridge 入口应调用此方法）。
    fn handle_key_event_policed(&self, data: &KeyEventData) -> KeyAction {
        let action = self.handle_key_event(data);
        if self.preedit_uses_placeholder() {
            action.with_composition_placeholder()
        } else {
            action
        }
    }

    /// 新客户端连接建立时调用（仅桥接主通道，`pid` 取自 `GetNamedPipeClientProcessId`
    /// ——即对端宿主进程，因为 TSF DLL 与宿主同进程）。默认 no-op。
    ///
    /// 用途：服务重启或管道抖动重连时，若目标宿主早已是前台窗口，不会有新的
    /// `FOCUS_GAINED` 促发 per-app 兼容规则解析——旧连接断开、新连接建立就是这种情况下
    /// **唯一**能拿到该宿主 pid 的时机，借它提前解析（`caret_offset_*`/`caret_use_top`
    /// 等），否则用户得手动切一次焦点新配置才生效。
    fn handle_client_connected(&self, _pid: u32) {}

    /// 处理焦点获取（返回状态用于 ActivationStatusPush）
    fn handle_focus_gained(&self, data: &FocusData) -> Option<StatusUpdateData>;

    /// 处理焦点丢失。
    ///
    /// `client_token` 为发出该失焦的 TSF 实例（0 = 旧 DLL 未携带，实现方应保守放行）。
    /// 实现方**必须**据此做归属校验：DLL 的 `OnKillThreadFocus` 比 DocMgr 级失焦晚约
    /// 100ms 才发本命令，跨宿主切换时它必然晚于新宿主的 focus_gained 到达。
    ///
    /// `reason` 区分四种语义完全不同的「失焦」，实现方**不可一刀切**地全部按「离开输入法」
    /// 处理——尤其 [`FocusLostReason::CtxLost`] 来自 DocMgr 噪声层，清输入态会复发
    /// 「首字符直接上屏」。各 reason 的后果矩阵见 [`FocusLostReason`]。
    fn handle_focus_lost(&self, client_token: u64, reason: FocusLostReason);

    /// 处理 IME 激活（返回状态用于 ActivationStatusPush）
    fn handle_ime_activated(&self, client_token: u64) -> Option<StatusUpdateData>;

    /// 处理 IME 停用。`client_token` 语义同 [`Self::handle_focus_lost`]。
    fn handle_ime_deactivated(&self, client_token: u64);

    /// 处理模式通知
    fn handle_mode_notify(&self, flags: u32);

    /// 处理模式切换（返回状态和可选的待提交文本）
    fn handle_toggle_mode(&self) -> (Option<StatusUpdateData>, String);

    /// 处理系统模式切换（返回状态和可选的待提交文本）
    fn handle_system_mode_switch(&self, chinese_mode: bool) -> (Option<StatusUpdateData>, String);

    /// 处理菜单命令（返回状态更新）
    fn handle_menu_command(&self, command: &str) -> Option<StatusUpdateData>;

    /// 处理组合终止
    fn handle_composition_terminated(&self);

    /// 处理光标位置更新
    fn handle_caret_update(&self, data: &CaretData);

    /// focus_gained 随包携带的 caret：**只更新坐标缓存，不得触发任何显示决策**。
    ///
    /// 与 [`Self::handle_caret_update`] 的区别是本方法没有副作用——不消费首显等待、
    /// 不锚定组合起点、不 reshow。焦点事件带来的坐标是「当前这一刻」的，未必是宿主
    /// reflow 之后的权威值；拿它去满足首显闸门会让候选窗先在中间位置闪一下再跳走
    /// （Excel 单元格激活实测：1025,687 → **1369,1036** → 1590,1092，中间那个就是
    /// 焦点事件带来的）。真正的权威坐标由 OnLayoutChange 之后的 caret_update 送达。
    ///
    /// **刻意不给默认实现**。曾经给过（委托 `handle_caret_update`），结果 `DeferredHandler`
    /// 这类装饰器没重写它 → 吃默认实现 → 默认实现调装饰器自己的 `handle_caret_update`
    /// → 转发回内层旧路径，真正的实现从未被调用。每一步都合法，编译器全程沉默，
    /// 最后靠真机日志才发现。设为必需方法，让编译器逼每个实现者显式表态。
    fn handle_focus_gained_caret(&self, data: &CaretData);

    /// 处理光标待定（composition 刚启动，真正 caret 在 reflow 后到达）
    fn handle_caret_pending(&self);

    /// 处理首显试探采样（`CMD_CARET_PROBE`）。
    ///
    /// 首帧 reflow 期间 DLL 每次 layout change 采一次坐标发来，**这些采样未必可信**：
    /// 实测 WPS 前两条仍是上一轮的旧坐标，EverEdit 第一条就已正确。实现方要自行判定哪条
    /// 可采纳（例如「与上一轮权威坐标不同即视为已 reflow」），并且**默认应当忽略**——
    /// 不启用快速首显的宿主必须保持原有「等 reflow 权威坐标」的行为，一字不差。
    ///
    /// ⚠ **必需方法，不给默认实现**——理由同 [`Self::handle_focus_gained_caret`]：本 trait 有
    /// `DeferredHandler` 这类装饰器，它们逐个方法转发；一旦提供默认空实现，装饰器不重写也能
    /// 编译通过，于是真正的实现永远收不到消息。本方法就这么栽过一次：IPC 到达 150 次、解码
    /// 全部成功，coordinator 里却一条日志都没有，查了好几轮才定位到是装饰器吃了默认实现。
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
    /// **给默认实现是刻意的**，与本 trait 其余「必需方法」的取舍相反：信封的设计前提就是
    /// 「未知 kind 安静忽略」，那么"没实现"与"不认识这个 kind"在语义上是同一件事，
    /// 强制每个实现（含测试夹具）写一遍空函数换不来任何安全性。
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

    /// darwin: .app 候选右键上下文菜单动作（页内下标 + 动作串）。
    /// action ∈ {move_top, move_up, move_down, delete, reset_default, copy}。默认空。
    fn handle_candidate_context_menu(&self, _page_local_index: i32, _action: &str) {}

    /// darwin: .app 上报前台上下文（app bundle id / 窗口标题 / 选中文本），供命令直通车
    /// app()/title()/sel() 取值。聚焦时快照，缺 AX/IMKit 支持的字段为空串。默认空实现。
    fn handle_front_context(&self, _app: &str, _title: &str, _sel: &str) {}

    /// 返回当前权威模式 (chinese_mode, full_width)，供 FocusGained 同步路径回传 ModePush。
    /// `client_token`（PID<<32|instance）标识焦点进程：state_scope="app" 时按进程切换记忆状态。
    /// 必须极轻量（仅锁+内存查询），不得有任何阻塞/跨进程调用——DLL 正同步阻塞等本值。
    /// 与 Go `MessageHandler.GetCurrentMode` 对齐。默认返回中文模式（安全默认）。
    ///
    /// `window_class`：焦点顶层窗口类，语义同 [`FocusData::window_class`]，用于跳过 shell
    /// 过渡窗口的初始模式套用。
    ///
    /// ⚠ **这是「按应用套用初始模式」的第二个落点**，与重型段 `handle_focus_gained` 各算
    /// 各的（本方法早于它执行，DLL 正阻塞等回传值）。两处的门控条件必须同步改——只改一处
    /// 时症状是「日志显示跳过了、图标照样切」，因为真正把状态改掉的是先跑的这一个。
    fn get_current_mode(&self, _client_token: u64, _window_class: &str) -> (bool, bool) {
        (true, false)
    }

    /// compartment 禁用态变更（不换焦点）上报。默认空实现。
    fn handle_input_state_report(&self, _pid: u32, _disabled: bool, _reason: u8, _mask: u64) {}

    /// 诊断快照上报（焦点窗口链 / 前台窗口 / TSF 上下文实例 id）。
    ///
    /// 仅在服务端经 `CONFIG_KEY_DIAG_SNAPSHOT` 推开采集后才会到达；HUD 关闭时 DLL 一条不发。
    /// 纯观测数据，**不得**参与任何输入决策——它的采集时机与吃键路径无关，拿它做判据
    /// 会引入「诊断开着才正常」这类最难查的形态。默认空实现。
    fn handle_diag_snapshot(&self, _snap: &DiagSnapshotPayload) {}
}

#[cfg(test)]
mod placeholder_tests {
    use super::*;

    /// 编码串类组合必须换占位空格：真实编码泄漏进宿主 composition 后，独立编码栏
    /// （preedit_display = candidate_top）下会与候选窗 preedit 重复显示。
    #[test]
    fn placeholder_replaces_code_compositions() {
        let cases: Vec<(&str, KeyAction)> = vec![
            (
                "UpdateComposition",
                KeyAction::UpdateComposition {
                    text: "skce".into(),
                    caret_pos: 4,
                },
            ),
            (
                "InsertText",
                KeyAction::InsertText {
                    text: "可能".into(),
                    new_composition: Some("h".into()),
                    mode_changed: false,
                    chinese_mode: true,
                    has_new_composition: true,
                },
            ),
            (
                // direct_commit 顶码：曾漏掉本变体，余码编码直落宿主（真机复现于
                // 「skce 顶码后快打 h」，h 被嵌进宿主而非占位空格）。
                "CommitThenDeferComposition",
                KeyAction::CommitThenDeferComposition {
                    commit_text: "可能".into(),
                    deferred_composition: "h".into(),
                    timeout_ms: 150,
                },
            ),
        ];
        for (name, action) in cases {
            let composition = match action.with_composition_placeholder() {
                KeyAction::UpdateComposition { text, caret_pos } => {
                    assert_eq!(caret_pos, 0, "{name}: 占位后光标须置前");
                    text
                }
                KeyAction::InsertText {
                    new_composition, ..
                } => new_composition.expect("组合串不应消失"),
                KeyAction::CommitThenDeferComposition {
                    commit_text,
                    deferred_composition,
                    ..
                } => {
                    assert_eq!(commit_text, "可能", "{name}: 已承诺上屏的正文不得被改写");
                    deferred_composition
                }
                other => panic!("{name}: 变体不应改变，实际 {other:?}"),
            };
            assert_eq!(composition, " ", "{name}: 组合串应换成占位空格");
        }
    }

    /// 正文类组合刻意不换：HoldComposition 系列的组合内容是中文符号本身，
    /// 换成空格会把符号弄丢。守卫此边界，防止后来者"顺手补全所有变体"。
    #[test]
    fn placeholder_keeps_literal_symbol_compositions() {
        let held = KeyAction::HoldComposition {
            text: "。".into(),
            timeout_ms: 500,
        };
        match held.with_composition_placeholder() {
            KeyAction::HoldComposition { text, .. } => assert_eq!(text, "。"),
            other => panic!("HoldComposition 不应被改写，实际 {other:?}"),
        }

        let commit_hold = KeyAction::CommitAndHoldComposition {
            commit_text: "可能".into(),
            hold_text: "。".into(),
            timeout_ms: 500,
        };
        match commit_hold.with_composition_placeholder() {
            KeyAction::CommitAndHoldComposition {
                commit_text,
                hold_text,
                ..
            } => {
                assert_eq!(commit_text, "可能");
                assert_eq!(hold_text, "。");
            }
            other => panic!("CommitAndHoldComposition 不应被改写，实际 {other:?}"),
        }
    }

    /// 空组合串（无余码/无新组合）不得被塞进占位空格，否则宿主留下一个空组合。
    #[test]
    fn placeholder_skips_empty_compositions() {
        let empty_defer = KeyAction::CommitThenDeferComposition {
            commit_text: "可能".into(),
            deferred_composition: String::new(),
            timeout_ms: 150,
        };
        match empty_defer.with_composition_placeholder() {
            KeyAction::CommitThenDeferComposition {
                deferred_composition,
                ..
            } => assert!(deferred_composition.is_empty()),
            other => panic!("空组合串不应被改写，实际 {other:?}"),
        }
    }
}
