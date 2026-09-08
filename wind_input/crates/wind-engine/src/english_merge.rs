//! 英文候选混入（**引擎无关**）
//!
//! 把「查一次英文词库，把结果混进另一个引擎的候选列表」独立出来，供**单一来源引擎**
//! （纯拼音方案、纯码表方案如五笔）复用。接线点在 [`crate::manager::EngineManager`]——
//! 那里是 `convert` / `recheck_auto_commit` / `handle_top_code` **三条通路的共同收口**，
//! 一处接线即覆盖全部引擎类型，无需逐引擎改造，也无需包一层转发全部 trait 方法的装饰器
//! （`Engine` 有二十来个带默认实现的方法，漏转发一个就是静默改行为）。
//!
//! ## 与 `MixedEngine` 那套的分工——不是重复实现
//!
//! 混输的英文走 `MixedEngine::english_candidates` + `merge_sort_dedup`，其合并策略与
//! `MixedEngine::truncation_tier` 的**三方档位仲裁**（码表精确 / 短语 / 英文精确+码表前缀 /
//! 其余）耦合，而那套档位的语义又与 `convert_overflow` 的截断归属绑在一起。本模块服务的
//! 场景只有**一路**基础候选，三方仲裁无从谈起——故另写一份简单的，而不是把混输那套拆通用。
//!
//! ⇒ 混输方案**两份都不读**（见 `manager::EngineManager::english_merge_cfg` 的分流），
//! 否则两套英文各混一遍，档位与配额双重失真。
//!
//! ## 配置按引擎分两份，本模块只认折叠后的形态
//!
//! `[schema.codetable.english_merge]`（三项，且经方案级 `[engine.codetable.english_merge]`
//! 折叠）与 `[schema.pinyin.english_merge]`（两项，无 `block_commit`）是**两件独立的事**
//! ——理由见 `wind_config::CodetableEnglishMerge` 的文档。管理器把它们折叠成
//! [`Effective`] 再交给本模块，故这里的函数不关心取值来自哪一份。
//!
//! ## ★ 英文必须有保底席位，否则开关等于没做
//!
//! 混输里英文**没有任何配额**——这是已记录的已知缺口（见
//! `MixedEngine` 的 `english_has_no_quota_under_codetable_flood` 测试）：英文精确虽在档 2，
//! 但档内按合并顺序、码表先入，码表洪水下英文被整片挤掉。
//!
//! 那在混输里影响有限（码表候选数有限），在**纯拼音下却是致命的**：拼音一次就能吐满
//! `max_candidates`（`ConvertOptions::admit` 的实测表里 `ni` 装满 100 条），英文若追加在
//! 尾部再截断，会被**全部**丢掉——用户打开开关后什么也看不到，且日志、设置页均无痕迹。
//!
//! 故本模块反过来做：**取回来的每一条都保证活到最后**（[`merge`] 先腾座后追加）。
//!
//! 「英文条数」本身由 [`lookup`] 的**精确命中收窄**限住（至多 [`EXACT_MAX`] 条），
//! 不再需要按 `max_candidates` 比例分席——用户要的是「打 hello 能看见 hello」，
//! 不是「hello 的二十个前缀词把候选页占满」。
//!
//! ## 位次不在本模块决定
//!
//! 本模块只管「查哪些、谁活下来」。英文候选**排第几**由协调器
//! `place_english_after_common_exact` 在所有排序（含自动调频）跑完之后统一定位：
//! 排在「中文常用精确解」之后、其余候选之前。放在这里做不到——引擎看不见协调器随后并入的
//! 短语候选，也看不见调频的位置提升。

use crate::engine::Engine;
use wind_candidate::Candidate;

/// 扫描窗口：向英文引擎取多少条原始命中，用来从中挑出精确匹配。
///
/// 英文引擎是前缀匹配（`hen` → hen / hence / Henderson…），且**精确整串恒居首**
/// （`CodeTableEngine` 的 `整串精确匹配应居首` 守门断言）。取 8 条是给「同码多形态」
/// （`she` / `She`）留余量，不是给前缀留的——前缀在下面被整片丢弃。
const EXACT_SCAN: usize = 8;

