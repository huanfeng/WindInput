//! `${code_schema}` 的数据来源：候选的全拼 `code` + `boundary` → 本方案的双拼击键。
//!
//! 这条链路回答的是 GH#128 的问题「字或词的后面能不能显示双拼的编码」。它与
//! `${code_rev}`（拿候选文本去码表词库查反向索引）**不是一回事**：双拼编码不存在于
//! 任何词库里，双拼只是「全拼词库 + 一张键盘布局」，所以只能算，不能查。
//!
//! ⚠️ 这些用例**不需要真词库**：`schema_keys_of` 只吃候选已经带着的 `code`/`boundary`
//! 与方案的布局文件，不碰词典、不碰反查索引。故用仓库自带的 `data/` 作数据目录即可，
//! 不像 `shuangpin_separator.rs` 那样要 gate 在 `build_dev/data` 上。

use std::path::PathBuf;

use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data")
}

/// 出厂 `shuangpin` 方案用小鹤布局、`pinyin` 是全拼、`wubi86` 是码表。
fn mgr(active: &str) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec![
        "shuangpin".to_string(),
        "pinyin".to_string(),
        "wubi86".to_string(),
        "wubi86_pinyin".to_string(),
    ];
    cfg.schema.active = active.to_string();
    EngineManager::new(&cfg, Some(&data_dir()))
}

/// `你好` 的 boundary：code = `nihao`，音节起点 {0, 2} ⇒ `0b101`。
const NIHAO: u64 = 0b101;

/// ★ 核心：双拼方案下，候选的全拼码被算成用户实际要敲的键。
///
/// 小鹤下「你好」= `nihc`（`ni` 声母韵母各一键，`hao` = h + c）。这个值与
/// `shuangpin_separator.rs` 里真机链路用的是同一个，两处对不上就是有一处错了。
#[test]
fn shuangpin_schema_gives_keystrokes() {
    let m = mgr("shuangpin");
    assert_eq!(m.schema_keys_of("nihao", NIHAO).as_deref(), Some("nihc"));
    // 单音节、多音节都走同一条路。
    assert_eq!(m.schema_keys_of("ni", 0b1).as_deref(), Some("ni"));
    assert_eq!(
        m.schema_keys_of("zhongguo", 0b100001).as_deref(),
        Some("vsgo"),
        "zhong = v(zh) + s(ong)，guo = g + o(uo)"
    );
}

/// 全拼方案恒空——击键就是 `code` 本身，显示它是冗余。
///
/// 这与 `${code_rev}` 在码表方案下恒空是**同一条理由**（候选的码就是用户自己打的）。
/// 两处若给出不一致的取舍，用户会觉得其中一个坏了。
#[test]
fn full_pinyin_schema_has_no_keystroke_code() {
    assert_eq!(mgr("pinyin").schema_keys_of("nihao", NIHAO), None);
}

/// 码表方案恒空：它根本没有「双拼布局」这回事。
#[test]
fn codetable_schema_has_no_keystroke_code() {
    assert_eq!(mgr("wubi86").schema_keys_of("nihao", NIHAO), None);
}

/// 混输方案（`engine.type = "mixed"`）恒空——这是**取舍**，不是遗漏。
///
/// 混输下用户敲的是「主码表码 + 拼音码」的混合流，给出单一的双拼击键串会误导：
/// 那不是他在这个模式里实际要敲的东西。对照 `code_source_schema` 对 Mixed 是有转发的，
/// 因为「这个词的码表编码是什么」与输入方式无关，而击键恰恰就是输入方式本身。
///
/// 这条测试的作用是把这个取舍钉成可见的决定——否则下一个读者无从判断恒空是设计还是 bug。
#[test]
fn mixed_schema_has_no_keystroke_code() {
    assert_eq!(mgr("wubi86_pinyin").schema_keys_of("nihao", NIHAO), None);
}

