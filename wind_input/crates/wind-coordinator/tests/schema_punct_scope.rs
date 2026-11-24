//! 方案级标点（`[punct]`）在方案之间来回切换时的还原语义。设计见
//! `docs/design/schema-scoped-behavior.md` §4。
//!
//! `state.chinese_punct` 既是当前值又是唯一存储，被英文方案覆盖后，切回 `Follow`（不干预）的
//! 方案须回到不受方案影响的那个值（`punct_before_schema`），而不是保持上一个方案强加的值。
//! `restores_the_users_own_preference_not_a_hardcoded_chinese` 防「切回时硬置中文」的错误实现。

use std::path::PathBuf;
use wind_bridge::handler::MessageHandler;
use wind_config::Config;
use wind_coordinator::Coordinator;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 英文方案就绪判据：方案定义要在，且它得真的声明了 `[punct]`。
///
/// 后半条不可省：本组测试靠 `english.schema.toml` 里的 `mode = "english"` 驱动，那行被删时
/// 所有断言会以「标点从没变过」的方式静默通过。
fn ready() -> bool {
    let f = data_dir().join("schemas/english.schema.toml");
    let Ok(text) = std::fs::read_to_string(&f) else {
        return false;
    };
    text.contains("[punct]") && text.contains("english")
}

fn cfg() -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.default.chinese_punct = true;
    c
}

fn coord() -> std::sync::Arc<Coordinator> {
    Coordinator::new_headless(cfg(), Some(&data_dir()))
}

/// 切到下一个方案（available 里循环）。
fn cycle(c: &Coordinator) {
    c.handle_menu_command("switch_engine");
}

fn toggle_punct(c: &Coordinator) {
    c.handle_menu_command("toggle_punct");
}

/// 切到英文方案变英文标点，切回来必须变回中文。
#[test]
fn returning_to_a_follow_schema_restores_the_punct_state() {
    if !ready() {
        eprintln!("跳过：缺少英文方案或它没声明 [punct]");
        return;
    }
    let c = coord();
    assert!(c.is_chinese_punct(), "前提：五笔下是中文标点");

    cycle(&c);
    assert_eq!(c.active_schema_id(), "english");
    assert!(
        !c.is_chinese_punct(),
        "英文方案声明了 [punct] mode = english，应切成英文标点"
    );

    cycle(&c);
    assert_eq!(c.active_schema_id(), "wubi86");
    assert!(
        c.is_chinese_punct(),
        "★ 切回不声明 [punct] 的方案时必须还原成中文标点。\
         「跟随全局」的语义是「回到不受方案影响的那个值」，不是「保持上一个方案强加的值」"
    );
}

/// 手动切换在本代际内胜出：方案意图不会把它顶回去。
#[test]
fn manual_toggle_wins_within_the_same_generation() {
    if !ready() {
        eprintln!("跳过：缺少英文方案或它没声明 [punct]");
        return;
    }
    let c = coord();
    cycle(&c); // → english，英文标点
    assert!(!c.is_chinese_punct());

    toggle_punct(&c);
    assert!(c.is_chinese_punct(), "用户手动切成中文标点");
    // 再走几次同步点（按键/状态推送都会调 sync_schema_scope），代际没变就不该被顶回去。
    c.handle_menu_command("toggle_toolbar");
    c.handle_menu_command("toggle_toolbar");
    assert!(
        c.is_chinese_punct(),
        "代际未变时 sync_schema_scope 必须直接返回，不得把方案意图重新压上来"
    );
}

/// 手动值随代际失效：在英文方案里手动改过，切回五笔仍回到全局态。
#[test]
fn manual_value_expires_with_the_generation() {
    if !ready() {
        eprintln!("跳过：缺少英文方案或它没声明 [punct]");
        return;
    }
    let c = coord();
    cycle(&c); // → english
    toggle_punct(&c); // 手动改成中文标点
    assert!(c.is_chinese_punct());

    cycle(&c); // → wubi86
    assert!(
        c.is_chinese_punct(),
        "回到全局态（中文）——与手动值恰好同值，本条真正钉的是下一条"
    );
}

