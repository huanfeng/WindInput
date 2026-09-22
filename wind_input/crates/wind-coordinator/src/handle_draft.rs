//! 滑窗草稿的接线：落屏文本流 → 滑窗切分 → 异步批量落库。
//!
//! 状态机在 [`crate::draft_window`]（纯逻辑），存储在 `wind_store::draft_words`，
//! 召回在 `wind_dict::store_layer::StoreDraftLayer`。本模块只做接线与 IO 调度。
//! 设计见 `docs/design/auto-phrase-draft-layer.md`。
//!
//! # 按键路径上只做两件事
//!
//! 滑窗切分（纯内存）与入队（`Vec::push`）。取码、查重、写库全部在后台线程。
//!
//! 这不是优化而是刚性要求：草稿的产生速率约**每字 4 条**，而取码要查单字全码表、
//! 查重要查反查索引与用户词库、写库要抢 redb 的单写锁——任何一项落在上屏线程上，
//! 都是在给每一次按键加钱。旧的 `flush_auto_phrase` 敢在按键线程上做这些，
//! 是因为它一句话才触发一次；滑窗模型下那个前提没有了。

use crate::coordinator::Coordinator;
use wind_bridge::handler::KeyAction;

use std::sync::atomic::Ordering;
use tracing::{debug, warn};

/// 队列的硬上限。后台线程若长时间没跑起来（比如 redb 被暂停），
/// 队列不能无限涨——超出就丢最早的那些。
///
/// 丢弃是可接受的：草稿本就是机会性产物，丢几条的后果只是「那几个词这次没记住」，
/// 与造词前的状态一致。反过来，让一个内存 Vec 无界增长才是真的故障。
const DRAFT_QUEUE_MAX: usize = 4096;

impl Coordinator {
    /// 草稿层是否启用。**复用码表自动造词那一个开关**，不新增配置项
    /// （设计稿 §0：只重做码表那条路径，出厂仍是关的）。
    pub(crate) fn draft_enabled(&self) -> bool {
        self.auto_phrase_enabled()
    }

