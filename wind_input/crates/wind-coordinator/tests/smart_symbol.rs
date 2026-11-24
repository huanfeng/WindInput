//! 智能符号（同键连按切换中/英标点）端到端测试
//!
//! 覆盖两条通路：
//!   1. 反向（数字后智能标点）：`3.` 的 press1 照旧出英文 `.`，press2 换回中文 `。`。
//!   2. 模式进入键：`;` 被快捷输入占用，模式内二次按下出 `；` 并武装，第三次按下换 `;`。
//!
//! 每条用例都先断言 press1 的产物再断言 press2：press1 走错分支时必须当场红，
//! 否则 press2 的断言可能在「其实是正向流程」上假绿。

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
        seq_char: 0,
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

/// `1.1.` 快打时中间那个 `1` 不许被 press2 吃掉。
///
/// 两个 `.` 同键且在时限内，但第二个 `.` 的光标前是 `1` 而武装串末位是 `.`，
/// 只要客户端送了 `prev_char`，这道守卫就必须挡住 `ReplaceBackward`。
#[test]
fn digit_between_two_periods_is_not_eaten_by_press2() {
    let coord = Coordinator::new_headless(cfg_smart(), Some(&data_dir()));
    // "1." → 数字后智能：出英文句点，并反向武装。
    let a1 = press(&coord, VK_OEM_PERIOD, b'1' as u16);
    assert_eq!(inserted(&a1), Some("."), "实际: {:?}", a1);
    // "1" 透传（不经服务端出字），再按 "."：光标前是 `1`，不是武装串末位 `.`。
    let a2 = press(&coord, VK_OEM_PERIOD, b'1' as u16);
    assert!(
        replaced(&a2).is_none(),
        "光标前是数字而非武装串末位，不该判 press2（会删掉那个数字），实际: {:?}",
        a2
    );
    assert_eq!(
        inserted(&a2),
        Some("."),
        "应回落正常流程：数字后智能仍命中，出英文句点，实际: {:?}",
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

/// 总开关关闭时数字后行为不变：press1 出英文 `.`，第二次按下是普通标点追加（光标前已是 `.`，
/// 故出中文 `。`），不得出现任何 `ReplaceBackward`。
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

/// 反向只认 `punct.smart_list` 里的标点：列表收窄成 "." 后，数字后的 `,` 应走正向
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

/// 中文侧总开关与英文侧互不影响：只开 `smart_mode`（中文侧）时英文标点状态不该有替换。
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

/// 参与集合按源字符判定：把 `english_chars` 收窄成 ","，`.` 就不再参与。
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

/// 关掉 `english_mode`：标点键回到透传（吃键集为空），开关关闭须与无此功能时一致。
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

/// 超时后模式进入键必须交还给模式激活链：武装是有时限的劫持，不是永久接管。
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
// 跨进程不变量：press1 的符号此刻只活在宿主的组合态里（TSF/macOS 的 `_pendingCommitPrefix`，
// 薄宿主上已 `EditOp::Commit` 真上屏），服务端后续任何上屏都不得再把它拼进文本，否则双写成「。。%」。

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
        seq_char: 0,
    })
}

fn held(a: &KeyAction) -> Option<&str> {
    match a {
        KeyAction::HoldComposition { text, .. } => Some(text),
        _ => None,
    }
}

/// `。` 挂在组合态里，时限内打一个不在 `smart_chars` 参与集合里的标点（Shift+5 → `%`），
/// 上屏文本必须只有 `%`。出厂 `smart_chars` 不含 Shift+数字那族符号，下面另有一条同族断言。
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

/// 反向守卫：参与集合内的另一个标点走新的 HoldComposition 短路，同样只出自己那一份。
/// 少了这条，上面两条可能在「所有标点都不出 held」的错误实现上假绿。
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

