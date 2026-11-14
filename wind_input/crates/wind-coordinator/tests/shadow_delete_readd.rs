//! 删掉系统词后再**显式**加回同一个词（同编码同文本），候选必须重新出现。
//!
//! 删系统词走候选调整（shadow）的 `delete` 规则，读端 `apply_shadow` 按文本过滤、不看来源，
//! 于是加回来的用户词被同一条规则一并滤掉——「加了词但永远打不出」。修法：显式加词成功后
//! 清掉同 (数据方案, 编码, 文本) 的 `delete` 规则；自动学词 / 自动晋升**不清**（删过的词
//! 不该因为又打到一次就复活），批量导入也不清、只报数。
//!
//! 拼音右键删多字词改为按文字屏蔽（`code = *`），加词同样撤它；按文字的专项用例在
//! `pinyin_block_word.rs`。本文件里验证规则键形态的拼音用例改删单字。
//!
//! 每条用例都先断言「删除确实生效」再加词，否则加词前候选本就在，断言出现等于没测。
//! 最终候选一律经 Coordinator 取（引擎位次 ≠ 界面位次）。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时整族静默跳过（判据是耗时 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;
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
    c.schema.available = vec![
        "wubi86".into(),
        "pinyin".into(),
        "shuangpin".into(),
        "wubi86_pinyin".into(),
    ];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c
}

fn fresh_store(name: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_readd_{name}.redb"));
    let _ = std::fs::remove_file(&path); // 库文件不在时 open 会连带清掉派生的文本/状态目录
    Arc::new(Store::open(&path).unwrap())
}

fn open(cfg: &Config, store: &Arc<Store>) -> Arc<Coordinator> {
    Coordinator::new_headless_with_store(cfg.clone(), Some(&data_dir()), Arc::clone(store))
}

/// 新起一个协调器打 `input`，返回全部候选文本（每次新起，不受上一轮组合态影响）。
fn candidates(cfg: &Config, store: &Arc<Store>, input: &str) -> Vec<String> {
    let coord = open(cfg, store);
    type_keys(&coord, input);
    coord.debug_all_candidate_texts()
}

/// 打 `input`，把首页第 2 条（非首选，免得撞自动上屏）右键删除，返回被删的词与写端作用域。
/// 断言删除已生效——后续「重新出现」的断言才有意义。
fn delete_second(cfg: &Config, store: &Arc<Store>, input: &str) -> (String, (String, String)) {
    delete_on_page(cfg, store, input, None)
}

/// 同 [`delete_second`]，`want` 给定时删首页上的那一条。
fn delete_on_page(
    cfg: &Config,
    store: &Arc<Store>,
    input: &str,
    want: Option<&str>,
) -> (String, (String, String)) {
    let coord = open(cfg, store);
    type_keys(&coord, input);
    let page = coord.debug_page_texts();
    let idx = match want {
        Some(w) => page
            .iter()
            .position(|t| t == w)
            .unwrap_or_else(|| panic!("前提：`{input}` 首页应有「{w}」，实际 {page:?}")),
        None => {
            assert!(
                page.len() >= 2,
                "前提：`{input}` 首页至少两条候选，实际 {page:?}"
            );
            1
        }
    };
    let victim = page[idx].clone();
    let scope = coord
        .debug_candidate_op_scope()
        .expect("前提：主输入路应有候选调整作用域");
    coord.debug_candidate_op(CandidateOp::Delete, idx);
    let after = candidates(cfg, store, input);
    assert!(
        !after.contains(&victim),
        "前提：删除后 `{victim}` 应从 `{input}` 的候选里消失，实际 {after:?}"
    );
    (victim, scope)
}

