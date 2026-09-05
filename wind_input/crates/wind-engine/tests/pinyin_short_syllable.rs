//! 短输入的「音节数匹配」闸门回归测试（step 6.3，见 `STRICT_SYLLABLE_MATCH_MAX`）。
//!
//! 语义：输入在某候选的切分下只占到 ≤1 个音节时，该候选的音节数必须与之相等——
//! 即 `d` / `dian` 不再出「但是」「的时候」「电话」这类词组，对齐主流拼音输入法。
//!
//! **必须用真实词库**：判据的核心分支（同码多切分，`dian` 下 2 音节的「堤岸」）依赖
//! 词典的真值 `boundary`，而 `CodetableDict::merge_single` 造的内存词典 boundary 恒为 0
//! （P2b 记录在案）→ 会退化成 DAG 现切、把「堤岸」猜成 1 音节，测试照样绿但**测的是
//! 另一条路径**。同款假绿模式见 `project_build_dev_data_missing`。
//!
//! 词典缺失时自动跳过。

use std::path::PathBuf;
use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("build_dev")
        .join("data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

fn manager(dir: &std::path::Path) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string(), "shuangpin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    EngineManager::new(&cfg, Some(dir))
}

fn texts(mgr: &EngineManager, input: &str, limit: usize) -> Vec<String> {
    mgr.convert_with("pinyin", input, limit)
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

/// 核心断言 ①：**裸声母**（`d`/`s`/`n`，不成音节）只出单字。
///
/// 这类输入没有任何精确匹配，全部候选都来自 step4 前缀补全、码必然比输入长
/// （单字「的」的码是 `de`），所以判据只能是**字数**。
#[test]
fn bare_initial_yields_only_single_chars() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for input in ["d", "s", "n", "zh"] {
        let multi: Vec<String> = mgr
            .convert_with("pinyin", input, 60)
            .candidates
            .into_iter()
            .filter(|c| !c.is_abbrev && c.text.chars().count() > 1)
            .map(|c| format!("{}({})", c.text, c.code))
            .collect();
        assert!(
            multi.is_empty(),
            "`{input}` 只起了 1 个音节的头，不该出现多字候选：{multi:?}"
        );
    }
}

/// 核心断言 ②：**完整单音节**（`di`/`dian`）不出码更长的补全词。
///
/// ⚠️ 这里的判据是「码更长」而**不是**「字数 > 1」。`dian` 下的「堤岸」是合法的 2 字
/// 候选 —— 它的码恰好等于输入，只是切分为 `di|an`，用户打 `dian` 本就可能要它
/// （主流输入法同样会出）。本闸门挡的是「用户没打的音节」，不是「多字」。
/// 第一版把断言写成「无多字候选」，当场被「堤岸」证伪 —— 记在此处免得被"顺手改回去"。
#[test]
fn full_syllable_yields_no_longer_code_candidates() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for input in ["di", "dian", "shi", "zhong", "hao", "xia", "ying", "xiao"] {
        let over: Vec<String> = mgr
            .convert_with("pinyin", input, 60)
            .candidates
            .into_iter()
            // 简拼候选的 code 与击键不同域（`nh` → `nihao`），不适用本判据。
            .filter(|c| !c.is_abbrev && c.code.len() > input.len())
            .map(|c| format!("{}({})", c.text, c.code))
            .collect();
        assert!(
            over.is_empty(),
            "`{input}` 只表达 1 个音节，不该出现码更长的补全候选：{over:?}"
        );
    }
}

