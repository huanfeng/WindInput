//! 会话态动词 `commit_code`（上屏编码）的端到端测试。
//!
//! 它借回车的臂上屏编码，所以断言的形状是**对拍**：同一段输入，一边按回车、一边按绑了本
//! 动词的键，按键结果与收尾模式必须逐字相同。唯一的例外是 `input.enter_behavior = "clear"`：
//! 回车清空，本动词照样上屏编码。
//!
//! 走 `handle_key_event_policed`：重新分派挂在那个出口上，内层 `handle_key_event` 只登记。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP};

const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_LCONTROL: u32 = 0xA2;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
}

fn ev(k: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn press(coord: &Coordinator, k: u32) -> KeyAction {
    coord.handle_key_event_policed(&ev(k, EVENT_KEY_DOWN))
}

fn open_with(edit: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    // 五笔里 z 进临拼，好验 overlay 那条回车臂。
    c.input.temp_pinyin.enabled = true;
    c.schema.codetable.z_key_action = "temp_pinyin".into();
    edit(&mut c);
    Coordinator::new_headless_with_override(c, Some(&data_dir()), None)
}

fn bind(c: &mut Config, key: &str) {
    c.keys
        .session_actions
        .insert(key.into(), "commit_code".into());
}

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        press(coord, (ch.to_ascii_uppercase() as u32) & 0xFF);
    }
}

fn committed(act: &KeyAction) -> Option<String> {
    match act {
        KeyAction::InsertText { text, .. } => Some(text.clone()),
        _ => None,
    }
}

/// 两个实例走同一段输入，一个按回车、一个按 Tab，结果逐字相同。
fn assert_same_as_enter(edit: impl Fn(&mut Config), input: &str, mode: Option<&str>) -> String {
    let enter = open_with(&edit);
    let tab = open_with(|c| {
        edit(c);
        bind(c, "tab");
    });
    for coord in [&enter, &tab] {
        type_str(coord, input);
        assert_eq!(coord.debug_active_mode(), mode, "前提：应处在 {mode:?}");
    }
    let by_enter = press(&enter, VK_RETURN);
    let by_tab = press(&tab, VK_TAB);
    assert_eq!(
        format!("{by_tab:?}"),
        format!("{by_enter:?}"),
        "应与回车逐字相同"
    );
    assert_eq!(tab.debug_active_mode(), enter.debug_active_mode());
    committed(&by_tab).expect("应上屏")
}

#[test]
fn tab_commits_the_code_like_enter() {
    skip_without_data!();
    assert_eq!(assert_same_as_enter(|_| {}, "wq", None), "wq");
}

/// 临拼里同样借的是临拼自己那条回车臂。
#[test]
fn tab_commits_temp_pinyin_like_enter() {
    skip_without_data!();
    let text = assert_same_as_enter(|_| {}, "znihao", Some("temp_pinyin"));
    assert!(text.contains("nihao"), "应上屏拼音原码，实得 {text:?}");
}

/// ★ 回车设成「清空编码」时本动词仍上屏编码：那是回车这个键的配置。
#[test]
fn enter_behavior_clear_does_not_apply() {
    skip_without_data!();
    for (input, want) in [("wq", "wq"), ("znihao", "nihao")] {
        let c = open_with(|c| {
            c.input.enter_behavior = "clear".into();
            bind(c, "tab");
        });
        type_str(&c, input);
        let text = committed(&press(&c, VK_TAB)).expect("应上屏");
        assert!(text.contains(want), "{input}：实得 {text:?}");

        // 对照：同一配置下回车确实是清空，上面那条不是碰巧。
        let c = open_with(|c| c.input.enter_behavior = "clear".into());
        type_str(&c, input);
        assert!(matches!(press(&c, VK_RETURN), KeyAction::ClearComposition));
    }
    // 重新分派那一下的强制位不能漏到之后的回车上。
    let c = open_with(|c| {
        c.input.enter_behavior = "clear".into();
        bind(c, "tab");
    });
    type_str(&c, "wq");
    press(&c, VK_TAB);
    type_str(&c, "wq");
    assert!(
        matches!(press(&c, VK_RETURN), KeyAction::ClearComposition),
        "之后的回车应仍按配置清空"
    );
}

/// 极点 / 小鸭的习惯：单按左 Ctrl 上屏编码。修饰键只有 keyup 到得了服务端。
#[test]
fn left_ctrl_tap_commits_the_code() {
    skip_without_data!();
    let c = open_with(|c| bind(c, "lctrl"));
    type_str(&c, "wq");
    let act = c.handle_key_event_policed(&ev(VK_LCONTROL, EVENT_KEY_UP));
    assert_eq!(committed(&act).as_deref(), Some("wq"));
    assert!(c.debug_page_texts().is_empty(), "上屏后候选应清空");
}

/// 没在打字时不吞键：Tab 仍是宿主的制表符。
#[test]
fn idle_key_passes_through() {
    skip_without_data!();
    let c = open_with(|c| bind(c, "tab"));
    assert!(matches!(press(&c, VK_TAB), KeyAction::PassThrough));
}
