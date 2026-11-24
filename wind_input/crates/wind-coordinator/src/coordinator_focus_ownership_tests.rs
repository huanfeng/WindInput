//! 失焦事件的客户端归属校验：旧宿主迟到的 focus_lost 不得清掉新宿主刚建立的激活态，
//! 否则跨宿主切换后工具栏闪一下即隐藏。迟到时序见
//! docs/architecture/tsf-docmgr-focus-semantics.md 的「实测：宿主内部的 DocMgr 抖动是常态」。
use super::*;

/// 已有宿主 `token` 处于激活态、且焦点在可编辑控件里的协调器。
fn activated(token: u64) -> Arc<Coordinator> {
    let c = Coordinator::new_headless(Config::default(), None);
    c.push_server.set_active_token(token);
    let mut s = c.state.lock().unwrap();
    s.ime_active = true;
    s.has_edit_context = true;
    drop(s);
    c
}

/// 四种 reason 的后果矩阵——本设计的核心契约。
///
/// 三项后果彼此独立，任何一格改错都会复活一个已修的缺陷：
/// - `CtxLost` 那行的「输入态不清」＝ Excel「首字符不进编码、直接上屏」的防线；
/// - `DocChanged` 那行的「ime_active 不动」＝ 同宿主换文档不再误关工具栏；
/// - 各行的 `has_edit_context`＝ 应用内点到非文本框时工具栏能否隐藏。
#[test]
fn focus_lost_reason_consequence_matrix() {
    // (reason, ime_active 保留?, has_edit_context 保留?, 输入态保留?)
    let cases = [
        (FocusLostReason::Thread, false, false, false),
        (FocusLostReason::DocChanged, true, true, false),
        (FocusLostReason::CtxLost, true, false, true),
        (FocusLostReason::NoEditCtx, true, false, false),
    ];
    for (reason, keep_ime, keep_edit, keep_input) in cases {
        let c = activated(NOTEPAD);
        c.state.lock().unwrap().input_buffer.push_str("abc");

        c.handle_focus_lost(NOTEPAD, reason);

        let s = c.state.lock().unwrap();
        assert_eq!(
            s.ime_active, keep_ime,
            "{reason:?}: ime_active 应为 {keep_ime}"
        );
        assert_eq!(
            s.has_edit_context, keep_edit,
            "{reason:?}: has_edit_context 应为 {keep_edit}"
        );
        assert_eq!(
            !s.input_buffer.is_empty(),
            keep_input,
            "{reason:?}: 输入态保留应为 {keep_input}"
        );
    }
}

/// CtxLost 来自 DocMgr 噪声层（Excel 同一 DocMgr 6ms 内掉了又回），在那里清输入态
/// 就是「首字符直接上屏」的根因。单独立一条守住这个不变量。
#[test]
fn ctx_lost_never_touches_input_buffer() {
    let c = activated(NOTEPAD);
    c.state.lock().unwrap().input_buffer.push_str("nihao");
    c.handle_focus_lost(NOTEPAD, FocusLostReason::CtxLost);
    assert_eq!(
        c.state.lock().unwrap().input_buffer,
        "nihao",
        "CtxLost 绝不可清输入态，否则复发 Excel 首字符丢失"
    );
}

/// 陈旧失焦被丢弃时，四种 reason 都不得改动任何输入/激活状态。
///
/// 菜单是刻意的例外（见 `stale_focus_lost_still_closes_menu`），补断言时别把菜单算进来。
#[test]
fn stale_focus_lost_is_inert_for_all_reasons() {
    for reason in [
        FocusLostReason::Thread,
        FocusLostReason::DocChanged,
        FocusLostReason::CtxLost,
        FocusLostReason::NoEditCtx,
    ] {
        let c = activated(NOTEPAD);
        c.handle_focus_lost(TERMINAL, reason);
        let s = c.state.lock().unwrap();
        assert!(s.ime_active, "{reason:?}: 陈旧失焦不得清 ime_active");
        assert!(
            s.has_edit_context,
            "{reason:?}: 陈旧失焦不得清 has_edit_context"
        );
    }
}

const NOTEPAD: u64 = 0x0000_3644_0000_0001;
const TERMINAL: u64 = 0x0000_3ECC_0000_0001;

#[test]
fn stale_focus_lost_keeps_activation() {
    let c = activated(NOTEPAD);
    c.handle_focus_lost(TERMINAL, FocusLostReason::Thread);
    assert!(
        c.state.lock().unwrap().ime_active,
        "旧宿主迟到的失焦不得清激活态，否则工具栏闪一下即隐藏"
    );
}

#[test]
fn own_focus_lost_clears_activation() {
    let c = activated(NOTEPAD);
    c.handle_focus_lost(NOTEPAD, FocusLostReason::Thread);
    assert!(
        !c.state.lock().unwrap().ime_active,
        "当前活动客户端自己失焦仍须正常清激活态"
    );
}

#[test]
fn legacy_zero_token_still_clears() {
    let c = activated(NOTEPAD);
    c.handle_focus_lost(0, FocusLostReason::Thread);
    assert!(
        !c.state.lock().unwrap().ime_active,
        "旧 DLL 不带 token，保守放行以保持既有行为"
    );
}

