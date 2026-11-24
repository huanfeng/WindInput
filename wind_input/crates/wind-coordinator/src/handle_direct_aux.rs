//! 直接辅助码（双拼）：不按引导键，输入末 1～2 位自动当辅码，字形对得上的字词提到前面。
//! 设计见 `docs/design/aux-code-direct.md`；连续辅助码（`docs/design/aux-code-continuous.md`）
//! 开着时（出厂随 `direct`）主转换已带辅码，本模块的串尾一步只做奇数长末字 1 位
//! （`DirectPhraseRule::Last`），下述完整行为是逃生口下的。纯逻辑在 `wind_aux_code::direct`，
//! 本模块只做协调器那一半：
//!
//! - 门卫（§4）：主输入路、双拼、`enabled` + `direct`、前缀恰好切成完整双拼音节、辅码来源
//!   就绪（未就绪 → 本键原样、派后台构建）；
//! - 前缀候选：优先取 [`DirectAuxPrev`] 快照，否则对前缀单独调一次引擎（无状态），走与主候选
//!   同一条加工链，故命中项之间的顺序就是用户单打前缀时看到的顺序；
//! - 标记：命中项的 `consumed_length` 改成整串（上屏连辅码一起吃掉），`code` 保留前缀的拼音码
//!   （调频与学习记在前缀下）；组码区形态存 `direct_aux_body`。
//!
//! 调用点钉在 `build_candidates` 的 `apply_shadow` 之前、全部重排之后：翻页扩容只重跑
//! `build_candidates`，放到 `update_candidates` 会在翻页时丢失。

use crate::aux_code_source::AuxLookupNow;
use crate::coordinator::{Coordinator, State};
use std::sync::Arc;
use wind_aux_code::DirectPhraseRule;
use wind_candidate::{Candidate, CandidateSource, candidate_display_order};

/// 「上一个整音节输入」的主候选快照（`State.direct_aux_prev`）：连打时前缀恰是几键前的整个
/// 输入（`uidupl` 的前缀 `uidu` 就是第 4 键时的输入），直接取用、不必对前缀再调一次引擎。
///
/// 存的是那一键并入命中项之前、shadow 之前的主候选，即用户单打前缀时所见的顺序（shadow 在
/// 取用时按前缀的码补上）。只在输入恰好切成完整双拼音节时更新；组合复位、分段上屏、退回
/// 已上屏段时丢弃。那一键的引擎产出被截断过就不存：截断的快照会漏掉排在后面的命中，
/// 连打与退格重打给出的命中项就不一样。
pub(crate) struct DirectAuxPrev {
    input: String,
    /// 取快照时的候选裁剪状态：检索范围档位、单字模式、末页放宽。输入不变而切了其中任一项
    /// （`set_single_char_in` / `set_filter_mode` 会原地重建候选），快照就与当下的过滤链对不上，
    /// 不能复用——否则词会绕过单字过滤、生僻字会绕过检索范围被提到前面。
    filters: (wind_candidate::FilterMode, bool, bool),
    /// 取快照时用户词 / 临时词的结构代次（`Store::words_generation`，无 store 为 0）。输入不变
    /// 而词库变了（候选右键删用户词、设置页加词…）快照就过期了，不能复用——否则删掉的用户词
    /// 照样被当命中项提到首位。常用字标记不走词库，由写端（`toggle_common_char`）直接丢快照。
    /// 第二项是辅码设置 / 来源的失效代次（`aux_settings_generation`）：来源变了，连续模式的
    /// 主候选与命中判定都不同。
    data_gen: (u64, u64),
    /// `None` = 整音节那一键的主候选快照（未截断，前缀下有资格的候选一条不缺）。
    /// `Some((字母, 上限))` = 兜底解码的产出，引擎按辅码首字母准入过、按该上限取的——只能给
    /// 同首字母、同上限的键复用（奇数键算的给紧随其后的偶数键：2 位辅码命中集是首字母的子集）。
    admit: Option<(char, usize)>,
    /// 取快照时是否在连续模式（主转换走击键图 + 辅码表）。两种模式的主候选不同，不能互用。
    pub(crate) continuous: bool,
    /// 只存有资格的拼音候选（`is_direct_source`），不整表克隆。
    candidates: Vec<Candidate>,
    /// 那一键的双拼音节分段（`ui'du`），组码区用。
    preedit: String,
    /// 那一键的 shadow 归一码（双拼下是全拼码），前缀 shadow 用。
    shadow_code: String,
}