/// ★ `boundary == 0` 不出编码。
///
/// 模糊音变体命中与用户手输码的词条一律置 0（引擎侧「模糊变体命中一律 boundary=0
/// （不设防）」）。此时音节切分只能靠 DAG 猜，而它偏向少音节——`xian` 会被猜成一个
/// 音节而不是 `xi|an`。切错了显示出来的编码就是**错的**：用户照着敲得到别的字。
/// 宁可这一次不显示。
#[test]
fn zero_boundary_is_not_guessed() {
    let m = mgr("shuangpin");
    assert_eq!(m.schema_keys_of("nihao", 0), None);
    // 首位未置位同样是坏数据（第一个音节不从 0 开始），一样不猜。
    assert_eq!(m.schema_keys_of("nihao", 0b100), None);
    // ★ 即使 code 本身正好就是一个合法音节，boundary == 0 也不出编码。
    // 判据是「boundary 可不可信」，不是「这串能不能凑出音节」——按 code 长度分情况
    // 就等于有了第二条规则，而两条规则迟早会对同一个候选给出不同答案。
    assert_eq!(m.schema_keys_of("hao", 0), None, "单音节也不例外");
}

/// 查不到的音节 ⇒ 整串不出，不给半截。
///
/// 半截击键串是**错**的答案而不是不完整的答案。
#[test]
fn unknown_syllable_yields_nothing() {
    let m = mgr("shuangpin");
    assert_eq!(m.schema_keys_of("zzz", 0b1), None);
    assert_eq!(
        m.schema_keys_of("nizzz", 0b101),
        None,
        "前一个音节查得到也不给半截"
    );
    assert_eq!(m.schema_keys_of("", 0b1), None);
}

/// 缓存命中路径与首次构建路径必须给同一个答案。
#[test]
fn cache_hit_matches_first_build() {
    let sp = mgr("shuangpin");
    assert_eq!(sp.schema_keys_of("nihao", NIHAO).as_deref(), Some("nihc"));
    assert_eq!(sp.schema_keys_of("nihao", NIHAO).as_deref(), Some("nihc"));
}

/// ★ 同一个方案 id、布局却变了——这是单槽缓存**按 id 比对救不了**的那一格。
///
/// 用户在设置页把双拼布局从小鹤换成微软，写的是 `schema_overrides/shuangpin.toml`，
/// 方案 id 自始至终是 `shuangpin`。若只靠 `cache.0 == id` 判断，缓存永远命中，
/// 候选后面会一直挂着小鹤的键——一个只在「改过布局的用户」身上出现、且看起来像
/// 「编码显示错了」的 bug。`reload_from_config` 里那句失效就是为它而设。
///
/// 切换**活跃方案**反而不需要失效点：id 变了缓存自然 miss。两者别混为一谈。
#[test]
fn cache_is_invalidated_when_layout_changes_under_same_schema_id() {
    let dir = temp_data_dir("layout_swap");
    write_shuangpin_schema(&dir, "xiaohe");

    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    let m = EngineManager::new(&cfg, Some(&dir));

    // 先把小鹤那份建进缓存。
    assert_eq!(
        m.schema_keys_of("nihao", NIHAO).as_deref(),
        Some("nihc"),
        "小鹤：hao = h + c(ao)"
    );

    // 用户改布局：同一个方案 id，换一份 layout。
    write_shuangpin_schema(&dir, "mspy");
    m.reload_from_config(&cfg);

    assert_eq!(
        m.schema_keys_of("nihao", NIHAO).as_deref(),
        Some("nihk"),
        "微软：hao = h + k(ao)。仍得 nihc 说明反向表没跟着布局失效"
    );
}

/// 造一个只含双拼方案与内置布局的临时数据目录。
fn temp_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_sp_rev_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let sp = dir.join("schemas/shuangpin");
    std::fs::create_dir_all(&sp).expect("建临时 schemas 目录");
    for id in ["xiaohe", "mspy"] {
        std::fs::copy(
            data_dir().join(format!("schemas/shuangpin/{id}.toml")),
            sp.join(format!("{id}.toml")),
        )
        .expect("复制内置布局");
    }
    dir
}

fn write_shuangpin_schema(dir: &std::path::Path, layout: &str) {
    std::fs::write(
        dir.join("schemas/shuangpin.schema.toml"),
        format!(
            "[schema]\nid = \"shuangpin\"\nname = \"双拼\"\n\
             [engine]\ntype = \"pinyin\"\n\
             [engine.pinyin]\nscheme = \"shuangpin\"\n\
             [engine.pinyin.shuangpin]\nlayout = \"{layout}\"\n"
        ),
    )
    .expect("写方案文件");
}