/// 首页第一个单字候选。拼音删多字词按文字屏蔽（`code = *`，见 `pinyin_block_word.rs`），
/// 只有单字仍按编码记规则——验证规则键形态的用例删它。
fn first_single_char(cfg: &Config, store: &Arc<Store>, input: &str) -> String {
    let coord = open(cfg, store);
    type_keys(&coord, input);
    let page = coord.debug_page_texts();
    page.iter()
        .find(|t| t.chars().count() == 1)
        .cloned()
        .unwrap_or_else(|| panic!("前提：`{input}` 首页应有单字，实际 {page:?}"))
}

/// 拼音桶 `code` 上恰有一条删除规则 `victim`（按编码，不是按文字屏蔽）。
fn assert_rule_at(store: &Store, code: &str, victim: &str) {
    assert_eq!(
        store
            .get_shadow_rules("pinyin", code)
            .unwrap()
            .map(|r| r.deleted),
        Some(vec![victim.to_string()]),
        "规则应写在 `{code}` 上"
    );
    assert!(
        store.blocked_words("pinyin").unwrap().is_empty(),
        "单字不按文字屏蔽"
    );
}

fn assert_back(cfg: &Config, store: &Arc<Store>, input: &str, victim: &str, how: &str) {
    let back = candidates(cfg, store, input);
    assert!(
        back.iter().any(|t| t == victim),
        "{how}之后 `{victim}` 应重新出现在 `{input}` 的候选里，实际 {back:?}"
    );
}

/// 码表：删系统词 → 命令栏 `dict.add` 加回（`try_dict_add`）→ 重新出现。
#[test]
fn codetable_readd_via_dict_add_shows_again() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("wubi86");
    let store = fresh_store("ct_cmd");
    let (victim, _) = delete_second(&cfg, &store, "aa");
    let coord = open(&cfg, &store);
    coord.debug_run_command(&format!(r#"$CC("加词", dict.add("{victim}", "aa"))"#));
    assert!(
        store
            .get_user_words("wubi86", "aa")
            .unwrap()
            .iter()
            .any(|r| r.text == victim),
        "前提：dict.add 应把词写进用户词库"
    );
    assert_back(&cfg, &store, "aa", &victim, "dict.add 加词");
}

/// 码表：设置页加词（`user_words_api::add`）同样要清。
#[test]
fn codetable_readd_via_settings_shows_again() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("wubi86");
    let store = fresh_store("ct_settings");
    let (victim, _) = delete_second(&cfg, &store, "aa");
    let coord = open(&cfg, &store);
    wind_coordinator::user_words_api::add(&*coord, "wubi86", "aa", &victim, 0).unwrap();
    assert_back(&cfg, &store, "aa", &victim, "设置页加词");
}

/// 全拼：shadow 键是击键串本身（`nihao`），与用户词扁平码同形；设置页给带空格的码也要认。
#[test]
fn pinyin_readd_via_settings_shows_again() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("pinyin");
    let store = fresh_store("py_settings");
    let single = first_single_char(&cfg, &store, "nihao");
    let (victim, (schema, code)) = delete_on_page(&cfg, &store, "nihao", Some(&single));
    assert_eq!((schema.as_str(), code.as_str()), ("pinyin", "nihao"));
    assert_rule_at(&store, "nihao", &victim);
    let coord = open(&cfg, &store);
    // 设置页列表/输入框的码带音节空格，存储域是扁平码
    wind_coordinator::user_words_api::add(&*coord, "pinyin", "ni hao", &victim, 0).unwrap();
    assert!(
        store.get_shadow_rules("pinyin", "nihao").unwrap().is_none(),
        "规则已撤"
    );
    assert_back(&cfg, &store, "nihao", &victim, "设置页加词");
}

