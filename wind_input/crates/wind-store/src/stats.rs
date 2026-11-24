//! 输入统计存储（redb，按日期键）
//!
//! 与 Go 版本 `wind_input/internal/store/stats.go` 完整对齐：每日聚合 `DailyStats`
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
//! 速度 = `speed_chars × 60000 / active_millis × speed_factor`，四个量都与实际字数统计分开。
//! 整秒截断、段首口径不对称、一次上屏多字三条偏差在采集期结构性修掉，只有打错字无从观测
//! 才交给系数；别指望系数替代前三条。旧口径的偏差分析见
//! `docs/architecture/input-stats-speed-model.md`。

use crate::store::{META, STATS_DAILY, Store};
use chrono::{Duration, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 旧 META 表中存放统计全局元数据的键（P4a 起只供迁移读）。
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
    Email = 12,      // 邮箱模式（Rust 特有）
}

impl CommitSource {
    /// 来源总数（by_source 数组长度）。
    ///
    /// 加变体即改本值。存量记录的 `by_source` 只有旧长度，`stat_collector::Shared::record`
    /// 读回后先 `resize` 到本值再索引，那道 resize 别优化掉。
    pub const COUNT: usize = 13;
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
    // 速度分子：与 `total()` 刻意分开。它扣掉封顶后多出的字符（`speed_chars_of`）与段首
    // 无法计时的字符，量纲不同，不能共用累加器。
    #[serde(default)]
    pub speed_chars: u32,
    // 速度分母（毫秒）。`active_seconds` 整秒截断会单向砍小分母，故另存毫秒。
    #[serde(default)]
    pub active_millis: u64,
    // 本记录是否由 v2 速度模型写过。旧库记录没有上面两个量，读回时要回退到旧口径；
    // 不能用「speed_chars == 0」判断：新模型下它可以合法为 0（当天全是段首上屏）。
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
    /// v2 之前的记录只能按旧口径（全部字符 / 整秒活跃时间）回退，系统性偏高；
    /// 历史曲线因此有一道无法回补的台阶。
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

/// 单次上屏计入速度分子的字符数（实际字数统计不走这里）。
///
/// 一次上屏只占一个时间间隔，短码出长词、快捷指令出一串会把速度顶到荒谬值，故击键数
/// ≤ [`SPEED_SHORT_KEYSTROKES`] 时按 [`SPEED_SHORT_CHAR_CAP`] 封顶。
///
/// `keystrokes == 0` 表示未知，不封顶：TSF 英文批量上报一次就是几十个 1:1 击键的字符，
/// 误封会把英文速度砍到个位数。
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
/// `factor` 是打错字重打的经验修正（见 `StatsConfig::speed_factor`），出厂取 < 1；
/// 非有限值或 ≤ 0 当 1.0 处理，配置写坏不该把速度清零。
///
/// 分母小时结果是外推值，不可直接拿去刷新历史最快，见 [`qualifies_for_max_speed`]。
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
/// 段首上屏不计时（见 `StatCollector::record`），每天开头会有「字数已多、活跃时间才几秒」
/// 的窗口，外推出上千字/分。`max_speed` 是永久记录，被污染就降不回来，故要求样本量足够。
pub fn qualifies_for_max_speed(chars: u32, active_millis: u64) -> bool {
    active_millis >= MIN_SPEED_SAMPLE_MS && chars >= MIN_SPEED_SAMPLE_CHARS
}

/// 一个月的文件：`{version: 1, days: {YYYY-MM-DD: DailyStats}}`。
#[derive(Serialize, Deserialize, Default)]
struct MonthFile {
    version: u32,
    #[serde(default)]
    days: std::collections::BTreeMap<String, DailyStats>,
}

/// 旧表里的每日统计与元数据（迁移用）。
pub(crate) type LegacyStats = (Vec<(String, DailyStats)>, Option<StatsMeta>);

/// `YYYY-MM-DD` 的月份部分；格式不对（含非 ASCII 数字）⇒ `None`。先验形态再按字节切：
/// 直接 `&date[..7]` 遇全角数字会切在字符中间 panic，带走整个服务。
fn month_of(date: &str) -> Option<&str> {
    let b = date.as_bytes();
    let ok = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    ok.then(|| &date[..7])
}

impl Store {
    /// 每日统计目录：`<data>/stats/`（P5 起随用户目录走；按月一个 `YYYY-MM.toml`）。
    pub fn stats_dir(&self) -> std::path::PathBuf {
        self.user_text_dir().join("stats")
    }

    fn stats_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.stats_io.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn month_path(&self, ym: &str) -> std::path::PathBuf {
        self.stats_dir().join(format!("{ym}.toml"))
    }

