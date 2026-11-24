use super::*;
use crate::codetable::{CodeTableEngine, CommitOptions};
use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::codetable::CodetableDict;
use wind_dict::{DictManager, SystemDictLayer};

/// 构建一个内存码表引擎（可选开启全码自动上屏）。
fn ct_engine(entries: &[(&str, &str, i32)], at_full: bool) -> Box<dyn Engine> {
    let mut d = CodetableDict::empty();
    for (i, (code, text, w)) in entries.iter().enumerate() {
        d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
    let opts = CommitOptions {
        auto_commit_at_full: at_full,
        auto_commit_min_len: 4,
        ..Default::default()
    };
    Box::new(CodeTableEngine::new(4, opts, Arc::new(dm)))
}

// ── 截断的拼音保底配额（`truncate_with_pinyin_quota`）──

fn ct_cand(text: &str, weight: i32) -> Candidate {
    Candidate {
        text: text.into(),
        weight,
        source: CandidateSource::CodeTable,
        ..Default::default()
    }
}

fn py_cand(text: &str, weight: i32) -> Candidate {
    Candidate {
        text: text.into(),
        weight,
        source: CandidateSource::Pinyin,
        ..Default::default()
    }
}

/// 复刻 `pu`（495 条码表 + 拼音）现场：码表候选多到吃满整个配额，拼音一条都进不来。
/// 保底后拼音应拿到 `max/PINYIN_QUOTA_DIVISOR` 席。
#[test]
fn pinyin_gets_minimum_quota_when_codetable_floods() {
    let mut cands: Vec<Candidate> = (0..20)
        .map(|i| ct_cand(&format!("码{i}"), 500_000))
        .collect();
    cands.extend((0..5).map(|i| py_cand(&format!("拼{i}"), 100 - i)));
    MixedEngine::truncate_with_pinyin_quota(&mut cands, 10);
    assert_eq!(cands.len(), 10, "总数仍受 max_candidates 约束");
    let py = cands
        .iter()
        .filter(|c| c.source == CandidateSource::Pinyin)
        .count();
    assert_eq!(py, 2, "10/5=2 席保底（否则协调器的拼音精确档无候选可提）");
    // 挤掉的是权重最低的码表候选，码表仍占多数。
    assert_eq!(cands.len() - py, 8);
}

/// 反向锁：尾部没有拼音候选时（`kh` 这类非音节码、纯五笔溢出串），一条码表候选都不许被挤掉。
#[test]
fn no_pinyin_means_no_codetable_is_evicted() {
    let mut cands: Vec<Candidate> = (0..20)
        .map(|i| ct_cand(&format!("码{i}"), 500_000))
        .collect();
    MixedEngine::truncate_with_pinyin_quota(&mut cands, 10);
    assert_eq!(cands.len(), 10);
    assert!(
        cands.iter().all(|c| c.source == CandidateSource::CodeTable),
        "无拼音可补时不得腾位"
    );
    assert_eq!(cands[0].text, "码0", "顺序不应被打乱");
}

/// 未超上限时原样不动（不触发任何腾位逻辑）。
#[test]
fn under_limit_is_untouched() {
    let mut cands = vec![ct_cand("码", 500_000), py_cand("拼", 69)];
    MixedEngine::truncate_with_pinyin_quota(&mut cands, 10);
    assert_eq!(cands.len(), 2);
}

/// 拼音本就够席位时不额外腾位（避免把配额当成"必须凑满"的硬指标）。
#[test]
fn existing_pinyin_above_quota_needs_no_eviction() {
    let mut cands: Vec<Candidate> = (0..5)
        .map(|i| py_cand(&format!("拼{i}"), 900 - i))
        .collect();
    cands.extend((0..20).map(|i| ct_cand(&format!("码{i}"), 100)));
    MixedEngine::truncate_with_pinyin_quota(&mut cands, 10);
    let py = cands
        .iter()
        .filter(|c| c.source == CandidateSource::Pinyin)
        .count();
    assert_eq!(py, 5, "前 10 条里已有 5 条拼音 ≥ 配额 2，不动");
    assert_eq!(cands.len(), 10);
}

/// 接线验证：配额逻辑必须真的挂在 `convert` 的截断上，不能只是个没人调的函数
/// （「函数写对了但生产端不调」是本仓反复出现的欠账形态）。
#[test]
fn convert_applies_pinyin_quota() {
    // 码表 20 条同码候选（权重高，会吃满配额）；拼音 5 条。
    let entries: Vec<(String, String, i32)> = (0..20)
        .map(|i| ("aa".to_string(), format!("码{i}"), 9000 - i))
        .collect();
    let refs: Vec<(&str, &str, i32)> = entries
        .iter()
        .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
        .collect();
    let e = MixedEngine::new(
        ct_engine(&refs, false),
        Some(Box::new(FakePinyinMulti { n: 5 })),
        None,
        MixConfig::default(),
    );
    let r = e.convert("aa", 10).unwrap();
    assert_eq!(r.candidates.len(), 10);
    let py = r
        .candidates
        .iter()
        .filter(|c| c.source == CandidateSource::Pinyin)
        .count();
    assert_eq!(py, 2, "convert 必须走带配额的截断");
}

// ── 截断的存活集合契约（`sort_dedup_truncate`，见 `docs/design/mixed-source-tier-quota.md`）──
//
// 上面那组只断言各来源的候选数，而数相等不等于集合相等：置顶（shadow pin）在协调器应用、
// 位于引擎截断之后，被截掉的候选规则读得到却没有目标可顶，表现为「置顶了但没反应」且
// 完全静默。故判据须是集合。

/// 带编码的码表候选（去重与并码位测试用）。
fn ct_cand_coded(text: &str, code: &str, weight: i32) -> Candidate {
    Candidate {
        code: code.into(),
        ..ct_cand(text, weight)
    }
}

fn py_cand_coded(text: &str, code: &str, weight: i32) -> Candidate {
    Candidate {
        code: code.into(),
        ..py_cand(text, weight)
    }
}

/// 测试用截断上下文。两个判据串默认取一个不会命中任何候选的串，使全部候选落到
/// 非精确档——想测档 0 / 档 2 的分野时显式传入对应的码。
fn tctx<'a>(codetable_exact: &'a str, english_exact: &'a str) -> TruncationCtx<'a> {
    TruncationCtx {
        codetable_exact,
        english_exact,
    }
}

/// 取 `(source, text)` 集合——存活集合判据的统一写法。
///
/// 用 `{:?}` 而非给共享 crate 的 `CandidateSource` 加 `Ord`：那会凭空造出「来源有大小」的
/// 语义，而本设计恰恰在说来源之间不可比。
fn survivor_set(cands: &[Candidate]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = cands
        .iter()
        .map(|c| (format!("{:?}", c.source), c.text.clone()))
        .collect();
    v.sort();
    v
}

/// 存活的是哪些候选，不只是几条。
///
/// 码表 12 条（权重递减）+ 拼音 5 条，`max=10` ⇒ 拼音保底 2 席、码表 8 席。
/// 码表留权重最高的 8 条（码0..码7），拼音从被截掉的尾部按序补 2 条（拼0、拼1）。
#[test]
fn survivor_set_is_the_contract_not_the_count() {
    let mut cands: Vec<Candidate> = (0..12)
        .map(|i| ct_cand(&format!("码{i}"), 9000 - i))
        .collect();
    cands.extend((0..5).map(|i| py_cand(&format!("拼{i}"), 100 - i)));
    MixedEngine::sort_dedup_truncate(&mut cands, 10, tctx("zzzz", "zzzz"));

    let mut expect: Vec<(String, String)> = (0..8)
        .map(|i| ("CodeTable".to_string(), format!("码{i}")))
        .collect();
    expect.extend((0..2).map(|i| ("Pinyin".to_string(), format!("拼{i}"))));
    expect.sort();
    assert_eq!(survivor_set(&cands), expect);
}

/// 同 `text` 跨来源时保留码表那条：跨来源保留优先级须显式定义，不能隐含在权重加成里。
/// 用拼音「的」（真实词频全表最高之一）作对手：没有档位它会碾压码表。
#[test]
fn same_text_across_sources_keeps_codetable() {
    let mut cands = vec![
        // 真实词频，不带任何加成——拼音「的」是全表最高之一，若无档位它会碾压码表。
        py_cand_coded("的", "de", 15_378_475),
        ct_cand_coded("的", "r", 9950),
    ];
    MixedEngine::sort_dedup_truncate(&mut cands, 10, tctx("r", "zzzz"));
    assert_eq!(cands.len(), 1, "同文只留一条");
    assert_eq!(
        cands[0].source,
        CandidateSource::CodeTable,
        "跨来源同文须保留码表那条"
    );
    assert_eq!(cands[0].code, "r");
}

/// 同来源同文并码位；跨来源不并（两套编码不同域）。理由见 `Candidate::absorb_codes_from`：
/// 跨来源并入会给码表造出「拼音码位有常用字」的假事实，反过来误滤同码的码表生僻字。
#[test]
fn dedup_merges_codes_within_source_only() {
    let mut same = vec![
        ct_cand_coded("的", "r", 900),
        ct_cand_coded("的", "rqto", 100),
    ];
    MixedEngine::sort_dedup_truncate(&mut same, 10, tctx("r", "zzzz"));
    assert_eq!(same.len(), 1);
    assert_eq!(same[0].merged_codes, vec!["rqto"], "同来源并码位");

    let mut cross = vec![
        ct_cand_coded("的", "r", 900),
        py_cand_coded("的", "de", 100),
    ];
    MixedEngine::sort_dedup_truncate(&mut cross, 10, tctx("r", "zzzz"));
    assert_eq!(cross.len(), 1);
    assert!(
        cross[0].merged_codes.is_empty(),
        "跨来源不并码位——并了会造出假的同码关系"
    );
}

/// 短语必须活过码表洪水：短语在档 1、码表前缀补全在档 2 ⇒ 短语权重再低也先活。
///
/// 本引擎目前收不到短语候选（`is_phrase` 无生产置位点，见 `truncation_tier`），
/// 故这条锁的是函数契约而非生产行为。
#[test]
fn phrase_survives_codetable_flood() {
    let mut cands: Vec<Candidate> = (0..20)
        .map(|i| ct_cand(&format!("码{i}"), 1000 - i))
        .collect();
    cands.push(Candidate {
        is_phrase: true,
        weight: 1,
        ..ct_cand("短语正文", 0)
    });
    MixedEngine::sort_dedup_truncate(&mut cands, 5, tctx("zzzz", "zzzz"));
    assert!(
        cands.iter().any(|c| c.is_phrase),
        "短语权重虽为 1，靠 +1M 档位活下来"
    );
}

/// 码表精确全码不被高频前缀补全挤掉（生产路径 `convert`）：精确 `code == input` 的权重
/// 可能远低于某条前缀补全，仍须活下来并排在前。
#[test]
fn exact_code_outranks_high_weight_prefix_completion() {
    let mut entries: Vec<(String, String, i32)> = vec![("aa".into(), "精确低频".into(), 1)];
    entries.extend((0..8).map(|i| (format!("aab{i}"), format!("补全{i}"), 9000 - i)));
    let refs: Vec<(&str, &str, i32)> = entries
        .iter()
        .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
        .collect();
    let e = MixedEngine::new(ct_engine(&refs, false), None, None, MixConfig::default());
    let r = e.convert("aa", 3).unwrap();
    assert_eq!(
        r.candidates[0].text, "精确低频",
        "精确全码权重仅 1，仍须压过权重 9000 的前缀补全"
    );
}

/// 码表精确/补全的分野由档位保证，不靠权重量级：直接调本函数时也成立。
#[test]
fn exact_beats_completion_at_function_level_not_only_via_boosts() {
    // 模拟码表子引擎的输出：精确在前（cmp_exact_first），但权重低得多、且不带加成。
    let mut cands = vec![
        ct_cand_coded("精确低频", "aa", 1),
        ct_cand_coded("补全", "aab", 9000),
    ];
    MixedEngine::sort_dedup_truncate(&mut cands, 10, tctx("aa", "zzzz"));
    assert_eq!(
        cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["精确低频", "补全"],
        "档 0 恒先于档 2，与权重取值范围无关"
    );
}

/// 档内保持子引擎原序，层级链不被 weight 抹掉。
///
/// 拼音候选全部落进档 3。拼音引擎的排序链（`cmp_match_layers().then(weight)`）里，层级键
/// （简拼 / 前缀补全 / 子短语 / 全拼降级）是布尔的、等价于惩罚 ∞，weight 表达不了；档内按
/// weight 重排会让高词频简拼反超低词频精确候选。稳定排序 + 只按档位排 ⇒ 同档保持子引擎原序。
#[test]
fn within_tier_keeps_sub_engine_order() {
    let abbrev = Candidate {
        is_abbrev: true,
        ..py_cand_coded("简拼高频", "n", 9000)
    };
    // 子引擎给出的序：精确在前（cmp_match_layers 首键 is_abbrev 判负）。
    let mut cands = vec![py_cand_coded("精确", "ni", 1), abbrev];
    MixedEngine::sort_dedup_truncate(&mut cands, 10, tctx("zzzz", "zzzz"));
    assert_eq!(
        cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["精确", "简拼高频"],
        "档内须保持子引擎原序：词频 9000 的简拼不得反超词频 1 的精确候选"
    );
}

/// 英文有保底席位：码表洪水下英文精确照样活到最后（至多 `english_max_count` 条）。
///
/// 英文精确与码表前缀补全同档（档 2，见 [`MixedEngine::truncation_tier`]），档内码表先入，
/// 单靠档位英文会被整片挤掉。`truncate_with_english_seats` 先腾座、后追加兑现保底，与码表
/// 方案的 `english_merge::merge` 同一手法。显示序仍由协调器决定，本档位只决定截断存活。
#[test]
fn english_survives_codetable_flood() {
    // 码表 1 条精确全码 + 20 条前缀补全（真实词频 9000 递减）。
    let mut entries: Vec<(String, String, i32)> = (0..20)
        .map(|i| (format!("hel{i}"), format!("码{i}"), 9000 - i))
        .collect();
    entries.push(("hel".into(), "好".into(), 1));
    let refs: Vec<(&str, &str, i32)> = entries
        .iter()
        .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
        .collect();
    let build = |ct: &[(&str, &str, i32)], english: Arc<dyn Engine>, max_count: usize| {
        MixedEngine::new(
            ct_engine(ct, false),
            None,
            Some(english),
            MixConfig {
                auto_commit_block_on_pinyin: false,
                english_complete_min_length: 3,
                english_max_count: max_count,
                ..Default::default()
            },
        )
    };
    let english_of = |r: &ConvertResult| {
        r.candidates
            .iter()
            .filter(|c| c.source == CandidateSource::English)
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
    };
    let words = || {
        english_engine(&[
            ("hel", "hel", 1),
            ("hello", "hello", 9999),
            ("help", "help", 50),
        ])
    };

    // 洪水下：完整单词 + 补全合计 2 条全数活下来，总数不超上限；码表精确首选仍在首位。
    let r = build(&refs, words(), 2).convert("hel", 5).unwrap();
    assert_eq!(r.candidates.len(), 5, "腾座后总数不得超出 max_candidates");
    assert_eq!(r.candidates[0].text, "好", "给英文腾座不得挤掉码表精确首选");
    assert_eq!(
        english_of(&r),
        vec!["hel", "hello"],
        "完整单词优先，再补前缀词"
    );

    // 上限 1：只保完整单词那一条。
    let r1 = build(&refs, words(), 1).convert("hel", 5).unwrap();
    assert_eq!(english_of(&r1), vec!["hel"]);
}

/// 产出多条 `source=Pinyin` 候选的假拼音引擎（`FakePinyin` 只给一条，测不了配额）。
struct FakePinyinMulti {
    n: usize,
}
impl Engine for FakePinyinMulti {
    fn convert(&self, input: &str, _max: usize) -> anyhow::Result<ConvertResult> {
        let candidates = (0..self.n)
            .map(|i| Candidate {
                text: format!("拼{i}"),
                code: input.to_string(),
                weight: 100 - i as i32,
                source: CandidateSource::Pinyin,
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
        EngineType::Pinyin
    }
}

#[test]
fn mixed_propagates_auto_commit_without_pinyin() {
    // 主码表唯一全码自动上屏；无次引擎 → 无拼音候选 → 放行。
    let primary = ct_engine(&[("aaaa", "工", 100)], true);
    let e = MixedEngine::new(primary, None, None, MixConfig::default());
    let r = e.convert("aaaa", 50).unwrap();
    assert!(r.should_commit, "无拼音候选时应放行全码上屏");
    assert_eq!(r.commit_text, "工");
}

#[test]
fn mixed_blocks_auto_commit_when_pinyin_present() {
    // 次引擎对同一输入也产出候选（模拟拼音命中）+ 守护①显式开 → 否决上屏。
    let primary = ct_engine(&[("aaaa", "工", 100)], true);
    let secondary = ct_engine(&[("aaaa", "啊啊", 50)], false);
    let e = MixedEngine::new(
        primary,
        Some(secondary),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: true,
            ..Default::default()
        },
    );
    let r = e.convert("aaaa", 50).unwrap();
    assert!(!r.should_commit, "有拼音候选且守护开时应否决全码上屏");
}

#[test]
fn mixed_allows_auto_commit_when_guard_off() {
    // 守护关 → 即便有拼音候选也放行。
    let primary = ct_engine(&[("aaaa", "工", 100)], true);
    let secondary = ct_engine(&[("aaaa", "啊啊", 50)], false);
    let e = MixedEngine::new(
        primary,
        Some(secondary),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            ..Default::default()
        },
    );
    let r = e.convert("aaaa", 50).unwrap();
    assert!(r.should_commit, "守护关时应放行");
    assert_eq!(r.commit_text, "工");
}

/// 构建开启顶码上屏的码表引擎（max_code_len=4）。
fn ct_engine_topcode(entries: &[(&str, &str, i32)]) -> Box<dyn Engine> {
    let mut d = CodetableDict::empty();
    for (i, (code, text, w)) in entries.iter().enumerate() {
        d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
    let opts = CommitOptions {
        top_code_commit: true,
        ..Default::default()
    };
    Box::new(CodeTableEngine::new(4, opts, Arc::new(dm)))
}

/// 可配假拼音引擎：`word`="" 表示无候选（has_pinyin=false）；`syllables` 同时驱动
/// is_whole_syllable_pinyin(=`syllables>0`) 与 completed_syllable_count(=`syllables`)——
/// 用于单测顶码/满码共用的拼音①②否决（含 ②(b) 单音节前缀保护）。
struct FakePinyin {
    word: &'static str,
    syllables: usize,
}
impl Engine for FakePinyin {
    fn convert(&self, input: &str, _max: usize) -> anyhow::Result<ConvertResult> {
        let candidates = if self.word.is_empty() {
            vec![]
        } else {
            vec![Candidate {
                text: self.word.to_string(),
                code: input.to_string(),
                weight: 1000,
                consumed_length: input.chars().count(),
                source: CandidateSource::Pinyin,
                ..Default::default()
            }]
        };
        Ok(ConvertResult {
            candidates,
            ..Default::default()
        })
    }
    fn reset(&self) {}
    fn engine_type(&self) -> EngineType {
        EngineType::Pinyin
    }
    fn is_whole_syllable_pinyin(&self, _prefix: &str) -> bool {
        self.syllables > 0
    }
    fn completed_syllable_count(&self, _prefix: &str) -> usize {
        self.syllables
    }
}

// ── 满码空码清空：拼音「后续可能性」守护 ──

/// 构建开启「满码空码清空」的码表引擎（max_code_len=4）。
fn ct_engine_clear(entries: &[(&str, &str, i32)]) -> Box<dyn Engine> {
    let mut d = CodetableDict::empty();
    for (i, (code, text, w)) in entries.iter().enumerate() {
        d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
    let opts = CommitOptions {
        clear_on_empty_max: true,
        ..Default::default()
    };
    Box::new(CodeTableEngine::new(4, opts, Arc::new(dm)))
}

/// 清空守护专用假拼音：恒无候选（has_pinyin=false，把协调器的候选非空复核排除在外），
/// 仅可配「整串是否为合法拼音前缀」——正是本守护要验的那一位。
struct FakePinyinPrefix {
    may_continue: bool,
}
impl Engine for FakePinyinPrefix {
    fn convert(&self, _input: &str, _max: usize) -> anyhow::Result<ConvertResult> {
        Ok(ConvertResult::default())
    }
    fn reset(&self) {}
    fn engine_type(&self) -> EngineType {
        EngineType::Pinyin
    }
    fn is_possible_pinyin_sequence(&self, _prefix: &str) -> bool {
        self.may_continue
    }
}

fn mixed_with_prefix_pinyin(may_continue: bool) -> MixedEngine {
    MixedEngine::new(
        ct_engine_clear(&[("aaaa", "工", 100)]),
        Some(Box::new(FakePinyinPrefix { may_continue })),
        None,
        MixConfig::default(),
    )
}

#[test]
fn clear_fires_when_pinyin_cannot_continue() {
    // 满码(4) 码表无候选无后继 + 拼音无候选且非合法前缀 → 清空。
    let r = mixed_with_prefix_pinyin(false).convert("qqqq", 50).unwrap();
    assert!(r.candidates.is_empty(), "前置：此输入确无候选");
    assert!(r.should_clear, "拼音无后续可能时应清空");
}

#[test]
fn clear_vetoed_when_pinyin_may_continue() {
    // 同上，但拼音判「还没打完」（zhon→zhong 中途态）→ 守护住，不得清空。
    // 合并候选为空，协调器的 `state.candidates.is_empty()` 复核挡不住——只能靠这一位。
    let r = mixed_with_prefix_pinyin(true).convert("zhon", 50).unwrap();
    assert!(r.candidates.is_empty(), "前置：此刻确无候选");
    assert!(
        !r.should_clear,
        "拼音仍可能有后续时不得清空，否则吞掉中途输入"
    );
}

/// 开关关 → 拼音「还没打完」不再拦清空。用户明确关掉「有拼音候选时否决上屏」即表态
/// 不要拼音干预，此时满码无候选就该清空（真机诉求：nunl 打满 4 码不清空）。
#[test]
fn clear_fires_when_pinyin_guard_disabled() {
    let e = MixedEngine::new(
        ct_engine_clear(&[("aaaa", "工", 100)]),
        Some(Box::new(FakePinyinPrefix { may_continue: true })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            ..Default::default()
        },
    );
    let r = e.convert("zhon", 50).unwrap();
    assert!(
        r.should_clear,
        "① 关时拼音后续可能性不得再拦清空（用户已表态不要拼音干预）"
    );
}

/// 开关关 + 拼音确有候选 → 同样清空。锁住「两道守护一起受开关支配」，
/// 只放开其中一道等于没放开（nunl 即便无候选也会被 may_continue 拦住）。
#[test]
fn clear_fires_when_guard_disabled_even_with_pinyin_candidates() {
    let e = MixedEngine::new(
        ct_engine_clear(&[("aaaa", "工", 100)]),
        Some(Box::new(FakePinyin {
            word: "嫩",
            syllables: 1,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            ..Default::default()
        },
    );
    let r = e.convert("nunl", 50).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "嫩"),
        "前置：拼音此刻确有候选"
    );
    assert!(r.should_clear, "① 关时有拼音候选也不得拦清空");
}

/// 反向锁：开关开（出厂默认）时两道守护照常拦住，勿把上面两例误改成无条件清空。
#[test]
fn clear_still_vetoed_when_guard_enabled() {
    let e = MixedEngine::new(
        ct_engine_clear(&[("aaaa", "工", 100)]),
        Some(Box::new(FakePinyinPrefix { may_continue: true })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: true,
            ..Default::default()
        },
    );
    assert!(
        !e.convert("zhon", 50).unwrap().should_clear,
        "① 开时中途态必须守住（zhon→zhong 不得被吞）"
    );
}

#[test]
fn overflow_never_clears() {
    // 超长（>max_code_len）有意不清空：已切入纯拼音语境，「码表满码无候选」前提不成立。
    let r = mixed_with_prefix_pinyin(false)
        .convert("qqqqq", 50)
        .unwrap();
    assert!(!r.should_clear, "overflow 分支不得产生清空");
}

// ── 顶码上屏：与满码全码自动上屏共用同一套拼音①②否决 ──

#[test]
fn topcode_vetoed_by_pinyin_candidate() {
    // ① auto_commit_block_on_pinyin 显式开（默认关）+ 整串有拼音候选 → 抑制顶码。
    let primary = ct_engine_topcode(&[("wang", "王", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "网",
            syllables: 0,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: true,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("wangb"),
        None,
        "① 开 + 有拼音候选应抑制顶码"
    );
}

#[test]
fn topcode_allowed_when_no_pinyin_candidate() {
    // 纯五笔溢出（整串无拼音候选）→ 顶码正常上屏（② 默认开也不拦）。
    // 默认下 ⓪ 亦为开，此例同时守着它的 `has_pinyin` 前提（详见
    // `topcode_allowed_when_overflow_has_no_pinyin`）。
    let primary = ct_engine_topcode(&[("aaaa", "工", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "",
            syllables: 0,
        })),
        None,
        MixConfig::default(),
    );
    assert_eq!(
        e.handle_top_code("aaaab"),
        Some(("工".to_string(), "b".to_string())),
        "无拼音候选时顶码应正常上屏"
    );
}

#[test]
fn topcode_vetoed_by_pinyin_word_when_block_on_pinyin_off() {
    // ① 关、② 开：整串是强拼音词（网吧）→ 仍抑制顶码。
    let primary = ct_engine_topcode(&[("wang", "王", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "网吧",
            syllables: 2,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            ..Default::default()
        },
    );
    assert_eq!(e.handle_top_code("wangba"), None, "② 强拼音词应抑制顶码");
}

#[test]
fn topcode_allowed_when_both_guards_off() {
    // ①② 都关：即便整串像拼音也顶码倒向五笔（王 + 余码 ba）。
    // ⓪ 须显式关以隔离变量——`MixConfig::default()` 的 pinyin_only_overflow 为 true，
    // 而本串有拼音候选，不关掉的话拦截来自 ⓪ 而非被测的 ①②。
    let primary = ct_engine_topcode(&[("wang", "王", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "网吧",
            syllables: 2,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            block_commit_on_pinyin_word: false,
            pinyin_only_overflow: false,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("wangba"),
        Some(("王".to_string(), "ba".to_string())),
        "①② 都关时顶码倒向五笔"
    );
}

#[test]
fn topcode_override_ignores_pinyin_veto() {
    // top_code_override_pinyin 开 = 顶码优先，无视拼音①②否决，强制倒向五笔。
    let primary = ct_engine_topcode(&[("wang", "王", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "网吧",
            syllables: 2,
        })),
        None,
        MixConfig {
            top_code_override_pinyin: true,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("wangba"),
        Some(("王".to_string(), "ba".to_string())),
        "顶码优先应无视拼音否决"
    );
}

#[test]
fn topcode_vetoed_by_single_syllable_prefix_when_block_on_pinyin_off() {
    // ① 关、② 开：前缀 "wang" 是单个完整拼音音节（中途打拼音词 wangba）→ 抑制顶码，
    // 即便 "wangb" 尚未构成完整拼音词。
    let primary = ct_engine_topcode(&[("wang", "王", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "网",
            syllables: 1,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("wangb"),
        None,
        "① 关 + ② 开：单音节前缀（中途打拼音词）应抑制顶码"
    );
}

#[test]
fn topcode_allowed_for_multi_syllable_prefix_when_block_on_pinyin_off() {
    // ① 关、② 开：前缀 "aipu"=ai+pu 是完整多音节单元、无强词 → 放行顶码倒向五笔（落实）。
    // ⓪ 须显式关以隔离变量（理由同 `topcode_allowed_when_both_guards_off`）。
    let primary = ct_engine_topcode(&[("aipu", "落实", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "矮",
            syllables: 2,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            pinyin_only_overflow: false,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("aipux"),
        Some(("落实".to_string(), "x".to_string())),
        "① 关 + ② 开：多音节前缀无强词应放行顶码"
    );
}

// ── ⓪ pinyin_only_overflow：超码长归拼音管，顶码不得抢 ──

#[test]
fn topcode_vetoed_by_pinyin_only_overflow() {
    // 与 `topcode_allowed_when_both_guards_off` 构成单一变量对照：同样的码表/假拼音/
    // 输入串、同样 ①② 都关，唯一差别是 ⓪ 开 → 拦截只可能来自 ⓪。
    let primary = ct_engine_topcode(&[("wang", "王", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "网吧",
            syllables: 2,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            block_commit_on_pinyin_word: false,
            pinyin_only_overflow: true,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("wangba"),
        None,
        "⓪ 开 + 有拼音候选：超码长归拼音管，顶码不得抢"
    );
}

#[test]
fn topcode_pinyin_only_overflow_protects_youyoud() {
    // 混输下打 `youyoud`（悠悠的），第 5 键 `o` 使缓冲 "youyo" 超 4 码，不得顶出 `youy` 的
    // 首选「变凉」+ 余码 `oud`。本例让 ①② 双双落空，只有 ⓪ 能救：
    // - ① 关（用户层 auto_commit_block_on_pinyin=false 覆盖了系统层的 true）；
    // - ②(b) 落空：前 4 码 "youy" = you + 残尾 y，不是完整音节（syllables=2 使
    //   completed_syllable_count != 1）；
    // - ②(a) 落空：整串 "youyo" 拼不出「≥2 汉字」的强词（word 只 1 字）。
    let primary = ct_engine_topcode(&[("youy", "变凉", 864)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "悠",
            syllables: 2,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            pinyin_only_overflow: true,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("youyo"),
        None,
        "①② 落空时 ⓪ 应兜住：超码长的拼音串不得被五笔顶码截胡"
    );
}

#[test]
fn topcode_allowed_when_overflow_has_no_pinyin() {
    // ⓪ 开但整串无拼音候选（纯五笔溢出）→ 必须放行顶码。一刀切禁顶会让用户卡死：
    // convert_overflow 此时只查拼音，同样交不出候选，那串既不上屏也没候选。
    let primary = ct_engine_topcode(&[("aaaa", "工", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "",
            syllables: 0,
        })),
        None,
        MixConfig {
            pinyin_only_overflow: true,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("aaaab"),
        Some(("工".to_string(), "b".to_string())),
        "⓪ 开但无拼音候选：纯五笔溢出应正常顶码"
    );
}

#[test]
fn topcode_override_beats_pinyin_only_overflow() {
    // top_code_override_pinyin 是总开关，压过 ⓪ 与 ①②。
    let primary = ct_engine_topcode(&[("wang", "王", 100)]);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "网吧",
            syllables: 2,
        })),
        None,
        MixConfig {
            pinyin_only_overflow: true,
            top_code_override_pinyin: true,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("wangba"),
        Some(("王".to_string(), "ba".to_string())),
        "顶码优先应无视 ⓪"
    );
}

// ── ③ auto_commit_block_on_english：有英文候选时顶码不得抢 ──

/// `gith` 在五笔主码表是「不算」，英文词库有 GitHub：打到第 5 键 `u` 时缓冲 `githu` 超 4 码，
/// 不得顶出「不算」+ 余码 `u`。secondary=None 是刻意的：一并锁住「③ 必须在 `Some(sec)` 块
/// 之外」——英文守护与拼音子引擎无关，纯码表 + 英文的混输同样该生效。
#[test]
fn topcode_vetoed_by_english_candidate() {
    let primary = ct_engine_topcode(&[("gith", "不算", 1822)]);
    let english = english_engine(&[("github", "GitHub", 100)]);
    let e = MixedEngine::new(
        primary,
        None,
        Some(english),
        MixConfig {
            auto_commit_block_on_english: true,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("githu"),
        None,
        "③ 开 + 有英文候选：顶码不得抢（且无拼音子引擎时也须生效）"
    );
}

#[test]
fn topcode_allowed_when_no_english_candidate() {
    // ③ 开但整串无英文候选 → 顶码正常（判据要求英文确有候选，不是开关一开就禁）。
    let primary = ct_engine_topcode(&[("gith", "不算", 1822)]);
    let english = english_engine(&[("hello", "hello", 50)]);
    let e = MixedEngine::new(
        primary,
        None,
        Some(english),
        MixConfig {
            auto_commit_block_on_english: true,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("githu"),
        Some(("不算".to_string(), "u".to_string())),
        "③ 开但无英文候选：顶码应正常"
    );
}

#[test]
fn topcode_english_guard_off_allows_topcode() {
    // ③ 关（出厂默认）→ 即便有英文候选也顶码，保持零回归。
    let primary = ct_engine_topcode(&[("gith", "不算", 1822)]);
    let english = english_engine(&[("github", "GitHub", 100)]);
    let e = MixedEngine::new(
        primary,
        None,
        Some(english),
        MixConfig {
            auto_commit_block_on_english: false,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("githu"),
        Some(("不算".to_string(), "u".to_string())),
        "③ 关时应保持旧行为"
    );
}

#[test]
fn topcode_override_beats_english_guard() {
    // top_code_override_pinyin 是顶码总开关，压过 ③（名字只提 pinyin 属历史局限）。
    let primary = ct_engine_topcode(&[("gith", "不算", 1822)]);
    let english = english_engine(&[("github", "GitHub", 100)]);
    let e = MixedEngine::new(
        primary,
        None,
        Some(english),
        MixConfig {
            auto_commit_block_on_english: true,
            top_code_override_pinyin: true,
            ..Default::default()
        },
    );
    assert_eq!(
        e.handle_top_code("githu"),
        Some(("不算".to_string(), "u".to_string())),
        "顶码优先应无视 ③"
    );
}

#[test]
fn mixed_recheck_auto_commit_after_filter() {
    // 引擎按未过滤候选（含生僻同码字）判不唯一而否决满码上屏；智能过滤后只剩唯一精确
    // 全码码表候选 → 复评据显示候选放行（bug: 显示只剩一个却不上屏）。
    let primary = ct_engine(&[("hhnu", "X", 100), ("hhnu", "愳", 1)], true);
    let e = MixedEngine::new(primary, None, None, MixConfig::default());
    // 原始转换：两个精确 hhnu → 不唯一，引擎不给上屏意向。
    let r = e.convert("hhnu", 50).unwrap();
    assert!(!r.should_commit, "两个精确同码候选时引擎不自动上屏");
    // 模拟智能过滤后仅剩一个码表精确全码候选 → 复评放行。
    let filtered = vec![Candidate {
        text: "X".into(),
        code: "hhnu".into(),
        source: CandidateSource::CodeTable,
        ..Default::default()
    }];
    assert_eq!(
        e.recheck_auto_commit("hhnu", &filtered),
        Some("X".to_string()),
        "过滤后唯一精确全码应复评放行"
    );
    // 拼音/英文来源不参与满码自动上屏：即便过滤后剩一个拼音候选也不放行。
    let py_only = vec![Candidate {
        text: "往".into(),
        code: "hhnu".into(),
        source: CandidateSource::Pinyin,
        ..Default::default()
    }];
    assert_eq!(e.recheck_auto_commit("hhnu", &py_only), None);
}

#[test]
fn mixed_blocks_auto_commit_when_pinyin_word() {
    // 主码表 mama 唯一全码本会自动上屏；① 关但整串是强拼音词 妈妈（②）→ 否决满码上屏。
    let primary = ct_engine(&[("mama", "X", 100)], true);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "妈妈",
            syllables: 2,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            ..Default::default()
        },
    );
    let r = e.convert("mama", 50).unwrap();
    assert!(!r.should_commit, "整串是强拼音词时应否决满码上屏");
}

#[test]
fn mixed_allows_auto_commit_when_pinyin_word_guard_off() {
    // ①② 都关 → 即便整串是强拼音词也放行满码上屏（零回归）。
    let primary = ct_engine(&[("mama", "X", 100)], true);
    let e = MixedEngine::new(
        primary,
        Some(Box::new(FakePinyin {
            word: "妈妈",
            syllables: 2,
        })),
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            block_commit_on_pinyin_word: false,
            ..Default::default()
        },
    );
    let r = e.convert("mama", 50).unwrap();
    assert!(r.should_commit, "①② 都关时应放行满码上屏");
    assert_eq!(r.commit_text, "X");
}

#[test]
fn source_hint_marks_pinyin_candidates() {
    let mut cands = vec![
        Candidate {
            text: "工".into(),
            source: CandidateSource::CodeTable,
            ..Default::default()
        },
        Candidate {
            text: "你好".into(),
            source: CandidateSource::Pinyin,
            ..Default::default()
        },
        Candidate {
            text: "拟".into(),
            source: CandidateSource::Pinyin,
            comment: "ni".into(),
            ..Default::default()
        },
    ];
    MixedEngine::add_source_hints(&mut cands);
    assert_eq!(cands[0].comment, "", "码表候选不标记");
    assert_eq!(cands[1].comment, "拼");
    assert_eq!(cands[2].comment, "拼|ni", "已有 comment 时前置拼接");
}

/// 内存英文引擎（EnglishEngine 包码表；code=小写英文词，前缀匹配）。
fn english_engine(entries: &[(&str, &str, i32)]) -> Arc<dyn Engine> {
    let mut d = CodetableDict::empty();
    for (i, (code, text, w)) in entries.iter().enumerate() {
        d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "en")));
    let ct = CodeTableEngine::new(32, CommitOptions::default(), Arc::new(dm));
    Arc::new(crate::english::EnglishEngine::new(ct))
}

#[test]
fn mixed_mixes_english_when_enabled() {
    // enable_english（english=Some）：混输主路径应混入英文词库候选。
    // 判据是精确命中：显示侧走 `english_for_display`，前缀扩展不入列。
    // 词必须 ≤ 4 字符：`ct_engine` 的 max_code_length=4，更长的输入走 `convert_overflow`，
    // 而它在 `secondary=None` 时早退只查码表，英文不参与。
    // 码表侧给 `help` / `hel` 都备好中文候选：中文侧为空时英文改给前缀补全
    // （见 `english_merge::select`），那是另一条路。
    let primary = ct_engine(&[("hao", "好", 100), ("help", "帮", 100)], false);
    let english = english_engine(&[("help", "help", 50), ("helpful", "helpful", 40)]);
    let e = MixedEngine::new(
        primary,
        None,
        Some(english),
        MixConfig {
            auto_commit_block_on_pinyin: false,
            ..Default::default()
        },
    );
    let r = e.convert("help", 50).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "help"),
        "开启英文时混输应含英文候选 help，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    assert!(
        r.candidates
            .iter()
            .filter(|c| c.text == "help")
            .all(|c| c.source == CandidateSource::English && !c.is_exact_code),
        "英文候选须标记 English，且不得带着码表域的 is_exact_code"
    );
    assert!(
        !r.candidates.iter().any(|c| c.text == "helpful"),
        "同一次查询里的前缀扩展 helpful 不得入列"
    );

    // 前缀扩展不入列：打 `hel` 一条英文都不该有。
    let r_prefix = e.convert("hel", 50).unwrap();
    assert!(
        !r_prefix
            .candidates
            .iter()
            .any(|c| c.source == CandidateSource::English),
        "前缀扩展进了列表：{:?}",
        r_prefix
            .candidates
            .iter()
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
}

#[test]
fn mixed_no_english_when_disabled() {
    // english=None：不混入英文候选（零回归）。
    let primary = ct_engine(&[("hao", "好", 100)], false);
    let e = MixedEngine::new(
        primary,
        None,
        None,
        MixConfig {
            auto_commit_block_on_pinyin: false,
            ..Default::default()
        },
    );
    let r = e.convert("hel", 50).unwrap();
    assert!(
        !r.candidates.iter().any(|c| c.text == "hello"),
        "关闭英文时不应有英文候选"
    );
}

#[test]
fn mixed_english_respects_min_length() {
    // min_english_length=3：2 字符以内不查英文，3 字符起才混入。
    // 短词 `he` 在词库里是精确命中，却仍须被长度闸门挡掉，这样测的才是闸门本身。
    // 码表侧 `he` / `hel` 都有中文候选：中文侧为空时长度门槛另按 2 放宽（前缀补全那条路）。
    let primary = ct_engine(&[("x", "叉", 100), ("hela", "乐", 100)], false);
    let english = english_engine(&[("he", "he", 60), ("hel", "hel", 50)]);
    let e = MixedEngine::new(
        primary,
        None,
        Some(english),
        MixConfig {
            auto_commit_block_on_pinyin: false,
            min_english_length: 3,
            ..Default::default()
        },
    );
    let r2 = e.convert("he", 50).unwrap();
    assert!(
        !r2.candidates.iter().any(|c| c.text == "he"),
        "2 字符（< min 3）不应出英文候选，哪怕它是精确命中"
    );
    let r3 = e.convert("hel", 50).unwrap();
    assert!(
        r3.candidates.iter().any(|c| c.text == "hel"),
        "3 字符（>= min 3）应出英文候选，实际: {:?}",
        r3.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

#[test]
fn mixed_blocks_auto_commit_when_english_present() {
    // 主码表唯一全码本会自动上屏；开英文守护 + 有英文候选 → 否决（留给用户选英文）。
    let primary = ct_engine(&[("good", "工", 100)], true);
    let english = english_engine(&[("good", "good", 50), ("goodbye", "goodbye", 40)]);
    let e = MixedEngine::new(
        primary,
        None,
        Some(english),
        MixConfig {
            auto_commit_block_on_pinyin: false,
            auto_commit_block_on_english: true,
            ..Default::default()
        },
    );
    let r = e.convert("good", 50).unwrap();
    assert!(!r.should_commit, "开英文守护且有英文候选时应否决全码上屏");
    assert!(
        r.candidates.iter().any(|c| c.text == "good"),
        "应含英文候选 good"
    );
}

#[test]
fn mixed_allows_auto_commit_when_english_guard_off() {
    // 两级英文守护都关 → 即便整串是英文单词也放行全码上屏。
    let build = |word_level: bool| {
        MixedEngine::new(
            ct_engine(&[("good", "工", 100)], true),
            None,
            Some(english_engine(&[("good", "good", 50)])),
            MixConfig {
                auto_commit_block_on_pinyin: false,
                block_commit_on_english_word: word_level,
                ..Default::default()
            },
        )
    };
    let r = build(false).convert("good", 50).unwrap();
    assert!(r.should_commit, "英文守护关时应放行全码上屏");
    assert_eq!(r.commit_text, "工");
    // 成词级出厂开：整串 `good` 是英文单词 ⇒ 否决（前缀级仍关着）。
    assert!(!MixConfig::default().auto_commit_block_on_english);
    assert!(!build(true).convert("good", 50).unwrap().should_commit);
}

/// 满码空码清空同样受英文守护：整串是英文单词（码表无匹配）时不清空；两级都关才清。
#[test]
fn mixed_word_level_vetoes_clear_on_empty_max() {
    let build = |word_level: bool| {
        let mut d = CodetableDict::empty();
        d.merge_single("aaaa".into(), "工".into(), 100, 0);
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
        let opts = CommitOptions {
            clear_on_empty_max: true,
            ..Default::default()
        };
        MixedEngine::new(
            Box::new(CodeTableEngine::new(4, opts, Arc::new(dm))),
            None,
            Some(english_engine(&[("word", "word", 50)])),
            MixConfig {
                auto_commit_block_on_pinyin: false,
                block_commit_on_english_word: word_level,
                ..Default::default()
            },
        )
    };
    assert!(
        build(false).convert("word", 50).unwrap().should_clear,
        "前提：两级都关时 word（码表满码无匹配）清空"
    );
    assert!(
        !build(true).convert("word", 50).unwrap().should_clear,
        "成词级：整串是英文单词，不得清空"
    );
}

/// 成词级只管整词：英文词的前缀（`goo`→good）在前缀级关着时不拦满码。
#[test]
fn mixed_word_level_ignores_prefix() {
    let e = MixedEngine::new(
        ct_engine(&[("gooa", "工", 100)], true),
        None,
        Some(english_engine(&[("gooab", "gooab", 50)])),
        MixConfig {
            auto_commit_block_on_pinyin: false,
            ..Default::default()
        },
    );
    assert!(
        e.convert("gooa", 50).unwrap().should_commit,
        "gooa 只是英文词前缀，成词级不该否决"
    );
}

/// 带通配键 `z` 的内存码表（码长 4）。
fn ct_wildcard(entries: &[(&str, &str, i32)]) -> Box<dyn Engine> {
    ct_wildcard_with(entries, false)
}

fn ct_wildcard_with(entries: &[(&str, &str, i32)], single_only: bool) -> Box<dyn Engine> {
    let mut d = CodetableDict::empty();
    for (i, (code, text, w)) in entries.iter().enumerate() {
        d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
    Box::new(CodeTableEngine::new(
        4,
        CommitOptions {
            wildcard: Some('z'),
            reverse_key: Some('z'),
            wildcard_single_only: single_only,
            ..Default::default()
        },
        Arc::new(dm),
    ))
}

/// 按输入查表的假拼音（`FakePinyin` 不看输入、恒出同一条，测不了「字面串走拼音」）。
struct FakePinyinTable {
    entries: Vec<(&'static str, &'static str)>,
}
impl Engine for FakePinyinTable {
    fn convert(&self, input: &str, _max: usize) -> anyhow::Result<ConvertResult> {
        let candidates = self
            .entries
            .iter()
            .filter(|(k, _)| *k == input)
            .enumerate()
            .map(|(i, (_, t))| Candidate {
                text: t.to_string(),
                code: input.to_string(),
                weight: 1000 - i as i32,
                consumed_length: input.len(),
                source: CandidateSource::Pinyin,
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
        EngineType::Pinyin
    }
}

fn mixed_wc(ct: &[(&str, &str, i32)], py: Vec<(&'static str, &'static str)>) -> MixedEngine {
    MixedEngine::new(
        ct_wildcard(ct),
        Some(Box::new(FakePinyinTable { entries: py })),
        None,
        MixConfig::default(),
    )
}

fn slot(p: &str) -> String {
    p.replace('?', &wind_dict::WILDCARD_SLOT.to_string())
}

/// spec §10：字面混输 ⊕ 主码表通配。引擎次序 = 存活优先级：通配等长 → 字面 → 通配更长。
/// 通配结果不给上屏 / 清空意向；组合区是原始缓冲。
#[test]
fn wildcard_merges_literal_mixed_with_primary_pattern() {
    let e = mixed_wc(
        &[
            ("abi", "蒸", 9000),
            ("adi", "藉", 8000),
            ("abic", "蒸笼", 500),
        ],
        vec![("azi", "阿紫")],
    );
    let r = e
        .convert_wildcard("azi", &slot("a?i"), 50)
        .expect("有拼音子引擎时也接通配请求");
    let texts: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, ["蒸", "藉", "阿紫", "蒸笼"]);
    assert!(!r.should_commit && r.commit_text.is_empty() && !r.should_clear);
    assert_eq!(r.preedit_display, "azi");
    assert!(
        r.candidates
            .iter()
            .filter(|c| c.source == CandidateSource::CodeTable)
            .all(|c| c.is_wildcard),
        "码表侧全部来自通配"
    );
}

/// 码表之间按 (text, code) 去重（同字不同码各留），拼音与任一码表候选同字即丢——包括与
/// 排在字面之后的通配更长补全同字。
#[test]
fn wildcard_merge_drops_pinyin_same_text_keeps_codetable_codes() {
    let e = mixed_wc(
        &[
            ("abi", "蒸", 9000),
            ("aci", "蒸", 100),
            ("abic", "蒸笼", 500),
        ],
        vec![("azi", "蒸"), ("azi", "蒸笼"), ("azi", "阿紫")],
    );
    let r = e.convert_wildcard("azi", &slot("a?i"), 50).unwrap();
    let pairs: Vec<(&str, &str, CandidateSource)> = r
        .candidates
        .iter()
        .map(|c| (c.text.as_str(), c.code.as_str(), c.source))
        .collect();
    let zheng: Vec<&str> = pairs.iter().filter(|p| p.0 == "蒸").map(|p| p.1).collect();
    assert_eq!(zheng, ["abi", "aci"], "同字不同码各留，拼音「蒸」被丢");
    assert!(
        pairs
            .iter()
            .filter(|p| p.0 == "蒸笼")
            .all(|p| p.2 == CandidateSource::CodeTable),
        "与通配更长补全同字的拼音也被丢：{pairs:?}"
    );
    assert!(
        pairs
            .iter()
            .any(|p| p.0 == "阿紫" && p.2 == CandidateSource::Pinyin)
    );
}

/// 通配结果全排在字面之前，截断后拼音仍有保底席位。
#[test]
fn wildcard_merge_keeps_pinyin_quota() {
    let e = mixed_wc(
        &[
            ("aai", "甲", 900),
            ("abi", "乙", 800),
            ("aci", "丙", 700),
            ("adi", "丁", 600),
            ("aei", "戊", 500),
            ("afi", "己", 400),
        ],
        vec![("azi", "阿紫"), ("azi", "阿姊")],
    );
    let r = e.convert_wildcard("azi", &slot("a?i"), 5).unwrap();
    assert_eq!(r.candidates.len(), 5);
    let py = r
        .candidates
        .iter()
        .filter(|c| c.source == CandidateSource::Pinyin)
        .count();
    assert!(py >= 1, "5 / PINYIN_QUOTA_DIVISOR = 1 席保底，实际 {py}");
}

/// 通配码长取主码表的，但 `max_code_length` 不代理（它还决定短语自动上屏门槛）。
/// 对照：无拼音子引擎时只代理主码表、不混拼音。
#[test]
fn wildcard_code_length_is_primary_max_without_proxying_max_code_length() {
    let mixed = mixed_wc(&[("ab", "甲", 10)], vec![]);
    assert_eq!(mixed.wildcard_code_length(), 4);
    assert_eq!(
        Engine::max_code_length(&mixed),
        0,
        "混输 max_code_length 维持 0"
    );
    assert!(mixed.wildcard_mixes_pinyin());
    assert_eq!(
        mixed.wildcard_key(),
        Some('z'),
        "混输代理主码表的通配键（spec §10）"
    );

    let solo = MixedEngine::new(
        ct_wildcard(&[("ab", "甲", 10)]),
        None,
        None,
        MixConfig::default(),
    );
    assert!(!solo.wildcard_mixes_pinyin());
    assert_eq!(solo.wildcard_code_length(), 4);
    assert_eq!(solo.wildcard_key(), Some('z'), "对照：无拼音时代理主码表");
    let r = solo
        .convert_wildcard("az", &slot("a?"), 10)
        .expect("主码表开了通配");
    let texts: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, ["甲"]);
}

/// spec §11：混输下主码表通配按 `max_candidates` 查，不再被主码表内部的固定 100 截断。
#[test]
fn wildcard_primary_follows_max_candidates() {
    let many: Vec<(String, String, i32)> = (0..150u32)
        .map(|i| {
            let c1 = (b'a' + (i / 26) as u8) as char;
            let c2 = (b'a' + (i % 26) as u8) as char;
            (format!("q{c1}{c2}"), format!("字{i}"), 1)
        })
        .collect();
    let refs: Vec<(&str, &str, i32)> = many
        .iter()
        .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
        .collect();
    let e = mixed_wc(&refs, vec![("qzz", "阿紫")]);
    let r = e.convert_wildcard("qzz", &slot("q??"), 300).unwrap();
    let ct = r
        .candidates
        .iter()
        .filter(|c| c.source == CandidateSource::CodeTable)
        .count();
    assert_eq!(ct, 150, "主码表通配全部进来（旧实现截在 100）");
    assert!(
        r.candidates.iter().any(|c| c.text == "阿紫"),
        "字面半边照常合并"
    );
}

/// reverse-mode spec §2：混输下仅单字只作用于通配那一侧，字面混输（拼音）的多字词照出。
#[test]
fn wildcard_single_only_spares_literal_side() {
    let e = MixedEngine::new(
        ct_wildcard_with(&[("qa", "甲", 10), ("qb", "乙丙", 20)], true),
        Some(Box::new(FakePinyinTable {
            entries: vec![("qz", "阿紫")],
        })),
        None,
        MixConfig::default(),
    );
    let r = e.convert_wildcard("qz", &slot("q?"), 50).unwrap();
    let texts: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
    assert!(texts.contains(&"甲"), "{texts:?}");
    assert!(!texts.contains(&"乙丙"), "通配侧的词组被滤：{texts:?}");
    assert!(texts.contains(&"阿紫"), "拼音侧多字词不受影响：{texts:?}");
}
/// spec §3.2：反查模式在混输下只查主码表，拼音不参与。
#[test]
fn mixed_convert_reverse_is_primary_only() {
    let e = mixed_wc(&[("qa", "甲", 10)], vec![("qz", "阿紫")]);
    assert_eq!(e.reverse_wildcard_key(), Some('z'));
    let r = e.convert_reverse("qz", &slot("q?"), 50).unwrap();
    assert!(
        r.candidates
            .iter()
            .all(|c| c.source == CandidateSource::CodeTable),
        "{:?}",
        r.candidates
    );
    assert!(r.candidates.iter().any(|c| c.text == "甲"));
}
