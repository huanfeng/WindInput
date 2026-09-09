//! 按键管线 / 模式决策（对齐 Go `pipeline_decider.go` 的*意图*，按 Rust 现状裁剪）。
//!
//! 设计依据：`docs/redesign/key-pipeline.md`。本模块是 S1「模式收编为单点决策」的落点。
//!
//! ## 与 Go 决策器的差异（刻意为之）
//!
//! Go 的 `decider` 持有一组 `Processor` trait 对象，并用 `applyEngineDiff(Capability)`
//! 在切换 processor 时挂卸**共享引擎**的词典层。Rust 现状不需要这套机制：
//!
//! - **Capability 不移植**：Rust 各模式按 schema id 独立查询引擎
//!   （`EngineManager::convert_with(schema_id, ...)`），不存在被多模式改写的共享引擎，
//!   因此没有「引擎副作用」需要单点统一。强行引入 Capability 只是死抽象。
//! - **CommitStrategy 推迟到 S4**：全码/空码上屏逻辑在 Rust coordinator 里尚未实现
//!   （`codetable::engine::should_auto_commit` 为空实现），现在没有调用点消费策略归属。
//!   按「避免过早抽象」原则，等 S4 实现全码上屏时再与真实调用点一起设计。
//!
//! ## 本阶段交付：单一活跃模式 + 单点分派
//!
//! 原先三个散装 bool（`temp_pinyin_mode/quick_input_mode/temp_english_mode`）+ 三处串行
//! `if` 分派 + 分散的激活/复位入口，收敛为单一 [`ModeKind`] 字段 `State.active`：
//!
//! - 结构上保证「同一时刻至多一个独占模式」（三 bool 无法表达这一不变式）。
//! - 键事件分派变为对 `state.active` 的单次 `match`（唯一入口）。
//! - S3 新增 URL / 特殊模式时，只需加 `ModeKind` 变体 + 一条 `match` 臂，而非再加 bool + 分支。
//!
//! 决策链顺序仍由 `Coordinator::handle_key_event` 承载（对齐 key-pipeline.md §2.1）；
//! 各模式的具体按键处理仍是 `Coordinator` 上与其紧耦合的 `handle_*_key` 方法，
//! 故采用 enum 分派而非 trait 对象（避免把 ~600 行逻辑套进 `Ctx` 间接层，更「boring」）。

/// 当前激活的独占输入模式。`None` 表示普通码表/拼音输入（default processor）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModeKind {
    /// 临时拼音：码表方案下经触发键临时切到拼音反查。
    TempPinyin,
    /// 临时英文：Shift+字母触发的临时英文输入。
    TempEnglish,
    /// 网址模式：普通输入累积到某前缀（如 "www."/"http"）时夺取，原样累积 ASCII。
    Url,
    /// Unicode 码点模式：普通输入累积到某前缀（出厂 `u+`）时夺取，续打十六进制出该码点。
    ///
    /// 与 [`Self::Url`] 同属**前缀夺取式**（两者共用 `try_prefix_hijack` 那道闸门与
    /// [`Rewind`] 回退骨架），差别只在缓冲怎么解释：url 原样累积文本、不产候选，
    /// 本模式把缓冲当十六进制求值、产一条候选。
    Unicode,
    /// 特殊模式：引导键触发，自带码表 + 全码上屏策略。载荷为 `features.special_modes` 下标。
    Special(u8),
    /// 生僻字模式：引导键/热键触发，**用当前活跃方案的编码**输入，候选只留生僻字。
    ///
    /// # 为什么它与 [`Self::Special`] 共用整套按键处理
    ///
    /// special 模式的实质是「overlay 生命周期 + 一个码表引擎 + 候选」。本模式只换其中一项
    /// ——引擎从「overlay 方案自带的那张码表」换成「当前活跃方案」（`overlay_engine_schema`
    /// 的分支），再给候选加一道生僻准入。故按键处理、缓冲、光标、选词、退格全部复用
    /// `handle_special_key` 那一套，**不另写一份**：另写等于把同一条输入流实现两遍，
    /// 两份迟早分叉，而分叉的表现是「生僻字模式里退格/翻页行为和别处不一样」。
    ///
    /// ⚠️ 由此本模式会走到一切 `Special(_)` 的既有路径上。凡是从 `overlay_spec`
    /// （方案 `[overlay]` 段快照）取值的地方，本模式恒为 `None` ⇒ 落到该项的默认档，
    /// 这是**刻意**的：那些是「被叠加使用的那张码表怎么表现」，本模式没有那张码表。
    ///
    /// 无载荷：实例是单例（不像 special 那样一个引导键一份码表），身份不来自任何方案，
    /// 故也没有下标可带——同 [`Self::TempPinyin`] / [`Self::AuxCode`]。
    RareChar,
    /// 临时 mix：引导键触发，合并多个成员方案候选。载荷为 `features.mix_modes` 下标。
    Mix(u8),
    /// 辅助码：拼音候选的字形二次筛选。独占输入流（组码中无法同时打拼音），但候选
    /// 列表**只保留命中者**——被滤候选直接丢弃，候选窗只显示匹配词（还原不靠残留
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
    /// 夺取前的**来源流**缓冲快照（回放目标由 [`Self::origin`] 决定）。
    pub snapshot: String,
    /// 夺取瞬间的模式 buffer（「是否退到边界」判定：当前模式 buffer == 此值即在边界）。
    pub host_text: String,
    /// `snapshot` 该回放到哪条输入流。见 [`RewindOrigin`]。
    pub origin: RewindOrigin,
}

/// 夺取的**来源流** —— 决定 `Coordinator::rewind_hijack` 把 `snapshot` 放回哪里。
///
/// # 为什么需要它
///
/// 回退一直被写死为「放回 `input_buffer` 并重算候选」，因为此前每个夺取式模式都只有
/// 一个入口、来源恒是正常输入流。Unicode 模式打破了这一点：小写 `u+` 从码表缓冲夺取，
/// 大写 `U+` 却是从**临时英文**转交过来的（Shift+U 先被 `try_activate_mode` 截进临英）。
/// 两者若共用一条回放路径，`U+` 退回去就会把大写 `U` 塞进码表缓冲——码表拿一个它不认
/// 的字符去查词，用户看到的是空候选，而他明明只按了一次退格。
///
/// ⚠️ 新增来源时，除了本枚举还要在 `rewind_hijack` 的 `match` 里接住它。只加变体不接
/// 分支会落到既有分支上，症状与上面那条一模一样（状态清得掉、回错地方），且不报错。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RewindOrigin {
    /// 正常码表/拼音输入流（`State::input_buffer`）。URL 夺取、z 夺取、小写 `u+` 走这条。
    #[default]
    Normal,
    /// 临时英文缓冲（`State::temp_english_buffer`）。大写 `U+` 的转交走这条。
    TempEnglish,
}
