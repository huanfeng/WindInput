//! 方案级短语加载（`[phrases]`）的端到端测试。
//!
//! 设计见 `docs/design/schema-scoped-behavior.md` §6。
//!
//! # ★★ 这组测试真正要钉住的是「六个消费点一个都没漏」
//!
//! 短语查询有六个消费点：两处候选生成（`lookup` / `lookup_prefix`）、两处「这个码位归短语
//! 管」的判据（`phrase_owns_code` 内含 `has_exact_code` + `has_longer_code`、
//! `phrase_has_exact_code`）、临英两处。
//!
//! **只漏掉 `phrase_owns_code` 的表现最刁钻**：短语候选不出现了（前两处已过滤），但顶码与
//! 全码自动上屏仍被短语层否决 ⇒ **打字卡住不上屏，且零日志**。所以本文件里
//! `phrases_off_does_not_veto_top_code` 那条比「候选里没有短语」那条重要得多——
//! 后者一眼能看出来，前者只会被用户描述成「有时候打字就卡住了」。
//!
//! 编译期强制（`PhraseScope` 是必填参数）挡的是「新增查询方法时忘了过滤」；
//! 本文件挡的是「传了 scope 但传错了口径」。两者互补，缺一不可。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_phrase::PhraseSeed;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_wubi() -> bool {
    data_dir().join("schemas/wubi86.schema.toml").exists()
}

fn key(vk: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 造一个只含 `[phrases]` 段的方案 override。
fn phrases_override(tag: &str, schema_id: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_phscope_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{schema_id}.toml")),
        format!("[phrases]\n{body}\n"),
    )
    .unwrap();
    dir
}

fn cfg() -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c
}

fn seed(code: &str, text: &str, category: &str) -> PhraseSeed {
    PhraseSeed {
        code: code.into(),
        text: text.into(),
        weight: 5000,
        position: 0,
        is_system: true,
        category: category.into(),
    }
}

/// 装一批带分类的短语。`a`/`aa` 刻意与五笔码位重叠，用于观察顶码是否被否决。
fn install(coord: &Coordinator) {
    coord.debug_install_phrases(vec![
        seed("aa", "未分类短语", ""),
        seed("aab", "更长的未分类", ""),
        seed("ab", "工作短语", "工作"),
        seed("ac", "日期短语", "日期"),
    ]);
}

fn coord_with(tag: &str, body: &str) -> Arc<Coordinator> {
    let ov = phrases_override(tag, "wubi86", body);
    Coordinator::new_headless_with_override(cfg(), Some(&data_dir()), Some(ov))
}

fn page_contains(coord: &Coordinator, text: &str) -> bool {
    coord.debug_page_texts().iter().any(|t| t == text)
}

