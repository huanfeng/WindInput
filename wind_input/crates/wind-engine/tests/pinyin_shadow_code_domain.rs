//! 归一码可行性验证：为「拼音候选置顶」选定 shadow key 的编码域，先用真实引擎把可能的
//! key 碰撞逐条打出来。
//!
//! shadow 规则的存储键是 `"{schema}\0{code}"`，而 `EngineManager::data_schema_id` 已把全拼/双拼
//! 折叠成同一个 `schema`（`"pinyin"`），`code` 取哪个域直接决定两种方案的规则是共享、隔离还是
//! 互相串扰。双拼下同一次输入会同时走两条不同码域的召回路径（见 `sp_abbrev_and_full_coexist`），
//! 分界不能靠推理。
//!
//! 本文件只断言引擎既有行为，作为归一方案前提的回归锁：任何一条翻红都意味着前提变了。
//!
//! 自带 wdat 夹具，不依赖 `build_dev/data`：简拼索引只有 mmap 词典才有，而依赖真实词库的
//! 测试在该目录缺失时会静默跳过。同款做法见 `pinyin_mixed_abbrev.rs` / `pinyin_abbrev_index.rs`。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter};
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

/// 最小 wdat。每个条目都是某条断言的支点：
///
/// | 码 | 词 | 切分 | 简拼 | 作用 |
/// |---|---|---|---|---|
/// | `nihao` | 你好 | ni\|hao | `nh` | 小鹤 `nh` 的简拼解释 |
/// | `nang`  | 囊   | nang    | —    | 小鹤 `nh` 的全拼解释（h=ang） |
/// | `hao`   | 好   | hao     | —    | 小鹤 `hc` 的全拼解释（c=ao），基线用例 |
/// | `xian`  | 先   | xian    | —    | 手动分隔符对照：`xian` 有、`xi'an` 无 |
/// | `xian`  | 西安 | xi\|an  | —    | 手动分隔符对照：两串都有 |
///
/// 权重不参与任何质量闸门：用例都是 1~2 音节的直接查询，只断言「某词在不在候选里」。
fn fixture(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_shadow_code_domain_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // (text, weight, order, boundary)  boundary = 各音节起始字节位
    w.add_with_boundary("nihao".into(), vec![("你好".into(), 5328, 0, 0b101)]);
    w.add_with_boundary("nang".into(), vec![("囊".into(), 812, 0, 0b1)]);
    w.add_with_boundary("hao".into(), vec![("好".into(), 24036, 0, 0b1)]);
    w.add_with_boundary(
        "xian".into(),
        vec![
            ("先".into(), 12000, 0, 0b1),    // xian，1 音节
            ("西安".into(), 3000, 1, 0b101), // xi|an，2 音节
        ],
    );
    w.add_abbrev("nh".into(), vec![("nihao".into(), 9000)]);
    w.write(&wdat).unwrap();

    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

/// 小鹤双拼引擎。键位事实（`data/schemas/shuangpin/xiaohe.toml`）：`h`=ang、`c`=ao。
fn sp_engine(tag: &str) -> PinyinEngine {
    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    PinyinEngine::new(PyConfig::default(), fixture(tag))
        .with_shuangpin(ShuangpinConverter::new(layout))
}

/// 全拼引擎（无双拼转换器）。
fn full_engine(tag: &str) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), fixture(tag))
}

