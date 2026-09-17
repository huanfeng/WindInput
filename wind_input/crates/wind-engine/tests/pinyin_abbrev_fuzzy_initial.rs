//! 改**首字母**的模糊组（`n↔l` / `f↔h` / `r↔l`）在简拼召回里的覆盖。
//!
//! 与 `sh↔s` / `zh↔z` / `ch↔c` 的区别是全部要害所在：那三组模糊前后**首字母相同**
//! （都是 s/z/c），而简拼索引的键就是各音节首字母拼起来的串 —— 于是它们天然走得通，
//! 这三组却在**召回侧**就断了：用户打 `nqc`，索引里的「篮球场」挂在 `lqc` 下，
//! `search_abbrev` 根本捞不到，后面的逐段校验一次都不会被调到。
//!
//! 所以 `Initial` 段与 `Syllable` 段是**同一个缺口的两面**，必须一起修：
//! 召回侧枚举键的首字母变体，校验侧放宽 `Initial` 段的比较。
//!
//! 自带 wdat 夹具，理由同 `pinyin_mixed_abbrev.rs`（简拼索引只有 mmap 词典才有）。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::fuzzy::{self, FuzzyConfig};
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

/// 夹具。三组词各覆盖一个改首字母的模糊组：
///
/// | 码 | 词 | 切分 | 简拼键 | 覆盖 |
/// |---|---|---|---|---|
/// | `lanqiuchang`  | 篮球场 | lan\|qiu\|chang  | `lqc` | `n↔l` |
/// | `fangbianmian` | 方便面 | fang\|bian\|mian | `fbm` | `f↔h` |
/// | `richu`        | 日出   | ri\|chu          | `rc`  | `r↔l` |
/// | `nihao`        | 你好   | ni\|hao          | `nh`  | 对照：n 开头的真词 |
fn fixture(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_abbrev_fz_init_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // lan(0..3) qiu(3..6) chang(6..11) → bit 0/3/6
    w.add_with_boundary(
        "lanqiuchang".into(),
        vec![("篮球场".into(), 1191, 0, 0b1001001)],
    );
    // fang(0..4) bian(4..8) mian(8..12) → bit 0/4/8
    //
    // ⚠️ 刻意**不用**「方案」(fa)：`fa` 的模糊对应键 `ha` 本身是个合法音节（哈），
    // 整串被音节覆盖 ⇒ `is_abbreviation` 判假、`mixed_covered` 为真，两条简拼路都不进。
    // 那测的是「这串算不算简拼」，与本文件要测的召回无关。
    w.add_with_boundary(
        "fangbianmian".into(),
        vec![("方便面".into(), 3140, 0, 0b100010001)],
    );
    // ri(0..2) chu(2..5) → bit 0/2
    w.add_with_boundary("richu".into(), vec![("日出".into(), 2380, 0, 0b101)]);
    // nan(0..3) qu(3..5) cai(5..8) → bit 0/3/5。挂在**精确键** `nqc` 下，权重刻意低于
    // 「篮球场」(1191)：支点是「不打折时模糊解反而更高」，一旦纯简拼的模糊命中不吃
    // fuzzy_penalized，它就会把这条精确命中压下去。
    w.add_with_boundary("nanqucai".into(), vec![("南区菜".into(), 800, 0, 0b101001)]);
    // ni(0..2) hao(2..5) → bit 0/2
    w.add_with_boundary("nihao".into(), vec![("你好".into(), 5328, 0, 0b101)]);
    w.add_abbrev("lqc".into(), vec![("lanqiuchang".into(), 1191)]);
    w.add_abbrev("nqc".into(), vec![("nanqucai".into(), 800)]);
    w.add_abbrev("fbm".into(), vec![("fangbianmian".into(), 3140)]);
    w.add_abbrev("rc".into(), vec![("richu".into(), 2380)]);
    w.add_abbrev("nh".into(), vec![("nihao".into(), 5328)]);
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn engine_with(tag: &str, f: FuzzyConfig) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), fixture(tag)).with_fuzzy(f)
}
fn nl(tag: &str) -> PinyinEngine {
    engine_with(
        tag,
        FuzzyConfig {
            n_l: true,
            ..Default::default()
        },
    )
}
fn texts(e: &PinyinEngine, input: &str) -> Vec<String> {
    e.convert(input, 50)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

// ── 纯简拼路径（step 5）──────────────────────────────────────────────

/// `nqc` → 篮球场：整串都是声母，走纯简拼。键 `nqc` 要能找到挂在 `lqc` 下的词。
#[test]
fn plain_abbrev_hits_through_initial_fuzzy() {
    let e = nl("plain_nl");
    let t = texts(&e, "nqc");
    assert!(
        t.contains(&"篮球场".to_string()),
        "开 n_l 后 nqc 应出「篮球场」: {t:?}"
    );
}

/// `f↔h`：`hbm` → 方便面（`fbm` 下的词）。
#[test]
fn plain_abbrev_hits_through_f_h() {
    let e = engine_with(
        "plain_fh",
        FuzzyConfig {
            f_h: true,
            ..Default::default()
        },
    );
    let t = texts(&e, "hbm");
    assert!(
        t.contains(&"方便面".to_string()),
        "开 f_h 后 hbm 应出「方便面」: {t:?}"
    );
}

/// `r↔l`：`lc` → 日出（`rc` 下的词）。
#[test]
fn plain_abbrev_hits_through_r_l() {
    let e = engine_with(
        "plain_rl",
        FuzzyConfig {
            r_l: true,
            ..Default::default()
        },
    );
    let t = texts(&e, "lc");
    assert!(
        t.contains(&"日出".to_string()),
        "开 r_l 后 lc 应出「日出」: {t:?}"
    );
}

// ── 混合简拼路径（step 5b）────────────────────────────────────────────

/// `nanqc` = nan(音节段) + q + c → 篮球场。
///
/// 两处都要放宽才成：键从 `nqc` 变出 `lqc` 才召得回，`Syllable("nan")` 对上 `lan`
/// 才过得了校验（后者已由模糊段校验覆盖）。
#[test]
fn mixed_abbrev_hits_through_initial_fuzzy() {
    let e = nl("mixed_nl");
    let t = texts(&e, "nanqc");
    assert!(
        t.contains(&"篮球场".to_string()),
        "开 n_l 后 nanqc 应出「篮球场」: {t:?}"
    );
}

/// `lqiuc` = l + qiu(音节段) + c → 篮球场。声母段在**开头**、音节段在中间。
#[test]
fn mixed_abbrev_initial_seg_stays_exact_when_possible() {
    let e = nl("mixed_exact");
    let t = texts(&e, "lqiuc");
    assert!(
        t.contains(&"篮球场".to_string()),
        "lqiuc 本就精确，须恒命中: {t:?}"
    );
}

// ── 不开模糊音时不得放宽 ──────────────────────────────────────────────

/// 模糊音全关：`nqc` / `nanqc` 都不得命中「篮球场」。
#[test]
fn fuzzy_off_keeps_initial_strict() {
    let e = PinyinEngine::new(PyConfig::default(), fixture("off"));
    for input in ["nqc", "nanqc"] {
        let t = texts(&e, input);
        assert!(
            !t.contains(&"篮球场".to_string()),
            "模糊音关闭时 {input} 不得命中「篮球场」: {t:?}"
        );
    }
}

/// 只开 `n_l` 时，`f↔h` / `r↔l` 那两组不得顺带生效。
#[test]
fn only_enabled_groups_widen() {
    let e = nl("only_nl");
    assert!(
        !texts(&e, "hbm").contains(&"方便面".to_string()),
        "只开 n_l，f_h 不得生效"
    );
    assert!(
        !texts(&e, "lc").contains(&"日出".to_string()),
        "只开 n_l，r_l 不得生效"
    );
}

/// 精确键仍照常命中，且不因变体枚举而丢失（回归护栏）。
#[test]
fn exact_key_still_hits_with_fuzzy_on() {
    let e = nl("exact_key");
    assert!(
        texts(&e, "lqc").contains(&"篮球场".to_string()),
        "精确键 lqc 须恒命中"
    );
    assert!(
        texts(&e, "nh").contains(&"你好".to_string()),
        "精确键 nh 须恒命中"
    );
}

/// `initials_fuzzy_equal`（无分配，热路径用）与 `initial_alternatives`（枚举，召回侧用）
/// 必须**逐位一致**：一个放宽了另一个没放宽，就是「召回得到但校验判否」或反过来。
#[test]
fn initial_alternatives_matches_pairwise_equal() {
    let letters: Vec<char> = "abcdefghijklmnopqrstuvwxyz".chars().collect();
    // 六组标志位的全部 2^6 组合，逐对交叉验证。
    for bits in 0u8..64 {
        let cfg = FuzzyConfig {
            zh_z: bits & 1 != 0,
            ch_c: bits & 2 != 0,
            sh_s: bits & 4 != 0,
            n_l: bits & 8 != 0,
            f_h: bits & 16 != 0,
            r_l: bits & 32 != 0,
            ..Default::default()
        };
        for &a in &letters {
            let alts = fuzzy::initial_alternatives(a, &cfg);
            for &b in &letters {
                assert_eq!(
                    alts.contains(&b),
                    fuzzy::initials_fuzzy_equal(a, b, &cfg),
                    "bits={bits} a={a} b={b}: 两个判据不一致"
                );
            }
        }
    }
}

// ── 折扣：模糊召回不得压过精确解 ────────────────────────────────────────

/// 纯简拼路径的模糊命中必须吃 `fuzzy_penalized` 折扣并标 `is_fuzzy`。
///
/// **`is_abbrev` 沉底救不了这个**：那只把简拼整层压到全拼之后，**层内仍按 weight 降序**。
/// 折扣是层内唯一区分「精确键命中」与「变体键命中」的机制 —— 去掉它，`lqc` 下权重更高的
/// 词会把唯一那条 `nqc` 精确词直接挤下去，而真实词库里热键的头部权重远高于冷门键，
/// 这是必然发生而非可能发生。
///
/// 同一个不变量在**混合**路径由 `pinyin_abbrev_recall.rs` 的
/// `fuzzy_mixed_abbrev_is_penalized_and_marked` 守着，纯简拼这条当初漏了。
#[test]
fn plain_abbrev_fuzzy_is_penalized_and_ordered_after_exact() {
    let e = nl("plain_penalty");
    let r = e.convert("nqc", 50).expect("convert 成功");

    let fz = r
        .candidates
        .iter()
        .find(|c| c.text == "篮球场")
        .expect("nqc 应经变体键 lqc 召回「篮球场」");
    let ex = r
        .candidates
        .iter()
        .find(|c| c.text == "南区菜")
        .expect("nqc 是「南区菜」的精确键，须一并召回");

    // 键 nqc→lqc 只有首位不同 ⇒ 1 处 ⇒ 1191 × 0.5 = 595.5 → 596。
    assert!(fz.is_fuzzy, "变体键命中须标 is_fuzzy");
    assert_eq!(fz.weight, 596, "变体键命中须按处数折扣");
    assert!(!ex.is_fuzzy, "精确键命中不得标 is_fuzzy");
    assert_eq!(ex.weight, 800, "精确键命中权重须原样");

    // 折扣的意义：不打折时 1191 > 800，次序会反过来。
    let pos = |t: &str| r.candidates.iter().position(|c| c.text == t).unwrap();
    assert!(
        pos("南区菜") < pos("篮球场"),
        "精确解须排在模糊解之前: {:?}",
        r.candidates
            .iter()
            .map(|c| (&c.text, c.weight, c.is_fuzzy))
            .collect::<Vec<_>>()
    );
}