/// 精确命中至多保留几条。
///
/// 同一个码可能对应多条词条（大小写形态、`she` / `she'd` 那种撇号变体在**本表里不同码**
/// 故不在此列）。2 条足够，且候选面上英文再多也没有意义。
const EXACT_MAX: usize = 2;

/// 最小触发长度的回退值（配置为 0 时）。与 `manager::build_engine` 给
/// `schema.mix.min_english_length` 的回退**同值**，两处口径一致。
const DEFAULT_MIN_LENGTH: usize = 3;

/// 本次转换**生效**的英文混入参数。
///
/// 来源有两个且字段不同——码表侧是 `schema.codetable.english_merge`（三项，且经方案级
/// `[engine.codetable.english_merge]` 折叠），拼音侧是 `schema.pinyin.english_merge`
/// （两项，无 `block_commit`）。本结构是它们折叠后的**共同形态**，让下游三条通路只认一种。
///
/// ⚠️ 拼音侧构造时 `block_commit` 恒为 `false`：那一项否决的是满码自动上屏 / 顶码上屏，
/// 拼音方案两者都没有。写成 `false` 不是"默认关"，是"这条路上没有可否决的动作"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effective {
    pub enable: bool,
    pub min_length: usize,
    pub block_commit: bool,
}

/// 配置值 0 视作「用回退值」，口径同 `schema.mix.min_english_length`。
pub fn min_length_or_default(configured: usize) -> usize {
    if configured > 0 {
        configured
    } else {
        DEFAULT_MIN_LENGTH
    }
}

/// 查英文词库，**只取精确命中**（`code == 小写化输入`）。短输入不查。
///
/// ## ★ 为什么丢弃前缀扩展
///
/// 英文引擎是前缀匹配，`hen` 会带出 hence / henceforth / Henderson / Hendrix，
/// `github` 会带出 GitHub Pages / GitHub Copilot CLI 等 7 条长词组（实测）。这些条目在
/// 本功能里**没有任何使用场景**：用户在打中文时它们是纯噪音；用户真想打某个英文词时，
/// 正确操作是继续敲完（`githu` → `github`），而不是从一屏 GitHub 开头的词组里挑。
///
/// 丢掉前缀后英文候选天然只剩 1~2 条，**配额机制随之取消**（原先按
/// `max_candidates/10` 封顶 5 席的 `seats_for` 已删）——不是放宽，是那个问题不存在了：
/// 当初要配额正是因为前缀扩展会成片涌入。
///
/// ## ⚠️ 与 [`has_any`] 的判据**刻意不同**
///
/// 本函数管「显示哪些」，`has_any` 管「要不要否决上屏」。后者必须仍看**前缀**：
/// 用户打到 `gith` 时精确命中还不存在，若否决判据也收窄成精确，五笔满码就会在第 4 键
/// 把中文顶上屏，`github` 永远敲不完——那正是 `block_commit` 存在的理由。
/// 两者收窄一处、保留一处，是这次改动里最容易被后人「顺手统一」掉的地方。
///
/// ## 清 `is_exact_code`
///
/// 该标志是**码表域**的（语义为「码 == 输入的完全匹配」，服务于码表精确档），而拼音引擎
/// 从不设它。把它跨来源带进拼音候选列表，会让英文在协调器的 `cmp_exact_first`
/// （位置在 `by_weight` **之前**）无条件压过全部中文——真机现象即「打 hen，权重 500 的
/// 英文排在权重 86016 的『很』之前」。英文不是赢了权重，是赢在一个拼音压根没参赛的键上。
///
/// 输入小写化以匹配英文词库（`EnglishEngine` 的 code 列已小写化）。查询失败静默退化为空
/// ——英文只是捎带的增强，不该让它的故障影响主候选（同 `Engine::convert` 永不 panic 的约定）。
pub fn lookup(english: &dyn Engine, input: &str, min_length: usize) -> Vec<Candidate> {
    if input.chars().count() < min_length_or_default(min_length) {
        return Vec::new();
    }
    let lower = input.to_lowercase();
    let Ok(r) = english.convert(&lower, EXACT_SCAN) else {
        return Vec::new();
    };
    r.candidates
        .into_iter()
        .filter(|c| c.code == lower)
        .map(|mut c| {
            // 见函数文档「清 is_exact_code」。
            c.is_exact_code = false;
            c
        })
        .take(EXACT_MAX)
        .collect()
}

