//! 删掉系统词后再**显式**加回同一个词（同编码同文本），候选必须重新出现。
//!
//! 删系统词走候选调整（shadow）的 `delete` 规则，读端 `apply_shadow` 按文本过滤、不看来源，
//! 于是加回来的用户词被同一条规则一并滤掉——「加了词但永远打不出」。修法：显式加词成功后
//! 清掉同 (数据方案, 编码, 文本) 的 `delete` 规则；自动学词 / 自动晋升**不清**（删过的词
//! 不该因为又打到一次就复活）。
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
    let coord = open(cfg, store);
    type_keys(&coord, input);
    let page = coord.debug_page_texts();
    assert!(
        page.len() >= 2,
        "前提：`{input}` 首页至少两条候选，实际 {page:?}"
    );
    let victim = page[1].clone();
    let scope = coord
        .debug_candidate_op_scope()
        .expect("前提：主输入路应有候选调整作用域");
    coord.debug_candidate_op(CandidateOp::Delete, 1);
    let after = candidates(cfg, store, input);
    assert!(
        !after.contains(&victim),
        "前提：删除后 `{victim}` 应从 `{input}` 的候选里消失，实际 {after:?}"
    );
    (victim, scope)
}

fn assert_back(cfg: &Config, store: &Arc<Store>, input: &str, victim: &str, how: &str) {
    let back = candidates(cfg, store, input);
    assert!(
        back.iter().any(|t| t == victim),
        "{how}之后 `{victim}` 应重新出现在 `{input}` 的候选里，实际 {back:?}"
    );
}

/// 码表：删系统词 → 命令栏 `dict.add` 加回（与快捷加词同一条 `try_dict_add`）→ 重新出现。
#[test]
fn codetable_readd_via_dict_add_shows_again() {
    if !has_schemas() {
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
        return;
    }
    let cfg = config("pinyin");
    let store = fresh_store("py_settings");
    let (victim, (schema, code)) = delete_second(&cfg, &store, "nihao");
    assert_eq!((schema.as_str(), code.as_str()), ("pinyin", "nihao"));
    let coord = open(&cfg, &store);
    // 设置页列表/输入框的码带音节空格，存储域是扁平码
    wind_coordinator::user_words_api::add(&*coord, "pinyin", "ni hao", &victim, 0).unwrap();
    assert_back(&cfg, &store, "nihao", &victim, "设置页加词");
}

/// 全拼带手动分隔符：shadow 键是 `ni'hao`（`'` 是硬边界、不归一，见 `ConvertResult::shadow_code`），
/// 用户词码是扁平的 `nihao`。两者须按「去掉分隔符」对齐，否则删在 `ni'hao` 的词加回后照样不出。
#[test]
fn pinyin_readd_after_delete_under_separator() {
    if !has_schemas() {
        return;
    }
    let mut cfg = config("pinyin");
    cfg.keys.select_key_groups.clear(); // 出厂 `;'` 选二三候选，会把 `'` 吃掉
    let store = fresh_store("py_sep");
    let (victim, (_, code)) = delete_second(&cfg, &store, "ni'hao");
    assert_eq!(code, "ni'hao", "前提：全拼 shadow 键保留分隔符");
    let coord = open(&cfg, &store);
    coord.debug_run_command(&format!(r#"$CC("加词", dict.add("{victim}", "nihao"))"#));
    assert_back(&cfg, &store, "ni'hao", &victim, "dict.add 加词");
}

/// 双拼（小鹤 `nihc` = ni hao）删系统词：规则键须是读端同款的归一码（连写全拼 `nihao`），
/// 落拼音族共享桶。前：写端用击键 `nihc`，读端用 `nihao`，删了不生效。
#[test]
fn shuangpin_delete_writes_normalized_code() {
    if !has_schemas() {
        return;
    }
    let cfg = config("shuangpin");
    let store = fresh_store("sp_delete");
    let (victim, (_, code)) = delete_second(&cfg, &store, "nihc");
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
        return;
    }
    let cfg = config("shuangpin");
    let store = fresh_store("sp_settings");
    let (victim, _) = delete_second(&cfg, &store, "nihc");
    let coord = open(&cfg, &store);
    wind_coordinator::user_words_api::add(&*coord, "shuangpin", "nihao", &victim, 0).unwrap();
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

/// 导入（TSV 走 Rime/TSV 分支）：导入的词同样是用户显式要的。
#[test]
fn import_tsv_clears_delete_rule() {
    if !has_schemas() {
        return;
    }
    let cfg = config("wubi86");
    let store = fresh_store("ct_import_tsv");
    let (victim, _) = delete_second(&cfg, &store, "aa");
    let coord = open(&cfg, &store);
    wind_coordinator::dict_transfer_api::import(
        &*coord,
        "wubi86",
        &format!("aa\t{victim}\t100\n"),
        false,
        false,
        None,
    )
    .unwrap();
    assert_back(&cfg, &store, "aa", &victim, "TSV 导入");
}

/// 导入（WindDict 多段分支）：文件只带用户词段。
#[test]
fn import_wdict_clears_delete_rule() {
    if !has_schemas() {
        return;
    }
    let cfg = config("wubi86");
    let store = fresh_store("ct_import_wdict");
    let (victim, _) = delete_second(&cfg, &store, "aa");
    let src = fresh_store("ct_import_wdict_src");
    src.add_user_word("wubi86", "aa", &victim, 100, 0).unwrap();
    let text = src
        .export_dict_wdict("wubi86", "2026-01-01T00:00:00+08:00")
        .unwrap();
    let coord = open(&cfg, &store);
    wind_coordinator::dict_transfer_api::import(&*coord, "wubi86", &text, false, false, None)
        .unwrap();
    assert_back(&cfg, &store, "aa", &victim, "wdict 导入");
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
        return;
    }
    let mut cfg = config("pinyin");
    cfg.schema.pinyin.auto_learn.enabled = true;
    cfg.schema.pinyin.auto_learn.promote_count = 1;
    let store = fresh_store("py_auto");

    let coord = open(&cfg, &store);
    type_keys(&coord, "nihao");
    let page = coord.debug_page_texts();
    let idx = page
        .iter()
        .position(|t| t == "你好")
        .unwrap_or_else(|| panic!("前提：`nihao` 首页应有「你好」，实际 {page:?}"));
    coord.debug_candidate_op(CandidateOp::Delete, idx);
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
