//! 双击判定（纯逻辑，不触 Win32，可单元测试）。
//!
//! 候选窗没有开 `CS_DBLCLKS`，也不该开：开了之后第二下按下会变成 `WM_LBUTTONDBLCLK`
//! 而不是 `WM_LBUTTONDOWN`，候选项上的快速连点（选词后紧接着选下一个组合的词）就会被吞掉。
//! 所以双击在处理器里自己判：只喂「空白处、没拖动的单击」，两次单击的间隔与位移都落在
//! 系统双击阈值内（`GetDoubleClickTime` / `SM_CXDOUBLECLK`）即算一次双击。

use std::time::{Duration, Instant};

/// 系统双击阈值：(最大间隔, 允许位移的全宽 (x, y))。跟随用户在控制面板里的设置。
pub fn system_thresholds() -> (Duration, (i32, i32)) {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK,
        };
        unsafe {
            (
                Duration::from_millis(u64::from(GetDoubleClickTime())),
                (
                    GetSystemMetrics(SM_CXDOUBLECLK),
                    GetSystemMetrics(SM_CYDOUBLECLK),
                ),
            )
        }
    }
    #[cfg(not(windows))]
    {
        // Windows 出厂值，非 Windows 上只有测试会走到。
        (Duration::from_millis(500), (4, 4))
    }
}

/// 系统拖动阈值（`SM_CXDRAG` / `SM_CYDRAG`，像素）：按下后光标挪动不超过它算点击。
pub fn drag_threshold() -> (i32, i32) {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDRAG, SM_CYDRAG};
        unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) }
    }
    #[cfg(not(windows))]
    {
        // Windows 出厂值，非 Windows 上只有测试会走到。
        (4, 4)
    }
}

/// 最近一次单击，用于和下一次比对。
#[derive(Debug, Default)]
pub struct DoubleClick {
    last: Option<(Instant, i32, i32)>,
}

impl DoubleClick {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一次单击（屏幕坐标），返回它是否与上一次构成双击。
    ///
    /// 构成双击后清空记录：连点三下只算一次双击，第三下重新起算——否则三击会截两张图。
    /// `slop` 是两次单击允许的**全宽**位移（系统给的就是矩形宽高，故各取一半比较）。
    pub fn on_click(
        &mut self,
        now: Instant,
        x: i32,
        y: i32,
        max_interval: Duration,
        slop: (i32, i32),
    ) -> bool {
        let hit = match self.last {
            Some((t, lx, ly)) => {
                now.duration_since(t) <= max_interval
                    && (x - lx).abs() <= slop.0 / 2
                    && (y - ly).abs() <= slop.1 / 2
            }
            None => false,
        };
        self.last = if hit { None } else { Some((now, x, y)) };
        hit
    }

    /// 中间插进了别的动作（拖动、点候选、翻页）：前一次单击不再能配对。
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: Duration = Duration::from_millis(500);
    const SLOP: (i32, i32) = (4, 4);

    #[test]
    fn two_quick_clicks_in_place_make_a_double_click() {
        let mut d = DoubleClick::new();
        let t0 = Instant::now();
        assert!(!d.on_click(t0, 10, 10, WIN, SLOP), "第一下不是双击");
        assert!(d.on_click(t0 + Duration::from_millis(200), 11, 9, WIN, SLOP));
    }

    #[test]
    fn slow_second_click_is_a_new_first_click() {
        let mut d = DoubleClick::new();
        let t0 = Instant::now();
        d.on_click(t0, 10, 10, WIN, SLOP);
        assert!(!d.on_click(t0 + Duration::from_millis(600), 10, 10, WIN, SLOP));
        // 慢的那一下成为新的第一下，紧跟一下即构成双击。
        assert!(d.on_click(t0 + Duration::from_millis(700), 10, 10, WIN, SLOP));
    }

    #[test]
    fn moved_second_click_is_not_a_double_click() {
        let mut d = DoubleClick::new();
        let t0 = Instant::now();
        d.on_click(t0, 10, 10, WIN, SLOP);
        assert!(!d.on_click(t0 + Duration::from_millis(100), 13, 10, WIN, SLOP));
    }

    #[test]
    fn triple_click_fires_only_once() {
        let mut d = DoubleClick::new();
        let t0 = Instant::now();
        d.on_click(t0, 10, 10, WIN, SLOP);
        assert!(d.on_click(t0 + Duration::from_millis(100), 10, 10, WIN, SLOP));
        assert!(
            !d.on_click(t0 + Duration::from_millis(200), 10, 10, WIN, SLOP),
            "第三下不应再截一张"
        );
    }

    #[test]
    fn reset_breaks_the_pair() {
        let mut d = DoubleClick::new();
        let t0 = Instant::now();
        d.on_click(t0, 10, 10, WIN, SLOP);
        d.reset();
        assert!(!d.on_click(t0 + Duration::from_millis(100), 10, 10, WIN, SLOP));
    }
}
