//! 输入统计存储（redb，按日期键）
//!
//! 与 Go 版本 `wind_input/internal/store/stats.go` **完整对齐**：每日聚合 `DailyStats`
//! （字符分类 / 24 小时分布 / 码长 / 选重 / 活跃时间 / 按方案 / 按来源）+ 全局 `StatsMeta`
//! （累计 / 首日 / 连续天数 / 最快速度）。采集器见 `stat_collector.rs`。
//!
//! - 每日：key = "YYYY-MM-DD"（STATS_DAILY 表），value = `DailyStats` 的 JSON。
//!   旧库只含 `{chinese, english}`，新增字段全部 `#[serde(default)]`，向后兼容无需迁移脚本。
//! - 元数据：存入现有 `META` 表的 `stats_meta` 键（不新增表定义）。
//! - `date` 是 redb 的 key，不进 `DailyStats` 结构（对齐 Go 用 bucket key）。
//!
//! # 速度模型（v2）
//!
//! 速度 = `speed_chars × 60000 / active_millis × speed_factor`，**四个量都与实际字数统计
//! 分开**。v1 用「全部字符 / 整秒活跃时间」，四条单向正偏差叠乘，实测偏高一倍以上：
//!
//! | 偏差 | 成因 | v2 的对策 |
//! |---|---|---|
//! | ① 整秒截断 | `num_seconds()` 向零截断，0.8s 的间隔记 0s；手越快砍得越狠 | 分母改毫秒（`active_millis`） |
//! | ② 口径不对称 | 间隔 ≥ 阈值时**时间丢弃、字数照计** | 段首事件的字数也不进分子 |
//! | ③ 一次上屏 = 一个时间点 | 4 键出 7 字、1 键出一整条快捷指令 | [`speed_chars_of`] 按击键数封顶 |
//! | ④ 打错字无从感知 | 退格重打的字符计两遍、耗时算一遍 | `speed_factor` 经验修正 |
//!
//! ①②③ 是结构性的，必须在采集期修；只有 ④ 无法从输入法内部观测，才交给系数。
//! **不要指望系数能替代 ①②③** —— 那等于用一个魔法数去掩盖三个可以精确修掉的量。

use crate::store::{META, STATS_DAILY, Store};
use chrono::{Duration, NaiveDate};
use redb::ReadableTable;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// META 表中存放统计全局元数据的键。
const STATS_META_KEY: &str = "stats_meta";

/// 上屏来源分类（对齐 Go `CommitSource`，末尾追加 Rust 特有的 Url/Mix）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum CommitSource {
    #[default]
    Candidate = 0, // 候选词选择
    RawInput = 1,    // 原始编码上屏（回车/无候选/顶码）
    Punctuation = 2, // 标点符号
    TempEnglish = 3, // 临时英文
    TempPinyin = 4,  // 临时拼音
    QuickInput = 5,  // 快捷输入
    FullWidth = 6,   // 全角转换（保留枚举值，Rust 暂不单独产出）
    ModeSwitch = 7,  // 模式切换上屏
    TsfDirect = 8,   // TSF 直接输入（保留枚举值，Rust 暂无该路径）
    SpecialMode = 9, // 引导键特殊模式
    Url = 10,        // 网址模式（Rust 特有）
    Mix = 11,        // 混合模式（Rust 特有）
}

impl CommitSource {
    /// 来源总数（by_source 数组长度）。
    pub const COUNT: usize = 12;
    /// 数组索引。
    pub fn index(self) -> usize {
        self as usize
    }
}

/// 每个方案的独立统计（对齐 Go `SchemaStats`）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SchemaStats {
    #[serde(default)]
    pub total_chars: u32,
    #[serde(default)]
    pub commit_count: u32,
    #[serde(default)]
    pub code_len_sum: u32,
    #[serde(default)]
    pub code_len_count: u32,
    #[serde(default)]
    pub cand_pos_dist: [u32; 5],
}

