//! **字词范围**（`word_scope`）：候选里出单字、出词组，还是两者都出。
//!
//! 五笔系输入法的传统功能（极点 / 万能 / QQ 五笔的「字词 / 单字 / 词组」三态）。
//! 三层取值，顺序不可换——临时态（热键）> 方案级 `[candidate] word_scope` > 全局
//! `input.word_scope`，收口在 `Coordinator::effective_word_scope`。
//!
//! 判据本身（哪条候选算「单字」、谁豁免）在 `wind_candidate::word_scope_admits` 有单元
//! 测试；本文件只管**接线**：过滤有没有真的作用到主候选链上、临时态与配置层的优先级、
//! 切方案的失效点。
//!
//! ⚠️ `build_dev/data` 不存在时端到端用例**静默跳过而计数照绿**（判据是耗时，正常秒级
//! vs 0.0x s）——本仓栽过一次，见 `codetable_filter_scope_consistency.rs` 的文件头。
//! 恢复命令 `.\scripts\dev.ps1 gd`。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_candidate::WordScope;
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

fn wubi_config(word_scope: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.word_scope = word_scope.into();
    cfg
}

fn coord_with(word_scope: &str) -> std::sync::Arc<Coordinator> {
    Coordinator::new_headless(wubi_config(word_scope), Some(&data_dir()))
}

/// 按键走**生产入口** `handle_key_event_policed`（同 `codetable_filter_scope_consistency`
/// 的理由：直接调内部的 `handle_key_event` 会绕过收口，等于验证一条不存在的路径）。
fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn candidates_for(word_scope: &str, code: &str) -> Vec<String> {
    let coord = coord_with(word_scope);
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

/// 前置对照：默认（`all`）档下字与词**都在**。
///
/// 这一条不是凑数——没有它，下面两条「某一类不在」的断言可能只是因为这个码压根没有
/// 那一类候选，测了个寂寞。
#[test]
fn all_scope_keeps_both_chars_and_words() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let list = candidates_for("all", MIXED_CODE);
    assert!(
        has_single_char(&list),
        "默认档 {MIXED_CODE} 应有单字候选: {list:?}"
    );
    assert!(
        has_multi_char(&list),
        "默认档 {MIXED_CODE} 应有词组候选: {list:?}"
    );
}

/// 主用例：`char` 档下一个词都不出，单字照旧。
#[test]
fn char_scope_drops_words_globally() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let list = candidates_for("char", MIXED_CODE);
    assert!(
        !has_multi_char(&list),
        "单字档不该出现任何多字候选: {list:?}"
    );
    assert!(
        has_single_char(&list),
        "单字档仍要有单字候选（滤空了就是接错了位置）: {list:?}"
    );
}

/// 对称用例：`phrase` 档下一个单字都不出。
///
/// 与上一条一起，把「两档互补」这个性质钉在**端到端**层面（判据层的互补另有单元测试）。
#[test]
fn phrase_scope_drops_single_chars_globally() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let list = candidates_for("phrase", MIXED_CODE);
    assert!(
        !has_single_char(&list),
        "词组档不该出现任何单字候选: {list:?}"
    );
    assert!(has_multi_char(&list), "词组档仍要有词候选: {list:?}");
}

/// 未知配置值回退 `all`（不是回退成「什么都不出」）。
///
/// 配置是用户可手改的文本；拼错一个词让候选列表凭空少一半、且毫无提示，是最难倒推的
/// 那类现象。
#[test]
fn unknown_config_value_falls_back_to_all() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let list = candidates_for("single", MIXED_CODE); // 「single」不是合法取值
    assert!(
        has_multi_char(&list) && has_single_char(&list),
        "未知取值应回退 all（字词都出）: {list:?}"
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
    let coord = coord_with("all");
    press(&coord, MIXED_CODE);
    assert!(
        has_multi_char(&coord.debug_all_candidate_texts()),
        "前置：默认档应有词"
    );

    coord.set_word_scope(WordScope::Char);
    assert!(
        !has_multi_char(&coord.debug_all_candidate_texts()),
        "切到单字档后，**当前这次组合**的候选就要重建（不能等下次按键）"
    );
    assert_eq!(
        coord.debug_config_word_scope(),
        "all",
        "临时态绝不能写回 input.word_scope——它是方案级配置，持久化要落 schema_overrides"
    );
}

