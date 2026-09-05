//! 二进制协议定义：命令码、Header、Payload 结构体
//!
//! 与 Go 版本 `wind_input/internal/ipc/binary_protocol.go` 和
//! C++ 版本 `wind_tsf/include/BinaryProtocol.h` 字节级对齐。

use std::fmt;

// ──────────────────────────────────────────────
// Protocol constants
// ──────────────────────────────────────────────

/// 协议版本号 (v1.1)
pub const PROTOCOL_VERSION: u16 = 0x1001;
/// 异步标志位（version 字段高位）
pub const ASYNC_FLAG: u16 = 0x8000;
/// 版本掩码（取主版本号，排除 ASYNC_FLAG 位 0x8000）
pub const VERSION_MASK: u16 = 0x7000;

// ──────────────────────────────────────────────
// Command codes — 上游 (C++ → Go/Rust)
// ──────────────────────────────────────────────

// 按键事件
pub const CMD_KEY_EVENT: u16 = 0x0101;
pub const CMD_COMMIT_REQUEST: u16 = 0x0104;

// 焦点 & 激活
pub const CMD_FOCUS_GAINED: u16 = 0x0201;
pub const CMD_FOCUS_LOST: u16 = 0x0202;
pub const CMD_IME_ACTIVATED: u16 = 0x0203;
pub const CMD_IME_DEACTIVATED: u16 = 0x0204;
pub const CMD_MODE_NOTIFY: u16 = 0x0205;
pub const CMD_TOGGLE_MODE: u16 = 0x0207;
pub const CMD_MENU_COMMAND: u16 = 0x0208;
pub const CMD_COMPOSITION_TERMINATED: u16 = 0x0209;
pub const CMD_SHOW_CONTEXT_MENU: u16 = 0x020A;
pub const CMD_SYSTEM_MODE_SWITCH: u16 = 0x020B;
pub const CMD_INPUT_STATE_REPORT: u16 = 0x0213;
/// 诊断快照（上行，异步）：焦点窗口链 + 前台窗口 + TSF 上下文实例 id。载荷见
/// [`DiagSnapshotPayload`]。
///
/// **刻意不并入 `CMD_FOCUS_GAINED`**：那条路径是宿主 UI 线程上的**同步** IPC 往返
/// （见 `TextService.cpp` 的 `focusIpcT0` 计时），首字延迟直接挂在它身上；本命令要做
/// 三次窗口类名查询 + band 查询，塞进去等于给每次焦点切换加固定开销。故独立成命令、
/// 异步发送，且由 [`CONFIG_KEY_DIAG_SNAPSHOT`] 门控——HUD 关闭时 DLL 一次都不采集。
pub const CMD_DIAG_SNAPSHOT: u16 = 0x0214;

/// `CMD_FOCUS_LOST` 载荷的 reason 字段（第 9 字节）。与 C++ `BinaryProtocol.h` 的
/// `FOCUS_LOST_REASON_*` 一一对应，改动须两边同步。
///
/// 「失焦」在 TSF 里是四件语义不同的事，过去挤在一个命令里，服务端只能一刀切地
/// 「清激活态 + 清输入态」：于是同宿主换文档也会关掉工具栏，而 DocMgr 级失焦因为不敢
/// 清输入态干脆什么都不发、工具栏永不隐藏。拆开后三项后果各自独立：
///
/// | reason        | ime_active | has_edit_context | 输入态 |
/// |---------------|------------|------------------|--------|
/// | `Thread`      | false      | false            | 清     |
/// | `DocChanged`  | 不动       | 不动             | 清     |
/// | `CtxLost`     | 不动       | false            | **不清** |
/// | `NoEditCtx`   | 不动       | false            | 清     |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusLostReason {
    /// 整个应用失去前台（DLL 的 `OnKillThreadFocus`）。真正意义上离开本输入法。
    Thread,
    /// 同一宿主内换了文档。宿主没变、输入法仍在服务它，故不动激活态。
    DocChanged,
    /// 焦点离开可编辑控件（DLL 的 DocMgr 级失焦）。
    ///
    /// **绝不可用它清输入态**：该事件来自噪声层（Excel 实测同一 DocMgr 6ms 内掉了又回），
    /// 在那里销毁输入态正是「首字符不进编码、直接上屏」的根因。只翻可见性标志才安全。
    CtxLost,
    /// 换到了没有可编辑控件的文档（QQ Ctrl+1 切会话）。残留 buffer 无处可去，须清。
    NoEditCtx,
}

impl FocusLostReason {
    /// 从协议字节解码。未知值与缺省一律按 [`Self::Thread`] 处理——那是旧 DLL 不带
    /// reason 时的隐含语义，也是后果最完整的一种，误判方向安全。
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::DocChanged,
            2 => Self::CtxLost,
            3 => Self::NoEditCtx,
            _ => Self::Thread,
        }
    }

    /// 是否应清空输入态（buffer / preedit / 候选）。
    pub fn clears_input(self) -> bool {
        !matches!(self, Self::CtxLost)
    }

    /// 是否应清 `ime_active`（本输入法整体不再服务任何宿主）。
    pub fn clears_ime_active(self) -> bool {
        matches!(self, Self::Thread)
    }

    /// 是否应清 `has_edit_context`（焦点不在可编辑控件里了）。
    pub fn clears_edit_context(self) -> bool {
        !matches!(self, Self::DocChanged)
    }
}

// 上行（darwin .app / Windows host-render DLL）：鼠标候选交互。方向与下行 0x020D/0x020E 由 dispatch 上下文区分。
pub const CMD_CANDIDATE_SELECT: u16 = 0x020D; // payload: pageLocalIndex i32 LE；<0 为翻页按钮（-1 上页 / -2 下页，与 SHM 命中矩形约定一致）
pub const CMD_CANDIDATE_HOVER: u16 = 0x020E; // payload: pageLocalIndex i32 LE (-1=无；Windows 另带 anchorX/belowY/aboveY 三个 i32，当前仅取 index)
pub const CMD_CANDIDATE_CONTEXT_MENU: u16 = 0x020F; // 上行：候选右键动作 (payload: index i32 + actionLen u32 + action UTF-8)
pub const CMD_MENU_ACTION: u16 = 0x0210; // 上行：统一菜单项被选中 (payload: 菜单 id i32 LE)
pub const CMD_CANDIDATE_SCROLL: u16 = 0x0211; // 上行：host 候选框滚轮 (payload: delta i32，WHEEL_DELTA 倍数，正=上滚)；服务端解释成「上下键调整高亮项」（到页边界翻到相邻页），见 Coordinator::handle_candidate_scroll
/// 上报前台上下文（命令直通车 `app()`/`title()`/`sel()` 取值，目前仅 darwin `.app` 发）：
/// payload = appLen u32 + app(UTF-8) + titleLen u32 + title + selLen u32 + sel，均 LE 长度前缀。
///
/// 该值原为 `0x0211`，与 `CMD_CANDIDATE_SCROLL` 同码位、靠 `cfg` 分臂区分平台。那是唯一
/// 一处**同方向**的语义复用，代价是「macOS 永远做不了滚轮翻页」。macOS 尚未发布，故直接
/// 迁到空闲码位消除该约束，两平台此后码位含义完全一致。
pub const CMD_FRONT_CONTEXT: u16 = 0x0215;
/// Host render: DLL 侧 Band 窗口创建失败（异步上行，payload = reason u32）。
/// 服务端收到后记日志并让 UI 回退本地窗口。与 C++ BinaryProtocol.h:36 对齐。
pub const CMD_HOST_RENDER_FAILED: u16 = 0x0212;

// 光标 & 选区
pub const CMD_CARET_UPDATE: u16 = 0x0301;
pub const CMD_SELECTION_CHANGED: u16 = 0x0302;
pub const CMD_CARET_PENDING: u16 = 0x0303;
/// 首显试探采样：DLL 在首帧 reflow 的每次 layout change 各发一条（限 5 条），payload 同
/// `CaretPayload`。**采样不等于可信**——首帧期间宿主可能仍返回旧坐标（实测 WPS 前两条是上
/// 一轮的值、EverEdit 第一条就已正确），是否采纳由服务端按 per-app 策略判定。
pub const CMD_CARET_PROBE: u16 = 0x0304;

// Host Render
pub const CMD_HOST_RENDER_REQUEST: u16 = 0x0501;

// darwin 专用 host-render push 帧（方向与上行 0x05xx 由 push 通道语义区分）。
// 字节布局须与 Swift wind_macos/.../BinaryCodec.swift decoder 及 Go binary_codec.go 一致。
pub const CMD_HOST_RENDER_FRAME: u16 = 0x0502; // SHM 新帧就绪通知 (seq+几何+flags+scale, 28B)
pub const CMD_CANDIDATE_RECTS: u16 = 0x0503; // 候选命中矩形 (panel-local)
pub const CMD_MODE_STATUS: u16 = 0x0504; // 输入模式状态 (菜单栏指示器)
pub const CMD_CANDIDATE_MENU_FLAGS: u16 = 0x0505; // 每候选右键菜单禁用位
pub const CMD_MENU_SHOW: u16 = 0x0506; // 统一菜单树 (响应 CmdShowContextMenu)
// 0x0507 曾是 CMD_OPEN_SETTINGS（payload 为「页名+参数」空格串）。已改走扩展信封的
// `settings.open`（body 为 JSON argv 数组），码位空出——新增下行专用码位可从这里取。
pub const CMD_TOOLTIP_SHOW: u16 = 0x0508; // 候选悬停 tooltip
pub const CMD_TOOLTIP_HIDE: u16 = 0x0509;
pub const CMD_STATUS_SHOW: u16 = 0x050A; // 模式状态气泡
pub const CMD_STATUS_HIDE: u16 = 0x050B;
pub const CMD_TOAST_SHOW: u16 = 0x050C; // Toast 通知
pub const CMD_TOAST_HIDE: u16 = 0x050D;
// 命令直通车按键合成（darwin 下行）：服务进程无辅助功能授权无法 post CGEvent，
// 故把 key.tap/seq/hold/release/type 推给 .app 侧 KeySynthesizer 合成（.app 有授权）。
// combo 载荷：keyLen u32 + key + modCount u32 + modCount×(modLen u32 + mod)，key/mod 均 UTF-8。
pub const CMD_KEY_TAP: u16 = 0x050E; // 单个 combo
pub const CMD_KEY_SEQ: u16 = 0x050F; // comboCount u32 + comboCount×combo
pub const CMD_KEY_HOLD: u16 = 0x0510; // 单个 combo（按下保持）
pub const CMD_KEY_RELEASE: u16 = 0x0511; // 单个 combo（抬起）
pub const CMD_KEY_TYPE: u16 = 0x0512; // 整段 UTF-8 文本（无长度前缀），.app 走 insertText 上屏

