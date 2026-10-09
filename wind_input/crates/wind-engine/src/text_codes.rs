//! 「按词查编码」的**用户层**：某方案用户词的「词 → 全部编码」内存索引。
//!
//! 系统层是反查索引（`wind_dict::ReverseIndex`）。用户词在 store 里按 `schema\0code\0text`
//! 排序，按词查只能扫全表（可达十九万条），故在内存里按词排一份。形态照搬词语联想的
//! `user_assoc`：代次过期、过期时照用旧的、后台单飞重建，绝不在按键线程上扫表。
//!
//! 与 `user_assoc` 的区别：收**全部长度**（单字也要）、留**全部编码**、**按方案分槽**。
//! 设计见 `docs/design/text-code-lookup.md` §3.2。临时词第一期不收（没有使用方要）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 「按词查编码」的一次快照：系统层（反查索引）+ 用户层。某层 `None` = 这一层还没就绪，
/// **不是**「查不到」（沿用 `EngineManager::word_codes_in` 的三态约定）。
#[derive(Default, Clone)]
pub struct TextCodeView {
    pub(crate) system: Option<Arc<wind_dict::cached::ReverseIndex>>,
    pub(crate) user: Option<Arc<UserTextIndex>>,
}

impl TextCodeView {
    pub fn system_ready(&self) -> bool {
        self.system.is_some()
    }

    /// 至少有一层可查。
    pub fn has_any(&self) -> bool {
        self.system.is_some() || self.user.is_some()
    }

    /// 按「系统层 → 用户层」依次把该词的码交给 `pred`，任一返回 true 即停并返回 true。零分配。
    pub fn any_code(&self, text: &str, pred: &mut dyn FnMut(&str) -> bool) -> bool {
        if let Some(sys) = &self.system
            && let Some(list) = sys.codes_of(text)
            && list.iter().any(&mut *pred)
        {
            return true;
        }
        self.user
            .as_ref()
            .is_some_and(|u| u.codes_of(text).any(&mut *pred))
    }

    /// 该词全部编码：系统层在前、用户层补不重复者，整体按码长稳定排序（与反查索引「码长升序」同口径）。
    pub fn codes_of(&self, text: &str) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .system
            .as_ref()
            .and_then(|s| s.codes_of(text))
            .map(|l| l.iter().collect())
            .unwrap_or_default();
        if let Some(u) = &self.user {
            for c in u.codes_of(text) {
                if !v.contains(&c) {
                    v.push(c);
                }
            }
        }
        v.sort_by_key(|c| c.len());
        v
    }

    /// 编码提示用的「全码」：全部码里的最大码长；该长度上**系统层优先**（取其中最后一个，
    /// 与改用户层之前 `ReverseIndex::codes_of(..).last()` 同口径），系统层在该长度没有码
    /// 才取用户层的。
    ///
    /// 不能直接取 [`Self::codes_of`] 的最后一个：同长时用户码排在系统码后面，用户给「工」
    /// 加个同长的 `gggg`，提示就从词库里的 `aaaa` 变成了它。
    pub(crate) fn hint_code<'a>(&'a self, text: &str) -> Option<&'a str> {
        let sys: Vec<&'a str> = self
            .system
            .as_ref()
            .and_then(|s| s.codes_of(text))
            .map(|l| l.iter().collect())
            .unwrap_or_default();
        let user: Vec<&'a str> = self
            .user
            .as_ref()
            .map(|u| u.codes_of(text).collect())
            .unwrap_or_default();
        let max = sys.iter().chain(&user).map(|c| c.len()).max()?;
        let at_max = |v: Vec<&'a str>| v.into_iter().rev().find(|c| c.len() == max);
        at_max(sys).or_else(|| at_max(user))
    }
}

/// 同时保留的方案份数上限。在用的方案通常是：主码表、联想方案、辅助码引用的方案，
/// 取 4 留一格余量；超出时淘汰最久未用且不在重建中的那份。
const MAX_SLOTS: usize = 4;

