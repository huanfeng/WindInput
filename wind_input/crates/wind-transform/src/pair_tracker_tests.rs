use super::*;
use std::time::Duration;

fn tracker_with_pair(now: Instant) -> PairTracker {
    let mut t = PairTracker::new();
    t.set_owner_token(7);
    t.push_at('（', '）', now);
    t
}

#[test]
fn push_records_owner_and_activity() {
    let now = Instant::now();
    let t = tracker_with_pair(now);
    assert_eq!(t.owner_token(), 7);
    assert!(!t.is_empty());
    assert!(!t.is_stale_at(now, 120), "刚压栈不应陈旧");
}

#[test]
fn stale_after_ttl_elapsed() {
    let now = Instant::now();
    let t = tracker_with_pair(now);
    assert!(
        !t.is_stale_at(now + Duration::from_secs(119), 120),
        "未到 TTL 不应陈旧"
    );
    assert!(
        t.is_stale_at(now + Duration::from_secs(120), 120),
        "到达 TTL 即陈旧"
    );
}

/// 时效从**最后一次活动**算起而非压栈时刻：在括号里持续输入不应误过期。
#[test]
fn touch_refreshes_ttl() {
    let now = Instant::now();
    let mut t = tracker_with_pair(now);
    t.touch_at(now + Duration::from_secs(119));
    assert!(
        !t.is_stale_at(now + Duration::from_secs(200), 120),
        "刷新后应从新时刻起算，不该陈旧"
    );
}

/// 栈空时 touch 不留痕：没有状态需要保活，否则空栈也会攒出一个活动时间。
#[test]
fn touch_on_empty_stack_is_noop() {
    let now = Instant::now();
    let mut t = PairTracker::new();
    t.touch_at(now);
    assert!(!t.is_stale_at(now + Duration::from_secs(999), 1));
}

#[test]
fn ttl_zero_never_expires() {
    let now = Instant::now();
    let t = tracker_with_pair(now);
    assert!(!t.is_stale_at(now + Duration::from_secs(86400), 0));
}

/// 多字符配对：左右两段与跳出格数都要原样存住。
/// 单字符路径的 `push` 必须仍然等价于 `jump_steps = 1`，否则标点配对会跟着改行为。
#[test]
fn text_pair_keeps_segments_and_steps() {
    let now = Instant::now();
    let mut t = PairTracker::new();
    t.push_text_at("<!--".into(), "-->".into(), 3, now);
    let e = t.peek().expect("应有一层");
    assert_eq!(e.left, "<!--");
    assert_eq!(e.right, "-->");
    assert_eq!(e.jump_steps, 3);

    t.push_at('（', '）', now);
    let e = t.peek().expect("应有第二层");
    assert_eq!(e.jump_steps, 1, "单字符配对跳出恒 1 格");
}

/// 右符号跳出只认单字符右段——多字符右段配不上一次标点按键，
/// 若这里返回 true，打一个 `-` 就会误跳出整个 `-->`。
#[test]
fn right_is_char_rejects_multichar_right() {
    let mut t = PairTracker::new();
    t.push_text_at("<!--".into(), "-->".into(), 3, Instant::now());
    let e = t.peek().unwrap();
    assert!(!e.right_is_char('-'), "多字符右段不该被单字符命中");
    assert!(!e.right_is_char('>'));

    t.push('（', '）');
    assert!(t.peek().unwrap().right_is_char('）'));
    assert!(
        !t.peek().unwrap().right_is_char(')'),
        "半角右括号不该命中全角栈顶"
    );
}

/// 栈是 LIFO：分级跳出（两条 `ime.pair` 写在同一词条里）靠的就是它，
/// 内层后压先弹。这条锁住顺序，避免有人改成队列。
#[test]
fn nested_text_pairs_pop_inner_first() {
    let mut t = PairTracker::new();
    t.push_text("（".into(), "）".into(), 1);
    t.push_text("【".into(), "】".into(), 1);
    assert_eq!(t.pop().unwrap().right, "】", "内层先弹");
    assert_eq!(t.pop().unwrap().right, "）", "外层后弹");
    assert!(t.is_empty());
}

#[test]
fn clear_resets_owner_and_activity() {
    let now = Instant::now();
    let mut t = tracker_with_pair(now);
    t.clear();
    assert_eq!(t.owner_token(), 0, "清空须一并复位归属，避免下次压栈继承");
    assert!(t.is_empty());
    assert!(!t.is_stale_at(now + Duration::from_secs(999), 1));
}
