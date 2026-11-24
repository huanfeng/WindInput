//! 残码前缀补全的上浮约束回归测试
//!
//! 残码存在时（`meiy` 的 `y`），前缀补全候选不标 `is_prefix`、上浮到精确子串单字之前，
//! 否则「没有」会被数百个单字压到十几页之后。但上浮须按「补全距离 + 置信度」约束
//! （`COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES` / `COMPLETION_FAR_WEIGHT_FLOOR`），否则双拼
//! 奇数键必有残码，候选 2~5 位会被冷僻长词占满并逐键跳动。依据见
//! docs/architecture/candidate-sorting-rules.md §4.2。
//!
//! 距离不能单独作判据：`zhongguorenm`→「中国人民解放军」距离 +4 却合理，同为 +4 的
//! 「…物权法」是噪音，判别力在 weight。
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
    // 必须显式设回 2 / 3，不能吃出厂默认（4 / 5）：本文件测上浮与降级，前提是候选已被召回，
    // 而出厂 `min_syllables` 会让 2~3 音节样本在召回层就空掉，断言以「候选缺失」失败、
    // 被测逻辑一次都没执行。召回门槛的出厂行为由 `pinyin_completion_recall_gate` 守。
    cfg.schema.pinyin.completion.min_syllables = 2;
    cfg.schema.pinyin.completion.max_extra_syllables = 3;
    EngineManager::new(&cfg, Some(dir))
}

fn rank_of(mgr: &EngineManager, schema: &str, input: &str, text: &str) -> Option<usize> {
    mgr.convert_with(schema, input, 12)
        .candidates
        .iter()
        .position(|c| c.text == text)
}

/// 残码补全必须仍能上浮：这批是该机制存在的理由，全部须留在前列。
/// 含近距离（+1/+2）与远距离但高频（+4/+5）两类。
#[test]
fn test_useful_completions_still_float() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for (input, want, note) in [
        ("meiy", "没有", "距离+1 w=339165"),
        ("nih", "你好", "距离+1 w=5328"),
        ("nihaom", "你好吗", "距离+1 w=166，低词频但近距离须豁免"),
        ("zhongguor", "中国人", "距离+1 w=21385"),
        // 这两条在距离 2，走 `COMPLETION_FAR_WEIGHT_FLOOR`(100) 放行；收紧无条件上浮距离
        // 挡掉的是 w=18 的「中华人民」，见 `low_freq_far_completion_does_not_outrank_sentence`。
        ("beijingd", "北京大学", "距离+2 w=2010，靠 FLOOR 放行"),
        ("jisuanjik", "计算机科学", "距离+2 w=1609，靠 FLOOR 放行"),
        // `zhonghuar`→「中华人民共和国」不在此列：音节 extra=4 超出 `max_extra_syllables`
        // 出厂值 3，默认不产出（与 `nih`→「你会怎么做」同形，weight 分不开，只能靠旋钮取舍）。
        // 调到 4 即恢复，由 `far_completion_returns_when_max_extra_raised` 守。
        (
            "zhongguorenm",
            "中国人民解放军",
            "距离+4 w=252，extra=3 恰好卡在出厂值上，是本档的下边界",
        ),
        ("zhonghuarenmingongheg", "中华人民共和国", "距离+1 w=3113"),
    ] {
        let rank = rank_of(&mgr, "pinyin", input, want);
        assert!(
            rank.is_some_and(|r| r < 6),
            "「{}」({}) 应仍在 {} 的前列，实际位置 {:?}",
            want,
            note,
            input,
            rank
        );
    }
}

/// 两个旋钮确实在起作用：`max_extra_syllables` 调大即恢复远距离补全，调小即收紧。
/// 以 `zhonghuar`→「中华人民共和国」（extra=4）为标尺；缺了这条，过滤被写死、旋钮变摆设也不会红。
#[test]
fn far_completion_returns_when_max_extra_raised() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };

    let has = |max_extra: u32| {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["pinyin".to_string()];
        cfg.schema.active = "pinyin".to_string();
        // `zhonghuar` 的 started = 3，出厂 `min_syllables` 会先把上限收紧到 started，
        // `max_extra` 不参与运算；要单独观察 `max_extra` 须把 min 降到不挡路。
        cfg.schema.pinyin.completion.min_syllables = 2;
        cfg.schema.pinyin.completion.max_extra_syllables = max_extra;
        EngineManager::new(&cfg, Some(&dir))
            .convert_with("pinyin", "zhonghuar", 300)
            .candidates
            .iter()
            .any(|c| c.text == "中华人民共和国")
    };

    assert!(!has(3), "max_extra=3 下 extra=4 的补全不该产出");
    assert!(has(4), "调到 4 后「中华人民共和国」必须回来");
}

