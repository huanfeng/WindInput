//! 词语联想的**用户词文本索引**：按文本前缀取用户词 / 临时词（论坛 t185）。
//!
//! # 为什么要单独建一份
//!
//! 系统词的联想走反查索引（`.dict.yaml` 构建，词 → 编码），store 层的用户词与临时词
//! 不在里面。而 store 的键是 `schema\0code\0text`——按**编码**有序，按文本前缀查只能
//! 全表扫；用户词库实测可达十九万条，联想跑在上屏的同步链路上，扫不起。
//!
//! 于是在内存里按**文本**排一份（紧凑布局：一整块文本 + 定长条目），前缀查询是二分 +
//! 顺序走，与反查索引同形。
//!
//! # 何时过期
//!
//! 两个代次：
//!
//! - **结构代次**（[`wind_store::Store::words_generation`]：增删词、改权重）一变就过期；
//! - **临时词计数代次**（[`wind_store::Store::words_count_generation`]）变了，且本索引
//!   已建成满 [`COUNT_REBUILD_MIN_INTERVAL`] 才过期。临时词的 count 决定分档（门槛 2）
//!   与档内排序，但选词时它几乎每次都在变——若每变必重建，开着联想时几乎每次上屏都
//!   在后台全表扫一遍用户词库。节流的代价：临时词 count 的变化最多晚几秒才反映到联想
//!   分档上（跨过门槛那一刻的那一轮联想可能还按旧档排）。用户词的 count 不参与联想，
//!   store 根本不记代次。
//!
//! 过期时**本次照用旧索引**，另起后台线程重建（单飞）——绝不在按键线程上扫表。代价是
//! 刚写入的词要到重建完成后的下一次上屏才进联想（毫秒级到百毫秒级），换来按键路径零扫描。
//!
//! 只收两字及以上的词：联想要的是「上文的严格延长」，上文至少一个字，单字永远轮不到。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 只因临时词 count 变化而重建的最短间隔，见模块文档「何时过期」。
pub const COUNT_REBUILD_MIN_INTERVAL: Duration = Duration::from_secs(5);

/// 一条命中：词、它的一个记录码（供查 FREQ）、层内排序键与使用次数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserHit<'a> {
    pub text: &'a str,
    pub code: &'a str,
    /// 层内排序键（降序）：用户词 = 权重；临时词 = `count << 32 | created_at`。
    pub rank: i64,
    /// store 记录的使用次数（临时词的门槛看它；用户词仅作参考）。
    pub count: u32,
}

/// 建表输入的一行：(词, 码, 排序键, 次数)。
type Row = (String, String, i64, u32);

/// 一层（用户词或临时词）按文本排序的紧凑表。
#[derive(Default)]
struct TextTable {
    /// 全部「词 + 码」首尾相接。
    buf: String,
    /// 按文本字节序升序、文本唯一。
    entries: Vec<Entry>,
}

struct Entry {
    off: u32,
    text_len: u16,
    code_len: u16,
    rank: i64,
    count: u32,
}

impl TextTable {
    /// `rows` 可重复、可乱序；同文本多码时取排序键最大的那条（次数取和）。
    fn build(mut rows: Vec<Row>) -> Self {
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)));
        let mut t = TextTable::default();
        for (text, code, rank, count) in rows {
            if text.len() > u16::MAX as usize || code.len() > u16::MAX as usize {
                continue;
            }
            if let Some(last) = t.entries.last_mut()
                && t.buf[last.off as usize..last.off as usize + last.text_len as usize] == *text
            {
                last.count = last.count.saturating_add(count);
                continue;
            }
            t.entries.push(Entry {
                off: t.buf.len() as u32,
                text_len: text.len() as u16,
                code_len: code.len() as u16,
                rank,
                count,
            });
            t.buf.push_str(&text);
            t.buf.push_str(&code);
        }
        t
    }

    fn hit(&self, e: &Entry) -> UserHit<'_> {
        let (o, tl, cl) = (e.off as usize, e.text_len as usize, e.code_len as usize);
        UserHit {
            text: &self.buf[o..o + tl],
            code: &self.buf[o + tl..o + tl + cl],
            rank: e.rank,
            count: e.count,
        }
    }

    fn contains(&self, text: &str) -> bool {
        let i = self.entries.partition_point(|e| self.hit(e).text < text);
        self.entries
            .get(i)
            .is_some_and(|e| self.hit(e).text == text)
    }

    /// 以 `prefix` 开头且严格更长、且过 `keep` 的词，按排序键降序（同键按文本）取前 `limit` 条。
    fn with_prefix(
        &self,
        prefix: &str,
        limit: usize,
        keep: impl Fn(&UserHit<'_>) -> bool,
    ) -> Vec<UserHit<'_>> {
        let start = self.entries.partition_point(|e| self.hit(e).text < prefix);
        let mut hits: Vec<UserHit<'_>> = self.entries[start..]
            .iter()
            .map(|e| self.hit(e))
            .take_while(|h| h.text.starts_with(prefix))
            .filter(|h| h.text.len() > prefix.len() && keep(h))
            .collect();
        hits.sort_by(|a, b| b.rank.cmp(&a.rank).then(a.text.cmp(b.text)));
        hits.truncate(limit);
        hits
    }
}

