//! 智能符号（同键连按切换中/英标点）端到端测试
//!
//! 覆盖两条新增通路：
//!   1. **反向**（数字后智能标点）：`3.` 的 press1 照旧出英文 `.`，press2 换回中文 `。`。
//!   2. **模式进入键**：`;` 被快捷输入占用，模式内二次按下出 `；` 并武装，第三次按下换 `;`。
//!
//! 这里的每条用例都**先断言 press1 的产物**再断言 press2——press1 走错分支（如反向用例里
//! 出了中文 `。`）时必须当场炸，否则 press2 的断言会在「其实是正向流程」上侥幸通过，成为假绿。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

const VK_OEM_1: u32 = 0xBA; // ;
const VK_OEM_COMMA: u32 = 0xBC; // ,
const VK_OEM_PERIOD: u32 = 0xBE; // .

fn data_dir() -> PathBuf {
    // 三级：crates/wind-coordinator → crates → wind_input → 仓库根（build_dev 在仓库根）。
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 标点用例不碰引擎，但模式进入（快捷输入）要求方案目录在场。
fn has_data() -> bool {
    data_dir().join("schemas").exists()
}

fn cfg_smart() -> Config {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.default.chinese_punct = true;
    cfg.input.symbol.smart_mode = true;
    cfg
}

fn press(coord: &Coordinator, vk: u32, prev_char: u16) -> KeyAction {
    coord.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char,
    })
}

fn inserted(a: &KeyAction) -> Option<&str> {
    match a {
        KeyAction::InsertText { text, .. } => Some(text),
        _ => None,
    }
}

fn replaced(a: &KeyAction) -> Option<(u32, &str)> {
    match a {
        KeyAction::ReplaceBackward { count, text } => Some((*count, text)),
        _ => None,
    }
}

/// 反向主用例：光标前是数字 → press1 出英文（数字后智能语义不变），press2 换回中文。
/// 改造前这里 press1 之后就没有下文了——`smart_symbol_arm_str` 遇数字后智能直接不武装。
#[test]
fn after_digit_press1_english_then_press2_back_to_chinese() {
    let coord = Coordinator::new_headless(cfg_smart(), Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_PERIOD, b'5' as u16);
    assert_eq!(
        inserted(&a1),
        Some("."),
        "数字后 press1 必须仍出英文句点（数字后智能语义不变），实际: {:?}",
        a1
    );
    let a2 = press(&coord, VK_OEM_PERIOD, '.' as u16);
    assert_eq!(
        replaced(&a2),
        Some((1, "。")),
        "时限内同键 press2 应把英文句点换成中文句号，实际: {:?}",
        a2
    );
}

/// 正向回归锁：非数字后照旧「press1 中文 → press2 英文」，方向维度不得污染既有语义。
#[test]
fn normal_press1_chinese_then_press2_english() {
    let coord = Coordinator::new_headless(cfg_smart(), Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(inserted(&a1), Some("。"), "实际: {:?}", a1);
    let a2 = press(&coord, VK_OEM_PERIOD, '。' as u16);
    assert_eq!(replaced(&a2), Some((1, ".")), "实际: {:?}", a2);
}

/// 总开关关闭时数字后行为**完全维持改造前**：press1 出英文 `.`，第二次按下只是普通标点追加
/// （此时光标前已是 `.` 而非数字，故出中文 `。`），**不得**出现任何 `ReplaceBackward`。
/// 屏上因此是 `3.。`——与开着开关时的 `3。`（替换）恰成对照，这正是该开关的全部差别。
#[test]
fn after_digit_without_smart_mode_never_replaces() {
    let mut cfg = cfg_smart();
    cfg.input.symbol.smart_mode = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_PERIOD, b'5' as u16);
    assert_eq!(inserted(&a1), Some("."), "实际: {:?}", a1);
    let a2 = press(&coord, VK_OEM_PERIOD, '.' as u16);
    assert_eq!(
        replaced(&a2),
        None,
        "关掉智能符号总开关后不得有任何删改替换，实际: {:?}",
        a2
    );
    assert_eq!(inserted(&a2), Some("。"), "实际: {:?}", a2);
}

