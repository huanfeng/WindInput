//! 混合简拼的两条召回断裂：截断配额缺失、段校验不走模糊音。
//!
//! **自带 wdat 夹具，不依赖 `build_dev/data`** —— 理由同 `pinyin_mixed_abbrev.rs`：
//! 简拼索引只有 mmap 词典才有，而依赖真实词库的测试在该目录缺失时会静默跳过、计数照常绿。
//!
//! 两条断裂的真机表现都是「同一个词，多打或少打一个字母、或者开个模糊音，就没了」：
//!
//! | 输入 | 模糊音 | 真实词库下的位次（limit=300） |
//! |---|---|---|
//! | `shengrikl` | 关 | 第 121 位，活 |
//! | `shengrikl` | 开 | 第 337 位，**被截** |
//! | `senrikl`   | 任意 | **未产生** |

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::fuzzy::FuzzyConfig;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

/// 夹具。两组词各自支撑一个 Stage：
///
/// | 码 | 词 | 切分 | 简拼键 | 用途 |
/// |---|---|---|---|---|
/// | `haopiaoliang` | 好漂亮 | hao\|piao\|liang | `hpl` | Stage 1 截断配额 |
/// | `shengrikuaile` | 生日快乐 | sheng\|ri\|kuai\|le | `srkl` | Stage 2 模糊段校验 |
///
/// `hao` 码下另塞 20 个同音字：它们经 step 3 子短语进候选，把 `max_candidates` 占满，
/// 从而复现「简拼候选恒沉底 ⇒ 总数一涨就被整批截掉」。权重取真实量级（`cn_dicts` 口径）。
fn fixture(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_abbrev_recall_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // hao(0..3) piao(3..7) liang(7..12) → bit 0/3/7
    w.add_with_boundary(
        "haopiaoliang".into(),
        vec![("好漂亮".into(), 120, 0, 0b10001001)],
    );
    // sheng(0..5) ri(5..7) kuai(7..11) le(11..13) → bit 0/5/7/11
    w.add_with_boundary(
        "shengrikuaile".into(),
        vec![("生日快乐".into(), 1806, 0, 0b100010100001)],
    );
    // `hao` 的同音字群：单音节、无简拼语义，纯粹用来把候选席位占满。
    let hao_chars = [
        "好", "号", "毫", "豪", "耗", "浩", "壕", "嚎", "蚝", "郝", "皓", "蒿", "薅", "貉", "嗥",
        "灏", "颢", "镐", "昊", "澔",
    ];
    w.add_with_boundary(
        "hao".into(),
        hao_chars
            .iter()
            .enumerate()
            .map(|(i, c)| ((*c).into(), 9000 - i as i32 * 100, i as u32, 0b1))
            .collect(),
    );
    w.add_abbrev("hpl".into(), vec![("haopiaoliang".into(), 120)]);
    w.add_abbrev("srkl".into(), vec![("shengrikuaile".into(), 1806)]);
    w.write(&wdat).unwrap();

    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn engine(tag: &str) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), fixture(tag))
}

/// 开「sh↔s + en↔eng」——真机上打出 `senrikl` 要的正是这两组同时生效。
fn fuzzy_engine(tag: &str) -> PinyinEngine {
    let f = FuzzyConfig {
        sh_s: true,
        en_eng: true,
        ..Default::default()
    };
    PinyinEngine::new(PyConfig::default(), fixture(tag)).with_fuzzy(f)
}

fn texts(e: &PinyinEngine, input: &str, limit: usize) -> Vec<String> {
    e.convert(input, limit)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

// ── Stage 1：截断保底配额 ──────────────────────────────────────────────

/// 简拼候选**恒在最末位**（`cmp_match_layers` 里 `is_abbrev` 是最沉的层），
/// 一旦同层之外的候选数超过 `max_candidates`，它就整批消失。
///
/// 真机复现路径是模糊音：`shengrikl` 的候选总数从 121（关）涨到 337（开），
/// 而「生日快乐」恒是最后一条，于是越过协调器的 limit=300 被 `truncate` 丢弃。
/// 这里用 20 个 `hao` 同音字 + 小 limit 造出同一个形状，不依赖模糊音也不依赖真实词库。
#[test]
fn abbrev_candidate_survives_truncation() {
    let e = engine("trunc");
    let t = texts(&e, "haopl", 10);
    assert!(
        t.contains(&"好漂亮".to_string()),
        "简拼候选须有保底席位，不能因同音字占满配额被整批截掉: {t:?}"
    );
}

/// 配额**只补不挤空**：没有简拼候选可补时，截断行为与改动前逐条一致。
#[test]
fn quota_does_not_displace_when_no_abbrev_candidate() {
    let e = engine("trunc_noop");
    let t = texts(&e, "hao", 5);
    assert_eq!(t.len(), 5, "无简拼候选时须恰好截到 limit: {t:?}");
    assert_eq!(t[0], "好", "首选不受配额影响");
}

// ── Stage 2：混合简拼走模糊音 ──────────────────────────────────────────

/// `senrikl` = sen(模糊音段) + ri + k + l。
///
/// 投影键 `srkl` 本就能召回 `shengrikuaile`，卡在逐段校验：`AbbrevSeg::Syllable` 走的是
/// 精确字符串相等，`sen` 永远等不上词典里的 `sheng`。
#[test]
fn mixed_abbrev_matches_through_fuzzy_syllable() {
    let e = fuzzy_engine("fuzzy_seg");
    let t = texts(&e, "senrikl", 50);
    assert!(
        t.contains(&"生日快乐".to_string()),
        "开了 sh_s+en_eng 后 senrikl 应召回「生日快乐」: {t:?}"
    );
}

/// 不开模糊音时 `senrikl` **不该**命中——模糊匹配只在用户明确开启时放宽。
#[test]
fn fuzzy_off_keeps_mixed_abbrev_strict() {
    let e = engine("fuzzy_off");
    let t = texts(&e, "senrikl", 50);
    assert!(
        !t.contains(&"生日快乐".to_string()),
        "模糊音关闭时不得放宽段校验: {t:?}"
    );
}

/// 不含模糊音的混合简拼照旧命中（回归护栏：改段校验不能动到精确路径）。
#[test]
fn exact_mixed_abbrev_still_hits() {
    for (tag, e) in [
        ("exact_off", engine("exact_off")),
        ("exact_on", fuzzy_engine("exact_on")),
    ] {
        let t = texts(&e, "shengrikl", 50);
        assert!(
            t.contains(&"生日快乐".to_string()),
            "[{tag}] shengrikl 应恒命中「生日快乐」: {t:?}"
        );
    }
}
