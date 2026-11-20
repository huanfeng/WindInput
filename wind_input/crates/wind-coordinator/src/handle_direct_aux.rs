//! 直接辅助码（双拼）：不按引导键，输入末 1～2 位自动当辅码，字形对得上的字词提到前面。
//! 连续辅助码（`docs/design/aux-code-continuous.md`）开着时（出厂随 `direct`），主转换已带辅码，
//! 本模块的串尾一步只做奇数长末字 1 位（`DirectPhraseRule::Last`）；下述完整行为是逃生口下的。
//! 设计见 `docs/design/aux-code-direct.md`；切分 / 匹配 / 并入的纯逻辑在
//! `wind_aux_code::direct`，本模块只做协调器那一半：
//!
//! - **门卫**（§4）：主输入路、双拼、`enabled` + `direct`、前缀恰好切成完整双拼音节、
//!   辅码来源就绪（方案来源的系统层未就绪 → 本键原样、派后台构建，与引导键进入同一处理）；
//! - **前缀候选**：连打时前缀恰是几键前的整个输入，直接取那一键的主候选快照
//!   （[`DirectAuxPrev`]，只留有资格的拼音候选；那一键被截断过、或检索范围 / 单字 / 放宽状态
//!   与当时不同都不用）；否则对前缀单独调一次引擎（`convert_with_opts`，引擎无状态，不碰本次
//!   会话），走与主候选同一条加工链（展开 / 常用字标记 / 显示序 / 检索范围 / 单字 / 调频 /
//!   shadow），所以「命中项之间的顺序」就是用户单打前缀时看到的顺序；
//! - **标记**：命中项的 `consumed_length` 改成整串（上屏连辅码一起吃掉），`code` 保留前缀
//!   的拼音码（调频与学习记在前缀下，「释读」记 `shidu`）；组码区形态存 `direct_aux_body`。
//!
//! ★ 调用点钉在 `build_candidates` 的 `apply_shadow` **之前**、全部重排之后：翻页扩容
//! （`expand_candidates`）只重跑 `build_candidates`，放到 `update_candidates` 会在翻页时丢失。

use crate::aux_code_source::AuxLookupNow;
use crate::coordinator::{Coordinator, State};
use std::sync::Arc;
use wind_aux_code::DirectPhraseRule;
use wind_candidate::{Candidate, CandidateSource, candidate_display_order};

/// 「上一个整音节输入」的主候选快照（`State.direct_aux_prev`）：连打时前缀恰是几键前的整个
/// 输入（`uidup` / `uidupl` 的前缀 `uidu` 就是敲到第 4 键时的输入），那一键的候选早已算好，
/// 直接取用即可，不必对前缀再调一次引擎（设计附录 A「取上一次按键的候选，免重算」）。
///
/// 存的是那一键**并入命中项之前、shadow 之前**的主候选：已走完展开 / 常用字 / 显示序 / 检索
/// 范围 / 单字 / 调频（调频的码就是那时的输入 = 现在的前缀），也就是用户单打前缀时所见的
/// 顺序（shadow 在取用时按前缀的码补上）。只在输入恰好切成完整双拼音节时才更新；奇数键不动它，
/// 留给紧随其后的偶数键。组合复位（上屏 / 清空）、分段上屏、退回已上屏段时丢弃。
///
/// ★ 那一键的引擎产出**到了上限**（被截断过）就不存：兜底解码按辅码首字母在截断**前**准入，
/// 截断过的快照会漏掉排在后面的命中（单音节前缀 `wu` 数百个同音字，`wuk` 曾少了 喔 呒 圄），
/// 于是连打与退格重打给出的命中项不一样。
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
    /// 只存**有资格**的拼音候选（`is_direct_source`），不整表克隆。
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
            // 只要吃满前缀的候选，且在截断**之前**丢掉其余（否则同音单字会把词挤出配额）。
            require_full_match: true,
            no_abbrev_quota: true,
            // 按辅码**首字母**准入（2 位辅码的命中集是 1 位的子集，见 `direct_matches`）：
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
mod tests {
    //! 小词库 + 小码表夹具的无头集成测试：真实的小鹤双拼引擎（布局取入库的
    //! `data/schemas/shuangpin/xiaohe.toml`）、rime 源格式的迷你词库、`字=码` 码表，按键走
    //! `handle_key_event` 入口。真实数据（`build_dev/data`）那组在下方 `real_data_tests`。
    use crate::coordinator::Coordinator;
    use crate::pipeline::ModeKind;
    use std::sync::Arc;
    use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
    use wind_config::Config;
    use wind_ipc::protocol::EVENT_KEY_DOWN;
    use wind_keys::keymap;
    use wind_store::Store;

    /// 迷你词库。权重让「湿度」天然居首、「释读」垫底——直接辅助码要把它提上来。
    const DICT: &str = "释读\tshi du\t10\n湿度\tshi du\t5000\n适度\tshi du\t3000\n\
                        十度\tshi du\t1000\n试读\tshi du\t500\n\
                        是\tshi\t9000\n十\tshi\t5000\n湿\tshi\t400\n释\tshi\t50\n\
                        读\tdu\t3000\n度\tdu\t2000\n\
                        国庆\tguo qing\t800\n国情\tguo qing\t600\n国\tguo\t5000\n\
                        想\txiang\t5000\n向\txiang\t4000\n像\txiang\t3000\n\
                        你好\tni hao\t3000\n拟好\tni hao\t200\n你号\tni hao\t100\n\
                        你\tni\t8000\n拟\tni\t300\n好\thao\t6000\n号\thao\t2000\n";
    /// 小鹤形码（取自 flypy_full.txt；你 / 拟 / 好 / 号为夹具自拟）。
    const AUX: &str = "释=pl\n湿=dy\n适=zk\n十=al\n试=yg\n读=yd\n度=gy\n是=or\n\
                       国=ky\n庆=gd\n情=xo\n想=mx\n向=pk\n像=rn\n\
                       你=rx\n拟=tw\n好=hn\n号=kd\n";

    struct Fixture {
        dir: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// `aux` 是 `[engine.aux_code]` 里 `files` 之外的行（enabled / direct）；`scheme` 空 = 全拼。
    fn fixture(tag: &str, scheme: &str, aux: &str) -> (std::path::PathBuf, Fixture) {
        fixture_with(tag, scheme, aux, "", "")
    }

