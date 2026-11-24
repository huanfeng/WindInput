//! 短输入的音节数闸门回归测试（step 6.3，上限见 `completion_syllable_cap`）。
//!
//! 输入只有 ≤1 个音节时，前缀补全不得给出更多音节的词：`d` / `dian` 不出「但是」「的时候」
//! 「电话」，对齐主流拼音输入法。
//!
//! 必须用真实词库：同码多切分的分支（`dian` 下 2 音节的「堤岸」）依赖词典的真值 `boundary`，
//! 内存词典 boundary 恒为 0、会退化成 DAG 现切，测试照样绿却测的是另一条路径。
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

/// 核心断言 ①：裸声母（`d`/`s`/`n`，不成音节）只出单字。全部候选都来自 step4 前缀补全、
/// 码必然比输入长（「的」是 `de`），所以判据只能是字数。
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

/// 核心断言 ②：完整单音节（`di`/`dian`）不出码更长的补全词。
///
/// 判据是「码更长」而不是「字数 > 1」：`dian` 下的「堤岸」(di|an) 码等于输入，是合法的 2 字
/// 候选。本闸门挡的是用户没打的音节，不是多字。
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

/// 取数配额不得被注定丢弃的词组吃光，单字必须能一直翻下去。
///
/// 若只在装配后 `retain`，`limit` 配额在取数阶段就被多音节词占满（`d` 请求 300 条只得几十条
/// 单字）。判据下推到词库层（`search_prefix_syllable_capped`）后 top-N 直接是 N 条合格条目。
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

/// 单音节输入的候选不得因此变空：过滤的是音节数超出的词，不是前缀查询本身。
/// `d` 短于 `is_abbreviation` 的 2 字母下限，全部候选（含「的」）都来自 step4 前缀补全；
/// 按来源整条关掉前缀查询会让它零候选。
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

/// 尺子必须是「输入自身的音节数」，不是「输入在候选切分下占的音节数」。
///
/// 后者会让 `xia` 因「西安」的 `xi|an` 切分被算成 2 音节、整批放行词组；`ying` 同理漏出
/// 「因果」(yin|guo)。输入的音节数是输入自己的属性，全局唯一。
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
    // 反向对照：同样这些词，在输入真的够 2 个音节时必须回来。
    //
    // 取数放大到 2000：这类候选排在全部精确匹配之后（`yin` 的同音字有 272 条），
    // 取 300 会把名次问题误判成没产出。
    //
    // 用 `yinguo` 而非 `yingu`：后者 DAG 切分为 `yin|gu`，放「因果」出来等于把已打完的音节
    // 继续拉长（与「打 `shen` 出 `sheng`」同一条通道），被音节边界对齐挡掉，见
    // `wind_dict::cached::prefix_syllable_aligned`。出厂 `min_syllables` 下整音节输入只剩精确
    // 匹配，「补全仍可达」由残码位补全 `xiah` 承载。
    for (input, want) in [("xian", "西安"), ("yinguo", "因果"), ("xiah", "下滑")] {
        let t = texts(&mgr, input, 2000);
        assert!(
            t.iter().any(|x| x == want),
            "`{input}` 已表达 2 个音节，「{want}」必须可达，实际共 {} 条",
            t.len()
        );
    }
}

/// 同码多切分不得误杀：`dian` 下 2 音节的「堤岸」(di|an) 必须保留。
///
/// 它码等于输入、不是前缀补全，不受闸门约束；若拿 `Dag::maximum_match()` 的音节数
/// （`[dian]` 恒 1）去裁所有候选，「堤岸」就会被误删。
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

/// 反向对照：多打一个字母后，被滤掉的词组必须立刻回来。缺了这条，上面的断言可以靠
/// 「把前缀补全整个废掉」来满足。`dianh` 的残码使输入音节数为 2，「电话」合法。
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

/// 简拼不受本闸门影响：它的 code 与击键不同域，音节数判据在各自源头按击键字母数施加。
/// 若按全拼域裁，`nh` 会被当成 1 音节输入，「你好」被整批误杀。
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

/// 双拼下行为同构：每 2 键 1 音节，奇数键必有残码。`ni`（小鹤 1 个完整音节）只出单字；
/// `nih`（1 音节 + 残码声母）输入音节数为 2，「你好」回来。
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
