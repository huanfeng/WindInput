//! 工具栏自动隐藏状态机（纯逻辑，不触 Win32，可单元测试）。
//!
//! 生命周期：显示（on_shown 重置计时）→ 超时 → 1 秒线性淡出 → 隐藏。
//! 光标在工具栏内或拖动中顺延计时；淡出中光标移入取消淡出恢复不透明。
//! 未启用/无活动计时时 `is_active()` 为 false，调用方走快速路径（不取时间）。
//!
//! **悬停唤回（假隐藏，论坛 t167）**：开启后淡出完毕不真隐藏，而是以 [`GHOST_ALPHA`]
//! 留在原位——几乎看不见，但仍可被鼠标命中；光标移到那里即恢复不透明，移开后按原逻辑
//! 满 delay 再淡出。真隐藏（全屏 / 英文态 / 失焦经 `HideToolbar`）照旧走 `on_hidden`，
//! 与假隐藏无关。

use std::time::{Duration, Instant};

/// 淡出时长（固定 1 秒，不做配置）。
pub(crate) const FADE_DURATION: Duration = Duration::from_millis(1000);

/// 淡出期间的重绘间隔（约 60fps）。
///
/// 只在淡出这 1 秒内生效，是 [`AutoHide::next_deadline`] 唯一会要求高频唤醒的场景。
/// 取 60fps 而非消息循环从前的 ~125fps：alpha 只有 256 级，1 秒 60 帧的步进已在肉眼
/// 分辨力之下，再高只是白费唤醒。
const FADE_FRAME: Duration = Duration::from_millis(16);

/// 假隐藏时的整窗常数 alpha。
///
/// 不能取 0：Layered Window 的命中测试按合成后的 alpha 判透明，0 就等于真隐藏——鼠标
/// 穿透过去，悬停唤回无从谈起。取 1（≈0.4% 不透明度）肉眼不可见，又保留命中区域。
pub const GHOST_ALPHA: u8 = 1;

/// tick 推进后调用方需执行的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoHideAction {
    /// 无事可做。
    None,
    /// 淡出中：以该 alpha 重提交窗口（255→0 线性）。
    Fade(u8),
    /// 淡出被取消（光标移入）：恢复不透明（alpha=255），计时已重置。
    Restore,
    /// 淡出完成：隐藏窗口。
    Hide,
    /// 淡出完成（悬停唤回开启）：以 [`GHOST_ALPHA`] 留在原位，等光标移入。
    Ghost,
}

pub struct AutoHide {
    enabled: bool,
    delay: Duration,
    /// 到期时刻；None = 未启用或已隐藏。
    deadline: Option<Instant>,
    /// 淡出起点；Some = 淡出动画进行中。
    fade_start: Option<Instant>,
    /// 上一次 `tick_at` 时光标是否占用工具栏（在栏内或拖动中）。
    ///
    /// 用于识别「刚离开」这一沿并据此重置计时。早先消息循环每 ~8ms 跑一次 tick，
    /// 占用期间**每轮**顺延 deadline，于是「离开时刻 + delay」是自然的结果；循环改为
    /// 事件驱动后 tick 只在唤醒时发生，最后一次顺延可能远早于真正的离开时刻，隐藏会
    /// 相应提前。显式记住这一沿，才能让隐藏时刻与轮询年代一致。
    was_engaged: bool,
    /// 悬停唤回开关（`ui.toolbar.auto_hide_hover_reveal`）。
    hover_reveal: bool,
    /// 处于假隐藏（已淡出到 [`GHOST_ALPHA`]，等光标移入）。
    ghosted: bool,
}

impl AutoHide {
    pub fn new() -> Self {
        Self {
            enabled: false,
            delay: Duration::from_secs(5),
            deadline: None,
            fade_start: None,
            was_engaged: false,
            hover_reveal: false,
            ghosted: false,
        }
    }

    /// 配置悬停唤回。返回 true = 关掉时正处于假隐藏，调用方需恢复不透明并重新计时
    /// （此后到期走真隐藏）。
    pub fn configure_hover_reveal(&mut self, on: bool) -> bool {
        self.hover_reveal = on;
        if !on && self.ghosted {
            self.ghosted = false;
            return true;
        }
        false
    }

    /// 配置变更（SetToolbarAutoHide）。返回 true = 淡出被中断，调用方需恢复不透明。
    /// delay_ms 下限 1000（防误设 0 即隐；协调器侧另有秒级钳制，双保险）。
    pub fn configure(&mut self, enabled: bool, delay_ms: u64) -> bool {
        self.enabled = enabled;
        self.delay = Duration::from_millis(delay_ms.max(1000));
        if !enabled {
            let was_dimmed = self.fade_start.is_some() || self.ghosted;
            self.deadline = None;
            self.fade_start = None;
            self.was_engaged = false;
            self.ghosted = false;
            return was_dimmed;
        }
        false
    }

