//! 码表空码组词（`[engine.codetable] compose_input`）：码长以内的串若空码，补几条
//! 「码表没收、但这串码打得出」的词。
//!
//! 设计与实测见 `docs/design/codetable-sentence-input.md` §13。两条来源，按词频合并：
//!
//! - **规则反查**：组词表里的词按方案 `[[encoder.rules]]` 算出词组码，等于输入即命中。
//!   码表没收的词，五笔用户打的就是它的词组码（`wfbn` →「付了」）。
//! - **拆码拼字**：输入切成 ≥2 段、每段查码表，拼出的文字是组词表里的词即命中。
//!   逐字打简码时走这条。
//!
//! 组词表与码表整句共用一份构建口径（拼音库词频前 20 万，[`ComposeLexicon::build`]），
//! 但两个功能互斥（见 `CodeTableEngine::new`），同一时刻只会建其中一份。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use wind_config::schema::EncoderSpec;
use wind_dict::DictManager;
use wind_dict::cached::CachedDict;

use super::sentence::ComposeLexicon;

/// 最多补几条。实测正解落在前 5 的已有 98%（规则反查口径），多给只会拉长候选窗。
pub const COMPOSE_LIMIT: usize = 8;

/// 拆码时每段向码表取几条（同整句 `MAX_HOMOGRAPHS`：码表序不是词频序，取口要放宽）。
const SEGMENT_HITS: usize = 8;

/// 词组码打包上限：每码 7 位，`u64` 放得下 9 码。更长的方案不做规则反查（拆码照常）。
const PACK_MAX: usize = 9;

fn pack(code: &str) -> Option<u64> {
    if code.is_empty() || code.len() > PACK_MAX {
        return None;
    }
    code.bytes().try_fold(0u64, |v, b| {
        (b != 0 && b < 128).then_some(v << 7 | u64::from(b))
    })
}

struct ComposeData {
    lexicon: ComposeLexicon,
    /// `(打包后的词组码, 组词表下标)`，按码升序。同码多词相邻。
    by_code: Vec<(u64, u32)>,
}

/// 拼音词库的来源，同整句 `FreqSource`：生产给目录（预热线程里加载），测试直接给词库。
enum Source {
    SchemasDir(PathBuf),
    Dict(Arc<CachedDict>),
}

struct Tables {
    source: Option<Source>,
    encoder: EncoderSpec,
    /// 内层 `None` = 试过但没成（拼音库缺失等），记住免得每次按键重试。
    data: OnceLock<Option<ComposeData>>,
}

impl Tables {
    fn data(&self, dm: &DictManager, cap: usize) -> Option<&ComposeData> {
        let src = self.source.as_ref()?;
        self.data
            .get_or_init(|| {
                let loaded;
                let py: &CachedDict = match src {
                    Source::Dict(d) => d,
                    Source::SchemasDir(dir) => {
                        loaded = crate::manager::EngineManager::load_sentence_freq_dict(dir)?;
                        &loaded
                    }
                };
                Some(build(py, &self.encoder, dm, cap))
            })
            .as_ref()
    }
}

fn build(py: &CachedDict, encoder: &EncoderSpec, dm: &DictManager, cap: usize) -> ComposeData {
    let (lexicon, _) = ComposeLexicon::build(py, std::iter::empty());
    let mut by_code: Vec<(u64, u32)> = Vec::new();
    if !encoder.rules.is_empty() {
        let codes = single_char_full_codes(dm, cap);
        for (i, (text, _)) in lexicon.iter().enumerate() {
            let Ok(code) =
                crate::encoder::calc_word_code(text, encoder, |c| codes.get(&c).cloned())
            else {
                continue;
            };
            if code.chars().count() <= cap
                && let Some(k) = pack(&code)
            {
                by_code.push((k, i as u32));
            }
        }
        by_code.sort_unstable();
    }
    tracing::debug!(
        lexicon = lexicon.len(),
        by_code = by_code.len(),
        "码表空码组词：组词表构建完成"
    );
    ComposeData { lexicon, by_code }
}

/// 单字全码：同 `CachedDict::build_single_char_full_codes` 的判据（码长 ≤ 上限 → 最长 →
/// 权重高 → 码字典序小），只是取自已组装好的 `DictManager`，不必再按方案读一遍词库。
fn single_char_full_codes(dm: &DictManager, cap: usize) -> HashMap<char, String> {
    let mut best: HashMap<char, (String, i32)> = HashMap::new();
    dm.for_each_entry(&mut |code, text, w| {
        let mut cs = text.chars();
        let (Some(c), None) = (cs.next(), cs.next()) else {
            return;
        };
        if cap > 0 && code.chars().count() > cap {
            return;
        }
        let better = |(b, bw): &(String, i32)| {
            (code.len(), w)
                .cmp(&(b.len(), *bw))
                .then_with(|| b.as_str().cmp(code))
                .is_gt()
        };
        match best.get_mut(&c) {
            Some(slot) if better(slot) => *slot = (code.to_string(), w),
            Some(_) => {}
            None => {
                best.insert(c, (code.to_string(), w));
            }
        }
    });
    best.into_iter().map(|(c, (code, _))| (c, code)).collect()
}