// ──────────────────────────────────────────────
// 扩展信封 (0x0E01，上下行同码位、按方向区分)
// ──────────────────────────────────────────────

/// 通用扩展信封：`kindLen u32 + kind(UTF-8) + bodyLen u32 + body(任意字节)`。
///
/// # 为什么要有它
///
/// 每加一个小功能就占一个码位，代价是**三处常量必须同步**（本文件 /
/// `wind_tsf/include/BinaryProtocol.h` / Swift `ProtocolTypes.swift`），且码位一旦被某端
/// 固化就再难回收——`CMD_FRONT_CONTEXT` 当年就是这么撞上 `CMD_CANDIDATE_SCROLL` 的。
/// 低频消息没必要为此付出这个代价。
///
/// # 两档划分（新增消息时按此判断，不要凭直觉）
///
/// 判据是**一次连续输入里会不会被反复触发**：
///
/// - **高频**（每键 / 每帧 / 每次鼠标移动）→ **专用码位 + 定长或长度前缀的二进制**。
///   如 `CMD_KEY_EVENT`、`CMD_HOST_RENDER_FRAME`、`CMD_CANDIDATE_HOVER`。这类路径上
///   一次 JSON 解析与一次堆分配都是要算的。
/// - **低频**（菜单动作、位置回报、诊断上报、截图请求、设置深链）→ **走本信封**，
///   `kind` 命名 + `body` 放 JSON 或数据块。加功能=加一个 `kind` 字符串，不动协议常量。
///
/// # 演进性
///
/// - **未知 `kind` 一律安静忽略**（记 debug 日志，不报错、不断连）。这是新旧版本能互相
///   兼容的根本：旧端收到新 `kind` 就当没看见，而不是解析失败把连接搞坏。
/// - `body` 用 JSON 时，字段增删天然向前/向后兼容（未知字段忽略、缺失字段取默认）。
/// - `body` 是**不透明字节**，本层不解析——既支持 JSON，也支持二进制块（如截图回传）。
///   解析归消费方，避免 `wind-ipc` 被拖进业务语义。
///
/// # `kind` 命名约定
///
/// 小写点分，`域.动作`：`pos.candidate`、`diag.hud`、`settings.open`。域名沿用既有模块名，
/// 新域在下方 [`ext_kind`] 里登记一个常量，不要在调用处写裸字符串。
pub const CMD_EXT: u16 = 0x0E01;

/// 扩展信封的 `kind` 常量登记处。**新增 kind 必须在此登记**（而不是在调用点写裸串），
/// 否则两端拼写不一致的错误只会表现为「消息静默丢失」。
pub mod ext_kind {
    /// 下行：请求 `.app` 打开设置应用。body = `{"args":["--page=dict", …]}`。
    pub const SETTINGS_OPEN: &str = "settings.open";
    /// 上行：候选窗被拖动到新位置。body = `{"x":123,"y":456}`，wire 坐标系（屏幕左上为
    /// 原点、y 向下）下的**内容左上角**，与配置里的 `ui.candidate.custom_x/y` 同义。
    pub const POS_CANDIDATE: &str = "pos.candidate";
    /// 上行：状态提示气泡被拖动到新位置。body 同 [`POS_CANDIDATE`]。
    pub const POS_STATUS_TIP: &str = "pos.status_tip";
    /// 下行：请 `.app` 把某个原生浮窗截图存盘并复制到剪贴板。
    /// body = `{"target":"status_tip"|"tooltip","path":"/绝对路径.png"}`。
    ///
    /// 为什么由 `.app` 动手：状态气泡与悬停提示是 `.app` 侧的原生 NSPanel，**像素不在
    /// 服务进程**（候选窗相反，那是本进程光栅化后经 SHM 推下去的，故直接在本进程截）。
    /// 文件名与随后的 Toast 文案仍由服务端决定，保持与 Windows 逐字一致。
    pub const SHOT_PANEL: &str = "shot.panel";
    /// 上行：[`SHOT_PANEL`] 的结果。
    /// body = `{"ok":bool,"path":"…","clipboard":bool,"reason":"…"}`（`reason` 仅失败时）。
    pub const SHOT_RESULT: &str = "shot.result";
    /// 下行：问 `.app` 候选窗此刻在哪，答案走上行 [`POS_CANDIDATE`]。body 空。
    pub const POS_CANDIDATE_QUERY: &str = "pos.candidate.query";
    /// 下行：问 `.app` 状态气泡此刻在哪，答案走上行 [`POS_STATUS_TIP`]。body 空。
    pub const POS_STATUS_TIP_QUERY: &str = "pos.status_tip.query";
    //
    // 为什么这两个「位置」要一问一答，而不是服务进程自己记账：
    //
    // 浮窗是 `.app` 侧的原生 NSPanel，服务进程发出去的只是**建议落点**。`.app` 还会按
    // 所在屏的可见区钳制、在下方放不下时翻到光标上方、以及沿用用户本次组合内拖出来的
    // 落位——三者都会让实际位置与服务进程发的那个值不同。用户点「固定位置」时要以**当前
    // 看到的位置**落盘，记账值会把窗口摆到一个它从没出现过的地方（而 Windows 那边读的是
    // 真 `GetWindowRect`，不存在这个问题）。
}

// 批处理
pub const CMD_BATCH_EVENTS: u16 = 0x0F01;
pub const CMD_BATCH_RESPONSE: u16 = 0x0F02;

// 输入统计
pub const CMD_INPUT_STATS: u16 = 0x0F03;

// ──────────────────────────────────────────────
// Command codes — 下游 (Go/Rust → C++ 响应)
// ──────────────────────────────────────────────

pub const CMD_ACK: u16 = 0x0001;
pub const CMD_PASS_THROUGH: u16 = 0x0002;

// 文本操作
pub const CMD_COMMIT_TEXT: u16 = 0x0101;
pub const CMD_UPDATE_COMPOSITION: u16 = 0x0102;
pub const CMD_CLEAR_COMPOSITION: u16 = 0x0103;
pub const CMD_COMMIT_RESULT: u16 = 0x0105;
pub const CMD_COMMIT_TEXT_WITH_CURSOR: u16 = 0x0106;
pub const CMD_MOVE_CURSOR: u16 = 0x0107;
pub const CMD_DELETE_PAIR: u16 = 0x0108;
/// 删除光标前 N 个字符并插入文本（智能符号替换）。
pub const CMD_REPLACE_BACKWARD: u16 = 0x0109;
/// HoldComposition 响应 (0x010A)：开启组合显示 text，在 timeout_ms 毫秒后自动提交。
/// 载荷：timeout_ms(u32 LE) + text_len(u32 LE) + UTF-8 text
pub const CMD_HOLD_COMPOSITION: u16 = 0x010A;
/// CommitAndHoldComposition 响应 (0x010B)：先提交 commit_text，再开 HoldComposition 放入 hold_text。
/// 载荷：timeout_ms(u32 LE) + commit_len(u32 LE) + hold_len(u32 LE) + commit_utf8 + hold_utf8
pub const CMD_COMMIT_AND_HOLD: u16 = 0x010B;
/// CommitThenDeferComposition 响应 (0x010C)：先真提交 commit_text，
/// 余码新组合 deferred_composition 延迟到触发键 keyup（或 timeout_ms 兜底）才开。
/// 载荷：timeout_ms(u32 LE) + commit_len(u32 LE) + defer_len(u32 LE) + commit_utf8 + defer_utf8
pub const CMD_COMMIT_THEN_DEFER: u16 = 0x010C;
/// ClearCompositionThenPassThrough 响应 (0x010D)：收掉组合，并把**当前这个键**交还宿主。
/// 无载荷——要重放的就是宿主此刻正在处理的那个键，由它自己取 vk；带 vk 反而制造
/// 「服务端说重放 A、宿主正在处理 B」的错位可能。见 `KeyAction::ClearCompositionThenPassThrough`。
pub const CMD_CLEAR_THEN_PASS_THROUGH: u16 = 0x010D;

// 状态
pub const CMD_STATUS_UPDATE: u16 = 0x0202;
pub const CMD_STATE_PUSH: u16 = 0x0206;
pub const CMD_SERVICE_READY: u16 = 0x0207; // push only
pub const CMD_ACTIVATION_STATUS_PUSH: u16 = 0x020C;
/// FocusGained 同步路径的轻量模式回传（仅 chineseMode+fullWidth，4 字节 flags）。
/// DLL 在 OnSetFocus 内同步等本响应，首键前写好 _bChineseMode，根治"切应用首键上屏英文"
/// 竞态；同时解除 DLL 的同步等待（否则无响应会卡到 READ_TIMEOUT_MS）。位定义同 STATUS_*。
// 注：0x020D 双用途——下行此 CMD_MODE_PUSH（service→client push，仅编码）；
// 上行 CMD_CANDIDATE_SELECT（client→service 请求，仅 dispatch）。方向区分，勿在 dispatch 加 MODE_PUSH 臂。
pub const CMD_MODE_PUSH: u16 = 0x020D;
/// TSF 侧在前台应用进程中执行 ShellExecute（打开 URL / 启动程序），解决 Service 进程无前台权限的问题。
/// 载荷：target_len(u32 LE) + target(UTF-8) + params_len(u32 LE) + params(UTF-8)
pub const CMD_SHELL_EXEC: u16 = 0x020E;
/// 「只重取语言栏图标」的下行推送，无载荷。
///
/// 存在的理由是 `GetIcon` 是**被动回调**：服务端把新位图写进共享内存后，DLL 不会自己
/// 察觉，必须由 `OnUpdate(TF_LBI_ICON)` 让系统再来取一次。此前这件事完全寄生在状态推送
/// 上——而 `UpdateFullStatus` 的 `needUpdate` 去重会挡掉「状态没变、只有位图变了」的情形，
/// 于是调试菜单改角标形状要等下一次焦点切换才生效，演示动画更是根本动不起来。
///
/// 刻意**不带载荷**：本命令只回答「去重取一次」，图标内容的唯一真相在共享内存里。
/// 若把状态塞进载荷，就等于开了第二条真相通路，两者不一致时无从判定谁对。
///
/// 与状态推送的分工：状态变化走 `CMD_STATE_PUSH` / `CMD_ACTIVATION_STATUS_PUSH`
/// （它们本就会触发 `OnUpdate`），**不要**再叠一条本命令，否则每次切换都会让每个宿主
/// 多做一次无谓的 `GetIcon` 重绘。本命令只用于「位图变了但状态没变」的路径。
pub const CMD_REFRESH_ICON: u16 = 0x0216;
pub const CMD_SYNC_CONFIG: u16 = 0x0303;

