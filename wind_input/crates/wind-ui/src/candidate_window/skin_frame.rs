//! 候选窗异形皮肤的窗口侧接线：动画时钟、装饰事件（打字 / 上屏 / 翻页 / 中英）与动画开关。
//! 几何求值、布局外扩、绘制、对齐点换算在 [`wind_scene::candidate`]。
//! 设计见 `docs/design/theme-skin-animation.md` §3–§5。

use super::CandidateWindow;
use super::scene::AnimClock;
use wind_theme::skin::DecorEvent;

impl CandidateWindow {
    /// 刷新本帧皮肤几何与装饰动画（见 [`CandidateScene::refresh_skin`]）：由隐藏变可见时
    /// 重读动画开关、记动画起点。
    #[cfg(test)]
    pub(super) fn refresh_skin_geom(&mut self) {
        self.refresh_skin_geom_at(std::time::Instant::now());
    }

    /// 同 [`Self::refresh_skin_geom`]，时刻由调用方给（金样注入时间用）。
    #[cfg(test)]
    pub(super) fn refresh_skin_geom_at(&mut self, now: std::time::Instant) {
        let clock = self.begin_anim_frame(now);
        self.with_scene(|scene, inp| scene.refresh_skin(inp, clock));
    }

    /// 一帧开头的窗口侧动画簿记，返回交给场景的时钟：由隐藏变可见时重读动画开关、记动画起点。
    pub(super) fn begin_anim_frame(&mut self, now: std::time::Instant) -> AnimClock {
        if !self.visible {
            self.refresh_anim_allowed();
            self.anim_start = now;
        }
        // 这一帧就是按此刻重画的，之前登记的「待重绘」作废。
        self.anim_due = false;
        self.anim_clock(now)
    }

    /// 此刻的动画时钟：时间轴起点、可见性与动画开关都在窗口侧。
    fn anim_clock(&self, now: std::time::Instant) -> AnimClock {
        AnimClock {
            t: now.saturating_duration_since(self.anim_start),
            showing: !self.visible,
            allowed: self.anim_allowed,
        }
    }

    /// 重读「播不播动画」：`ui.theme.animation` 且系统没关动画效果。变了就让状态机重来。
    pub(super) fn refresh_anim_allowed(&mut self) {
        let system = (self.motion_allowed)();
        let allowed = self.theme_animation && system;
        // 冻结原因变了记一条：远程桌面等场景系统关了动画效果，看不到动画不是 bug。
        let why = (self.theme_animation, system);
        if self.theme.skin.is_some() && self.anim_reported != Some(why) {
            self.anim_reported = Some(why);
            tracing::info!(
                "skin: 主题动画{}（ui.theme.animation={}，系统动画效果={}）",
                if allowed { "播放" } else { "冻结" },
                why.0,
                if why.1 { "开" } else { "关" }
            );
        }
        if allowed != self.anim_allowed {
            self.anim_allowed = allowed;
            self.scene.decor_anims_key = None;
        }
    }

    /// 喂一个装饰事件；`only` 只给那一块装饰（`click`）。不播动画时不切换。
    /// 显示中画面可能变了就登记一次换帧重绘。
    pub(super) fn skin_event(&mut self, ev: DecorEvent, only: Option<usize>) {
        self.skin_event_at(ev, only, std::time::Instant::now());
    }

    /// 外部宿主报来的装饰点击（扩展信封 `skin.decor_click`）：给这块装饰触发 `click`。候选窗不在屏上
    /// （点击晚于收起到达）时丢掉；当前这一帧没有这块可点的装饰（下标越界、`pass`、宿主还按换皮肤
    /// 之前的那一帧报的）也丢掉——宿主的话只是线索，核对过才算数。
    pub fn skin_decor_click(&mut self, index: usize) {
        let known = self.scene.skin_geom.as_ref().is_some_and(|g| {
            g.host_decor()
                .iter()
                .any(|d| d.index == index && d.hit != wind_theme::skin::DecorHit::Pass)
        });
        if !known {
            tracing::debug!("skin.decor_click index={index} 不是当前帧可点的装饰，丢弃");
            return;
        }
        if self.visible {
            self.skin_event(DecorEvent::Click, Some(index));
        }
    }

    /// 同 [`Self::skin_event`]，时刻由调用方给（金样注入时间用）。
    pub(super) fn skin_event_at(
        &mut self,
        ev: DecorEvent,
        only: Option<usize>,
        now: std::time::Instant,
    ) {
        let clock = self.anim_clock(now);
        let changed = self.with_scene(|scene, inp| scene.decor_event(inp, ev, only, clock));
        if changed && self.visible {
            self.anim_due = true;
        }
    }

    /// 候选更新：比较相邻两次的编码串与页码，喂 `typing` / `backspace` / `page` / `empty`
    /// （`preedit` 取协调器下发的原串，编码由宿主自绘时也照样比）。
    ///
    /// 只有 `composing`（打字中）的帧才算编码：上屏后进联想时编码串变成空串或联想标识，
    /// 那不是退格也不是打字。不在打字中的帧之后再打字，按从空串起打算 `typing`。
    pub fn note_skin_input(
        &mut self,
        preedit: &str,
        composing: bool,
        page: usize,
        no_candidates: bool,
    ) {
        if self.theme.skin.is_none() {
            return;
        }
        let code = if composing { preedit } else { "" };
        let empty = composing && no_candidates;
        let prev = self
            .skin_input_prev
            .replace((code.to_string(), page, empty));
        let (old, old_page, old_empty) = match &prev {
            Some((p, n, e)) => (p.as_str(), Some(*n), *e),
            None => ("", None, false),
        };
        if !composing {
            // 联想 / 上屏后的帧：不报编码事件，只记下来作下一帧的基线。
        } else if code != old {
            let ev = if code.len() < old.len() && old.starts_with(code) {
                DecorEvent::Backspace
            } else {
                DecorEvent::Typing
            };
            self.skin_event(ev, None);
        } else if old_page.is_some_and(|n| n != page) {
            self.skin_event(DecorEvent::Page, None);
        }
        if empty && !old_empty {
            self.skin_event(DecorEvent::Empty, None);
        }
    }

    /// 工具栏状态：中英变了喂 `mode_cn` / `mode_en`（第一次只记下，不算切换）。
    pub fn note_skin_mode(&mut self, chinese: bool) {
        if self.theme.skin.is_none() {
            return;
        }
        if self
            .skin_mode_prev
            .replace(chinese)
            .is_some_and(|c| c != chinese)
        {
            let ev = if chinese {
                DecorEvent::ModeCn
            } else {
                DecorEvent::ModeEn
            };
            self.skin_event(ev, None);
        }
    }

    /// 协调器发来的装饰事件。
    pub fn skin_signal(&mut self, sig: wind_ui_types::SkinSignal) {
        match sig {
            wind_ui_types::SkinSignal::Commit => self.skin_event(DecorEvent::Commit, None),
        }
    }

    /// `ui.theme.animation`。
    pub fn set_theme_animation(&mut self, on: bool) {
        if self.theme_animation != on {
            self.theme_animation = on;
            self.refresh_anim_allowed();
            // 显示中就地换成播放 / 首帧。
            self.anim_due = self.visible;
        }
    }
}

#[cfg(test)]
#[path = "skin_frame_tests.rs"]
mod tests;