    /// 每次显示/重绘（render 单点）后调用：重置计时。未启用时 no-op（保持快速路径）。
    pub fn on_shown(&mut self, now: Instant) {
        if self.enabled {
            self.deadline = Some(now + self.delay);
            self.fade_start = None;
            self.ghosted = false;
        }
    }

    /// 隐藏（含失活防抖与自动隐藏自身）时调用：清空计时与淡出。
    pub fn on_hidden(&mut self) {
        self.deadline = None;
        self.fade_start = None;
        self.was_engaged = false;
        self.ghosted = false;
    }

    /// 下一次需要 `tick_at` 的时刻；`None` = 无需为本状态机安排唤醒。
    ///
    /// 消息循环据此决定睡多久（与其它计时器取最早者）。**唯一会要求高频唤醒的是淡出**：
    /// 那 1 秒内每 [`FADE_FRAME`] 推进一级 alpha。其余情形要么返回那个 5 秒级的到期时刻，
    /// 要么返回 `None`。
    ///
    /// 光标占用工具栏期间照常返回到期时刻（而非 `None`）：到点醒来发现仍被占用就再顺延
    /// 一轮，每 delay 一次的唤醒可忽略。
    ///
    /// 假隐藏期间返回 `None`：唤回它的是光标移入时的 `WM_MOUSEMOVE`，那条消息本身就会
    /// 唤醒消息循环，没有东西等着到期。反过来若在此返回 `None`、只等 `WM_MOUSELEAVE`
    /// 来唤醒，工具栏就把「自动隐藏还能不能发生」全押在那条消息必达上——窗口在光标停留
    /// 期间被隐藏或重建都会让它不再到来，代价是自动隐藏永久失效。
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        if self.fade_start.is_some() {
            return Some(now + FADE_FRAME);
        }
        self.deadline
    }

    /// 处于假隐藏：窗口还在、肉眼看不见。
    pub fn is_ghosted(&self) -> bool {
        self.ghosted
    }

    /// 是否有活动计时/淡出。false 时调用方跳过 tick 推进（不取 Instant::now()）。
    pub fn is_active(&self) -> bool {
        self.deadline.is_some() || self.fade_start.is_some() || self.ghosted
    }

    /// 推进状态机。cursor_inside=光标在工具栏窗口内；dragging=拖动中。
    pub fn tick_at(&mut self, now: Instant, cursor_inside: bool, dragging: bool) -> AutoHideAction {
        if !self.is_active() {
            return AutoHideAction::None;
        }
        let engaged = cursor_inside || dragging;
        // 「刚离开」这一沿：见 `was_engaged` 字段说明。必须在下面提前返回之前取，
        // 否则占用分支自己的 return 会把它吃掉。
        let just_left = self.was_engaged && !engaged;
        self.was_engaged = engaged;
        if self.ghosted {
            // 假隐藏：只等光标移入。移入即恢复并从此刻重新计时（移开的那一沿由上面的
            // `was_engaged` 照常处理，与正常显示时同一套）。
            if engaged {
                self.ghosted = false;
                self.deadline = Some(now + self.delay);
                return AutoHideAction::Restore;
            }
            return AutoHideAction::None;
        }
        if engaged {
            // 悬停/拖动：顺延计时；淡出中则取消恢复。
            self.deadline = Some(now + self.delay);
            if self.fade_start.take().is_some() {
                return AutoHideAction::Restore;
            }
            return AutoHideAction::None;
        }
        if just_left {
            // 光标刚移出：计时从此刻起算。轮询年代由「占用期间每轮顺延」自然达成，
            // 事件驱动下必须显式做。此时 fade_start 必为 None（上一轮占用分支已清）。
            self.deadline = Some(now + self.delay);
            return AutoHideAction::None;
        }
        if let Some(start) = self.fade_start {
            let elapsed = now.duration_since(start);
            if elapsed >= FADE_DURATION {
                self.deadline = None;
                self.fade_start = None;
                if self.hover_reveal {
                    self.ghosted = true;
                    return AutoHideAction::Ghost;
                }
                return AutoHideAction::Hide;
            }
            let t = elapsed.as_secs_f32() / FADE_DURATION.as_secs_f32();
            return AutoHideAction::Fade((255.0 * (1.0 - t)) as u8);
        }
        match self.deadline {
            Some(d) if now >= d => {
                self.fade_start = Some(now);
                AutoHideAction::Fade(255)
            }
            _ => AutoHideAction::None,
        }
    }
}

impl Default for AutoHide {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "auto_hide_tests.rs"]
mod tests;
