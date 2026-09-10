//! **单字输入**（`single_char`）：开启后候选只出单字。
//!
//! 五笔系输入法的传统功能。取值收口在 `Coordinator::effective_single_char`：英文引擎恒关
//! （不变量，压过临时态），其余引擎临时态（热键）压过配置层，否则**按引擎分流**——
//! 码表/混输取 `schema.codetable.single_char`（⊕ 方案级 `[engine.codetable]` 覆盖），
//! 拼音取 `schema.pinyin.single_char`（无方案级覆盖）。
//!
//! 判据本身（哪条候选算「单字」、谁豁免）在 `wind_candidate::single_char_admits` 有单元
//! 测试；本文件只管**接线**：过滤有没有真的作用到主候选链上、临时态与配置层的优先级、
//! 切方案的失效点。
//!
//! ⚠️ 曾有第三档「只出词组」，2026-09-09 按用户实测反馈删除（表现「非常奇怪」：单字被
//! 整批滤掉后大量码位只剩零星几条甚至空列表）。别看着「两档互补」好看就加回来。
//!
//! ⚠️ `build_dev/data` 不存在时端到端用例**静默跳过而计数照绿**（判据是耗时，正常秒级
//! vs 0.0x s）——本仓栽过一次，见 `codetable_filter_scope_consistency.rs` 的文件头。
//! 恢复命令 `.\scripts\dev.ps1 gd`。

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

/// 按键走**生产入口** `handle_key_event_policed`（同 `codetable_filter_scope_consistency`
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

/// 前置对照：关闭（出厂）时字与词**都在**。
///
/// 这一条不是凑数——没有它，下面那条「词不在」的断言可能只是因为这个码压根没有词候选，
/// 测了个寂寞。
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

/// 热键切换立刻生效，且**不写配置**。
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