/// 反向只认 `punct.smart_list` 里的标点：把列表收窄成 "."，同样在数字后的 `,` 应走**正向**
/// （press1 中文 `，` → press2 英文 `,`）。锁住「方向由数字后智能判定，而非由 prev_char 是数字」。
#[test]
fn digit_context_outside_smart_list_stays_forward() {
    let mut cfg = cfg_smart();
    cfg.input.punct.smart_list = ".".to_string();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_COMMA, b'5' as u16);
    assert_eq!(
        inserted(&a1),
        Some("，"),
        "逗号不在 smart_list 里，数字后也该出中文，实际: {:?}",
        a1
    );
    let a2 = press(&coord, VK_OEM_COMMA, '，' as u16);
    assert_eq!(replaced(&a2), Some((1, ",")), "实际: {:?}", a2);
}

/// 需求 2 主用例：`;` 被快捷输入占用 → 进模式 → 模式内二次按下出 `；` 并武装 →
/// 第三次按下换英文 `;`（而不是又进一次模式）。
#[test]
fn mode_trigger_third_press_replaces_with_english() {
    if !has_data() {
        eprintln!("跳过：缺少 build_dev/data/schemas");
        return;
    }
    let coord = Coordinator::new_headless(cfg_smart(), Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_1, 0);
    assert!(
        matches!(a1, KeyAction::UpdateComposition { .. }),
        "第一次按 ; 应进入快捷输入模式，实际: {:?}",
        a1
    );
    let a2 = press(&coord, VK_OEM_1, 0);
    assert_eq!(
        inserted(&a2),
        Some("；"),
        "模式内二次按下应上屏中文分号并退出，实际: {:?}",
        a2
    );
    let a3 = press(&coord, VK_OEM_1, '；' as u16);
    assert_eq!(
        replaced(&a3),
        Some((1, ";")),
        "时限内第三次按下应替换为英文分号（须抢在模式激活之前），实际: {:?}",
        a3
    );
}

/// 需求 2 的门控：符号不在 `symbol.smart_chars` 里就不武装，第三次按下回到「再进一次模式」——
/// 与改造前行为一致（用户拍板：模式进入键仍受参与集合限制）。
#[test]
fn mode_trigger_not_in_smart_chars_keeps_old_behavior() {
    if !has_data() {
        eprintln!("跳过：缺少 build_dev/data/schemas");
        return;
    }
    let mut cfg = cfg_smart();
    cfg.input.symbol.smart_chars = "。，".to_string(); // 不含 ；
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, VK_OEM_1, 0);
    let a2 = press(&coord, VK_OEM_1, 0);
    assert_eq!(inserted(&a2), Some("；"), "实际: {:?}", a2);
    let a3 = press(&coord, VK_OEM_1, '；' as u16);
    assert!(
        matches!(a3, KeyAction::UpdateComposition { .. }),
        "未武装时第三次按下应照旧进入模式，实际: {:?}",
        a3
    );
}

// ── 英文标点状态（中文输入 + 工具栏标点切英文，`english_punct_mode`）────────────────

fn cfg_en_punct() -> Config {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.default.chinese_punct = false; // 标点切英文
    cfg.input.symbol.english_punct_mode = true;
    cfg
}

