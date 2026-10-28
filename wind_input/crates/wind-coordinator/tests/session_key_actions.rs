//! 会话态按键功能表（`keys.session_actions`）的端到端测试。
//!
//! 覆盖两个用户诉求：**Tab 向下翻页**、**CapsLock 向上翻页**。
//! 设计见 `docs/design/session-key-actions.md`。
//!
//! ⚠️ 依赖词库的用例以 `if !has_schemas() { return; }` 开头——缺 `build_dev/data` 时会
//! **静默跳过且计数照绿**。判据是耗时：假绿 0.0x s，真跑约 1s+。缺数据先跑
//! `scripts/dev.ps1 gd`。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP, MOD_CAPSLOCK, MOD_SHIFT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    data_dir().join("schemas/wubi86.schema.toml").exists()
}

const VK_TAB: u32 = 0x09;
const VK_CAPITAL: u32 = 0x14;
const VK_A: u32 = 0x41;
const VK_Z: u32 = 0x5A;

fn key(vk: u32, modifiers: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

/// 配置：wubi86 + 指定的会话态绑定。
///
/// ⚠️ 显式写进 `session_actions` 的键**压过**默认 `page_keys`/`highlight_keys` 的折算
/// （见 `Config::migrate_nav_keys_into_session_actions`），所以这里写 `tab = page_next`
/// 就能覆盖出厂的 `tab = highlight_down`，不必先清空 `highlight_keys`。
fn cfg_with(binds: &[(&str, &str)]) -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    for (k, v) in binds {
        c.keys.session_actions.insert(k.to_string(), v.to_string());
    }
    c
}

/// 打出一串码并确认候选**多于一页**——否则翻页测试无从证伪（翻页在只有一页时是 no-op，
/// 断言「页码没变」会恒真）。
fn type_until_multipage(coord: &Coordinator) {
    coord.handle_key_event(&key(VK_A, 0, EVENT_KEY_DOWN));
    let (_, _, total) = coord.debug_page_info();
    assert!(
        total > 1,
        "样例码 'a' 应产出多页候选（实际 {total} 页）；否则翻页断言无法证伪，请换一个码"
    );
}

/// ★★ 跨 crate 键名表一致性。
///
/// `wind-config` 不能依赖 `wind-keys`（后者经 `wind-cmdbar` 反向依赖前者，加进去成环），
/// 于是会话态键名解析**存在两份实现**：`hotkey::session_key_to_vk`（决定键进不进 TSF
/// 转发白名单）与 `keymap::session_key_name_to_vk`（决定按下时查不查得到绑定）。
///
/// 两份漂移的后果是**静默半失效**：白名单里有、查表查不到 ⇒ 键被转发过来却什么也不做；
/// 反过来则是绑定配了永不触发。两种都没有任何报错。本仓已在跨仓 schema 契约上栽过同型的坑。
///
/// 本测试是这条契约**唯一**的编译期外守门——它所在的 crate 是唯一同时看得见两份实现的地方。
#[test]
fn session_key_tables_agree_across_crates() {
    let names = [
        "tab",
        "shift+tab",
        "capslock",
        "caps",
        "pageup",
        "pgup",
        "prior",
        "pagedown",
        "pgdn",
        "next",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "minus",
        "equal",
        "lbracket",
        "rbracket",
        "comma",
        "period",
        "semicolon",
        "quote",
        "slash",
        "backtick",
        "backslash",
        // 唯一收进会话态表的字母（见 `session_key_to_vk` 里那段「只收 z」的理由）。
        // 它在两处表里走的分支不同（wind-config 是 match 臂、wind-keys 是显式 if），
        // 正是最容易只改一边的形态。
        "z",
        // 纯修饰键：`select_key_groups = ["lrctrl"]` 折算后的形态。
        // ⚠️ 一期这几个只有 hotkey.rs 认得、wind-keys 漏了，而当时的键名列表恰好没覆盖
        // 到它们——**这条测试的覆盖面就是它的全部价值**，漏一个名字等于那个名字没被守。
        "lshift",
        "rshift",
        "lctrl",
        "lcontrol",
        "rctrl",
        "rcontrol",
        // 功能键：两处表各写一份，靠这里守住。
        "f1",
        "f2",
        "f9",
        "f12",
        "shift+f1",
    ];
    for name in names {
        let a = wind_config::hotkey::session_key_to_vk(name);
        let b = wind_keys::keymap::session_key_name_to_vk(name);
        match (a, b) {
            (Some((vk, shift)), Some(k)) => {
                assert_eq!(vk, k.vk, "键名 {name} 的 VK 两份解析不一致");
                assert_eq!(shift, k.shift, "键名 {name} 的 shift 两份解析不一致");
            }
            (a, b) => panic!("键名 {name} 只被其中一份认得：hotkey={a:?} keymap={b:?}"),
        }
    }
    // 反向：两份都必须拒绝同样的东西。ctrl/alt 组合归 `keys.key_actions`，不进本表。
    for bad in ["ctrl+tab", "alt+tab", "ctrl+shift+tab", "pgeup", ""] {
        assert!(
            wind_config::hotkey::session_key_to_vk(bad).is_none(),
            "hotkey 侧不该认得 {bad:?}"
        );
        assert!(
            wind_keys::keymap::session_key_name_to_vk(bad).is_none(),
            "keymap 侧不该认得 {bad:?}"
        );
    }
}

