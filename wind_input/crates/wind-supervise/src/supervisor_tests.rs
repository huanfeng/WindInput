use super::*;

const MODE: &str = "WIND_SUPERVISE_HELPER";

/// 以本测试二进制自身充当子进程：只有设了 `MODE` 才会动作，正常跑测试时它是空测试。
#[test]
fn helper_child() {
    match std::env::var(MODE).as_deref() {
        Ok("exit1") => std::process::exit(1),
        Ok("sleep") => std::thread::sleep(Duration::from_secs(600)),
        _ => {}
    }
}

fn sup(mode: &'static str) -> Supervisor {
    Supervisor::new(move || {
        let mut c = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        c.args(["supervisor::tests::helper_child", "--exact", "--nocapture"])
            .env(MODE, mode);
        Ok(c)
    })
    .with_backoff(Backoff::new(
        Duration::from_millis(10),
        Duration::from_secs(60),
        3,
    ))
}

/// 反复 tick 直到出事件（最多 10 秒）。
fn next_event(s: &mut Supervisor) -> Event {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(e) = s.tick(Instant::now()) {
            return e;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("10 秒内没有事件");
}

/// 等到 Started 并返回管道：测试要握着它们，子进程的 stdout 才不会因对端关闭而退出。
fn started(s: &mut Supervisor) -> (ChildStdin, ChildStdout) {
    match next_event(s) {
        Event::Started { stdin, stdout, .. } => (stdin, stdout),
        e => panic!("意外事件 {e:?}"),
    }
}

#[test]
fn idle_until_wanted_then_starts() {
    let mut s = sup("sleep");
    assert!(s.tick(Instant::now()).is_none());
    s.set_wanted(true, Instant::now());
    let _io = started(&mut s);
    assert!(s.is_running());
    s.stop(Duration::from_millis(100));
    assert!(!s.is_running());
}

#[test]
fn crash_restarts_with_backoff_then_trips() {
    let mut s = sup("exit1");
    s.set_wanted(true, Instant::now());
    let mut exits = 0;
    loop {
        match next_event(&mut s) {
            Event::Started { .. } => {}
            Event::Exited { next, .. } => {
                exits += 1;
                if next == Decision::Tripped {
                    break;
                }
                assert!(matches!(next, Decision::RestartAfter(_)));
            }
            e => panic!("意外事件 {e:?}"),
        }
    }
    assert_eq!(exits, 3);
    assert!(s.is_tripped());
    // 熔断后不再拉起。
    std::thread::sleep(Duration::from_millis(100));
    assert!(s.tick(Instant::now()).is_none());
    assert!(!s.is_running());
    // 手动干预解除熔断。
    s.retry(Instant::now());
    assert!(matches!(next_event(&mut s), Event::Started { .. }));
}

#[test]
fn set_wanted_false_cancels_pending_restart() {
    let mut s = sup("exit1");
    s.set_wanted(true, Instant::now());
    loop {
        if let Event::Exited { .. } = next_event(&mut s) {
            break;
        }
    }
    assert!(s.next_start().is_some());
    s.set_wanted(false, Instant::now());
    assert!(s.next_start().is_none());
    std::thread::sleep(Duration::from_millis(100));
    assert!(s.tick(Instant::now()).is_none());
}

#[test]
fn stop_kills_after_grace() {
    let mut s = sup("sleep");
    s.set_wanted(true, Instant::now());
    let _io = started(&mut s);
    let t = Instant::now();
    s.stop(Duration::from_millis(200));
    let el = t.elapsed();
    assert!(el >= Duration::from_millis(200), "宽限内不应强杀: {el:?}");
    assert!(el < Duration::from_secs(5), "强杀后应立即返回: {el:?}");
    assert!(!s.wanted());
}

#[test]
fn kill_is_reaped_as_exit() {
    let mut s = sup("sleep");
    s.set_wanted(true, Instant::now());
    let _io = started(&mut s);
    s.kill();
    assert!(matches!(next_event(&mut s), Event::Exited { .. }));
}

#[test]
fn factory_error_counts_as_failed_start() {
    let mut s = Supervisor::new(|| Err("没有可执行文件".into()));
    s.set_wanted(true, Instant::now());
    match next_event(&mut s) {
        Event::StartFailed { error, next } => {
            assert_eq!(error, "没有可执行文件");
            assert!(matches!(next, Decision::RestartAfter(_)));
        }
        e => panic!("意外事件 {e:?}"),
    }
}

#[test]
fn open_log_rotates_when_over_max() {
    let dir = std::env::temp_dir().join(format!("wind-supervise-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let p = dir.join("x.log");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&p, vec![b'a'; 100]).unwrap();
    drop(crate::open_log(&p, 50).unwrap());
    assert_eq!(std::fs::metadata(dir.join("x.old.log")).unwrap().len(), 100);
    assert_eq!(std::fs::metadata(&p).unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
