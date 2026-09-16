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
use wind_engine::pinyin::fuzzy::FuzzyConfig;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_engine::{ConvertOptions, Engine};

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
    // 与「生日快乐」**同一个声母投影键 `srkl`** 的精确解：sen|ri|kuai|le，权重刻意低一截。
    // 支点在于「不打折时模糊那条反而更高」：1806 > 1000，一旦模糊命中不吃 fuzzy_penalized
    // 折扣，它就会把这条精确命中压下去 —— 那正是要守住的不变量。
    // sen(0..3) ri(3..5) kuai(5..9) le(9..11) → bit 0/3/5/9
    w.add_with_boundary(
        "senrikuaile".into(),
        vec![("森日快乐".into(), 1000, 0, 0b1000101001)],
    );
    w.add_abbrev("hpl".into(), vec![("haopiaoliang".into(), 120)]);
    w.add_abbrev(
        "srkl".into(),
        vec![("shengrikuaile".into(), 1806), ("senrikuaile".into(), 1000)],
    );
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
///
/// ⚠️ **limit 的取值是本用例的全部要害**，两侧各有一个会让它退化成假护栏的早退分支：
/// - `limit < ABBREV_QUOTA_DIVISOR`(10) ⇒ `quota == 0` ⇒ 第二个早退，测到的是短路；
/// - `limit >= 候选总数`（夹具里 `hao` 恰好 20 条）⇒ `cands.len() <= max` ⇒ 第一个早退。
///
/// 只有 `10 <= limit < 20` 能真正走进腾位逻辑。取 10：`quota = 1`、总数 20 > 10，
/// 于是 `kept == 0`、`extra` 为空 —— 正是「该补的没有，于是一条都不许挤」那条路径。
#[test]
fn quota_does_not_displace_when_no_abbrev_candidate() {
    let e = engine("trunc_noop");
    let r = e.convert("hao", 10).expect("convert 成功");
    let c = &r.candidates;
    // 前提自检：夹具确实产出了超过 limit 的候选，且其中没有简拼候选。
    // 任一条不成立，下面的断言就测不到腾位逻辑（见上方 doc）。
    assert_eq!(
        e.convert("hao", 40).unwrap().candidates.len(),
        20,
        "前提：hao 的候选总数须 > limit(10)，否则走 len <= max 的早退"
    );
    assert!(
        !c.iter().any(|x| x.is_abbrev),
        "前提：hao 是完整音节，不该有简拼候选"
    );

    assert_eq!(c.len(), 10, "无简拼可补时须恰好截到 limit，不得少一条");
    assert_eq!(c[0].text, "好", "首选不受配额影响");
    // 腾位一条都没发生：10 席仍全是 hao 的同音字，没被替换成别的东西。
    assert!(
        c.iter().all(|x| x.code == "hao"),
        "不得挤掉任何候选: {:?}",
        c.iter().map(|x| (&x.text, &x.code)).collect::<Vec<_>>()
    );
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
    for tag in ["exact_off", "exact_on"] {
        let e = if tag == "exact_off" {
            engine(tag)
        } else {
            fuzzy_engine(tag)
        };
        let t = texts(&e, "shengrikl", 50);
        assert!(
            t.contains(&"生日快乐".to_string()),
            "[{tag}] shengrikl 应恒命中「生日快乐」: {t:?}"
        );
    }
}

// ── Stage 2 的不变量：模糊命中必须比精确命中低一档 ────────────────────────

/// 模糊命中的简拼候选须吃 `fuzzy_penalized` 折扣并标 `is_fuzzy`。
///
/// 「模糊命中恒低精确命中一档」是全仓不变量（`FUZZY_WEIGHT_SCALE` 那段论证，
/// 词图侧另有同轴的 `FUZZY_SYLLABLE_LOG_PENALTY`）。混合简拼的段校验是后开的一条
/// 召回通路，**不是这条不变量的例外** —— 否则同一个投影键下，一条权重更高的模糊命中
/// 会盖过精确命中。
#[test]
fn fuzzy_mixed_abbrev_is_penalized_and_marked() {
    let e = fuzzy_engine("penalty");
    let r = e.convert("senrikl", 50).expect("convert 成功");

    let fz = r
        .candidates
        .iter()
        .find(|c| c.text == "生日快乐")
        .expect("senrikl 应召回模糊命中的「生日快乐」");
    // sen→sheng 改了声母与韵母两处 ⇒ 1806 × 0.5² = 452（四舍五入）
    assert!(fz.is_fuzzy, "模糊命中须标 is_fuzzy");
    assert_eq!(fz.weight, 452, "模糊命中须按改动处数折扣(1806 × 0.5² )");

    let ex = r
        .candidates
        .iter()
        .find(|c| c.text == "森日快乐")
        .expect("senrikl 对 sen|ri|kuai|le 是精确解，应同时召回");
    assert!(!ex.is_fuzzy, "精确命中不得标 is_fuzzy");
    assert_eq!(ex.weight, 1000, "精确命中不得被折扣");

    // 折扣的意义：精确解压过原始权重更高的模糊解。不打折时 1806 > 1000，顺序会反。
    let pos = |t: &str| r.candidates.iter().position(|c| c.text == t).unwrap();
    assert!(
        pos("森日快乐") < pos("生日快乐"),
        "精确命中须排在模糊命中之前: {:?}",
        r.candidates
            .iter()
            .map(|c| (&c.text, c.weight, c.is_fuzzy))
            .collect::<Vec<_>>()
    );
}