/// 配置同步键名（对齐 C++ BinaryProtocol.h CONFIG_KEY_*）。
///
/// ⚠ 名为 config，实为**服务端 → DLL 的单向键值推送通道**，承载的不止「配置」：
/// `CONFIG_KEY_LANGBAR_TOOLTIP` 就是随状态变化的**显示内容**。判断一个东西该不该走
/// 这里，看的不是「它是不是配置」，而是这两条：
///   ① 需要**广播**给所有宿主（状态推送是定向的，见 `push_activation_status`）；
///   ② 变化频率**低于**状态推送（否则每次状态变化都要多发一条）。
/// 两条都满足才走这里；只满足①不满足②的，应当考虑并进状态推送而不是在这里高频刷。
///
/// 沿用 config 这个名字是因为两侧 + C++ 回调链（`SyncConfigCallback`）都已叫它，
/// 改名要动的地方远多于收益——但语义得在这里写清楚，否则下一个人会按字面意思判断。
pub const CONFIG_KEY_ENGLISH_PAIRS: &str = "en_pairs";
/// 配对跳出键（VK 码集合）同步键名。TSF 端英文模式配对跳出直接消费；
/// 中文模式仅用于「有待跳出配对时」放行转发（真正裁决在协调器）。
pub const CONFIG_KEY_JUMP_OUT_KEYS: &str = "jump_out_keys";
/// 密码框强制英文抑制的策略开关（会话级，右键菜单「高级」可关）同步键名。
/// TSF 端据此 + 自身持有的 InputScope 掩码在 `OnTestKeyDown` 本地判定是否放行：
/// 吃键决策发生在 IPC 之前，协调器回 PassThrough 已太晚（形成「吃了再吐」丢键）。
pub const CONFIG_KEY_PASSWORD_SUPPRESS: &str = "password_suppress";
/// 「英文半角列有自定义标点映射」的源字符集合同步键名。英文模式（非全角）下 TSF 默认直接
/// 透传标点键、引擎收不到，英半列因此永远不生效；TSF 据此集合**精确**吃下这些键转发引擎
/// （集合为空 = 行为与历史完全一致）。判据须与 `wind_punct::custom_english_punct_chars`
/// 同源，漂移即「吃了再吐」丢键。
pub const CONFIG_KEY_CUSTOM_EN_PUNCT: &str = "custom_en_punct";
/// 配对状态时效（秒，0=不过期）同步键名。TSF 端持有吃键闸门（`_pairPendingDepth`），
/// 必须能本地判定状态是否陈旧：若只有协调器过期而 DLL 仍吃跳出键，协调器回 PassThrough
/// 已太晚（形成「吃了再吐」丢键）。故 TTL 判据以 DLL 侧为准，此键把阈值推给它。
///
/// **刻意不并入 `CONFIG_KEY_JUMP_OUT_KEYS` 的 payload**：那个格式已经改过一次
/// （前置 `right_symbol` u8，两侧解析偏移 1→2），再叠字段容易出现偏移不同步。
pub const CONFIG_KEY_PAIR_STATE_TTL: &str = "pair_state_ttl";
/// 诊断快照采集开关（会话级，随输入诊断 HUD 显隐）同步键名。格式：`enabled(u8)`。
///
/// 采集本身要查三次窗口类名 + band，属于「只有排查时才值得付」的开销，故默认关闭、
/// 由服务端在 HUD 打开时推开。**必须在握手时也推一次**：DLL 每次重连都从默认值
/// （关）起步，只在切换时推会让重连后的宿主永远不采集（`push_connect_fix` 记过同型）。
pub const CONFIG_KEY_DIAG_SNAPSHOT: &str = "diag_snapshot";

/// 语言栏按钮的悬停提示文本。格式：`[ch:u16(LE)]...`（UTF-16LE，无长度前缀，
/// `value` 本身就是整段）。与 `CONFIG_KEY_CUSTOM_EN_PUNCT` 同惯例——C++ 侧照此可以
/// 直接构造 `std::wstring`，不必再跨类去借编码转换函数。
///
/// 文案与选择逻辑都在服务端：DLL 只存一份字符串，`GetTooltipString` 原样返回。
/// 收归的理由与 `InputBlock` 同一条——DLL 本地只有 `_bChineseMode` / `_bCapsLock` 两个
/// 量，判不出「密码框」「已禁用」这些成因，删掉那些分支后 tooltip 就只能说个大概；
/// 而这些成因服务端全都有。
///
/// **刻意走 sync_config 而不是并进 activation status push**，决定性理由是**推送范围**：
/// 那条推送是定向的（`hostRenderAvail` 按事件源 pid 算，广播会污染无关客户端，真机
/// 踩过 SearchHost 的 Band 重建循环），而 tooltip 必须广播——非事件源的宿主悬停时
/// 同样要看到最新文案。**所以它即使能塞进去也不该塞，格式修好了也不该迁回去。**
///
/// 次要理由：那条消息的 `icon_label` 是尾部不定长段（无长度前缀、占满剩余 payload），
/// 本来也加不进去，见 `encode_status_update_ex`。
///
/// 变化频率低于状态变化（tooltip 只有几种取值，全半角/标点变化不影响它），
/// 故服务端只在**文本真的变了**时才推。
pub const CONFIG_KEY_LANGBAR_TOOLTIP: &str = "langbar_tooltip";

// 消费确认
pub const CMD_CONSUMED: u16 = 0x0401;

// Host Render
/// 仅 Windows 使用；darwin 端 SHM 名固定（endpoint::shm_name），无 setup 握手。
pub const CMD_HOST_RENDER_SETUP: u16 = 0x0501;

/// Host 窗口种类（与 C++ HostWindowKind 对齐，BinaryProtocol.h:359-365）
pub const HOST_WINDOW_CANDIDATE: u32 = 0;
pub const HOST_WINDOW_TOOLTIP: u32 = 1;
pub const HOST_WINDOW_STATUS: u32 = 2;
pub const HOST_WINDOW_KIND_COUNT: usize = 3;

/// CMD_HOST_RENDER_SETUP 响应的单条通道描述（Windows；对齐 C++ HostRenderSetupEntryHeader）
#[derive(Clone, Debug)]
pub struct HostRenderSetupEntry {
    pub window_kind: u32,
    pub max_buffer_size: u32,
    pub shm_name: String,
    pub event_name: String,
}

/// SHM 内 hit-rect 表条目（20B，对齐 C++ HostRenderHitRect；index<0 为翻页按钮）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostRenderHitRect {
    pub index: i32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl HostRenderHitRect {
    pub const SIZE: usize = 20;
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.index.to_le_bytes());
        b[4..8].copy_from_slice(&self.x.to_le_bytes());
        b[8..12].copy_from_slice(&self.y.to_le_bytes());
        b[12..16].copy_from_slice(&self.w.to_le_bytes());
        b[16..20].copy_from_slice(&self.h.to_le_bytes());
        b
    }
}

// ──────────────────────────────────────────────
// IPC Header (8 bytes, little-endian)
// ──────────────────────────────────────────────

/// 8 字节 IPC 消息头
///
/// ```text
/// Offset  Size  Field
/// 0       2     version  (含 ASYNC_FLAG)
/// 2       2     command
/// 4       4     payload_length
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C, packed)]
pub struct IpcHeader {
    pub version: u16,
    pub command: u16,
    pub length: u32,
}

impl IpcHeader {
    pub const SIZE: usize = 8;

    pub fn new(command: u16, payload_len: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            command,
            length: payload_len,
        }
    }

    pub fn new_async(command: u16, payload_len: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION | ASYNC_FLAG,
            command,
            length: payload_len,
        }
    }

    pub fn is_async(&self) -> bool {
        self.version & ASYNC_FLAG != 0
    }

    pub fn major_version(&self) -> u16 {
        self.version & VERSION_MASK
    }

    pub fn to_bytes(&self) -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf[0..2].copy_from_slice(&self.version.to_le_bytes());
        buf[2..4].copy_from_slice(&self.command.to_le_bytes());
        buf[4..8].copy_from_slice(&self.length.to_le_bytes());
        buf
    }

    pub fn from_bytes(buf: &[u8; 8]) -> Self {
        Self {
            version: u16::from_le_bytes([buf[0], buf[1]]),
            command: u16::from_le_bytes([buf[2], buf[3]]),
            length: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
        }
    }
}

impl fmt::Debug for IpcHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let version = self.version;
        let command = self.command;
        let length = self.length;
        f.debug_struct("IpcHeader")
            .field("version", &format_args!("0x{:04X}", version))
            .field("command", &format_args!("0x{:04X}", command))
            .field("length", &length)
            .field("async", &self.is_async())
            .finish()
    }
}

// ──────────────────────────────────────────────
// Key Payload (18 bytes)
// ──────────────────────────────────────────────

/// 按键事件载荷 (18 bytes)
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct KeyPayload {
    pub key_code: u32,
    pub scan_code: u32,
    pub modifiers: u32,
    pub event_type: u8, // 0=keydown, 1=keyup
    pub toggles: u8,    // CapsLock/NumLock/ScrollLock
    pub event_seq: u16,
    pub prev_char: u16,
}

impl KeyPayload {
    pub const SIZE: usize = 18;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..4].copy_from_slice(&self.key_code.to_le_bytes());
        buf[4..8].copy_from_slice(&self.scan_code.to_le_bytes());
        buf[8..12].copy_from_slice(&self.modifiers.to_le_bytes());
        buf[12] = self.event_type;
        buf[13] = self.toggles;
        buf[14..16].copy_from_slice(&self.event_seq.to_le_bytes());
        buf[16..18].copy_from_slice(&self.prev_char.to_le_bytes());
        buf
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            key_code: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            scan_code: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            modifiers: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            event_type: buf[12],
            toggles: buf[13],
            event_seq: u16::from_le_bytes([buf[14], buf[15]]),
            prev_char: u16::from_le_bytes([buf[16], buf[17]]),
        })
    }
}

// ──────────────────────────────────────────────
// Caret Payload (20 bytes)
// ──────────────────────────────────────────────

/// 光标位置载荷 (20 bytes)
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct CaretPayload {
    pub x: i32,
    pub y: i32,
    pub height: i32,
    pub composition_start_x: i32,
    pub composition_start_y: i32,
}

