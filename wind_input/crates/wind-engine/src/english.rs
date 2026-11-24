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

    /// 词组分词索引的落盘目录（`None` = 不落盘，堆上建）。见 [`LazyPhraseIndex`] 的「落盘」一节。
    ///
    /// 必须在 [`Self::with_phrase_seg`] 之前调用：那边一开就起预热线程，线程手里攥着
    /// 的是当时那个 `LazyPhraseIndex`，之后再换就是白建一份、按键时再建一份。
    pub fn with_phrase_cache_dir(mut self, dir: Option<std::path::PathBuf>) -> Self {
        debug_assert!(
            self.seg_sep.is_none(),
            "with_phrase_cache_dir 须在 with_phrase_seg 之前调用"
        );
        self.phrase = Arc::new(LazyPhraseIndex::with_cache_dir(dir));
        self
    }

    /// 开启词组分词输入并指定分词符。
    ///
    /// 由构建方在引擎组装完毕后调用；`None` 保持关闭。开启时顺带把索引构建推给后台线程——
    /// 不预热的话那笔全表扫描会恰好落在用户第一次按下分词符的那一刻。
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
        // 单段照查，别在这里挡：走到这里的单段只有 `ip'`（刚按下分词符）这一种，原路径拿它
        // 查 code 前缀必然落空，再挡掉分词路径候选就整片消失。单段语义是「首词以这段开头的词组」。
        // 空 `segs`（整串只有分词符）由 `PhraseSegIndex::search` 自己挡。
        self.phrase
            .get(self.inner.dict_manager())
            .search(&segs, max)
    }
}

/// 剥离查询的命中值不值得并进候选：只收多词条目与含分词符的词。
///
/// 剥离查询要接住两类：撇号词（`o'clock` 在去撇号词库里 code 是 `oclock`）与拼接式编码的词组
/// （`mac'os` → `macos` 命中 `macOS Tahoe`，分段路径配不上）。其余命中是不打分词符本来就有的
/// 普通单词补全，放进来会铺满 `max_candidates`、挤掉用户要的词组。
///
/// 判据落在 text 而不是 code：词边界只在 text 的空白里，code 那边两种编码方案并存
/// （见 `english_phrase` 模块头）。
fn stripped_hit_is_relevant(text: &str, sep: char) -> bool {
    text.contains(sep) || text.split_whitespace().nth(1).is_some()
}

impl Engine for EnglishEngine {
    fn memory_parts(&self) -> Vec<crate::engine::MemPart> {
        let mut v = self.inner.memory_parts();
        if let Some(bytes) = self.phrase.heap_bytes_if_built() {
            // 镜像是一整块：常驻时 1 次分配，mmap 时堆上没有它（0 字节 / 0 块），
            // 映射的文件字节另列一项——那不是私有内存，混进堆字节会让「降没降」无从判断。
            let mapped = self.phrase.mapped_bytes_if_built().unwrap_or(0);
            v.push(crate::engine::MemPart {
                name: "LazyPhraseIndex".into(),
                bytes,
                allocs: usize::from(mapped == 0),
            });
            if mapped > 0 {
                v.push(crate::engine::MemPart {
                    name: "LazyPhraseIndex.mmap".into(),
                    bytes: mapped,
                    allocs: 0,
                });
            }
        }
        v
    }

    fn convert(&self, input: &str, max_candidates: usize) -> anyhow::Result<ConvertResult> {
        let mut r = self.inner.convert(input, max_candidates)?;
        // 英文候选统一标记来源（词频归属 / 融合加权档区分用）。
        for c in &mut r.candidates {
            c.source = CandidateSource::English;
        }
        // 含分词符时追加两路候选，词组分段在前、剥离查询在后（同权重条目成片存在，
        // `natural_order` 即优先级；按段找词组是用户的明确意图）。三路统一按 text 去重：
        // 协调器虽也去重，但引擎内紧跟着 `truncate(max_candidates)`，重复会白占名额。
        // 护栏在引擎单测，按整表断言的 e2e 测不出这一点。
        if let Some(sep) = self.seg_sep
            && input.contains(sep)
        {
            let mut seen: std::collections::HashSet<String> =
                r.candidates.iter().map(|c| c.text.clone()).collect();
            let mut extra: Vec<wind_candidate::Candidate> = Vec::new();

            // ── 一、词组分段候选 ──────────────────────────────────────────────
            //
            // 合并而不是「见到分词符就改走分词路径」：词库里有 code 本身含撇号的词
            // （`you're` / `O'Reilly`），劫持式实现会让它们打全码时反而查不到。
            for c in self.phrase_candidates(input, max_candidates) {
                if seen.insert(c.text.clone()) {
                    extra.push(c);
                }
            }

            // ── 二、剥掉分词符再查一次 ────────────────────────────────────────
            //
            // 撇号词的编码在词库之间不统一（出厂词库 code 保留撇号，第三方词库常去掉），
            // 分词符对用户是输入语法，不该要求他知道词库是哪种编码。剥离命中必须过
            // [`stripped_hit_is_relevant`]，否则 `p'o` → `po` 的普通补全会灌满候选。
            let stripped: String = input.chars().filter(|c| *c != sep).collect();
            if !stripped.is_empty() {
                for mut c in self.inner.convert(&stripped, max_candidates)?.candidates {
                    if !stripped_hit_is_relevant(&c.text, sep) || !seen.insert(c.text.clone()) {
                        continue;
                    }
                    c.source = CandidateSource::English;
                    extra.push(c);
                }
            }

            if !extra.is_empty() {
                // 从原路径 `natural_order` 的最大值续号而不是按条数：`natural_order` 是词库里
                // 上万的序号，按条数续号会让追加候选同权重时排到原路径精确命中的撇号词前面。
                let base = r
                    .candidates
                    .iter()
                    .map(|c| c.natural_order)
                    .max()
                    .map_or(0, |m| m + 1);
                r.candidates
                    .extend(extra.into_iter().enumerate().map(|(i, mut c)| {
                        c.natural_order = base + i as i32;
                        c
                    }));
                r.candidates.truncate(max_candidates);
            }
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
        // 词组索引必须跟着词库走：热摘返回 true 时上层不重建引擎（见
        // `EngineManager::set_dict_enabled_live`），不作废就会继续召回已禁用词库里的词组。
        // 只在本引擎认领了这个 id 时作废，认领不了说明是别的方案的库。
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
#[path = "english_tests.rs"]
mod tests;