/// 空码组词器。表是懒构建的，由 [`Self::prewarm`] 推给后台线程。
pub struct WordComposer {
    max_code_length: usize,
    tables: Arc<Tables>,
}

impl WordComposer {
    /// `encoder` 取方案的 `[encoder]`；没配规则时只做拆码拼字。
    pub fn new(max_code_length: usize, encoder: EncoderSpec) -> Self {
        Self {
            max_code_length: max_code_length.max(1),
            tables: Arc::new(Tables {
                source: None,
                encoder,
                data: OnceLock::new(),
            }),
        }
    }

    fn tables_mut(&mut self) -> &mut Tables {
        Arc::get_mut(&mut self.tables).expect("组词器的数据来源须在 prewarm 之前配置")
    }

    /// 生产路径：拼音词库到预热线程（或首次组词）时才从数据目录读。
    pub fn with_schemas_dir(mut self, dir: PathBuf) -> Self {
        self.tables_mut().source = Some(Source::SchemasDir(dir));
        self
    }

    /// 测试路径：直接给已加载的拼音词库。
    pub fn with_pinyin_dict(mut self, dict: Arc<CachedDict>) -> Self {
        self.tables_mut().source = Some(Source::Dict(dict));
        self
    }

    /// 后台建表，免得第一次空码时按键线程等上几百毫秒（拼音库加载 + 20 万词取码）。
    pub fn prewarm(&self, dm: Arc<DictManager>) {
        let tables = Arc::clone(&self.tables);
        let cap = self.max_code_length;
        let spawned = std::thread::Builder::new()
            .name("codetable-compose-warm".into())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let ok = tables.data(&dm, cap).is_some();
                tracing::info!(
                    ms = t0.elapsed().as_millis(),
                    ok,
                    "码表空码组词：后台预热完成"
                );
            });
        if let Err(e) = spawned {
            tracing::warn!("码表空码组词：预热线程启动失败（{e}），退回首次组词时现场构建");
        }
    }

    /// 诊断：已建好的表的自报大小。不触发构建。
    pub(crate) fn memory_parts(&self) -> Vec<crate::engine::MemPart> {
        let Some(Some(d)) = self.tables.data.get() else {
            return Vec::new();
        };
        vec![crate::engine::MemPart {
            name: "compose/ComposeLexicon".into(),
            bytes: d.lexicon.heap_bytes() + d.by_code.capacity() * size_of::<(u64, u32)>(),
            allocs: 3,
        }]
    }

    /// 为 `input` 组词，按词频降序，至多 [`COMPOSE_LIMIT`] 条 `(文本, 词频)`。
    ///
    /// 不判空码与码长——那是调用方（`CodeTableEngine::decode_compose`）的闸门。
    pub fn compose(&self, input: &str, dm: &DictManager) -> Vec<(String, i32)> {
        let Some(data) = self.tables.data(dm, self.max_code_length) else {
            return Vec::new();
        };
        let mut found: HashMap<String, i32> = HashMap::new();
        if let Some(k) = pack(input) {
            let lo = data.by_code.partition_point(|e| e.0 < k);
            for e in data.by_code[lo..].iter().take_while(|e| e.0 == k) {
                let (t, f) = data.lexicon.entry(e.1 as usize);
                found.insert(t.to_string(), f);
            }
        }
        self.split_compose(input, dm, &data.lexicon, &mut found);
        let mut out: Vec<(String, i32)> = found.into_iter().collect();
        out.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.truncate(COMPOSE_LIMIT);
        out
    }

    /// 拆码拼字：切成 ≥2 段，每段取码表命中，DFS 拼接；拼到一半已不是任何词的前缀就剪掉。
    fn split_compose(
        &self,
        input: &str,
        dm: &DictManager,
        lexicon: &ComposeLexicon,
        found: &mut HashMap<String, i32>,
    ) {
        // 码元多为 ASCII，按字节切；非 ASCII 码元的方案不做拆码。
        if !input.is_ascii() || input.len() < 2 {
            return;
        }
        let n = input.len();
        let cap = self.max_code_length;
        // hits[p][l-1] = input[p..p+l] 的码表命中文本。特殊语法值（`$CC` / `{date}`）不参与，
        // 同逆切分 `split_segment`。
        let hits: Vec<Vec<Vec<String>>> = (0..n)
            .map(|p| {
                (1..=cap.min(n - p))
                    .map(|l| {
                        dm.search(&input[p..p + l], SEGMENT_HITS)
                            .into_iter()
                            .map(|c| c.text)
                            .filter(|t| !t.contains('$') && !t.contains('{'))
                            .collect()
                    })
                    .collect()
            })
            .collect();
        struct Walk<'a> {
            n: usize,
            hits: &'a [Vec<Vec<String>>],
            lexicon: &'a ComposeLexicon,
            found: &'a mut HashMap<String, i32>,
        }
        fn go(w: &mut Walk<'_>, pos: usize, segs: usize, acc: &mut String) {
            if pos == w.n {
                if segs >= 2
                    && let Some(f) = w.lexicon.get(acc)
                {
                    w.found.insert(acc.clone(), f);
                }
                return;
            }
            for (l, texts) in w.hits[pos].iter().enumerate() {
                let end = pos + l + 1;
                // 一段吃满整串 = 码表自己的精确命中，不归这里。
                if segs == 0 && end == w.n {
                    continue;
                }
                for t in texts {
                    let keep = acc.len();
                    acc.push_str(t);
                    if w.lexicon.has_prefix(acc) {
                        go(w, end, segs + 1, acc);
                    }
                    acc.truncate(keep);
                }
            }
        }
        let mut w = Walk {
            n,
            hits: &hits,
            lexicon,
            found,
        };
        go(&mut w, 0, 0, &mut String::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wind_config::schema::EncoderRule;
    use wind_dict::SystemDictLayer;
    use wind_dict::codetable::CodetableDict;

    fn dict(entries: &[(&str, &str, i32)]) -> CachedDict {
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as _);
        }
        CachedDict::Memory(d)
    }

    fn wubi_rules() -> EncoderSpec {
        let rule = |eq: usize, range: Vec<usize>, f: &str| EncoderRule {
            length_equal: eq,
            length_in_range: range,
            formula: f.into(),
        };
        EncoderSpec {
            rules: vec![
                rule(2, vec![], "AaAbBaBb"),
                rule(3, vec![], "AaBaCaCb"),
                rule(0, vec![4, 10], "AaBaCaZa"),
            ],
            ..Default::default()
        }
    }

    /// 码表：付 wfi / 了 bnh（全码）+ 一简 了=b、二简 付=wf；没收「付了」。
    fn codetable() -> DictManager {
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(
            dict(&[
                ("wfi", "付", 800),
                ("wf", "付", 9950),
                ("bnh", "了", 900),
                ("b", "了", 9999),
                ("ggg", "王", 900),
            ]),
            "ct",
        )));
        dm
    }

    fn composer(py: &[(&str, i32)]) -> WordComposer {
        let entries: Vec<(String, String, i32)> = py
            .iter()
            .enumerate()
            .map(|(i, (t, w))| (format!("py{i}"), t.to_string(), *w))
            .collect();
        let refs: Vec<(&str, &str, i32)> = entries
            .iter()
            .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
            .collect();
        WordComposer::new(4, wubi_rules()).with_pinyin_dict(Arc::new(dict(&refs)))
    }

    fn texts(v: Vec<(String, i32)>) -> Vec<String> {
        v.into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn rule_code_finds_word_missing_from_codetable() {
        let c = composer(&[("付了", 500), ("王了", 50)]);
        // 付了 = 付(wf) + 了(bn) = wfbn
        assert_eq!(texts(c.compose("wfbn", &codetable())), vec!["付了"]);
    }

    #[test]
    fn split_codes_compose_word_from_short_codes() {
        let c = composer(&[("付了", 500)]);
        // 二简「付」wf + 一简「了」b：不是词组码，靠拆码拼字。
        assert_eq!(texts(c.compose("wfb", &codetable())), vec!["付了"]);
    }

    #[test]
    fn results_sorted_by_frequency() {
        let dm = codetable();
        dm.register_layer(Box::new(SystemDictLayer::new(
            dict(&[("b", "子", 9000)]),
            "ct2",
        )));
        // `wfb` 拆成 付(wf) + 了/子(b)，两个都是词，词频高的在前。
        let c = composer(&[("付了", 50), ("付子", 500)]);
        assert_eq!(texts(c.compose("wfb", &dm)), vec!["付子", "付了"]);
    }

    #[test]
    fn both_sources_reach_the_same_word_once() {
        let c = composer(&[("王了", 500)]);
        let dm = codetable();
        // 王了 = 王(gg) + 了(bn) = ggbn（规则反查）；`gggb` = 王(ggg) + 了(b)（拆码）。
        assert_eq!(texts(c.compose("ggbn", &dm)), vec!["王了"]);
        assert_eq!(texts(c.compose("gggb", &dm)), vec!["王了"]);
    }

    #[test]
    fn single_segment_is_not_composed() {
        // 一段吃满整串 = 码表自己的精确命中，组词不重复产出单字。
        let c = composer(&[("付了", 500)]);
        assert!(c.compose("wfi", &codetable()).is_empty());
    }

    #[test]
    fn missing_pinyin_dict_yields_nothing() {
        let c = WordComposer::new(4, wubi_rules());
        assert!(c.compose("wfbn", &codetable()).is_empty());
    }

    #[test]
    fn single_char_full_code_prefers_longest_then_weight() {
        let codes = single_char_full_codes(&codetable(), 4);
        assert_eq!(codes[&'付'], "wfi");
        assert_eq!(codes[&'了'], "bnh");
    }
}