/// 诉求一：`tab = "page_next"` 后，有候选时按 Tab 翻到下一页。
#[test]
fn tab_pages_next_when_candidates_shown() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_with(&[("tab", "page_next")]), Some(&data_dir()));
    type_until_multipage(&coord);
    let (page0, _, _) = coord.debug_page_info();

    coord.handle_key_event(&key(VK_TAB, 0, EVENT_KEY_DOWN));
    let (page1, _, _) = coord.debug_page_info();
    assert_eq!(page1, page0 + 1, "Tab 应翻到下一页");
}

/// 对照组：不配 `tab = page_next` 时，Tab 走出厂默认（高亮下移），**页码不变**。
///
/// 没有这一条，上面那个用例在「Tab 本来就翻页」时也会绿——而出厂默认恰恰是高亮下移，
/// 两者的区别只在页码有没有动。
#[test]
fn tab_defaults_to_highlight_not_paging() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_with(&[]), Some(&data_dir()));
    type_until_multipage(&coord);
    let (page0, sel0, _) = coord.debug_page_info();

    coord.handle_key_event(&key(VK_TAB, 0, EVENT_KEY_DOWN));
    let (page1, sel1, _) = coord.debug_page_info();
    assert_eq!(page1, page0, "默认配置下 Tab 不该翻页");
    assert_ne!(sel1, sel0, "默认配置下 Tab 应移动高亮");
}

/// 用户明确要求的门控：**没配 `capslock` 就不装全局钩子**。
///
/// ★ 钩子是全局的（对所有进程的所有按键生效），装了就有代价：安全软件告警、系统超时后
/// 静默移除、闸门滞留会让别的应用 CapsLock 失灵。绝大多数用户不配这一项，他们的进程里
/// 就不该存在这个钩子——这是本功能唯一的风险控制手段。
///
/// 判据取**编译后的绑定表**而非原始配置串：动词/键名写错的条目已在 `ConfigBundle::build`
/// 里被剔除，那些情况装钩子纯属白担风险（用户的配置本来也不会生效）。
#[test]
fn capslock_hook_installs_only_when_bound() {
    let bare = Coordinator::new_headless(Config::default(), None);
    assert!(
        !bare.capslock_bound(),
        "默认配置不该装全局钩子——用户没要求这个功能却要承担全局钩子的代价"
    );

    let bound = Coordinator::new_headless(cfg_with(&[("capslock", "page_next")]), None);
    assert!(bound.capslock_bound(), "配了 capslock 才装钩子");

    // 动词写错 → 该绑定被 ConfigBundle 剔除 → 不该装钩子。
    let typo = Coordinator::new_headless(cfg_with(&[("capslock", "page_nextt")]), None);
    assert!(
        !typo.capslock_bound(),
        "动词无法识别时绑定不生效，此时装钩子是白担风险"
    );
}

