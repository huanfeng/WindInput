use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 每次成功投递都必须唤醒一次——这是本类型存在的全部理由。
#[test]
fn send_wakes_every_time() {
    let (tx, rx) = mpsc::channel();
    let woke = Arc::new(AtomicUsize::new(0));
    let w = woke.clone();
    let sender = UiSender::new(
        tx,
        Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }),
    );

    sender.send(UiCommand::HideCandidates).unwrap();
    sender.send(UiCommand::HideCandidates).unwrap();
    assert_eq!(woke.load(Ordering::SeqCst), 2, "投递了但没唤醒");
    assert_eq!(rx.try_iter().count(), 2, "唤醒了但命令没进通道");
}

/// 唤醒必须发生在投递**之后**：唤醒回调运行时，命令应当已经在通道里。
///
/// 顺序写反时 UI 线程会醒来看到空通道再睡回去，那条命令要等下一次唤醒——这条测试
/// 在唤醒回调里直接查通道，把该顺序钉死。
#[test]
fn wake_happens_after_the_command_is_queued() {
    let (tx, rx) = mpsc::channel();
    let rx = Arc::new(std::sync::Mutex::new(rx));
    let seen = Arc::new(AtomicUsize::new(0));

    let (r, s) = (rx.clone(), seen.clone());
    let sender = UiSender::new(
        tx,
        Arc::new(move || {
            // 唤醒时刻通道里应已有那条命令。
            let n = r.lock().unwrap().try_iter().count();
            s.store(n, Ordering::SeqCst);
        }),
    );

    sender.send(UiCommand::HideCandidates).unwrap();
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "唤醒时通道是空的：投递与唤醒的顺序写反了"
    );
}

/// 接收端已关闭时应报错，且不触发唤醒。
#[test]
fn closed_channel_reports_error_without_waking() {
    let (tx, rx) = mpsc::channel();
    drop(rx);
    let woke = Arc::new(AtomicUsize::new(0));
    let w = woke.clone();
    let sender = UiSender::new(
        tx,
        Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }),
    );

    assert!(sender.send(UiCommand::HideCandidates).is_err());
    assert_eq!(woke.load(Ordering::SeqCst), 0, "投递失败却仍唤醒");
}

/// `without_wake` 照常投递，只是不唤醒（macOS forwarder / 哑通道）。
#[test]
fn without_wake_still_delivers() {
    let (tx, rx) = mpsc::channel();
    let sender = UiSender::without_wake(tx);
    sender.send(UiCommand::HideCandidates).unwrap();
    assert_eq!(rx.try_iter().count(), 1);
}
