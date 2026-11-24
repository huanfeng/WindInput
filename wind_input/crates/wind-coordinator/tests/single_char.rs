//! 单字输入（`single_char`）：开启后候选只出单字。
//!
//! 五笔系输入法的传统功能。取值收口在 `Coordinator::effective_single_char`：英文引擎恒关
//! （不变量，压过临时态），其余引擎临时态（热键）压过配置层，否则按引擎分流——
//! 码表/混输取 `schema.codetable.single_char`（⊕ 方案级 `[engine.codetable]` 覆盖），
//! 拼音取 `schema.pinyin.single_char`（无方案级覆盖）。
//!
//! 判据本身（哪条候选算「单字」、谁豁免）在 `wind_candidate::single_char_admits` 有单元
//! 测试；本文件只管接线：过滤有没有真的作用到主候选链上、临时态与配置层的优先级、
//! 切方案的失效点。
//!
//! 不设「只出词组」档：单字被整批滤掉后大量码位只剩零星几条甚至空列表，用户反馈很怪。
//!
//! `build_dev/data` 不存在时端到端用例静默跳过而计数照绿（判据是耗时 0.0x s）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready(d: &std::path::Path) -> bool {
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
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

fn wubi_config(single_char: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.single_char = single_char;
    cfg
}

fn coord_with(single_char: bool) -> std::sync::Arc<Coordinator> {
    Coordinator::new_headless(wubi_config(single_char), Some(&data_dir()))
}

/// 按键走生产入口 `handle_key_event_policed`（同 `codetable_filter_scope_consistency`
/// 的理由：直接调内部的 `handle_key_event` 会绕过收口，等于验证一条不存在的路径）。
fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn candidates_for(single_char: bool, code: &str) -> Vec<String> {
    let coord = coord_with(single_char);
    press(&coord, code);
    coord.debug_all_candidate_texts()
}

fn has_multi_char(list: &[String]) -> bool {
    list.iter().any(|t| t.chars().count() > 1)
}

fn has_single_char(list: &[String]) -> bool {
    list.iter().any(|t| t.chars().count() == 1)
}

/// 现场：`dddd` 下五笔有「大」「大厦」「硕大」等，字与词同码，是这一族用例的天然舞台。
const MIXED_CODE: &str = "dddd";

// ─────────────────────── 全局配置层 ───────────────────────

/// 前置对照：关闭（出厂）时字与词都在。没有它，下一条「词不在」可能只是因为这个码本无词。
#[test]
fn off_keeps_both_chars_and_words() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let list = candidates_for(false, MIXED_CODE);
    assert!(
        has_single_char(&list),
        "关闭时 {MIXED_CODE} 应有单字候选: {list:?}"
    );
    assert!(
        has_multi_char(&list),
        "关闭时 {MIXED_CODE} 应有词组候选: {list:?}"
    );
}

/// 主用例：开启后一个词都不出，单字照旧。
#[test]
fn on_drops_words_globally() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let list = candidates_for(true, MIXED_CODE);
    assert!(
        !has_multi_char(&list),
        "单字输入不该出现任何多字候选: {list:?}"
    );
    assert!(
        has_single_char(&list),
        "单字输入仍要有单字候选（滤空了就是接错了位置）: {list:?}"
    );
}

// ─────────────────────── 运行时临时态 ───────────────────────

/// 热键切换立刻生效，且不写配置。
#[test]
fn runtime_switch_takes_effect_without_touching_config() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with(false);
    press(&coord, MIXED_CODE);
    assert!(
        has_multi_char(&coord.debug_all_candidate_texts()),
        "前置：关闭时应有词"
    );

    coord.set_single_char(true);
    assert!(
        !has_multi_char(&coord.debug_all_candidate_texts()),
        "开启后，**当前这次组合**的候选就要重建（不能等下次按键）"
    );
    assert!(
        !coord.debug_config_single_char(),
        "临时态绝不能写回 schema.codetable.single_char——持久化要落 schema_overrides"
    );
}

