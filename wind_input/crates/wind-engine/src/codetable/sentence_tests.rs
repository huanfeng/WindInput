use super::*;
use wind_candidate::Candidate;
use wind_dict::layer::{DictLayer, LayerType};

/// 最小内存词库层：直接持 `(code, text, weight)` 列表。
///
/// 本模块的判据全在「码长关系 + 权重量纲」上，手写词条才能把一简/二简/全码的结构摆清楚；
/// 真实词库会把判据淹没在噪声里（且 worktree 未必有 build_dev/data）。
struct MemLayer(Vec<(String, String, i32)>);

impl MemLayer {
    fn new(entries: &[(&str, &str, i32)]) -> Self {
        Self(
            entries
                .iter()
                .map(|(c, t, w)| (c.to_string(), t.to_string(), *w))
                .collect(),
        )
    }
}

impl DictLayer for MemLayer {
    fn name(&self) -> &str {
        "mem"
    }
    fn layer_type(&self) -> LayerType {
        LayerType::System
    }
    fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        let mut v: Vec<Candidate> = self
            .0
            .iter()
            .filter(|(c, _, _)| c == code)
            .map(|(c, t, w)| Candidate {
                text: t.clone(),
                code: c.clone(),
                weight: *w,
                ..Default::default()
            })
            .collect();
        v.sort_by_key(|c| std::cmp::Reverse(c.weight));
        v.truncate(limit);
        v
    }
    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        let mut v: Vec<Candidate> = self
            .0
            .iter()
            .filter(|(c, _, _)| c.starts_with(prefix) && c != prefix)
            .map(|(c, t, w)| Candidate {
                text: t.clone(),
                code: c.clone(),
                weight: *w,
                ..Default::default()
            })
            .collect();
        v.sort_by_key(|c| std::cmp::Reverse(c.weight));
        v.truncate(limit);
        v
    }
    fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        for (c, t, w) in &self.0 {
            f(c, t, *w);
        }
    }
}

/// 五笔结构的缩微模型：一简 / 二简 / 三简 / 3 码全码 / 4 码全码 / 4 码词组俱全，
/// 权重照抄极点词库的层级带（简码 9999/9950/9000，全码带百位）。
fn dm() -> DictManager {
    let dm = DictManager::new();
    dm.register_layer(Box::new(MemLayer::new(&[
        // 一简（皆为简码：各自都有更长的码）
        ("g", "一", 9999),
        ("a", "工", 9999),
        ("d", "在", 9999),
        ("w", "人", 9999),
        // 二简（简码：各自都配了 4 码全码，否则它们自己就是全码——
        //        这正是「二简 655 条里有 1 条其实是全码」的那种情形）
        ("aa", "式", 9950),
        ("wt", "何", 9950),
        // 三简（简码：有 4 码全码）
        ("aaa", "工", 9000),
        // 3 码全码（无更长码 —— 「整句只认 4 码」走不通的那 1604 条的代表）
        ("hci", "皮", 1200),
        // 4 码全码单字
        ("aaaa", "工", 800),
        ("ggll", "一", 700),
        ("dhfh", "在", 650),
        ("wtgf", "人", 600),
        ("aagg", "式", 500),
        ("wtwt", "何", 450),
        // 4 码词组
        ("aawt", "工作", 1241),
        ("dhwt", "在人", 90),
        ("hcig", "皮衣", 400),
    ])));
    dm
}

/// 分隔符的字符串形式（测试里拼串用）。
const SEP: &str = "'";

fn decoder() -> CodeSentenceDecoder {
    CodeSentenceDecoder::new(4)
}