/// 连续辅助码交给引擎的选项：击键图 + 本键的辅码视图 + 编码栏辅码分隔符
/// （[`wind_engine::AuxCodeSettings::preedit_separator`]，`None` = 引擎默认）。
pub(crate) fn continuous_opts(
    lookup: &Arc<AuxLookupNow>,
    separator: Option<Arc<str>>,
) -> wind_engine::engine::ConvertOptions {
    wind_engine::engine::ConvertOptions {
        sp_raw_lattice: true,
        aux_continuous: Some(lookup.clone() as Arc<dyn wind_engine::AuxCharLookup>),
        aux_preedit_separator: separator,
        ..Default::default()
    }
}

/// 连续辅助码对本键生效时的输入：本键的辅码视图与编码栏辅码分隔符（同一次设置快照取出）。
pub(crate) struct ContinuousAux {
    pub(crate) lookup: Arc<AuxLookupNow>,
    pub(crate) separator: Option<Arc<str>>,
}

impl ContinuousAux {
    pub(crate) fn opts(&self) -> wind_engine::engine::ConvertOptions {
        continuous_opts(&self.lookup, self.separator.clone())
    }
}

/// 串尾一步的词组规则：连续模式只认末字（词中辅码同一语义），只做串尾时任一字。
fn direct_rule(continuous: bool) -> DirectPhraseRule {
    if continuous {
        DirectPhraseRule::Last
    } else {
        DirectPhraseRule::Any
    }
}

impl Coordinator {
    /// 连续辅助码对本键是否生效，生效时给出本键的辅码视图与分隔符（每键现取，不跨键缓存）。
    ///
    /// 条件：主输入路（临拼 / 混输 / 引导键辅助码态都不做）、`direct` 生效（已折叠 `enabled` 与
    /// 「只认双拼」）、没写逃生口 `continuous = false`、有来源、方案来源的反查索引就绪。
    /// 未就绪派后台构建，本键 `None`——整条走旧路径，不是「击键图开、无辅码」。
    pub(crate) fn continuous_aux_lookup(&self, state: &State) -> Option<ContinuousAux> {
        if state.active.is_some() {
            return None;
        }
        let settings = self.engine_mgr.aux_code_settings_cached();
        if !settings.continuous || settings.sources.is_empty() {
            return None;
        }
        let rt = self.ensure_aux_code_runtime(&settings.sources);
        let lookup = rt.lookup(&self.engine_mgr);
        let unready: Vec<String> = lookup
            .unready_schemas()
            .into_iter()
            .map(str::to_string)
            .collect();
        if !unready.is_empty() {
            for id in &unready {
                self.spawn_index_warm(id, false);
            }
            tracing::debug!("continuous aux: 方案来源索引未就绪，本键走旧路径");
            return None;
        }
        Some(ContinuousAux {
            lookup: Arc::new(lookup),
            separator: settings.preedit_separator.clone(),
        })
    }

    /// [`Self::continuous_aux_lookup`] 的选项形态：`None` = 走 `engine_mgr.convert`。
    #[cfg(test)]
    pub(crate) fn continuous_aux_opts(
        &self,
        state: &State,
    ) -> Option<wind_engine::engine::ConvertOptions> {
        self.continuous_aux_lookup(state).map(|c| c.opts())
    }

