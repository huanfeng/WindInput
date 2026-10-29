//! 状态提示气泡的定位方式（C2-33 / GH#148）。几何换算留在渲染端。

/// 气泡锚点：`Screen*` = 前台窗口所在显示器的**工作区**，`Window*` = 前台窗口的可见边框。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTipAnchor {
    ScreenCenter,
    ScreenTopLeft,
    ScreenTopRight,
    ScreenBottomLeft,
    ScreenBottomRight,
    WindowCenter,
    WindowBottomLeft,
}

impl StatusTipAnchor {
    /// 是否以前台**窗口**为参照（否则以前台窗口所在屏幕的工作区为参照）。
    pub fn is_window(self) -> bool {
        matches!(self, Self::WindowCenter | Self::WindowBottomLeft)
    }

    /// 拿不到宿主窗口边框时的降级：窗口锚点换成同位置的屏幕锚点，屏幕锚点原样返回。
    pub fn screen_equivalent(self) -> Self {
        match self {
            Self::WindowCenter => Self::ScreenCenter,
            Self::WindowBottomLeft => Self::ScreenBottomLeft,
            other => other,
        }
    }
}

/// 气泡怎么定位。与 `ShowStatusTip` 顶层的光标坐标配合使用：
///
/// - `Caret`：光标下方（`above` 时光标上方），放不下翻到另一侧，挡住候选窗时避让；叠加用户偏移；
/// - `Fixed`：内容左上屏幕坐标；`(0,0)` = 尚未摆过，由渲染端落到**光标所在屏**（故仍需光标）；
/// - `Anchor`：锚点，不读光标（渲染端取不到前台窗口时才拿光标选屏）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTipPlacement {
    Caret {
        offset_x: i32,
        offset_y: i32,
        above: bool,
    },
    Fixed {
        x: i32,
        y: i32,
    },
    Anchor(StatusTipAnchor),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_anchors_degrade_to_matching_screen_anchor() {
        use StatusTipAnchor as A;
        assert_eq!(A::WindowCenter.screen_equivalent(), A::ScreenCenter);
        assert_eq!(A::WindowBottomLeft.screen_equivalent(), A::ScreenBottomLeft);
        assert_eq!(A::ScreenTopRight.screen_equivalent(), A::ScreenTopRight);
        assert!(A::WindowCenter.is_window() && !A::ScreenCenter.is_window());
    }
}