/// 全拼带手动分隔符：shadow 键是 `ni'hao`（`'` 是硬边界、不归一，见 `ConvertResult::shadow_code`），
/// 用户词码是扁平的 `nihao`。两者须按「去掉分隔符」对齐，否则删在 `ni'hao` 的词加回后照样不出。
#[test]
fn pinyin_readd_after_delete_under_separator() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut cfg = config("pinyin");
    cfg.keys.select_key_groups.clear(); // 出厂 `;'` 选二三候选，会把 `'` 吃掉
    let store = fresh_store("py_sep");
    let single = first_single_char(&cfg, &store, "ni'hao");
    let (victim, (_, code)) = delete_on_page(&cfg, &store, "ni'hao", Some(&single));
    assert_eq!(code, "ni'hao", "前提：全拼 shadow 键保留分隔符");
    assert_rule_at(&store, "ni'hao", &victim);
    let coord = open(&cfg, &store);
    coord.debug_run_command(&format!(r#"$CC("加词", dict.add("{victim}", "nihao"))"#));
    assert!(
        store
            .get_shadow_rules("pinyin", "ni'hao")
            .unwrap()
            .is_none(),
        "`ni'hao` 上的规则去掉分隔符后与 `nihao` 对齐，应已撤"
    );
    assert_back(&cfg, &store, "ni'hao", &victim, "dict.add 加词");
}

/// 双拼（小鹤 `nihc` = ni hao）删系统词：规则键须是读端同款的归一码（连写全拼 `nihao`），
/// 落拼音族共享桶。前：写端用击键 `nihc`，读端用 `nihao`，删了不生效。删单字（多字词按文字屏蔽）。
#[test]
fn shuangpin_delete_writes_normalized_code() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("shuangpin");
    let store = fresh_store("sp_delete");
    let single = first_single_char(&cfg, &store, "nihc");
    let (victim, (_, code)) = delete_on_page(&cfg, &store, "nihc", Some(&single));
    assert_eq!(code, "nihao", "前提：双拼完整转换时作用域码是连写全拼");
    assert_eq!(
        store
            .get_shadow_rules("pinyin", "nihao")
            .unwrap()
            .map(|r| r.deleted),
        Some(vec![victim.clone()])
    );
    assert!(store.get_shadow_rules("pinyin", "nihc").unwrap().is_none());
    // 与全拼共享同一条规则
    let full = candidates(&config("pinyin"), &store, "nihao");
    assert!(
        !full.contains(&victim),
        "全拼 `nihao` 也应隐藏，实际 {full:?}"
    );
}

/// 双拼删系统词 → 设置页加回：用户词码是全拼，与归一后的规则键同域，共用同一条清理；
/// 双拼方案 id 经 `data_schema_id` 折叠到拼音桶后能清到。
#[test]
fn shuangpin_readd_via_settings_shows_again() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("shuangpin");
    let store = fresh_store("sp_settings");
    let single = first_single_char(&cfg, &store, "nihc");
    let (victim, _) = delete_on_page(&cfg, &store, "nihc", Some(&single));
    assert_rule_at(&store, "nihao", &victim);
    let coord = open(&cfg, &store);
    wind_coordinator::user_words_api::add(&*coord, "shuangpin", "nihao", &victim, 0).unwrap();
    assert!(
        store.get_shadow_rules("pinyin", "nihao").unwrap().is_none(),
        "规则已撤"
    );
    assert_back(&cfg, &store, "nihc", &victim, "设置页加词");
}

