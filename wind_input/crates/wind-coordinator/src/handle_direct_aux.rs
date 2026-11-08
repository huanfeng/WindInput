//! 直接辅助码（双拼）：不按引导键，输入末 1～2 位自动当辅码，字形对得上的字词提到前面。
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
    data_gen: u64,
    /// `None` = 整音节那一键的主候选快照（未截断，前缀下有资格的候选一条不缺）。
    /// `Some((字母, 上限))` = 兜底解码的产出，引擎按辅码首字母准入过、按该上限取的——只能给
    /// 同首字母、同上限的键复用（奇数键算的给紧随其后的偶数键：2 位辅码命中集是首字母的子集）。
    admit: Option<(char, usize)>,
    /// 只存**有资格**的拼音候选（`is_direct_source`），不整表克隆。
    candidates: Vec<Candidate>,
    /// 那一键的双拼音节分段（`ui'du`），组码区用。
    preedit: String,
    /// 那一键的 shadow 归一码（双拼下是全拼码），前缀 shadow 用。
    shadow_code: String,
}

impl Coordinator {
    /// 把直接辅助码的命中项并入 `candidates`（主候选，已排序去重过滤、尚未 shadow）。
    /// 任一门卫不过 / 无命中 → 候选原样。组码区形态写进 `state.direct_aux_body`（无命中清空）。
    pub(crate) fn apply_direct_aux(
        &self,
        state: &mut State,
        candidates: &mut Vec<Candidate>,
        limit: usize,
        truncated: bool,
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
            self.merge_direct_hits_into(state, candidates, limit, split, syllables, &settings);
        }
        // 奇数键（或前缀以外的不成音节输入）不动快照，留给后面的键。
        if let Some(list) = pre_merge {
            state.direct_aux_prev = (!truncated).then(|| DirectAuxPrev {
                filters: self.direct_aux_filters(state),
                data_gen: self.direct_aux_data_gen(),
                admit: None,
                input,
                candidates: list,
                preedit: state.preedit_split_body.clone(),
                shadow_code: state.shadow_code.clone(),
            });
        }
    }

    fn merge_direct_hits_into(
        &self,
        state: &mut State,
        candidates: &mut Vec<Candidate>,
        limit: usize,
        split: wind_aux_code::DirectSplit<'_>,
        syllables: usize,
        settings: &wind_engine::AuxCodeSettings,
    ) {
        let rt = self.ensure_aux_code_runtime(&settings.sources);
        let lookup = Arc::new(rt.lookup(&self.engine_mgr));
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
                let (pool, preedit, shadow_code) =
                    self.decode_direct_prefix(state, prefix, limit, split.aux, settings, &lookup);
                state.direct_aux_prev = Some(DirectAuxPrev {
                    input: prefix.to_string(),
                    filters,
                    data_gen,
                    admit: Some((letter, limit)),
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
        self.apply_shadow(&mut pool, prefix_shadow);

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
                        DirectPhraseRule::Any,
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
        // 组码区：前缀按双拼音节切（引擎给的击键分段，如 `ui'du`）+ 空格 + 辅码。与引导键模式
        // 同口径地用空白隔开辅码；仍是「缓冲按序插入分隔符」的形态，光标换算照常成立。
        let body = if preedit.is_empty() { prefix } else { &preedit };
        state.direct_aux_body = format!("{body} {}", split.aux);
    }

    /// 快照复用的前提：候选裁剪状态与取快照时相同（见 [`DirectAuxPrev::filters`]）。
    fn direct_aux_data_gen(&self) -> u64 {
        self.store.as_ref().map_or(0, |s| s.words_generation())
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
    fn decode_direct_prefix(
        &self,
        state: &State,
        prefix: &str,
        limit: usize,
        aux: &str,
        settings: &wind_engine::AuxCodeSettings,
        lookup: &Arc<crate::aux_code_source::AuxLookupNow>,
    ) -> (Vec<Candidate>, String, String) {
        let max_phrase_len = settings.max_phrase_len;
        let admit_lookup = lookup.clone();
        let letter: String = aux.chars().take(1).collect();
        let opts = wind_engine::engine::ConvertOptions {
            // 只要吃满前缀的候选，且在截断**之前**丢掉其余（否则同音单字会把词挤出配额）。
            require_full_match: true,
            no_abbrev_quota: true,
            // 按辅码**首字母**准入（2 位辅码的命中集是 1 位的子集，见 `direct_matches`）：
            // 截断前就只剩可能命中的——`ui`（shi）有几百个同音字，全量取一遍要多花十几毫秒。
            admit: Some(Arc::new(move |text: &str| {
                wind_aux_code::direct_matches(
                    text,
                    &*admit_lookup,
                    &letter,
                    DirectPhraseRule::Any,
                    max_phrase_len,
                )
            })),
            ..Default::default()
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
        self.apply_filter(state, &mut pool);
        self.apply_single_char(state, &mut pool);
        let rerank_len = pool.iter().take_while(|c| !c.is_scope_filtered).count();
        self.apply_freq_rerank(&mut pool[..rerank_len], prefix);
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
                        想\txiang\t5000\n向\txiang\t4000\n像\txiang\t3000\n";
    /// 小鹤形码（取自 flypy_full.txt）。
    const AUX: &str = "释=pl\n湿=dy\n适=zk\n十=al\n试=yg\n读=yd\n度=gy\n是=or\n\
                       国=ky\n庆=gd\n情=xo\n想=mx\n向=pk\n像=rn\n";

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
            format!("---\nname: mini\nversion: \"1\"\nsort: by_weight\n...\n{DICT}"),
        )
        .unwrap();
        std::fs::write(schemas.join("aux_code/t.txt"), AUX).unwrap();
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
        let (dir, _f) = fixture("odd", "shuangpin", ON);
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
        let (dir, _f) = fixture("snap", "shuangpin", ON);
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
        let (dir, _f) = fixture("deluser", "shuangpin", ON);
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
        let (dir, _f) = fixture("addmid", "shuangpin", ON);
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
        let (dir, _f) = fixture("single", "shuangpin", ON);
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
        let (dir, _f) = fixture("hl", "shuangpin", ON);
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

    /// 偶数长度：`xlrn` = xiang + `rn`，像=rn。无全音节候选可保留时命中项排最前。
    #[test]
    fn even_two_letter_aux() {
        let (dir, _f) = fixture("even", "shuangpin", ON);
        let (c, _) = coord("even", &dir);
        type_str(&c, "xlrn");
        assert_eq!(
            texts(&c).first().map(String::as_str),
            Some("像"),
            "{:?}",
            texts(&c)
        );
        // 国庆 / 国情：1 位辅码 k 两者都中（国=ky）；g 只中国庆（庆=gd，情=xo）。
        let (c, _) = coord("even2", &dir);
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
        let (dir, _f) = fixture("page", "shuangpin", ON);
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
        let (dir, _f) = fixture("guide", "shuangpin", ON);
        let (c, _) = coord("guide", &dir);
        type_str(&c, "uidup");
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::AuxCode);
        let mut cands = vec![wind_candidate::Candidate {
            text: "湿度".into(),
            ..Default::default()
        }];
        c.apply_direct_aux(&mut st, &mut cands, 50, false);
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
        let (dir, _f) = fixture("cold", "shuangpin", ON);
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

    /// 直接辅助的结果上仍可按反引号进引导键筛选，筛的是当前整张候选表。
    #[test]
    fn guide_key_filters_current_list() {
        let (dir, _f) = fixture("guide2", "shuangpin", ON);
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

    /// §11 真实数据示例。`*` = 直接辅助命中项。开关两侧的前 5 名打到 stderr 供报告。
    #[test]
    fn xiaohe_examples() {
        if !has_data() {
            return;
        }
        let (on, off) = (coord(true), coord(false));
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

    /// 连打（取几键前的快照）与退格重打（快照作废、对前缀单独解码）给出**同一批**命中项。
    ///
    /// 单音节前缀是高危区：`wu` 有数百个同音字，主候选在 limit 处被截断，而兜底解码按辅码
    /// 首字母在截断**前**准入——截断过的快照会漏掉排在后面的命中（`wuk` 曾少了 喔 呒 圄）。
    #[test]
    fn typed_through_and_retyped_hits_agree() {
        if !has_data() {
            return;
        }
        let on = coord(true);
        let hits = |c: &Coordinator| -> Vec<String> {
            cands(c)
                .into_iter()
                .filter(|(_, h)| *h)
                .map(|(t, _)| t)
                .collect()
        };
        for input in [
            "wuk", "jip", "yuy", "jib", "yiy", "yil", "lik", "uiy", "uidup", "goqkk", "uidupl",
            "jxlm",
        ] {
            type_each(&on, input, |_| {});
            let through = hits(&on);
            press(&on, keymap::VK_BACK);
            on.state.lock().unwrap().direct_aux_prev = None;
            let last = input.chars().last().unwrap();
            press(&on, keymap::VK_A + (last as u32 - 'a' as u32));
            let retyped = hits(&on);
            assert_eq!(through, retyped, "{input}: 连打与退格重打的命中项不一致");
            assert!(!through.is_empty(), "{input}: 没有命中，对照没测到东西");
        }
    }

    /// 6 音节整句逐键输入：**偶数键**首选与关闭直接辅助码时逐键一致（全音节解析优先），
    /// 前缀 ≥ 3 音节的**奇数键**亦然（长句保首选）。
    ///
    /// 变异对照（设计 §11）：偶数保留数改 0、奇数门槛改 99，本用例须变红。
    #[test]
    fn sentences_keep_top_keystroke_by_keystroke() {
        if !has_data() {
            return;
        }
        let (on, off) = (coord(true), coord(false));
        // 图书馆开放日 / 计算机语言学 / 动物园开门了（小鹤）。刻意挑「前 3 音节是词、第 4 音节
        // 的首键恰是其中某字的辅码开头」的句子（图=kd 配 kai、计=yu 配 yu、园=ke 配 kai）：
        // 第 7 键时前缀单独解码能出词且真的命中，才测得出「命中了也不抢长句首选」——前缀
        // 不成词的句子逐键一条命中都没有，对照形同虚设。
        let sentences = ["tuuugrkdfhri", "jisrjiyuyjxt", "dswuyrkdmfle"];
        let mut checked = 0;
        let mut total_hits = 0usize;
        for s in sentences {
            let mut want: Vec<Option<String>> = Vec::new();
            type_each(&off, s, |_| {
                want.push(cands(&off).first().map(|(t, _)| t.clone()))
            });
            let mut got: Vec<Option<String>> = Vec::new();
            let mut hits = 0usize;
            type_each(&on, s, |typed| {
                let v = cands(&on);
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