/// 打一串码（小写字母）。
fn type_code(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

// ───────────────────── enabled ─────────────────────

/// 默认（不写 `[phrases]`）全部加载——这是本功能的零回归基线。
#[test]
fn phrases_load_by_default() {
    if !has_wubi() {
        eprintln!("跳过：缺少 wubi86 方案");
        return;
    }
    let coord = coord_with("default", "# 空段，全部默认");
    install(&coord);
    type_code(&coord, "aa");
    assert!(
        page_contains(&coord, "未分类短语"),
        "默认应加载全部短语，实得 {:?}",
        coord.debug_page_texts()
    );
}

/// `enabled = false` ⇒ 短语候选整体消失。
#[test]
fn phrases_off_removes_all_phrase_candidates() {
    if !has_wubi() {
        eprintln!("跳过：缺少 wubi86 方案");
        return;
    }
    let coord = coord_with("off", "enabled = false");
    install(&coord);
    type_code(&coord, "aa");
    let page = coord.debug_page_texts();
    assert!(
        !page.iter().any(|t| t.contains("短语")),
        "关掉后不应有任何短语候选，实得 {page:?}"
    );
}

/// ★★ `enabled = false` ⇒ 短语**不再否决顶码**。
///
/// 这是六个消费点里最危险的一处（`phrase_owns_code`）。漏了它的表现不是「短语还在」，
/// 而是**打字卡住不上屏**：候选面已经没有短语了，但顶码路径仍以为「这个码位归短语管」，
/// 于是拒绝顶码、也不出候选，用户看到的是按键毫无反应。
///
/// 判据用「有没有上屏」而不是「候选里有没有短语」——后者在漏接时**照样通过**。
#[test]
fn phrases_off_does_not_veto_top_code() {
    if !has_wubi() {
        eprintln!("跳过：缺少 wubi86 方案");
        return;
    }
    // 顶码默认关（出厂 true、Config::default() false），必须显式打开，否则测的是关着的功能。
    let mut c = cfg();
    c.schema.codetable.top_code_commit = true;
    let ov = phrases_override("veto", "wubi86", "enabled = false");
    let coord = Coordinator::new_headless_with_override(c, Some(&data_dir()), Some(ov));
    // 装一条**比满码长**的短语：它在码表侧既无精确匹配也无更长后继，正是顶码最爱命中的
    // 形态，也正是 `phrase_owns_code` 存在的理由。
    coord.debug_install_phrases(vec![seed("aaaaa", "五码短语", "")]);

    // 打满 4 码 + 第 5 码：短语层若仍在管这个码位，第 5 码会被否决顶码。
    type_code(&coord, "aaaa");
    let before = coord.debug_page_texts();
    assert!(
        !before.iter().any(|t| t == "五码短语"),
        "关掉后候选里不该有短语（这条是前置条件，不是本用例的重点）：{before:?}"
    );
    // 关键断言：`aaaaa` 这个码位不再归短语管。
    assert!(
        !coord.debug_phrase_owns_code("aaaa"),
        "enabled = false 后短语层不得再声称拥有该码位——否则顶码与自动上屏被静默否决，\
         用户看到的是「打字卡住不上屏」且没有任何日志"
    );
    assert!(
        !coord.debug_phrase_owns_code("aaaaa"),
        "精确码同理（has_exact_code 那一半）"
    );
}

/// 反向对照：开着时短语**确实**拥有该码位。
///
/// 没有这条，「恒不否决」与「按开关否决」两种实现都能让上面那条通过。
#[test]
fn phrases_on_does_veto_top_code() {
    if !has_wubi() {
        eprintln!("跳过：缺少 wubi86 方案");
        return;
    }
    let coord = coord_with("veto_on", "# 默认开");
    coord.debug_install_phrases(vec![seed("aaaaa", "五码短语", "")]);
    assert!(
        coord.debug_phrase_owns_code("aaaa"),
        "开着时 `aaaa` 还能续打成 `aaaaa`，该码位归短语管（has_longer_code 那一半）"
    );
    assert!(coord.debug_phrase_owns_code("aaaaa"), "精确码同理");
}

// ───────────────────── categories ─────────────────────

/// 白名单：只加载列出的分类。
#[test]
fn categories_whitelist() {
    if !has_wubi() {
        eprintln!("跳过：缺少 wubi86 方案");
        return;
    }
    let coord = coord_with("white", r#"categories = ["工作"]"#);
    install(&coord);
    type_code(&coord, "ab");
    assert!(
        page_contains(&coord, "工作短语"),
        "白名单内的应加载，实得 {:?}",
        coord.debug_page_texts()
    );
    assert!(
        !coord.debug_phrase_owns_code("ac"),
        "白名单外的分类不该再占码位"
    );
    assert!(
        !coord.debug_phrase_owns_code("aa"),
        "★ 未分类（category == \"\"）不在白名单里 ⇒ 一并被滤掉。\
         这正是「分类 UI 落地前所有存量短语都是未分类」的那个坑"
    );
}

/// ★ 空串 `""` 显式匹配未分类短语。
///
/// 不引入 `default` 之类的映射名：存储层本来就是空串，多一层映射就多一处要对齐的地方。
#[test]
fn empty_category_string_matches_uncategorized() {
    if !has_wubi() {
        eprintln!("跳过：缺少 wubi86 方案");
        return;
    }
    let coord = coord_with("uncat", r#"categories = ["", "工作"]"#);
    install(&coord);
    assert!(coord.debug_phrase_owns_code("aa"), "空串应匹配未分类");
    assert!(coord.debug_phrase_owns_code("ab"), "同时列出的分类也要在");
    assert!(!coord.debug_phrase_owns_code("ac"), "未列出的仍被滤掉");
}

/// 黑名单在白名单之后再减。
#[test]
fn exclude_subtracts_after_whitelist() {
    if !has_wubi() {
        eprintln!("跳过：缺少 wubi86 方案");
        return;
    }
    let coord = coord_with("excl", r#"exclude_categories = ["日期"]"#);
    install(&coord);
    assert!(coord.debug_phrase_owns_code("aa"), "未分类不受影响");
    assert!(coord.debug_phrase_owns_code("ab"), "其它分类不受影响");
    assert!(!coord.debug_phrase_owns_code("ac"), "被排除的分类滤掉");
}

/// ★ 空列表 = **不施加这一项限制**，不是「一条都不要」。
///
/// 「一条都不要」由 `enabled = false` 表达。两个字段语义完全对称，这条钉住的正是那个
/// 「三态里有一态是重复的」的结论（设计文档 §6.2）。
#[test]
fn empty_lists_mean_no_restriction() {
    if !has_wubi() {
        eprintln!("跳过：缺少 wubi86 方案");
        return;
    }
    let coord = coord_with("empty", "categories = []\nexclude_categories = []");
    install(&coord);
    for code in ["aa", "ab", "ac"] {
        assert!(
            coord.debug_phrase_owns_code(code),
            "空列表不该滤掉任何东西，{code} 被滤了"
        );
    }
}
