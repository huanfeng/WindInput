//! 含生僻字的**词**在检索范围过滤下的可见性（端到端，真实词库）。论坛 t103 / A2-6。
//!
//! 现场：常用性判据是「整串每字都常用」（`CommonChars::is_string_common`），字表是
//! 《通用规范汉字表》8105 字。于是词库正常收录、只因某一个字在表外的词，整条被判非常用
//! ——常用字档直接滤掉，智能档在同码有常用候选时也滤掉。用户的话是「词组能直接打出来
//! 是不是比较好」。出厂档 `input.rare_phrase = "keep"` 让过滤只作用在单字上。
//!
//! # 素材为什么是这几个词
//!
//! 「饕餮」「耄耋」「旮旯」「貔貅」这类**看着生僻的词其实全在表内**（三级字表收了它们），
//! 拿它们做素材两档结果相同，测了个寂寞。表外的是异体字、日韩汉字与冷僻化学/人名用字：
//! 「磺」（苯磺酸）、「馎饦」、五笔的「磳」。⛔ 换素材前先 grep `charsets/common_han.yaml`
//! 确认那个字真的不在表内，否则用例会静默变成空跑。
//!
//! ⚠️ `build_dev/data` 不存在时**整族跳过而计数照绿**（判据是耗时：正常秒级，跳过 0.0x s）。
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
        && d.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists()
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

fn config(schema: &str, filter_mode: &str, rare_phrase: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.into()];
    cfg.schema.active = schema.into();
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = filter_mode.into();
    cfg.input.rare_phrase = rare_phrase.into();
    cfg
}

/// 敲入 `code` 后的候选文本列表。按键走**生产入口** `handle_key_event_policed`。
fn candidates(schema: &str, filter_mode: &str, rare_phrase: &str, code: &str) -> Vec<String> {
    let coord =
        Coordinator::new_headless(config(schema, filter_mode, rare_phrase), Some(&data_dir()));
    for c in code.chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    coord.debug_all_candidate_texts()
}

/// 同一个码在两档下的对照：出厂 `keep` 要出这个词，旧行为 `filter` 要滤掉它。
///
/// 两边都断言是刻意的——只断言 `keep` 出得来的话，「豁免根本没生效、这个码本来就没被滤」
/// 同样能绿，用例就成了假护栏。
fn assert_word_only_survives_under_keep(schema: &str, mode: &str, code: &str, word: &str) {
    let keep = candidates(schema, mode, "keep", code);
    assert!(
        keep.iter().any(|t| t == word),
        "[{schema}/{mode}] 打 {code} 应能出「{word}」，实得 {keep:?}"
    );
    let strict = candidates(schema, mode, "filter", code);
    assert!(
        !strict.iter().any(|t| t == word),
        "[{schema}/{mode}] filter 档该保留旧行为（滤掉「{word}」），实得 {strict:?}"
    );
}

/// 常用字档：整个词只因「磺」在表外就整条消失。
#[test]
fn general_keeps_word_whose_char_is_off_table() {
    if !dict_ready(&data_dir()) {
        return;
    }
    assert_word_only_survives_under_keep("pinyin", "general", "benhuangsuan", "苯磺酸");
}

/// 智能档（出厂档）同样有现场：同码位有常用词「剥脱」，「馎饦」就被遮蔽掉。
///
/// ★ 智能档下**不是每个**含表外字的词都会消失——孤儿码位（同码没有常用候选）本就放行。
/// 故素材必须挑一个**同码有常用词**的：挑了孤儿码位的词，两档都会出它，下面 `filter` 档
/// 那半的「不该出现」当场变红，用例看着像坏了，实则是素材选错。
#[test]
fn smart_keeps_word_shadowed_by_a_common_word() {
    if !dict_ready(&data_dir()) {
        return;
    }
    assert_word_only_survives_under_keep("pinyin", "smart", "botuo", "馎饦");
}

/// 五笔：4 码词位上坐着常用单字「磋」，「磳碟」因「磳」在表外被整条滤掉。
///
/// 码表方案是这个问题最容易撞上的地方——五笔词组恒 4 码，而 4 码位上几乎必然有常用字。
#[test]
fn wubi_phrase_survives_on_a_code_shared_with_common_char() {
    if !dict_ready(&data_dir()) {
        return;
    }
    assert_word_only_survives_under_keep("wubi86", "smart", "duda", "磳碟");
}

