//! 农历（夏历）换算：公历日期 → 农历年月日 + 干支 + 生肖 + 传统节日。
//!
//! 用的是清代以来行用的时宪历规则，三条：
//!
//! 1. 朔日为月首（定朔，非平朔）；
//! 2. 冬至所在的朔望月恒为十一月；
//! 3. 两个十一月之间若有 13 个月（即含 13 个朔望月），则其中第一个不含中气的月为闰月。
//!
//! 天文量（朔时刻、太阳黄经）见 [`astro`]（为什么不查表也见那里），本模块只负责组织成历法。
//! 快捷输入（绑用户打的日期）与短语（绑当前时间）的 `$L*` 变量共用这一份实现。

pub mod astro;

use astro::*;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// 支持范围（公历年，闭区间）。
///
/// 下限取 1900 是惯例；上限 2100 受 ΔT 外推精度限制——再往后朔时刻的误差
/// 会开始威胁「朔落在午夜附近」的判定。超出范围一律返回 `None`，
/// 由调用方渲染成空串并丢弃该条候选，不 panic、不给错值。
pub const MIN_YEAR: i32 = 1900;
pub const MAX_YEAR: i32 = 2100;

const MONTH_NAMES: [&str; 12] = [
    "正", "二", "三", "四", "五", "六", "七", "八", "九", "十", "冬", "腊",
];

/// 日名。初十/二十/三十不写「廿」，21–29 才用「廿」。
const DAY_NAMES: [&str; 30] = [
    "初一", "初二", "初三", "初四", "初五", "初六", "初七", "初八", "初九", "初十", "十一", "十二",
    "十三", "十四", "十五", "十六", "十七", "十八", "十九", "二十", "廿一", "廿二", "廿三", "廿四",
    "廿五", "廿六", "廿七", "廿八", "廿九", "三十",
];

const GAN: [&str; 10] = ["甲", "乙", "丙", "丁", "戊", "己", "庚", "辛", "壬", "癸"];
const ZHI: [&str; 12] = [
    "子", "丑", "寅", "卯", "辰", "巳", "午", "未", "申", "酉", "戌", "亥",
];
const ANIMALS: [&str; 12] = [
    "鼠", "牛", "虎", "兔", "龙", "蛇", "马", "羊", "猴", "鸡", "狗", "猪",
];

/// 按农历月日固定的传统节日。**闰月不过节**（闰五月初五不是端午）。
///
/// 除夕不在表里：它是「腊月最后一天」，而腊月是大月还是小月逐年不同
/// （2024 年是腊月三十，2025–2028 连续四年都是廿九），只能由「次日为正月初一」判定。
const FESTIVALS: &[((u32, u32), &str)] = &[
    ((1, 1), "春节"),
    ((1, 15), "元宵节"),
    ((2, 2), "龙抬头"),
    ((5, 5), "端午节"),
    ((7, 7), "七夕"),
    ((7, 15), "中元节"),
    ((8, 15), "中秋节"),
    ((9, 9), "重阳节"),
    ((12, 8), "腊八节"),
    ((12, 23), "小年"),
];

/// 一个农历日期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LunarDate {
    /// 农历年。**以正月初一为界**，不是公历年——公历 2026-01-01 属农历 2025 年（乙巳）。
    /// 干支与生肖都据此推，用公历年推是农历实现最常见的错。
    pub year: i32,
    /// 月，1–12（`leap` 为真时表示闰该月）。
    pub month: u32,
    /// 日，1–30。
    pub day: u32,
    /// 是否闰月。
    pub leap: bool,
    /// 传统节日；非节日为 `None`。
    pub festival: Option<&'static str>,
}