/// 单日聚合统计（对齐 Go `DailyStat`；`date` 为 redb key，不入结构）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DailyStats {
    // 字符分类
    #[serde(default)]
    pub chinese: u32,
    #[serde(default)]
    pub english: u32,
    #[serde(default)]
    pub punct: u32,
    #[serde(default)]
    pub other: u32,
    // 时段分布（按小时）
    #[serde(default)]
    pub hours: [u32; 24],
    // 上屏次数
    #[serde(default)]
    pub commit_count: u32,
    // 码长统计（仅候选词上屏）
    #[serde(default)]
    pub code_len_sum: u32,
    #[serde(default)]
    pub code_len_count: u32,
    #[serde(default)]
    pub code_len_dist: [u32; 6], // [1码,2码,3码,4码,5码,6码+]
    // 选重统计（仅候选词上屏）：[首选,2选,3选,4选,5选+]
    #[serde(default)]
    pub cand_pos_dist: [u32; 5],
    // 活跃输入时间（秒），连续输入间隔 < 阈值视为活跃
    #[serde(default)]
    pub active_seconds: u32,
    // ── 速度统计专用量（v2 模型，见模块文档「速度模型」）──
    // 速度分子：与 `total()` **刻意分开**。上屏字数要如实计（用户看的是产出），
    // 而速度分子要扣掉「短码打长词/一键出一串」这类字符（见 `speed_chars_of`），
    // 以及段首那些无法测量耗时的字符。两个量纲不同，共用一个累加器必错。
    #[serde(default)]
    pub speed_chars: u32,
    // 速度分母（毫秒）。`active_seconds` 用 `num_seconds()` 整秒截断，间隔 0.8s 记 0s，
    // 打得越快分母被砍得越狠（单向偏差），故速度另存毫秒。
    #[serde(default)]
    pub active_millis: u64,
    // 本记录是否由 v2 速度模型写过。旧库记录没有上面两个量，读回时要回退到旧口径；
    // 不能用「speed_chars == 0」判断——新模型下它合法地可以是 0（当天全是段首上屏）。
    #[serde(default)]
    pub speed_v2: bool,
    // 按方案分类
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub by_schema: HashMap<String, SchemaStats>,
    // 按来源分类（索引 = CommitSource::index）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_source: Vec<u32>,
}

impl DailyStats {
    /// 总字符数 = 各分类之和（与 Go `TotalChars` 增量恒等，故不单独存储）。
    pub fn total(&self) -> u32 {
        self.chinese
            .saturating_add(self.english)
            .saturating_add(self.punct)
            .saturating_add(self.other)
    }

    /// 速度的 (分子字符数, 分母毫秒)。区间速度把多天的两个分量分别累加后再除。
    ///
    /// v2 之前的记录没有这两个量，只能按旧口径（全部字符 / 整秒活跃时间）回退——
    /// 那个口径系统性偏高，故历史曲线与新数据之间会有一道台阶，这是无法回补的：
    /// 逐次上屏的击键数与毫秒间隔当时就没存下来。
    pub fn speed_parts(&self) -> (u32, u64) {
        if self.speed_v2 {
            (self.speed_chars, self.active_millis)
        } else {
            (self.total(), self.active_seconds as u64 * 1000)
        }
    }
}

/// 统计全局元数据（对齐 Go `StatsMeta`）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct StatsMeta {
    #[serde(default)]
    pub total_chars: u64,
    #[serde(default)]
    pub first_day: String,
    #[serde(default)]
    pub streak_current: u32,
    #[serde(default)]
    pub streak_max: u32,
    #[serde(default)]
    pub streak_last_day: String,
    #[serde(default)]
    pub max_speed: u32, // 历史最快速度（字/分钟，按天计算）
}

/// 统计摘要，对齐前端 StatsSummary（Stage 4 将扩展富字段）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct StatsSummary {
    pub today: u32,
    pub week: u32,
    pub month: u32,
    pub total: u64,
    pub streak: u32,
}

/// 触发单次上屏封顶的击键数上限：击键数 ≤ 此值即视为「短码出长词」。
pub const SPEED_SHORT_KEYSTROKES: u32 = 4;
/// 触发封顶时单次上屏最多计入速度分子的字符数。
pub const SPEED_SHORT_CHAR_CAP: u32 = 4;

/// 单次上屏计入**速度分子**的字符数（实际字数统计不走这里，如实计全部）。
///
/// 一次上屏在时间轴上只占一个间隔，可产出的字符数却不封顶：4 键出「中华人民共和国」、
/// 1 键触发快捷指令出二十几个字符——这些字符全额进分子会把速度顶到荒谬的值。
/// 故击键数 ≤ [`SPEED_SHORT_KEYSTROKES`] 时按 [`SPEED_SHORT_CHAR_CAP`] 封顶。
///
/// `keystrokes == 0` 表示**未知**（该上屏路径没有编码可依据），此时不封顶：
/// 宁可漏封（速度偏高一点）也不能误伤——TSF 英文批量上报一次就是几十个 1:1 击键的字符，
/// 误判成「一键出一串」会把英文速度直接砍到 4。
pub fn speed_chars_of(rune_count: u32, keystrokes: u32) -> u32 {
    if keystrokes > 0 && keystrokes <= SPEED_SHORT_KEYSTROKES {
        rune_count.min(SPEED_SHORT_CHAR_CAP)
    } else {
        rune_count
    }
}

/// 速度分母下限（毫秒）：分母再小也按此值算，否则外推出天文数字。
const MIN_SPEED_WINDOW_MS: u64 = 5_000;

