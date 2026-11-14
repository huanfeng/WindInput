//! 拼音「屏蔽这个词」：右键删多字的系统词 / 整句，按**文字**屏蔽（候选调整里 `code = *` 的删除行），
//! 所有输入串、所有来源都不再出现；删多字用户词 / 临时词则真删所有编码的条目、不留屏蔽记录；
//! 单字与码表仍按编码隐藏。设计见 docs/design/system-dict-patch-and-candidate-adjust.md §4a。
//!
//! 每条用例先断言删除前候选确实在，最终候选一律经 Coordinator 取（引擎位次 ≠ 界面位次）。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时整族静默跳过（判据是耗时 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;
use wind_store::shadow::BLOCK_ALL_CODES;
use wind_ui_types::CandidateOp;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "shuangpin"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
        && d.join("schemas/pinyin/rime_frost.dict.yaml").exists()
}

fn key(k: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn type_keys(coord: &Coordinator, input: &str) {
    for ch in input.chars() {
        let vk = if ch == '\'' {
            0xDE // VK_OEM_7
        } else {
            (ch.to_ascii_uppercase() as u32) & 0xFF
        };
        coord.handle_key_event_policed(&key(vk));
    }
}

fn config(active: &str) -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "shuangpin".into()];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.keys.select_key_groups.clear(); // 出厂 `;'` 选二三候选，会把 `'` 吃掉
    c
}

fn fresh_store(name: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_pyblock_{name}.redb"));
    let _ = std::fs::remove_file(&path); // 库文件不在时 open 会连带清掉派生的文本/状态目录
    Arc::new(Store::open(&path).unwrap())
}

fn open(cfg: &Config, store: &Arc<Store>) -> Arc<Coordinator> {
    Coordinator::new_headless_with_store(cfg.clone(), Some(&data_dir()), Arc::clone(store))
}

/// 新起一个协调器打 `input`，返回全部候选文本。
fn candidates(cfg: &Config, store: &Arc<Store>, input: &str) -> Vec<String> {
    let coord = open(cfg, store);
    type_keys(&coord, input);
    coord.debug_all_candidate_texts()
}

fn has(cfg: &Config, store: &Arc<Store>, input: &str, word: &str) -> bool {
    candidates(cfg, store, input).iter().any(|t| t == word)
}

/// 打 `input`，右键删首页上的 `want`。
fn delete_on_page(cfg: &Config, store: &Arc<Store>, input: &str, want: &str) {
    let coord = open(cfg, store);
    type_keys(&coord, input);
    let page = coord.debug_page_texts();
    let idx = page
        .iter()
        .position(|t| t == want)
        .unwrap_or_else(|| panic!("前提：`{input}` 首页应有「{want}」，实际 {page:?}"));
    coord.debug_candidate_op(CandidateOp::Delete, idx);
}

/// 打 `input` 后按空格，返回 (上屏文本, 上屏后剩下的缓冲)。
fn space_commit(cfg: &Config, store: &Arc<Store>, input: &str) -> (String, String) {
    let coord = open(cfg, store);
    type_keys(&coord, input);
    let text = match coord.handle_key_event_policed(&key(0x20)) {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("`{input}` 空格应上屏，实际 {other:?}"),
    };
    (text, coord.debug_input_buffer())
}

/// 翻页找候选并选中，返回上屏文本（None = 仍在组合区）。
fn pick(coord: &Coordinator, want: &str) -> Option<String> {
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if t.is_empty() {
            return None;
        }
        if let Some(p) = t.iter().position(|x| x == want) {
            return match coord.handle_key_event_policed(&key(0x31 + p as u32)) {
                KeyAction::InsertText { text, .. } => Some(text),
                _ => None,
            };
        }
        coord.handle_key_event_policed(&key(0x22)); // PageDown
    }
    None
}

fn blocked(store: &Store) -> Vec<String> {
    let mut v: Vec<String> = store
        .blocked_words("pinyin")
        .unwrap()
        .iter()
        .cloned()
        .collect();
    v.sort();
    v
}

