//! 统计报表：概览汇总与按日区间。桌面设置页（`wind-webdata` 的 `stats.*`）与移动端
//! （`wind-mobile`）共用这一份算法——两端各算一套，「今日速度」「日均」迟早对不上。
//!
//! 今天的数据一律取采集器内存（始终最新、完整），历史从 store 读：今天的那一行可能还没 flush。

use chrono::Datelike;

use crate::stat_collector::StatCollector;
use crate::stats::{
    DailyStats, StatsMeta, calculate_streaks, qualifies_for_max_speed, speed_per_minute_ms,
};
use crate::store::Store;

/// 概览汇总。字段与桌面 `stats.summary` 的 JSON 一一对应。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatsSummary {
    pub today_chars: u32,
    pub today_chinese: u32,
    pub today_english: u32,
    pub total_chars: u64,
    pub active_days: u64,
    pub daily_avg: u64,
    pub streak_current: u32,
    pub streak_max: u32,
    pub week_chars: u64,
    pub month_chars: u64,
    pub max_day_chars: u32,
    pub max_day_date: String,
    pub avg_code_len: f64,
    pub first_select_rate: f64,
    pub today_speed: u32,
    pub overall_speed: u32,
    pub max_speed: u32,
}

/// 今天的日期键（`YYYY-MM-DD`，本地时区，与 `daily_stats` 表的键同形）。
pub fn today_str() -> String {
    chrono::Local::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string()
}

/// 统计口径：全部设备合计 / 仅本机 / 某台他机。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatsScope {
    All,
    SelfOnly,
    Device(String),
}

impl StatsScope {
    /// 设置页的设备参数：`all` = 全部设备，`self` = 本机，其余须是 [`Store::list_device_stats`] 里的设备号。
    pub fn parse(store: Option<&Store>, device: &str) -> Result<StatsScope, String> {
        Ok(match device {
            "all" => StatsScope::All,
            "self" => StatsScope::SelfOnly,
            d => {
                let known = store.is_some_and(|s| s.list_device_stats().iter().any(|x| x == d));
                if !known {
                    return Err(format!("未知设备：{d}"));
                }
                StatsScope::Device(d.to_string())
            }
        })
    }
}

type Days = Vec<(String, DailyStats)>;

/// 按口径取 [from, to] 的按天数据（不含采集器内存）。`All` 把各设备同日记录并入本机。
fn scoped_days(store: &Store, scope: &StatsScope, from: &str, to: &str) -> Days {
    match scope {
        StatsScope::SelfOnly => store.daily_stats(from, to).unwrap_or_default(),
        StatsScope::Device(d) => store.device_daily_stats(d, from, to),
        StatsScope::All => {
            let mut days = store.daily_stats(from, to).unwrap_or_default();
            for dev in store.list_device_stats() {
                days = merge_days(days, store.device_daily_stats(&dev, from, to));
            }
            days
        }
    }
}

/// 按日期并集合并两份升序的按天数据，同日 `merge_from`。
fn merge_days(base: Days, extra: Days) -> Days {
    if extra.is_empty() {
        return base;
    }
    let mut m: std::collections::BTreeMap<String, DailyStats> = base.into_iter().collect();
    for (d, st) in extra {
        m.entry(d).or_default().merge_from(&st);
    }
    m.into_iter().collect()
}

/// 今天（采集器内存）按口径折算后的记录：`All` 并入各设备今天的行，`Device` 只取该设备的行。
fn scoped_today(
    store: &Store,
    collector: Option<&StatCollector>,
    scope: &StatsScope,
    today: &str,
) -> DailyStats {
    let mem = || collector.map(|c| c.get_today_stat()).unwrap_or_default();
    match scope {
        StatsScope::SelfOnly => mem(),
        StatsScope::Device(d) => store
            .device_daily_stats(d, today, today)
            .into_iter()
            .next()
            .map(|(_, r)| r)
            .unwrap_or_default(),
        StatsScope::All => {
            let mut t = mem();
            for dev in store.list_device_stats() {
                for (_, r) in store.device_daily_stats(&dev, today, today) {
                    t.merge_from(&r);
                }
            }
            t
        }
    }
}