/// 敲完 `code` 的**最后一击**返回的动作 + 其后的候选面；`auto_commit_at_full` 开着。
fn drive_with_auto_commit(rare_phrase: &str, code: &str) -> (KeyAction, Vec<String>) {
    let mut cfg = config("wubi86", "smart", rare_phrase);
    cfg.schema.codetable.auto_commit_at_full = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let mut last = KeyAction::PassThrough;
    for c in code.chars() {
        last = coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    (last, coord.debug_all_candidate_texts())
}

/// ★ 已知且**已拍板接受**的连带影响：词被放行之后，那个码位上的「满码唯一即自动上屏」
/// 不再触发。
///
/// 现场＝五笔 `dqqm`：改前「矽钢」(矽 在表外)被滤掉、「三角钢」是唯一精确匹配 ⇒ 满码直接
/// 上屏；改后两条并存 ⇒ 复评判「不唯一」而否决。链路：`codetable/engine.rs` 的
/// `decide_auto_commit` 按 `c.code == input && !c.is_scope_filtered` 筛精确子集，而被豁免的
/// 词落在 `kept` 里、**不带** `is_scope_filtered`（那个标记只给翻页放宽补回来的候选），
/// 故它就在那个子集内。
///
/// ⚠️ **素材极稀有，别随手换**：扫遍出厂五笔词库的全部 4 码位，「改前精确唯一 + 改后多出
/// 一条含表外字的词」只有 `dqqm` 这一处。上面几条用例用的 `duda` 在这里**测不出东西**——
/// 那个码位还坐着「磅礴」，改前就已经不唯一、本来就不自动上屏（实测过）。换素材前先按这个
/// 条件重新扫一遍词库。
///
/// admin 2026-09-17 拍板**算正常**：自动上屏的前提本就是「精确候选唯一」，词既然合法可见，
/// 唯一性确实不成立了——与切到「全部字符」档时同理。⛔ 别把它当 bug 顺手「修」成
/// 「唯一性判据忽略被豁免的词」：那会造出「屏幕上明明两条候选，却直接上屏首选」这种显示与
/// 处置对不上的语义，正是 `phrase_vs_auto_commit.rs` 那一族在防的东西。
///
/// 反向对照（`filter` 档照旧上屏）不可省：没有它，`auto_commit_at_full` 哪天整个坏掉，
/// 这一条也照绿。
#[test]
fn admitted_word_suppresses_full_code_auto_commit() {
    if !dict_ready(&data_dir()) {
        return;
    }
    let (action, cands) = drive_with_auto_commit("keep", "dqqm");
    if let KeyAction::InsertText { text, .. } = &action {
        panic!("词已放行、精确候选不止一条时不得自动上屏，实际上屏了「{text}」；候选面={cands:?}");
    }
    assert!(
        cands.iter().any(|t| t == "三角钢") && cands.iter().any(|t| t == "矽钢"),
        "两条候选都该留在候选面上，实际: {cands:?}"
    );

    let (strict_action, strict_cands) = drive_with_auto_commit("filter", "dqqm");
    assert!(
        matches!(&strict_action, KeyAction::InsertText { text, .. } if text == "三角钢"),
        "filter 档下「磳碟」被滤 ⇒「磋」仍是唯一精确匹配 ⇒ 照旧满码上屏，\
         实际动作={strict_action:?}；候选面={strict_cands:?}"
    );
}

/// ★★★ 反向边界：豁免**只放行词**，表外**单字**照滤。
///
/// 判据若写成「凡非常用皆放行」或忘了数字素簇，这一条会红：`rui` 的候选里会冒出「叡」
/// 以及后面几十个扩展区字，用户看到的是「修完词，候选窗里多出一大堆生僻字」。
/// 第二段断言（`gb18030` 下它在）不可省——没有它，词库里压根没有 `rui → 叡` 这条
/// 也能让用例绿，等于没测。
#[test]
fn single_chars_are_still_filtered() {
    if !dict_ready(&data_dir()) {
        return;
    }
    let kept = candidates("pinyin", "general", "keep", "rui");
    assert!(
        !kept.iter().any(|t| t == "叡"),
        "表外单字不该被词豁免捎带出来，实得 {kept:?}"
    );
    let all = candidates("pinyin", "gb18030", "keep", "rui");
    assert!(
        all.iter().any(|t| t == "叡"),
        "前一条的前提：这个码位下确实有「叡」，它是被过滤掉的而非词库里没有"
    );
}