    /// 把直接辅助码的命中项并入 `candidates`（主候选，已排序去重过滤、尚未 shadow）。
    /// 任一门卫不过 / 无命中 → 候选原样。组码区形态写进 `state.direct_aux_body`（无命中清空）。
    ///
    /// `continuous`：本键主转换走了连续辅助码（[`Self::continuous_aux_lookup`]）。这时串尾一步
    /// 只补引擎不产出的「末字 / 单字 + 末尾辅码」提前（[`DirectPhraseRule::Last`]，与词中辅码
    /// 同一语义），不再做词组任一字匹配；前缀兜底解码用同一份选项。
    pub(crate) fn apply_direct_aux(
        &self,
        state: &mut State,
        candidates: &mut Vec<Candidate>,
        limit: usize,
        truncated: bool,
        continuous: Option<&Arc<AuxLookupNow>>,
    ) {
        state.direct_aux_body.clear();
        // 主输入路：临拼 / 混输 / 引导键辅助码态都不做（引导键态本就筛的是现成候选表）。
        if state.active.is_some() {
            state.direct_aux_prev = None;
            return;
        }
        // 两件事各要一道双拼音节判定（纯内存；全拼 / 码表 / 混输恒 None，零额外成本）：
        // 本键输入整串成音节 → 存快照给后面的键当前缀；前缀成音节 → 本键做直接辅助。
        let input = state.input_buffer.clone();
        let whole = self
            .engine_mgr
            .shuangpin_full_syllable_count(&input)
            .is_some();
        let split = wind_aux_code::split_direct(&input);
        let syllables = split.and_then(|s| self.engine_mgr.shuangpin_full_syllable_count(s.prefix));
        if !whole && syllables.is_none() {
            return;
        }
        // 缓存版：每个双拼按键都走到这里，未开的用户也一样，不能逐键读盘解析方案文件。
        let settings = self.engine_mgr.aux_code_settings_cached();
        if !settings.direct || settings.sources.is_empty() {
            state.direct_aux_prev = None;
            return;
        }
        // 只留有资格的拼音候选（命中项只会从它们里出），不整表克隆。
        let pre_merge = whole.then(|| {
            candidates
                .iter()
                .filter(|c| {
                    c.source == CandidateSource::Pinyin
                        && wind_aux_code::is_direct_source(c, input.len())
                })
                .cloned()
                .collect::<Vec<_>>()
        });
        if let (Some(split), Some(syllables)) = (split, syllables) {
            self.merge_direct_hits_into(
                state, candidates, limit, split, syllables, &settings, continuous,
            );
        }
        // 奇数键（或前缀以外的不成音节输入）不动快照，留给后面的键。
        if let Some(list) = pre_merge {
            state.direct_aux_prev = (!truncated).then(|| DirectAuxPrev {
                filters: self.direct_aux_filters(state),
                data_gen: self.direct_aux_data_gen(),
                admit: None,
                continuous: continuous.is_some(),
                input,
                candidates: list,
                preedit: state.preedit_split_body.clone(),
                shadow_code: state.shadow_code.clone(),
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn merge_direct_hits_into(
        &self,
        state: &mut State,
        candidates: &mut Vec<Candidate>,
        limit: usize,
        split: wind_aux_code::DirectSplit<'_>,
        syllables: usize,
        settings: &wind_engine::AuxCodeSettings,
        continuous: Option<&Arc<AuxLookupNow>>,
    ) {
        // 连续模式串尾一步只做奇数长 1 位；末字 2 位由引擎出，单字 2 位与万象一致，需 `/` 才提前。
        if continuous.is_some() && split.aux.len() == 2 {
            return;
        }
        let lookup = match continuous {
            Some(l) => l.clone(),
            None => Arc::new(
                self.ensure_aux_code_runtime(&settings.sources)
                    .lookup(&self.engine_mgr),
            ),
        };
        let rule = direct_rule(continuous.is_some());
        // 方案来源的系统层（反查索引）未就绪：本键不做、派后台构建。按键线程绝不现建。
        let unready = lookup.unready_schemas();
        if !unready.is_empty() {
            for id in unready {
                self.spawn_index_warm(id, false);
            }
            tracing::debug!("direct aux: 方案来源索引未就绪，本键不做直接辅助");
            return;
        }
        let prefix = split.prefix;
        let letter = split.aux.chars().next().unwrap_or_default();
        let filters = self.direct_aux_filters(state);
        let data_gen = self.direct_aux_data_gen();
        let snapshot = state
            .direct_aux_prev
            .as_ref()
            .filter(|p| {
                p.input == prefix
                    && p.filters == filters
                    && p.data_gen == data_gen
                    && p.continuous == continuous.is_some()
                    && p.admit.is_none_or(|a| a == (letter, limit))
            })
            .map(|p| {
                (
                    p.candidates.clone(),
                    p.preedit.clone(),
                    p.shadow_code.clone(),
                )
            });
        let (mut pool, preedit, shadow_code) = match snapshot {
            Some(s) => s,
            // 没有可用的快照（前缀那一键被截断过、退格改了前缀、光标中间编辑、切了过滤…）：
            // 对前缀单独解码一次，走与主候选同一条加工链；结果留作快照给紧随其后的偶数键。
            None => {
                let (pool, preedit, shadow_code) = self.decode_direct_prefix(
                    state,
                    prefix,
                    limit,
                    split.aux,
                    settings,
                    &lookup,
                    continuous.is_some(),
                );
                state.direct_aux_prev = Some(DirectAuxPrev {
                    input: prefix.to_string(),
                    filters,
                    data_gen,
                    admit: Some((letter, limit)),
                    continuous: continuous.is_some(),
                    candidates: pool.clone(),
                    preedit: preedit.clone(),
                    shadow_code: shadow_code.clone(),
                });
                (pool, preedit, shadow_code)
            }
        };
        // 用户在前缀下隐藏的词，当辅码命中项也不该冒出来（置顶同理保留其次序）。
        let prefix_shadow = if shadow_code.is_empty() {
            prefix
        } else {
            &shadow_code
        };
        self.run_passes(
            state,
            &mut pool,
            crate::postprocess::profile::DIRECT_AUX_MERGE,
            &crate::postprocess::PassCtx {
                owner: None,
                freq_code: None,
                shadow_code: Some(prefix_shadow),
            },
        );

        let input_len = state.input_buffer.len();
        let hits: Vec<Candidate> = pool
            .into_iter()
            .filter(|c| {
                c.source == CandidateSource::Pinyin
                    && wind_aux_code::is_direct_source(c, prefix.len())
                    && wind_aux_code::direct_matches(
                        &c.text,
                        &*lookup,
                        split.aux,
                        rule,
                        settings.max_phrase_len,
                    )
            })
            .map(|mut c| {
                wind_aux_code::mark_direct_hit(&mut c, input_len);
                c
            })
            .collect();
        if hits.is_empty() {
            return;
        }
        let placement = wind_aux_code::direct_placement(split.aux.len(), syllables);
        *candidates = wind_aux_code::merge_direct_hits(
            std::mem::take(candidates),
            hits,
            input_len,
            placement,
        );
        // 组码区：前缀按双拼音节切（引擎给的击键分段，如 `ui'du`）+ 辅码分隔符（默认空格，与
        // 引擎辅码整句同一项）+ 辅码；仍是「缓冲按序插入分隔符」的形态，光标换算照常成立。
        let body = if preedit.is_empty() { prefix } else { &preedit };
        state.direct_aux_body = format!("{body}{}{}", settings.preedit_separator(), split.aux);
    }

    /// 快照复用的前提：候选裁剪状态与取快照时相同（见 [`DirectAuxPrev::filters`]）。
    fn direct_aux_data_gen(&self) -> (u64, u64) {
        (
            self.store.as_ref().map_or(0, |s| s.words_generation()),
            self.engine_mgr.aux_settings_generation(),
        )
    }

    fn direct_aux_filters(&self, state: &State) -> (wind_candidate::FilterMode, bool, bool) {
        (
            state.filter_mode,
            self.effective_single_char(state),
            state.scope_relaxed,
        )
    }

    /// 对前缀单独解码（快照缺失时的兜底）：`convert_with_opts` 不碰本次会话（引擎无状态），
    /// 结果走与主候选同一条加工链（shadow 除外，由调用方按前缀的码补）。
    /// 返回（候选, 双拼音节分段, shadow 归一码）。
    #[allow(clippy::too_many_arguments)]
    fn decode_direct_prefix(
        &self,
        state: &State,
        prefix: &str,
        limit: usize,
        aux: &str,
        settings: &wind_engine::AuxCodeSettings,
        lookup: &Arc<AuxLookupNow>,
        continuous: bool,
    ) -> (Vec<Candidate>, String, String) {
        let rule = direct_rule(continuous);
        let max_phrase_len = settings.max_phrase_len;
        let admit_lookup = lookup.clone();
        let letter: String = aux.chars().take(1).collect();
        // 智能档按码位看「同码位有没有常用字」：池里只有命中项时同码位的常用字不在，生僻命中
        // 会被当孤儿码位放行（连打取快照时不会）。常用字一并准入，命中仍由调用方按辅码筛。
        let common = (state.filter_mode == wind_candidate::FilterMode::Smart)
            .then(|| (self.common_chars.clone(), self.engine_mgr.charsets()));
        let opts = wind_engine::engine::ConvertOptions {
            // 只要吃满前缀的候选，且在截断之前丢掉其余（否则同音单字会把词挤出配额）。
            require_full_match: true,
            no_abbrev_quota: true,
            // 按辅码首字母准入（2 位辅码的命中集是 1 位的子集，见 `direct_matches`）：
            // 截断前就只剩可能命中的——`ui`（shi）有几百个同音字，全量取一遍要多花十几毫秒。
            admit: Some(Arc::new(move |text: &str| {
                wind_aux_code::direct_matches(text, &*admit_lookup, &letter, rule, max_phrase_len)
                    || common.as_ref().is_some_and(|(cc, cs)| {
                        cc.read()
                            .unwrap_or_else(|e| e.into_inner())
                            .is_string_common(text, cs)
                    })
            })),
            // 连续模式下与主转换同一份选项（前缀里也可能有词中辅码），快照与兜底才一致。
            ..if continuous {
                continuous_opts(lookup, settings.preedit_separator.clone())
            } else {
                Default::default()
            }
        };
        let active = self.engine_mgr.active_schema_id();
        let r = self
            .engine_mgr
            .convert_with_opts(&active, prefix, limit, opts);
        let mut pool = self.finalize_candidates(r.candidates, prefix);
        self.mark_common(&mut pool);
        let ignore_weight = self.engine_mgr.active_base_sort_ignores_weight();
        pool.sort_by(|a, b| candidate_display_order(a, b, ignore_weight, false, prefix));
        // 去重同主路径：被弃条目的码位要并进幸存者（`merged_codes`），检索范围按码位分组时
        // 才不会丢掉「该码位下有常用字」这一事实（见 `build_candidates` 同处注释）。
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut deduped: Vec<Candidate> = Vec::with_capacity(pool.len());
        for c in pool {
            if let Some(&idx) = seen.get(&c.text) {
                deduped[idx].absorb_codes_from(&c);
                continue;
            }
            seen.insert(c.text.clone(), deduped.len());
            deduped.push(c);
        }
        let mut pool = deduped;
        self.run_passes(
            state,
            &mut pool,
            crate::postprocess::profile::DIRECT_AUX_PREFIX,
            &crate::postprocess::PassCtx {
                owner: None,
                freq_code: Some(prefix),
                shadow_code: None,
            },
        );
        (pool, r.preedit_pinyin, r.shadow_code)
    }
}

#[cfg(test)]
#[path = "handle_direct_aux_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "handle_direct_aux_real_data_tests.rs"]
mod real_data_tests;