/// 混输（`wubi86_pinyin`）：删除规则落在混输方案自己的桶，加词落主码表 `wubi86`。
/// 活跃方案是混输时，加词也要清掉混输桶里的那条。
#[test]
fn mixed_readd_via_dict_add_shows_again() {
    if !has_schemas()
        || !data_dir()
            .join("schemas/wubi86_pinyin.schema.toml")
            .exists()
    {
        eprintln!("跳过：缺 build_dev 词库或混输方案");
        return;
    }
    let cfg = config("wubi86_pinyin");
    let store = fresh_store("mixed_cmd");
    let (victim, (schema, _)) = delete_second(&cfg, &store, "aa");
    assert_eq!(schema, "wubi86_pinyin", "前提：混输删除规则落混输桶");
    let coord = open(&cfg, &store);
    coord.debug_run_command(&format!(r#"$CC("加词", dict.add("{victim}", "aa"))"#));
    assert_back(&cfg, &store, "aa", &victim, "混输下 dict.add 加词");
}

/// 导入是批量操作：**不撤**删除规则（一批里混着用户删过的词，不该悄悄全复活），只在结果的
/// `userWords` 段报「其中几条被隐藏」。`extra` 是一条没被删过的词，确认计数不是导入条数。
fn assert_import_keeps_rule_and_counts(
    cfg: &Config,
    store: &Arc<Store>,
    victim: &str,
    out: &wind_coordinator::dict_transfer_api::DictImportOutcome,
    how: &str,
) {
    let words = out
        .sections
        .iter()
        .find(|s| s.key == "userWords")
        .unwrap_or_else(|| panic!("{how}：结果应有 userWords 段，实际 {out:?}"));
    assert_eq!(words.imported, 2, "前提：{how}导入两条，实际 {words:?}");
    assert_eq!(
        words.hidden_by_shadow, 1,
        "{how}：两条里一条被隐藏，实际 {words:?}"
    );
    assert!(
        store
            .get_user_words("wubi86", "aa")
            .unwrap()
            .iter()
            .any(|r| r.text == victim),
        "前提：{how}把词写进了用户词库"
    );
    let after = candidates(cfg, store, "aa");
    assert!(
        !after.iter().any(|t| t == victim),
        "{how}不撤删除规则，`{victim}` 应仍隐藏，实际 {after:?}"
    );
    assert!(
        after.iter().any(|t| t == "测词"),
        "未删过的导入词照常出现，实际 {after:?}"
    );
}

/// 导入（TSV 走 Rime/TSV 分支）。
#[test]
fn import_tsv_keeps_delete_rule_and_counts_hidden() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("wubi86");
    let store = fresh_store("ct_import_tsv");
    let (victim, _) = delete_second(&cfg, &store, "aa");
    let coord = open(&cfg, &store);
    let out = wind_coordinator::dict_transfer_api::import(
        &*coord,
        "wubi86",
        &format!("aa\t{victim}\t100\naa\t测词\t100\n"),
        false,
        false,
        None,
    )
    .unwrap();
    assert_import_keeps_rule_and_counts(&cfg, &store, &victim, &out, "TSV 导入");
}

/// 导入（WindDict 多段分支）。
#[test]
fn import_wdict_keeps_delete_rule_and_counts_hidden() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("wubi86");
    let store = fresh_store("ct_import_wdict");
    let (victim, _) = delete_second(&cfg, &store, "aa");
    let src = fresh_store("ct_import_wdict_src");
    src.add_user_word("wubi86", "aa", &victim, 100, 0).unwrap();
    src.add_user_word("wubi86", "aa", "测词", 100, 0).unwrap();
    let text = src
        .export_dict_wdict("wubi86", "2026-01-01T00:00:00+08:00")
        .unwrap();
    let coord = open(&cfg, &store);
    let out =
        wind_coordinator::dict_transfer_api::import(&*coord, "wubi86", &text, false, false, None)
            .unwrap();
    assert_import_keeps_rule_and_counts(&cfg, &store, &victim, &out, "wdict 导入");
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
                wind_bridge::handler::KeyAction::InsertText { text, .. } => Some(text),
                _ => None,
            };
        }
        coord.handle_key_event_policed(&key(0x22)); // PageDown
    }
    None
}

/// 反向：**自动**学词（分步组词 → 临时词 → 达阈值自动晋升用户词）不得复活被删的词。
///
/// 先断言自动路径确实把词写进了用户词库——否则「仍被隐藏」只是因为根本没写进去。
#[test]
fn auto_learn_does_not_revive_deleted_word() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut cfg = config("pinyin");
    cfg.schema.pinyin.auto_learn.enabled = true;
    cfg.schema.pinyin.auto_learn.promote_count = 1;
    let store = fresh_store("py_auto");

    // 按编码的删除规则（存量，或设置页手加的「隐藏」）。右键删多字词现在是按文字屏蔽，
    // 屏蔽下根本不学，见 `pinyin_block_word.rs::segmented_commit_of_blocked_word_is_not_learned`。
    store.delete_shadow("pinyin", "nihao", "你好").unwrap();
    assert!(!candidates(&cfg, &store, "nihao").contains(&"你好".to_string()));

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

    let after = candidates(&cfg, &store, "nihao");
    assert!(
        !after.contains(&"你好".to_string()),
        "自动学词不得清掉用户的删除规则，实际 {after:?}"
    );
}

