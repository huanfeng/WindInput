//! 双字母声母（`zh`/`ch`/`sh`）作为**一个**简拼段。
//!
//! 简拼域此前有一条硬假设：一个音节恰好占一个 ASCII 字母。于是 `zh` 只能被解释成
//! `z`+`h` 两段，用户敲「这个」的 `zhge` 得到的是 3 音节词（之后个 / 镇魂歌），
//! 而「这个」挂在键 `zg` 下、是 2 音节 —— 段数校验当场判否。
//!
//! 真机复现（`build_dev/data`，`wind_repl`，limit=300）：
//!
//! | 输入 | 组合区 | 结果 |
//! |---|---|---|
//! | `zg`     | `z'g`    | 「这个」第 1 |
//! | `zhge`   | `z'h'ge` | 之后个 / 镇魂歌 / 泽火革，**无「这个」** |
//! | `zhy`    | `z'h'y`  | 总会有 / 最好用，**无「这样」**（它挂在 `zy`） |
//! | `baichx` | `bai'chx`| 只有 `bai` 的单字，**一条词都无** |
//!
//! 组合区把根因直接显示给了用户：`z'h'ge`。
//!
//! 修好后三者都落到已有的键上：`zh|ge`→`zg`、`zh|y`→`zy`、`bai|ch|x`→`bcx`，
//! **索引一个字节都不用动**——双字母声母的投影首字母本来就是 z/c/s。
//!
//! 自带 wdat 夹具，不依赖 `build_dev/data`（同 `pinyin_mixed_abbrev.rs` 的理由）。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::fuzzy::FuzzyConfig;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

/// 夹具词表：码、词、权重、boundary（bit i = 位置 i 是音节起点）。
///
/// 每个简拼键下都刻意放了**同键的竞争词**，用来钉住「双字母声母确实在收紧判据」：
/// `zhy` 只该出 zhe|… 的词，不该把 `zy` 键下 zi|…、zuo|… 的词一并捞回来。
const ENTRIES: &[(&str, &str, i32, u64)] = &[
    ("zhege", "这个", 555006, 0b1001),          // zhe|ge        → 0,3
    ("zheyang", "这样", 143944, 0b1001),        // zhe|yang      → 0,3
    ("zhiyou", "只有", 152949, 0b1001),         // zhi|you       → 0,3  ← 同键 zy 的竞争词
    ("zuoyong", "作用", 55558, 0b1001),         // zuo|yong      → 0,3  ← 同上
    ("zhonghua", "中华", 20000, 0b10001),       // zhong|hua     → 0,5
    ("baichengxian", "拜城县", 1, 0b100001001), // bai|cheng|xian → 0,3,8
    ("buchuxian", "不出现", 517, 0b100101),     // bu|chu|xian   → 0,2,5  ← 同键 bcx 的竞争词
    ("nihao", "你好", 5328, 0b101),             // ni|hao        → 0,2    ← 单字母声母不回归
    ("chengshi", "城市", 30000, 0b100001),      // cheng|shi     → 0,5
    ("shanghai", "上海", 40000, 0b100001),      // shang|hai     → 0,5
];

/// 简拼索引：键 → 该键下的全拼码（按权重降序，`search_abbrev` 据此截断）。
const ABBREV: &[(&str, &[&str])] = &[
    ("zg", &["zhege"]),
    ("zy", &["zhiyou", "zheyang", "zuoyong"]), // 权重序：只有 > 这样 > 作用
    ("zh", &["zhonghua"]),
    ("bcx", &["buchuxian", "baichengxian"]),
    ("nh", &["nihao"]),
    ("cs", &["chengshi"]),
    ("sh", &["shanghai"]),
];

fn dict(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_retroflex_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    for (code, text, weight, boundary) in ENTRIES {
        w.add_with_boundary(
            (*code).into(),
            vec![((*text).into(), *weight, 0, *boundary)],
        );
    }
    let weight_of = |code: &str| {
        ENTRIES
            .iter()
            .find(|(c, ..)| *c == code)
            .map(|(_, _, w, _)| *w)
            .unwrap_or(0)
    };
    for (key, codes) in ABBREV {
        w.add_abbrev(
            (*key).into(),
            codes.iter().map(|c| ((*c).into(), weight_of(c))).collect(),
        );
    }
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn engine(tag: &str) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), dict(tag))
}

