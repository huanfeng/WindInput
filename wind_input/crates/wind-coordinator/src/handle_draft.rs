//! 滑窗草稿的接线：落屏文本流 → 滑窗切分 → 异步批量落库。
//!
//! 状态机在 [`crate::draft_window`]（纯逻辑），存储在 `wind_store::draft_words`，
//! 召回在 `wind_store::store_layer::StoreDraftLayer`。本模块只做接线与 IO 调度。
//! 设计见 `docs/design/auto-phrase-draft-layer.md`。
//!
//! 按键路径上只做滑窗切分（纯内存）与入队，取码、查重、写库全部在后台线程：草稿约
//! 每字 4 条，任何一项落在上屏线程上都是给每次按键加钱。

use crate::coordinator::{Coordinator, State};
use crate::draft_window::DraftWord;
use wind_bridge::handler::KeyAction;

use std::sync::atomic::Ordering;
use tracing::{debug, info, warn};

/// 队列的硬上限。后台线程长时间没跑起来（如存储不可写）时丢最早的那些：草稿本就是
/// 机会性产物，丢几条只是「这次没记住」，无界增长的 Vec 才是真故障。
const DRAFT_QUEUE_MAX: usize = 4096;

impl Coordinator {
    /// 草稿层是否启用。复用码表自动造词那一个开关，不新增配置项。
    pub(crate) fn draft_enabled(&self) -> bool {
        self.auto_phrase_enabled()
    }

