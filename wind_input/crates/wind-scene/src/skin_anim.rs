//! 皮肤装饰的事件状态机（设计见 `docs/design/theme-skin-animation.md` §16）。
//!
//! 纯逻辑：一块装饰一台 [`Machine`]。时刻是「时间轴时刻」——调用方的动画起点到此刻的
//! 经过时长，片段各帧时长经 [`Timelines`] 查。常驻状态循环播放；一次性片段播一遍后转
//! `then`（缺省回到当前常驻状态）；新事件直接打断。窗口隐藏时只记常驻状态，显示时片段从头。

use std::time::Duration;

use wind_theme::skin::{DecorAction, DecorEvent, RvDecorAnim};

/// 静态图当片段时一帧的时长：一次性片段显示这么久后转 `then`。
pub const STILL_CLIP_MS: u32 = 1000;

/// 片段在状态机里的身份。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipId {
    /// 整个装饰文件（没写状态，或状态不可用时）。
    Base,
    /// `states` 的第几个。
    State(usize),
    /// `on` 的第几条动作里的 `play` 片段。
    On(usize),
}

/// 查片段各帧时长（毫秒，已按降帧映射；非空）。None = 片段不可用（越界、文件缺失等），
/// 引用它的动作当作没写。
pub trait Timelines {
    fn clip(&self, id: ClipId) -> Option<&[u32]>;
}

/// 什么都当作可用的单帧片段：还没求过几何（从未显示）时用，此时只会记常驻状态。
pub struct AnyClip;

impl Timelines for AnyClip {
    fn clip(&self, _: ClipId) -> Option<&[u32]> {
        Some(&[STILL_CLIP_MS])
    }
}

/// 此刻该画的帧。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pose {
    pub clip: ClipId,
    /// 片段内第几帧。
    pub frame: usize,
    /// 下次要重算的时间轴时刻（换帧、片段播完、idle 到点）；None = 静止。
    pub next: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Play {
    /// `on` 的下标。
    on: usize,
    /// None = `@prev`。
    then: Option<usize>,
    since: Duration,
}

#[derive(Clone, Debug)]
pub struct Machine {
    /// 常驻状态；None = 整个文件循环。
    resident: Option<usize>,
    /// 常驻状态的循环起点。
    since: Duration,
    play: Option<Play>,
    visible: bool,
    /// 最后一个（非 idle）事件的时刻。
    last_event: Duration,
    /// 本轮静默里已触发到的最大 idle 秒数。
    idle_done: u32,
}

fn total(d: &[u32]) -> Duration {
    Duration::from_millis(d.iter().map(|&x| x as u64).sum())
}

fn valid_state(tl: &dyn Timelines, s: usize) -> bool {
    tl.clip(ClipId::State(s)).is_some()
}

impl Machine {
    /// 初始状态；`visible` 是此刻窗口是否已显示（显示中换了变体时为 true）。
    pub fn new(spec: &RvDecorAnim, t: Duration, visible: bool) -> Self {
        Self {
            resident: spec.initial,
            since: t,
            play: None,
            visible,
            last_event: t,
            idle_done: 0,
        }
    }

    /// 不播放时（关了动画）的画面：初始状态首帧。
    pub fn still(spec: &RvDecorAnim, tl: &dyn Timelines) -> Pose {
        let clip = match spec.initial {
            Some(s) if valid_state(tl, s) => ClipId::State(s),
            _ => ClipId::Base,
        };
        Pose {
            clip,
            frame: 0,
            next: None,
        }
    }

    /// 窗口由隐藏变可见：常驻状态从头播，再触发 `show`。
    pub fn show(&mut self, spec: &RvDecorAnim, t: Duration, tl: &dyn Timelines) -> bool {
        self.visible = true;
        self.play = None;
        self.since = t;
        // 隐藏期间不计静默：先把计时拨到此刻，否则 settle 会补发隐藏期间「到点」的 idle。
        self.last_event = t;
        self.idle_done = 0;
        self.fire(spec, DecorEvent::Show, t, tl);
        true
    }

