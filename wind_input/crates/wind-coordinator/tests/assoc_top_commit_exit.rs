//! 联想态下的**顶屏类按键**（标点 / 小键盘 / 非码元字符 / 进模式、开软键盘的引导键）
//! 必须把联想态收干净——与空格选联想、Esc 取消之后的状态一致。
//!
//! 现场：有联想候选时按标点，联想态不退出，之后候选窗位置不再跟随光标。
//!
//! 根因：智能符号 `hold_composition` 方案的 press1 在「无输入」时**短路**返回
//! `HoldComposition`，而联想态恰是「缓冲空、已转换段空、候选非空」——标点臂里那段
//! 清候选的代码根本走不到。联想候选于是留在 `state.candidates` 里：
//! `handle_caret_update` 按「候选非空」把此后的空闲光标上报都当成组合期间的上报，
//! 锁住组合起点，候选窗就钉在原处；UI 那边也收不到隐藏命令。
//!
//! 其余顶屏出口（普通标点、小键盘、非码元字符）虽清了候选，但没走 `exit_assoc`：
//! 编码栏的「联想输入」标识与自动隐藏计时都留着。进软键盘的引导键则连候选都不清。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{CaretData, KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, caret_source};
use wind_ui_types::UiCommand;

const VK_PERIOD: u32 = 0xBE;
const VK_NUMPAD1: u32 = 0x61;
const VK_BACKSLASH: u32 = 0xDC;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
}

fn key(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn caret(x: i32) -> CaretData {
    CaretData {
        x,
        y: 200,
        height: 20,
        composition_start_x: x,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    }
}

/// 混输方案 + 词语联想（与 `assoc_end_to_end` 同一夹具口径）。
fn open(
    tweak: impl FnOnce(&mut Config),
) -> (Arc<Coordinator>, std::sync::mpsc::Receiver<UiCommand>) {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86_pinyin".to_string()];
    cfg.schema.active = "wubi86_pinyin".to_string();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = "word".to_string();
    cfg.input.association.mode = "one_shot".to_string();
    // 非嵌入：联想态才会往编码栏写「联想输入」标识（嵌入模式下不给）。
    cfg.ui.candidate.preedit_display = "candidate_top".to_string();
    tweak(&mut cfg);
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    c.prewarm_indexes();
    (c, rx)
}

/// 打 `q` + 空格上屏「我」进联想态；返回联想候选（非空为前提）。
fn enter_assoc(c: &Coordinator) -> Vec<String> {
    c.handle_key_event(&key(0x51)); // q
    // 释放首显闸门（headless 无宿主上报）。
    c.handle_caret_update(&caret(100));
    c.handle_key_event(&key(0x20));
    let hits = c.debug_assoc_texts();
    assert!(!hits.is_empty(), "前提：进了联想态");
    hits
}

fn hid(rx: &std::sync::mpsc::Receiver<UiCommand>) -> bool {
    rx.try_iter()
        .filter(|c| matches!(c, UiCommand::HideCandidates))
        .count()
        > 0
}

/// 下一次输入的组合状态与候选窗位置正常：不挂联想、候选下发、位置落在新光标处。
///
/// ⚠️ 如实交代：headless 下没有真宿主的组合起点上报时序，这条位置断言**单独撤掉修复
/// 并不会红**（实测）——缺陷的判据在各用例里「联想态已退出 / 收到隐藏命令」那两条上，
/// 它们对应的正是 `handle_caret_update` 的「候选非空 = 组合中」判据。这里只守修复后
/// 下一轮输入没有被带坏。
fn next_input_follows_caret(c: &Coordinator, rx: &std::sync::mpsc::Receiver<UiCommand>) {
    // 顶屏上屏后宿主重排：先报一帧 150（标点刚落地的位置），光标随后停在 400。
    // 残留联想态时前一帧会被当成「组合期间」的上报锁成组合起点，候选窗从此钉在 150。
    c.handle_caret_update(&caret(150));
    c.handle_caret_update(&caret(400));
    let _ = rx.try_iter().count();
    c.handle_key_event(&key(0x51)); // q，新一轮组合
    assert!(c.debug_assoc_texts().is_empty(), "新输入不该挂着联想");
    c.handle_caret_update(&caret(400));
    let xs: Vec<i32> = rx
        .try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates {
                caret_x,
                candidates,
                ..
            } if !candidates.is_empty() => Some(caret_x),
            _ => None,
        })
        .collect();
    assert!(!xs.is_empty(), "新输入应下发候选");
    assert_eq!(
        *xs.last().unwrap(),
        400,
        "候选窗应跟随新光标，实际下发位置 {xs:?}"
    );
}

