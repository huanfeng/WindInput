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
//! ⇒ 混输方案**不读** `[schema.english_merge]`（见 `wind_config::EnglishMergeGlobal`），
//! 否则两套英文各混一遍，档位与配额双重失真。
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
//! 故本模块反过来做：**取数时就按席位数取**（[`seats_for`]），取回来的每一条都保证活到
//! 最后（[`merge`] 先腾座后追加）。代价是英文条数有上限，而这正是想要的——用户要的是
//! 「打 hello 能看见 hello」，不是「hello 的二十个前缀词把候选页占满」。

use crate::engine::Engine;
use wind_candidate::Candidate;

/// 英文候选至多占 `max_candidates / ENGLISH_QUOTA_DIVISOR` 席。
///
/// 比混输给拼音的保底（`PINYIN_QUOTA_DIVISOR = 5`，即 20%）小一半：拼音保底是为了让**整类
/// 来源**在码表洪水下不至于全军覆没，可能需要几十条；英文要的只是「用户正在打的那个词
/// 及其少数几个补全」，多给的席位全是噪音。
const ENGLISH_QUOTA_DIVISOR: usize = 10;

/// 英文席位硬上限。`max_candidates` 在协调器侧可放到数百（生僻字模式会加大重取），
/// 光靠比例分母会让英文席位跟着膨胀，而英文的有效候选数并不随之增长。
const ENGLISH_MAX_SEATS: usize = 5;

/// 最小触发长度的回退值（配置为 0 时）。与 `manager::build_engine` 给
/// `schema.mix.min_english_length` 的回退**同值**，两处口径一致。
const DEFAULT_MIN_LENGTH: usize = 3;

/// 本次转换分给英文的席位数。恒 ≥ 1：`max_candidates` 很小时（协调器某些探针只要几条）
/// 比例算下来会是 0，那等于开关静默失效。
pub fn seats_for(max_candidates: usize) -> usize {
    (max_candidates / ENGLISH_QUOTA_DIVISOR).clamp(1, ENGLISH_MAX_SEATS)
}

/// 配置值 0 视作「用回退值」，口径同 `schema.mix.min_english_length`。
pub fn min_length_or_default(configured: usize) -> usize {
    if configured > 0 {
        configured
    } else {
        DEFAULT_MIN_LENGTH
    }
}

/// 查英文词库。短输入不查（避免两三个字母就刷一屏前缀词）。
///
/// 输入小写化以匹配英文词库（`EnglishEngine` 的 code 列已小写化）。查询失败静默退化为空
/// ——英文只是捎带的增强，不该让它的故障影响主候选（同 `Engine::convert` 永不 panic 的约定）。
pub fn lookup(
    english: &dyn Engine,
    input: &str,
    min_length: usize,
    seats: usize,
) -> Vec<Candidate> {
    if input.chars().count() < min_length_or_default(min_length) {
        return Vec::new();
    }
    let lower = input.to_lowercase();
    match english.convert(&lower, seats) {
        Ok(r) => r.candidates,
        Err(_) => Vec::new(),
    }
}

/// 是否存在英文候选。供**上屏否决**判据用（`wind-engine/AGENTS.md`：否决必须叠
/// 「对方确有候选」，只看开关就禁上屏会把「英文被顶掉」修成「谁都上不了屏」）。
///
/// 只问有无，故只取 1 条。
pub fn has_any(english: &dyn Engine, input: &str, min_length: usize) -> bool {
    !lookup(english, input, min_length, 1).is_empty()
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
        merge(&mut calm, lookup(&eng, "hel", 0, seats_for(max)), max);
        assert!(
            !english_texts(&calm).is_empty(),
            "无洪水时英文应在场——否则下面那条断言测不到东西"
        );

        // 洪水：拼音已装满 max，英文仍须活下来，且总数不超 max。
        let mut flooded = pinyin_flood(max);
        merge(&mut flooded, lookup(&eng, "hel", 0, seats_for(max)), max);
        assert_eq!(
            english_texts(&flooded),
            vec!["hello", "help"],
            "拼音装满配额时英文被整片截掉 = 开关等于没做"
        );
        assert_eq!(flooded.len(), max, "腾座后总数不得超出 max_candidates");
    }

    /// 短输入不查英文：两个字母就刷前缀词会淹掉正常中文输入。
    #[test]
    fn short_input_skips_lookup() {
        let eng = FakeEnglish(vec![("he", "he"), ("hello", "hello")]);
        assert!(
            lookup(&eng, "he", 0, 5).is_empty(),
            "默认回退长度 3，两字母不该查"
        );
        assert!(!lookup(&eng, "hel", 0, 5).is_empty());
        // 显式配置覆盖回退值。
        assert!(!lookup(&eng, "he", 2, 5).is_empty());
    }

    /// 席位恒 ≥1：`max_candidates` 很小时比例算下来是 0，那等于开关静默失效。
    #[test]
    fn seats_never_zero() {
        assert_eq!(seats_for(0), 1);
        assert_eq!(seats_for(5), 1);
        assert_eq!(seats_for(20), 2);
        assert_eq!(seats_for(100), 5, "硬上限 5，不随 max 膨胀");
        assert_eq!(seats_for(1000), 5);
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
        merge(&mut base, lookup(&eng, "ok", 1, 5), 10);
        assert_eq!(base.len(), 1, "同文本不该重复入列");
    }
}