    /// 窗口隐藏：在播的片段丢弃（有 `then` 的直接转过去），再触发 `hide`。
    pub fn hide(&mut self, spec: &RvDecorAnim, t: Duration, tl: &dyn Timelines) -> bool {
        self.settle(spec, t, tl);
        if let Some(p) = self.play.take()
            && let Some(s) = p.then
        {
            self.resident = Some(s);
        }
        self.visible = false;
        self.fire(spec, DecorEvent::Hide, t, tl);
        true
    }

    /// 外部事件（`idle_*` 由本机按时刻自己触发，不经这里）。返回画面是否可能变了。
    pub fn fire(
        &mut self,
        spec: &RvDecorAnim,
        ev: DecorEvent,
        t: Duration,
        tl: &dyn Timelines,
    ) -> bool {
        self.settle(spec, t, tl);
        self.last_event = t;
        self.idle_done = 0;
        match spec.on.iter().position(|(e, _)| *e == ev) {
            Some(i) => self.apply(spec, i, t, tl),
            None => false,
        }
    }

    /// 推进到 `t`，返回该画的帧。
    pub fn seek(&mut self, spec: &RvDecorAnim, t: Duration, tl: &dyn Timelines) -> Pose {
        self.settle(spec, t, tl);
        let idle = self.next_idle(spec).map(|(_, at)| at);
        let pose = match self.play {
            Some(p) => {
                let d = tl.clip(ClipId::On(p.on)).unwrap_or(&[]);
                let (frame, until) = crate::skin::frame_at(d, t.saturating_sub(p.since));
                Pose {
                    clip: ClipId::On(p.on),
                    frame,
                    next: Some(until.map_or(p.since + total(d), |u| t + u)),
                }
            }
            None => {
                let clip = match self.resident {
                    Some(s) if valid_state(tl, s) => ClipId::State(s),
                    _ => ClipId::Base,
                };
                let d = tl.clip(clip).unwrap_or(&[]);
                let (frame, until) = crate::skin::frame_at(d, t.saturating_sub(self.since));
                Pose {
                    clip,
                    frame,
                    next: until.map(|u| t + u),
                }
            }
        };
        Pose {
            next: [pose.next, idle].into_iter().flatten().min(),
            ..pose
        }
    }

    /// 下一个待触发的 idle：(秒数, 时刻)。只在显示中计。
    fn next_idle(&self, spec: &RvDecorAnim) -> Option<(u32, Duration)> {
        if !self.visible {
            return None;
        }
        spec.on
            .iter()
            .filter_map(|(e, _)| match e {
                DecorEvent::Idle(n) if *n > self.idle_done => Some(*n),
                _ => None,
            })
            .min()
            .map(|n| (n, self.last_event + Duration::from_secs(n as u64)))
    }

    /// 按时间先后处理 `t` 之前到期的事：一次性片段播完、idle 到点。
    fn settle(&mut self, spec: &RvDecorAnim, t: Duration, tl: &dyn Timelines) {
        loop {
            let end = self
                .play
                .map(|p| p.since + tl.clip(ClipId::On(p.on)).map_or(Duration::ZERO, total));
            let idle = self.next_idle(spec);
            match (end, idle) {
                (Some(e), i) if e <= t && i.is_none_or(|(_, at)| e <= at) => {
                    let p = self.play.take().expect("end 来自 play");
                    if let Some(s) = p.then {
                        self.resident = Some(s);
                    }
                    self.since = e;
                }
                (_, Some((n, at))) if at <= t => {
                    self.idle_done = n;
                    if let Some(i) = spec.on.iter().position(|(e, _)| *e == DecorEvent::Idle(n)) {
                        self.apply(spec, i, at, tl);
                    }
                }
                _ => return,
            }
        }
    }