/// 某个数据方案（store 归属 id）的用户词 + 临时词文本索引。
pub struct UserAssocIndex {
    data_schema: String,
    /// 建索引**之前**读到的结构代次与计数代次。先读后扫：扫描期间若有写入，代次已前进，
    /// 下次必判过期。
    generation: u64,
    count_generation: u64,
    built_at: Instant,
    user: TextTable,
    temp: TextTable,
}

impl UserAssocIndex {
    /// 全量扫 store 建索引（**会扫整张用户词表**，只在后台线程 / 预热 / 测试里调），扫完标记
    /// 待回收（库空闲 3 秒后回收 redb 读缓存）。
    ///
    /// ★ 临时词的排序键以 **count** 打头：临时词权重是写入时的定值（自动造词恒为
    /// `LEARN_ADD_WEIGHT`），按权重排等于按字典序排。
    pub fn build(store: &wind_store::Store, data_schema: &str) -> Self {
        let generation = store.words_generation();
        let count_generation = store.words_count_generation();
        let (mut user, mut temp): (Vec<Row>, Vec<Row>) = (Vec::new(), Vec::new());
        let multi = |t: &str| t.chars().nth(1).is_some();
        let mut scanned = 0usize;
        let r = store
            .for_each_user_word(data_schema, "", &mut |w| {
                scanned += 1;
                if multi(w.text) {
                    user.push((w.text.into(), w.code.into(), w.weight as i64, w.count));
                }
                true
            })
            .and_then(|_| {
                store.for_each_temp_word(data_schema, "", &mut |w| {
                    scanned += 1;
                    if multi(w.text) {
                        let rank = ((w.count as i64) << 32) | (w.created_at & 0xFFFF_FFFF);
                        temp.push((w.text.into(), w.code.into(), rank, w.count));
                    }
                    true
                })
            });
        if let Err(e) = r {
            tracing::warn!("联想用户词索引：读 store 失败 schema={data_schema}: {e}");
        }
        let idx = UserAssocIndex {
            data_schema: data_schema.to_string(),
            generation,
            count_generation,
            built_at: Instant::now(),
            user: TextTable::build(user),
            temp: TextTable::build(temp),
        };
        // 扫完标记待回收，库空闲 3 秒后回收（理由见 `text_codes::UserTextIndex::build`）。
        store.mark_scan_pending_rows(scanned);
        idx
    }

    /// 相对 store 当前状态是否过期。`count_interval`：仅临时词 count 变化时，建成多久后
    /// 才算过期（按键路径传 [`COUNT_REBUILD_MIN_INTERVAL`]，预热 / 测试传 0）。
    fn is_stale(&self, store: &wind_store::Store, count_interval: Duration) -> bool {
        self.generation != store.words_generation()
            || (self.count_generation != store.words_count_generation()
                && self.built_at.elapsed() >= count_interval)
    }

    /// 用户词或临时词里有没有这个词（精确匹配，只含两字及以上）。
    pub fn contains(&self, text: &str) -> bool {
        self.user.contains(text) || self.temp.contains(text)
    }

    pub fn user_with_prefix(&self, prefix: &str, limit: usize) -> Vec<UserHit<'_>> {
        self.user.with_prefix(prefix, limit, |_| true)
    }

    /// 临时词；`keep` 按次数分档（个人档 / 补位档）。
    pub fn temp_with_prefix(
        &self,
        prefix: &str,
        limit: usize,
        keep: impl Fn(&UserHit<'_>) -> bool,
    ) -> Vec<UserHit<'_>> {
        self.temp.with_prefix(prefix, limit, keep)
    }
}

