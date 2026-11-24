use super::*;
use std::time::{Duration, Instant};

fn idle_timer() -> OneShotTimer {
    OneShotTimer {
        pending: Mutex::new(Vec::new()),
        cv: std::sync::Condvar::new(),
        fire: |_, _| {},
    }
}

/// 覆盖式登记：待办任何时刻只有一个，且是最近一次 arm 的那个。
#[test]
fn arm_replaces_previous_pending() {
    let t = idle_timer();
    let dead = std::sync::Weak::<Coordinator>::new();
    let base = Instant::now();

    t.arm(base + Duration::from_secs(10), 1, dead.clone());
    t.arm(base + Duration::from_secs(20), 2, dead.clone());
    t.arm(base + Duration::from_secs(30), 3, dead);

    let g = t.pending.lock().unwrap();
    assert_eq!(g.len(), 1, "同一协调器只应有一条待办");
    let (deadline, token, _) = &g[0];
    assert_eq!(*token, 3, "只应保留最近一次 arm 的 token");
    assert_eq!(
        *deadline,
        base + Duration::from_secs(30),
        "到期时刻也应随最近一次 arm 更新"
    );
}

/// 按协调器分槽：别的协调器的 arm 不得顶掉这一个的待办（并行测试里就是这么丢超时的）。
#[test]
fn arm_keeps_other_coordinators_pending() {
    let t = idle_timer();
    let a = Coordinator::new_headless(Config::default(), None);
    let b = Coordinator::new_headless(Config::default(), None);
    let base = Instant::now();
    t.arm(base + Duration::from_secs(10), 1, Arc::downgrade(&a));
    t.arm(base + Duration::from_secs(20), 2, Arc::downgrade(&b));
    t.arm(base + Duration::from_secs(30), 3, Arc::downgrade(&a));
    let g = t.pending.lock().unwrap();
    let mut tokens: Vec<u64> = g.iter().map(|(_, tok, _)| *tok).collect();
    tokens.sort();
    assert_eq!(tokens, vec![2, 3], "a 的旧待办被 a 自己顶掉，b 的保留");
}

/// 线程真的会在到期后回调；且协调器已释放时安全跳过（不 panic）。
#[test]
fn fires_after_deadline_and_tolerates_dead_coordinator() {
    let t = Arc::new(idle_timer());
    let worker = t.clone();
    std::thread::spawn(move || worker.run());

    t.arm(
        Instant::now() + Duration::from_millis(30),
        7,
        std::sync::Weak::<Coordinator>::new(), // upgrade 必失败，走"协调器已没了"分支
    );

    // 到期后待办应被取走（说明线程确实醒来处理了），且不 panic
    std::thread::sleep(Duration::from_millis(200));
    assert!(t.pending.lock().unwrap().is_empty(), "到期后待办应已被消费");
}