/// 每分钟字数 = 分子 × 60000 / 分母(ms) × 修正系数。
///
/// `factor` 是经验修正（见 `StatsConfig::speed_factor`）：输入法无从知道用户打错了没有，
/// 打错后退格重打的字符会被计两遍、耗时却只算一遍，方向恒为正偏差，故出厂取 < 1。
/// 非有限值或 ≤ 0 一律当 1.0 处理（配置写坏不该把速度清零）。
///
/// 分母小时结果仍是**外推值**而非实测速度，展示当日/区间速度尚可（分母通常够大），
/// 但绝不可直接拿去刷新历史最快——那个是永久记录，见 [`qualifies_for_max_speed`]。
pub fn speed_per_minute_ms(chars: u64, active_millis: u64, factor: f32) -> u32 {
    if chars == 0 || active_millis == 0 {
        return 0;
    }
    let ms = active_millis.max(MIN_SPEED_WINDOW_MS);
    let f = if factor.is_finite() && factor > 0.0 {
        factor as f64
    } else {
        1.0
    };
    let v = (chars as f64) * 60_000.0 * f / (ms as f64);
    v.round().clamp(0.0, u32::MAX as f64) as u32
}

/// 计入「历史最快」所需的最小活跃时长（毫秒）。
const MIN_SPEED_SAMPLE_MS: u64 = 60_000;
/// 计入「历史最快」所需的最小字数。
const MIN_SPEED_SAMPLE_CHARS: u32 = 100;

/// 该样本是否够格刷新「历史最快」。
///
/// 活跃时间只累加**相邻两次上屏的间隔**，段首那次不计时（见 `StatCollector::record`），
/// 于是每天开头必然出现「字数已有几十上百、活跃时间还是几秒」的窗口；
/// 此时 [`speed_per_minute_ms`] 会外推出上千字/分。而 `max_speed` 是永久记录，
/// 一旦被这种样本污染就再也降不回来（除非重算），故这里要求样本量足够才参与比较。
pub fn qualifies_for_max_speed(chars: u32, active_millis: u64) -> bool {
    active_millis >= MIN_SPEED_SAMPLE_MS && chars >= MIN_SPEED_SAMPLE_CHARS
}