/// 诉求二：`capslock = "page_prev"` 后，有候选时按 CapsLock 翻到上一页。
///
/// ⚠️ **本测试覆盖的不是真机主路径**。真机上有会话时 CapsLock 被服务进程的
/// `WH_KEYBOARD_LL` 钩子在 TSF 之前就吃掉了（TSF 根本收不到，见设计文档 §7），动作经
/// `handle_capslock_hook_press` 走同一个 `apply_session_action`。这里仍按 keyup 喂，
/// 测的是**动词到动作的映射**那一段；钩子本身跨进程且需 Win32 消息泵，只能真机验。
///
/// 保留它的价值：无会话时钩子放行，TSF 照常发 CapsLock keyup，这条路径依然真实存在。
#[test]
fn capslock_pages_prev_on_key_up() {
    if !has_schemas() {
        return;
    }
    let coord =
        Coordinator::new_headless(cfg_with(&[("capslock", "page_prev")]), Some(&data_dir()));
    type_until_multipage(&coord);
    // 先翻到第 2 页，否则「上一页」在首页是 no-op，断言无法证伪。
    coord.handle_key_event(&key(0x22 /* PageDown */, 0, EVENT_KEY_DOWN));
    let (page0, _, _) = coord.debug_page_info();
    assert_eq!(page0, 1, "前置条件：应已在第 2 页");

    coord.handle_key_event(&key(VK_CAPITAL, MOD_CAPSLOCK, EVENT_KEY_UP));
    let (page1, _, _) = coord.debug_page_info();
    assert_eq!(page1, 0, "CapsLock 的 keyup 应翻到上一页");
}

/// ★★ CapsLock 翻页**不得**吞掉正在打的编码。
///
/// 回归保护：keyup 分支里 CapsLock 的原有处理会调 `take_input_on_mode_switch`，把待输入
/// 上屏或丢弃。会话态绑定若排在它**之后**，用户每翻一页就毁一次输入，现象是「翻页时
/// 编码莫名没了」——极难联想到是大小写同步干的。
#[test]
fn capslock_paging_preserves_composition() {
    if !has_schemas() {
        return;
    }
    let coord =
        Coordinator::new_headless(cfg_with(&[("capslock", "page_prev")]), Some(&data_dir()));
    type_until_multipage(&coord);
    let before = coord.debug_page_texts();
    assert!(!before.is_empty(), "前置条件：应有候选");

    coord.handle_key_event(&key(VK_CAPITAL, MOD_CAPSLOCK, EVENT_KEY_UP));
    assert!(
        !coord.debug_page_texts().is_empty(),
        "翻页后候选不应消失——说明编码被 CapsLock 的状态同步分支吞了"
    );
}

/// ★ 无候选时 CapsLock 回落原语义（大小写状态同步），不被会话态绑定截走。
///
/// 「有会话归绑定、无会话归原语义」正是两张表的分野。判据落在这里而不是 C++ 侧：
/// 服务端拿到 keyup 后自己决定归谁，C++ 只负责转发。
#[test]
fn capslock_without_candidates_falls_back_to_state_sync() {
    if !has_schemas() {
        return;
    }
    let coord =
        Coordinator::new_headless(cfg_with(&[("capslock", "page_prev")]), Some(&data_dir()));
    // 不打任何码，直接按 CapsLock。
    // ⚠️ 判「无候选」要用 `debug_page_texts()` 而不是 `debug_page_info().2`——后者是
    // `total_pages`，空候选时返回 1（页数下限）而非 0。
    assert!(coord.debug_page_texts().is_empty(), "前置条件：不应有候选");

    let act = coord.handle_key_event(&key(VK_CAPITAL, MOD_CAPSLOCK, EVENT_KEY_UP));
    // 原语义会回一个状态更新；被会话态绑定截走则会是 Consumed。
    assert!(
        !matches!(act, wind_bridge::handler::KeyAction::Consumed),
        "无候选时 CapsLock 应落回状态同步，实际被绑定截走: {act:?}"
    );
}

