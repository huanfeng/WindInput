//! 自动造词写进词库的**编码**必须是规范的词条码，不是候选的匹配用码。
//!
//! 现场：开模糊音(sh_s + en_eng)打 `senrikl`，分步选「生日」「快乐」上屏后，
//! 临时词库里躺着 `senrikuaile → 生日快乐` —— 前半 `senri` 是用户敲的模糊原码、
//! 后半 `kuaile` 是词典全拼码，两段分处两个域。这条码**用任何方式都打不出来**：
//! `senrikl` 不行、`shengrikuaile` 也不行，只有一字不差敲 `senrikuaile` 才行。
//!
//! 根子是 `committed_segs` 存的是候选的匹配用码，而那个码上绑着三个别的用途
//! （`consumed_length` 的 `starts_with` 判据、preedit 跟随、词频记账），不能为造词改掉。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}
fn has_dict() -> bool {
    data_dir()
        .join("schemas/pinyin/rime_frost.dict.yaml")
        .exists()
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
fn cfg() -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.schema.pinyin.fuzzy.enabled = true;
    c.schema.pinyin.fuzzy.sh_s = true;
    c.schema.pinyin.fuzzy.en_eng = true;
    c.schema.pinyin.auto_learn.enabled = true;
    c
}
/// 翻页找候选并选中，返回上屏文本（None = 未上屏，仍在组合区）。
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
        coord.handle_key_event_policed(&key(0x22));
    }
    None
}

/// 分步组出的词，写库用的码须是**各段规范词条码**的拼接。
#[test]
fn learned_code_uses_dict_code_not_typed_code() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let db = std::env::temp_dir().join("wind_learn_code_norm.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg(), Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    for ch in "senrikl".chars() {
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    assert!(pick(&coord, "生日").is_none(), "选「生日」应留在组合区分步");
    assert_eq!(
        pick(&coord, "快乐").as_deref(),
        Some("生日快乐"),
        "再选「快乐」应整体上屏"
    );

    let temps: Vec<(String, String, u64)> = store
        .search_temp_words_prefix("pinyin", "", 200)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.code, r.text, r.boundary))
        .collect();
    let (code, _, boundary) = temps
        .iter()
        .find(|(_, t, _)| t == "生日快乐")
        .unwrap_or_else(|| panic!("应造出「生日快乐」，实际: {temps:?}"));

    // 用户敲的是 senrikl，但词在词典里登记的码是 shengri + kuaile。
    assert_eq!(
        code, "shengrikuaile",
        "造词码须用规范词条码，不得掺入模糊音原码 senri"
    );
    // 模糊命中的 boundary 此前恒 0（与原码不同域，见 lookup_with_fuzzy），
    // 换成规范码后边界与之同域，必须是真值 —— 否则简拼索引算不出 srkl。
    assert_ne!(*boundary, 0, "规范码的 boundary 须为真值，简拼索引依赖它");
}