fn texts(e: &PinyinEngine, input: &str) -> Vec<String> {
    e.convert(input, 300)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

/// `zh` + 完整音节：`zhge` = zh|ge，投影键 `zg`。
#[test]
fn retroflex_initial_before_full_syllable() {
    let e = engine("before_syllable");
    let t = texts(&e, "zhge");
    assert!(t.contains(&"这个".to_string()), "zhge 应出「这个」: {t:?}");
}

/// 全是声母段、但其中一个是双字母：`zhy` = zh|y，投影键 `zy`。
///
/// 这一条是**纯简拼路径表达不了**的形态（它逐字母切，只能给出 `z|h|y`），
/// 所以混合路径那道「必须既有声母段又有音节段」的守卫要为它放行。
#[test]
fn retroflex_initial_in_all_initial_pattern() {
    let e = engine("all_initial");
    let t = texts(&e, "zhy");
    assert!(t.contains(&"这样".to_string()), "zhy 应出「这样」: {t:?}");
    // 判据是**收紧**的：同在 `zy` 键下，`zhi|you` 的首音节以 zh 开头 ⇒ 该进；
    // `zuo|yong` 不以 zh 开头 ⇒ 不该进。两条一起断言，才说明段校验真的在比 zh 而不是 z。
    assert!(
        t.contains(&"只有".to_string()),
        "zhi|you 的首音节以 zh 开头，应进: {t:?}"
    );
    assert!(
        !t.contains(&"作用".to_string()),
        "zuo|yong 不以 zh 开头，不该进: {t:?}"
    );
}

/// 全拼段在前、双字母声母在后：`baichx` = bai|ch|x，投影键 `bcx`。
#[test]
fn retroflex_initial_after_full_syllable() {
    let e = engine("after_syllable");
    let t = texts(&e, "baichx");
    assert!(
        t.contains(&"拜城县".to_string()),
        "baichx 应出「拜城县」: {t:?}"
    );
    assert!(
        !t.contains(&"不出现".to_string()),
        "baichx 不该出 bu|chu| 的词（第一段必须等于 bai）: {t:?}"
    );
}

/// 三个双字母声母都要生效，不能只接一个。
#[test]
fn all_three_retroflex_initials_work() {
    let e = engine("all_three");
    assert!(
        texts(&e, "chshi").contains(&"城市".to_string()),
        "ch + shi 应出「城市」"
    );
    assert!(
        texts(&e, "shhai").contains(&"上海".to_string()),
        "sh + hai 应出「上海」"
    );
    assert!(
        texts(&e, "zhhua").contains(&"中华".to_string()),
        "zh + hua 应出「中华」"
    );
}

/// 不回归：单字母声母的老形态照常工作，纯简拼与全拼也不受影响。
#[test]
fn single_letter_initial_and_plain_paths_unchanged() {
    let e = engine("no_regression");
    assert!(
        texts(&e, "nhao").contains(&"你好".to_string()),
        "单字母声母 + 音节（老形态）"
    );
    assert!(texts(&e, "nh").contains(&"你好".to_string()), "纯简拼");
    assert!(texts(&e, "nihao").contains(&"你好".to_string()), "全拼");
    // `zh` 本身是个合法简拼键（中华），双字母解释不能把它顶掉。
    assert!(
        texts(&e, "zh").contains(&"中华".to_string()),
        "zh 作为纯简拼键仍要命中"
    );
}

/// 两种解释**并存**：`zhy` 既该出 zh|y 的词，也不该丢掉 z|h|y 的纯简拼老结果。
///
/// 用户要的是「宽松」，不是「换一种严格」——老写法打惯了的人不能因此打不出词。
#[test]
fn both_interpretations_coexist() {
    let e = engine("coexist");
    // 「城市」cheng|shi 的两种打法都要活：`cs`（纯简拼，逐字母）与 `chs`（ch 作一段）。
    // 用户要的是「宽松」，不是「换一种严格」——老写法打惯了的人不能因此打不出词。
    assert!(
        texts(&e, "cs").contains(&"城市".to_string()),
        "纯简拼 c|s 仍命中"
    );
    assert!(
        texts(&e, "chs").contains(&"城市".to_string()),
        "ch|s 也应命中"
    );
}

/// 模糊音关闭时，双字母声母段是**收紧**的：`zh` 不匹配 `z` 开头的音节。
/// 开了 `zh_z` 才放宽 —— 与该开关的语义一致。
#[test]
fn retroflex_segment_relaxes_only_under_its_fuzzy_flag() {
    // 关：zh 段严格要求音节以 zh 开头，`zuo|yong` 进不来（见上一条测试）。
    assert!(
        !texts(&engine("fuzzy_off"), "zhy").contains(&"作用".to_string()),
        "zh_z 关闭时不该放宽到 z"
    );

    // 开：zh ↔ z 等价 ⇒ zuo|yong 也满足首段，进候选（并按模糊折扣沉在精确解之后）。
    let f = FuzzyConfig {
        zh_z: true,
        ..Default::default()
    };
    let loose = PinyinEngine::new(PyConfig::default(), dict("fuzzy_on")).with_fuzzy(f);
    assert!(
        texts(&loose, "zhy").contains(&"作用".to_string()),
        "开 zh_z 后 zh 可放宽到 z ⇒ zuo|yong 也进候选"
    );
}