/// 是否存在英文候选。供**上屏否决**判据用（`wind-engine/AGENTS.md`：否决必须叠
/// 「对方确有候选」，只看开关就禁上屏会把「英文被顶掉」修成「谁都上不了屏」）。
///
/// ⚠️ **仍按前缀判**，不复用 [`lookup`] 的精确收窄——理由见该函数文档「与 has_any 的判据
/// 刻意不同」。只问有无，故只取 1 条。
pub fn has_any(english: &dyn Engine, input: &str, min_length: usize) -> bool {
    if input.chars().count() < min_length_or_default(min_length) {
        return false;
    }
    let lower = input.to_lowercase();
    match english.convert(&lower, 1) {
        Ok(r) => !r.candidates.is_empty(),
        Err(_) => false,
    }
}

/// 把英文候选混入 `base`，保证它们活过截断。
///
/// 次序**不做保证**：本函数只决定「谁活下来」，最终显示序由协调器
/// `candidate_display_order` 无条件重排全部候选决定（candidate-sorting-rules.md §6）。
/// 同 `MixedEngine::truncate_with_pinyin_quota` 的分工。
pub fn merge(base: &mut Vec<Candidate>, english: Vec<Candidate>, max_candidates: usize) {
    if english.is_empty() || max_candidates == 0 {
        return;
    }
    // 同文本时保留基础候选（它才是本方案的正主），把英文那条的码位并进去。
    // `absorb_codes_from` 自行挡掉跨来源合并（两套编码不同域），故此处无需再判 source。
    let mut kept: Vec<Candidate> = Vec::with_capacity(english.len());
    for c in english {
        match base.iter_mut().find(|b| b.text == c.text) {
            Some(existing) => existing.absorb_codes_from(&c),
            None => kept.push(c),
        }
    }
    if kept.is_empty() {
        return;
    }
    // ★ **先腾座、后追加**。基础候选通常已经恰好装满 `max_candidates`，
    // 「先 extend 再 truncate」会把英文原样截回去——那正是混输里英文无保底的现场。
    if base.len() + kept.len() > max_candidates {
        base.truncate(max_candidates.saturating_sub(kept.len()));
    }
    base.extend(kept);
}

#[cfg(test)]
mod tests {
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

    /// ★ 本模块存在的理由：拼音把配额吐满时，英文仍须在场。
    ///
    /// 正向对照（`calm`）必须先立——否则「洪水下在场」可能只是因为假引擎压根没产出，
    /// 断言变成空转（对照 `MixedEngine` 那条同名教训）。
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

    /// ★ 只收精确命中：前缀扩展一条都不进列表。
    ///
    /// 真机现象：全拼打 `hen` 混进 hen / hence / henceforth / Henderson / Hendrix 五条，
    /// 后四条纯噪音；混输打 `github` 混进 8 条 GitHub 开头的长词组。
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

    /// ★ 上屏否决仍按**前缀**判，不随 `lookup` 一起收窄成精确。
    ///
    /// 收窄了的话，五笔打到第 4 键 `gith` 时精确命中还不存在 ⇒ 否决不生效 ⇒ 满码把中文
    /// 顶上屏 ⇒ `github` 永远敲不完。这正是 `block_commit` 要防的事。
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
}