/// ★★ 可达性：CapsLock 的绑定必须进 keyup 转发白名单。
///
/// 这是 keyup 类绑定**唯一**的可达性来源——不在白名单里，TSF 根本不发这个 keyup，
/// 绑定在配置里躺着但永远不触发。`key_actions` 四期正是漏了这一环（设计文档 §4.4 补充段）。
///
/// 断言取的是 `push_activation_status` 真正推给 C++ 的那份，不是旁路重算——用重算值
/// 断言等于没测。
#[test]
fn capslock_binding_enters_key_up_forward_set() {
    let coord = Coordinator::new_headless(cfg_with(&[("capslock", "page_prev")]), None);
    let want = (MOD_CAPSLOCK << 16) | VK_CAPITAL;
    let hashes = coord.debug_key_up_hotkeys();
    assert!(
        hashes.iter().any(|h| h & 0x0000_FFFF == VK_CAPITAL),
        "CapsLock 未进 keyup 转发集，绑定永不触发。实际: {hashes:02X?}"
    );
    // policy 位会叠加在高位，故比对时掩掉；raw 部分必须精确等于 (MOD_CAPSLOCK, VK_CAPITAL)。
    assert!(
        hashes.iter().any(|h| h & 0x01FF_FFFF == want),
        "CapsLock 的 keyup hash 应含 MOD_CAPSLOCK，否则 C++ 侧算出的 hash 对不上。实际: {hashes:02X?}"
    );
}

// ───────────────────────── 二期：cancel 动词与 Esc 收敛 ─────────────────────────

const VK_ESCAPE: u32 = 0x1B;
const VK_BACKTICK: u32 = 0xC0;
const VK_PERIOD: u32 = 0xBE;

fn press(coord: &Coordinator, ch: char) -> wind_bridge::handler::KeyAction {
    let vk = match ch {
        'a'..='z' => 0x41 + (ch as u32 - 'a' as u32),
        '.' => VK_PERIOD,
        _ => panic!("press() 未覆盖的字符: {ch}"),
    };
    coord.handle_key_event(&key(vk, 0, EVENT_KEY_DOWN))
}

/// 用户诉求三：`tab = "cancel"` 后，打字中途按 Tab 放弃整段（Esc 的替代键）。
#[test]
fn cancel_discards_composition_in_normal_input() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_with(&[("tab", "cancel")]), Some(&data_dir()));
    type_until_multipage(&coord);
    assert!(!coord.debug_page_texts().is_empty(), "前置条件：应有候选");

    let act = coord.handle_key_event(&key(VK_TAB, 0, EVENT_KEY_DOWN));
    assert!(
        matches!(act, wind_bridge::handler::KeyAction::ClearComposition),
        "Tab 绑 cancel 后应放弃整段，实际: {act:?}"
    );
    assert!(coord.debug_page_texts().is_empty(), "候选应被清空");
}

/// `clear` 是 `cancel` 的别名——用户按「清空」的心智去写照样能用。
#[test]
fn clear_is_an_alias_of_cancel() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_with(&[("tab", "clear")]), Some(&data_dir()));
    type_until_multipage(&coord);
    let act = coord.handle_key_event(&key(VK_TAB, 0, EVENT_KEY_DOWN));
    assert!(
        matches!(act, wind_bridge::handler::KeyAction::ClearComposition),
        "clear 应与 cancel 同义，实际: {act:?}"
    );
}