/// 概览汇总（全部设备合计；没有他机统计时即本机）。
pub fn summary(store: &Store, collector: &StatCollector) -> StatsSummary {
    summary_for(store, collector, &StatsScope::All)
}

/// 全局元数据按口径折算。`SelfOnly` 与「没有他机统计的 `All`」原样用采集器的 meta；
/// 否则累计、连续天数、最高速度由按天数据重算。
fn scoped_meta(
    store: &Store,
    collector: &StatCollector,
    scope: &StatsScope,
    days: &Days,
    today: &str,
    today_stat: &DailyStats,
) -> StatsMeta {
    let local = collector.get_meta();
    let devices = match scope {
        StatsScope::SelfOnly => return local,
        StatsScope::All => {
            let d = store.list_device_stats();
            if d.is_empty() {
                return local;
            }
            d
        }
        StatsScope::Device(d) => vec![d.clone()],
    };
    let factor = collector.speed_factor();
    let mut meta = StatsMeta::default();
    // 日期并集（含今天内存值）重算连续天数。
    let mut dates: Vec<String> = days
        .iter()
        .filter(|(d, r)| r.total() > 0 && d.as_str() != today)
        .map(|(d, _)| d.clone())
        .collect();
    if today_stat.total() > 0 {
        dates.push(today.to_string());
    }
    dates.sort();
    dates.dedup();
    let (cur, mx, last) = calculate_streaks(&dates);
    meta.streak_current = cur;
    // All 不小于本机：本机 meta 可能含已清理的历史，按天数据推不回来。
    meta.streak_max = if *scope == StatsScope::All {
        mx.max(local.streak_max)
    } else {
        mx
    };
    meta.streak_last_day = last;
    let recomputed = dates.first().cloned().unwrap_or_default();
    meta.first_day = if *scope == StatsScope::All {
        [local.first_day.clone(), recomputed]
            .into_iter()
            .filter(|d| !d.is_empty())
            .min()
            .unwrap_or_default()
    } else {
        recomputed
    };
    // 最高速度：各设备取最大。他机只有按天数据，逐设备逐日按「够格」口径推；
    // 本机用落库的成品值（它可能来自已被清理的历史，按天数据推不回来）。
    // 合并后的同日记录是多机时长相加，不能当作单人速度，所以逐设备算再取最大。
    let mut max_speed = if *scope == StatsScope::All {
        local.max_speed
    } else {
        0
    };
    for dev in &devices {
        for (_, r) in store.device_daily_stats(dev, "0000-01-01", "9999-12-31") {
            let (c, ms) = r.speed_parts();
            if qualifies_for_max_speed(c, ms) {
                max_speed = max_speed.max(speed_per_minute_ms(c as u64, ms, factor));
            }
        }
    }
    meta.max_speed = max_speed;
    // 累计字数：All 是本机累计（含已清理的历史）加各设备按天合计；Device 只是该设备的合计。
    let dev_total: u64 = devices
        .iter()
        .flat_map(|d| store.device_daily_stats(d, "0000-01-01", "9999-12-31"))
        .map(|(_, r)| r.total() as u64)
        .sum();
    meta.total_chars = if *scope == StatsScope::All {
        local.total_chars + dev_total
    } else {
        dev_total
    };
    meta
}