/// hold 预览期间的 Ctrl/Alt 组合键（Ctrl+V 等宿主快捷键）服务端必须回 `PassThrough`：
/// TSF 侧「吃键 → 收口 → 重放」（`KeyEventSink.cpp` 的 `hold_commit_then_replay`）只在 PassThrough 时触发，
/// 改成吃掉或别的动作，粘贴在所有宿主上都会消失。
#[test]
fn hold_then_ctrl_combo_passes_through() {
    const VK_V: u32 = 0x56;
    const VK_S: u32 = 0x53;
    const MOD_CTRL: u32 = 0x0002; // 与 wind_ipc::protocol::MOD_CTRL 同值
    const MOD_ALT: u32 = 0x0004; // 与 wind_ipc::protocol::MOD_ALT 同值
    for (vk, mods) in [(VK_V, MOD_CTRL), (VK_S, MOD_CTRL), (VK_V, MOD_ALT)] {
        let coord = Coordinator::new_headless(cfg_hold(), Some(&data_dir()));
        assert_eq!(held(&press(&coord, VK_OEM_PERIOD, 0)), Some("。"));
        let a2 = press_mod(&coord, vk, mods, '。' as u16);
        assert!(
            matches!(a2, KeyAction::PassThrough),
            "hold 期间 vk=0x{vk:02X} mods=0x{mods:X} 应回 PassThrough，实际: {:?}",
            a2
        );
    }
}

// ── 武装态失效：press1 之后中间夹了别的输入 ─────────────────────────────────────────
//
// 前提：press1 与 press2 之间什么都没发生。中间打字母出了候选时，press2 的
// `ReplaceBackward` / `CommitReplacingHeld` 会落在组合区上，把候选削成一个英文标点。
//
// 两道判据各守一族宿主：
//   - `smart_symbol_press2` 的「press1 之后又打了编码」——所有入口都经过它；
//   - `handle_key_event_policed` 的「非同键按键解除武装」——只在 bridge 按键出口（桌面客户端）。
//
// 因此 `intervening_passthrough_key_disarms` 与 `numpad_punct_press2_survives_the_disarm_guard`
// 必须走 policed 入口，拿内层 `handle_key_event` 测会假绿；
// `intervening_letters_do_not_trigger_press2_mobile_entry` 刻意用内层入口（`wind-mobile` 的走法）
// 盯住第一道判据；其余几条走 policed 是为了贴近桌面真实路径。

const VK_N: u32 = 0x4E;
const VK_I: u32 = 0x49;
const VK_1: u32 = 0x31;

fn press_policed(coord: &Coordinator, vk: u32, prev_char: u16) -> KeyAction {
    coord.handle_key_event_policed(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char,
        seq_char: 0,
    })
}