/// ★ 现场复现：智能符号 `hold_composition` 下按标点，联想必须退出、候选窗必须隐藏，
/// 下一次输入的候选窗位置跟随光标。
#[test]
fn smart_hold_punct_exits_assoc() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (c, rx) = open(|cfg| {
        cfg.input.symbol.smart_mode = true;
        cfg.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
    });
    let hits = enter_assoc(&c);
    let _ = rx.try_iter().count();
    let act = c.handle_key_event(&key(VK_PERIOD));
    match &act {
        KeyAction::HoldComposition { text, .. } => {
            assert!(
                hits.iter().all(|h| !text.contains(h.as_str())),
                "不该顶屏联想候选：{text:?}"
            );
        }
        other => panic!("前提：智能符号 hold 出标点，实得 {other:?}"),
    }
    assert!(c.debug_assoc_texts().is_empty(), "标点之后联想态必须退出");
    assert!(hid(&rx), "联想窗必须随标点收起");
    next_input_follows_caret(&c, &rx);
}

/// 普通标点：联想候选清掉之外，编码栏的「联想输入」标识也得清掉——与 Esc 取消一致。
#[test]
fn plain_punct_clears_assoc_hint() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (c, rx) = open(|_| {});
    enter_assoc(&c);
    let act = c.handle_key_event(&key(VK_PERIOD));
    assert!(
        matches!(act, KeyAction::InsertText { .. }),
        "前提：标点上屏，实得 {act:?}"
    );
    assert!(c.debug_assoc_texts().is_empty());
    assert!(
        c.debug_preedit().is_empty(),
        "「联想输入」标识必须随联想一起清掉，实得 {:?}",
        c.debug_preedit()
    );
    next_input_follows_caret(&c, &rx);
}

/// 小键盘（direct 档顶屏 + 出字）：同标点。
#[test]
fn numpad_clears_assoc_hint() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (c, rx) = open(|_| {});
    let hits = enter_assoc(&c);
    let act = c.handle_key_event(&key(VK_NUMPAD1));
    match &act {
        KeyAction::InsertText { text, .. } => {
            assert!(hits.iter().all(|h| !text.contains(h.as_str())), "{text:?}");
        }
        other => panic!("前提：小键盘出字，实得 {other:?}"),
    }
    assert!(c.debug_assoc_texts().is_empty());
    assert!(c.debug_preedit().is_empty(), "{:?}", c.debug_preedit());
    next_input_follows_caret(&c, &rx);
}

/// 论坛 t285 的边界：小键盘在「空闲 + 半角」时改为透传，但联想态**不算空闲**——
/// 联想挂着占位组合与候选，透传会把组合悬在宿主里、联想也收不掉。必须仍由我们出字
/// （联想态不顶屏，故上屏的只有数字本身）。判据见 message_handler 小键盘臂的 `has_comp`。
#[test]
fn numpad_in_assoc_is_not_passthrough() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (c, _rx) = open(|_| {});
    enter_assoc(&c);
    let act = c.handle_key_event(&key(VK_NUMPAD1));
    match &act {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "1", "联想态小键盘应只上屏数字（不顶联想候选）")
        }
        other => panic!("联想态小键盘不得透传，应 InsertText，实得 {other:?}"),
    }
    assert!(c.debug_assoc_texts().is_empty(), "联想应已收掉");
}

/// 绑到软键盘的引导键：软键盘不是模式，进不了「各 `enter_*` 清候选」那条隐式退出；
/// 联想候选原样挂着、占位组合也没人收。
#[test]
fn softkeyboard_key_exits_assoc() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (c, rx) = open(|cfg| {
        cfg.keys
            .key_actions
            .insert("backslash".into(), "softkeyboard".into());
    });
    enter_assoc(&c);
    let _ = rx.try_iter().count();
    let act = c.handle_key_event(&key(VK_BACKSLASH));
    assert!(c.debug_assoc_texts().is_empty(), "开软键盘后联想必须退出");
    assert!(hid(&rx), "联想窗必须收起");
    assert!(
        matches!(act, KeyAction::InsertText { ref text, .. } if text.is_empty()),
        "占位组合必须收掉（空文本上屏结束组合），实得 {act:?}"
    );
}

