//! `${code_schema}` 的数据来源：候选的全拼 `code` + `boundary` → 本方案的双拼击键。
//!
//! 与 `${code_rev}`（拿候选文本查码表反向索引）不同：双拼编码不存在于任何词库，只是「全拼
//! 词库 + 一张键盘布局」，只能算不能查。
//!
//! 不需要真词库：`shuangpin_code_of` 只吃候选带的 `code`/`boundary` 与布局文件，故用仓库自带的
//! `data/` 即可，不必 gate 在 `build_dev/data` 上。

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

/// 双拼方案下，候选的全拼码被算成用户实际要敲的键。
///
/// 小鹤下「你好」= `nihc`（`hao` = h + c），与 `shuangpin_separator.rs` 真机链路用的值相同，
/// 两处对不上就是有一处错了。
#[test]
fn shuangpin_schema_gives_keystrokes() {
    let m = mgr("shuangpin");
    assert_eq!(m.shuangpin_code_of("nihao", NIHAO).as_deref(), Some("nihc"));
    // 单音节、多音节都走同一条路。
    assert_eq!(m.shuangpin_code_of("ni", 0b1).as_deref(), Some("ni"));
    assert_eq!(
        m.shuangpin_code_of("zhongguo", 0b100001).as_deref(),
        Some("vsgo"),
        "zhong = v(zh) + s(ong)，guo = g + o(uo)"
    );
}

/// 活跃方案不是双拼时，回退到已装的双拼方案。
///
/// 想看双拼编码的多半是用全拼打字、正往双拼迁移的人，所以不能只认活跃方案。
/// 本函数是引擎层 API，只回答「这串拼音的双拼编码是什么」，不带候选来源门控；
/// 「码表候选该不该显示」是注释层 `${shuangpin}` 那道 `source == Pinyin` 的事。
#[test]
fn non_shuangpin_schemas_fall_back_to_installed_shuangpin() {
    for active in ["pinyin", "wubi86", "wubi86_pinyin"] {
        assert_eq!(
            mgr(active).shuangpin_code_of("nihao", NIHAO).as_deref(),
            Some("nihc"),
            "{active}：应回退到 available 里的双拼方案取布局"
        );
    }
}

/// 一个双拼方案都没装 ⇒ 恒空。回退链走到头也找不到布局，无从谈起。
#[test]
fn no_shuangpin_schema_installed_yields_none() {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string(), "wubi86".to_string()];
    cfg.schema.active = "pinyin".to_string();
    let m = EngineManager::new(&cfg, Some(&data_dir()));
    assert_eq!(m.shuangpin_code_of("nihao", NIHAO), None);
}

/// `primary_pinyin` 显式指向某个双拼方案时，优先于 `available` 的扫描顺序。
///
/// 与 `resolve_primary_codetable` 同一判据：显式配置优先，没配才按 available 顺序找。
#[test]
fn primary_pinyin_wins_over_available_scan() {
    let mut cfg = Config::default();
    // available 里把双拼排在最后，靠 primary_pinyin 指名才轮得到它。
    cfg.schema.available = vec![
        "pinyin".to_string(),
        "wubi86".to_string(),
        "shuangpin".to_string(),
    ];
    cfg.schema.active = "pinyin".to_string();
    cfg.schema.primary_pinyin = "shuangpin".to_string();
    let m = EngineManager::new(&cfg, Some(&data_dir()));
    assert_eq!(m.shuangpin_code_of("nihao", NIHAO).as_deref(), Some("nihc"));
}

/// `boundary == 0` 不出编码。
///
/// 模糊音变体命中与用户手输码的词条 boundary 一律为 0，切分只能靠 DAG 猜且偏向少音节
/// （`xian` 被猜成一个音节而非 `xi|an`）。切错了显示的编码就是错的，宁可不显示。
#[test]
fn zero_boundary_is_not_guessed() {
    let m = mgr("shuangpin");
    assert_eq!(m.shuangpin_code_of("nihao", 0), None);
    // 首位未置位同样是坏数据（第一个音节不从 0 开始），一样不猜。
    assert_eq!(m.shuangpin_code_of("nihao", 0b100), None);
    // 即使 code 本身正好是一个合法音节，boundary == 0 也不出编码：判据是 boundary 可不可信，
    // 按 code 长度分情况等于第二条规则，迟早与第一条对同一候选给出不同答案。
    assert_eq!(m.shuangpin_code_of("hao", 0), None, "单音节也不例外");
}

/// 查不到的音节 ⇒ 整串不出，不给半截：半截击键串是错的答案而不是不完整的答案。
#[test]
fn unknown_syllable_yields_nothing() {
    let m = mgr("shuangpin");
    assert_eq!(m.shuangpin_code_of("zzz", 0b1), None);
    assert_eq!(
        m.shuangpin_code_of("nizzz", 0b101),
        None,
        "前一个音节查得到也不给半截"
    );
    assert_eq!(m.shuangpin_code_of("", 0b1), None);
}

/// 缓存命中路径与首次构建路径必须给同一个答案。
#[test]
fn cache_hit_matches_first_build() {
    let sp = mgr("shuangpin");
    assert_eq!(
        sp.shuangpin_code_of("nihao", NIHAO).as_deref(),
        Some("nihc")
    );
    assert_eq!(
        sp.shuangpin_code_of("nihao", NIHAO).as_deref(),
        Some("nihc")
    );
}

/// 同一个方案 id、布局却变了：单槽缓存按 id 比对救不了。
///
/// 设置页把双拼布局从小鹤换成微软写的是 `schema_overrides/shuangpin.toml`，方案 id 不变；
/// 只靠 `cache.0 == id` 判断会一直挂着小鹤的键。`reload_from_config` 里的失效就是为它而设。
/// 切换活跃方案则不需要失效点：id 变了缓存自然 miss。
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
        m.shuangpin_code_of("nihao", NIHAO).as_deref(),
        Some("nihc"),
        "小鹤：hao = h + c(ao)"
    );

    // 用户改布局：同一个方案 id，换一份 layout。
    write_shuangpin_schema(&dir, "mspy");
    m.reload_from_config(&cfg);

    assert_eq!(
        m.shuangpin_code_of("nihao", NIHAO).as_deref(),
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