/// 还原的是用户自己的偏好，不是硬编码的中文；没有这条，「切回时置中文」也能让上面几条全绿。
#[test]
fn restores_the_users_own_preference_not_a_hardcoded_chinese() {
    if !ready() {
        eprintln!("跳过：缺少英文方案或它没声明 [punct]");
        return;
    }
    let c = coord();
    // 用户在五笔下就把标点设成了英文——这是他的全局偏好。
    toggle_punct(&c);
    assert!(!c.is_chinese_punct(), "前提：五笔下已是英文标点");

    cycle(&c); // → english（意图也是英文标点，看不出差别）
    assert!(!c.is_chinese_punct());

    cycle(&c); // → wubi86
    assert!(
        !c.is_chinese_punct(),
        "★★ 必须还原成**用户设的英文标点**。若这里变回中文，说明实现是「切回来就置中文」，\
         用户的全局偏好被一次切方案抹掉了"
    );
}

/// 连续来回切换不会「越还越旧」：每次进有意图的方案都重新记，出来即还清。
#[test]
fn repeated_round_trips_stay_stable() {
    if !ready() {
        eprintln!("跳过：缺少英文方案或它没声明 [punct]");
        return;
    }
    let c = coord();
    for round in 0..3 {
        cycle(&c);
        assert!(!c.is_chinese_punct(), "第 {round} 轮：英文方案下是英文标点");
        cycle(&c);
        assert!(c.is_chinese_punct(), "第 {round} 轮：切回五笔应是中文标点");
    }
}

// ── 方案意图 vs 其它写 `chinese_punct` 的入口 ─────────────────────────
//
// 方案意图只在活跃方案换代时由 `sync_schema_scope` 落地，代际内改写 `chinese_punct` 的入口
// （IME 激活的重置、`follow_mode` 的中英跟随）若不让位就会把它顶回中文且无人纠正。原则：范围
// 更小的意图优先，方案意图压过这两者；用户手动 `toggle_punct` 不在此列（本代际内胜出）。

fn coord_follow(follow: bool) -> std::sync::Arc<Coordinator> {
    let mut c = cfg();
    c.input.punct.follow_mode = follow;
    Coordinator::new_headless(c, Some(&data_dir()))
}

/// IME 激活会把标点重置成全局默认，不能顶掉英文方案自己的英文标点。
#[test]
fn ime_activation_does_not_override_schema_punct_intent() {
    if !ready() {
        eprintln!("跳过：缺少英文方案或它没声明 [punct]");
        return;
    }
    let c = coord();
    c.handle_menu_command("toggle_mode"); // 先在五笔下进英文态
    cycle(&c);
    assert_eq!(c.active_schema_id(), "english");
    assert!(!c.is_chinese_punct(), "前提：英文方案下是英文标点");

    c.handle_ime_activated(1);
    assert!(
        !c.is_chinese_punct(),
        "★ IME 激活的标点重置不得压过方案意图（GH#164）"
    );
}

/// `follow_mode` 下英文方案里中英来回切，标点仍以方案意图为准。
#[test]
fn follow_mode_does_not_override_schema_punct_intent() {
    if !ready() {
        eprintln!("跳过：缺少英文方案或它没声明 [punct]");
        return;
    }
    let c = coord_follow(true);
    cycle(&c);
    assert!(!c.is_chinese_punct(), "前提：英文方案下是英文标点");
    c.handle_menu_command("toggle_mode"); // → 英文态
    c.handle_menu_command("toggle_mode"); // → 中文态
    assert!(c.is_chinese_mode());
    assert!(
        !c.is_chinese_punct(),
        "★ follow_mode 切回中文态不得把英文方案的英文标点顶成中文"
    );
}

/// 上面的优先级只对声明了意图的方案成立：Follow 方案里 follow_mode 照常跟随。
#[test]
fn follow_mode_still_works_in_a_schema_without_intent() {
    if !ready() {
        eprintln!("跳过：缺少英文方案或它没声明 [punct]");
        return;
    }
    let c = coord_follow(true);
    c.handle_menu_command("toggle_mode"); // 五笔英文态
    assert!(!c.is_chinese_punct());
    c.handle_menu_command("toggle_mode"); // 回中文态
    assert!(c.is_chinese_punct(), "无方案意图时 follow_mode 行为不变");
}