    /// [`fixture`]，词库与码表各追加几行。
    fn fixture_with(
        tag: &str,
        scheme: &str,
        aux: &str,
        more_dict: &str,
        more_aux: &str,
    ) -> (std::path::PathBuf, Fixture) {
        let dir =
            std::env::temp_dir().join(format!("wind_direct_aux_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let schemas = dir.join("schemas");
        std::fs::create_dir_all(schemas.join("aux_code")).unwrap();
        std::fs::create_dir_all(schemas.join("sp")).unwrap();
        std::fs::create_dir_all(schemas.join("shuangpin")).unwrap();
        let layout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin/xiaohe.toml");
        std::fs::copy(layout, schemas.join("shuangpin/xiaohe.toml")).unwrap();
        std::fs::write(
            schemas.join("sp/mini.dict.yaml"),
            format!("---\nname: mini\nversion: \"1\"\nsort: by_weight\n...\n{DICT}{more_dict}"),
        )
        .unwrap();
        std::fs::write(schemas.join("aux_code/t.txt"), format!("{AUX}{more_aux}")).unwrap();
        let pinyin = if scheme.is_empty() {
            String::new()
        } else {
            format!(
                "[engine.pinyin]\nscheme = \"{scheme}\"\n[engine.pinyin.shuangpin]\nlayout = \"xiaohe\"\n"
            )
        };
        std::fs::write(
            schemas.join("sp.schema.toml"),
            format!(
                "[schema]\nid = \"sp\"\nname = \"sp\"\n[engine]\ntype = \"pinyin\"\n{pinyin}\
                 [engine.aux_code]\nfiles = [\"aux_code/t.txt\"]\n{aux}\
                 [key_actions]\nbacktick = \"aux_code\"\n\
                 [[dictionaries]]\nid = \"mini\"\npath = \"sp/mini.dict.yaml\"\n\
                 type = \"rime_pinyin\"\ndefault = true\n"
            ),
        )
        .unwrap();
        (dir.clone(), Fixture { dir })
    }

    fn coord(tag: &str, data_dir: &std::path::Path) -> (Arc<Coordinator>, Arc<Store>) {
        coord_with(tag, data_dir, |_| {})
    }

    /// [`coord`]，另改一处配置。
    fn coord_with(
        tag: &str,
        data_dir: &std::path::Path,
        tweak: impl FnOnce(&mut Config),
    ) -> (Arc<Coordinator>, Arc<Store>) {
        let path =
            std::env::temp_dir().join(format!("wind_direct_aux_{tag}_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let mut cfg = Config::default();
        cfg.schema.available = vec!["sp".into()];
        cfg.schema.active = "sp".into();
        cfg.input.default.chinese_mode = true;
        // 出厂 L2 开着拼音调频，`Config::default()` 是类型默认（关）——不开就验不到记账码。
        cfg.schema.pinyin.frequency.enabled = true;
        tweak(&mut cfg);
        let c = Coordinator::new_headless_with_store(cfg, Some(data_dir), store.clone());
        (c, store)
    }

    fn press(c: &Coordinator, vk: u32) -> KeyAction {
        c.handle_key_event(&KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        })
    }

    fn type_str(c: &Coordinator, s: &str) {
        for ch in s.chars() {
            press(c, keymap::VK_A + (ch as u32 - 'a' as u32));
        }
    }

    fn texts(c: &Coordinator) -> Vec<String> {
        c.state
            .lock()
            .unwrap()
            .candidates
            .iter()
            .map(|c| c.text.clone())
            .collect()
    }

    const ON: &str = "enabled = true\ndirect = true\n";

    /// 界面候选的（文字 / 消费数 / code）指纹。
    fn fingerprint(c: &Coordinator) -> Vec<String> {
        c.state
            .lock()
            .unwrap()
            .candidates
            .iter()
            .map(|c| format!("{}/{}/{}", c.text, c.consumed_length, c.code))
            .collect()
    }

    /// 界面首选的（文字 / 消费数）。
    fn top_tc(c: &Coordinator) -> Option<String> {
        c.state
            .lock()
            .unwrap()
            .candidates
            .first()
            .map(|c| format!("{}/{}", c.text, c.consumed_length))
    }

    /// 清空组合后敲入 `input`，返回界面候选指纹。
    fn fp_after(c: &Coordinator, input: &str) -> Vec<String> {
        press(c, keymap::VK_ESCAPE);
        type_str(c, input);
        fingerprint(c)
    }

    /// 逃生口：`[engine.aux_code] continuous = false` 退回串尾直接辅助。下面钉「词组任一字」
    /// （`uidu`+`p`、释 = pl）的老用例都跑在它下面：连续模式里那条规则已不做。
    const TAIL_ONLY: &str = "enabled = true\ndirect = true\ncontinuous = false\n";

    /// ★ 连续模式不生效的三种配置（`direct` 关 / `enabled` 关 / 全拼）：界面候选（文字 / 消费数 /
    /// code）与接入连续辅码之前逐字节相同。期望值是接入前用同一夹具录下的。
    #[test]
    fn continuous_off_is_byte_identical() {
        let sp_off = [
            (
                "nihchn",
                vec![
                    "你好/4/nihao",
                    "拟好/4/nihao",
                    "你号/4/nihao",
                    "你/2/ni",
                    "拟/2/ni",
                ],
            ),
            (
                "uidup",
                vec![
                    "湿度/4/shidu",
                    "适度/4/shidu",
                    "十度/4/shidu",
                    "试读/4/shidu",
                    "释读/4/shidu",
                    "是/2/shi",
                    "十/2/shi",
                    "湿/2/shi",
                    "释/2/shi",
                ],
            ),
            ("nitwhc", vec!["你/2/ni", "拟/2/ni"]),
            (
                "goqkg",
                vec![
                    "国庆国/5/guoqingg",
                    "国庆/4/guoqing",
                    "国情/4/guoqing",
                    "国/2/guo",
                ],
            ),
            ("xlrn", vec!["想/2/xiang", "向/2/xiang", "像/2/xiang"]),
        ];
        for (tag, aux) in [
            ("co_off_direct", "enabled = true\n"),
            ("co_off_total", "enabled = false\ndirect = true\n"),
        ] {
            let (dir, _f) = fixture(tag, "shuangpin", aux);
            let (c, _) = coord(tag, &dir);
            for (input, want) in &sp_off {
                assert_eq!(&fp_after(&c, input), want, "{tag} {input}");
            }
        }
        let (dir, _f) = fixture("co_quanpin", "", ON);
        let (c, _) = coord("co_quanpin", &dir);
        for (input, want) in [
            (
                "nihaohn",
                vec![
                    "你好/5/nihao",
                    "拟好/5/nihao",
                    "你号/5/nihao",
                    "你/2/ni",
                    "拟/2/ni",
                ],
            ),
            (
                "shidup",
                vec![
                    "湿度/5/shidu",
                    "适度/5/shidu",
                    "十度/5/shidu",
                    "试读/5/shidu",
                    "释读/5/shidu",
                    "是/3/shi",
                    "十/3/shi",
                    "湿/3/shi",
                    "释/3/shi",
                ],
            ),
            ("nitwhao", vec!["你/2/ni", "拟/2/ni"]),
        ] {
            assert_eq!(fp_after(&c, input), want, "quanpin {input}");
        }
    }

    /// ★ 逃生口关掉连续模式：候选与接入前开着 `direct` 的串尾直接辅助逐字节相同（期望值同上，
    /// 接入前录下）。
    #[test]
    fn continuous_escape_hatch_restores_tail() {
        let (dir, _f) = fixture("co_hatch", "shuangpin", TAIL_ONLY);
        let (c, _) = coord("co_hatch", &dir);
        for (input, want) in [
            (
                "nihchn",
                vec![
                    "你好/6/nihao",
                    "拟好/6/nihao",
                    "你号/4/nihao",
                    "你/2/ni",
                    "拟/2/ni",
                ],
            ),
            (
                "uidup",
                vec![
                    "释读/5/shidu",
                    "湿度/4/shidu",
                    "适度/4/shidu",
                    "十度/4/shidu",
                    "试读/4/shidu",
                    "是/2/shi",
                    "十/2/shi",
                    "湿/2/shi",
                    "释/2/shi",
                ],
            ),
            ("nitwhc", vec!["你/2/ni", "拟/2/ni"]),
            (
                "goqkk",
                vec!["国庆/5/guoqing", "国情/5/guoqing", "国/2/guo"],
            ),
            ("xlrn", vec!["像/4/xiang", "想/2/xiang", "向/2/xiang"]),
        ] {
            assert_eq!(fp_after(&c, input), want, "{input}");
        }
        assert!(c.continuous_aux_opts(&c.state.lock().unwrap()).is_none());
    }

    /// 连续模式：辅码打在词中（「拟」= tw，跟在 `ni` 后），界面首选即目标整句，消费整串；
    /// 上屏后缓冲为空。反向对照：同输入在串尾直接辅助下首选不是它。
    #[test]
    fn continuous_mid_sentence_aux() {
        let (dir, _f) = fixture("co_mid", "shuangpin", ON);
        let (c, _) = coord("co_mid", &dir);
        assert!(
            c.continuous_aux_opts(&c.state.lock().unwrap()).is_some(),
            "direct 开 + 双拼 + 就绪：连续模式生效"
        );
        let fp = fp_after(&c, "nitwhc");
        assert_eq!(top_tc(&c).as_deref(), Some("拟好/6"), "{fp:?}");
        match press(&c, keymap::VK_SPACE) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, "拟好"),
            other => panic!("应整体上屏：{other:?}"),
        }
        assert!(c.state.lock().unwrap().input_buffer.is_empty());
        let (dir, _f) = fixture("co_mid_tail", "shuangpin", TAIL_ONLY);
        let (c, _) = coord("co_mid_tail", &dir);
        type_str(&c, "nitwhc");
        assert_ne!(top_tc(&c).as_deref(), Some("拟好/6"));
    }

    /// 整句切换键（露 2 算 3）：过闸的整串整句（`nihhc`：你 + 辅码 h + 好）是池里第 1 名、界面首选
    /// （整句块按名次摆，它不带名次时会被末槽整句「你航 / 你行」压到第 3）；切换键把它换走，
    /// 转到末一格时从池里换进来的那条照样消费整串，上屏后缓冲为空（辅码字母不滞留）。
    #[test]
    fn continuous_sn_in_sentence_pool() {
        let (dir, _f) = fixture_with(
            "co_pool",
            "shuangpin",
            ON,
            "行\thang\t5000\n藏\tcang\t3000\n才\tcai\t3000\n你航\tni hang\t3000\n",
            "你=hz\n",
        );
        let (c, _) = coord_with("co_pool", &dir, |cfg| {
            cfg.schema.pinyin.sentence_count = 2;
            cfg.schema.pinyin.sentence_max_count = 3;
            cfg.schema.pinyin.sentence_cycle_key = "tab".into();
        });
        type_str(&c, "nihhc");
        let pool: Vec<(String, usize)> = {
            let s = c.state.lock().unwrap();
            s.sentence_pool
                .iter()
                .map(|c| (c.text.clone(), c.consumed_length))
                .collect()
        };
        assert_eq!(pool.first(), Some(&("你好".to_string(), 5)), "{pool:?}");
        assert!(pool.len() >= 2, "前提：池里另有整句：{pool:?}");
        assert_eq!(top_tc(&c).as_deref(), Some("你好/5"));
        // 转一圈：每一步都有一条从池里换进来的「你好」，它消费整串。
        for _ in 1..pool.len() {
            press(&c, keymap::VK_TAB);
            assert_ne!(top_tc(&c).as_deref(), Some("你好/5"), "切换键换走了首位");
        }
        let at = c
            .state
            .lock()
            .unwrap()
            .candidates
            .iter()
            .position(|x| x.text == "你好")
            .expect("末一格窗口里池首那条在候选里");
        assert_eq!(fingerprint(&c)[at], "你好/5/nihao");
        match press(&c, keymap::VK_1 + at as u32) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, "你好"),
            other => panic!("应整体上屏：{other:?}"),
        }
        assert!(c.state.lock().unwrap().input_buffer.is_empty());
    }