/// 出厂 `DeleteReplace` 方案 + 宿主读不回文档：`。` → 字母 `ni`（有候选）→ `.`。
///
/// `prev_char == 0` 是 press2 判定给「宿主读不回文档」留的兜底口，光标前字符对照因此失效；
/// 此时光标在组合区里，若返回 `ReplaceBackward{1, "."}` 删掉的就是候选本身。
#[test]
fn intervening_letters_do_not_trigger_press2() {
    if !has_data() {
        return;
    }
    let mut cfg = cfg_smart();
    // 标点顶码上屏：出厂 `schema.codetable.punct_commit = false` 会让这一按变成吞键
    // （`Consumed`），下面的正面断言就无从谈起。本用例要看的正是普通标点流程那条路。
    cfg.schema.codetable.punct_commit = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let a1 = press_policed(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(
        inserted(&a1),
        Some("。"),
        "press1 应出中文句号，实际: {:?}",
        a1
    );
    let an = press_policed(&coord, VK_N, 0);
    assert!(
        matches!(an, KeyAction::UpdateComposition { .. }),
        "字母应进编码缓冲形成组合，实际: {:?}",
        an
    );
    press_policed(&coord, VK_I, 0);
    let a2 = press_policed(&coord, VK_OEM_PERIOD, 0);
    assert!(
        replaced(&a2).is_none(),
        "中间打过编码 ⇒ 这一按不是 press2，替换会削掉组合区里的候选，实际: {:?}",
        a2
    );
    // 反面断言不够强：`PassThrough` / `Consumed` / `ClearComposition` 也满足它。这一按该落普通
    // 标点流程（顶屏候选 + 追加中文句号），不钉住正面产物，变成吞键时上面那条照样绿。
    assert!(
        inserted(&a2).is_some_and(|t| t.ends_with('。')),
        "应落普通标点流程：顶屏候选后追加中文句号，实际: {:?}",
        a2
    );
}

/// 同一场景，但宿主读得回文档：候选窗自显 preedit 时应用侧组合是「占位空格 + 光标置前」，
/// 读回的光标前一字符恰好是 press1 上屏的 `。`，与武装串末位匹配。
///
/// 与上一条是同一缺陷互不重叠的两条触发路径（绕过对照 / 对照本身成立），只修一条另一条照样复现。
#[test]
fn placeholder_preedit_intervening_letters_do_not_trigger_press2() {
    if !has_data() {
        return;
    }
    let mut cfg = cfg_smart();
    cfg.ui.candidate.preedit_display = "candidate_top".to_string();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let a1 = press_policed(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(
        inserted(&a1),
        Some("。"),
        "press1 应出中文句号，实际: {:?}",
        a1
    );
    let an = press_policed(&coord, VK_N, '。' as u16);
    assert!(
        matches!(&an, KeyAction::UpdateComposition { text, caret_pos } if text == " " && *caret_pos == 0),
        "候选窗自显 preedit 时应用侧组合应是占位空格 + 光标置前（本用例的前提），实际: {:?}",
        an
    );
    press_policed(&coord, VK_I, '。' as u16);
    let a2 = press_policed(&coord, VK_OEM_PERIOD, '。' as u16);
    assert!(
        replaced(&a2).is_none(),
        "光标前虽确是武装串 `。`，但那是占位组合前面的旧字符，不该判 press2，实际: {:?}",
        a2
    );
}

/// `HoldComposition` 方案下的同一场景：press2 发 `CommitReplacingHeld`，语义是覆盖当前组合，
/// 而组合此刻装的是拼音候选。
///
/// 出口处「C++ 已 flush 掉 hold」的清理按 `held_text.is_some()` 开门，HoldComposition 恰好满足；
/// 锁在这里防它被收窄（`DeleteReplace` 的 `held_text` 恒为 None）。
#[test]
fn hold_composition_intervening_letters_do_not_replace_composition() {
    if !has_data() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_hold(), Some(&data_dir()));
    let a1 = press_policed(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(
        held(&a1),
        Some("。"),
        "press1 应把中文句号挂进组合态，实际: {:?}",
        a1
    );
    press_policed(&coord, VK_N, 0);
    press_policed(&coord, VK_I, 0);
    let a2 = press_policed(&coord, VK_OEM_PERIOD, 0);
    assert!(
        !matches!(a2, KeyAction::CommitReplacingHeld { .. }),
        "组合里此刻是候选不是 held 符号，覆盖提交会把候选整条吃掉，实际: {:?}",
        a2
    );
}

/// 中间那一键不进编码缓冲时同样要解除武装——单独锁「非同键按键即失效」那道判据。
///
/// 本条走数字键（中文模式空缓冲下透传），缓冲前后都是空的，只有出口处那道判据能挡住；
/// 两道判据各有用例单独盯住，删任意一道都有红。
#[test]
fn intervening_passthrough_key_disarms() {
    let coord = Coordinator::new_headless(cfg_smart(), Some(&data_dir()));
    let a1 = press_policed(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(
        inserted(&a1),
        Some("。"),
        "press1 应出中文句号，实际: {:?}",
        a1
    );
    // 数字键透传，缓冲仍为空。「缓冲不留痕」是本用例的唯一前提：哪天数字键进了缓冲，
    // 本条会靠判据 1 继续绿，判据 2 就再无覆盖。
    let ad = press_policed(&coord, VK_1, '。' as u16);
    assert!(
        matches!(ad, KeyAction::PassThrough | KeyAction::NotHandled),
        "本用例前提是数字键透传、不进缓冲（否则判据 1 会顶替判据 2 让本条假绿），实际: {:?}",
        ad
    );
    // 宿主读不回文档（prev_char=0）：唯一还能挡住的就是「中间按过别的键」。
    let a2 = press_policed(&coord, VK_OEM_PERIOD, 0);
    assert!(
        replaced(&a2).is_none(),
        "中间按过别的键 ⇒ 光标前已不是 press1 那个符号，不该判 press2，实际: {:?}",
        a2
    );
}

/// 同一场景，移动端入口（`wind-mobile` 的 `MobileCore::key_down` 直接调内层
/// `Coordinator::handle_key_event`，见 `crates/wind-mobile/src/lib.rs`）。
///
/// 这条路不经 bridge 出口，「press1 之后又打了编码」是唯一防线，故刻意用非 policed 的 `press`。
#[test]
fn intervening_letters_do_not_trigger_press2_mobile_entry() {
    if !has_data() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_smart(), Some(&data_dir()));
    let a1 = press(&coord, VK_OEM_PERIOD, 0);
    assert_eq!(
        inserted(&a1),
        Some("。"),
        "press1 应出中文句号，实际: {:?}",
        a1
    );
    press(&coord, VK_N, 0);
    press(&coord, VK_I, 0);
    let a2 = press(&coord, VK_OEM_PERIOD, 0);
    assert!(
        replaced(&a2).is_none(),
        "移动端入口同样不得把组合区里的候选当成 press1 的符号删掉，实际: {:?}",
        a2
    );
}

/// 小键盘标点（英文模式 + 全角，经 `full_width_source_char` 吃下）的 press2 不得被
/// 「非同键按键解除武装」误伤：`punct_char` 不认小键盘 `.`（VK_DECIMAL），出口判据若只问它，
/// press1 会把自己刚武装的状态立刻解除。
#[test]
fn numpad_punct_press2_survives_the_disarm_guard() {
    const VK_DECIMAL: u32 = 0x6E;
    let mut cfg = cfg_en_mode();
    cfg.input.default.full_width = true; // 英文全角：小键盘键由 core 接手出字
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let a1 = press_policed(&coord, VK_DECIMAL, 0);
    assert_eq!(
        inserted(&a1),
        Some("．"),
        "英文全角 press1 应出全角句点，实际: {:?}",
        a1
    );
    let a2 = press_policed(&coord, VK_DECIMAL, '．' as u16);
    assert_eq!(
        replaced(&a2),
        Some((1, "。")),
        "同一个小键盘键的 press2 应照常换中文形，实际: {:?}",
        a2
    );
}

// ── 英文半角：中间输入被 DLL 直接透传，服务端靠 toggles 位得知 ─────────────────────
//
// 英文模式 + 半角下 TSF 只吃标点键（`_IsCustomEnglishPunctKey` → `IsPunctuationKey`），中间的
// 字母不产生按键事件，英文模式又没有编码缓冲，前两道判据同时失明：`.` → 快打 `abc` → `.`
// 会把 `c` 换成 `。`。DLL 用 `toggles` 的空闲位 `TOGGLE_PASSTHROUGH_KEY` 上报「上次按键之后
// 有键被透传」（不动 KeyPayload 布局），服务端只在这一位为真时解除武装。

/// C++ `TOGGLE_PASSTHROUGH_KEY` 的值。刻意写字面量而不是引常量——引了就会跟着一起漂，
/// 本组用例便再也证明不了「服务端读的确实是 DLL 写的那一位」。
/// 与 C++ 头文件的一致性另由 `wind-ipc/tests/toggle_bits_match_cpp.rs` 对账。
const TOGGLES_PASSTHROUGH: u8 = 0x08;

fn press_policed_toggles(coord: &Coordinator, vk: u32, prev_char: u16, toggles: u8) -> KeyAction {
    coord.handle_key_event_policed(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles,
        event_seq: 0,
        prev_char,
        seq_char: 0,
    })
}

/// 主用例：press1 与 press2 之间 DLL 报了「有键被透传」⇒ 不判 press2。
/// `prev_char = 0` 是宿主读不回文档那族（微信/终端），光标前字符那道对照在那里恒放行。
#[test]
fn english_mode_passthrough_flag_disarms_press2() {
    let coord = Coordinator::new_headless(cfg_en_mode(), Some(&data_dir()));
    let a1 = press_policed_toggles(&coord, VK_OEM_PERIOD, 0, 0);
    assert_eq!(
        inserted(&a1),
        Some("."),
        "英文模式 press1 应由 core 出英文句点，实际: {:?}",
        a1
    );
    // 用户快打 `abc`：DLL 全部透传，服务端一个事件都收不到，只在下一个标点事件上看到这一位。
    let a2 = press_policed_toggles(&coord, VK_OEM_PERIOD, 0, TOGGLES_PASSTHROUGH);
    assert!(
        replaced(&a2).is_none(),
        "中间有透传输入 ⇒ 不是 press2，替换会删掉用户刚打的字母，实际: {:?}",
        a2
    );
    assert_eq!(
        inserted(&a2),
        Some("."),
        "应落普通流程：照常出英文句点（并作为新的 press1 重新武装），实际: {:?}",
        a2
    );
}

/// 对照：同一时序、这一位为 0（没有透传或旧版 DLL 不报）⇒ press2 必须照常触发。
/// 新判据写成恒真时本条立刻红。
#[test]
fn english_mode_without_passthrough_flag_still_replaces() {
    let coord = Coordinator::new_headless(cfg_en_mode(), Some(&data_dir()));
    let a1 = press_policed_toggles(&coord, VK_OEM_PERIOD, 0, 0);
    assert_eq!(inserted(&a1), Some("."), "实际: {:?}", a1);
    let a2 = press_policed_toggles(&coord, VK_OEM_PERIOD, '.' as u16, 0);
    assert_eq!(
        replaced(&a2),
        Some((1, "。")),
        "没有透传 ⇒ press2 照常把英文句点换成中文句号，实际: {:?}",
        a2
    );
}

/// 这一位与 CapsLock 位互不干扰（同一个 `toggles` 字节，bit0 是锁定态、bit3 是透传事实）。
/// 搭车位最容易出的错就是掩码写错把两件事搅在一起：CapsLock 开着时 press2 不该因此失效。
#[test]
fn passthrough_flag_does_not_disturb_capslock_bit() {
    let coord = Coordinator::new_headless(cfg_en_mode(), Some(&data_dir()));
    let a1 = press_policed_toggles(&coord, VK_OEM_PERIOD, 0, 0x01);
    assert_eq!(
        inserted(&a1),
        Some("."),
        "CapsLock 开着照常出英文句点，实际: {:?}",
        a1
    );
    let a2 = press_policed_toggles(&coord, VK_OEM_PERIOD, '.' as u16, 0x01);
    assert_eq!(
        replaced(&a2),
        Some((1, "。")),
        "只开 CapsLock 位不该解除武装，实际: {:?}",
        a2
    );
}

/// keyup 带着这一位不得解除武装——钉住 C++「只在 keydown 那一发消费」的设计。
/// toggle 键（Shift/Ctrl/CapsLock）的 keyup 会发到服务端，若也消费该位，「中间按过 Shift」
/// 就会被吃掉，press2 又能误删一次。
#[test]
fn passthrough_flag_on_keyup_does_not_disarm() {
    let coord = Coordinator::new_headless(cfg_en_mode(), Some(&data_dir()));
    let a1 = press_policed_toggles(&coord, VK_OEM_PERIOD, 0, 0);
    assert_eq!(inserted(&a1), Some("."), "实际: {:?}", a1);
    // Shift 抬起（C++ 只对 toggle 键转发 keyup）。这一发带着位——但它不该被当成
    // 「press1 之后有过透传」而解除武装：事实的归属者是紧随其后的那个 keydown。
    coord.handle_key_event_policed(&KeyEventData {
        key_code: 0x10, // VK_SHIFT
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_UP,
        toggles: TOGGLES_PASSTHROUGH,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    });
    let a2 = press_policed_toggles(&coord, VK_OEM_PERIOD, '.' as u16, 0);
    assert_eq!(
        replaced(&a2),
        Some((1, "。")),
        "keyup 上的位不得解除武装，press2 应照常触发，实际: {:?}",
        a2
    );
}
