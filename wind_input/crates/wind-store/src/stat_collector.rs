//! 输入统计采集器（内存聚合 + 后台定时 flush）
//!
//! 对齐 Go `wind_input/internal/store/stat_collector.go`：
//! - `record()` 把单次上屏事件聚合进当日内存 `DailyStats`，跨天自动 flush 旧日并开新日；
//! - 后台线程每 30s flush 一次；`Drop`（对齐 Go `Close`）停线程并最终 flush；
//! - 活跃时间：两次上屏间隔 < 15s 视为持续输入，累加毫秒（用于速度统计）。
//!
//! 速度模型（分子分母口径必须对称）见 `stats.rs` 模块文档的「速度模型」一节。

use crate::stats::{
    CommitSource, DailyStats, StatsMeta, qualifies_for_max_speed, speed_chars_of,
    speed_per_minute_ms,
};
use crate::store::Store;
use chrono::{DateTime, Local, NaiveDate, Timelike};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tracing::warn;

/// 活跃输入判定阈值（毫秒）：两次上屏间隔小于此值视为持续输入。
const ACTIVE_THRESHOLD_MS: i64 = 15_000;
/// 后台 flush 周期（秒）。
const FLUSH_INTERVAL_SECS: u64 = 30;

/// 单次上屏的统计事件（仅元数据，不含原文）。
#[derive(Debug, Clone)]
pub struct StatEvent {
    pub timestamp: DateTime<Local>,
    pub chinese: u32,
    pub english: u32,
    pub punct: u32,
    pub other: u32,
    pub code_len: u32,      // 编码长度（0 = 标点/直接输入）
    pub candidate_pos: i32, // 候选位置：0=首选 … -1=非候选
    /// 本次上屏耗用的击键数，**0 = 未知**。仅用于速度分子封顶（见 `speed_chars_of`），
    /// 不进任何实际字数统计。多数路径等于 `code_len`；「一键出一串」的路径
    /// （重复上屏 / 快捷指令 / 网址上屏）须显式传 1，否则封顶规则盖不住它们——
    /// 那些路径的 `code_len` 恰恰是 0。
    pub keystrokes: u32,
    pub schema_id: String,
    pub source: CommitSource,
}

impl StatEvent {
    /// 本次事件的总字符数（各分类之和）。
    pub fn rune_count(&self) -> u32 {
        self.chinese
            .saturating_add(self.english)
            .saturating_add(self.punct)
            .saturating_add(self.other)
    }
}

impl Default for StatEvent {
    fn default() -> Self {
        Self {
            timestamp: Local::now(),
            chinese: 0,
            english: 0,
            punct: 0,
            other: 0,
            code_len: 0,
            candidate_pos: -1,
            keystrokes: 0,
            schema_id: String::new(),
            source: CommitSource::default(),
        }
    }
}

/// 把 v1 记录就地折算成 v2 口径。
///
/// 当天中途升级的场景：早上用旧版打了 3000 字（只有 `active_seconds`），下午换新版继续打。
/// 不折算的话新版会从 `active_millis = 0` 起累加，而 `active_seconds` 那半天的时间还在，
/// 两个分母各说各话；折算后当天的旧半段按旧口径入账、新半段按新口径增量，语义自洽。
fn migrate_to_speed_v2(d: &mut DailyStats) {
    if !d.speed_v2 {
        d.active_millis = d.active_seconds as u64 * 1000;
        d.speed_chars = d.total();
        d.speed_v2 = true;
    }
}

/// 内存聚合状态（受 `Shared.inner` 互斥锁保护）。
struct Inner {
    today: DailyStats,
    today_date: String,
    meta: StatsMeta,
    dirty: bool,
    last_commit: Option<DateTime<Local>>,
    /// 挂起（备份还原期间）：任何 flush（含后台定时那次）都不落盘，`resume` 清掉。
    suspended: bool,
    /// 元数据没读到（IO 错误）：`meta` 不可信，不写回（否则零值会盖掉累计值，M-3），下次 flush
    /// 重读。
    meta_unknown: bool,
}