    /// 读一个月：机写文件，坏了从 `.bak` 自愈或另存后从空开始（§2.5）；读不了（IO）⇒ Err。
    fn read_month(
        &self,
        ym: &str,
    ) -> anyhow::Result<std::collections::BTreeMap<String, DailyStats>> {
        let parse = |b: &[u8]| -> anyhow::Result<MonthFile> {
            Ok(toml::from_str(std::str::from_utf8(b)?)?)
        };
        Ok(
            crate::machine_file::load_machine_file(&self.month_path(ym), &parse)?
                .map(|f| f.days)
                .unwrap_or_default(),
        )
    }

    fn write_month(
        &self,
        ym: &str,
        days: std::collections::BTreeMap<String, DailyStats>,
    ) -> anyhow::Result<()> {
        let p = self.month_path(ym);
        if days.is_empty() {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            return Ok(());
        }
        let text = format!(
            "{}\n{}",
            wind_userdata::text::banner::STATS,
            toml::to_string(&MonthFile { version: 1, days })?
        );
        wind_userdata::text::fold::commit_text(&p, text.as_bytes(), None)
    }

    /// 现存的月份（`YYYY-MM`，升序）。
    fn stat_months(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.stats_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                let ym = n.strip_suffix(".toml")?;
                (ym.len() == 7 && ym.as_bytes()[4] == b'-').then(|| ym.to_string())
            })
            .collect();
        v.sort();
        v
    }

    /// 累加某日的中文/英文上屏字符数（读改写）。Stage 1 保留，供现有采集单点使用。
    pub fn record_stat(&self, date: &str, chinese: u32, english: u32) -> anyhow::Result<()> {
        if chinese == 0 && english == 0 {
            return Ok(());
        }
        let Some(ym) = month_of(date) else {
            anyhow::bail!("日期格式不对：{date}");
        };
        let _g = self.stats_lock();
        let mut days = self.read_month(ym)?;
        let rec = days.entry(date.to_string()).or_default();
        rec.chinese = rec.chinese.saturating_add(chinese);
        rec.english = rec.english.saturating_add(english);
        self.write_month(ym, days)
    }

    /// 覆盖写入某日完整统计（采集器 flush 用，30 秒一次，不在按键路径）。
    pub fn put_daily_stat(&self, date: &str, stat: &DailyStats) -> anyhow::Result<()> {
        let Some(ym) = month_of(date) else {
            anyhow::bail!("日期格式不对：{date}");
        };
        let _g = self.stats_lock();
        let mut days = self.read_month(ym)?;
        days.insert(date.to_string(), stat.clone());
        self.write_month(ym, days)
    }

    /// 取某日统计（无则零值）。
    pub fn get_daily_stat(&self, date: &str) -> anyhow::Result<DailyStats> {
        let Some(ym) = month_of(date) else {
            return Ok(DailyStats::default());
        };
        let _g = self.stats_lock();
        Ok(self.read_month(ym)?.remove(date).unwrap_or_default())
    }

    /// 取日期区间 [from, to]（含端点；YYYY-MM-DD 字典序即时间序），只读涉及的月份文件。
    pub fn daily_stats(&self, from: &str, to: &str) -> anyhow::Result<Vec<(String, DailyStats)>> {
        let (Some(fm), Some(tm)) = (month_of(from), month_of(to)) else {
            return Ok(Vec::new());
        };
        let _g = self.stats_lock();
        let mut out = Vec::new();
        for ym in self.stat_months() {
            if ym.as_str() < fm || ym.as_str() > tm {
                continue;
            }
            // 一个月读不了（IO）不拖垮整个区间：跳过它，其余照常
            let days = match self.read_month(&ym) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("统计 {ym} 读不了，跳过：{e:#}");
                    continue;
                }
            };
            for (d, st) in days {
                if d.as_str() >= from && d.as_str() <= to {
                    out.push((d, st));
                }
            }
        }
        Ok(out)
    }

    /// 全部每日统计（升序）。
    fn all_daily_stats(&self) -> anyhow::Result<Vec<(String, DailyStats)>> {
        self.daily_stats("0000-01-01", "9999-12-31")
    }

    /// 导出全部每日统计为 jsonl（每行 {"date","stat"}）。
    pub fn export_stats_jsonl(&self) -> anyhow::Result<String> {
        let all = self.all_daily_stats()?;
        let mut out = String::new();
        for (date, stat) in all {
            out.push_str(&serde_json::to_string(
                &serde_json::json!({ "date": date, "stat": stat }),
            )?);
            out.push('\n');
        }
        Ok(out)
    }

    /// 从 jsonl 导入每日统计。overwrite=false 时已存在日跳过（以本地为准）。每个月份只写一次。
    /// 返回 (imported, skipped_bad_lines)。
    pub fn import_stats_jsonl(
        &self,
        text: &str,
        overwrite: bool,
    ) -> anyhow::Result<(usize, usize)> {
        let normalized = wind_utils::text::normalize_input(text);
        let text = normalized.as_ref();
        let mut by_month: std::collections::BTreeMap<String, Vec<(String, DailyStats)>> =
            Default::default();
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
            let Some(ym) = month_of(date) else {
                skipped += 1;
                continue;
            };
            by_month
                .entry(ym.to_string())
                .or_default()
                .push((date.to_string(), stat));
        }
        let _g = self.stats_lock();
        let mut imported = 0usize;
        for (ym, rows) in by_month {
            let mut days = self.read_month(&ym)?;
            for (date, stat) in rows {
                // 非覆盖：已存在的日以本地为准；批内重复日期「先到者胜」
                if !overwrite && days.contains_key(&date) {
                    continue;
                }
                days.insert(date, stat);
                imported += 1;
            }
            self.write_month(&ym, days)?;
        }
        Ok((imported, skipped))
    }

    pub(crate) fn stats_meta_path(&self) -> std::path::PathBuf {
        self.stats_dir().join("meta.toml")
    }

    /// 元数据：没有 ⇒ None；坏了从 `.bak` 自愈，`.bak` 也不行 ⇒ 另存后 None（§2.5）。
    fn load_stats_meta(&self) -> anyhow::Result<Option<StatsMeta>> {
        let parse = |b: &[u8]| -> anyhow::Result<StatsMeta> {
            Ok(toml::from_str(std::str::from_utf8(b)?)?)
        };
        crate::machine_file::load_machine_file(&self.stats_meta_path(), &parse)
    }

    /// 读取统计全局元数据（无则零值）。
    pub fn get_stats_meta(&self) -> anyhow::Result<StatsMeta> {
        Ok(self.load_stats_meta()?.unwrap_or_default())
    }

    /// 读取统计全局元数据；文件没有（被删、坏了且备份不可用）而月数据还在 ⇒ 由月数据重算并写回。
    /// 收集器用它起步：把零值当真再累加写回会永久抹掉累计值（M-3）。
    pub fn stats_meta_or_recalc(&self, speed_factor: f32) -> anyhow::Result<StatsMeta> {
        match self.load_stats_meta()? {
            Some(m) => Ok(m),
            None if !self.stat_months().is_empty() => {
                tracing::warn!("统计元数据缺失，由每日数据重算");
                self.recalculate_stats_meta(speed_factor)
            }
            None => Ok(StatsMeta::default()),
        }
    }

    /// 写入统计全局元数据。
    pub fn put_stats_meta(&self, meta: &StatsMeta) -> anyhow::Result<()> {
        let text = format!(
            "{}\n{}",
            wind_userdata::text::banner::STATS,
            toml::to_string(meta)?
        );
        wind_userdata::text::fold::commit_text(&self.stats_meta_path(), text.as_bytes(), None)
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
        let _g = self.stats_lock();
        let mut n = 0usize;
        for ym in self.stat_months() {
            n += self.read_month(&ym).map(|d| d.len()).unwrap_or(0);
            self.write_month(&ym, Default::default())?;
        }
        match std::fs::remove_file(self.stats_meta_path()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(n)
    }

    /// 删除 `before`（不含）之前的统计，返回删除条数。整月在前的直接删文件。
    pub fn prune_stats_before(&self, before: &str) -> anyhow::Result<usize> {
        let Some(bm) = month_of(before) else {
            return Ok(0);
        };
        let _g = self.stats_lock();
        let mut n = 0usize;
        for ym in self.stat_months() {
            if ym.as_str() > bm {
                break;
            }
            let mut days = self.read_month(&ym)?;
            let before_len = days.len();
            days.retain(|d, _| d.as_str() >= before);
            n += before_len - days.len();
            if days.len() != before_len {
                self.write_month(&ym, days)?;
            }
        }
        Ok(n)
    }

    /// 旧 `STATS_DAILY` 表全部行与旧 META 里的 `stats_meta`（P4a 迁移用）。
    pub(crate) fn legacy_stats(&self) -> anyhow::Result<LegacyStats> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(STATS_DAILY)?;
            let mut days = Vec::new();
            for item in t.range::<&str>(..)? {
                let (k, v) = item?;
                if let Ok(rec) = serde_json::from_slice::<DailyStats>(v.value()) {
                    days.push((k.value().to_string(), rec));
                }
            }
            let m = txn.open_table(META)?;
            let meta = m
                .get(STATS_META_KEY)?
                .and_then(|g| serde_json::from_slice(g.value()).ok());
            Ok((days, meta))
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
#[path = "stats_tests.rs"]
mod tests;