    /// 整串整句进池后，露 N 算 3（N = 1、2）：带名次的候选不多于 N（整句块与切换窗口都按它算），
    /// 每按一次切换键窗口仍是 N 条，转满一圈回到「你好/5」居首。
    #[test]
    fn continuous_sn_pool_window_keeps_show_count() {
        let (dir, _f) = fixture_with(
            "co_pool_show",
            "shuangpin",
            ON,
            "行\thang\t5000\n藏\tcang\t3000\n才\tcai\t3000\n你航\tni hang\t3000\n",
            "你=hz\n",
        );
        for show in [1u8, 2] {
            let (c, _) = coord_with(&format!("co_pool_show{show}"), &dir, |cfg| {
                cfg.schema.pinyin.sentence_count = show;
                cfg.schema.pinyin.sentence_max_count = 3;
                cfg.schema.pinyin.sentence_cycle_key = "tab".into();
            });
            type_str(&c, "nihhc");
            let ranked = || -> Vec<(String, u8)> {
                let s = c.state.lock().unwrap();
                s.candidates
                    .iter()
                    .filter(|x| x.sentence_rank > 0)
                    .map(|x| (x.text.clone(), x.sentence_rank))
                    .collect()
            };
            let k = c.state.lock().unwrap().sentence_pool.len();
            assert!(k > show as usize, "前提：池比露的多（露 {show}）：{k}");
            assert_eq!(top_tc(&c).as_deref(), Some("你好/5"), "露 {show}");
            let r = ranked();
            assert!(r.len() <= show as usize, "露 {show}：{r:?}");
            for step in 1..=k {
                press(&c, keymap::VK_TAB);
                let r = ranked();
                assert!(r.len() <= show as usize, "露 {show} 第 {step} 次：{r:?}");
                if step < k {
                    assert_ne!(
                        top_tc(&c).as_deref(),
                        Some("你好/5"),
                        "露 {show} 第 {step} 次"
                    );
                }
            }
            assert_eq!(top_tc(&c).as_deref(), Some("你好/5"), "露 {show}：转满一圈");
        }
    }

