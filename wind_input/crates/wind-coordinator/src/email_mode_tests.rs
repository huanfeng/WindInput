//! 邮箱模式与两模式共用补全候选源的 crate 内行为测试（白盒，零 RPC、零词库）。
//!
//! 刻意用 `new_headless_with_store`（不传 data_dir）而不是集成测试里的
//! `new_headless(cfg, Some(&data_dir()))`：后者在没有 `build_dev/data` 的 worktree 里
//! 会走 `has_schemas()` 的跳过分支，**整族静默变绿**（AGENTS.md 那条「0.0x 秒全绿＝假绿」）。
//! 邮箱模式不依赖任何词库——它的候选来自预置后缀表与学习数据——所以没有理由把测试
//! 绑在词库上。

use std::sync::Arc;

use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;
use wind_store::completion::CompletionKind;

use crate::coordinator::Coordinator;

/// 带临时 store 的无头 Coordinator。`cfg` 由调用方调好开关。
fn coord_with(tag: &str, cfg: Config) -> (Arc<Coordinator>, Arc<Store>) {
    let path = std::env::temp_dir().join(format!(
        "wind_email_mode_{}_{}_{:?}.redb",
        std::process::id(),
        tag,
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let c = Coordinator::new_headless_with_store(cfg, None, Arc::clone(&store));
    (c, store)
}

/// 开了邮箱模式的默认配置。
fn email_cfg() -> Config {
    let mut c = Config::default();
    c.input.email.enabled = true;
    c
}

fn key(key_code: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn press_letter(c: &Coordinator, ch: char) -> KeyAction {
    c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0))
}

fn press_digit(c: &Coordinator, ch: char) -> KeyAction {
    c.handle_key_event(&key(ch as u32, 0))
}

/// `@` = Shift+2。C++ 侧把它归 Punctuation，中文模式无条件吃键，故这一帧一定到得了 Rust。
///
/// 从 `VK_0` 派生而不是写 `0x32`：keymap 只导出区间端点（`VK_0`/`VK_9`），而 AGENTS.md
/// 那条「禁止裸十六进制」管的是**从哪里得到这个数**，不是算不算术。
fn press_at(c: &Coordinator) -> KeyAction {
    c.handle_key_event(&key(wind_keys::keymap::VK_0 + 2, MOD_SHIFT))
}

fn press(c: &Coordinator, vk: u32) -> KeyAction {
    c.handle_key_event(&key(vk, 0))
}

fn composition(action: &KeyAction) -> Option<String> {
    match action {
        KeyAction::UpdateComposition { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn committed(action: &KeyAction) -> Option<String> {
    match action {
        KeyAction::InsertText { text, .. } => Some(text.clone()),
        _ => None,
    }
}

// ───────────────────────── 触发判据 ─────────────────────────

#[test]
fn at_sign_hijacks_only_when_buffer_is_not_empty() {
    let (c, _s) = coord_with("enter", email_cfg());
    press_letter(&c, 'a');
    press_letter(&c, 'b');
    let act = press_at(&c);
    assert_eq!(
        composition(&act).as_deref(),
        Some("ab@"),
        "缓冲非空时按 @ 应夺取进入邮箱模式，组合区带上用户名与 @，实际 {act:?}"
    );
    assert_eq!(c.debug_active_mode(), Some("email"));
}

#[test]
fn at_sign_on_empty_buffer_does_not_enter() {
    let (c, _s) = coord_with("empty", email_cfg());
    let act = press_at(&c);
    assert_ne!(
        c.debug_active_mode(),
        Some("email"),
        "空缓冲按 @ 不该进邮箱模式（否则用户再也没法单独打一个 @），实际 {act:?}"
    );
}

#[test]
fn disabled_email_mode_never_hijacks() {
    // 出厂态：邮箱模式关闭 ⇒ @ 照常走标点流水线，一切与加这个功能之前相同。
    let (c, _s) = coord_with("off", Config::default());
    press_letter(&c, 'a');
    press_at(&c);
    assert_eq!(c.debug_active_mode(), None, "关闭时不该进任何 overlay 模式");
}

// ───────────────────────── 缓冲与光标 ─────────────────────────

#[test]
fn suffix_accumulates_including_digits_and_dots() {
    let (c, _s) = coord_with("accum", email_cfg());
    press_letter(&c, 'a');
    press_at(&c);
    press_letter(&c, 'q');
    press_letter(&c, 'q');
    // `.` 与数字都是合法邮箱字符，必须入缓冲而不是被当成翻页/选词键。
    let act = press(&c, wind_keys::keymap::VK_PERIOD);
    assert_eq!(composition(&act).as_deref(), Some("a@qq."));
    press_letter(&c, 'c');
    let act = press_digit(&c, '1');
    assert_eq!(
        composition(&act).as_deref(),
        Some("a@qq.c1"),
        "数字键在邮箱模式里是字符而非序号选词 —— 否则打不出 163.com 这类后缀"
    );
}

#[test]
fn backspace_at_the_hijack_boundary_rewinds_to_normal_input() {
    let (c, _s) = coord_with("rewind", email_cfg());
    press_letter(&c, 'a');
    press_letter(&c, 'b');
    press_at(&c); // 缓冲 "ab@"，这就是夺取边界
    press_letter(&c, 'q'); // "ab@q"

    // 第一次退格：删掉 q，退回边界，仍在邮箱模式内。
    let act = press(&c, wind_keys::keymap::VK_BACK);
    assert_eq!(composition(&act).as_deref(), Some("ab@"));
    assert_eq!(c.debug_active_mode(), Some("email"));

    // 第二次退格：已在边界 ⇒ 撤销夺取，把 "ab" 放回正常码表输入流。
    let act = press(&c, wind_keys::keymap::VK_BACK);
    assert_eq!(
        c.debug_active_mode(),
        None,
        "退到夺取边界再按退格应回退出模式，实际 {act:?}"
    );
}

#[test]
fn escape_abandons_without_committing() {
    let (c, _s) = coord_with("esc", email_cfg());
    press_letter(&c, 'a');
    press_at(&c);
    press_letter(&c, 'q');
    let act = press(&c, wind_keys::keymap::VK_ESCAPE);
    assert_eq!(committed(&act), None, "Esc 不该上屏任何东西");
    assert_eq!(c.debug_active_mode(), None, "Esc 应退出邮箱模式");
}

// ───────────────────────── 候选与上屏 ─────────────────────────

#[test]
fn space_commits_the_highlighted_completion() {
    let (c, _s) = coord_with("commit", email_cfg());
    press_letter(&c, 'a');
    press_at(&c);
    // 预置表以 qq.com 打头 ⇒ 首选就是 a@qq.com。
    let act = press(&c, wind_keys::keymap::VK_SPACE);
    assert_eq!(
        committed(&act).as_deref(),
        Some("a@qq.com"),
        "有候选时空格应上屏高亮候选，实际 {act:?}"
    );
    assert_eq!(c.debug_active_mode(), None, "上屏后应退出模式");
}

#[test]
fn typed_suffix_filters_the_candidates() {
    let (c, _s) = coord_with("filter", email_cfg());
    press_letter(&c, 'a');
    press_at(&c);
    press_letter(&c, 'g');
    let act = press(&c, wind_keys::keymap::VK_SPACE);
    assert_eq!(
        committed(&act).as_deref(),
        Some("a@gmail.com"),
        "打了 g 之后候选只剩 g 开头的后缀，实际 {act:?}"
    );
}

#[test]
fn unmatched_suffix_commits_the_raw_buffer() {
    let (c, _s) = coord_with("raw", email_cfg());
    press_letter(&c, 'a');
    press_at(&c);
    // 预置表里没有 z 开头的后缀 ⇒ 无候选 ⇒ 空格上屏缓冲原文（自定义域名走这条路）。
    for ch in ['z', 'z', 'z'] {
        press_letter(&c, ch);
    }
    let act = press(&c, wind_keys::keymap::VK_SPACE);
    assert_eq!(
        committed(&act).as_deref(),
        Some("a@zzz"),
        "无候选时空格上屏缓冲原文，实际 {act:?}"
    );
}

#[test]
fn empty_suffix_list_still_lets_the_user_type_freely() {
    // 用户在设置里把预置后缀删干净 ⇒ 没有候选，但模式照常可用。
    let mut cfg = email_cfg();
    cfg.input.email.suffixes = vec![];
    let (c, _s) = coord_with("nosuffix", cfg);
    press_letter(&c, 'a');
    press_at(&c);
    for ch in ['q', 'q'] {
        press_letter(&c, ch);
    }
    let act = press(&c, wind_keys::keymap::VK_SPACE);
    assert_eq!(committed(&act).as_deref(), Some("a@qq"));
}

// ───────────────────────── 学习 ─────────────────────────

#[test]
fn committing_learns_the_suffix_without_the_username() {
    let (c, store) = coord_with("learn", email_cfg());
    press_letter(&c, 'a');
    press_at(&c);
    press(&c, wind_keys::keymap::VK_SPACE); // 上屏 a@qq.com

    let rec = store
        .get_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap()
        .expect("上屏后应学下后缀");
    assert_eq!(rec.count, 1);
    // 学的是域名，不含用户名 —— 这正是邮箱学习不需要第二道隐私开关的理由。
    assert_eq!(
        store
            .list_completions(CompletionKind::EmailSuffix, "", 0, 0)
            .unwrap()
            .1,
        1,
        "只该学下一条（后缀），不该把整个邮箱地址也记一份"
    );
}

#[test]
fn a_learned_suffix_outranks_the_preset_table() {
    let (c, store) = coord_with("rank", email_cfg());
    // 手工喂两次「公司域名」，模拟用户用过。
    store
        .record_completion(CompletionKind::EmailSuffix, "mycorp.cn")
        .unwrap();
    store
        .record_completion(CompletionKind::EmailSuffix, "mycorp.cn")
        .unwrap();

    press_letter(&c, 'a');
    press_at(&c);
    let act = press(&c, wind_keys::keymap::VK_SPACE);
    assert_eq!(
        committed(&act).as_deref(),
        Some("a@mycorp.cn"),
        "学过的后缀应排到预置表之前 —— 这是自动学习的全部意义，实际 {act:?}"
    );
}

#[test]
fn a_suffix_present_in_both_sources_appears_once() {
    let (c, store) = coord_with("dedup", email_cfg());
    store
        .record_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap();
    press_letter(&c, 'a');
    press_at(&c);
    let hits = c
        .debug_page_texts()
        .into_iter()
        .filter(|t| t == "a@qq.com")
        .count();
    assert_eq!(
        hits, 1,
        "qq.com 既在预置表又已学过，候选里只该出现一次（否则用户怎么用都消不掉其中一条）"
    );
}

#[test]
fn learning_stops_when_the_mode_is_switched_off_mid_session() {
    // 闸门在写入侧也要有：开关是运行时可改的，不能只在进入模式时判一次。
    let (c, store) = coord_with("offlearn", email_cfg());
    press_letter(&c, 'a');
    press_at(&c);
    c.refresh_config_in_memory(|cfg| cfg.input.email.enabled = false);
    press(&c, wind_keys::keymap::VK_SPACE);
    assert_eq!(
        store
            .list_completions(CompletionKind::EmailSuffix, "", 0, 0)
            .unwrap()
            .1,
        0,
        "关掉邮箱模式后不该再学新后缀"
    );
}

// ───────────────────────── 切分工具 ─────────────────────────

#[test]
fn split_helpers_use_the_last_at_sign() {
    assert_eq!(Coordinator::email_user_part("abc@qq.com"), "abc");
    assert_eq!(Coordinator::email_suffix_part("abc@qq.com"), "qq.com");
    assert_eq!(Coordinator::email_suffix_part("abc@"), "");
    // 手滑打出第二个 @ 时按最后一个切，才能让用户把后半截打完；按第一个切会拿
    // "b@" 去查后缀，一条也匹配不上。
    assert_eq!(Coordinator::email_user_part("a@b@"), "a@b");
    assert_eq!(Coordinator::email_suffix_part("a@b@qq"), "qq");
    // 没有 @ 时整串是用户名，后缀为空（缓冲被退格删到只剩用户名的中间态）。
    assert_eq!(Coordinator::email_user_part("abc"), "abc");
    assert_eq!(Coordinator::email_suffix_part("abc"), "");
}

// ═════════════════════ 网址模式：历史补全（§5.1） ═════════════════════
//
// 与邮箱共用 `mode_completion.rs` 的候选源与上屏语义，故并在同一个文件里测——两者
// 分叉正是那份共用代码要防的事，测试分家等于把对照关系也拆了。

/// 开了网址模式的配置。`history` 控制第二道开关。
fn url_cfg(history: bool) -> Config {
    let mut c = Config::default();
    c.input.url.enabled = true;
    c.input.url.prefixes = vec!["www.".into()];
    c.input.url.history_enabled = history;
    c
}

/// 打满 `www.` 进入网址模式（`.` 是补满前缀的那一键）。
fn enter_url(c: &Coordinator) -> KeyAction {
    press_letter(c, 'w');
    press_letter(c, 'w');
    press_letter(c, 'w');
    press(c, wind_keys::keymap::VK_PERIOD)
}

#[test]
fn url_history_off_keeps_the_old_behaviour_exactly() {
    // 出厂态：历史关着 ⇒ 恒无候选 ⇒ 空格上屏缓冲原文，与加补全之前逐字相同。
    // 这是「不惊动既有用户」那半条承诺的守护测试。
    let (c, store) = coord_with("url_off", url_cfg(false));
    // 库里先埋一条历史，证明关着时**连查都不查**，而不是查出来再丢掉。
    store
        .record_completion(CompletionKind::UrlHistory, "www.example.com")
        .unwrap();

    let act = enter_url(&c);
    assert_eq!(composition(&act).as_deref(), Some("www."));
    assert!(c.debug_page_texts().is_empty(), "历史关闭时不该有候选");

    press_letter(&c, 'e');
    let act = press(&c, wind_keys::keymap::VK_SPACE);
    assert_eq!(
        committed(&act).as_deref(),
        Some("www.e"),
        "无候选时空格上屏缓冲原文，实际 {act:?}"
    );
    assert_eq!(
        store
            .list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap()
            .1,
        1,
        "历史关闭时上屏不该新增记录（库里只剩预先埋的那条）"
    );
}

#[test]
fn url_history_on_completes_from_the_past() {
    let (c, store) = coord_with("url_on", url_cfg(true));
    store
        .record_completion(CompletionKind::UrlHistory, "www.example.com")
        .unwrap();

    enter_url(&c);
    press_letter(&c, 'e');
    assert_eq!(
        c.debug_page_texts(),
        vec!["www.example.com".to_string()],
        "打过的网址应作为补全候选出现"
    );
    let act = press(&c, wind_keys::keymap::VK_SPACE);
    assert_eq!(
        committed(&act).as_deref(),
        Some("www.example.com"),
        "有候选时空格上屏高亮候选，实际 {act:?}"
    );
}

#[test]
fn url_commit_records_history_when_enabled() {
    let (c, store) = coord_with("url_rec", url_cfg(true));
    enter_url(&c);
    for ch in ['a', 'b'] {
        press_letter(&c, ch);
    }
    press(&c, wind_keys::keymap::VK_SPACE); // 上屏 www.ab

    let rec = store
        .get_completion(CompletionKind::UrlHistory, "www.ab")
        .unwrap()
        .expect("开着历史时上屏应记一条");
    assert_eq!(rec.count, 1);
}

#[test]
fn a_candidate_identical_to_the_buffer_is_not_offered() {
    // 与缓冲逐字相同的那条不该占掉首选位：选它与不选它上屏结果完全一样，
    // 摆在那里只会让用户以为自己在选什么东西。
    let (c, store) = coord_with("url_same", url_cfg(true));
    store
        .record_completion(CompletionKind::UrlHistory, "www.")
        .unwrap();
    store
        .record_completion(CompletionKind::UrlHistory, "www.a.com")
        .unwrap();
    enter_url(&c);
    assert_eq!(
        c.debug_page_texts(),
        vec!["www.a.com".to_string()],
        "只该给出与缓冲不同的补全项"
    );
}

#[test]
fn url_history_is_pruned_to_the_configured_ceiling() {
    let mut cfg = url_cfg(true);
    cfg.input.url.history_max = 2;
    let (c, store) = coord_with("url_prune", cfg);
    // 先埋两条高频的，再上屏一条新的 —— 新条目 count=1，应当场被裁掉。
    for text in ["www.hot1", "www.hot2"] {
        for _ in 0..5 {
            store
                .record_completion(CompletionKind::UrlHistory, text)
                .unwrap();
        }
    }
    enter_url(&c);
    press_letter(&c, 'z');
    press(&c, wind_keys::keymap::VK_SPACE); // 上屏 www.z

    let (rows, total) = store
        .list_completions(CompletionKind::UrlHistory, "", 0, 0)
        .unwrap();
    assert_eq!(total, 2, "应裁剪到上限，实际 {rows:?}");
    assert!(
        rows.iter().all(|(t, _)| t != "www.z"),
        "裁剪按与补全展示同一个排序取舍，最冷的那条（刚上屏、count=1）该出局：{rows:?}"
    );
}
