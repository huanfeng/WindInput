//! 按键管线 / 模式决策。设计依据：`docs/redesign/key-pipeline.md`。
//!
//! 独占模式收敛为单一 [`ModeKind`] 字段 `State.active`：结构上保证「同一时刻至多一个独占
//! 模式」，键事件分派是对它的单次 `match`；新增模式只加一个变体 + 一条 `match` 臂。
//!
//! 不引入 Capability / 共享引擎挂卸：各模式按 schema id 独立查询引擎
//! （`EngineManager::convert_with(schema_id, ...)`），没有需要单点统一的引擎副作用。
//! 决策链顺序由 `Coordinator::handle_key_event` 承载；各模式的按键处理是 `Coordinator` 上紧耦合的
//! `handle_*_key` 方法，故用 enum 分派而非 trait 对象。

/// 当前激活的独占输入模式。`None` 表示普通码表/拼音输入（default processor）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModeKind {
    /// 临时拼音：码表方案下经触发键临时切到拼音反查。
    TempPinyin,
    /// 临时英文：Shift+字母触发的临时英文输入。
    TempEnglish,
    /// 网址模式：普通输入累积到某前缀（如 "www."/"http"）时夺取，原样累积 ASCII。
    Url,
    /// 邮箱模式：正常输入中按下 `@` 时夺取（缓冲非空），续打后缀并补全常用邮箱。
    ///
    /// 与 [`Self::Url`] 只差触发判据：后缀触发（本键是 `@`，缓冲整体作为用户名），见
    /// `docs/design/prefix-hijack-modes.md` §5.2。触发后同一道闸门、同一套 [`Rewind`] 骨架、同一个
    /// 补全候选源，故 `handle_email.rs` 与 `handle_url.rs` 结构刻意逐行平行。
    Email,
    /// Unicode 码点模式：普通输入累积到某前缀（出厂 `u+`）时夺取，续打十六进制出该码点。
    ///
    /// 与 [`Self::Url`] 同属前缀夺取式（两者共用 `try_prefix_hijack` 那道闸门与
    /// [`Rewind`] 回退骨架），差别只在缓冲怎么解释：url 原样累积文本、不产候选，
    /// 本模式把缓冲当十六进制求值、产一条候选。
    Unicode,
    /// 特殊模式：引导键触发，自带码表 + 全码上屏策略。载荷为 `features.special_modes` 下标。
    Special(u8),
    /// 生僻字模式：引导键/热键触发，用当前活跃方案的编码输入，候选只留生僻字。
    ///
    /// 是 [`Self::Special`] 的参数变体：只把引擎换成当前活跃方案（`overlay_engine_schema`）、
    /// 加一道生僻准入，按键处理全部复用 `handle_special_key`，免得同一条输入流实现两遍后分叉。
    /// 从 `overlay_spec` 取值的地方本模式恒为 `None`、落默认档，这是刻意的（它没有 overlay 码表）。
    /// 无载荷：单例，身份不来自任何方案。
    RareChar,
    /// 反查模式：用户自绑键进入，用当前活跃方案的编码查字，缓冲里通配键在任何位置
    /// （含首位）都作通配，不看 `schema.codetable.wildcard` 主开关（spec
    /// `codetable-reverse-mode.md` §3）。
    ///
    /// 与 [`Self::RareChar`] 同理是 special 的参数变体，只有候选另由 `handle_reverse` 构建（不做
    /// 自动上屏、不吃词频重排与候选调整）。它会写 `has_more` / `candidate_limit` /
    /// `scope_relaxed`，故 `exit_special_mode` 一并复位这三位。无载荷：单例。
    Reverse,
    /// 临时 mix：引导键触发，合并多个成员方案候选。载荷为 `features.mix_modes` 下标。
    Mix(u8),
    /// 辅助码：拼音候选的字形二次筛选。独占输入流（组码中无法同时打拼音），但候选
    /// 列表只保留命中者——被滤候选直接丢弃，候选窗只显示匹配词（还原不靠残留
    /// 标记，退出/退格都从会话快照恢复）。
    AuxCode,
}

/// 统一夺取回退登记（对齐 Go decider 的 armRewind/canRewind/rewindHijack）。
///
/// 「夺取式」模式从正常输入流中抢走若干字符进入独占模式（URL 抢前缀、z 抢前导拼音）。
/// 登记此结构后，在前缀边界退格时撤销夺取、把 `snapshot` 回放回正常码表输入流，
/// 而非停留在无候选的独占模式里。URL 与 z（后续）共用这一套，避免各写各的回退。
#[derive(Clone, Debug)]
pub struct Rewind {
    /// 夺取前的来源流缓冲快照（回放目标由 [`Self::origin`] 决定）。
    pub snapshot: String,
    /// 夺取瞬间的模式 buffer（「是否退到边界」判定：当前模式 buffer == 此值即在边界）。
    pub host_text: String,
    /// `snapshot` 该回放到哪条输入流。见 [`RewindOrigin`]。
    pub origin: RewindOrigin,
}

/// 夺取的来源流 —— 决定 `Coordinator::rewind_hijack` 把 `snapshot` 放回哪里。
///
/// 同一模式可从不同流进入：小写 `u+` 从码表缓冲夺取，大写 `U+` 从临时英文转交。共用一条回放
/// 路径会把大写 `U` 塞进码表缓冲、得到空候选。新增来源时必须同时在 `rewind_hijack` 的 `match`
/// 里接住它，否则静默落到既有分支、回错地方。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RewindOrigin {
    /// 正常码表/拼音输入流（`State::input_buffer`）。URL 夺取、z 夺取、小写 `u+` 走这条。
    #[default]
    Normal,
    /// 临时英文缓冲（`State::temp_english_buffer`）。大写 `U+` 的转交走这条。
    TempEnglish,
    /// 快捷输入缓冲（`State::mix_buffer`）。快捷输入里按 `@` 转交邮箱模式走这条。
    ///
    /// `idx` 是融合实例（`State::mix_id`），`prefix` 是显示态前缀（`;`，或字母引导时的 `z`）。
    /// 不能像临英那样清空前缀：它是组合区里看得见的引导符，字母引导的实例回车上屏还要靠它
    /// 把字母还回去（`guide_to_return`）。
    Mix { idx: u8, prefix: String },
}