/// 联想态下按右括号**跳出**已插入的配对：跳出语义保留（`MoveCursorRight`），但占位组合
/// 不能被晾在宿主里——`MoveCursorRight` 不碰组合，必须另行收口：下一个透传键改判为
/// 「收组合 + 交还按键」。
#[test]
fn right_bracket_jump_out_in_assoc_releases_placeholder() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (c, _rx) = open(|cfg| {
        cfg.input.auto_pair.chinese = true;
        cfg.input.auto_pair.jump_out_keys = vec!["right_symbol".into()];
    });
    // Shift+9 = 「（」，自动补「）」，光标落在中间。
    let act = c.handle_key_event(&KeyEventData {
        modifiers: wind_ipc::protocol::MOD_SHIFT,
        ..key(0x39)
    });
    assert!(
        matches!(act, KeyAction::InsertTextWithCursor { .. }),
        "前提：左括号配对插入，实得 {act:?}"
    );
    enter_assoc(&c);
    // 走按键唯一出口（`handle_key_event_policed`）：两道占位收口挂在那里。
    let act = c.handle_key_event_policed(&KeyEventData {
        modifiers: wind_ipc::protocol::MOD_SHIFT,
        ..key(0x30)
    });
    assert!(
        matches!(act, KeyAction::MoveCursorRight { .. }),
        "前提：右括号跳出配对，实得 {act:?}"
    );
    assert!(c.debug_assoc_texts().is_empty(), "联想应已退出");
    // 下一个透传键（←）：占位组合若还悬着，必须顺带收掉。
    let act = c.handle_key_event_policed(&key(0x25));
    assert!(
        matches!(act, KeyAction::ClearCompositionThenPassThrough),
        "占位组合应被收口，实得 {act:?}"
    );
}

/// 联想态 + CapsLock 开着按标点：占位组合不能被晾着——要么服务端出标点（结束组合），
/// 要么按 CapsLock 的英文语义交还宿主时**顺带收组合**；绝不能是裸透传。联想退出。
#[test]
fn capslock_punct_in_assoc_is_committed_by_server() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (c, _rx) = open(|_| {});
    enter_assoc(&c);
    let act = c.handle_key_event_policed(&KeyEventData {
        toggles: wind_ipc::protocol::TOGGLE_CAPSLOCK,
        ..key(VK_PERIOD)
    });
    assert!(
        matches!(
            act,
            KeyAction::InsertText { .. } | KeyAction::ClearCompositionThenPassThrough
        ),
        "占位组合必须收口，实得 {act:?}"
    );
    assert!(c.debug_assoc_texts().is_empty(), "联想应已退出");
}

/// 智能符号 press1 收掉联想后，时限内同键 press2 照常换英文形，联想不会回来。
#[test]
fn smart_symbol_press2_after_assoc_exit() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    for method in [
        wind_config::config::SmartMethod::DeleteReplace,
        wind_config::config::SmartMethod::HoldComposition,
    ] {
        let (c, _rx) = open(|cfg| {
            cfg.input.symbol.smart_mode = true;
            cfg.input.symbol.smart_method = method;
        });
        enter_assoc(&c);
        let first = c.handle_key_event_policed(&key(VK_PERIOD));
        assert!(
            c.debug_assoc_texts().is_empty(),
            "{method:?}：press1 后联想退出"
        );
        let second = c.handle_key_event_policed(&KeyEventData {
            prev_char: '。' as u16,
            seq_char: 0,
            ..key(VK_PERIOD)
        });
        assert!(
            matches!(
                second,
                KeyAction::ReplaceBackward { .. } | KeyAction::CommitReplacingHeld { .. }
            ),
            "{method:?}：press2 应换英文形，press1={first:?} press2={second:?}"
        );
        assert!(c.debug_assoc_texts().is_empty(), "{method:?}：联想不得回来");
    }
}