impl LunarDate {
    /// 月名，如「正」「腊」。
    pub fn month_name(&self) -> &'static str {
        MONTH_NAMES[(self.month - 1) as usize]
    }

    /// 月的完整写法，如「四月」「闰四月」。
    pub fn month_text(&self) -> String {
        format!(
            "{}{}月",
            if self.leap { "闰" } else { "" },
            self.month_name()
        )
    }

    /// 日的写法，如「初三」「廿九」。
    pub fn day_text(&self) -> &'static str {
        DAY_NAMES[(self.day - 1) as usize]
    }

    /// 月日连写，如「四月廿九」「闰四月初十」。
    pub fn date_text(&self) -> String {
        format!("{}{}", self.month_text(), self.day_text())
    }

    /// 干支年，如「丙午」。
    pub fn ganzhi(&self) -> String {
        // 1984 年为甲子年（干支纪年的现代基准）
        let idx = (self.year - 1984).rem_euclid(60) as usize;
        format!("{}{}", GAN[idx % 10], ZHI[idx % 12])
    }

    /// 生肖，如「马」。
    pub fn animal(&self) -> &'static str {
        ANIMALS[(self.year - 1984).rem_euclid(12) as usize]
    }

    /// 干支年 + 月日，如「丙午年四月廿九」。
    pub fn full_text(&self) -> String {
        format!("{}年{}", self.ganzhi(), self.date_text())
    }
}

/// 是否为农历变量名（`$LY` `$LYN` `$LZ` `$LM` `$LD` `$LMD` `$LF`）。
///
/// 与 [`var`] 的分支**必须一致**：这里认了而那边不认，该变量会静默变成
/// 「本次取不到值」，症状与超出范围无从区分。由 `vars_and_names_agree` 兜底。
pub fn is_var(name: &str) -> bool {
    matches!(name, "LY" | "LYN" | "LZ" | "LM" | "LD" | "LMD" | "LF")
}

/// 按变量名取值。
///
/// - `None` = 不认识这个变量（如 `$NOPE`）→ 调用方让整条模板作废（配置写错了）。
/// - `Some("")` = 认识但本次没有值（`$LF` 在非节日）→ 渲染成空串，整条照常出，
///   使 `$LY年$LMD$LF` 这类追加式写法平日也能用。约定同 [`crate::vars::number_var`]。
///
/// 「算不出来」（日期超范围或非法）在更外层由 [`solar_to_lunar`] 返回 `None` 作废整条。
pub fn var(name: &str, l: &LunarDate) -> Option<String> {
    Some(match name {
        "LY" => l.ganzhi(),
        // 农历年的数字写法。与公历年可能差 1——2026-01-01 的 $LYN 是 2025，
        // 因为农历年以正月初一为界。想要公历年用 $Y。
        "LYN" => l.year.to_string(),
        "LZ" => l.animal().to_string(),
        "LM" => l.month_text(),
        "LD" => l.day_text().to_string(),
        "LMD" => l.date_text(),
        // 非节日 → 空串（不是 None）。见上文「`None` 与空串的分工」。
        "LF" => l.festival.unwrap_or("").to_string(),
        _ => return None,
    })
}

/// 一个「冬至到冬至」的月序列。
#[derive(Debug, Clone)]
struct Cycle {
    /// (月首 JD, 月号 1–12, 是否闰)
    months: Vec<(i64, u32, bool)>,
    /// 序列末月的次月朔日（区间右开端点）
    end_jd: i64,
    /// 本序列中正月初一的 JD（农历年归属的分界）
    zheng_jd: i64,
}