/// **真正的验收**：造出来的词，用户下次得真能打出来。
///
/// ⚠️ **必须用系统词库里没有的词**。第一版拿「生日快乐」测，结果是个假护栏：
/// `shengrikuaile` 在系统词库里本就有这个词，`can_type` 恒真 —— 造词码写成什么样
/// 它都绿（实测：把造词改回用候选 `code`，这条照样过）。
/// 改用「圣日快乐」：`senri` 的候选里有「圣日」（同音字），而这个**词**系统词库没有，
/// 所以打 `shengrikuaile` 能出它，只可能来自刚造的那条临时词。
///
/// 两种打法都要通：
/// - 全拼 `shengrikuaile`：直接命中规范码；
/// - 简拼 `srkl`：规范码在简拼路径上同样召得回。
///
/// ⚠️ 简拼这条**不守 boundary**，别把它当 boundary 的护栏：boundary=0 时词进
/// `abbrev_index::group_of` 的兜底组，引擎侧仍会用 DAG 对 `code` 现切声母串召回
/// （实测：变异「码取规范码但 boundary 退回候选那份」下本条仍绿）。
/// boundary 真值由上一条测试的 `assert_ne!(boundary, 0)` 守着 —— 那条在同一个变异下会红。
#[test]
fn learned_word_is_actually_typeable() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let db = std::env::temp_dir().join("wind_learn_code_typeable.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg(), Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    // 造一个系统词库里没有的词（见上方 ⚠️）。
    for ch in "senrikl".chars() {
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    assert!(pick(&coord, "圣日").is_none(), "选「圣日」应留在组合区分步");
    assert_eq!(
        pick(&coord, "快乐").as_deref(),
        Some("圣日快乐"),
        "再选「快乐」应整体上屏"
    );
    // 前提自检：这个词系统词库确实没有，否则下面测的是系统词库而非刚造的词。
    let fresh = Coordinator::new_headless_with_store(
        cfg(),
        Some(&data_dir()),
        Arc::new(Store::open(std::env::temp_dir().join("wind_learn_code_fresh.redb")).unwrap()),
    );
    fresh.prewarm_indexes();
    assert!(
        !type_and_find(&fresh, "shengrikuaile", "圣日快乐"),
        "前提：未造词的干净实例不得打出「圣日快乐」"
    );

    assert!(
        type_and_find(&coord, "shengrikuaile", "圣日快乐"),
        "规范全拼码须能打出刚造的词"
    );
    assert!(
        type_and_find(&coord, "srkl", "圣日快乐"),
        "规范码在简拼路径上同样须召得回"
    );
}

/// 敲入 `input`，翻页找 `want`，之后 Esc 清空。
fn type_and_find(coord: &Coordinator, input: &str, want: &str) -> bool {
    for ch in input.chars() {
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    let mut found = false;
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if t.is_empty() {
            break;
        }
        if t.iter().any(|x| x == want) {
            found = true;
            break;
        }
        coord.handle_key_event_policed(&key(0x22));
    }
    coord.handle_key_event_policed(&key(0x1B));
    found
}

/// 论坛 t215 / A2-44 的原始路径：开 c=ch，**用翘舌打法**分步组出「菜就多练」，
/// 下次仍用翘舌打法得能直接打出来。
///
/// 造词写库用的是规范码 `caijiuduolian`（上面两条守的就是这个），而此前 step 6
/// 只拿用户敲的 `chaijiuduolian` 去查 store —— 于是用户原话里「多打几遍加入词库，
/// 还是不能通过翘舌打出」：学进去的词只有平舌打法认。
#[test]
fn word_learned_by_fuzzy_typing_is_retypeable_by_fuzzy_typing() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = cfg();
    c.schema.pinyin.fuzzy.ch_c = true;
    let db = std::env::temp_dir().join("wind_learn_code_fuzzy_retype.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord =
        Coordinator::new_headless_with_store(c.clone(), Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    // 前提：未造词时翘舌打法出不来这个词，否则下面测的是系统词库。
    assert!(
        !type_and_find(&coord, "chaijiuduolian", "菜就多练"),
        "前提：未造词时 chaijiuduolian 不得出「菜就多练」"
    );

    for ch in "chaijiuduolian".chars() {
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    assert!(pick(&coord, "菜").is_none(), "选「菜」应留在组合区分步");
    assert!(pick(&coord, "就").is_none(), "选「就」应留在组合区分步");
    assert!(pick(&coord, "多").is_none(), "选「多」应留在组合区分步");
    assert_eq!(
        pick(&coord, "练").as_deref(),
        Some("菜就多练"),
        "选完应整体上屏"
    );

    let learned: Vec<String> = store
        .search_temp_words_prefix("pinyin", "", 200)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.text == "菜就多练")
        .map(|r| r.code)
        .collect();
    assert_eq!(learned, ["caijiuduolian"], "造词码须为规范码");

    assert!(
        type_and_find(&coord, "chaijiuduolian", "菜就多练"),
        "翘舌打法须能打出按规范码学进去的词"
    );
    assert!(
        type_and_find(&coord, "caijiuduolian", "菜就多练"),
        "平舌打法照旧"
    );
}

/// 敲入 `input` 后直接选中整句 `want`（不分步），返回上屏文本。
fn type_and_pick(coord: &Coordinator, input: &str, want: &str) -> Option<String> {
    for ch in input.chars() {
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    pick(coord, want)
}

/// 临时库里文本为 `text` 的全部记录 (码, 边界)。
fn temp_records(store: &Store, text: &str) -> Vec<(String, u64)> {
    store
        .search_temp_words_prefix("pinyin", "", 500)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.text == text)
        .map(|r| (r.code, r.boundary))
        .collect()
}

/// 模糊**整句**一次上屏（单段整句造词）：写库码须为规范码，不是所打码。
///
/// 整句候选的 `code` 是所打码 `chaijiuduolian`；此前不带 `learn_code`，
/// `learn_phrase_on_commit` 便把它原样写进临时库 —— 下次同样打法**零罚精确命中**这条
/// 非规范记录，平舌打法反而打不出。改后写的是 `caijiuduolian`，翘舌打法只能经模糊召回它。
///
/// 这条走的是**系统词**模糊节点（`build` 的模糊分支），与 S2 开关无关。
#[test]
fn fuzzy_sentence_commit_learns_canonical_code() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = cfg();
    c.schema.pinyin.fuzzy.ch_c = true;
    let db = std::env::temp_dir().join("wind_learn_code_fuzzy_sentence.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    const SENT: &str = "才就多练";
    assert_eq!(
        type_and_pick(&coord, "chaijiuduolian", SENT).as_deref(),
        Some(SENT),
        "前提：翘舌打法的整句是「{SENT}」且一次上屏"
    );
    // cai|jiu|duo|lian → 位 0/3/6/9
    assert_eq!(
        temp_records(&store, SENT),
        [("caijiuduolian".to_string(), 0b10_0100_1001u64)],
        "造词码须为规范码，且边界在规范码坐标下"
    );
}

/// 同上，经 S2 的用户词模糊节点组出的整句：`wo` + 用户词「菜就多练」（规范码 caijiuduolian）。
#[test]
fn s2_fuzzy_sentence_commit_learns_canonical_code() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = cfg();
    c.schema.pinyin.fuzzy.ch_c = true;
    c.schema.pinyin.sentence_uses_user_words = true;
    let db = std::env::temp_dir().join("wind_learn_code_s2_fuzzy_sentence.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    store
        .add_user_word("pinyin", "caijiuduolian", "菜就多练", 1200, 0b10_0100_1001)
        .unwrap();
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    const SENT: &str = "我菜就多练";
    assert_eq!(
        type_and_pick(&coord, "wochaijiuduolian", SENT).as_deref(),
        Some(SENT),
        "前提：整句是「{SENT}」且一次上屏"
    );
    // wo|cai|jiu|duo|lian → 位 0/2/5/8/11
    assert_eq!(
        temp_records(&store, SENT),
        [("wocaijiuduolian".to_string(), 0b1001_0010_0101u64)],
        "造词码须为规范码"
    );
}

/// 模糊打法选中**临时词**，也要推进它的晋升计数（6b）。
///
/// 模糊召回的候选 `code` 是所打码（`chaijiuduolian`），记录码在 `meta.store_code`
/// （`caijiuduolian`）。6b 曾拿 `code` 去点查临时库 ⇒ 永远查不到 ⇒ 选多少次都不涨，
/// 模糊打法下这个词永远晋升不了。
#[test]
fn fuzzy_pick_of_temp_word_advances_its_count() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = cfg();
    c.schema.pinyin.fuzzy.ch_c = true;
    let db = std::env::temp_dir().join("wind_learn_code_fuzzy_temp_count.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    const WORD: &str = "菜就多练";
    // cai|jiu|duo|lian → 位 0/3/6/9
    let n = store
        .learn_temp_word("pinyin", "caijiuduolian", WORD, 500, 0b10_0100_1001)
        .unwrap();
    assert_eq!(n, 1, "前提：临时词初始计数 1");
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    assert_eq!(
        type_and_pick(&coord, "chaijiuduolian", WORD).as_deref(),
        Some(WORD),
        "前提：翘舌打法能召回并选中临时词「{WORD}」"
    );
    assert_eq!(
        store
            .get_temp_word("pinyin", "caijiuduolian", WORD)
            .unwrap(),
        Some(2),
        "模糊打法选中临时词，计数应推进到 2"
    );
    assert_eq!(
        temp_records(&store, WORD),
        [("caijiuduolian".to_string(), 0b10_0100_1001u64)],
        "不得在所打码下另写一条记录"
    );
}

/// 模糊整句**重复上屏**：每次只计一次。
///
/// 第二次上屏时整句候选同文并入了临时词「才就多练 / caijiuduolian」：造词路径按规范码
/// `+1`，6b 若再按记录码点查命中又 `+1`。「刚造词就跳过」的守卫比的必须是 6b **命中的码**
/// （规范码），拿所打码比会漏判 —— 计数变 3。
#[test]
fn fuzzy_sentence_repeat_commit_counts_once() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = cfg();
    c.schema.pinyin.fuzzy.ch_c = true;
    let db = std::env::temp_dir().join("wind_learn_code_fuzzy_sentence_twice.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    const SENT: &str = "才就多练";
    for round in 1..=2u32 {
        assert_eq!(
            type_and_pick(&coord, "chaijiuduolian", SENT).as_deref(),
            Some(SENT),
            "前提：第 {round} 次整句上屏"
        );
        assert_eq!(
            store
                .get_temp_word("pinyin", "caijiuduolian", SENT)
                .unwrap(),
            Some(round),
            "第 {round} 次上屏后计数应为 {round}"
        );
    }
}

/// S2（整句组词使用用户词）：**单个用户词**组成的整句上屏，不得往临时层再复制一份，
/// 候选页也不得出现同文重复——精确与模糊（c=ch）两种打法都一样。
///
/// 设计约定：用户词进整句只影响排序，不产生新词；多词整句（「有盖伦吗」）才照常作为新词
/// 学进临时层。这条钉的是前者，免得以后改造词路径时把已在用户库的词又学一遍。
#[test]
fn s2_single_user_word_sentence_does_not_duplicate() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = cfg();
    c.schema.pinyin.fuzzy.ch_c = true;
    c.schema.pinyin.sentence_uses_user_words = true;
    let db = std::env::temp_dir().join("wind_learn_code_s2_no_dup.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    const WORD: &str = "菜就多练";
    store
        .add_user_word("pinyin", "caijiuduolian", WORD, 1200, 0b10_0100_1001)
        .unwrap();
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    for input in ["caijiuduolian", "chaijiuduolian", "chaijiuduolian"] {
        for ch in input.chars() {
            coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
        }
        let page = coord.debug_page_texts();
        assert_eq!(
            page.iter().filter(|t| *t == WORD).count(),
            1,
            "[{input}] 候选页里「{WORD}」应恰好一条，实际: {page:?}"
        );
        assert_eq!(
            pick(&coord, WORD).as_deref(),
            Some(WORD),
            "[{input}] 前提：选中上屏"
        );
        assert!(
            temp_records(&store, WORD).is_empty(),
            "[{input}] 已在用户库的词不得再写进临时层"
        );
    }
}

/// ②b 混合整句一次上屏：造词码是**完整全拼 + 规范边界**，不是「简拼字母 + 全拼」的击键
/// 混合码（`bzdnihao` 这种码几乎打不出来，只会在临时层堆积）。简拼段的节点带回查出的
/// 词条码（`LatticeNode::canon`）；出厂配置（模糊关）即生效。
///
/// 落库后两种打法都够得着：全拼码点查命中，简拼索引按规范边界建出 `bzdnh`。
#[test]
fn mixed_abbrev_sentence_commit_learns_full_pinyin() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = cfg();
    c.schema.pinyin.fuzzy.enabled = false;
    let db = std::env::temp_dir().join("wind_learn_code_mixed_abbrev.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    const SENT: &str = "不知道你好";
    assert_eq!(
        type_and_pick(&coord, "bzdnihao", SENT).as_deref(),
        Some(SENT),
        "前提：混合整句是「{SENT}」且一次上屏"
    );
    // bu|zhi|dao|ni|hao → 位 0/2/5/8/10
    assert_eq!(
        temp_records(&store, SENT),
        [("buzhidaonihao".to_string(), 0b101_0010_0101u64)],
        "造词码须为完整全拼 + 规范边界"
    );
    let by_abbrev: Vec<String> = store
        .search_temp_words_by_abbrev("pinyin", "bzdnh", 0)
        .unwrap()
        .into_iter()
        .map(|r| r.text)
        .collect();
    assert!(
        by_abbrev.iter().any(|t| t == SENT),
        "简拼索引须按规范边界召回，实际: {by_abbrev:?}"
    );
}

/// ②b 混合整句与用户库里的同文词合并后上屏：造词码是完整全拼，`learn_phrase_on_commit`
/// 按（码, 词）查到用户库已有这条，**不再写临时层**。
///
/// 此前造词码是击键混合码 `bzdnihao`，查重按它查不到用户库那条 `buzhidaonihao`，于是每上屏
/// 一次就在临时层多一条打不出来的 `bzdnihao → 不知道你好`（实测，简拼节点 canon 置 None 即复现；
/// 同 `s2_single_user_word_sentence_does_not_duplicate` 的约定）。
#[test]
fn mixed_abbrev_sentence_does_not_duplicate_user_word() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = cfg();
    c.schema.pinyin.fuzzy.enabled = false;
    let db = std::env::temp_dir().join("wind_learn_code_mixed_abbrev_dup.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    const SENT: &str = "不知道你好";
    store
        .add_user_word("pinyin", "buzhidaonihao", SENT, 1200, 0b101_0010_0101)
        .unwrap();
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    assert_eq!(
        type_and_pick(&coord, "bzdnihao", SENT).as_deref(),
        Some(SENT),
        "前提：选中上屏"
    );
    assert!(
        temp_records(&store, SENT).is_empty(),
        "已在用户库的词不得再写进临时层，实际: {:?}",
        temp_records(&store, SENT)
    );
}

/// ②b 里的**用户简拼节点**（S2 开，临时层「拜城县」）：`bcxrmzf` 选整句「拜城县人民政府」上屏，
/// 造词码是完整全拼——拜城县段取临时记录自己的规范码，人民政府段取系统词条码。
#[test]
fn user_abbrev_node_sentence_commit_learns_full_pinyin() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = cfg();
    c.schema.pinyin.fuzzy.enabled = false;
    c.schema.pinyin.sentence_uses_user_words = true;
    let db = std::env::temp_dir().join("wind_learn_code_user_abbrev_node.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    // bai|cheng|xian → 位 0/3/8
    store
        .learn_temp_word("pinyin", "baichengxian", "拜城县", 800, 0b1_0000_1001)
        .unwrap();
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    const SENT: &str = "拜城县人民政府";
    assert_eq!(
        type_and_pick(&coord, "bcxrmzf", SENT).as_deref(),
        Some(SENT),
        "前提：整句是「{SENT}」且一次上屏"
    );
    // bai|cheng|xian|ren|min|zheng|fu → 位 0/3/8/12/15/18/23
    let want: u64 = [0, 3, 8, 12, 15, 18, 23]
        .iter()
        .fold(0, |m, b| m | (1u64 << b));
    assert_eq!(
        temp_records(&store, SENT),
        [("baichengxianrenminzhengfu".to_string(), want)],
        "造词码须为完整全拼 + 规范边界"
    );
}

/// 自动晋升校验（`maybe_promote_temp`）端到端：临时层预置 `count = promote_count − 1` 的记录，
/// 打 `bzdnihao` 选整句「不知道你好」一次，把计数推到阈值。
///
/// - 旧版 ②b 写下的击键混合码 `bzdnihao`（边界按击键逐字母：b|z|d|ni|hao）经 6b 推进到阈值，
///   **不晋升**，留在临时层；
/// - 对照：规范码 `buzhidaonihao` 同样条件下（经造词推进到阈值）正常晋升。
#[test]
fn legacy_mixed_code_is_not_auto_promoted() {
    if !has_dict() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    const SENT: &str = "不知道你好";
    const PROMOTE: usize = 3;
    for (tag, code, boundary, want_promoted) in [
        ("legacy", "bzdnihao", 0b10_1111u64, false),
        ("canon", "buzhidaonihao", 0b101_0010_0101, true),
    ] {
        let mut c = cfg();
        c.schema.pinyin.fuzzy.enabled = false;
        c.schema.pinyin.auto_learn.promote_count = PROMOTE;
        let db = std::env::temp_dir().join(format!("wind_learn_code_promo_{tag}.redb"));
        let _ = std::fs::remove_file(&db);
        let store = Arc::new(Store::open(&db).unwrap());
        for _ in 0..PROMOTE - 1 {
            store
                .learn_temp_word("pinyin", code, SENT, 800, boundary)
                .unwrap();
        }
        let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
        coord.prewarm_indexes();
        assert_eq!(
            type_and_pick(&coord, "bzdnihao", SENT).as_deref(),
            Some(SENT),
            "[{tag}] 前提：选中上屏"
        );
        let promoted = store
            .get_user_words("pinyin", code)
            .unwrap()
            .iter()
            .any(|r| r.text == SENT);
        assert_eq!(promoted, want_promoted, "[{tag}] {code} 是否晋升");
        if !want_promoted {
            assert_eq!(
                store.get_temp_word("pinyin", code, SENT).unwrap(),
                Some(PROMOTE as u32),
                "[{tag}] 前提：计数确实推到了阈值（否则没晋升不说明校验生效）"
            );
        }
    }
}