/// 精确路径不受本次改动影响：`shengrikl` 命中「生日快乐」时权重原样、不标 fuzzy。
#[test]
fn exact_mixed_abbrev_keeps_raw_weight() {
    let e = fuzzy_engine("exact_weight");
    let r = e.convert("shengrikl", 50).expect("convert 成功");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "生日快乐")
        .expect("应命中");
    assert!(!c.is_fuzzy, "精确段校验不得标 is_fuzzy");
    assert_eq!(c.weight, 1806, "精确命中权重须原样");
}

/// step 6.2 **简拼族前缀回退**路径同样要走模糊段校验。
///
/// 那条路是另一组调用点（`recall_abbrev_prefix`，与 step 5b 的系统词库路径各走各的），
/// 只测 step 5b 会让它改回精确比较也不报警。它还是最值得盯的一处：候选自带**击键域**的
/// `consumed_length`，而模糊命中让击键与词典音节对不齐。
#[test]
fn fuzzy_mixed_abbrev_hits_via_prefix_fallback() {
    let e = fuzzy_engine("prefix_fb");
    // 整串 `senriklx` 无解（`x` 成不了音节也配不上任何段）⇒ 退到最长可命中前缀 `senrikl`。
    let r = e.convert("senriklx", 50).expect("convert 成功");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "生日快乐")
        .unwrap_or_else(|| {
            panic!(
                "前缀回退路径应召回模糊命中的「生日快乐」，实际: {:?}",
                r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
            )
        });
    assert!(c.is_fuzzy, "前缀回退路径的模糊命中同样须标 is_fuzzy");
    assert_eq!(c.weight, 452, "前缀回退路径同样须吃折扣(1806 × 0.5²)");
    assert!(
        c.consumed_length > 0 && c.consumed_length < "senriklx".len(),
        "前缀回退须只消费前缀、留残码续输，实际 consumed={}",
        c.consumed_length
    );
}

/// 调用方声明不重排时，配额必须**整个让路**，等价于裸 `truncate`。
///
/// 补位把简拼候选放在尾部并腾位挤掉等量的既有候选，这在会重排的调用方那里是「进得来」，
/// 在不重排的调用方那里就是**净损失** —— 挤掉的名额不会回来。生僻字模式是现场：
/// 它刻意不排序，而引擎按常用度排序 ⇒ 尾部正是它要的生僻字；补进来的简拼词又会被
/// 「只出单字」删光。详见 `ConvertOptions::no_abbrev_quota`。
#[test]
fn no_abbrev_quota_is_equivalent_to_plain_truncate() {
    let e = engine("no_quota");
    let no_quota = ConvertOptions {
        no_abbrev_quota: true,
        ..Default::default()
    };

    // 前提：默认配额下「好漂亮」确实是靠补位才进来的（同
    // `abbrev_candidate_survives_truncation`）。这条不成立则下面测不到东西。
    let with = e.convert("haopl", 10).expect("convert 成功");
    assert!(
        with.candidates.iter().any(|c| c.text == "好漂亮"),
        "前提：默认配额会把简拼候选补进来"
    );

    let without = e
        .convert_with_opts("haopl", 10, no_quota)
        .expect("convert 成功");
    assert!(
        !without.candidates.iter().any(|c| c.text == "好漂亮"),
        "关掉配额后不得补位: {:?}",
        without
            .candidates
            .iter()
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );

    // 要害断言：**也不得腾位**。关掉配额的结果须与「不截断时取前 N 条」逐条相同 ——
    // 只验「没补进来」会漏掉「补了又挤掉别的」这种更糟的形态。
    let full = e.convert("haopl", 100).expect("convert 成功");
    let expect: Vec<&String> = full.candidates.iter().take(10).map(|c| &c.text).collect();
    let actual: Vec<&String> = without.candidates.iter().map(|c| &c.text).collect();
    assert_eq!(actual, expect, "关掉配额须逐条等价于裸 truncate");
}