/// 全拼删多字系统词：写一条 `*` 屏蔽行（不写按编码的行），同码、带分隔符的另一种拼法、模糊音、
/// 简拼、混合简拼下都不再出现。
#[test]
fn pinyin_delete_system_word_blocks_everywhere() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut cfg = config("pinyin");
    cfg.schema.pinyin.fuzzy.enabled = true;
    cfg.schema.pinyin.fuzzy.n_l = true;
    let store = fresh_store("full");
    for input in ["nihao", "ni'hao", "lihao", "nh", "nhao"] {
        assert!(
            has(&cfg, &store, input, "你好"),
            "前提：删前 `{input}` 有「你好」"
        );
    }
    delete_on_page(&cfg, &store, "nihao", "你好");
    assert_eq!(blocked(&store), vec!["你好".to_string()]);
    assert!(
        store.get_shadow_rules("pinyin", "nihao").unwrap().is_none(),
        "不写按编码的删除行"
    );
    for input in ["nihao", "ni'hao", "lihao", "nh", "nhao"] {
        let after = candidates(&cfg, &store, input);
        assert!(
            !after.iter().any(|t| t == "你好"),
            "`{input}` 不应再有「你好」，实际 {after:?}"
        );
    }
}

/// 单字仍只在当前编码下隐藏。
#[test]
fn pinyin_delete_single_char_stays_per_code() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("pinyin");
    let store = fresh_store("single");
    assert!(has(&cfg, &store, "n", "你"), "前提：`n` 有「你」");
    delete_on_page(&cfg, &store, "ni", "你");
    assert!(blocked(&store).is_empty(), "单字不按文字屏蔽");
    assert_eq!(
        store
            .get_shadow_rules("pinyin", "ni")
            .unwrap()
            .map(|r| r.deleted),
        Some(vec!["你".to_string()])
    );
    assert!(!has(&cfg, &store, "ni", "你"));
    assert!(has(&cfg, &store, "n", "你"), "别的编码不受影响");
}

/// 双拼（小鹤 `nihc` = ni hao）删除同样按文字生效，与全拼共用。
#[test]
fn shuangpin_delete_blocks_by_text() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let sp = config("shuangpin");
    let store = fresh_store("sp");
    assert!(has(&sp, &store, "nihc", "你好"), "前提");
    delete_on_page(&sp, &store, "nihc", "你好");
    assert_eq!(blocked(&store), vec!["你好".to_string()]);
    assert!(!has(&sp, &store, "nihc", "你好"));
    let full = config("pinyin");
    assert!(!has(&full, &store, "nihao", "你好"), "全拼同样屏蔽");
    assert!(!has(&full, &store, "nh", "你好"), "简拼同样屏蔽");
}

/// 整句文字恰为被屏蔽的词（`nihao` 拼出的「你好」）：首选换成另一条**覆盖整串**的解读，
/// 空格上屏后缓冲清空，不是只上半截。编码栏仍是整串的切分。
#[test]
fn blocked_sentence_top_still_covers_whole_input() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("pinyin");
    let store = fresh_store("sentence");
    assert_eq!(space_commit(&cfg, &store, "nihao").0, "你好", "前提");
    store.block_word("pinyin", "你好").unwrap();
    let coord = open(&cfg, &store);
    type_keys(&coord, "nihao");
    assert_eq!(coord.debug_preedit(), "ni'hao", "编码栏是整串切分");
    let (text, rest) = space_commit(&cfg, &store, "nihao");
    assert_ne!(text, "你好");
    assert_eq!(text.chars().count(), 2, "首选覆盖两个音节，实际「{text}」");
    assert!(rest.is_empty(), "空格上屏整串，缓冲应清空，实际剩 `{rest}`");
    // 作为更长整句的一段、由更小的块拼出，不拦
    let (long, rest) = space_commit(&cfg, &store, "nihaoshijie");
    assert!(rest.is_empty());
    assert_eq!(long, "你好世界", "被屏蔽词作为更长整句的一段不拦");
}

/// 分段上屏拼出被屏蔽的词：上屏照常，但不学进临时词库 / 用户词库。
#[test]
fn segmented_commit_of_blocked_word_is_not_learned() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut cfg = config("pinyin");
    cfg.schema.pinyin.auto_learn.enabled = true;
    cfg.schema.pinyin.auto_learn.promote_count = 1;
    let store = fresh_store("seg");
    // 对照：没屏蔽时分段上屏「你」「好」学得进去
    let coord = open(&cfg, &store);
    type_keys(&coord, "nihao");
    assert!(pick(&coord, "你").is_none(), "选「你」应留在组合区分步");
    assert_eq!(pick(&coord, "好").as_deref(), Some("你好"));
    assert!(
        store
            .get_user_words("pinyin", "nihao")
            .unwrap()
            .iter()
            .any(|r| r.text == "你好"),
        "前提：分步组词 + promote_count=1 应自动晋升进用户词库"
    );

    let store = fresh_store("seg_blocked");
    store.block_word("pinyin", "你好").unwrap();
    let coord = open(&cfg, &store);
    type_keys(&coord, "nihao");
    assert!(pick(&coord, "你").is_none());
    assert_eq!(pick(&coord, "好").as_deref(), Some("你好"), "上屏照常");
    assert!(store.get_user_words("pinyin", "nihao").unwrap().is_empty());
    assert!(store.get_temp_words("pinyin", "nihao").unwrap().is_empty());
    assert!(!has(&cfg, &store, "nihao", "你好"));
}