/// 全拼敲 `xv` 删的词（规则键 `xv`），设置页加回时码被 ü 归一成 `xu`：拼音桶的键补一个 ü 归一形态再比。
#[test]
fn pinyin_readd_after_delete_under_v_spelling() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("pinyin");
    let store = fresh_store("py_v");
    let (victim, (_, code)) = delete_on_page(&cfg, &store, "xv", Some("需"));
    assert_eq!(code, "xv", "前提：全拼规则键是原始击键");
    let coord = open(&cfg, &store);
    wind_coordinator::user_words_api::add(&*coord, "pinyin", "xv", &victim, 0).unwrap();
    assert!(
        store
            .get_user_words("pinyin", "xu")
            .unwrap()
            .iter()
            .any(|r| r.text == victim),
        "前提：设置页加词把 `xv` 归一成 `xu` 落库"
    );
    assert_back(&cfg, &store, "xv", &victim, "设置页加词");
}

/// 反向：双拼未完整转换时规则键是原始击键（小鹤 `jvq`，末尾单键拼不成音节），`v` 是布局键。
/// 它 ü 归一后（`juq`）切不成全拼，不得据此撤掉——哪怕加的词码恰好就是 `juq`。
///
/// 规则直接写进 store：`jvq` 首页全是多字词，右键删会按文字屏蔽（`code = *`），而这里验的是
/// 按编码的存量规则（或设置页手加的「隐藏」）。
#[test]
fn shuangpin_uncovered_key_not_cleared_by_v_form() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("shuangpin");
    let store = fresh_store("sp_uncovered");
    let coord = open(&cfg, &store);
    type_keys(&coord, "jvq");
    let page = coord.debug_page_texts();
    let victim = page
        .get(1)
        .cloned()
        .unwrap_or_else(|| panic!("前提：`jvq` 首页至少两条候选，实际 {page:?}"));
    let (_, code) = coord
        .debug_candidate_op_scope()
        .expect("前提：主输入路应有候选调整作用域");
    assert_eq!(code, "jvq", "前提：未完整转换时规则键是原始击键");
    store.delete_shadow("pinyin", &code, &victim).unwrap();
    assert!(
        !candidates(&cfg, &store, "jvq").contains(&victim),
        "前提：规则生效"
    );
    let coord = open(&cfg, &store);
    wind_coordinator::user_words_api::add(&*coord, "shuangpin", "juq", &victim, 0).unwrap();
    assert_eq!(
        store
            .get_shadow_rules("pinyin", "jvq")
            .unwrap()
            .map(|r| r.deleted),
        Some(vec![victim.clone()]),
        "双拼原始击键上的删除规则不该被 ü 形态撤掉"
    );
}

/// 混输下删、切到纯码表后在设置页加回、再切回混输：混输桶不随当前方案，只要混输方案已启用且以
/// 该数据方案为成员就清。
#[test]
fn mixed_bucket_cleared_when_adding_from_codetable() {
    if !has_schemas()
        || !data_dir()
            .join("schemas/wubi86_pinyin.schema.toml")
            .exists()
    {
        eprintln!("跳过：缺 build_dev 词库或混输方案");
        return;
    }
    let mixed = config("wubi86_pinyin");
    let store = fresh_store("mixed_from_ct");
    let (victim, _) = delete_second(&mixed, &store, "aa");
    let coord = open(&config("wubi86"), &store);
    wind_coordinator::user_words_api::add(&*coord, "wubi86", "aa", &victim, 0).unwrap();
    assert_back(&mixed, &store, "aa", &victim, "纯码表下设置页加词");
}