/// 切换从**当前生效**状态取反，一开一关回到原点。
#[test]
fn toggle_flips_and_flips_back() {
    // 判据前置：`effective_single_char` 先看**活动引擎类型**，无词库时引擎压根加载不出来
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

/// 切到与当前生效状态**相同**的值不留临时态。
///
/// 否则「方案本来就开着，用户又按了一次开」会平白留下一个 `Some(true)`，
/// 随后的切方案清空就变成了用户可感知的跳变（他明明什么都没改）。
#[test]
fn switching_to_the_current_state_leaves_no_override() {
    // 判据前置：`effective_single_char` 先看**活动引擎类型**，无词库时引擎压根加载不出来
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

/// 临时态**压过配置层**：全局开着，热键关掉就该出词。
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

/// ★ 单字输入的**核心价值**：同码的词被滤掉后，满码唯一即自动上屏（定长盲打）。
///
/// # 这条为什么不需要动引擎
///
/// `decide_auto_commit`（`wind-engine` 的 `codetable/engine.rs`）数的是**引擎自己**那份
/// 候选里 `code == input` 的条数，本项的过滤却做在协调器侧 ⇒ 引擎看见的列表里那个
/// 词还在，按它判恒是「不唯一、不上屏」。
///
/// 救回来的是既有的**显示态复评**（`handle_candidate.rs` 里 `recheck_auto_commit` 那一段）：
/// 引擎没给出上屏意向时，按**最终显示列表**再判一次。它当初是为智能过滤加的
/// （滤掉同码生僻字后复评放行，见 `recheck_auto_commit_unique_after_filter`），而判据落在
/// 「用户看见的是哪些候选」上、与过滤的成因无关，于是本项天然搭上了同一趟车。
///
/// ⇒ 本条同时是那条复评的**第二个消费者**的守门：将来谁把复评改成只认智能过滤
/// （比如加一个 `filter_mode` 判据），这里会红。
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

    // ★ 反向对照：关闭时同一个码有多条精确候选，**必须不**自动上屏。
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
    // 方案段里写了**别的**键，段存在但没表态。
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

/// 方案级写错**类型** ⇒ 不得凭空开启。
///
/// `single_char = "yes"` 是类型错误（本字段是布尔），走**段级降级**——`[engine.codetable]`
/// 整段回落出厂，本项自然回到「没表态」⇒ 跟随全局。与同段其它 `Option` 同一条路。
///
/// ⛔ 别为它挂 `tolerant_de::tolerant` 想做成字段级容错：那个适配器只认字符串枚举，
/// 挂上去连正确的 `single_char = true` 都会解析失败（本仓已实测踩过）。
///
/// 全局取**关**是刻意的：这样「把 `"yes"` 错当成 true」的实现才会红。全局取开的话
/// 无论实现对错都是开，测了个寂寞。
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

/// ★ 失效点：切方案清掉临时态，回到新方案配置的取值。
///
/// 收口在 `sync_schema_scope`（代际驱动）——本仓唯一能覆盖全部五条切方案路径的地方。
/// 命令式地在各切方案处逐个清必然漏接（`finish_user_schema_switch` 自己的注释就写明
/// 它只覆盖五条里的两条，启动载入那条一条都不走）。
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

/// ★ 本项的过滤必须排在 `apply_shadow` **之前**。
///
/// # 为什么用源码顺序断言，而不是行为断言
///
/// 这条约束的失效表现是「用户右键置顶/隐藏的位次对不上」，而端到端造一条 shadow 规则要
/// 真实的 `store`（`new_headless` 给的是 `None`）。行为断言写不出来，就只剩两种选择：
/// 不测，或者把「位置」本身断言掉。选后者——`ShadowPin.position` 是**绝对下标**，记的是
/// 用户右键当时所见的那个列表；滤在 shadow 之后，位次就对不上了。
/// 出简让全踩过一模一样的坑（「置顶写得进去、下次打同一个码毫无变化」）。
///
/// ⚠️ 本条对重构**不友好是刻意的**：改了函数名或调用写法它就红，那时请顺着这段注释
/// 判断新写法有没有破坏次序，而不是顺手把字符串改一改让它变绿。
///
/// ⚠️ 已知弱点：它钉的是**文本位置**而非执行顺序。把调用挪进一个「定义在文件更靠前、
/// 却在 shadow 之后被调用」的辅助函数，本条会假绿。今天可靠是因为两个锚点都落在
/// `build_candidates` 同一个函数体内，文本序 = 执行序。
#[test]
fn single_char_filter_runs_before_shadow() {
    const SRC: &str = include_str!("../src/handle_candidate.rs");
    let scope = SRC
        .find("self.apply_single_char(state, &mut candidates);")
        .expect("主候选链里应有单字输入过滤（改了写法请一并更新本测试）");
    let shadow = SRC
        .find("let mut user_pinned = self.apply_shadow(&mut candidates, &shadow_code);")
        .expect("主候选链里应有 shadow（改了写法请一并更新本测试）");
    assert!(
        scope < shadow,
        "单字输入过滤必须排在 apply_shadow 之前：shadow 的位次记的是用户所见的列表"
    );
}

/// 补全池必须走**同一条**过滤链。
///
/// 主列表被滤空之后才会从补全池取一条；那一条若不过本项过滤，用户看到的就是
/// 「开了单字模式还是冒出一个词」。同款教训在补全池收口那段注释里已经写过一次
/// （shadow 不过滤 ⇒「隐藏完当场又被补回来」）。
#[test]
fn completion_pool_goes_through_the_same_filter() {
    const SRC: &str = include_str!("../src/handle_candidate.rs");
    let filter = SRC
        .find("self.apply_filter(state, &mut completion_pool);")
        .expect("补全池应走检索范围过滤");
    let scope = SRC
        .find("self.apply_single_char(state, &mut completion_pool);")
        .expect("补全池也必须走单字输入过滤，否则会从补全池冒出词");
    assert!(filter < scope, "两道过滤的相对次序应与主链一致");
}

// ─────────────────────── 两侧独立 ───────────────────────

/// ★★ 码表侧与拼音侧是**两个字段**，互不影响。
///
/// 这是 2026-09-08 定下的配置形状的核心性质：分成两份，「五笔只出单字、拼音照常出词」
/// 在全局层就表达得了，不必逐方案配。合成一个字段的实现会让本条红。
///
/// ⚠️ 两个方向都要测。只测「拼音不受码表侧管辖」的话，一个把拼音分支写成恒 `false` 的
/// 实现（或者引擎类型没判出来、落到 `_ => false` 兜底）照样通过——第二段就是为了排除
/// 那种假绿：它证明拼音分支**确实走到了**，取的确实是 `schema.pinyin.single_char`。
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

/// `[keys.session_actions]` 绑 `single_char` 时，**空闲按该键必须放行**。
///
/// 这张表收的是 Tab / 翻页键那一批**宿主另有原义**的键（`data/config.toml` 的示例键正是
/// Tab）。执行臂若无条件 `Consumed`，用户照示例写下 `tab = "single_char"` 之后，
/// Tab 在所有程序里当场失效——不只是「无候选时」，是完全空闲时也吞。
///
/// 判据取「有会话」而非「有候选」：单字模式下某个码本来就可能一条候选都不剩，那时若按
/// 「无候选」放行，用户就再也关不掉它了。`SessionAction::requires_candidates` 把
/// `SingleChar` 与 `Cancel` 并列，为的就是这个；本测试钉的是**执行臂**的对应守卫。
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

/// 英文引擎恒关是**不变量**，压得过临时态。
///
/// 这道判据必须问在 `single_char_override` **之前**：写在后面就被绕过去了——英文方案下
/// 按一次 `single_char:on`（或 `toggle`），词库英文候选走主链被整批滤光，
/// 只剩 `english_head_candidates` 追加的输入原文（那批插在过滤之后）。
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