impl Store {
    /// 累加某日的中文/英文上屏字符数（读改写）。Stage 1 保留，供现有采集单点使用。
    pub fn record_stat(&self, date: &str, chinese: u32, english: u32) -> anyhow::Result<()> {
        if chinese == 0 && english == 0 {
            return Ok(());
        }
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(STATS_DAILY)?;
                let mut rec: DailyStats = t
                    .get(date)?
                    .and_then(|g| serde_json::from_slice(g.value()).ok())
                    .unwrap_or_default();
                rec.chinese = rec.chinese.saturating_add(chinese);
                rec.english = rec.english.saturating_add(english);
                let bytes = serde_json::to_vec(&rec)?;
                t.insert(date, bytes.as_slice())?;
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 覆盖写入某日完整统计（采集器 flush 用）。
    pub fn put_daily_stat(&self, date: &str, stat: &DailyStats) -> anyhow::Result<()> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(STATS_DAILY)?;
                let bytes = serde_json::to_vec(stat)?;
                t.insert(date, bytes.as_slice())?;
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 取某日统计（无则零值）。
    pub fn get_daily_stat(&self, date: &str) -> anyhow::Result<DailyStats> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(STATS_DAILY)?;
            Ok(t.get(date)?
                .and_then(|g| serde_json::from_slice(g.value()).ok())
                .unwrap_or_default())
        })
    }

    /// 取日期区间 [from, to]（含端点；YYYY-MM-DD 字典序即时间序）。
    pub fn daily_stats(&self, from: &str, to: &str) -> anyhow::Result<Vec<(String, DailyStats)>> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(STATS_DAILY)?;
            let mut out = Vec::new();
            for item in t.range(from..)? {
                let (k, v) = item?;
                let date = k.value();
                if date > to {
                    break;
                }
                if let Ok(rec) = serde_json::from_slice::<DailyStats>(v.value()) {
                    out.push((date.to_string(), rec));
                }
            }
            Ok(out)
        })
    }

    /// 全部每日统计（升序）。
    fn all_daily_stats(&self) -> anyhow::Result<Vec<(String, DailyStats)>> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(STATS_DAILY)?;
            let mut out = Vec::new();
            for item in t.range::<&str>(..)? {
                let (k, v) = item?;
                if let Ok(rec) = serde_json::from_slice::<DailyStats>(v.value()) {
                    out.push((k.value().to_string(), rec));
                }
            }
            Ok(out)
        })
    }

    /// 导出全部每日统计为 jsonl（每行 {"date","stat"}）。
    pub fn export_stats_jsonl(&self) -> anyhow::Result<String> {
        let all = self.daily_stats("0000-01-01", "9999-12-31")?;
        let mut out = String::new();
        for (date, stat) in all {
            out.push_str(&serde_json::to_string(
                &serde_json::json!({ "date": date, "stat": stat }),
            )?);
            out.push('\n');
        }
        Ok(out)
    }

    /// 从 jsonl 导入每日统计。overwrite=false 时已存在日跳过（以本地为准）。
    /// 返回 (imported, skipped_bad_lines)。
    pub fn import_stats_jsonl(
        &self,
        text: &str,
        overwrite: bool,
    ) -> anyhow::Result<(usize, usize)> {
        let normalized = wind_utils::text::normalize_input(text);
        let text = normalized.as_ref();
        // 非覆盖模式先一次性收集已存在日期，避免逐行开读事务（备份可跨数年 daily）。
        let mut existing: std::collections::HashSet<String> = if overwrite {
            Default::default()
        } else {
            self.daily_stats("0000-01-01", "9999-12-31")?
                .into_iter()
                .map(|(d, _)| d)
                .collect()
        };
        let mut imported = 0usize;
        let mut skipped = 0usize;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                skipped += 1;
                continue;
            };
            let (Some(date), Some(stat)) = (
                v.get("date").and_then(|x| x.as_str()),
                v.get("stat")
                    .and_then(|x| serde_json::from_value::<DailyStats>(x.clone()).ok()),
            ) else {
                skipped += 1;
                continue;
            };
            if !overwrite {
                if existing.contains(date) {
                    continue;
                }
                // 批内重复日期同样"先到者胜"，与逐行判存的原语义一致。
                existing.insert(date.to_string());
            }
            self.put_daily_stat(date, &stat)?;
            imported += 1;
        }
        Ok((imported, skipped))
    }

    /// 读取统计全局元数据（无则零值）。
    pub fn get_stats_meta(&self) -> anyhow::Result<StatsMeta> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(META)?;
            Ok(t.get(STATS_META_KEY)?
                .and_then(|g| serde_json::from_slice(g.value()).ok())
                .unwrap_or_default())
        })
    }

    /// 写入统计全局元数据。
    pub fn put_stats_meta(&self, meta: &StatsMeta) -> anyhow::Result<()> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(META)?;
                let bytes = serde_json::to_vec(meta)?;
                t.insert(STATS_META_KEY, bytes.as_slice())?;
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 从现存每日数据重建全局元数据（prune 后/meta 缺失时调用，对齐 Go `RecalculateStatsMeta`）。
    ///
    /// `speed_factor` 见 [`speed_per_minute_ms`]：`max_speed` 是落库的成品值，重算时必须
    /// 与展示端用同一个系数，否则「历史最快」会比当日速度高出一个恒定倍数。
    pub fn recalculate_stats_meta(&self, speed_factor: f32) -> anyhow::Result<StatsMeta> {
        let all = self.all_daily_stats()?;
        let mut meta = StatsMeta::default();
        let mut dates = Vec::with_capacity(all.len());
        for (date, stat) in &all {
            if meta.first_day.is_empty() {
                meta.first_day = date.clone();
            }
            meta.total_chars += stat.total() as u64;
            // 样本太小的日子不参与历史最快（见 qualifies_for_max_speed）；
            // 本函数从零重算，故也是修正历史污染值的入口。
            let (sp_chars, sp_ms) = stat.speed_parts();
            if qualifies_for_max_speed(sp_chars, sp_ms) {
                let sp = speed_per_minute_ms(sp_chars as u64, sp_ms, speed_factor);
                if sp > meta.max_speed {
                    meta.max_speed = sp;
                }
            }
            dates.push(date.clone());
        }
        let (cur, mx, last) = calculate_streaks(&dates);
        meta.streak_current = cur;
        meta.streak_max = mx;
        meta.streak_last_day = last;
        self.put_stats_meta(&meta)?;
        Ok(meta)
    }

    /// 统计摘要：today/week(近7日)/month(近30日)/total/streak（连续天数）。
    pub fn stats_summary(&self, today: &str) -> anyhow::Result<StatsSummary> {
        let all = self.all_daily_stats()?;
        let today_date = NaiveDate::parse_from_str(today, "%Y-%m-%d").ok();

        let mut s = StatsSummary::default();
        let mut have: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (d, rec) in &all {
            let t = rec.total();
            s.total += t as u64;
            if t > 0 {
                have.insert(d.clone());
            }
            if d == today {
                s.today = t;
            }
            if let Some(td) = today_date
                && let Ok(dd) = NaiveDate::parse_from_str(d, "%Y-%m-%d")
            {
                let days = (td - dd).num_days();
                if (0..7).contains(&days) {
                    s.week = s.week.saturating_add(t);
                }
                if (0..30).contains(&days) {
                    s.month = s.month.saturating_add(t);
                }
            }
        }

        if let Some(td) = today_date {
            let mut cur = if have.contains(today) {
                td
            } else {
                td - Duration::days(1)
            };
            while have.contains(&cur.format("%Y-%m-%d").to_string()) {
                s.streak += 1;
                cur -= Duration::days(1);
            }
        }
        Ok(s)
    }

    /// 清空所有统计（每日 + 元数据），返回删除的天数。
    pub fn clear_stats(&self) -> anyhow::Result<usize> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let n;
            {
                let mut t = txn.open_table(STATS_DAILY)?;
                let keys: Vec<String> = {
                    let mut ks = Vec::new();
                    for item in t.range::<&str>(..)? {
                        ks.push(item?.0.value().to_string());
                    }
                    ks
                };
                n = keys.len();
                for k in keys {
                    t.remove(k.as_str())?;
                }
            }
            {
                // 同事务清除元数据，保证 daily 与 meta 一致。
                let mut m = txn.open_table(META)?;
                m.remove(STATS_META_KEY)?;
            }
            txn.commit()?;
            Ok(n)
        })
    }

    /// 删除 `before`（不含）之前的统计，返回删除条数。
    pub fn prune_stats_before(&self, before: &str) -> anyhow::Result<usize> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let n;
            {
                let mut t = txn.open_table(STATS_DAILY)?;
                let keys: Vec<String> = {
                    let mut ks = Vec::new();
                    for item in t.range::<&str>(..before)? {
                        ks.push(item?.0.value().to_string());
                    }
                    ks
                };
                n = keys.len();
                for k in keys {
                    t.remove(k.as_str())?;
                }
            }
            txn.commit()?;
            Ok(n)
        })
    }
}