/// 某方案用户词的「词 → 全部编码」紧凑表：一整块 `buf`（词 + 以 `\t` 连接的编码）+ 定长条目。
#[derive(Default)]
pub struct UserTextIndex {
    data_schema: String,
    /// 建表**之前**读到的代次。先读后扫：扫描期间有写入，代次已前进，下次必判过期。
    generation: (u64, u64),
    buf: String,
    /// 按词字节序升序、词唯一。
    entries: Vec<Entry>,
}

struct Entry {
    off: u32,
    text_len: u32,
    codes_len: u32,
}

impl UserTextIndex {
    /// `rows` 为 (词, 码)，可重复、可乱序。
    pub fn from_rows(
        data_schema: &str,
        generation: (u64, u64),
        mut rows: Vec<(String, String)>,
    ) -> Self {
        rows.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.len().cmp(&b.1.len()))
                .then(a.1.cmp(&b.1))
        });
        rows.dedup();
        let mut idx = UserTextIndex {
            data_schema: data_schema.to_string(),
            generation,
            ..Default::default()
        };
        let mut i = 0;
        while i < rows.len() {
            let text = rows[i].0.as_str();
            let off = idx.buf.len();
            idx.buf.push_str(text);
            let codes_start = idx.buf.len();
            let mut first = true;
            while i < rows.len() && rows[i].0 == text {
                if !first {
                    idx.buf.push('\t');
                }
                idx.buf.push_str(&rows[i].1);
                first = false;
                i += 1;
            }
            idx.entries.push(Entry {
                off: off as u32,
                text_len: (codes_start - off) as u32,
                codes_len: (idx.buf.len() - codes_start) as u32,
            });
        }
        idx
    }

    /// 全量扫该方案的用户词建表（**会扫整张用户词表**，只在后台线程 / 预热 / 测试里调）。
    ///
    /// 扫完标记「有扫描待回收」（[`wind_store::Store::mark_scan_pending_rows`]，小库不到门槛
    /// 不标记）：库空闲 3 秒后由 store 的回收线程丢 redb 读缓存并整理堆。不当场回收——自动造词
    /// 让打字中反复重建，当场关库重开会让按键线程陪等、每次重建都从冷缓存起扫。
    pub fn build(store: &wind_store::Store, data_schema: &str) -> Self {
        let generation = store.words_generation_of(data_schema);
        let mut rows = Vec::new();
        if let Err(e) = store.for_each_user_word(data_schema, "", &mut |w| {
            rows.push((w.text.to_string(), w.code.to_string()));
            true
        }) {
            tracing::warn!("按词查编码用户层：读 store 失败 schema={data_schema}: {e}");
        }
        let scanned = rows.len();
        let idx = Self::from_rows(data_schema, generation, rows);
        store.mark_scan_pending_rows(scanned);
        idx
    }

    fn text_of(&self, e: &Entry) -> &str {
        &self.buf[e.off as usize..(e.off + e.text_len) as usize]
    }

    /// 该词的全部用户编码（码长升序）；不在表里返回空迭代。
    pub fn codes_of(&self, text: &str) -> impl Iterator<Item = &str> {
        let i = self.entries.partition_point(|e| self.text_of(e) < text);
        let codes = self
            .entries
            .get(i)
            .filter(|e| !text.is_empty() && self.text_of(e) == text)
            .map(|e| {
                let s = (e.off + e.text_len) as usize;
                &self.buf[s..s + e.codes_len as usize]
            });
        codes.into_iter().flat_map(|s| s.split('\t'))
    }

    /// 供 `get_or_refresh` / `prewarm` 判过期。
    fn is_stale(&self, store: &wind_store::Store) -> bool {
        self.generation != store.words_generation_of(&self.data_schema)
    }
}

#[derive(Default)]
struct Slot {
    index: Option<Arc<UserTextIndex>>,
    building: bool,
    last_used: u64,
}

