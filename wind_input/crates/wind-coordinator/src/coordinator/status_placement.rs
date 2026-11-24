//! 状态气泡定位（C2-33 / GH#148）：定位方式按应用覆盖、坐标不可信时的兜底、焦点气泡的锚点超时。
//!
//! 设计见 `docs/design/per-app-overrides.md`「④ 状态气泡定位」。
//!
//! - **读取**只经 [`Coordinator::status_position`]：规则（`compat.toml` 的
//!   `status_position_mode`）优先，否则全局 `ui.status.position_mode`。方式与坐标**同层取**
//!   （同 `rule_candidate_fixed_pos`）：规则配了方式就用规则自己的 `status_x/y`，否则整套回落全局。
//!   `show_tip_at` 与 `show_focus_status_if_enabled` 两处都读它——漏一处就是「有时生效有时不生效」。
//! - **兜底**（`fallback_position`）只在 `follow_caret` 且原始坐标不可信时生效，见
//!   [`Coordinator::placement_for`]。`last` 逐字保持引入前的行为。
//! - **焦点气泡**：挂起等 TSF 坐标时，兜底为锚点才加 [`FOCUS_TIP_ANCHOR_TIMEOUT_MS`] 超时。

use super::*;
use wind_config::app_compat::{StatusAnchor, StatusFallback, StatusPositionMode};
use wind_ui_types::{StatusTipAnchor, StatusTipPlacement};

/// 焦点气泡挂起后等 TSF 权威坐标的时限（仅兜底为锚点时）。DLL 的排队档通常 1~2ms 内补来
/// 坐标；150ms 足够盖住慢宿主，又短到用户感觉不到「切过去气泡才慢半拍出现」。
pub(crate) const FOCUS_TIP_ANCHOR_TIMEOUT_MS: u64 = 150;

/// 解析后的气泡定位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusPosition {
    pub(crate) mode: StatusPositionMode,
    /// `fixed` 的坐标，与 `mode` 同层（规则的 `status_x/y` 或全局的 `custom_x/y`）。
    pub(crate) x: i32,
    pub(crate) y: i32,
    /// 坐标不可信时的兜底（规则优先，否则全局；与 `mode` 各自独立回落）。
    pub(crate) fallback: StatusFallback,
}

/// 配置层锚点 → 表现层锚点。两边各有一份枚举：wind-config 是叶子 crate，wind-ui-types 是
/// 纯数据协议 crate，互不依赖，映射只在这里。
pub(crate) fn ui_anchor(a: StatusAnchor) -> StatusTipAnchor {
    match a {
        StatusAnchor::ScreenCenter => StatusTipAnchor::ScreenCenter,
        StatusAnchor::ScreenTopLeft => StatusTipAnchor::ScreenTopLeft,
        StatusAnchor::ScreenTopRight => StatusTipAnchor::ScreenTopRight,
        StatusAnchor::ScreenBottomLeft => StatusTipAnchor::ScreenBottomLeft,
        StatusAnchor::ScreenBottomRight => StatusTipAnchor::ScreenBottomRight,
        StatusAnchor::WindowCenter => StatusTipAnchor::WindowCenter,
        StatusAnchor::WindowBottomLeft => StatusTipAnchor::WindowBottomLeft,
    }
}

impl Coordinator {
    /// 当前焦点应用的气泡定位（规则优先回落全局）。
    pub(crate) fn status_position(&self) -> StatusPosition {
        self.status_position_for(&self.active_process_name(), &self.active_focus_window())
    }

    /// 指定进程（窗口）的气泡定位。方式与坐标同层；兜底独立回落。
    pub(crate) fn status_position_for(&self, proc_name: &str, win: &FocusWindow) -> StatusPosition {
        let (rule_mode, rule_fallback) = self.with_compat_rule(proc_name, win, |r| match r {
            Some(r) => (
                r.status_position_mode.map(|m| (m, r.status_x, r.status_y)),
                r.status_fallback_position,
            ),
            None => (None, None),
        });
        let bundle = self.rt();
        let si = &bundle.config.ui.status;
        let (mode, x, y) = rule_mode.unwrap_or((si.position(), si.custom_x, si.custom_y));
        StatusPosition {
            mode,
            x,
            y,
            fallback: rule_fallback.unwrap_or_else(|| si.fallback()),
        }
    }

