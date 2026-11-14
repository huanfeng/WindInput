//! 宿主无关的编辑指令。

/// 对宿主文本区的一次编辑操作。
///
/// # 为什么需要它
///
/// 核心原本的按键返回值 `KeyAction` 是**按 TSF 的编排方式**长出来的：既有宿主无关的
/// 编辑意图（提交文本、更新组合区、回退替换），也混着 TSF 专属的时序编排
/// （`HoldComposition` 要求宿主起一个超时定时器、`CommitThenDeferComposition` 要求
/// 宿主等到 keyup 才开新组合），还夹着状态通知。
///
/// 薄宿主只能理解第一类。Android 侧此前的做法是 `match` 剩下的一律压成
/// 「已消费、无输出」——于是**智能符号配对、回退替换、配对跳出在 Android 上静默失效**，
/// 不报错、不掉键，只是功能没有。`InputConnection` 完全做得到这些，是类型没把语义带过来。
///
/// `EditOp` 只保留第一类：**任何宿主都能执行的编辑意图**。TSF 编排降级为
/// [`KeyOutcome::hint`]，薄宿主直接忽略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOp {
    /// 上屏文本（追加到光标处）
    Commit(String),

    /// 设置组合区内容与光标（字节偏移，恒在字符边界）。
    /// 空文本 = 清除组合区。
    SetComposition { text: String, caret: usize },

    /// 删除光标前 `count` 个**字符**（非字节）。
    ///
    /// 计数单位是字符而不是字节或字素簇：核心的配对/替换逻辑按字符计数，
    /// 宿主换算成自己的单位（`InputConnection.deleteSurroundingTextInCodePoints`）。
    DeleteBackward { count: usize },

    /// 删除光标前 `count` 个字符后插入 `text`（智能符号替换：把「。」换成「.」）。
    ///
    /// 不拆成 `DeleteBackward` + `Commit` 两条：宿主的撤销栈会把它们记成两步，
    /// 用户按一次撤销只回退一半。
    ReplaceBackward { count: usize, text: String },

    /// 光标水平移动 `delta` 个字符（正右负左）。配对跳出用。
    MoveCursor { delta: i32 },
}

/// TSF 专属的时序编排提示。**薄宿主可以完全忽略。**
///
/// 这些不是编辑操作，是「什么时候做下一步」的约定，源于 TSF 宿主对组合区提交时机的
/// 特殊要求（详见核心 `KeyAction::HoldComposition` / `CommitThenDeferComposition`
/// 的原始注释）。[`KeyOutcome::ops`] 里已经给出了忽略编排时的**等价降级序列**，
/// 所以忽略它的宿主行为依然正确，只是少了那点时序上的讲究。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimingHint {
    /// 组合区里的内容在 `timeout_ms` 后自动提交（智能符号 hold 方案）。
    /// 忽略它 = 立即提交，用户少了一次「再按一下换成英文符号」的机会窗口。
    AutoCommitAfter { timeout_ms: u32 },

    /// 新组合区应延迟到本次按键的 keyup（或 `timeout_ms` 兜底）才建立。
    /// 忽略它 = 立即建立，在 diff 式宿主上可能被合并成一次编辑。
    DeferCompositionUntilKeyUp { timeout_ms: u32 },
}

/// 一次按键的完整结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyOutcome {
    /// 是否被输入法消费。`false` 时宿主对这一键执行默认行为；[`ops`](Self::ops) 通常为空，
    /// 唯一例外是「清组合再交还」（联想态退格、回车透传）：宿主**先按序执行 ops、再走默认行为**。
    pub consumed: bool,

    /// 按序执行的编辑指令。空 = 无文本变更（如纯模式切换）。
    pub ops: Vec<EditOp>,

    /// TSF 时序编排，薄宿主忽略即可（[`ops`](Self::ops) 已含等价降级）。
    pub hint: Option<TimingHint>,

    /// 模式是否发生变化（宿主据此刷新指示器）。
    /// 具体状态走推送通道，这里只给一个「要不要去取」的信号。
    pub mode_changed: bool,
}

impl KeyOutcome {
    /// 不消费：宿主执行默认行为。
    pub fn passthrough() -> Self {
        Self::default()
    }

    /// 消费但无文本变更（纯模式切换、无效按键等）。
    pub fn consumed_silently() -> Self {
        Self {
            consumed: true,
            ..Self::default()
        }
    }
}