/// 连续天数计算（对齐 Go `calculateStreaks`）：dates 须按升序。
fn calculate_streaks(dates: &[String]) -> (u32, u32, String) {
    let mut current = 0u32;
    let mut max = 0u32;
    let mut last = String::new();
    let mut prev: Option<NaiveDate> = None;
    for date in dates {
        let day = match NaiveDate::parse_from_str(date, "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => continue,
        };
        if last.is_empty() {
            current = 1;
            max = 1;
        } else if let Some(p) = prev {
            if (day - p).num_days() <= 1 {
                current += 1;
            } else {
                current = 1;
            }
        }
        if current > max {
            max = current;
        }
        prev = Some(day);
        last = date.clone();
    }
    (current, max, last)
}

/// 上屏文本字符分类：返回 (中文, 英文)。Stage 1 保留 2 分类，供现有采集单点使用。
pub fn classify_chars(text: &str) -> (u32, u32) {
    let mut chinese = 0u32;
    let mut english = 0u32;
    for ch in text.chars() {
        if is_cjk(ch) {
            chinese += 1;
        } else if ch.is_ascii_alphabetic() {
            english += 1;
        }
    }
    (chinese, english)
}

/// 上屏文本 4 分类：返回 (中文, 英文, 标点, 其他)。对齐 Go `ClassifyChars`。
pub fn classify_chars_full(text: &str) -> (u32, u32, u32, u32) {
    let (mut chinese, mut english, mut punct, mut other) = (0u32, 0u32, 0u32, 0u32);
    for ch in text.chars() {
        if is_cjk(ch) {
            chinese += 1;
        } else if ch.is_ascii_alphabetic() {
            english += 1;
        } else if is_punct_or_symbol(ch) {
            punct += 1;
        } else {
            other += 1;
        }
    }
    (chinese, english, punct, other)
}

