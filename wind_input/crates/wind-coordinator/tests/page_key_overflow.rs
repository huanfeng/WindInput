//! 翻页键翻不动时的处置 `keys.overflow.page_key` 的端到端验证。
//!
//! `commit_and_input` 档：主路上可打印的翻页键（`-` `=` `,` `.` `[` `]`）翻不动、且这批
//! 候选从未翻过页时，不再空转吞键，而是落到标点臂——顶高亮候选 + 输出该键的符号。
//!
//! ## ⚠️ 反向对照不可省
//!
//! 每条正向用例都配一条 `ignore`（出厂）的对照：同一操作在出厂档下必须仍是吞键。只测正向的话，
//! 哪怕开关整个没接线（键因别的原因落到了标点臂），用例一样会绿。
//!
//! 词典缺失时自动跳过 —— ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**，
//! 确认真的跑了看 `--nocapture` 里有没有「跳过：」那行。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_keys::keymap::{VK_BACKTICK, VK_COMMA, VK_DOWN, VK_EQUAL, VK_MINUS, VK_PERIOD, VK_PRIOR};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn data_ready() -> bool {
    let d = data_dir();
    let ok = d.join("schemas/pinyin/rime_frost.dict.yaml").exists()
        && d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists();
    if !ok {
        eprintln!("跳过：词库不存在");
    }
    ok
}