#[test]
fn short_code_index_marks_only_short_entries() {
    let dm = dm();
    let idx = ShortCodeIndex::build(&dm);

    // 一简「工 a」是简码，整句权重换成它 4 码全码 aaaa 的 800。
    assert_eq!(idx.resolve("a", "工", 9999), (800, true));
    // 三简「工 aaa」同样是简码。
    assert_eq!(idx.resolve("aaa", "工", 9000), (800, true));
    // 4 码全码不是简码，权重原样。
    assert_eq!(idx.resolve("aaaa", "工", 800), (800, false));
    // 3 码全码「皮 hci」不是简码——它没有更长的码，所以「整句只认 4 码单元」走不通。
    assert_eq!(idx.resolve("hci", "皮", 1200), (1200, false));
    // 词组不是简码。
    assert_eq!(idx.resolve("aawt", "工作", 1241), (1241, false));

    // 二简也要被认出来（它们各自配了 4 码全码）。
    assert_eq!(idx.resolve("aa", "式", 9950), (500, true));

    // 进表的恰是简码那几条：a/g/d/w 四个一简 + aa/wt 两个二简 + aaa 一个三简。
    // 这个数字是夹具结构的函数（二简得配了全码才算简码），改夹具必须回头核对。
    assert_eq!(idx.len(), 7, "简码条目数");
}

#[test]
fn sentence_prefers_word_over_short_code_chain() {
    // `aawt` 整串就是「工作」一个词 ⇒ 按约定不产整句（交给精确匹配路径）。
    // 这里验证更长的串里词边能赢过简码链：`aawtaawt` 应解成「工作工作」而不是简码碎片。
    let dm = dm();
    let r = decoder().decode("aawtaawt", &dm).expect("应有整串解");
    assert_eq!(r.text, "工作工作");
    assert_eq!(r.words, vec!["工作", "工作"]);
}

#[test]
fn sentence_uses_three_code_full_entry() {
    // 3 码全码「皮」必须能参与整句：`hciaawt` → 皮 + 工作。
    // 若把「不足 4 码一律当简码抑制」，这里会解不出或解错。
    let dm = dm();
    let r = decoder().decode("hciaawt", &dm).expect("应有整串解");
    assert_eq!(r.text, "皮工作");
}

#[test]
fn single_word_solution_is_rejected() {
    // 整串恰是一个词 ⇒ 不产整句（对齐 librime 排除「单个词覆盖全串」）。
    let dm = dm();
    assert!(decoder().decode("aawt", &dm).is_none());
}

/// 特殊语法词条（`$CC` 命令 / `{..}` 模板）不进整句 lattice。
///
/// 它们要到协调器 `finalize_candidates` 才展开，且只认「整条候选即命令」，拼进整句就是把
/// 源码当文本显示/上屏。与逆切分 `split_segment` 同判据：剔光的段视同无码 ⇒ 含该段的
/// 整句解不存在。
#[test]
fn direct_command_entry_never_enters_sentence_lattice() {
    let dm = DictManager::new();
    dm.register_layer(Box::new(MemLayer::new(&[
        ("niys", "你用", 900),
        // 同码放一正一「直通」两条：修复后正常词条上位，修复前直通源码抢进整句。
        ("oix", "偶", 600),
        (
            "oix",
            r#"$CC("[查]：{last(1)}", open("https://flypy.cc/ix/?q={url(last(1))}"))"#,
            700,
        ),
    ])));
    let r = decoder().decode("niysoix", &dm).expect("应有整句解");
    assert_eq!(r.text, "你用偶", "直通源码须让位给正常词条");
    assert_eq!(r.words, vec!["你用", "偶"]);

    // 对照组：段码只有直通词条时，整段视同无码 ⇒ 覆盖整串的解不存在 ⇒ 不产整句。
    let dm2 = DictManager::new();
    dm2.register_layer(Box::new(MemLayer::new(&[
        ("niys", "你用", 900),
        (
            "oix",
            r#"$CC("[查]：{world}", open("https://x/?q={url(last(1))}"))"#,
            700,
        ),
    ])));
    assert!(
        decoder().decode("niysoix", &dm2).is_none(),
        "直通段视同无码，含该段的整句须无解"
    );
}