    fn apply(&mut self, spec: &RvDecorAnim, i: usize, t: Duration, tl: &dyn Timelines) -> bool {
        match &spec.on[i].1 {
            &DecorAction::State(s) => {
                if !valid_state(tl, s) {
                    return false;
                }
                if !self.visible {
                    let changed = self.resident != Some(s);
                    self.resident = Some(s);
                    return changed;
                }
                // 已在这个状态里循环：不从头来（连续打字不该每键重播）。
                if self.play.is_none() && self.resident == Some(s) {
                    return false;
                }
                self.play = None;
                self.resident = Some(s);
                self.since = t;
                true
            }
            DecorAction::Play { then, .. } => {
                if tl.clip(ClipId::On(i)).is_none() {
                    return false;
                }
                let then = then.filter(|&s| valid_state(tl, s));
                if !self.visible {
                    // 隐藏时片段丢弃，直接落到播完之后的状态。
                    return match then {
                        Some(s) if self.resident != Some(s) => {
                            self.resident = Some(s);
                            true
                        }
                        _ => false,
                    };
                }
                self.play = Some(Play {
                    on: i,
                    then,
                    since: t,
                });
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wind_theme::skin::DecorClip;

    /// 各片段时长表：`states[i]` / `on[i]` 为 None 即不可用。
    struct Tl {
        base: Vec<u32>,
        states: Vec<Option<Vec<u32>>>,
        on: Vec<Option<Vec<u32>>>,
    }

    impl Timelines for Tl {
        fn clip(&self, id: ClipId) -> Option<&[u32]> {
            match id {
                ClipId::Base => Some(&self.base),
                ClipId::State(i) => self.states.get(i)?.as_deref(),
                ClipId::On(i) => self.on.get(i)?.as_deref(),
            }
        }
    }

    const IDLE: usize = 0;
    const BUSY: usize = 1;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn frames(first: u32, last: u32) -> DecorClip {
        DecorClip::Frames { first, last }
    }

    /// 两个状态（idle 两帧各 100、busy 两帧各 50）与给定的动作；`play` 片段一律两帧各 100。
    fn fixture(on: Vec<(DecorEvent, DecorAction)>) -> (RvDecorAnim, Tl) {
        let tl = Tl {
            base: vec![100; 6],
            states: vec![Some(vec![100, 100]), Some(vec![50, 50])],
            on: on
                .iter()
                .map(|(_, a)| match a {
                    DecorAction::Play { .. } => Some(vec![100, 100]),
                    DecorAction::State(_) => None,
                })
                .collect(),
        };
        let spec = RvDecorAnim {
            states: vec![("idle".into(), frames(0, 1)), ("busy".into(), frames(2, 3))],
            initial: Some(IDLE),
            on,
        };
        (spec, tl)
    }

    fn play(then: Option<usize>) -> DecorAction {
        DecorAction::Play {
            clip: frames(4, 5),
            then,
        }
    }

    fn shown(spec: &RvDecorAnim, tl: &Tl) -> Machine {
        let mut m = Machine::new(spec, Duration::ZERO, false);
        m.show(spec, Duration::ZERO, tl);
        m
    }

    #[test]
    fn resident_state_loops_from_show() {
        let (spec, tl) = fixture(vec![]);
        let mut m = shown(&spec, &tl);
        let p = m.seek(&spec, ms(150), &tl);
        assert_eq!((p.clip, p.frame), (ClipId::State(IDLE), 1));
        assert_eq!(p.next, Some(ms(200)));
        let p = m.seek(&spec, ms(230), &tl);
        assert_eq!(p.frame, 0, "循环回首帧");
    }

    #[test]
    fn state_event_switches_and_repeats_do_not_restart() {
        let (spec, tl) = fixture(vec![(DecorEvent::Typing, DecorAction::State(BUSY))]);
        let mut m = shown(&spec, &tl);
        assert!(m.fire(&spec, DecorEvent::Typing, ms(1000), &tl));
        let p = m.seek(&spec, ms(1060), &tl);
        assert_eq!((p.clip, p.frame), (ClipId::State(BUSY), 1));
        assert!(
            !m.fire(&spec, DecorEvent::Typing, ms(1070), &tl),
            "已在 busy：连续打字不重播"
        );
        assert_eq!(m.seek(&spec, ms(1070), &tl).frame, 1);
    }

    #[test]
    fn play_then_state_and_prev() {
        let (spec, tl) = fixture(vec![
            (DecorEvent::Typing, DecorAction::State(BUSY)),
            (DecorEvent::Commit, play(Some(IDLE))),
            (DecorEvent::Page, play(None)),
        ]);
        let mut m = shown(&spec, &tl);
        m.fire(&spec, DecorEvent::Typing, ms(0), &tl);
        m.fire(&spec, DecorEvent::Page, ms(500), &tl);
        let p = m.seek(&spec, ms(550), &tl);
        assert_eq!((p.clip, p.frame, p.next), (ClipId::On(2), 0, Some(ms(600))));
        assert_eq!(
            m.seek(&spec, ms(650), &tl).next,
            Some(ms(700)),
            "播完的时刻"
        );
        let p = m.seek(&spec, ms(720), &tl);
        assert_eq!(
            (p.clip, p.frame),
            (ClipId::State(BUSY), 0),
            "@prev 回到常驻状态并从头播（700 起）"
        );
        m.fire(&spec, DecorEvent::Commit, ms(1000), &tl);
        let p = m.seek(&spec, ms(1250), &tl);
        assert_eq!(
            (p.clip, p.frame),
            (ClipId::State(IDLE), 0),
            "then = idle，1200 播完后 idle 从头"
        );
    }

    #[test]
    fn new_event_interrupts_playing_clip() {
        let (spec, tl) = fixture(vec![
            (DecorEvent::Typing, DecorAction::State(BUSY)),
            (DecorEvent::Commit, play(Some(IDLE))),
            (DecorEvent::Page, play(None)),
        ]);
        let mut m = shown(&spec, &tl);
        m.fire(&spec, DecorEvent::Commit, ms(0), &tl);
        m.fire(&spec, DecorEvent::Page, ms(50), &tl);
        assert_eq!(
            m.seek(&spec, ms(120), &tl).clip,
            ClipId::On(2),
            "换成新片段"
        );
        let p = m.seek(&spec, ms(260), &tl);
        assert_eq!(
            p.clip,
            ClipId::State(IDLE),
            "被打断的 commit 的 then 不再生效"
        );
        m.fire(&spec, DecorEvent::Page, ms(300), &tl);
        m.fire(&spec, DecorEvent::Typing, ms(320), &tl);
        let p = m.seek(&spec, ms(330), &tl);
        assert_eq!(
            (p.clip, p.frame),
            (ClipId::State(BUSY), 0),
            "state 事件打断片段，从头播"
        );
    }

    #[test]
    fn hidden_window_keeps_states_and_drops_clips() {
        let (spec, tl) = fixture(vec![
            (DecorEvent::Typing, DecorAction::State(BUSY)),
            (DecorEvent::Commit, play(Some(IDLE))),
            (DecorEvent::Page, play(None)),
        ]);
        let mut m = shown(&spec, &tl);
        m.fire(&spec, DecorEvent::Typing, ms(0), &tl);
        m.hide(&spec, ms(100), &tl);
        assert!(!m.fire(&spec, DecorEvent::Page, ms(200), &tl), "片段丢弃");
        assert!(
            m.fire(&spec, DecorEvent::Commit, ms(300), &tl),
            "直接落到 then"
        );
        m.fire(&spec, DecorEvent::Typing, ms(400), &tl);
        m.show(&spec, ms(5000), &tl);
        let p = m.seek(&spec, ms(5020), &tl);
        assert_eq!(
            (p.clip, p.frame),
            (ClipId::State(BUSY), 0),
            "隐藏时改的状态在显示时生效，并从头播"
        );
    }

    #[test]
    fn hide_mid_clip_lands_on_then() {
        let (spec, tl) = fixture(vec![(DecorEvent::Commit, play(Some(BUSY)))]);
        let mut m = shown(&spec, &tl);
        m.fire(&spec, DecorEvent::Commit, ms(0), &tl);
        m.hide(&spec, ms(50), &tl);
        m.show(&spec, ms(1000), &tl);
        assert_eq!(m.seek(&spec, ms(1000), &tl).clip, ClipId::State(BUSY));
    }

    #[test]
    fn show_event_plays_from_start() {
        let (spec, tl) = fixture(vec![(DecorEvent::Show, play(None))]);
        let mut m = Machine::new(&spec, Duration::ZERO, false);
        m.show(&spec, ms(3000), &tl);
        let p = m.seek(&spec, ms(3010), &tl);
        assert_eq!((p.clip, p.frame), (ClipId::On(0), 0));
    }

    #[test]
    fn idle_fires_after_quiet_period_once_per_threshold() {
        let (spec, tl) = fixture(vec![
            (DecorEvent::Typing, DecorAction::State(BUSY)),
            (DecorEvent::Idle(5), DecorAction::State(IDLE)),
            (DecorEvent::Idle(8), play(None)),
        ]);
        let mut m = shown(&spec, &tl);
        m.fire(&spec, DecorEvent::Typing, ms(1000), &tl);
        let p = m.seek(&spec, ms(1010), &tl);
        assert_eq!(p.clip, ClipId::State(BUSY));
        assert_eq!(p.next, Some(ms(1050)), "换帧早于 idle");
        assert_eq!(m.seek(&spec, ms(5990), &tl).clip, ClipId::State(BUSY));
        let p = m.seek(&spec, ms(6000), &tl);
        assert_eq!((p.clip, p.frame), (ClipId::State(IDLE), 0), "静默 5 秒");
        let p = m.seek(&spec, ms(9050), &tl);
        assert_eq!(
            (p.clip, p.frame),
            (ClipId::On(2), 0),
            "8 秒那条照常触发（idle 不重置静默计时）"
        );
        let p = m.seek(&spec, ms(30_000), &tl);
        assert_eq!(p.clip, ClipId::State(IDLE), "播完回常驻；之后不再重复触发");
        assert_eq!(p.next, Some(ms(30_100)), "只剩换帧");
        m.fire(&spec, DecorEvent::Typing, ms(31_000), &tl);
        assert_eq!(
            m.seek(&spec, ms(31_000), &tl).next,
            Some(ms(31_050)),
            "新事件开始新一轮静默"
        );
        assert_eq!(m.seek(&spec, ms(36_000), &tl).clip, ClipId::State(IDLE));
    }

    #[test]
    fn idle_does_not_run_while_hidden() {
        let (spec, tl) = fixture(vec![(DecorEvent::Idle(1), DecorAction::State(BUSY))]);
        let mut m = shown(&spec, &tl);
        m.hide(&spec, ms(10), &tl);
        m.show(&spec, ms(60_000), &tl);
        let p = m.seek(&spec, ms(60_500), &tl);
        assert_eq!(p.clip, ClipId::State(IDLE));
        assert_eq!(p.next, Some(ms(60_600)));
        assert_eq!(m.seek(&spec, ms(61_000), &tl).clip, ClipId::State(BUSY));
    }

    #[test]
    fn unusable_clips_are_ignored() {
        let (spec, mut tl) = fixture(vec![
            (DecorEvent::Typing, DecorAction::State(BUSY)),
            (DecorEvent::Commit, play(Some(BUSY))),
        ]);
        tl.states[BUSY] = None;
        tl.on[1] = None;
        let mut m = shown(&spec, &tl);
        assert!(!m.fire(&spec, DecorEvent::Typing, ms(10), &tl));
        assert!(!m.fire(&spec, DecorEvent::Commit, ms(20), &tl));
        assert_eq!(m.seek(&spec, ms(30), &tl).clip, ClipId::State(IDLE));
        tl.states[IDLE] = None;
        assert_eq!(
            m.seek(&spec, ms(30), &tl).clip,
            ClipId::Base,
            "常驻状态不可用时整个文件循环"
        );
    }

    #[test]
    fn no_states_loops_base_and_clips_return_to_it() {
        let spec = RvDecorAnim {
            on: vec![(DecorEvent::Click, play(None))],
            ..Default::default()
        };
        let tl = Tl {
            base: vec![100, 100, 100],
            states: vec![],
            on: vec![Some(vec![40])],
        };
        let mut m = shown(&spec, &tl);
        assert_eq!(m.seek(&spec, ms(250), &tl).frame, 2);
        m.fire(&spec, DecorEvent::Click, ms(300), &tl);
        let p = m.seek(&spec, ms(310), &tl);
        assert_eq!(
            (p.clip, p.next),
            (ClipId::On(0), Some(ms(340))),
            "单帧片段按时长结束"
        );
        let p = m.seek(&spec, ms(350), &tl);
        assert_eq!((p.clip, p.frame), (ClipId::Base, 0));
    }

    #[test]
    fn still_pose_is_initial_first_frame() {
        let (spec, mut tl) = fixture(vec![]);
        assert_eq!(
            Machine::still(&spec, &tl),
            Pose {
                clip: ClipId::State(IDLE),
                frame: 0,
                next: None
            }
        );
        tl.states[IDLE] = None;
        assert_eq!(Machine::still(&spec, &tl).clip, ClipId::Base);
    }
}
