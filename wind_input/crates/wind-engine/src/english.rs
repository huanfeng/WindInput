//! 英文引擎（临时英文 / 融合英文候选用）
//!
//! 薄封装 [`CodeTableEngine`]：复用其词典加载与前缀匹配，但作为独立引擎类型，
//! 便于后续英文专属演化（词频归属、融合加权档、独立学习）。英文词库以 `type = "english"`
//! 声明（code 列小写化，大小写不敏感前缀匹配），构造时关闭码表的自动上屏 / 顶码 / 编码提示
//! （英文词变长，无「满码顶字」语义）。
//!
//! 大小写适配（输入 `HEL` → `HELLO`）由 coordinator 层的临时英文按输入形态后处理，
//! 融合模式（快捷 / 混输）不做适配，故本引擎只吐词库原文候选。

use crate::codetable::CodeTableEngine;
use crate::engine::{ConvertResult, Engine, EngineType};
use crate::english_phrase::{LazyPhraseIndex, split_segments};
use std::sync::Arc;
use wind_candidate::CandidateSource;

/// 英文引擎：内部复用码表引擎的查询，候选统一标记为 [`CandidateSource::English`]。
pub struct EnglishEngine {
    inner: CodeTableEngine,
    /// 词组分词索引（懒建 + 后台预热）。恒建结构、按 `seg_sep` 决定是否真的去填它。
    phrase: Arc<LazyPhraseIndex>,
    /// 词组分词符。`None` = 功能关闭，`convert` 逐字节退回原路径。
    seg_sep: Option<char>,
}

impl EnglishEngine {
    pub fn new(inner: CodeTableEngine) -> Self {
        Self {
            inner,
            phrase: Arc::new(LazyPhraseIndex::new()),
            seg_sep: None,
        }
    }

    /// 开启词组分词输入并指定分词符（t42）。
    ///
    /// 由构建方在引擎组装完毕后调用；`None` 保持关闭。开启时顺带把索引构建推给后台线程——
    /// 不预热的话那笔全表扫描会恰好落在用户**第一次按下分词符**的那一刻。
    pub fn with_phrase_seg(mut self, sep: Option<char>) -> Self {
        self.seg_sep = sep;
        if sep.is_some() {
            self.phrase.prewarm(Arc::clone(self.inner.dict_manager()));
        }
        self
    }

    /// 词组分词候选。无分词符 / 功能关闭 / 切不出段时返回空。
    fn phrase_candidates(&self, input: &str, max: usize) -> Vec<wind_candidate::Candidate> {
        let Some(sep) = self.seg_sep else {
            return Vec::new();
        };
        if !input.contains(sep) {
            return Vec::new();
        }
        let segs = split_segments(input, sep);
        // ★ 单段照查，别在这里挡。
        //
        // 「单段等价于普通前缀补全、原路径已经做了」这个理由只对**不含分词符**的输入成立，
        // 而那种输入在上面就被 `!input.contains(sep)` 挡掉了、根本走不到这里。能走到这里的
        // 单段只有一种形状：`ip'`（用户刚按下分词符）。此时原路径拿 `ip'` 去查 code 前缀
        // 必然落空（词库里没有 code 以 `ip'` 开头的条目），再把分词路径也挡掉，候选就整片
        // 消失了——实测反馈正是「打到 ' 时变空候选」。
        //
        // 单段查询的语义是「列出首词以这一段开头的词组」，正是按下分词符时该看到的东西。
        // 空 `segs`（整串只有分词符）由 `PhraseSegIndex::search` 自己挡。
        self.phrase
            .get(self.inner.dict_manager())
            .search(&segs, max)
    }
}

