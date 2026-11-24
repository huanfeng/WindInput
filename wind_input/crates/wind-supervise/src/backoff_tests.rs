use super::*;

const S: Duration = Duration::from_secs(1);

#[test]
fn delays_double_then_trip_on_third_failure() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    b.on_start(t0);
    assert_eq!(b.on_exit(t0 + S), Decision::RestartAfter(S));
    b.on_start(t0 + 2 * S);
    assert_eq!(b.on_exit(t0 + 3 * S), Decision::RestartAfter(2 * S));
    b.on_start(t0 + 5 * S);
    assert_eq!(b.on_exit(t0 + 6 * S), Decision::Tripped);
    assert!(b.is_tripped());
    assert_eq!(b.on_exit(t0 + 600 * S), Decision::Tripped);
}

#[test]
fn stable_run_clears_history() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    b.on_start(t0);
    b.on_exit(t0 + S);
    b.on_start(t0 + 2 * S);
    b.on_exit(t0 + 3 * S);
    b.on_start(t0 + 5 * S);
    // 稳定跑了 2 分钟才崩：从头算。
    assert_eq!(b.on_exit(t0 + 125 * S), Decision::RestartAfter(S));
}

#[test]
fn failures_older_than_window_expire() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    b.on_start(t0);
    b.on_exit(t0 + S);
    b.on_start(t0 + 2 * S);
    b.on_exit(t0 + 30 * S);
    // 第一次失败已出窗口，窗口内只剩两次：不熔断。
    b.on_start(t0 + 32 * S);
    assert!(matches!(b.on_exit(t0 + 62 * S), Decision::RestartAfter(_)));
}

#[test]
fn reset_rearms() {
    let t0 = Instant::now();
    let mut b = Backoff::new(S, 60 * S, 1);
    b.on_start(t0);
    assert_eq!(b.on_exit(t0), Decision::Tripped);
    b.reset();
    b.on_start(t0 + S);
    assert_eq!(b.on_exit(t0 + 2 * S), Decision::Tripped);
}
