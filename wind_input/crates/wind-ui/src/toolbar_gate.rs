//! 工具栏可见性闸门（纯逻辑，不触 Win32，可单元测试）。
//!
//! 只回答一个问题：`UpdateToolbar` / `HideToolbar` 到达时，**何时**真正显示或隐藏。
//! 工具栏状态本身由调用方持有（见 `manager.rs` 的 `toolbar_pending_state`）。
//!
//! ## 两侧都要迟滞，且两侧的理由不同
//!
//! - **隐藏侧 50ms**：吸收应用间切换的 `FocusLost → FocusGained` 串（Alt+Tab）。
//! - **显示侧 120ms**：吸收宿主在 DocMgr 层的焦点 churn。
//!
//! 早期只有隐藏侧有迟滞，于是 show→hide 方向的抖动原样穿透：50ms 只是把消失推迟，
//! 呈现出来就是「闪一下」。实测 QQ 密码框每约 180ms 一轮「A(可编辑) 获焦 →
//! B(READONLY) 获焦」，服务端据此发出的 focus_gained/focus_lost 对间隔仅约 17ms，
//! 工具栏因此以约 5Hz 持续闪烁（2026-08-03）。DocMgr 级本就是噪声层（Excel 把同一
//! DocMgr 置空再设回、VSCode 一次切换伴随 5 次事件），不该让它的每次翻转直接驱动 UI。
//!
//! ⚠ **显示迟滞只作用于「不可见 → 可见」这个转变**。`UpdateToolbar` 同时承担「更新
//! 中英/标点/全半角」与「让工具栏出现」两件事，整条延迟会让已显示时按 Shift 切中英
//! 也慢一档，手感明显退化。故 `on_update` 要求调用方传入当前可见性。

use std::time::{Duration, Instant};

/// 隐藏迟滞：`HideToolbar` 后延后这么久才真正隐藏，期间收到 `UpdateToolbar` 即取消。
pub const HIDE_DEBOUNCE: Duration = Duration::from_millis(50);
/// 显示迟滞：工具栏从不可见变为可见前的静默期，期间收到 `HideToolbar` 即撤销。
///
/// 取值依据：需明显大于 DocMgr churn 的 gained→lost 间隔（实测约 17ms），又要小到
/// 用户察觉不到工具栏「晚出现」。工具栏是常驻 UI 而非跟随光标的候选窗，120ms 无感。
pub const SHOW_DEBOUNCE: Duration = Duration::from_millis(120);

/// `UpdateToolbar` 到达后调用方需执行的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// 立即渲染（工具栏已可见，内容更新不受迟滞影响）。
    RenderNow,
    /// 已排入显示迟滞窗口；调用方只需暂存状态，等 `tick_at` 返回 `Show`。
    Deferred,
}

/// `HideToolbar` 到达后调用方需执行的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideAction {
    /// 撤销了待显示：工具栏本就不可见，无需隐藏；调用方须丢弃暂存状态。
    CancelledPending,
    /// 已排入隐藏迟滞窗口，等 `tick_at` 返回 `Hide`。
    Scheduled,
}

/// `tick_at` 推进后调用方需执行的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateTick {
    None,
    /// 显示迟滞到期：用暂存状态渲染（`Toolbar::update` 内部会 show）。
    Show,
    /// 隐藏迟滞到期：隐藏窗口。
    Hide,
}

/// 工具栏显隐迟滞闸门。
///
/// 不变式：`show_at` 与 `hide_at` **不会同时为 `Some`**——待显示意味着当前不可见
/// （无可隐藏之物），待隐藏意味着当前可见（`on_update` 会先清掉待隐藏）。
pub struct ToolbarGate {
    show_at: Option<Instant>,
    hide_at: Option<Instant>,
}

impl ToolbarGate {
    pub fn new() -> Self {
        Self {
            show_at: None,
            hide_at: None,
        }
    }

    /// `UpdateToolbar` 到达。`visible` = 工具栏当前是否可见。
    pub fn on_update(&mut self, now: Instant, visible: bool) -> UpdateAction {
        // 取消待定隐藏（切回本输入法 → 保持显示）。
        self.hide_at = None;
        if visible {
            // 已可见：内容立即生效，且不应有待显示项。
            self.show_at = None;
            return UpdateAction::RenderNow;
        }
        // 不可见 → 待显示。**从第一条 UpdateToolbar 起算**：若每条都重置计时，
        // 连发多条（启动、主题下发、状态同步）会把窗口一再推后，工具栏迟迟不出现。
        self.show_at.get_or_insert(now + SHOW_DEBOUNCE);
        UpdateAction::Deferred
    }

    /// `HideToolbar` 到达。
    pub fn on_hide(&mut self, now: Instant) -> HideAction {
        if self.show_at.take().is_some() {
            // 撤销待显示——消除 DocMgr churn 闪烁的关键一步。
            return HideAction::CancelledPending;
        }
        self.hide_at = Some(now + HIDE_DEBOUNCE);
        HideAction::Scheduled
    }

    /// 是否有待定的显示/隐藏。false 时调用方可跳过 `tick_at`（不取 `Instant::now()`）。
    pub fn is_active(&self) -> bool {
        self.show_at.is_some() || self.hide_at.is_some()
    }

    /// 下一次需要 [`Self::tick_at`] 的时刻；`None` = 闸门空闲，无需为它安排唤醒。
    ///
    /// 消息循环据此决定睡多久。两个迟滞窗口本来就只有几十到一百多毫秒，睡过头就是
    /// 「工具栏该显示时没显示」。
    pub fn deadline(&self) -> Option<Instant> {
        // 类型文档的不变式保证两者不同时为 `Some`。仍取较早者而非任选其一：这样即便将来
        // 不变式被放松，最坏结果也只是多醒一次，而不是漏掉一次迟滞到期。
        match (self.show_at, self.hide_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// 推进闸门。到期则清空对应计时并返回动作。
    pub fn tick_at(&mut self, now: Instant) -> GateTick {
        if let Some(d) = self.show_at
            && now >= d
        {
            self.show_at = None;
            return GateTick::Show;
        }
        if let Some(d) = self.hide_at
            && now >= d
        {
            self.hide_at = None;
            return GateTick::Hide;
        }
        GateTick::None
    }
}

impl Default for ToolbarGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "toolbar_gate_tests.rs"]
mod tests;