/// 冷僻长词补全须沉底：它们是奇偶跳动的噪音源。
/// 「沉底」指排到精确匹配之后，不是从候选中消失。
#[test]
fn test_far_lowfreq_completions_are_demoted() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    let noise = [
        "中华人民共和国企业所得税",
        "中华人民共和国治安管理处罚法",
        "中华人民共和国道路交通安全法",
        "中华人民共和国物权法",
    ];
    let cands = mgr
        .convert_with("pinyin", "zhonghuarenmingongheg", 12)
        .candidates;
    for (i, c) in cands.iter().enumerate().take(6) {
        assert!(
            !noise.contains(&c.text.as_str()),
            "冷僻条文名「{}」不该出现在前 6 位（第 {} 位）",
            c.text,
            i + 1
        );
    }
}

/// 补全折扣：同一上浮层内，未输入音节更多的候选须让位于更短的候选。
///
/// 上浮层内若只比裸词频，`nih` 下「你会发现」(w=13330, extra=3) 会压过「你好」(w=5328, extra=1)；
/// `COMPLETION_WEIGHT_DISCOUNT`（0.5^extra）折后 你好 2664 > 你会发现 1666。
///
/// 断言用相对位置：首选「你会」(w=22262) 赢「你好」是词库词频使然，不在本机制职责内。
#[test]
fn completion_discount_demotes_words_with_more_unentered_syllables() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    let cands = mgr.convert_with("pinyin", "nih", 12).candidates;
    let pos = |t: &str| cands.iter().position(|c| c.text == t);
    let texts = || cands.iter().map(|c| &c.text).collect::<Vec<_>>();

    let nihao = pos("你好").expect("「你好」应在 nih 的候选中");
    let faxian = pos("你会发现").expect("「你会发现」应仍在候选中（沉底而非消失）");
    assert!(
        nihao < faxian,
        "「你好」(extra=1) 须在「你会发现」(extra=3) 之前，实际: {:?}",
        texts()
    );

    // 4 音节噪音整体不该占据首屏前三。
    for (i, c) in cands.iter().enumerate().take(3) {
        assert!(
            c.text.chars().count() <= 2,
            "nih 首屏前三不该出现 {} 字词「{}」（第 {} 位），实际: {:?}",
            c.text.chars().count(),
            c.text,
            i + 1,
            texts()
        );
    }
}

/// step 6.5b：整句须让位于「恰好用完残码的补全」，且只让给它。
///
/// 判据复刻 librime `has_exact_match_phrase`：补全词音节数 == 已完成音节数 + 1。
/// 反例同等重要：`beijingdaxuex` 的「北京大学校长」6 音节 ≠ 4+1、w=4，不该触发让位；
/// 缺了这半边，把判据放宽成「extra ≤ 2」也照样绿。
#[test]
fn sentence_yields_to_completion_that_exhausts_trailing_partial() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    let top = |input: &str| -> String {
        mgr.convert_with("pinyin", input, 6).candidates[0]
            .text
            .clone()
    };

    // 正例：补全恰好用完残码（音节数 == completed + 1）⇒ 整句让位。只剩 Viterbi 选出的字
    // 与词库补全不一致的情形（`zhongguorenm` 整句「中国人吗」vs 补全「中国人民」）；
    // 残码整句恰好就是正解的输入由 `test_trailing_partial_completes_into_sentence` 覆盖。
    for (input, want, note) in [
        ("zhongguorenm", "中国人民", "4 == 3+1，整句「中国人吗」让位"),
        (
            "zhonghuarenmingongheg",
            "中华人民共和国",
            "7 == 6+1，整句「中华人民共和」让位",
        ),
    ] {
        assert_eq!(top(input), want, "{input}：{note}");
    }

    // 反例：补全音节数 > completed + 1 ⇒ 不得触发让位。「北京大学校长」6 ≠ 4+1，w=4。
    // 只断言「校长没夺走首位」：残码 `x` 会被 step 2c 补成整句，首选不一定是「北京大学」。
    assert_ne!(
        top("beijingdaxuex"),
        "北京大学校长",
        "beijingdaxuex：「北京大学校长」6 音节 ≠ 4+1，不该夺走首位"
    );

    // 无残码时本机制整个不启动（trailing_partial=false），整句照常居首。
    assert_eq!(top("nihao"), "你好", "无残码：整句不受影响");
    assert_eq!(top("nihaoma"), "你好吗", "无残码：整句不受影响");
}

