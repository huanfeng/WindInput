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
        lock: false,
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

fn locked(then: Option<usize>) -> DecorAction {
    DecorAction::Play {
        clip: frames(4, 5),
        then,
        lock: true,
    }
}

#[test]
fn locked_clip_ignores_other_clips_and_defers_state() {
    let (spec, tl) = fixture(vec![
        (DecorEvent::Click, locked(None)),
        (DecorEvent::Page, play(None)),
        (DecorEvent::Typing, DecorAction::State(BUSY)),
    ]);
    let mut m = shown(&spec, &tl);
    m.fire(&spec, DecorEvent::Click, ms(0), &tl);
    assert!(
        !m.fire(&spec, DecorEvent::Page, ms(50), &tl),
        "别的片段丢弃"
    );
    assert!(
        !m.fire(&spec, DecorEvent::Click, ms(60), &tl),
        "自己再点也不重播"
    );
    m.fire(&spec, DecorEvent::Typing, ms(70), &tl);
    let p = m.seek(&spec, ms(150), &tl);
    assert_eq!(
        (p.clip, p.frame),
        (ClipId::On(0), 1),
        "仍在播 click，没被打断"
    );
    let p = m.seek(&spec, ms(210), &tl);
    assert_eq!(
        (p.clip, p.frame),
        (ClipId::State(BUSY), 0),
        "播完（200）后落到期间记下的 typing 状态"
    );
}

#[test]
fn locked_clip_then_state_wins_over_deferred_state() {
    let (spec, tl) = fixture(vec![
        (DecorEvent::Click, locked(Some(IDLE))),
        (DecorEvent::Typing, DecorAction::State(BUSY)),
    ]);
    let mut m = shown(&spec, &tl);
    m.fire(&spec, DecorEvent::Click, ms(0), &tl);
    m.fire(&spec, DecorEvent::Typing, ms(50), &tl);
    assert_eq!(m.seek(&spec, ms(250), &tl).clip, ClipId::State(IDLE));
}

#[test]
fn locked_clip_is_dropped_when_hidden() {
    let (spec, tl) = fixture(vec![(DecorEvent::Click, locked(Some(IDLE)))]);
    let mut m = shown(&spec, &tl);
    m.fire(&spec, DecorEvent::Click, ms(0), &tl);
    m.hide(&spec, ms(50), &tl);
    m.show(&spec, ms(1000), &tl);
    assert_eq!(m.seek(&spec, ms(1010), &tl).clip, ClipId::State(IDLE));
    assert!(
        m.fire(&spec, DecorEvent::Click, ms(1020), &tl),
        "显示后可再触发"
    );
}

#[test]
fn alt_actions_are_picked_at_random() {
    let (spec, tl) = fixture(vec![
        (DecorEvent::Click, play(None)),
        (DecorEvent::Click, play(None)),
        (DecorEvent::Click, play(None)),
    ]);
    let mut seen = [0usize; 3];
    let mut m = shown(&spec, &tl);
    for k in 0..300u64 {
        m.fire(&spec, DecorEvent::Click, ms(1000 * k + 7 * k * k), &tl);
        match m.seek(&spec, ms(1000 * k + 7 * k * k), &tl).clip {
            ClipId::On(i) => seen[i] += 1,
            other => panic!("该在播 click 片段：{other:?}"),
        }
    }
    assert!(
        seen.iter().all(|&n| n > 50),
        "三条都该被挑到且大致均匀：{seen:?}"
    );
}

#[test]
fn single_action_does_not_touch_random_state() {
    let (spec, tl) = fixture(vec![(DecorEvent::Click, play(None))]);
    let mut m = shown(&spec, &tl);
    let r = m.rng;
    m.fire(&spec, DecorEvent::Click, ms(10), &tl);
    assert_eq!(m.rng, r);
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
