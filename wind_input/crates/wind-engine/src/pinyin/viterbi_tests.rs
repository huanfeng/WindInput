use super::*;

/// 锁定 nodes[end_pos] 的索引约定：输入 "nihao"（5 字节）的节点在 char_end=5 结束，
/// decode 若误读 nodes[end_pos-1] 就取不到它，words 为空。
#[test]
fn test_decode_index_convention() {
    let input_len = 5usize; // "nihao"
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    // 单个双字词 "你好" 覆盖 [0,5]，结束于位置 5
    nodes[5].push(WordNode {
        start: 0,
        end: 5,
        word: "你好".to_string(),
        syl_mask: 0b101, // ni|hao
        log_prob: 10.0,
        canon: None,
        aux: 0,
    });
    let decoder = ViterbiDecoder::new();
    let result = decoder.decode(&nodes, input_len);
    assert_eq!(result.words, vec!["你好".to_string()], "应解码出 你好");
    assert!(result.log_prob.is_finite());
    assert_eq!(result.boundary, 0b101, "整句边界应回填节点自身的切分");
}

/// 两段路径：ni(0..2) + hao(2..5)，供多个用例复用。
fn two_segment_lattice() -> (Vec<Vec<WordNode>>, usize) {
    let input_len = 5usize;
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    nodes[2].push(WordNode {
        start: 0,
        end: 2,
        word: "你".to_string(),
        syl_mask: 0b1,
        log_prob: 3.0,
        canon: None,
        aux: 0,
    });
    nodes[5].push(WordNode {
        start: 2,
        end: 5,
        word: "好".to_string(),
        syl_mask: 0b1,
        log_prob: 3.0,
        canon: None,
        aux: 0,
    });
    (nodes, input_len)
}

/// 两段路径：验证多节点拼接。
#[test]
fn test_decode_two_segments() {
    let (nodes, input_len) = two_segment_lattice();
    let decoder = ViterbiDecoder::new();
    let result = decoder.decode(&nodes, input_len);
    assert_eq!(result.words, vec!["你".to_string(), "好".to_string()]);
    // 两个单音节节点分别起于 0 与 2 → {0,2}
    assert_eq!(result.boundary, 0b101);
}

fn node(start: usize, end: usize, word: &str, log_prob: f64) -> WordNode {
    WordNode {
        start,
        end,
        word: word.to_string(),
        syl_mask: 0b1,
        log_prob,
        canon: None,
        aux: 0,
    }
}

/// beam 的存在意义：让「前缀较差、但搭配更好」的路径能够胜出。
///
/// 单状态 DP 下 `dp[2]` 只留分高的「甲」，此后无论上下文分怎么打都选不出「乙丙」。
/// 选出「乙丙」还要求回溯（按 `(prev_pos, prev_word)` 找前驱）准确落到非最优的前缀线上。
#[test]
fn beam_lets_a_weaker_prefix_win_on_context() {
    let input_len = 4usize;
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    nodes[2].push(node(0, 2, "甲", 4.0));
    nodes[2].push(node(0, 2, "乙", 3.0)); // 前缀劣 1.0
    nodes[4].push(node(2, 4, "丙", 1.0));

    let plain = ViterbiDecoder::new().decode(&nodes, input_len);
    assert_eq!(plain.words, vec!["甲".to_string(), "丙".to_string()]);

    struct PreferYiBing;
    impl Grammar for PreferYiBing {
        fn query(&self, context: &str, word: &str, _is_rear: bool) -> f64 {
            if context == "乙" && word == "丙" {
                5.0
            } else {
                0.0
            }
        }
    }
    let ctx = ViterbiDecoder::with_grammar(Arc::new(PreferYiBing)).decode(&nodes, input_len);
    assert_eq!(
        ctx.words,
        vec!["乙".to_string(), "丙".to_string()],
        "beam 应保留次优前缀，让上下文分翻盘"
    );
    // 两个节点分别起于 0 与 2，回溯若走错线，boundary 也会跟着错。
    assert_eq!(ctx.boundary, 0b101);
}