/// 距离 ≥2 的补全必须过 `COMPLETION_FAR_WEIGHT_FLOOR`：低频长词不得靠近距离豁免登顶。
///
/// `zhonghuar` 下 w=18 的「中华人民」不得压过整句，否则逐字符输入时首选长度来回跳动。
/// 同时守着收紧没误伤高频远距离补全：`beijingd`→北京大学(w=2010)、`jisuanjik`→计算机科学
/// (w=1609) 走 FLOOR 那条路，仍须留在首屏。
#[test]
fn low_freq_far_completion_does_not_outrank_sentence() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    let cands = mgr.convert_with("pinyin", "zhonghuar", 6).candidates;
    let texts = || cands.iter().map(|c| &c.text).collect::<Vec<_>>();
    // 首选必须是某条整句，但不断言是哪一条：「中华人」(消费 9 键) 与「中华」(消费 8 键)
    // 的先后由协调器比较链 ⓪ `consumed_length` 决定，引擎级排序不看 consumed。
    // 本用例要守的是下面那条：w=0 的「种花人」与 w=18 的「中华人民」都不得靠上浮夺走首位。
    assert!(
        cands[0].is_sentence,
        "zhonghuar 首选应是整句（「中华人」或「中华」皆可），实际: {:?}",
        texts()
    );
    assert!(
        !cands.iter().take(3).any(|c| c.text == "中华人民"),
        "「中华人民」(w=18, 距离 2) 未过 FLOOR，不该进前 3，实际: {:?}",
        texts()
    );

    // 反向对照：高频远距离补全不得被这次收紧误伤。
    for (input, want) in [("beijingd", "北京大学"), ("jisuanjik", "计算机科学")] {
        let rank = rank_of(&mgr, "pinyin", input, want);
        assert!(
            rank.is_some_and(|r| r < 6),
            "{input}→「{want}」词频远高于 FLOOR，收紧后仍须在首屏，实际位置 {rank:?}"
        );
    }
}

/// step 6.5b 的置信度门槛：冷僻补全不得把整句顶掉，整句权重更不得被压成负数。
///
/// `zhonghuar` 下「种花人」音节数 3 == completed 2 + 1，满足 6.5b 的音节判据，但 w=0。
/// 让位是二值开关，没有 `SENTENCE_YIELD_WEIGHT_FLOOR` 时首选会变成「种花人」、整句被压到负权重。
#[test]
fn sentence_does_not_yield_to_low_confidence_completion() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    let cands = mgr.convert_with("pinyin", "zhonghuar", 6).candidates;
    let sentence = cands
        .iter()
        .find(|c| c.text == "中华")
        .expect("整句「中华」须在候选中");
    assert!(
        !sentence.is_sentence_demoted,
        "「种花人」(w=0) 不该触发整句让位，实际候选: {:?}",
        cands
            .iter()
            .map(|c| (&c.text, c.weight))
            .collect::<Vec<_>>()
    );
    assert!(
        sentence.weight > 0,
        "整句权重不得被降成 0/负数，实际 {}",
        sentence.weight
    );
}

/// 双拼奇偶键的候选形态须稳定：奇数键（残码）与相邻偶数键（完整音节）
/// 的前若干候选不应出现整批替换。这是用户报告的原始现象。
#[test]
fn test_shuangpin_parity_does_not_thrash() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    if !mgr.ensure_schema("shuangpin") {
        eprintln!("跳过：缺 shuangpin 方案");
        return;
    }

    // vshxrfmbgshego = zhong hua ren min gong he guo
    let odd = mgr.convert_with("shuangpin", "vshxrfmbgsheg", 6).candidates;
    let noise_in_odd = odd
        .iter()
        .take(5)
        .filter(|c| c.text.starts_with("中华人民共和国") && c.text.chars().count() > 7)
        .count();
    assert_eq!(
        noise_in_odd,
        0,
        "奇数键前 5 位不应被超长条文名占据，实际: {:?}",
        odd.iter().take(5).map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// step 2c：尾部残码作为「待定音节」入图，由 Viterbi 补出最优单字。
/// `buzhidaok` 应得「不知道看」；没有这一步时该候选根本不存在，排序救不了。
#[test]
fn test_trailing_partial_completes_into_sentence() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    // 后三条靠 step 2c 直接产出。「人」「吗」不在虚词表、被取代的「让」「们」在：
    // 同时守着 `score_node_partial_final`（残码位不给虚词优待），去掉它即变红。
    for (input, want) in [
        ("buzhidaok", "不知道看"),
        ("jisuanjik", "计算机看"),
        ("nihaom", "你好吗"),
        ("zhongguor", "中国人"),
        ("zhonghuar", "中华人"),
    ] {
        let cands = mgr.convert_with("pinyin", input, 300).candidates;
        let hit = cands.iter().find(|c| c.text == want).unwrap_or_else(|| {
            panic!(
                "{input} 应产出残码补全整句「{want}」，实际前 8: {:?}",
                cands.iter().take(8).map(|c| &c.text).collect::<Vec<_>>()
            )
        });
        assert!(hit.is_sentence, "「{want}」须带整句身份");
        assert_eq!(
            hit.consumed_length,
            input.len(),
            "残码整句必须解释**全部**输入（这正是它区别于 step 2 结果之处）"
        );
    }
}