    /// 上屏后把文字喂进滑窗流。由 `note_commit_action` 统一调用。
    ///
    /// ⚠️ 判据取的是**一切真落屏的文字**（五种 `KeyAction` 变体），与
    /// `note_commit_action` 的自提交打点同源，而**不是** `feed_auto_phrase` 那两种。
    /// 滑窗的对象是「最近落屏的文本流」，凡是落到屏幕上的字都该进流——
    /// 这正是「词组上屏不再中断造词」得以成立的地方。
    pub(crate) fn note_draft_commit(&self, action: &KeyAction) {
        if !self.draft_enabled() {
            return;
        }
        let text = match action {
            // 回改已上屏内容：先把被改掉的部分从流里退出去，再喂新文本。
            // 直接追加会让被替换掉的字留在流里，制造与退格幽灵字同源的脏数据。
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
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        let ap = self.engine_mgr.codetable_settings().auto_phrase;
        let idle = self.auto_phrase_idle_timeout();
        let windows = {
            let mut buf = self.draft_window.lock().unwrap_or_else(|e| e.into_inner());
            buf.on_commit(
                text,
                std::time::Instant::now(),
                idle,
                ap.min_phrase_len,
                ap.max_phrase_len,
            )
        }; // 锁在此释放：入队与 flush 判定不该持着流缓冲的锁。
        let broke = windows.is_empty() && !text.chars().all(crate::handle_addword::is_han);
        self.enqueue_drafts(windows);
        if broke {
            // 非汉字上屏 = 一段话结束，是最常见的落库时机。
            // 它**不走** `terminate_auto_phrase`（标点/空格那条在 `feed_auto_phrase` 里内联
            // 判断），所以断流的 flush 必须在这里补，否则一段话打完队列还躺着。
            self.spawn_draft_flush();
        }
    }

    /// 断流：标点/焦点丢失/IME 停用/模式切换/切换方案/光标移动/idle 超时。
    ///
    /// 顺带把队列冲一次——一段话打完正是落库的好时机，而且它兜住了「用户打完就不动了、
    /// 队列没攒够 batch」这个常见情形。
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

    fn enqueue_drafts(&self, words: Vec<String>) {
        if words.is_empty() {
            return;
        }
        let full = {
            let mut q = self.draft_queue.lock().unwrap_or_else(|e| e.into_inner());
            q.extend(words);
            if q.len() > DRAFT_QUEUE_MAX {
                // 丢最早的：它们离用户当前的输入最远，最不可能马上被用到。
                let drop = q.len() - DRAFT_QUEUE_MAX;
                q.drain(..drop);
                warn!("draft: 队列超上限，丢弃最早的 {drop} 条（后台线程没跟上？）");
            }
            q.len() >= self.draft_flush_batch()
        };
        if full {
            self.spawn_draft_flush();
        }
    }

    /// 起一个后台线程把队列里的词取码、查重、批量落库。
    ///
    /// 已有线程在跑时直接返回（`draft_flushing` 这道闸）：没有它，队列每满一次就会
    /// spawn 一个新线程去抢同一把 redb 写锁，与 `is_building_reverse_index` 挡住的是
    /// 同一类问题。
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
                // 循环到队列空：flush 期间用户还在打字，新词会继续入队。
                // 不循环的话，那些词要等下一次「队列满 / 断流」才落库。
                loop {
                    let Some(c) = weak.upgrade() else { break };
                    let batch = {
                        let mut q = c.draft_queue.lock().unwrap_or_else(|e| e.into_inner());
                        if q.is_empty() {
                            break;
                        }
                        std::mem::take(&mut *q)
                    };
                    c.flush_draft_batch(batch);
                    // ⚠️ 必须在**取完下一批之前**放掉 c：Arc 活着会拖住 Coordinator 的析构。
                    drop(c);
                }
                if let Some(c) = weak.upgrade() {
                    c.draft_flushing.store(false, Ordering::Release);
                    // ⚠️ 复位之后**必须再看一眼队列**：从上面判定「队列空」到这一行之间，
                    // 主线程可能已经入了队并调过 `spawn_draft_flush` —— 而那次调用会因为
                    // `draft_flushing` still true 被 `compare_exchange` 挡掉。
                    // 不补这一下就是经典的**丢失唤醒**：那批词一直躺到下一次触发才落库，
                    // 表现为「打完一段话，草稿过很久才出现，甚至这一段永远没出现」。
                    //
                    // 这个窗口是靠一次 flaky 的端到端测试才暴露的（5 秒轮询等不到落库）。
                    // `spawn_draft_flush` 自己会判队列空不空，所以这里无条件调用是安全的。
                    c.spawn_draft_flush();
                }
            });
        if spawned.is_err() {
            self.draft_flushing.store(false, Ordering::Release);
            debug!("draft: 起不了 flush 线程，本批留在队列里等下次");
        }
    }

    /// 一批草稿词：取码 → 查重 → 单次写事务落库。**跑在后台线程上。**
    fn flush_draft_batch(&self, words: Vec<String>) {
        let Some(store) = &self.store else { return };
        let Some(sc) = self.resolve_phrase_schemas() else {
            // 索引没就绪：整批丢掉，不留在队列里等。
            // 留着等的话，用户在索引就绪前打的每一个字都会堆在内存里，而那些词早已
            // 不是「最近输入」了——草稿的价值本就系于时效。
            return;
        };
        let mut seen = std::collections::HashSet::new();
        let mut items: Vec<(String, String)> = Vec::new();
        for word in words {
            // 批内去重：滑窗对同一段文字反复切，一批里同一个词出现多次是常态。
            // 不去重的话，同样的取码与查重会做很多遍——这是本函数最贵的两步。
            if !seen.insert(word.clone()) {
                continue;
            }
            if let Some(code) = self.encode_and_dedup(&sc, &word, true) {
                items.push((code, word));
            }
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

    /// 选中一条**草稿候选**上屏：把它从草稿层跃迁进临时词库。返回是否真的跃迁了。
    ///
    /// 这是「用过即转正」的第一跳，也是草稿层存在的意义——草稿只有被用过才会留下，
    /// 没被用过的到期自动丢弃，过滤因此发生在使用端而不是产生端。
    ///
    /// 跃迁后它就是一条普通临时词：再被用到 `count++`，累到 `promote_count` 晋升进
    /// 用户词库，容量超限时按 `(count, created_at, weight)` 淘汰。
    ///
    /// ⚠️ **调用方必须据此跳过「6b 临时词使用累积」**：跃迁本身已经把 count 记成 1，
    /// 再让 6b 点查命中一次就是同一次上屏 count +2 —— 与 `learn_phrase_on_commit`
    /// 的返回值要跳过 6b 是同一个坑（那里记着「单段时两者 key 完全相同」）。
    pub(crate) fn promote_draft_on_commit(&self, code: &str, text: &str, boundary: u64) -> bool {
        if !self.draft_enabled() {
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
                // 跃迁写入的 count 恒为 1（草稿本就是第一次被用）；用户把
                // `promote_count` 设成 1 时，这一次就该直接进用户词库。
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

    /// 启动时跑一次的**全表**过期清理。不看开关，也不分方案。
    ///
    /// `maybe_evict_drafts` 只跟在落库之后跑，够不着两类草稿：
    /// ① **开关关掉之后**——此后再没有落库，旧草稿永远没人收；
    /// ② **用户已经不用的方案**——落库带的是当前方案的前缀，碰不到它。
    /// 两类都会永久躺在库里，而它们恰恰是最没有价值的那些。
    ///
    /// 不看开关是刻意的：关掉功能的用户更需要这次清理，而且因为没有新的写入，
    /// 他的草稿表会在一个 TTL 之内自行排空。
    ///
    /// ⚠️ 只做**过期**清理，不做容量淘汰——容量上限是按方案定义的（`evict_drafts`
    /// 的语义是「这个方案保留 N 条」），全表跨方案套同一个上限会把小方案连坐清空。
    pub(crate) fn purge_drafts_on_start(&self) {
        let Some(store) = &self.store else { return };
        let ttl = self.draft_ttl_secs();
        if ttl <= 0 {
            return;
        }
        match store.purge_all_expired_drafts(ttl) {
            // 无条件打这一行（哪怕一条没清）：`剩余` 是草稿表规模在真机上**唯一**的
            // 观测出口，而 `draft_max_entries` 的默认值至今还是纸面估算。
            // 想调那个值的人需要的就是这个数。
            Ok((k, kept)) => debug!("draft: 启动清理过期 {k} 条，剩余 {kept} 条"),
            Err(e) => warn!("draft: 启动清理失败: {e}"),
        }
    }

    /// 队列攒到这么多条就触发一次后台落库。
    ///
    /// 取值的两头：太小则频繁抢 redb 的单写锁（草稿是这个库里最高频的写入方），
    /// 太大则一批的取码与查重堆在一起、且崩溃时丢得更多。**待实测调参。**
    /// 配成 0 会让每次入队都触发 flush，故下限钳到 1。
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
    /// ⚠️ 默认值来自设计稿 §7 的**纸面估算上界**，不是实测值，待真机数据出来后重定。
    pub(crate) fn draft_max_entries(&self) -> usize {
        self.engine_mgr
            .codetable_settings()
            .auto_phrase
            .draft_max_entries
    }
}

#[cfg(test)]
mod tests {
    use crate::coordinator::Coordinator;
    use std::sync::Arc;
    use wind_config::config::Config;
    use wind_store::Store;

    fn cfg_with_ttl(hours: u32) -> Config {
        let mut cfg = Config::default();
        cfg.schema.codetable.auto_phrase.draft_ttl_hours = hours;
        cfg
    }

    /// 带一个真 store 的无头协调器（`new_headless` 的 store 恒为 `None`，
    /// 用它测不到启动清理真的走进了存储层）。
    fn coord_with_store(
        tag: &str,
        hours: u32,
    ) -> (Arc<Coordinator>, Arc<Store>, std::path::PathBuf) {
        let db = std::env::temp_dir().join(format!("wind_draft_start_{tag}.redb"));
        let _ = std::fs::remove_file(&db);
        let store = Arc::new(Store::open(&db).unwrap());
        let c = Coordinator::new_headless_with_store(cfg_with_ttl(hours), None, Arc::clone(&store));
        (c, store, db)
    }

    /// 单位换算：配置里是**小时**，存储层的 `ttl_secs` 要的是**秒**。
    ///
    /// 漏掉 ×3600 不会让功能报错，只会让草稿在 24 **秒**后就被判过期——开关看着是开的，
    /// 用户却永远等不到一条草稿被用上。这类「只错在量纲上」的缺陷最难从行为上察觉，
    /// 故直接钉住换算本身。
    #[test]
    fn draft_ttl_is_hours_in_config_but_seconds_at_the_store() {
        let c = Coordinator::new_headless(cfg_with_ttl(24), None);
        assert_eq!(c.draft_ttl_secs(), 86_400);
        let c = Coordinator::new_headless(cfg_with_ttl(1), None);
        assert_eq!(c.draft_ttl_secs(), 3_600);
        // 0 = 永不过期，启动清理据此整个跳过。
        let c = Coordinator::new_headless(cfg_with_ttl(0), None);
        assert_eq!(c.draft_ttl_secs(), 0);
    }

    /// 启动清理**不能误杀还没过期的草稿**。它跑在启动线程上、没有任何 UI 反馈，
    /// 错删的表现是「昨天记下的词今天一条也召不回」，用户无从分辨是没记住还是被清了。
    #[test]
    fn startup_purge_keeps_drafts_that_are_still_fresh() {
        let (c, store, db) = coord_with_store("fresh", 24);
        store
            .add_drafts("wubi86", &[("aaaa".into(), "刚记的".into())])
            .unwrap();
        c.purge_drafts_on_start();
        assert_eq!(
            store.search_drafts("wubi86", "aaaa", 0).unwrap(),
            vec!["刚记的"],
            "还在有效期内的草稿不该被启动清理带走"
        );
        let _ = std::fs::remove_file(&db);
    }

    /// 两条早退：`TTL = 0`（永不过期）与**没有 store** 的宿主（测试 / 移动端裸构造）。
    /// 它跑在启动线程上，这两条是它不会在那里 panic 的全部依据。
    #[test]
    fn startup_purge_is_a_quiet_noop_without_a_ttl_or_a_store() {
        let (c, store, db) = coord_with_store("nottl", 0);
        store
            .add_drafts("wubi86", &[("aaaa".into(), "刚记的".into())])
            .unwrap();
        c.purge_drafts_on_start();
        assert_eq!(
            store.count_drafts("wubi86").unwrap(),
            1,
            "TTL=0 ＝ 永不过期，一条都不该动"
        );
        let _ = std::fs::remove_file(&db);
        // store 为 None：只要不 panic 即可。
        Coordinator::new_headless(cfg_with_ttl(24), None).purge_drafts_on_start();
    }
}