/// 纯文本直通 `$CC("标签", type("字"))` 进整句，用上屏文本「字」而非标签；
/// 带插值 / 多动作 / Effect 的仍不进（AtomGit#1 后续：文本词条不该被一并屏蔽）。
#[test]
fn plain_type_entry_joins_sentence_with_typed_text() {
    let dm = DictManager::new();
    dm.register_layer(Box::new(MemLayer::new(&[
        ("niys", "你用", 900),
        ("oba", r#"$CC("横_一", type("一"))"#, 700),
    ])));
    let r = decoder()
        .decode("niysoba", &dm)
        .expect("纯 type 词条应进整句");
    assert_eq!(r.text, "你用一");
    assert_eq!(r.words, vec!["你用", "一"]);

    for src in [
        r#"$CC("x", type("{last(1)}"))"#,
        r#"$CC("x", type("a"), open("https://x/"))"#,
        r#"$CC("x", open("https://x/"))"#,
    ] {
        let dm2 = DictManager::new();
        dm2.register_layer(Box::new(MemLayer::new(&[
            ("niys", "你用", 900),
            ("oba", src, 700),
        ])));
        assert!(decoder().decode("niysoba", &dm2).is_none(), "{src}");
    }
}

#[test]
fn unreachable_input_yields_none() {
    // 尾部 `z` 无任何词条 ⇒ 没有覆盖整串的路径 ⇒ 不产整句。
    // 测 `log_prob.is_finite()` 而非 `words.is_empty()`：dp[len] 不可达时 decode 会从最远
    // 可达位置回溯并返回一条部分路径。
    let dm = dm();
    assert!(decoder().decode("aawtz", &dm).is_none());
}

#[test]
fn short_code_still_usable_when_no_full_code_path() {
    // 简码不是被禁掉、只是被罚：没有别的解法时它照样成句。
    // `ghci` = 一(g，一简) + 皮(hci，3 码全码)，`ghci` 整串与 `ghc` 都无词条。
    let dm = dm();
    let r = decoder().decode("ghci", &dm).expect("应有整串解");
    assert_eq!(r.text, "一皮");
}

#[test]
fn tail_short_code_is_penalized_less_than_middle() {
    // 同一条简码边，串尾比串中便宜——这是「用户还没打完」的表达。
    // 用打分函数直接对账，避免依赖某个具体输入串的解码结果。
    let mid = score_code_node(700, CODE_DICT_TOTAL) - SHORT_CODE_PENALTY;
    let tail = score_code_node(700, CODE_DICT_TOTAL) - SHORT_CODE_TAIL_PENALTY;
    assert!(tail > mid, "串尾简码应罚得更轻");
}

#[test]
fn same_span_prefers_frequency_not_word_length() {
    // 同跨度下纯按词频，不因「是词组」而偏好（见 `score_code_node` 文档 ①）。
    // 形如真实词库的 `ggll`：单字「一」与四字词「来来回回」同码同跨度，「一」词频更高；
    // 加回多字词加成的话「来来回回」会赢，本测试会红。
    let dm = DictManager::new();
    dm.register_layer(Box::new(MemLayer::new(&[
        ("g", "一", 9999),
        ("ggll", "一", 700),
        ("ggll", "来来回回", 600),
        ("hci", "皮", 1200),
    ])));
    let r = decoder().decode("ggllhci", &dm).expect("应有整串解");
    assert_eq!(
        r.words,
        vec!["一", "皮"],
        "同跨度应按词频选「一」，不得因字数偏好「来来回回」"
    );
}

#[test]
fn pinyin_freq_overrides_codetable_weight_bands() {
    // 词频来源的核心行为：接上拼音词库后完全不看码表 weight。
    //
    // 同一条边（`wh`）上两个字对抗，两套权重给出相反结论：
    // - 码表 weight：「丁」9950（简码带）vs「个」100（普通带）⇒ 丁赢
    // - 真实词频：  「个」215733       vs「丁」1000        ⇒ 个赢
    // 必须同码同跨度才构成竞争，跨度不同就不在一条边上，测的只是哪条路径走得通。
    let entries: &[(&str, &str, i32)] =
        &[("wh", "丁", 9950), ("wh", "个", 100), ("hci", "皮", 1200)];
    let dm = DictManager::new();
    dm.register_layer(Box::new(MemLayer::new(entries)));

    // 「拼音词库」：只需是一份带真实词频的 (code,text,weight) 表，码是什么无所谓——
    // `SentenceFreq` 按 text 查，不碰码。
    let py = || {
        CachedDict::Memory({
            let mut d = wind_dict::codetable::CodetableDict::empty();
            d.merge_single("ge".into(), "个".into(), 215_733, 0);
            d.merge_single("ding".into(), "丁".into(), 1_000, 1);
            d.merge_single("pi".into(), "皮".into(), 5_000, 2);
            d
        })
    };

    let with_freq = decoder().with_pinyin_dict(std::sync::Arc::new(py()));
    let r = with_freq.decode("whhci", &dm).expect("应有整串解");
    assert_eq!(r.words, vec!["个", "皮"], "接上真实词频后应按词频选「个」");

    // 对照：退化路径（无拼音词库）被权重带带偏，选「丁」。不是锁定错误行为，而是标明
    // 没有词频来源时准确率确实会掉。
    let r2 = decoder().decode("whhci", &dm).expect("应有整串解");
    assert_eq!(
        r2.words,
        vec!["丁", "皮"],
        "退化路径按码表权重带走 —— 这是已知的准确率损失"
    );
}

#[test]
fn sentence_freq_falls_back_to_oov_for_missing_text() {
    // 拼音库查不到的字给 OOV_FREQ（=罕见），不是中性值：码表收了大量生僻字，
    // 按中位数计价会让它们在整句里与常用字平起平坐。
    let dm = DictManager::new();
    dm.register_layer(Box::new(MemLayer::new(&[("qq", "兲", 9950)])));
    let py = CachedDict::Memory({
        let mut d = wind_dict::codetable::CodetableDict::empty();
        d.merge_single("de".into(), "的".into(), 1_000_000, 0);
        d
    });
    let f = SentenceFreq::build(&dm, &py);
    assert_eq!(f.get("兲"), OOV_FREQ, "码表有、拼音库无 ⇒ 兜底为罕见");
    // 只留交集：拼音库里的「的」不在码表里，不该进表（控制内存）。
    assert_eq!(f.len(), 0, "只保留码表里出现过的 text");
}

#[test]
fn split_code_marks_engine_chosen_boundaries() {
    // 切分串必须来自 Viterbi 实际走的那条路径（boundary），不是按词长重猜——
    // 同一串码可有多种切法。
    let dm = dm();
    let r = decoder().decode("aawtaawt", &dm).expect("应有整串解");
    assert_eq!(r.split_code("aawtaawt"), "aawt@aawt".replace('@', SEP));
}

#[test]
fn manual_separator_splits_into_segments() {
    // 手动分隔符：每段独立解码。`aa` + `wt` 各是一个二简字、段内各只有一个词，
    // 所以「至少两个词」必须按整体词数判，不能判在段级。
    let dm = dm();
    let input = format!("aa{SEP}wt");
    let r = decoder().decode(&input, &dm).expect("分段应能解出");
    assert_eq!(r.words, vec!["式", "何"]);
    // 用户自己敲的分隔符原样留下，不再叠一个。
    assert_eq!(r.split_code(&input), input);
}

#[test]
fn manual_separator_overrides_greedy_split() {
    // 分隔符的用途：把本该被贪心切走的串掰开。
    // `aawt` 整串是词「工作」，加了分隔符后强制读成两个二简字。
    let dm = dm();
    let joined = decoder().decode("aawtaawt", &dm).expect("应有解");
    assert_eq!(joined.words, vec!["工作", "工作"]);

    let input = format!("aa{SEP}wt{SEP}aa{SEP}wt");
    let split = decoder().decode(&input, &dm).expect("应有解");
    assert_eq!(
        split.words,
        vec!["式", "何", "式", "何"],
        "分隔符是硬边界，不得跨段成词"
    );
}

#[test]
fn redundant_separators_are_tolerated() {
    // 首尾/连续分隔符是用户多敲的，不该让整句失败。
    let dm = dm();
    let input = format!("{SEP}aa{SEP}{SEP}wt{SEP}");
    let r = decoder().decode(&input, &dm).expect("冗余分隔符应被容忍");
    assert_eq!(r.words, vec!["式", "何"]);
}

#[test]
fn separator_segment_without_solution_fails_whole() {
    // 某一段解不出 ⇒ 整体无解。分隔符是用户的明确表态，不能悄悄跨段重切。
    let dm = dm();
    let input = format!("aa{SEP}zz");
    assert!(decoder().decode(&input, &dm).is_none());
}

/// 拼音词库替身：`(text, 词频)`，码无所谓（`SentenceFreq` 按 text 查）。
fn pinyin_dict(entries: &[(&str, i32)]) -> std::sync::Arc<CachedDict> {
    let mut d = wind_dict::codetable::CodetableDict::empty();
    for (i, (t, w)) in entries.iter().enumerate() {
        d.merge_single(format!("py{i}"), t.to_string(), *w, i as _);
    }
    std::sync::Arc::new(CachedDict::Memory(d))
}

/// 组词夹具：五个单字，全是全码（都没有更长的码）。
///
/// `aabbcc` 有两种切法：`甲|乙|丙`（3 条边）与 `丁|戊`（2 条边）。单字词频相同，
/// 每边罚让 2 条边的那种赢——除非解码器知道「甲乙」是个词。
fn compose_dm() -> DictManager {
    let dm = DictManager::new();
    dm.register_layer(Box::new(MemLayer::new(&[
        ("aa", "甲", 500),
        ("bb", "乙", 500),
        ("cc", "丙", 500),
        ("aab", "丁", 500),
        ("bcc", "戊", 500),
    ])));
    dm
}

const CHARS: [(&str, i32); 5] = [
    ("甲", 10_000),
    ("乙", 10_000),
    ("丙", 10_000),
    ("丁", 10_000),
    ("戊", 10_000),
];

#[test]
fn composes_lexicon_word_across_char_units() {
    // 词典组词的核心行为：码表里没有「甲乙」，逐字打出来也要认得它是个词。
    let dm = compose_dm();
    let with_word = [&CHARS[..], &[("甲乙", 500_000)]].concat();
    let r = decoder()
        .with_pinyin_dict(pinyin_dict(&with_word))
        .decode("aabbcc", &dm)
        .expect("应有整串解");
    assert_eq!(r.words, vec!["甲乙", "丙"]);
    // 组出的词在组合区仍按编码单元切分：用户打的是三个码，不是两个。
    assert_eq!(r.split_code("aabbcc"), "aa@bb@cc".replace('@', SEP));

    // 对照：拼音库里没有这个词 ⇒ 不组词，每边罚让 2 条边的切法赢。
    // 没有这条，「无条件把相邻单字拼起来」的实现同样能让上面变绿。
    let r = decoder()
        .with_pinyin_dict(pinyin_dict(&CHARS))
        .decode("aabbcc", &dm)
        .expect("应有整串解");
    assert_eq!(r.words, vec!["丁", "戊"]);
}

#[test]
fn composed_word_may_cover_whole_input() {
    // 超码长时一个词覆盖整串也要放行：它只能是组词边，精确路径产不出来，
    // 「排除单词解」会把这条候选整个弄丢（如逐字打「喃喃自语」）。
    let dm = compose_dm();
    let with_word = [&CHARS[..], &[("甲乙丙", 500_000)]].concat();
    let r = decoder()
        .with_pinyin_dict(pinyin_dict(&with_word))
        .decode("aabbcc", &dm)
        .expect("应有整串解");
    assert_eq!(r.words, vec!["甲乙丙"]);
}

#[test]
fn trailing_separator_keeps_composed_single_word() {
    // 无分隔符能出的组词单词解，多敲一个尾部分隔符（冗余，本该被容忍）不能让它消失。
    let dm = compose_dm();
    let with_word = [&CHARS[..], &[("甲乙丙", 500_000)]].concat();
    let dec = decoder().with_pinyin_dict(pinyin_dict(&with_word));
    let input = format!("aabbcc{SEP}");
    let r = dec.decode(&input, &dm).expect("尾部分隔符不应让整句消失");
    assert_eq!(r.words, vec!["甲乙丙"]);
    // 对照：码长内的单词解仍按老规矩排除（那是码表词条，精确路径会出）。
    assert!(dec.decode(&format!("aawt{SEP}"), &self::dm()).is_none());
}

#[test]
fn missing_grammar_file_falls_back_to_plain_decoding() {
    // 定制版可能删掉模型文件：打不开就按无模型解码，结果与不挂模型逐条相同。
    let dm = dm();
    let plain = decoder().decode_nbest("aawtaawt", &dm, 3);
    let missing = decoder()
        .with_grammar_file("/nonexistent/wanxiang-mini.gram".into(), 0.5)
        .decode_nbest("aawtaawt", &dm, 3);
    let texts = |v: &[SentenceResult]| v.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
    assert_eq!(texts(&missing), texts(&plain));
}

/// 只出用户词的层：candidate 带 `is_user_dict`，与生产 `StoreUserLayer` 经 composite 后的形态一致。
struct UserLayer(MemLayer);

impl DictLayer for UserLayer {
    fn name(&self) -> &str {
        "user"
    }
    fn layer_type(&self) -> LayerType {
        LayerType::User
    }
    fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        let mut v = self.0.search(code, limit);
        for c in &mut v {
            c.meta.is_user_dict = true;
        }
        v
    }
    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        self.0.search_prefix(prefix, limit)
    }
    fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        self.0.for_each_entry(f)
    }
}

