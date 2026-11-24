use super::{WakeChannel, confirmed, watch_enabled};

/// 测试用的兜底时限：远小于生产的 `PARK_FALLBACK`，免得一条测试跑满十秒。
const T: std::time::Duration = std::time::Duration::from_millis(400);

/// ★ 唤醒通道的第一条性质：`wake()` 之后 `park` 必须**立刻**返回。
///
/// 这条断的是「工具栏亮起来了、线程却还在睡」。没有它的话，把 `wake()` 写成空函数、
/// 或把 `notify_toolbar` 那处调用删掉，都只表现为「偶尔慢十秒」，测试全绿。
#[test]
fn a_wake_makes_park_return_immediately() {
    let ch = WakeChannel::new();
    ch.wake();
    let t0 = std::time::Instant::now();
    ch.park(T);
    assert!(
        t0.elapsed() < T / 2,
        "已经叫过了就不该再睡，实测睡了 {:?}",
        t0.elapsed()
    );
}

/// ★★ 第二条：**挂起之前**到达的唤醒不得丢失。
///
/// 线程读闸与拿 `WAKE` 锁之间有一个窗口，信号可能落在那里。若把标志的消费放在进门处
/// （`*woken = false` 写在 `wait` 之前），这个信号会被抹掉、线程照睡到兜底 —— 而那个
/// 窗口恰好是「用户刚点进输入框」。这里模拟的就是那一格：先 `wake()`（模拟窗口内到达），
/// 再 `park()`。
#[test]
fn a_wake_arriving_before_park_is_not_lost() {
    let ch = WakeChannel::new();
    ch.wake();
    ch.wake(); // 重复叫不叠加，仍是一次性信号
    let t0 = std::time::Instant::now();
    ch.park(T);
    assert!(t0.elapsed() < T / 2, "挂起前到达的信号被吞了");

    // 而且消费掉之后不得有残留：这一次 park 必须真的等下去。
    let t1 = std::time::Instant::now();
    std::thread::scope(|sc| {
        sc.spawn(|| {
            std::thread::sleep(T / 4);
            ch.wake();
        });
        ch.park(T);
    });
    assert!(
        t1.elapsed() >= T / 5,
        "上一次的信号有残留，park 没有真的等，实测只等了 {:?}",
        t1.elapsed()
    );
    assert!(t1.elapsed() < T, "不该等到兜底超时 —— 叫醒信号没送到");
}

/// ★ 开关判据必须**两项合取**：父项「全屏时隐藏工具栏」关着时也不该开工。
///
/// 只看 `fullscreen_watch` 的实现会让「父项关掉」这一格照跑线程，而那时
/// `fullscreen_cached` 根本没有生产读者 —— 纯无效功，且设置页把子项置灰了，用户关不掉。
#[test]
fn watching_requires_both_the_parent_and_its_own_switch() {
    let both_on = |hide, watch| wind_config::config::ToolbarConfig {
        hide_in_fullscreen: hide,
        fullscreen_watch: watch,
        ..Default::default()
    };
    let mut t = both_on(true, true);
    assert!(watch_enabled(&t), "两项都开 ⇒ 开工");
    t.fullscreen_watch = false;
    assert!(!watch_enabled(&t), "自己关掉 ⇒ 不开工");
    t.fullscreen_watch = true;
    t.hide_in_fullscreen = false;
    assert!(
        !watch_enabled(&t),
        "父项关掉 ⇒ 同样不开工（设置页正是按这条置灰的）"
    );
    t.fullscreen_watch = false;
    assert!(!watch_enabled(&t), "都关 ⇒ 不开工");
}

/// 两拍确认的三条性质：首次采样不认、连续同值才认、中途换值要重新攒。
///
/// 第三条是真正要钉的——只写「连续两次相同」而漏掉「换值时重新计数」的实现，在
/// 「全屏 / 非全屏 / 全屏 / 非全屏」这种交替抖动下会每隔一拍就认一次，去抖等于没做。
#[test]
fn a_flip_needs_two_identical_samples_in_a_row() {
    let mut last = None;
    assert!(!confirmed(&mut last, true), "首次采到的值没有对照，不能认");
    assert!(confirmed(&mut last, true), "连着两拍同值才认");
    assert!(
        !confirmed(&mut last, false),
        "换了值要重新攒两拍，单拍抖动不得通过"
    );
    assert!(
        !confirmed(&mut last, true),
        "又换回去同样不算——交替抖动一拍都不该认"
    );
    assert!(confirmed(&mut last, true), "稳定下来之后照常认");
}

/// 挂起期间历史作废（线程里 `last = None`）之后，重新开工的第一拍同样不认——
/// 否则「挂起前最后一拍」会与「重新开工第一拍」凑成两拍，而这两拍之间隔着整段
/// 挂起时间，前台早就换了。
#[test]
fn history_is_discarded_across_a_park() {
    let mut last = Some(true);
    assert!(confirmed(&mut last, true), "挂起前：两拍已凑齐");
    last = None; // 线程挂起时做的事
    assert!(!confirmed(&mut last, true), "跨挂起不得与挂起前那拍凑对");
    assert!(confirmed(&mut last, true));
}
