//! 全屏否决项与全屏复查线程的开销闸（`coordinator/fullscreen_watch.rs`）。
//!
//! 复查线程本身要问系统前台，跨平台跑不了；能锁住、也正是它依赖的两条性质在这里测：
//! 缓存翻转的两个方向都会重算可见性，以及闸门与显示判据同源。

use crate::coordinator::Coordinator;
use std::sync::atomic::Ordering;
use wind_config::Config;
use wind_ui_types::UiCommand;

/// 三项合取全置真（= 用户停在某宿主的可编辑控件里且开着工具栏）的协调器 + UI 通道。
fn coord() -> (
    std::sync::Arc<Coordinator>,
    std::sync::mpsc::Receiver<UiCommand>,
) {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    {
        let mut s = c.state.lock().unwrap_or_else(|e| e.into_inner());
        s.ime_active = true;
        s.has_edit_context = true;
        s.toolbar_visible = true;
    }
    // 构造过程已推过一次工具栏，去重缓存非空 —— 不归零的话首次断言测的是构造顺序。
    c.reset_toolbar_push_dedup();
    (c, rx)
}

/// 排空通道，返回最后一条工具栏命令是显示（true）还是隐藏（false）；没推过则 None。
fn last_toolbar(rx: &std::sync::mpsc::Receiver<UiCommand>) -> Option<bool> {
    let mut last = None;
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            UiCommand::UpdateToolbar(_) => last = Some(true),
            UiCommand::HideToolbar => last = Some(false),
            _ => {}
        }
    }
    last
}

/// 全屏缓存翻转的两个方向都要重算可见性：进全屏收走工具栏，退出全屏放回来。
///
/// 只测进不测退的话，「进全屏隐藏、此后永不恢复」的实现照样绿，而用户看到的是工具栏
/// 莫名消失。
#[test]
fn fullscreen_hides_and_leaving_it_restores_the_toolbar() {
    let (c, rx) = coord();
    c.notify_toolbar();
    assert_eq!(last_toolbar(&rx), Some(true), "非全屏下工具栏应显示");

    c.fullscreen_cached.store(true, Ordering::Relaxed);
    c.notify_toolbar();
    assert_eq!(last_toolbar(&rx), Some(false), "进全屏应隐藏工具栏");

    c.fullscreen_cached.store(false, Ordering::Relaxed);
    c.notify_toolbar();
    assert_eq!(last_toolbar(&rx), Some(true), "退出全屏应恢复工具栏");
}

/// `ui.toolbar.hide_in_fullscreen=false` 时全屏不改变工具栏显隐——那是用户偏好，
/// 探测照常进行（同一次探测还要给候选窗的独占全屏判据刷值），但不作用于工具栏。
#[test]
fn hide_in_fullscreen_off_keeps_the_toolbar_visible() {
    let mut cfg = Config::default();
    cfg.ui.toolbar.hide_in_fullscreen = false;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
    {
        let mut s = c.state.lock().unwrap_or_else(|e| e.into_inner());
        s.ime_active = true;
        s.has_edit_context = true;
        s.toolbar_visible = true;
    }
    c.reset_toolbar_push_dedup();
    c.fullscreen_cached.store(true, Ordering::Relaxed);
    c.notify_toolbar();
    assert_eq!(
        last_toolbar(&rx),
        Some(true),
        "关掉开关后全屏不应隐藏工具栏"
    );
}

/// 复查线程的开销闸必须与 `notify_toolbar` 的显示判据同源：三项里任一为假就关闸
/// （工具栏本就不显示，前台全不全屏无人关心）。闸比判据窄会让复查漏掉该管的场景。
#[test]
fn watch_gate_follows_the_three_way_conjunction() {
    let (c, _rx) = coord();
    assert!(c.toolbar_wants_display(), "三项全真时闸应开");
    for (name, clear) in [
        ("ime_active", 0usize),
        ("has_edit_context", 1),
        ("toolbar_visible", 2),
    ] {
        {
            let mut s = c.state.lock().unwrap_or_else(|e| e.into_inner());
            s.ime_active = clear != 0;
            s.has_edit_context = clear != 1;
            s.toolbar_visible = clear != 2;
        }
        assert!(!c.toolbar_wants_display(), "{name} 为假时闸应关");
    }
}

/// 单飞闸必须被归还——`ProbeGate` 的 `Drop` 是唯一的归还点。
///
/// 攥死的后果是此后所有探测（焦点事件的与复查线程的）全部 early return，全屏缓存永久冻结。
#[test]
fn the_probe_gate_is_always_returned() {
    let (c, _rx) = coord();
    assert!(c.try_take_probe_gate(), "空闲时应取得闸");
    assert!(!c.try_take_probe_gate(), "已在途时必须挡住第二次");
    {
        let _gate = c.adopt_probe_gate();
        assert!(!c.try_take_probe_gate(), "持有期间仍挡住");
    }
    assert!(c.try_take_probe_gate(), "析构后闸必须已归还");
}