#[test]
fn user_word_missing_from_lexicon_gets_floor_frequency() {
    // 用户造的词（人名、新词）多半不在拼音库里，按「罕见」计价永远输给逐字拆开；
    // 给它组词表的入表门槛作下限才能赢回来。竞争者：`xxyy` 也能切成高频字「子」+「丑」。
    let entries: &[(&str, &str, i32)] = &[("xx", "子", 500), ("yy", "丑", 500), ("zz", "寅", 500)];
    let py = pinyin_dict(&[
        ("子", 100_000),
        ("丑", 100_000),
        ("寅", 100_000),
        // 组词表里唯一的词 ⇒ 入表门槛 = 它的词频。
        ("别的词", 50_000),
    ]);
    let run = |user: bool| {
        let dm = DictManager::new();
        dm.register_layer(Box::new(MemLayer::new(entries)));
        let word = MemLayer::new(&[("xxyy", "盖伦", 100)]);
        if user {
            dm.register_layer(Box::new(UserLayer(word)));
        } else {
            dm.register_layer(Box::new(word));
        }
        decoder()
            .with_pinyin_dict(py.clone())
            .decode("xxyyzz", &dm)
            .expect("应有整串解")
            .words
    };
    assert_eq!(run(true), vec!["盖伦", "寅"], "用户词应拿到词频下限");
    // 对照：同一条词条不是用户词 ⇒ 仍按罕见计价，输给逐字拆开。
    assert_eq!(run(false), vec!["子", "丑", "寅"]);
}

