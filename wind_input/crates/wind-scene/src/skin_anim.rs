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
    /// 播放期间不接受别的事件（`state` 事件只记下，播完生效）。
    lock: bool,
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
    /// 同一事件有 `alt` 备选时随机挑一条用的状态（splitmix64；不读系统时钟，wasm 也能跑）。
    rng: u64,
}

/// 每台状态机一个不同的起始种子；真正的「随机」来自挑选时混进去的事件时刻。
fn next_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0x853c_49e6_748f_ea9b);
    SEED.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed)
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
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
            rng: next_seed(),
        }
    }

    /// 事件对应的动作下标；`alt` 展开后同一事件有多条，等概率随机挑一条（只有一条时不动随机状态）。
    fn pick(&mut self, spec: &RvDecorAnim, ev: DecorEvent, t: Duration) -> Option<usize> {
        let mut it = spec.on.iter().enumerate().filter(|(_, (e, _))| *e == ev);
        let first = it.next()?.0;
        let rest: Vec<usize> = it.map(|(i, _)| i).collect();
        if rest.is_empty() {
            return Some(first);
        }
        self.rng = splitmix(self.rng ^ (t.as_nanos() as u64));
        let n = rest.len() + 1;
        let k = (self.rng % n as u64) as usize;
        Some(if k == 0 { first } else { rest[k - 1] })
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
        match self.pick(spec, ev, t) {
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
                    if let Some(i) = self.pick(spec, DecorEvent::Idle(n), at) {
                        self.apply(spec, i, at, tl);
                    }
                }
                _ => return,
            }
        }
    }

    fn apply(&mut self, spec: &RvDecorAnim, i: usize, t: Duration, tl: &dyn Timelines) -> bool {
        // 锁定中的片段播放时：切常驻状态的事件只记下（播完才轮到它），别的一次性片段丢弃。
        if self.visible && self.play.is_some_and(|p| p.lock) {
            if let DecorAction::State(s) = &spec.on[i].1
                && valid_state(tl, *s)
            {
                self.resident = Some(*s);
            }
            return false;
        }
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
            DecorAction::Play { then, lock, .. } => {
                let (then, lock) = (*then, *lock);
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
                    lock,
                    since: t,
                });
                true
            }
        }
    }
}

#[cfg(test)]
#[path = "skin_anim_tests.rs"]
mod tests;