/// 每个位置的保留线数不超过 [`BEAM_WIDTH`]。
///
/// 用 grammar 的调用次数间接观测：位置 2 上放 `BEAM_WIDTH + 3` 个不同末词，
/// 扩展 `2..4` 那条边时只会从保留下来的那几条各调一次。
#[test]
fn beam_width_caps_expansion() {
    let input_len = 4usize;
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    for i in 0..(BEAM_WIDTH + 3) {
        nodes[2].push(node(0, 2, &format!("w{i}"), i as f64));
    }
    nodes[4].push(node(2, 4, "尾", 0.0));

    let rec = Arc::new(Recorder {
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let r = ViterbiDecoder::with_grammar(rec.clone()).decode(&nodes, input_len);

    let calls = rec.calls.lock().unwrap().clone();
    let tail_calls = calls.iter().filter(|c| c.1 == "尾").count();
    assert_eq!(tail_calls, BEAM_WIDTH, "只应从保留的 {BEAM_WIDTH} 条线扩展");
    // 截断按分数降序，最高分那条必须留下
    assert_eq!(
        r.words,
        vec![format!("w{}", BEAM_WIDTH + 2), "尾".to_string()]
    );
}

/// 同分时保留先到的一条（与单路径 DP「严格大于才覆盖」一致）；破掉会让无模型时的整句结果漂移。
#[test]
fn equal_scores_keep_the_earliest() {
    let input_len = 2usize;
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    nodes[2].push(node(0, 2, "先", 1.0));
    nodes[2].push(node(0, 2, "后", 1.0));
    let r = ViterbiDecoder::new().decode(&nodes, input_len);
    assert_eq!(r.words, vec!["先".to_string()]);
}

/// K=1 的 K-best 必须与单路径 DP 逐位相同，这是 N-best 零回归的依据。
///
/// 同分那条尤其要看住：`push_topk` 取「第一个严格小于」的位置插入；写成 `<=` 会让后到的
/// 同分项顶掉先到的。
#[test]
fn k_of_one_matches_single_path_dp() {
    let input_len = 4usize;
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    nodes[2].push(node(0, 2, "先", 1.0));
    nodes[2].push(node(0, 2, "后", 1.0)); // 同分，须保留「先」
    nodes[4].push(node(2, 4, "来", 1.0));
    nodes[4].push(node(0, 4, "先来", 1.5)); // 整词分更高，但两段合计 2.0 更优

    let single = ViterbiDecoder::decode_dp(&nodes, input_len);
    let nbest = ViterbiDecoder::decode_dp_nbest(&nodes, input_len, 1);
    assert_eq!(nbest.len(), 1);
    assert_eq!(nbest[0].words, single.words);
    assert_eq!(nbest[0].log_prob, single.log_prob);
    assert_eq!(nbest[0].boundary, single.boundary);
    assert_eq!(single.words, vec!["先".to_string(), "来".to_string()]);
}

/// K>1 时次优解要出得来，且按分数降序。
///
/// 次优整句的典型形态是「某一段换了个词」，所以转移时必须对前驱的每一条都试；只接前驱最优线，
/// K 条里就看不到真正的第二解。
#[test]
fn nbest_returns_ranked_alternatives() {
    let input_len = 4usize;
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    // 前段两个互斥的词，分数不同
    nodes[2].push(node(0, 2, "盖", 2.0));
    nodes[2].push(node(0, 2, "概", 1.0));
    // 后段唯一
    nodes[4].push(node(2, 4, "伦", 1.0));

    let r = ViterbiDecoder::decode_dp_nbest(&nodes, input_len, 3);
    assert_eq!(r.len(), 2, "只有两条完整路径");
    assert_eq!(r[0].words, vec!["盖".to_string(), "伦".to_string()]);
    assert_eq!(r[1].words, vec!["概".to_string(), "伦".to_string()]);
    assert!(
        r[0].log_prob > r[1].log_prob,
        "必须按分数降序: {:?}",
        r.iter().map(|x| x.log_prob).collect::<Vec<_>>()
    );
}

/// 每条解的 `boundary` 要跟着自己那条路径走，不能都取最优解的。
///
/// 下游拿它回填 `Candidate::boundary`，双拼校验与自动造词都依赖它；串了就是「选了第 2 条整句，
/// 学进词库的却是第 1 条的切分」。
#[test]
fn each_alternative_carries_its_own_boundary() {
    let input_len = 4usize;
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    // 路径甲：两段各 2 字节 ⇒ 位 0、2
    nodes[2].push(node(0, 2, "甲", 2.0));
    nodes[4].push(node(2, 4, "乙", 2.0));
    // 路径乙：整段一个词、单音节 ⇒ 只有位 0
    nodes[4].push(node(0, 4, "丙", 3.0));

    let r = ViterbiDecoder::decode_dp_nbest(&nodes, input_len, 3);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].words, vec!["甲".to_string(), "乙".to_string()]);
    assert_eq!(r[0].boundary, 0b101, "两段: 位 0 与位 2");
    assert_eq!(r[1].words, vec!["丙".to_string()]);
    assert_eq!(r[1].boundary, 0b1, "一段: 只有位 0");
}