/// 英文标点状态：press1 出英文 `.`，时限内再按换成中文 `。`。
#[test]
fn english_punct_press1_english_then_press2_chinese() {
    let coord = Coordinator::new_headless(cfg_en_punct(), Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(
        inserted(&a1),
        Some("."),
        "英文标点状态 press1 应出英文句点，实际: {:?}",
        a1
    );
    let a2 = press(&coord, VK_OEM_PERIOD, '.' as u16);
    assert_eq!(
        replaced(&a2),
        Some((1, "。")),
        "时限内 press2 应换成中文句号，实际: {:?}",
        a2
    );
}

/// 中文侧总开关与英文侧**互不影响**：只开 `smart_mode`（中文侧）时英文标点状态不该有替换。
#[test]
fn english_punct_requires_its_own_switch() {
    let mut cfg = cfg_en_punct();
    cfg.input.symbol.english_punct_mode = false;
    cfg.input.symbol.smart_mode = true; // 中文侧开着也不该外溢到英文标点状态
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(inserted(&a1), Some("."), "实际: {:?}", a1);
    let a2 = press(&coord, VK_OEM_PERIOD, '.' as u16);
    assert_eq!(
        replaced(&a2),
        None,
        "英文侧开关关闭时不得有任何替换，实际: {:?}",
        a2
    );
}

/// 参与集合按**源字符**判定：把 `english_chars` 收窄成 ","，`.` 就不再参与。
#[test]
fn english_punct_outside_english_chars_not_armed() {
    let mut cfg = cfg_en_punct();
    cfg.input.symbol.english_chars = ",".to_string();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, VK_OEM_PERIOD, 0);
    let a2 = press(&coord, VK_OEM_PERIOD, '.' as u16);
    assert_eq!(replaced(&a2), None, "实际: {:?}", a2);
    // 同一份配置下逗号仍参与——证明上面的 None 是集合判定所致，而非整个开关没生效。
    press(&coord, VK_OEM_COMMA, 0);
    let b2 = press(&coord, VK_OEM_COMMA, ',' as u16);
    assert_eq!(replaced(&b2), Some((1, "，")), "实际: {:?}", b2);
}

// ── 英文输入模式（整个输入法切英文，`english_mode`）──────────────────────────────────

fn cfg_en_mode() -> Config {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = false; // 英文输入模式
    cfg.input.symbol.english_mode = true;
    cfg
}

/// 英文输入模式：press1 出英文 `.`（此前这个键是直接透传给宿主的），press2 换中文 `。`。
/// 前置条件是 core 把 `english_chars` 并入了推给 DLL 的吃键集，否则引擎根本收不到这个键。
#[test]
fn english_mode_press1_english_then_press2_chinese() {
    let coord = Coordinator::new_headless(cfg_en_mode(), Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(
        inserted(&a1),
        Some("."),
        "英文模式 press1 应由 core 出英文句点（而非 PassThrough），实际: {:?}",
        a1
    );
    let a2 = press(&coord, VK_OEM_PERIOD, '.' as u16);
    assert_eq!(
        replaced(&a2),
        Some((1, "。")),
        "时限内 press2 应换成中文句号，实际: {:?}",
        a2
    );
}

/// 关掉 `english_mode`：标点键回到**透传**（吃键集为空，DLL 压根不吃、core 也不接手）。
/// 这条同时锁住「开关关闭 = 与历史行为完全一致」，是本功能不惊扰纯英文用户的底线。
#[test]
fn english_mode_off_passes_through() {
    let mut cfg = cfg_en_mode();
    cfg.input.symbol.english_mode = false;
    cfg.input.symbol.smart_mode = true; // 中文侧开着也不该外溢
    cfg.input.symbol.english_punct_mode = true; // 英文标点状态开着同样不该外溢到英文模式
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_PERIOD, 0);
    assert!(
        matches!(a1, KeyAction::PassThrough),
        "关掉 english_mode 后标点键应透传，实际: {:?}",
        a1
    );
}

/// 超时后模式进入键必须**交还**给模式激活链：武装是有时限的劫持，不是永久接管。
#[test]
fn mode_trigger_after_timeout_enters_mode_again() {
    if !has_data() {
        eprintln!("跳过：缺少 build_dev/data/schemas");
        return;
    }
    let mut cfg = cfg_smart();
    cfg.input.symbol.smart_timeout_ms = 1;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, VK_OEM_1, 0);
    let a2 = press(&coord, VK_OEM_1, 0);
    assert_eq!(inserted(&a2), Some("；"), "实际: {:?}", a2);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let a3 = press(&coord, VK_OEM_1, '；' as u16);
    assert!(
        matches!(a3, KeyAction::UpdateComposition { .. }),
        "超时后第三次按下应重新进入模式，实际: {:?}",
        a3
    );
}

// ── HoldComposition（组合态预览）方案：held 符号的去向只由宿主端交代 ────────────────
//
// 本组用例守的是一条**跨进程**的不变量：press1 的符号此刻只活在宿主的组合态里
// （TSF/macOS 的 `_pendingCommitPrefix`、薄宿主上则已 `EditOp::Commit` 真上屏），
// 服务端后续任何一次上屏都**不得再把它拼进文本**——拼了就是双写，真机表现为
// 「。」+「。%」＝「。。%」。此前这条通路一个测试都没有，双写活了下来。

