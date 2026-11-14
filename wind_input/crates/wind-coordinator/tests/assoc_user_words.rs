//! 联想两处缺口（论坛 t185）：
//!
//! - **A**：词语联想只查系统词库的反查索引，用户词库 / 临时词在联想里永远不出现。
//! - **B**：满码唯一自动上屏（`auto_commit_at_full`）走 `InputOutcome::AutoCommit` 出口，
//!   绕过了 `commit_selected` 末尾的联想接线 ⇒ 开了自动上屏的五笔用户单字上屏后没有联想。
//!
//! 夹具取楼主真机场景：wubi86，`awsk` = 「荷」（全码唯一），用户词「荷载」（`awfa`，
//! 系统词库里该码只有「花卉」）。
//!
//! 词典缺失时自动跳过（`build_dev/data` 不存在时整族静默跳过而计数照绿，判据是耗时）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
}

fn press(c: &Coordinator, vk: u32) -> KeyAction {
    c.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    })
}

/// 逐字母敲，返回最后一键的动作（满码自动上屏就发生在那一键上）。
fn type_code(c: &Coordinator, code: &str) -> KeyAction {
    let mut last = KeyAction::Consumed;
    for ch in code.chars() {
        last = press(c, (ch.to_ascii_uppercase() as u32) & 0xFF);
    }
    last
}

fn committed(act: &KeyAction) -> Option<&str> {
    match act {
        KeyAction::InsertText { text, .. } => Some(text),
        KeyAction::CommitThenDeferComposition { commit_text, .. } => Some(commit_text),
        _ => None,
    }
}

/// 上屏动作是否**带了占位组合**——联想态能收到后续按键的命门。
fn opened_composition(act: &KeyAction) -> bool {
    match act {
        KeyAction::CommitThenDeferComposition {
            deferred_composition,
            ..
        } => !deferred_composition.is_empty(),
        KeyAction::InsertText {
            new_composition, ..
        } => new_composition.as_deref().is_some_and(|c| !c.is_empty()),
        _ => false,
    }
}

fn coord(
    tag: &str,
    kind: &str,
    at_full: bool,
    seed: impl FnOnce(&wind_store::Store),
) -> (Arc<Coordinator>, Arc<wind_store::Store>) {
    coord_with(tag, kind, at_full, seed, |_| {})
}

fn coord_with(
    tag: &str,
    kind: &str,
    at_full: bool,
    seed: impl FnOnce(&wind_store::Store),
    tweak: impl FnOnce(&mut Config),
) -> (Arc<Coordinator>, Arc<wind_store::Store>) {
    let store_path = std::env::temp_dir().join(format!("wind_assoc_user_{tag}.redb"));
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());
    seed(&store);
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = kind.to_string();
    cfg.input.association.mode = "continuous".to_string();
    cfg.schema.codetable.auto_commit_at_full = at_full;
    // 联想档内按选词次数重排只在调频开着时生效（`assoc_prefix_words` 查 `enabled`）。
    // `Config::default()` 不读 data/config.toml，码表调频默认关，故显式打开。
    cfg.schema.codetable.frequency.enabled = true;
    tweak(&mut cfg);
    let c = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store.clone());
    // 生产由启动后的预热线程建索引（系统反查索引 + 用户词文本索引），headless 不跑那个线程。
    c.prewarm_indexes();
    (c, store)
}

/// ★ B：满码唯一自动上屏之后要进联想态（与手动选词上屏一致）。
#[test]
fn auto_commit_at_full_enters_assoc() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (c, _) = coord("b_on", "word", true, |_| {});
    let act = type_code(&c, "awsk");
    assert_eq!(
        committed(&act),
        Some("荷"),
        "前提：awsk 满码唯一自动上屏「荷」"
    );
    assert!(
        opened_composition(&act),
        "自动上屏后该挂占位组合进联想态，实得 {act:?}"
    );
    let texts = c.debug_assoc_texts();
    assert!(
        !texts.is_empty() && texts.iter().all(|t| t.starts_with('荷')),
        "联想候选应是以「荷」开头的词，实得 {texts:?}"
    );
}

/// B 的反向对照：联想关着时自动上屏照旧纯上屏，不挂占位组合。
#[test]
fn auto_commit_at_full_respects_assoc_off() {
    if !dict_ready() {
        return;
    }
    let (c, _) = coord("b_off", "off", true, |_| {});
    let act = type_code(&c, "awsk");
    assert_eq!(committed(&act), Some("荷"));
    assert!(!opened_composition(&act), "联想关着不该挂占位组合：{act:?}");
    assert!(c.debug_assoc_texts().is_empty());
}