impl Engine for EnglishEngine {
    fn convert(&self, input: &str, max_candidates: usize) -> anyhow::Result<ConvertResult> {
        let mut r = self.inner.convert(input, max_candidates)?;
        // 英文候选统一标记来源（词频归属 / 融合加权档区分用）。
        for c in &mut r.candidates {
            c.source = CandidateSource::English;
        }
        // ★ 含分词符时，原路径**再用剥掉分词符的串查一次**，结果并进来。
        //
        // 撇号词的编码在词库之间不统一，这是实打实的：出厂英文词库保留撇号
        // （`o'clock` 的 code 就是 `o'clock`，57 条如此），而用户自制/第三方词库常把它
        // 去掉（实测靶机那份 3.8 MB 词库里是 `o'clock → oclock`）。
        //
        // 分词符对用户而言是**输入语法**，他打 `o'clock` 时脑子里想的是那个词，不该被要求
        // 先知道自己这份词库是哪种编码方案。只查原串的话，去撇号那种词库下从打第 4 个键
        // （`o'cl`）起就是零候选——实测反馈正是这个。
        //
        // 两个串都查、按 text 去重：保留撇号的词库由原串命中，去撇号的由剥离串命中，
        // 两种方案下表现一致。代价是多一次 Trie 前缀查询，只在缓冲含分词符时发生。
        if let Some(sep) = self.seg_sep
            && input.contains(sep)
        {
            let stripped: String = input.chars().filter(|c| *c != sep).collect();
            if !stripped.is_empty() {
                let seen: std::collections::HashSet<String> =
                    r.candidates.iter().map(|c| c.text.clone()).collect();
                let mut alt = self.inner.convert(&stripped, max_candidates)?;
                alt.candidates.retain(|c| !seen.contains(&c.text));
                for c in &mut alt.candidates {
                    c.source = CandidateSource::English;
                }
                r.candidates.extend(alt.candidates);
            }
        }
        // 词组分词候选**追加在原路径之后**，不是二选一。
        //
        // ★ 为什么合并而不是「见到分词符就改走分词路径」：词库里有 57 条 code 本身含撇号
        // （`you're` / `let's` / `O'Reilly`）。分词符取 `'` 时，劫持式实现会让这些词在
        // 打全码时反而查不到——而它们原本是能精确命中的。合并则两边各查各的：
        // `you'r` 由原路径出 `you're`、分词路径出空；`envi'deg` 反过来。零回归。
        //
        // 两侧重复的可能性可以忽略：分词路径只出**多词**条目，而原路径要命中同一条，
        // 得有一条 code 恰好等于带分词符的输入串——真出现了也是词库里确有此码，
        // 那条候选本就该在。
        let extra = self.phrase_candidates(input, max_candidates);
        if !extra.is_empty() {
            // ★ 取原路径 `natural_order` 的**最大值**，不是它们的条数。
            //
            // 这两个数差着几个量级：`natural_order` 来自词库、是上万的序号（实测 `o'c` 的
            // `o'clock` 拿到 12085），而条数只有个位数。按条数续号的话分词候选会拿到 1..8，
            // 同权重时反而排在原路径候选**前面**——与本注释想避免的恰好相反。
            let base = r
                .candidates
                .iter()
                .map(|c| c.natural_order)
                .max()
                .map_or(0, |m| m + 1);
            r.candidates
                .extend(extra.into_iter().enumerate().map(|(i, mut c)| {
                    // natural_order 接在原路径之后续号：它在下游是**同权重时的定序依据**，
                    // 让分词候选从 0 重新开始会与原路径候选交错。
                    c.natural_order = base + i as i32;
                    c
                }));
            r.candidates.truncate(max_candidates);
        }
        // 英文无「自动上屏」语义：即使内部误判也抹掉（构造已关，此为双保险）。
        r.should_commit = false;
        r.commit_text.clear();
        Ok(r)
    }

    fn reset(&self) {
        self.inner.reset();
    }

    fn engine_type(&self) -> EngineType {
        EngineType::English
    }

    fn set_dict_enabled(&self, dict_id: &str, enabled: bool) -> bool {
        let claimed = self.inner.set_dict_enabled(dict_id, enabled);
        // ★ 词组索引必须跟着词库走。
        //
        // 关闭方向走的是**热摘**（`unregister_layer`），返回 true 表示「目标已达成」⇒ 上层
        // 不会重建引擎（见 `EngineManager::set_dict_enabled_live`）。索引是在引擎构造时建的，
        // 没有这一行就会继续召回已禁用词库里的词组——出厂 `en_ext` 一本就带 779/787 条，
        // 而同一串输入走原路径已经查不到它们了，表现为「关了没反应、重启才好」。
        //
        // 只在本引擎认领了这个 id 时作废：认领不了说明那是别的方案的库，与本索引无关。
        if claimed {
            self.phrase.invalidate();
        }
        claimed
    }

