use super::*;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

/// 把线程局部的那份缓存清干净，让用例从确定状态起步。
fn reset() {
    paint_cache_evict_if_idle(Instant::now() + crate::image_cache::IDLE_EVICT_AFTER * 2);
}

/// 往缓存里留下点驻留并把最后取用时刻挪到过去。解码失败的源同样会占表并打点，
/// 用它就不必在本模块再搭一份图片编码的脚手架。
fn stage_stale_entry() {
    with_image_cache_for_test(|cache| {
        assert!(cache.src_size("data:image/png;base64,####").is_none());
        let stale = Instant::now()
            .checked_sub(crate::image_cache::IDLE_EVICT_AFTER + Duration::from_secs(1))
            .expect("单调时钟回拨");
        cache.set_last_use_for_test(stale);
    });
}

/// macOS 的 forwarder 线程只有一个阻塞 recv，闲置回收得挂在**等待命令**这件事上。
/// 等待期间到点就收——收不掉的话，那个平台的内存永远不落。
#[test]
fn recv_evicting_idle_reclaims_while_waiting() {
    reset();
    stage_stale_entry();
    assert!(
        paint_cache_next_deadline().is_some(),
        "布置的驻留应当要求唤醒"
    );

    let (tx, rx) = channel::<u32>();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        let _ = tx.send(7);
    });

    assert_eq!(recv_evicting_idle(&rx), Some(7));
    assert_eq!(
        paint_cache_next_deadline(),
        None,
        "等命令的这段时间里就该把缓存收掉"
    );
    sender.join().expect("发送线程");
}

/// 缓存空着时退化成普通 recv：既不忙等，也要如实报告发送端断开。
#[test]
fn recv_evicting_idle_is_plain_recv_when_cache_empty() {
    reset();
    assert_eq!(paint_cache_next_deadline(), None);

    let (tx, rx) = channel::<u32>();
    tx.send(1).expect("发送");
    assert_eq!(recv_evicting_idle(&rx), Some(1));
    drop(tx);
    assert_eq!(recv_evicting_idle(&rx), None, "发送端断开该返回 None");
}

/// 带截止时刻：没命令就按时返回 `Timeout`，不早不晚；闲置回收照旧在等待里发生。
#[test]
fn recv_until_times_out_and_still_reclaims() {
    reset();
    stage_stale_entry();
    let (tx, rx) = channel::<u32>();
    let wait = Duration::from_millis(30);
    let start = Instant::now();
    assert_eq!(
        recv_evicting_idle_until(&rx, Some(start + wait)),
        Recv::Timeout
    );
    assert!(start.elapsed() >= wait, "不能提前返回");
    assert_eq!(paint_cache_next_deadline(), None, "等待期间该把缓存收掉");

    tx.send(3).expect("发送");
    assert_eq!(
        recv_evicting_idle_until(&rx, Some(Instant::now() + wait)),
        Recv::Cmd(3)
    );
    drop(tx);
    assert_eq!(
        recv_evicting_idle_until(&rx, Some(Instant::now() + wait)),
        Recv::Disconnected
    );
}
