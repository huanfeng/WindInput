use super::*;
use std::time::{Duration, Instant};

/// 先投递后等待也必须立刻醒——错过的唤醒不能丢（见模块文档）。
#[test]
fn wake_before_wait_is_not_lost() {
    let (waker, port) = channel();
    waker.wake();
    let t0 = Instant::now();
    port.wait(Some(Duration::from_secs(5)));
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "已置位的唤醒被丢弃，wait 睡满了超时"
    );
}

/// 无唤醒时应睡满超时。这条防的是「事件驱动退化成忙循环」——那正是本次改动要消灭的。
#[test]
fn wait_without_wake_blocks_until_timeout() {
    let (_waker, port) = channel();
    let t0 = Instant::now();
    port.wait(Some(Duration::from_millis(120)));
    assert!(
        t0.elapsed() >= Duration::from_millis(80),
        "无唤醒却提前返回（实际 {:?}），事件驱动会退化成忙循环",
        t0.elapsed()
    );
}

/// 唤醒是一次性的：消费后再等应重新阻塞。
#[test]
fn wake_is_consumed_once() {
    let (waker, port) = channel();
    waker.wake();
    port.wait(Some(Duration::from_secs(5)));
    let t0 = Instant::now();
    port.wait(Some(Duration::from_millis(120)));
    assert!(
        t0.elapsed() >= Duration::from_millis(80),
        "同一次唤醒被消费了两次（实际 {:?}）",
        t0.elapsed()
    );
}

/// 跨线程唤醒（真实用法：协调器线程 → UI 线程）。
#[test]
fn wake_from_another_thread() {
    let (waker, port) = channel();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        waker.wake();
    });
    let t0 = Instant::now();
    port.wait(Some(Duration::from_secs(5)));
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "跨线程唤醒没生效，wait 睡满了超时"
    );
}