/// 坐标来源，与 C++ `BinaryProtocol.h` 的 `CARET_SRC_*` 逐值对齐。
///
/// 同一组 x/y/h 可能来自语义完全不同的通道：TSF context 的插入点、组合起点，或**跨窗口**的
/// Win32 光标。压进同一个字段后下游就再也分不开了——曾因此让 `GetGUIThreadInfo` 的光标冒充
/// TSF 插入点，在 Word 非正文行错位 814px、在桌面输入定位到任务栏。
pub mod caret_source {
    pub const UNKNOWN: i32 = 0;
    pub const TSF_SELECTION: i32 = 1;
    pub const TSF_COMPOSITION: i32 = 2;
    pub const TSF_CACHED: i32 = 3;
    pub const GUI_CARET: i32 = 4;
    pub const CONSOLE: i32 = 5;
    pub const LAST_KNOWN: i32 = 6;
    /// 组合刚启动时的异步探测值（`CaretProbeKind::FirstShowProbe`）——**reflow 前**的坐标。
    ///
    /// 出自 TSF `GetTextExt`，但绝大多数宿主对这次请求选择内联执行，等同同步取，
    /// 拿到的是宿主尚未重排的旧值（Excel 实测与随后的权威值差 16px）。所以它
    /// **不可作权威、不可参与任何首显决策**——2026-08-01 曾让它走普通 probe 通道，
    /// 被 fast 档的判据采信提前首显，16px 偏差随后又被 settle 容差吞掉，错位就此固定。
    ///
    /// 它唯一的正当用途是**刷新坐标缓存**：连续快速上屏时（五笔 4 码自动上屏 + 长按，
    /// 33ms 一键），宿主的 `OnLayoutChange` 有 50ms debounce、被输入彻底压住，整段
    /// **一条权威 caret_update 都不来**（实测松手后 82ms 才到），此时它是唯一的位置来源。
    /// 缓存里那份几百毫秒前的旧坐标会让每轮兜底首显都钉在原地，实测偏差 456px——
    /// 相比之下 reflow 前的 ~30px 好得多。
    ///
    /// ★ 同一条数据源在两个场景里价值相反，区别不在来源而在**用途**：拿它做决策有害，
    /// 拿它更新缓存有益。故独立成一个 source 值，由消费端按用途区分，而不是放开来源。
    pub const PRE_REFLOW: i32 = 7;

    /// 是否属 TSF 语义域——即「这个坐标和组合起点出自同一个 context」。
    /// 只有这一类才可作权威坐标，也只有这一类才可与组合起点做距离比较。
    pub fn is_tsf(source: i32) -> bool {
        matches!(source, TSF_SELECTION | TSF_COMPOSITION | TSF_CACHED)
    }

    pub fn name(source: i32) -> &'static str {
        match source {
            TSF_SELECTION => "tsf_selection",
            TSF_COMPOSITION => "tsf_composition",
            TSF_CACHED => "tsf_cached",
            GUI_CARET => "gui_caret",
            CONSOLE => "console",
            LAST_KNOWN => "last_known",
            PRE_REFLOW => "pre_reflow",
            _ => "unknown",
        }
    }
}

impl CaretPayload {
    pub const SIZE: usize = 20;

    /// 从 v2 载荷（24 字节 = CaretPayload + source i32 LE）读取坐标来源。
    ///
    /// 短包一律 `UNKNOWN`：旧 DLL 只发 20 字节、macOS .app 只发 12 字节。故新旧两侧可任意组合，
    /// 消费端把 UNKNOWN 当「无法判定」处理即可（保持既有行为，不新增闸门）。
    pub fn source_from_bytes(buf: &[u8]) -> i32 {
        if buf.len() >= 24 {
            i32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]])
        } else {
            caret_source::UNKNOWN
        }
    }

    /// 整个组合 range 的包围矩形 `(left, top, right, bottom)`，v3（40 字节）载荷才有。
    ///
    /// ★ 它与 `composition_start_*` 的区别只有「折不折叠」，答的却是两个不同的问题：
    /// 后者答「组合从哪开始」，本字段答「组合占了多大一块」。组合一旦换行，两者分处
    /// 不同行——**只有本字段能看出跨行发生了**。没有它时协调器只能拿两个孤立点的像素
    /// 距离跟「3 倍行高」比大小来猜，而行高本身在宿主间、甚至同一宿主的首行与次行之间
    /// 都会变（记事本实测 74/42），阈值随之在 126/222 之间跳，换行能否被跟上全看运气。
    ///
    /// 返回 `None` 表示本帧没有组合矩形：旧 DLL / macOS 短包，或宿主 `GetTextExt` 取不到
    /// （含 `TS_E_NOLAYOUT`——那是「布局还没算完」，不是「不支持」）。四值全 0 同样按
    /// `None` 处理，它是 DLL 侧「未取到」的编码。
    pub fn comp_rect_from_bytes(buf: &[u8]) -> Option<(i32, i32, i32, i32)> {
        if buf.len() < 40 {
            return None;
        }
        let v = |o: usize| i32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        let rect = (v(24), v(28), v(32), v(36));
        if rect == (0, 0, 0, 0) {
            None
        } else {
            Some(rect)
        }
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            x: i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            y: i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            height: i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            composition_start_x: i32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            composition_start_y: i32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
        })
    }
}

// ──────────────────────────────────────────────
// Focus Gained Payload (39 bytes: 旧 36 + disabled(1) + reason(1) + caret_source(1))
// ──────────────────────────────────────────────

/// 焦点获取载荷 (39 bytes: 旧 36 + disabled(1) + reason(1) + caret_source(1))
#[derive(Clone, Copy, Debug)]
pub struct FocusGainedPayload {
    pub caret: CaretPayload,
    pub client_token: u64,
    pub input_scope_mask: u64,
    pub disabled: u8,
    pub reason: u8,
    /// 上面那个 `caret` 的来源（[`caret_source`] 之一）。
    ///
    /// ⚠ 焦点 caret 一度被认为「只更新缓存、不参与显示决策」而无需来源信息。**「焦点切换时
    /// 显示状态提示气泡」推翻了这个前提**——气泡就锚在这个坐标上。而 `OnSetFocus` 不是按键
    /// 上下文，同步 edit session 必被宿主拒绝，回退链会交出**跨窗口的** Win32 光标却仍以成功
    /// 返回；不带来源就无从分辨「拿到了一个坐标」和「拿到了**那个**坐标」。
    ///
    /// 旧 DLL 只发 38 字节，落 [`caret_source::UNKNOWN`]。
    pub caret_source: i32,
    // ⚠ 第 39 字节起是**两个前后相接的变长段**，都不在本结构里（放进来会让 `Copy` 失效
    // 并波及全部既有调用点），各由 `crate::codec` 的解码函数单独取：
    //
    //   [0..39 定长][bundleIdLen:u32][bundleId][windowClassLen:u32][windowClass]
    //
    //   ① bundleId    darwin 专属：宿主 app 的 bundle id，服务端当「进程名」用于
    //                 compat.toml 匹配与 per-app 记忆。见 `decode_focus_gained_bundle_id`。
    //   ② windowClass 焦点所在**顶层窗口**的类名（UTF-8）。服务端据此把 shell 的过渡型
    //                 窗口（任务栏 / Alt+Tab 切换器）与停留型窗口（桌面 / 文件管理器）
    //                 分开——它们同属 explorer.exe，仅凭进程名无法区分。
    //                 见 `decode_focus_gained_window_class`。
    //
    // ⚠ **窗口类段必须按顺序走，不能用固定偏移**：bundleId 是变长的，macOS 上非空。
    // Windows DLL 因此要发 `bundleIdLen=0` 占位，让两个平台共用同一条线性走法。
    // ⚠ **再追加新段一律接在最后**，且要在这张图上补一行——两个人同时往尾部加字段而
    // 各自不知情时，字节偏移会互相错位，而逐段 `>=` 兼容的解码**不会报错，只会解出垃圾**。
}

impl FocusGainedPayload {
    pub const SIZE: usize = 39;
    /// **变长段区的起点**（第一段是 `bundleIdLen:u32`，其后各段按顺序紧接）。
    ///
    /// 刻意不叫 `BUNDLE_ID_OFFSET`：段不止一个了，那个名字会让下一个加段的人以为
    /// 「从 bundleId 的偏移开始走」是巧合而另找起点，而**所有段都必须从这里线性走过**
    /// ——中间任何一段变长，后面全部错位。
    pub const VAR_SECTION_OFFSET: usize = 39;

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        // 向后兼容：至少要有旧 36 字节；disabled/reason/caret_source 缺省 0
        //
        // ⚠ 长度判据刻意**不用 `Self::SIZE`**：那样每加一个尾部字段都会把旧 DLL 整体拒掉。
        // 这里的每个 `>=` 分支对应协议史上的一次尾部追加，逐个补齐即可。
        if buf.len() < 36 {
            return None;
        }
        let caret = CaretPayload::from_bytes(&buf[0..20])?;
        let client_token = u64::from_le_bytes([
            buf[20], buf[21], buf[22], buf[23], buf[24], buf[25], buf[26], buf[27],
        ]);
        let input_scope_mask = u64::from_le_bytes([
            buf[28], buf[29], buf[30], buf[31], buf[32], buf[33], buf[34], buf[35],
        ]);
        let disabled = if buf.len() >= 37 { buf[36] } else { 0 };
        let reason = if buf.len() >= 38 { buf[37] } else { 0 };
        let caret_source = if buf.len() >= 39 {
            buf[38] as i32
        } else {
            caret_source::UNKNOWN
        };
        Some(Self {
            caret,
            client_token,
            input_scope_mask,
            disabled,
            reason,
            caret_source,
        })
    }
}

// ──────────────────────────────────────────────
// Input State Report Payload (14 bytes)
// ──────────────────────────────────────────────

/// compartment 变更时的最新输入态上报载荷 (14 bytes)
#[derive(Clone, Copy, Debug)]
pub struct InputStateReportPayload {
    pub pid: u32,
    pub disabled: u8,
    pub reason: u8,
    pub input_scope_mask: u64,
}

impl InputStateReportPayload {
    pub const SIZE: usize = 14;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.pid.to_le_bytes());
        b[4] = self.disabled;
        b[5] = self.reason;
        b[6..14].copy_from_slice(&self.input_scope_mask.to_le_bytes());
        b
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            pid: u32::from_le_bytes(buf[0..4].try_into().ok()?),
            disabled: buf[4],
            reason: buf[5],
            input_scope_mask: u64::from_le_bytes(buf[6..14].try_into().ok()?),
        })
    }
}

// ──────────────────────────────────────────────
// Diag Snapshot Payload (64B 定长头 + 3 段变长类名)
// ──────────────────────────────────────────────