/// 概览汇总：今日 / 累计 / 本周（周日起）/ 本月，近 90 天的最高日、平均码长、首选率与速度。
pub fn summary_for(store: &Store, collector: &StatCollector, scope: &StatsScope) -> StatsSummary {
    let today = today_str();
    let today_stat = scoped_today(store, Some(collector), scope, &today);
    let all = scoped_days(store, scope, "0000-01-01", "9999-12-31");
    let meta = scoped_meta(store, collector, scope, &all, &today, &today_stat);
    let today_total = today_stat.total();

    // 活跃天数（DB 天数；今天有数据但未 flush 时 +1）+ 日均。
    let mut active_days = all.iter().filter(|(_, r)| r.total() > 0).count() as u64;
    let today_in_db = all.iter().any(|(d, _)| d == &today);
    if today_total > 0 && !today_in_db {
        active_days += 1;
    }
    let daily_avg = meta.total_chars.checked_div(active_days).unwrap_or(0);

    // 周（周日起）/ 月统计（YYYY-MM-DD 字典序），今天用内存值。
    let now = chrono::Local::now().date_naive();
    let week_start = now - chrono::Duration::days(now.weekday().num_days_from_sunday() as i64);
    let month_start = now.with_day(1).unwrap_or(now);
    let sum_since = |from: chrono::NaiveDate| -> u64 {
        with_today_mem(
            store,
            scope,
            &from.format("%Y-%m-%d").to_string(),
            &today,
            &today_stat,
        )
        .iter()
        .map(|(_, r)| r.total() as u64)
        .sum()
    };
    let week_chars = sum_since(week_start);
    let month_chars = sum_since(month_start);

    // 近 90 天：最高日 / 平均码长 / 首选率 / 平均速度。
    let recent_from = (now - chrono::Duration::days(90))
        .format("%Y-%m-%d")
        .to_string();
    let recent = with_today_mem(store, scope, &recent_from, &today, &today_stat);
    let (mut max_day_chars, mut max_day_date) = (0u32, String::new());
    let (mut cl_sum, mut cl_cnt, mut first_sel, mut cand_sel) = (0u64, 0u64, 0u64, 0u64);
    let (mut sp_chars, mut sp_millis) = (0u64, 0u64);
    for (d, r) in &recent {
        let t = r.total();
        if t > max_day_chars {
            max_day_chars = t;
            max_day_date = d.clone();
        }
        cl_sum += r.code_len_sum as u64;
        cl_cnt += r.code_len_count as u64;
        first_sel += r.cand_pos_dist[0] as u64;
        cand_sel += r.cand_pos_dist.iter().map(|&v| v as u64).sum::<u64>();
        // 区间速度：两个分量分别累加后再除一次。逐日算速度再平均是错的——
        // 那会给只打了几十字的日子和整天码字的日子同样的权重。
        let (c, ms) = r.speed_parts();
        sp_chars += c as u64;
        sp_millis += ms;
    }
    let avg_code_len = if cl_cnt > 0 {
        cl_sum as f64 / cl_cnt as f64
    } else {
        0.0
    };
    let first_select_rate = if cand_sel > 0 {
        first_sel as f64 / cand_sel as f64
    } else {
        0.0
    };
    // 系数取自采集器而非配置：`max_speed` 是采集器在 flush 时算好落库的成品值，
    // 两处若各读各的，热改配置后「历史最快」与「今日速度」会差出一个恒定倍数。
    let factor = collector.speed_factor();
    let (today_sp_chars, today_sp_ms) = today_stat.speed_parts();

    StatsSummary {
        today_chars: today_total,
        today_chinese: today_stat.chinese,
        today_english: today_stat.english,
        total_chars: meta.total_chars,
        active_days,
        daily_avg,
        streak_current: meta.streak_current,
        streak_max: meta.streak_max,
        week_chars,
        month_chars,
        max_day_chars,
        max_day_date,
        avg_code_len,
        first_select_rate,
        today_speed: speed_per_minute_ms(today_sp_chars as u64, today_sp_ms, factor),
        overall_speed: speed_per_minute_ms(sp_chars, sp_millis, factor),
        max_speed: meta.max_speed,
    }
}