fn key_event(key_code: u32) -> KeyEventData {
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

fn press_vk(coord: &Coordinator, vk: u32) -> KeyAction {
    coord.handle_key_event(&key_event(vk))
}

/// 按下一串字母，返回最后一键的动作。
fn press(coord: &Coordinator, code: &str) -> KeyAction {
    let mut last = KeyAction::Consumed;
    for c in code.chars() {
        debug_assert!(c.is_ascii_alphabetic(), "符号键请用 press_vk");
        last = coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    last
}

fn committed(action: &KeyAction) -> Option<&str> {
    match action {
        KeyAction::InsertText { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

fn composing(action: &KeyAction) -> Option<&str> {
    match action {
        KeyAction::UpdateComposition { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

/// 拼音主方案。`buffer_symbol_chars` 出厂是 `"-"`，传 `""` 让 `-` 不被缓冲闸门截走。
fn pinyin_config(page_key: &str, buffer_symbol_chars: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.buffer_symbol_chars = buffer_symbol_chars.into();
    cfg.keys.overflow.page_key = page_key.into();
    cfg
}

/// 五笔主方案（拼音在 available 里，供临拼取用）。
///
/// `punct_commit` 显式打开：L1 默认是 `false`（L2 出厂 `true`），关着时标点臂对有编码的
/// 按键一律吞掉，测不到顶屏。
fn wubi_config(page_key: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.punct_commit = true;
    cfg.keys.overflow.page_key = page_key.into();
    cfg
}

fn coord(cfg: Config) -> std::sync::Arc<Coordinator> {
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

/// 打 `gong`（拼音，多页）并断言前提：首选「共」、至少两页、停在首页。
fn type_gong(c: &Coordinator) {
    press(c, "gong");
    let page = c.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("共"),
        "前提：gong 首选应是「共」，实际: {page:?}"
    );
    assert!(page.len() >= 2, "前提：首页要有竞争者，实际: {page:?}");
    let (cur, _, total) = c.debug_page_info();
    assert_eq!(cur, 0, "前提：停在首页");
    assert!(total >= 2, "前提：gong 至少两页候选，实际 {total} 页");
}

/// 打 `wqvb`（五笔，单页：你好 / 您好）并断言前提。
fn type_wqvb(c: &Coordinator) {
    press(c, "wqvb");
    let page = c.debug_page_texts();
    assert_eq!(
        page,
        vec!["你好".to_string(), "您好".to_string()],
        "前提：wqvb 单页两条（你好 / 您好）"
    );
    assert_eq!(c.debug_page_info().2, 1, "前提：wqvb 只有一页");
}

// ───────────────────────── 首页上翻：顶屏 + 出符号 ─────────────────────────

/// 主用例：首页按 `-`（上翻翻不动）→ 上屏「首选 + -」。
#[test]
fn minus_unpaged_first_page_commits_highlight_and_symbol() {
    if !data_ready() {
        return;
    }
    let c = coord(pinyin_config("commit_and_input", ""));
    type_gong(&c);
    let act = press_vk(&c, VK_MINUS);
    assert_eq!(committed(&act), Some("共-"), "实际: {act:?}");

    // 反向对照：出厂 ignore 下同一操作仍是空转吞键，组合与候选原样。
    let c = coord(pinyin_config("ignore", ""));
    type_gong(&c);
    let act = press_vk(&c, VK_MINUS);
    assert!(matches!(act, KeyAction::Consumed), "实际: {act:?}");
    assert_eq!(c.debug_page_texts().first().map(String::as_str), Some("共"));
}

/// 符号入缓冲闸门在前、保持优先：`buffer_symbol_chars = "-"` 时首页的 `-` 照旧进缓冲。
#[test]
fn symbol_buffer_gate_still_wins() {
    if !data_ready() {
        return;
    }
    for page_key in ["commit_and_input", "ignore"] {
        let c = coord(pinyin_config(page_key, "-"));
        type_gong(&c);
        let act = press_vk(&c, VK_MINUS);
        assert_eq!(
            composing(&act),
            Some("gong-"),
            "{page_key}：`-` 应先进缓冲，实际: {act:?}"
        );
    }
}

/// 翻过页再翻回首页：此刻 `-` 仍是翻页键（空转吞键），不出字。
#[test]
fn minus_after_paging_back_is_still_swallowed() {
    if !data_ready() {
        return;
    }
    for page_key in ["commit_and_input", "ignore"] {
        let c = coord(pinyin_config(page_key, ""));
        type_gong(&c);
        press_vk(&c, VK_EQUAL);
        assert_eq!(c.debug_page_info().0, 1, "前提：`=` 翻到第 2 页");
        press_vk(&c, VK_MINUS);
        assert_eq!(c.debug_page_info().0, 0, "前提：`-` 翻回第 1 页");
        let act = press_vk(&c, VK_MINUS);
        assert!(
            matches!(act, KeyAction::Consumed),
            "{page_key}：翻过页后首页的 `-` 应吞键，实际: {act:?}"
        );
        assert_eq!(c.debug_page_info().0, 0);
        assert!(!c.debug_page_texts().is_empty(), "{page_key}：组合应还在");
    }
}

/// 上翻之外：高亮挪过之后顶的是**高亮项**，不是首选。
#[test]
fn highlight_moved_commits_highlighted() {
    if !data_ready() {
        return;
    }
    let c = coord(pinyin_config("commit_and_input", ""));
    type_gong(&c);
    let second = c.debug_page_texts()[1].clone();
    press_vk(&c, VK_DOWN);
    assert_eq!(c.debug_page_info().1, 1, "前提：↓ 高亮移到第 2 项");
    let act = press_vk(&c, VK_MINUS);
    assert_eq!(
        committed(&act),
        Some(format!("{second}-").as_str()),
        "实际: {act:?}"
    );

    let c = coord(pinyin_config("ignore", ""));
    type_gong(&c);
    press_vk(&c, VK_DOWN);
    let act = press_vk(&c, VK_MINUS);
    assert!(matches!(act, KeyAction::Consumed), "实际: {act:?}");
}

/// 全角态出全角符号：符号走的是标点流水线，不是原样拼一个 `-`。
#[test]
fn fullwidth_outputs_fullwidth_symbol() {
    if !data_ready() {
        return;
    }
    let mut cfg = pinyin_config("commit_and_input", "");
    cfg.input.default.full_width = true;
    let c = coord(cfg);
    type_gong(&c);
    let act = press_vk(&c, VK_MINUS);
    assert_eq!(committed(&act), Some("共－"), "实际: {act:?}");

    let mut cfg = pinyin_config("ignore", "");
    cfg.input.default.full_width = true;
    let c = coord(cfg);
    type_gong(&c);
    let act = press_vk(&c, VK_MINUS);
    assert!(matches!(act, KeyAction::Consumed), "实际: {act:?}");
}

// ───────────────────────── 末页下翻 ─────────────────────────

/// 单页码按 `=`（下翻翻不动）→ 上屏「首选 + =」。关掉末页放宽，隔离那条优先通路。
#[test]
fn equal_on_single_page_commits() {
    if !data_ready() {
        return;
    }
    let mut cfg = wubi_config("commit_and_input");
    cfg.input.scope_relax.page_end_key = false;
    let c = coord(cfg);
    type_wqvb(&c);
    let act = press_vk(&c, VK_EQUAL);
    assert_eq!(committed(&act), Some("你好="), "实际: {act:?}");

    let mut cfg = wubi_config("ignore");
    cfg.input.scope_relax.page_end_key = false;
    let c = coord(cfg);
    type_wqvb(&c);
    let act = press_vk(&c, VK_EQUAL);
    assert!(matches!(act, KeyAction::Consumed), "实际: {act:?}");
}

/// 翻到末页再按 `=`：翻过页了，仍吞键、不上屏。
#[test]
fn equal_at_last_page_after_paging_is_swallowed() {
    if !data_ready() {
        return;
    }
    for page_key in ["commit_and_input", "ignore"] {
        let mut cfg = pinyin_config(page_key, "");
        cfg.input.scope_relax.page_end_key = false;
        let c = coord(cfg);
        type_gong(&c);
        for _ in 0..50 {
            let (cur, _, total) = c.debug_page_info();
            if cur + 1 >= total && !c.debug_has_more() {
                break;
            }
            press_vk(&c, VK_EQUAL);
        }
        let (cur, _, total) = c.debug_page_info();
        assert!(
            cur > 0 && cur + 1 == total,
            "前提：已翻到末页 {cur}/{total}"
        );
        let act = press_vk(&c, VK_EQUAL);
        assert!(
            matches!(act, KeyAction::Consumed),
            "{page_key}：末页的 `=` 应吞键，实际: {act:?}"
        );
        assert_eq!(c.debug_page_info().0, cur, "{page_key}：页码不动");
    }
}

/// 智能档、确有被滤字的单页码：第一下 `=` 是检索范围放宽，不上屏。
///
/// `gggg` 智能档 3 条（王 / 五一 / 琵琶），全部字符档多一条被滤的字。
#[test]
fn scope_relax_takes_precedence() {
    if !data_ready() {
        return;
    }
    let all = {
        let mut cfg = wubi_config("ignore");
        cfg.input.filter_mode = "gb18030".into();
        let c = coord(cfg);
        press(&c, "gggg");
        c.debug_candidate_count()
    };
    let mut cfg = wubi_config("commit_and_input");
    cfg.input.filter_mode = "smart".into();
    cfg.input.scope_relax.page_end_key = true;
    let c = coord(cfg);
    press(&c, "gggg");
    let before = c.debug_candidate_count();
    assert!(
        all > before,
        "前提：gggg 智能档有被滤的字（{all} vs {before}）"
    );
    assert_eq!(c.debug_page_info().2, 1, "前提：gggg 只有一页");

    let act = press_vk(&c, VK_EQUAL);
    assert!(
        committed(&act).is_none(),
        "放宽优先，不该上屏，实际: {act:?}"
    );
    assert!(c.debug_scope_relaxed(), "第一下 `=` 应放宽检索范围");
    assert!(c.debug_candidate_count() > before, "放宽后被滤的字追加回来");
}

// ───────────────────────── 其它翻页键 ─────────────────────────

/// `,.` 作翻页键：单页按 `.` → 首选 + 中文句号（走标点转换）。
#[test]
fn comma_period_group_converts_punct() {
    if !data_ready() {
        return;
    }
    let mk = |page_key: &str| {
        let mut cfg = wubi_config(page_key);
        cfg.keys.page_keys = vec!["comma_period".into()];
        cfg.input.scope_relax.page_end_key = false;
        coord(cfg)
    };
    let c = mk("commit_and_input");
    type_wqvb(&c);
    let act = press_vk(&c, VK_PERIOD);
    assert_eq!(committed(&act), Some("你好。"), "实际: {act:?}");

    let c = mk("commit_and_input");
    type_wqvb(&c);
    let act = press_vk(&c, VK_COMMA);
    assert_eq!(
        committed(&act),
        Some("你好，"),
        "上翻 `,` 同理，实际: {act:?}"
    );

    let c = mk("ignore");
    type_wqvb(&c);
    let act = press_vk(&c, VK_PERIOD);
    assert!(matches!(act, KeyAction::Consumed), "实际: {act:?}");
}

/// 不可打印的翻页键（PageUp）不受影响：开关开着，首页按它仍吞键。
#[test]
fn non_printable_page_key_unaffected() {
    if !data_ready() {
        return;
    }
    for page_key in ["commit_and_input", "ignore"] {
        let c = coord(pinyin_config(page_key, ""));
        type_gong(&c);
        let act = press_vk(&c, VK_PRIOR);
        assert!(
            matches!(act, KeyAction::Consumed),
            "{page_key}：PageUp 应吞键，实际: {act:?}"
        );
        assert_eq!(c.debug_page_texts().first().map(String::as_str), Some("共"));
    }
}

/// overlay（临拼）有自己的翻页语义，不归本开关：首页按 `-` 仍吞键、留在临拼。
#[test]
fn overlay_temp_pinyin_unaffected() {
    if !data_ready() {
        return;
    }
    for page_key in ["commit_and_input", "ignore"] {
        let c = coord(wubi_config(page_key));
        press_vk(&c, VK_BACKTICK);
        assert!(c.debug_in_temp_pinyin(), "前提：反引号进临拼");
        press(&c, "gong");
        assert!(
            c.debug_page_info().2 >= 2 && !c.debug_page_texts().is_empty(),
            "前提：临拼 gong 多页"
        );
        let act = press_vk(&c, VK_MINUS);
        assert!(
            matches!(act, KeyAction::Consumed),
            "{page_key}：临拼首页的 `-` 应吞键，实际: {act:?}"
        );
        assert!(c.debug_in_temp_pinyin(), "{page_key}：仍在临拼");
    }
}

// ───────────────────────── 按键级策略（翻页动作的后缀）─────────────────────────
// 设计：docs/design/keys-overflow-per-key.md。全局档保留为默认，自定义按键里给单个键绑
// `page_prev:commit_and_input` / `page_prev:ignore` 把策略落到这个键上——「有人只要逗号翻不动
// 时上屏，其余翻页键无效就是无效」（GH#249）。

/// 后缀优先于全局档：全局 `ignore`，只把 `,` 绑成「上一页（翻不动时上屏）」——同方案下 `,`
/// 上屏「你好，」、`.` 是不带后缀的翻页、回落全局档仍吞键（不被殃及）。
#[test]
fn comma_suffix_commits_and_period_falls_back() {
    if !data_ready() {
        return;
    }
    let mut cfg = wubi_config("ignore");
    cfg.keys.page_keys = vec!["comma_period".into()];
    cfg.keys
        .session_actions
        .insert("comma".into(), "page_prev:commit_and_input".into());
    let c = coord(cfg);
    type_wqvb(&c);
    let act = press_vk(&c, VK_COMMA);
    assert_eq!(
        committed(&act),
        Some("你好，"),
        "带后缀：`,` 翻不动应顶屏出符号，实际: {act:?}"
    );

    // 同样配置，`.` 不带后缀 → 回落全局 `ignore`，吞键。
    let cfg2 = {
        let mut cfg = wubi_config("ignore");
        cfg.keys.page_keys = vec!["comma_period".into()];
        cfg.keys
            .session_actions
            .insert("comma".into(), "page_prev:commit_and_input".into());
        cfg
    };
    let c2 = coord(cfg2);
    type_wqvb(&c2);
    let act = press_vk(&c2, VK_PERIOD);
    assert!(
        matches!(act, KeyAction::Consumed),
        "不带后缀的 `.` 应回落全局档吞键，实际: {act:?}"
    );
}

/// 显式压制：全局 `commit_and_input`，`,` 绑 `page_prev:ignore`——「用户能关」正是
/// 按键级策略存在的理由；不带后缀的 `.` 仍按全局档上屏。
#[test]
fn suffix_ignore_suppresses_global_commit() {
    if !data_ready() {
        return;
    }
    let mk = || {
        let mut cfg = wubi_config("commit_and_input");
        cfg.keys.page_keys = vec!["comma_period".into()];
        cfg.keys
            .session_actions
            .insert("comma".into(), "page_prev:ignore".into());
        coord(cfg)
    };
    let c = mk();
    type_wqvb(&c);
    let act = press_vk(&c, VK_COMMA);
    assert!(
        matches!(act, KeyAction::Consumed),
        "`:ignore`：`,` 应吞键，实际: {act:?}"
    );

    let c2 = mk();
    type_wqvb(&c2);
    let act = press_vk(&c2, VK_PERIOD);
    assert_eq!(
        committed(&act),
        Some("你好。"),
        "不带后缀的 `.` 走全局档上屏，实际: {act:?}"
    );
}

/// 带后缀同样守「翻过页仍吞键」红线：翻页中途永远不出符号。
#[test]
fn suffix_paged_still_swallows() {
    if !data_ready() {
        return;
    }
    let mut cfg = pinyin_config("ignore", "");
    cfg.keys.page_keys = vec!["comma_period".into()];
    cfg.keys
        .session_actions
        .insert("comma".into(), "page_prev:commit_and_input".into());
    let c = coord(cfg);
    type_gong(&c);
    press_vk(&c, VK_PERIOD);
    assert_eq!(c.debug_page_info().0, 1, "前提：`.` 翻到第 2 页");
    press_vk(&c, VK_COMMA);
    assert_eq!(c.debug_page_info().0, 0, "前提：`,` 翻回第 1 页");
    let act = press_vk(&c, VK_COMMA);
    assert!(
        matches!(act, KeyAction::Consumed),
        "翻过页后首页的 `,` 应吞键，实际: {act:?}"
    );
}