/// 焦点窗口句柄的来源域（`DiagSnapshotPayload::focus_hwnd_source`）。
///
/// ⚠ **三条通路给出的不是同一件东西**，压进一个字段而不标来源，下游就再也分不开了
/// ——这与 [`caret_source`] 给 caret 坐标分域是同一个教训（曾让 `GetGUIThreadInfo` 的
/// Win32 光标冒充 TSF 插入点，Word 非正文行错位 814px）。这里尤其要命：
/// `Foreground` 域的窗口**可能根本不属于本进程**（Win10 任务栏搜索就是前台窗口归
/// SearchUI、焦点在 explorer），拿它当"焦点窗口"去推 per-app 判据必然推错。
pub mod window_source {
    /// 一个都没拿到（受限宿主可能三条全空）。
    pub const NONE: u8 = 0;
    /// `ITfContextView::GetWnd()`——TSF 域，最准；受限宿主（SearchHost）常返回 null。
    pub const TSF_VIEW: u8 = 1;
    /// `GetGUIThreadInfo().hwndFocus`——线程域，属于本进程但未必是 TSF 上下文所在窗口。
    pub const GUI_THREAD: u8 = 2;
    /// `GetForegroundWindow()`——**跨进程**，最后兜底，判据价值最低。
    pub const FOREGROUND: u8 = 3;

    /// 来源的中文标签（HUD 展示用）。
    pub fn label(v: u8) -> &'static str {
        match v {
            TSF_VIEW => "TSF",
            GUI_THREAD => "GUI",
            FOREGROUND => "前台",
            _ => "无",
        }
    }
}

/// [`DiagSnapshotPayload::flags`] 位：本次焦点相对上一次换了 DocMgr。
///
/// 只有 DLL 知道这件事（它持有 `_pLastActiveDocMgr`），服务端无从推导，故必须随包上报。
pub const DIAG_FLAG_DOCMGR_CHANGED: u8 = 1 << 0;

/// 诊断快照载荷：焦点窗口链 + 前台窗口 + TSF 上下文实例 id。
///
/// 布局＝64 字节定长头 + 三段 `len(u16 LE) + UTF-8` 类名（focus / root / foreground）。
/// 定长头刻意留 `reserved`，加字段时优先吃它，避免又一次改动偏移量。
///
/// **句柄一律按 u64 传**：DLL 可能是 32 位也可能是 64 位，`HWND` 宽度不同；统一
/// 零扩展成 u64 后两侧偏移才是一个固定值。这些值只用于展示与同一性比较（"还是不是
/// 刚才那个窗口/文档"），服务进程不会拿它去调任何 Win32 API——跨进程句柄无效。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagSnapshotPayload {
    /// 宿主进程 id（DLL 所在进程）。
    pub pid: u32,
    /// 前台窗口所属进程 id。与 `pid` 不同即说明"输入焦点和前台窗口分属两个进程"。
    pub fg_pid: u32,
    /// 焦点窗口句柄，来源见 `focus_hwnd_source`。
    pub focus_hwnd: u64,
    /// 焦点窗口的顶层窗口（`GetAncestor(GA_ROOT)`）。**per-app 窗口级判据取这个**
    /// ——控件自身类名（`Edit`/`DirectUIHWND`）跨版本不稳定，顶层类名
    /// （`Shell_TrayWnd`/`CabinetWClass`/`Progman`）才是干净的判据。
    pub root_hwnd: u64,
    /// 前台窗口句柄（可能属于别的进程）。
    pub fg_hwnd: u64,
    /// 焦点 `ITfDocumentMgr` 的指针值，仅作实例同一性标识。
    pub docmgr_id: u64,
    /// 焦点 `ITfContext`（DocMgr 的 top context）指针值，仅作实例同一性标识。
    pub context_id: u64,
    /// DLL 的焦点会话序号（`_focusSessionId`），用于把 HUD 快照与日志对齐。
    pub focus_session_id: u32,
    /// 顶层窗口的 z-band（`GetWindowBand`，未文档化导出；取不到为 0）。
    pub root_band: u32,
    /// DLL 的 host-render band 窗口当前 band（0 = 未建 host 窗口）。
    pub host_band: u32,
    /// `focus_hwnd` 的来源域，取值见 [`window_source`]。
    pub focus_hwnd_source: u8,
    /// 位标志，见 [`DIAG_FLAG_DOCMGR_CHANGED`]。
    pub flags: u8,
    /// 焦点窗口类名。
    pub focus_class: String,
    /// 顶层窗口类名。
    pub root_class: String,
    /// 前台窗口类名。
    pub fg_class: String,
}

impl DiagSnapshotPayload {
    /// 定长头字节数（变长类名区跟在其后）。
    pub const HEAD_SIZE: usize = 64;

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = vec![0u8; Self::HEAD_SIZE];
        b[0..4].copy_from_slice(&self.pid.to_le_bytes());
        b[4..8].copy_from_slice(&self.fg_pid.to_le_bytes());
        b[8..16].copy_from_slice(&self.focus_hwnd.to_le_bytes());
        b[16..24].copy_from_slice(&self.root_hwnd.to_le_bytes());
        b[24..32].copy_from_slice(&self.fg_hwnd.to_le_bytes());
        b[32..40].copy_from_slice(&self.docmgr_id.to_le_bytes());
        b[40..48].copy_from_slice(&self.context_id.to_le_bytes());
        b[48..52].copy_from_slice(&self.focus_session_id.to_le_bytes());
        b[52..56].copy_from_slice(&self.root_band.to_le_bytes());
        b[56..60].copy_from_slice(&self.host_band.to_le_bytes());
        b[60] = self.focus_hwnd_source;
        b[61] = self.flags;
        // b[62..64] reserved，保持 0
        for s in [&self.focus_class, &self.root_class, &self.fg_class] {
            let bytes = s.as_bytes();
            let len = bytes.len().min(u16::MAX as usize);
            b.extend_from_slice(&(len as u16).to_le_bytes());
            b.extend_from_slice(&bytes[..len]);
        }
        b
    }

    /// 解析。头部不足即 `None`；变长区**残缺只让对应类名退化为空串**，不否掉整包
    /// ——诊断数据宁可缺一格也要能显示其余部分（同 `de_initial_mode` 的爆炸半径思路）。
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::HEAD_SIZE {
            return None;
        }
        let mut p = Self {
            pid: u32::from_le_bytes(buf[0..4].try_into().ok()?),
            fg_pid: u32::from_le_bytes(buf[4..8].try_into().ok()?),
            focus_hwnd: u64::from_le_bytes(buf[8..16].try_into().ok()?),
            root_hwnd: u64::from_le_bytes(buf[16..24].try_into().ok()?),
            fg_hwnd: u64::from_le_bytes(buf[24..32].try_into().ok()?),
            docmgr_id: u64::from_le_bytes(buf[32..40].try_into().ok()?),
            context_id: u64::from_le_bytes(buf[40..48].try_into().ok()?),
            focus_session_id: u32::from_le_bytes(buf[48..52].try_into().ok()?),
            root_band: u32::from_le_bytes(buf[52..56].try_into().ok()?),
            host_band: u32::from_le_bytes(buf[56..60].try_into().ok()?),
            focus_hwnd_source: buf[60],
            flags: buf[61],
            ..Default::default()
        };
        let mut off = Self::HEAD_SIZE;
        let next = |buf: &[u8], off: &mut usize| -> String {
            if *off + 2 > buf.len() {
                return String::new();
            }
            let len = u16::from_le_bytes([buf[*off], buf[*off + 1]]) as usize;
            *off += 2;
            let end = (*off + len).min(buf.len());
            let s = String::from_utf8_lossy(&buf[*off..end]).into_owned();
            *off = end;
            s
        };
        p.focus_class = next(buf, &mut off);
        p.root_class = next(buf, &mut off);
        p.fg_class = next(buf, &mut off);
        Some(p)
    }

    /// 本次焦点是否换了 DocMgr。
    pub fn docmgr_changed(&self) -> bool {
        self.flags & DIAG_FLAG_DOCMGR_CHANGED != 0
    }

    /// 前台窗口是否属于**别的**进程。Win10 任务栏搜索的判据信号：焦点在 explorer，
    /// 前台窗口却归 SearchUI/SearchApp——只看进程名永远看不出这件事。
    pub fn foreground_is_other_process(&self) -> bool {
        self.fg_pid != 0 && self.pid != 0 && self.fg_pid != self.pid
    }
}

// ──────────────────────────────────────────────
// Commit Request Payload (12 + variable)
// ──────────────────────────────────────────────

/// 提交请求载荷 (12 + variable)
#[derive(Clone, Debug)]
pub struct CommitRequestPayload {
    pub barrier_seq: u16,
    pub trigger_key: u16,
    pub modifiers: u32,
    pub input_buffer: String,
}

// ──────────────────────────────────────────────
// Status Header (12 bytes + variable)
// ──────────────────────────────────────────────

/// 状态更新头 (12 bytes)
#[derive(Clone, Debug)]
pub struct StatusHeader {
    pub flags: u32,
    pub key_down_count: u32,
    pub key_up_count: u32,
    pub key_hashes: Vec<u32>,
    pub icon_label: String,
}

// ──────────────────────────────────────────────
// Commit Text Header (12 bytes + variable)
// ──────────────────────────────────────────────

/// Commit 文本头 (12 bytes)
#[derive(Clone, Debug)]
pub struct CommitTextHeader {
    pub flags: u32,
    pub text_length: u32,
    pub composition_length: u32,
}

impl CommitTextHeader {
    pub const SIZE: usize = 12;

    pub fn has_new_composition(&self) -> bool {
        self.flags & 0x02 != 0
    }

    pub fn chinese_mode(&self) -> bool {
        self.flags & 0x04 != 0
    }

    pub fn mode_changed(&self) -> bool {
        self.flags & 0x01 != 0
    }
}

// ──────────────────────────────────────────────
// Shared Render Header (64 bytes)
// ──────────────────────────────────────────────

/// 共享渲染头 (64 bytes)
pub const SHARED_RENDER_MAGIC: u32 = 0x57494E44; // 'WIND'
pub const SHARED_RENDER_VERSION: u32 = 1;
pub const MAX_SHARED_RENDER_SIZE: usize = 4 * 1024 * 1024; // 4MB

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct SharedRenderHeader {
    pub magic: u32,
    pub version: u32,
    pub sequence: u32,
    pub flags: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub data_size: u32,
    pub rect_count: u32,           // @40 候选 hit 矩形数
    pub rects_offset: u32,         // @44 hit 矩形表相对 SHM 基址偏移
    pub rendered_hover_index: i32, // @48 高亮候选索引（-1 无 / -2,-3 翻页）
    pub target_instance_id: u32,   // @52 darwin 忽略
    pub reserved: [u32; 2],        // @56..64
}

impl SharedRenderHeader {
    pub const SIZE: usize = 64;