/// `[from, to]` 每一天的统计（含首尾），缺的日子补零值，今天用采集器内存覆盖。
/// 日期格式 `YYYY-MM-DD`，解析失败返回空表。
pub fn daily_range(
    store: &Store,
    collector: Option<&StatCollector>,
    from: &str,
    to: &str,
) -> anyhow::Result<Vec<(String, DailyStats)>> {
    daily_range_for(store, collector, &StatsScope::All, from, to)
}

/// [`daily_range`] 按口径取。`Device` 不用采集器内存。
pub fn daily_range_for(
    store: &Store,
    collector: Option<&StatCollector>,
    scope: &StatsScope,
    from: &str,
    to: &str,
) -> anyhow::Result<Vec<(String, DailyStats)>> {
    // 本机月表读不了时报错；他机的坏文件跳过。
    let mut by_date: std::collections::HashMap<String, DailyStats> = match scope {
        StatsScope::SelfOnly | StatsScope::All => {
            let mut days = store.daily_stats(from, to)?;
            if *scope == StatsScope::All {
                for dev in store.list_device_stats() {
                    days = merge_days(days, store.device_daily_stats(&dev, from, to));
                }
            }
            days.into_iter().collect()
        }
        StatsScope::Device(_) => scoped_days(store, scope, from, to).into_iter().collect(),
    };
    let today = today_str();
    if today.as_str() >= from && today.as_str() <= to {
        let ts = scoped_today(store, collector, scope, &today);
        if ts.total() > 0 && (collector.is_some() || matches!(scope, StatsScope::Device(_))) {
            by_date.insert(today, ts);
        }
    }
    let mut out = Vec::new();
    if let (Ok(f), Ok(t)) = (
        chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d"),
        chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d"),
    ) {
        let mut cur = f;
        while cur <= t {
            let key = cur.format("%Y-%m-%d").to_string();
            let rec = by_date.get(&key).cloned().unwrap_or_default();
            out.push((key, rec));
            cur += chrono::Duration::days(1);
        }
    }
    Ok(out)
}

/// 清空全部统计：store 里的历史 + 采集器内存（今日 + 元数据），否则汇总仍读到旧内存值。
pub fn clear(store: Option<&Store>, collector: Option<&StatCollector>) -> anyhow::Result<()> {
    if let Some(store) = store {
        store.clear_stats()?;
    }
    if let Some(c) = collector {
        c.reset();
    }
    Ok(())
}

/// 用给定月表整体替换本机统计（同步认领另一台设备的身份时）：挂起采集器（今日先落盘、之后不写），
/// 替换后由月表重算元数据，再让采集器重载今日与元数据——否则内存里的旧累计会被写回。
/// 输入不合法时报错，什么都不动。返回写入的月数。
pub fn replace_months(
    store: &Store,
    collector: Option<&StatCollector>,
    months: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<usize> {
    let parsed = crate::stats::parse_stat_months(months)?;
    if let Some(c) = collector {
        c.suspend();
    }
    let r = store.replace_stat_months(parsed);
    // 替换失败也重算：盘上可能已改了一部分
    let factor = collector.map_or(1.0, StatCollector::speed_factor);
    let meta = store.recalculate_stats_meta(factor);
    if let Some(c) = collector {
        c.resume();
    }
    let n = r?;
    meta?;
    Ok(n)
}

/// 取 [from, today] 的每日统计，今天用采集器内存值替换/追加（对齐 Go GetSummary 用内存今天）。
fn with_today_mem(
    store: &Store,
    scope: &StatsScope,
    from: &str,
    today: &str,
    today_stat: &DailyStats,
) -> Vec<(String, DailyStats)> {
    let mut days = scoped_days(store, scope, from, today);
    let mut has = false;
    for (d, r) in days.iter_mut() {
        if d == today {
            *r = today_stat.clone();
            has = true;
        }
    }
    if !has && today >= from && today_stat.total() > 0 {
        days.push((today.to_string(), today_stat.clone()));
    }
    days
}

#[cfg(test)]
#[path = "stats_report_tests.rs"]
mod tests;