/// **取数配额不得被注定丢弃的词组吃光** —— 单字必须能一直翻下去。
///
/// 现场：闸门刚上线时过滤只在候选装配完成后 `retain`，而 `limit` 配额在**取数阶段**
/// 就被多音节词占满了。实测 `d` 请求 300 条只得 31 条单字、请求 1000 条只得 68 条，
/// 且把上限提到 5000 也纹丝不动（`MAX_COMPLETION_CANDIDATES` clamp 在 1000）——
/// 用户翻两页就没了，正是「候选只有 30 个左右」的成因。
///
/// 修法是把判据下推到词库层（`search_prefix_with_boundary_syllable_matched`），
/// 使 top-N 直接是 N 条合格条目。**这条测试在下推之前必红**，是该修复的守卫。
#[test]
fn short_input_fills_the_requested_quota() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for input in ["d", "s", "y", "zh"] {
        for (req, floor) in [(300usize, 250usize), (1000, 800)] {
            let n = mgr.convert_with("pinyin", input, req).candidates.len();
            assert!(
                n >= floor,
                "`{input}` 请求 {req} 条应至少拿到 {floor} 条单字，实际 {n} 条 \
                 —— 配额多半又被前缀补全的词组吃掉了"
            );
        }
    }
}

/// 单音节输入的候选**不得因此变空**：过滤的是"音节数超出"的词，不是前缀查询本身。
///
/// `d` 尤其关键：它短于 `is_abbreviation` 的 2 字母下限、简拼路径不启动，全部候选
/// （含单字「的」，code=`de`）都来自 step4 前缀补全。若按来源整条关掉前缀查询而不是
/// 按音节数过滤，这里会是零候选。
#[test]
fn short_input_still_has_candidates() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for (input, want) in [("d", "的"), ("s", "是"), ("dian", "点"), ("shi", "是")] {
        let t = texts(&mgr, input, 60);
        assert!(t.len() >= 5, "`{input}` 候选过少：{t:?}");
        assert!(
            t.iter().any(|x| x == want),
            "`{input}` 应含「{want}」，实际前 10={:?}",
            &t[..10.min(t.len())]
        );
    }
}

/// **尺子必须是「输入自身的音节数」，不是「输入在候选切分下占的音节数」。**
///
/// 真机现场（用户报）：`xia` 下冒出词组。成因是判据一度按**每条候选自己的切分**算
/// 输入占了几个音节 —— 「西安」的码 `xian` 切分为 `xi|an`，第 2 个音节起点 2 落在
/// `[0,3)` 内 ⇒ 算出 2 音节 ⇒ 整批词组放行。`ying` 同理漏出 `yin|guo`（因果）。
///
/// 输入的音节数是**输入自己的属性**、全局唯一（`xia` = `[xia]` 一个音节），
/// 不该外包给候选。这条测试对着两个原始现场，回退即红。
#[test]
fn syllable_count_comes_from_input_not_candidate_segmentation() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for (input, ghost) in [("xia", "西安"), ("ying", "因果"), ("xia", "西岸")] {
        let t = texts(&mgr, input, 300);
        assert!(
            !t.iter().any(|x| x == ghost),
            "`{input}` 是 1 个音节，不该因某候选的 `xi|an` / `yin|guo` 切分而放行「{ghost}」"
        );
    }
    // 反向对照：同样这些词，在**输入真的够 2 个音节**时必须回来。
    //
    // ⚠️ 取数放大到 2000：这类候选按层级排在全部精确匹配之后，而 `yin` 的同音字有 272 条
    // 挡在前面。取 300 会误判成「没产出」，那是**名次问题不是产出问题**，本条只钉产出。
    //
    // ⚠️ **2026-09-04：`yingu` 换成 `yinguo`**（音节边界对齐上线）。`yingu` 的 DAG 切分是
    // `yin|gu`，而 `gu` 本身是合法音节 —— 放「因果」(`yin|guo`) 出来就等于允许把已打完的
    // 音节继续拉长，那正是「打 `shen` 出 `sheng` 的字」的同一条通道，两者在音节结构上完全
    // 同形、不可能只挡一个（主流拼音输入法取的也都是严格档）。见
    // `wind_dict::cached::prefix_syllable_aligned` 与
    // `wind-coordinator/tests/pinyin_syllable_alignment.rs`。
    //
    // ★ 由此还有一条推论，解释了这里为什么剩下的两个样本一个是精确、一个是残码：
    //   出厂 `min_syllables = 4` 下，**整音节**输入（started < 4）不再存在跨音节的前缀补全
    //   —— cap 把候选音节数压到 ≤ started，对齐判据又要求候选在 `completed_len` 处开新音节，
    //   两者合起来只剩「候选码 == 输入」即精确匹配。「补全仍可达」这一半由 `xiah`（残码位
    //   补全，判据位落在残码之前）承载。
    for (input, want) in [("xian", "西安"), ("yinguo", "因果"), ("xiah", "下滑")] {
        let t = texts(&mgr, input, 2000);
        assert!(
            t.iter().any(|x| x == want),
            "`{input}` 已表达 2 个音节，「{want}」必须可达，实际共 {} 条",
            t.len()
        );
    }
}