    pub const FLAG_VISIBLE: u32 = 0x0001;
    pub const FLAG_CONTENT_READY: u32 = 0x0002;
    pub const FLAG_SOFTWARE_SHADOW: u32 = 0x0004;
    /// 帧里的 `(x, y)` 是**用户固定位置**的绝对屏幕坐标，不是按光标推算出来的落点。
    ///
    /// 只对 macOS 的 host-render 路径有意义：`.app` 收到普通帧时会自己做「下方放不下就
    /// 翻到光标上方」的兜底，那套逻辑在固定位置下是错的——窗口本来就不跟光标走，一旦
    /// 固定点靠近屏幕底边就会被莫名弹到顶上。置本位即告诉 `.app`：照搬坐标，只做屏幕
    /// 边界钳制，不要翻转。
    pub const FLAG_ABSOLUTE_POS: u32 = 0x0008;

    pub fn new(x: i32, y: i32, width: u32, height: u32, stride: u32, data_size: u32) -> Self {
        Self {
            magic: SHARED_RENDER_MAGIC,
            version: SHARED_RENDER_VERSION,
            sequence: 0,
            flags: Self::FLAG_VISIBLE | Self::FLAG_CONTENT_READY,
            x,
            y,
            width,
            height,
            stride,
            data_size,
            rect_count: 0,
            rects_offset: 0,
            rendered_hover_index: -1,
            target_instance_id: 0,
            reserved: [0; 2],
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        // SAFETY: SharedRenderHeader is repr(C, packed) with no padding
        unsafe { std::mem::transmute_copy(self) }
    }
}

// ──────────────────────────────────────────────
// 语言栏图标 SHM（服务端预渲染 → wind_tsf 的 GetIcon 消费）
// ──────────────────────────────────────────────

/// 图标 SHM 魔数 `'WICO'`（字节序 W,I,C,O，小端读作 `0x4F43_4957`）。
pub const ICON_SHM_MAGIC: u32 = 0x4F43_4957;
/// 布局版本。**本功能尚未随任何版本发布**，故开发期内改布局（补 40/48 两档、放大
/// SHM）不占版本号——外面没有任何一代在跑，留着历史编号只会让人误以为存在兼容包袱。
/// 首个发布版本即 1；发布之后再改布局才 bump。
///
/// 版本不匹配时读端直接判失败、退回本地绘制，而不是硬读——DLL 与服务分属两个
/// 部署单元，先更新哪个都可能。降级的表现是「图标还在、只是没有角标」，可接受；
/// 硬读的表现是任务栏上出现一张错位的花屏。
pub const ICON_SHM_VERSION: u32 = 1;

/// 图标 SHM 名（不走握手，两端各自按固定规则拼）。
///
/// ⚠️ **跨仓命名契约，无编译期约束**：C++ 侧 `Globals.h` 的 `WIND_ICON_SHM_NAME`
/// 必须与本函数结果逐字一致，否则 DLL 永远打不开 SHM、静默退回本地绘制
/// （表现为「图标能显示但从不跟随标点变化」，没有任何报错）。
///
/// `suffix` 取 `wind_config::variant::pipe_suffix()`（`""` / `"_dev"`），
/// 与主/推送管道同源。**不要**改用 `app_dir_name()` 那套 `Dev` 风格后缀——
/// macOS 侧曾因两种后缀风格混用，导致 dev 变体的 bridge/SHM 全程握不上手。
///
/// `Local\` 前缀提供终端服务会话级隔离，与 HostRender 的 SHM 同策略（不含 SID）。
///
/// ★ **名字里必须带版本，且要随 [`ICON_SHM_VERSION`] 走。** 命名 section 的名字一旦
/// 存在，**尺寸也就被钉死了**：只要还有一个进程持着映射，内核对象就活着，此时以更大的
/// view 大小去 `MapViewOfFile` 会返回 `ACCESS_DENIED`——一个指向权限、完全不指向真正
/// 原因的错误码。开发期把 SHM 从 64 KiB 提到 128 KiB 时就是这样卡住的：几十个宿主
/// 进程里的旧 DLL 各持一份 64 KiB 映射，新服务怎么也建不出 128 KiB，而持有者名单里有
/// `explorer.exe` 与 `SearchHost.exe`，腾干净约等于注销一次。
///
/// 版本进名字之后，新旧两代用的是不同的内核对象，各自活到自己最后一个持有者退出为止，
/// 互不阻塞。代价是跨版本的新服务 + 旧 DLL 会互相看不见 SHM——那正是想要的结果：
/// 退回本地绘制（图标照常显示，只是没角标），而不是读出一张按旧布局解释的花屏。
///
/// 直接拼 [`ICON_SHM_VERSION`] 而不是写死 `_v1`，是为了让「改版本号」与「改名字」
/// 变成同一个动作——两者分开写就一定会有一次只改其中一个。
pub fn icon_shm_name(suffix: &str) -> String {
    format!("Local\\WindInput_IconShm_v{ICON_SHM_VERSION}{suffix}")
}

/// 预渲染的尺寸档，对应 100/125/150/175/200/250/300% DPI。
///
/// 备多档而非按 DPI 现算一个，是因为 `ITfLangBarItemButton::GetIcon` **没有尺寸参数**：
/// 图标多大由我们创建位图时决定，系统拿去后如何缩放不可见。备齐档位后，
/// 选档逻辑将来若要修正，改的只是选择，不必重做渲染。
///
/// 40/48 两档是真机实测补的：300% 缩放下该给 48，而档位表原本止于 32，
/// DLL 只能取最接近的 32 再由系统放大——放大正是最糊的那种情形
/// （同机对照：原生无缩放的那档明显更清晰）。
pub const ICON_SIZES: [u16; 7] = [16, 20, 24, 28, 32, 40, 48];

/// 主题档：亮色任务栏用深色图标，暗色任务栏用浅色图标。
pub const ICON_THEME_LIGHT: u8 = 0;
pub const ICON_THEME_DARK: u8 = 1;
pub const ICON_THEME_COUNT: usize = 2;

/// 变体总数 = 尺寸档 × 主题档。
pub const ICON_VARIANT_COUNT: usize = ICON_SIZES.len() * ICON_THEME_COUNT;

/// 单个变体的位图字节数（BGRA，非预乘）。
pub const fn icon_variant_bytes(size_px: u16) -> usize {
    (size_px as usize) * (size_px as usize) * 4
}

/// 单 slot 字节数 = 全部变体位图之和。
pub const fn icon_slot_stride() -> usize {
    let mut total = 0usize;
    let mut i = 0usize;
    while i < ICON_SIZES.len() {
        total += icon_variant_bytes(ICON_SIZES[i]) * ICON_THEME_COUNT;
        i += 1;
    }
    total
}

/// 变体表相对 SHM 基址的偏移（紧跟 header）。
pub const ICON_TABLE_OFFSET: usize = IconShmHeader::SIZE;

/// slot 0 起点。表尾对齐到 512 边界，留出加变体档位的余量。
///
/// ⚠ 加尺寸档时**必须一并复核本值**：变体表紧跟 64 B 的 header，可用空间是
/// `ICON_SLOT0_OFFSET - ICON_TABLE_OFFSET`，每个变体 16 B。原来的 256 只放得下
/// 12 个变体，而 7 档 × 2 主题 = 14 个已经装不下——`icon_shm_layout_fits_without_overlap`
/// 正是为这一步准备的。
pub const ICON_SLOT0_OFFSET: usize = 512;

/// SHM 总大小，取 128 KiB 整（7 档双主题双缓冲实际用量约 109 KiB）。
pub const ICON_SHM_SIZE: usize = 128 * 1024;

/// 图标 SHM 头部（64 B）。
///
/// 双缓冲：写端始终写 `1 - active_slot`，写完再切换 `active_slot` 并递增 `sequence`。
/// 读端读 `sequence` → 拷贝 → 重读 `sequence`，两次不等说明拷贝期间发生了切换，重试。
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct IconShmHeader {
    pub magic: u32,         // @0
    pub version: u32,       // @4
    pub sequence: u32,      // @8   每次更新递增（读端据此做 seqlock 校验）
    pub active_slot: u32,   // @12  0 或 1
    pub variant_count: u32, // @16
    pub slot_stride: u32,   // @20  单 slot 字节数
    pub slot0_offset: u32,  // @24  slot 0 相对 SHM 基址偏移
    pub table_offset: u32,  // @28  变体表相对 SHM 基址偏移
    pub reserved: [u32; 8], // @32..64
}

impl IconShmHeader {
    pub const SIZE: usize = 64;

