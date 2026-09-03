//! 协调器输入流程端到端测试
//!
//! 覆盖基础功能目标：五笔/拼音基本输入流程 + 方案切换 + 中英切换。
//! 使用 `Coordinator::new_headless`（不启动 Win32 UI 线程），通过模拟按键事件
//! 断言返回的 `KeyAction`，验证整条"字母累积 → 候选 → 选词上屏"链路。
//!
//! 词典缺失时自动跳过（无数据 CI 环境）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP};
use wind_webdata::WebDataRpc;

fn data_dir() -> PathBuf {
    // 三级：crates/wind-coordinator → crates → wind_input → 仓库根（build_dev 在仓库根）。
    // 曾误写成两级，解析到 wind_input/build_dev/data —— 该目录不存在，于是下面的
    // exists() 判假、整个测试族静默走「跳过」分支通过。**判据是耗时 0.00s**。
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    let ok = |id: &str| {
        d.join(format!("schemas/{}.schema.toml", id)).exists()
            || d.join(format!("schemas/{}.schema.yaml", id)).exists()
    };
    ok("wubi86") && ok("pinyin")
}

/// 让指定方案获得 `[overlay]` 段（= 成为 overlay 方案），返回可传给构造函数的 override 目录。
///
/// 特殊模式的实例集合已是「带 `[overlay]` 段的已安装方案」，不再是 `schema.special_modes`
/// 数组。测试不能往真实 `data/schemas` 写文件，故走 override 层——`read_schema` 的
/// `merge_toml` 会把它合并进方案，效果等同方案自带该段，真实词库分毫不动。
fn overlay_override_dir(tag: &str, schemas: &[(&str, bool)]) -> PathBuf {
    overlay_override_dir_with_codetable(tag, schemas, "")
}

/// 同 [`overlay_override_dir`]，但额外给每个方案写一段 `[engine.codetable]` 覆盖。
///
/// ★ **overlay 方案不继承全局 `schema.codetable`**（`EngineManager::codetable_baseline`
/// 按 `[overlay]` 段存在与否分流，取内置基线）。所以想让它按某个码表行为跑，必须写在
/// **方案自己名下**——拨 `cfg.schema.codetable.*` 对它完全无效，而那样写出来的测试
/// 会以「行为没生效」的形态失败，很容易被误读成功能坏了。
fn overlay_override_dir_with_codetable(
    tag: &str,
    schemas: &[(&str, bool)],
    codetable: &str,
) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_overlay_ov_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (id, show_all) in schemas {
        let ct = if codetable.is_empty() {
            String::new()
        } else {
            format!("[engine.codetable]\n{codetable}")
        };
        std::fs::write(
            dir.join(format!("{id}.toml")),
            format!("[overlay]\nkind = \"special\"\nshow_all_on_enter = {show_all}\n{ct}"),
        )
        .unwrap();
    }
    dir
}

/// 把某个键绑成「进入该 overlay 方案」。
///
/// 引导键的落点是 `keys.key_actions`（五c 收编），`special:<id>` 里的 `<id>` 现在
/// 就是**方案 id**——实例即方案，不再有指向别处的实例别名。
fn bind_special(cfg: &mut Config, key: &str, schema_id: &str) {
    cfg.keys
        .key_actions
        .insert(key.into(), format!("special:{schema_id}"));
}

fn config_with(active: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = active.into();
    cfg.input.default.chinese_mode = true;
    cfg.keys.toggle_mode_keys = vec!["lshift".into(), "rshift".into()];
    cfg.keys.switch_engine = "ctrl+shift+e".into();
    cfg
}

fn key_event(key_code: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn key_event_mods(key_code: u32, event_type: u8, modifiers: u32) -> KeyEventData {
    KeyEventData {
        modifiers,
        ..key_event(key_code, event_type)
    }
}

/// 按下一个字母键（vk = ASCII 大写）
fn press_letter(coord: &Coordinator, c: char) -> KeyAction {
    let vk = (c.to_ascii_uppercase() as u32) & 0xFF;
    coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN))
}

fn action_text(action: &KeyAction) -> Option<String> {
    match action {
        KeyAction::UpdateComposition { text, .. } => Some(text.clone()),
        KeyAction::InsertText { text, .. } => Some(text.clone()),
        _ => None,
    }
}

#[test]
fn test_wubi_basic_input_and_commit() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    assert_eq!(coord.active_schema_id(), "wubi86");
    assert!(coord.is_chinese_mode());

    // 累积 "aaaa"
    let mut last = KeyAction::PassThrough;
    for c in ['a', 'a', 'a', 'a'] {
        last = press_letter(&coord, c);
    }
    let preedit = action_text(&last).expect("应返回 UpdateComposition");
    // 组合区只显示编码，不含候选列表（候选在候选窗口）
    assert_eq!(preedit, "aaaa", "五笔组合区应只显示编码，实际: {}", preedit);

    // 空格上屏首选
    let commit = coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN));
    match commit {
        KeyAction::InsertText { text, .. } => {
            assert!(!text.is_empty(), "上屏文本应非空");
            // 同 test_mixed_wubi_exact_priority：`aaaa` 是 gen_dict `[protected_codes]`
            // 保护码，组内次序由上游给定（上游把键名汉字「工」放首位，补权时代则是
            // 「恭恭敬敬」在前）。本用例要钉的是「空格上屏首选」这条通路，具体是哪条
            // 五笔词条属实现细节，写死会在词库重新生成的前后各红一次。
            assert!(
                matches!(text.as_str(), "工" | "恭恭敬敬"),
                "空格应上屏五笔首选（aaaa 的五笔候选为 工/恭恭敬敬），实际: {}",
                text
            );
        }
        other => panic!("空格应上屏 InsertText，实际: {:?}", other),
    }
}

#[test]
fn test_wubi_number_select() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // "a" → 组合区显示编码 "a"，候选在候选窗口
    let act = press_letter(&coord, 'a');
    let preedit = action_text(&act).unwrap();
    assert_eq!(preedit, "a", "组合区应只显示编码 a，实际: {}", preedit);

    // 数字键 2 选第二个候选
    let commit = coord.handle_key_event(&key_event(0x32, EVENT_KEY_DOWN));
    match commit {
        KeyAction::InsertText { text, .. } => assert!(!text.is_empty()),
        other => panic!("数字键应上屏，实际: {:?}", other),
    }
}

#[test]
fn test_url_mode_enter_and_commit() {
    if !has_schemas() {
        return;
    }
    // #11 网址输入：打满前缀 "www." 夺取进入网址模式，续打累积，空格上屏原文。
    let mut cfg = config_with("wubi86");
    cfg.input.url.enabled = true;
    cfg.input.url.prefixes = vec!["www.".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // w w w . → 进入网址模式（最后一键补满前缀）
    press_letter(&coord, 'w');
    press_letter(&coord, 'w');
    press_letter(&coord, 'w');
    let enter = coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)); // VK_OEM_PERIOD '.'
    match &enter {
        KeyAction::UpdateComposition { text, .. } => {
            assert_eq!(text, "www.", "进入网址模式组合区应为 www.，实际: {}", text);
        }
        other => panic!(
            "打满 www. 应进入网址模式(UpdateComposition)，实际: {:?}",
            other
        ),
    }

    // 续打 g o → 缓冲累积（网址字符不上屏）
    press_letter(&coord, 'g');
    let acc = press_letter(&coord, 'o');
    match &acc {
        KeyAction::UpdateComposition { text, .. } => {
            assert_eq!(text, "www.go", "网址续打应累积，实际: {}", text);
        }
        other => panic!("网址续打应 UpdateComposition，实际: {:?}", other),
    }

    // 空格上屏原文
    let commit = coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN));
    match commit {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "www.go", "网址空格应上屏原文，实际: {}", text);
        }
        other => panic!("网址空格应上屏 InsertText，实际: {:?}", other),
    }
}

#[test]
fn test_pin_candidate_hotkey_consumed_and_gated() {
    if !has_schemas() {
        return;
    }
    use wind_ipc::protocol::MOD_CTRL;
    // #12 候选热键：默认 pin=ctrl+number。有候选+有输入码时 Ctrl+2 消费按键（置顶第2候选）；
    // 无组合时 Ctrl+2 不应被当作候选热键吞掉（透传给应用）。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));

    // 有组合：输入 "aaaa" 产生候选，Ctrl+2 → Consumed
    for c in ['a', 'a', 'a', 'a'] {
        press_letter(&coord, c);
    }
    let pin = coord.handle_key_event(&key_event_mods(0x32, EVENT_KEY_DOWN, MOD_CTRL));
    assert!(
        matches!(pin, KeyAction::Consumed),
        "有候选时 Ctrl+2 应被候选热键消费，实际: {:?}",
        pin
    );

    // Ctrl+0 → 第 10 候选（候选窗最大 10 项），同样应被消费（范围校验在 candidate_op 内）。
    let pin0 = coord.handle_key_event(&key_event_mods(0x30, EVENT_KEY_DOWN, MOD_CTRL));
    assert!(
        matches!(pin0, KeyAction::Consumed),
        "Ctrl+0 应作为第 10 候选热键被消费，实际: {:?}",
        pin0
    );

    // 无组合：另起 coordinator，未输入任何码，Ctrl+2 → 不消费（PassThrough）
    let coord2 = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    let no_comp = coord2.handle_key_event(&key_event_mods(0x32, EVENT_KEY_DOWN, MOD_CTRL));
    assert!(
        matches!(no_comp, KeyAction::PassThrough),
        "无组合时 Ctrl+2 不应被候选热键吞掉，实际: {:?}",
        no_comp
    );
}

#[test]
fn test_delete_candidate_hotkey_shift_gating() {
    if !has_schemas() {
        return;
    }
    use wind_ipc::protocol::{MOD_CTRL, MOD_SHIFT};
    // 默认 delete=ctrl+shift+number。有候选时 Ctrl+Shift+3 消费（删除第3候选）。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    for c in ['a', 'a', 'a', 'a'] {
        press_letter(&coord, c);
    }
    let del = coord.handle_key_event(&key_event_mods(0x33, EVENT_KEY_DOWN, MOD_CTRL | MOD_SHIFT));
    assert!(
        matches!(del, KeyAction::Consumed),
        "有候选时 Ctrl+Shift+3 应被删除热键消费，实际: {:?}",
        del
    );
}

#[test]
fn test_candidate_action_hotkey_modifier_exact_match() {
    if !has_schemas() {
        return;
    }
    use wind_ipc::protocol::{MOD_ALT, MOD_CTRL, MOD_LCTRL, MOD_SHIFT};
    // 回归：候选热键的修饰位按**相等**判，不按包含判。
    //
    // 旧判据是 `"ctrl+number" if has_ctrl && !has_shift`（完全不看 Alt）⇒ 出厂配置
    // （pin=ctrl+number）下按 Ctrl+Alt+2 会命中 pin 那条臂、把第 2 个候选静默置顶，
    // 而 TSF 侧同时把这个键交还宿主 ⇒ 宿主快捷键与置顶同时发生，用户完全看不出原因。
    // 每条用例都要重新起一段组合：不命中候选热键的 Ctrl/Alt 组合会落到「清空组合并隐藏
    // 候选窗」那条兜底臂（`ClearComposition`），把缓冲清空。
    let fresh = || {
        let c = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
        for ch in ['a', 'a', 'a', 'a'] {
            press_letter(&c, ch);
        }
        c
    };

    // ① 多一个 Alt ⇒ 与 pin(ctrl+number) / delete(ctrl+shift+number) 都不相等。
    //    正确结局是落到兜底臂 `ClearComposition`（语义：组合清掉，键仍归宿主），
    //    **不是** `Consumed`（那才是「被候选热键认领了」）。
    let coord = fresh();
    let ctrl_alt =
        coord.handle_key_event(&key_event_mods(0x32, EVENT_KEY_DOWN, MOD_CTRL | MOD_ALT));
    assert!(
        matches!(ctrl_alt, KeyAction::ClearComposition),
        "出厂配置下 Ctrl+Alt+2 不该被候选热键认领（旧实现会误置顶），实际: {:?}",
        ctrl_alt
    );

    // ② Ctrl+Shift+Alt 同理：比 delete 模板多一位。
    let coord = fresh();
    let all_three = coord.handle_key_event(&key_event_mods(
        0x33,
        EVENT_KEY_DOWN,
        MOD_CTRL | MOD_SHIFT | MOD_ALT,
    ));
    assert!(
        matches!(all_three, KeyAction::ClearComposition),
        "Ctrl+Shift+Alt+3 不该被删除热键认领，实际: {:?}",
        all_three
    );

    // ③ 左右具体位（TSF 的 `GetCurrentModifiers` 恒附带，见 BinaryProtocol.h）**不参与比较**。
    //    漏掉这条掩码的话等值判据会一个都匹配不上——把功能整个判死，失效方向与 ①② 相反，
    //    而 ①② 全绿。**两个方向各要有一条用例**。
    let coord = fresh();
    let with_specific =
        coord.handle_key_event(&key_event_mods(0x32, EVENT_KEY_DOWN, MOD_CTRL | MOD_LCTRL));
    assert!(
        matches!(with_specific, KeyAction::Consumed),
        "带左 Ctrl 具体位的 Ctrl+2 仍应命中置顶，实际: {:?}",
        with_specific
    );
}

#[test]
fn test_candidate_action_hotkey_ctrl_alt_number() {
    if !has_schemas() {
        return;
    }
    use wind_ipc::protocol::{MOD_ALT, MOD_CTRL, MOD_SHIFT};
    // 值域第三项 `ctrl+alt+number`。两个方向都要测：配了要命中，没配要放过。
    let mut cfg = config_with("wubi86");
    cfg.keys.delete_candidate = "ctrl+alt+number".into(); // pin 留出厂 ctrl+number
    let fresh = || {
        let c = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
        for ch in ['a', 'a', 'a', 'a'] {
            press_letter(&c, ch);
        }
        c
    };

    let hit = fresh().handle_key_event(&key_event_mods(0x33, EVENT_KEY_DOWN, MOD_CTRL | MOD_ALT));
    assert!(
        matches!(hit, KeyAction::Consumed),
        "配成 ctrl+alt+number 后 Ctrl+Alt+3 应被删除热键消费，实际: {:?}",
        hit
    );

    // 出厂的 pin=ctrl+number 仍各行其是，没有被 Ctrl+Alt 那条抢走。
    let pin = fresh().handle_key_event(&key_event_mods(0x32, EVENT_KEY_DOWN, MOD_CTRL));
    assert!(
        matches!(pin, KeyAction::Consumed),
        "Ctrl+2 仍应命中置顶，实际: {:?}",
        pin
    );

    // delete 已改配到 Ctrl+Alt，原来的 Ctrl+Shift+数字 就该彻底不认了
    // ——否则是「新值域加上了、旧绑定没撤下」，用户以为改了其实是两个都通。
    let stale =
        fresh().handle_key_event(&key_event_mods(0x33, EVENT_KEY_DOWN, MOD_CTRL | MOD_SHIFT));
    assert!(
        matches!(stale, KeyAction::ClearComposition),
        "delete 改配后 Ctrl+Shift+3 不该再被认领，实际: {:?}",
        stale
    );
}

#[test]
fn test_overflow_number_key_ignore_default() {
    if !has_schemas() {
        return;
    }
    // 默认 overflow.number_key = "ignore"：数字键越界当前页候选时吞键无效（保留组合）。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a');
    let count = coord.debug_candidate_count();
    if count == 0 || count >= 9 {
        return; // 保证数字 9 必然越界
    }
    let act = coord.handle_key_event(&key_event(0x39, EVENT_KEY_DOWN)); // 主键盘 9
    assert!(
        matches!(act, KeyAction::Consumed),
        "默认 ignore 下越界数字键应吞键(Consumed)，实际: {:?}",
        act
    );
}

#[test]
fn test_overflow_number_key_commit_and_input() {
    if !has_schemas() {
        return;
    }
    // overflow.number_key = "commit_and_input"：越界时顶字上屏高亮候选 + 追加数字字符。
    let mut cfg = config_with("wubi86");
    cfg.keys.overflow.number_key = "commit_and_input".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_letter(&coord, 'a');
    let count = coord.debug_candidate_count();
    if count == 0 || count >= 9 {
        return;
    }
    let act = coord.handle_key_event(&key_event(0x39, EVENT_KEY_DOWN)); // 越界数字 9
    match act {
        KeyAction::InsertText { text, .. } => {
            assert!(
                text.ends_with('9'),
                "commit_and_input 应以越界数字 9 结尾，实际: {}",
                text
            );
            assert!(
                text.chars().count() >= 2,
                "应为高亮候选 + 数字，实际: {}",
                text
            );
        }
        other => panic!("commit_and_input 应 InsertText，实际: {:?}", other),
    }
}

#[test]
fn test_numpad_direct_outputs_digit() {
    if !has_schemas() {
        return;
    }
    // 默认 numpad_behavior 为空 → direct：不把该键解释为选词，但**已打的码不丢**——
    // 顶屏当前高亮候选后接着输出小键盘数字（旧契约为「丢弃编码只输出数字」，已废止：
    // 丢掉用户已打的码是数据丢失，且与主键盘标点键的既有行为不一致）。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a'); // 产生组合 + 候选
    // 小键盘 5 (VK_NUMPAD5 = 0x65)
    let act = coord.handle_key_event(&key_event(0x65, EVENT_KEY_DOWN));
    match act {
        KeyAction::InsertText { text, .. } => {
            assert!(
                text.ends_with('5') && text.chars().count() > 1,
                "direct 小键盘应顶屏候选再接数字 5，实际: {}",
                text
            );
        }
        other => panic!("direct 小键盘应 InsertText，实际: {:?}", other),
    }

    // 空组合时无候选可顶：仅输出数字本身。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    let act = coord.handle_key_event(&key_event(0x65, EVENT_KEY_DOWN));
    assert_eq!(
        action_text(&act).unwrap_or_default(),
        "5",
        "空组合 direct 小键盘应只输出数字"
    );
}

#[test]
fn test_numpad_follow_main_selects_like_main() {
    if !has_schemas() {
        return;
    }
    // follow_main：小键盘数字键应与主键盘数字键选同一候选。
    let mut cfg = config_with("wubi86");
    cfg.input.numpad_behavior = "follow_main".into();
    let coord_np = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_letter(&coord_np, 'a');
    // 小键盘 2 (VK_NUMPAD2 = 0x62)
    let np = coord_np.handle_key_event(&key_event(0x62, EVENT_KEY_DOWN));

    // 对照：主键盘 2 (0x32) 选第二候选
    let coord_main = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord_main, 'a');
    let main = coord_main.handle_key_event(&key_event(0x32, EVENT_KEY_DOWN));

    let np_text = action_text(&np).unwrap_or_default();
    let main_text = action_text(&main).unwrap_or_default();
    assert!(!np_text.is_empty(), "follow_main 小键盘 2 应上屏候选");
    assert_eq!(
        np_text, main_text,
        "follow_main 小键盘 2 应与主键盘 2 选同一候选（np={}, main={}）",
        np_text, main_text
    );
}

#[test]
fn test_numpad_follow_main_empty_passthrough() {
    if !has_schemas() {
        return;
    }
    // follow_main + 空缓冲：小键盘数字应透传（由应用原样输出数字），不被 IME 吞。
    let mut cfg = config_with("wubi86");
    cfg.input.numpad_behavior = "follow_main".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let act = coord.handle_key_event(&key_event(0x67, EVENT_KEY_DOWN)); // VK_NUMPAD7
    assert!(
        matches!(act, KeyAction::PassThrough),
        "follow_main 空缓冲小键盘数字应 PassThrough，实际: {:?}",
        act
    );
}

/// 全角态、指定 numpad 档位与中英模式的协调器。
fn coord_full_width(
    numpad_behavior: &str,
    half_width: bool,
    chinese: bool,
) -> std::sync::Arc<Coordinator> {
    let mut cfg = config_with("wubi86");
    cfg.input.numpad_behavior = numpad_behavior.into();
    cfg.input.numpad_half_width = half_width;
    // remember_last_state 会让三态改从 state.toml 恢复，全角就设不上了。
    cfg.input.default.remember_last_state = false;
    cfg.input.default.full_width = true;
    cfg.input.default.chinese_mode = chinese;
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

#[test]
fn test_numpad_half_width_direct() {
    if !has_schemas() {
        return;
    }
    // direct 档、空缓冲：出厂（开关关）走 commit_highlight_then_char 的 to_full_width。
    let off = coord_full_width("direct", false, true);
    assert_eq!(
        action_text(&off.handle_key_event(&key_event(0x65, EVENT_KEY_DOWN))).unwrap_or_default(),
        "５",
        "开关关时 direct 小键盘 5 应出全角"
    );

    let on = coord_full_width("direct", true, true);
    assert_eq!(
        action_text(&on.handle_key_event(&key_event(0x65, EVENT_KEY_DOWN))).unwrap_or_default(),
        "5",
        "开关开时 direct 小键盘 5 应出半角"
    );
    // 小键盘小数点：direct 档不走中文标点，只被全角转换成 `．`；开关开后回 ASCII。
    let on_dot = coord_full_width("direct", true, true);
    assert_eq!(
        action_text(&on_dot.handle_key_event(&key_event(0x6E, EVENT_KEY_DOWN))).unwrap_or_default(),
        ".",
        "开关开时 direct 小键盘 . 应出半角"
    );
}

#[test]
fn test_numpad_half_width_follow_main() {
    if !has_schemas() {
        return;
    }
    // follow_main 档：键已归一化成主键盘键、来源只剩 `State::numpad_origin` 一个凭据，
    // 空缓冲数字落 convert_punct 流水线。
    let off = coord_full_width("follow_main", false, true);
    assert_eq!(
        action_text(&off.handle_key_event(&key_event(0x67, EVENT_KEY_DOWN))).unwrap_or_default(),
        "７",
        "开关关时 follow_main 小键盘 7 应出全角"
    );

    let on = coord_full_width("follow_main", true, true);
    assert_eq!(
        action_text(&on.handle_key_event(&key_event(0x67, EVENT_KEY_DOWN))).unwrap_or_default(),
        "7",
        "开关开时 follow_main 小键盘 7 应出半角"
    );
    // 小键盘 `.` 在 follow_main 下归一化成 VK_PERIOD，本会被中文标点转成「。」——
    // 开关的语义是整条流水线跳过，故这里也必须是 ASCII。
    let on_dot = coord_full_width("follow_main", true, true);
    assert_eq!(
        action_text(&on_dot.handle_key_event(&key_event(0x6E, EVENT_KEY_DOWN))).unwrap_or_default(),
        ".",
        "开关开时 follow_main 小键盘 . 应出半角（不转中文句号）"
    );
}

#[test]
fn test_numpad_half_width_english_mode() {
    if !has_schemas() {
        return;
    }
    // 英文模式全角走 handle_english_full_width → convert_punct，同样受管辖
    // （用户拍板：中英两种模式都生效）。
    let off = coord_full_width("direct", false, false);
    assert_eq!(
        action_text(&off.handle_key_event(&key_event(0x65, EVENT_KEY_DOWN))).unwrap_or_default(),
        "５",
        "英文模式开关关时小键盘 5 应出全角"
    );

    let on = coord_full_width("direct", true, false);
    assert_eq!(
        action_text(&on.handle_key_event(&key_event(0x65, EVENT_KEY_DOWN))).unwrap_or_default(),
        "5",
        "英文模式开关开时小键盘 5 应出半角"
    );
}

#[test]
fn test_numpad_half_width_leaves_main_keyboard_alone() {
    if !has_schemas() {
        return;
    }
    // ★ 守 `State::numpad_origin` 的**无条件重写**：只在为真时置位的话，先按一次小键盘
    // 会把来源标记留给后面的主键盘按键，表现为「主键盘数字也跟着出半角」。
    let coord = coord_full_width("direct", true, true);
    assert_eq!(
        action_text(&coord.handle_key_event(&key_event(0x65, EVENT_KEY_DOWN))).unwrap_or_default(),
        "5",
        "小键盘 5 应出半角"
    );
    assert_eq!(
        action_text(&coord.handle_key_event(&key_event(0x37, EVENT_KEY_DOWN))).unwrap_or_default(),
        "７",
        "紧接着的主键盘 7 仍应出全角（开关只管小键盘）"
    );
}

#[test]
fn test_pinyin_basic_input() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    assert_eq!(coord.active_schema_id(), "pinyin");

    let mut last = KeyAction::PassThrough;
    for c in "nihao".chars() {
        last = press_letter(&coord, c);
    }
    let preedit = action_text(&last).expect("应返回 UpdateComposition");
    // 拼音组合区显示音节分隔的拼音串，不含候选
    assert_eq!(
        preedit, "ni'hao",
        "拼音组合区应显示 'ni'hao'，实际: {}",
        preedit
    );

    // 空格上屏首选，应得到 你好
    let commit = coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN));
    match commit {
        KeyAction::InsertText { text, .. } => {
            assert!(text.contains("你好"), "空格上屏应含 你好，实际: {}", text);
        }
        other => panic!("空格应上屏 InsertText，实际: {:?}", other),
    }
}

/// `z_key_action = "temp_pinyin"`：`znihao` 应经临时拼音上屏「你好」，不含字面 z。
/// 无论 z 在方案里是死码（首键即进临拼，身份③）还是活码前缀（后续字母处 z-fallback 夺取，
/// 身份②→③），都收敛到临拼编码「nihao」——故对 schema 细节鲁棒。
#[test]
fn test_z_letter_trigger_temp_pinyin() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "znihao".chars() {
        press_letter(&coord, c);
    }
    let commit = coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN));
    match commit {
        KeyAction::InsertText { text, .. } => {
            assert!(
                text.contains("你好"),
                "znihao 应经临拼上屏 你好，实际: {}",
                text
            );
            assert!(!text.contains('z'), "上屏不应含字面 z，实际: {}", text);
        }
        other => panic!("空格应上屏 InsertText，实际: {:?}", other),
    }
}

/// `z_key_action` 未配时：znihao 走正常五笔，不进临拼（回归保护——不误触发）。
#[test]
fn test_z_not_trigger_stays_normal() {
    if !has_schemas() {
        return;
    }
    // 出厂 z_key_action 为空。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    let a = press_letter(&coord, 'z');
    // 正常五笔：z 进缓冲（组合区含 z）或作码，绝不进临拼前缀语义。
    if let Some(disp) = action_text(&a) {
        assert!(
            disp.starts_with('z') || disp.is_empty(),
            "z 未配触发键应作正常码累积，实际组合区: {}",
            disp
        );
    }
}

/// `z_key_action = "mix:<id>"`：z 进融合模式。
///
/// 判据取候选内容而非组合区文本——普通输入按 z 后组合区同样是 "z"（五笔码），两者在
/// 屏幕上完全同形。内置 quick_mix 的成员含拼音，故 `zni` 应出「你」；五笔下 `zni` 不会。
#[test]
fn test_z_key_action_enters_mix() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.z_key_action = "mix:quick_mix".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let a = press_vk(&coord, 0x5A, false); // z
    assert_eq!(
        action_text(&a).unwrap_or_default(),
        "z",
        "组合区应显示引导键 z（vk_to_prefix_char_with_letters）"
    );
    press_str(&coord, "ni");
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t == "你"),
        "应已进入 mix：其拼音成员应出「你」，实际: {:?}",
        texts
    );
}

/// z 引导进模式后空缓冲回车：**原样吐回 `z`**，与符号引导键（`;` 吐回 `;`）同语义。
///
/// 字母引导键旧实现下 `mix_prefix` 恒为空（`vk_to_prefix_char` 不认字母），于是这里只能
/// 清空退出——用户按下的键凭空消失。前缀改用 `vk_to_prefix_char_with_letters` 后两类
/// 引导键的回车语义才对齐。
#[test]
fn test_z_key_action_mix_empty_enter_echoes_guide_key() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.z_key_action = "mix:quick_mix".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_vk(&coord, 0x5A, false); // z
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "z", "空缓冲回车应原样上屏引导键");
        }
        other => panic!("应上屏引导键 z，实际: {:?}", other),
    }
}

/// `z_key_action = "temp_english"`：z 进临时英文，缓冲装英文原文（空格上屏原文）。
#[test]
fn test_z_key_action_enters_temp_english() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_english".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_vk(&coord, 0x5A, false); // z
    for c in "hello".chars() {
        press_letter(&coord, c);
    }
    // 临英首候选恒为用户原文；五笔下 "zhello" 绝不会上屏成 "hello"。
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "hello", "临英空格应上屏原文");
        }
        other => panic!("空格应上屏 InsertText，实际: {:?}", other),
    }
}

/// `z_key_action = "special:<方案id>"`：z 进特殊模式，候选来自该 overlay 方案自身。
#[test]
fn test_z_key_action_enters_special() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    let ov = overlay_override_dir("test_z_key_action_enters_special", &[("pinyin", false)]);
    cfg.schema.codetable.z_key_action = "special:pinyin".into();
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov));
    press_vk(&coord, 0x5A, false); // z
    press_str(&coord, "ni");
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t == "你"),
        "特殊模式应按其引用方案（pinyin）出候选，实际: {:?}",
        texts
    );
}

/// `z_key_action` 指向不存在的目标：**不得吞键**，z 落普通输入作正常码。
///
/// 吞键的后果是把 z 这个编码键废掉，且用户从现象上完全看不出原因——配错一个 id
/// 就再也打不出 z 开头的编码。门卫没过一律返回 None，见 `enter_bound_action`。
#[test]
fn test_z_key_action_unknown_target_falls_through() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.z_key_action = "special:nonexistent".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let a = press_vk(&coord, 0x5A, false); // z
    if let Some(disp) = action_text(&a) {
        assert!(
            disp.starts_with('z') || disp.is_empty(),
            "未知目标应落普通输入，实际组合区: {}",
            disp
        );
    }
    // 判据取候选：回车此时无从区分（普通输入与进了模式都上屏 "z"）。
    press_str(&coord, "ni");
    let texts = coord.debug_page_texts();
    assert!(
        !texts.iter().any(|t| t == "你"),
        "不该进任何模式：`zni` 应作五笔码，不出拼音候选，实际: {:?}",
        texts
    );
}

/// 字母引导键进临拼后回车放弃：上屏的原码**必须带上引导字母**（`zhang` 而非 `hang`）。
///
/// 符号引导键（`` ` ``）不带——它在码表里不产出编码，按它只可能是为了开模式。字母不同：
/// z 在码表里是合法编码字符，用户按下时它既可能是开关也可能是码。回车放弃临拼的语义正是
/// 「别猜了，把我打的原样给我」，此时吞掉 z 就是猜错了还不还。
///
/// 对照组见 `test_temp_pinyin_nonempty_enter_commit_still_outputs_code`（符号引导仍不带）。
#[test]
fn test_temp_pinyin_letter_guide_enter_keeps_guide_letter() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let entered = press_vk(&coord, 0x5A, false); // z → 进临拼，prefix = "z"
    assert!(coord.debug_in_temp_pinyin(), "前置条件：z 应进入临拼");
    // 组合区必须显示按下的 z。`temp_pinyin_prefix_for` 若用不带字母的映射，VK_Z 会返回
    // None 并兜底成反引号——组合区凭空显示一个没按过的 `，下面归还引导字母的判据
    // （首字符是否字母）也会永远不成立。
    assert_eq!(
        action_text(&entered).unwrap_or_default(),
        "z",
        "组合区应显示按下的引导字母，而非兜底反引号"
    );
    for c in "hang".chars() {
        press_letter(&coord, c);
    }
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(
                text, "zhang",
                "字母引导键回车应上屏含引导字母的原码，实际丢了 z"
            );
        }
        other => panic!("回车应上屏原码，实际: {:?}", other),
    }
}

/// 字母引导键进临拼后**切中英文**放弃：同样带上引导字母（与回车同进同出）。
///
/// 「上屏原码」在临拼有三个同源出口——回车、切中英文、mix 回车——判据收在
/// `Coordinator::guide_to_return`。只改回车会立刻造出「回车带 z、Shift 切英文不带」的
/// 不一致，而 `take_input_on_mode_switch` 的注释还写着「与各自回车上屏一致」。
#[test]
fn test_temp_pinyin_letter_guide_mode_switch_keeps_guide_letter() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    cfg.keys.commit_on_switch = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_vk(&coord, 0x5A, false); // z
    for c in "hang".chars() {
        press_letter(&coord, c);
    }
    // 左 Shift 释放：中→英，commit_on_switch=true → 上屏原码。
    let act = coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    assert_eq!(
        action_text(&act).unwrap_or_default(),
        "zhang",
        "字母引导键切中英文应上屏含引导字母的原码，实际: {:?}",
        act
    );
}

/// z-fallback 夺取后退格：应回到**夺取前那一帧**（`z` + repeat 候选），一次到位。
///
/// 曾退到 `zx`——即 `input_buffer + 触发夺取的那一键`。而夺取的前提恰恰是
/// `has_code_prefix("zx") == false`，所以那个落点**必然无候选**：判据说「这里没东西」，
/// 回退目标偏要退到那里。用户看到的是「第一下退格只让候选窗消失、编码还在」，得再按一次。
///
/// 且 `zx` 这一帧用户从未见过——按下 x 的同一帧就被夺取进临拼了。回退目标必须是用户
/// 实际见过的某一帧，否则无论内部账目多自洽，读起来都像卡了一下。
#[test]
fn test_z_fallback_backspace_returns_to_hijack_origin() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    // repeat 开 + 有上屏历史 → 首键 z 让位（裁决①），buffer 变 "z"，为 fallback 铺路。
    cfg.schema.codetable.z_key_repeat = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // 先上屏一次，给 z_key_repeat 提供历史。
    press_letter(&coord, 'a');
    let committed = match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("前置条件：空格应上屏，实际: {:?}", other),
    };

    press_vk(&coord, 0x5A, false); // z → repeat 让位，落普通输入
    assert!(
        !coord.debug_in_temp_pinyin(),
        "前置条件：repeat 开时首键 z 应让位，不进临拼"
    );
    press_letter(&coord, 'x'); // "zx" 非活码前缀 → fallback 夺取
    assert!(
        coord.debug_in_temp_pinyin(),
        "前置条件：zx 破前缀应触发 z-fallback 夺取"
    );

    coord.handle_key_event(&key_event(0x08, EVENT_KEY_DOWN)); // Backspace
    assert!(!coord.debug_in_temp_pinyin(), "退格应撤销夺取、退出临拼");
    let texts = coord.debug_page_texts();
    assert!(
        texts.contains(&committed),
        "退格应一次回到夺取前那一帧（buffer=\"z\"，repeat 候选「{}」在列）；\
         若退到了 zx 则无任何候选，实际: {:?}",
        committed,
        texts
    );
}

/// 临拼模式下切中英文：应遵循 keys.commit_on_switch —— 开启（默认）时把拼音原码上屏，
/// 而非无条件清空（回归保护：此前 take_input_on_mode_switch 独占分支对临拼恒返回空串）。
///
/// 用**符号**引导键（`` ` ``）以隔离关切：本测试只管 commit_on_switch 这个开关本身。
/// 字母引导键会把引导字母一并归还（`znihao` 而非 `nihao`），那是另一条语义，
/// 由 `test_temp_pinyin_letter_guide_mode_switch_keeps_guide_letter` 单独锁。
#[test]
fn test_temp_pinyin_commit_on_mode_switch() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    cfg.keys.commit_on_switch = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    // 进入临拼（反引号引导），缓冲拼音码 nihao（不选词、不上屏）。
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN)); // `
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    // 左 Shift 释放：中→英切换，commit_on_switch=true → 应上屏拼音原码 nihao。
    let act = coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    let text = action_text(&act).unwrap_or_default();
    assert_eq!(
        text, "nihao",
        "临拼切英文应按 commit_on_switch 上屏原码 nihao，实际: {:?}",
        act
    );
    assert!(!coord.is_chinese_mode(), "左 Shift 应切到英文");
}

/// 关闭 commit_on_switch 时：临拼切中英文应清空，不上屏原码。
/// 同样用符号引导键隔离关切（理由见上一个测试）。
#[test]
fn test_temp_pinyin_no_commit_on_mode_switch_when_disabled() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    cfg.keys.commit_on_switch = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN)); // `
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    let act = coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    let text = action_text(&act).unwrap_or_default();
    assert!(
        text.is_empty(),
        "commit_on_switch=false 时临拼切换应清空，实际上屏: {:?}",
        text
    );
}

/// 只按了模式进入符（缓冲空）时切英文：应像回车一样原样上屏该前缀符号，而非清空。
/// commit_on_switch=on（上屏编码选项）时对齐回车空缓冲上屏语义。
#[test]
fn test_temp_pinyin_prefix_only_commits_symbol_on_switch() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.commit_on_switch = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    // 只按反引号进入临拼（缓冲空，只有前缀 `）。
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    // 左 Shift 释放切英文：应上屏前缀符号 `（与回车空缓冲上屏一致）。
    let act = coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    let text = action_text(&act).unwrap_or_default();
    assert_eq!(text, "`", "只按进入符切英文应上屏该符号 `，实际: {:?}", act);
    assert!(!coord.is_chinese_mode(), "左 Shift 应切到英文");
}

/// 快捷输入只按进入符 ; 时切英文：应像回车一样原样上屏 ;（非中文 ；）。
#[test]
fn test_quick_input_prefix_only_commits_symbol_on_switch() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.commit_on_switch = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入快捷输入（空缓冲）
    let act = coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    let text = action_text(&act).unwrap_or_default();
    assert_eq!(
        text, ";",
        "只按进入符 ; 切英文应原样上屏 ;，实际: {:?}",
        act
    );
}

/// 关闭 commit_on_switch 时：只按进入符切英文应清空，不上屏符号。
#[test]
fn test_prefix_only_no_commit_on_switch_when_disabled() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.commit_on_switch = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN)); // ` 进入临拼（空缓冲）
    let act = coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    let text = action_text(&act).unwrap_or_default();
    assert!(
        text.is_empty(),
        "commit_on_switch=false 时只按进入符切英文应清空，实际上屏: {:?}",
        text
    );
}

/// 断言某次分隔符键返回的动作**未**把 `'` 压入组合区。
fn separator_not_inserted(act: &KeyAction) -> bool {
    !matches!(act, KeyAction::UpdateComposition { text, .. } if text.contains('\''))
}

/// Task 8 / Fix Round 1：`auto` 真语义——默认选键组（`semicolon_quote` 含 `'`=VK_OEM_7）下，
/// `'` 保留三选键功能、**不**作分隔符；改由反引号(`, VK_OEM_3=0xC0)作硬边界压入缓冲。
#[test]
fn separator_auto_avoids_quote_when_it_is_select_key() {
    if !has_schemas() {
        return;
    }
    // ' (VK_OEM_7=0xDE)：默认作三选键 → 不作分隔符，preedit 不应出现 '
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    for c in "xi".chars() {
        press_letter(&coord, c);
    }
    let q = coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN));
    assert!(
        separator_not_inserted(&q),
        "auto+默认选键组：引号应保留选键功能、不作分隔符，实际: {:?}",
        q
    );

    // 反引号(0xC0)：' 被占 → 反引号作分隔符，压入 ' 并固定音节边界
    let coord2 = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    for c in "xi".chars() {
        press_letter(&coord2, c);
    }
    let b = coord2.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    let pre = action_text(&b).expect("反引号应作分隔符并返回组合区");
    assert!(
        pre.contains('\''),
        "auto+默认选键组：反引号应插入分隔符，实际 preedit: {}",
        pre
    );
    let mut last = b;
    for c in "an".chars() {
        last = press_letter(&coord2, c);
    }
    assert_eq!(
        action_text(&last).unwrap(),
        "xi'an",
        "反引号手动分隔符应固定音节边界"
    );
    assert!(
        !coord2.debug_page_texts().is_empty(),
        "分隔后仍应有候选（如「西」/「西安」）"
    );
}

/// Fix Round 1：`auto` 下若 `'` **不**在选键组（此处 `comma_period`）→ `'` 空闲、作分隔符；
/// 反引号则不作分隔符。
#[test]
fn separator_auto_uses_quote_when_not_select_key() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("pinyin");
    cfg.keys.select_key_groups = vec!["comma_period".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "xi".chars() {
        press_letter(&coord, c);
    }
    let q = coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN));
    let pre = action_text(&q).expect("引号应作分隔符并返回组合区");
    assert!(
        pre.contains('\''),
        "auto+选键组不含引号：引号应作分隔符，实际: {}",
        pre
    );

    let mut cfg2 = config_with("pinyin");
    cfg2.keys.select_key_groups = vec!["comma_period".into()];
    let coord2 = Coordinator::new_headless(cfg2, Some(&data_dir()));
    for c in "xi".chars() {
        press_letter(&coord2, c);
    }
    let b = coord2.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    assert!(
        separator_not_inserted(&b),
        "auto+选键组不含引号：反引号不应作分隔符，实际: {:?}",
        b
    );
}

/// Fix Round 1：显式 `quote` 模式尊重用户指定值——即使默认选键组含 `'`，引号仍作分隔符（覆盖选键）。
#[test]
fn separator_explicit_quote_overrides_select_key() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("pinyin");
    cfg.schema.pinyin.separator = "quote".into(); // 显式，默认选键组仍含 '
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "xi".chars() {
        press_letter(&coord, c);
    }
    let q = coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN));
    let pre = action_text(&q).expect("显式 quote 引号应作分隔符并返回组合区");
    assert!(
        pre.contains('\''),
        "显式 quote 模式：引号应作分隔符（覆盖选键），实际: {}",
        pre
    );
}

/// 双拼在 `auto` 下不占用任何分隔符键——引号/反引号均不作分隔符。
///
/// ⚠️ **同一个现象，判据已经换过一次**。原先是「双拼一律禁用」（键位判定里按引擎类型
/// 早退）；引擎侧支持分隔符后（`docs/design/shuangpin-separator.md`）改由 `auto` 的
/// 空闲判定拦住：双拼下 `'` 被选词键占、反引号被方案的 `[key_actions]` 占给辅助码，
/// 两个都不空闲 ⇒ auto 不启用。
///
/// ⇒ 本用例守的是**「auto 会避让已占用的键」**，不再是「双拼这条路走不通」。
/// 显式配 `quote` 仍然生效，那条由
/// [`separator_works_for_shuangpin_when_explicitly_enabled`] 守——两条必须并存：
/// 少了后者，把判据改回按引擎类型早退，这条照样绿。
#[test]
fn separator_off_by_default_for_shuangpin() {
    let d = data_dir();
    if !d.join("schemas/shuangpin.schema.toml").exists()
        && !d.join("schemas/shuangpin.schema.yaml").exists()
    {
        return;
    }
    let mut cfg = config_with("pinyin");
    cfg.schema.available = vec!["shuangpin".into(), "pinyin".into()];
    cfg.schema.active = "shuangpin".into(); // separator 保持默认 auto
    let coord = Coordinator::new_headless(cfg, Some(&d));
    for c in "ui".chars() {
        press_letter(&coord, c);
    }
    let q = coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN));
    assert!(
        separator_not_inserted(&q),
        "双拼：引号不应作分隔符，实际: {:?}",
        q
    );

    let mut cfg2 = config_with("pinyin");
    cfg2.schema.available = vec!["shuangpin".into(), "pinyin".into()];
    cfg2.schema.active = "shuangpin".into();
    let coord2 = Coordinator::new_headless(cfg2, Some(&d));
    for c in "ui".chars() {
        press_letter(&coord2, c);
    }
    let b = coord2.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    assert!(
        separator_not_inserted(&b),
        "双拼：反引号不应作分隔符，实际: {:?}",
        b
    );
}

/// 双拼显式配 `separator = "quote"` 后，`'` 作分隔符压入缓冲。
///
/// 这是「引擎侧支持 + 键位判定放行」两件事在协调器这一层的会合点。
/// ⚠️ 与上一条互为正反：只留上一条的话，把 `pinyin_ok` 改回对双拼早退它照样绿
///（出厂本来就是 none），放行有没有真的发生根本测不出来。
///
/// ⚠️ **必须覆盖在方案层**：`separator` 方案级优先、全局只作回落，而
/// `shuangpin.schema.toml` 出厂就写了 `none`——改 `cfg.schema.pinyin.separator`
/// 会被它盖掉，测试变成恒绿的假用例。
#[test]
fn separator_works_for_shuangpin_when_explicitly_enabled() {
    let d = data_dir();
    if !d.join("schemas/shuangpin.schema.toml").exists()
        && !d.join("schemas/shuangpin.schema.yaml").exists()
    {
        return;
    }
    let ov = std::env::temp_dir().join("wind_sp_sep_quote_ov");
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).expect("建 override 目录");
    std::fs::write(
        ov.join("shuangpin.toml"),
        "[engine.pinyin]\nseparator = \"quote\"\n",
    )
    .expect("写方案覆盖");

    let mut cfg = config_with("pinyin");
    cfg.schema.available = vec!["shuangpin".into(), "pinyin".into()];
    cfg.schema.active = "shuangpin".into();
    let coord = Coordinator::new_headless_with_override(cfg, Some(&d), Some(ov));
    for c in "ui".chars() {
        press_letter(&coord, c);
    }
    let q = coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN));
    assert!(
        !separator_not_inserted(&q),
        "双拼在方案层配了 quote 后，`'` 应作分隔符压入缓冲，实际: {q:?}"
    );
}

/// 回归：**临时拼音**里手动分隔符必须与主输入路同样生效。
///
/// 缺陷形态（用户报障）：纯五笔方案 + `z_key_action = "temp_pinyin"`，在临拼里按分隔符键
/// **直接把第一个字上屏了**。两层根因叠加：
/// ① `handle_temp_pinyin_key` 根本没有分隔符臂 ⇒ 键落到 `_` 兜底的「其它键：有候选则上屏
///    高亮候选」（主路径的分隔符臂在 `message_handler.rs`，而临拼在那之前就 early return 了）；
/// ② 即便补上臂，`manual_separator_key` 问的是**活跃**引擎（五笔=码表）⇒ 判据恒 false。
///
/// 故本测试必须用**码表主方案 + 临拼**这个组合：在纯拼音方案下跑，根因②测不出来。
#[test]
fn separator_works_in_temp_pinyin_over_codetable_schema() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "zxi".chars() {
        press_letter(&coord, c);
    }
    assert!(coord.debug_in_temp_pinyin(), "前置条件：z 引导应已进入临拼");
    // 反引号(0xC0)：出厂 auto + 选键组含 `'` ⇒ 反引号作分隔符（与主路径同一套键位判定）。
    let b = coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    let pre = action_text(&b).expect("临拼下反引号应作分隔符并返回组合区");
    assert!(
        pre.contains('\''),
        "临拼：反引号应插入手动分隔符，实际组合区: {}",
        pre
    );
    let mut last = b;
    for c in "an".chars() {
        last = press_letter(&coord, c);
    }
    assert_eq!(
        action_text(&last).as_deref(),
        Some("zxi'an"),
        "临拼手动分隔符应固定音节边界（z 为引导前缀）"
    );
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t == "西安"),
        "分隔后候选应含整句「西安」，实际: {:?}",
        texts
    );
}

/// 回归：临拼下分隔符键**不得**再走「其它键 → 上屏高亮候选」那条兜底。
///
/// 与上一条互为正反面：上一条锁「插进去了」，这一条锁「没把字打出去」。判据取
/// **KeyAction 不是上屏类**——只看组合区文本的话，上屏那一帧同样会返回组合区更新
/// （分段上屏保留剩余拼音），恒绿。
#[test]
fn separator_in_temp_pinyin_does_not_commit_candidate() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "zxi".chars() {
        press_letter(&coord, c);
    }
    assert!(coord.debug_in_temp_pinyin(), "前置条件：应已进入临拼");
    let act = coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    assert!(
        !matches!(act, KeyAction::InsertText { .. }),
        "临拼按分隔符键不得上屏候选，实际: {:?}",
        act
    );
    assert!(coord.debug_in_temp_pinyin(), "按分隔符键后应仍在临拼模式内");
}

/// 造一个「临拼目标 = 微软双拼（`;` 是韵母 ing）」的环境：wubi86 主方案 + z 引导，
/// primary_pinyin 指向 shuangpin，并用 override 把布局换成 mspy。
///
/// 出厂 shuangpin 用的是小鹤（韵母全在字母键上），测不出符号韵母键这条路。
fn coord_with_mspy_temp_pinyin(tag: &str) -> Option<std::sync::Arc<Coordinator>> {
    let d = data_dir();
    if !has_schemas() || !d.join("schemas/shuangpin/mspy.toml").exists() {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("wind_tp_mspy_{tag}"));
    std::fs::create_dir_all(&dir).expect("建 override 目录失败");
    std::fs::write(
        dir.join("shuangpin.toml"),
        "[engine.pinyin.shuangpin]\nlayout = \"mspy\"\n",
    )
    .expect("写 override 失败");
    let mut cfg = config_with("wubi86");
    cfg.schema.available = vec!["wubi86".into(), "shuangpin".into()];
    cfg.schema.primary_pinyin = "shuangpin".into();
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    Some(Coordinator::new_headless_with_override(
        cfg,
        Some(&d),
        Some(dir),
    ))
}

/// 回归：临拼下**双拼布局的符号韵母键**必须能进缓冲。
///
/// 缺陷形态：主输入路的码元闸门 `try_code_char_gate` 位于 `message_handler.rs` 那句
/// `Some(ModeKind::TempPinyin) => return handle_temp_pinyin_key(..)` 之后，临拼走不到；
/// 而临拼的字母臂只认 `VK_A..=VK_Z` ⇒ 微软/搜狗/紫光双拼的 `;`(=ing) 落到兜底臂被
/// `select_key_offset` 认成次选键，**把第 2 个候选打了出去**，`ying` 一族音节在临拼里
/// 完全不可达。
///
/// 与分隔符那两条是同一个形状（overlay 是主路径的平行实现），故判据同样按 overlay
/// 方案取（`is_code_char_of`），不是按活跃的五笔方案。
#[test]
fn temp_pinyin_accepts_shuangpin_symbol_final_key() {
    let Some(coord) = coord_with_mspy_temp_pinyin("final") else {
        return;
    };
    for c in "zy".chars() {
        press_letter(&coord, c);
    }
    assert!(coord.debug_in_temp_pinyin(), "前置条件：z 引导应已进入临拼");
    let act = coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; = VK_SEMICOLON
    assert!(
        !matches!(act, KeyAction::InsertText { .. }),
        "`;` 是 mspy 的韵母键(ing)，不得被当成次选键上屏，实际: {:?}",
        act
    );
    assert!(
        coord.debug_in_temp_pinyin(),
        "按韵母键后应仍在临拼模式内（此前会被兜底臂上屏并退出）"
    );
    let pre = action_text(&act).expect("`;` 应作码元进缓冲并返回组合区");
    assert!(
        pre.contains(';'),
        "组合区应含击键 `;`（双拼 preedit 显示原始按键），实际: {}",
        pre
    );
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t == "应" || t == "英" || t == "影"),
        "y+; 应解释为 ying 并出候选，实际: {:?}",
        texts
    );
}

/// 反向对照：**全拼**临拼下 `;` 仍是次选键，不得被新的码元臂夺走。
///
/// 这条锁住新臂的零回归依据——拼音引擎的码元集完全由双拼布局推导，全拼恒 `None`
/// ⇒ 回落默认 `a-z` ⇒ 非字母恒 false。若哪天有人把回落改成「空集」或「全放行」，
/// 这条会红，而上面那条仍绿。
#[test]
fn temp_pinyin_full_pinyin_keeps_semicolon_as_select_key() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    // primary_pinyin 留空 = 全拼（见 resolve_temp_pinyin_target）。
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "zni".chars() {
        press_letter(&coord, c);
    }
    assert!(coord.debug_in_temp_pinyin(), "前置条件：应已进入临拼");
    let n = coord.debug_page_texts().len();
    assert!(n >= 2, "前置条件：ni 应至少有 2 个候选，实际 {}", n);
    let act = coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN));
    assert!(
        matches!(act, KeyAction::InsertText { .. }),
        "全拼临拼下 `;` 应仍是次选键、选词上屏，实际: {:?}",
        act
    );
}

/// 写一份只含 `[engine.pinyin] separator = <mode>` 的方案级 override，返回目录。
/// 每个用例独立目录（并发写同一文件会撕裂），用法同
/// `codetable_short_code_yield.rs` 的 `override_with_level`。
fn override_with_separator(tag: &str, mode: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_sep_ov_{tag}"));
    std::fs::create_dir_all(&dir).expect("建 override 目录失败");
    std::fs::write(
        dir.join("pinyin.toml"),
        format!("[engine.pinyin]\nseparator = \"{mode}\"\n"),
    )
    .expect("写 override 失败");
    dir
}

/// 方案级 `[engine.pinyin].separator` 关掉分隔符，压过全局 `auto`。
///
/// 这一项做成方案级可覆盖，是因为**键位预算按方案分**：双拼出厂把反引号给了辅助码
/// （`shuangpin.schema.toml` 的 `[key_actions]`），而全拼的音节边界只能靠符号键表达。
/// 全局唯一的 `separator` 表达不了这个组合——一改全局，另一个方案的键就被静默夺走。
///
/// 反向对照是既有的 `separator_auto_avoids_quote_when_it_is_select_key`：同样的按键、
/// 无 override 时反引号**会**插入分隔符。
#[test]
fn separator_schema_override_none_disables_it() {
    if !has_schemas() {
        return;
    }
    let ov = override_with_separator("none", "none");
    let coord =
        Coordinator::new_headless_with_override(config_with("pinyin"), Some(&data_dir()), Some(ov));
    for c in "xi".chars() {
        press_letter(&coord, c);
    }
    let b = coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    assert!(
        separator_not_inserted(&b),
        "方案级 separator=none 应压过全局 auto，反引号不该作分隔符，实际: {:?}",
        b
    );
}

/// 方案级 `separator = "quote"` 把分隔符**改到** `'` 上（全局仍是 auto）。
///
/// 与上一条互补：那条只证明「方案值能关掉功能」，可能被一个「读不到就当 none」的
/// 错误实现假绿；这条要求方案值被真正读出来并按它选键——`'` 出厂是三选键，只有
/// 显式模式才夺得走。
#[test]
fn separator_schema_override_selects_quote_key() {
    if !has_schemas() {
        return;
    }
    let ov = override_with_separator("quote", "quote");
    let coord =
        Coordinator::new_headless_with_override(config_with("pinyin"), Some(&data_dir()), Some(ov));
    for c in "xi".chars() {
        press_letter(&coord, c);
    }
    let q = coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN));
    let pre = action_text(&q).expect("方案级 quote：引号应作分隔符并返回组合区");
    assert!(
        pre.contains('\''),
        "方案级 separator=quote 应让引号作分隔符（覆盖三选键），实际: {}",
        pre
    );
}

/// C1 回归：全拼手动分隔符 `xi'an` 选「西安」应**全消费**上屏、组合区清空无残留。
/// 修复前引擎按剥除 `'` 的 query 算 consumed_length，协调器却按含 `'` 缓冲切片 → 误判 partial、
/// 残留尾字符 "n"（组合区变「西安n」）。修复后 consumed_length 回映射到含 `'` 的原始输入空间。
#[test]
fn separator_full_commit_consumes_all() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    for c in "xi".chars() {
        press_letter(&coord, c);
    }
    // 反引号(0xC0)作硬分隔符（默认 auto + 选键组含 ' → 反引号作分隔符，参照 Task 8 现有测试）。
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    let mut last = KeyAction::PassThrough;
    for c in "an".chars() {
        last = press_letter(&coord, c);
    }
    assert_eq!(
        action_text(&last).as_deref(),
        Some("xi'an"),
        "缓冲应为 xi'an"
    );

    let texts = coord.debug_page_texts();
    let p = texts
        .iter()
        .position(|t| t == "西安")
        .unwrap_or_else(|| panic!("候选应含整句「西安」，实际: {:?}", texts));
    // 数字键选「西安」→ 全消费上屏，无残留尾字符
    match coord.handle_key_event(&key_event(0x31 + p as u32, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "西安", "分隔符输入选「西安」应完整上屏，不残留 'n'");
        }
        other => panic!("选「西安」应上屏 InsertText，实际: {:?}", other),
    }
    assert_eq!(
        coord.debug_candidate_count(),
        0,
        "全消费后组合区候选应清空（无残留拼音续转）"
    );
}

/// C1 回归：`xi'an` 两步分段——先选「西」组合区剩 "an"（`'` 随已消费段吃掉，非 "'an"），
/// 再选「安」整体上屏「西安」并清空。
#[test]
fn separator_two_step_segmentation() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    for c in "xi".chars() {
        press_letter(&coord, c);
    }
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    for c in "an".chars() {
        press_letter(&coord, c);
    }
    // 先选「西」（子短语，仅消费 xi 段；边界紧跟的 `'` 应归入已消费侧）
    let texts = coord.debug_page_texts();
    let p_xi = texts
        .iter()
        .position(|t| t == "西")
        .unwrap_or_else(|| panic!("候选应含子短语「西」，实际: {:?}", texts));
    let step = coord.handle_key_event(&key_event(0x31 + p_xi as u32, EVENT_KEY_DOWN));
    let disp = action_text(&step).expect("选「西」应返回 UpdateComposition");
    assert!(
        disp.starts_with('西') && disp.ends_with("an") && !disp.contains('\''),
        "选「西」后组合区应为「西」+剩余 an（无 ' 残留），实际: {:?}",
        disp
    );

    // 再选「安」→ 整体上屏「西安」，组合区清空
    let texts2 = coord.debug_page_texts();
    let p_an = texts2
        .iter()
        .position(|t| t == "安")
        .unwrap_or_else(|| panic!("剩余 an 的候选应含「安」，实际: {:?}", texts2));
    match coord.handle_key_event(&key_event(0x31 + p_an as u32, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "西安", "两步分段最终应上屏「西安」");
        }
        other => panic!("选「安」应上屏 InsertText，实际: {:?}", other),
    }
    assert_eq!(coord.debug_candidate_count(), 0, "两步选完组合区应清空");
}

/// C1 回归（鼠标版）：点选分段候选须与数字键同为分步提交——先点「西」组合区留活剩 "an"、
/// 候选续查出「安」，再点「安」整体上屏「西安」。
///
/// 曾因 `mouse_select` 独走 `commit_candidate`（无条件清缓冲、不看 consumed_length）而：
/// 剩余码被丢弃、候选窗直接消失，且第二步只上屏「安」丢掉已确认的「西」段。
#[test]
fn mouse_select_two_step_segmentation() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    for c in "xi".chars() {
        press_letter(&coord, c);
    }
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    for c in "an".chars() {
        press_letter(&coord, c);
    }
    // 鼠标点选「西」（子短语，仅消费 xi 段）→ 分步提交，组合区留活
    let texts = coord.debug_page_texts();
    let p_xi = texts
        .iter()
        .position(|t| t == "西")
        .unwrap_or_else(|| panic!("候选应含子短语「西」，实际: {:?}", texts));
    let step = coord
        .debug_mouse_select(p_xi)
        .expect("主输入路点选应产生待推送的 KeyAction");
    let disp = action_text(&step).unwrap_or_else(|| {
        panic!(
            "点选「西」应为 UpdateComposition（组合区留活），实际: {:?}",
            step
        )
    });
    assert!(
        disp.starts_with('西') && disp.ends_with("an") && !disp.contains('\''),
        "点选「西」后组合区应为「西」+剩余 an（无 ' 残留），实际: {:?}",
        disp
    );
    // 剩余分词的候选必须还在（原 bug：候选窗直接消失，count 归 0）
    assert!(
        coord.debug_candidate_count() > 0,
        "点选分段候选后应续查剩余码的候选，不应清空"
    );

    // 再点「安」→ 整体上屏「西安」（含已确认的「西」段），组合区清空
    let texts2 = coord.debug_page_texts();
    let p_an = texts2
        .iter()
        .position(|t| t == "安")
        .unwrap_or_else(|| panic!("剩余 an 的候选应含「安」，实际: {:?}", texts2));
    match coord.debug_mouse_select(p_an) {
        Some(KeyAction::InsertText { text, .. }) => {
            assert_eq!(
                text, "西安",
                "两步点选最终应上屏「西安」，不得丢失已确认的「西」段"
            );
        }
        other => panic!("点选「安」应上屏 InsertText，实际: {:?}", other),
    }
    assert_eq!(coord.debug_candidate_count(), 0, "两步点选完组合区应清空");
}

#[test]
fn test_schema_switch_via_menu() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    assert_eq!(coord.active_schema_id(), "wubi86");

    coord.handle_menu_command("switch_engine");
    assert_eq!(coord.active_schema_id(), "pinyin", "切换后应为 pinyin");

    coord.handle_menu_command("switch_engine");
    assert_eq!(coord.active_schema_id(), "wubi86", "再切回 wubi86");
}

/// `toggle_schema:<id>` 往返：按过去、再按回来源。
///
/// 与 `switch_schema:<id>` 的唯一差别就是第二次按——那个是 no-op，这个回来源。
fn config_with_toggle_hotkey(active: &str, target: &str) -> Config {
    let mut cfg = config_with(active);
    cfg.keys
        .key_actions
        .insert("ctrl+shift+n".into(), format!("toggle_schema:{target}"));
    cfg
}

/// 按下 Ctrl+Shift+N（配好的往返热键）。
fn press_toggle_hotkey(coord: &Coordinator) -> KeyAction {
    use wind_ipc::protocol::{MOD_CTRL, MOD_SHIFT};
    coord.handle_key_event(&key_event_mods(0x4E, EVENT_KEY_DOWN, MOD_CTRL | MOD_SHIFT))
}

#[test]
fn test_toggle_schema_returns_to_origin() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        config_with_toggle_hotkey("wubi86", "pinyin"),
        Some(&data_dir()),
    );
    assert_eq!(coord.active_schema_id(), "wubi86");

    press_toggle_hotkey(&coord);
    assert_eq!(coord.active_schema_id(), "pinyin", "第一次按应切到目标方案");

    press_toggle_hotkey(&coord);
    assert_eq!(
        coord.active_schema_id(),
        "wubi86",
        "第二次按应回到来源方案——这正是与 switch_schema 的区别"
    );

    press_toggle_hotkey(&coord);
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "第三次按再过去（来源已在上一步用掉并重记）"
    );
}

/// 已在目标方案且无来源记录（刚启动就按）：**不动作**。
///
/// 守的是「往返键退化成随机跳转键」：此时没有任何依据说明用户想去哪，切走比不动更糟。
#[test]
fn test_toggle_schema_without_origin_is_noop() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        config_with_toggle_hotkey("pinyin", "pinyin"),
        Some(&data_dir()),
    );
    assert_eq!(coord.active_schema_id(), "pinyin");

    press_toggle_hotkey(&coord);
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "无来源时按往返键不该切走"
    );
}

/// 期间用**别的方式**切了方案，来源作废。
///
/// 构造：往返进 pinyin（记下 origin=wubi86）→ 用菜单循环两次绕回 pinyin → 再按往返键。
/// 来源若没被清，这一按会把用户送回几步之前的 wubi86；清了则是 no-op。
///
/// ★ 循环两次而非一次是必须的：只循环一次时 current(wubi86) != target(pinyin)，
/// 会走"切过去"分支，清没清来源的结果完全相同，用例区分不出来。
#[test]
fn test_toggle_schema_origin_invalidated_by_other_switch() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        config_with_toggle_hotkey("wubi86", "pinyin"),
        Some(&data_dir()),
    );
    press_toggle_hotkey(&coord);
    assert_eq!(coord.active_schema_id(), "pinyin");

    coord.handle_menu_command("switch_engine");
    coord.handle_menu_command("switch_engine");
    assert_eq!(coord.active_schema_id(), "pinyin", "循环两次绕回 pinyin");

    press_toggle_hotkey(&coord);
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "循环切换已作废来源，此时按往返键应不动作，而非弹回 wubi86"
    );
}

/// 来源失效必须覆盖**绕过 `finish_user_schema_switch` 的切换路径**。
///
/// 设置页的 `schema.setActive` RPC 不走那个"统一收尾"——它只同步拆字/注释库便返回。
/// 切方案在协调器侧共五条路径，只有两条走 finish，所以「在 finish 里清来源」只能清一半。
/// 本用例走的正是没被 finish 覆盖的那条：来源改由 `schema_generation` 代际校验失效后，
/// 它才成立。
#[test]
fn test_toggle_schema_origin_invalidated_via_path_bypassing_finish() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        config_with_toggle_hotkey("wubi86", "pinyin"),
        Some(&data_dir()),
    );
    press_toggle_hotkey(&coord);
    assert_eq!(coord.active_schema_id(), "pinyin", "往返键进入 pinyin");

    let set_active = |id: &str| {
        coord
            .web_data_rpc("schema.setActive", &serde_json::json!({ "id": id }))
            .expect("schema.setActive 应成功");
    };
    set_active("wubi86");
    assert_eq!(coord.active_schema_id(), "wubi86");
    set_active("pinyin");
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "绕开 finish 绕回 pinyin"
    );

    press_toggle_hotkey(&coord);
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "经 RPC 的切换同样要作废来源，此时应不动作而非弹回 wubi86"
    );
}

#[test]
fn test_schema_switch_clears_input() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // 输入后切换方案应清空缓冲
    press_letter(&coord, 'a');
    coord.handle_menu_command("switch_engine");
    // 切换后再输入拼音，预编辑不应残留五笔内容
    let act = press_letter(&coord, 'n');
    let preedit = action_text(&act).unwrap_or_default();
    assert!(
        preedit.starts_with('n'),
        "切换后预编辑应从新输入 'n' 开始，实际: {}",
        preedit
    );
}

#[test]
fn test_chinese_punctuation() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    assert!(coord.is_chinese_mode());

    // 空缓冲下按 . (VK_OEM_PERIOD=0xBE) → 中文句号 。
    let act = coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN));
    match act {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "。"),
        other => panic!("应上屏中文句号，实际: {:?}", other),
    }
    // 逗号 , (0xBC) → ，
    match coord.handle_key_event(&key_event(0xBC, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "，"),
        other => panic!("应上屏中文逗号，实际: {:?}", other),
    }
    // Shift+1 = ! → ！
    let shifted = KeyEventData {
        key_code: 0x31,
        scan_code: 0,
        modifiers: 0x0001, // MOD_SHIFT
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    };
    match coord.handle_key_event(&shifted) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "！"),
        other => panic!("Shift+1 应上屏中文叹号，实际: {:?}", other),
    }
}

#[test]
fn test_punct_commits_candidate_first() {
    if !has_schemas() {
        return;
    }
    // punct_commit 默认关闭（标点键在有编码时吞键、不顶字上屏），须显式开启才有此行为。
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.punct_commit = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    // 输入 aaaa（有候选），再按句号 → 先上屏首选候选，再接中文句号
    for _ in 0..4 {
        press_letter(&coord, 'a');
    }
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert!(text.ends_with("。"), "应以中文句号结尾，实际: {}", text);
            assert!(
                text.chars().count() >= 2,
                "应包含上屏候选+句号，实际: {}",
                text
            );
        }
        other => panic!("应上屏候选+句号，实际: {:?}", other),
    }
}

#[test]
fn test_arrow_down_then_space_selects_highlighted() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // "a" 在五笔下有多个候选
    press_letter(&coord, 'a');
    let texts = coord.debug_page_texts();
    if texts.len() < 2 {
        eprintln!("跳过：当前页候选不足 2 个");
        return;
    }
    let second = texts[1].clone();

    // 初始高亮在第 0 项
    let (_, sel0, _) = coord.debug_page_info();
    assert_eq!(sel0, 0, "初始高亮应为第 0 项");

    // 下方向键 → 高亮移到第 1 项
    coord.handle_key_event(&key_event(0x28, EVENT_KEY_DOWN));
    let (_, sel1, _) = coord.debug_page_info();
    assert_eq!(sel1, 1, "下方向键后高亮应为第 1 项");

    // 空格上屏高亮项（第 2 个候选）
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, second, "空格应上屏高亮的第 2 个候选");
        }
        other => panic!("空格应上屏高亮候选，实际: {:?}", other),
    }
}

#[test]
fn test_page_down_changes_page_and_renumbers() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a');
    let (_, _, total_pages) = coord.debug_page_info();
    if total_pages < 2 {
        eprintln!("跳过：候选不足两页");
        return;
    }
    let page1_first = coord.debug_page_texts()[0].clone();

    // PageDown(0x22) → 翻到第 2 页
    coord.handle_key_event(&key_event(0x22, EVENT_KEY_DOWN));
    let (page, sel, _) = coord.debug_page_info();
    assert_eq!(page, 1, "PageDown 后应在第 2 页（0-based=1）");
    assert_eq!(sel, 0, "翻页后高亮应归零");

    let page2_first = coord.debug_page_texts()[0].clone();
    assert_ne!(page1_first, page2_first, "第 2 页首项应不同于第 1 页首项");

    // 第 2 页按数字键 '1' → 上屏第 2 页的首项（编号重置）
    match coord.handle_key_event(&key_event(0x31, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, page2_first, "第 2 页数字键 1 应上屏第 2 页首项");
        }
        other => panic!("数字键应上屏，实际: {:?}", other),
    }
}

#[test]
fn test_page_up_wraps_at_first_page() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a');
    // 第 1 页按 PageUp 应保持在第 1 页（不越界）
    coord.handle_key_event(&key_event(0x21, EVENT_KEY_DOWN));
    let (page, _, _) = coord.debug_page_info();
    assert_eq!(page, 0, "首页 PageUp 应仍在首页");
}

#[test]
fn test_minus_equal_paging_when_candidates() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a');
    let (_, _, total_pages) = coord.debug_page_info();
    if total_pages < 2 {
        return;
    }
    // '=' (0xBB) 下一页
    coord.handle_key_event(&key_event(0xBB, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_page_info().0, 1, "'=' 应翻到下一页");
    // '-' (0xBD) 上一页
    coord.handle_key_event(&key_event(0xBD, EVENT_KEY_DOWN));
    assert_eq!(coord.debug_page_info().0, 0, "'-' 应翻回上一页");
}

#[test]
fn test_second_third_candidate_keys() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a');
    let texts = coord.debug_page_texts();
    if texts.len() < 3 {
        eprintln!("跳过：当前页候选不足 3 个");
        return;
    }
    let second = texts[1].clone();

    // 分号(;, VK_OEM_1=0xBA) → 上屏第 2 个候选
    match coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, second, "分号应上屏第 2 个候选");
        }
        other => panic!("分号应上屏次选候选，实际: {:?}", other),
    }

    // 重新输入，引号(', VK_OEM_7=0xDE) → 上屏第 3 个候选
    press_letter(&coord, 'a');
    let texts2 = coord.debug_page_texts();
    let third = texts2[2].clone();
    match coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, third, "引号应上屏第 3 个候选");
        }
        other => panic!("引号应上屏三选候选，实际: {:?}", other),
    }
}

#[test]
fn test_empty_buffer_semicolon_enters_quick_input() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    // 空缓冲下按分号 → 进入快捷输入（分号是默认快捷输入触发键），组合区前缀 ";"
    match coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)) {
        KeyAction::UpdateComposition { text, .. } => {
            assert_eq!(text, ";", "空缓冲分号应进入快捷输入显示前缀");
        }
        other => panic!("空缓冲分号应进入快捷输入，实际: {:?}", other),
    }
}

#[test]
fn test_temp_pinyin_backtick_trigger_and_commit() {
    if !has_schemas() {
        return;
    }
    // 五笔方案下，反引号(`, VK_OEM_3=0xC0)触发临时拼音
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    assert_eq!(coord.active_schema_id(), "wubi86");

    // 按反引号进入临时拼音，组合区应显示前缀 "`"
    let act = coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    let preedit = action_text(&act).expect("反引号应进入临时拼音并返回组合区");
    assert_eq!(preedit, "`", "进入临时拼音组合区应为前缀 `");

    // 输入拼音 nihao
    let mut last = act;
    for c in "nihao".chars() {
        last = press_letter(&coord, c);
    }
    let preedit = action_text(&last).unwrap();
    assert_eq!(
        preedit, "`ni'hao",
        "临时拼音组合区应为 `ni'hao，实际: {}",
        preedit
    );

    // 候选应来自拼音引擎（含 你好）
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t.contains("你好")),
        "临时拼音候选应含 你好，实际: {:?}",
        texts
    );

    // 空格上屏首选并退出临时拼音
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert!(text.contains("你好"), "应上屏 你好，实际: {}", text);
        }
        other => panic!("空格应上屏候选，实际: {:?}", other),
    }

    // 退出后五笔输入应恢复正常（输入 a 显示编码 a）
    let act = press_letter(&coord, 'a');
    assert_eq!(action_text(&act).unwrap(), "a", "退出临时拼音后五笔应正常");
}

/// 回归：`` ` `` 与 z **同绑** `temp_pinyin` 时，用 `` ` `` 进临拼后按 z 必须累积拼音。
///
/// 缺陷形态：「进入键二次按下」那条分支的判据只问得出「这个键是不是**某个**临拼引导键」
/// （临拼是全局单例、没有 special / mix 那样的实例 id），产出却取 `temp_pinyin_prefix`
/// ——进入时按的那个键。两者不同源，于是按 z 被判成二次按下、上屏了进入键的 `·`，
/// z 开头的拼音（zi / zuo / zhang）一个都打不出来。
///
/// 判据取「候选含 张」而非组合区文本：组合区 `` `z `` 在「z 入了拼音缓冲」与别的形态下
/// 可能同形，只有真跑到拼音引擎才出得来汉字。
#[test]
fn test_temp_pinyin_other_trigger_key_letter_accumulates_pinyin() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    // z 也绑临拼——与出厂已绑 backtick 的全局配置构成「两个键同绑同一动作」。
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let act = press_vk(&coord, 0xC0, false);
    assert_eq!(
        action_text(&act).as_deref(),
        Some("`"),
        "反引号应进入临时拼音"
    );

    let act = press_letter(&coord, 'z');
    assert!(
        !matches!(act, KeyAction::InsertText { .. }),
        "临拼内按 z 不应上屏（曾被判成进入键二次按下、上屏 `·`），实际: {:?}",
        act
    );
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_pinyin"),
        "按 z 后应仍在临拼内"
    );
    for c in "hang".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t.contains("张")),
        "临拼 zhang 候选应含 张，实际: {:?}",
        texts
    );
}

/// 反向对照：同一份「两个键同绑临拼」的配置下，按**进入用的那个键**照旧上屏中文标点并退出。
/// 少了这条，上面那个测试把整条「二次按下」分支删掉也照样绿。
#[test]
fn test_temp_pinyin_own_trigger_key_still_commits_punct() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    press_vk(&coord, 0xC0, false); // 进入
    match press_vk(&coord, 0xC0, false) {
        // 二次按下
        KeyAction::InsertText { text, .. } => {
            assert!(!text.is_empty(), "二次按反引号应上屏标点");
        }
        other => panic!("二次按反引号应上屏标点并退出，实际: {:?}", other),
    }
    assert_eq!(coord.debug_active_mode(), None, "上屏标点后应已退出临拼");
}

#[test]
fn test_temp_pinyin_commit_and_enter_with_candidates() {
    if !has_schemas() {
        return;
    }
    // 五笔下已有候选时按反引号 → 顶屏高亮候选 + 进入临时拼音
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a');
    let first = coord.debug_page_texts()[0].clone();

    // 反引号：应上屏当前高亮候选并进入临时拼音。默认 top_commit_mode=direct_commit：
    // 真提交候选、前缀新组合延迟到触发键 keyup 才开（与顶码上屏同一分流）。
    match coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN)) {
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            ..
        } => {
            assert_eq!(commit_text, first, "应真提交当前高亮候选");
            assert_eq!(deferred_composition, "`", "延迟新组合应为临时拼音前缀");
        }
        other => panic!("有候选按反引号应顶屏+进临时拼音，实际: {:?}", other),
    }

    // 现已在临时拼音模式：输入拼音 nihao 应得拼音候选
    let mut last = KeyAction::PassThrough;
    for c in "nihao".chars() {
        last = press_letter(&coord, c);
    }
    assert_eq!(action_text(&last).unwrap(), "`ni'hao", "应处于临时拼音模式");
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert!(text.contains("你好")),
        other => panic!("空格应上屏拼音候选，实际: {:?}", other),
    }
}

#[test]
fn test_temp_pinyin_esc_exits() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    press_letter(&coord, 'n');
    // Esc 退出
    match coord.handle_key_event(&key_event(0x1B, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!("Esc 应清空组合区退出，实际: {:?}", other),
    }
    // 退出后五笔正常
    let act = press_letter(&coord, 'a');
    assert_eq!(action_text(&act).unwrap(), "a");
}

#[test]
fn test_temp_pinyin_not_triggered_in_pinyin_mode() {
    if !has_schemas() {
        return;
    }
    // 拼音方案下反引号不应触发临时拼音（仅码表/混输方案启用）。
    // 注：旧断言是 assert_ne!(txt, "`ni")——进临拼时根本不会产出该串，故恒真、从未真正设防，
    // 判据缺失（引导键分支无引擎类型检查）多年未被发现。现直接断言"未进入临拼模式"。
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    let act = coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    assert!(
        !coord.debug_in_temp_pinyin(),
        "拼音方案不应进入临时拼音，实际 act={act:?}"
    );
    // 且反引号应作标点上屏（不被模式吞掉）。
    let txt = action_text(&act).unwrap_or_default();
    assert!(
        txt.contains('`') || txt.contains('·'),
        "反引号应作标点输出，实际: {txt:?}"
    );
}

/// 组合意外终止（鼠标点击移光标 / 焦点切换 / 宿主强制 EndComposition）必须整体复位
/// overlay 模式，不能只清 input_buffer——临拼/快捷的缓冲与前缀不在 input_buffer 里。
/// 真机现象（回归）：` 进临拼后点鼠标移光标，再按 d 组合区显示 `d（模式残留）。
#[test]
fn test_composition_terminated_resets_overlay_modes() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));

    // ` 进入临时拼音
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN));
    assert!(coord.debug_in_temp_pinyin(), "反引号应进入临时拼音");

    // 宿主终止组合（鼠标点击移光标）
    coord.handle_composition_terminated();
    assert!(
        !coord.debug_in_temp_pinyin(),
        "组合终止后不应残留临时拼音模式"
    );

    // 再按 d：应走普通输入（五笔码），而非临拼的 `d
    let act = press_vk(&coord, 0x44, false);
    let txt = match &act {
        KeyAction::UpdateComposition { text, .. } => text.clone(),
        _ => String::new(),
    };
    assert!(
        !txt.starts_with('`'),
        "终止后按键不应续在临拼前缀上: {txt:?}"
    );
}

/// 按下一个字符键（vk + 可选 shift）
fn press_vk(coord: &Coordinator, vk: u32, shift: bool) -> KeyAction {
    let mut ev = key_event(vk, EVENT_KEY_DOWN);
    if shift {
        ev.modifiers = 0x0001;
    }
    coord.handle_key_event(&ev)
}

/// 按**字符**敲键：按美式主键盘布局还原成 `(vk, shift)`。只覆盖测试用到的 ASCII 子集，
/// 未覆盖的字符直接 panic —— 与其静默按错键位产出一个看不懂的断言失败，不如当场报出来。
fn press_char(coord: &Coordinator, c: char) -> KeyAction {
    let (vk, shift) = match c {
        'a'..='z' => ((c.to_ascii_uppercase() as u32) & 0xFF, false),
        'A'..='Z' => ((c as u32) & 0xFF, true),
        '0'..='9' => (c as u32, false),
        '-' => (0xBD, false),
        '_' => (0xBD, true),
        ',' => (0xBC, false),
        '<' => (0xBC, true),
        '.' => (0xBE, false),
        '>' => (0xBE, true),
        '=' => (0xBB, false),
        '+' => (0xBB, true),
        '*' => (0x38, true),
        '(' => (0x39, true),
        ')' => (0x30, true),
        ';' => (0xBA, false),
        ':' => (0xBA, true),
        '\'' => (0xDE, false),
        '"' => (0xDE, true),
        other => panic!("press_char 未覆盖字符 {:?}", other),
    };
    press_vk(coord, vk, shift)
}

/// 依次敲入一串字符，返回最后一次按键的动作。
fn press_str(coord: &Coordinator, s: &str) -> KeyAction {
    let mut last = KeyAction::Consumed;
    for c in s.chars() {
        last = press_char(coord, c);
    }
    last
}

/// 快捷输入模式下切中英文：与临拼一致，遵循 keys.commit_on_switch —— 开启（默认）时把
/// 剩余原码上屏（前缀 ; 不输出），而非无条件清空（回归保护：独占分支曾对 mix 恒返回空串）。
#[test]
fn test_quick_input_commit_on_mode_switch() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.commit_on_switch = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入快捷输入
    press_vk(&coord, 0x31, false); // 1
    press_vk(&coord, 0xBB, true); // +
    press_vk(&coord, 0x32, false); // 2
    // 左 Shift 释放：中→英切换，commit_on_switch=true → 上屏原码 1+2（前缀 ; 不输出）。
    let act = coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    let text = action_text(&act).unwrap_or_default();
    assert_eq!(
        text, "1+2",
        "快捷输入切英文应按 commit_on_switch 上屏原码 1+2，实际: {:?}",
        act
    );
    assert!(!coord.is_chinese_mode(), "左 Shift 应切到英文");
}

/// 关闭 commit_on_switch 时：快捷输入切中英文应清空，不上屏原码。
#[test]
fn test_quick_input_no_commit_on_mode_switch_when_disabled() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.commit_on_switch = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入快捷输入
    press_vk(&coord, 0x31, false); // 1
    press_vk(&coord, 0xBB, true); // +
    press_vk(&coord, 0x32, false); // 2
    let act = coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    let text = action_text(&act).unwrap_or_default();
    assert!(
        text.is_empty(),
        "commit_on_switch=false 时快捷输入切换应清空，实际上屏: {:?}",
        text
    );
}

#[test]
fn test_quick_input_calc() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // 分号(;, VK_OEM_1=0xBA)进入快捷输入，组合区前缀 ";"
    let act = coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN));
    assert_eq!(action_text(&act).unwrap(), ";", "分号应进入快捷输入");

    // 输入 1+2*3：1(0x31) +(Shift+=,0xBB) 2(0x32) *(Shift+8,0x38) 3(0x33)
    press_vk(&coord, 0x31, false);
    press_vk(&coord, 0xBB, true); // +
    press_vk(&coord, 0x32, false);
    press_vk(&coord, 0x38, true); // *
    let last = press_vk(&coord, 0x33, false);
    assert_eq!(action_text(&last).unwrap(), ";1+2*3", "组合区应为 ;1+2*3");

    // 首选是**结果**（用算式形态的是少数），等式次之，随后是结果的金额读法
    let texts = coord.debug_page_texts();
    assert_eq!(texts[0], "7", "计算首选应为结果，实际: {:?}", texts);
    assert_eq!(texts[1], "1+2*3=7", "等式形态应为次选，实际: {:?}", texts);
    assert!(
        texts.contains(&"柒元整".to_string()),
        "计算结果应同时给出金额读法，实际: {:?}",
        texts
    );

    // 字母 a 选第 1 个候选上屏
    match press_vk(&coord, 0x41, false) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "7"),
        other => panic!("字母 a 应上屏首选，实际: {:?}", other),
    }
}

/// 幂运算 `^`（Shift+6）：优先级高于乘除，结果作首选。
#[test]
fn test_quick_input_power_operator() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_vk(&coord, 0x32, false); // 2
    press_vk(&coord, 0xBB, true); // +
    press_vk(&coord, 0x33, false); // 3
    press_vk(&coord, 0x36, true); // ^ (Shift+6)
    press_vk(&coord, 0x32, false); // 2
    let texts = coord.debug_page_texts();
    assert_eq!(texts[0], "11", "2+3^2 应先算幂（=2+9），实际: {:?}", texts);
    assert_eq!(texts[1], "2+3^2=11", "实际: {:?}", texts);
}

/// 协调器层的归属判据：**一个点归数字、两个点归日期**，两组互斥（见 `has_second_dot`）。
///
/// 逐键走完整链路而非直调 `wind_quick_input`：判据依赖「裁剪前的缓冲」，而缓冲是协调器
/// 攒的——单元测试直接传字符串验不出「协调器有没有原样把点交下去」。
#[test]
fn test_quick_input_date_requires_second_dot() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    for vk in [0x32, 0x30, 0x32, 0x36] {
        press_vk(&coord, vk, false); // 2026
    }
    press_vk(&coord, 0xBE, false); // .
    press_vk(&coord, 0x33, false); // 3
    // 一个点：日期整组让开，只剩数字读法。断言走全量候选，页内看不全。
    let before = coord.debug_all_candidate_texts();
    assert!(
        !before.iter().any(|t| t.contains('月')),
        "2026.3 只有一个点，不该有日期候选，实际: {:?}",
        before
    );
    assert!(
        before.iter().any(|t| t.contains('元')),
        "2026.3 应照常出金额，实际: {:?}",
        before
    );
    press_vk(&coord, 0xBE, false); // 第二个 . —— 归属在此翻转
    let after = coord.debug_page_texts();
    assert!(
        after.contains(&"2026年3月".to_string()),
        "2026.3. 应给出年月候选（第三段为空不得让候选全空），实际: {:?}",
        after
    );
    // 反向：同一步**收掉**金额。尾点被 `trim_pending_tail` 裁掉后 `2026.3` 又是合法
    // 小数，判据若看裁剪后的串，这里会两组同屏。
    let all = coord.debug_all_candidate_texts();
    assert!(
        !all.iter().any(|t| t.contains('元')),
        "2026.3. 不该有金额候选，实际: {:?}",
        all
    );
    assert_eq!(all.len(), 4, "只剩年月四条，实际: {:?}", all);
}

/// 重复上屏（成员 `quick_input.repeat`）：空缓冲时把上次上屏内容作唯一候选，空格再上屏一次。
#[test]
fn test_quick_input_repeat_last_commit() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // 先用快捷输入上屏一次计算结果
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_vk(&coord, 0x31, false); // 1
    press_vk(&coord, 0xBB, true); // +
    press_vk(&coord, 0x32, false); // 2
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "3", "空格应上屏计算结果"),
        other => panic!("空格应上屏首选，实际: {:?}", other),
    }
    // 再进快捷输入：空缓冲应出重复候选
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts,
        vec!["3"],
        "空缓冲应显示上次上屏内容，实际: {:?}",
        texts
    );
    // 空格重复上屏
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "3", "空格应重复上屏"),
        other => panic!("空格应重复上屏，实际: {:?}", other),
    }
}

/// ★ 快捷输入的编码栏拆分**不受成员顺序影响**。
///
/// 组合区形态该由「谁真的给得出分段」决定，而成员顺序管的是候选优先级。判据曾写成
/// 「第一个 `preedit_display` 非空的真实方案」——码表/英文引擎的 `preedit_display` 恒等于
/// 原始输入，于是用户只要把一个码表方案（真实案例是快符 `kf`）排在 `$primary_pinyin`
/// 之前，拼音的拆分串就永远轮不上，表现为「快捷输入里完全没有拆分显示」。
#[test]
fn mix_preedit_split_survives_codetable_member_first() {
    if !has_schemas() {
        return;
    }
    for members in [
        vec!["wubi86", "$primary_pinyin"],
        vec!["$primary_pinyin", "wubi86"],
    ] {
        let mut cfg = config_with("wubi86");
        cfg.schema.mix_modes[0].members = members.iter().map(|s| s.to_string()).collect();
        let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
        coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
        let last = press_str(&coord, "nihao");
        assert_eq!(
            action_text(&last).unwrap_or_default(),
            ";ni'hao",
            "members={:?} 时组合区应显示拼音音节拆分",
            members
        );
    }
}

/// 移除成员即关闭该来源：members 去掉 `quick_input.calc` 后，算式不再产出计算候选
/// （金额来源仍会对结果求值，故这里连 number 一起移除，验证「开关=增删」）。
#[test]
fn test_quick_input_member_removal_disables_source() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0]
        .members
        .retain(|m| m != wind_quick_input::MEMBER_CALC && m != wind_quick_input::MEMBER_NUMBER);
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_vk(&coord, 0x31, false); // 1
    press_vk(&coord, 0xBB, true); // +
    press_vk(&coord, 0x32, false); // 2
    let texts = coord.debug_page_texts();
    assert!(
        texts.is_empty(),
        "移除 calc/number 成员后算式不应有候选，实际: {:?}",
        texts
    );
    // 日期成员仍在：日期照常出候选（证明关的是单个来源而非整个快捷输入）
    let coord2 = {
        let mut c = config_with("wubi86");
        c.schema.mix_modes[0]
            .members
            .retain(|m| m != wind_quick_input::MEMBER_CALC);
        Coordinator::new_headless(c, Some(&data_dir()))
    };
    coord2.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN));
    for vk in [0x31, 0x32] {
        press_vk(&coord2, vk, false); // 12
    }
    press_vk(&coord2, 0xBE, false); // .
    for vk in [0x32, 0x35] {
        press_vk(&coord2, vk, false); // 25
    }
    press_vk(&coord2, 0xBE, false); // 第二个 . —— 一个点归数字，日期要打到这里
    // 断言走全量候选而非当页：这条测的是「来源在不在」，不该顺带绑死候选面的分页与排序。
    let texts2 = coord2.debug_all_candidate_texts();
    assert!(
        texts2.iter().any(|t| t.ends_with("月25日")),
        "date 成员仍在，日期候选应照常产出，实际: {:?}",
        texts2
    );
}

// ─────────────────── 自由输入（free_input）───────────────────
//
// 判据：一个字符若不在**当前透镜的合法字符集**内，它不可能是编码，只能是字面内容 →
// 转 Free 透镜，此后一切可打印键字面入缓冲。判据是缓冲的纯函数，没有切换键。
// 正向用例覆盖四种进入路径（大写字母 / 文本透镜里的符号 / 首字符符号 / 数字透镜里的大写），
// 反向对照锁住 `free_input = off` 与「合法缓冲不受影响」两条底线。

/// 进入路径①：首字符大写字母。任何 member 的查询都在小写域，大写恒越界。
#[test]
fn quick_input_free_uppercase_first_char_enters_literal() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    let last = press_str(&coord, "GetTestData()");
    assert_eq!(
        action_text(&last).unwrap(),
        ";GetTestData()",
        "大小写与括号都应原样进组合区"
    );
    assert_eq!(
        coord.debug_page_texts(),
        vec!["GetTestData()"],
        "自由输入的唯一候选＝所打原文"
    );
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "GetTestData()"),
        other => panic!("空格应上屏原文，实际: {:?}", other),
    }
}

/// 进入路径②：文本透镜里出现符号（`_`）。前半段 `test` 仍是合法拼音/英文编码。
#[test]
fn quick_input_free_symbol_upgrades_text_lens() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "test");
    // 此刻仍是文本透镜：缓冲全小写，成员照常出候选。
    assert!(
        !coord.debug_page_texts().is_empty(),
        "test 是合法编码，文本透镜应有候选"
    );
    let last = press_str(&coord, "_data");
    assert_eq!(
        action_text(&last).unwrap(),
        ";test_data",
        "下划线应字面入缓冲而非顶屏退出"
    );
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "test_data"),
        other => panic!("空格应上屏原文，实际: {:?}", other),
    }
}

/// 进入路径③：首字符就是非表达式符号（`<`）。此前 `<` 会开数字透镜、后续字母被当选词键吞掉。
#[test]
fn quick_input_free_non_expr_first_char_enters_literal() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    let last = press_str(&coord, "<TAB>");
    assert_eq!(
        action_text(&last).unwrap(),
        ";<TAB>",
        "尖括号内的字母不应被数字透镜当成选词键"
    );
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "<TAB>"),
        other => panic!("空格应上屏原文，实际: {:?}", other),
    }
}

/// 进入路径④：数字透镜里出现大写字母。小写字母仍是选词键，大写才越界。
#[test]
fn quick_input_free_uppercase_upgrades_numeric_lens() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "12.5");
    assert!(
        !coord.debug_page_texts().is_empty(),
        "12.5 是合法数字，数字透镜应有候选"
    );
    let last = press_str(&coord, "GB");
    assert_eq!(action_text(&last).unwrap(), ";12.5GB");
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "12.5GB"),
        other => panic!("空格应上屏原文，实际: {:?}", other),
    }
}

/// `-` 在**文本**透镜让位字面输入（否则 `all-in-one` 的连字符会被 `minus_equal` 键组
/// 吃成翻页）。翻页职责转给 PageUp/PageDown。
#[test]
fn quick_input_free_hyphen_is_literal_in_text_lens() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    let last = press_str(&coord, "all-in-one");
    assert_eq!(action_text(&last).unwrap(), ";all-in-one");
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "all-in-one"),
        other => panic!("空格应上屏原文，实际: {:?}", other),
    }
}

/// ★反向对照：`-` 在**数字**透镜仍是减法运算符（它在表达式字符集内，不越界）。
/// 同一个键在两个透镜里归属不同，这正是「越界判据按透镜分」而非全局字符集的理由。
#[test]
fn quick_input_free_hyphen_still_operator_in_numeric_lens() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "1-5");
    let texts = coord.debug_page_texts();
    assert_eq!(texts[0], "-4", "1-5 应作减法求值，实际: {:?}", texts);
}

/// ★反向对照：括号是表达式字符，`(1+2)*3` 不该被误判成自由输入。
#[test]
fn quick_input_free_parens_still_calc() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "(1+2)*3");
    let texts = coord.debug_page_texts();
    assert_eq!(texts[0], "9", "括号算式应照常求值，实际: {:?}", texts);
}

/// ★反向对照：文本透镜里数字键仍是选词键（它是功能键，不参与越界判定）。
#[test]
fn quick_input_free_digits_still_select_in_text_lens() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "nihao");
    let texts = coord.debug_page_texts();
    assert!(!texts.is_empty(), "nihao 应有拼音候选");
    let first = texts[0].clone();
    match press_char(&coord, '1') {
        KeyAction::InsertText { text, .. } => {
            assert!(
                text.ends_with(&first),
                "数字键 1 应选首候选上屏（而非字面输入），实际: {:?}",
                text
            );
        }
        other => panic!("数字键应选词上屏，实际: {:?}", other),
    }
}

/// **行为变更的正反两面**：同一串按键在 `auto` 下变字面、在 `off` 下维持顶屏出中文标点。
///
/// 这是本次改动影响面最大的一条 —— `;nihao,` 不再出「你好，」。用户拍板如此
/// （快捷输入是为特殊内容而进的模式，要标点可以先按空格上屏再打），此处把两种取值
/// 各自锁住，避免日后有人只看到一半就"顺手修好"。
#[test]
fn quick_input_free_comma_literal_vs_off_top_commits() {
    if !has_schemas() {
        return;
    }
    // auto（出厂默认）：逗号字面入缓冲，组合区继续存在。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    let last = press_str(&coord, "nihao,");
    assert_eq!(
        action_text(&last).unwrap(),
        ";nihao,",
        "auto 下逗号应字面入缓冲"
    );

    // off：维持既有「顶屏高亮候选 + 转换后标点 → 退出」。
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
    let coord_off = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord_off.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord_off, "nihao");
    match press_char(&coord_off, ',') {
        KeyAction::InsertText { text, .. } => {
            assert!(
                !text.contains("nihao"),
                "off 下应顶屏候选而非上屏原码，实际: {:?}",
                text
            );
            assert!(
                text.ends_with('，') || text.ends_with(','),
                "off 下应带上转换后的标点，实际: {:?}",
                text
            );
        }
        other => panic!("off 下逗号应顶屏上屏并退出，实际: {:?}", other),
    }
}

/// ★反向对照：`off` 下 `-` 仍是翻页键，不得字面入缓冲。
#[test]
fn quick_input_free_off_keeps_minus_as_page_key() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "nihao");
    let before = coord.debug_page_texts();
    assert!(!before.is_empty(), "nihao 应有拼音候选");
    press_char(&coord, '-');
    // 强断言：`-` 若被当成字面输入，缓冲会变成 `nihao-`，该串查不出拼音候选 → 候选列表
    // 必然改变。仅断言「返回的动作里没有 `-`」是不够的——翻页返回 Consumed 时
    // `action_text` 为 None，断言会被整条跳过，成为一条永远绿的假测试。
    assert_eq!(
        coord.debug_page_texts(),
        before,
        "off 下 `-` 应作翻页键（单页时为空操作），候选不该变化"
    );
    // 再按空格：上屏的应是候选词，而不是把 `nihao-` 当原码吐出来。
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert!(
            !text.contains('-') && !text.contains("nihao"),
            "off 下应上屏候选词，实际: {:?}",
            text
        ),
        other => panic!("空格应上屏候选，实际: {:?}", other),
    }
}

/// Ctrl 组合守卫：`Ctrl+E` 不该被当成字面 `e` 插进缓冲（此前 mix 没有这道守卫）。
#[test]
fn quick_input_ctrl_combo_does_not_insert_literal() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "ni");
    let mut ev = key_event(0x45, EVENT_KEY_DOWN); // E
    ev.modifiers = 0x0002; // MOD_CTRL
    match coord.handle_key_event(&ev) {
        KeyAction::ClearComposition => {}
        other => panic!(
            "有待输入内容时 Ctrl 组合应放弃整段组合，实际: {:?}（回归：曾插入字面 e）",
            other
        ),
    }
}

/// 带 `'` 的英文：`rock'n'roll`。`'` 是默认的第 3 候选键，不夺取就走不到字面输入那一步。
///
/// 修复前实测：`;rock` 按 `'` 选走第 3 候选「日欧」，而它 `consumed_length=2` 还会触发
/// 分步确认——`ro` 被吃掉转成汉字、缓冲只剩 `ck`，组合区变成 `;日欧c'k`
/// （末尾那个 `'` 是拼音引擎的**音节分隔显示**，不是用户打的）。整串输入被打散。
#[test]
fn quick_input_free_apostrophe_word() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    let last = press_str(&coord, "rock'n'roll");
    assert_eq!(
        action_text(&last).unwrap(),
        ";rock'n'roll",
        "撇号应字面入缓冲，而不是被当成第 3 候选键"
    );
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "rock'n'roll"),
        other => panic!("空格应上屏原文，实际: {:?}", other),
    }
}

/// 分号同理（`for(;;)`）。注意首字符 `(` 已经把透镜带进 Free，此处锁的是**缓冲非空时**
/// 的分号不再被当选词键；空缓冲按分号仍是「上屏符号并退出」（引导键二次按下），不受影响。
#[test]
fn quick_input_free_semicolon_in_buffer() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    let last = press_str(&coord, "for(;;)");
    assert_eq!(action_text(&last).unwrap(), ";for(;;)");
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "for(;;)"),
        other => panic!("空格应上屏原文，实际: {:?}", other),
    }
}

/// `Shift+数字` 是 `!@#$%^&*(` 九个符号，**从来不是选词键**。
///
/// 第③步的数字选词键此前没判 shift（第④步一直有），于是 `;for(` 里的 `(`（=Shift+9）
/// 被当成「选第 9 个候选」吃掉。自由输入上线前这些符号本就走不进缓冲，故一直没暴露。
#[test]
fn quick_input_shift_digit_is_symbol_not_select_key() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "abc");
    assert!(
        !coord.debug_page_texts().is_empty(),
        "abc 应有候选——没有候选的话选词键本就不会触发，测不出问题"
    );
    // `!` = Shift+1；若被当成「选第 1 个候选」，这里会得到 InsertText 而非组合区更新。
    let act = press_vk(&coord, 0x31, true);
    assert_eq!(
        action_text(&act).as_deref(),
        Some(";abc!"),
        "Shift+1 应作符号 `!` 字面入缓冲，实际: {:?}",
        act
    );
}

/// ★反向对照：`free_input_takes_select_keys = false` 时 `'` 仍是第 3 候选键。
/// 缺了这条，「夺取」就可能被实现成无条件生效而无人察觉。
#[test]
fn quick_input_select_keys_kept_when_not_taken() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0].free_input_takes_select_keys = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "rock");
    let texts = coord.debug_page_texts();
    assert!(texts.len() >= 3, "需要至少 3 个候选才能验证第 3 候选键");
    let third = texts[2].clone();
    let act = press_vk(&coord, 0xDE, false); // '
    // 第 3 候选若是部分匹配会走分步确认（组合区保留），否则整体上屏；两种都说明它**被选中了**，
    // 而不是作字面入缓冲——故判据是「组合区/上屏文本里出现了第 3 候选的文本」。
    let out = action_text(&act).unwrap_or_default();
    assert!(
        out.contains(&third),
        "关掉夺取后 `'` 应仍选第 3 候选 {:?}，实际动作: {:?}",
        third,
        act
    );
    assert!(
        !out.contains("rock'"),
        "关掉夺取后 `'` 不应字面入缓冲，实际: {:?}",
        out
    );
}

/// ★★ 用户报「已把自由输入设为 `off`，`;` / `'` 仍不能选第 2/3 候选」——本测试是该报告的
/// 判据：`off` 下 `free_on = false`，第④步的夺取条件（`free_on && takes`）整体不成立，
/// 二三候选键必须**原样保留**。
///
/// 与 `quick_input_select_keys_kept_when_not_taken` 刻意分开：那条关的是 `takes` 子开关
/// （`free_input` 仍是 `auto`），本条关的是 `free_input` 总开关。两个开关各有各的短路点，
/// 只测一个的话另一个若被写成「无条件夺取」不会被察觉。
#[test]
fn quick_input_off_keeps_select_keys() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入快捷输入
    press_str(&coord, "rock");
    let texts = coord.debug_page_texts();
    assert!(texts.len() >= 3, "需要至少 3 个候选，实际: {:?}", texts);
    let third = texts[2].clone();
    let act = press_vk(&coord, 0xDE, false); // '
    // 判据同 `quick_input_select_keys_kept_when_not_taken`：第 3 候选若是部分匹配会走分步
    // 确认（组合区保留）、否则整体上屏，两种都说明它被选中了；而字面入缓冲会得到 `rock'`。
    let out = action_text(&act).unwrap_or_default();
    assert!(
        out.contains(&third),
        "off 下 `'` 应仍选第 3 候选 {:?}，实际动作: {:?}",
        third,
        act
    );
    assert!(
        !out.contains("rock'"),
        "off 下 `'` 不应字面入缓冲，实际: {:?}",
        out
    );
}

/// 同上，测 `;`（第 2 候选键）。**必须与 `'` 分开测**：`;` 同时是本 mix 的引导键，
/// 它在第④步之前还要过一道「进入键二次按下 → 上屏符号并退出」的判定（`handle_mode.rs`），
/// 那道判定只要求缓冲空，若哪天守卫写漏，`'` 绿着而 `;` 是红的。
#[test]
fn quick_input_off_keeps_semicolon_as_second_select_key() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入快捷输入
    press_str(&coord, "rock");
    let texts = coord.debug_page_texts();
    assert!(texts.len() >= 2, "需要至少 2 个候选，实际: {:?}", texts);
    let second = texts[1].clone();
    let act = press_vk(&coord, 0xBA, false); // ;
    let out = action_text(&act).unwrap_or_default();
    assert!(
        out.contains(&second),
        "off 下 `;` 应仍选第 2 候选 {:?}，实际动作: {:?}",
        second,
        act
    );
}

/// ★★★ **数字透镜**下的二三候选键——用户报「数字输入模式下 `;` / `'` 选不了候选，
/// 把自由输入设成 `off` 也没用」的回归锁。
///
/// 根因不在夺取判据，而在第①步的口径：`mix_numeric_input_char` 收的是「一切非字母可打印
/// 字符」，比本透镜 `accepts` 的 `is_expr_char` 宽得多，`;` `'` 被当表达式字符吞进缓冲并
/// `return`，第④步成了不可达代码 —— `free_on` 的判定在④，救不回来。
///
/// 与文本透镜那两条（`quick_input_off_keeps_*`）**必须分开测**：它们走的是①的不同臂，
/// 文本臂只收字母、天然让开，全绿也说明不了数字臂的死活。
#[test]
fn quick_input_numeric_lens_off_keeps_select_keys() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "123"); // 首字符是数字 → 数字透镜
    let texts = coord.debug_page_texts();
    assert!(
        texts.len() >= 2,
        "数字透镜需要至少 2 个候选才能验证第 2 候选键，实际: {:?}",
        texts
    );
    let second = texts[1].clone();
    let act = press_vk(&coord, 0xBA, false); // ;
    let out = action_text(&act).unwrap_or_default();
    assert!(
        out.contains(&second),
        "数字透镜 + off 下 `;` 应选第 2 候选 {:?}，实际动作: {:?}",
        second,
        act
    );
    assert!(
        !out.contains("123;"),
        "`;` 不应被当表达式字符吞进缓冲，实际: {:?}",
        out
    );
}

/// 同上，`'` = 第 3 候选键。与 `;` 分开测：`;` 还是本 mix 的引导键，两者在①之前经过的
/// 判定不同。
#[test]
fn quick_input_numeric_lens_off_keeps_quote_as_third_select_key() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "123");
    let texts = coord.debug_page_texts();
    assert!(texts.len() >= 3, "需要至少 3 个候选，实际: {:?}", texts);
    let third = texts[2].clone();
    let act = press_vk(&coord, 0xDE, false); // '
    let out = action_text(&act).unwrap_or_default();
    assert!(
        out.contains(&third),
        "数字透镜 + off 下 `'` 应选第 3 候选 {:?}，实际动作: {:?}",
        third,
        act
    );
}

/// ★★ **反向对照，缺了它这次改造就等于「无条件让开」而无人察觉**：出厂默认
/// （`auto` + `takes = true`）下，数字透镜的 `;` 仍必须字面入缓冲。
///
/// 让①让开是有条件的——夺取生效时本臂不让，行为逐字节不变。
#[test]
fn quick_input_numeric_lens_auto_still_takes_select_keys() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "123");
    let act = press_vk(&coord, 0xBA, false); // ;
    assert_eq!(
        action_text(&act).as_deref(),
        Some(";123;"),
        "出厂默认（auto + 夺取）下数字透镜的 `;` 应字面入缓冲，实际: {:?}",
        act
    );
}

/// `free_input_takes_select_keys = false` 在**数字透镜**下同样要生效。
///
/// 这条修好的是一个连带缺陷：该开关此前只在文本透镜有效，数字透镜下 `;` `'` 在①就被吞了，
/// 关掉夺取也没用 —— 开关的声明语义（「保住 `;`/`'` 的选词手感」）只兑现了一半。
#[test]
fn quick_input_numeric_lens_respects_takes_select_keys_off() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0].free_input_takes_select_keys = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "123");
    let texts = coord.debug_page_texts();
    assert!(texts.len() >= 2, "需要至少 2 个候选，实际: {:?}", texts);
    let second = texts[1].clone();
    let act = press_vk(&coord, 0xBA, false); // ;
    let out = action_text(&act).unwrap_or_default();
    assert!(
        out.contains(&second),
        "关掉夺取后数字透镜的 `;` 应选第 2 候选 {:?}，实际动作: {:?}",
        second,
        act
    );
}

/// ★反向对照：数字键**不在**夺取范围——它是文本透镜唯一的选词通路。
/// 这条同时钉住了已知缺口：`;utf8` 里的 `8` 仍会选词，要打这类串需先切进自由输入。
#[test]
fn quick_input_digit_select_keys_are_never_taken() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "nihao");
    let texts = coord.debug_page_texts();
    assert!(!texts.is_empty(), "nihao 应有拼音候选");
    let first = texts[0].clone();
    match press_char(&coord, '1') {
        KeyAction::InsertText { text, .. } => assert!(
            text.ends_with(&first),
            "数字键必须始终是选词键（夺取范围只含二三候选键），实际: {:?}",
            text
        ),
        other => panic!("数字键应选词上屏，实际: {:?}", other),
    }
}

/// Ctrl 守卫的**模式处理器全覆盖**：五个独占模式（mix / 临拼 / 临英 / 特殊 / URL）都必须
/// 把 Ctrl 组合当成宿主快捷键，而不是普通输入。
///
/// 盘查依据是「枚举 `match state.active` 的全部分支，逐个问它接了吗」——只 grep `MOD_CTRL`
/// 会得出「已经实现了」的错误结论：全仓唯一一处命中在**临拼的字母臂**（`handle_temp.rs`），
/// 而临拼自己的数字臂/标点臂、以及 mix / 临英 / 特殊 / URL 四个模式全都没有。
/// 已接线的调用点无法告诉你漏了谁。
///
/// ⚠️ 本测试只覆盖 mix / 临拼 / 临英三个能在 headless 里稳定构造的模式；特殊模式需要
/// 配置码表方案、URL 模式需要 `input.url.prefixes` 命中，二者的守卫接线与这三个同构
/// （同一个 `overlay_ctrl_alt_guard`），但**未被本测试覆盖**。
#[test]
fn overlay_modes_treat_ctrl_combo_as_host_shortcut() {
    if !has_schemas() {
        return;
    }
    let ctrl_e = || {
        let mut ev = key_event(0x45, EVENT_KEY_DOWN); // E
        ev.modifiers = 0x0002; // MOD_CTRL
        ev
    };
    // 三个模式共用一个 Coordinator：引擎 reader / LRU 跨实例共享且带配额语义，一个测试里
    // 建多个 Coordinator 会与并行跑的其它测试争用，导致**无关测试**偶发红。
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // ① mix（快捷输入）
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    press_str(&coord, "ni");
    assert!(
        matches!(
            coord.handle_key_event(&ctrl_e()),
            KeyAction::ClearComposition
        ),
        "mix 下 Ctrl 组合应放弃整段组合（回归：曾插入字面 e）"
    );

    // ② 临时拼音（五笔方案下 z 引导）
    press_vk(&coord, 0x5A, false); // z
    assert!(coord.debug_in_temp_pinyin(), "z 应进入临时拼音");
    press_str(&coord, "ni");
    assert!(
        matches!(
            coord.handle_key_event(&ctrl_e()),
            KeyAction::ClearComposition
        ),
        "临拼下 Ctrl 组合应放弃整段组合"
    );

    // ③ 临时英文（Shift+字母进入）。此前只有字母臂判了 Ctrl，此处锁住入口处的统一守卫。
    press_vk(&coord, 0x48, true); // Shift+H
    press_str(&coord, "el");
    assert!(
        matches!(
            coord.handle_key_event(&ctrl_e()),
            KeyAction::ClearComposition
        ),
        "临英下 Ctrl 组合应放弃整段组合"
    );
}

/// 空缓冲时 Ctrl 组合应**透传**给宿主，而不是被模式吞掉。
#[test]
fn overlay_ctrl_combo_passes_through_on_empty_buffer() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入 mix，缓冲空
    let mut ev = key_event(0x41, EVENT_KEY_DOWN); // A
    ev.modifiers = 0x0002; // MOD_CTRL
    assert!(
        matches!(coord.handle_key_event(&ev), KeyAction::PassThrough),
        "空缓冲时 Ctrl+A 应透传给宿主（让宿主自己全选），不该被模式消费"
    );
}

/// `free_input = always` 的实例：引导键本身也必须能作字面字符打进缓冲。
/// 否则「进模式后第一个想打的就是引导符」永远只会上屏符号并退出。
#[test]
fn quick_input_free_always_allows_trigger_key_as_literal() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Always;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入
    let last = press_str(&coord, ";;");
    assert_eq!(
        action_text(&last).unwrap(),
        ";;;",
        "always 实例下引导键应字面入缓冲（组合区＝前缀 ; + 缓冲 ;;）"
    );
}

/// 格式表必须随 `data/` 部署，且部署的就是出厂表。
///
/// 缺文件时 `FormatTable::load` 会**静默回落内置默认表**——候选照常、日志只有一条 warn，
/// 但用户拿不到那份可拷贝的样板文件，「自定义」这个特性等于不存在。打包漏文件是这类
/// 数据文件的典型故障（`data/` 是整目录复制，新增文件本不该漏，但没有测试就没人知道）。
#[test]
fn test_quick_format_table_is_deployed() {
    if !has_schemas() {
        return;
    }
    let p = data_dir().join("system.quick.toml");
    assert!(
        p.is_file(),
        "system.quick.toml 未随 data/ 部署到 {}",
        p.display()
    );
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(
        wind_quick_input::FormatTable::parse(&text).unwrap(),
        wind_quick_input::FormatTable::builtin(),
        "部署出去的格式表与内置默认表不一致"
    );
}

/// 打开快捷输入并输入一串字符（`;` + chars）。
fn quick_type(coord: &Coordinator, s: &str) {
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    for c in s.chars() {
        let vk = match c {
            '0'..='9' => 0x30 + (c as u32 - '0' as u32),
            '.' => 0xBE,
            _ => panic!("quick_type 不支持字符 {c:?}"),
        };
        press_vk(coord, vk, false);
    }
}

fn quick_coord(store_tag: &str) -> (std::sync::Arc<Coordinator>, std::path::PathBuf) {
    let p = std::env::temp_dir().join(format!("wind_quickfmt_{store_tag}.redb"));
    let _ = std::fs::remove_file(&p);
    let store = std::sync::Arc::new(wind_store::Store::open(&p).unwrap());
    let coord =
        Coordinator::new_headless_with_store(config_with("wubi86"), Some(&data_dir()), store);
    (coord, p)
}

/// ★ 右键调序对**这一类的所有输入**生效，而不是只对当下这一个日期。
///
/// 这是格式调整与候选调整（shadow）最本质的区别：shadow 的键是 `(方案, 输入码)`，
/// 若把格式调整存进去，用户调完换个日期就失效——症状是「当时有效、隔天失效」，
/// 间歇性发作且日志干净。本用例正是为钉住这条边界而写。
#[test]
fn test_quick_format_move_top_applies_to_other_inputs() {
    if !has_schemas() {
        return;
    }
    use wind_ui_types::CandidateOp;
    let (coord, p) = quick_coord("movetop");

    quick_type(&coord, "2026.6.19");
    let before = coord.debug_page_texts();
    let lunar_idx = before
        .iter()
        .position(|t| t.starts_with("农历"))
        .unwrap_or_else(|| panic!("日期候选应含农历，实际: {before:?}"));
    assert!(lunar_idx > 0, "农历出厂不在首位，实际: {before:?}");

    // 右键「这种格式排最前」
    coord.debug_candidate_op(CandidateOp::MoveTop, lunar_idx);
    let after = coord.debug_page_texts();
    assert!(
        after[0].starts_with("农历"),
        "调序应当场生效（不必重启），实际: {after:?}"
    );

    // ★ 换一个完全不同的日期：调整仍在
    coord.handle_key_event(&key_event(0x1B, EVENT_KEY_DOWN)); // Esc 退出
    quick_type(&coord, "2025.12.25");
    let other = coord.debug_page_texts();
    assert!(
        other[0].starts_with("农历"),
        "格式调整应作用于整类输入，换个日期就失效说明存错了落点，实际: {other:?}"
    );
    let _ = std::fs::remove_file(&p);
}

/// 「不再显示这种格式」→ 该条消失，其余不受影响；「恢复本类默认」把它带回来。
#[test]
fn test_quick_format_disable_and_reset() {
    if !has_schemas() {
        return;
    }
    use wind_ui_types::CandidateOp;
    let (coord, p) = quick_coord("disable");

    quick_type(&coord, "2026.6.19");
    let before = coord.debug_page_texts();
    let n_before = before.len();
    let lunar_idx = before.iter().position(|t| t.starts_with("农历")).unwrap();

    coord.debug_candidate_op(CandidateOp::Delete, lunar_idx);
    let after = coord.debug_page_texts();
    assert!(
        !after.iter().any(|t| t.starts_with("农历")),
        "停用后该条不该出现，实际: {after:?}"
    );
    assert_eq!(after.len(), n_before - 1, "只少这一条");
    assert_eq!(after[0], before[0], "首选不受影响");

    // 恢复本类默认：停用的格式点不到了，只能整类重置——这正是它必须存在的理由
    coord.debug_candidate_op(CandidateOp::Reset, 0);
    let back = coord.debug_page_texts();
    assert_eq!(back, before, "恢复后应与出厂逐条逐序相同");
    let _ = std::fs::remove_file(&p);
}

/// 调整只作用于本类：调了日期不影响数字。
#[test]
fn test_quick_format_adjust_does_not_leak_across_kinds() {
    if !has_schemas() {
        return;
    }
    use wind_ui_types::CandidateOp;
    let (coord, p) = quick_coord("kinds");

    quick_type(&coord, "123");
    let num_before = coord.debug_page_texts();
    coord.handle_key_event(&key_event(0x1B, EVENT_KEY_DOWN));

    quick_type(&coord, "2026.6.19");
    let d = coord.debug_page_texts();
    let lunar_idx = d.iter().position(|t| t.starts_with("农历")).unwrap();
    coord.debug_candidate_op(CandidateOp::MoveTop, lunar_idx);
    coord.handle_key_event(&key_event(0x1B, EVENT_KEY_DOWN));

    quick_type(&coord, "123");
    assert_eq!(
        coord.debug_page_texts(),
        num_before,
        "改日期的顺序不该动到数字类"
    );
    let _ = std::fs::remove_file(&p);
}

#[test]
fn test_quick_input_date_space_commits() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    // 输入 2025.12.25
    for vk in [0x32, 0x30, 0x32, 0x35] {
        press_vk(&coord, vk, false);
    }
    press_vk(&coord, 0xBE, false); // .
    for vk in [0x31, 0x32] {
        press_vk(&coord, vk, false);
    }
    press_vk(&coord, 0xBE, false); // .
    for vk in [0x32, 0x35] {
        press_vk(&coord, vk, false);
    }
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t == "2025年12月25日"),
        "日期候选应含 2025年12月25日，实际: {:?}",
        texts
    );
    // 中文日期是首选（中文输入法场景下最常用），全汉字写法次之，且不产出补零的中文写法
    assert_eq!(texts[0], "2025年12月25日", "实际: {:?}", texts);
    // 判据含「日」字以排除农历那两条（农历日名是「初六」「廿九」，不带「日」）——
    // 农历候选也含「年」，只按「年」过滤会把两类混在一起数。
    let cn: Vec<&str> = texts
        .iter()
        .filter(|t| t.contains('年') && t.contains('日'))
        .map(|s| s.as_str())
        .collect();
    assert_eq!(
        cn,
        vec!["2025年12月25日", "二〇二五年十二月二十五日"],
        "公历中文日期恰两条：阿拉伯数字式与全汉字式，均不补零（补零写法不合 GB/T 15835），实际: {:?}",
        texts
    );
    // 农历两条追加在公历之后，不得挤占首选
    assert_eq!(
        &texts[5..7],
        ["农历冬月初六", "乙巳年冬月初六"],
        "农历两条应排在公历五条之后，实际: {:?}",
        texts
    );
    // 空格上屏高亮（首选）
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "2025年12月25日"),
        other => panic!("空格应上屏日期首选，实际: {:?}", other),
    }
}

#[test]
fn test_quick_input_double_semicolon_outputs_literal() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入
    // 再按 ; → 按标点配置上屏（默认中文标点 → ；）并退出
    match coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "；", "双分号应按中文标点上屏 ；"),
        other => panic!("双分号应上屏标点，实际: {:?}", other),
    }
    // 退出后五笔正常
    let act = press_letter(&coord, 'a');
    assert_eq!(action_text(&act).unwrap(), "a");
}

#[test]
fn test_quick_input_colon_enters_numeric_symbol_lens() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入快捷输入

    // 冒号与分号共用 VK_OEM_1，但带 Shift；它应作为数字/符号输入进入 mix 缓冲，
    // 不应被误判为“触发键二次按下”而直接上屏中文冒号。
    match press_vk(&coord, 0xBA, true) {
        KeyAction::UpdateComposition { text, .. } => {
            assert_eq!(text, ";:", "冒号应进入快捷输入数字/符号模式");
        }
        other => panic!("冒号应更新快捷输入组合区，实际: {:?}", other),
    }
}

#[test]
fn test_quick_input_esc_exits() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN));
    press_vk(&coord, 0x31, false);
    match coord.handle_key_event(&key_event(0x1B, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!("Esc 应退出快捷输入，实际: {:?}", other),
    }
}

#[test]
fn test_semicolon_still_selects_second_candidate_with_candidates() {
    if !has_schemas() {
        return;
    }
    // 有候选时分号仍应作二三候选（不进入快捷输入）
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a');
    let texts = coord.debug_page_texts();
    if texts.len() < 2 {
        return;
    }
    let second = texts[1].clone();
    match coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, second, "有候选时分号应选第 2 个候选");
        }
        other => panic!("有候选时分号应作二三候选，实际: {:?}", other),
    }
}

// ───── 模式键空缓冲回车上屏触发符号本身（仅空缓冲场景，补输被模式键占用的符号）─────

#[test]
fn test_quick_input_empty_enter_outputs_trigger_symbol() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // 分号进入快捷输入（空缓冲），随即按回车 → 原样上屏触发符号 ;（不按中英标点转换）
    let act = coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    assert_eq!(action_text(&act).unwrap(), ";", "分号应进入快捷输入");
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, ";", "空缓冲回车应原样上屏触发符号 ;（非中文 ；）");
        }
        other => panic!("空缓冲回车应上屏触发符号，实际: {:?}", other),
    }
    // 退出后五笔输入恢复正常
    let act = press_letter(&coord, 'a');
    assert_eq!(
        action_text(&act).unwrap(),
        "a",
        "回车上屏后应已退出快捷输入"
    );
}

#[test]
fn test_temp_pinyin_empty_enter_outputs_trigger_symbol() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // 反引号进入临时拼音（空缓冲），随即按回车 → 原样上屏触发符号 `
    let act = coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN)); // `
    assert_eq!(action_text(&act).unwrap(), "`", "反引号应进入临时拼音");
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "`", "空缓冲回车应原样上屏触发符号 `");
        }
        other => panic!("空缓冲回车应上屏触发符号，实际: {:?}", other),
    }
    let act = press_letter(&coord, 'a');
    assert_eq!(
        action_text(&act).unwrap(),
        "a",
        "回车上屏后应已退出临时拼音"
    );
}

#[test]
fn test_quick_input_empty_enter_clear_behavior_discards() {
    if !has_schemas() {
        return;
    }
    // enter_behavior=clear：空缓冲回车放弃退出，不上屏任何符号（严格遵循配置）
    let mut cfg = config_with("wubi86");
    cfg.input.enter_behavior = "clear".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ; 进入
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!("clear 模式空缓冲回车应清空退出，实际: {:?}", other),
    }
}

// ───── enter_behavior=clear 在各模式的「非空缓冲」路径同样生效 ─────
//
// 回归保护：此前四个模式 handler 的回车分支都把 enter_behavior 判断写在
// `if buffer.is_empty()` **内部**，于是「打了码再按回车」走非空缓冲路径无条件上屏原码，
// 配置只对「什么都没打就回车」生效。指纹＝空缓冲时配置生效、打了码就失效。
//
// 每个测试都必须先断言「确实进了模式」：触发键若没生效，按键会落到主输入路径，
// 而主输入路径的 clear 同样返回 ClearComposition —— 不验进入就是假绿。

/// 临时拼音打了码再回车：clear 应整段放弃，不上屏拼音原码。
#[test]
fn test_temp_pinyin_nonempty_enter_clear_discards() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.enter_behavior = "clear".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let act = coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN)); // ` 进入临拼
    assert_eq!(action_text(&act).unwrap(), "`", "反引号应进入临时拼音");
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    let disp = action_text(&press_letter(&coord, 'o')).unwrap_or_default();
    assert!(
        disp.starts_with('`'),
        "字母应进临拼缓冲（组合区以 ` 开头），实际: {:?}",
        disp
    );
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!("clear 模式临拼非空缓冲回车应清空不上屏，实际: {:?}", other),
    }
    let act = press_letter(&coord, 'a');
    assert_eq!(action_text(&act).unwrap(), "a", "回车清空后应已退出临拼");
}

/// 对照组：commit 模式（默认）下同样操作仍应上屏原码。
/// 没有它，上面的测试无法区分「配置生效」与「临拼回车本来就不上屏」。
#[test]
fn test_temp_pinyin_nonempty_enter_commit_still_outputs_code() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN)); // `
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "nihao", "commit 模式临拼回车应上屏拼音原码");
        }
        other => panic!("commit 模式临拼回车应上屏原码，实际: {:?}", other),
    }
}

/// 快捷输入（混合模式）打了码再回车：clear 应整段放弃。
#[test]
fn test_quick_input_nonempty_enter_clear_discards() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.enter_behavior = "clear".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let act = coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    assert_eq!(action_text(&act).unwrap(), ";", "分号应进入快捷输入");
    let mut disp = String::new();
    for c in "abc".chars() {
        disp = action_text(&press_letter(&coord, c)).unwrap_or_default();
    }
    assert!(
        disp.starts_with(';'),
        "字母应进快捷输入缓冲，实际组合区: {:?}",
        disp
    );
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!(
            "clear 模式快捷输入非空缓冲回车应清空不上屏，实际: {:?}",
            other
        ),
    }
}

/// 对照组：commit 模式下快捷输入回车仍上屏缓冲原文。
#[test]
fn test_quick_input_nonempty_enter_commit_still_outputs_code() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)); // ;
    for c in "abc".chars() {
        press_letter(&coord, c);
    }
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "abc", "commit 模式快捷输入回车应上屏缓冲原文");
        }
        other => panic!("commit 模式快捷输入回车应上屏原文，实际: {:?}", other),
    }
}

/// 特殊模式打了码再回车：clear 应整段放弃，不上屏编码原文。
#[test]
fn test_special_mode_nonempty_enter_clear_discards() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.enter_behavior = "clear".into();
    let ov = overlay_override_dir(
        "test_special_mode_nonempty_enter_clear_d",
        &[("pinyin", false)],
    );
    bind_special(&mut cfg, "backslash", "pinyin");
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov));
    let act = coord.handle_key_event(&key_event(0xDC, EVENT_KEY_DOWN)); // \
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "反斜杠应进入特殊模式，实际: {:?}",
        act
    );
    let mut disp = String::new();
    for c in "ni".chars() {
        disp = action_text(&press_letter(&coord, c)).unwrap_or_default();
    }
    assert!(
        disp.contains("ni"),
        "字母应进特殊模式编码缓冲，实际组合区: {:?}",
        disp
    );
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!(
            "clear 模式特殊模式非空缓冲回车应清空不上屏，实际: {:?}",
            other
        ),
    }
}

/// 临时英文**豁免** clear：打了内容再回车仍须上屏。
///
/// 临英缓冲装的是英文原文而非「编码」，且 `space_as_input` 开启后空格被占作输入字符、
/// 上屏职责整个压在回车上 —— clear 若管辖非空缓冲，本模式一个上屏通路都不剩。
/// 故临英的 clear 只管空缓冲（见下一个测试），非空缓冲无条件上屏。
#[test]
fn test_temp_english_nonempty_enter_clear_still_commits() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with_english_trigger("wubi86", "slash");
    cfg.input.enter_behavior = "clear".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let act = coord.handle_key_event(&key_event(0xBF, EVENT_KEY_DOWN)); // /
    assert_eq!(action_text(&act).unwrap(), "/", "斜杠应进入临时英文");
    let mut disp = String::new();
    for c in "abc".chars() {
        disp = action_text(&press_letter(&coord, c)).unwrap_or_default();
    }
    assert!(
        disp.starts_with('/'),
        "字母应进临英缓冲，实际组合区: {:?}",
        disp
    );
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "abc", "临英非空缓冲回车应豁免 clear、照常上屏原文");
        }
        other => panic!("临英非空缓冲回车应上屏原文，实际: {:?}", other),
    }
}

/// 临英 clear 的**保留边界**：空缓冲（只按了触发键）回车仍按 clear 放弃，不回显触发键字符。
/// 没有它，「豁免」会被误实现成「临英完全不读 enter_behavior」而无人察觉。
#[test]
fn test_temp_english_empty_enter_clear_discards_prefix() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with_english_trigger("wubi86", "slash");
    cfg.input.enter_behavior = "clear".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let act = coord.handle_key_event(&key_event(0xBF, EVENT_KEY_DOWN)); // /
    assert_eq!(action_text(&act).unwrap(), "/", "斜杠应进入临时英文");
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!("clear 模式临英空缓冲回车应清空不上屏，实际: {:?}", other),
    }
}

/// 用户实报场景：`space_as_input` + `enter_behavior=clear` 叠加曾使临英**没有任何上屏通路**
/// —— 空格让位给输入字符，回车又被 clear 拿走，打进去的英文只能靠 Esc 丢弃。
#[test]
fn test_temp_english_space_as_input_enter_clear_still_commits() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with_english_trigger("wubi86", "slash");
    cfg.input.enter_behavior = "clear".into();
    cfg.input.temp_english.space_as_input = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let act = coord.handle_key_event(&key_event(0xBF, EVENT_KEY_DOWN)); // /
    assert_eq!(action_text(&act).unwrap(), "/", "斜杠应进入临时英文");
    for c in "hi".chars() {
        press_letter(&coord, c);
    }
    coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)); // 空格入缓冲
    let mut last = KeyAction::Consumed;
    for c in "there".chars() {
        last = press_letter(&coord, c);
    }
    assert_eq!(
        action_text(&last).unwrap(),
        "/hi there",
        "前置条件：空格应入缓冲而非上屏"
    );
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(
                text, "hi there",
                "space_as_input + clear 下回车仍须上屏整句"
            );
        }
        other => panic!("回车应上屏整句，实际: {:?}", other),
    }
}

/// 字母写进 mix 的 `trigger_keys` **不生效**——该键落普通输入，作正常码字母。
///
/// 反证测试：本仓曾支持任意 a-z 作 special/mix 引导键（`key_name_to_vk_with_letters`），
/// 但那条路没有三重身份裁决，字母一配就无条件抢键——而字母天然是编码键，被抢走就意味着
/// 该字母在本方案里永远打不出编码。字母的特殊能力已收归方案级 `z_key_action`（只管 z、
/// 经裁决链）。这里锁住「全局 trigger_keys 只认符号」这条边界，防止哪天为省事又把
/// `key_name_to_vk_with_letters` 接回来。
#[test]
fn test_mix_letter_trigger_key_is_ignored() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.mix_modes = vec![wind_config::config::MixModeConfig {
        id: "mix_z".into(),
        name: "测试".into(),
        short_name: "测".into(),
        trigger_keys: vec!["z".into()],
        members: vec!["quick_input".into()],
        ..Default::default()
    }];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    // 按 z(0x5A)：不该进 mix，应作五笔正常码进缓冲。
    let a = coord.handle_key_event(&key_event(0x5A, EVENT_KEY_DOWN));
    if let Some(disp) = action_text(&a) {
        assert!(
            disp.starts_with('z') || disp.is_empty(),
            "字母引导键应被忽略、z 作正常码累积，实际组合区: {}",
            disp
        );
    }
    // 若真进了 mix，空缓冲回车会走 ClearComposition；作正常码则不会。
    let enter = coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN));
    assert!(
        !matches!(enter, KeyAction::ClearComposition),
        "z 不该进 mix（进了才会空缓冲清空退出），实际: {:?}",
        enter
    );
}

#[test]
fn test_phrase_date_expansion() {
    if !has_schemas() {
        return;
    }
    // 短语层存储于 store（TOML 只是同步种子，见 build() 的 store.sync_system_phrases），
    // 无 store 时短语层不建、"date" 不会展开——须用 new_headless_with_store 注入真实 store。
    // 输入 "date" → 短语层应展开当前日期候选（如 2026年6月14日）
    let store_path = std::env::temp_dir().join("wind_phrase_date_test.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let coord =
        Coordinator::new_headless_with_store(config_with("wubi86"), Some(&data_dir()), store);
    for c in "date".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    // 短语高权重 → 应在候选中且靠前；校验存在「年…月…日」格式
    let has_date_phrase = texts
        .iter()
        .any(|t| t.contains('年') && t.contains('月') && t.contains('日'));
    assert!(
        has_date_phrase,
        "输入 date 应出现日期短语候选，实际: {:?}",
        texts
    );
}

#[test]
fn test_phrase_time_expansion() {
    if !has_schemas() {
        return;
    }
    // 短语层需真实 store 才会同步/启用（见 test_phrase_date_expansion 注释）。
    let store_path = std::env::temp_dir().join("wind_phrase_time_test.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let coord =
        Coordinator::new_headless_with_store(config_with("wubi86"), Some(&data_dir()), store);
    for c in "time".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    // 时间短语 $HH:$mm:$ss → 含冒号的时间串
    let has_time = texts
        .iter()
        .any(|t| t.matches(':').count() >= 1 && t.chars().any(|c| c.is_ascii_digit()));
    assert!(has_time, "输入 time 应出现时间短语候选，实际: {:?}", texts);
}

#[test]
fn test_s2t_converts_committed_candidate() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    if !coord.debug_set_s2t(true) {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    // 拼音输入 hanzi → 候选含 汉字；开启简繁后上屏应为 漢字
    for c in "hanzi".chars() {
        press_letter(&coord, c);
    }
    // 找到"汉字"所在候选位置并用数字键选择；若首选即是则空格
    let texts = coord.debug_page_texts();
    let pos = texts.iter().position(|t| t == "汉字");
    let commit = if let Some(p) = pos {
        // 数字键 (p+1)
        coord.handle_key_event(&key_event(0x31 + p as u32, EVENT_KEY_DOWN))
    } else {
        // 退化：直接空格上屏首选，仅校验为繁体（不强等于）
        coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN))
    };
    match commit {
        KeyAction::InsertText { text, .. } => {
            if pos.is_some() {
                assert_eq!(text, "漢字", "开启简繁后 汉字 应上屏为 漢字");
            } else {
                // 至少不应是简体"汉字"
                assert_ne!(text, "汉字");
            }
        }
        other => panic!("应上屏 InsertText，实际: {:?}", other),
    }
}

#[test]
fn test_s2t_converts_candidate_display() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    if !coord.debug_set_s2t(true) {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    for c in "hanzi".chars() {
        press_letter(&coord, c);
    }
    // 内部候选仍是简体（供词频/匹配）
    let internal = coord.debug_page_texts();
    // 显示文本应为繁体
    let display = coord.debug_page_display_texts();
    if let Some(p) = internal.iter().position(|t| t == "汉字") {
        assert_eq!(display[p], "漢字", "候选显示应为繁体 漢字");
    } else {
        eprintln!("跳过：候选未含 汉字");
    }
    // 简体与显示长度一致、且至少有一项被转换
    assert_eq!(internal.len(), display.len());
    assert!(
        internal.iter().zip(&display).any(|(a, b)| a != b),
        "开启简繁后显示应有候选被转换"
    );
}

#[test]
fn test_s2t_one_to_many_variant_expansion() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    if !coord.debug_set_s2t(true) {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    // 拼音输入 chu → 单字候选「出」（STCharacters 多值行 出→出 齣）应紧跟变体「齣」。
    for c in "chu".chars() {
        press_letter(&coord, c);
    }
    let internal = coord.debug_page_texts();
    let display = coord.debug_page_display_texts();
    let Some(p) = display.iter().position(|t| t == "出") else {
        panic!("输入 chu 候选应含「出」，实际: {:?}", display);
    };
    assert!(
        p + 1 < display.len() && display[p + 1] == "齣",
        "「出」之后应紧跟 1对多变体「齣」，实际: {:?}",
        display
    );
    // 变体候选**内部 text 保持简体**（词频/匹配域不被繁体污染）。
    assert_eq!(internal[p], "出");
    assert_eq!(internal[p + 1], "出", "变体候选内部 text 应仍是简体「出」");
    // 选中变体（页内第 p+2 项，数字键 1-based）→ 上屏「齣」而非默认转换的「出」。
    match coord.handle_key_event(&key_event(0x31 + (p + 1) as u32, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "齣", "选中变体候选应上屏「齣」");
        }
        other => panic!("应上屏 InsertText，实际: {:?}", other),
    }
}

#[test]
fn test_s2t_variant_absent_when_disabled() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    // 简繁关闭：不展开变体，「出」之后不应出现内部 text 重复的变体候选。
    for c in "chu".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    let dup_adjacent = texts.windows(2).any(|w| w[0] == "出" && w[1] == "出");
    assert!(
        !dup_adjacent,
        "简繁关闭时不应出现展开产生的相邻重复候选: {:?}",
        texts
    );
}

#[test]
fn test_s2t_disabled_keeps_simplified() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    // 默认关闭简繁：上屏保持简体
    for c in "hanzi".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if let Some(p) = texts.iter().position(|t| t == "汉字") {
        match coord.handle_key_event(&key_event(0x31 + p as u32, EVENT_KEY_DOWN)) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, "汉字", "默认应保持简体"),
            other => panic!("应上屏，实际: {:?}", other),
        }
    }
}

#[test]
fn test_smart_punct_after_digit() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // 句号(0xBE)，光标前字符为数字 '5'(0x35) → 应输出英文 '.'
    let mut ev = key_event(0xBE, EVENT_KEY_DOWN);
    ev.prev_char = '5' as u16;
    match coord.handle_key_event(&ev) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, ".", "数字后句号应为英文 ."),
        other => panic!("应上屏英文句号，实际: {:?}", other),
    }
    // 光标前为非数字（'a'）→ 应为中文句号 。
    let mut ev2 = key_event(0xBE, EVENT_KEY_DOWN);
    ev2.prev_char = 'a' as u16;
    match coord.handle_key_event(&ev2) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "。", "字母后句号应为中文 。"),
        other => panic!("应上屏中文句号，实际: {:?}", other),
    }
    // 逗号(0xBC)数字后 → 英文 ','
    let mut ev3 = key_event(0xBC, EVENT_KEY_DOWN);
    ev3.prev_char = '9' as u16;
    match coord.handle_key_event(&ev3) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, ",", "数字后逗号应为英文 ,"),
        other => panic!("应上屏英文逗号，实际: {:?}", other),
    }
}

#[test]
fn test_dynamic_paging_expands_candidates() {
    if !has_schemas() {
        return;
    }
    // 单字母前缀通常有大量候选：旧实现固定封顶 50，新实现按前缀加载全部（≥初始上限再分级扩展）
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a');
    let initial = coord.debug_candidate_count();
    // 核心修复：不再固定截断到 50（'a' 前缀候选应远超旧上限）
    assert!(
        initial > 50,
        "应加载超过旧固定上限(50)的全部前缀候选，实际: {}",
        initial
    );

    // 若仍达到初始分级上限，翻页到边界应动态扩展加载更多
    if coord.debug_has_more() {
        for _ in 0..15 {
            coord.handle_key_event(&key_event(0x22, EVENT_KEY_DOWN)); // PageDown
        }
        let expanded = coord.debug_candidate_count();
        assert!(
            expanded > initial,
            "翻页到边界应动态加载更多候选: {} -> {}",
            initial,
            expanded
        );
    }
}

/// 按下 Shift+字母
fn press_shift_letter(coord: &Coordinator, c: char) -> KeyAction {
    let vk = (c.to_ascii_uppercase() as u32) & 0xFF;
    let mut ev = key_event(vk, EVENT_KEY_DOWN);
    ev.modifiers = 0x0001; // MOD_SHIFT
    coord.handle_key_event(&ev)
}

#[test]
fn test_temp_english_shift_letter_commit() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // Shift+H 进入临时英文，首字母大写
    let act = press_shift_letter(&coord, 'h');
    assert_eq!(
        action_text(&act).unwrap(),
        "H",
        "Shift+H 应进入临时英文显示 H"
    );

    // 续输 ello（无 Shift → 小写）
    let mut last = act;
    for c in "ello".chars() {
        last = press_letter(&coord, c);
    }
    assert_eq!(action_text(&last).unwrap(), "Hello", "组合区应为 Hello");

    // 空格上屏
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "Hello"),
        other => panic!("空格应上屏 Hello，实际: {:?}", other),
    }
    // 退出后五笔恢复正常
    let act = press_letter(&coord, 'a');
    assert_eq!(action_text(&act).unwrap(), "a");
}

#[test]
fn test_temp_english_digits_and_punct() {
    if !has_schemas() {
        return;
    }
    // 关闭英文候选查词：本测试验证「数字在无可选候选时应入缓冲」，若开着词库候选，
    // "ver" 命中真实英文词（Verb/Verbal…）会让数字被解释成候选翻页选词（设计如此，
    // 见 handle_temp.rs 数字分支注释），与本测试意图无关，故关闭以消除数据耦合。
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.show_candidates = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'v'); // V
    press_letter(&coord, 'e');
    press_letter(&coord, 'r');
    // 数字入缓冲
    press_vk(&coord, 0x32, false); // 2
    let last = press_letter(&coord, 'b');
    assert_eq!(action_text(&last).unwrap(), "Ver2b", "数字应入缓冲");
    // 句号(0xBE)：上屏缓冲 + 中文句号（默认中文标点）
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "Ver2b。", "应上屏缓冲+中文句号"),
        other => panic!("标点应上屏缓冲+标点，实际: {:?}", other),
    }
}

/// 临英候选排布：`原文 → 大小写变形 → 词库原文`，且词库候选**不再被套上输入的大小写形态**。
/// 回归点：临英由 Shift+字母进入，缓冲首字母恒大写，旧实现据此把整列词库候选适配成
/// `Help`/`Held`/`Hell`，于是「候选全是大写首字母」。
#[test]
fn test_temp_english_case_variants_and_dict_keeps_original_case() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_shift_letter(&coord, 'h'); // H
    press_letter(&coord, 'e');
    press_letter(&coord, 'l'); // 缓冲 "Hel"

    let texts = coord.debug_all_candidate_texts();
    assert_eq!(
        texts.iter().take(3).collect::<Vec<_>>(),
        vec!["Hel", "hel", "HEL"],
        "前三候选应为 原文 → 全小写 → 全大写（原文已是首字母大写，Title 变形被去重），实际: {:?}",
        texts
    );
    assert!(
        texts.iter().any(|t| t == "help"),
        "词库候选应保持原文小写，实际: {:?}",
        texts
    );
    assert!(
        !texts.iter().any(|t| t == "Help"),
        "词库候选不应被适配成输入的首字母大写形态，实际: {:?}",
        texts
    );
    assert!(
        texts.iter().any(|t| t == "Helen"),
        "词库中本就大写的专有名词应原样保留，实际: {:?}",
        texts
    );
}

/// 变形候选对全小写 / 全大写输入同样自洽：缺哪种形态就补哪种，原文永远排首位。
#[test]
fn test_temp_english_case_variants_from_lowercase_entry() {
    if !has_schemas() {
        return;
    }
    // 触发键进入 → 缓冲首字母不受 Shift 影响，可打出全小写原文。
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.trigger_keys = vec!["/".to_string()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_vk(&coord, 0xBF, false); // "/" 进入临英
    for c in "hel".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_all_candidate_texts();
    assert_eq!(
        texts.iter().take(3).collect::<Vec<_>>(),
        vec!["hel", "Hel", "HEL"],
        "全小写输入应补出首字母大写与全大写两个变形，实际: {:?}",
        texts
    );
}

/// `case_variants = false`：不再生成大小写变形候选，原文之后直接是词库候选。
///
/// 与上面两个测试互为正反面——它们钉住「开着时变形恒在前三位」，这条钉住「关掉即消失」。
/// 只有开态测试的话，开关没接上（读了配置但没用）照样全绿。
#[test]
fn test_temp_english_case_variants_disabled() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.case_variants = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h'); // H
    press_letter(&coord, 'e');
    press_letter(&coord, 'l'); // 缓冲 "Hel"

    let texts = coord.debug_all_candidate_texts();
    assert_eq!(
        texts.first().map(|s| s.as_str()),
        Some("Hel"),
        "原文仍是首候选"
    );
    assert!(
        !texts.iter().any(|t| t == "hel" || t == "HEL"),
        "关掉后不得再有大小写变形候选，实际: {:?}",
        texts
    );
    assert!(
        texts.iter().any(|t| t == "help"),
        "词库候选不受影响（本开关只管变形项），实际: {:?}",
        texts
    );
}

/// allow_symbols 开：数字键 1-9 一律入缓冲（英文原文优先于选词），即使此刻有词库候选。
#[test]
fn test_temp_english_allow_symbols_digits_go_to_buffer() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.allow_symbols = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l'); // "Hel" —— 此刻词库候选非空
    assert!(
        coord.debug_all_candidate_texts().len() > 1,
        "前置条件：此刻应有候选，否则测不出「有候选时数字仍入缓冲」"
    );
    let act = press_vk(&coord, 0x32, false); // 2
    assert_eq!(
        action_text(&act).unwrap(),
        "Hel2",
        "allow_symbols 开启时数字应入缓冲而非选第 2 个候选"
    );
    // 符号同样入缓冲（既有行为），并可继续与数字混排。
    let act = press_vk(&coord, 0xBD, false); // "-"
    assert_eq!(action_text(&act).unwrap(), "Hel2-", "符号应入缓冲");
}

/// 数字兜底：白名单**不含数字**，但缓冲已含符号（`C++`）时数字仍直接入缓冲。
///
/// 回归点：原判据是 `state.candidates.len() > 1`，本意「有候选可选才选词」，但取值口径不对
/// ——首候选恒是原文，`case_variants` 又对含符号串照样产出变形（`C++` → `c++`），于是
/// `len > 1` 恒真，按 `1` 直接上屏 `C++` 并退出临英，`C++11` 根本打不出来。
/// 判据改到缓冲上（含非字母字符 = 纯文本累积态），与白名单是否含数字无关。
#[test]
fn test_temp_english_digits_go_to_buffer_after_symbol_even_if_not_listed() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.allow_symbols = true;
    cfg.input.temp_english.symbol_chars = "+".into(); // 刻意只放行 `+`，不含 0-9
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'c');
    press_vk(&coord, 0xBB, true); // Shift+= → `+`
    let act = press_vk(&coord, 0xBB, true); // 再一个 `+`
    assert_eq!(action_text(&act).unwrap(), "C++", "前置：`+` 应入缓冲");
    assert!(
        coord.debug_all_candidate_texts().len() > 1,
        "前置条件：候选须 >1（原文 + 大小写变形），否则旧判据本就放行，测不出兜底"
    );
    let act = press_vk(&coord, 0x31, false); // `1`
    assert_eq!(
        action_text(&act).unwrap(),
        "C++1",
        "缓冲含符号后数字应入缓冲，而非选首候选并退出"
    );
}

/// 白名单**之外**的符号维持旧语义：上屏高亮候选 + 转换后标点 → 退出临英。
///
/// 这条通路是「打完英文顺手按句号上屏」的唯一实现。旧实现下 allow_symbols 一开它整体消失
/// （任何符号都只入缓冲）；改造后只有列入的字符被摘出去，其余照旧。
#[test]
fn test_temp_english_unlisted_punct_keeps_commit_and_exit() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.allow_symbols = true;
    cfg.input.temp_english.symbol_chars = "+".into(); // 不含 `.`
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        // `.` 未列入 → 上屏「高亮候选 + 转换后标点」并退出（默认高亮 = 首候选 = 原文）
        KeyAction::InsertText { text, .. } => {
            assert!(
                text.starts_with("Hel") && text.chars().count() > 3,
                "`.` 未列入白名单，应上屏候选 + 标点并退出，实际: {text:?}"
            );
        }
        other => panic!("`.` 未列入白名单，应走标点上屏臂，实际: {:?}", other),
    }
}

/// 对照组：allow_symbols 关（默认）时数字键仍是选词键——守住既有行为不被上面的改动误伤。
#[test]
fn test_temp_english_digits_still_select_when_symbols_disallowed() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l'); // 候选 [Hel, hel, HEL, held, ...]
    match coord.handle_key_event(&key_event(0x32, EVENT_KEY_DOWN)) {
        // 2 → 第 2 个候选 = 全小写变形
        KeyAction::InsertText { text, .. } => assert_eq!(text, "hel"),
        other => panic!("数字键应选第 2 个候选并上屏，实际: {:?}", other),
    }
}

/// 二三候选键（默认 `;` `'`）在临英下应选中对应候选。
/// 回归点：临英曾是唯一没接 `select_key_offset` 的模式处理器，`;` 一路落到标点臂被判成
/// 「上屏高亮候选 + 标点」，用户按次选键实得**首候选被直接上屏**。
#[test]
fn test_temp_english_select_keys_pick_candidates() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    // 显式声明键组，使本测试不随默认值漂移（默认亦为 semicolon_quote）。
    cfg.keys.select_key_groups = vec!["semicolon_quote".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l'); // 候选 [Hel, hel, HEL, held, ...]
    // 前置条件：页内至少 3 项，否则 `gi < end` 不成立，选词分支根本执行不到（假绿）。
    assert!(
        coord.debug_all_candidate_texts().len() >= 3,
        "前置条件：应有 ≥3 个候选，否则测不到二/三选键的选词分支"
    );
    match coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)) {
        // `;` → 次选 = 第 2 候选（全小写变形）
        KeyAction::InsertText { text, .. } => assert_eq!(text, "hel"),
        other => panic!("`;` 应选第 2 候选并上屏，实际: {:?}", other),
    }

    // 三选键 `'` → 第 3 候选（全大写变形）。复用同一 Coordinator 重新进临英——
    // 上屏后临英已退出，重打即可。刻意不新建实例：引擎 reader / LRU 跨实例共享且带
    // 配额语义（见 mmap 共享 reader 的设计），一个测试建多个实例会与并行跑的其他
    // 测试争用，实测会让无关测试偶发失败。
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    match coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "HEL"),
        other => panic!("`'` 应选第 3 候选并上屏，实际: {:?}", other),
    }
}

/// 对照组一：`;` **被列入白名单**时让位于字符输入——列入的字符语义是「入缓冲而非上屏退出
/// **或选词**」，与数字臂同构，不能被选词接线破坏。
#[test]
fn test_temp_english_select_keys_yield_to_input_when_listed() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.select_key_groups = vec!["semicolon_quote".into()];
    cfg.input.temp_english.allow_symbols = true;
    cfg.input.temp_english.symbol_chars = ";".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    assert!(
        coord.debug_all_candidate_texts().len() >= 3,
        "前置条件：应有 ≥3 个候选，否则「有候选仍不选词」无从谈起"
    );
    let act = press_vk(&coord, 0xBA, false); // `;`
    assert_eq!(
        action_text(&act).unwrap(),
        "Hel;",
        "`;` 在白名单内时应入缓冲而非选第 2 候选"
    );
}

/// 对照组一之反向：总开关开着、但 `;` **不在**白名单里时它仍是选词键。
///
/// 这是本次「bool → 白名单」改造的核心收益，也是唯一能证明改造真的落地的方向：
/// 旧实现下 allow_symbols 一开，`;` 无条件让位，本断言必红。缺了这条反向对照，
/// 上面那条测试在旧实现下同样是绿的（旧实现让位得更狠），等于什么都没锁住。
#[test]
fn test_temp_english_select_keys_still_select_when_not_listed() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.select_key_groups = vec!["semicolon_quote".into()];
    cfg.input.temp_english.allow_symbols = true;
    cfg.input.temp_english.symbol_chars = "+-_".into(); // 刻意不含 `;`
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    assert!(
        coord.debug_all_candidate_texts().len() >= 3,
        "前置条件：应有 ≥3 个候选，否则测不到选词分支"
    );
    match coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(
            text, "hel",
            "`;` 不在白名单时应照常选第 2 候选（全小写变形）"
        ),
        other => panic!("`;` 未列入白名单，应选第 2 候选并上屏，实际: {:?}", other),
    }
}

/// 对照组二：页内候选不足时 `;` 仍走标点臂（上屏高亮候选 + 标点并退出），
/// 守住越界语义不被选词接线误伤。`show_candidates` 关 → 候选只剩原文一项。
#[test]
fn test_temp_english_select_key_overflow_falls_back_to_punct() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.select_key_groups = vec!["semicolon_quote".into()];
    cfg.input.temp_english.show_candidates = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    assert_eq!(
        coord.debug_all_candidate_texts().len(),
        1,
        "前置条件：show_candidates 关时应只剩原文候选，次选键才会越界"
    );
    let act = press_vk(&coord, 0xBA, false); // `;`
    let text = action_text(&act).expect("越界时应按标点臂上屏");
    assert!(
        text.starts_with("Hel") && text.chars().count() == 4,
        "越界时应上屏「原文 + 转换后标点」，实际: {:?}",
        text
    );
}

// ── 修饰键作二三候选键（select_key_groups = "lrshift" / "lrctrl"）──────────────
//
// 这组键与可打印选词键（`;` `'`）走**完全不同的物理通路**：纯修饰键的 keydown 不能吃
// （宿主要看得见修饰键，否则 AutoCAD 正交模式失效并卡顿），所以 TSF 只在判定为「轻敲」
// 后转发一个 keyup 过来，选词判定挂在 keyup 上。keydown 侧的一切（`!shift` 守卫、
// Ctrl/Alt 组合清空闸门）都与它无关，也正因如此，这条路必须由 keyup 事件单独钉住。

/// 左 Ctrl 轻敲（keyup）→ 选第 2 候选；右 Ctrl → 选第 3 候选。
#[test]
fn modifier_select_key_up_picks_candidate() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("pinyin");
    cfg.keys.select_key_groups = vec!["lrctrl".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_letter(&coord, 'n');
    press_letter(&coord, 'i');
    let cands = coord.debug_all_candidate_texts();
    assert!(
        cands.len() >= 3,
        "前置条件：应有 ≥3 个候选，否则选词分支根本执行不到（假绿）"
    );
    let (second, third) = (cands[1].clone(), cands[2].clone());
    match coord.handle_key_event(&key_event(0xA2, EVENT_KEY_UP)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, second, "左 Ctrl 应选第 2 候选"),
        other => panic!("左 Ctrl 抬起应选第 2 候选并上屏，实际: {:?}", other),
    }

    press_letter(&coord, 'n');
    press_letter(&coord, 'i');
    match coord.handle_key_event(&key_event(0xA3, EVENT_KEY_UP)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, third, "右 Ctrl 应选第 3 候选"),
        other => panic!("右 Ctrl 抬起应选第 3 候选并上屏，实际: {:?}", other),
    }
}

/// 同一个键既是切换键又是选词键时的裁决：**有候选选词**，且不得顺带切走中英文。
#[test]
fn modifier_select_key_up_wins_over_toggle_when_candidates() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("pinyin");
    cfg.keys.select_key_groups = vec!["lrctrl".into()];
    cfg.keys.toggle_mode_keys = vec!["lctrl".into(), "rctrl".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_letter(&coord, 'n');
    press_letter(&coord, 'i');
    let second = coord.debug_all_candidate_texts()[1].clone();
    match coord.handle_key_event(&key_event(0xA2, EVENT_KEY_UP)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, second),
        other => panic!("有候选时左 Ctrl 应选词，实际: {:?}", other),
    }
    assert!(coord.is_chinese_mode(), "选词不得顺带切走中英文");
}

/// 同上配置但**无候选**（空闲）：回落到中英文切换。
#[test]
fn modifier_select_key_up_falls_back_to_toggle_when_idle() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("pinyin");
    cfg.keys.select_key_groups = vec!["lrctrl".into()];
    cfg.keys.toggle_mode_keys = vec!["lctrl".into(), "rctrl".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xA2, EVENT_KEY_UP));
    assert!(!coord.is_chinese_mode(), "空闲时左 Ctrl 应切到英文");
}

/// 只把 Ctrl 配成选词键（切换键仍是 Shift）：空闲时敲 Ctrl **不得**切中英文。
/// 回归点：`is_toggle_mode_keycode` 若只看「key_up 白名单里有没有这个 key_code」，
/// 选词用的 Ctrl 登记同样在那张表里，就会被误当切换键。
#[test]
fn modifier_select_key_alone_does_not_toggle() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("pinyin");
    cfg.keys.select_key_groups = vec!["lrctrl".into()];
    cfg.keys.toggle_mode_keys = vec!["lshift".into(), "rshift".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key_event(0xA2, EVENT_KEY_UP));
    assert!(coord.is_chinese_mode(), "未配为切换键的 Ctrl 不应切中英文");
    // 对照：配了的 Shift 照切，证明上面的「不切」不是整条 keyup 通路坏掉。
    coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    assert!(!coord.is_chinese_mode(), "左 Shift 仍应切到英文");
}

/// 越界（页内候选不足以命中该位次）：吞键，既不上屏也不切换。
/// per_page=1 让第 2 位次必然越界，且此时 Ctrl 同时是切换键——若越界放行给 toggle，
/// 这里就会切走中英文。
#[test]
fn modifier_select_key_up_swallowed_on_overflow() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("pinyin");
    cfg.keys.select_key_groups = vec!["lrctrl".into()];
    cfg.keys.toggle_mode_keys = vec!["lctrl".into(), "rctrl".into()];
    cfg.ui.candidate.per_page = 1;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_letter(&coord, 'n');
    press_letter(&coord, 'i');
    let act = coord.handle_key_event(&key_event(0xA2, EVENT_KEY_UP));
    assert!(
        action_text(&act).is_none(),
        "越界不应上屏任何文本，实际: {:?}",
        act
    );
    assert!(coord.is_chinese_mode(), "越界时不得回落到中英文切换");
}

/// space_as_input 开：空格被占作输入字符，回车接过「上屏高亮候选」的职责。
/// 回归点：该配置下空格不再选词，`allow_symbols` 再开则数字键也让位，若回车仍固定上屏原文，
/// 就一个选词键都不剩、候选窗形同虚设。
#[test]
fn test_temp_english_space_as_input_enter_commits_highlighted() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.space_as_input = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l'); // 候选 [Hel, hel, HEL, hell, ...]
    coord.handle_key_event(&key_event(0x28, EVENT_KEY_DOWN)); // ↓ 高亮第 1 项
    let (_, sel, _) = coord.debug_page_info();
    assert_eq!(sel, 1, "前置条件：下方向键应把高亮移到第 1 项");
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "hel", "space_as_input 下回车应上屏高亮候选")
        }
        other => panic!("回车应上屏高亮候选，实际: {:?}", other),
    }
}

/// 同上配置但**未导航**：高亮停在首候选（=用户原文），故回车仍上屏原文——
/// 对「回车上屏原文」的既有直觉向下兼容，只有主动导航过才会上屏别的候选。
#[test]
fn test_temp_english_space_as_input_enter_without_nav_commits_original() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.space_as_input = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "Hel", "未导航时回车应上屏原文"),
        other => panic!("回车应上屏原文，实际: {:?}", other),
    }
}

/// space_as_input 开的端到端：空格入缓冲打出带空格的短句，回车上屏整句。
#[test]
fn test_temp_english_space_as_input_multiword_enter() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_english.space_as_input = true;
    cfg.input.temp_english.trigger_keys = vec!["/".to_string()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_vk(&coord, 0xBF, false); // "/" 进入（首字母不受 Shift 影响）
    for c in "hi".chars() {
        press_letter(&coord, c);
    }
    coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)); // 空格入缓冲
    let mut last = KeyAction::Consumed;
    for c in "there".chars() {
        last = press_letter(&coord, c);
    }
    assert_eq!(
        action_text(&last).unwrap(),
        "/hi there",
        "空格应入缓冲（组合区含触发键前缀）"
    );
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "hi there", "回车应上屏整句（高亮在首候选=原文）")
        }
        other => panic!("回车应上屏整句，实际: {:?}", other),
    }
}

/// 对照组：space_as_input 关（默认）时回车仍固定上屏原文，**即使已导航到别的候选**——
/// 此时空格才是选词键，回车的「放弃候选、要我打的原文」语义必须保住。
#[test]
fn test_temp_english_enter_commits_original_when_space_selects() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    coord.handle_key_event(&key_event(0x28, EVENT_KEY_DOWN)); // ↓ 高亮第 1 项 (hel)
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "Hel", "默认配置下回车应上屏原文而非高亮候选")
        }
        other => panic!("回车应上屏原文，实际: {:?}", other),
    }
}

/// direct（默认）：临英缓冲是文本，小键盘数字/符号直接入缓冲 →「英文数字连输」可用。
#[test]
fn test_temp_english_numpad_direct_inputs() {
    if !has_schemas() {
        return;
    }
    // 小键盘数字在临英下曾被静默吃掉（只认主键盘 0x30-0x39，小键盘 0x60-0x69 落标点臂
    // → punct_char 判 None → Consumed）。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_shift_letter(&coord, 'v'); // V
    press_letter(&coord, 'e');
    press_letter(&coord, 'r');
    let last = press_vk(&coord, 0x62, false); // 小键盘 2 (VK_NUMPAD2)
    assert_eq!(
        action_text(&last).unwrap(),
        "Ver2",
        "小键盘数字应入临英缓冲"
    );
    // 小键盘小数点 / 减号同样入缓冲。
    press_vk(&coord, 0x6E, false); // VK_DECIMAL '.'
    let last = press_vk(&coord, 0x6D, false); // VK_SUBTRACT '-'
    assert_eq!(action_text(&last).unwrap(), "Ver2.-", "小键盘符号应入缓冲");
}

/// follow_main：入口归一化 → 小键盘键在**所有模式**下与主键盘同键完全一致。
/// 归一化是唯一实现手段，故本测试即是「所有模式一致」的守护。
#[test]
fn test_numpad_follow_main_matches_mainboard_all_modes() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.numpad_behavior = "follow_main".into();
    // 临英下主键盘数字的语义依赖候选有无，关掉词库候选以固定为「入缓冲」，
    // 令主/小键盘对照不受真实英文词库数据影响（同 test_temp_english_digits_and_punct）。
    cfg.input.temp_english.show_candidates = false;

    // ① 临时英文：小键盘 2 ≡ 主键盘 2（无候选 → 入缓冲）
    let coord = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    press_shift_letter(&coord, 'v');
    press_letter(&coord, 'e');
    press_letter(&coord, 'r');
    let np = press_vk(&coord, 0x62, false); // VK_NUMPAD2
    let coord2 = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    press_shift_letter(&coord2, 'v');
    press_letter(&coord2, 'e');
    press_letter(&coord2, 'r');
    let main = press_vk(&coord2, 0x32, false); // 主键盘 2
    assert_eq!(
        action_text(&np),
        action_text(&main),
        "临英：小键盘 2 应与主键盘 2 一致"
    );

    // ② 普通码表：小键盘 2 ≡ 主键盘 2（有候选 → 选第 2 个候选）
    let coord = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    press_letter(&coord, 'a');
    let np = press_vk(&coord, 0x62, false);
    let coord2 = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    press_letter(&coord2, 'a');
    let main = press_vk(&coord2, 0x32, false);
    assert_eq!(
        action_text(&np),
        action_text(&main),
        "普通码表：小键盘 2 应与主键盘 2 一致（同选第 2 候选）"
    );

    // ③ 运算符须连 Shift 一并归一：小键盘 * ≡ 主键盘 Shift+8
    let coord = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    press_letter(&coord, 'a');
    let np = press_vk(&coord, 0x6A, false); // VK_MULTIPLY
    let coord2 = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    press_letter(&coord2, 'a');
    let main = press_vk(&coord2, 0x38, true); // Shift+8 = '*'
    assert_eq!(
        action_text(&np),
        action_text(&main),
        "小键盘 * 应与主键盘 Shift+8 一致"
    );
}

/// 数字键 0 选当前页第 10 个候选（主键盘 / 小键盘 follow_main 一致）。
/// 主键盘 0 此前落兜底流水线只输出 '0'，不选第 10——「0 = 第10候选」是通行约定，
/// 也是 follow_main 下 Numpad0「和主键盘一样」的前提。
#[test]
fn test_number_zero_selects_tenth_candidate() {
    if !has_schemas() {
        return;
    }
    // 0 选「当前页第 10 个」，故须每页容量 ≥10（默认 per_page=7 时第 10 越界）。
    // 拼音 "shi" 候选远多于 10，确保 0 选第 10 而非越界 overflow。
    let mut cfg = config_with("pinyin");
    cfg.input.numpad_behavior = "follow_main".into();
    cfg.ui.candidate.per_page = 10;
    let type_shi = |c: &Coordinator| {
        for ch in "shi".chars() {
            press_letter(c, ch);
        }
    };

    let a = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    type_shi(&a);
    let main0 = action_text(&press_vk(&a, 0x30, false)); // 主键盘 0

    let b = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    type_shi(&b);
    let np0 = action_text(&press_vk(&b, 0x60, false)); // 小键盘 0 (VK_NUMPAD0, follow_main)

    assert!(
        main0.as_deref().is_some_and(|t| !t.is_empty()),
        "主键盘 0 应选中第 10 候选并上屏（shi 候选足够多），实际: {:?}",
        main0
    );
    assert_eq!(np0, main0, "小键盘 0 (follow_main) 应与主键盘 0 选同一候选");

    // 空缓冲下的 0 不进选词臂：输出数字本身，不回归 fullwidth（此处半角态 → '0'）。
    let c = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    let empty0 = c.handle_key_event(&key_event(0x30, EVENT_KEY_DOWN));
    assert!(
        matches!(&empty0, KeyAction::PassThrough) || action_text(&empty0).as_deref() == Some("0"),
        "空缓冲主键盘 0 应输出数字 0（透传或上屏），实际: {:?}",
        empty0
    );
}

/// direct 下编码型模式：不丢已打的码——顶屏当前高亮候选后再输出该数字。
#[test]
fn test_numpad_direct_commits_candidate_then_digit() {
    if !has_schemas() {
        return;
    }
    // 默认 numpad_behavior 为空 → direct。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_letter(&coord, 'a'); // 产生候选
    // 对照组：取此刻首候选文本（direct 应顶屏它）。
    let expect_head = {
        let c2 = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
        press_letter(&c2, 'a');
        // 空格上屏高亮候选 = direct 应顶屏的同一个候选。
        action_text(&c2.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN))).unwrap()
    };
    let act = press_vk(&coord, 0x62, false); // 小键盘 2
    assert_eq!(
        action_text(&act).unwrap(),
        format!("{}2", expect_head),
        "direct：应顶屏高亮候选再接小键盘数字（旧行为是丢弃编码只输出数字）"
    );
}

#[test]
fn test_temp_english_esc_exits() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_shift_letter(&coord, 'a');
    match coord.handle_key_event(&key_event(0x1B, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!("Esc 应退出临时英文，实际: {:?}", other),
    }
}

fn config_mixed() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86_pinyin".into(), "wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86_pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

/// 混输打 `xu`：拼音精确候选「需」必须进首页，不得被码表 `xu*` 的前缀补全整体压后。
///
/// **真机现场**（本测试即其回归）：首选是码表精确全码「弱」（`xu` 是二简码，权重 9950+1e7），
/// 而拼音的「需」（`code==xu` 精确匹配、该音节最高频字 6999）被 `xu*` 的码表前缀补全整体压住。
/// 短路本改动实测「需」落在**第 98 位**（报告者 `per_page=5` ⇒ 正是其所报的第 20 页）；候选前 12 条
/// 全是五笔：`["弱","缮","绊","弹","缯","缔","绞","缣","缢","弱点","弹幕","弹性"]`。
/// 词库侧规模：主库 130 条加 extra 4 条，按 `text` 去重后 124 条 `xu*` 前缀补全。
/// 根因是混输的档位系统只承认码表那一半「精确 vs 前缀」：码表精确 `+1e7`、码表前缀补全
/// `+PARTIAL_MATCH_BOOST`(500K)，而拼音**不分精确与补全**统一 `÷PINYIN_TIER_SCALE`(100)。
///
/// ⚠️ `new_headless` 的 `store` 为 `None` ⇒ `freq_rerank` 不参与（其触发前提要求有词频记录），
/// 故本测试测的是纯 `candidate_display_order` 的效果 —— 正是文档「验证匹配层类改动必须关自动
/// 调频」要求的隔离条件。`freq_tier` 侧的同款档位另由 wind-engine 的单测覆盖。
#[test]
fn mixed_xu_pinyin_exact_reaches_first_page() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    // 贴合真机配置：filter_mode=general（只保留常用字）——提档判据 is_common 与该模式同口径。
    let mut cfg = config_mixed();
    cfg.input.filter_mode = "general".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_letter(&coord, 'x');
    press_letter(&coord, 'u');
    let texts = coord.debug_all_candidate_texts();

    // 前置一：码表精确全码仍稳居首位（本改动**不得**动摇「码表精确 > 拼音」这条硬约束）。
    assert_eq!(
        texts.first().map(String::as_str),
        Some("弱"),
        "码表精确全码「弱」必须仍是首选，实际: {:?}",
        &texts[..texts.len().min(10)]
    );
    // 前置二：确认码表前缀补全候选**确实在场** —— 否则本测试退化成「没有竞争者的假绿」。
    assert!(
        texts
            .iter()
            .any(|t| t == "弹幕" || t == "弹性" || t == "弱点"),
        "前置：xu 的码表前缀补全候选应在候选列表内，实际: {:?}",
        &texts[..texts.len().min(20)]
    );

    let pos = texts.iter().position(|t| t == "需").unwrap_or_else(|| {
        panic!(
            "「需」应在候选中，实际: {:?}",
            &texts[..texts.len().min(20)]
        )
    });
    assert!(
        pos < 7,
        "「需」应进首页（per_page=7），实际第 {} 位；前 12 条: {:?}",
        pos + 1,
        &texts[..texts.len().min(12)]
    );
}

/// 混输打 `aaw`（本意是 `aawt`→「工作」）：拼音的**部分匹配整句**不得抢走首位。
///
/// 真机现象：`aaw` 时首选变成拼音「啊啊」，把 `a`+`a` 拆成两个音节。
///
/// ★ 这是「拼音精确档」判据的边界：五笔 `aaw` **无精确全码**（候选全是 `aawt` 工作 / `aawf`
/// 工会 一类前缀补全），所以没有 `is_exact_code=true` 的候选占着首位 —— 一旦拼音被误判进精确
/// 档，它就直接是首选。而「啊啊」正是那个误判：
/// - 它是 Viterbi 整句（词条 `啊啊 a a`），`code` 取 `completed`="aa"、`consumed_length=2`，
///   只解释了 3 键中的 2 键，`w` 是残码；
/// - 但 `is_partial` **是 false** —— 整句走 `insert(0)` 不经 `push_hit` 闭包，且同文合并时
///   `mod.rs` 还会主动 `existing.is_partial = false`（其语义是「这不是子短语」，不是「消费了整串」）。
///
/// 故判据不能拿 `!is_partial` 代替「消费整串」，必须直接问 `consumed_length`。
#[test]
fn mixed_aaw_partial_sentence_does_not_preempt_codetable() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let mut cfg = config_mixed();
    cfg.input.filter_mode = "general".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "aaw".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_all_candidate_texts();
    // 前置：确认码表前缀补全确实在场（否则本用例退化成「没有竞争者」的假绿）。
    assert!(
        texts.iter().any(|t| t == "工作"),
        "前置：aawt→「工作」应在候选内，实际: {:?}",
        &texts[..texts.len().min(15)]
    );
    assert_eq!(
        texts.first().map(String::as_str),
        Some("工作"),
        "首选应是码表前缀补全 aawt→「工作」(w=2268)，而非只消费 2/3 键的拼音整句「啊啊」；         前 10 条: {:?}",
        &texts[..texts.len().min(10)]
    );
}

/// 反向锁：**纯拼音**方案不受本改动影响（拼音精确档只在混输生效）。
/// 纯拼音下全体候选同为 `Pinyin` 来源，若那个层级键误在此生效，会退化成「is_common 优先」，
/// 把含生僻字的多字词硬降到全部常用单字之后。此处以「打 `xu` 首选仍是最高频字」把住基线。
#[test]
fn pure_pinyin_xu_order_is_unaffected() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let mut cfg = config_with("pinyin");
    cfg.input.filter_mode = "general".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_letter(&coord, 'x');
    press_letter(&coord, 'u');
    let texts = coord.debug_all_candidate_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("需"),
        "纯拼音下 xu 首选应是最高频字「需」，实际: {:?}",
        &texts[..texts.len().min(10)]
    );
}

#[test]
fn test_mixed_wubi_exact_priority() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_mixed(), Some(&data_dir()));
    assert_eq!(coord.active_schema_id(), "wubi86_pinyin");
    // 五笔精确码 aaaa（+10M）应压过拼音候选排首位
    for _ in 0..4 {
        press_letter(&coord, 'a');
    }
    let texts = coord.debug_page_texts();
    assert!(!texts.is_empty(), "混输应有候选");
    // 本用例要钉的是**来源**：五笔精确码压过拼音候选。具体是哪条五笔词条属实现细节，
    // 不可硬编码——`aaaa` 是 gen_dict `[protected_codes]` 保护码，组内次序由上游给定
    // （上游把键名汉字「工」放首位，补权时代则是「恭恭敬敬」在前）。写死任一具体值都会
    // 在词库重新生成的前后各红一次，而「拼音候选跑到首位」这个真正要防的回归照样能被抓住。
    assert!(
        matches!(texts[0].as_str(), "工" | "恭恭敬敬"),
        "五笔精确匹配应排首位（aaaa 的五笔候选为 工/恭恭敬敬），实际: {:?}",
        &texts[..texts.len().min(3)]
    );
}

#[test]
fn test_mixed_pinyin_supplement() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_mixed(), Some(&data_dir()));
    // 输入 nihao（拼音）→ 次引擎应补充拼音候选 你好
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t.contains("你好")),
        "混输应含拼音补充候选 你好，实际: {:?}",
        &texts[..texts.len().min(8)]
    );
}

/// ★ 混输**超码长**回捞的码表前缀候选只解释得了前 N 码，选中时必须只消费那 N 码。
///
/// `yijg` 是五笔全码「就是」，再打一个 `a` 即超码长（五笔 4 码封顶）。引擎的
/// `codetable_owns_overflow` 把「就是」回捞到首位（拼音的 `jg` 不成音节，主张不了这串），
/// 此时它的 `code` 只覆盖 `yijg` —— 选中后 `a` 必须留在缓冲里继续参与输入。
///
/// 修复前码表候选 `consumed_length` 恒 0 ⇒ 协调器 `commit_selected` 的
/// `partial = consumed > 0 && consumed < total` 恒为 false ⇒ 走「消费整串」分支整体上屏，
/// 尾码 `a` 凭空消失。同一条链路上 `github` 打出的是更刺眼的版本（首选「不算」+ 吃掉 `ub`）。
#[test]
fn test_mixed_overflow_prefix_candidate_consumes_only_prefix() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_mixed(), Some(&data_dir()));
    for c in "yijga".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("就是"),
        "前置：回捞的码表候选应在首位，否则下面选中的不是被测候选。实际: {:?}",
        &texts[..texts.len().min(5)]
    );

    // 空格选首选：应留在组合区（分段），而非整体上屏。
    let act = coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN));
    match &act {
        KeyAction::UpdateComposition { text, .. } => assert_eq!(
            text, "就是a",
            "「就是」应进组合区前缀、尾码 a 留在缓冲继续输入"
        ),
        other => panic!("不应整串上屏（那会吃掉尾码 a），实际: {other:?}"),
    }
}

/// ★ 真机回归（用户报告）：混输 + 英文词库下打 `github`，首候选变成五笔词「不算」，
/// 空格上屏还把尾码 `ub` 一并吃掉。
///
/// 成因是超码长归属判据只问了「拼音主张不主张」：`github` 前 4 码 `gith` 在五笔主库确是精确
/// 全码「不算」，而 `gi` 不成音节 ⇒ 拼音交不出候选，于是归属判给码表，码表精确 `+1e7` 把英文
/// 精确档 `+500K` 整层压掉。判据补上「英文主张不主张」后归属回到英文。
///
/// 配置取用户的真实场景：`enable_english` + `auto_commit_block_on_english` 都开（后者不开的话
/// 第 5 键 `githu` 就被顶码顶走了，那是另一条通路，见 `mixed_overflow_codetable_claim.rs` 的
/// `topcode_on_english_word_is_still_governed_by_the_english_guard`）。
#[test]
fn test_mixed_english_word_keeps_overflow_ownership() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_mixed();
    cfg.schema.mix.enable_english = true;
    cfg.schema.mix.auto_commit_block_on_english = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "github".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    assert!(
        !texts.iter().any(|t| t == "不算"),
        "码表前缀候选（只解释得了 gith）不得夺走 github 的归属，实际: {:?}",
        &texts[..texts.len().min(6)]
    );
    assert!(
        texts.iter().any(|t| t.eq_ignore_ascii_case("github")),
        "英文候选 GitHub 应在列，实际: {:?}",
        &texts[..texts.len().min(6)]
    );
}

/// ★ 真机回归（用户报告的原始配置：**英文词库关着**，即出厂默认）：打 `github` 首候选是五笔词
/// 「不算」，空格上屏还把整个缓冲吃掉。
///
/// 此时英文引擎不在场，前三条归属判据全部放行（`gith` 是精确全码、`gi` 不成音节 ⇒ 拼音主张
/// 不了、英文缺席），全靠第四条「拼音须交得出候选」兜住：`github` 拼音一条候选都出不来，
/// 说明它连开头都不在中文语境里，码表没有依据主张它。候选保持为空，空格直接上屏原码。
///
/// 对照 `test_mixed_overflow_prefix_candidate_consumes_only_prefix`：`yijga` 的拼音出得来「以」，
/// 码表照常主张 —— 两条用例只差「拼音交不交得出候选」这一个变量。
#[test]
fn test_mixed_non_chinese_overflow_falls_back_to_raw_code() {
    if !has_schemas() {
        return;
    }
    // 出厂默认即 enable_english=false，此处不额外开启，就是用户的配置。
    let coord = Coordinator::new_headless(config_mixed(), Some(&data_dir()));
    for c in "github".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    assert!(
        texts.is_empty(),
        "github 不该被五笔前 4 码强行解释，候选应为空，实际: {:?}",
        &texts[..texts.len().min(6)]
    );

    let act = coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN));
    match &act {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "github", "空码空格应上屏原码全串")
        }
        other => panic!("应上屏原码 github，实际: {other:?}"),
    }
}

#[test]
fn test_mode_toggle_via_shift() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    assert!(coord.is_chinese_mode());

    // TSF 吃掉 toggle 键的 keydown、仅在干净单击后于 keyUp 转发，故服务端收到 keyUp 即切换。
    coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    assert!(!coord.is_chinese_mode(), "左 Shift 释放应切到英文");

    // 英文模式下字母透传
    let act = press_letter(&coord, 'a');
    assert!(matches!(act, KeyAction::PassThrough), "英文模式字母应透传");

    // 再切回中文（右 Shift 也应生效）
    coord.handle_key_event(&key_event(0xA1, EVENT_KEY_UP));
    assert!(coord.is_chinese_mode(), "右 Shift 释放应切回中文");
}

#[test]
fn test_candidate_op_move_top_and_delete() {
    if !has_schemas() {
        return;
    }
    use wind_ui_types::CandidateOp;
    // candidate_op 的置顶/删除经 self.store 持久化 Shadow 规则，故需注入真实 store
    // （new_headless 的 store=None 会让 pin/delete 变空操作）。
    // 用码表方案（非拼音）：拼音普通候选禁调位（见 handle_candidate.rs 的
    // "拼音普通候选禁调位" 分支——无稳定位置语义，pin 与衰减软置前冲突），MoveTop 恒为空操作。
    let store_path = std::env::temp_dir().join("wind_candidate_op_test.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let coord =
        Coordinator::new_headless_with_store(config_with("wubi86"), Some(&data_dir()), store);
    // 五笔输入 "a" 以获取多个候选
    press_letter(&coord, 'a');
    let before = coord.debug_page_texts();
    if before.len() < 2 {
        return; // 候选不足，跳过
    }
    let second = before[1].clone();

    // 置顶第二项 → 应成为首项
    coord.debug_candidate_op(CandidateOp::MoveTop, 1);
    let after = coord.debug_page_texts();
    assert_eq!(after.first(), Some(&second), "置顶后第二项应排首位");

    // 删除一个多字候选 → 应从候选中消失
    if let Some((pl, w)) = after
        .iter()
        .enumerate()
        .find(|(_, w)| w.chars().count() >= 2)
        .map(|(i, w)| (i, w.clone()))
    {
        coord.debug_candidate_op(CandidateOp::Delete, pl);
        let after2 = coord.debug_page_texts();
        assert!(!after2.contains(&w), "删除后 '{}' 不应再出现", w);
    }
}

/// 用户词与临时词**同文同码**时，右键删除必须把两张表都删掉。
///
/// 这是「删了等于没删」的一条真实来路：`add_user_word` 不清临时表，于是「先被自动学过、
/// 后来又手动加词」的词在 `user_words` 和 `temp_words` 各留一条。删除若只处理其中一张，
/// 剩下那张继续供出同一条候选，屏幕表现与没删一模一样。
///
/// 引擎侧合并分支会把两条并成一条候选（两个标记都置），删除据标记逐表处理。
///
/// ⚠️ 判据必须落在 **store 记录**上，不能用「候选消失」：真机词库下整句层会把同样的文本
/// 重新合成出来，那样删对了候选照样在。真机根因（临时词被合并盖成用户词标记）的守门测试
/// 在引擎侧 `merged_store_candidate_keeps_source_flags`——本仓精简词库的整句层拼不出那条
/// 同文候选，合并分支在这里根本走不到。
#[test]
fn test_delete_word_candidate_removes_both_user_and_temp_records() {
    if !has_schemas() {
        return;
    }
    use wind_ui_types::CandidateOp;
    let store_path = std::env::temp_dir().join("wind_delete_dual_record.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store
        .add_user_word("pinyin", "zaiyebuhao", "再也不好", 800, 0)
        .expect("预置用户词失败");
    store
        .learn_temp_word("pinyin", "zaiyebuhao", "再也不好", 800, 0)
        .expect("预置临时词失败");
    let coord = Coordinator::new_headless_with_store(
        config_with("pinyin"),
        Some(&data_dir()),
        store.clone(),
    );
    for c in "zaiyebuhao".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    let Some(pl) = texts.iter().position(|w| w == "再也不好") else {
        panic!("造词「再也不好」应出现在候选中，实际={:?}", texts);
    };
    coord.debug_candidate_op(CandidateOp::Delete, pl);
    let users = store
        .get_user_words("pinyin", "zaiyebuhao")
        .unwrap_or_default();
    let temps = store
        .get_temp_words("pinyin", "zaiyebuhao")
        .unwrap_or_default();
    assert!(
        !users.iter().any(|r| r.text == "再也不好"),
        "删除后用户词记录应消失，实际残留={:?}",
        users
    );
    assert!(
        !temps.iter().any(|r| r.text == "再也不好"),
        "删除后临时词记录应消失（只删一张表 = 用户眼里的「点了没反应」），实际残留={:?}",
        temps
    );
}

#[test]
fn test_candidate_op_delete_single_char_hides() {
    if !has_schemas() {
        return;
    }
    use wind_ui_types::CandidateOp;
    // 单字保护已取消：隐藏候选对单字同样生效（shadow 按 code+word 键控，
    // 仅该编码下隐藏，设置页可恢复）。
    let store_path = std::env::temp_dir().join("wind_candidate_op_single_test.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let coord =
        Coordinator::new_headless_with_store(config_with("pinyin"), Some(&data_dir()), store);
    for c in "shi".chars() {
        press_letter(&coord, c);
    }
    let before = coord.debug_page_texts();
    if let Some((pl, w)) = before
        .iter()
        .enumerate()
        .find(|(_, w)| w.chars().count() == 1)
        .map(|(i, w)| (i, w.clone()))
    {
        coord.debug_candidate_op(CandidateOp::Delete, pl);
        let after = coord.debug_page_texts();
        assert!(!after.contains(&w), "单字 '{}' 隐藏后不应再出现", w);
    }
}

#[test]
fn test_web_schema_get_config_and_encode_real() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    // schema.getConfig：三层合并视图，应含 schema/engine 段（无 override 时即基础方案）。
    let cfg = coord
        .web_data_rpc("schema.getConfig", &serde_json::json!({ "id": "pinyin" }))
        .unwrap();
    assert!(cfg.is_object(), "getConfig 应返回对象");
    assert!(cfg.get("schema").is_some(), "应含 schema 段");
    assert!(cfg.get("engine").is_some(), "应含 engine 段");
    // dict.encode：拼音方案出拼音码；dict.genPinyin 同源。
    //
    // 契约是**带空格的音节码**（`ni hao`），让设置页用户看清拼音词库的音节格式。
    // 原断言只有 `is_string()`，契约从扁平码改成空格码时它照样绿——弱断言等于没有断言。
    let code = coord
        .web_data_rpc(
            "dict.encode",
            &serde_json::json!({ "schemaId": "pinyin", "text": "你好" }),
        )
        .unwrap();
    assert_eq!(
        code.as_str(),
        Some("ni hao"),
        "dict.encode 应回带空格的音节码"
    );
    let gen_code = coord
        .web_data_rpc("dict.genPinyin", &serde_json::json!({ "text": "你好" }))
        .unwrap();
    assert_eq!(gen_code.as_str(), Some("ni hao"), "dict.genPinyin 同源同形");
}

#[test]
fn test_web_theme_preview_real() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    // theme.preview：内置 default 主题合并 base 链后的配置（只读）。
    let prev = coord
        .web_data_rpc("theme.preview", &serde_json::json!({ "name": "default" }))
        .unwrap();
    assert!(prev.is_object(), "preview 应返回对象");
    // theme.list 至少含若干内置主题
    let list = coord
        .web_data_rpc("theme.list", &serde_json::json!({}))
        .unwrap();
    assert!(
        list.as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "应列出内置主题"
    );
}

#[test]
fn test_stats_recorded_through_deferred_policed() {
    // 回归：生产链路是 bridge → DeferredHandler → Coordinator，bridge 调 handle_key_event_policed。
    // 若 DeferredHandler 不转发 policed，则 Coordinator 的统计埋点被跳过、上屏计数恒为 0。
    // 本测试经 DeferredHandler 走完整 policed 链路，断言 store 真实记录了上屏中文字数。
    if !has_schemas() {
        return;
    }
    use std::sync::Arc;
    use wind_bridge::deferred::DeferredHandler;

    let store_path = std::env::temp_dir().join("wind_stats_deferred_test.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());
    let coord = Coordinator::new_headless_with_store(
        config_with("pinyin"),
        Some(&data_dir()),
        store.clone(),
    );
    let deferred = DeferredHandler::new();
    deferred.set_ready(coord.clone());

    // 经 policed 输入 "nihao" + 空格 → 上屏 你好
    for c in "nihao".chars() {
        let vk = (c.to_ascii_uppercase() as u32) & 0xFF;
        deferred.handle_key_event_policed(&key_event(vk, EVENT_KEY_DOWN));
    }
    let commit = deferred.handle_key_event_policed(&key_event(0x20, EVENT_KEY_DOWN));
    assert!(
        matches!(commit, KeyAction::InsertText { .. }),
        "空格应上屏 InsertText，实际: {:?}",
        commit
    );

    // 统计采集器为后台线程定时落库，测试需显式 flush 才能读到（生产由定时器/关闭时落库）。
    coord.debug_flush_stats();

    // 统计应经 policed 链路真实落库（features.stats.enabled 默认 true）。
    let all = store.daily_stats("2000-01-01", "2099-12-31").unwrap();
    let chinese: u32 = all.iter().map(|(_, r)| r.chinese).sum();
    assert!(
        chinese >= 2,
        "上屏'你好'应记 ≥2 个中文字，实际 chinese={}（policed 埋点未触达？）",
        chinese
    );
    let _ = std::fs::remove_file(&store_path);
}

// ---- select_key overflow（次/三选键越界，对齐 Go handleOverflowSelectKey）----
// 触发场景：五笔 "qqqq" 仅 2 个候选 ["金","狗狗"]，按三选键 '（VK_OEM_7）→ idx=2 越界。

#[test]
fn test_overflow_select_key_ignore_default() {
    if !has_schemas() {
        return;
    }
    // 默认 overflow.select_key = "ignore"：三选键越界（页内候选 < 3）时吞键无效。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    for c in "qqqq".chars() {
        press_letter(&coord, c);
    }
    let count = coord.debug_candidate_count();
    if count == 0 || count >= 3 {
        return; // 需 < 3 才能让 '（三选）越界
    }
    let act = coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN)); // ' VK_OEM_7
    assert!(
        matches!(act, KeyAction::Consumed),
        "默认 ignore 下三选键越界应吞键(Consumed)，实际: {:?}",
        act
    );
}

#[test]
fn test_overflow_select_key_commit() {
    if !has_schemas() {
        return;
    }
    // overflow.select_key = "commit"：越界时只上屏当前高亮候选，不追加触发键字符。
    let mut cfg = config_with("wubi86");
    cfg.keys.overflow.select_key = "commit".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "qqqq".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if texts.is_empty() || texts.len() >= 3 {
        return;
    }
    let highlighted = texts[0].clone();
    match coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, highlighted, "commit 应只上屏高亮候选，无追加字符");
        }
        other => panic!("commit 应 InsertText，实际: {:?}", other),
    }
}

#[test]
fn test_overflow_select_key_commit_and_input() {
    if !has_schemas() {
        return;
    }
    // overflow.select_key = "commit_and_input"：越界时上屏高亮候选 + 追加（转换后的）触发键字符。
    let mut cfg = config_with("wubi86");
    cfg.keys.overflow.select_key = "commit_and_input".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "qqqq".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if texts.is_empty() || texts.len() >= 3 {
        return;
    }
    let highlighted = texts[0].clone();
    match coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert!(
                text.starts_with(&highlighted),
                "commit_and_input 应以高亮候选开头，实际: {}",
                text
            );
            assert!(
                text.chars().count() > highlighted.chars().count(),
                "commit_and_input 应在候选后追加触发键字符，实际: {}",
                text
            );
        }
        other => panic!("commit_and_input 应 InsertText，实际: {:?}", other),
    }
}

// ---- 有候选时按融合「快捷」触发键：顶字 + 进融合模式（现唯一的快捷输入形态，支持拼音）----

#[test]
fn test_semicolon_with_candidates_enters_mix_and_accepts_pinyin() {
    if !has_schemas() {
        return;
    }
    // 隔离选词职责（select_key_groups 置空），专测「有候选 → 按 ; 顶字 + 进融合 → 可打拼音」。
    let mut cfg = config_with("wubi86");
    cfg.keys.select_key_groups = vec![];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_letter(&coord, 'a'); // 产生候选
    let texts = coord.debug_page_texts();
    if texts.is_empty() {
        return;
    }
    let highlighted = texts[0].clone();
    // 默认 top_commit_mode=direct_commit：真提交高亮候选、前缀新组合延迟到 keyup 才开。
    match coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)) {
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            ..
        } => {
            assert_eq!(commit_text, highlighted, "; 应顶字真提交当前高亮候选");
            assert_eq!(deferred_composition, ";", "进入融合模式应延迟开前缀组合 ;");
        }
        other => panic!("有候选按 ; 应顶字+进融合模式，实际: {:?}", other),
    }
    // 融合模式输入拼音 nihao → 候选应含「你好」（拼音成员生效，证明能打拼音）
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t.contains("你好")),
        "融合模式应能输入拼音（nihao→你好），实际: {:?}",
        texts
    );
}

#[test]
fn test_semicolon_overflow_falls_to_mix_not_overflow() {
    if !has_schemas() {
        return;
    }
    // ; 同时是选词键(默认 semicolon_quote)与融合触发键；恰好 1 个候选时次选越界
    // → 不走 overflow，而是顶字 + 进融合（对齐 Go 优先级：选词 < 进模式 < overflow）。
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    for c in "yyyg".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if texts.len() != 1 {
        return; // 需恰好 1 个候选让 ; 次选越界
    }
    let only = texts[0].clone();
    match coord.handle_key_event(&key_event(0xBA, EVENT_KEY_DOWN)) {
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            ..
        } => {
            assert_eq!(commit_text, only, "1 候选时 ; 应顶字真提交该候选");
            assert_eq!(
                deferred_composition, ";",
                "并进入融合模式（延迟开前缀组合）"
            );
        }
        other => panic!("1 候选时 ; 应顶字+进融合，实际: {:?}", other),
    }
}

#[test]
fn test_special_trigger_with_candidates_commits_and_enters() {
    if !has_schemas() {
        return;
    }
    // 特殊模式引导键在「有候选」时应与 mix/临拼一致：顶屏高亮候选 + 进模式
    // （此前只有空缓冲入口，有候选时 \ 落标点流程上屏 、）。默认 direct_commit：
    // 真提交候选、引导符新组合延迟到 keyup 才开。
    let mut cfg = config_with("wubi86");
    let ov = overlay_override_dir(
        "test_special_trigger_with_candidates_com",
        &[("pinyin", false)],
    );
    bind_special(&mut cfg, "backslash", "pinyin");
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov));
    press_letter(&coord, 'a');
    let texts = coord.debug_page_texts();
    if texts.is_empty() {
        return;
    }
    let highlighted = texts[0].clone();
    match coord.handle_key_event(&key_event(0xDC, EVENT_KEY_DOWN)) {
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            ..
        } => {
            assert_eq!(commit_text, highlighted, "\\ 应顶字真提交当前高亮候选");
            assert_eq!(deferred_composition, "\\", "进入特殊模式应延迟开引导符组合");
        }
        other => panic!("有候选按 \\ 应顶屏+进特殊模式，实际: {:?}", other),
    }
    // 已在特殊模式：后续输入走其引用方案，组合区以引导符 \ 开头。
    let act = press_letter(&coord, 'n');
    let preedit = action_text(&act).unwrap();
    assert!(
        preedit.starts_with('\\'),
        "顶屏进入后应处于特殊模式（组合区以 \\ 开头），实际: {}",
        preedit
    );
}

// ---- 以词定字（select_char_keys，对齐 Go handleSelectChar/handleSelectCharWithOverflow）----
// comma_period 组：`,`(VK_OEM_COMMA=0xBC) 取第 1 字，`.`(VK_OEM_PERIOD=0xBE) 取第 2 字。

#[test]
fn test_select_char_first_and_second() {
    if !has_schemas() {
        return;
    }
    // 启用以词定字 comma_period：从当前高亮候选词逐字上屏。
    let mut cfg = config_with("pinyin");
    cfg.keys.select_char_keys = vec!["comma_period".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if texts.is_empty() {
        return;
    }
    let word: Vec<char> = texts[0].chars().collect();
    if word.len() < 2 {
        return; // 需高亮词 ≥ 2 字方能测第 1/第 2 字
    }
    // `,` → 取第 1 字
    match coord.handle_key_event(&key_event(0xBC, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, word[0].to_string(), ", 应上屏高亮词第 1 字");
        }
        other => panic!(", 应以词定字上屏第 1 字，实际: {:?}", other),
    }
    // 重新输入，`.` → 取第 2 字
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, word[1].to_string(), ". 应上屏高亮词第 2 字");
        }
        other => panic!(". 应以词定字上屏第 2 字，实际: {:?}", other),
    }
}

#[test]
fn test_fullwidth_space_on_empty_buffer() {
    if !has_schemas() {
        return;
    }
    // 全角态空缓冲按空格 → 上屏全角空格 U+3000（对齐设置端展示基线与微软拼音行为）。
    // 回归：空格键先于标点流水线被 VK_SPACE 分支截获，空缓冲曾恒 PassThrough 半角空格，
    // 全角转换（fullwidth.rs 已支持 ' '→U+3000）与自定义映射「空格」行均够不着。
    let mut cfg = config_with("pinyin");
    cfg.input.default.full_width = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "\u{3000}", "全角态空格应上屏全角空格");
        }
        other => panic!("全角态空缓冲空格应上屏全角空格，实际: {:?}", other),
    }
    // 半角态（默认）维持透传，保留宿主对空格键的原生语义。
    let coord2 = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    assert!(
        matches!(
            coord2.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)),
            KeyAction::PassThrough
        ),
        "半角态空缓冲空格应透传"
    );
}

#[test]
fn test_select_char_brackets_group() {
    if !has_schemas() {
        return;
    }
    // 回归：select_char_index 曾误用选词键组解析（select_key_vks 不识别 brackets），
    // 致配置 brackets 后 `[`/`]` 直接走标点流水线上屏【】。brackets 仅存在于
    // select_char_vks，须用它解析。`[`(VK_OEM_4=0xDB) 取第 1 字，`]`(VK_OEM_6=0xDD) 取第 2 字。
    let mut cfg = config_with("pinyin");
    cfg.keys.select_char_keys = vec!["brackets".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if texts.is_empty() {
        return;
    }
    let word: Vec<char> = texts[0].chars().collect();
    if word.len() < 2 {
        return; // 需高亮词 ≥ 2 字方能测第 1/第 2 字
    }
    // `[` → 取第 1 字
    match coord.handle_key_event(&key_event(0xDB, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, word[0].to_string(), "[ 应上屏高亮词第 1 字");
        }
        other => panic!("[ 应以词定字上屏第 1 字，实际: {:?}", other),
    }
    // 重新输入，`]` → 取第 2 字
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    match coord.handle_key_event(&key_event(0xDD, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, word[1].to_string(), "] 应上屏高亮词第 2 字");
        }
        other => panic!("] 应以词定字上屏第 2 字，实际: {:?}", other),
    }
}

#[test]
fn test_select_char_disabled_by_default() {
    if !has_schemas() {
        return;
    }
    // 默认 select_char_keys 为空 → `,` 不作以词定字，走正常标点流水线（零回归）。
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if texts.is_empty() {
        return;
    }
    let first_char = texts[0].chars().next().unwrap().to_string();
    let act = coord.handle_key_event(&key_event(0xBC, EVENT_KEY_DOWN));
    if let KeyAction::InsertText { text, .. } = &act {
        assert_ne!(
            *text, first_char,
            "默认禁用时 , 不应只上屏首字（应走标点：顶词+逗号）"
        );
    }
}

// ---- 临时词晋升闭环 promote_count ----

#[test]
fn temp_word_promotes_after_threshold_selections() {
    // 验证 6a 造词路径晋升闭环：
    // - get_temp_word 点查 API 正确反映 count
    // - count >= promote_count → promote_temp_word 晋升入用户词库
    // - 晋升后临时层删除（get_temp_word → None），用户层新增
    // - promote_count=0 禁用语义：永不晋升（零回归保证）
    //
    // 注：6b 整词选中路径需要引擎把临时层词条作为普通候选返回；
    // 无头 harness 中引擎与 store 临时层未直接联通，该路径由
    // handle_addword.rs 内 learn_phrase_on_commit 单元测试覆盖。
    use std::sync::Arc;

    let store_path = std::env::temp_dir().join("wind_promote_thresh_integ.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());

    // 1. 两次累积 → count=2
    let c1 = store
        .learn_temp_word("wubi86", "abcd", "测试", 800, 0)
        .unwrap();
    assert_eq!(c1, 1, "第 1 次 count 应为 1");
    assert_eq!(
        store.get_temp_word("wubi86", "abcd", "测试").unwrap(),
        Some(1),
        "get_temp_word 应返回 count=1"
    );

    let c2 = store
        .learn_temp_word("wubi86", "abcd", "测试", 800, 0)
        .unwrap();
    assert_eq!(c2, 2, "第 2 次 count 应为 2");

    // 2. count=2 >= promote_count=2 → 晋升
    assert!(
        store.promote_temp_word("wubi86", "abcd", "测试").unwrap(),
        "count 达阈值时 promote 应返回 true"
    );
    assert_eq!(
        store.get_temp_word("wubi86", "abcd", "测试").unwrap(),
        None,
        "晋升后临时层应删除"
    );
    let user = store.get_user_words("wubi86", "abcd").unwrap();
    assert!(
        user.iter().any(|r| r.text == "测试"),
        "晋升后用户词层应含该词"
    );

    // 3. promote_count=0 禁用语义：手动验证 maybe_promote_temp 语义等价
    //    （当 promote_count=0 时，coordinator 永不调用 promote_temp_word）。
    //    此处用 get_temp_word None → 确认未晋升的词不在临时层。
    store
        .learn_temp_word("wubi86", "zzzz", "不晋升", 800, 0)
        .unwrap();
    // promote_count=0 时不晋升：临时层仍有该词
    assert_eq!(
        store.get_temp_word("wubi86", "zzzz", "不晋升").unwrap(),
        Some(1),
        "promote_count=0 时临时层应保留"
    );

    // 4. 不存在的词返回 None
    assert_eq!(
        store.get_temp_word("wubi86", "xxxx", "无").unwrap(),
        None,
        "不存在的词应返回 None"
    );

    let _ = std::fs::remove_file(&store_path);
}

#[test]
fn test_select_char_overflow_ignore_default() {
    if !has_schemas() {
        return;
    }
    // 高亮词仅 1 字时按 `.`（取第 2 字）越界，默认 overflow.select_char_key = ignore → 吞键。
    let mut cfg = config_with("wubi86");
    cfg.keys.select_char_keys = vec!["comma_period".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "qqqq".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if texts.is_empty() || texts[0].chars().count() != 1 {
        return; // 需高亮为单字词方能让 . 越界
    }
    let act = coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)); // . VK_OEM_PERIOD
    assert!(
        matches!(act, KeyAction::Consumed),
        "默认 ignore 下以词定字越界应吞键(Consumed)，实际: {:?}",
        act
    );
}

#[test]
fn test_select_char_overflow_commit() {
    if !has_schemas() {
        return;
    }
    // overflow.select_char_key = commit：越界时上屏当前高亮候选，不追加触发键字符。
    let mut cfg = config_with("wubi86");
    cfg.keys.select_char_keys = vec!["comma_period".into()];
    cfg.keys.overflow.select_char_key = "commit".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "qqqq".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if texts.is_empty() || texts[0].chars().count() != 1 {
        return;
    }
    let highlighted = texts[0].clone();
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, highlighted, "commit 应只上屏高亮候选，无追加字符");
        }
        other => panic!("commit 应 InsertText，实际: {:?}", other),
    }
}

#[test]
fn test_select_char_overflow_commit_and_input() {
    if !has_schemas() {
        return;
    }
    // overflow.select_char_key = commit_and_input：越界时上屏高亮候选 + 追加转换后的触发键字符。
    let mut cfg = config_with("wubi86");
    cfg.keys.select_char_keys = vec!["comma_period".into()];
    cfg.keys.overflow.select_char_key = "commit_and_input".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "qqqq".chars() {
        press_letter(&coord, c);
    }
    let texts = coord.debug_page_texts();
    if texts.is_empty() || texts[0].chars().count() != 1 {
        return;
    }
    let highlighted = texts[0].clone();
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert!(
                text.starts_with(&highlighted),
                "commit_and_input 应以高亮候选开头，实际: {}",
                text
            );
            assert!(
                text.chars().count() > highlighted.chars().count(),
                "commit_and_input 应在候选后追加触发键字符，实际: {}",
                text
            );
        }
        other => panic!("commit_and_input 应 InsertText，实际: {:?}", other),
    }
}

#[test]
fn test_english_stats_callable_without_store() {
    // handle_english_stats 无 store 时应静默跳过，不崩溃。
    // 验证 MessageHandler trait 接口存在且协调器已实现。
    let coord = Coordinator::new_headless(config_with("wubi86"), None);
    coord.handle_english_stats(5, 3, 2, 1);
}

fn config_with_english_trigger(active: &str, trigger: &str) -> wind_config::Config {
    let mut cfg = config_with(active);
    cfg.input.temp_english.trigger_keys = vec![trigger.to_string()];
    cfg
}

#[test]
fn test_temp_english_trigger_key_shows_prefix() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        config_with_english_trigger("wubi86", "slash"),
        Some(&data_dir()),
    );
    // 空缓冲按 / 进入临时英文，preedit 应显示前缀 "/"
    let act = coord.handle_key_event(&key_event(0xBF, EVENT_KEY_DOWN));
    assert_eq!(
        action_text(&act).as_deref(),
        Some("/"),
        "触发键进入临时英文，preedit 应显示前缀 /"
    );
}

#[test]
fn test_temp_english_trigger_key_prefix_in_preedit() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        config_with_english_trigger("wubi86", "slash"),
        Some(&data_dir()),
    );
    // 触发键进入后继续输入字母，preedit = 前缀 + 缓冲
    coord.handle_key_event(&key_event(0xBF, EVENT_KEY_DOWN)); // /
    let act = press_letter(&coord, 'h');
    assert_eq!(
        action_text(&act).as_deref(),
        Some("/h"),
        "输入 h 后 preedit 应为 /h"
    );
}

#[test]
fn test_temp_english_trigger_key_enter_empty_commits_prefix() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        config_with_english_trigger("wubi86", "slash"),
        Some(&data_dir()),
    );
    // 触发键进入，空缓冲直接回车 → 上屏触发键字符 "/"
    coord.handle_key_event(&key_event(0xBF, EVENT_KEY_DOWN)); // /
    let act = coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)); // Enter
    assert_eq!(
        action_text(&act).as_deref(),
        Some("/"),
        "空缓冲回车应上屏触发键字符 /"
    );
}

/// Bug 复现（协调层）：双拼模式下，存储在 "pinyin" 域的用户词应出现在候选中。
/// 小鹤双拼输入 "dabologe" → 全拼 "daboluoge"，store 中有该用户词，候选应包含「大菠萝哥」。
#[test]
fn test_shuangpin_userword_appears_in_candidates() {
    let d = data_dir();
    let sp_schema = d.join("schemas/shuangpin.schema.toml");
    if !sp_schema.exists() {
        eprintln!("跳过：缺少 shuangpin.schema.toml");
        return;
    }

    // 构造带用户词的 store
    let store_path = std::env::temp_dir().join("wind_sp_userword_test.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    // 用户词存在 "pinyin" 域（拼音族共享存储的规范 schema_id）
    store
        .add_user_word("pinyin", "daboluoge", "大菠萝哥", 0, 0)
        .expect("add_user_word 失败");

    // 创建双拼方案协调器并注入 store
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".into()];
    cfg.schema.active = "shuangpin".into();
    cfg.input.default.chinese_mode = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&d), store);

    // 输入小鹤双拼 "dabologe" → 应转换为全拼 "daboluoge"
    for c in "dabologe".chars() {
        press_letter(&coord, c);
    }

    let all = coord.debug_all_candidate_texts();
    assert!(
        all.iter().any(|t| t == "大菠萝哥"),
        "双拼输入 \"dabologe\" 经转换后应命中用户词「大菠萝哥」，实际候选: {:?}",
        all
    );

    let _ = std::fs::remove_file(&store_path);
}

// 顶码触发序列：wubi86 下 skce 满码，第 5 键 y 溢出 → 顶码上屏，余码 y。
fn drive_top_code(coord: &Coordinator) -> KeyAction {
    for ch in ['s', 'k', 'c', 'e'] {
        press_letter(coord, ch);
    }
    // 'y' = VK 0x59
    coord.handle_key_event(&key_event(0x59, EVENT_KEY_DOWN))
}

#[test]
fn top_code_pre_confirm_returns_insert_text() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.punct_commit = true;
    cfg.schema.codetable.top_code_commit = true;
    cfg.input.top_commit_mode = wind_config::TopCommitMode::PreConfirm;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    match drive_top_code(&coord) {
        KeyAction::InsertText {
            has_new_composition,
            ..
        } => {
            assert!(has_new_composition, "顶码应带余码新组合");
        }
        other => panic!("pre_confirm 顶码应返回 InsertText，实际: {:?}", other),
    }
}

#[test]
fn top_code_direct_commit_returns_commit_then_defer() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.punct_commit = true;
    cfg.schema.codetable.top_code_commit = true;
    cfg.input.top_commit_mode = wind_config::TopCommitMode::DirectCommit;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    match drive_top_code(&coord) {
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            timeout_ms,
        } => {
            assert!(!commit_text.is_empty(), "应有顶出文本");
            assert!(!deferred_composition.is_empty(), "应有余码新组合");
            assert_eq!(timeout_ms, 150);
        }
        other => panic!(
            "direct_commit 顶码应返回 CommitThenDeferComposition，实际: {:?}",
            other
        ),
    }
}

// 顶码前缓冲 skce 注入短语/命令作首选，用于验证顶码上屏对短语(cmdbar)类型生效。
//
// ⚠️ 短语权重 3000 是**必需的**，不是随手写的大数：五笔 `skce` 是「可能」(w=2301) 的全码，
// 二者同属精确档（`is_exact_code` 均为 true），先后由权重裁决。此处曾写 100，靠
// `PHRASE_WEIGHT_BASE`(40M) 兜着才排首；该常量删除后短语立刻输给「可能」，本 helper 的
// 三个测试同时失败——这正是删 40M 的预期效果，故是**调权重**而非改断言。
//
// 取 3000 不取 2302：留出余量，词库下次按词频重排后 2301 会变。仍在约定值域 0~10000 内。
fn coord_with_skce_phrase(
    phrase_text: &str,
    mode: wind_config::TopCommitMode,
    tag: &str,
) -> std::sync::Arc<Coordinator> {
    let store_path = std::env::temp_dir().join(format!("wind_top_code_phrase_{tag}.redb"));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    // build() 构造期读 enabled_phrases_for_input()，故须在建 coordinator 前入库。
    store.add_phrase("skce", phrase_text, 0, 3000).unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.punct_commit = true;
    cfg.schema.codetable.top_code_commit = true;
    cfg.input.top_commit_mode = mode;
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store)
}

/// 精确码短语与码表精确候选**同属精确档、先后完全由权重裁决**——`PHRASE_WEIGHT_BASE`(40M)
/// 删除后这一步比较才第一次真正生效。
///
/// 现场：五笔 `skce` 既是系统词「可能」(w=2301) 的全码，也可作短语码。40M 在时短语恒赢，
/// 这个比较等于从未执行过（本仓混输六个权重加成也是这样，拆掉后一批从未跑过的比较第一次
/// 生效，而当时全套测试一条没红）。
///
/// ⚠️ **两个方向都测**。只断言「高权重短语排首」证明不了裁决者是权重——40M 时代同样是那个
/// 结果，测试会一路绿到底。低权重方向失败才说明比较真的发生了。
///
/// 层次上刻意放在用户入口（打 s-k-c-e 看候选），而非排序函数的单元测试：引擎/排序全绿
/// 不等于打得出，二者要各有一份。
#[test]
fn phrase_and_codetable_exact_compete_by_weight() {
    if !has_schemas() {
        return;
    }
    // 「可能」的词频会随词库重排变动，故断言只比「谁在前」，不写死 2301。
    for (weight, want_first) in [(100, "可能"), (3000, "短语文本")] {
        let store_path = std::env::temp_dir().join(format!("wind_phrase_vs_exact_{weight}.redb"));
        let _ = std::fs::remove_file(&store_path);
        let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
        store.add_phrase("skce", "短语文本", 0, weight).unwrap();
        let coord =
            Coordinator::new_headless_with_store(config_with("wubi86"), Some(&data_dir()), store);
        for ch in ['s', 'k', 'c', 'e'] {
            press_letter(&coord, ch);
        }
        let texts = coord.debug_all_candidate_texts();
        assert_eq!(
            texts.first().map(String::as_str),
            Some(want_first),
            "短语权重 {weight} vs 五笔「可能」：首选应为 {want_first}，实际 {texts:?}"
        );
        let _ = std::fs::remove_file(&store_path);
    }
}

#[test]
fn top_code_plain_phrase_first_commits_phrase_text() {
    if !has_schemas() {
        return;
    }
    // 普通短语作 skce 首选：顶码应上屏短语文本 + 余码 y 续打（pre_confirm）。
    let coord = coord_with_skce_phrase(
        "顶码短语文本",
        wind_config::TopCommitMode::PreConfirm,
        "plain",
    );
    match drive_top_code(&coord) {
        KeyAction::InsertText {
            text,
            has_new_composition,
            ..
        } => {
            assert_eq!(text, "顶码短语文本", "顶码应上屏短语首选文本");
            assert!(has_new_composition, "顶码应带余码 y 新组合");
        }
        other => panic!("普通短语顶码应返回 InsertText，实际: {:?}", other),
    }
}

#[test]
fn top_code_text_command_first_commits_evaluated_text() {
    if !has_schemas() {
        return;
    }
    // 纯文本 $CC 命令（type 文本，无副作用）作 skce 首选：顶码同步求值命令文本上屏，
    // 而非上屏 display 标签「标签」（区分命令求值路径与普通短语路径）。
    let coord = coord_with_skce_phrase(
        r#"$CC("标签", type("命令文本"))"#,
        wind_config::TopCommitMode::PreConfirm,
        "textcmd",
    );
    match drive_top_code(&coord) {
        KeyAction::InsertText {
            text,
            has_new_composition,
            ..
        } => {
            assert_eq!(
                text, "命令文本",
                "纯文本命令顶码应上屏求值文本(而非 display 标签)"
            );
            assert!(has_new_composition, "顶码应带余码 y 新组合");
        }
        other => panic!("纯文本命令顶码应返回 InsertText，实际: {:?}", other),
    }
}

#[test]
fn top_code_phrase_code_no_codetable_char_still_commits() {
    if !has_schemas() {
        return;
    }
    // 用户真机场景：短语专属码 date（五笔码表无字）敲满码后再敲字符应顶短语 + 余码续打。
    // 引擎 handle_top_code 原 `first()?` 在 prefix 无字时短路 None → 顶码不触发（datea 累积）；
    // 修复后返回 Some(("", 余码))，coordinator 用短语显示首选顶码。用内置 date 日期短语验证
    // （系统短语须真 store 才同步，见 test_phrase_date_expansion）。
    let store_path = std::env::temp_dir().join("wind_top_code_datecode.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.punct_commit = true;
    cfg.schema.codetable.top_code_commit = true;
    cfg.input.top_commit_mode = wind_config::TopCommitMode::PreConfirm;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['d', 'a', 't', 'e'] {
        press_letter(&coord, ch);
    }
    // 'g' = VK 0x47（溢出触发键，dateg=5>满码4 且码表无匹配）→ 顶 date 日期短语，余码 g
    match coord.handle_key_event(&key_event(0x47, EVENT_KEY_DOWN)) {
        KeyAction::InsertText {
            text,
            has_new_composition,
            ..
        } => {
            assert!(
                text.contains('年') && text.contains('月') && text.contains('日'),
                "date 短语码(码表无字)溢出应顶出日期短语，实际: {:?}",
                text
            );
            assert!(has_new_composition, "应带余码 g 新组合");
        }
        other => panic!(
            "date 短语码顶码应返回 InsertText(顶短语)，实际: {:?}(顶码未触发?)",
            other
        ),
    }
    let _ = std::fs::remove_file(&store_path);
}

#[test]
fn phrase_auto_commit_unique_exact_no_longer() {
    if !has_schemas() {
        return;
    }
    // 开启「全码唯一自动上屏」时，唯一精确码短语（无更长后继）应自动上屏。
    // 引擎 decide_auto_commit 只认码表候选（短语 code 空、且在引擎 convert 后追加），故短语原不进
    // 判据；phrase_auto_commit 补齐。注入短语码 kkkkx（五笔码表 4 码封顶，5 码处必无码表候选，
    // 短语成唯一候选）；kkkk 处有多个码表候选（非唯一）→ 第 4 键不会提前自动上屏。
    let store_path = std::env::temp_dir().join("wind_phrase_autocommit.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase("kkkkx", "唯一测试短语", 0, 100).unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.auto_commit_at_full = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    // 第 5 键 'x'(VK 0x58) → 只剩注入短语 kkkkx 唯一 + 无更长后继 → 自动上屏
    match coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "唯一测试短语", "唯一精确码短语应自动上屏其文本");
        }
        other => panic!(
            "短语全码唯一应自动上屏(InsertText)，实际: {:?}(未触发自动上屏?)",
            other
        ),
    }
    let _ = std::fs::remove_file(&store_path);
}

/// **码长超过方案满码长的短语不得被顶码劫走**（回归：5 码短语落在 4 码五笔方案里）。
///
/// 上一个测试用 `Config::default()`，其 `top_code_commit` 结构体零值是 `false`——而出厂
/// toml 里是 `true`（`config.rs` 的 `[schema.codetable] top_code_commit = true`）。于是它
/// 验证的自动上屏路径**在真机默认配置下压根走不到**：顶码排在 `update_candidates` 之前
/// 且命中即 return。本测试把开关拨到出厂值，堵上那个盲区。
///
/// 修复前实测：`CommitThenDeferComposition { commit_text: "串口", deferred_composition: "x" }`
/// ——`kkkk` 的码表首选被顶上屏、余码 `x` 落回缓冲，`kkkkx` 这条短语永远打不出来。
/// 真机现场是 5 码短语 `zzsfz`：`zzsf` 在码表无字，顶上屏的正是该短语自身的前缀候选，
/// 于是「词条上屏了，但多出一个 z」。
#[test]
fn top_code_yields_to_overlong_phrase() {
    if !has_schemas() {
        return;
    }
    let store_path = std::env::temp_dir().join("wind_phrase_overlong_topcode.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase("kkkkx", "唯一测试短语", 0, 100).unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.auto_commit_at_full = true;
    cfg.schema.codetable.top_code_commit = true; // 出厂即开，正是真机默认
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    // 第 5 键 'x'：短语侧有精确码 kkkkx → 否决顶码 → 落到自动上屏，且**不得留余码**。
    match coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "唯一测试短语", "应整体上屏短语，而非顶码劫持");
        }
        other => panic!("超长短语应整体上屏，实际: {:?}(顶码劫持? 余码残留?)", other),
    }
    let _ = std::fs::remove_file(&store_path);
}

/// 对照组：同码长的**码表用户词**本就不受影响（引擎 `has_full_input_match` 查得到它）。
/// 配它是为了钉死「差异来自存放位置（短语库 vs 码表词库），不是码长本身」——
/// 真机反馈正是「`zzsfz` 不行、`abcde` 没问题」，两者恰好分居两处。
#[test]
fn top_code_never_hijacked_overlong_user_word() {
    if !has_schemas() {
        return;
    }
    let store_path = std::env::temp_dir().join("wind_userword_overlong_topcode.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store
        .add_user_word("wubi86", "kkkkx", "用户词条", 0, 0)
        .unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.auto_commit_at_full = true;
    cfg.schema.codetable.top_code_commit = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    match coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "用户词条", "码表用户词应整体上屏");
        }
        other => panic!("码表用户词应整体上屏，实际: {:?}", other),
    }
    let _ = std::fs::remove_file(&store_path);
}

/// **短语的上屏文本必须保留真换行**——`Candidate::text` 就是上屏内容，不得在装配期改写。
///
/// 回归背景：短语候选装配曾调 `clamp_candidate_display` 做「一行化」（换行/制表→空格），
/// 而用户词候选**不走这一步**。于是同一条含换行的内容，用户词能上屏换行、短语上屏成空格；
/// 候选窗里也看不到 ↵，因为 text 里的换行早在装配期就没了。
///
/// 一行化是**显示层**关注点（杜绝多行候选撑破候选窗），现由渲染层 `visible_whitespace`
/// 承担（换行→↵、制表→⇥，只投影不改数据）。数据层不得再有同类改写。
#[test]
fn phrase_commit_keeps_real_newline() {
    if !has_schemas() {
        return;
    }
    let store_path = std::env::temp_dir().join("wind_phrase_keep_newline.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    // 与 phrase_auto_commit 同构造：kkkkx 在五笔 5 码处必无码表候选 → 短语唯一 → 自动上屏。
    store.add_phrase("kkkkx", "甲\n乙", 0, 100).unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.auto_commit_at_full = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    match coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(
                text, "甲\n乙",
                "短语上屏文本须保留真换行；若得到「甲 乙」说明装配期又做了一行化"
            );
        }
        other => panic!("短语全码唯一应自动上屏(InsertText)，实际: {:?}", other),
    }
    let _ = std::fs::remove_file(&store_path);
}

#[test]
fn phrase_auto_commit_effect_command_executes() {
    if !has_schemas() {
        return;
    }
    // 含副作用 $CC 命令（Effect 动作）作唯一精确码短语：不再被自动上屏排除，
    // 应清组合并异步执行（与空格选中命令同语义 → ClearComposition）。
    // ask() 为未实现 Effect（异步执行仅 warn 降级），测试无真实副作用。
    let store_path = std::env::temp_dir().join("wind_phrase_autocmd_effect.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store
        .add_phrase("kkkkx", r#"$CC("标签", ask("x"))"#, 0, 100)
        .unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.auto_commit_at_full = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    // 第 5 键 'x' → 唯一含副作用命令候选 + 无更长后继 → 清组合 + 异步执行
    match coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!(
            "含副作用命令全码唯一应清组合并异步执行(ClearComposition)，实际: {:?}",
            other
        ),
    }
    assert!(
        coord.debug_all_candidate_texts().is_empty(),
        "命令自动执行后候选应已清空"
    );
    let _ = std::fs::remove_file(&store_path);
}

/// 精确匹配模式（`single_code_input`）+ 空码补全（`single_code_complete`）下，短语前缀
/// 补全**只出首选一条**——与码表引擎同分支「从更长编码取首个候选」的规格一致。
///
/// 回归：原 `allow_prefix` 在补全分支放行整串前缀命中，致空码补全冒出多条「后续」。
/// 注入同前缀 zzq 的三条短语（码 zzqa/zzqb/zzqc，五笔无 zzq 精确字 → 触发补全分支）。
fn coord_with_prefix_phrases(complete: bool) -> std::sync::Arc<Coordinator> {
    let store_path =
        std::env::temp_dir().join(format!("wind_phrase_complete_{}.redb", complete as u8));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    // 权重递增：首选应为权重最高的 zzqc。
    store.add_phrase("zzqa", "短语甲", 0, 10).unwrap();
    store.add_phrase("zzqb", "短语乙", 0, 20).unwrap();
    store.add_phrase("zzqc", "短语丙", 0, 30).unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.single_code_input = true;
    cfg.schema.codetable.single_code_complete = complete;
    cfg.input.phrase.min_prefix = 2;
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store)
}

#[test]
fn exact_mode_phrase_complete_yields_single_hit() {
    if !has_schemas() {
        return;
    }
    let coord = coord_with_prefix_phrases(true);
    for ch in ['z', 'z', 'q'] {
        press_letter(&coord, ch);
    }
    let texts = coord.debug_all_candidate_texts();
    let phrase_hits: Vec<&String> = texts.iter().filter(|t| t.starts_with("短语")).collect();
    assert_eq!(
        phrase_hits.len(),
        1,
        "精确模式空码补全应只出首选一条短语，实际: {:?}",
        texts
    );
    assert_eq!(
        phrase_hits[0], "短语丙",
        "补全应取权重最高的首选（HashMap 序不定，须先定序）"
    );
}

#[test]
fn exact_mode_without_complete_suppresses_phrase_prefix() {
    if !has_schemas() {
        return;
    }
    // 补全关闭：精确模式应彻底抑制短语前缀枚举（证明上一个测试的一条来自补全分支）。
    let coord = coord_with_prefix_phrases(false);
    for ch in ['z', 'z', 'q'] {
        press_letter(&coord, ch);
    }
    let texts = coord.debug_all_candidate_texts();
    assert!(
        !texts.iter().any(|t| t.starts_with("短语")),
        "补全关闭时精确模式不应出短语前缀候选，实际: {:?}",
        texts
    );
}

/// 精确匹配空码补全的判据须落在**最终显示列表**上，而非某一层的局部视野。
///
/// 回归：码表引擎在协调器注入短语**之前**按自己那半边判空，于是 `aab`（五笔全库无精确字、
/// 主库有 aabx 后继）无条件被补上一条更长编码候选；随后精确码短语 aab 再进来 → 屏幕上短语
/// 旁边多出一条与输入无关的「后续」。反向同源：引擎抢先把列表填非空，又会让短语侧的补全
/// 枚举误判「已有候选」而放弃，该补的短语反倒不补。
///
/// `aab` 的选取依据：六个 wubi86 词库均无 code=="aab" 的精确项，主库有 4 条 aab? 后继——
/// 即「码表侧必然想补、且补得出来」，是这个 bug 的最小复现条件。
fn coord_exact_completion(with_phrase: bool) -> std::sync::Arc<Coordinator> {
    let store_path =
        std::env::temp_dir().join(format!("wind_exact_completion_{}.redb", with_phrase as u8));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    if with_phrase {
        store.add_phrase("aab", "短语占位", 0, 10).unwrap();
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.single_code_input = true;
    cfg.schema.codetable.single_code_complete = true;
    cfg.input.phrase.min_prefix = 2;
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store)
}

#[test]
fn exact_mode_completion_yields_to_phrase() {
    if !has_schemas() {
        return;
    }
    let coord = coord_exact_completion(true);
    for ch in ['a', 'a', 'b'] {
        press_letter(&coord, ch);
    }
    let texts = coord.debug_all_candidate_texts();
    assert_eq!(
        texts,
        vec!["短语占位".to_string()],
        "短语已占位时不应再补码表后续——补全以最终屏幕候选数为准，实际: {:?}",
        texts
    );
}

#[test]
fn exact_mode_completion_fires_without_phrase() {
    if !has_schemas() {
        return;
    }
    // 对照组：同一编码在无短语时仍应补上一条码表后续。证明上一个测试里「没有多余候选」
    // 来自补全**让位**，而不是补全整体被改坏了。
    let coord = coord_exact_completion(false);
    for ch in ['a', 'a', 'b'] {
        press_letter(&coord, ch);
    }
    let texts = coord.debug_all_candidate_texts();
    assert_eq!(
        texts.len(),
        1,
        "无短语时精确模式空码应补且仅补一条码表后续，实际: {:?}",
        texts
    );
}

/// 短语自动上屏须过 `auto_commit_min_len` 闸（与码表「满码唯一自动上屏」同规格）。
///
/// 回归：`phrase_auto_commit` 原只判「唯一 + 无更长后继」、不设最短码长，致短码短语
/// （如 3 码 `ocd` 的 $CC 命令在 4 码方案里）绕过「满码」语义直接上屏/执行。
///
/// 复用 kkkkx（5 码，五笔 4 码封顶 → 必无更长后继）隔离出 min_len 单一变量：
/// 显式设 6 → 5 < 6 应被拦；设 5 → 恰好达标应放行（边界为 >=）。
fn coord_with_phrase_min_len(min_len: usize, tag: &str) -> std::sync::Arc<Coordinator> {
    let store_path = std::env::temp_dir().join(format!("wind_phrase_minlen_{tag}.redb"));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase("kkkkx", "唯一测试短语", 0, 100).unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.auto_commit_at_full = true;
    cfg.schema.codetable.auto_commit_min_len = min_len;
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store)
}

#[test]
fn phrase_auto_commit_blocked_below_min_len() {
    if !has_schemas() {
        return;
    }
    // min_len=6 > 短语码长 5：即便唯一且无更长后继，也不得自动上屏。
    let coord = coord_with_phrase_min_len(6, "block");
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    let act = coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN));
    assert!(
        !matches!(act, KeyAction::InsertText { .. }),
        "码长 5 < min_len 6 时短语不得自动上屏，实际: {:?}",
        act
    );
    assert!(
        coord
            .debug_all_candidate_texts()
            .contains(&"唯一测试短语".to_string()),
        "未达 min_len 应留在候选里等用户选，实际: {:?}",
        coord.debug_all_candidate_texts()
    );
}

#[test]
fn phrase_auto_commit_at_min_len_boundary() {
    if !has_schemas() {
        return;
    }
    // min_len=5 == 短语码长 5：边界为 >=，应自动上屏（证明上一个测试拦的是 min_len 本身，
    // 而非 kkkkx 这个构造本来就不会自动上屏）。
    let coord = coord_with_phrase_min_len(5, "boundary");
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    match coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "唯一测试短语", "码长恰达 min_len 应自动上屏");
        }
        other => panic!("码长 5 == min_len 5 应自动上屏，实际: {:?}", other),
    }
}

// 码表用户词库值内嵌 $CC 命令（用户真机场景 bccc=$CC(...)）自动上屏测试基建：
// 注入 5 码用户词 kkkkx（五笔 4 码封顶，5 码处必无码表候选 → 唯一 + 无更长后继，
// 与短语侧同构造）。原三重漏判：引擎意向 commit_text=原始 $CC 源 vs 展开后候选
// text=display 标签 → 复核不匹配被否决；recheck 因意向已 Some 不跑；phrase_auto_commit
// 只认 is_phrase。修复=复核按 phrase_template 补匹配 + 首选命令分流(command_auto_outcome)。
fn coord_with_dict_command(template: &str, tag: &str) -> std::sync::Arc<Coordinator> {
    let store_path = std::env::temp_dir().join(format!("wind_dict_autocmd_{tag}.redb"));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store
        .add_user_word("wubi86", "kkkkx", template, 0, 0)
        .unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.auto_commit_at_full = true;
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store)
}

#[test]
fn dict_effect_command_auto_commit_executes() {
    if !has_schemas() {
        return;
    }
    // 含副作用 $CC 命令用户词条：全码唯一自动命中应清组合并异步执行（ClearComposition）。
    let coord = coord_with_dict_command(r#"$CC("《》", ask("x"))"#, "effect");
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    match coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!(
            "含副作用命令词条全码唯一应清组合异步执行(ClearComposition)，实际: {:?}",
            other
        ),
    }
}

#[test]
fn dict_text_command_auto_commit_evaluates() {
    if !has_schemas() {
        return;
    }
    // 纯文本 $CC 命令用户词条：全码唯一自动命中应同步求值上屏其文本（而非 display 标签）。
    let coord = coord_with_dict_command(r#"$CC("标签", type("命令文本"))"#, "text");
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    match coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "命令文本", "纯文本命令词条应自动上屏求值文本");
        }
        other => panic!(
            "纯文本命令词条全码唯一应自动上屏(InsertText)，实际: {:?}",
            other
        ),
    }
}

#[test]
fn special_mode_effect_command_auto_commit_executes() {
    if !has_schemas() {
        return;
    }
    // 快符特殊模式（引用 wubi86 方案）：编码命中唯一含副作用 $CC 词条时，
    // 自动上屏应走命令执行路径（退出模式 + 异步执行 → ClearComposition），
    // 而非因引擎意向(原始 $CC 源)与展开后 display 文本复核不匹配而静默不触发。
    let store_path = std::env::temp_dir().join("wind_special_autocmd_effect.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store
        .add_user_word("wubi86", "kkkkx", r#"$CC("《》", ask("x"))"#, 0, 0)
        .unwrap();
    let mut cfg = config_with("wubi86");
    // ★ 自动上屏写在方案名下：overlay 方案不继承全局 `schema.codetable`。
    let ov = overlay_override_dir_with_codetable(
        "special_mode_effect_command_auto_commit_",
        &[("wubi86", false)],
        "auto_commit_at_full = true\n",
    );
    bind_special(&mut cfg, "backslash", "wubi86");
    let coord =
        Coordinator::new_headless_with_store_override(cfg, Some(&data_dir()), store, Some(ov));
    // 空缓冲按 \ 进入特殊模式
    let act = coord.handle_key_event(&key_event(0xDC, EVENT_KEY_DOWN));
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "\\ 应进入特殊模式，实际: {:?}",
        act
    );
    for ch in ['k', 'k', 'k', 'k'] {
        press_letter(&coord, ch);
    }
    match coord.handle_key_event(&key_event(0x58, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!(
            "特殊模式命中唯一含副作用命令词条应清组合异步执行(ClearComposition)，实际: {:?}",
            other
        ),
    }
    let _ = std::fs::remove_file(&store_path);
}

#[test]
fn special_mode_exact_completion_shows_longer_code() {
    if !has_schemas() {
        return;
    }
    // 需求回归：特殊模式（引用 wubi86）在精确匹配模式 + 空码补全下，输入 `aab` 无精确候选，
    // 但主库有 `aab?` 更长后继 → 引擎备下 completion_hint（备货不 push）。此前特殊模式只消费
    // result.candidates、丢弃 completion_hint → 屏幕全空；修复后应采纳这条更长编码首选，与主码表
    // 方案一致。single_code_input/single_code_complete 配在全局 schema.codetable、方案未覆盖 →
    // tri-state 回落全局（manager.rs resolved），故特殊模式独立引擎也拿到这两个开关。
    // `aab` 复用 project_phrase_candidate_commit §三 的回归码（六库均无精确 aab、主库有 4 条 aab? 后继）。
    let store_path = std::env::temp_dir().join("wind_special_exact_completion.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.single_code_input = true;
    cfg.schema.codetable.single_code_complete = true;
    let ov = overlay_override_dir(
        "special_mode_exact_completion_shows_long",
        &[("wubi86", false)],
    );
    bind_special(&mut cfg, "backslash", "wubi86");
    let coord =
        Coordinator::new_headless_with_store_override(cfg, Some(&data_dir()), store, Some(ov));
    // 空缓冲按 \ 进入特殊模式
    let act = coord.handle_key_event(&key_event(0xDC, EVENT_KEY_DOWN));
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "\\ 应进入特殊模式，实际: {:?}",
        act
    );
    for ch in ['a', 'a', 'b'] {
        press_letter(&coord, ch);
    }
    let texts = coord.debug_all_candidate_texts();
    assert!(
        !texts.is_empty(),
        "精确匹配+空码补全下，特殊模式 aab 无精确候选时应补一条更长编码候选（completion_hint），实际候选: {:?}",
        texts
    );
    let _ = std::fs::remove_file(&store_path);
}

#[test]
fn special_mode_show_all_on_enter_lists_candidates() {
    if !has_schemas() {
        return;
    }
    // 需求：show_all_on_enter 开启时，进入模式（空编码、尚未敲码）即枚举方案码表首页候选；
    // 关闭时（默认）进入模式候选为空、敲码才出。用同一份配置的开/关两态对照。
    let make = |show_all: bool| {
        let store_path =
            std::env::temp_dir().join(format!("wind_special_showall_{}.redb", show_all));
        let _ = std::fs::remove_file(&store_path);
        let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
        let mut cfg = config_with("wubi86");
        let ov = overlay_override_dir(
            "special_mode_show_all_on_enter_lists_can",
            &[("wubi86", show_all)],
        );
        bind_special(&mut cfg, "backslash", "wubi86");
        let coord =
            Coordinator::new_headless_with_store_override(cfg, Some(&data_dir()), store, Some(ov));
        // 空缓冲按 \ 进入特殊模式（尚未敲任何编码）
        let act = coord.handle_key_event(&key_event(0xDC, EVENT_KEY_DOWN));
        assert!(
            matches!(act, KeyAction::UpdateComposition { .. }),
            "\\ 应进入特殊模式，实际: {:?}",
            act
        );
        let texts = coord.debug_all_candidate_texts();
        let _ = std::fs::remove_file(&store_path);
        texts
    };
    assert!(
        !make(true).is_empty(),
        "show_all_on_enter 开启时，进入模式（空编码）应立即枚举出码表候选"
    );
    assert!(
        make(false).is_empty(),
        "show_all_on_enter 关闭（默认）时，进入模式（空编码）候选应为空"
    );
}

#[test]
fn special_mode_show_all_respects_single_code_input() {
    if !has_schemas() {
        return;
    }
    // show_all_on_enter 遵循方案 single_code_input：精确匹配模式下进入即展示最多补 1 条
    // （与空码补全「取首位后续码」同语义）；非精确模式枚举整页（多条）。
    let make = |single_code: bool| {
        let store_path =
            std::env::temp_dir().join(format!("wind_special_showall_single_{}.redb", single_code));
        let _ = std::fs::remove_file(&store_path);
        let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
        let mut cfg = config_with("wubi86");
        // ★ 写在**方案自己名下**，不是全局：overlay 方案不继承全局 `schema.codetable`。
        let ov = overlay_override_dir_with_codetable(
            "special_mode_show_all_respects_single_co",
            &[("wubi86", true)],
            &format!("single_code_input = {single_code}\n"),
        );
        bind_special(&mut cfg, "backslash", "wubi86");
        let coord =
            Coordinator::new_headless_with_store_override(cfg, Some(&data_dir()), store, Some(ov));
        coord.handle_key_event(&key_event(0xDC, EVENT_KEY_DOWN));
        let texts = coord.debug_all_candidate_texts();
        let _ = std::fs::remove_file(&store_path);
        texts
    };
    assert_eq!(
        make(true).len(),
        1,
        "精确匹配模式下 show_all_on_enter 应最多补 1 条"
    );
    assert!(
        make(false).len() > 1,
        "非精确模式下 show_all_on_enter 应枚举整页（多条）"
    );
}

#[test]
fn clear_on_empty_max_keeps_phrase_candidate() {
    if !has_schemas() {
        return;
    }
    // 回归：满码空码清空（clear_on_empty_max）开启 + 短语专属码（码表无字，如 zzbd）时，
    // should_clear 由码表引擎在**追加短语之前**算出 true（仅看码表空候选），但协调器随后追加了
    // 精确码短语候选 → 不应清空缓冲。原 bug：`None if should_clear => Clear` 未复查叠加短语后的
    // 最终候选，把短语列表连同缓冲一并误清（handle_candidate.rs）。
    // 复用 kkkkx（五笔码表 4 码封顶，5 码处必无码表候选 → is_empty 且满码 → should_clear 成立）。
    let store_path = std::env::temp_dir().join("wind_phrase_clear_empty.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase("kkkkx", "空码短语文本", 0, 100).unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.clear_on_empty_max = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['k', 'k', 'k', 'k', 'x'] {
        press_letter(&coord, ch);
    }
    let texts = coord.debug_all_candidate_texts();
    assert!(
        texts.iter().any(|t| t == "空码短语文本"),
        "满码空码清空开启时，短语专属码候选不应被清空，实际候选: {:?}",
        texts
    );
    let _ = std::fs::remove_file(&store_path);
}

#[test]
fn top_code_plain_phrase_direct_commit_defers() {
    if !has_schemas() {
        return;
    }
    // 普通短语首选 + direct_commit：走成熟 CommitThenDeferComposition 路径，commit_text=短语文本。
    let coord = coord_with_skce_phrase(
        "顶码短语文本",
        wind_config::TopCommitMode::DirectCommit,
        "direct",
    );
    match drive_top_code(&coord) {
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            timeout_ms,
        } => {
            assert_eq!(
                commit_text, "顶码短语文本",
                "direct_commit 顶码应真提交短语文本"
            );
            assert!(!deferred_composition.is_empty(), "应有余码 y 新组合");
            assert_eq!(timeout_ms, 150);
        }
        other => panic!(
            "普通短语 direct_commit 顶码应返回 CommitThenDeferComposition，实际: {:?}",
            other
        ),
    }
}

/// 配对跳出键：中文配对开 + 配置 Tab 为跳出键。
/// 输入左括号插入配对后，按 Tab 应等效输入右符号跳出（MoveCursorRight）；
/// 栈空后再按 Tab 应透传给宿主（不吞正常按键）。
#[test]
fn auto_pair_jump_out_key_moves_cursor_right() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true; // 中文配对开（默认 chinese_punct=true → 用 cn_pairs）
    cfg.input.auto_pair.jump_out_keys = vec!["tab".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // 左括号（Shift+9 → '（'）：插入配对，光标置于中间
    let ins = coord.handle_key_event(&key_event_mods(0x39, EVENT_KEY_DOWN, 0x0001));
    match ins {
        KeyAction::InsertTextWithCursor {
            text,
            cursor_offset,
        } => {
            assert_eq!(text, "（）", "应插入中文配对");
            assert_eq!(cursor_offset, 1, "光标应落在配对中间");
        }
        other => panic!("左括号应插入配对，实际: {:?}", other),
    }

    // 按 Tab：配对栈非空 → 跳出（光标右移）
    let jump = coord.handle_key_event(&key_event(0x09, EVENT_KEY_DOWN));
    assert!(
        matches!(jump, KeyAction::MoveCursorRight { .. }),
        "Tab 应跳出配对（MoveCursorRight），实际: {:?}",
        jump
    );

    // 再按 Tab：栈已空 → 不拦截，透传给宿主
    let passthrough = coord.handle_key_event(&key_event(0x09, EVENT_KEY_DOWN));
    assert!(
        matches!(passthrough, KeyAction::PassThrough),
        "栈空时 Tab 应透传，实际: {:?}",
        passthrough
    );
}

/// 中英文切换**不清**配对栈：切走再切回后 Tab 仍能跳出。
/// 切模式既不移动光标也不消除已插入的右符号，「光标紧贴右符号」的前提仍成立。
/// （对照组见 `auto_pair_focus_lost_clears_stack`：失焦才该清。）
#[test]
fn auto_pair_stack_survives_mode_switch() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true;
    cfg.input.auto_pair.jump_out_keys = vec!["tab".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // 前置：左括号插入配对，栈里确有一层（不断言就无从区分「保住了」与「压根没进栈」）。
    let ins = coord.handle_key_event(&key_event_mods(0x39, EVENT_KEY_DOWN, 0x0001));
    assert!(
        matches!(ins, KeyAction::InsertTextWithCursor { .. }),
        "前置：左括号应插入配对，实际: {ins:?}"
    );

    // 左 Shift 释放切英文 → 再切回中文。两次都断言模式确实翻转，否则本测试会退化成
    // 「压根没切过模式」的假绿。
    coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    assert!(!coord.is_chinese_mode(), "前置：应已切到英文");
    coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    assert!(coord.is_chinese_mode(), "前置：应已切回中文");

    // 核心断言：配对栈跨模式切换存活 → Tab 仍跳出。
    let jump = coord.handle_key_event(&key_event(0x09, EVENT_KEY_DOWN));
    assert!(
        matches!(jump, KeyAction::MoveCursorRight { .. }),
        "中英切换不应清配对栈，切回后 Tab 应仍能跳出，实际: {jump:?}"
    );
}

/// 跨模式跳出（本次改造的核心目标）：中文里打的配对，切到英文后 Tab 应能跳出。
///
/// 旧实现下这条必失败——协调器的跳出判定写在中文 composition 路径里，而英文模式在更早处
/// 就 `PassThrough` 了，那段判定是死代码。
#[test]
fn auto_pair_jump_out_works_in_english_mode() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true;
    cfg.input.auto_pair.jump_out_keys = vec!["tab".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let ins = coord.handle_key_event(&key_event_mods(0x39, EVENT_KEY_DOWN, 0x0001));
    assert!(
        matches!(ins, KeyAction::InsertTextWithCursor { .. }),
        "前置：中文模式下左括号应插入配对，实际: {ins:?}"
    );
    coord.handle_key_event(&key_event(0xA0, EVENT_KEY_UP));
    assert!(!coord.is_chinese_mode(), "前置：应已切到英文");

    let jump = coord.handle_key_event(&key_event(0x09, EVENT_KEY_DOWN));
    assert!(
        matches!(jump, KeyAction::MoveCursorRight { .. }),
        "英文模式应能跳出中文模式建立的配对，实际: {jump:?}"
    );
}

/// 英文半角普通配对键由协调器接手（此前由 DLL 的 `_englishPairEngine` 本地插入）。
/// 这是「四条建立路径全部入同一个栈」的关键一步。
#[test]
fn english_halfwidth_pair_handled_by_coordinator() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.default.chinese_mode = false; // 英文模式
    cfg.input.auto_pair.english = true;
    cfg.input.auto_pair.jump_out_keys = vec!["tab".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    assert!(!coord.is_chinese_mode(), "前置：应处于英文模式");

    // Shift+9 → `(`：协调器出字并补右括号
    let ins = coord.handle_key_event(&key_event_mods(0x39, EVENT_KEY_DOWN, 0x0001));
    match ins {
        KeyAction::InsertTextWithCursor {
            ref text,
            cursor_offset,
        } => {
            assert_eq!(text, "()", "英文半角应插入 ASCII 配对");
            assert_eq!(cursor_offset, 1, "光标应落在配对中间");
        }
        other => panic!("英文半角左括号应由协调器插入配对，实际: {other:?}"),
    }

    let jump = coord.handle_key_event(&key_event(0x09, EVENT_KEY_DOWN));
    assert!(
        matches!(jump, KeyAction::MoveCursorRight { .. }),
        "英文模式应能跳出自己建立的配对，实际: {jump:?}"
    );
}

/// 吃键面未扩大（硬性约束的回归保护）：配对开关关闭时，协调器不得接手配对键。
/// 接手即意味着 DLL 也吃了它，而 DLL 的判据是 `IsEnabled() && 在配对表内`——
/// 两侧一旦不同源就是「吃了再吐」丢键。
#[test]
fn english_pair_not_handled_when_disabled() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.default.chinese_mode = false;
    cfg.input.auto_pair.english = false; // 配对关
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let act = coord.handle_key_event(&key_event_mods(0x39, EVENT_KEY_DOWN, 0x0001));
    assert!(
        matches!(act, KeyAction::PassThrough),
        "配对关闭时英文括号必须透传（吃键面不得扩大），实际: {act:?}"
    );
}

/// 失焦后配对状态的存废。**跨焦点保留已放弃**（2026-07-29 真机后决定）：
/// 配对状态在 core 全局单栈与每个宿主进程各自一份的 DLL 计数两处，作用域模型对不齐，
/// 加上焦点离开期间用户做了什么输入法无法感知，保留本质上是猜测——实测大部分情况失效。
/// 故凡是会清输入缓冲的 reason，一律连配对状态一起清；`CtxLost` 是 DocMgr 噪声层，
/// 它本来就不清任何输入态，配对状态也跟着不清。
fn pair_state_after_focus_lost(reason: wind_bridge::handler::FocusLostReason) -> KeyAction {
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true;
    cfg.input.auto_pair.jump_out_keys = vec!["tab".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let ins = coord.handle_key_event(&key_event_mods(0x39, EVENT_KEY_DOWN, 0x0001));
    assert!(
        matches!(ins, KeyAction::InsertTextWithCursor { .. }),
        "前置：左括号应插入配对，实际: {ins:?}"
    );
    coord.handle_focus_lost(0, reason);
    coord.handle_key_event(&key_event(0x09, EVENT_KEY_DOWN))
}

#[test]
fn auto_pair_cleared_on_real_focus_loss() {
    if !has_schemas() {
        return;
    }
    use wind_bridge::handler::FocusLostReason;
    for reason in [
        FocusLostReason::Thread,
        FocusLostReason::DocChanged,
        FocusLostReason::NoEditCtx,
    ] {
        let act = pair_state_after_focus_lost(reason);
        assert!(
            matches!(act, KeyAction::PassThrough),
            "{reason:?} 属真实失焦，配对状态须清空、Tab 应透传，实际: {act:?}"
        );
    }
}

/// `CtxLost` 是 DocMgr 噪声层（Excel 实测同一 DocMgr 6ms 内掉了又回），它**不清任何输入态**，
/// 配对状态也跟着不清——在这里清就是把 Excel 那类抖动变成「配对忽然跳不出去」。
#[test]
fn auto_pair_survives_ctx_lost_noise() {
    if !has_schemas() {
        return;
    }
    let act = pair_state_after_focus_lost(wind_bridge::handler::FocusLostReason::CtxLost);
    assert!(
        matches!(act, KeyAction::MoveCursorRight { .. }),
        "CtxLost 是噪声层，不该清配对状态，实际: {act:?}"
    );
}

/// 配对跳出键未配置时：Tab 不被吞（回归保护——默认空集不启用）。
#[test]
fn auto_pair_no_jump_out_key_passes_tab_through() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true;
    // jump_out_keys 默认只含 right_symbol → Tab 不在其中，不该被吞
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // 插入配对
    let ins = coord.handle_key_event(&key_event_mods(0x39, EVENT_KEY_DOWN, 0x0001));
    assert!(
        matches!(ins, KeyAction::InsertTextWithCursor { .. }),
        "左括号应插入配对，实际: {:?}",
        ins
    );

    // 未配置跳出键：Tab 即使栈非空也不跳出，透传
    let tab = coord.handle_key_event(&key_event(0x09, EVENT_KEY_DOWN));
    assert!(
        matches!(tab, KeyAction::PassThrough),
        "未配置跳出键时 Tab 应透传，实际: {:?}",
        tab
    );
}

/// `right_symbol` 在跳出列表内：打右括号 → 光标越过已配对的右符号（不重复插入）。
#[test]
fn jump_out_right_symbol_enabled_moves_cursor() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true;
    cfg.input.auto_pair.jump_out_keys = vec!["right_symbol".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // Shift+9 → `（`：插入配对
    let ins = coord.handle_key_event(&key_event_mods(0x39, EVENT_KEY_DOWN, 0x0001));
    assert!(
        matches!(ins, KeyAction::InsertTextWithCursor { .. }),
        "左括号应插入配对，实际: {ins:?}"
    );
    // Shift+0 → `）`：栈顶正是它 → 跳出
    let jump = coord.handle_key_event(&key_event_mods(0x30, EVENT_KEY_DOWN, 0x0001));
    assert!(
        matches!(jump, KeyAction::MoveCursorRight { .. }),
        "启用 right_symbol 时右括号应跳出，实际: {jump:?}"
    );
}

/// `right_symbol` 不在跳出列表内：打右括号 → **正常上屏该字符，不跳出**。
/// 回归保护：列表里没有就是没有，不做隐式补偿（用户拍板的语义）。
#[test]
fn jump_out_right_symbol_disabled_commits_char() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true;
    cfg.input.auto_pair.jump_out_keys = vec!["tab".into()]; // 只留 Tab，不含 right_symbol
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let ins = coord.handle_key_event(&key_event_mods(0x39, EVENT_KEY_DOWN, 0x0001));
    assert!(
        matches!(ins, KeyAction::InsertTextWithCursor { .. }),
        "左括号应插入配对，实际: {ins:?}"
    );
    let act = coord.handle_key_event(&key_event_mods(0x30, EVENT_KEY_DOWN, 0x0001));
    assert!(
        !matches!(act, KeyAction::MoveCursorRight { .. }),
        "未启用 right_symbol 时右括号不该跳出，实际: {act:?}"
    );
    assert!(
        format!("{act:?}").contains('）'),
        "应正常上屏右括号，实际: {act:?}"
    );
    // Tab 仍可跳出（栈未被右符号消费）
    let tab = coord.handle_key_event(&key_event(0x09, EVENT_KEY_DOWN));
    assert!(
        matches!(tab, KeyAction::MoveCursorRight { .. }),
        "Tab 应仍能跳出，实际: {tab:?}"
    );
}

/// 引号配对回归：**连按引号键每次都开新的一对**，绝不交替。
///
/// 历史 bug：引号是唯一的对称配对键，`PunctuationConverter` 用交替开关决定出左还是出右
/// （第 1 次 `“`、第 2 次 `”`），而自动配对**一次按键就把左右都吐出去了**、开关却只前进
/// 一格 → 第 2 次按键给出 `”` → 不是左符号（不插对）、却是右符号（跳出或裸提交单个 `”`）
/// → 「出对 / 出单」严格交替循环。修法是配对生效时把交替态钉死在「左」，
/// 左右判定单一收口到配对栈，跳出交给 `jump_out_keys`。
#[test]
fn auto_pair_quote_always_opens_new_pair() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true;
    cfg.input.auto_pair.chinese_pairs.push("“”".into());
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // Shift+VK_OEM_7(0xDE) = `"` → 中文双引号
    for round in 1..=3 {
        let act = coord.handle_key_event(&key_event_mods(0xDE, EVENT_KEY_DOWN, 0x0001));
        match act {
            KeyAction::InsertTextWithCursor {
                text,
                cursor_offset,
            } => {
                assert_eq!(text, "“”", "第 {round} 次按引号应插入完整一对");
                assert_eq!(cursor_offset, 1, "第 {round} 次光标应落在配对中间");
            }
            other => {
                panic!("第 {round} 次按引号应插入配对（不得跳出/裸出单引号），实际: {other:?}")
            }
        }
    }
}

/// 引号不在配对表内时，保持原生「第一次左、第二次右」交替（不被上面的钉左误伤）。
#[test]
fn quote_alternates_when_not_in_pair_table() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true; // 配对开，但配对表**不含**引号
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let first = coord.handle_key_event(&key_event_mods(0xDE, EVENT_KEY_DOWN, 0x0001));
    let second = coord.handle_key_event(&key_event_mods(0xDE, EVENT_KEY_DOWN, 0x0001));
    assert!(
        format!("{first:?}").contains('“'),
        "首次应出左引号，实际: {first:?}"
    );
    assert!(
        format!("{second:?}").contains('”'),
        "第二次应出右引号（原生交替），实际: {second:?}"
    );
}

/// 英文模式（半角）下「英文半角」列生效：DLL 按 core 推送的字符集合吃下这些标点键转发，
/// 此处必须出字。
///
/// 历史：英文非全角时 DLL 直接透传标点键（真机日志 `decision=passthrough_not_handled`），
/// 引擎收不到 → 四列里的「英半」是打不到的死格（英全列有 `english_fullwidth` 分支才生效）。
#[test]
fn english_mode_uses_english_half_width_column() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.default.chinese_mode = false; // 英文输入模式
    cfg.input.punct.custom_enabled = true;
    cfg.input.punct.custom_mappings.insert(
        "\"1".into(),
        vec!["E".into(), "＂".into(), "R".into(), "#".into()],
    );
    cfg.input.punct.custom_mappings.insert(
        "\"2".into(),
        vec!["￥".into(), "＂".into(), "%".into(), "$".into()],
    );
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    assert!(!coord.is_chinese_mode(), "前置：应处于英文模式");

    // Shift+VK_OEM_7 两次 → 英半列的左形 / 右形（`#` → `$`）
    let first = coord.handle_key_event(&key_event_mods(0xDE, EVENT_KEY_DOWN, 0x0001));
    let second = coord.handle_key_event(&key_event_mods(0xDE, EVENT_KEY_DOWN, 0x0001));
    assert_eq!(
        action_text(&first).as_deref(),
        Some("#"),
        "英文模式首次应出英半列的左形，实际: {first:?}"
    );
    assert_eq!(
        action_text(&second).as_deref(),
        Some("$"),
        "英文模式第二次应出英半列的右形，实际: {second:?}"
    );
}

/// 吃键集 ⊆ 出字集的**反向**保证：没配英半列的标点键在英文模式下仍须透传。
///
/// DLL 只吃 core 推送的字符集合内的键，core 也只接手同一集合——两侧同源。若此处误接手
/// （返回 Consumed 之类）就会吞掉 DLL 根本没吃的键；反之若 DLL 吃了而这里不出字，
/// 就是「吃了再吐」，Chrome/Electron 不回退合成 WM_CHAR，键直接丢失。
#[test]
fn english_mode_uncovered_punct_still_passes_through() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.default.chinese_mode = false;
    cfg.input.punct.custom_enabled = true;
    // 只给双引号配英半列，逗号不配
    cfg.input.punct.custom_mappings.insert(
        "\"1".into(),
        vec!["".into(), "".into(), "".into(), "#".into()],
    );
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let comma = coord.handle_key_event(&key_event(0xBC, EVENT_KEY_DOWN)); // VK_OEM_COMMA
    assert!(
        matches!(comma, KeyAction::PassThrough),
        "未配英半列的标点键在英文模式下必须透传（DLL 也没吃它），实际: {comma:?}"
    );
    // 单引号（同一物理键、无 Shift）也没配 → 同样透传
    let quote = coord.handle_key_event(&key_event(0xDE, EVENT_KEY_DOWN));
    assert!(
        matches!(quote, KeyAction::PassThrough),
        "同键无 Shift 的 `'` 未配英半列，应透传，实际: {quote:?}"
    );
}

/// 自定义映射 × 引号配对：`"1`/`"2` 两行 = **左形/右形**，配对时一次按键两行都用上。
///
/// 语义定名（用户拍板）：界面上的「第一次 / 第二次」实质是左形 / 右形，「第几次」只是没有
/// 自动配对时按次序推导角色的说法。此前配对判定用硬编码的内置 `“”`，而上屏走自定义映射：
/// 把引号自定义成 `「」` 后判定不命中 → 不钉左 → 交替态照旧前进 → 第 2 次按键出 `」`（右符号）
/// → 「出对 / 出单」交替循环复发；反过来若判定命中却钉左，`"2` 那行就永远取不到。
#[test]
fn custom_quote_mapping_pairs_by_left_right_rows() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = true; // 默认中文配对表已含「」
    cfg.input.punct.custom_enabled = true;
    cfg.input
        .punct
        .custom_mappings
        .insert("\"1".into(), vec!["「".into()]);
    cfg.input
        .punct
        .custom_mappings
        .insert("\"2".into(), vec!["」".into()]);
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // 连按两次：每次都插入由「左形 + 右形」组成的完整一对，第二次不退化成裸右符号。
    for round in 1..=2 {
        let act = coord.handle_key_event(&key_event_mods(0xDE, EVENT_KEY_DOWN, 0x0001));
        match act {
            KeyAction::InsertTextWithCursor {
                text,
                cursor_offset,
            } => {
                assert_eq!(
                    text, "「」",
                    "第 {round} 次按引号应插入自定义左右形组成的一对"
                );
                assert_eq!(cursor_offset, 1, "第 {round} 次光标应落在配对中间");
            }
            other => panic!("第 {round} 次按引号应插入自定义配对，实际: {other:?}"),
        }
    }
}

/// 自定义映射 + 引号**不**参与配对时，两行仍按「第一次左 / 第二次右」交替取用。
#[test]
fn custom_quote_mapping_alternates_without_pairing() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.auto_pair.chinese = false; // 配对关 → 回到按次序取行
    cfg.input.punct.custom_enabled = true;
    cfg.input
        .punct
        .custom_mappings
        .insert("\"1".into(), vec!["@".into()]);
    cfg.input
        .punct
        .custom_mappings
        .insert("\"2".into(), vec!["￥".into()]);
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let first = coord.handle_key_event(&key_event_mods(0xDE, EVENT_KEY_DOWN, 0x0001));
    let second = coord.handle_key_event(&key_event_mods(0xDE, EVENT_KEY_DOWN, 0x0001));
    assert_eq!(action_text(&first).as_deref(), Some("@"), "首次应取 \"1 行");
    assert_eq!(
        action_text(&second).as_deref(),
        Some("￥"),
        "第二次应取 \"2 行"
    );
}

fn action_caret(action: &KeyAction) -> Option<u32> {
    match action {
        KeyAction::UpdateComposition { caret_pos, .. } => Some(*caret_pos),
        _ => None,
    }
}

// ---- 编码区光标（对齐 Go engine_default_cursor_move / engine_default_delete golden）----

const VK_LEFT: u32 = 0x25;
const VK_RIGHT: u32 = 0x27;
const VK_HOME: u32 = 0x24;
const VK_END: u32 = 0x23;
const VK_DELETE: u32 = 0x2E;
const VK_BACK: u32 = 0x08;

/// 无修饰键按下（复用文件上方的 `press_vk(coord, vk, shift)`）。
fn tap(coord: &Coordinator, vk: u32) -> KeyAction {
    press_vk(coord, vk, false)
}

fn type_str(coord: &Coordinator, s: &str) -> KeyAction {
    let mut last = KeyAction::PassThrough;
    for c in s.chars() {
        last = press_letter(coord, c);
    }
    last
}

/// 按**文本**选中候选：翻页找到它，再按该页对应的数字键。
///
/// ⚠️ **不要在分步上屏类测试里写死候选位置**。这族测试要守的是「选中一个只消费部分
/// 输入的候选之后，退格/光标/只读前缀的行为」，候选排在第几位是它**不关心**的东西。
/// 旧写法 `tap(&coord, 0x33)`（写死数字键 3 = 第 3 个候选）在排序按「消费输入长度优先」
/// 调整后全部失效 —— 单字候选被整体推到消费整串的词之后（`nihao` 下「你」到了第 35 位），
/// 而机制本身完好。依赖了不关心的东西，就会为不相干的改动买单。
fn select_by_text(coord: &Coordinator, text: &str) -> KeyAction {
    let (_, _, total_pages) = coord.debug_page_info();
    for _ in 0..total_pages.max(1) {
        if let Some(i) = coord.debug_page_texts().iter().position(|t| t == text) {
            return tap(coord, 0x31 + i as u32); // '1'..'9'
        }
        tap(coord, 0x22); // PageDown
    }
    panic!(
        "候选中找不到「{text}」（共 {total_pages} 页）：{:?}",
        coord.debug_all_candidate_texts()
    );
}

/// 光标左移跨过引擎插入的音节分隔符时，caret 需按**显示串**位置换算（buffer "nihao" 的第 2
/// 字节 → 显示 "ni'hao" 的第 2 位，一次左移跨两个显示位）。这是 buffer→display 映射的核心用例。
#[test]
fn test_pinyin_cursor_maps_through_separator() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    let last = type_str(&coord, "nihao");
    assert_eq!(action_text(&last).as_deref(), Some("ni'hao"));
    assert_eq!(action_caret(&last), Some(6), "初始光标在末尾");

    // ni'ha|o → ni'h|ao：缓冲内左移一字符，显示位同步左移一位
    assert_eq!(action_caret(&tap(&coord, VK_LEFT)), Some(5));
    assert_eq!(action_caret(&tap(&coord, VK_LEFT)), Some(4));
    // ni'h|ao → ni|'hao：缓冲从 "nih|ao" 退到 "ni|hao"，显示上跨过分隔符 '（4 → 2）
    assert_eq!(action_caret(&tap(&coord, VK_LEFT)), Some(2));

    // Home / End 到两端
    assert_eq!(action_caret(&tap(&coord, VK_HOME)), Some(0));
    assert_eq!(action_caret(&tap(&coord, VK_END)), Some(6));
    // 右移到边界后再右移：无位可动 → 吃掉，不透传给宿主
    assert!(matches!(tap(&coord, VK_RIGHT), KeyAction::Consumed));
}

/// 光标移动不改变组合区文本，也不重算候选（光标不参与引擎查询）。
#[test]
fn test_cursor_move_keeps_text_and_candidates() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    let before = action_text(&type_str(&coord, "nihao")).unwrap();
    let moved = tap(&coord, VK_LEFT);
    assert_eq!(
        action_text(&moved).as_deref(),
        Some(before.as_str()),
        "左移只改 caret，组合区文本不变"
    );
    // 移回末尾后空格上屏，候选与移动前一致（未因光标移动而重算）
    tap(&coord, VK_END);
    match coord.handle_key_event(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert!(!text.is_empty()),
        other => panic!("空格应上屏，实际: {:?}", other),
    }
}

/// 光标在中间时字母插到光标处（而非追加末尾），候选按新的完整缓冲重算。
#[test]
fn test_insert_at_cursor_position() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    assert_eq!(action_text(&type_str(&coord, "aa")).as_deref(), Some("aa"));
    assert_eq!(action_caret(&tap(&coord, VK_LEFT)), Some(1)); // a|a
    let act = press_letter(&coord, 'b'); // a|a + b → ab|a
    assert_eq!(action_text(&act).as_deref(), Some("aba"), "应插在光标处");
    assert_eq!(action_caret(&act), Some(2), "插入后光标随之后移");
}

/// Delete 删光标后一字符且光标不动；Backspace 删光标前一字符。
#[test]
fn test_delete_and_backspace_at_cursor() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    type_str(&coord, "abc");
    tap(&coord, VK_HOME); // |abc
    let act = tap(&coord, VK_DELETE); // 删 'a' → |bc
    assert_eq!(action_text(&act).as_deref(), Some("bc"));
    assert_eq!(action_caret(&act), Some(0), "Delete 后光标不动");

    tap(&coord, VK_END); // bc|
    let act = tap(&coord, VK_BACK); // 删 'c' → b|
    assert_eq!(action_text(&act).as_deref(), Some("b"));
    assert_eq!(action_caret(&act), Some(1));
}

/// 边界三态：无组合 → 透传宿主；有组合但已在边界 → 吃掉（含光标在最左时的 Backspace，
/// 若透传会让宿主删掉组合区之前的正文）。
#[test]
fn test_cursor_boundary_semantics() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    // 无组合：方向键/Delete 透传给宿主，宿主照常移动文档光标
    assert!(matches!(tap(&coord, VK_LEFT), KeyAction::PassThrough));
    assert!(matches!(tap(&coord, VK_RIGHT), KeyAction::PassThrough));
    assert!(matches!(tap(&coord, VK_HOME), KeyAction::PassThrough));
    assert!(matches!(tap(&coord, VK_DELETE), KeyAction::PassThrough));

    type_str(&coord, "aa");
    tap(&coord, VK_HOME); // |aa
    assert!(
        matches!(tap(&coord, VK_LEFT), KeyAction::Consumed),
        "已在最左：吃掉不透传"
    );
    assert!(
        matches!(tap(&coord, VK_BACK), KeyAction::Consumed),
        "光标在最左时 Backspace 吃掉，不得透传给宿主"
    );
    tap(&coord, VK_END); // aa|
    assert!(
        matches!(tap(&coord, VK_DELETE), KeyAction::Consumed),
        "光标在末尾：前删无物，吃掉"
    );
}

/// 已转换前缀是**只读**的：光标进不去（Home 只到剩余编码开头），caret 需含前缀的 UTF-16 长度。
/// Delete 把剩余编码删空时回退最后一段（对齐 Go handleDelete → popConfirmedSegment）。
#[test]
fn test_committed_prefix_is_readonly_and_delete_pops_segment() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    type_str(&coord, "nihao");
    // 分步确认「你」（只消费 "ni"），剩余编码 "hao" 留在组合区
    let act = select_by_text(&coord, "你");
    assert_eq!(action_text(&act).as_deref(), Some("你hao"));
    assert_eq!(
        action_caret(&act),
        Some(4),
        "caret = 前缀「你」1 个 UTF-16 单元 + 剩余 \"hao\" 3 个"
    );

    // Home 只到剩余编码开头（caret=1，即「你」之后），不进只读前缀
    assert_eq!(action_caret(&tap(&coord, VK_HOME)), Some(1));
    assert!(
        matches!(tap(&coord, VK_LEFT), KeyAction::Consumed),
        "已在剩余编码最左：吃掉，不得退进已转换前缀"
    );

    // Delete 三次删空 "hao" → 回退段「你」，其码 "ni" 并回缓冲
    tap(&coord, VK_DELETE);
    tap(&coord, VK_DELETE);
    let act = tap(&coord, VK_DELETE);
    assert_eq!(
        action_text(&act).as_deref(),
        Some("ni"),
        "删空剩余编码应回退已转换段，而非留下空组合区"
    );
    assert_eq!(action_caret(&act), Some(2), "回退后光标落在码末尾");
}

/// Backspace 的段回退**优先于光标**：即便光标在剩余编码最左（Backspace 本该无字符可删），
/// 有已转换段时仍先回退段（Go handleBackspace 的分支顺序）。
#[test]
fn test_backspace_pops_segment_regardless_of_cursor() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    type_str(&coord, "nihao");
    select_by_text(&coord, "你"); // 「你」+ "hao"
    tap(&coord, VK_HOME); // 光标到剩余编码最左
    let act = tap(&coord, VK_BACK);
    assert_eq!(
        action_text(&act).as_deref(),
        Some("ni'hao"),
        "段回退优先：码 \"ni\" 并回缓冲前部，与 \"hao\" 合成 \"nihao\""
    );
    assert_eq!(action_caret(&act), Some(6), "回退后光标拉到缓冲末尾");
}

/// 回归：**双拼**分步上屏后退格，回退的必须是原始击键码而非全拼码。
///
/// 引擎只把 `consumed_length` 回映射到双拼击键空间，候选的 `code` 刻意保持全拼语义。
/// 曾因 `committed_segs` 只记全拼码，退格把 `hao` 并回击键缓冲 `ma` → `haoma` 被当双拼
/// 重解析成 `ha|o|ma`，preedit 变 `ha'oma`，此后整串错乱。
///
/// 用 `hcma`（小鹤：hao=hc、ma=ma）而非 `nihc`：**必须选一个双拼码 ≠ 全拼码的首音节**，
/// 否则 bug 隐身——`ni` 两种码恰好相同，正是它让这个缺陷表现为「有时正常」。
/// 末尾的 `nihc` 是对照组，锁住等长场景不被改动波及。
#[test]
fn test_shuangpin_backspace_restores_raw_keys() {
    let d = data_dir();
    if !d.join("schemas/shuangpin.schema.toml").exists() {
        return;
    }
    let sp_cfg = || {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["shuangpin".into()];
        cfg.schema.active = "shuangpin".into();
        cfg.input.default.chinese_mode = true;
        cfg
    };

    // hcma → 「好吗」。选单字候选「好」（分步上屏，消费 hc 两键），再退格。
    let coord = Coordinator::new_headless(sp_cfg(), Some(&d));
    type_str(&coord, "hcma");
    let act = select_by_text(&coord, "好");
    assert_eq!(
        action_text(&act).as_deref(),
        Some("好ma"),
        "分步上屏：「好」入前缀，剩余击键 ma"
    );
    let act = tap(&coord, VK_BACK);
    assert_eq!(
        action_text(&act).as_deref(),
        Some("hc'ma"),
        "退格须还原**击键** hc（全拼 hao 会被重解析成 ha|o → \"ha'oma\"）"
    );

    // 对照：ni 的双拼码与全拼码相同，行为不得改变。
    let coord = Coordinator::new_headless(sp_cfg(), Some(&d));
    type_str(&coord, "nihc");
    select_by_text(&coord, "你");
    assert_eq!(
        action_text(&tap(&coord, VK_BACK)).as_deref(),
        Some("ni'hc"),
        "等长场景行为不变"
    );
}

// ---- 双拼：非字母韵母键（微软/搜狗/紫光的 `;` = ing）----

/// 把内置 shuangpin 方案的布局换成指定 id（override 层，不动真实方案文件）。
fn shuangpin_layout_override(tag: &str, layout: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_sp_layout_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("shuangpin.toml"),
        format!("[engine.pinyin.shuangpin]\nlayout = \"{layout}\"\n"),
    )
    .unwrap();
    dir
}

fn shuangpin_cfg() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".into()];
    cfg.schema.active = "shuangpin".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

/// 组码中的 `;` 必须作 **ing 韵母**进缓冲，而不是被次选键 / quick_mix 引导键 / 标点吃掉。
///
/// 这三条通路会**依次**拦这个键，缺一条没接就仍旧打不出——曾经的
/// `is_shuangpin_final` 只跳过了选词分支，`y;` 的实际结果是「也」上屏 + 进快捷输入。
/// 现由码元字符集在 `try_code_char_gate` 一处仲裁（拼音引擎的 `input_chars` 从布局推导）。
#[test]
fn test_shuangpin_symbol_final_enters_buffer() {
    let d = data_dir();
    if !d.join("schemas/shuangpin.schema.toml").exists() {
        eprintln!("跳过：缺 shuangpin.schema.toml");
        return;
    }
    let ov = shuangpin_layout_override("mspy_final", "mspy");
    let coord =
        Coordinator::new_headless_with_override(shuangpin_cfg(), Some(&d), Some(ov.clone()));

    press_letter(&coord, 'y');
    let act = press_char(&coord, ';');
    // 组合区显示**原始击键**（双拼刻意如此，见 pinyin/mod.rs 的 Fix A：`hcma` 显示
    // `hc'ma` 而非 `hao'ma`），故这里是 `y;` 不是 `ying`；`;` 真的进了缓冲的证据
    // 是候选——转换成 ying 才查得到这些字。
    assert_eq!(
        action_text(&act).as_deref(),
        Some("y;"),
        "`;` 应作韵母进缓冲（组合区显示击键），而不是顶字上屏"
    );
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "不得被 quick_mix 引导键截走"
    );
    let page = coord.debug_page_texts();
    assert!(
        page.iter().any(|t| t == "应" || t == "英" || t == "影"),
        "`y;` 须解成 ying 并给出其候选，实际={page:?}"
    );

    // 退格删的是那一个击键，回到 `y` 继续组合（不是把整串清掉、也不是删出半个音节）。
    assert_eq!(
        action_text(&tap(&coord, VK_BACK)).as_deref(),
        Some("y"),
        "退格应还原到 y"
    );

    // 多音节：`;` 须参与音节边界切分，`n;` + `ni` 显示为 `n;'ni`（击键 + 边界分隔符）。
    // 单音节场景看不出边界有没有算对，这条才锁得住。
    let coord_multi =
        Coordinator::new_headless_with_override(shuangpin_cfg(), Some(&d), Some(ov.clone()));
    press_letter(&coord_multi, 'n');
    press_char(&coord_multi, ';');
    press_letter(&coord_multi, 'n');
    let act = press_letter(&coord_multi, 'i');
    assert_eq!(
        action_text(&act).as_deref(),
        Some("n;'ni"),
        "`;` 须参与音节边界（ning + ni），而不是被当成粘在串里的普通字符"
    );

    // Shift+`;` 是 `:`，不在码元集里 → 仍作标点（顶字上屏 + 全角冒号）。
    let coord2 = Coordinator::new_headless_with_override(shuangpin_cfg(), Some(&d), Some(ov));
    press_letter(&coord2, 'y');
    let act = press_char(&coord2, ':');
    let text = action_text(&act).unwrap_or_default();
    assert!(
        text.ends_with('：') || text.ends_with(':'),
        "Shift+`;` 应作冒号标点，实际={text:?}"
    );
}

/// 空缓冲下的 `;` **不归双拼**：它在布局里只作韵母（第二码），故不进首码集，
/// quick_mix 引导键照常生效。这是「首码集是仲裁者」契约的另一半——
/// 只接前半条（`;` 进全集）会让用户永远进不去快捷输入。
#[test]
fn test_shuangpin_symbol_final_yields_on_empty_buffer() {
    let d = data_dir();
    if !d.join("schemas/shuangpin.schema.toml").exists() {
        eprintln!("跳过：缺 shuangpin.schema.toml");
        return;
    }
    let ov = shuangpin_layout_override("mspy_lead", "mspy");
    let coord = Coordinator::new_headless_with_override(shuangpin_cfg(), Some(&d), Some(ov));

    press_char(&coord, ';');
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "空缓冲按 `;` 应进快捷输入（`;` 不是双拼首码）"
    );
}

/// 反向对照：小鹤布局的键全是字母 → 码元集回落 `a-z`，`;` 仍是次选键。
/// 没有这条，上面两个测试也可能被「无条件把 `;` 当码元」蒙混过去。
#[test]
fn test_shuangpin_alpha_only_layout_keeps_semicolon_as_select_key() {
    let d = data_dir();
    if !d.join("schemas/shuangpin.schema.toml").exists() {
        eprintln!("跳过：缺 shuangpin.schema.toml");
        return;
    }
    let ov = shuangpin_layout_override("xiaohe_sel", "xiaohe");
    let coord = Coordinator::new_headless_with_override(shuangpin_cfg(), Some(&d), Some(ov));

    type_str(&coord, "ni");
    let second = coord.debug_page_texts().get(1).cloned();
    let act = press_char(&coord, ';');
    assert_eq!(
        action_text(&act),
        second,
        "小鹤布局下 `;` 应仍选第 2 个候选，不得被当成码元累积"
    );
}

// ---- overlay 模式的编码区光标 ----

/// 临时英文：Shift+字母进入时缓冲已含首字母，光标须落其后（回归：曾因光标停在 0 而把
/// 后续字符插到首字母之前，"Hello" 变成 "elloH"）；随后可在编码区内移动并插入。
#[test]
fn test_temp_english_cursor_edit() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_shift_letter(&coord, 'h'); // 进入临英，缓冲 "H"
    let act = type_str(&coord, "ello");
    assert_eq!(action_text(&act).as_deref(), Some("Hello"));
    assert_eq!(action_caret(&act), Some(5));

    // He|llo → 插入 'X'
    tap(&coord, VK_HOME);
    assert_eq!(action_caret(&tap(&coord, VK_RIGHT)), Some(1));
    let act = press_letter(&coord, 'x');
    assert_eq!(action_text(&act).as_deref(), Some("Hxello"), "应插在光标处");
    assert_eq!(action_caret(&act), Some(2));

    // Delete 删光标后的 'e'
    let act = tap(&coord, VK_DELETE);
    assert_eq!(action_text(&act).as_deref(), Some("Hxllo"));
    assert_eq!(action_caret(&act), Some(2), "Delete 后光标不动");
}

/// 网址模式：夺取进入时缓冲已含前缀（"www."），光标须落其后；支持光标位编辑。
#[test]
fn test_url_mode_cursor_edit() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.url.enabled = true;
    cfg.input.url.prefixes = vec!["www.".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in ['w', 'w', 'w'] {
        press_letter(&coord, c);
    }
    let enter = coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)); // '.' 补满前缀
    assert_eq!(action_text(&enter).as_deref(), Some("www."));
    let act = type_str(&coord, "ab");
    assert_eq!(
        action_text(&act).as_deref(),
        Some("www.ab"),
        "续打应追加在前缀之后"
    );
    assert_eq!(action_caret(&act), Some(6));

    // www.a|b → 退格删 'a'
    assert_eq!(action_caret(&tap(&coord, VK_LEFT)), Some(5));
    let act = tap(&coord, VK_BACK);
    assert_eq!(action_text(&act).as_deref(), Some("www.b"));
    assert_eq!(action_caret(&act), Some(4));
}

/// 临时拼音：与主输入同构——caret 需跨过引擎插入的音节分隔符，且模式引导符（`）作为只读
/// 前缀计入 caret，光标进不去。
#[test]
fn test_temp_pinyin_cursor_maps_through_separator() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    cfg.input.temp_pinyin.trigger_keys = vec!["backtick".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let enter = coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN)); // ` 进入临拼
    assert_eq!(
        action_text(&enter).as_deref(),
        Some("`"),
        "组合区显示引导符"
    );

    let act = type_str(&coord, "nihao");
    assert_eq!(action_text(&act).as_deref(), Some("`ni'hao"));
    assert_eq!(action_caret(&act), Some(7), "引导符 1 + 显示串 6");

    // 左移三次：`ni'ha|o → `ni'h|ao → `ni|'hao（跨过分隔符，6 → 5 → 3）
    assert_eq!(action_caret(&tap(&coord, VK_LEFT)), Some(6));
    assert_eq!(action_caret(&tap(&coord, VK_LEFT)), Some(5));
    assert_eq!(action_caret(&tap(&coord, VK_LEFT)), Some(3));

    // Home 只到剩余拼音开头（引导符之后），不进只读前缀
    assert_eq!(action_caret(&tap(&coord, VK_HOME)), Some(1));
    assert!(
        matches!(tap(&coord, VK_LEFT), KeyAction::Consumed),
        "已在最左：吃掉，不得退进引导符"
    );
}

// ── 全角（英文模式 / 中文模式数字）─────────────────────────────────────────────
// 背景：全角横跨两层门控——C++ `OnTestKeyDown` 决定是否吃键转发，Rust 决定是否转全角。
// 两侧不一致即「吃了再吐」(OnTestKeyDown(TRUE)+OnKeyDown(FALSE))，严格 TSF 宿主直接丢键。
// 下列用例锁的是 Rust 侧「C++ 吃了就必须出字」的契约。

/// 英文模式 + 全角的配置（C++ `english_fullwidth` 分支会吃 Letter|Number|Punctuation|Space）。
fn config_english_fullwidth() -> wind_config::Config {
    let mut cfg = config_with("pinyin");
    cfg.input.default.chinese_mode = false;
    cfg.input.default.full_width = true;
    cfg
}

#[test]
fn test_english_fullwidth_letters_digits_space() {
    if !has_schemas() {
        return;
    }
    // 回归：英文模式曾无条件 PassThrough（从不读 full_width），而 C++ 已为全角吃下这些键
    // → 吃了再吐 → Chrome/VSCode 等严格宿主里空格/数字/符号完全打不出。
    let coord = Coordinator::new_headless(config_english_fullwidth(), Some(&data_dir()));
    let cases = [
        (0x41_u32, "ａ", "小写字母"),
        (0x35, "５", "数字"),
        (0x20, "\u{3000}", "空格"),
        (0xBD, "－", "标点(减号)"),
        (0x60, "０", "小键盘数字"),
    ];
    for (vk, want, what) in cases {
        match coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN)) {
            KeyAction::InsertText { text, .. } => {
                assert_eq!(text, want, "英文全角{}应上屏全角", what)
            }
            other => panic!("英文全角{}应出字（透传即丢键），实际: {:?}", what, other),
        }
    }
}

#[test]
fn test_english_fullwidth_shift_and_capslock_case() {
    if !has_schemas() {
        return;
    }
    // 键被 TSF 吃下后系统不再代劳大小写，须由 Rust 按 CapsLock 镜像 XOR Shift 自行决定。
    use wind_ipc::protocol::MOD_SHIFT;
    let coord = Coordinator::new_headless(config_english_fullwidth(), Some(&data_dir()));
    // Shift+A → 大写全角
    match coord.handle_key_event(&key_event_mods(0x41, EVENT_KEY_DOWN, MOD_SHIFT)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "Ａ", "Shift+字母应大写全角"),
        other => panic!("实际: {:?}", other),
    }
    // Shift+1 → '!' 的全角（走 punct_char 的 shifted 支）
    match coord.handle_key_event(&key_event_mods(0x31, EVENT_KEY_DOWN, MOD_SHIFT)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "！", "Shift+1 应出全角叹号"),
        other => panic!("实际: {:?}", other),
    }
    // CapsLock 开（toggles bit0）+ 无 Shift → 大写全角；镜像由每键 toggles 快照校准。
    let caps = KeyEventData {
        toggles: 0x01,
        ..key_event(0x41, EVENT_KEY_DOWN)
    };
    match coord.handle_key_event(&caps) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "Ａ", "CapsLock 应大写全角"),
        other => panic!("实际: {:?}", other),
    }
    // CapsLock + Shift → 相互抵消回小写
    let caps_shift = KeyEventData {
        toggles: 0x01,
        modifiers: MOD_SHIFT,
        ..key_event(0x41, EVENT_KEY_DOWN)
    };
    match coord.handle_key_event(&caps_shift) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "ａ", "CapsLock+Shift 应抵消回小写"),
        other => panic!("实际: {:?}", other),
    }
}

#[test]
fn test_english_halfwidth_still_passthrough() {
    if !has_schemas() {
        return;
    }
    // 零回归：英文半角仍须透传（C++ 此时也不吃键），保留宿主 WM_KEYDOWN 原生语义。
    let mut cfg = config_with("pinyin");
    cfg.input.default.chinese_mode = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for vk in [0x41_u32, 0x35, 0x20, 0xBD] {
        assert!(
            matches!(
                coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN)),
                KeyAction::PassThrough
            ),
            "英文半角 vk=0x{:02X} 应透传",
            vk
        );
    }
}

#[test]
fn test_english_fullwidth_ctrl_alt_not_intercepted() {
    if !has_schemas() {
        return;
    }
    // Ctrl/Alt 组合是快捷键：C++ 的 ClassifyInputKey 对其返回 None 本就不吃，
    // Rust 侧须对称放行，否则会把宿主快捷键（Ctrl+A 等）吞成全角字符。
    use wind_ipc::protocol::{MOD_ALT, MOD_CTRL};
    let coord = Coordinator::new_headless(config_english_fullwidth(), Some(&data_dir()));
    for mods in [MOD_CTRL, MOD_ALT] {
        assert!(
            matches!(
                coord.handle_key_event(&key_event_mods(0x41, EVENT_KEY_DOWN, mods)),
                KeyAction::PassThrough
            ),
            "英文全角下 Ctrl/Alt 组合应透传给宿主"
        );
    }
}

#[test]
fn test_english_fullwidth_autopair_uses_fullwidth_pairs() {
    if !has_schemas() {
        return;
    }
    // 配对表须由 english_pairs 逐字符过同一条流水线派生：打 `(` 出 `（` 就配 `）`。
    // 关键回归：不可复用 cn_pairs——`to_full_width('[')` = `［`(U+FF3B) 而 cn_pairs 是
    // `【`(U+3010)，混用会「打 [ 出 【 却配 ］」。故此处专测 `[`。
    let mut cfg = config_english_fullwidth();
    cfg.input.auto_pair.english = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    match coord.handle_key_event(&key_event(0xDB, EVENT_KEY_DOWN)) {
        KeyAction::InsertTextWithCursor {
            text,
            cursor_offset,
        } => {
            assert_eq!(text, "［］", "全角 `[` 应配全角 `］`，而非中文的 【】");
            assert_eq!(cursor_offset, 1, "光标应落在配对之间");
        }
        other => panic!("英文全角 `[` 应插入全角配对，实际: {:?}", other),
    }
}

#[test]
fn test_chinese_fullwidth_digits_1_to_9() {
    if !has_schemas() {
        return;
    }
    // 回归：中文全角空缓冲下 1-9 曾恒 PassThrough（无视 full_width），而 C++ 为全角专门
    // 在无 session 时也吃数字（`chinese_fullwidth_number`）→ 吃了再吐 → 部分应用丢键、
    // 部分出半角。`0` 因无该 match 臂、落标点流水线，反而一直正常——本测锁死两者一致。
    let mut cfg = config_with("pinyin");
    cfg.input.default.full_width = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for (vk, want) in [
        (0x31_u32, "１"),
        (0x35, "５"),
        (0x39, "９"),
        (0x30, "０"), // `0` 走另一条路（标点流水线），须与 1-9 结果一致
    ] {
        match coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN)) {
            KeyAction::InsertText { text, .. } => {
                assert_eq!(text, want, "中文全角数字 vk=0x{:02X} 应上屏全角", vk)
            }
            other => panic!("中文全角数字 vk=0x{:02X} 应出字，实际: {:?}", vk, other),
        }
    }
}

#[test]
fn test_chinese_halfwidth_digits_still_passthrough() {
    if !has_schemas() {
        return;
    }
    // 零回归：半角态空缓冲数字仍透传（C++ 此时不吃），保留宿主原生按键语义。
    let coord = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    for vk in [0x31_u32, 0x39] {
        assert!(
            matches!(
                coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN)),
                KeyAction::PassThrough
            ),
            "中文半角空缓冲数字应透传"
        );
    }
}

#[test]
fn test_chinese_capslock_fullwidth_space_and_numpad() {
    if !has_schemas() {
        return;
    }
    // 回归：CapsLock+全角分支原用 printable_char 取字符，而它不含 VK_SPACE(punct_char 无该键)
    // 也不含小键盘 → 落 PassThrough。但 C++ 在中文全角下对空格(chinese_fullwidth_space)
    // 与小键盘(chinese_fullwidth_number)都吃键 → 吃了再吐 → 严格 TSF 宿主丢键。
    // 现由 full_width_source_char 统一收口，保证 Rust 出字集 ⊇ C++ 吃键集。
    let mut cfg = config_with("pinyin");
    cfg.input.default.full_width = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for (vk, want, what) in [
        (0x20_u32, "\u{3000}", "空格"),
        (0x60, "０", "小键盘 0"),
        (0x41, "Ａ", "字母(CapsLock 大写)"),
    ] {
        let ev = KeyEventData {
            toggles: 0x01, // CapsLock ON
            ..key_event(vk, EVENT_KEY_DOWN)
        };
        match coord.handle_key_event(&ev) {
            KeyAction::InsertText { text, .. } => {
                assert_eq!(text, want, "CapsLock+全角 {} 应上屏全角", what)
            }
            other => panic!(
                "CapsLock+全角 {} 应出字（透传即丢键），实际: {:?}",
                what, other
            ),
        }
    }
}

#[test]
fn test_chinese_fullwidth_numpad_direct_no_caps() {
    if !has_schemas() {
        return;
    }
    // 定位用：中文全角、非 CapsLock、空缓冲、default(direct) numpad_behavior 下，
    // 小键盘数字应走 numpad direct 分支的 to_full_width 出全角。
    // 若本测通过而真机仍半角/丢键 → 问题在 C++ 吃键或 full_width 跨进程同步，不在 core 逻辑。
    let mut cfg = config_with("pinyin");
    cfg.input.default.full_width = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for (vk, want) in [(0x60_u32, "０"), (0x65, "５"), (0x69, "９")] {
        match coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN)) {
            KeyAction::InsertText { text, .. } => {
                assert_eq!(
                    text, want,
                    "中文全角小键盘(direct) vk=0x{:02X} 应出全角",
                    vk
                )
            }
            other => panic!("中文全角小键盘 vk=0x{:02X} 应出字，实际: {:?}", vk, other),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// 码表自动造词端到端
//
// ⚠ 必须走 `handle_key_event_policed`（bridge 真入口，server.rs:440 调的就是它）。
// 本文件其余测试调的是裸 `handle_key_event`，那条路**不经过**自提交打点与造词投喂，
// 用它写造词测试会得到「永远不造词」的假象。
// ──────────────────────────────────────────────────────────────────────────

use std::sync::Arc;
use wind_store::Store;

/// 建一个开/关自动造词的 wubi86 无头协调器 + 独立 store。
fn auto_phrase_coord(tag: &str, enabled: bool) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.auto_phrase.enabled = enabled;
    let db = std::env::temp_dir().join(format!("wind_auto_phrase_{tag}.redb"));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), Arc::clone(&store));
    // 造词要用的两张表（反查索引 + 单字全码表）都是惰性全量构建，生产里由启动后的
    // 后台预热线程建好（construct.rs → prewarm_indexes）。headless 不跑那个线程。
    //
    // 而造词路径现在会在它们未就绪时**主动跳过**——那是刻意的：跑在上屏线程上不能现建
    // （大词库秒级、TSF 同步 IPC 会卡整机），更不能把「没就绪」当成「查不到」继续，
    // 否则查重失效、会往临时层写系统词库已有的重复条目。故测试须自己预热。
    coord.prewarm_indexes();
    (coord, store, db)
}

/// 枚举某方案下全部临时词（空前缀即扫该方案全部键）。
fn temp_words(store: &Store, schema: &str) -> Vec<(String, String)> {
    store
        .search_temp_words_prefix(schema, "", 200)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.code, r.text))
        .collect()
}

/// 敲「字母 + 空格」上屏一个字，返回上屏文本。
fn commit_one_char(coord: &Coordinator, letter: u8) -> String {
    coord.handle_key_event_policed(&key_event(letter as u32, EVENT_KEY_DOWN));
    match coord.handle_key_event_policed(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("空格应上屏 InsertText，实际: {:?}", other),
    }
}

/// 连续单字上屏 → 终止信号 → 造出词组并写入临时词库。
///
/// 覆盖历史上「完全不工作」的两个断裂：触发源（旧实现挂在拼音专属的 `committed_segs` 上，
/// 码表恒不满足）与编码算法（旧实现拼接各段全码，造出的码查不出来）。
#[test]
fn test_codetable_auto_phrase_learns_from_single_chars() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let (coord, store, db) = auto_phrase_coord("learn", true);

    let a = commit_one_char(&coord, b'A');
    let b = commit_one_char(&coord, b'A');
    let word = format!("{a}{b}");
    assert_eq!(word.chars().count(), 2, "应上屏两个单字，实际: {:?}", word);

    // 造词发生在终止信号（此处用失焦，等价于打完一句切窗口）。
    coord.handle_focus_lost(0, wind_bridge::handler::FocusLostReason::Thread);

    let words = temp_words(&store, "wubi86");
    let hit = words
        .iter()
        .find(|(_, t)| *t == word)
        .unwrap_or_else(|| panic!("终止信号后应造出「{word}」，临时层实际: {words:?}"));
    // 五笔二字词规则 AaAbBaBb = 各字全码前两位 → 码长恒为 4。
    // 这条同时否掉了「拼接各字全码」的旧做法（那会得到 7~8 位）。
    assert_eq!(
        hit.0.chars().count(),
        4,
        "二字词组码应为 4 位（各字全码前两位），实际: {}",
        hit.0
    );
    let _ = std::fs::remove_file(&db);
}

/// 造词只在终止信号发生：上屏过程中不得写库，否则每打一个字就造一次半截词。
#[test]
fn test_codetable_auto_phrase_does_not_learn_before_terminator() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let (coord, store, db) = auto_phrase_coord("before_term", true);
    commit_one_char(&coord, b'A');
    commit_one_char(&coord, b'A');
    assert!(
        temp_words(&store, "wubi86").is_empty(),
        "终止信号之前不应写入任何临时词，实际: {:?}",
        temp_words(&store, "wubi86")
    );
    let _ = std::fs::remove_file(&db);
}

/// 开关关闭时闸门有效，一个词都不造。
#[test]
fn test_codetable_auto_phrase_disabled_learns_nothing() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let (coord, store, db) = auto_phrase_coord("disabled", false);
    commit_one_char(&coord, b'A');
    commit_one_char(&coord, b'A');
    coord.handle_focus_lost(0, wind_bridge::handler::FocusLostReason::Thread);
    assert!(
        temp_words(&store, "wubi86").is_empty(),
        "开关关闭时不应造词，实际: {:?}",
        temp_words(&store, "wubi86")
    );
    let _ = std::fs::remove_file(&db);
}

/// 单字不成词：只上屏一个字就终止，不应写库（min_phrase_len=2）。
#[test]
fn test_codetable_auto_phrase_single_char_is_not_a_word() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let (coord, store, db) = auto_phrase_coord("single", true);
    commit_one_char(&coord, b'A');
    coord.handle_focus_lost(0, wind_bridge::handler::FocusLostReason::Thread);
    assert!(
        temp_words(&store, "wubi86").is_empty(),
        "单字不应成词，实际: {:?}",
        temp_words(&store, "wubi86")
    );
    let _ = std::fs::remove_file(&db);
}

/// 前缀匹配的全局短语（此处以 `$AA` 组 marker 为例）按**来源**统一处理：来源=短语库、全局、
/// 不与方案挂钩，故前缀命中一律避让、不占首位——码表下与更长编码补全按权重同档、拼音/混输下
/// 降到拼音精确候选之下。**不按语法类型区分**（`$CC`/`$SS`/静态同规则），也不再靠 40M 类别硬顶。
///
/// 回归：marker 来自 `lookup_prefix`（前缀枚举、码严格更长＝非完全匹配），曾被标 `is_exact_code=true` +
/// `PHRASE_WEIGHT_BASE`(40M，该常量后已整体删除) 抬进精确档并整体上浮，压过普通候选（用户报「系统/用户短语前缀
/// 匹配时优先级偏高、压普通编码/候选」）。现改为 `is_exact_code=false` + `is_prefix=!codetable` +
/// `weight=hit.weight`。低权重（1）确保 marker 可靠沉到码表候选之下，隔离出「避让」这一单一断言。
/// 构造组短语码 `nia`（严格长于输入 `ni` → 前缀枚举命中）。
fn coord_with_group_phrase(schema: &str, tag: &str) -> std::sync::Arc<Coordinator> {
    let store_path = std::env::temp_dir().join(format!("wind_group_marker_{tag}.redb"));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store
        .add_phrase("nia", r#"$AA("测试组", "①②③")"#, 0, 1)
        .unwrap();
    let mut cfg = config_with(schema);
    cfg.input.phrase.min_prefix = 2;
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store)
}

/// 断言：前缀 marker 仍在候选列表里，但**不占首位**（避让首选普通候选）。
fn assert_group_marker_defers(coord: &Coordinator, mode: &str) {
    let texts = coord.debug_all_candidate_texts();
    let group_pos = texts.iter().position(|t| t == "测试组");
    assert!(
        group_pos.is_some(),
        "[{mode}] 前缀枚举应仍列出组 marker，实际: {:?}",
        texts
    );
    assert_ne!(
        group_pos,
        Some(0),
        "[{mode}] 前缀匹配的组 marker 不应占首位（须避让普通候选），实际: {:?}",
        texts
    );
}

#[test]
fn prefix_group_marker_defers_below_pinyin_candidates() {
    if !has_schemas() {
        return;
    }
    let coord = coord_with_group_phrase("pinyin", "pinyin");
    for ch in ['n', 'i'] {
        press_letter(&coord, ch);
    }
    // 拼音：is_prefix 使 marker 落到拼音精确候选（is_prefix=false）之下。
    assert_group_marker_defers(&coord, "pinyin");
}

#[test]
fn prefix_group_marker_defers_in_codetable_too() {
    if !has_schemas() {
        return;
    }
    // 码表：marker 不再靠 is_exact_code+40M 置顶（旧行为），改按权重——低权重沉到码表候选之下。
    // 与拼音测试同断言，印证「按来源统一避让」而非按引擎模式分档。
    let coord = coord_with_group_phrase("wubi86", "wubi");
    for ch in ['n', 'i'] {
        press_letter(&coord, ch);
    }
    assert_group_marker_defers(&coord, "wubi86");
}

/// 真机回归（`nunl`）：混输下满 4 码，五笔无候选，拼音只有**部分匹配**「嫩」——
/// `nun` 是标准音节表中的稀有音节（为双拼转换真值补入），故 `nunl` 被切成
/// 「完成音节 nun + 残码 l」，候选只消费 3 码。用户诉求：这不算匹配，满码应清空。
///
/// **这是三道门串联的唯一端到端验证**，缺任何一道都不会清空：
/// ① 码表 `clear_on_empty_max`（满码 + 无候选 + 无更长后继）
/// ② 混输 `should_clear`（两道拼音守护，受 `auto_commit_block_on_pinyin` 支配）
/// ③ 协调器 `clear_blocked_by_candidates`（拼音部分匹配不算有效候选）
#[test]
fn test_mixed_full_code_clears_when_only_partial_pinyin() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_mixed();
    cfg.schema.codetable.clear_on_empty_max = true;
    cfg.schema.mix.auto_commit_block_on_pinyin = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // 前置：打到 nun（3 键）时拼音候选「嫩」确实存在——否则后面测的根本不是本场景
    // （多道闸门串联时，「无候选」会让测试静默退化成从不执行被测分支的假绿）。
    // 必须查**全部**候选而非当前页：nun 的首页被五笔前缀词（习惯/憧憬…）占满，
    // 「嫩」排在第 8 位、落到第二页去了。
    for c in "nun".chars() {
        press_letter(&coord, c);
    }
    let all = coord.debug_all_candidate_texts();
    assert!(
        all.iter().any(|t| t == "嫩"),
        "前置：nun 应出拼音候选「嫩」（41448 大字表的异读注音），实际: {:?}",
        &all[..all.len().min(10)]
    );

    // 第 4 键 l：满码，五笔无候选，拼音只剩部分匹配（嫩/嫰/黁，code 均为 nun、消费 3 码）→ 清空。
    // 注意此刻候选列表**非空**（3 条），旧判据 `state.candidates.is_empty()` 正是在这里拦下清空的。
    match press_letter(&coord, 'l') {
        KeyAction::ClearComposition => {}
        other => panic!(
            "满 4 码仅剩拼音部分匹配时应清空缓冲，实际: {:?}，候选: {:?}",
            other,
            coord.debug_all_candidate_texts()
        ),
    }
}

/// 反向锁，与上一个测试构成**单一变量对照**：同样满 4 码、同样关掉守护开关、同样是
/// 「完整音节 + 单个声母字母」的结构（`wanl` vs `nunl`），唯一差别是拼音候选的类型——
///
/// | 输入 | 候选 | code | consumed | 判定 |
/// |---|---|---|---|---|
/// | `nunl` | 嫩 | `nun`（比输入**短**） | 3 < 4 | 部分匹配 → 清空 |
/// | `wanl` | 完了/晚了 | `wanle`（比输入**长**） | 4 = 4 | 前缀补全 → 拦住 |
///
/// 这一条锁住的正是「拼音还没打完」的中途态保护：前缀补全候选消费整串，天然拦下清空，
/// 用户接着打 `wanle` 不会被吞。**关掉守护开关并不会牺牲这类中途态**——真正被清空的只有
/// 「候选全是部分匹配」的串，也就是确实打岔了的那些。
#[test]
fn test_mixed_full_code_keeps_prefix_completion_candidates() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_mixed();
    cfg.schema.codetable.clear_on_empty_max = true;
    cfg.schema.mix.auto_commit_block_on_pinyin = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let mut last = None;
    for c in "wanl".chars() {
        last = Some(press_letter(&coord, c));
    }
    assert!(
        !matches!(last, Some(KeyAction::ClearComposition)),
        "wanl 有前缀补全候选（wanle→完了），不得清空"
    );
    let all = coord.debug_all_candidate_texts();
    assert!(
        all.iter().any(|t| t == "完了" || t == "晚了"),
        "应保留消费整串的前缀补全候选，实际: {:?}",
        &all[..all.len().min(10)]
    );
}

/// 翻页键（默认 `-`/`=`）在临英下应翻页。
/// 回归点：`handle_candidate_nav` 曾按 `ModeKind` 把临英整类排除出可打印导航键
/// （`include_printable` 恒 false），于是 `=` 落到 `_ =>` 标点臂被判成「上屏高亮候选 +
/// 标点」——用户按 `=` 想翻页，实得首候选连同 `=` 被直接上屏并退出临英（`Hel=`）。
/// 与二三候选键 `;`/`'` 是同一条兜底臂的两个出口，但成因不同（那次是漏调选词偏移）。
#[test]
fn test_temp_english_page_keys_flip_pages_when_symbols_disallowed() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    // 显式声明键组与页大小，使本测试不随默认值漂移（默认亦含 minus_equal）。
    cfg.keys.page_keys = vec!["pageupdown".into(), "minus_equal".into()];
    cfg.ui.candidate.per_page = 3;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    // 前置条件：多于一页，否则 page_next 返回 false，翻页分支测不出差异（假绿）。
    let (_, _, total_pages) = coord.debug_page_info();
    assert!(
        total_pages >= 2,
        "前置条件：应有 ≥2 页候选，否则测不到翻页，实际 {total_pages} 页"
    );
    let page0_first = coord.debug_page_texts()[0].clone();

    let act = press_vk(&coord, 0xBB, false); // `=` 下一页
    assert!(
        matches!(act, KeyAction::Consumed),
        "`=` 应作翻页被消费，而非上屏退出临英，实际: {act:?}"
    );
    assert_eq!(coord.debug_page_info().0, 1, "`=` 应翻到第 2 页");
    assert_ne!(
        coord.debug_page_texts()[0],
        page0_first,
        "第 2 页首候选应与第 1 页不同"
    );

    let act = press_vk(&coord, 0xBD, false); // `-` 上一页
    assert!(
        matches!(act, KeyAction::Consumed),
        "`-` 应作翻页被消费，实际: {act:?}"
    );
    assert_eq!(coord.debug_page_info().0, 0, "`-` 应翻回第 1 页");
}

/// 对照组：`=` **被列入白名单**时翻页键让位于字符输入——列入的字符语义是「入缓冲，
/// 而非上屏退出、选词或导航」，与二三候选键 / 数字臂同构，不能被翻页接线破坏。
#[test]
fn test_temp_english_page_keys_yield_to_input_when_listed() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.page_keys = vec!["pageupdown".into(), "minus_equal".into()];
    cfg.ui.candidate.per_page = 3;
    cfg.input.temp_english.allow_symbols = true;
    cfg.input.temp_english.symbol_chars = "=".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    assert!(
        coord.debug_page_info().2 >= 2,
        "前置条件：应有 ≥2 页候选，否则「有得翻却不翻」无从谈起"
    );
    let act = press_vk(&coord, 0xBB, false); // `=`
    assert_eq!(
        action_text(&act).unwrap(),
        "Hel=",
        "`=` 在白名单内时应入缓冲而非翻页"
    );
    assert_eq!(coord.debug_page_info().0, 0, "让位输入时不应翻页");
}

/// 对照组之反向：白名单只含 `-` 时，同一键组的另一半 `=` 仍翻页。
///
/// 锁住「按字符而非按键组让位」这个判据——`minus_equal` 是成对键组，旧实现下
/// allow_symbols 一开两个一起让位；改造后 `-` 入缓冲、`=` 照旧翻下一页。
/// 这也是出厂白名单含 `-` 的已知代价（上一页只剩 ↑ 与 PgUp）的行为快照。
#[test]
fn test_temp_english_page_key_group_split_by_whitelist() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.keys.page_keys = vec!["pageupdown".into(), "minus_equal".into()];
    cfg.ui.candidate.per_page = 3;
    cfg.input.temp_english.allow_symbols = true;
    cfg.input.temp_english.symbol_chars = "-".into(); // 只列 `-`，不列 `=`
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_shift_letter(&coord, 'h');
    press_letter(&coord, 'e');
    press_letter(&coord, 'l');
    assert!(
        coord.debug_page_info().2 >= 2,
        "前置条件：应有 ≥2 页候选，否则测不到翻页分支"
    );
    let act = press_vk(&coord, 0xBB, false); // `=` 未列入 → 仍翻页
    assert!(
        matches!(act, KeyAction::Consumed),
        "`=` 不在白名单时应作翻页被消费，实际: {act:?}"
    );
    assert_eq!(coord.debug_page_info().0, 1, "`=` 应翻到第 2 页");
    let act = press_vk(&coord, 0xBD, false); // `-` 已列入 → 入缓冲
    assert_eq!(
        action_text(&act).unwrap(),
        "Hel-",
        "`-` 在白名单内时应入缓冲，不参与翻页"
    );
}

// ───── 普通输入（拼音/码表）的大写字母：只进显示与上屏原码，不进匹配 ─────

/// 缓冲非空时 Shift+字母：组合区如实显示大写，回车上屏也是大写（打 `aBC` 得 `aBC`）。
///
/// 边界（既有行为，不在本功能范围内）：**空缓冲**的 Shift+字母是临时英文的进入方式，
/// 到不了这里；故用户要打的临时短英文须以小写字母起头。
#[test]
fn normal_input_keeps_uppercase_in_preedit_and_raw_commit() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    let last = press_str(&coord, "aBC");
    assert_eq!(
        action_text(&last).unwrap(),
        "aBC",
        "组合区应如实呈现所打的大小写"
    );
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "aBC", "回车上屏原码应保留大写"),
        other => panic!("回车应上屏原码，实际: {:?}", other),
    }
}

/// ★ 反向对照：大写**只影响呈现**。`aBC` 与 `abc` 的候选列表必须逐项相同——
/// 这条是「不影响码表和拼音的基础匹配」的直接判据，若哪天有人把大写写进查询串就会红。
#[test]
fn normal_input_uppercase_does_not_change_candidates() {
    if !has_schemas() {
        return;
    }
    let upper = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_str(&upper, "aBC");
    let lower = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_str(&lower, "abc");
    assert!(
        !lower.debug_page_texts().is_empty(),
        "前置条件：abc 须有候选，否则两边同为空的「相等」什么也没证明"
    );
    assert_eq!(
        upper.debug_page_texts(),
        lower.debug_page_texts(),
        "大写只改呈现，候选必须与全小写完全一致"
    );
}

/// 拼音的组合区是音节拆分串（含引擎插入的 `'`）：大写按序投影回去，分隔位置不受影响。
#[test]
fn normal_input_uppercase_projects_onto_pinyin_split_display() {
    if !has_schemas() {
        return;
    }
    let upper = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    let up = action_text(&press_str(&upper, "nIhao")).unwrap();
    let lower = Coordinator::new_headless(config_with("pinyin"), Some(&data_dir()));
    let low = action_text(&press_str(&lower, "nihao")).unwrap();
    assert_eq!(
        up.to_ascii_lowercase(),
        low,
        "投影只该改大小写，分隔符与拆分位置必须与全小写一致"
    );
    assert!(
        up.contains('I'),
        "第 2 个字母打的是大写，应如实显示: {}",
        up
    );
}

/// 退格后大写随之收缩：`aBC` 退一格 → `aB`，上屏也是 `aB`。
#[test]
fn normal_input_uppercase_survives_backspace() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_str(&coord, "aBC");
    let act = coord.handle_key_event(&key_event(0x08, EVENT_KEY_DOWN)); // Backspace
    assert_eq!(action_text(&act).unwrap(), "aB");
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "aB"),
        other => panic!("回车应上屏原码，实际: {:?}", other),
    }
}

/// ★ 陈旧大写不得串到下一轮输入：`aBC` → Esc → 再打全小写 `abc`，上屏必须是 `abc`。
/// 缓冲有二十余处写入点，靠的不是逐个接线而是「影子串与缓冲失配即作废」，本条守住它。
#[test]
fn normal_input_uppercase_does_not_leak_into_next_composition() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    press_str(&coord, "aBC");
    coord.handle_key_event(&key_event(0x1B, EVENT_KEY_DOWN)); // Esc 放弃整段
    let last = press_str(&coord, "abc");
    assert_eq!(action_text(&last).unwrap(), "abc", "上一轮的大写不该复活");
    match coord.handle_key_event(&key_event(0x0D, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "abc"),
        other => panic!("回车应上屏原码，实际: {:?}", other),
    }
}

// ── 特殊模式（快符等）的候选词条操作 ─────────────────────────────────────────
//
// 回归背景：特殊方案接上词库管理时只接了**读端**（`apply_shadow_in` / `record_selection_in`
// 按 `effective_data_schema` 归属），三条写侧通路仍停在旧假设上——右键菜单被 `overlay`
// 判据整块拒开、`candidate_op` 因 `input_buffer` 恒空而首行 return、删除热键按 ModeKind
// 整类屏蔽。**同一能力的多条通路只接一条等于没接**，且失效完全静默。

/// 主方案**拼音** + 快符引用**五笔**：这个错配组合是回归的关键。
/// 若引擎类型或归属方案任一处照抄主方案，症状分别是「快符候选被当拼音候选禁掉调位」
/// 与「shadow 规则写进 pinyin 桶、读的却是 wubi86 桶」——后者记账看着成功、顺序永不动。
fn special_op_fixture(
    tag: &str,
    show_all: bool,
) -> (
    std::sync::Arc<Coordinator>,
    std::path::PathBuf,
    std::sync::Arc<wind_store::Store>,
) {
    let store_path = std::env::temp_dir().join(format!("wind_special_op_{}.redb", tag));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let mut cfg = config_with("pinyin");
    let ov = overlay_override_dir(tag, &[("wubi86", show_all)]);
    bind_special(&mut cfg, "backslash", "wubi86");
    let coord = Coordinator::new_headless_with_store_override(
        cfg,
        Some(&data_dir()),
        store.clone(),
        Some(ov),
    );
    (coord, store_path, store)
}

fn enter_special_mode_via_backslash(coord: &Coordinator) {
    let act = coord.handle_key_event(&key_event(0xDC, EVENT_KEY_DOWN));
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        r"\ 应进入特殊模式，实际: {:?}",
        act
    );
}

/// 作用域解析：快符的落点是「它引用的方案 + 它自己的编码缓冲」。
/// 菜单可用性与写端准入共用此判据，故这一条同时锁住两条通路。
#[test]
fn special_mode_candidate_op_scope_uses_own_schema_and_buffer() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let (coord, path, _store) = special_op_fixture("scope", false);

    assert_eq!(
        coord.debug_candidate_op_scope(),
        None,
        "未输入时无候选也无落点"
    );

    enter_special_mode_via_backslash(&coord);
    assert_eq!(
        coord.debug_candidate_op_scope(),
        None,
        "特殊模式空码态不应有落点：读端 apply_shadow_in 首行即对空码 return，放行只会写进永不被读的规则"
    );

    press_letter(&coord, 'a');
    assert_eq!(
        coord.debug_candidate_op_scope(),
        Some(("wubi86".to_string(), "a".to_string())),
        "快符落点应为「引用方案 wubi86 + special_buffer」，而非主方案 pinyin + input_buffer"
    );

    let _ = std::fs::remove_file(&path);
}

/// 删除：候选真消失、列表不被清空、规则落在快符方案桶且不污染主方案桶。
#[test]
fn special_mode_candidate_delete_writes_to_own_schema_bucket() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    use wind_ui_types::CandidateOp;
    let (coord, path, store) = special_op_fixture("delete", false);
    enter_special_mode_via_backslash(&coord);
    press_letter(&coord, 'a');

    let before = coord.debug_page_texts();
    assert!(
        !before.is_empty(),
        "快符输入 a 应有候选（否则本用例失去意义）"
    );
    let target = before[0].clone();

    coord.debug_candidate_op(CandidateOp::Delete, 0);
    let after = coord.debug_page_texts();

    // ★ 这条锁的是「重建走对了路径」：主路径 update_candidates 读 input_buffer，而快符下它
    //   恒为空——走错的表现不是「界面没刷新」，是候选窗当场被清空。
    assert!(
        !after.is_empty(),
        "删除后候选列表不应被清空（重建须走 update_special_candidates）"
    );
    assert!(!after.contains(&target), "被删的 '{}' 不应再出现", target);

    // ★ 读写同源：规则必须落在快符方案桶。写进主方案桶的表现同样是「删了没反应」——
    //   因为读端查的是 wubi86 桶。两种失败在屏幕上完全同形，只有查桶才分得开。
    let special_bucket = store.get_shadow_rules("wubi86", "a").unwrap();
    assert!(
        special_bucket
            .as_ref()
            .is_some_and(|r| r.deleted.contains(&target)),
        "shadow 规则应写入快符方案桶 wubi86/a，实际: {:?}",
        special_bucket
    );
    assert!(
        store
            .get_shadow_rules("pinyin", "a")
            .unwrap()
            .is_none_or(|r| !r.deleted.contains(&target)),
        "主方案 pinyin 桶不应被污染"
    );

    let _ = std::fs::remove_file(&path);
}

/// 调位：快符引用的是码表方案，即便主方案是拼音也应可置顶。
/// 此前 `current_engine_type()` 问的是主方案，这里会被整体误禁。
#[test]
fn special_mode_candidate_move_top_uses_own_engine_type() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    use wind_ui_types::CandidateOp;
    let (coord, path, _store) = special_op_fixture("movetop", false);
    enter_special_mode_via_backslash(&coord);
    press_letter(&coord, 'a');

    let before = coord.debug_page_texts();
    assert!(
        before.len() >= 2,
        "快符输入 a 应有 ≥2 个候选（否则置顶无从验证），实际: {:?}",
        before
    );
    let second = before[1].clone();

    coord.debug_candidate_op(CandidateOp::MoveTop, 1);
    assert_eq!(
        coord.debug_page_texts().first(),
        Some(&second),
        "快符（码表方案）候选应可置顶——引擎类型须按快符引用的方案取，而非主方案 pinyin"
    );

    let _ = std::fs::remove_file(&path);
}

/// 空码浏览态（`show_all_on_enter`）：**必须支持**词条操作，且规则要真的被读回来。
///
/// 这是真机报「快符右键仍只有复制」的那个场景：`max_code_length=1` +
/// `auto_commit_at_full` 的方案敲一码即上屏，浏览态是用户唯一能右键的时机。
/// 空码是合法 shadow 键位（key = `"{schema}\0{code}"`），读写两端都按候选非空放行。
#[test]
fn special_mode_show_all_browse_state_supports_candidate_op() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    use wind_ui_types::CandidateOp;
    let (coord, path, store) = special_op_fixture("browse", true);
    enter_special_mode_via_backslash(&coord);

    let before = coord.debug_all_candidate_texts();
    assert!(!before.is_empty(), "show_all_on_enter 应枚举出候选");
    assert_eq!(
        coord.debug_candidate_op_scope(),
        Some(("wubi86".to_string(), String::new())),
        "浏览态落点＝快符方案 + 空码；返回 None 的表现就是右键只剩「复制」"
    );

    let target = before[0].clone();
    coord.debug_candidate_op(CandidateOp::Delete, 0);
    let after = coord.debug_all_candidate_texts();
    assert!(!after.is_empty(), "浏览态删除后不应清空候选");
    assert!(
        !after.contains(&target),
        "浏览态删除的 '{}' 不应再出现",
        target
    );

    // ★ 规则必须落在「快符方案 + 空码」桶，且能被浏览态重新枚举时读回——只写不读的表现是
    //   「当场看着删掉了、重进模式又回来了」。
    let rec = store.get_shadow_rules("wubi86", "").unwrap();
    assert!(
        rec.as_ref().is_some_and(|r| r.deleted.contains(&target)),
        "shadow 规则应写入 wubi86 + 空码桶，实际: {:?}",
        rec
    );

    let _ = std::fs::remove_file(&path);
}

/// 浏览态的调整必须**跨重新进入**存活：退出再进快符，被隐藏的候选不能复活。
/// 这一条锁的是读端——`update_special_candidates` 的空码分支若不调 `apply_shadow_in`，
/// 上一条测试照样全绿（规则确实写进了 store），只是永远没人读。
#[test]
fn special_mode_browse_state_shadow_survives_reenter() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    use wind_ui_types::CandidateOp;
    let (coord, path, _store) = special_op_fixture("reenter", true);
    enter_special_mode_via_backslash(&coord);

    let before = coord.debug_all_candidate_texts();
    assert!(!before.is_empty(), "show_all_on_enter 应枚举出候选");
    let target = before[0].clone();
    coord.debug_candidate_op(CandidateOp::Delete, 0);

    // Esc 退出特殊模式，再按 \ 重新进入 → 重新枚举一遍码表
    coord.handle_key_event(&key_event(0x1B, EVENT_KEY_DOWN));
    enter_special_mode_via_backslash(&coord);

    let reentered = coord.debug_all_candidate_texts();
    assert!(!reentered.is_empty(), "重进后应重新枚举出候选");
    assert!(
        !reentered.contains(&target),
        "重进浏览态后被隐藏的 '{}' 不应复活（空码分支须应用 shadow）",
        target
    );

    let _ = std::fs::remove_file(&path);
}

/// 其余 overlay 不随快符一起放开：临拼没有独立词库落点（`effective_data_schema` 对它
/// 返回 None，2026-08-04 用户拍板），放行会让规则静默落回主方案桶。
#[test]
fn temp_pinyin_overlay_still_has_no_candidate_op_scope() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.input.temp_pinyin.enabled = true;
    // z 进临拼改由方案级 `z_key_action` 配置——字母不再作全局 trigger_keys（那里只认符号）。
    // 本测试的关切（临拼没有候选词条操作作用域）与 z 怎么进临拼无关，只换配置写法。
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    press_letter(&coord, 'z'); // 空缓冲按 z 进入临拼
    for c in "hao".chars() {
        press_letter(&coord, c);
    }
    assert!(
        !coord.debug_all_candidate_texts().is_empty(),
        "临拼 hao 应有候选（否则本用例失去意义）"
    );
    assert_eq!(
        coord.debug_candidate_op_scope(),
        None,
        "临拼无独立词库落点，右键仍应只给复制"
    );
}

/// 精确匹配模式 + 进入即展示：浏览态只显示 1 条，隐藏它之后应**补上下一条**而非整屏空白。
///
/// 回归 2026-08-05 真机：`enumerate` 在引擎内按 `single_code_input` 先截到 1 条，协调器
/// 再 apply_shadow 过滤 → 0 条。「截断 → 过滤」这个次序把「隐藏一条」变成了「清空列表」。
/// 不变量：**从池中择 N 条必须发生在过滤之后**。
#[test]
fn special_mode_browse_exact_mode_hides_first_shows_next() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    use wind_ui_types::CandidateOp;
    let store_path = std::env::temp_dir().join("wind_special_browse_exact.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let mut cfg = config_with("pinyin");
    // ★ 精确匹配写在方案名下：wubi86 在本用例里带 `[overlay]` 段，不继承全局码表配置。
    let ov = overlay_override_dir_with_codetable(
        "special_mode_browse_exact_mode_hides_fir",
        &[("wubi86", true)],
        "single_code_input = true\n",
    );
    bind_special(&mut cfg, "backslash", "wubi86");
    let coord =
        Coordinator::new_headless_with_store_override(cfg, Some(&data_dir()), store, Some(ov));

    let act = coord.handle_key_event(&key_event(0xDC, EVENT_KEY_DOWN));
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        r"\ 应进入特殊模式，实际: {:?}",
        act
    );

    let first = coord.debug_all_candidate_texts();
    assert_eq!(
        first.len(),
        1,
        "精确匹配模式的浏览态应只展示 1 条，实际: {:?}",
        first
    );
    let hidden = first[0].clone();

    coord.debug_candidate_op(CandidateOp::Delete, 0);
    let after = coord.debug_all_candidate_texts();
    assert_eq!(
        after.len(),
        1,
        "隐藏首条后应补上下一条（截断须在 shadow 之后），实际: {:?}",
        after
    );
    assert_ne!(after[0], hidden, "补上的应是下一条，不该还是被隐藏的那条");

    let _ = std::fs::remove_file(&store_path);
}

/// 空码补全补出来的 `$CC` 直通命令候选，必须显示 display 标签而非 `$CC(...)` 源码。
///
/// 回归用户反馈：`completion_hint` 直接取自引擎、**绕过了 `finalize_candidates` 这个
/// 统一展开汇聚点**，而同一行下面的 `result.candidates` 走了。于是同一条词条，正常命中
/// 时显示标签、被当作补全兜底时显示源码。
#[test]
fn completion_hint_command_shows_display_label_not_source() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let store_path = std::env::temp_dir().join("wind_completion_hint_cc.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    // `aab` 复用既有回归码：六库均无精确 `aab`，但主库有 `aab?` 后继（见
    // special_mode_exact_completion_shows_longer_code）。给用户词一个压倒性权重，
    // 保证被备下的那条 hint 就是它。
    store
        .add_user_word("wubi86", "aaby", r#"$CC("《》", ask("x"))"#, 9_999_999, 0)
        .unwrap();
    let mut cfg = config_with("pinyin");
    // 精确匹配 + 空码补全：aab 无精确候选、更长后继有 → 引擎备下 completion_hints。
    cfg.schema.codetable.single_code_input = true;
    cfg.schema.codetable.single_code_complete = true;
    // 走**特殊模式**路径：它取补全池的引擎序首条（weight 降序），不经跨来源重排，
    // 故高权重用户词必然中选——主路径要与主库后继混排，取到哪条不稳定，断不住。
    // 这也正是用户报告该现象的场景（快符）。
    let ov = overlay_override_dir(
        "completion_hint_command_shows_display_la",
        &[("wubi86", false)],
    );
    bind_special(&mut cfg, "backslash", "wubi86");
    let coord =
        Coordinator::new_headless_with_store_override(cfg, Some(&data_dir()), store, Some(ov));

    let act = coord.handle_key_event(&key_event(0xDC, EVENT_KEY_DOWN));
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        r"\ 应进入特殊模式，实际: {:?}",
        act
    );
    for ch in ['a', 'a', 'b'] {
        press_letter(&coord, ch);
    }
    let texts = coord.debug_all_candidate_texts();
    assert!(
        !texts.is_empty(),
        "精确匹配+空码补全下 aab 应补出更长编码候选（本用例的前提）"
    );
    assert!(
        !texts.iter().any(|t| t.contains("$CC")),
        "补全候选不该显示 $CC 源码（须过 finalize_candidates 汇聚点），实际: {:?}",
        texts
    );
    assert!(
        texts.iter().any(|t| t == "《》"),
        "补全出的命令候选应显示 display 标签「《》」，实际: {:?}",
        texts
    );

    let _ = std::fs::remove_file(&store_path);
}

/// 空码补全的判空必须落在**过滤之后**：短语候选被 shadow 滤光时，应补出引擎备下的
/// 更长编码候选，而不是空屏。
///
/// 构造要点：候选必须在**过滤阶段**才消失。若引擎那一层就没候选，老代码的早判同样
/// 会补全，测不到次序差异。故让短语层出一条候选（引擎层无精确候选、已备好 hint），
/// 再用一条 shadow 规则把这条短语滤掉。
#[test]
fn completion_fills_after_shadow_empties_the_list() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let store_path = std::env::temp_dir().join("wind_completion_after_shadow.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    // 短语占住 aab（引擎层对 aab 无精确候选，故 completion_hints 已备货）。
    store.add_phrase("aab", "短语占位", 0, 100).unwrap();
    // 直接写 shadow 规则把这条短语隐藏——右键删短语走的是「禁用短语」，那会让它在引擎
    // 层就不再产出，重新落回「引擎层已空」，测不到本用例要锁的次序。
    store.delete_shadow("wubi86", "aab", "短语占位").unwrap();

    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.single_code_input = true;
    cfg.schema.codetable.single_code_complete = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);

    for ch in ['a', 'a', 'b'] {
        press_letter(&coord, ch);
    }
    let texts = coord.debug_all_candidate_texts();
    assert!(
        !texts.contains(&"短语占位".to_string()),
        "被 shadow 的短语不该出现，实际: {:?}",
        texts
    );
    assert!(
        !texts.is_empty(),
        "短语被滤光后应补出更长编码候选，而非空屏——判空须在 apply_filter/apply_shadow 之后"
    );

    let _ = std::fs::remove_file(&store_path);
}

// ===== 双拼下的全拼降级输入（schema.pinyin.shuangpin.allow_full_pinyin）=====
//
// ⚠️ 引擎侧已有 `wind-engine` 的 `full_pinyin_*` 一组单测覆盖转换逻辑。下面三条验的是
// **用户入口**：配置到底有没有传到引擎、候选到底有没有出现在协调器的候选列表里。
// 本仓反复出现的故障形态正是「引擎 convert 全绿，用户却打不出」——判据在分派层而非
// 引擎层，故两层都得有测试。

/// 构造双拼协调器，`allow` 控制全拼降级开关。
fn shuangpin_coord(allow: bool) -> Option<std::sync::Arc<Coordinator>> {
    let d = data_dir();
    if !d.join("schemas/shuangpin.schema.toml").exists() {
        eprintln!("跳过：缺少 shuangpin.schema.toml");
        return None;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".into()];
    cfg.schema.active = "shuangpin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.shuangpin.allow_full_pinyin = allow;
    Some(Coordinator::new_headless(cfg, Some(&d)))
}

/// 开启后，双拼方案下按**全拼**打 `nihao`（5 键）应能出「你好」。
///
/// 双拼的正确打法是 4 键 `nihc`；5 键 `nihao` 被双拼解释为 ni|ha|o，与「你好」的词典
/// 边界 ni|hao 不符而遭边界校验拒绝——全拼降级支路补的正是这一条。
#[test]
fn shuangpin_full_pinyin_enabled_recalls_word() {
    let Some(coord) = shuangpin_coord(true) else {
        return;
    };
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    let all = coord.debug_all_candidate_texts();
    assert!(
        all.iter().any(|t| t == "你好"),
        "开启 allow_full_pinyin 后按全拼打 nihao 应出「你好」，实际候选前 10: {:?}",
        &all[..all.len().min(10)]
    );
}

/// 反向对照：关闭时行为与改动前逐字一致。
///
/// **这条不可省**——没有它就无法区分「支路真的生效了」与「双拼本来就出得来这个词」，
/// 上面那条会退化成一个恒真断言。
#[test]
fn shuangpin_full_pinyin_disabled_keeps_old_behavior() {
    let Some(coord) = shuangpin_coord(false) else {
        return;
    };
    for c in "nihao".chars() {
        press_letter(&coord, c);
    }
    let all = coord.debug_all_candidate_texts();
    assert!(
        !all.iter().any(|t| t == "你好"),
        "关闭时 5 键 nihao 不该出「你好」（正确双拼打法是 nihc），实际候选前 10: {:?}",
        &all[..all.len().min(10)]
    );
}

/// 整句：真实词库下全拼降级流也要组得出句子，而不只是查得到词。
///
/// 这是「完整的全拼也能工作」与「勉强查得到几个词」的分界线。
#[test]
fn shuangpin_full_pinyin_composes_sentence() {
    let Some(coord) = shuangpin_coord(true) else {
        return;
    };
    for c in "wojintianhenkaixin".chars() {
        press_letter(&coord, c);
    }
    let all = coord.debug_all_candidate_texts();
    assert!(
        all.iter().any(|t| t == "我今天很开心"),
        "全拼长串应组出整句「我今天很开心」，实际候选前 10: {:?}",
        &all[..all.len().min(10)]
    );
}

/// ★★ 真机回归：`zaijian` 选「再见」必须**吃掉全部 7 键**，且排在同码词之前。
///
/// 首版的三个缺陷在这一条用例里一起现形过，逐条都值得记住：
/// ① 双拼流的**简拼前缀回退**（step 6.2）先用前 4 键 `zaij` 召回了「再见」并标
///    `consumed=4`，本支路想以 `consumed=7`（完整解释）补一条，却被「同文保留先到者」
///    挡掉 ⇒ 用户选中后缓冲里凭空剩下 `ian`。去重规则因此改为「保留解释得更多的那条」；
/// ② 支路的子短语 `zai` 同音单字 20+ 条吃光了 `MAX_FULL_PINYIN_RECALL` 配额 ⇒ ① 精确整词
///    只挤进 4 条、③ 前缀补全一条不剩，「再见」压根没被召回。故 ② 逐级限流；
/// ③ 全拼候选一律沉底 ⇒ 「再见」排到第 8 位。故高置信候选（精确整词 + 消费整串）不沉底。
#[test]
fn shuangpin_full_pinyin_zaijian_commits_whole_buffer() {
    let Some(coord) = shuangpin_coord(true) else {
        return;
    };
    for c in "zaijian".chars() {
        press_letter(&coord, c);
    }
    let page = coord.debug_page_texts();
    let pos = page
        .iter()
        .position(|t| t == "再见")
        .unwrap_or_else(|| panic!("「再见」应在首页，实际: {:?}", page));

    // ③ 高置信全拼候选不该被同码的冷僻组合压住。
    if let Some(p_other) = page.iter().position(|t| t == "在建") {
        assert!(
            pos < p_other,
            "「再见」(w=2837) 应排在同码的「在建」(w=375) 之前，实际: {:?}",
            page
        );
    }

    // ①② 选中后必须消费整个缓冲，不留余码。
    match coord.debug_mouse_select(pos) {
        Some(KeyAction::InsertText {
            text,
            has_new_composition,
            ..
        }) => {
            assert_eq!(text, "再见");
            assert!(
                !has_new_composition,
                "「再见」完整解释了 7 键，选中后不得留余码（曾因让位给 consumed=4 的简拼候选而剩 `ian`）"
            );
        }
        other => panic!(
            "选「再见」应整体上屏 InsertText，实际: {:?}（UpdateComposition 即意味着有余码）",
            other
        ),
    }
    assert_eq!(coord.debug_candidate_count(), 0, "上屏后组合区应清空");
}

/// ★ 编码栏必须**跟随高亮候选**在两种切分间切换，而不是按键时算定一次就不动。
///
/// `zaijian` 有两种读法：双拼读作 `za'ij'ia'n`（4 段），全拼读作 `zai'jian`（2 段）。
/// 高亮在全拼候选（「再见」）上时显示后者，移到双拼候选上时必须切回前者——三段编码配着
/// 两字候选，用户看不懂，退格时更会以为光标错位。
///
/// ⚠️ 首版把形态判定写在引擎里（按首选就地算定 preedit_display），真机现象正是「翻页或
/// 移动光标到双拼候选，编码栏还停在全拼拆分」：引擎每次按键只 convert 一次，而高亮是之后
/// 才移动的。判定必须落在协调器的 `effective_preedit_body`（由 `sync_preedit_to_highlight`
/// 在每次高亮变化时重算）。
#[test]
fn shuangpin_preedit_follows_highlight_between_domains() {
    let Some(coord) = shuangpin_coord(true) else {
        return;
    };
    let mut last = None;
    for c in "zaijian".chars() {
        last = Some(press_letter(&coord, c));
    }
    let preedit0 = action_text(&last.expect("按键应有回执")).unwrap_or_default();
    assert_eq!(
        preedit0,
        "zai'jian",
        "首选是全拼候选「再见」，编码栏应按全拼切分；实际候选: {:?}",
        coord.debug_page_texts()
    );

    // 逐个下移高亮，落到任一**双拼**候选上时，编码栏须切回双拼切分。
    // 不锁定具体位置（词库权重会浮动），只要求这个切换确实发生。
    let mut switched_back = false;
    for _ in 0..24 {
        let act = coord.handle_key_event(&key_event(0x28, EVENT_KEY_DOWN));
        if action_text(&act).as_deref() == Some("za'ij'ia'n") {
            switched_back = true;
            break;
        }
    }
    assert!(
        switched_back,
        "高亮移到双拼候选时编码栏应切回 za'ij'ia'n，实际候选: {:?}",
        coord.debug_all_candidate_texts()
    );
}

/// ★ 双拼下打**简拼**，编码栏须按简拼切（每键一个声母），不能按双拼的两键一音节切。
///
/// `wbwn`（万般无奈）在双拼里一个合法音节都拼不出，`build_raw_preedit` 于是原样回显
/// `wbwn`；`wfwt`（无法无天）更糟——`wf` 恰好是合法双拼音节，切成 `wf'wt`，两段编码配着
/// 四字候选，用户根本看不出自己打的是简拼。两种切法都成立，只能由高亮候选来选。
///
/// ⚠️ 反向对照（下半段）不可省：只断言「简拼时显示 w'f'w't」的话，把 `preedit_display`
/// 无条件改成简拼切法也能全绿，而那样双拼候选的编码栏就废了。
#[test]
fn shuangpin_abbrev_preedit_splits_by_keystroke() {
    let Some(coord) = shuangpin_coord(true) else {
        return;
    };
    let mut last = None;
    for c in "wfwt".chars() {
        last = Some(press_letter(&coord, c));
    }
    let preedit0 = action_text(&last.expect("按键应有回执")).unwrap_or_default();
    assert_eq!(
        preedit0,
        "w'f'w't",
        "首选是简拼候选「无法无天」，编码栏应按简拼击键切分；实际候选: {:?}",
        coord.debug_page_texts()
    );

    // 高亮下移到双拼候选（`wf` 的单字，如「问」）时，编码栏须切回双拼切分。
    // 不锁定具体位置（词库权重会浮动），只要求这个切换确实发生。
    let mut switched_back = false;
    for _ in 0..24 {
        let act = coord.handle_key_event(&key_event(0x28, EVENT_KEY_DOWN));
        if action_text(&act).as_deref() == Some("wf'wt") {
            switched_back = true;
            break;
        }
    }
    assert!(
        switched_back,
        "高亮移到双拼候选时编码栏应切回 wf'wt，实际候选: {:?}",
        coord.debug_all_candidate_texts()
    );
}

/// 双拼下**非**简拼的击键不受影响：`nhao` 的首选是双拼候选，仍按 `nh'ao` 显示。
/// 与上一条构成配对——防止简拼分支把所有双拼击键都抢过去。
#[test]
fn shuangpin_non_abbrev_preedit_unchanged() {
    let Some(coord) = shuangpin_coord(true) else {
        return;
    };
    let mut last = None;
    for c in "nhao".chars() {
        last = Some(press_letter(&coord, c));
    }
    assert_eq!(
        action_text(&last.expect("按键应有回执")).unwrap_or_default(),
        "nh'ao",
        "首选是双拼候选时编码栏应保持双拼切分；实际候选: {:?}",
        coord.debug_page_texts()
    );
}

/// 双拼正路不受支路影响：开着开关，4 键 `nihc` 照样出「你好」。
#[test]
fn shuangpin_full_pinyin_does_not_break_native_path() {
    let Some(coord) = shuangpin_coord(true) else {
        return;
    };
    for c in "nihc".chars() {
        press_letter(&coord, c);
    }
    let all = coord.debug_all_candidate_texts();
    assert!(
        all.iter().any(|t| t == "你好"),
        "双拼正确打法 nihc 应照常出「你好」，实际候选前 10: {:?}",
        &all[..all.len().min(10)]
    );
}

/// issue #64：macOS 的 ⌘（Command）走 `MOD_WIN` 位，未命中热键时必须归宿主。
///
/// 回归前的行为：判据只掩 `MOD_CTRL | MOD_ALT`，于是中文模式下 ⌘+字母一路走到字母臂，
/// 被当成码元累积（⌘C → 组合区出现 "c"）且响应为 `UpdateComposition` = 吃键，
/// 宿主再也收不到这一键 —— 网页版 WPS/GitHub 里表现为复制粘贴失灵。
///
/// 不依赖词库：只断言「键去了哪条分支」，与候选内容无关。
#[test]
fn cmd_combo_goes_to_host_issue64() {
    const MOD_WIN: u32 = 0x0008;
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    let coord = Coordinator::new_headless(cfg, None);

    // 空缓冲：⌘C / ⌘V / ⌘A 一律透传，且**不得**污染输入缓冲。
    for (vk, name) in [(0x43u32, "⌘C"), (0x56, "⌘V"), (0x41, "⌘A")] {
        let act = coord.handle_key_event(&key_event_mods(vk, EVENT_KEY_DOWN, MOD_WIN));
        assert!(
            matches!(act, KeyAction::PassThrough),
            "{name} 应透传给宿主，实际: {act:?}"
        );
    }

    // 组码中：⌘C 清掉组合但**不算输入**（不得再返回 UpdateComposition）。
    // 注：`ClearComposition` 在 macOS 侧不等于吃键，由 BridgeResponseRouter 的
    // hostShortcut 判为「组合已清、按键交还宿主」。
    press_letter(&coord, 'n');
    press_letter(&coord, 'i');
    let act = coord.handle_key_event(&key_event_mods(0x43, EVENT_KEY_DOWN, MOD_WIN));
    assert!(
        matches!(act, KeyAction::ClearComposition),
        "组码中 ⌘C 应清组合而非当码元，实际: {act:?}"
    );

    // Ctrl 组合的既有行为不变（回归保护）。
    let ctrl_c = coord.handle_key_event(&key_event_mods(0x43, EVENT_KEY_DOWN, 0x0002));
    assert!(
        matches!(ctrl_c, KeyAction::PassThrough),
        "空缓冲 Ctrl+C 应照旧透传，实际: {ctrl_c:?}"
    );
}

/// 顶码上屏（`schema.codetable.top_code_commit`）必须与用户**所见的显示首选**同形：
/// 简繁开启时上屏繁体。
///
/// 回归背景：`commit_top_text` 的三条来路曾各自把候选的简体原文直接送去上屏，于是顶码
/// 出简体、空格出繁体（2026-08-20 反馈）。转换已收进 `commit_top_text` 内部。
///
/// 判据是「上屏文本 == 顶码前的显示首选」而非写死汉字——顶出什么取决于码表数据，
/// 写死会在词库调整时变成假红。
#[test]
fn test_s2t_converts_top_code_commit() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.top_code_commit = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    if !coord.debug_set_s2t(true) {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    // 满码 jfuj：显示首选「時間」，内部 text 仍是简体「时间」。
    // 探针须选简繁不同的首选，否则断言恒真、验不出漏转（键名字类全码如 cccc 首选是
    // 「又」，简繁同形，不能用）。
    for c in "jfuj".chars() {
        press_letter(&coord, c);
    }
    let simplified = coord.debug_page_texts()[0].clone();
    let displayed = coord.debug_page_display_texts()[0].clone();
    assert_ne!(
        simplified, displayed,
        "探针失效：jfuj 的首选须简繁不同才验得出漏转（码表数据变动？换一个探针）"
    );
    // 第 5 码触发顶码：顶出前 4 码的显示首选，余码 j 续打。
    // 出厂 top_commit_mode=direct_commit → CommitThenDeferComposition；pre_confirm → InsertText。
    let commit = match press_letter(&coord, 'j') {
        KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text,
        KeyAction::InsertText { text, .. } => text,
        other => panic!("jfuj + j 应触发顶码上屏，实际: {:?}", other),
    };
    assert_eq!(
        commit, displayed,
        "顶码上屏须与显示首选同形（繁体），不得回落简体"
    );
}

/// 顶屏进模式（临拼 / mix / 特殊模式 / 临英四处共用 `take_committed_with_highlight`）
/// 的上屏文本同样须过简繁转换。
///
/// 回归背景：这四处曾各自复制同一段「take_committed + 高亮候选」拼接代码，四份**全部**
/// 漏掉简繁转换（2026-08-20 反馈）。此处以临拼为代表锁住收口后的行为。
#[test]
fn test_s2t_converts_commit_and_enter_mode() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
    if !coord.debug_set_s2t(true) {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    // cc → 显示首选「雙」，内部 text 仍是简体「双」。
    press_letter(&coord, 'c');
    press_letter(&coord, 'c');
    let simplified = coord.debug_page_texts()[0].clone();
    let displayed = coord.debug_page_display_texts()[0].clone();
    assert_ne!(
        simplified, displayed,
        "探针失效：cc 的首选须简繁不同才验得出漏转（码表数据变动？换一个探针）"
    );
    // 反引号：顶屏高亮候选 + 进入临时拼音（与顶码同一 top_commit_mode 分流）。
    match coord.handle_key_event(&key_event(0xC0, EVENT_KEY_DOWN)) {
        KeyAction::CommitThenDeferComposition { commit_text, .. } => {
            assert_eq!(
                commit_text, displayed,
                "顶屏进模式须上屏显示首选（繁体），不得回落简体"
            );
        }
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, displayed, "顶屏进模式须上屏显示首选（繁体）");
        }
        other => panic!("有候选按反引号应顶屏 + 进临时拼音，实际: {:?}", other),
    }
}

/// 真机回归其二：**顶码切点必须按短语码长走**，而非固定的方案满码长。
///
/// `zzsfz` 是 5 码短语，方案满码长 4。敲到 `zzsfza` 时引擎把 prefix 切在 `zzsf`，与顶码前
/// 的缓冲 `zzsfz` 对不上 → 落进「多级溢出」分支 → `zzsf` 在 wubi86 码表无字 → 放弃顶码。
/// 用户看到的是「进了空码状态，而不是顶码」。4 码短语正常，正因为两种切法恰好重合。
#[test]
fn top_code_splits_at_phrase_code_length() {
    if !has_schemas() {
        return;
    }
    let store_path = std::env::temp_dir().join("wind_phrase_topcode_split.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase("zzsfz", "词条内容", 0, 100).unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.top_code_commit = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['z', 'z', 's', 'f', 'z'] {
        press_letter(&coord, ch);
    }
    // 第 6 键 'a'：`zzsfz` 已溢出（短语侧既非精确码也无更长后继）→ 顶该短语 + 余码 a。
    match press_letter(&coord, 'a') {
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            ..
        } => {
            assert_eq!(commit_text, "词条内容", "应顶出 5 码短语本身");
            assert_eq!(deferred_composition, "a", "余码应是溢出的那一个字符");
        }
        other => panic!("超长短语溢出应顶码，实际: {:?}(切点切错→进空码?)", other),
    }
    let _ = std::fs::remove_file(&store_path);
}

/// 真机回归其三：**短语候选的 `code` 恒为空串**，前缀命中与精确命中在候选上无从分辨——
/// 顶码必须回头问短语层，否则会把「还没打完的短语」提前兑现。
///
/// `zzsfz` 敲到 `zzsf` 时就以 `is_phrase=true, is_prefix=false` 排在候选首位（普通字面短语的
/// 前缀命中**不打 `is_prefix` 标记**，那个标记只给 `$SS`/`$AA` 组导航用）。于是再敲一个 `a`
/// ——`zzsfa` 这条码短语里根本没有——旧行为顶出了 `zzsfz` 的内容。正确行为是落进空码。
///
/// 构造关键：`zzsf` 在 wubi86 码表**无字**，否则码表候选会占住首位、由正常码表顶码兜底，
/// 这条缺陷就被掩盖（`kkkk` 一类有字的前缀就测不出来）。
#[test]
fn top_code_rejects_prefix_only_phrase_hit() {
    if !has_schemas() {
        return;
    }
    let store_path = std::env::temp_dir().join("wind_phrase_topcode_prefixhit.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase("zzsfz", "词条内容", 0, 100).unwrap();
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.top_code_commit = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['z', 'z', 's', 'f'] {
        press_letter(&coord, ch);
    }
    assert_eq!(
        coord.debug_all_candidate_texts(),
        vec!["词条内容".to_string()],
        "构造前提：zzsf 处唯一候选是 zzsfz 的前缀命中"
    );
    // 第 5 键 'a'：`zzsf` 不是精确短语码 → 那条候选只是前缀命中，不得顶码上屏。
    match press_letter(&coord, 'a') {
        KeyAction::UpdateComposition { text, .. } => {
            assert_eq!(text, "zzsfa", "码应原样留在编码栏");
        }
        other => panic!("不匹配的码应进空码状态，实际: {:?}(误顶前缀命中?)", other),
    }
    assert!(
        coord.debug_all_candidate_texts().is_empty(),
        "zzsfa 无任何匹配，应是空码状态，实际: {:?}",
        coord.debug_all_candidate_texts()
    );
    let _ = std::fs::remove_file(&store_path);
}

/// 混输（`wubi86_pinyin`）下的超长短语顶码 —— 与纯码表同结论。
///
/// 补这条是因为真机方案是混输，而上面三条回归全用纯 `wubi86`：混输的 `handle_top_code`
/// 在委托 primary 之前另有一整条拼音/英文否决链（`pinyin_only_overflow` 等），走的不是
/// 同一段代码。**调查这个 bug 时我一度以为混输下没修好**，实际是当时的探针漏了
/// `top_code_commit`——`Config::default()` 里它是 `false`，而出厂 toml 是 `true`。
///
/// ⚠️ 任何顶码测试都必须显式打开 `top_code_commit`，否则测的是一个关着的功能，
/// 无论代码对错都「不顶码」。
#[test]
fn top_code_overlong_phrase_in_mixed_schema() {
    if !has_schemas() {
        return;
    }
    let store_path = std::env::temp_dir().join("wind_phrase_topcode_mixed.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase("zzsfz", "TEST", 1800, 0).unwrap();
    let mut cfg = config_mixed();
    cfg.schema.codetable.top_code_commit = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for ch in ['z', 'z', 's', 'f'] {
        press_letter(&coord, ch);
    }
    // 打满 5 码：短语精确命中，停在候选态（不得被顶码劫走）。
    match press_letter(&coord, 'z') {
        KeyAction::UpdateComposition { text, .. } => assert_eq!(text, "zzsfz"),
        other => panic!("混输下 5 码短语不该被顶码劫走，实际: {:?}", other),
    }
    // 第 6 键溢出：以短语码为切点顶码 + 余码。
    match press_letter(&coord, 'a') {
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            ..
        } => {
            assert_eq!(commit_text, "TEST");
            assert_eq!(deferred_composition, "a");
        }
        other => panic!("混输下超长短语溢出应顶码，实际: {:?}", other),
    }
    let _ = std::fs::remove_file(&store_path);
}

// ───── punct_on_empty_behavior：空码时标点键是否丢弃废码 ─────
//
// 「空码」= 打了码但一个候选都没有（多为码表打错字根）。既有行为是把废码连同标点一起
// 送上屏；`clear` 让废码作废、标点照常出。
//
// 标点有**两个彼此独立的上屏出口**，各写一对测试：普通标点，以及智能符号
// `hold_composition` 的 `CommitAndHoldComposition`。只接一个的后果是「开了智能符号的
// 宿主上开关不生效」——这种间歇性不一致没有对照测试根本发现不了。
//
// ⚠️ 每个测试都必须先断言「候选确实为空」：`bbqq` 若哪天进了词库，测试会退化成在验
// 「有候选时按标点顶屏首选」，那条路与本开关无关，断言却照样能过。
//
// ⚠️ 必须显式开 `punct_commit`：结构体零值是 `false`（出厂 toml 是 `true`），关着时标点
// 分支在 `has_input` 守卫处就 `return Consumed` 吞键，压根走不到被测代码。

/// 构造 wubi86 + 标点顶屏已开的配置。`clear` 决定空码时是否丢弃废码。
fn config_punct_on_empty(clear: bool) -> Config {
    config_punct_on_empty_value(if clear { "clear" } else { "commit" })
}

/// 同上，但直取三态之一（`commit` / `clear` / `clear_no_input`）。
fn config_punct_on_empty_value(value: &str) -> Config {
    let mut cfg = config_with("wubi86");
    // 出厂即开，测试须显式打开——零值 false 会让标点在 has_input 分支被吞掉。
    cfg.schema.codetable.punct_commit = true;
    // ⚠️ 每个分支都**显式写**，不靠「不设＝commit」：出厂默认已经是 clear，靠缺省表达
    // commit 的话，对照组会跟着默认值漂移——那正是它要防的东西。出厂默认本身由
    // `punct_on_empty_default_discards_without_any_config` 单独守。
    cfg.input.punct_on_empty_behavior = value.into();
    cfg
}

/// 打出一串空码，返回协调器。先验候选确实为空，避免测试悄悄换了被测路径。
fn type_empty_code(coord: &Coordinator) {
    for c in "bbqq".chars() {
        press_letter(coord, c);
    }
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提失守：bbqq 应当无候选（空码），实际: {:?}",
        coord.debug_page_texts()
    );
}

/// clear：空码时按句号，废码不上屏，只出中文句号。
#[test]
fn test_punct_on_empty_clear_discards_raw_code() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_punct_on_empty(true), Some(&data_dir()));
    type_empty_code(&coord);
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "。", "clear 应丢弃废码 bbqq，只上屏句号");
        }
        other => panic!("空码按句号应上屏标点，实际: {:?}", other),
    }
}

/// 对照组：默认 commit 下同样操作仍把废码顶上屏。
///
/// 没有它，上面那条测试无法区分「配置生效」与「这条路本来就不上屏原码」。
#[test]
fn test_punct_on_empty_commit_still_outputs_raw_code() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_punct_on_empty(false), Some(&data_dir()));
    type_empty_code(&coord);
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "bbqq。", "commit（默认）应把废码连同句号一起上屏");
        }
        other => panic!("空码按句号应上屏原码+标点，实际: {:?}", other),
    }
}

/// clear：智能符号 `hold_composition` 出口同样丢弃废码，符号仍进 held。
#[test]
fn test_punct_on_empty_clear_discards_in_hold_composition() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_punct_on_empty(true);
    cfg.input.symbol.smart_mode = true;
    cfg.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    type_empty_code(&coord);
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::CommitAndHoldComposition {
            commit_text,
            hold_text,
            ..
        } => {
            assert_eq!(commit_text, "", "clear 应丢弃废码，commit_text 为空");
            assert_eq!(hold_text, "。", "符号本身仍照常进 held");
        }
        other => panic!(
            "智能符号 hold 下空码按句号应走 CommitAndHold，实际: {:?}",
            other
        ),
    }
}

/// 对照组：智能符号 hold 出口在默认 commit 下仍顶废码上屏。
#[test]
fn test_punct_on_empty_commit_outputs_raw_code_in_hold_composition() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_punct_on_empty(false);
    cfg.input.symbol.smart_mode = true;
    cfg.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    type_empty_code(&coord);
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::CommitAndHoldComposition {
            commit_text,
            hold_text,
            ..
        } => {
            assert_eq!(commit_text, "bbqq", "commit（默认）应把废码顶上屏");
            assert_eq!(hold_text, "。", "符号本身进 held");
        }
        other => panic!(
            "智能符号 hold 下空码按句号应走 CommitAndHold，实际: {:?}",
            other
        ),
    }
}

/// 行为层守门：**一个相关开关都不设**，只靠出厂默认，空码按标点就应丢弃废码。
///
/// 上面那一族测试全都显式写了 `punct_on_empty_behavior`，所以把出厂默认改回 `commit`
/// 它们一条都不会红——本仓实测过「只翻一个默认值，全量两千多条无一失败」。这条补的正是
/// 那个缺口：它不写该键，读到什么就是真实用户装完就用的行为。
///
/// ⚠️ `punct_commit` 仍要显式对齐 L2（结构体零值 false、出厂 toml true）。不对齐的话标点
/// 在 `has_input` 守卫处就被吞掉，断言会以「没上屏废码」的形态通过——测的却是一个标点功能
/// 整个关闭的系统，是彻底的假绿。
#[test]
fn punct_on_empty_default_discards_without_any_config() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_with("wubi86");
    cfg.schema.codetable.punct_commit = true; // 对齐 L2，非本测试的被测项
    assert_eq!(
        cfg.input.punct_on_empty_behavior, "clear",
        "出厂默认应为 clear；这行读的就是 L1，改它等于改产品决策"
    );
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    type_empty_code(&coord);
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "。", "装完就用的默认行为：废码丢弃，只上屏句号");
        }
        other => panic!("空码按句号应上屏标点，实际: {:?}", other),
    }
}

/// 反向对照：同一条路径下，回车与空格的出厂默认**仍然上屏原码**。
///
/// 这组不一致是产品决策，不是漏改。没有这条对照，将来有人「顺手把三个统一成 clear」时，
/// 上面那条测试照样绿——它只看标点。
#[test]
fn enter_and_space_on_empty_still_commit_by_default() {
    if !has_schemas() {
        return;
    }
    for (vk, name) in [(0x0D_u32, "回车"), (0x20, "空格")] {
        let coord = Coordinator::new_headless(config_with("wubi86"), Some(&data_dir()));
        type_empty_code(&coord);
        match coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN)) {
            KeyAction::InsertText { text, .. } => {
                assert!(
                    text.starts_with("bbqq"),
                    "{name}的出厂默认仍应上屏原码（保住「我就要这串原码」的出口），实际: {text:?}"
                );
            }
            other => panic!("{name}空码应上屏原码，实际: {other:?}"),
        }
    }
}

// ───── clear_no_input：废码丢弃，标点本身也不上屏 ─────
//
// ★ 这一族配置的行为其实是**两根轴**：「废码上不上屏」与「按键字符本身出不出」。
// `clear` 是「丢废码、出标点」，`clear_no_input` 是「丢废码、也不出标点」——后者与回车/
// 空格的 `clear` 才是同一格（那两个键的 clear 本就返回 `ClearComposition`，键本身不产出）。
//
// ⚠️ 每条都必须断言**具体的 KeyAction 变体**，不能只看「文本为空」：`InsertText("")` 与
// `ClearComposition` 在「屏幕上没多出字」这个层面无法区分，而前者是错的——它会把一次空
// 提交推给宿主，组合态的收尾时机随宿主而异。

/// clear_no_input：普通标点出口——废码丢弃，句号也不上屏，整个按键当没按过。
#[test]
fn test_punct_on_empty_clear_no_input_swallows_punct() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        config_punct_on_empty_value("clear_no_input"),
        Some(&data_dir()),
    );
    type_empty_code(&coord);
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!("clear_no_input 应收组合且不产出任何字符，实际: {:?}", other),
    }
}

/// clear_no_input：智能符号 hold 出口——**不得**走 CommitAndHoldComposition。
///
/// ★ 这条是本族最容易漏的一条。上一版的 `clear` 只把 `commit_text` 置空却仍走 hold 分支，
/// 照搬到 `clear_no_input` 就会挂一个屏幕上并不存在的 hold 态：下一次按同键的 press2 会去
/// 删一个从未上屏的符号，表现为「开了智能符号后，丢废码会顺手吃掉前面一个字」。
#[test]
fn test_punct_on_empty_clear_no_input_skips_hold_composition() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_punct_on_empty_value("clear_no_input");
    cfg.input.symbol.smart_mode = true;
    cfg.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    type_empty_code(&coord);
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::ClearComposition => {}
        other => panic!(
            "标点不上屏就没有可 hold 的对象，不该走 CommitAndHold，实际: {:?}",
            other
        ),
    }
}

/// clear_no_input 同样只管空码那一支：有候选时仍顶屏首选 + 标点。
///
/// 没有这条，把短路点提到 `has_input` 守卫之上（一个很自然的「简化」）不会红任何测试，
/// 而那会让**所有**按标点顶屏的场景都变成吞键。
#[test]
fn test_punct_on_empty_clear_no_input_does_not_affect_nonempty_candidates() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(
        config_punct_on_empty_value("clear_no_input"),
        Some(&data_dir()),
    );
    for c in "ffff".chars() {
        press_letter(&coord, c);
    }
    let first = coord
        .debug_page_texts()
        .first()
        .cloned()
        .expect("前提失守：ffff 应当有候选");
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(
                text,
                format!("{first}。"),
                "有候选时 clear_no_input 不得改变「按标点顶屏首选」的既有语义"
            );
        }
        other => panic!("有候选按句号应顶屏首选+标点，实际: {:?}", other),
    }
}

/// ★★ 第三条通路：以词定字（`select_char_keys`）会在标点臂**之前**劫走这几个键。
///
/// 空码时 `handle_select_char` 拿不到字源，若仍被拦截就会退到 `keys.overflow.select_char_key`
/// （出厂 `ignore` ＝吞键并**保留**编码），`punct_on_empty_behavior` 整个够不着。修法是空码时
/// 一律放行——那不是「以词定字越界」，而是「此刻这个键不该算以词定字键」。
///
/// ★★★ **三档必须一起断言**。曾经只放行了非 `commit` 两档（拿标点策略的取值当以词定字的
/// 判据），`commit` 档漏网：开了以词定字得到 `Consumed`（吞键、废码留着），没开得到
/// `bbqq。`。只测 `clear` 的话那个漏洞完全照不到——**这条测试当初就是这么漏过去的**。
///
/// 每档的期望值都取「没开以词定字时该键本来的行为」，即本族其余测试已经钉死的那个结果：
/// 放行是否正确，判据就是「开不开以词定字，结果一模一样」。
#[test]
fn test_punct_on_empty_reaches_select_char_keys() {
    if !has_schemas() {
        return;
    }
    for (tier, expect) in [
        // commit：废码 + 标点一起上屏（与 test_punct_on_empty_commit_still_outputs_raw_code 同）
        ("commit", Some("bbqq。")),
        // clear：丢废码，只出标点（与 test_punct_on_empty_clear_discards_raw_code 同）
        ("clear", Some("。")),
        // clear_no_input：什么都不出，收组合（与 test_punct_on_empty_clear_no_input_swallows_punct 同）
        ("clear_no_input", None),
    ] {
        let mut cfg = config_punct_on_empty_value(tier);
        cfg.keys.select_char_keys = vec!["comma_period".into()];
        let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
        type_empty_code(&coord);
        let act = coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN));
        match (expect, &act) {
            (Some(want), KeyAction::InsertText { text, .. }) => assert_eq!(
                text, want,
                "{tier} 档：开了以词定字后行为须与没开时一致，实际: {text:?}"
            ),
            (None, KeyAction::ClearComposition) => {}
            _ => panic!("{tier} 档：空码 + 以词定字应放行到标点臂，期望 {expect:?}，实际: {act:?}"),
        }
    }
}

/// 反向夹逼：**有候选**时以词定字照常生效，放行判据不得把它一起放跑。
///
/// 与上一条成对。只有上一条时，把拦截条件整个删掉也能过。
#[test]
fn test_select_char_still_works_with_punct_on_empty_clear() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config_punct_on_empty_value("clear");
    cfg.keys.select_char_keys = vec!["comma_period".into()];
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "ffff".chars() {
        press_letter(&coord, c);
    }
    let Some(first) = coord.debug_page_texts().first().cloned() else {
        return;
    };
    let Some(first_char) = first.chars().next() else {
        return;
    };
    match coord.handle_key_event(&key_event(0xBC, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(
                text,
                first_char.to_string(),
                "有候选时 `,` 仍应以词定字取第 1 字，而不是被当成普通标点"
            );
        }
        other => panic!("有候选时 `,` 应以词定字，实际: {:?}", other),
    }
}

/// 出厂零回归：`clear_no_input` 不得成为默认。
///
/// 出厂仍是 `clear`（标点照常上屏）——`clear_no_input` 是给「拿标点当取消键」的人准备的，
/// 让它变成默认等于让所有人的标点在空码时凭空消失。
#[test]
fn punct_on_empty_clear_no_input_is_not_the_default() {
    let cfg = Config::default();
    assert_eq!(
        cfg.input.punct_on_empty_behavior, "clear",
        "出厂须为 clear；clear_no_input 只能是用户显式选择"
    );
}

/// 有候选时按标点仍顶屏首选——`clear` 只管空码那一支，不得误伤正常顶屏。
#[test]
fn test_punct_on_empty_clear_does_not_affect_nonempty_candidates() {
    if !has_schemas() {
        return;
    }
    let coord = Coordinator::new_headless(config_punct_on_empty(true), Some(&data_dir()));
    for c in "ffff".chars() {
        press_letter(&coord, c);
    }
    let first = coord
        .debug_page_texts()
        .first()
        .cloned()
        .expect("前提失守：ffff 应当有候选");
    match coord.handle_key_event(&key_event(0xBE, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(
                text,
                format!("{first}。"),
                "有候选时 clear 不得改变「按标点顶屏首选」的既有语义"
            );
        }
        other => panic!("有候选按句号应顶屏首选+标点，实际: {:?}", other),
    }
}