    /// 该进程在 compat 规则里配了气泡定位方式时返回 `(方式, x, y)`；`None` = 跟随全局。
    /// 落盘分流的判据（有规则写规则、否则写全局），与读取侧 [`Self::status_position_for`] 同源。
    pub(crate) fn rule_status_position(
        &self,
        proc_name: &str,
        win: &FocusWindow,
    ) -> Option<(StatusPositionMode, i32, i32)> {
        self.with_compat_rule(proc_name, win, |r| {
            let r = r?;
            r.status_position_mode.map(|m| (m, r.status_x, r.status_y))
        })
    }

    /// 定位方式 + 原始坐标是否可信 → 下发给 UI 的定位；`None` = 不显示（兜底 `hide`）。
    ///
    /// ★ `follow_caret` + 坐标不可信 + 兜底 `last` 必须与本功能引入前**逐字相同**：仍发
    /// `Caret`，由调用方拿 `resolve_caret_for_ui` 回退后的坐标（最近一次有效坐标）定位。
    pub(crate) fn placement_for(
        pos: StatusPosition,
        raw_valid: bool,
        offset_x: i32,
        offset_y: i32,
        above: bool,
    ) -> Option<StatusTipPlacement> {
        let caret = StatusTipPlacement::Caret {
            offset_x,
            offset_y,
            above,
        };
        match pos.mode {
            StatusPositionMode::Fixed => Some(StatusTipPlacement::Fixed { x: pos.x, y: pos.y }),
            StatusPositionMode::Anchor(a) => Some(StatusTipPlacement::Anchor(ui_anchor(a))),
            StatusPositionMode::FollowCaret if raw_valid => Some(caret),
            StatusPositionMode::FollowCaret => match pos.fallback {
                StatusFallback::Last => Some(caret),
                StatusFallback::Hide => None,
                StatusFallback::Anchor(a) => Some(StatusTipPlacement::Anchor(ui_anchor(a))),
            },
        }
    }

    /// 焦点气泡挂起：置位并推进代际；兜底为锚点时登记 [`FOCUS_TIP_ANCHOR_TIMEOUT_MS`] 超时。
    ///
    /// 作废靠两道：挂起位被清（TSF 坐标到达补显示 / 失焦 `hide_tip` / 下次焦点事件直接显示）
    /// 与代际变化（下次焦点事件重新挂起）。到期回调两道都校验，**不在同步路径上睡眠**。
    pub(crate) fn park_focus_tip(&self, fallback: StatusFallback) {
        let token = self
            .pending_focus_tip_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        self.pending_focus_tip
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if !matches!(fallback, StatusFallback::Anchor(_)) {
            return;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        super::first_show::arm_focus_tip_timer(
            std::time::Instant::now()
                + std::time::Duration::from_millis(FOCUS_TIP_ANCHOR_TIMEOUT_MS),
            token,
            weak,
        );
    }

    /// 焦点气泡锚点超时到期（共享定时器线程回调）：挂起仍在且未被新的挂起取代 ⇒ 按锚点显示。
    ///
    /// 到期时**按此刻**的定位重算：期间用户可能改了配置或菜单，兜底已不再是锚点就放弃。
    pub(crate) fn fire_focus_tip_timeout(&self, token: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        if !self.pending_focus_tip.load(Relaxed)
            || self.pending_focus_tip_gen.load(Relaxed) != token
        {
            return;
        }
        let pos = self.status_position();
        let StatusFallback::Anchor(a) = pos.fallback else {
            return;
        };
        if pos.mode != StatusPositionMode::FollowCaret {
            return;
        }
        // compare_exchange：与「TSF 坐标恰好同时到达」竞争时只让一方显示。
        if self
            .pending_focus_tip
            .compare_exchange(true, false, Relaxed, Relaxed)
            .is_err()
        {
            return;
        }
        debug!("focus_tip → 超时: {FOCUS_TIP_ANCHOR_TIMEOUT_MS}ms 内无 TSF 坐标，显示在锚点");
        self.show_tip_placed(
            &self.status_indicator_text(),
            StatusTipPlacement::Anchor(ui_anchor(a)),
        );
    }
}

#[cfg(test)]
#[path = "status_placement_tests.rs"]
mod tests;