/// 缓存槽：只留一份（当前联想方案的），外加「后台重建进行中」单飞标记。
#[derive(Default)]
pub(crate) struct UserAssocSlot {
    index: Option<Arc<UserAssocIndex>>,
    building: bool,
}

pub(crate) type SharedSlot = Arc<Mutex<UserAssocSlot>>;

/// 取可用索引（可能略旧）；缺失或过期时起一次后台重建（已有在建则不重复起）。
///
/// 方案不符时返回 `None`（不拿别的方案的用户词顶替）。
pub(crate) fn get_or_refresh(
    slot: &SharedSlot,
    store: &Arc<wind_store::Store>,
    data_schema: &str,
) -> Option<Arc<UserAssocIndex>> {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    let current = g
        .index
        .as_ref()
        .filter(|i| i.data_schema == data_schema)
        .cloned();
    let stale = current
        .as_ref()
        .is_none_or(|i| i.is_stale(store, COUNT_REBUILD_MIN_INTERVAL));
    if stale && !g.building {
        g.building = true;
        let (slot2, store2, schema2) = (slot.clone(), store.clone(), data_schema.to_string());
        let spawned = std::thread::Builder::new()
            .name("user-assoc-index".into())
            .spawn(move || {
                // 建索引中途 panic 也要复位 `building`，否则单飞标记卡死、此后永不重建。
                let _reset = BuildingGuard(slot2.clone());
                let idx = Arc::new(UserAssocIndex::build(&store2, &schema2));
                slot2.lock().unwrap_or_else(|e| e.into_inner()).index = Some(idx);
            });
        if let Err(e) = spawned {
            tracing::warn!("联想用户词索引：起重建线程失败: {e}");
            g.building = false;
        }
    }
    current
}

/// 后台重建线程退出（含 panic 展开）时复位单飞标记。
struct BuildingGuard(SharedSlot);

impl Drop for BuildingGuard {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).building = false;
    }
}