/// 标点/符号判定（ASCII 标点 + 常见 CJK/全角标点区段），近似 Go `unicode.IsPunct||IsSymbol`。
/// 全角数字(FF10-FF19)和全角字母(FF21-FF3A/FF41-FF5A)在 Go 中为 other，此处同样排除。
fn is_punct_or_symbol(ch: char) -> bool {
    if ch.is_ascii_punctuation() {
        return true;
    }
    matches!(ch as u32,
        0x2000..=0x206F   // 通用标点
        | 0x3000..=0x303F // CJK 符号和标点
        | 0xFE30..=0xFE4F // CJK 兼容形式
        // 全角 ASCII 标点区段（跳过 FF10-FF19 全角数字、FF21-FF3A/FF41-FF5A 全角字母）
        | 0xFF00..=0xFF0F // ！＂＃＄％＆＇（）＊＋，－．／
        | 0xFF1A..=0xFF20 // ：；＜＝＞？＠
        | 0xFF3B..=0xFF40 // ［＼］＾＿｀
        | 0xFF5B..=0xFFEF // ｛｜｝～ + 半宽形式
    )
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x4E00..=0x9FFF   // CJK 统一表意文字
        | 0x3400..=0x4DBF // 扩展 A
        | 0xF900..=0xFAFF // 兼容表意文字
        | 0x20000..=0x2A6DF // 扩展 B
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn stats_jsonl_roundtrip_skip_existing() {
        let path = tmp("wind_st_io.redb");
        let s = Store::open(&path).unwrap();
        let d = DailyStats {
            chinese: 42,
            ..Default::default()
        };
        s.put_daily_stat("2026-07-01", &d).unwrap();
        let text = s.export_stats_jsonl().unwrap();
        assert!(text.contains("2026-07-01"));

        let path2 = tmp("wind_st_io2.redb");
        let s2 = Store::open(&path2).unwrap();
        let local = DailyStats {
            chinese: 7,
            ..Default::default()
        };
        s2.put_daily_stat("2026-07-01", &local).unwrap();
        // overwrite=false：已存在日跳过
        let (imp, _) = s2.import_stats_jsonl(&text, false).unwrap();
        assert_eq!(imp, 0);
        assert_eq!(s2.get_daily_stat("2026-07-01").unwrap().chinese, 7);
        // overwrite=true：覆盖
        let (imp2, _) = s2.import_stats_jsonl(&text, true).unwrap();
        assert_eq!(imp2, 1);
        assert_eq!(s2.get_daily_stat("2026-07-01").unwrap().chinese, 42);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn test_record_and_daily() {
        let path = tmp("wind_stats_daily.redb");
        let s = Store::open(&path).unwrap();
        s.record_stat("2026-06-18", 10, 2).unwrap();
        s.record_stat("2026-06-18", 5, 0).unwrap(); // 累加
        s.record_stat("2026-06-20", 3, 7).unwrap();

        assert_eq!(s.get_daily_stat("2026-06-18").unwrap().total(), 17);
        let range = s.daily_stats("2026-06-18", "2026-06-20").unwrap();
        assert_eq!(range.len(), 2);
        assert_eq!(range[0].0, "2026-06-18");
        assert_eq!(range[1].1.total(), 10);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_summary_and_streak() {
        let path = tmp("wind_stats_summary.redb");
        let s = Store::open(&path).unwrap();
        s.record_stat("2026-06-20", 100, 0).unwrap(); // today
        s.record_stat("2026-06-19", 50, 0).unwrap();
        s.record_stat("2026-06-18", 30, 0).unwrap();
        s.record_stat("2026-06-01", 5, 0).unwrap();

        let sum = s.stats_summary("2026-06-20").unwrap();
        assert_eq!(sum.today, 100);
        assert_eq!(sum.week, 180, "近7日=100+50+30");
        assert_eq!(sum.month, 185, "近30日含06-01");
        assert_eq!(sum.total, 185);
        assert_eq!(sum.streak, 3, "06-18~06-20 连续");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_classify_chars() {
        let (zh, en) = classify_chars("你好abc，123");
        assert_eq!(zh, 2);
        assert_eq!(en, 3);
    }

    // ── Stage 1 新增 ──

    #[test]
    fn test_speed_per_minute_ms() {
        assert_eq!(speed_per_minute_ms(252, 6_000, 1.0), 2520);
        assert_eq!(speed_per_minute_ms(252, 120_000, 1.0), 126);
        assert_eq!(
            speed_per_minute_ms(60, 3_000, 1.0),
            720,
            "极短时间触发 5s 下限"
        );
        assert_eq!(speed_per_minute_ms(0, 60_000, 1.0), 0);
        assert_eq!(speed_per_minute_ms(100, 0, 1.0), 0);
        // 亚秒间隔不再被截断成 0：v1 的 `num_seconds()` 会把这 900ms 记成 0 秒，
        // 于是分母凭空消失、速度被外推——这正是 v1 偏高的头号来源。
        assert_eq!(speed_per_minute_ms(2, 900, 1.0), 24, "分母走 5s 下限");
    }

    /// 修正系数直接乘在结果上，且写坏的值不得把速度清零。
    #[test]
    fn speed_factor_scales_result_and_tolerates_garbage() {
        assert_eq!(speed_per_minute_ms(300, 60_000, 1.0), 300);
        assert_eq!(speed_per_minute_ms(300, 60_000, 0.85), 255);
        for bad in [0.0f32, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                speed_per_minute_ms(300, 60_000, bad),
                300,
                "非法系数 {bad} 应按 1.0 处理而非清零"
            );
        }
    }

    /// 短码出长词/一键出一串：速度分子封顶，**实际字数不受影响**（那由调用方另计）。
    #[test]
    fn speed_chars_capped_only_for_short_keystrokes() {
        // 4 键出 7 字（「中华人民共和国」）→ 只计 4。
        assert_eq!(speed_chars_of(7, 4), 4);
        // 1 键触发快捷指令出 30 字 → 只计 4。
        assert_eq!(speed_chars_of(30, 1), 4);
        // 击键数超过阈值 → 不封顶（正常整句输入不该被削）。
        assert_eq!(speed_chars_of(12, 5), 12);
        // 封顶只取下限，不会把短内容拔高。
        assert_eq!(speed_chars_of(1, 1), 1);
        assert_eq!(speed_chars_of(2, 3), 2);
        // 击键数未知（0）一律不封顶：宁可漏封也不误伤 TSF 英文那种批量 1:1 上报。
        assert_eq!(speed_chars_of(30, 0), 30);
    }

    /// v1 记录没有速度专用量，须回退到旧口径而不是当成「0 字符」。
    #[test]
    fn speed_parts_falls_back_for_v1_records() {
        let v1 = DailyStats {
            chinese: 240,
            active_seconds: 120,
            ..Default::default()
        };
        assert_eq!(v1.speed_parts(), (240, 120_000));

        // v2 记录即使分子为 0（当天全是段首上屏）也不能回退——那会读成 240。
        let v2 = DailyStats {
            chinese: 240,
            active_seconds: 120,
            speed_v2: true,
            speed_chars: 0,
            active_millis: 0,
            ..Default::default()
        };
        assert_eq!(v2.speed_parts(), (0, 0));

        let v2b = DailyStats {
            chinese: 240,
            speed_v2: true,
            speed_chars: 180,
            active_millis: 90_500,
            ..Default::default()
        };
        assert_eq!(v2b.speed_parts(), (180, 90_500));
    }

    /// 守护：小样本不得刷新「历史最快」。
    ///
    /// 上面那几个用例正说明 `speed_per_minute` 在分母小时给的是**外推值**
    /// （252 字 / 6 秒 → 2520 字每分）。展示当日速度时无妨，写进永久的 max_speed
    /// 就是污染——现实中表现为「历史最快 1500 字/分」这种不可能的数字。
    #[test]
    fn small_samples_never_qualify_for_max_speed() {
        // 典型污染场景：当天开头，第一次上屏的字不计时间，第二次上屏才攒出几秒。
        assert!(
            !qualifies_for_max_speed(125, 5_000),
            "125字/5秒 外推 1500，须挡掉"
        );
        assert!(!qualifies_for_max_speed(252, 6_000));
        // 时长够但字数太少 → 仍不算（几十个字的偶发爆发不代表稳定速度）。
        assert!(!qualifies_for_max_speed(50, 300_000));
        // 字数够但时长太短 → 不算。
        assert!(!qualifies_for_max_speed(5000, 30_000));
        // 两者都够 → 计入。
        assert!(qualifies_for_max_speed(100, 60_000));
        assert!(qualifies_for_max_speed(6000, 600_000));
    }

    #[test]
    fn test_classify_chars_full() {
        let (zh, en, pu, ot) = classify_chars_full("你好abc，123");
        assert_eq!(zh, 2);
        assert_eq!(en, 3);
        assert_eq!(pu, 1, "，为全角标点");
        assert_eq!(ot, 3, "123 计入其他");
    }

    #[test]
    fn test_classify_fullwidth_digits_as_other() {
        // 全角数字 U+FF10-FF19 对齐 Go：unicode.IsPunct/IsSymbol 均为 false → other
        let (zh, en, pu, ot) = classify_chars_full("０１２３４５６７８９");
        assert_eq!(pu, 0, "全角数字不应计为标点");
        assert_eq!(ot, 10, "全角数字应计为其他");
        assert_eq!(zh, 0);
        assert_eq!(en, 0);
    }

    #[test]
    fn test_classify_fullwidth_letters_as_other() {
        // 全角字母 U+FF21-FF3A/FF41-FF5A 对齐 Go：非 ASCII alpha、非 IsPunct → other
        let (zh, en, pu, ot) = classify_chars_full("ＡＢＣａｂｃ");
        assert_eq!(pu, 0, "全角字母不应计为标点");
        assert_eq!(ot, 6, "全角字母应计为其他");
        assert_eq!(zh, 0);
        assert_eq!(en, 0);
    }

    #[test]
    fn test_daily_stats_backward_compat() {
        // 旧库只含 {chinese, english}，新字段应回落默认值。
        let rec: DailyStats = serde_json::from_str(r#"{"chinese":10,"english":2}"#).unwrap();
        assert_eq!(rec.chinese, 10);
        assert_eq!(rec.english, 2);
        assert_eq!(rec.total(), 12);
        assert_eq!(rec.hours, [0u32; 24]);
        assert_eq!(rec.active_seconds, 0);
        assert!(rec.by_schema.is_empty());
    }

    #[test]
    fn test_put_get_daily_full() {
        let path = tmp("wind_stats_put_full.redb");
        let s = Store::open(&path).unwrap();
        let mut stat = DailyStats {
            chinese: 50,
            english: 10,
            punct: 5,
            other: 3,
            commit_count: 20,
            code_len_sum: 80,
            code_len_count: 20,
            active_seconds: 300,
            ..Default::default()
        };
        stat.hours[9] = 30;
        stat.code_len_dist[3] = 12;
        stat.cand_pos_dist[0] = 18;
        stat.by_source = vec![0; CommitSource::COUNT];
        stat.by_source[CommitSource::Candidate.index()] = 60;
        stat.by_schema.insert(
            "wubi86".into(),
            SchemaStats {
                total_chars: 60,
                commit_count: 20,
                ..Default::default()
            },
        );

        s.put_daily_stat("2026-06-20", &stat).unwrap();
        let got = s.get_daily_stat("2026-06-20").unwrap();
        assert_eq!(got, stat, "完整字段往返一致");
        assert_eq!(got.total(), 68);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_meta_put_get() {
        let path = tmp("wind_stats_meta.redb");
        let s = Store::open(&path).unwrap();
        // 默认空
        assert_eq!(s.get_stats_meta().unwrap(), StatsMeta::default());
        let meta = StatsMeta {
            total_chars: 12345,
            first_day: "2026-01-01".into(),
            streak_current: 7,
            streak_max: 30,
            streak_last_day: "2026-06-20".into(),
            max_speed: 420,
        };
        s.put_stats_meta(&meta).unwrap();
        assert_eq!(s.get_stats_meta().unwrap(), meta);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_clear_resets_meta() {
        let path = tmp("wind_stats_clear_meta.redb");
        let s = Store::open(&path).unwrap();
        s.record_stat("2026-06-10", 1, 0).unwrap();
        s.record_stat("2026-06-20", 1, 0).unwrap();
        s.put_stats_meta(&StatsMeta {
            total_chars: 2,
            ..Default::default()
        })
        .unwrap();

        assert_eq!(s.clear_stats().unwrap(), 2);
        assert_eq!(s.daily_stats("2026-01-01", "2026-12-31").unwrap().len(), 0);
        assert_eq!(
            s.get_stats_meta().unwrap(),
            StatsMeta::default(),
            "clear 应同时重置元数据"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_recalculate_stats_meta_after_prune() {
        let path = tmp("wind_stats_recalc.redb");
        let s = Store::open(&path).unwrap();
        for (date, total, active) in [
            ("2026-04-20", 100u32, 60u32),
            ("2026-04-21", 200, 120),
            ("2026-04-23", 300, 60),
        ] {
            let stat = DailyStats {
                chinese: total,
                active_seconds: active,
                ..Default::default()
            };
            s.put_daily_stat(date, &stat).unwrap();
        }

        assert_eq!(s.prune_stats_before("2026-04-21").unwrap(), 1);
        let meta = s.recalculate_stats_meta(1.0).unwrap();
        assert_eq!(meta.total_chars, 500);
        assert_eq!(meta.first_day, "2026-04-21");
        assert_eq!(meta.streak_current, 1);
        assert_eq!(meta.streak_max, 1);
        assert_eq!(meta.streak_last_day, "2026-04-23");
        assert_eq!(meta.max_speed, 300, "300字/60s = 300字/分");
        let _ = std::fs::remove_file(&path);
    }

    /// 重算时同样要挡掉小样本，且它是**修正历史污染值的入口**：
    /// 之前写进去的虚高 max_speed，重算一次即可回落到真实值。
    #[test]
    fn recalculate_ignores_small_samples_for_max_speed() {
        let path = std::env::temp_dir().join("wind_recalc_speed_test.redb");
        let _ = std::fs::remove_file(&path);
        let s = Store::open(&path).unwrap();

        // 一天正常（240字/120秒 = 120字/分），一天是"开头窗口"式的小样本
        // （125字/5秒，外推 1500 字/分）。
        for (date, total, active) in [("2026-04-21", 240u32, 120u32), ("2026-04-22", 125, 5)] {
            let stat = DailyStats {
                chinese: total,
                active_seconds: active,
                ..Default::default()
            };
            s.put_daily_stat(date, &stat).unwrap();
        }
        // 先人为写入被污染的历史值，模拟旧版本留下的数据。
        s.put_stats_meta(&StatsMeta {
            max_speed: 1500,
            ..Default::default()
        })
        .unwrap();

        let meta = s.recalculate_stats_meta(1.0).unwrap();
        assert_eq!(
            meta.max_speed, 120,
            "小样本那天应被忽略，且旧的污染值 1500 被重算覆盖"
        );
        let _ = std::fs::remove_file(&path);
    }
}