/// 整串走不通的解，分数必须是 `-inf`，哪怕 `words` 有内容。
///
/// 这承担着压制部分解的功能：放出真实分数后，只解释了前半截的解会以正常权重和完整解竞争
/// （如 `bzdhaobuhao` 下「不知道哈」压过词频高得多的「不知道」）。
#[test]
fn partial_reach_paths_report_neg_infinity() {
    let input_len = 6usize;
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    // 只能走到位置 4，末尾两字节无人认领。
    nodes[2].push(node(0, 2, "甲", 2.0));
    nodes[4].push(node(2, 4, "乙", 2.0));

    let r = ViterbiDecoder::decode_dp_nbest(&nodes, input_len, 3);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].words, vec!["甲".to_string(), "乙".to_string()]);
    assert_eq!(
        r[0].log_prob,
        f64::NEG_INFINITY,
        "部分解不得带正常分数出去竞争"
    );
    // 单路径版本同样如此，两侧必须一致。
    assert_eq!(
        ViterbiDecoder::decode_dp(&nodes, input_len).log_prob,
        f64::NEG_INFINITY
    );
}

/// 记录每次 `query` 的入参，用来验证 context / is_rear 是怎么构造的。
struct Recorder {
    calls: std::sync::Mutex<Vec<(String, String, bool)>>,
}

impl Grammar for Recorder {
    fn query(&self, context: &str, word: &str, is_rear: bool) -> f64 {
        self.calls
            .lock()
            .unwrap()
            .push((context.to_string(), word.to_string(), is_rear));
        0.0
    }
}

/// 挂恒 0 的 `NullGrammar` 与完全不挂，结果必须逐位相同。
///
/// 守的是 `grammar.rs` 里「为什么 `NullGrammar` 必须返回 0.0」：若返回 librime 那样的
/// `kPenalty`，整句分会多出 `词数 × 常数`，词数不同的路径之间的比较随之改变。
/// 挂上模型后 `log_prob` 含转移分，`lexical_log_prob` 仍只是节点分之和（混合整句的每字分闸门读它）。
#[test]
fn lexical_log_prob_excludes_grammar_scores() {
    struct Penalize;
    impl Grammar for Penalize {
        fn query(&self, _context: &str, _word: &str, _is_rear: bool) -> f64 {
            -5.0
        }
    }
    let input_len = 4;
    let mut nodes = vec![Vec::new(); input_len + 1];
    nodes[2].push(node(0, 2, "你", -3.0));
    nodes[4].push(node(2, 4, "好", -2.0));
    let plain = ViterbiDecoder::new().decode(&nodes, input_len);
    assert_eq!(plain.lexical_log_prob, plain.log_prob);
    let with = ViterbiDecoder::with_grammar(Arc::new(Penalize)).decode(&nodes, input_len);
    assert_eq!(with.words, ["你", "好"]);
    assert_eq!(with.lexical_log_prob, -5.0);
    assert_eq!(with.log_prob, -15.0, "两次转移各 −5");
}