/// ★★★ 判据放宽的守门：**有会话但无候选**时 `cancel` 仍须生效。
///
/// 一期的守卫是「无候选就返回 None」，那一格里 Tab 根本不会被接管。网址模式是这一格的
/// 天然样本——它原样累积文本、从不产候选，而用户此刻显然处在一个输入会话里。
///
/// 这条测试若失败，说明 `requires_candidates` 的分派被改回了「一刀切守候选」。
#[test]
fn cancel_works_in_session_without_candidates() {
    if !has_schemas() {
        return;
    }
    // ⚠️ `input.url.enabled` **出厂默认关闭**，测试须显式拨开——否则 `www.` 只是普通编码，
    // 前置条件不成立。这类「判据依赖一个默认关闭的开关」正是集成测试假绿的常见来源。
    let mut cfg = cfg_with(&[("tab", "cancel")]);
    cfg.input.url.enabled = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "www.".chars() {
        press(&coord, c);
    }
    assert_eq!(
        coord.debug_active_mode(),
        Some("url"),
        "前置条件：`www.` 应夺取进网址模式"
    );
    assert!(
        coord.debug_page_texts().is_empty(),
        "前置条件：网址模式不产候选——这正是本测试要覆盖的那一格"
    );

    coord.handle_key_event(&key(VK_TAB, 0, EVENT_KEY_DOWN));
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "无候选时 cancel 仍须退出网址模式"
    );
}

/// `cancel` 在 overlay 模式里等同 Esc：退出模式并放弃内容。
///
/// ⚠️ 必须先断言**确实进了临拼**：触发键若没生效，按键会落到主输入路径，而那里的
/// cancel 返回的 `ClearComposition` 与这里一模一样——不验进入就是假绿。
#[test]
fn cancel_exits_overlay_mode() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_with(&[("tab", "cancel")]), Some(&data_dir()));
    coord.handle_key_event(&key(VK_BACKTICK, 0, EVENT_KEY_DOWN));
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_pinyin"),
        "前置条件：反引号应进入临时拼音"
    );
    for c in "ni".chars() {
        press(&coord, c);
    }

    coord.handle_key_event(&key(VK_TAB, 0, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_active_mode(), None, "cancel 应退出临时拼音");
}

/// ★ 无会话时 Tab 不被截走——空闲按 Tab 该是宿主的制表符。
///
/// 「有会话归绑定、无会话归原语义」是两张表的分野，这条守的就是那道边界。
#[test]
fn cancel_not_triggered_without_session() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_with(&[("tab", "cancel")]), Some(&data_dir()));
    assert!(coord.debug_page_texts().is_empty(), "前置条件：不应有候选");

    let act = coord.handle_key_event(&key(VK_TAB, 0, EVENT_KEY_DOWN));
    assert!(
        !matches!(act, wind_bridge::handler::KeyAction::ClearComposition),
        "空闲时 Tab 不该被 cancel 截走，实际: {act:?}"
    );
}