/// 起步用的元数据：读不了 ⇒ 零值 + 标记不可信。
fn load_meta(store: &Store, speed_factor: f32) -> (StatsMeta, bool) {
    match store.stats_meta_or_recalc(speed_factor) {
        Ok(m) => (m, false),
        Err(e) => {
            warn!("统计元数据读不了，暂不写回：{e:#}");
            (StatsMeta::default(), true)
        }
    }
}

/// 采集器与后台线程共享的状态。
struct Shared {
    store: Arc<Store>,
    inner: Mutex<Inner>,
    /// 速度修正系数的 `f32::to_bits`。`max_speed` 在 flush 时就地算出并落库，
    /// 必须与展示端用同一个系数，故系数得跟着采集器走而不是只留在展示层。
    speed_factor: AtomicU32,
}

impl Shared {
    fn speed_factor(&self) -> f32 {
        f32::from_bits(self.speed_factor.load(Ordering::Relaxed))
    }
}

/// 输入统计采集器。
pub struct StatCollector {
    shared: Arc<Shared>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

fn today_string() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

impl StatCollector {
    /// 创建采集器：加载当日数据与元数据，启动后台定时 flush 线程。
    ///
    /// `speed_factor` 见 `stats::speed_per_minute_ms`。做成必填参数而非「先建后设」，
    /// 是为了让漏接线在编译期就暴露：默认值的真相源在 `wind-config`，本 crate 不该有第二份。
    pub fn new(store: Arc<Store>, speed_factor: f32) -> Self {
        let today = today_string();
        let mut today_stat = store.get_daily_stat(&today).unwrap_or_default();
        migrate_to_speed_v2(&mut today_stat);
        let (meta, meta_unknown) = load_meta(&store, speed_factor);
        let mut inner = Inner {
            today: today_stat,
            today_date: today.clone(),
            meta,
            dirty: false,
            last_commit: None,
            suspended: false,
            meta_unknown,
        };
        if inner.meta.first_day.is_empty() {
            inner.meta.first_day = today;
        }
        let shared = Arc::new(Shared {
            store,
            inner: Mutex::new(inner),
            speed_factor: AtomicU32::new(speed_factor.to_bits()),
        });
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let handle = {
            let sh = shared.clone();
            let st = stop.clone();
            thread::spawn(move || background_loop(sh, st))
        };
        Self {
            shared,
            stop,
            handle: Some(handle),
        }
    }

    /// 记录一次上屏事件。
    pub fn record(&self, event: StatEvent) {
        self.shared.record(event);
    }

    /// 更新速度修正系数（配置热更新时调用）。
    pub fn set_speed_factor(&self, factor: f32) {
        self.shared
            .speed_factor
            .store(factor.to_bits(), Ordering::Relaxed);
    }

    /// 当前速度修正系数（展示端取此值，保证与 `max_speed` 落库口径一致）。
    pub fn speed_factor(&self) -> f32 {
        self.shared.speed_factor()
    }

    /// 将内存数据持久化（线程安全）。
    pub fn flush(&self) {
        self.shared.flush();
    }

    /// 返回当日统计快照（跨天则先 flush 旧日）。
    pub fn get_today_stat(&self) -> DailyStats {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        let today = today_string();
        if inner.today_date != today {
            self.shared.flush_locked(&mut inner);
            inner.today = DailyStats::default();
            inner.today_date = today;
        }
        inner.today.clone()
    }

    /// 返回元数据快照。
    pub fn get_meta(&self) -> StatsMeta {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .meta
            .clone()
    }

    /// 清空内存统计（配合 Store::clear_stats）。
    pub fn reset(&self) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        let today = today_string();
        inner.today = DailyStats::default();
        inner.today_date = today.clone();
        inner.meta = StatsMeta {
            first_day: today,
            ..Default::default()
        };
        inner.dirty = false;
        inner.last_commit = None;
    }