/// 循环切换从**当前生效**档位算起，按 `WORD_SCOPES` 表序走一圈回到原点。
#[test]
fn cycle_walks_the_table_and_returns() {
    let coord = coord_with("all");
    assert_eq!(coord.debug_effective_word_scope(), "all");
    coord.cycle_word_scope();
    assert_eq!(coord.debug_effective_word_scope(), "char");
    coord.cycle_word_scope();
    assert_eq!(coord.debug_effective_word_scope(), "phrase");
    coord.cycle_word_scope();
    assert_eq!(coord.debug_effective_word_scope(), "all", "循环应回到原点");
}

/// 切到与当前生效档位**相同**的值不留临时态。
///
/// 否则「方案配的就是 char，用户又按了一次切到 char」会平白留下一个 `Some(Char)`，
/// 随后的切方案清空就变成了用户可感知的跳变（他明明什么都没改）。
#[test]
fn switching_to_the_current_scope_leaves_no_override() {
    let coord = coord_with("char");
    assert_eq!(coord.debug_effective_word_scope(), "char");
    coord.set_word_scope(WordScope::Char);
    assert!(
        !coord.debug_has_word_scope_override(),
        "切到已生效的档位不该留下临时态"
    );
}

/// 临时态**压过配置层**：全局配 `char`，热键切 `all` 就该出词。
#[test]
fn runtime_override_beats_config() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with("char");
    press(&coord, MIXED_CODE);
    assert!(
        !has_multi_char(&coord.debug_all_candidate_texts()),
        "前置：全局配 char 时不该有词"
    );

    coord.set_word_scope(WordScope::All);
    assert!(
        has_multi_char(&coord.debug_all_candidate_texts()),
        "热键切到 all 应压过全局的 char"
    );
}

// ─────────────────────── 与满码自动上屏的口径 ───────────────────────

/// ★ 单字档的**核心价值**：同码的词被滤掉后，满码唯一即自动上屏（定长盲打）。
///
/// # 这条为什么不需要动引擎
///
/// `decide_auto_commit`（`wind-engine` 的 `codetable/engine.rs`）数的是**引擎自己**那份
/// 候选里 `code == input` 的条数，字词范围的过滤却做在协调器侧 ⇒ 引擎看见的列表里那个
/// 词还在，按它判恒是「不唯一、不上屏」。
///
/// 救回来的是既有的**显示态复评**（`handle_candidate.rs` 里 `recheck_auto_commit` 那一段）：
/// 引擎没给出上屏意向时，按**最终显示列表**再判一次。它当初是为智能过滤加的
/// （滤掉同码生僻字后复评放行，见 `recheck_auto_commit_unique_after_filter`），而判据落在
/// 「用户看见的是哪些候选」上、与过滤的成因无关，于是字词范围天然搭上了同一趟车。
///
/// ⇒ 本条同时是那条复评的**第二个消费者**的守门：将来谁把复评改成只认智能过滤
/// （比如加一个 `filter_mode` 判据），这里会红。
#[test]
fn char_scope_enables_auto_commit_at_full_code() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_config("char");
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
        "单字档下同码只剩一个单字，满码应自动上屏它；实际 {last:?}"
    );

    // ★ 反向对照：默认档下同一个码有多条精确候选，**必须不**自动上屏。
    // 少了这条，一个「无论如何都上屏首选」的错误实现照样通过上面那条。
    let mut cfg2 = wubi_config("all");
    cfg2.schema.codetable.auto_commit_at_full = true;
    let c2 = Coordinator::new_headless(cfg2, Some(&data_dir()));
    let mut last2 = None;
    for c in MIXED_CODE.chars() {
        last2 =
            Some(c2.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF)));
    }
    assert!(
        !matches!(last2, Some(KeyAction::InsertText { .. })),
        "默认档下 {MIXED_CODE} 有多条精确候选，不该自动上屏；实际 {last2:?}"
    );
    assert!(
        c2.debug_all_candidate_texts().len() > 1,
        "前置：默认档该码应有多条候选"
    );
}