#[test]
fn nbest_returns_distinct_ranked_alternatives() {
    let dm = dm();
    let rs = decoder().decode_nbest("aawtaawt", &dm, 3);
    assert!(rs.len() >= 2, "应有备选，实得 {}", rs.len());
    assert_eq!(rs[0].text, "工作工作", "名次 1 与单条解码一致");
    let mut texts: Vec<&str> = rs.iter().map(|r| r.text.as_str()).collect();
    texts.dedup();
    assert_eq!(texts.len(), rs.len(), "按文本去重");
    assert!(
        rs.windows(2).all(|w| w[0].log_prob >= w[1].log_prob),
        "按分数降序"
    );
    assert!(decoder().decode_nbest("aawtaawt", &dm, 1).len() == 1);
}

/// 把「工作」罚到出局的语法模型。
struct HateGongzuo;

impl Grammar for HateGongzuo {
    fn query(&self, _context: &str, word: &str, _is_rear: bool) -> f64 {
        if word == "工作" { -100.0 } else { 0.0 }
    }
}

#[test]
fn grammar_picks_rank_one_and_plain_nbest_fills_the_rest() {
    // 挂模型时 Viterbi 的 N-best 只给一条，备选由无模型解码补位。
    let dm = dm();
    let plain = decoder().decode("aawtaawt", &dm).expect("应有解").text;
    assert_eq!(plain, "工作工作");
    let rs = decoder()
        .with_grammar(std::sync::Arc::new(HateGongzuo))
        .decode_nbest("aawtaawt", &dm, 3);
    assert_ne!(rs[0].text, plain, "名次 1 应来自挂模型的解码");
    assert_eq!(
        rs.get(1).map(|r| r.text.as_str()),
        Some(plain.as_str()),
        "名次 2 起是无模型的最优解"
    );
}

#[test]
fn empty_and_non_ascii_input_is_rejected() {
    let dm = dm();
    assert!(decoder().decode("", &dm).is_none());
    assert!(decoder().decode("工作", &dm).is_none());
}