/// 删多字用户词：所有编码下的用户 / 临时条目都删掉，词频与联想记录清掉，并屏蔽这个词
/// （只删不屏蔽，分段上屏或组句又会把它造回来）。
#[test]
fn delete_user_word_purges_all_codes() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("pinyin");
    let store = fresh_store("user");
    store
        .add_user_word("pinyin", "nihao", "倪豪", 50_000_000, 0b101)
        .unwrap();
    store
        .add_user_word("pinyin", "lihao", "倪豪", 50_000_000, 0b101)
        .unwrap();
    store
        .learn_temp_word("pinyin", "nihaoa", "倪豪", 10, 0)
        .unwrap();
    store.record_freq("pinyin", "nihao", "倪豪").unwrap();
    store.record_assoc_pick("pinyin", "倪豪", "先生").unwrap();
    delete_on_page(&cfg, &store, "nihao", "倪豪");
    for code in ["nihao", "lihao"] {
        assert!(
            !store
                .get_user_words("pinyin", code)
                .unwrap()
                .iter()
                .any(|r| r.text == "倪豪"),
            "`{code}` 下的用户词应删掉"
        );
    }
    assert!(store.get_temp_words("pinyin", "nihaoa").unwrap().is_empty());
    assert!(store.get_freq("pinyin", "nihao", "倪豪").unwrap().is_none());
    assert!(store.assoc_history("pinyin", "倪豪", 0).unwrap().is_empty());
    assert_eq!(blocked(&store), vec!["倪豪".to_string()], "同时屏蔽");
    assert_eq!(
        store.list_shadow_rules("pinyin").unwrap().len(),
        1,
        "只有那条 `*` 行"
    );
    assert!(!has(&cfg, &store, "lihao", "倪豪"));
}

/// 合并候选（系统词 + 同文用户词）：真删用户那条并屏蔽，系统那条也不再出现。
#[test]
fn delete_merged_candidate_purges_and_blocks() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("pinyin");
    let store = fresh_store("merged");
    store
        .add_user_word("pinyin", "nihao", "你好", 0, 0b101)
        .unwrap();
    delete_on_page(&cfg, &store, "nihao", "你好");
    assert!(store.get_user_words("pinyin", "nihao").unwrap().is_empty());
    assert_eq!(blocked(&store), vec!["你好".to_string()]);
    assert!(!has(&cfg, &store, "nihao", "你好"), "系统那条也不出现");
    assert!(!has(&cfg, &store, "nh", "你好"));
}

/// GH#269 的形态：选过的整句学进了临时词库，删掉它后候选消失，再分段上屏也不会学回来。
#[test]
fn delete_learned_sentence_stays_gone() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut cfg = config("pinyin");
    cfg.schema.pinyin.auto_learn.enabled = true;
    cfg.schema.pinyin.auto_learn.promote_count = 0; // 留在临时词库
    let store = fresh_store("learned");
    let input = "tazaijialikanshu";
    let word = "他在家里看书"; // 词库里没有，整句合成
    let learned = |store: &Store| {
        let mut n = 0;
        store
            .for_each_temp_word("pinyin", "", &mut |w| {
                n += usize::from(w.text == word);
                true
            })
            .unwrap();
        n
    };
    let coord = open(&cfg, &store);
    type_keys(&coord, input);
    assert_eq!(pick(&coord, word).as_deref(), Some(word), "前提：整句可选");
    assert_eq!(learned(&store), 1, "前提：选中的合成整句学进临时词库");

    delete_on_page(&cfg, &store, input, word);
    assert_eq!(learned(&store), 0, "临时词真删");
    assert_eq!(blocked(&store), vec![word.to_string()], "并屏蔽");
    let after = candidates(&cfg, &store, input);
    assert!(!after.iter().any(|t| t == word), "候选消失，实际 {after:?}");
    let (text, rest) = space_commit(&cfg, &store, input);
    assert!(rest.is_empty(), "首选仍覆盖整串，剩 `{rest}`");
    assert_ne!(text, word);

    // 分段上屏又拼出它：上屏照常、不学回
    let coord = open(&cfg, &store);
    type_keys(&coord, input);
    let mut out = String::new();
    for seg in ["他", "在", "家里", "看书"] {
        if let Some(t) = pick(&coord, seg) {
            out = t;
        }
    }
    assert_eq!(out, word, "前提：分段上屏拼出整句");
    assert_eq!(learned(&store), 0, "不再学回");
}