/// 按锚年缓存。每次按键都要重算候选，而一次 `build_cycle` 要算十几次朔与太阳黄经；
/// 用户输入的日期又高度集中在少数几年，缓存命中率接近 1。
static CACHE: LazyLock<RwLock<HashMap<i32, Cycle>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// 构造锚年 `anchor` 的月序列：从 `anchor-1` 年冬至所在月（十一月）起，
/// 到 `anchor` 年冬至所在月（下一个十一月）止。
fn build_cycle(anchor: i32) -> Cycle {
    let ws_prev = winter_solstice_jd(anchor - 1);
    let ws_cur = winter_solstice_jd(anchor);

    // 定位包含某个 JD 的朔日：从估值出发向两侧收敛。
    // k_near 可能差一个月（朔恰在月初/月末时），故两个方向都要校正。
    let month_start_of = |jd: i64| -> f64 {
        let mut k = k_near(jd);
        while jde_to_china_jd(new_moon_jde(k)) > jd {
            k -= 1.0;
        }
        while jde_to_china_jd(new_moon_jde(k + 1.0)) <= jd {
            k += 1.0;
        }
        k
    };

    let k0 = month_start_of(ws_prev);
    let k1 = month_start_of(ws_cur);
    let n_months = (k1 - k0) as i64;
    // 13 个月 → 需置闰。12 个月为平年。
    let is_leap_year = n_months == 13;

    let mut months = Vec::with_capacity(n_months as usize + 1);
    let mut leap_done = false;
    let mut label: u32 = 11; // 冬至所在月恒为十一月

    for i in 0..=n_months {
        let start = jde_to_china_jd(new_moon_jde(k0 + i as f64));
        let next = jde_to_china_jd(new_moon_jde(k0 + i as f64 + 1.0));
        // 含中气 ⟺ 月首与月末的 30° 区间号不同（朔望月至多跨一个中气）
        let has_term = major_term_index(start) != major_term_index(next - 1);
        let mut leap = false;
        if is_leap_year && !leap_done && !has_term && i > 0 {
            // 闰月沿用上一个月的月号，故这一轮不递增 label
            leap = true;
            leap_done = true;
        } else if i > 0 {
            label = label % 12 + 1;
        }
        months.push((start, label, leap));
    }

    let end_jd = jde_to_china_jd(new_moon_jde(k0 + n_months as f64 + 1.0));
    let zheng_jd = months
        .iter()
        .find(|(_, l, lp)| *l == 1 && !*lp)
        .map(|(s, _, _)| *s)
        .unwrap_or(i64::MAX);

    Cycle {
        months,
        end_jd,
        zheng_jd,
    }
}

/// 取（或构造并缓存）锚年的月序列，在锁内执行 `f` 以免 clone 整个序列。
fn with_cycle<T>(anchor: i32, f: impl FnOnce(&Cycle) -> T) -> T {
    if let Ok(c) = CACHE.read()
        && let Some(cy) = c.get(&anchor)
    {
        return f(cy);
    }
    let cy = build_cycle(anchor);
    let out = f(&cy);
    if let Ok(mut c) = CACHE.write() {
        c.insert(anchor, cy);
    }
    out
}

/// 公历 → 农历。超出 [`MIN_YEAR`]–[`MAX_YEAR`] 或公历日期本身非法（如 2 月 31 日）
/// 返回 `None`。
///
/// 非法日期必须挡在这里：`jd_from_ymd` 对 `2026-02-31` 会照算不误（得到 3 月 3 日），
/// 于是农历候选会给出一个「看起来对」的错值。
pub fn solar_to_lunar(y: i32, m: u32, d: u32) -> Option<LunarDate> {
    if !(MIN_YEAR..=MAX_YEAR).contains(&y) {
        return None;
    }
    // 公历日期合法性（闰年、月大小）交给 chrono，本模块不重复实现一份
    chrono::NaiveDate::from_ymd_opt(y, m, d)?;

    let jd = jd_from_ymd(y, m, d);
    // 一个 jd 必落在 cycle(y) 或 cycle(y+1) 之一：cycle(y) 覆盖到 y 年冬至月，
    // 其后的腊月要到 cycle(y+1) 才出现。
    for anchor in [y, y + 1] {
        let hit = with_cycle(anchor, |cy| {
            if jd < cy.months[0].0 || jd >= cy.end_jd {
                return None;
            }
            let idx = cy.months.iter().rposition(|(s, _, _)| *s <= jd)?;
            let (start, label, leap) = cy.months[idx];
            let day = (jd - start + 1) as u32;
            // 农历年以正月初一为界：正月前的腊月/冬月属上一农历年
            let lunar_year = if jd < cy.zheng_jd { anchor - 1 } else { anchor };
            let is_eve = jd + 1 == cy.zheng_jd;
            Some((lunar_year, label, day, leap, is_eve))
        });
        if let Some((year, month, day, leap, is_eve)) = hit {
            let festival = if is_eve {
                Some("除夕")
            } else if leap {
                // 闰月不过节
                None
            } else {
                FESTIVALS
                    .iter()
                    .find(|((fm, fd), _)| *fm == month && *fd == day)
                    .map(|(_, name)| *name)
            };
            return Some(LunarDate {
                year,
                month,
                day,
                leap,
                festival,
            });
        }
    }
    None
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