    /// 挂起：先把内存里的落盘，之后任何 flush 都不写，直到 [`Self::resume`]。备份还原统计期间用——
    /// 不挂起的话，后台 30 秒那次 flush 可能恰在「还原写完、resume 之前」把旧的今天与总计写回去。
    pub fn suspend(&self) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.shared.flush_locked(&mut inner);
        inner.suspended = true;
    }

    /// 解除挂起但不重载（还原没碰统计时用：内存里攒的照常落盘）。
    pub fn unsuspend(&self) {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .suspended = false;
    }

    /// 暂停：flush 后由调用方释放 store（Windows 热替换）。
    pub fn pause(&self) {
        self.flush();
    }

    /// 恢复：重新从 store 加载当日与元数据。
    pub fn resume(&self) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        let today = today_string();
        inner.today = self.shared.store.get_daily_stat(&today).unwrap_or_default();
        migrate_to_speed_v2(&mut inner.today);
        inner.today_date = today.clone();
        let (meta, unknown) = load_meta(&self.shared.store, self.shared.speed_factor());
        inner.meta = meta;
        inner.meta_unknown = unknown;
        if inner.meta.first_day.is_empty() {
            inner.meta.first_day = today;
        }
        inner.dirty = false;
        inner.last_commit = None;
        inner.suspended = false;
    }
}

impl Drop for StatCollector {
    fn drop(&mut self) {
        {
            let (lock, cv) = &*self.stop;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        // 最终 flush（对齐 Go Close），确保退出时未落库数据持久化。
        self.shared.flush();
    }
}

impl Shared {
    fn record(&self, event: StatEvent) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let today = today_string();
        if inner.today_date != today {
            self.flush_locked(&mut inner);
            inner.today = DailyStats::default();
            inner.today_date = today;
        }

        let rune = event.rune_count();
        {
            let d = &mut inner.today;
            d.chinese = d.chinese.saturating_add(event.chinese);
            d.english = d.english.saturating_add(event.english);
            d.punct = d.punct.saturating_add(event.punct);
            d.other = d.other.saturating_add(event.other);
            d.commit_count = d.commit_count.saturating_add(1);

            let hour = event.timestamp.hour() as usize;
            if hour < 24 {
                d.hours[hour] = d.hours[hour].saturating_add(rune);
            }

            // 码长统计（仅候选词上屏且 code_len > 0）
            if event.code_len > 0 {
                d.code_len_sum = d.code_len_sum.saturating_add(event.code_len);
                d.code_len_count = d.code_len_count.saturating_add(1);
                let idx = (event.code_len - 1).min(5) as usize;
                d.code_len_dist[idx] = d.code_len_dist[idx].saturating_add(1);
            }
            // 选重统计（仅候选词上屏）
            if event.candidate_pos >= 0 {
                let idx = event.candidate_pos.min(4) as usize;
                d.cand_pos_dist[idx] = d.cand_pos_dist[idx].saturating_add(1);
            }
            // 按方案统计
            if !event.schema_id.is_empty() {
                let ss = d.by_schema.entry(event.schema_id.clone()).or_default();
                ss.total_chars = ss.total_chars.saturating_add(rune);
                ss.commit_count = ss.commit_count.saturating_add(1);
                if event.code_len > 0 {
                    ss.code_len_sum = ss.code_len_sum.saturating_add(event.code_len);
                    ss.code_len_count = ss.code_len_count.saturating_add(1);
                }
                if event.candidate_pos >= 0 {
                    let idx = event.candidate_pos.min(4) as usize;
                    ss.cand_pos_dist[idx] = ss.cand_pos_dist[idx].saturating_add(1);
                }
            }
            // 按来源统计
            if d.by_source.len() < CommitSource::COUNT {
                d.by_source.resize(CommitSource::COUNT, 0);
            }
            let si = event.source.index();
            d.by_source[si] = d.by_source[si].saturating_add(rune);
        }