    pub fn new() -> Self {
        Self {
            magic: ICON_SHM_MAGIC,
            version: ICON_SHM_VERSION,
            sequence: 0,
            active_slot: 0,
            variant_count: ICON_VARIANT_COUNT as u32,
            slot_stride: icon_slot_stride() as u32,
            slot0_offset: ICON_SLOT0_OFFSET as u32,
            table_offset: ICON_TABLE_OFFSET as u32,
            reserved: [0; 8],
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        // SAFETY: repr(C, packed) 且字段总长恰为 SIZE，无填充
        unsafe { std::mem::transmute_copy(self) }
    }
}

impl Default for IconShmHeader {
    fn default() -> Self {
        Self::new()
    }
}

/// 变体表条目（16 B）。两个 slot 共用同一张表，`offset` 相对**所属 slot 起点**。
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct IconVariant {
    pub size_px: u16,  // @0
    pub theme: u8,     // @2   ICON_THEME_LIGHT / ICON_THEME_DARK
    pub flags: u8,     // @3
    pub offset: u32,   // @4   相对所属 slot 起点
    pub byte_len: u32, // @8
    pub reserved: u32, // @12
}

impl IconVariant {
    pub const SIZE: usize = 16;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        // SAFETY: repr(C, packed) 且字段总长恰为 SIZE，无填充
        unsafe { std::mem::transmute_copy(self) }
    }
}

/// 按固定顺序（尺寸档外层、主题内层）生成变体表。
///
/// 顺序即契约：C++ 侧靠 `(size_px, theme)` 匹配而非下标，故顺序变化不会致错，
/// 但保持稳定可以让排查时的十六进制转储可读。
pub fn icon_variant_table() -> Vec<IconVariant> {
    let mut table = Vec::with_capacity(ICON_VARIANT_COUNT);
    let mut offset = 0u32;
    for &size_px in &ICON_SIZES {
        for theme in [ICON_THEME_LIGHT, ICON_THEME_DARK] {
            let byte_len = icon_variant_bytes(size_px) as u32;
            table.push(IconVariant {
                size_px,
                theme,
                flags: 0,
                offset,
                byte_len,
                reserved: 0,
            });
            offset += byte_len;
        }
    }
    table
}

// ──────────────────────────────────────────────
// Modifier flags
// ──────────────────────────────────────────────

pub const MOD_SHIFT: u32 = 0x0001;
pub const MOD_CTRL: u32 = 0x0002;
pub const MOD_ALT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
pub const MOD_LSHIFT: u32 = 0x0010;
pub const MOD_RSHIFT: u32 = 0x0020;
pub const MOD_LCTRL: u32 = 0x0040;
pub const MOD_RCTRL: u32 = 0x0080;
pub const MOD_CAPSLOCK: u32 = 0x0100;

/// **宿主快捷键修饰位**：带上其中任意一位的键，未命中热键白名单就归宿主，输入法一概不碰。
///
/// 含 `MOD_WIN` 是 macOS 的刚需：那边 `MOD_WIN` 承载的是 **Command 键**（见
/// `KeyHandler.toModifiers`），⌘C/⌘V/⌘A 全靠这条判据交还宿主。此前判据只掩
/// `MOD_CTRL | MOD_ALT`（Win 键上没有快捷键语义，是 Windows 血统的遗留），于是中文模式下
/// ⌘+字母一路走到字母臂被当成码元吃掉 —— 表现为「开着输入法就复制粘贴不了」（issue #64）。
///
/// Windows 侧无回归：Win+键由系统抢先处理，本就到不了 TSF。
pub const MOD_SHORTCUT: u32 = MOD_CTRL | MOD_ALT | MOD_WIN;

/// 计算热键哈希值：(modifiers << 16) | (keyCode & 0xFFFF)
pub fn calc_key_hash(modifiers: u32, key_code: u32) -> u32 {
    (modifiers << 16) | (key_code & 0xFFFF)
}

/// 热键策略位
pub const HOTKEY_POLICY_CHINESE_ONLY: u32 = 0x40000000;
pub const HOTKEY_POLICY_SESSION: u32 = 0x80000000;

// ──────────────────────────────────────────────
// Status flags (与 Go StatusChineseMode 等对齐)
// ──────────────────────────────────────────────

pub const STATUS_CHINESE_MODE: u32 = 0x0001;
pub const STATUS_FULL_WIDTH: u32 = 0x0002;
pub const STATUS_CHINESE_PUNCT: u32 = 0x0004;
pub const STATUS_TOOLBAR_VISIBLE: u32 = 0x0008;
pub const STATUS_MODE_CHANGED: u32 = 0x0010;
pub const STATUS_CAPS_LOCK: u32 = 0x0020;
pub const STATUS_HOST_RENDER_AVAIL: u32 = 0x0040;
/// 软键盘面板开着。**C++ 的吃键判定要用它**：中文模式无 input session 时数字键本是
/// 交还宿主的（`session_select_or_page` 那支只在有 session 时吃），而软键盘的数字行
/// 需要它们被吃下来。位值必须与 `BinaryProtocol.h` 的 `STATUS_SOFT_KEYBOARD` 一致。
pub const STATUS_SOFT_KEYBOARD: u32 = 0x0080;
/// 当前面是**键盘面**（`send_keys`），不是符号面。
///
/// ★ 它决定 C++ 要不要启用软键盘总闸。键盘面把按键交还输入法：字母/数字/标点一律
/// 落回常规判定链（中文模式吃并转发、英文模式放行），与没开面板时完全一致；只有
/// Esc 与翻页仍归面板。少了这一位，键盘面在英文模式下会「吃了不发」——总闸吃掉键，
/// 而常规链路在英文态返回 PassThrough，键彻底消失。
/// 位值必须与 `BinaryProtocol.h` 的 `STATUS_SOFT_KEYBOARD_KEYS` 一致。
pub const STATUS_SOFT_KEYBOARD_KEYS: u32 = 0x0100;

// ──────────────────────────────────────────────
// Commit result flags
// ──────────────────────────────────────────────

pub const COMMIT_FLAG_MODE_CHANGED: u16 = 0x0001;
pub const COMMIT_FLAG_HAS_NEW_COMPOSITION: u16 = 0x0002;
pub const COMMIT_FLAG_CHINESE_MODE: u16 = 0x0004;
// bit3 已被 CommitText 的 replacingHeld 占用（见 encode_commit_text_replacing_held），
// barrier 的 CommitResult 路径不用它——此处登记以免同一位被挪作他用。
pub const COMMIT_FLAG_REPLACING_HELD: u16 = 0x0008;

// ──────────────────────────────────────────────
// Event type
// ──────────────────────────────────────────────

pub const EVENT_KEY_DOWN: u8 = 0;
pub const EVENT_KEY_UP: u8 = 1;

#[cfg(test)]
mod input_diag_wire_tests {
    use super::*;

    #[test]
    fn focus_gained_backward_compat_36_bytes() {
        // 旧 36 字节载荷（无 disabled/reason）仍可解，新字段默认 0
        let mut buf = vec![0u8; 36];
        buf[20..28].copy_from_slice(&7u64.to_le_bytes()); // client_token
        buf[28..36].copy_from_slice(&(1u64 << 31).to_le_bytes()); // input_scope_mask
        let p = FocusGainedPayload::from_bytes(&buf).unwrap();
        assert_eq!(p.client_token, 7);
        assert_eq!(p.input_scope_mask, 1 << 31);
        assert_eq!(p.disabled, 0);
        assert_eq!(p.reason, 0);
    }

    #[test]
    fn focus_gained_reads_new_fields_38_bytes() {
        let mut buf = vec![0u8; 38];
        buf[36] = 1; // disabled
        buf[37] = 2; // reason
        let p = FocusGainedPayload::from_bytes(&buf).unwrap();
        assert_eq!(p.disabled, 1);
        assert_eq!(p.reason, 2);
    }

    #[test]
    fn input_state_report_roundtrip() {
        let r = InputStateReportPayload {
            pid: 4242,
            disabled: 1,
            reason: 1,
            input_scope_mask: 1 << 31,
        };
        let bytes = r.to_bytes();
        assert_eq!(bytes.len(), InputStateReportPayload::SIZE);
        let d = InputStateReportPayload::from_bytes(&bytes).unwrap();
        assert_eq!(d.pid, 4242);
        assert_eq!(d.disabled, 1);
        assert_eq!(d.reason, 1);
        assert_eq!(d.input_scope_mask, 1 << 31);
    }

    fn sample_diag() -> DiagSnapshotPayload {
        DiagSnapshotPayload {
            pid: 4242,
            fg_pid: 777,
            focus_hwnd: 0x0000_0000_00A1_B2C3,
            root_hwnd: 0x0000_0001_1122_3344,
            fg_hwnd: 0x0000_0000_0055_6677,
            docmgr_id: 0x7FF6_1234_5678_9ABC,
            context_id: 0x7FF6_1234_5678_0000,
            focus_session_id: 19,
            root_band: 13,
            host_band: 6,
            focus_hwnd_source: window_source::TSF_VIEW,
            flags: DIAG_FLAG_DOCMGR_CHANGED,
            focus_class: "Edit".into(),
            root_class: "Shell_TrayWnd".into(),
            fg_class: "Windows.UI.Core.CoreWindow".into(),
        }
    }

    #[test]
    fn diag_snapshot_roundtrip() {
        let p = sample_diag();
        let bytes = p.to_bytes();
        let d = DiagSnapshotPayload::from_bytes(&bytes).expect("应可解析");
        assert_eq!(d, p, "全字段往返必须一致");
        assert!(d.docmgr_changed());
        assert!(
            d.foreground_is_other_process(),
            "fg_pid≠pid 即前台属于别的进程——Win10 搜索框场景的关键信号"
        );
    }

    /// 头部偏移是两侧手写序列化的唯一契约（C++ 侧同样手写字节，没有编译期约束把它们绑住）。
    /// 钉死总长与几个关键字段的落点，改布局时必须显式过这一关并同步 `BinaryProtocol.h`。
    #[test]
    fn diag_snapshot_head_layout_is_frozen() {
        let p = sample_diag();
        let b = p.to_bytes();
        assert_eq!(DiagSnapshotPayload::HEAD_SIZE, 64);
        assert_eq!(&b[0..4], &4242u32.to_le_bytes(), "pid @0");
        assert_eq!(&b[4..8], &777u32.to_le_bytes(), "fg_pid @4");
        assert_eq!(&b[48..52], &19u32.to_le_bytes(), "focus_session_id @48");
        assert_eq!(&b[52..56], &13u32.to_le_bytes(), "root_band @52");
        assert_eq!(b[60], window_source::TSF_VIEW, "focus_hwnd_source @60");
        assert_eq!(&b[62..64], &[0, 0], "reserved 必须留零");
        // 变长区紧跟头部：第一段是 focus_class。
        assert_eq!(&b[64..66], &4u16.to_le_bytes(), "focus_class 长度前缀");
        assert_eq!(&b[66..70], b"Edit");
    }

    /// 变长区被截断时只让对应类名退化为空串，头部字段与已完整的段必须照常可读。
    /// 诊断数据缺一格也要能显示其余部分——整包否掉等于排查时两眼一抹黑。
    #[test]
    fn diag_snapshot_tolerates_truncated_tail() {
        let p = sample_diag();
        let full = p.to_bytes();
        // 砍到只剩头部 + 第一段类名
        let cut = 64 + 2 + 4;
        let d = DiagSnapshotPayload::from_bytes(&full[..cut]).expect("头部完整即应可解析");
        assert_eq!(d.root_band, 13, "头部字段不受尾部截断影响");
        assert_eq!(d.focus_class, "Edit", "完整的段照常可读");
        assert_eq!(d.root_class, "", "缺失的段退化为空串");
        assert_eq!(d.fg_class, "");
        // 头部本身不足则整包无效（无法信任任何字段）。
        assert!(DiagSnapshotPayload::from_bytes(&full[..63]).is_none());
    }