#[derive(Default)]
pub(crate) struct UserTextSlots {
    map: HashMap<String, Slot>,
    tick: u64,
    /// [`clear`] 一次加一。后台重建发起时记下，写回 / 复位 `building` 前比对：清空之后
    /// 旧线程既不该把表写进新槽，也不该把新槽的 `building` 复位（那会让新一轮重建重复起）。
    epoch: u64,
}

pub(crate) type SharedSlots = Arc<Mutex<UserTextSlots>>;

impl UserTextSlots {
    /// 为 `key` 腾位：新方案进来且已满时，淘汰最久未用、且不在重建中的一份。
    ///
    /// 所有槽都在重建时不淘汰，新方案照样进来，份数会暂时超过 [`MAX_SLOTS`]——在建的槽
    /// 被删掉的话它的重建线程白跑、单飞标记也跟着丢；多出的那份等下次有空闲槽时再被淘汰。
    fn make_room_for(&mut self, key: &str) {
        if self.map.contains_key(key) || self.map.len() < MAX_SLOTS {
            return;
        }
        if let Some(victim) = self
            .map
            .iter()
            .filter(|(_, s)| !s.building)
            .min_by_key(|(_, s)| s.last_used)
            .map(|(k, _)| k.clone())
        {
            self.map.remove(&victim);
        }
    }

    fn touch(&mut self, key: &str) -> &mut Slot {
        self.make_room_for(key);
        self.tick += 1;
        let tick = self.tick;
        let slot = self.map.entry(key.to_string()).or_default();
        slot.last_used = tick;
        slot
    }
}

/// 取可用索引（可能略旧）；缺失或过期时起一次后台重建（已有在建则不重复起）。
pub(crate) fn get_or_refresh(
    slots: &SharedSlots,
    store: &Arc<wind_store::Store>,
    data_schema: &str,
) -> Option<Arc<UserTextIndex>> {
    let mut g = slots.lock().unwrap_or_else(|e| e.into_inner());
    let slot = g.touch(data_schema);
    let current = slot.index.clone();
    let stale = current.as_ref().is_none_or(|i| i.is_stale(store));
    if stale && !slot.building {
        slot.building = true;
        let epoch = g.epoch;
        let (slots2, store2, key) = (slots.clone(), store.clone(), data_schema.to_string());
        let spawned = std::thread::Builder::new()
            .name("user-text-index".into())
            .spawn(move || {
                // 建表中途 panic 也要复位 `building`，否则单飞标记卡死、此后永不重建。
                let _reset = BuildingGuard(slots2.clone(), key.clone(), epoch);
                let idx = Arc::new(UserTextIndex::build(&store2, &key));
                install(&slots2, &key, epoch, idx);
            });
        if let Err(e) = spawned {
            tracing::warn!("按词查编码用户层：起重建线程失败: {e}");
            if let Some(s) = g.map.get_mut(data_schema) {
                s.building = false;
            }
        }
    }
    current
}

/// 后台重建的结果写回槽。发起后被 [`clear`] 过（代次不符）就丢弃。
fn install(slots: &SharedSlots, key: &str, epoch: u64, idx: Arc<UserTextIndex>) {
    let mut g = slots.lock().unwrap_or_else(|e| e.into_inner());
    if g.epoch != epoch {
        return;
    }
    if let Some(s) = g.map.get_mut(key) {
        s.index = Some(idx);
    }
}

/// 复位 `building`（第三项是发起时的代次，被清空过就不碰新槽）。
struct BuildingGuard(SharedSlots, String, u64);

impl Drop for BuildingGuard {
    fn drop(&mut self) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if g.epoch != self.2 {
            return;
        }
        if let Some(s) = g.map.get_mut(&self.1) {
            s.building = false;
        }
    }
}

/// 丢掉全部槽（含在建的：其重建线程写回时找不到槽即作罢）。
pub(crate) fn clear(slots: &SharedSlots) {
    let mut g = slots.lock().unwrap_or_else(|e| e.into_inner());
    g.map.clear();
    g.epoch += 1;
}