/// ★ A：用户词参与词语联想，且排在系统词之前。
#[test]
fn user_word_joins_word_assoc_ahead_of_system() {
    if !dict_ready() {
        return;
    }
    let (c, _) = coord("a_user", "word", false, |s| {
        s.add_user_word("wubi86", "awfa", "荷载", 0, 0).unwrap();
    });
    type_code(&c, "awsk");
    let act = press(&c, 0x20);
    assert_eq!(committed(&act), Some("荷"));
    let texts = c.debug_assoc_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("荷载"),
        "用户词应排在联想首位，实得 {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "荷花"),
        "系统词仍在联想里（排在用户词之后），实得 {texts:?}"
    );
}

/// ★ A：运行中新增的用户词不必重启就能进联想（写代次过期 → 后台重建）。
///
/// 过期时本次联想先用旧索引（不在按键线程上重建），重建完成后下一次上屏即可见。
#[test]
fn user_word_added_at_runtime_joins_word_assoc() {
    if !dict_ready() {
        return;
    }
    let (c, store) = coord("a_fresh", "word", false, |_| {});
    store.add_user_word("wubi86", "awfa", "荷载", 0, 0).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        press(&c, 0x1B); // Esc 退出上一轮联想
        type_code(&c, "awsk");
        press(&c, 0x20);
        if c.debug_assoc_texts().first().map(String::as_str) == Some("荷载") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "新增用户词 5 秒内仍未进联想：{:?}",
            c.debug_assoc_texts()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// ★ A：临时词也参与（排在系统词之后补位）。
#[test]
fn temp_word_joins_word_assoc() {
    if !dict_ready() {
        return;
    }
    let (c, _) = coord("a_temp", "word", false, |s| {
        s.add_user_word("wubi86", "awfa", "荷载", 0, 0).unwrap();
        s.learn_temp_word("wubi86", "awfx", "荷载组合", 10, 0)
            .unwrap();
    });
    // 选「荷载」：awfa 下候选〔花卉 / 荷载〕里找它的位置。
    type_code(&c, "awfa");
    let idx = c
        .debug_page_texts()
        .iter()
        .position(|t| t == "荷载")
        .expect("前提：awfa 下有用户词「荷载」");
    let act = press(&c, 0x31 + idx as u32);
    assert_eq!(committed(&act), Some("荷载"));
    let texts = c.debug_assoc_texts();
    assert!(
        texts.iter().any(|t| t == "荷载组合"),
        "临时词应出现在联想里，实得 {texts:?}"
    );
}

/// 上屏「荷」（awsk + 空格）后的联想列表。
fn assoc_after_he(c: &Coordinator) -> Vec<String> {
    press(c, 0x1B);
    type_code(c, "awsk");
    let act = press(c, 0x20);
    assert_eq!(committed(&act), Some("荷"));
    c.debug_assoc_texts()
}

/// ★ 方案 A：选词次数（FREQ）在档内重排。系统序「荷兰(1633) > 荷花(1298)」，
/// 用户平时打过十次「荷花」⇒ 联想里「荷花」到「荷兰」前面。
#[test]
fn freq_reranks_system_words_within_tier() {
    if !dict_ready() {
        return;
    }
    let (c, _) = coord("f_base", "word", false, |_| {});
    let base = assoc_after_he(&c);
    assert_eq!(
        base.first().map(String::as_str),
        Some("荷兰"),
        "反向对照：无词频时 {base:?}"
    );

    let (c, _) = coord("f_used", "word", false, |s| {
        for _ in 0..10 {
            s.record_freq("wubi86", "awaw", "荷花").unwrap();
        }
    });
    let got = assoc_after_he(&c);
    assert_eq!(
        got.first().map(String::as_str),
        Some("荷花"),
        "实得 {got:?}"
    );
    assert!(got.iter().any(|t| t == "荷兰"), "荷兰仍在：{got:?}");
}

/// 反向对照：调频关、只记次数（`record_always`，论坛 t278）⇒ 次数**不得**改联想顺序。
///
/// 写端放开后库里就有了数据；联想打分若不查 `enabled`，用户「只想记、不想调」的
/// 联想顺序会被悄悄改掉。
#[test]
fn record_always_freq_does_not_rerank_assoc() {
    if !dict_ready() {
        return;
    }
    let (c, _) = coord_with(
        "f_record_only",
        "word",
        false,
        |s| {
            for _ in 0..10 {
                s.record_freq("wubi86", "awaw", "荷花").unwrap();
            }
        },
        |cfg| {
            cfg.schema.codetable.frequency.enabled = false;
            cfg.schema.codetable.frequency.record_always = true;
        },
    );
    let got = assoc_after_he(&c);
    assert_eq!(
        got.first().map(String::as_str),
        Some("荷兰"),
        "只记次数时联想应保持系统序：{got:?}"
    );
}

/// ★ 临时词门槛：count=1 只在系统词之后补位；count≥2 进个人档（用户词之后、系统词之前）。
#[test]
fn temp_word_needs_reuse_to_enter_personal_tier() {
    if !dict_ready() {
        return;
    }
    let (c, _) = coord("t_once", "word", false, |s| {
        s.learn_temp_word("wubi86", "awgg", "荷叶田田", 800, 0)
            .unwrap();
    });
    let got = assoc_after_he(&c);
    // 「荷」的系统延长词只有 8 条，第 9 个名额空着 ⇒ count=1 的临时词可以补位，但只能在最后。
    let pos = got.iter().position(|t| t == "荷叶田田");
    assert!(
        pos.is_none_or(|p| p == got.len() - 1 && got[..p].iter().any(|t| t == "荷兰")),
        "count=1 只能排在全部系统词之后补位：{got:?}"
    );

    let (c, _) = coord("t_twice", "word", false, |s| {
        s.add_user_word("wubi86", "awfa", "荷载", 0, 0).unwrap();
        s.learn_temp_word("wubi86", "awgg", "荷叶田田", 800, 0)
            .unwrap();
        s.learn_temp_word("wubi86", "awgg", "荷叶田田", 800, 0)
            .unwrap();
    });
    let got = assoc_after_he(&c);
    assert_eq!(
        got.iter().take(2).map(String::as_str).collect::<Vec<_>>(),
        vec!["荷载", "荷叶田田"],
        "用户词 → count≥2 临时词 → 系统词，实得 {got:?}"
    );
}

/// ★ 自动造词噪声（一堆 count=1 的临时词）不挤掉系统词；个人档内按 count 而非字典序排。
#[test]
fn temp_noise_does_not_displace_system_words() {
    if !dict_ready() {
        return;
    }
    let (c, _) = coord("t_noise", "word", false, |s| {
        for i in 0..12 {
            s.learn_temp_word("wubi86", "awzz", &format!("荷噪{i:02}"), 800, 0)
                .unwrap();
        }
        // 两条 count≥2：「荷乙」用得更多，应排在字典序靠前的「荷甲」之前。
        for _ in 0..2 {
            s.learn_temp_word("wubi86", "awyy", "荷甲词", 800, 0)
                .unwrap();
        }
        for _ in 0..4 {
            s.learn_temp_word("wubi86", "awyy", "荷乙词", 800, 0)
                .unwrap();
        }
    });
    let got = assoc_after_he(&c);
    assert!(
        !got.iter().any(|t| t.starts_with("荷噪")),
        "噪声挤进来了：{got:?}"
    );
    assert!(got.iter().any(|t| t == "荷兰"), "系统词被挤掉：{got:?}");
    assert_eq!(
        got.iter().take(2).map(String::as_str).collect::<Vec<_>>(),
        vec!["荷乙词", "荷甲词"],
        "个人档按 count 降序，实得 {got:?}"
    );
}

/// 在联想态里选中 `word`（按它在当前页的位置按数字键），返回上屏文本。
fn pick_assoc(c: &Coordinator, word: &str) -> String {
    let idx = c
        .debug_page_texts()
        .iter()
        .position(|t| t == word)
        .unwrap_or_else(|| panic!("前提：联想当前页有「{word}」：{:?}", c.debug_page_texts()));
    let act = press(c, 0x31 + idx as u32);
    committed(&act).unwrap_or_default().to_string()
}

/// ★ 方案 B（History）：在「荷」之后选过两次「荷枪实弹」，第三次上屏「荷」后它排第一，且不重复出现。
#[test]
fn assoc_history_promotes_picked_word() {
    if !dict_ready() {
        return;
    }
    let (c, _) = coord("h_pick", "word", false, |_| {});
    let base = assoc_after_he(&c);
    let base_pos = base.iter().position(|t| t == "荷枪实弹");
    assert!(
        base_pos.is_some_and(|p| p > 0),
        "前提：系统序里它不在首位 {base:?}"
    );
    assert_eq!(pick_assoc(&c, "荷枪实弹"), "枪实弹", "只补剩余部分");
    assoc_after_he(&c);
    pick_assoc(&c, "荷枪实弹");
    let got = assoc_after_he(&c);
    assert_eq!(
        got.first().map(String::as_str),
        Some("荷枪实弹"),
        "实得 {got:?}"
    );
    assert_eq!(
        got.iter().filter(|t| *t == "荷枪实弹").count(),
        1,
        "去重：{got:?}"
    );
}

/// History 只记在**它的上文**下：「荷」之后选的，不影响别的上文。
#[test]
fn assoc_history_is_keyed_by_context() {
    if !dict_ready() {
        return;
    }
    let (c, store) = coord("h_ctx", "word", false, |_| {});
    assoc_after_he(&c);
    pick_assoc(&c, "荷枪实弹");
    let rows = store.assoc_history("wubi86", "荷", 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "荷枪实弹");
    assert!(
        store
            .assoc_history("wubi86", "荷枪", 10)
            .unwrap()
            .is_empty()
    );
}

/// ★ History 读端过滤存在性：从补位档选过的临时词经 History 前置；删掉它之后不再出现
/// （历史表本身不随删词级联清理）。
#[test]
fn assoc_history_drops_deleted_words() {
    if !dict_ready() {
        return;
    }
    let (c, store) = coord("h_del", "word", false, |s| {
        s.learn_temp_word("wubi86", "awgg", "荷叶田田", 800, 0)
            .unwrap();
    });
    let got = assoc_after_he(&c);
    assert_eq!(
        got.last().map(String::as_str),
        Some("荷叶田田"),
        "前提：count=1 的临时词只在补位档 {got:?}"
    );
    // 它在第二页：PageDown 翻过去再选。
    press(&c, 0x22);
    pick_assoc(&c, "荷叶田田");
    let got = assoc_after_he(&c);
    assert_eq!(
        got.first().map(String::as_str),
        Some("荷叶田田"),
        "前提：选过一次后经 History 前置 {got:?}"
    );
    assert_eq!(
        store.get_temp_word("wubi86", "awgg", "荷叶田田").unwrap(),
        Some(1),
        "选联想候选不给临时词记次数（它在别的上文下仍是补位档）"
    );

    store
        .remove_temp_word("wubi86", "awgg", "荷叶田田")
        .unwrap();
    // 生产里删词后用户词索引在后台重建；这里同步建好再看。
    c.prewarm_indexes();
    let got = assoc_after_he(&c);
    assert!(
        !got.iter().any(|t| t == "荷叶田田"),
        "删掉的临时词仍从 History 冒出来：{got:?}"
    );
    assert!(
        !store.assoc_history("wubi86", "荷", 10).unwrap().is_empty(),
        "反向对照：历史行还在，是读端挡住的"
    );
}

/// ★ History 先过滤存在性再截名额：一批已删掉（词库里没有）的高分历史词不得挤掉
/// 排在它们后面的有效历史。旧实现先按 limit 截断再过滤，名额被死词永久占住。
#[test]
fn assoc_history_deleted_words_do_not_hold_slots() {
    if !dict_ready() {
        return;
    }
    let (c, _) = coord("h_slots", "word", false, |s| {
        for i in 0..20 {
            for _ in 0..3 {
                s.record_assoc_pick("wubi86", "荷", &format!("荷已删{i:02}"))
                    .unwrap();
            }
        }
        s.record_assoc_pick("wubi86", "荷", "荷枪实弹").unwrap();
    });
    let got = assoc_after_he(&c);
    assert!(
        !got.iter().any(|t| t.starts_with("荷已删")),
        "词库里没有的历史词不该出现：{got:?}"
    );
    assert_eq!(
        got.first().map(String::as_str),
        Some("荷枪实弹"),
        "有效历史应经 History 前置，实得 {got:?}"
    );
}

/// ★ 拼音方案：FREQ 的键是候选码（无分隔的全拼，如 `zhongguowenhua`），联想查 FREQ 用的
/// 反查索引码与之同形 ⇒ 正常打字选过的词在联想里上浮。
#[test]
fn pinyin_freq_reranks_assoc() {
    if !data_dir()
        .join("schemas/pinyin/rime_frost.dict.yaml")
        .exists()
    {
        return;
    }
    let store_path = std::env::temp_dir().join("wind_assoc_user_py_freq.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.schema.pinyin.frequency.enabled = true;
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = "word".into();
    cfg.input.association.mode = "continuous".into();
    let c = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    c.prewarm_indexes();
    let after_zhongguo = |c: &Coordinator| {
        press(c, 0x1B);
        type_code(c, "zhongguo");
        let act = press(c, 0x20);
        assert_eq!(committed(&act), Some("中国"));
        c.debug_assoc_texts()
    };
    let base = after_zhongguo(&c);
    let target = "中国文化";
    assert!(
        base.iter().position(|t| t == target).is_some_and(|p| p > 0),
        "前提：系统序里「{target}」在联想页上但不在首位 {base:?}"
    );
    for _ in 0..3 {
        press(&c, 0x1B);
        type_code(&c, "zhongguowenhua");
        let idx = c
            .debug_page_texts()
            .iter()
            .position(|t| t == target)
            .expect("前提：zhongguowenhua 首页有「中国文化」");
        let act = press(&c, 0x31 + idx as u32);
        assert_eq!(committed(&act), Some(target));
    }
    let got = after_zhongguo(&c);
    assert_eq!(
        got.first().map(String::as_str),
        Some(target),
        "正常打字选过的词应在联想里上浮：{got:?}"
    );
}
