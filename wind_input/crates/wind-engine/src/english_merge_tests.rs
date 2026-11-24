use super::*;
use crate::engine::{ConvertResult, EngineType};
use wind_candidate::CandidateSource;

/// 固定吐 N 条英文候选的假引擎。
struct FakeEnglish(Vec<(&'static str, &'static str)>);

impl Engine for FakeEnglish {
    fn convert(&self, input: &str, max_candidates: usize) -> anyhow::Result<ConvertResult> {
        let candidates: Vec<Candidate> = self
            .0
            .iter()
            .filter(|(code, _)| code.starts_with(input))
            .take(max_candidates)
            .map(|(code, text)| Candidate {
                text: (*text).into(),
                code: (*code).into(),
                source: CandidateSource::English,
                ..Default::default()
            })
            .collect();
        Ok(ConvertResult {
            candidates,
            ..Default::default()
        })
    }
    fn reset(&self) {}
    fn engine_type(&self) -> EngineType {
        EngineType::English
    }
}

fn pinyin_flood(n: usize) -> Vec<Candidate> {
    (0..n)
        .map(|i| Candidate {
            text: format!("字{i}"),
            code: "he".into(),
            source: CandidateSource::Pinyin,
            ..Default::default()
        })
        .collect()
}

fn english_texts(cands: &[Candidate]) -> Vec<&str> {
    cands
        .iter()
        .filter(|c| c.source == CandidateSource::English)
        .map(|c| c.text.as_str())
        .collect()
}

/// 本模块存在的理由：拼音把配额吐满时，英文仍须在场。
/// 先立正向对照（`calm`），否则「洪水下在场」可能只是假引擎压根没产出，断言空转。
#[test]
fn english_survives_pinyin_flood() {
    let eng = FakeEnglish(vec![("hello", "hello"), ("help", "help")]);
    let max = 20;

    // 正向对照：无洪水时英文在场。
    let mut calm = pinyin_flood(2);
    merge(&mut calm, lookup(&eng, "hello", 0), max);
    assert!(
        !english_texts(&calm).is_empty(),
        "无洪水时英文应在场——否则下面那条断言测不到东西"
    );

    // 洪水：拼音已装满 max，英文仍须活下来，且总数不超 max。
    let mut flooded = pinyin_flood(max);
    merge(&mut flooded, lookup(&eng, "hello", 0), max);
    assert_eq!(
        english_texts(&flooded),
        vec!["hello"],
        "拼音装满配额时英文被整片截掉 = 开关等于没做"
    );
    assert_eq!(flooded.len(), max, "腾座后总数不得超出 max_candidates");
}

/// 只收精确命中：前缀扩展一条都不进列表（否则 `hen` 会混进 hence / Henderson 等噪音）。
#[test]
fn prefix_expansions_are_dropped() {
    let eng = FakeEnglish(vec![
        ("hen", "hen"),
        ("hence", "hence"),
        ("henderson", "Henderson"),
        ("hendrix", "Hendrix"),
    ]);
    let got = lookup(&eng, "hen", 0);
    assert_eq!(
        got.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["hen"],
        "前缀扩展必须整片丢弃，只留精确命中"
    );

    // 没有精确命中时一条都不出——用户该继续敲完，而不是从前缀词里挑。
    assert!(
        lookup(&eng, "hend", 0).is_empty(),
        "`hend` 无精确命中，不得拿 Henderson/Hendrix 顶上"
    );
}

/// 上屏否决仍按前缀判，不随 `lookup` 一起收窄成精确：否则五笔打到第 4 键 `gith` 时精确
/// 命中还不存在，满码把中文顶上屏，`github` 永远敲不完（`block_commit` 要防的事）。
#[test]
fn veto_still_matches_prefix() {
    let eng = FakeEnglish(vec![("github", "GitHub")]);
    assert!(
        lookup(&eng, "gith", 0).is_empty(),
        "前提：`gith` 无精确命中（否则下面测不到差别）"
    );
    assert!(
        has_any(&eng, "gith", 0),
        "否决判据必须仍按前缀命中，否则打到一半就被顶上屏"
    );
    // 短输入门槛对否决同样生效。
    assert!(!has_any(&eng, "gi", 0), "两字母不该触发否决");
}

