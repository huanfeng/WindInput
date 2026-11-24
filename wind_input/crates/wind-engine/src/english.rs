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
    /// ⚠️ 必须在 [`Self::with_phrase_seg`] **之前**调用：那边一开就起预热线程，线程手里攥着
    /// 的是当时那个 `LazyPhraseIndex`，之后再换就是白建一份、按键时再建一份。
    pub fn with_phrase_cache_dir(mut self, dir: Option<std::path::PathBuf>) -> Self {
        debug_assert!(
            self.seg_sep.is_none(),
            "with_phrase_cache_dir 须在 with_phrase_seg 之前调用"
        );
        self.phrase = Arc::new(LazyPhraseIndex::with_cache_dir(dir));
        self
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

/// 剥离查询的命中**值不值得并进候选**：只收多词条目与含分词符的词。
///
/// 剥离查询存在的唯一理由是「带分词符的串在某些词库里编码不含分词符」，它要接住的是两类：
/// - **撇号词**：`o'clock` 在去撇号词库里 code 是 `oclock`，而 text 仍带着撇号；
/// - **词组**：`mac'os` 剥成 `macos` 恰好命中拼接式编码的 `macOS Tahoe` 一族——分段路径对
///   它们无能为力（`macOS` 在 text 里是**一个**词，段 `os` 无词可配），剥离是它们唯一的通路。
///
/// 这两类之外的命中一律是普通单词的前缀补全，而它们**不打分词符时本来就在**（直接打 `po`
/// 就能出 pocket / pod / poem）。打了分词符还出它们不但没有新增信息，还会按词库权重铺满整个
/// `max_candidates`，把用户真正用分词符请求的词组挤到看不见的地方。
///
/// ⚠️ 判据落在 **text** 而不是 code：这与 `english_phrase` 整个模块的立足点是同一条——词边界
/// 只在 text 的空白里，code 那边两种编码方案并存（见该模块头部的表）。
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
        // ★ 含分词符时追加两路候选，**词组分段在前、剥离查询在后**，三路统一按 text 去重。
        //
        // 顺序就是优先级：`natural_order` 在下游是同权重时的定序依据，而词库里 weight 相同的
        // 条目成片存在（出厂词组大量 weight = 0），实际次序多半就由它定。用户按下分词符是在
        // 表达「我要按段找词组」，分段候选理应压过「把分词符当没打过」的剥离候选。
        //
        // 去重必须**跨三路**：原路径与剥离路径可能命中同一条（同 text 两种编码），分段路径
        // 与剥离路径更会成片相撞——`mac'os` 下 `Mac OS X` 既被分段路径召回（两段各配上一个
        // 词），又被剥离串 `macos` 前缀命中 code `macosx`。
        //
        // ⚠️ 重复**到不了用户眼前**：协调器 `handle_candidate.rs` 那道按 text 的通用去重
        // （带 `merged_codes` 归并）会接住。要在这里去重是因为紧跟着的
        // `truncate(max_candidates)` 在引擎内——每条重复都白占一个名额，挤掉一条本可召回的
        // 词组，而协调器再去重也变不回来。故本条的护栏在引擎单测，不在 e2e（按整表断言的
        // e2e 实测恒绿，那是假护栏）。
        if let Some(sep) = self.seg_sep
            && input.contains(sep)
        {
            let mut seen: std::collections::HashSet<String> =
                r.candidates.iter().map(|c| c.text.clone()).collect();
            let mut extra: Vec<wind_candidate::Candidate> = Vec::new();

            // ── 一、词组分段候选 ──────────────────────────────────────────────
            //
            // ★ 为什么合并而不是「见到分词符就改走分词路径」：词库里有 57 条 code 本身含撇号
            // （`you're` / `let's` / `O'Reilly`）。分词符取 `'` 时，劫持式实现会让这些词在打
            // 全码时反而查不到——而它们原本是能精确命中的。合并则两边各查各的：`you'r` 由原
            // 路径出 `you're`、分词路径出空；`envi'deg` 反过来。零回归。
            for c in self.phrase_candidates(input, max_candidates) {
                if seen.insert(c.text.clone()) {
                    extra.push(c);
                }
            }

            // ── 二、剥掉分词符再查一次 ────────────────────────────────────────
            //
            // 撇号词的编码在词库之间不统一，这是实打实的：出厂英文词库保留撇号（`o'clock`
            // 的 code 就是 `o'clock`，57 条如此），而用户自制/第三方词库常把它去掉（实测靶机
            // 那份 3.8 MB 词库里是 `o'clock → oclock`）。分词符对用户而言是**输入语法**，他打
            // `o'clock` 时脑子里想的是那个词，不该被要求先知道自己这份词库是哪种编码方案。
            //
            // ★★ 但剥离命中必须过 [`stripped_hit_is_relevant`] 那道闸：不设闸的话 `p'o` 被剥成
            // `po`，pocket / pod / poem …一整屏普通单词补全按词库权重灌进来，分段候选一条都挤不
            // 进 `max_candidates`——实测反馈正是「打 `p'o` 出来的全是不相干的词」。
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
                // ★ 取原路径 `natural_order` 的**最大值**，不是它们的条数。
                //
                // 这两个数差着几个量级：`natural_order` 来自词库、是上万的序号（实测 `o'c` 的
                // `o'clock` 拿到 12085），而条数只有个位数。按条数续号的话追加的候选会拿到
                // 1..8，同权重时反而排在原路径候选**前面**——而原路径那条是带着分词符打全码
                // 精确命中的撇号词，它该在最前。
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
#[path = "english_tests.rs"]
mod tests;