        // ── 活跃时间与速度分子（两者口径必须对称）──
        //
        // 只有「能测到耗时」的那次上屏，其字符才计入速度分子：当天首次上屏、以及发呆
        // 超阈值后重新开打的那一次，前面花了多久无从得知，把它的字数算进分子而时间算 0，
        // 正是 v1 速度虚高的主因之一。字数照样进 `today.chinese/...`（用户看的产出不变），
        // 只是不进 `speed_chars`。
        //
        // 间隔取**毫秒**：`num_seconds()` 向零截断，0.8s 的间隔会被记成 0s，手越快分母
        // 被砍得越狠，且方向恒为「速度偏高」。
        let counted_ms = match inner.last_commit {
            Some(last) => {
                let ms = (event.timestamp - last).num_milliseconds();
                (0..ACTIVE_THRESHOLD_MS).contains(&ms).then_some(ms as u64)
            }
            None => None,
        };
        if let Some(ms) = counted_ms {
            let d = &mut inner.today;
            d.active_millis = d.active_millis.saturating_add(ms);
            // 旧字段仍被日曲线与历史数据消费，由毫秒总量派生（比逐次截断再相加更准）。
            d.active_seconds = (d.active_millis / 1000).min(u32::MAX as u64) as u32;
            d.speed_chars = d
                .speed_chars
                .saturating_add(speed_chars_of(rune, event.keystrokes));
        }
        inner.today.speed_v2 = true;
        inner.last_commit = Some(event.timestamp);
        inner.meta.total_chars = inner.meta.total_chars.saturating_add(rune as u64);
        inner.dirty = true;
    }

    fn flush(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.flush_locked(&mut inner);
    }

    fn flush_locked(&self, inner: &mut Inner) {
        // P4a 起统计落本机状态文件，不再受 redb 暂停影响；备份还原期间挂起不写
        if !inner.dirty || inner.suspended {
            return;
        }
        if let Err(e) = self.store.put_daily_stat(&inner.today_date, &inner.today) {
            warn!("flush DailyStats failed: {}", e);
            return;
        }
        update_streak(&mut inner.meta, &inner.today_date);
        // flush 每 30 秒一次，当天开头几次的活跃时间还是几秒——不设门槛就会把
        // 外推出来的上千字/分永久写进历史最快（见 qualifies_for_max_speed）。
        let (sp_chars, sp_ms) = inner.today.speed_parts();
        if qualifies_for_max_speed(sp_chars, sp_ms) {
            let sp = speed_per_minute_ms(sp_chars as u64, sp_ms, self.speed_factor());
            if sp > inner.meta.max_speed {
                inner.meta.max_speed = sp;
            }
        }
        if inner.meta_unknown {
            let (meta, unknown) = load_meta(&self.store, self.speed_factor());
            if unknown {
                // 当日数据已落盘；元数据等能读到再写
                inner.dirty = false;
                return;
            }
            inner.meta = meta;
            inner.meta_unknown = false;
            update_streak(&mut inner.meta, &inner.today_date);
        }
        if let Err(e) = self.store.put_stats_meta(&inner.meta) {
            warn!("flush StatsMeta failed: {}", e);
            return;
        }
        inner.dirty = false;
    }
}

/// 连续天数更新（对齐 Go `updateStreak`）。
fn update_streak(meta: &mut StatsMeta, today: &str) {
    if meta.streak_last_day.is_empty() {
        meta.streak_current = 1;
        meta.streak_last_day = today.to_string();
        if meta.streak_max < 1 {
            meta.streak_max = 1;
        }
        return;
    }
    if meta.streak_last_day == today {
        return;
    }
    let last = NaiveDate::parse_from_str(&meta.streak_last_day, "%Y-%m-%d");
    let td = NaiveDate::parse_from_str(today, "%Y-%m-%d");
    match (last, td) {
        (Ok(last), Ok(td)) => {
            if (td - last).num_days() <= 1 {
                meta.streak_current += 1;
            } else {
                meta.streak_current = 1;
            }
            meta.streak_last_day = today.to_string();
            if meta.streak_current > meta.streak_max {
                meta.streak_max = meta.streak_current;
            }
        }
        _ => {
            meta.streak_current = 1;
            meta.streak_last_day = today.to_string();
        }
    }
}

fn background_loop(shared: Arc<Shared>, stop: Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cv) = &*stop;
    loop {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let (stopped, _) = cv
            .wait_timeout_while(guard, Duration::from_secs(FLUSH_INTERVAL_SECS), |s| !*s)
            .unwrap_or_else(|e| e.into_inner());
        if *stopped {
            break;
        }
        drop(stopped);
        shared.flush();
    }
}

#[cfg(test)]
#[path = "stat_collector_tests.rs"]
mod tests;