/// 切换从当前生效状态取反，一开一关回到原点。
#[test]
fn toggle_flips_and_flips_back() {
    // 判据前置：`effective_single_char` 先看活动引擎类型，无词库时引擎压根加载不出来
    // （`current_engine_type()` 为 None），临时态还没轮到就被判成关——那是环境缺数据，
    // 不是接线坏了。CI 无 `build_dev/data`，为此红过一轮。
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with(false);
    assert!(!coord.debug_effective_single_char());
    coord.toggle_single_char();
    assert!(coord.debug_effective_single_char(), "切换一次应开启");
    coord.toggle_single_char();
    assert!(!coord.debug_effective_single_char(), "再切一次应回到关闭");
}

/// 切到与当前生效状态相同的值不留临时态。
///
/// 否则会平白留下 `Some(true)`，随后切方案清空时变成用户可感知的跳变。
#[test]
fn switching_to_the_current_state_leaves_no_override() {
    // 判据前置：`effective_single_char` 先看活动引擎类型，无词库时引擎压根加载不出来
    // （`current_engine_type()` 为 None），临时态还没轮到就被判成关——那是环境缺数据，
    // 不是接线坏了。CI 无 `build_dev/data`，为此红过一轮。
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with(true);
    assert!(coord.debug_effective_single_char());
    coord.set_single_char(true);
    assert!(
        !coord.debug_has_single_char_override(),
        "切到已生效的状态不该留下临时态"
    );
}

/// 临时态压过配置层：全局开着，热键关掉就该出词。
#[test]
fn runtime_override_beats_config() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with(true);
    press(&coord, MIXED_CODE);
    assert!(
        !has_multi_char(&coord.debug_all_candidate_texts()),
        "前置：全局开启时不该有词"
    );

    coord.set_single_char(false);
    assert!(
        has_multi_char(&coord.debug_all_candidate_texts()),
        "热键关掉应压过全局的开启"
    );
}

// ─────────────────────── 与满码自动上屏的口径 ───────────────────────

/// 单字输入的核心价值：同码的词被滤掉后，满码唯一即自动上屏（定长盲打）。
///
/// 引擎的 `decide_auto_commit` 看不到协调器侧的过滤，靠的是协调器按最终显示列表再判一次的
/// `recheck_auto_commit`（原为智能过滤而设，见 `recheck_auto_commit_unique_after_filter`）。
/// 谁把复评改成只认智能过滤，这里会红。
#[test]
fn single_char_enables_auto_commit_at_full_code() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_config(true);
    cfg.schema.codetable.auto_commit_at_full = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let mut last = None;
    for c in MIXED_CODE.chars() {
        last = Some(
            coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF)),
        );
    }
    assert!(
        matches!(&last, Some(KeyAction::InsertText { text, .. }) if text.chars().count() == 1),
        "单字输入下同码只剩一个单字，满码应自动上屏它；实际 {last:?}"
    );

    // 反向对照：关闭时同一个码有多条精确候选，必须不自动上屏。
    // 少了这条，一个「无论如何都上屏首选」的错误实现照样通过上面那条。
    let mut cfg2 = wubi_config(false);
    cfg2.schema.codetable.auto_commit_at_full = true;
    let c2 = Coordinator::new_headless(cfg2, Some(&data_dir()));
    let mut last2 = None;
    for c in MIXED_CODE.chars() {
        last2 =
            Some(c2.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF)));
    }
    assert!(
        !matches!(last2, Some(KeyAction::InsertText { .. })),
        "关闭时 {MIXED_CODE} 有多条精确候选，不该自动上屏；实际 {last2:?}"
    );
    assert!(
        c2.debug_all_candidate_texts().len() > 1,
        "前置：关闭时该码应有多条候选"
    );
}

// ─────────────────────── 方案级配置与失效点 ───────────────────────

/// 建一个隔离的 override 目录（同 `schema_key_actions.rs` 的做法：`new_headless` 会取真实
/// 用户目录，测试写进去要污染用户配置）。
fn make_override(tag: &str, schema_id: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_sc_ov_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{schema_id}.toml")), body).unwrap();
    dir
}