fn texts(e: &PinyinEngine, input: &str) -> Vec<String> {
    e.convert(input, 30)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

/// 引擎给出的 shadow 归一码（空串 = 消费方落回原始击键）。
fn shadow_code(e: &PinyinEngine, input: &str) -> String {
    e.convert(input, 30)
        .map(|r| r.shadow_code)
        .unwrap_or_default()
}

// ───────────────────────── 基线：归一化想达成的效果 ─────────────────────────

/// 双拼击键 `hc` 与全拼 `hao` 指向同一个词：两侧候选集一致，是「把双拼击键归一成全拼码」、
/// 共享一条 shadow 规则的前提。
#[test]
fn sp_keystroke_and_full_pinyin_agree_on_candidates() {
    let sp = sp_engine("baseline_sp");
    let full = full_engine("baseline_full");
    assert!(
        texts(&sp, "hc").contains(&"好".to_string()),
        "小鹤 hc = h(声母) + c(ao) = hao，应出「好」，实际: {:?}",
        texts(&sp, "hc")
    );
    assert!(
        texts(&full, "hao").contains(&"好".to_string()),
        "全拼 hao 应出「好」"
    );
}

// ───────────────────────── C2：双拼简拼串扰（关键冲突） ─────────────────────────

/// 核心证据：双拼下同一次击键会同时产出两条不同码域的候选。
///
/// 小鹤 `nh` 转成全拼 `nang` 出「囊」，同时 `abbr_query` 取原始击键 `nh` 走简拼索引出「你好」。
/// 若把双拼 shadow key 归一成 `nang`，全拼用户在 `nang` 下置顶的词会在双拼打 `nh` 时被顶到首位。
/// 反向则无害：全拼打 `nh` 时「囊」不在候选里，`apply_shadow` 静默忽略（见 `wind-candidate`
/// 的 `missing_pin_word_is_ignored`）。故简拼形态的击键必须保留击键域。
#[test]
fn sp_abbrev_and_full_coexist_in_one_keystroke() {
    let e = sp_engine("coexist");
    let got = texts(&e, "nh");
    assert!(
        got.contains(&"囊".to_string()),
        "nh 经双拼转换 = nang，应出全拼域候选「囊」，实际: {got:?}"
    );
    assert!(
        got.contains(&"你好".to_string()),
        "nh 同时是简拼（abbr_query 取原始击键），应出击键域候选「你好」，实际: {got:?}"
    );
}

/// C2 的对照组：全拼下 `nang` 只有全拼解释。与上一条合起来才证明 `nang` 在全拼侧与双拼侧
/// 指向不同的候选集合，不能作为两者共享的 shadow key。
#[test]
fn full_pinyin_nang_has_no_abbrev_branch() {
    let e = full_engine("nang_control");
    let got = texts(&e, "nang");
    assert!(
        got.contains(&"囊".to_string()),
        "全拼 nang 应出「囊」，实际: {got:?}"
    );
    assert!(
        !got.contains(&"你好".to_string()),
        "全拼 nang 是完整音节、非简拼形态，不该出「你好」，实际: {got:?}"
    );
}

// ───────────────────────── C1：手动分隔符不可剥除 ─────────────────────────

/// 全拼 `xi'an` 与 `xian` 是两个不同的输入：`'` 是硬边界，`xi'an` 出不了单音节的「先」。
/// 所以归一时不能统一剥掉 `'`；全拼路径的归一码取 `input_buffer` 原样，只有双拼路径做击键→全拼转换。
#[test]
fn manual_separator_is_a_distinct_key() {
    let e = full_engine("separator");
    let with_sep = texts(&e, "xi'an");
    let without = texts(&e, "xian");
    assert!(
        without.contains(&"先".to_string()) && without.contains(&"西安".to_string()),
        "xian 无边界约束，两种切分都应出，实际: {without:?}"
    );
    assert!(
        with_sep.contains(&"西安".to_string()),
        "xi'an 应出「西安」，实际: {with_sep:?}"
    );
    assert!(
        !with_sep.contains(&"先".to_string()),
        "`'` 是硬边界，xi'an 不该出单音节的「先」——两串候选集不同，不可合并为一个 key，实际: {with_sep:?}"
    );
}

// ───────────────────────── 归一码取值：上述冲突分析的落地形态 ─────────────────────────

/// `ConvertResult::shadow_code` 的完整取值表：上面几条证明冲突客观存在，这条钉住归一逻辑
/// 按冲突分析做了取舍。判别力自证（改坏任一条都会翻红）：
/// - 判据换成 `!stroke_is_plain_abbrev`（形态判据）→ 第 1 条红：双拼两键里韵母键的字母多半
///   也是合法声母，`is_abbreviation` 普遍判真，归一整体失效；
/// - 加上 `!abbrev_full_hit` → 第 2 条红：能杜绝串扰但让 key 随词库内容摇摆，已按稳定性优先否掉；
/// - 去掉 `mixed_covered` → 第 3 条红：`oy` 那类含无效键对的串会吐出半翻译的脏串；
/// - 让全拼也走归一 → 第 4/5 条红。
#[test]
fn shadow_code_normalization_table() {
    let sp = sp_engine("norm_sp");
    let full = full_engine("norm_full");

    // ① 双拼常态：转换完整覆盖击键 → 归一到全拼，与全拼共享一条规则。
    assert_eq!(
        shadow_code(&sp, "hc"),
        "hao",
        "小鹤 hc 应归一为全拼 hao——这正是本功能要达成的「双拼与全拼共享候选调整」"
    );

    // ② 击键同时有简拼解释时**仍然归一**：key 只取决于双拼布局，不随词库摇摆。
    //    代价是一处已知窄串扰（跨方案混用时），取舍见引擎侧注释。
    assert_eq!(
        shadow_code(&sp, "nh"),
        "nang",
        "nh 虽同时出简拼候选「你好」，key 仍取全拼域——稳定性优先于杜绝串扰"
    );

    // ③ 双拼含无效键对：`oy` 拼不出音节、被原样回写进 full_pinyin，那不是干净的全拼域。
    assert_eq!(
        shadow_code(&sp, "nihaoy"),
        "",
        "含无匹配键对时 full_pinyin 混着未翻译的原始字母，不可作为归一码"
    );

    // ④⑤ 全拼恒空串 = 恒等变换：存量规则零迁移，且 `'` 作为硬边界不被剥除。
    assert_eq!(
        shadow_code(&full, "hao"),
        "",
        "全拼路径必须恒等，不做任何归一"
    );
    assert_eq!(
        shadow_code(&full, "xi'an"),
        "",
        "全拼含手动分隔符时同样恒等——剥掉 `'` 会与 xian 撞 key，而两者候选集不同"
    );
}