const VK_5: u32 = 0x35; // Shift+5 → '%'
const MOD_SHIFT: u32 = 0x0001; // 与 wind_ipc::protocol::MOD_SHIFT 同值

fn cfg_hold() -> Config {
    let mut cfg = cfg_smart();
    cfg.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
    cfg
}

fn press_mod(coord: &Coordinator, vk: u32, modifiers: u32, prev_char: u16) -> KeyAction {
    coord.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char,
    })
}

fn held(a: &KeyAction) -> Option<&str> {
    match a {
        KeyAction::HoldComposition { text, .. } => Some(text),
        _ => None,
    }
}

/// 主用例：`。` 挂在组合态里，时限内打一个**不在 `smart_chars` 参与集合**里的标点
/// （Shift+5 → `%`）。上屏文本必须只有 `%`。
///
/// 出厂 `smart_chars = "。，？！：；、～￥·……——"` 不含 Shift+数字那族符号，用户正是
/// 从 `%` 上发现的；`=` `-` `/` `[` 同理，故下面另有一条同族断言。
#[test]
fn hold_then_non_member_punct_commits_symbol_only_once() {
    let coord = Coordinator::new_headless(cfg_hold(), Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(
        held(&a1),
        Some("。"),
        "press1 应把中文句号挂进组合态（HoldComposition），实际: {:?}",
        a1
    );
    let a2 = press_mod(&coord, VK_5, MOD_SHIFT, '。' as u16);
    assert_eq!(
        inserted(&a2),
        Some("%"),
        "held 的「。」由宿主端 absorb 收口，服务端不得再拼一份（拼了真机就是「。。%」），实际: {:?}",
        a2
    );
}

/// 同族第二条：不带 Shift 的非参与集合标点（`=`）走的是同一行代码，一并钉住。
#[test]
fn hold_then_equals_commits_symbol_only_once() {
    const VK_EQUAL: u32 = 0xBB;
    let coord = Coordinator::new_headless(cfg_hold(), Some(&data_dir()));
    assert_eq!(held(&press(&coord, VK_OEM_PERIOD, 0)), Some("。"));
    let a2 = press(&coord, VK_EQUAL, '。' as u16);
    assert_eq!(
        inserted(&a2),
        Some("="),
        "非参与集合标点不得把 held 符号再上屏一次，实际: {:?}",
        a2
    );
}

/// 反向守卫：**参与集合内**的另一个标点走的是另一条短路（新的 HoldComposition），
/// 它同样只出自己那一份。少了这条，上面两条可能在「所有标点都不出 held」的错误实现上假绿。
#[test]
fn hold_then_member_punct_holds_new_symbol_only() {
    let coord = Coordinator::new_headless(cfg_hold(), Some(&data_dir()));
    assert_eq!(held(&press(&coord, VK_OEM_PERIOD, 0)), Some("。"));
    let a2 = press(&coord, VK_OEM_COMMA, '。' as u16);
    assert_eq!(
        held(&a2),
        Some("，"),
        "参与集合内的标点应挂起自己那一份，旧符号交给宿主端 absorb，实际: {:?}",
        a2
    );
}

/// press2（同键连按）不受本次改动影响：仍走 `CommitReplacingHeld` 覆盖组合态。
/// 这条是回归护栏——把「服务端不出 held」误推广到 press2 上，就会打出「。.」。
#[test]
fn hold_press2_still_replaces_held() {
    let coord = Coordinator::new_headless(cfg_hold(), Some(&data_dir()));
    assert_eq!(held(&press(&coord, VK_OEM_PERIOD, 0)), Some("。"));
    let a2 = press(&coord, VK_OEM_PERIOD, '。' as u16);
    match a2 {
        KeyAction::CommitReplacingHeld { ref text, .. } => {
            assert_eq!(text, ".", "press2 应以英文句点覆盖组合态里的中文句号");
        }
        other => panic!("press2 应返回 CommitReplacingHeld，实际: {:?}", other),
    }
}