#[test]
fn stale_ime_deactivated_keeps_activation() {
    let c = activated(NOTEPAD);
    c.handle_ime_deactivated(TERMINAL);
    assert!(
        c.state.lock().unwrap().ime_active,
        "IME_DEACTIVATED 与 focus_lost 同为异步写，乱序风险相同"
    );
}

#[test]
fn own_ime_deactivated_clears_activation() {
    let c = activated(NOTEPAD);
    c.handle_ime_deactivated(NOTEPAD);
    assert!(!c.state.lock().unwrap().ime_active);
}

// ———————————————— 焦点变化关闭菜单 ————————————————
//
// 菜单是模态 UI，任何焦点变动都该终结它；输入态清理则必须保守。两者不能共用
// `clears_input`，否则 CtxLost 豁免 / 陈旧失焦丢弃会顺带吞掉关菜单，切走窗口菜单还挂着。

/// 构造「菜单已打开 `age` 时长」的状态。
/// `checked_sub` 失败（开机不足 `age`）时落到 `None`，守卫按不豁免处理，与本组期望一致。
fn open_menu(c: &Coordinator, age: std::time::Duration) {
    let mut s = c.state.lock().unwrap();
    s.menu_open = true;
    s.menu_opened_at = std::time::Instant::now().checked_sub(age);
}

/// 打开够久的菜单
fn open_menu_settled(c: &Coordinator) {
    open_menu(c, crate::handle_menu::MENU_FOCUS_GUARD * 4);
}

/// `CtxLost` 是本组的关键用例：它不清输入态（Excel 首字符防线），但必须关菜单。
/// 两者从此各行其是——这正是本次解耦要证明的事。
#[test]
fn ctx_lost_closes_menu_but_keeps_input() {
    let c = activated(NOTEPAD);
    c.state.lock().unwrap().input_buffer.push_str("nihao");
    open_menu_settled(&c);

    c.handle_focus_lost(NOTEPAD, FocusLostReason::CtxLost);

    let s = c.state.lock().unwrap();
    assert!(!s.menu_open, "CtxLost 必须关菜单（它是一次真实的焦点变动）");
    assert_eq!(
        s.input_buffer, "nihao",
        "CtxLost 仍绝不可清输入态，否则复发 Excel 首字符丢失"
    );
}

/// 陈旧失焦同样要关菜单：stale 只说明不该动激活态，不说明没发生焦点变动。
/// 跨宿主切换时旧宿主的失焦恒被判 stale，跟着丢弃的话菜单就永远挂着。
#[test]
fn stale_focus_lost_still_closes_menu() {
    let c = activated(NOTEPAD);
    open_menu_settled(&c);

    c.handle_focus_lost(TERMINAL, FocusLostReason::Thread);

    let s = c.state.lock().unwrap();
    assert!(!s.menu_open, "陈旧失焦也要关菜单");
    assert!(s.ime_active, "但仍不得清激活态（工具栏闪隐的老缺陷）");
}

/// 切进新的可编辑上下文也算外部动作。
#[test]
fn focus_gained_closes_menu() {
    let c = activated(NOTEPAD);
    open_menu_settled(&c);
    c.handle_focus_gained(&FocusData {
        x: 10,
        y: 20,
        height: 16,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: NOTEPAD,
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        bundle_id: String::new(),
        window_class: String::new(),
        window_title: String::new(),
    });
    assert!(!c.state.lock().unwrap().menu_open);
}

/// 守卫期：菜单刚弹出时到达的焦点事件是打开菜单这一动作的尾迹（旧宿主 focus_lost 迟到），
/// 不豁免就会菜单弹出即消失。
///
/// 用 `CtxLost` 而非 `Thread`：后者走 `clears_input` 分支，会无条件复位菜单态（窗口已被
/// `notify_ui_hide` 隐藏），刻意不受守卫保护，拿它测守卫会测错对象。
#[test]
fn menu_survives_focus_event_within_guard() {
    let c = activated(NOTEPAD);
    open_menu(&c, std::time::Duration::from_millis(0));

    c.handle_focus_lost(NOTEPAD, FocusLostReason::CtxLost);

    assert!(
        c.state.lock().unwrap().menu_open,
        "守卫期内的焦点事件不得关掉刚弹出的菜单"
    );
}

/// 同一宿主内多个 DocMgr 共用一个 token，一律放行——那层抖动（doc_changed 先发
/// focus_lost 紧接 focus_gained，间隔 <10ms）由 UI 层 50ms 隐藏防抖吸收，不归本校验管。
#[test]
fn same_host_doc_churn_is_not_stale() {
    let c = activated(NOTEPAD);
    assert!(!c.is_stale_focus_event(NOTEPAD, "test"));
}

/// 服务端刚启动、尚无任何客户端获焦：无从判定归属，放行。
#[test]
fn no_active_client_is_not_stale() {
    let c = Coordinator::new_headless(Config::default(), None);
    assert!(!c.is_stale_focus_event(TERMINAL, "test"));
}
