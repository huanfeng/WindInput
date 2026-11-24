use super::decide_eat;

/// 正常一次：闸门开着按下、松手，两半都吃掉，账也平了。
#[test]
fn paired_down_and_up_are_both_eaten() {
    let (eat_down, eaten) = decide_eat(true, true, false);
    assert!(eat_down, "闸门开时 down 必须吃");
    assert!(eaten, "吃了 down 就要记下欠一个 up");
    let (eat_up, eaten) = decide_eat(false, true, eaten);
    assert!(eat_up, "配对的 up 必须吃");
    assert!(!eaten, "销账");
}

/// 动作自己在 down 与 up 之间把闸门关掉（选词上屏 ⇒ 候选清空）。up 必须照吃，
/// 否则宿主收到孤儿 up，TSF 会把它当成「用户切了大写」通知服务端。
#[test]
fn up_is_eaten_even_after_gate_closed_mid_press() {
    let (_, eaten) = decide_eat(true, true, false);
    let (eat_up, eaten_after) = decide_eat(false, /* 闸门已关 */ false, eaten);
    assert!(eat_up, "闸门中途关掉，配对的 up 仍必须吃");
    assert!(!eaten_after);
}

/// 反方向的守卫：没吃过 down 的 up 一律放行，闸门开着也不例外（闸门置位前就按下的
/// 那次按下正是这个竞态窗口）。
#[test]
fn orphan_up_is_never_eaten() {
    assert_eq!(decide_eat(false, false, false), (false, false));
    assert_eq!(decide_eat(false, true, false), (false, false));
}

/// 闸门关且没有欠账时 down 放行：没接管过的按下绝不能碰。
#[test]
fn down_with_gate_closed_and_nothing_owed_passes_through() {
    assert_eq!(decide_eat(true, false, false), (false, false));
}

/// 按住 CapsLock 不放、闸门中途关掉，重复的 keydown 必须继续吃，否则每个重复 down
/// 都翻转一次真正的大写锁定，而最后的 up 仍被吃掉。
#[test]
fn repeated_down_is_still_eaten_after_gate_closed_mid_press() {
    let (eat, mut eaten) = decide_eat(true, true, false);
    assert!(eat);
    // 绑定动作在这里把闸门关了。用户还按着不放，系统开始重复发 down。
    for i in 0..5 {
        let (eat, next) = decide_eat(true, /* 闸门已关 */ false, eaten);
        assert!(
            eat,
            "第 {i} 次重复 down 漏给了系统——每漏一次就翻转一次真正的大写锁定"
        );
        eaten = next;
    }
    // 最后的 up 照常销账，状态自愈。
    assert_eq!(decide_eat(false, false, eaten), (true, false));
}

/// 按住不放：系统重复发 keydown，重复置位幂等，最后一次 up 一并销账。
#[test]
fn repeated_down_is_idempotent() {
    let mut eaten = false;
    for _ in 0..5 {
        let (eat, next) = decide_eat(true, true, eaten);
        assert!(eat);
        eaten = next;
    }
    assert!(eaten);
    assert_eq!(decide_eat(false, true, eaten), (true, false));
}