    /// 连续模式整句候选的 code 去掉了辅码（`nitwhc` → `nihao`，不是双拼转换读出的 `niteihao`），
    /// 词频随它记在 `nihao` 下。
    #[test]
    fn continuous_sentence_freq_goes_to_aux_free_code() {
        let (dir, _f) = fixture("co_freq", "shuangpin", ON);
        let (c, store) = coord("co_freq", &dir);
        type_str(&c, "nitwhc");
        assert_eq!(top_tc(&c).as_deref(), Some("拟好/6"));
        match press(&c, keymap::VK_SPACE) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, "拟好"),
            other => panic!("应整体上屏：{other:?}"),
        }
        assert!(
            store.get_freq("pinyin", "nihao", "拟好").unwrap().is_some(),
            "应记在无辅码的 nihao 下"
        );
        assert!(
            store
                .get_freq("pinyin", "niteihao", "拟好")
                .unwrap()
                .is_none(),
            "带辅码读音的码不能进词频"
        );
    }

    /// 连续模式辅码候选的编码栏：音节之间 `'`、辅码前空格（`ni tw'hc`、串尾 `ni'hc hn`），
    /// 界面候选的 code 是去辅码的拼音码；高亮移到不带辅码的候选时回到双拼分段。
    #[test]
    fn continuous_aux_preedit_shape() {
        let (dir, _f) = fixture("co_shape", "shuangpin", ON);
        let (c, _) = coord("co_shape", &dir);
        let fp = fp_after(&c, "nitwhc");
        assert_eq!(
            fp.first().map(String::as_str),
            Some("拟好/6/nihao"),
            "{fp:?}"
        );
        assert_eq!(c.debug_preedit(), "ni tw'hc");
        let fp = fp_after(&c, "nihchn");
        assert_eq!(
            fp.first().map(String::as_str),
            Some("你好/6/nihao"),
            "{fp:?}"
        );
        assert_eq!(c.debug_preedit(), "ni'hc hn");
        // 高亮移到第一条不带辅码的候选。
        let plain = {
            let st = c.state.lock().unwrap();
            st.candidates
                .iter()
                .position(|x| x.meta.aux_keys.is_none() && !x.is_direct_aux)
                .expect("应有不带辅码的候选")
        };
        for _ in 0..plain {
            press(&c, keymap::VK_DOWN);
        }
        assert_eq!(c.debug_preedit(), "ni'hc'hn");
    }

    /// 编码栏辅码分隔符走方案内部项 `preedit_separator`：引擎辅码整句与串尾直接辅助同一项；
    /// 空串时辅码直接贴在音节后；白名单外的值（字母、`'`、`;`）不用、退回默认空格。
    #[test]
    fn continuous_preedit_separator_setting() {
        for (tag, line, mid, tail) in [
            (
                "co_sep_empty",
                "preedit_separator = \"\"\n",
                "nitw'hc",
                "ui'duy",
            ),
            (
                "co_sep_dash",
                "preedit_separator = \"-\"\n",
                "ni-tw'hc",
                "ui'du-y",
            ),
            (
                "co_sep_bad",
                "preedit_separator = \"x\"\n",
                "ni tw'hc",
                "ui'du y",
            ),
            (
                "co_sep_quote",
                "preedit_separator = \"'\"\n",
                "ni tw'hc",
                "ui'du y",
            ),
            (
                "co_sep_semicolon",
                "preedit_separator = \";\"\n",
                "ni tw'hc",
                "ui'du y",
            ),
        ] {
            let (dir, _f) = fixture(tag, "shuangpin", &format!("{ON}{line}"));
            let (c, _) = coord(tag, &dir);
            fp_after(&c, "nitwhc");
            assert_eq!(c.debug_preedit(), mid, "{tag}");
            let t = fp_after(&c, "uiduy");
            assert_eq!(t.first().map(String::as_str), Some("试读/5/shidu"), "{tag}");
            assert_eq!(c.debug_preedit(), tail, "{tag}");
        }
        // 逃生口下的串尾直接辅助同样走这一项。
        let (dir, _f) = fixture(
            "co_sep_tail",
            "shuangpin",
            &format!("{TAIL_ONLY}preedit_separator = \"\"\n"),
        );
        let (c, _) = coord("co_sep_tail", &dir);
        type_str(&c, "uidup");
        assert_eq!(c.debug_preedit(), "ui'dup");
    }

    /// 带辅码候选高亮时，光标在缓冲里逐位移动，编码栏里的插入符位置单调前进，且插入符之前的
    /// 显示串去掉分隔符后恰是缓冲的那一段。
    #[test]
    fn aux_preedit_cursor_maps() {
        let (dir, _f) = fixture("co_caret", "shuangpin", ON);
        let (c, _) = coord("co_caret", &dir);
        type_str(&c, "nitwhc");
        let mut st = c.state.lock().unwrap();
        assert_eq!(st.preedit, "ni tw'hc");
        let mut last = None;
        for pos in 0..=st.input_buffer.len() {
            st.input_cursor_pos = pos;
            let d = c.ui_caret_bytes(&st);
            assert!(last.is_none_or(|l| d > l), "pos {pos}: {d} 不单调");
            last = Some(d);
            let shown: String = st.preedit[..d]
                .chars()
                .filter(|&ch| ch != '\'' && ch != ' ')
                .collect();
            assert_eq!(shown, st.input_buffer[..pos], "pos {pos}");
        }
    }

    /// 分段上屏（钉现状，待确认 #5）：`nirxhc` 首选「你好」把 `rx` 当「你」的辅码；改选不带辅码的
    /// 单字「你」时不连带吞掉 `rx`，剩余缓冲 `rxhc` 照普通输入重解（候选同单打 `rxhc`）。
    #[test]
    fn partial_commit_leaves_aux_letters() {
        let (dir, _f) = fixture("co_partial", "shuangpin", ON);
        let (c, _) = coord("co_partial", &dir);
        let fresh = fp_after(&c, "rxhc");
        let fp = fp_after(&c, "nirxhc");
        assert_eq!(
            fp.first().map(String::as_str),
            Some("你好/6/nihao"),
            "{fp:?}"
        );
        let mut st = c.state.lock().unwrap();
        let (idx, ni) = st
            .candidates
            .iter()
            .enumerate()
            .find(|(_, x)| x.text == "你" && x.consumed_length == 2)
            .map(|(i, x)| (i, x.clone()))
            .expect("单字「你」");
        assert!(ni.meta.aux_keys.is_none());
        c.commit_selected(&mut st, &ni, idx as i32);
        assert_eq!(st.committed_text, "你");
        assert_eq!(st.input_buffer, "rxhc");
        let rest: Vec<String> = st
            .candidates
            .iter()
            .map(|x| format!("{}/{}/{}", x.text, x.consumed_length, x.code))
            .collect();
        assert_eq!(rest, fresh);
    }

    /// 候选调整按输入的归一码（`shadow_code`）记、按文字匹配，与候选 `code` 无关：对辅码整句
    /// 置顶 / 让别的词置顶都照常生效，置顶后的辅码整句身份（消费数、码、编码栏）不变。
    #[test]
    fn continuous_shadow_pin_on_aux_sentence() {
        let (dir, _f) = fixture("co_shadow", "shuangpin", ON);
        let (c, store) = coord("co_shadow", &dir);
        fp_after(&c, "nitwhc");
        let (schema, key) = {
            let st = c.state.lock().unwrap();
            (c.engine_mgr.data_schema_id("sp"), st.shadow_code.clone())
        };
        assert!(!key.is_empty());
        store.pin_shadow(&schema, &key, "你", None, 0).unwrap();
        let fp = fp_after(&c, "nitwhc");
        assert_eq!(fp.first().map(String::as_str), Some("你/2/ni"), "{fp:?}");
        store.pin_shadow(&schema, &key, "拟好", None, 0).unwrap();
        let fp = fp_after(&c, "nitwhc");
        assert_eq!(
            fp.first().map(String::as_str),
            Some("拟好/6/nihao"),
            "{fp:?}"
        );
        assert_eq!(c.debug_preedit(), "ni tw'hc");
    }

    /// 上屏带辅码的合成整句后，自动造的词记在去辅码的拼音码下（`hcnirx` → 好你，记 `haoni`）。
    #[test]
    fn learn_code_has_no_aux_letters() {
        let (dir, _f) = fixture("co_learn", "shuangpin", ON);
        let (c, store) = coord_with("co_learn", &dir, |cfg| {
            cfg.schema.pinyin.auto_learn.enabled = true;
        });
        type_str(&c, "hcnirx");
        let top = {
            let st = c.state.lock().unwrap();
            st.candidates.first().cloned().expect("应有候选")
        };
        assert_eq!(
            (top.text.as_str(), top.consumed_length, top.code.as_str()),
            ("好你", 6, "haoni"),
            "前提：串尾辅码 rx 属「你」"
        );
        assert!(top.is_synthesized && top.meta.aux_keys.is_some());
        match press(&c, keymap::VK_SPACE) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, "好你"),
            other => panic!("应整体上屏：{other:?}"),
        }
        let learned = store.get_temp_words("pinyin", "haoni").unwrap();
        assert!(learned.iter().any(|w| w.text == "好你"), "{learned:?}");
        for code in ["haoniruan", "haonirua", "haonir"] {
            assert!(
                store.get_temp_words("pinyin", code).unwrap().is_empty(),
                "{code}"
            );
        }
        assert!(store.get_freq("pinyin", "haoni", "好你").unwrap().is_some());
    }

    /// 连续模式下串尾一步只认**末字**（辅码限定紧挨在它前面的那个字，与万象 Pro 同）：
    /// 末字 1 位（`uidu`+`y`，读 = yd）、单字 1 位（`xl`+`r` → 像）照旧提前，编码栏照旧；
    /// 「词组任一字」（`uidu`+`p` 的释 = pl、`goqk`+`k` 的国 = ky）与单字 2 位（`xl`+`rn`）不再提前
    /// （万象需 `/` 才提前）。
    #[test]
    fn tail_rules_in_continuous_are_last_char() {
        let (dir, _f) = fixture("co_tail", "shuangpin", ON);
        let (c, _) = coord("co_tail", &dir);
        let t = fp_after(&c, "uiduy");
        assert_eq!(&t[..2], ["试读/5/shidu", "释读/5/shidu"], "{t:?}");
        assert_eq!(c.debug_preedit(), "ui'du y");
        let t = fp_after(&c, "xlrn");
        assert!(
            c.state
                .lock()
                .unwrap()
                .candidates
                .iter()
                .all(|c| !c.is_direct_aux),
            "单字 2 位不提前：{t:?}"
        );
        assert_ne!(t.first().map(String::as_str), Some("像/4/xiang"), "{t:?}");
        let t = fp_after(&c, "xlr");
        assert_eq!(t.first().map(String::as_str), Some("像/3/xiang"), "{t:?}");
        let t = fp_after(&c, "goqkg");
        assert_eq!(
            t.first().map(String::as_str),
            Some("国庆/5/guoqing"),
            "{t:?}"
        );
        assert!(!t.iter().any(|x| x.starts_with("国情/5")), "{t:?}");
        // 任一字规则不再生效。
        let t = fp_after(&c, "uidup");
        assert!(!t.iter().any(|x| x == "释读/5/shidu"), "{t:?}");
        let t = fp_after(&c, "goqkk");
        assert!(
            !t.iter().any(|x| x.ends_with("/5/guoqing")),
            "国 = ky 是首字，不再提前：{t:?}"
        );
    }

    /// 连续模式下串尾命中的上屏与编码栏：整体上屏、组码清空，词频记前缀的拼音码（`shidu`）；
    /// 高亮移到非命中项回到普通分段；单字模式下命中项照样过单字过滤。
    #[test]
    fn continuous_tail_hit_commit_preedit_single_char() {
        let (dir, _f) = fixture("co_commit", "shuangpin", ON);
        let (c, store) = coord("co_commit", &dir);
        type_str(&c, "uiduy");
        assert_eq!(c.debug_preedit(), "ui'du y");
        press(&c, keymap::VK_DOWN);
        press(&c, keymap::VK_DOWN);
        {
            let st = c.state.lock().unwrap();
            let hi = c.highlighted_global_index(&st);
            assert!(!st.candidates[hi].is_direct_aux, "第 3 位应是普通候选");
        }
        assert_eq!(c.debug_preedit(), "ui'du'y");
        press(&c, keymap::VK_ESCAPE);
        type_str(&c, "uiduy");
        match press(&c, keymap::VK_SPACE) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, "试读"),
            other => panic!("应整体上屏：{other:?}"),
        }
        assert!(c.state.lock().unwrap().input_buffer.is_empty());
        assert!(store.get_freq("pinyin", "shidu", "试读").unwrap().is_some());
        assert!(
            store
                .get_freq("pinyin", "shiduy", "试读")
                .unwrap()
                .is_none()
        );
        type_str(&c, "xlr");
        c.set_single_char(true);
        let t = texts(&c);
        assert!(t.iter().all(|x| x.chars().count() == 1), "{t:?}");
        assert_eq!(t.first().map(String::as_str), Some("像"), "{t:?}");
    }

    /// 光标中间编辑与首单元读法另取整串的双拼分段（`convert(..).preedit_pinyin`），不传连续
    /// 选项：分段来自双拼转换、与击键图无关，两种取法相同。
    #[test]
    fn continuous_preedit_split_same_as_plain_convert() {
        let (dir, _f) = fixture("co_split", "shuangpin", ON);
        let (c, _) = coord("co_split", &dir);
        type_str(&c, "n");
        let st = c.state.lock().unwrap();
        let lk = c.continuous_aux_lookup(&st).expect("连续模式生效").lookup;
        drop(st);
        for input in ["nitwhc", "uiduy", "xlrn", "goqkgd", "nitwh"] {
            let plain = c.engine_mgr.convert(input, 50).preedit_pinyin;
            let cont = c
                .engine_mgr
                .convert_active_with_opts(input, 50, super::continuous_opts(&lk, None))
                .preedit_pinyin;
            assert_eq!(cont, plain, "{input}");
        }
    }

    /// 引擎辅码整句与串尾命中同文同消费数时只出现一次（`nihchn`：好 = hn，你好 / 6）。
    #[test]
    fn tail_dedup_with_engine_sentence() {
        let (dir, _f) = fixture("co_dedup", "shuangpin", ON);
        let (c, _) = coord("co_dedup", &dir);
        let t = fp_after(&c, "nihchn");
        assert_eq!(t.first().map(String::as_str), Some("你好/6/nihao"), "{t:?}");
        assert_eq!(
            t.iter().filter(|x| x.starts_with("你好/")).count(),
            1,
            "{t:?}"
        );
    }

    /// 临拼 / 引导键辅助码态等非主输入路：不传连续辅码选项。
    #[test]
    fn continuous_respects_overlay() {
        let (dir, _f) = fixture("co_overlay", "shuangpin", ON);
        let (c, _) = coord("co_overlay", &dir);
        type_str(&c, "nitwhc");
        let mut st = c.state.lock().unwrap();
        assert!(c.continuous_aux_opts(&st).is_some(), "反向对照");
        for mode in [ModeKind::AuxCode, ModeKind::TempPinyin] {
            st.active = Some(mode);
            assert!(c.continuous_aux_opts(&st).is_none(), "{mode:?}");
        }
    }

    /// 翻页扩容只重跑 `build_candidates`：走同一份连续选项，首屏不变。
    #[test]
    fn continuous_expand_uses_same_opts() {
        let (dir, _f) = fixture("co_page", "shuangpin", ON);
        let (c, _) = coord("co_page", &dir);
        let before = fp_after(&c, "nitwhc");
        let mut st = c.state.lock().unwrap();
        st.has_more = true;
        c.expand_candidates(&mut st);
        drop(st);
        let after = fingerprint(&c);
        assert_eq!(after[..before.len().min(5)], before[..before.len().min(5)]);
        assert_eq!(top_tc(&c).as_deref(), Some("拟好/6"));
    }

    /// 组合中途改设置（设置页写方案覆盖层 → `aux_settings_gen` 推进）：关 `direct` / 开逃生口后
    /// 下一键回旧路径，串尾快照不得跨模式复用；改回后恢复。
    #[test]
    fn continuous_invalidates_on_settings_change() {
        let (dir, _f) = fixture("co_inval", "shuangpin", ON);
        let ov = dir.join("overrides");
        std::fs::create_dir_all(&ov).unwrap();
        let path = std::env::temp_dir().join(format!(
            "wind_direct_aux_co_inval_{}.redb",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let mut cfg = Config::default();
        cfg.schema.available = vec!["sp".into()];
        cfg.schema.active = "sp".into();
        cfg.input.default.chinese_mode = true;
        let c = Coordinator::new_headless_with_store_override(cfg, Some(&dir), store, Some(ov));
        let write = |s: &str| {
            let v: toml::Value = s.parse().unwrap();
            c.engine_mgr.write_schema_override("sp", &v).unwrap();
        };
        let top = |c: &Coordinator| fingerprint(c).first().cloned();
        type_str(&c, "nitwhc");
        let continuous_snapshot = c
            .state
            .lock()
            .unwrap()
            .direct_aux_prev
            .as_ref()
            .map(|p| p.continuous);
        assert_eq!(continuous_snapshot, Some(true), "连续模式下取的快照");
        // 前缀 `uidu` 的快照在连续模式下取，切到逃生口后下一键不得复用（要按串尾模式重取）。
        press(&c, keymap::VK_ESCAPE);
        type_str(&c, "uidu");
        // 辅码设置 / 来源的代次也在复用判据里：改了设置，旧快照的代次与当下对不上。
        let snap_gen = c
            .state
            .lock()
            .unwrap()
            .direct_aux_prev
            .as_ref()
            .map(|p| p.data_gen);
        write("[engine.aux_code]\ncontinuous = true\n");
        assert_ne!(
            snap_gen,
            Some(c.direct_aux_data_gen()),
            "设置变了，快照代次作废"
        );
        write("[engine.aux_code]\ncontinuous = false\n");
        type_str(&c, "p");
        let prev = c
            .state
            .lock()
            .unwrap()
            .direct_aux_prev
            .as_ref()
            .map(|p| p.continuous);
        assert_eq!(prev, Some(false), "跨模式不复用快照");
        assert_eq!(top(&c).as_deref(), Some("释读/5/shidu"));
        write("[engine.aux_code]\ncontinuous = true\n");
        press(&c, keymap::VK_ESCAPE);
        type_str(&c, "nitwhc");
        for (ov, want_continuous) in [
            ("[engine.aux_code]\ncontinuous = false\n", false),
            ("[engine.aux_code]\ndirect = false\n", false),
            ("[engine.aux_code]\ncontinuous = true\n", true),
        ] {
            write(ov);
            press(&c, keymap::VK_BACK);
            type_str(&c, "c");
            assert_eq!(
                c.continuous_aux_opts(&c.state.lock().unwrap()).is_some(),
                want_continuous,
                "{ov}"
            );
            let got = top(&c);
            if want_continuous {
                assert_eq!(top_tc(&c).as_deref(), Some("拟好/6"), "{ov}");
            } else {
                assert_eq!(got.as_deref(), Some("你/2/ni"), "{ov}: 回旧路径");
            }
        }
    }

    /// 基线：同一夹具，不开直接辅助码时 `uidup` 的首选不是「释读」——否则下面的正向用例测不出东西。
    #[test]
    fn baseline_without_direct() {
        let (dir, _f) = fixture("base", "shuangpin", "enabled = true\n");
        let (c, _) = coord("base", &dir);
        type_str(&c, "uidu");
        assert_eq!(texts(&c).first().map(String::as_str), Some("湿度"));
        type_str(&c, "p");
        assert_ne!(texts(&c).first().map(String::as_str), Some("释读"));
    }

    /// 奇数长度、前缀 2 音节：命中项排最前；组码区显示「前缀 + 空格 + 辅码」；上屏连辅码一起
    /// 吃掉、组码清空；调频记在前缀的拼音编码下（`shidu`），辅码字母不进词频。
    #[test]
    fn odd_hit_goes_first_and_commits_whole_buffer() {
        let (dir, _f) = fixture("odd", "shuangpin", TAIL_ONLY);
        let (c, store) = coord("odd", &dir);
        type_str(&c, "uidup");
        let t = texts(&c);
        assert_eq!(t.first().map(String::as_str), Some("释读"), "{t:?}");
        assert!(t.contains(&"湿度".to_string()), "其余候选去重接后：{t:?}");
        assert_eq!(t.iter().filter(|x| *x == "释读").count(), 1, "不重复");
        assert_eq!(
            c.debug_preedit(),
            "ui'du p",
            "高亮命中项：前缀音节 + 空格 + 辅码"
        );
        let act = press(&c, keymap::VK_SPACE);
        match act {
            KeyAction::InsertText { text, .. } => assert_eq!(text, "释读"),
            other => panic!("应整体上屏：{other:?}"),
        }
        let st = c.state.lock().unwrap();
        assert!(st.input_buffer.is_empty(), "上屏连辅码一起吃掉，组码清空");
        assert!(st.direct_aux_body.is_empty());
        drop(st);
        assert!(
            store.get_freq("pinyin", "shidu", "释读").unwrap().is_some(),
            "词频记在前缀的拼音编码下"
        );
        assert!(
            store
                .get_freq("pinyin", "shidup", "释读")
                .unwrap()
                .is_none()
        );
    }

    /// 连打时前缀取「几键前那一键」的候选快照；快照缺失（退格改了前缀、光标中间编辑…）
    /// 时对前缀单独解码兜底。两条路给出同样的命中。
    #[test]
    fn prefix_snapshot_and_fallback_decode_agree() {
        let (dir, _f) = fixture("snap", "shuangpin", TAIL_ONLY);
        let (c, _) = coord("snap", &dir);
        type_str(&c, "uidu");
        {
            let st = c.state.lock().unwrap();
            let prev = st.direct_aux_prev.as_ref().expect("整音节输入应留下快照");
            assert_eq!(prev.input, "uidu");
        }
        type_str(&c, "p");
        let via_snapshot = texts(&c);
        // 奇数键不动快照，留给紧随其后的偶数键。
        assert_eq!(
            c.state
                .lock()
                .unwrap()
                .direct_aux_prev
                .as_ref()
                .map(|p| p.input.clone()),
            Some("uidu".into())
        );
        press(&c, keymap::VK_BACK);
        c.state.lock().unwrap().direct_aux_prev = None;
        type_str(&c, "p");
        assert_eq!(texts(&c), via_snapshot, "兜底解码与快照给出同样的候选");
        assert_eq!(via_snapshot.first().map(String::as_str), Some("释读"));
    }

    /// ★ 输入不变而词库变了：候选右键删掉一个是命中项的用户词，原地重建（输入没变）时
    /// 前缀快照还是删之前那份，不作废的话删掉的词照样顶在首位。
    #[test]
    fn deleting_user_word_hit_drops_prefix_snapshot() {
        let (dir, _f) = fixture("deluser", "shuangpin", TAIL_ONLY);
        let (c, store) = coord("deluser", &dir);
        // 释毒：用户词，释=pl 命中辅码 p；权重垫底，不开直接辅助时排不到前面。
        store
            .add_user_word("pinyin", "shidu", "释毒", 0, 0)
            .unwrap();
        type_str(&c, "uidup");
        let t = texts(&c);
        let idx = t
            .iter()
            .position(|x| x == "释毒")
            .expect("用户词应是命中项");
        assert!(
            c.state.lock().unwrap().candidates[idx].is_direct_aux,
            "{t:?}"
        );
        c.candidate_op(wind_ui_types::CandidateOp::Delete, idx);
        let t = texts(&c);
        assert!(
            !t.contains(&"释毒".to_string()),
            "删掉的用户词不得再出现：{t:?}"
        );
        assert_eq!(t.first().map(String::as_str), Some("释读"), "{t:?}");
    }

    /// 同上的通用一面：组合中途词库被别处写了（设置页加词），下一键不得复用旧快照。
    #[test]
    fn store_write_mid_composition_invalidates_snapshot() {
        let (dir, _f) = fixture("addmid", "shuangpin", TAIL_ONLY);
        let (c, store) = coord("addmid", &dir);
        type_str(&c, "uidu");
        store
            .add_user_word("pinyin", "shidu", "释毒", 0, 0)
            .unwrap();
        type_str(&c, "p");
        let t = texts(&c);
        let hit = c
            .state
            .lock()
            .unwrap()
            .candidates
            .iter()
            .any(|x| x.text == "释毒" && x.is_direct_aux);
        assert!(hit, "新加的用户词应是命中项：{t:?}");
    }

    /// ★ 输入不变而切了单字模式：命中项同样要过单字过滤。快照只按前缀串认的话，会拿
    /// 切换前那份（含词）直接复用，「释读」照样顶在单字模式的首位。
    #[test]
    fn single_char_toggle_filters_hits_too() {
        let (dir, _f) = fixture("single", "shuangpin", TAIL_ONLY);
        let (c, _) = coord("single", &dir);
        type_str(&c, "uidup");
        assert_eq!(texts(&c).first().map(String::as_str), Some("释读"));
        c.set_single_char(true);
        let t = texts(&c);
        assert!(
            t.iter().all(|x| x.chars().count() == 1),
            "单字模式下不得有词：{t:?}"
        );
        // 反向对照：单字模式下直接辅助照常工作（`uip` 前缀 shi，释 = pl 中 p）。
        press(&c, keymap::VK_ESCAPE);
        type_str(&c, "uip");
        let t = texts(&c);
        assert_eq!(
            t.first().map(String::as_str),
            Some("释"),
            "单字命中项照常提前：{t:?}"
        );
    }

    /// 分段上屏后缓冲换了一段，旧快照不再对应任何前缀，作废。
    #[test]
    fn partial_commit_drops_prefix_snapshot() {
        let (dir, _f) = fixture("partial", "shuangpin", ON);
        let (c, _) = coord("partial", &dir);
        // 奇数长度收尾：上屏后剩 `gok`，不是整音节输入，不会顺手覆盖快照。
        type_str(&c, "uidugok");
        let mut st = c.state.lock().unwrap();
        let shi = st
            .candidates
            .iter()
            .find(|c| c.text == "湿度")
            .cloned()
            .expect("湿度");
        assert_eq!(shi.consumed_length, 4, "分段候选");
        c.commit_selected(&mut st, &shi, 0);
        assert_eq!(st.input_buffer, "gok");
        let stale = st
            .direct_aux_prev
            .as_ref()
            .is_some_and(|p| !st.input_buffer.starts_with(&p.input));
        assert!(!stale, "分段上屏后不得留着上屏前的快照");
    }

    /// 高亮移到非命中项时组码区回到普通双拼分段。
    #[test]
    fn preedit_follows_highlight() {
        let (dir, _f) = fixture("hl", "shuangpin", TAIL_ONLY);
        let (c, _) = coord("hl", &dir);
        type_str(&c, "uidup");
        assert_eq!(c.debug_preedit(), "ui'du p");
        press(&c, keymap::VK_DOWN);
        let st = c.state.lock().unwrap();
        let hi = c.highlighted_global_index(&st);
        assert!(!st.candidates[hi].is_direct_aux, "第 2 位应是普通候选");
        drop(st);
        assert_eq!(c.debug_preedit(), "ui'du'p", "普通候选：原双拼分段");
    }

    /// 偶数长度：`xlrn` = xiang + `rn`，像=rn。逃生口（串尾）下无全音节候选可保留时命中项排最前；
    /// 连续模式下单字 2 位不提前（万象需 `/` 才提前）。
    #[test]
    fn even_two_letter_aux() {
        let (dir, _f) = fixture("even", "shuangpin", TAIL_ONLY);
        let (c, _) = coord("even", &dir);
        type_str(&c, "xlrn");
        assert_eq!(
            texts(&c).first().map(String::as_str),
            Some("像"),
            "{:?}",
            texts(&c)
        );
        let (cdir, _f2) = fixture("even_c", "shuangpin", ON);
        let (cc, _) = coord("even_c", &cdir);
        type_str(&cc, "xlrn");
        assert!(
            cc.state
                .lock()
                .unwrap()
                .candidates
                .iter()
                .all(|c| !c.is_direct_aux),
            "连续模式：单字 2 位不提前：{:?}",
            texts(&cc)
        );
        // 国庆 / 国情：1 位辅码 k 两者都中（国=ky）；g 只中国庆（庆=gd，情=xo）。
        let (c, _) = coord("even2", &cdir);
        type_str(&c, "goqkg");
        let t = texts(&c);
        assert_eq!(t.first().map(String::as_str), Some("国庆"), "{t:?}");
        assert!(
            t.iter().position(|x| x == "国情") > Some(1),
            "国情不中 g，不被提前：{t:?}"
        );
    }

    /// 退格按原始按键串回滚：`uidup` 退一格回到 `uidu`，命中与组码区形态一并消失。
    #[test]
    fn backspace_rolls_back_to_plain_prefix() {
        let (dir, _f) = fixture("bs", "shuangpin", ON);
        let (c, _) = coord("bs", &dir);
        type_str(&c, "uidup");
        press(&c, keymap::VK_BACK);
        let st = c.state.lock().unwrap();
        assert_eq!(st.input_buffer, "uidu");
        assert!(st.direct_aux_body.is_empty());
        assert!(st.candidates.iter().all(|c| !c.is_direct_aux));
        assert_eq!(st.candidates.first().map(|c| c.text.as_str()), Some("湿度"));
    }

    /// 翻页扩容只重跑 `build_candidates`：命中项必须还在（插入点放在它里面的理由）。
    #[test]
    fn hits_survive_page_expansion() {
        let (dir, _f) = fixture("page", "shuangpin", TAIL_ONLY);
        let (c, _) = coord("page", &dir);
        type_str(&c, "uidup");
        let mut st = c.state.lock().unwrap();
        st.has_more = true;
        c.expand_candidates(&mut st);
        assert_eq!(st.candidates.first().map(|c| c.text.as_str()), Some("释读"));
        assert!(st.candidates[0].is_direct_aux);
    }

    /// 四种不生效：`direct` 关 / `enabled` 关 / 全拼 / 引导键态中。
    #[test]
    fn not_applicable_cases_leave_candidates_alone() {
        for (tag, scheme, aux) in [
            ("off_direct", "shuangpin", "enabled = true\n"),
            ("off_total", "shuangpin", "enabled = false\ndirect = true\n"),
            ("quanpin", "", ON),
        ] {
            let (dir, _f) = fixture(tag, scheme, aux);
            let (c, _) = coord(tag, &dir);
            type_str(&c, if scheme.is_empty() { "shidup" } else { "uidup" });
            let st = c.state.lock().unwrap();
            assert!(
                st.candidates.iter().all(|c| !c.is_direct_aux),
                "{tag}: 不应有直接辅助命中"
            );
            assert!(st.direct_aux_body.is_empty(), "{tag}");
        }
        // 引导键态：直接辅助停用（同一输入，门卫先看 `state.active`）。
        let (dir, _f) = fixture("guide", "shuangpin", TAIL_ONLY);
        let (c, _) = coord("guide", &dir);
        type_str(&c, "uidup");
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::AuxCode);
        let mut cands = vec![wind_candidate::Candidate {
            text: "湿度".into(),
            ..Default::default()
        }];
        c.apply_direct_aux(&mut st, &mut cands, 50, false, None);
        assert_eq!(cands.len(), 1);
        assert!(st.direct_aux_body.is_empty());
        st.active = None;
        drop(st);
        // 反向对照：同一 coordinator 在主输入路上确实会命中（上面那条不是因为别的原因空转）。
        assert_eq!(texts(&c).first().map(String::as_str), Some("释读"));
    }

    /// ★ 方案来源（`schema:<id>`）的系统层没就绪：本键不做直接辅助、候选原样，派后台构建；
    /// 建好后下一键即生效。按键线程绝不现建反查索引（与引导键进入同一处理）。
    #[test]
    fn schema_source_not_ready_is_noop_then_warms() {
        let id = format!("zz_da_cold_{}", std::process::id());
        struct CacheGuard(String);
        impl Drop for CacheGuard {
            fn drop(&mut self) {
                if let Some(cache) = Config::cache_dir() {
                    let _ = std::fs::remove_dir_all(cache.join(&self.0));
                }
            }
        }
        let _cg = CacheGuard(id.clone());
        let (dir, _f) = fixture("cold", "shuangpin", TAIL_ONLY);
        let schemas = dir.join("schemas");
        std::fs::create_dir_all(schemas.join(&id)).unwrap();
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"形\"\n[engine]\ntype = \"codetable\"\n\
                 [[dictionaries]]\nid = \"{id}_main\"\npath = \"{id}/{id}.dict.yaml\"\n\
                 type = \"rime_codetable\"\ndefault = true\n"
            ),
        )
        .unwrap();
        std::fs::write(
            schemas.join(format!("{id}/{id}.dict.yaml")),
            format!(
                "---\nname: {id}\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n\
                 pl\t释\t10\ndy\t湿\t10\nyd\t读\t10\ngy\t度\t10\n"
            ),
        )
        .unwrap();
        let sp = schemas.join("sp.schema.toml");
        let text = std::fs::read_to_string(&sp).unwrap();
        let swapped = text.replace(r#"["aux_code/t.txt"]"#, &format!(r#"["schema:{id}"]"#));
        assert_ne!(swapped, text);
        std::fs::write(&sp, swapped).unwrap();
        let (c, _) = coord("cold", &dir);
        type_str(&c, "uidup");
        // 「未就绪 ⇒ 原样」只在后台索引**此刻确实还没建好**时才能断言：词库只有四行，
        // 并行跑测试时后台线程偶尔抢在第一键之前建完（缓存构建路径少读一次源之后更常见），
        // 那时候选已经是辅助后的，按「未就绪」断言就是假红。就绪与否要看事实，不能当前提。
        if c.engine_mgr.reverse_index_if_ready(&id).is_none() {
            let st = c.state.lock().unwrap();
            assert!(
                st.candidates.iter().all(|c| !c.is_direct_aux),
                "未就绪：原样"
            );
            assert_ne!(st.candidates.first().map(|c| c.text.as_str()), Some("释读"));
        } else {
            eprintln!("后台索引已先于首键建好，跳过「未就绪」断言，只验就绪后生效");
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while c.engine_mgr.reverse_index_if_ready(&id).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "后台构建 5 秒内没建好"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        press(&c, keymap::VK_BACK);
        type_str(&c, "p");
        assert_eq!(
            texts(&c).first().map(String::as_str),
            Some("释读"),
            "就绪后下一键即生效"
        );
    }

    /// ★ 方案来源的反查索引未就绪：本键**整条**回旧路径（不传击键图、不做串尾），候选与
    /// `direct` 关时相同，并派后台构建；建好后下一键走连续路径。
    #[test]
    fn continuous_unready_falls_back() {
        let id = format!("zz_co_cold_{}", std::process::id());
        struct CacheGuard(String);
        impl Drop for CacheGuard {
            fn drop(&mut self) {
                if let Some(cache) = Config::cache_dir() {
                    let _ = std::fs::remove_dir_all(cache.join(&self.0));
                }
            }
        }
        let _cg = CacheGuard(id.clone());
        let (dir, _f) = fixture("co_cold", "shuangpin", ON);
        let schemas = dir.join("schemas");
        std::fs::create_dir_all(schemas.join(&id)).unwrap();
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"形\"\n[engine]\ntype = \"codetable\"\n\
                 [[dictionaries]]\nid = \"{id}_main\"\npath = \"{id}/{id}.dict.yaml\"\n\
                 type = \"rime_codetable\"\ndefault = true\n"
            ),
        )
        .unwrap();
        std::fs::write(
            schemas.join(format!("{id}/{id}.dict.yaml")),
            format!(
                "---\nname: {id}\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n\
                 rx\t你\t10\ntw\t拟\t10\nhn\t好\t10\nkd\t号\t10\n"
            ),
        )
        .unwrap();
        let sp = schemas.join("sp.schema.toml");
        let text = std::fs::read_to_string(&sp).unwrap();
        let swapped = text.replace(r#"["aux_code/t.txt"]"#, &format!(r#"["schema:{id}"]"#));
        assert_ne!(swapped, text);
        std::fs::write(&sp, swapped).unwrap();
        let (c, _) = coord("co_cold", &dir);
        // 放弃标记钉死「未就绪」：不起后台构建，不靠时序。
        c.engine_mgr.mark_reverse_index_skipped_for_test(&id, true);
        type_str(&c, "nitwhc");
        assert!(c.engine_mgr.reverse_index_if_ready(&id).is_none());
        assert!(
            c.continuous_aux_opts(&c.state.lock().unwrap()).is_none(),
            "未就绪：不传连续选项（不是「击键图开、无辅码」）"
        );
        assert_eq!(
            fingerprint(&c),
            ["你/2/ni", "拟/2/ni"],
            "未就绪：同 direct 关"
        );
        assert!(
            c.state
                .lock()
                .unwrap()
                .candidates
                .iter()
                .all(|c| !c.is_direct_aux)
        );
        // 解除标记、同步建好索引，下一键起走连续路径。
        c.engine_mgr.mark_reverse_index_skipped_for_test(&id, false);
        assert!(c.engine_mgr.prewarm_reverse_index(&id));
        press(&c, keymap::VK_BACK);
        type_str(&c, "c");
        assert!(c.continuous_aux_opts(&c.state.lock().unwrap()).is_some());
        assert_eq!(
            top_tc(&c).as_deref(),
            Some("拟好/6"),
            "就绪后下一键走连续路径"
        );
    }

    /// 连续模式下连打（取几键前的快照）与退格重打（快照作废、前缀兜底解码）给出同样的候选。
    #[test]
    fn continuous_snapshot_and_fallback_agree() {
        let (dir, _f) = fixture("co_snap", "shuangpin", ON);
        let (c, _) = coord("co_snap", &dir);
        for input in ["uiduy", "xlrn", "goqkg", "nihchn"] {
            let through = fp_after(&c, input);
            press(&c, keymap::VK_BACK);
            c.state.lock().unwrap().direct_aux_prev = None;
            type_str(&c, &input[input.len() - 1..]);
            assert_eq!(fingerprint(&c), through, "{input}");
        }
    }

    /// 直接辅助的结果上仍可按反引号进引导键筛选，筛的是当前整张候选表。
    #[test]
    fn guide_key_filters_current_list() {
        let (dir, _f) = fixture("guide2", "shuangpin", TAIL_ONLY);
        let (c, _) = coord("guide2", &dir);
        type_str(&c, "uidup");
        press(&c, keymap::VK_BACKTICK);
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        assert_eq!(st.candidates.first().map(|c| c.text.as_str()), Some("释读"));
    }
}