/// 提交一次探测结果：缓存翻转要落进去，并重算工具栏可见性。
///
/// 非 Windows 上 `foreground_fullscreen_kind` 恒 `None`，故直接喂形态给 `commit_fullscreen_kind`，
/// 测复查线程与焦点探测线程共用的那段提交逻辑。
#[test]
fn commit_writes_the_cache_and_recomputes_the_toolbar() {
    let (c, rx) = coord();
    c.notify_toolbar();
    assert_eq!(last_toolbar(&rx), Some(true), "前置：工具栏本应显示");

    c.commit_fullscreen_kind(crate::FullscreenKind::Covering);
    assert!(c.fullscreen_cached.load(Ordering::Relaxed), "缓存应置真");
    assert_eq!(last_toolbar(&rx), Some(false), "进全屏应收起工具栏");

    c.commit_fullscreen_kind(crate::FullscreenKind::None);
    assert!(!c.fullscreen_cached.load(Ordering::Relaxed), "缓存应归零");
    assert_eq!(last_toolbar(&rx), Some(true), "退出全屏应把工具栏放回来");
}

/// 复查线程那条提交只动 `fullscreen_cached`，不得碰独占位。
///
/// 它刻意不问判据①（跨进程 RPC，不能按拍反复发），手里没有独占位的新值；顺手清掉的话，
/// 放 PPT / 跑独占全屏游戏的机器会被误判成「不独占」，候选窗弹进独占全屏会冻死。
#[test]
fn the_periodic_commit_never_touches_the_exclusive_bit() {
    let (c, _rx) = coord();
    c.fullscreen_exclusive_cached.store(true, Ordering::Relaxed);
    c.commit_fullscreen_covering(true);
    assert!(
        c.fullscreen_exclusive_cached.load(Ordering::Relaxed),
        "复查不问判据①，就不能拿「没问到」去清独占位"
    );
    c.commit_fullscreen_covering(false);
    assert!(
        c.fullscreen_exclusive_cached.load(Ordering::Relaxed),
        "反向同理"
    );
}

/// `Covering → D3dExclusive`：`is_fs` 两端都是真、不翻转，但工具栏必须重算。
///
/// 独占全屏经 `ui_suppressed_by_host()` 单独否决工具栏，不受 `hide_in_fullscreen` 管（弹在
/// 独占全屏上会把游戏踢出独占态）。只看 `is_fs` 翻转的实现在此什么都不做：无边框切独占、
/// PPT 进放映时工具栏留在上面，退出放映时它再也回不来。
#[test]
fn switching_from_covering_to_exclusive_still_recomputes() {
    let mut cfg = Config::default();
    // 关掉开关，隔离出「只由独占位否决」这一格：否则 Covering 那步就已经把工具栏收了，
    // 断言分不出是哪一位起的作用。
    cfg.ui.toolbar.hide_in_fullscreen = false;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
    {
        let mut s = c.state.lock().unwrap_or_else(|e| e.into_inner());
        s.ime_active = true;
        s.has_edit_context = true;
        s.toolbar_visible = true;
    }
    c.reset_toolbar_push_dedup();
    c.commit_fullscreen_kind(crate::FullscreenKind::Covering);
    c.notify_toolbar();
    assert_eq!(
        last_toolbar(&rx),
        Some(true),
        "前置：关了开关的无边框全屏不该隐藏工具栏"
    );

    c.commit_fullscreen_kind(crate::FullscreenKind::D3dExclusive);
    assert_eq!(
        last_toolbar(&rx),
        Some(false),
        "切进 D3D 独占全屏必须收起工具栏，哪怕 is_fs 这一位没翻转"
    );
}

/// 闸门不含全屏否决：工具栏正因全屏而隐藏时，复查必须继续跑。
///
/// 把全屏写进闸门的话：进全屏 → 工具栏隐藏 → 闸门关闭 → 再没人看用户退没退出全屏，
/// 工具栏永远回不来。
#[test]
fn watch_gate_stays_open_while_hidden_by_fullscreen() {
    let (c, _rx) = coord();
    c.fullscreen_cached.store(true, Ordering::Relaxed);
    assert!(
        c.toolbar_wants_display(),
        "因全屏隐藏时闸仍须开着，否则退出全屏后无人恢复工具栏"
    );
}