/// **同码多切分不得误杀**：`dian` 下 2 音节的「堤岸」(di|an) 必须保留。
///
/// 这是本闸门最容易写错的地方。若拿 `syllables.len()`（`Dag::maximum_match()` 的最长
/// 匹配 ⇒ `[dian]` 恒 1 音节）当「输入的音节数」，「堤岸」会被判成「超出输入」而删掉。
/// 正确判据按**候选自己的切分**数：「堤岸」两个音节起点 0 与 2 都落在输入范围内
/// ⇒ used=2 ⇒ 压根不进严格档。
///
#[test]
fn same_code_multi_segmentation_survives() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    let t = texts(&mgr, "dian", 300);
    assert!(
        t.iter().any(|x| x == "堤岸"),
        "`dian` 下同码 2 音节切分「堤岸」(di|an) 必须保留"
    );
    assert!(
        !t.iter().any(|x| x == "电话"),
        "`dian` 下码更长的「电话」(dian|hua) 仍须滤掉"
    );
}

/// **反向对照**：多打一个字母后，被滤掉的词组必须立刻回来。
///
/// 缺了这条，上面的断言可以靠「把前缀补全整个废掉」来满足 —— 那是更严重的回归。
/// `dianh` 的残码 `h` 使 `hua` 的起始位落进输入范围 ⇒ used=2 ⇒ 不进严格档。
#[test]
fn one_more_letter_restores_word_candidates() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for (input, want) in [
        ("dianh", "电话"),
        ("nih", "你好"),
        ("meiy", "没有"),
        ("zhongg", "中国"),
    ] {
        let t = texts(&mgr, input, 12);
        assert!(
            t.iter().any(|x| x == want),
            "`{input}` 应仍出「{want}」，实际={t:?}"
        );
    }
}

/// 简拼不受本闸门影响：它的 code 与击键不同域，走各自源头的「音节数 == 字母数」判据。
///
/// `nh` 的 used 若按全拼域算是 1（`nihao` 只有起始位 0 落在 `[0,2)` 内），
/// total=2 ⇒ 会被整批误杀。`is_abbrev` 的跳过分支正是为此。
#[test]
fn abbrev_candidates_are_exempt() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    let t = texts(&mgr, "nh", 30);
    assert!(
        t.iter().any(|x| x == "你好"),
        "简拼 `nh` 应出「你好」，实际={t:?}"
    );
}

/// 双拼下行为同构：每 2 键 1 音节，奇数键必有残码 ⇒ used 自动 +1。
///
/// `ni`（小鹤 2 键 = 1 个完整音节 `ni`）进严格档只出单字；
/// `nih`（3 键 = 1 音节 + 残码声母）used=2 ⇒ 放开，「你好」回来。
#[test]
fn shuangpin_parity_matches_full_pinyin() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    let over: Vec<String> = mgr
        .convert_with("shuangpin", "ni", 60)
        .candidates
        .into_iter()
        .filter(|c| !c.is_abbrev && c.code.len() > "ni".len())
        .map(|c| format!("{}({})", c.text, c.code))
        .collect();
    assert!(
        over.is_empty(),
        "双拼 `ni`（1 个完整音节）不该出码更长的补全候选：{over:?}"
    );
}