// ─────────────────────── 方案级配置与失效点 ───────────────────────

/// 建一个隔离的 override 目录（同 `schema_key_actions.rs` 的做法：`new_headless` 会取真实
/// 用户目录，测试写进去要污染用户配置）。
fn make_override(tag: &str, schema_id: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_ws_ov_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{schema_id}.toml")), body).unwrap();
    dir
}

/// 方案级 `[candidate] word_scope` 压过全局。
#[test]
fn schema_level_scope_beats_global() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let ov = make_override(
        "schema_char",
        "wubi86",
        "[candidate]\nword_scope = \"char\"\n",
    );
    // 全局是 all，方案说 char ⇒ 生效的是 char。
    let coord = Coordinator::new_headless_with_override(
        wubi_config("all"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    assert_eq!(
        coord.debug_effective_word_scope(),
        "char",
        "方案级声明应压过全局"
    );
    press(&coord, MIXED_CODE);
    assert!(
        !has_multi_char(&coord.debug_all_candidate_texts()),
        "方案级 char 应真的滤掉词: {:?}",
        coord.debug_all_candidate_texts()
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// `follow`（出厂）＝跟随全局，不是「什么都不做」。
///
/// 反向对照：少了这条，一个「方案段存在就当 char」的错误实现照样通过上面那条。
#[test]
fn schema_level_follow_falls_back_to_global() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let ov = make_override(
        "schema_follow",
        "wubi86",
        "[candidate]\nword_scope = \"follow\"\n",
    );
    let coord = Coordinator::new_headless_with_override(
        wubi_config("phrase"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    assert_eq!(
        coord.debug_effective_word_scope(),
        "phrase",
        "方案写 follow 时应回落到全局配的 phrase"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// ★ 失效点：切方案清掉临时态，回到新方案配置的档位。
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
    let mut cfg = wubi_config("all");
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.keys
        .key_actions
        .insert("lshift".into(), "switch_schema:pinyin".into());
    let coord = Coordinator::new_headless(cfg, Some(&d));

    coord.set_word_scope(WordScope::Char);
    assert!(coord.debug_has_word_scope_override(), "前置：临时态已置上");

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
        !coord.debug_has_word_scope_override(),
        "切方案应清掉字词范围的临时态"
    );
    assert_eq!(
        coord.debug_effective_word_scope(),
        "all",
        "清掉临时态后应回到配置层的档位"
    );
}

// ─────────────────────── 位置约束（源码级守门）───────────────────────

/// ★ 字词范围的过滤必须排在 `apply_shadow` **之前**。
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
#[test]
fn word_scope_filter_runs_before_shadow() {
    const SRC: &str = include_str!("../src/handle_candidate.rs");
    let scope = SRC
        .find("self.apply_word_scope(state, &mut candidates);")
        .expect("主候选链里应有字词范围过滤（改了写法请一并更新本测试）");
    let shadow = SRC
        .find("let mut user_pinned = self.apply_shadow(&mut candidates, &shadow_code);")
        .expect("主候选链里应有 shadow（改了写法请一并更新本测试）");
    assert!(
        scope < shadow,
        "字词范围过滤必须排在 apply_shadow 之前：shadow 的位次记的是用户所见的列表"
    );
}

/// 补全池必须走**同一条**过滤链。
///
/// 主列表被滤空之后才会从补全池取一条；那一条若不过字词范围，用户看到的就是
/// 「开了单字模式还是冒出一个词」。同款教训在补全池收口那段注释里已经写过一次
/// （shadow 不过滤 ⇒「隐藏完当场又被补回来」）。
#[test]
fn completion_pool_goes_through_the_same_filter() {
    const SRC: &str = include_str!("../src/handle_candidate.rs");
    let filter = SRC
        .find("self.apply_filter(state, &mut completion_pool);")
        .expect("补全池应走检索范围过滤");
    let scope = SRC
        .find("self.apply_word_scope(state, &mut completion_pool);")
        .expect("补全池也必须走字词范围过滤，否则单字档下会从补全池冒出词");
    assert!(filter < scope, "两道过滤的相对次序应与主链一致");
}