/// 方案级 `[engine.codetable] single_char` 压过全局。
#[test]
fn schema_level_beats_global() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let ov = make_override(
        "schema_on",
        "wubi86",
        "[engine.codetable]\nsingle_char = true\n",
    );
    // 全局关，方案说开 ⇒ 生效的是开。
    let coord = Coordinator::new_headless_with_override(
        wubi_config(false),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    assert!(coord.debug_effective_single_char(), "方案级声明应压过全局");
    press(&coord, MIXED_CODE);
    assert!(
        !has_multi_char(&coord.debug_all_candidate_texts()),
        "方案级开启应真的滤掉词: {:?}",
        coord.debug_all_candidate_texts()
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 不写这一项（出厂）＝跟随全局，不是「什么都不做」。
///
/// 反向对照：少了这条，一个「方案段存在就当开启」的错误实现照样通过上面那条。
#[test]
fn schema_level_absent_falls_back_to_global() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    // 方案段里写了别的键，段存在但没表态。
    let ov = make_override(
        "schema_absent",
        "wubi86",
        "[engine.codetable]\nz_key_repeat = true\n",
    );
    let coord = Coordinator::new_headless_with_override(
        wubi_config(true),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    assert!(
        coord.debug_effective_single_char(),
        "方案没表态时应回落到全局配的开启"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 方案级写错类型（`single_char = "yes"`）不得凭空开启：走段级降级，`[engine.codetable]`
/// 整段回落出厂，本项回到「没表态」、跟随全局。
///
/// 别挂 `tolerant_de::tolerant` 做字段级容错：它只认字符串枚举，挂上去连正确的
/// `single_char = true` 都会解析失败。全局取关是刻意的，否则把 `"yes"` 当 true 的实现也绿。
#[test]
fn schema_level_bad_value_does_not_turn_it_on() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let ov = make_override(
        "schema_bad",
        "wubi86",
        "[engine.codetable]\nsingle_char = \"yes\"\n",
    );
    let coord = Coordinator::new_headless_with_override(
        wubi_config(false),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    assert!(
        !coord.debug_effective_single_char(),
        "写错类型的方案级取值不该被当成开启，应跟随全局的关"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 失效点：切方案清掉临时态，回到新方案配置的取值。
///
/// 收口在 `sync_schema_scope`（代际驱动），唯一能覆盖全部切方案路径的地方；在各切方案处
/// 逐个清必然漏接。
#[test]
fn switching_schema_clears_the_runtime_override() {
    let d = data_dir();
    if !d.join("schemas/wubi86.schema.toml").exists()
        || !d.join("schemas/pinyin.schema.toml").exists()
    {
        eprintln!("跳过：缺少 wubi86 / pinyin 方案");
        return;
    }
    let mut cfg = wubi_config(false);
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.keys
        .key_actions
        .insert("lshift".into(), "switch_schema:pinyin".into());
    let coord = Coordinator::new_headless(cfg, Some(&d));

    coord.set_single_char(true);
    assert!(coord.debug_has_single_char_override(), "前置：临时态已置上");

    // 左 Shift 抬起 → 单向切到拼音。
    const VK_LSHIFT: u32 = 0xA0;
    coord.handle_key_event(&KeyEventData {
        key_code: VK_LSHIFT,
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_UP,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    });
    assert_eq!(coord.active_schema_id(), "pinyin", "前置：应已切到拼音");

    assert!(
        !coord.debug_has_single_char_override(),
        "切方案应清掉单字输入的临时态"
    );
    assert!(
        !coord.debug_effective_single_char(),
        "清掉临时态后应回到配置层的取值"
    );
}

// ─────────────────────── 位置约束（源码级守门）───────────────────────

// 「单字过滤排在 shadow 之前」「补全池也走过滤与单字」两条次序约束已由后处理步骤表保证，
// 守卫在 `src/postprocess.rs` 的单测里（profile 内容与规范顺序）。

// ─────────────────────── 两侧独立 ───────────────────────

/// 码表侧与拼音侧是两个字段，互不影响，「五笔只出单字、拼音照常出词」在全局层就能表达。
///
/// 两个方向都要测：只测一边时，把拼音分支写成恒 `false`（或落到 `_ => false` 兜底）的实现
/// 也会通过；第二段证明拼音分支确实取的是 `schema.pinyin.single_char`。
#[test]
fn codetable_and_pinyin_are_independent() {
    let d = data_dir();
    if !d.join("schemas/pinyin.schema.toml").exists() {
        eprintln!("跳过：缺少 pinyin 方案");
        return;
    }
    let pinyin_cfg = |ct: bool, py: bool| {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["pinyin".into()];
        cfg.schema.active = "pinyin".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.codetable.single_char = ct;
        cfg.schema.pinyin.single_char = py;
        cfg
    };

    // ① 码表侧开、拼音侧关 ⇒ 拼音方案下生效的是关。
    let c1 = Coordinator::new_headless(pinyin_cfg(true, false), Some(&d));
    assert!(
        !c1.debug_effective_single_char(),
        "拼音方案不该受 schema.codetable.single_char 管辖"
    );

    // ② 反过来配 ⇒ 生效的是开。这一段同时证明拼音分支真的走到了。
    let c2 = Coordinator::new_headless(pinyin_cfg(false, true), Some(&d));
    assert!(
        c2.debug_effective_single_char(),
        "拼音方案应取 schema.pinyin.single_char；若这里是关，多半是引擎类型没判出来、\
         落到了 `_ => false` 兜底分支"
    );

    // ③ 码表方案侧的反向对照：拼音侧开不该影响五笔。
    let mut cfg3 = wubi_config(false);
    cfg3.schema.pinyin.single_char = true;
    let c3 = Coordinator::new_headless(cfg3, Some(&d));
    assert!(
        !c3.debug_effective_single_char(),
        "码表方案不该受 schema.pinyin.single_char 管辖"
    );
}

// ─────────────────────── 按键接线：吞键边界与引擎不变量 ───────────────────────

const VK_TAB: u32 = 0x09;

/// `[keys.session_actions]` 绑 `single_char` 时，空闲按该键必须放行。
///
/// 这张表收的是 Tab 等宿主另有原义的键，执行臂无条件 `Consumed` 会让 Tab 在所有程序里失效。
/// 判据取「有会话」而非「有候选」：单字模式下可能一条候选都不剩，那时放行就关不掉了
/// （同 `SessionAction::requires_candidates`）。本测试钉执行臂的对应守卫。
#[test]
fn session_bound_single_char_releases_the_key_when_idle() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_config(false);
    cfg.keys
        .session_actions
        .insert("tab".into(), "single_char".into());
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // ① 完全空闲：必须把键还给宿主。
    let idle = coord.handle_key_event_policed(&key_event(VK_TAB));
    assert!(
        !matches!(idle, KeyAction::Consumed),
        "空闲时 Tab 必须还给宿主（实际 {idle:?}）——否则绑了这个动词就等于废掉 Tab 键"
    );
    assert!(
        !coord.debug_has_single_char_override(),
        "空闲按键不该置上临时态"
    );

    // ② 打了码之后：同一个键要生效（这半边证明上面那条不是「绑定压根没接上」的假绿）。
    press(&coord, "a");
    let active = coord.handle_key_event_policed(&key_event(VK_TAB));
    assert!(
        matches!(active, KeyAction::Consumed),
        "有会话时 Tab 应吞键并切换（实际 {active:?}）"
    );
    assert!(
        coord.debug_has_single_char_override(),
        "有会话时按键应置上临时态"
    );
}

/// 英文引擎恒关是不变量，压得过临时态。
///
/// 这道判据必须问在 `single_char_override` 之前，否则英文方案下按一次 `single_char:on`，
/// 词库英文候选会被整批滤光，只剩输入原文。
#[test]
fn english_engine_ignores_the_runtime_override() {
    let d = data_dir();
    if !d.join("schemas/english.schema.toml").exists() || !d.join("schemas/english").is_dir() {
        eprintln!("跳过：英文方案不存在");
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "english".into();
    cfg.input.default.chinese_mode = true;
    let coord = Coordinator::new_headless(cfg, Some(&d));

    coord.set_single_char(true);
    assert!(
        coord.debug_has_single_char_override(),
        "前置：临时态应已置上（否则下一条断言恒真，测不出东西）"
    );
    assert!(
        !coord.debug_effective_single_char(),
        "英文引擎下临时态必须被忽略"
    );

    // 候选层面复核：英文词是多字符，开启后一旦生效就会把它们全滤光。
    press(&coord, "the");
    assert!(
        has_multi_char(&coord.debug_all_candidate_texts()),
        "英文候选不该被单字输入滤光"
    );
}
