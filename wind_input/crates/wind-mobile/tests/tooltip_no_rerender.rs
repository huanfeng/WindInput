//! 长按取提示（`candidate_tooltip`）不得触发候选帧重渲染。
//!
//! 宿主在收到任意候选帧时会收起长按提示卡（候选一变，卡上的下标就指向别的词了）。
//! 若反查索引、按词查编码的用户层要等到长按取提示时才开始后台构建，建好后的补刷帧会把
//! 刚弹出的卡片收掉，用户的下一次点击就落到键盘上。所以它们必须在首个候选帧时就触发构建
//! （与桌面路径一致），
//! 让补刷帧落在「按键 → 收敛」这段里，而不是长按之后。
//!
//! 只放这一个测试：`MobileCore::new` 会写进程环境变量，同进程并行跑多个会互相干扰。

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use wind_keys::keymap::VK_ESCAPE;
use wind_mobile::{CandidateFrame, InputStatus, MobileCore, MobileEventSink};

#[derive(Default)]
struct Counter {
    frames: AtomicUsize,
}

impl MobileEventSink for Counter {
    fn on_candidates(&self, _: CandidateFrame) {
        self.frames.fetch_add(1, Ordering::SeqCst);
    }
    fn on_hide_candidates(&self) {}
    fn on_status(&self, _: InputStatus) {}
    fn on_toast(&self, _: String) {}
}

/// 连续 `quiet` 内没有新候选帧即返回；总超时 20s 视为失败
fn settle(c: &Counter, quiet: Duration) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = c.frames.load(Ordering::SeqCst);
    let mut since = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(5));
        let n = c.frames.load(Ordering::SeqCst);
        if n != last {
            last = n;
            since = Instant::now();
        } else if since.elapsed() >= quiet {
            return;
        }
        assert!(Instant::now() < deadline, "一直有新候选帧，无法收敛");
    }
}

#[test]
fn tooltip_does_not_trigger_rerender() {
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !(data_dir.join("config.toml").exists()
        && data_dir.join("schemas/wubi86.schema.toml").exists())
    {
        let msg = format!("缺少出厂数据 {}", data_dir.display());
        if std::env::var_os("CI").is_some() {
            eprintln!("跳过：{msg}（CI 环境）");
            return;
        }
        panic!("{msg}，本地不允许跳过（设 CI 环境变量可跳过）");
    }
    // 全新用户目录 + 冷启动：不预热索引，复现手机上「装完第一次用」的时序
    let user_root =
        std::env::temp_dir().join(format!("wind-mobile-tooltip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user_root);
    std::fs::create_dir_all(&user_root).unwrap();

    let counter = Arc::new(Counter::default());
    let core = MobileCore::new(&data_dir, &user_root, "wubi86", counter.clone());
    let t0 = Instant::now();
    while core.debug_coordinator().readiness() != wind_host::Readiness::Ready {
        assert!(t0.elapsed() < Duration::from_secs(60), "prepare 迟迟未就绪");
        std::thread::sleep(Duration::from_millis(10));
    }
    settle(&counter, Duration::from_millis(300));

    for c in "aa".chars() {
        core.key_down((c.to_ascii_uppercase() as u32) & 0xFF, 0);
    }
    // 首帧若触发了懒建结构的后台构建（实测几十毫秒），补刷帧在这段静默窗口里发完；
    // 没触发的话它们留到取提示时才建，下面的断言会抓到
    settle(&counter, Duration::from_millis(1500));
    let before = counter.frames.load(Ordering::SeqCst);

    // 长按：取提示，直到「编码」段出现（说明反查索引已就绪）
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let sections = core.candidate_tooltip(0);
        if sections
            .iter()
            .any(|s| s.title.as_deref().is_some_and(|t| t.contains("编码")))
        {
            break;
        }
        assert!(Instant::now() < deadline, "编码段一直没出现，索引没建成");
        std::thread::sleep(Duration::from_millis(20));
    }
    settle(&counter, Duration::from_millis(500));
    let after = counter.frames.load(Ordering::SeqCst);
    core.key_down(VK_ESCAPE, 0);
    let _ = std::fs::remove_dir_all(&user_root);

    assert_eq!(
        after,
        before,
        "取提示期间又收到 {} 个候选帧（索引在长按时才开始构建，补刷帧会收掉提示卡）",
        after - before
    );
}