/// ★★ Esc 收敛的回归保护：六处实现合并成 `cancel_session` 后，各模式行为不变。
///
/// ⚠️ **每个模式都必须先断言进入**。六处 Esc 的返回值本来就都是 `ClearComposition`，
/// 触发键没生效、按键落回主输入路径时返回值完全相同——不验进入，这条测试在收敛写错的
/// 情况下照样全绿。这与 `enter_behavior` 那次的假绿是同一个形状。
#[test]
fn escape_still_exits_each_mode_after_convergence() {
    if !has_schemas() {
        return;
    }
    // 临时拼音
    let coord = Coordinator::new_headless(cfg_with(&[]), Some(&data_dir()));
    coord.handle_key_event(&key(VK_BACKTICK, 0, EVENT_KEY_DOWN));
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_pinyin"),
        "前置条件：应进临拼"
    );
    press(&coord, 'n');
    coord.handle_key_event(&key(VK_ESCAPE, 0, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_active_mode(), None, "Esc 应退出临拼");

    // 网址模式（总开关出厂默认关闭，须显式拨开）
    let mut cfg = cfg_with(&[]);
    cfg.input.url.enabled = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "www.".chars() {
        press(&coord, c);
    }
    assert_eq!(
        coord.debug_active_mode(),
        Some("url"),
        "前置条件：应进网址模式"
    );
    coord.handle_key_event(&key(VK_ESCAPE, 0, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_active_mode(), None, "Esc 应退出网址模式");

    // 主输入路径
    let coord = Coordinator::new_headless(cfg_with(&[]), Some(&data_dir()));
    type_until_multipage(&coord);
    let act = coord.handle_key_event(&key(VK_ESCAPE, 0, EVENT_KEY_DOWN));
    assert!(
        matches!(act, wind_bridge::handler::KeyAction::ClearComposition),
        "Esc 在主输入路径应放弃整段，实际: {act:?}"
    );
    assert!(coord.debug_page_texts().is_empty(), "候选应被清空");
}

// ───────────────────────── 三期：选词键 / 以词定字键收编 ─────────────────────────

const VK_SEMICOLON: u32 = 0xBA;
const VK_LCONTROL: u32 = 0xA2;

/// ★★ 两张表的核心场景：**同一个键在两种状态下是两个动作**。
///
/// `;` 出厂既是快捷输入的引导键（`keys.key_actions`，无会话时）又是次选键
/// （`keys.session_actions`，有会话时）。这不是冲突，正是分表的理由——若两者合成一张表，
/// 这个配置根本表达不出来。
#[test]
fn semicolon_is_mode_trigger_when_idle_and_select_key_when_composing() {
    if !has_schemas() {
        return;
    }
    // 空闲态：`;` 进快捷输入。
    let coord = Coordinator::new_headless(cfg_with(&[]), Some(&data_dir()));
    let act = coord.handle_key_event(&key(VK_SEMICOLON, 0, EVENT_KEY_DOWN));
    assert!(
        matches!(
            act,
            wind_bridge::handler::KeyAction::UpdateComposition { .. }
        ),
        "空闲时 `;` 应进快捷输入（无会话态归 key_actions），实际: {act:?}"
    );

    // 组合态：`;` 选次选。
    let coord = Coordinator::new_headless(cfg_with(&[]), Some(&data_dir()));
    type_until_multipage(&coord);
    let page = coord.debug_page_texts();
    assert!(page.len() >= 2, "前置条件：当前页应有至少两个候选");
    let second = page[1].clone();

    let act = coord.handle_key_event(&key(VK_SEMICOLON, 0, EVENT_KEY_DOWN));
    match act {
        wind_bridge::handler::KeyAction::InsertText { text, .. } => assert_eq!(
            text, second,
            "有候选时 `;` 应选中次选（有会话态归 session_actions）"
        ),
        other => panic!("`;` 应上屏次选，实际: {other:?}"),
    }
}

/// ★ 修饰键选词键（`select_key_groups = ["lrctrl"]` 的折算形态）必须进 keyup 转发集。
///
/// 这是 keyup 类绑定唯一的可达性来源——不在白名单里，TSF 根本不发这个 keyup。三期把
/// 编译入口从 `compile_select_modifier_group` 换成了统一的 `session_actions` 段，本测试
/// 守的就是「换了入口之后这批键仍然到得了」。
#[test]
fn modifier_select_key_reaches_key_up_forward_set() {
    let mut cfg = cfg_with(&[]);
    cfg.keys.select_key_groups = vec!["lrctrl".into()];
    let coord = Coordinator::new_headless(cfg, None);
    let hashes = coord.debug_key_up_hotkeys();
    assert!(
        hashes.iter().any(|h| h & 0x0000_FFFF == VK_LCONTROL),
        "左 Ctrl 未进 keyup 转发集，修饰键选词永不触发。实际: {hashes:02X?}"
    );
}

/// 修饰键选词端到端：有候选时轻敲左 Ctrl 选次选。
///
/// ⚠️ 走 **keyup**——纯修饰键的 keydown 一律放行（吃掉会让 AutoCAD 看不到修饰键）。
#[test]
fn modifier_select_key_picks_second_candidate_on_key_up() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg_with(&[]);
    cfg.keys.select_key_groups = vec!["lrctrl".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    type_until_multipage(&coord);
    let page = coord.debug_page_texts();
    assert!(page.len() >= 2, "前置条件：当前页应有至少两个候选");
    let second = page[1].clone();

    let act = coord.handle_key_event(&key(VK_LCONTROL, 0, EVENT_KEY_UP));
    match act {
        wind_bridge::handler::KeyAction::InsertText { text, .. } => {
            assert_eq!(text, second, "左 Ctrl 的 keyup 应选中次选")
        }
        other => panic!("修饰键选词应上屏次选，实际: {other:?}"),
    }
}

/// z 是会话态表里唯一收进来的字母：**有候选时**按下即执行动作。
#[test]
fn z_selects_third_candidate_when_candidates_shown() {
    if !has_schemas() {
        return;
    }
    let coord =
        Coordinator::new_headless(cfg_with(&[("z", "select_candidate:3")]), Some(&data_dir()));
    type_until_multipage(&coord);
    let page = coord.debug_page_texts();
    assert!(page.len() >= 3, "前置条件：当前页应有至少三个候选");
    let third = page[2].clone();

    let act = coord.handle_key_event(&key(VK_Z, 0, EVENT_KEY_DOWN));
    match act {
        wind_bridge::handler::KeyAction::InsertText { text, .. } => {
            assert_eq!(text, third, "有候选时按 z 应选中第 3 个候选")
        }
        other => panic!("z 应上屏第三候选，实际: {other:?}"),
    }
}

/// ★★ **空缓冲按 z 必须照常起头组码**，不能被会话态绑定夺走。
///
/// 这是「只加 z」这个决定能成立的**唯一前提**：导航/选词类动词带
/// `requires_candidates` 守卫，无候选时 `apply_session_action` 返回 `None`，z 落回大
/// match 的字母臂。这条一旦破了，配过 z 的用户在该方案里再也起不了以 z 开头的码，
/// 而症状是「按 z 什么都没发生」——与键盘坏了同形。
///
/// 对照组不可省：若 `type_until_multipage` 之外 z 本来就打不出候选，上面那个用例
/// 也会「绿」，因为它只断言了按下之后的事。
#[test]
fn z_still_starts_composition_when_buffer_empty() {
    if !has_schemas() {
        return;
    }
    let coord =
        Coordinator::new_headless(cfg_with(&[("z", "select_candidate:3")]), Some(&data_dir()));
    // 空缓冲直接按 z：应进组码（有编码 ⇒ 出候选），而不是被当成选词键吞掉。
    coord.handle_key_event(&key(VK_Z, 0, EVENT_KEY_DOWN));
    let (_, _, total) = coord.debug_page_info();
    assert!(
        total > 0,
        "空缓冲按 z 应照常起头组码并出候选，实际候选数 {total}"
    );
}

/// 以词定字折算后仍认 `brackets`——该组**不在**选词键组的值域里。
///
/// 回归点：两组曾被张冠李戴（用选词键组的解析器解以词定字配置），`brackets` 静默失效。
/// 收编后两者靠**动词**区分而非靠解析器区分，那类错配从结构上消失了。
#[test]
fn select_char_brackets_still_work_after_fold() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg_with(&[]);
    cfg.keys.select_char_keys = vec!["brackets".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    type_until_multipage(&coord);
    let first = coord.debug_page_texts()[0].clone();
    let want_first_char: String = first.chars().take(1).collect();

    // `[` = VK_OEM_4 取第 1 字
    let act = coord.handle_key_event(&key(0xDB, 0, EVENT_KEY_DOWN));
    match act {
        wind_bridge::handler::KeyAction::InsertText { text, .. } => {
            assert_eq!(text, want_first_char, "`[` 应取高亮候选的第 1 字")
        }
        other => panic!("以词定字应上屏单字，实际: {other:?}"),
    }
}

/// Shift+Tab 与 Tab 是两条独立绑定（`shift+` 前缀），不会互相顶掉。
#[test]
fn shift_tab_and_tab_bind_independently() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        cfg_with(&[("tab", "page_next"), ("shift+tab", "page_prev")]),
        Some(&data_dir()),
    );
    type_until_multipage(&coord);

    coord.handle_key_event(&key(VK_TAB, 0, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_page_info().0, 1, "Tab 应翻到第 2 页");
    coord.handle_key_event(&key(VK_TAB, MOD_SHIFT, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_page_info().0, 0, "Shift+Tab 应翻回第 1 页");
}

// ———————————————— 候选反转时的高亮走向 ————————————————

const VK_UP: u32 = 0x26;
const VK_DOWN: u32 = 0x28;
const VK_NEXT: u32 = 0x22; // PageDown

/// 候选被反转排列时（竖排 + 上翻 + `ui.candidate.flip_when_above`），高亮按**屏幕方向**走。
///
/// 反转后屏幕从上到下是候选 n..1，此时 ↑ 指向的是候选序的「下一个」。判据本身只有 UI 侧
/// 算得出（要窗口尺寸 + 屏幕工作区），协调器只镜像 `UiEvent::CandidateFlipped`，故这里
/// 用 `debug_set_candidate_flipped` 走同一条分发。
#[test]
fn highlight_follows_visual_direction_when_flipped() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_with(&[]), Some(&data_dir()));
    type_until_multipage(&coord);

    // 基线：未反转时 ↓ = 候选序下一个，↑ 退回。
    coord.handle_key_event(&key(VK_DOWN, 0, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_page_info().1, 1, "未反转时 ↓ 应前进到候选 2");
    coord.handle_key_event(&key(VK_UP, 0, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_page_info().1, 0, "未反转时 ↑ 应退回候选 1");

    // 反转后两个方向对调。
    coord.debug_set_candidate_flipped(true);
    coord.handle_key_event(&key(VK_UP, 0, EVENT_KEY_DOWN));
    assert_eq!(
        coord.debug_page_info().1,
        1,
        "反转后候选 2 显示在候选 1 上方，↑ 应走向候选 2"
    );
    coord.handle_key_event(&key(VK_DOWN, 0, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_page_info().1, 0, "反转后 ↓ 应走回候选 1");

    // 上报回落后必须恢复原走向——否则窗口翻回下方时方向会一直反着。
    coord.debug_set_candidate_flipped(false);
    coord.handle_key_event(&key(VK_DOWN, 0, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_page_info().1, 1, "回落为未反转后 ↓ 应重新前进");
}

/// Tab / Shift+Tab 与 ↑↓ 同属 `highlight_up`/`highlight_down`，反转时**一并翻转**。
///
/// 这是刻意选定的行为：两组键绑在同一对动作上，只翻其中一组会让同一个动作出现两种走向。
#[test]
fn flip_also_applies_to_tab_bound_highlight() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_with(&[]), Some(&data_dir()));
    type_until_multipage(&coord);

    // 出厂默认 tab = highlight_down / shift+tab = highlight_up。
    coord.debug_set_candidate_flipped(true);
    coord.handle_key_event(&key(VK_TAB, MOD_SHIFT, EVENT_KEY_DOWN));
    assert_eq!(
        coord.debug_page_info().1,
        1,
        "反转后 Shift+Tab（highlight_up）应走向候选 2"
    );
    coord.handle_key_event(&key(VK_TAB, 0, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_page_info().1, 0, "反转后 Tab 应走回候选 1");
}

/// 反向对照：**翻页键不受反转影响**。
///
/// 缺了这条，「一律把四个 NavAction 全部对调」的实现也能让上面两条通过。页与页之间没有
/// 空间关系（新页在原处整体替换），反转只发生在页内。
#[test]
fn paging_direction_unaffected_by_flip() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(cfg_with(&[]), Some(&data_dir()));
    type_until_multipage(&coord);
    coord.debug_set_candidate_flipped(true);

    coord.handle_key_event(&key(VK_NEXT, 0, EVENT_KEY_DOWN));
    assert_eq!(
        coord.debug_page_info().0,
        1,
        "反转下 PageDown 仍应翻到下一页"
    );
}