    #[test]
    fn diag_snapshot_same_process_foreground_not_flagged() {
        let mut p = sample_diag();
        p.fg_pid = p.pid;
        assert!(!p.foreground_is_other_process());
        // pid 未知（0）时不得误报"跨进程"——那只是采集失败。
        p.fg_pid = 0;
        assert!(!p.foreground_is_other_process());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 头部与表项的实际内存布局必须等于两端约定的常量。
    ///
    /// `repr(C, packed)` 下加字段不会报错，只会悄悄改变后续字段偏移——
    /// 而 C++ 侧是按固定偏移读的，届时读到的是错位的垃圾值。这条断言是唯一的拦截点。
    #[test]
    fn icon_shm_struct_sizes_match_declared() {
        assert_eq!(std::mem::size_of::<IconShmHeader>(), IconShmHeader::SIZE);
        assert_eq!(std::mem::size_of::<IconVariant>(), IconVariant::SIZE);
        assert_eq!(IconShmHeader::new().to_bytes().len(), 64);
    }

    /// 头部关键字段的字节偏移——C++ 侧按这些偏移读。
    #[test]
    fn icon_shm_header_field_offsets() {
        let mut h = IconShmHeader::new();
        h.sequence = 0xA1A2_A3A4;
        h.active_slot = 1;
        let b = h.to_bytes();
        assert_eq!(&b[0..4], &ICON_SHM_MAGIC.to_le_bytes()); // magic @0
        assert_eq!(&b[4..8], &ICON_SHM_VERSION.to_le_bytes()); // version @4
        assert_eq!(&b[8..12], &0xA1A2_A3A4u32.to_le_bytes()); // sequence @8
        assert_eq!(&b[12..16], &1u32.to_le_bytes()); // active_slot @12
        assert_eq!(&b[16..20], &(ICON_VARIANT_COUNT as u32).to_le_bytes()); // variant_count @16
        assert_eq!(&b[32..64], &[0u8; 32]); // reserved @32..64
    }

    /// 魔数字节序必须是可读的 'W','I','C','O'——十六进制转储时能一眼认出是图标 SHM。
    #[test]
    fn icon_shm_magic_reads_as_wico() {
        assert_eq!(&ICON_SHM_MAGIC.to_le_bytes(), b"WICO");
    }

    /// 布局自洽：表不越进 slot0，双 slot 不超出 SHM 总大小。
    ///
    /// 加尺寸档时最容易越界，而越界的后果是写穿到下一个 slot——
    /// 表现为某些档位的图标随机变成别的档位的内容，极难反查。
    #[test]
    fn icon_shm_layout_fits_without_overlap() {
        let table_end = ICON_TABLE_OFFSET + IconVariant::SIZE * ICON_VARIANT_COUNT;
        assert!(
            table_end <= ICON_SLOT0_OFFSET,
            "变体表 {table_end} 越进了 slot0 起点 {ICON_SLOT0_OFFSET}"
        );
        let needed = ICON_SLOT0_OFFSET + icon_slot_stride() * 2;
        assert!(
            needed <= ICON_SHM_SIZE,
            "双 slot 共需 {needed} 字节，超出 SHM 总大小 {ICON_SHM_SIZE}"
        );
    }

    /// 变体表覆盖全部 (尺寸 × 主题) 组合，且偏移首尾相接、无空洞无重叠。
    #[test]
    fn icon_variant_table_is_contiguous_and_complete() {
        let table = icon_variant_table();
        assert_eq!(table.len(), ICON_VARIANT_COUNT);

        let mut expect_offset = 0u32;
        for v in &table {
            assert_eq!({ v.offset }, expect_offset, "变体表出现空洞或重叠");
            assert_eq!({ v.byte_len }, icon_variant_bytes(v.size_px) as u32);
            expect_offset += v.byte_len;
        }
        // 表末偏移恰好等于单 slot 长度：既无剩余也无溢出
        assert_eq!(expect_offset as usize, icon_slot_stride());

        // 每个尺寸档都要有亮/暗两份
        for &size in &ICON_SIZES {
            for theme in [ICON_THEME_LIGHT, ICON_THEME_DARK] {
                assert!(
                    table.iter().any(|v| v.size_px == size && v.theme == theme),
                    "缺变体 size={size} theme={theme}"
                );
            }
        }
    }

    /// SHM 名的变体后缀走管道那套 `_dev`，不是应用目录那套 `Dev`。
    ///
    /// 这两种风格混用过一次（macOS 侧），后果是 dev 变体两端各开各的共享内存、
    /// 全程握不上手且无任何报错。C++ 侧 `Globals.h` 的 `WIND_ICON_SHM_NAME` 必须逐字一致。
    #[test]
    fn icon_shm_name_uses_pipe_style_suffix() {
        assert_eq!(icon_shm_name(""), "Local\\WindInput_IconShm_v1");
        assert_eq!(icon_shm_name("_dev"), "Local\\WindInput_IconShm_v1_dev");
    }

    /// 版本号进了名字，所以 bump `ICON_SHM_VERSION` 会静默改掉 SHM 名——而 C++ 侧的
    /// `WIND_ICON_SHM_NAME` 是写死的字面量，跟不上就是「DLL 永远打不开 SHM」。
    /// 本测试让 bump 在这里先失败一次，逼着改的人去同步 `Globals.h`。
    #[test]
    fn icon_shm_name_carries_current_version() {
        assert_eq!(
            ICON_SHM_VERSION, 1,
            "改版本号必须同步 Globals.h 的 WIND_ICON_SHM_NAME"
        );
        assert!(
            icon_shm_name("").contains(&format!("_v{ICON_SHM_VERSION}")),
            "SHM 名必须带版本，否则改尺寸会被旧进程持有的同名 section 锁死"
        );
    }

    #[test]
    fn shared_render_header_field_offsets_match_go_swift() {
        // 对齐 Swift SharedMemoryReader.swift / Go binary_protocol.go 的命名字段偏移
        let mut h = SharedRenderHeader::new(
            0x11223344, 0x55667788, 0x99AABBCC, 0xDDEE0011, 0x22334455, 0x66778899,
        );
        h.sequence = 0xA1A2A3A4;
        h.rect_count = 0xB1B2B3B4;
        h.rects_offset = 0xC1C2C3C4;
        h.rendered_hover_index = -3;
        h.target_instance_id = 0xE1E2E3E4;
        let b = h.to_bytes();
        assert_eq!(b.len(), 64);
        assert_eq!(SharedRenderHeader::SIZE, 64);
        assert_eq!(&b[8..12], &0xA1A2A3A4u32.to_le_bytes()); // sequence @8
        assert_eq!(&b[40..44], &0xB1B2B3B4u32.to_le_bytes()); // rect_count @40
        assert_eq!(&b[44..48], &0xC1C2C3C4u32.to_le_bytes()); // rects_offset @44
        assert_eq!(&b[48..52], &(-3i32).to_le_bytes()); // rendered_hover_index @48
        assert_eq!(&b[52..56], &0xE1E2E3E4u32.to_le_bytes()); // target_instance_id @52
        assert_eq!(&b[56..64], &[0u8; 8]); // reserved[2] @56..64 = 0
    }

    /// 构造一个焦点载荷字节串，`with_source` 决定是否带上第 39 字节。
    fn focus_bytes(with_source: Option<u8>) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&111i32.to_le_bytes()); // caret.x
        b.extend_from_slice(&222i32.to_le_bytes()); // caret.y
        b.extend_from_slice(&33i32.to_le_bytes()); // caret.height
        b.extend_from_slice(&444i32.to_le_bytes()); // comp_start_x
        b.extend_from_slice(&555i32.to_le_bytes()); // comp_start_y
        b.extend_from_slice(&0x0123_4567_89AB_CDEFu64.to_le_bytes()); // client_token
        b.extend_from_slice(&0x8000_0000u64.to_le_bytes()); // input_scope_mask
        b.push(1); // disabled
        b.push(2); // reason
        if let Some(s) = with_source {
            b.push(s);
        }
        b
    }

    /// 尾部追加 `caret_source` 后，**旧 DLL 的 38 字节包必须照常解析**。
    ///
    /// 这条不是形式主义：长度判据若图省事写成 `buf.len() < Self::SIZE`，每加一个尾部字段就会
    /// 把所有旧 DLL 整体拒掉，而表现是「切换应用后输入法毫无反应」——焦点事件根本没进 core，
    /// 日志上看不出是解码拒绝还是压根没收到。
    #[test]
    fn focus_gained_v1_38_bytes_still_parses_with_unknown_source() {
        let b = focus_bytes(None);
        assert_eq!(b.len(), 38, "本用例要覆盖的就是旧 DLL 的 38 字节形态");
        let fg = FocusGainedPayload::from_bytes(&b).expect("38 字节旧包必须能解析");
        // CaretPayload 是 #[repr(packed)]，assert_eq! 会对字段取引用 → 非对齐引用编译错误。
        // 先按值拷到局部再断言。
        let (cx, cy, ch) = (fg.caret.x, fg.caret.y, fg.caret.height);
        assert_eq!(cx, 111);
        assert_eq!(cy, 222);
        assert_eq!(ch, 33);
        assert_eq!(fg.client_token, 0x0123_4567_89AB_CDEF);
        assert_eq!(fg.disabled, 1);
        assert_eq!(fg.reason, 2);
        assert_eq!(
            fg.caret_source,
            caret_source::UNKNOWN,
            "旧包没有来源信息，必须落 UNKNOWN 而不是碰巧读到别的字节"
        );
    }

    /// 39 字节新包读出真实来源，且**不影响任何既有字段的偏移**。
    #[test]
    fn focus_gained_v2_39_bytes_reads_caret_source() {
        let b = focus_bytes(Some(caret_source::TSF_SELECTION as u8));
        assert_eq!(b.len(), 39);
        let fg = FocusGainedPayload::from_bytes(&b).expect("39 字节新包必须能解析");
        assert_eq!(fg.caret_source, caret_source::TSF_SELECTION);
        // 既有字段逐个复核：尾部追加的全部意义就是它们的偏移没动。
        // （packed 字段先按值拷到局部，理由同上一个用例。）
        let (cx, csx, csy) = (
            fg.caret.x,
            fg.caret.composition_start_x,
            fg.caret.composition_start_y,
        );
        assert_eq!(cx, 111);
        assert_eq!(csx, 444);
        assert_eq!(csy, 555);
        assert_eq!(fg.client_token, 0x0123_4567_89AB_CDEF);
        assert_eq!(fg.input_scope_mask, 0x8000_0000);
        assert_eq!(fg.disabled, 1);
        assert_eq!(fg.reason, 2);
        assert_eq!(FocusGainedPayload::SIZE, 39);
    }

    /// GUI 回退坐标**不属于** TSF 语义域——焦点气泡的可信度闸门整个建立在这个判定上。
    #[test]
    fn gui_caret_is_not_tsf_domain() {
        assert!(caret_source::is_tsf(caret_source::TSF_SELECTION));
        assert!(caret_source::is_tsf(caret_source::TSF_COMPOSITION));
        assert!(caret_source::is_tsf(caret_source::TSF_CACHED));
        // 以下每一个都曾经或可能冒充插入点，必须全部判否
        assert!(!caret_source::is_tsf(caret_source::GUI_CARET));
        assert!(!caret_source::is_tsf(caret_source::CONSOLE));
        assert!(!caret_source::is_tsf(caret_source::LAST_KNOWN));
        assert!(!caret_source::is_tsf(caret_source::UNKNOWN));
    }
}