/// 英文临时词（自动造词的暂存区）不参与上屏否决：只有临时词命中时按「没有英文候选」
/// 处理，五笔四码照常自动上屏；临时词与用户词合并的（已晋升 / 手工加过）仍算。
#[test]
fn veto_ignores_temp_words() {
    struct Flags(Vec<(&'static str, bool, bool)>); // (code, is_temp, is_user)
    impl Engine for Flags {
        fn convert(&self, input: &str, max: usize) -> anyhow::Result<ConvertResult> {
            let candidates = self
                .0
                .iter()
                .filter(|(c, _, _)| c.starts_with(input))
                .take(max)
                .map(|(c, t, u)| {
                    let mut cand = Candidate {
                        text: (*c).into(),
                        code: (*c).into(),
                        source: CandidateSource::English,
                        ..Default::default()
                    };
                    cand.meta.is_temp_dict = *t;
                    cand.meta.is_user_dict = *u;
                    cand
                })
                .collect();
            Ok(ConvertResult {
                candidates,
                ..Default::default()
            })
        }
        fn reset(&self) {}
        fn engine_type(&self) -> EngineType {
            EngineType::English
        }
    }
    assert!(!has_any(&Flags(vec![("ggll", true, false)]), "ggll", 0));
    assert!(
        has_any(&Flags(vec![("ggll", true, true)]), "ggll", 0),
        "已入用户词库的照算"
    );
    assert!(
        has_any(
            &Flags(vec![("ggllx", true, false), ("ggllz", false, false)]),
            "ggll",
            0
        ),
        "临时词排在前面也要看得到后面的系统词"
    );
}

/// 精确命中清掉 `is_exact_code`：那是码表域标志，跨来源带进拼音列表会让英文
/// 在协调器 `cmp_exact_first`（位置在 `by_weight` 之前）无条件压过全部中文。
#[test]
fn exact_hit_clears_codetable_flag() {
    let eng = FakeEnglish(vec![("hen", "hen")]);
    let got = lookup(&eng, "hen", 0);
    assert_eq!(got.len(), 1);
    assert!(
        !got[0].is_exact_code,
        "英文候选不得带着码表域的 is_exact_code 进跨来源列表"
    );
}

/// 短输入不查英文：两个字母就刷前缀词会淹掉正常中文输入。
#[test]
fn short_input_skips_lookup() {
    let eng = FakeEnglish(vec![("he", "he"), ("hello", "hello")]);
    assert!(
        lookup(&eng, "he", 0).is_empty(),
        "默认回退长度 3，两字母不该查"
    );
    assert!(!lookup(&eng, "hello", 0).is_empty());
    // 显式配置覆盖回退值。
    assert!(!lookup(&eng, "he", 2).is_empty());
}

/// 拼音补全判据：整串任何切法都切不成完整全拼音节才补。
#[test]
fn prefix_complete_only_when_not_full_pinyin() {
    // 切不完整 ⇒ 补。
    for s in ["templa", "configu", "produc", "xiaom"] {
        assert!(prefix_complete_applies(s), "{s} 不是完整拼音，应补");
    }
    // 能切成完整音节 ⇒ 不补。`xian` 这类有多种切法的也算（xi'an / xian）。
    for s in ["chang", "shang", "zhongguo", "xianshi", "nihao"] {
        assert!(!prefix_complete_applies(s), "{s} 是完整拼音，不该补");
    }
    // 带分隔符 / 非字母：用户在用拼音语法或打符号，不补。
    assert!(!prefix_complete_applies("tem'pla"));
    // 大写按小写判（`Shang` 仍是完整拼音）。
    assert!(!prefix_complete_applies("Shang"));
}

/// 拼音侧：够长且不是完整拼音才补；完整拼音只给完整单词；长度门槛照码表口径。
#[test]
fn select_pinyin_completion_needs_non_pinyin() {
    let eng = FakeEnglish(vec![
        ("templa", "templa"),
        ("template", "template"),
        ("templates", "templates"),
        ("templar", "templar"),
        ("shang", "shang"),
        ("shanghai", "Shanghai"),
    ]);
    let cfg = Effective {
        block_commit: false,
        block_commit_on_word: false,
        complete_min_length: 5,
        complete_only_non_pinyin: true,
        ..codetable_cfg()
    };
    assert_eq!(
        texts_of(&select(&eng, "templa", false, &cfg)),
        vec!["templa", "template"],
        "完整单词优先，合计不超 max_count"
    );
    assert_eq!(
        texts_of(&select(&eng, "shang", false, &cfg)),
        vec!["shang"],
        "完整拼音不补"
    );
    let short = Effective {
        complete_min_length: 7,
        ..cfg
    };
    assert_eq!(
        texts_of(&select(&eng, "templa", false, &short)),
        vec!["templa"]
    );
}

/// 前缀补全至多 2 条，且不与精确命中重复。
#[test]
fn prefix_complete_caps_and_skips_exact() {
    let eng = FakeEnglish(vec![
        ("hello", "hello"),
        ("hellos", "hellos"),
        ("hellow", "Hellow"),
        ("hellox", "hellox"),
    ]);
    let exact = lookup(&eng, "hello", 0);
    assert_eq!(exact.len(), 1, "前提：精确命中 hello");
    let extra = lookup_prefix_complete(&eng, "hello", &exact, 2);
    let texts: Vec<&str> = extra.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["hellos", "Hellow"], "至多 2 条、跳过精确那条");
    assert!(extra.iter().all(|c| !c.is_exact_code));
    assert!(
        extra.iter().all(|c| c.meta.from_prefix_complete),
        "补全候选要带标记，协调器靠它定位"
    );

    // 无精确命中时照补。
    let none = lookup(&eng, "hell", 0);
    assert!(none.is_empty());
    assert_eq!(lookup_prefix_complete(&eng, "hell", &none, 2).len(), 2);
}

/// 前缀兜底页：与输入相同（大小写不敏感）的词排最前，且次序落到 weight。
#[test]
fn prefix_fallback_puts_exact_word_first() {
    let eng = FakeEnglish(vec![
        ("his", "his"),
        ("high", "high"),
        ("him", "him"),
        ("history", "history"),
        ("hi", "Hi"),
    ]);
    let got = lookup_prefix_fallback(&eng, "hi", 0);
    assert_eq!(texts_of(&got), vec!["Hi", "his", "high", "him", "history"]);
    assert!(
        got[1..].iter().all(|c| c.weight < got[0].weight),
        "次序要落到 weight，否则协调器重排会冲掉"
    );
}

/// 码表侧出厂口径（补全关、上限 2）。
fn codetable_cfg() -> Effective {
    Effective {
        enable: true,
        min_length: 0,
        block_commit: true,
        block_commit_on_word: true,
        complete_min_length: 0,
        complete_only_non_pinyin: false,
        max_count: max_count_or_default(0),
    }
}

fn texts_of(v: &[Candidate]) -> Vec<&str> {
    v.iter().map(|c| c.text.as_str()).collect()
}

fn git_words() -> FakeEnglish {
    FakeEnglish(vec![
        ("git", "git"),
        ("github", "GitHub"),
        ("githubpages", "GitHub Pages"),
        ("gitlab", "GitLab"),
    ])
}

/// 出厂口径：中文侧有候选时 3 / 4 码只出完整单词，不补前缀（现状不变）。
#[test]
fn select_default_only_exact_when_base_present() {
    let eng = git_words();
    let cfg = codetable_cfg();
    assert_eq!(texts_of(&select(&eng, "git", false, &cfg)), vec!["git"]);
    assert!(select(&eng, "gith", false, &cfg).is_empty());
    // 中文侧为空：给前缀补全一页（不受 max_count 约束）。
    assert_eq!(
        texts_of(&select(&eng, "gith", true, &cfg)),
        vec!["GitHub", "GitHub Pages"]
    );
}

/// `complete_min_length = 4`：4 码起中文有候选也补，3 码不补。
#[test]
fn select_complete_min_length_gates_completion() {
    let eng = git_words();
    let cfg = Effective {
        complete_min_length: 4,
        ..codetable_cfg()
    };
    let got = select(&eng, "gith", false, &cfg);
    assert_eq!(texts_of(&got), vec!["GitHub", "GitHub Pages"]);
    assert!(got.iter().all(|c| c.meta.from_prefix_complete));
    assert_eq!(
        texts_of(&select(&eng, "git", false, &cfg)),
        vec!["git"],
        "3 码不到补全门槛，只给完整单词"
    );
}

/// `max_count` 封顶：完整单词优先，补全只填剩下的座位。
#[test]
fn select_max_count_caps_exact_first() {
    let eng = git_words();
    let one = Effective {
        complete_min_length: 3,
        max_count: 1,
        ..codetable_cfg()
    };
    assert_eq!(texts_of(&select(&eng, "git", false, &one)), vec!["git"]);
    let three = Effective {
        max_count: 3,
        ..one
    };
    assert_eq!(
        texts_of(&select(&eng, "git", false, &three)),
        vec!["git", "GitHub", "GitHub Pages"]
    );
}

/// `complete_min_length` 比 `min_length` 小时，完整单词也从补全门槛起出：
/// 打 `go` 不能只补出 good / google 而漏掉 go。
#[test]
fn select_exact_gate_follows_completion_gate() {
    let eng = FakeEnglish(vec![("go", "go"), ("good", "good"), ("google", "Google")]);
    let cfg = Effective {
        complete_min_length: 2,
        ..codetable_cfg()
    };
    assert_eq!(
        texts_of(&select(&eng, "go", false, &cfg)),
        vec!["go", "good"]
    );
    // 不开补全时仍按 min_length（默认 3）：2 码一条都不出。
    assert!(select(&eng, "go", false, &codetable_cfg()).is_empty());
}

/// 两级否决：前缀级涵盖成词级；只开成词级时，英文词的前缀不拦、整词才拦。
#[test]
fn veto_two_levels() {
    let eng = git_words();
    let prefix = codetable_cfg();
    assert!(
        vetoes_commit(&eng, "gith", &[], &prefix),
        "前缀级：gith 是 github 的前缀"
    );
    let word_only = Effective {
        block_commit: false,
        ..prefix
    };
    assert!(
        !vetoes_commit(&eng, "gith", &[], &word_only),
        "成词级不管前缀"
    );
    assert!(
        vetoes_commit(&eng, "github", &[], &word_only),
        "整串是英文单词"
    );
    let off = Effective {
        block_commit_on_word: false,
        ..word_only
    };
    assert!(!vetoes_commit(&eng, "github", &[], &off));
}

/// 列表那一半同样不算英文临时词（只有临时词进了列表时放行上屏）。
#[test]
fn veto_shown_list_ignores_temp_words() {
    let none = FakeEnglish(vec![]);
    let mut temp = Candidate {
        text: "ggll".into(),
        code: "ggll".into(),
        source: CandidateSource::English,
        ..Default::default()
    };
    temp.meta.is_temp_dict = true;
    let cfg = codetable_cfg();
    assert!(!vetoes_commit(
        &none,
        "ggll",
        std::slice::from_ref(&temp),
        &cfg
    ));
    temp.meta.is_user_dict = true;
    assert!(
        vetoes_commit(&none, "ggll", &[temp], &cfg),
        "已入用户词库的照算"
    );
}

/// 否决门槛跟着「英文最早从几码露面」走。
#[test]
fn veto_gate_follows_completion_gate() {
    let cfg = codetable_cfg();
    assert_eq!(cfg.veto_min_length(), 3);
    let early = Effective {
        complete_min_length: 2,
        ..cfg
    };
    assert_eq!(early.veto_min_length(), 2, "补全 2 码起出，否决也该 2 码起");
    let late = Effective {
        complete_min_length: 5,
        ..cfg
    };
    assert_eq!(late.veto_min_length(), 3, "完整单词仍从 3 码起出");
}

/// 同文本不重复入列，且被丢弃那条的码位并进幸存者。
#[test]
fn same_text_dedups_into_base() {
    let eng = FakeEnglish(vec![("ok", "OK")]);
    let mut base = vec![Candidate {
        text: "OK".into(),
        code: "ok".into(),
        source: CandidateSource::English,
        ..Default::default()
    }];
    merge(&mut base, lookup(&eng, "ok", 1), 10);
    assert_eq!(base.len(), 1, "同文本不该重复入列");
}