#[test]
fn null_grammar_does_not_change_result() {
    let (nodes, input_len) = two_segment_lattice();
    let plain = ViterbiDecoder::new().decode(&nodes, input_len);
    let with_null = ViterbiDecoder::with_grammar(Arc::new(crate::pinyin::grammar::NullGrammar))
        .decode(&nodes, input_len);
    assert_eq!(plain.words, with_null.words);
    // 逐位相等而非近似：加 0.0 对任何有限值都不改变位模式。
    assert_eq!(plain.log_prob, with_null.log_prob);
    assert_eq!(plain.boundary, with_null.boundary);
}

/// context 按「回看两个词」构造，`is_rear` 只在落到整句末尾时为真。
/// 与上一条互补：上一条证明不接模型时没坏，这条证明接上去的通路是通的。
#[test]
fn grammar_receives_context_and_rear_flag() {
    let (nodes, input_len) = two_segment_lattice();
    let rec = Arc::new(Recorder {
        calls: std::sync::Mutex::new(Vec::new()),
    });
    ViterbiDecoder::with_grammar(rec.clone()).decode(&nodes, input_len);

    let calls = rec.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "两条边各打一次分");
    // 按 end_pos 升序遍历：先「你」(end=2) 后「好」(end=5)。
    assert_eq!(
        calls[0],
        (String::new(), "你".to_string(), false),
        "句首无上文，且不在末尾"
    );
    assert_eq!(
        calls[1],
        ("你".to_string(), "好".to_string(), true),
        "上文是前一个词，且落在整句末尾"
    );
}

/// 规范码随路径透传：每条解拼的是自己那条路径上的节点，DP（含 K-best）与 beam 同形；
/// 全精确的解 `pieces` 为 `None`、`learn_code` 为 `None`。
///
/// 输入 `zongguo`（z=zh 打法）：路径甲 = 「中」(zong→zhong, 模糊) + 「国」(guo, 精确)，
/// 路径乙 = 「宗国」整段精确。
#[test]
fn canon_pieces_follow_each_path() {
    let input = "zongguo";
    let input_len = input.len();
    let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
    let mut zhong = node(0, 4, "中", 3.0);
    zhong.canon = Some(("zhong".to_string(), 0b1));
    nodes[4].push(zhong);
    nodes[7].push(node(4, 7, "国", 3.0));
    let mut whole = node(0, 7, "宗国", 5.0);
    whole.syl_mask = 0b10001;
    nodes[7].push(whole);

    let r = ViterbiDecoder::decode_dp_nbest(&nodes, input_len, 2);
    assert_eq!(r[0].words, ["中", "国"]);
    // zhong|guo → 位 0/5（规范码坐标），而整句 boundary 仍是所打码坐标 0/4。
    assert_eq!(r[0].boundary, 0b10001);
    assert_eq!(
        r[0].learn_code(input),
        Some(("zhongguo".to_string(), 0b100001))
    );
    assert_eq!(r[1].words, ["宗国"]);
    assert_eq!(r[1].pieces, None, "全精确的解不收集片段");
    assert_eq!(r[1].learn_code(input), None);

    let beam = ViterbiDecoder::with_grammar(Arc::new(crate::pinyin::grammar::NullGrammar))
        .decode(&nodes, input_len);
    assert_eq!(beam.words, r[0].words);
    assert_eq!(beam.pieces, r[0].pieces, "beam 与 DP 收集的片段一致");
}