/// 单条显式加词撤销屏蔽（所有编码都回来）；批量导入不撤，结果里按文字计入被隐藏数。
#[test]
fn single_add_unblocks_but_import_only_counts() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("pinyin");
    let store = fresh_store("readd");
    delete_on_page(&cfg, &store, "nihao", "你好");
    assert!(!has(&cfg, &store, "nh", "你好"));

    let coord = open(&cfg, &store);
    let out = wind_coordinator::dict_transfer_api::import(
        &*coord,
        "pinyin",
        "nihao\t你好\t100\nnihao\t倪豪\t100\n",
        false,
        true, // 无边界的拼音码要补边界才收
        None,
    )
    .unwrap();
    let words = out
        .sections
        .iter()
        .find(|s| s.key == "userWords")
        .unwrap_or_else(|| panic!("结果应有 userWords 段，实际 {out:?}"));
    assert_eq!(words.imported, 2, "前提：导入两条，实际 {words:?}");
    assert_eq!(
        words.hidden_by_shadow, 1,
        "被屏蔽的「你好」计入，实际 {words:?}"
    );
    assert_eq!(blocked(&store), vec!["你好".to_string()], "导入不撤屏蔽");
    assert!(!has(&cfg, &store, "nihao", "你好"));

    wind_coordinator::user_words_api::add(&*coord, "pinyin", "ni hao", "你好", 0).unwrap();
    assert!(blocked(&store).is_empty(), "单条加词撤销屏蔽");
    assert!(has(&cfg, &store, "nihao", "你好"));
    assert!(has(&cfg, &store, "nh", "你好"), "别的编码也回来");
}

/// 码表方案的删除不变：按编码隐藏，不写屏蔽行。
#[test]
fn codetable_delete_stays_per_code() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("wubi86");
    let store = fresh_store("ct");
    let coord = open(&cfg, &store);
    type_keys(&coord, "aaaa");
    let page = coord.debug_page_texts();
    let (idx, victim) = page
        .iter()
        .enumerate()
        .find(|(_, t)| t.chars().count() >= 2)
        .map(|(i, t)| (i, t.clone()))
        .unwrap_or_else(|| panic!("前提：`aaaa` 首页应有多字词，实际 {page:?}"));
    coord.debug_candidate_op(CandidateOp::Delete, idx);
    assert_eq!(
        store
            .get_shadow_rules("wubi86", "aaaa")
            .unwrap()
            .map(|r| r.deleted),
        Some(vec![victim.clone()])
    );
    assert!(
        !store
            .list_shadow_rules("wubi86")
            .unwrap()
            .iter()
            .any(|(c, _)| c == BLOCK_ALL_CODES)
    );
    assert!(!has(&cfg, &store, "aaaa", &victim));
}

/// 联想：被屏蔽的整词不出，且是在截断**之前**滤——条数与没屏蔽时一样，空出的格由后面的补上。
#[test]
fn association_skips_blocked_word_before_truncation() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut cfg = config("pinyin");
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = "word".to_string();
    cfg.input.association.mode = "one_shot".to_string();
    let suggest = |store: &Arc<Store>| {
        let coord = open(&cfg, store);
        coord.prewarm_indexes();
        coord.debug_assoc_suggest("中")
    };
    let store = fresh_store("assoc");
    let before = suggest(&store);
    let victim = before
        .iter()
        .find(|t| t.chars().count() >= 2)
        .cloned()
        .unwrap_or_else(|| panic!("前提：「中」应有多字联想，实际 {before:?}"));
    store.block_word("pinyin", &victim).unwrap();
    let after = suggest(&store);
    assert!(
        !after.contains(&victim),
        "联想不应再出「{victim}」，实际 {after:?}"
    );
    assert_eq!(
        after.len(),
        before.len(),
        "先滤后截，条数不少：前 {before:?} 后 {after:?}"
    );

    // 真实上屏路径：选「中」进联想态，同样不出
    let coord = open(&cfg, &store);
    coord.prewarm_indexes();
    type_keys(&coord, "zhong");
    // 进联想态时上屏走的是 `CommitThenDeferComposition`，不是 `InsertText`，返回值不看
    let _ = pick(&coord, "中");
    let shown = coord.debug_assoc_texts();
    assert!(!shown.is_empty(), "前提：上屏后进了联想态");
    assert!(
        !shown.contains(&victim),
        "联想态不应出「{victim}」，实际 {shown:?}"
    );
    assert_eq!(shown.len(), before.len(), "联想态条数不少：{shown:?}");
}