    /// 上屏后把文字喂进滑窗流。由 `finish_session_action` 按来源调用（见
    /// `session_exit::exit_steps` 的 `draft`）。
    ///
    /// 判据取一切真落屏的文字（五种 `KeyAction` 变体），与 `note_self_commit` 同源，
    /// 而不是 `feed_auto_phrase` 那两种：凡是落到屏幕上的字都该进流，词组上屏才不中断造词。
    pub(crate) fn note_draft_commit(&self, action: &KeyAction) {
        if !self.draft_enabled() {
            return;
        }
        // 暂停学习：不喂流并就地断流，否则暂停前窗口里的字会和恢复后的字拼成跨界假词。
        if self.learning_suspended() {
            self.draft_window
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .terminate();
            return;
        }
        let text = match action {
            // 回改已上屏内容：先把被改掉的部分从流里退出去再喂新文本，否则留下幽灵字。
            KeyAction::ReplaceBackward { count, text } => {
                self.draft_window
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .rewind(*count as usize);
                text.as_str()
            }
            KeyAction::InsertText { text, .. } | KeyAction::InsertTextWithCursor { text, .. } => {
                text.as_str()
            }
            KeyAction::CommitAndHoldComposition { commit_text, .. }
            | KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text.as_str(),
            _ => {
                // 本次按键没有落屏：选词出口登记的上屏码（若有）就此作废，不留给以后的字。
                self.draft_window
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear_code_hint();
                return;
            }
        };
        // 输入法补的前缀空格不是用户输入：剥掉再喂（见 `Coordinator::auto_space_prefixed`）。
        let text = if self
            .auto_space_prefixed
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            text.strip_prefix([' ', '\u{3000}']).unwrap_or(text)
        } else {
            text
        };
        if text.is_empty() {
            return;
        }
        let ap = self.engine_mgr.codetable_settings().auto_phrase;
        let idle = self.auto_phrase_idle_timeout();
        let windows = {
            let mut buf = self.draft_window.lock().unwrap_or_else(|e| e.into_inner());
            buf.on_commit_hinted(
                text,
                std::time::Instant::now(),
                idle,
                ap.min_phrase_len,
                ap.max_phrase_len,
            )
        }; // 锁在此释放：入队与 flush 判定不该持着流缓冲的锁。
        let broke = windows.is_empty() && !text.chars().all(crate::draft_window::is_draft_char);
        self.enqueue_drafts(windows);
        if broke {
            // 非汉字上屏 = 一段话结束。它不走 `terminate_auto_phrase`，断流的 flush 得在这里补。
            self.spawn_draft_flush();
        }
    }

    /// 断流：标点/焦点丢失/IME 停用/模式切换/切换方案/光标移动/idle 超时。
    ///
    /// 顺带把队列冲一次，兜住「打完就不动了」的情形。
    pub(crate) fn terminate_draft_window(&self, reason: &str) {
        if !self.draft_enabled() {
            return;
        }
        let had = {
            let mut buf = self.draft_window.lock().unwrap_or_else(|e| e.into_inner());
            let had = !buf.is_empty();
            buf.terminate();
            had
        };
        if had {
            debug!("draft: 断流（{reason}）");
        }
        self.spawn_draft_flush();
    }

    /// 选词出口登记「这个字是用哪条码打出来的」，供下一次落屏随字进流。
    ///
    /// 只收主输入路上码表来源的单个汉字：词组候选带的是整词的码，拆不出各字；拼音/英文
    /// 候选的码不是本方案的码（误收也会在取码时回退全码表，见 `wind_engine::encoder::pick_hinted_code`）。
    ///
    /// `fallback_code` 是候选不带词条码时的退路（输入码）。
    pub(crate) fn note_draft_code_hint(
        &self,
        cand: &wind_candidate::Candidate,
        fallback_code: &str,
    ) {
        if !self.draft_enabled() || cand.source != wind_candidate::CandidateSource::CodeTable {
            return;
        }
        let mut cs = cand.text.chars();
        let (Some(ch), None) = (cs.next(), cs.next()) else {
            return;
        };
        if !crate::draft_window::is_draft_char(ch) {
            return;
        }
        let code = if cand.code.is_empty() {
            fallback_code
        } else {
            cand.code.as_str()
        };
        if code.is_empty() {
            return;
        }
        self.draft_window
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_code_hint(ch, code.to_string());
    }

    fn enqueue_drafts(&self, words: Vec<DraftWord>) {
        if words.is_empty() {
            return;
        }
        {
            let mut q = self.draft_queue.lock().unwrap_or_else(|e| e.into_inner());
            q.extend(words);
            if q.len() > DRAFT_QUEUE_MAX {
                // 丢最早的：它们离用户当前的输入最远，最不可能马上被用到。
                let drop = q.len() - DRAFT_QUEUE_MAX;
                q.drain(..drop);
                warn!("draft: 队列超上限，丢弃最早的 {drop} 条（后台线程没跟上？）");
            }
        }
        // 入队即起 flush，不设「攒够 N 条才触发」的门槛，否则刚打完的词马上召不回。
        // 攒批靠的是 `draft_flushing` 去重 + flush 线程「循环到队列空」，不会退化成每条
        // 一个事务。见 docs/design/auto-phrase-draft-layer.md 的「10.5 落库时机」。
        self.spawn_draft_flush();
    }

    /// 起一个后台线程把队列里的词取码、查重、批量落库。
    ///
    /// 已有线程在跑时直接返回（`draft_flushing` 这道闸），避免多个线程重复取码与写库。
    pub(crate) fn spawn_draft_flush(&self) {
        if !self.draft_enabled() {
            return;
        }
        {
            let q = self.draft_queue.lock().unwrap_or_else(|e| e.into_inner());
            if q.is_empty() {
                return;
            }
        }
        if self
            .draft_flushing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return; // 已有线程在跑，它会把队列里新增的一并带走
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            self.draft_flushing.store(false, Ordering::Release);
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("draft-flush".into())
            .spawn(move || {
                // 循环到队列空：flush 期间新入队的词一并带走。
                loop {
                    let Some(c) = weak.upgrade() else { break };
                    let batch = {
                        let mut q = c.draft_queue.lock().unwrap_or_else(|e| e.into_inner());
                        if q.is_empty() {
                            break;
                        }
                        // 每轮只取 `draft_flush_batch` 条（单次批量写的上限），免得一次吞下
                        // 整个队列、长时间占住后台线程。
                        let n = c.draft_flush_batch().min(q.len());
                        q.drain(..n).collect::<Vec<_>>()
                    };
                    c.flush_draft_batch(batch);
                    // 必须在取下一批之前放掉 c：Arc 活着会拖住 Coordinator 的析构。
                    drop(c);
                }
                if let Some(c) = weak.upgrade() {
                    c.draft_flushing.store(false, Ordering::Release);
                    // 复位后必须再看一眼队列：判定「队列空」到复位之间主线程入队调用的
                    // `spawn_draft_flush` 会被 `compare_exchange` 挡掉，不补就丢失唤醒、那批词
                    // 躺到下一次触发。`spawn_draft_flush` 自己判空，无条件调用是安全的。
                    c.spawn_draft_flush();
                }
            });
        if spawned.is_err() {
            self.draft_flushing.store(false, Ordering::Release);
            debug!("draft: 起不了 flush 线程，本批留在队列里等下次");
        }
    }

    /// 一批草稿词：取码 → 查重 → 单次批量写落库。**跑在后台线程上。**
    fn flush_draft_batch(&self, words: Vec<DraftWord>) {
        let Some(store) = &self.store else { return };
        let Some(sc) = self.resolve_phrase_schemas() else {
            // 索引没就绪：整批丢掉不留着等，草稿的价值系于时效。
            return;
        };
        let mut seen = std::collections::HashSet::new();
        let mut items: Vec<(String, String)> = Vec::new();
        // 取码失败按类别计数，整批只汇总一行 info（见下）。
        let mut failed: Vec<(&'static str, usize)> = Vec::new();
        for DraftWord { text, code_hints } in words {
            // 批内去重：滑窗对同一段文字反复切，同一个词一批里出现多次是常态。
            if !seen.insert(text.clone()) {
                continue;
            }
            // 窗口里的非汉字（〇，见 `is_draft_char`）必须在本码表真有码：取码对无码的非汉字是
            // 跳过（手工加词要的宽口径），放到草稿上就会把「二〇」按「二」的全码落库。
            if text.chars().any(|c| {
                !crate::handle_addword::is_han(c)
                    && self
                        .engine_mgr
                        .word_codes_in(&sc.encode, &c.to_string())
                        .is_none_or(|codes| codes.is_empty())
            }) {
                debug!("draft: 含本码表无码的非汉字，跳过: {text}");
                continue;
            }
            match self.encode_and_dedup(&sc, &text, &code_hints, true) {
                Ok(Some(code)) => items.push((code, text)),
                Ok(None) => {}
                Err(e) => match failed.iter_mut().find(|(k, _)| *k == e.kind()) {
                    Some((_, n)) => *n += 1,
                    None => failed.push((e.kind(), 1)),
                },
            }
        }
        if !failed.is_empty() {
            // INFO 只记数量与类别、不记词（日志规范），具体哪个字看 debug 级的 `encode_and_dedup`。
            let total: usize = failed.iter().map(|(_, n)| n).sum();
            let kinds: Vec<String> = failed.iter().map(|(k, n)| format!("{k}={n}")).collect();
            info!(
                "draft: 本批 {total} 个窗口取码失败、未造词（{}）；开 debug 日志可见具体原因",
                kinds.join(", ")
            );
        }
        if items.is_empty() {
            return;
        }
        let n = items.len();
        match store.add_drafts(&sc.write, &items) {
            Ok(_) => {
                debug!("draft: 落库 {n} 条");
                self.maybe_evict_drafts(store, &sc.write);
            }
            Err(e) => warn!("draft: 落库失败: {e}"),
        }
    }

    /// 草稿表的容量维护：清过期 + 超上限淘汰。跟在落库之后，同在后台线程。
    fn maybe_evict_drafts(&self, store: &wind_store::Store, schema: &str) {
        let ttl = self.draft_ttl_secs();
        if ttl > 0 {
            match store.purge_expired_drafts(schema, ttl) {
                Ok(k) if k > 0 => debug!("draft: 清理过期 {k} 条"),
                Ok(_) => {}
                Err(e) => warn!("draft: 清理过期失败: {e}"),
            }
        }
        let max = self.draft_max_entries();
        if max > 0 {
            match store.evict_drafts(schema, max) {
                Ok(k) if k > 0 => debug!("draft: 超上限淘汰 {k} 条（上限 {max}）"),
                Ok(_) => {}
                Err(e) => warn!("draft: 淘汰失败: {e}"),
            }
        }
    }

    /// 选中一条草稿候选上屏：把它从草稿层跃迁进临时词库。返回是否真的跃迁了。
    ///
    /// 「用过即转正」的第一跳：过滤发生在使用端，没被用过的到期自动丢弃。跃迁后就是普通
    /// 临时词，累到 `promote_count` 晋升用户词库。
    ///
    /// 调用方必须据此跳过「6b 临时词使用累积」：跃迁已把 count 记成 1，再累积就是同一次
    /// 上屏 +2（与 `learn_phrase_on_commit` 返回值要跳过 6b 同理）。
    pub(crate) fn promote_draft_on_commit(&self, code: &str, text: &str, boundary: u64) -> bool {
        // 暂停学习：转正也是从这次上屏里学，不做。
        if !self.draft_enabled() || self.learning_suspended() {
            return false;
        }
        let Some(store) = &self.store else {
            return false;
        };
        let active = self.engine_mgr.active_schema_id();
        let Some(schema) = self
            .engine_mgr
            .write_data_schema_id(&active, wind_candidate::CandidateSource::CodeTable)
        else {
            return false;
        };
        match store.promote_draft_to_temp(
            &schema,
            code,
            text,
            crate::coordinator::LEARN_ADD_WEIGHT,
            boundary,
        ) {
            Ok(true) => {
                debug!("draft: 用过即转正 {code} -> {text}");
                let promote_count = self
                    .engine_mgr
                    .codetable_settings()
                    .auto_phrase
                    .promote_count;
                // 跃迁写入的 count 恒为 1；`promote_count` 为 1 时这一次就该直接进用户词库。
                self.maybe_promote_temp(store, &schema, code, text, 1, promote_count);
                true
            }
            Ok(false) => false, // 不是草稿（或已过期被清），走常规路径
            Err(e) => {
                warn!("draft: 跃迁失败: {e}");
                false
            }
        }
    }

    /// 引擎替用户做主上屏（满码唯一自动上屏 / 空码顶出，两条 `InputOutcome::AutoCommit`
    /// 消费点共用）时，若被上屏的那条是草稿候选，同样按「用过」转正。
    ///
    /// 判据是词落没落屏而不是按了哪颗键，否则自动上屏时词频记了账、草稿没记账。按 `text`
    /// 匹配而不是取 `candidates.first()`：草稿候选恒沉底。此路径不接 6b 累积，
    /// `count = 1` 就是这次上屏的全部记账。
    pub(crate) fn promote_draft_on_auto_commit(&self, state: &State, text: &str, code: &str) {
        let Some(cand) = state
            .candidates
            .iter()
            .find(|c| c.is_draft && c.text == text)
        else {
            return;
        };
        let _ = self.promote_draft_on_commit(code, &cand.text, cand.boundary);
    }

    /// 启动时跑一次的全表过期清理，不看开关、不分方案。
    ///
    /// 兜住 `maybe_evict_drafts` 够不着的两类：开关关掉后的旧草稿、不再使用的方案的草稿。
    /// 只做过期清理不做容量淘汰：容量上限按方案定义，全表套同一个上限会把小方案连坐清空。
    // 唯一调用方是 construct::new（desktop-ui 特性）；wind-webdata 等不带该特性的依赖方编译时它无人调用。
    #[cfg_attr(not(feature = "desktop-ui"), allow(dead_code))]
    pub(crate) fn purge_drafts_on_start(&self) {
        let Some(store) = &self.store else { return };
        let ttl = self.draft_ttl_secs();
        if ttl <= 0 {
            return;
        }
        match store.purge_all_expired_drafts(ttl) {
            // 无条件打这一行：`剩余` 是真机上观测草稿表规模的唯一出口（调 `draft_max_entries` 用）。
            Ok((k, kept)) => debug!("draft: 启动清理过期 {k} 条，剩余 {kept} 条"),
            Err(e) => warn!("draft: 启动清理失败: {e}"),
        }
    }

    /// 单次批量写最多落库多少条草稿。不是触发落库的门槛（见 `enqueue_drafts`），
    /// 只决定一次事务吞多少。配成 0 会让 `drain(..0)` 空转成死循环，故下限钳到 1。
    pub(crate) fn draft_flush_batch(&self) -> usize {
        self.engine_mgr
            .codetable_settings()
            .auto_phrase
            .draft_flush_batch
            .max(1)
    }

    /// 草稿有效期（秒）。0 = 永不过期。
    pub(crate) fn draft_ttl_secs(&self) -> i64 {
        i64::from(
            self.engine_mgr
                .codetable_settings()
                .auto_phrase
                .draft_ttl_hours,
        ) * 3600
    }

    /// 草稿表容量上限。0 = 不限。
    ///
    /// 默认值来自设计稿 §7 的纸面估算上界，不是实测值。
    pub(crate) fn draft_max_entries(&self) -> usize {
        self.engine_mgr
            .codetable_settings()
            .auto_phrase
            .draft_max_entries
    }
}

#[cfg(test)]
#[path = "handle_draft_tests.rs"]
mod tests;