/// 已建好索引的槽数。
pub(crate) fn loaded(slots: &SharedSlots) -> usize {
    slots
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .map
        .values()
        .filter(|s| s.index.is_some())
        .count()
}

/// 阻塞地建好并放进槽（预热 / 测试用）。已是最新则不重建，返回是否真的建了。
pub(crate) fn prewarm(slots: &SharedSlots, store: &wind_store::Store, data_schema: &str) -> bool {
    {
        let mut g = slots.lock().unwrap_or_else(|e| e.into_inner());
        if g.touch(data_schema)
            .index
            .as_ref()
            .is_some_and(|i| !i.is_stale(store))
        {
            return false;
        }
    }
    let idx = Arc::new(UserTextIndex::build(store, data_schema));
    slots
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .touch(data_schema)
        .index = Some(idx);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(t, c)| (t.to_string(), c.to_string()))
            .collect()
    }

    /// 清空后，旧一轮重建的写回与 `building` 复位都不该落到新槽上。
    #[test]
    fn clear_fences_off_in_flight_rebuild() {
        let slots: SharedSlots = Default::default();
        let old_epoch = slots.lock().unwrap().epoch;
        slots.lock().unwrap().touch("wb").building = true;
        clear(&slots);
        // 清空后新一轮重建已起。
        slots.lock().unwrap().touch("wb").building = true;
        let idx = Arc::new(UserTextIndex::from_rows("wb", (0, 0), rows(&[("工", "a")])));
        install(&slots, "wb", old_epoch, idx);
        drop(BuildingGuard(slots.clone(), "wb".into(), old_epoch));
        let g = slots.lock().unwrap();
        let s = g.map.get("wb").unwrap();
        assert!(s.index.is_none(), "旧线程的结果不该写进清空后的新槽");
        assert!(s.building, "旧线程不该复位新一轮重建的单飞标记");
    }

    #[test]
    fn groups_codes_by_text_sorted_by_length() {
        let idx = UserTextIndex::from_rows(
            "wb",
            (0, 0),
            rows(&[
                ("工", "aaaa"),
                ("我", "q"),
                ("工", "a"),
                ("工", "aaaa"),
                ("工程", "aakg"),
            ]),
        );
        assert_eq!(
            idx.codes_of("工").collect::<Vec<_>>(),
            vec!["a", "aaaa"],
            "码长升序、去重"
        );
        assert_eq!(idx.codes_of("我").collect::<Vec<_>>(), vec!["q"]);
        assert_eq!(
            idx.codes_of("工程").collect::<Vec<_>>(),
            vec!["aakg"],
            "词组也收"
        );
        assert_eq!(idx.codes_of("无").count(), 0);
        assert_eq!(idx.codes_of("").count(), 0);
    }

    fn tmp_store(tag: &str) -> std::sync::Arc<wind_store::Store> {
        let p =
            std::env::temp_dir().join(format!("wind_text_codes_{tag}_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&p);
        std::sync::Arc::new(wind_store::Store::open(&p).unwrap())
    }

    #[test]
    fn prewarm_then_get_reads_user_words_of_that_schema_only() {
        let s = tmp_store("read");
        s.add_user_word("wb", "zzzz", "嗨", 0, 0).unwrap();
        s.add_user_word("pinyin", "hai", "嗨", 0, 0).unwrap();
        let slots = SharedSlots::default();
        assert!(prewarm(&slots, &s, "wb"));
        let idx = get_or_refresh(&slots, &s, "wb").expect("预热后就绪");
        assert_eq!(
            idx.codes_of("嗨").collect::<Vec<_>>(),
            vec!["zzzz"],
            "不串方案"
        );
        assert!(!prewarm(&slots, &s, "wb"), "已是最新则不重建");
    }

    /// ★ 别的方案写入不让本方案过期（依赖 store 的按方案代次）。
    #[test]
    fn other_schema_write_keeps_index_fresh() {
        let s = tmp_store("fresh");
        let slots = SharedSlots::default();
        prewarm(&slots, &s, "wb");
        s.add_user_word("pinyin", "nihao", "你好", 0, 0).unwrap();
        assert!(!prewarm(&slots, &s, "wb"), "拼音写入后五笔的索引仍是最新");
        s.add_user_word("wb", "wqvb", "你好", 0, 0).unwrap();
        assert!(prewarm(&slots, &s, "wb"), "本方案写入后必须重建");
    }

    #[test]
    fn get_or_refresh_returns_none_before_first_build() {
        let s = tmp_store("cold");
        let slots = SharedSlots::default();
        // 冷启动：本次拿不到（后台去建），调用方按「这一层没就绪」处理。
        assert!(get_or_refresh(&slots, &s, "wb").is_none());
    }

    /// 轮询到「该槽有索引且不在重建中」为止（上限 5 秒），返回那份索引。
    fn wait_built(slots: &SharedSlots, key: &str) -> Arc<UserTextIndex> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let g = slots.lock().unwrap();
                if let Some(s) = g.map.get(key)
                    && !s.building
                    && let Some(i) = &s.index
                {
                    return i.clone();
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "后台重建 5 秒内没有完成"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn building(slots: &SharedSlots, key: &str) -> bool {
        slots
            .lock()
            .unwrap()
            .map
            .get(key)
            .is_some_and(|s| s.building)
    }

    /// ★ 后台重建的结果会被**下一次调用**看到：冷启动那次返回 None 并起重建，建好后
    /// `get_or_refresh` 拿到新表、单飞标记已复位且不再起重建；之后本方案写入 → 本次照返回
    /// 旧表 + 后台重建 → 下一次拿到新内容。
    #[test]
    fn background_rebuild_is_seen_by_next_call() {
        let s = tmp_store("bg");
        s.add_user_word("wb", "zzzz", "嗨", 0, 0).unwrap();
        let slots = SharedSlots::default();
        assert!(
            get_or_refresh(&slots, &s, "wb").is_none(),
            "冷启动本次拿不到"
        );
        let built = wait_built(&slots, "wb");
        assert_eq!(built.codes_of("嗨").collect::<Vec<_>>(), vec!["zzzz"]);
        let got = get_or_refresh(&slots, &s, "wb").expect("建好后下一次调用拿得到");
        assert!(Arc::ptr_eq(&got, &built));
        assert!(!building(&slots, "wb"), "已是最新，不再起重建");

        s.add_user_word("wb", "aaaa", "嗨", 0, 0).unwrap();
        let stale = get_or_refresh(&slots, &s, "wb").expect("过期时照返回旧表");
        assert_eq!(
            stale.codes_of("嗨").collect::<Vec<_>>(),
            vec!["zzzz"],
            "本次是旧内容"
        );
        // `building` 在 get_or_refresh 返回前已于锁内置位，故这里等到的必是新表。
        let fresh = wait_built(&slots, "wb");
        assert!(!Arc::ptr_eq(&fresh, &stale));
        assert_eq!(
            fresh.codes_of("嗨").collect::<Vec<_>>(),
            vec!["aaaa", "zzzz"]
        );
        let again = get_or_refresh(&slots, &s, "wb").unwrap();
        assert!(Arc::ptr_eq(&again, &fresh), "下一次调用拿到新表");
        assert!(!building(&slots, "wb"));
    }

    /// 刚好到标记门槛的一份用户词（小库扫完不标记，见 `SCAN_RECLAIM_MIN_ROWS`）。
    fn big_dict() -> Vec<wind_store::wdict::WordIo> {
        (0..wind_store::store::SCAN_RECLAIM_MIN_ROWS)
            .map(|i| wind_store::wdict::WordIo {
                code: format!("x{i}"),
                text: format!("词{i}"),
                weight: 0,
                count: 0,
                boundary: None,
            })
            .collect()
    }

    const TICK: std::time::Duration = std::time::Duration::from_millis(20);
    const SCAN_IDLE: std::time::Duration = std::time::Duration::from_millis(150);
    const NEVER: std::time::Duration = std::time::Duration::from_secs(3600);

    /// 轮询到 `drops >= n`（上限 5 秒）。
    fn wait_drops(s: &wind_store::Store, n: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if s.page_cache_drops() >= n {
                return true;
            }
            std::thread::sleep(TICK / 2);
        }
        false
    }

    /// 小库：建完不标记，短档过后也不回收（门槛之下，留给空闲回收的长档）。
    #[test]
    fn small_build_keeps_the_page_cache() {
        let s = tmp_store("small");
        s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
        s.add_user_word("wb", "zzzz", "嗨", 0, 0).unwrap();
        let slots = SharedSlots::default();
        assert!(prewarm(&slots, &s, "wb"));
        std::thread::sleep(SCAN_IDLE * 4);
        assert_eq!(s.page_cache_drops(), 0);
    }

    /// ★ 建表是一次全表扫：预热（阻塞）与后台重建两条路建完都**只标记**，库空闲满短档后
    /// 才回收一次（`docs/design/memory-footprint.md` §6）——建完当场关库重开会让按键线程等。
    #[test]
    fn every_build_marks_and_reclaims_after_short_idle() {
        let s = tmp_store("reclaim");
        s.import_user_words("wb", &big_dict()).unwrap();
        s.add_user_word("wb", "zzzz", "嗨", 0, 0).unwrap();
        s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
        let slots = SharedSlots::default();
        assert!(prewarm(&slots, &s, "wb"));
        assert_eq!(s.page_cache_drops(), 0, "预热建完不立即回收");
        assert!(wait_drops(&s, 1), "空闲满短档后回收");

        s.add_user_word("wb", "aaaa", "嗨", 0, 0).unwrap();
        get_or_refresh(&slots, &s, "wb");
        let fresh = wait_built(&slots, "wb");
        assert_eq!(
            fresh.codes_of("嗨").collect::<Vec<_>>(),
            vec!["aaaa", "zzzz"]
        );
        assert!(wait_drops(&s, 2), "后台重建之后同样在空闲满短档后回收");
        std::thread::sleep(SCAN_IDLE * 3);
        assert_eq!(s.page_cache_drops(), 2, "每次扫描只回收一次");
    }

    /// ★ 模拟打字（自动造词）：持续访问库、其间不断写新词触发后台重建——一次都不回收；
    /// 停手满短档后回收**一次**。
    #[test]
    fn rebuilds_while_typing_never_reclaim_until_typing_stops() {
        let s = tmp_store("typing");
        s.import_user_words("wb", &big_dict()).unwrap();
        s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
        let slots = SharedSlots::default();
        let mut builds = 0;
        for i in 0..8 {
            // 「打字」：间隔远小于短档地查库；每轮造一个新词，令索引过期、后台重建。
            s.add_user_word("wb", &format!("y{i}"), &format!("新{i}"), 0, 0)
                .unwrap();
            get_or_refresh(&slots, &s, "wb");
            let t0 = std::time::Instant::now();
            while t0.elapsed() < SCAN_IDLE / 2 {
                let _ = s.search_user_words_prefix("wb", "x1", 5).unwrap();
                std::thread::sleep(TICK / 4);
            }
            if wait_built(&slots, "wb")
                .codes_of(&format!("新{i}"))
                .next()
                .is_some()
            {
                builds += 1;
            }
        }
        assert!(builds >= 4, "前提：打字期间确实重建过多次（{builds}）");
        assert_eq!(s.page_cache_drops(), 0, "持续打字期间不该回收");
        assert!(wait_drops(&s, 1), "停手满短档后回收");
        std::thread::sleep(SCAN_IDLE * 3);
        assert_eq!(s.page_cache_drops(), 1, "只回收一次");
    }

    #[test]
    fn slots_are_capped() {
        let s = tmp_store("cap");
        let slots = SharedSlots::default();
        for id in ["a", "b", "c", "d", "e", "f"] {
            prewarm(&slots, &s, id);
        }
        assert!(slots.lock().unwrap().map.len() <= MAX_SLOTS);
    }
}