    fn input_chars(&self) -> Option<&wind_config::CodeCharSet> {
        Engine::input_chars(&self.inner)
    }

    fn max_code_length(&self) -> usize {
        Engine::max_code_length(&self.inner)
    }

    fn has_full_input_match(&self, input: &str) -> bool {
        self.inner.has_full_input_match(input)
    }

    fn has_longer_code(&self, input: &str) -> bool {
        self.inner.has_longer_code(input)
    }

    // handle_top_code：用 trait 默认 None —— 英文无顶码上屏语义。
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codetable::CommitOptions;
    use crate::engine::Engine;
    use std::sync::Arc;
    use wind_dict::cached::CachedDict;
    use wind_dict::codetable::CodetableDict;
    use wind_dict::{DictManager, SystemDictLayer};

    /// 内存英文引擎，开着词组分词。
    fn engine(entries: &[(&str, &str, i32)]) -> EnglishEngine {
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "en")));
        let ct = CodeTableEngine::new(32, CommitOptions::default(), Arc::new(dm));
        EnglishEngine::new(ct).with_phrase_seg(Some(crate::english_phrase::PHRASE_SEPARATOR))
    }

    fn texts(e: &EnglishEngine, input: &str) -> Vec<String> {
        e.convert(input, 20)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    /// ★★ 撇号词的编码在词库之间不统一，两种方案都得能打出来。
    ///
    /// 出厂英文词库**保留**撇号（`o'clock` 的 code 就是 `o'clock`，57 条如此），而用户
    /// 自制/第三方词库常把它**去掉**（实测靶机那份 3.8 MB 词库里是 `o'clock → oclock`）。
    ///
    /// 用户打 `o'clock` 时脑子里想的是那个词，不该被要求先知道自己这份词库是哪种编码。
    /// 只查原串的话，去撇号那种词库下从第 4 个键（`o'cl`）起就是零候选——这条护栏就是
    /// 那次实测反馈的固化。**出厂词库表达不了这个场景**（它只有保留撇号那一种），
    /// 所以必须在这里用自造词库测。
    #[test]
    fn apostrophe_word_found_under_both_encoding_schemes() {
        // 方案一：code 保留撇号（出厂英文词库的形态）
        let keep = engine(&[("o'clock", "o'clock", 1000)]);
        assert_eq!(texts(&keep, "o'cl"), vec!["o'clock"], "保留撇号的词库");

        // 方案二：code 去掉撇号（用户自制词库的常见形态）
        let strip = engine(&[("oclock", "o'clock", 1000)]);
        assert_eq!(texts(&strip, "o'cl"), vec!["o'clock"], "去撇号的词库");

        // 两种方案下打完整串同样命中。
        assert_eq!(texts(&keep, "o'clock"), vec!["o'clock"]);
        assert_eq!(texts(&strip, "o'clock"), vec!["o'clock"]);
    }

    /// 剥离查询不得产生重复：两种 code 同时存在时，同一个 text 只出一条。
    #[test]
    fn stripped_query_does_not_duplicate() {
        let e = engine(&[("o'clock", "o'clock", 1000), ("oclock", "o'clock", 900)]);
        assert_eq!(
            texts(&e, "o'cl"),
            vec!["o'clock"],
            "同 text 的两条编码只该出一条"
        );
    }

    /// ★ 反向对照：不含分词符时**不做**剥离查询，行为逐字节不变。
    ///
    /// 没有这条，「恒查两次」与「只在含分词符时查两次」都能过上面几条。
    #[test]
    fn plain_input_does_not_trigger_stripped_query() {
        let e = engine(&[("oclock", "o'clock", 1000), ("ocl", "OCL", 900)]);
        // 打 `ocl`（不含分词符）：只有前缀匹配的结果，不会因为剥离而多出什么。
        let got = texts(&e, "ocl");
        assert!(got.contains(&"OCL".to_string()));
        assert!(got.contains(&"o'clock".to_string()));
        assert_eq!(got.len(), 2, "不含分词符时不该有额外查询，实际: {got:?}");
    }
}