#[cfg(test)]
mod real_data_tests {
    //! 真实数据（`build_dev/data`：小鹤双拼 + 雾凇词库 + `flypy_full.txt`），缺数据跳过。
    //! 放在 crate 内而不是 `tests/`：要看候选的 `is_direct_aux` 标记，区分「被直接辅助提上来」
    //! 与「本来就排在那儿」。
    use crate::coordinator::Coordinator;
    use std::sync::Arc;
    use wind_bridge::handler::{KeyEventData, MessageHandler};
    use wind_config::Config;
    use wind_ipc::protocol::EVENT_KEY_DOWN;
    use wind_keys::keymap;

    fn data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
    }

    fn has_data() -> bool {
        let d = data_dir().join("schemas");
        let ok = d.join("shuangpin.schema.toml").is_file()
            && d.join("aux_code/flypy_full.txt").is_file()
            && d.join("pinyin/rime_frost.dict.yaml").is_file();
        if !ok {
            eprintln!(
                "跳过：build_dev/data 缺小鹤双拼方案 / flypy_full.txt / 雾凇词库（先跑 gen-data）"
            );
        }
        ok
    }

    fn coord(direct: bool) -> Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["shuangpin".into()];
        cfg.schema.active = "shuangpin".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.pinyin.aux_code.enabled = true;
        cfg.schema.pinyin.aux_code.direct = direct;
        Coordinator::new_headless(cfg, Some(&data_dir()))
    }

    /// `direct` 开 + 逃生口（`[engine.aux_code] continuous = false`）：只做串尾直接辅助。
    fn coord_tail() -> Arc<Coordinator> {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let ov = std::env::temp_dir().join(format!(
            "wind_direct_aux_tail_ov_{}_{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&ov).unwrap();
        std::fs::write(
            ov.join("shuangpin.toml"),
            "[engine.aux_code]\ncontinuous = false\n",
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec!["shuangpin".into()];
        cfg.schema.active = "shuangpin".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.pinyin.aux_code.enabled = true;
        cfg.schema.pinyin.aux_code.direct = true;
        Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov))
    }

    fn press(c: &Coordinator, vk: u32) {
        c.handle_key_event(&KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    }

    /// 清空组合后逐键敲入 `input`，每键后回调一次（参数为已敲前缀）。
    fn type_each(c: &Coordinator, input: &str, mut each: impl FnMut(&str)) {
        press(c, keymap::VK_ESCAPE);
        for (i, ch) in input.char_indices() {
            press(c, keymap::VK_A + (ch as u32 - 'a' as u32));
            each(&input[..i + 1]);
        }
    }

    /// 候选（文本, 是否直接辅助命中）。
    fn cands(c: &Coordinator) -> Vec<(String, bool)> {
        c.state
            .lock()
            .unwrap()
            .candidates
            .iter()
            .map(|c| (c.text.clone(), c.is_direct_aux))
            .collect()
    }

    fn after(c: &Coordinator, input: &str) -> Vec<(String, bool)> {
        type_each(c, input, |_| {});
        cands(c)
    }

    fn top5(v: &[(String, bool)]) -> Vec<String> {
        v.iter()
            .take(5)
            .map(|(t, hit)| if *hit { format!("{t}*") } else { t.clone() })
            .collect()
    }

    fn first(v: &[(String, bool)]) -> Option<&str> {
        v.first().map(|(t, _)| t.as_str())
    }

    /// §11 真实数据示例（串尾直接辅助，逃生口下）。`*` = 直接辅助命中项。开关两侧的前 5 名
    /// 打到 stderr 供报告。
    #[test]
    fn xiaohe_examples() {
        if !has_data() {
            return;
        }
        let (on, off) = (coord_tail(), coord(false));
        for input in ["uidup", "uidupl", "goqkk", "goqkg", "xlr", "jxl", "jxlm"] {
            eprintln!(
                "{input}: on {:?} | off {:?}",
                top5(&after(&on, input)),
                top5(&after(&off, input))
            );
        }
        // 释 = pl；同音的湿 dy、适 zk、十 al、试 yg 都不以 p 开头。
        assert_eq!(first(&after(&on, "uidup")), Some("释读"));
        assert_ne!(first(&after(&off, "uidup")), Some("释读"), "反向对照");
        // `pl` 不是合法小鹤音节 ⇒ 无全音节候选可保留，命中项直接居首。
        assert_eq!(first(&after(&on, "uidupl")), Some("释读"));
        // 国 = ky：1 位 k 国庆、国情都中；庆 = gd、情 = xo：g 只中国庆。
        let k = after(&on, "goqkk");
        for w in ["国庆", "国情"] {
            assert!(
                k.iter().any(|(t, hit)| t == w && *hit),
                "goqkk 应命中 {w}：{k:?}"
            );
        }
        let g = after(&on, "goqkg");
        assert!(g.iter().any(|(t, hit)| t == "国庆" && *hit), "{g:?}");
        assert!(!g.iter().any(|(t, hit)| t == "国情" && *hit), "{g:?}");
        assert_eq!(first(&g), Some("国庆"));
        // 像 = rn、架 = lm：短前缀奇数长度，命中项居首。
        assert_eq!(first(&after(&on, "xlr")), Some("像"));
        // `jxl`：1 位 `l` 同时中 加 lk、甲、架 lm、驾…，架是命中项之一但未必居首。
        // 补打第 2 位 `jxlm` 是偶数长度：「加练」「价廉」是覆盖全部输入的整词，按全音节优先
        // 保留在前 2 位，架（lm）接在其后。
        let l = after(&on, "jxl");
        assert!(l.iter().any(|(t, hit)| t == "架" && *hit), "{l:?}");
        let lm = after(&on, "jxlm");
        assert!(lm.iter().any(|(t, hit)| t == "架" && *hit), "{lm:?}");
    }

    /// 连续模式（`direct` 开）的真实数据示例：词中辅码交给引擎；串尾一步只认末字 / 单字，
    /// 「词组任一字」不再提前（万象 Pro 同）。
    #[test]
    fn xiaohe_examples_continuous() {
        if !has_data() {
            return;
        }
        let on = coord(true);
        let hits = |v: &[(String, bool)]| -> Vec<String> {
            v.iter()
                .filter(|(_, h)| *h)
                .map(|(t, _)| t.clone())
                .collect()
        };
        // 词中辅码：国 = ky 跟在 go 后、你 = rx 跟在 ni 后。
        assert_eq!(first(&after(&on, "gokqk")), Some("国情"));
        assert_eq!(first(&after(&on, "nirxhc")), Some("你好"));
        // 末字 1 位：读 = yd、庆 = gd、情 = xo。
        let v = after(&on, "uiduy");
        assert_eq!(hits(&v)[..2], ["试读", "释读"], "{v:?}");
        assert_eq!(first(&after(&on, "goqkg")), Some("国庆"));
        assert_eq!(first(&after(&on, "goqkx")), Some("国情"));
        // 末字 2 位：引擎辅码整句即是，串尾同文去重。
        let v = after(&on, "goqkgd");
        assert_eq!(first(&v), Some("国庆"), "{v:?}");
        assert_eq!(v.iter().filter(|(t, _)| t == "国庆").count(), 1);
        // 单字 1 位：像 = rn。2 位（xlrn、jxlm）不提前，万象需 `/`。
        assert_eq!(first(&after(&on, "xlr")), Some("像"));
        for input in ["xlrn", "jxlm"] {
            let v = after(&on, input);
            assert!(hits(&v).is_empty(), "{input}: {v:?}");
        }
        // 任一字不再提前：释 = pl 是首字、国 = ky 是首字。
        for input in ["uidup", "uidupl", "goqkk"] {
            let v = after(&on, input);
            assert!(hits(&v).is_empty(), "{input}: {v:?}");
        }
    }

    /// 连打（取几键前的快照）与退格重打（快照作废、对前缀单独解码）给出**同一批**命中项。
    ///
    /// 单音节前缀是高危区：`wu` 有数百个同音字，主候选在 limit 处被截断，而兜底解码按辅码
    /// 首字母在截断**前**准入——截断过的快照会漏掉排在后面的命中（`wuk` 曾少了 喔 呒 圄）。
    #[test]
    fn typed_through_and_retyped_hits_agree() {
        if !has_data() {
            return;
        }
        let hits = |c: &Coordinator| -> Vec<String> {
            cands(c)
                .into_iter()
                .filter(|(_, h)| *h)
                .map(|(t, _)| t)
                .collect()
        };
        let single = ["wuk", "jip", "yuy", "jib", "yiy", "yil", "lik", "uiy"];
        let cases = [
            (coord_tail(), ["uidup", "goqkk", "uidupl", "jxlm"]),
            (coord(true), ["uiduy", "goqkg", "goqkx", "xlr"]),
        ];
        for (on, input) in cases
            .iter()
            .flat_map(|(c, words)| single.iter().chain(words).map(move |w| (c, *w)))
        {
            type_each(on, input, |_| {});
            let through = hits(on);
            press(on, keymap::VK_BACK);
            on.state.lock().unwrap().direct_aux_prev = None;
            let last = input.chars().last().unwrap();
            press(on, keymap::VK_A + (last as u32 - 'a' as u32));
            let retyped = hits(on);
            assert_eq!(through, retyped, "{input}: 连打与退格重打的命中项不一致");
            assert!(!through.is_empty(), "{input}: 没有命中，对照没测到东西");
        }
    }

    /// 检索范围（智能档）按码位看「同码位有没有常用字」：兜底解码按辅码首字母准入，同码位的
    /// 常用字（生 声 …，都不以 h 起码）若不进池，生僻的 焺 就成了孤儿码位被放行——退格重打
    /// `ugh` 比连打多出一条命中、还顶到首位。
    #[test]
    fn retyped_hits_respect_scope_filter() {
        if !has_data() {
            return;
        }
        for on in [coord_tail(), coord(true)] {
            type_each(&on, "ugh", |_| {});
            let through = cands(&on);
            press(&on, keymap::VK_BACK);
            on.state.lock().unwrap().direct_aux_prev = None;
            press(&on, keymap::VK_A + ('h' as u32 - 'a' as u32));
            let retyped = cands(&on);
            assert!(!retyped.iter().any(|(t, _)| t == "焺"), "{retyped:?}");
            assert_eq!(through, retyped);
        }
    }

    /// 6 音节整句逐键输入：**偶数键**首选与关闭直接辅助码时逐键一致（全音节解析优先），
    /// 前缀 ≥ 3 音节的**奇数键**亦然（长句保首选）。
    ///
    /// 变异对照（设计 §11）：偶数保留数改 0、奇数门槛改 99，本用例须变红。串尾（逃生口）与
    /// 连续模式各跑一遍。
    #[test]
    fn sentences_keep_top_keystroke_by_keystroke() {
        if !has_data() {
            return;
        }
        let off = coord(false);
        for on in [coord_tail(), coord(true)] {
            keep_top_keystroke_by_keystroke(&on, &off);
        }
    }

    fn keep_top_keystroke_by_keystroke(on: &Coordinator, off: &Coordinator) {
        // 图书馆开放日 / 计算机语言学 / 动物园开门了（小鹤）。刻意挑「前 3 音节是词、第 4 音节
        // 的首键恰是其中某字的辅码开头」的句子（图=kd 配 kai、计=yu 配 yu、园=ke 配 kai）：
        // 第 7 键时前缀单独解码能出词且真的命中，才测得出「命中了也不抢长句首选」——前缀
        // 不成词的句子逐键一条命中都没有，对照形同虚设。
        let sentences = ["tuuugrkdfhri", "jisrjiyuyjxt", "dswuyrkdmfle"];
        let mut checked = 0;
        let mut total_hits = 0usize;
        for s in sentences {
            let mut want: Vec<Option<String>> = Vec::new();
            type_each(off, s, |_| {
                want.push(cands(off).first().map(|(t, _)| t.clone()))
            });
            let mut got: Vec<Option<String>> = Vec::new();
            let mut hits = 0usize;
            type_each(on, s, |typed| {
                let v = cands(on);
                let n = v.iter().filter(|(_, h)| *h).count();
                if n > 0 {
                    eprintln!("  {typed}: {:?}", top5(&v));
                }
                hits += n;
                got.push(v.first().map(|(t, _)| t.clone()));
            });
            eprintln!("{s}: 整句 {:?}，逐键命中项合计 {hits}", want.last());
            total_hits += hits;
            for n in 3..=s.len() {
                // 短前缀（< 3 音节）奇数键按设计让位给命中项，不在约束内。门槛写死设计值 3
                // 而不引用常量：引用的话改常量会连带改掉本用例的检查范围，变异测不出来。
                if n % 2 == 1 && (n - 1) / 2 < 3 {
                    continue;
                }
                assert_eq!(
                    got[n - 1],
                    want[n - 1],
                    "{s} 第 {n} 键（{}）首选被直接辅助改动",
                    &s[..n]
                );
                checked += 1;
            }
        }
        assert!(checked >= 20);
        assert!(total_hits > 0, "逐键一条命中都没有，对照没测到东西");
    }

    /// 盘点（T1 Step 0）：`direct` 关 / 串尾（逃生口）/ 连续模式三列界面前 5，另列连续模式的
    /// 引擎序前 6（`文字/消费数`）。`-- --ignored --nocapture`，结果见设计 §5.4 的盘点表。
    #[test]
    #[ignore]
    fn inventory_tail_vs_continuous() {
        if !has_data() {
            return;
        }
        let (tail, cont, off) = (coord_tail(), coord(true), coord(false));
        let settings = cont.engine_mgr.aux_code_settings_cached();
        let rt = cont.ensure_aux_code_runtime(&settings.sources);
        let active = cont.engine_mgr.active_schema_id();
        for input in [
            "uidu", "uidup", "uidupl", "uiduy", "uipdu", "goqk", "goqkk", "goqkg", "goqkgd",
            "goqkx", "gokqk", "xl", "xlr", "xlrn", "jxl", "jxlm", "uip", "tuuugr", "tuuugrk",
            "tuuugrkd", "nihc", "nihcnz", "nirxhc", "qiddn", "qiddnb",
        ] {
            let lk = Arc::new(rt.lookup(&cont.engine_mgr));
            let r = cont.engine_mgr.convert_with_opts(
                &active,
                input,
                50,
                super::continuous_opts(&lk, None),
            );
            let eng: Vec<String> = r
                .candidates
                .iter()
                .take(6)
                .map(|c| format!("{}/{}", c.text, c.consumed_length))
                .collect();
            eprintln!(
                "INV\t{input}\toff {:?}\ttail {:?}\tcont {:?}\tengine {:?}",
                top5(&after(&off, input)),
                top5(&after(&tail, input)),
                top5(&after(&cont, input)),
                eng
            );
        }
    }

    /// 单键耗时：直接辅助开 / 关各跑一遍同一批输入（3 条整句 + §11 示例逐键敲入），
    /// 取每键平均。`cargo test … direct_aux_perf -- --ignored --nocapture`，≥ 3 次取中位。
    #[test]
    #[ignore]
    fn direct_aux_perf() {
        if !has_data() {
            return;
        }
        let inputs = [
            "wojbtmhfgcxk",
            "tamfvgzdkdhv",
            "mktmtmqihfhc",
            "uidupl",
            "goqkgd",
            "xlrn",
            "jxlm",
        ];
        let (on, off) = (coord(true), coord(false));
        // 预热：词库 mmap、码表懒加载、各种缓存。
        for c in [&on, &off] {
            for s in inputs {
                type_each(c, s, |_| {});
            }
        }
        let rounds = 20;
        // 分两组报：长句（单音节前缀的兜底解码每句只摊一次）与短词（每个词都要摊一次）。
        let groups: [(&str, &[&str]); 3] = [
            ("全部", &inputs),
            ("整句", &inputs[..3]),
            ("短词", &inputs[3..]),
        ];
        for (group, list) in groups {
            for (name, c) in [("off", &off), ("on", &on), ("off", &off), ("on", &on)] {
                let mut keys = 0u32;
                let t = std::time::Instant::now();
                for _ in 0..rounds {
                    for s in list {
                        type_each(c, s, |_| keys += 1);
                    }
                }
                let per = t.elapsed() / keys;
                eprintln!("direct {name} [{group}]: {keys} 键，平均 {per:?}/键");
            }
        }
    }
}