/// 阻塞地建好并放进槽（预热 / 测试用）。已是最新则不重建。
pub(crate) fn prewarm(slot: &SharedSlot, store: &wind_store::Store, data_schema: &str) -> bool {
    {
        let g = slot.lock().unwrap_or_else(|e| e.into_inner());
        if g.index
            .as_ref()
            .is_some_and(|i| i.data_schema == data_schema && !i.is_stale(store, Duration::ZERO))
        {
            return false;
        }
    }
    let idx = Arc::new(UserAssocIndex::build(store, data_schema));
    slot.lock().unwrap_or_else(|e| e.into_inner()).index = Some(idx);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts<'a>(v: Vec<UserHit<'a>>) -> Vec<(&'a str, i64)> {
        v.iter().map(|h| (h.text, h.rank)).collect()
    }

    fn row(t: &str, rank: i64, count: u32) -> Row {
        (t.into(), "c".into(), rank, count)
    }

    #[test]
    fn prefix_is_strict_sorted_by_rank_and_deduped() {
        let t = TextTable::build(vec![
            row("荷载", 5, 1),
            row("荷", 99, 1),
            row("荷载效应", 7, 1),
            row("荷载", 9, 2),
            row("花卉", 100, 1),
            row("荷包", 1, 1),
        ]);
        assert_eq!(
            texts(t.with_prefix("荷", 10, |_| true)),
            vec![("荷载", 9), ("荷载效应", 7), ("荷包", 1)]
        );
        assert_eq!(
            t.with_prefix("荷", 10, |_| true)[0].count,
            3,
            "同文本多码次数取和"
        );
        assert_eq!(
            texts(t.with_prefix("荷载", 10, |_| true)),
            vec![("荷载效应", 7)]
        );
        assert_eq!(texts(t.with_prefix("荷", 1, |_| true)), vec![("荷载", 9)]);
        assert!(t.with_prefix("荷", 10, |h| h.count > 5).is_empty());
        assert!(t.with_prefix("无", 10, |_| true).is_empty());
    }

    fn tmp_store(tag: &str) -> wind_store::Store {
        let p =
            std::env::temp_dir().join(format!("wind_user_assoc_{tag}_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&p);
        wind_store::Store::open(&p).unwrap()
    }

    /// ★ 连续选词（用户词 count +1、已有临时词 count +1）不让索引过期 ⇒ 不触发后台全表重建。
    #[test]
    fn repeated_picks_do_not_invalidate_index() {
        let s = tmp_store("picks");
        s.add_user_word("wb", "awfa", "荷载", 0, 0).unwrap();
        s.learn_temp_word("wb", "awgg", "荷叶田田", 800, 0).unwrap();
        let idx = UserAssocIndex::build(&s, "wb");
        for _ in 0..10 {
            s.on_word_selected("wb", "awfa", "荷载", 0, 0).unwrap();
            s.learn_temp_word("wb", "awgg", "荷叶田田", 800, 0).unwrap();
            s.increment_temp_if_exists("wb", "awgg", "荷叶田田")
                .unwrap();
            assert!(
                !idx.is_stale(&s, COUNT_REBUILD_MIN_INTERVAL),
                "选词后按键路径不该判过期"
            );
        }
        // 但临时词 count 确实变了：节流窗口过后（这里用 0 模拟）要跟上。
        assert!(idx.is_stale(&s, Duration::ZERO));
        // 结构变化（新词）立即过期。
        let idx = UserAssocIndex::build(&s, "wb");
        s.learn_temp_word("wb", "awhh", "荷塘月色", 800, 0).unwrap();
        assert!(idx.is_stale(&s, COUNT_REBUILD_MIN_INTERVAL));
    }

    /// 用户词权重变化（每 count_threshold 次 boost）是结构变化：排序键变了。
    #[test]
    fn user_weight_boost_invalidates_index() {
        let s = tmp_store("boost");
        s.add_user_word("wb", "awfa", "荷载", 0, 0).unwrap();
        let idx = UserAssocIndex::build(&s, "wb");
        s.on_word_selected("wb", "awfa", "荷载", 5, 2).unwrap();
        assert!(
            !idx.is_stale(&s, COUNT_REBUILD_MIN_INTERVAL),
            "count=1 未到阈值"
        );
        s.on_word_selected("wb", "awfa", "荷载", 5, 2).unwrap();
        assert!(
            idx.is_stale(&s, COUNT_REBUILD_MIN_INTERVAL),
            "count=2 权重 +5"
        );
    }

    #[test]
    fn contains_is_exact() {
        let s = tmp_store("contains");
        s.add_user_word("wb", "awfa", "荷载", 0, 0).unwrap();
        s.learn_temp_word("wb", "awgg", "荷叶田田", 800, 0).unwrap();
        let idx = UserAssocIndex::build(&s, "wb");
        assert!(idx.contains("荷载") && idx.contains("荷叶田田"));
        assert!(!idx.contains("荷") && !idx.contains("荷叶"));
    }

    /// ★ 建索引是一次全表扫：预热与后台重建两条路建完都**只标记**，库空闲满短档后才回收
    /// 一次（`docs/design/memory-footprint.md` §6）。
    #[test]
    fn every_build_marks_and_reclaims_after_short_idle() {
        const TICK: Duration = Duration::from_millis(20);
        const SCAN_IDLE: Duration = Duration::from_millis(150);
        let wait_drops = |s: &wind_store::Store, n: u64| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if s.page_cache_drops() >= n {
                    return true;
                }
                std::thread::sleep(TICK / 2);
            }
            false
        };
        let s = Arc::new(tmp_store("reclaim"));
        let big: Vec<_> = (0..wind_store::store::SCAN_RECLAIM_MIN_ROWS)
            .map(|i| wind_store::wdict::WordIo {
                code: format!("x{i}"),
                text: format!("词{i}"),
                weight: 0,
                count: 0,
                boundary: None,
            })
            .collect();
        s.import_user_words("wb", &big).unwrap();
        s.add_user_word("wb", "awfa", "荷载", 0, 0).unwrap();
        s.spawn_idle_cache_reclaimer(Duration::from_secs(3600), SCAN_IDLE, TICK);
        let slot = SharedSlot::default();
        assert!(prewarm(&slot, &s, "wb"));
        assert_eq!(s.page_cache_drops(), 0, "预热建完不立即回收");
        assert!(wait_drops(&s, 1), "空闲满短档后回收");

        s.add_user_word("wb", "awgg", "荷叶", 0, 0).unwrap();
        get_or_refresh(&slot, &s, "wb");
        assert!(wait_drops(&s, 2), "后台重建之后同样在空闲满短档后回收");
        assert!(
            slot.lock()
                .unwrap()
                .index
                .as_ref()
                .is_some_and(|i| i.contains("荷叶")),
            "回收前重建已完成"
        );
    }
}