/// 残码补全不得把已完成的音节重新切开。
///
/// 锁住 step 2c 与 `add_abbrev_nodes` 的分工：两者代码形状相近，但简拼节点会按声母重切，
/// 让残码走简拼会得到「不直达欧卡」「你黑暗欧美」。两条路径若被合并，本测试即红。
#[test]
fn test_partial_completion_preserves_completed_syllables() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for (input, prefix) in [
        ("buzhidaok", "不知道"),
        ("nihaom", "你好"),
        ("qingfengs", "清风"),
    ] {
        let cands = mgr.convert_with("pinyin", input, 300).candidates;
        let sentences: Vec<_> = cands
            .iter()
            .filter(|c| c.is_sentence && c.consumed_length == input.len())
            .collect();
        // 前置：没有这一句，step 2c 一旦被整体关掉本测试就**真空假绿**（「不该出现 X」型
        // 断言在 X 一个都不产生时恒真）。实测有效性时正是这条露的馅。
        assert!(
            !sentences.is_empty(),
            "前置：{input} 应产出至少一条残码整句，否则本用例退化成空断言"
        );
        for c in sentences {
            assert!(
                c.text.starts_with(prefix),
                "{input} 的残码整句「{}」没有保住已完成音节「{prefix}」——\
                 已完成部分被重新切分了（简拼通道的特征）",
                c.text
            );
        }
    }
}

/// step 2c 的门槛：`syllables.len() >= 2`。1 音节 + 残码（`nim`）的正解是词库补全
/// （你们/你没），残码整句「你吗」只会挤掉它；同 fcitx5 `partialLongWordLimit`。
#[test]
fn test_partial_completion_skips_single_syllable_input() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    let cands = mgr.convert_with("pinyin", "nim", 300).candidates;
    let sentences: Vec<_> = cands
        .iter()
        .filter(|c| c.is_sentence && c.consumed_length == 3)
        .map(|c| &c.text)
        .collect();
    assert!(
        sentences.is_empty(),
        "1 音节 + 残码不该走残码整句，实际产出: {sentences:?}"
    );
}

/// 残码整句必须产出、带整句身份、且消费整串。
///
/// `buzhidaok`/`zhonghuar` 走新建候选，`nihaom` 走同文合并（残码整句「你好吗」等于已有的
/// 前缀补全）；合并分支漏标 `is_sentence` 不会编译报错，只会让它退化成普通补全。
/// `consumed_length == 输入长度` 是它区别于 step 2 整句的关键，协调器比较链 ⓪ 靠它排序。
#[test]
fn partial_sentence_is_marked_sentence_and_consumes_all() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for (input, want) in [
        ("buzhidaok", "不知道看"),
        ("zhonghuar", "中华人"),
        ("nihaom", "你好吗"),
    ] {
        let cands = mgr.convert_with("pinyin", input, 300).candidates;
        let hit = cands
            .iter()
            .find(|c| c.text == want)
            .unwrap_or_else(|| panic!("{input} 应产出残码整句「{want}」"));
        assert!(hit.is_sentence, "「{want}」须带整句身份");
        assert_eq!(
            hit.consumed_length,
            input.len(),
            "「{want}」须消费整串（含残码），否则协调器 ⓪ 会把它排到 step 2 整句之后"
        );
    }
}

/// `weight <= 0` 的词条不得获得「距离 1 无条件上浮」的特权。
///
/// w≤0 是词库对存疑条目的标记（`lattice.rs::score_node` 对它罚分）；否则 `zhonghuar` 的
/// 「种花人」(w=0、距离 1) 会排到第 2。librime 用 `log(w > 0 ? w : DBL_EPSILON)` 结构上避免了这点。
#[test]
fn zero_weight_completion_does_not_get_unconditional_float() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    let cands = mgr.convert_with("pinyin", "zhonghuar", 300).candidates;
    let pos = |t: &str| cands.iter().position(|c| c.text == t);
    let head = || {
        cands
            .iter()
            .take(6)
            .map(|c| format!("{}(w={})", c.text, c.weight))
            .collect::<Vec<_>>()
    };

    let zhonghuaren = pos("中华人民").expect("「中华人民」应在候选中");
    let zhonghuaren0 = pos("种花人").expect("「种花人」应仍在候选中（降级≠销毁）");
    assert!(
        zhonghuaren < zhonghuaren0,
        "w=18 的「中华人民」须排在 w=0 的「种花人」之前，实际: {:?}",
        head()
    );
}