fn key_mods(k: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        modifiers,
        ..key(k)
    }
}

/// 临时英文：缓冲带大写（Shift+H 进入即 `Hels`），作用域码取小写 `hels`。删系统英文词的规则要写在
/// 读端同款的 `hels` 上；前：写在击键 `Hels` 上，删了照样出现。
///
/// 删的是词库里本身大写的词（`Helsinki`）。词库小写、靠大小写跟随显示成大写的词（`Help`）
/// 删了仍会出现：规则记的是显示文本、读端比的是词库文本，那是另一处问题，本用例不覆盖。
#[test]
fn temp_english_delete_with_uppercase_buffer_takes_effect() {
    let d = data_dir();
    if !d.join("schemas/english.schema.toml").exists()
        || !d.join("schemas/wubi86.schema.toml").exists()
    {
        eprintln!("跳过：缺 build_dev 英文 / 五笔方案");
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    let store = fresh_store("temp_en");
    let enter = |coord: &Coordinator| {
        coord.handle_key_event(&key_mods(u32::from(b'H'), wind_ipc::protocol::MOD_SHIFT));
        type_keys(coord, "els");
    };
    let coord = open(&cfg, &store);
    enter(&coord);
    let page = coord.debug_page_texts();
    // 首页前几条是输入原样的大小写变体（`Hels` / `hels` / `HELS`），删的要是词库词。
    let idx = page
        .iter()
        .position(|t| t == "Helsinki")
        .unwrap_or_else(|| panic!("前提：临英 `Hels` 首页应有词库词「Helsinki」，实际 {page:?}"));
    let victim = page[idx].clone();
    assert_eq!(
        coord.debug_candidate_op_scope().map(|s| s.1),
        Some("hels".to_string()),
        "前提：临英作用域码是小写化缓冲"
    );
    coord.debug_candidate_op(CandidateOp::Delete, idx);

    let again = open(&cfg, &store);
    enter(&again);
    let after = again.debug_all_candidate_texts();
    assert!(
        !after.contains(&victim),
        "临英删除后 `{victim}` 应隐藏，实际 {after:?}"
    );
}

/// 快捷加词（Ctrl+= → 回车，`confirm_add_word`）：「你好」在 `nihao` 下有按编码的删除规则
/// （存量，或设置页手加的「隐藏」；右键删多字词现为按文字屏蔽），分两次上屏「你」「好」，
/// 用快捷加词把最近两字加回，规则撤掉、`nihao` 下重新出现。
#[test]
fn quick_add_word_readd_shows_again() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let cfg = config("pinyin");
    let store = fresh_store("py_quick_add");
    let victim = "你好".to_string();
    store.delete_shadow("pinyin", "nihao", &victim).unwrap();
    assert!(
        !candidates(&cfg, &store, "nihao").contains(&victim),
        "前提：规则生效"
    );

    let coord = open(&cfg, &store);
    type_keys(&coord, "ni");
    assert_eq!(pick(&coord, "你").as_deref(), Some("你"));
    type_keys(&coord, "hao");
    assert_eq!(pick(&coord, "好").as_deref(), Some("好"));
    coord.handle_key_event(&key_mods(0xBB, wind_ipc::protocol::MOD_CTRL)); // Ctrl+=
    coord.handle_key_event(&key(0x0D)); // 回车确认
    assert!(
        store
            .get_user_words("pinyin", "nihao")
            .unwrap()
            .iter()
            .any(|r| r.text == victim),
        "前提：快捷加词应把「你好」写进用户词库"
    );
    assert!(
        store.get_shadow_rules("pinyin", "nihao").unwrap().is_none(),
        "规则已撤"
    );
    assert_back(&cfg, &store, "nihao", &victim, "快捷加词");
}
