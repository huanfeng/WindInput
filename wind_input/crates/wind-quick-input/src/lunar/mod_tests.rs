use super::*;

fn lunar(y: i32, m: u32, d: u32) -> LunarDate {
    solar_to_lunar(y, m, d).unwrap_or_else(|| panic!("换算失败: {y}-{m}-{d}"))
}

/// 春节锚点，跨 1900–2028，与独立来源（历表）逐年核对过。朔时刻、ΔT、东八区日界、
/// 冬至定月序任何一处系数写错，这里都会大面积飘红。
#[test]
fn spring_festival_anchors() {
    let cases = [
        (1900, 1, 31),
        (1910, 2, 10),
        (1920, 2, 20),
        (1930, 1, 30),
        (1940, 2, 8),
        (1950, 2, 17),
        (1960, 1, 28),
        (1970, 2, 6),
        (1980, 2, 16),
        (1990, 1, 27),
        (2000, 2, 5),
        (2010, 2, 14),
        (2011, 2, 3),
        (2012, 1, 23),
        (2013, 2, 10),
        (2014, 1, 31),
        (2015, 2, 19),
        (2016, 2, 8),
        (2017, 1, 28),
        (2018, 2, 16),
        (2019, 2, 5),
        (2020, 1, 25),
        (2021, 2, 12),
        (2022, 2, 1),
        (2023, 1, 22),
        (2024, 2, 10),
        (2025, 1, 29),
        (2026, 2, 17),
        (2027, 2, 6),
        (2028, 1, 26),
    ];
    for (y, m, d) in cases {
        let l = lunar(y, m, d);
        assert_eq!(
            (l.year, l.month, l.day, l.leap),
            (y, 1, 1, false),
            "{y}-{m}-{d} 应为{y}年正月初一，实得 {}",
            l.date_text()
        );
        assert_eq!(l.festival, Some("春节"));
    }
}

/// 闰月分布。闰月是农历最易错的部分，且错了会连累其后整年的月号。
#[test]
fn leap_month_anchors() {
    // (公历年月日, 期望月号, 期望日)
    let cases = [
        (2020, 5, 23, 4, 1), // 庚子年闰四月初一
        (2020, 6, 1, 4, 10), // 闰四月初十
        (2023, 3, 22, 2, 1), // 癸卯年闰二月初一
        (2023, 3, 25, 2, 4), // 闰二月初四
        (2025, 7, 25, 6, 1), // 乙巳年闰六月初一
    ];
    for (y, m, d, lm, ld) in cases {
        let l = lunar(y, m, d);
        assert!(l.leap, "{y}-{m}-{d} 应在闰月，实得 {}", l.date_text());
        assert_eq!((l.month, l.day), (lm, ld), "{y}-{m}-{d}");
    }
}

/// 平年不得出现闰月——只测「闰年有闰月」会漏掉「平年多算一个闰月」这一半。
#[test]
fn common_years_have_no_leap_month() {
    for y in [2021, 2022, 2024, 2026, 2027] {
        let mut jd = jd_from_ymd(y, 1, 1);
        let end = jd_from_ymd(y, 12, 31);
        let mut found = None;
        while jd <= end {
            let (yy, mm, dd) = ymd_from_jd(jd);
            let l = lunar(yy, mm, dd);
            // 该农历年内不应有闰月（跨年的部分属别的农历年，跳过）
            if l.leap && l.year == y {
                found = Some(l);
                break;
            }
            jd += 1;
        }
        assert!(found.is_none(), "{y} 农历年不应有闰月，实得 {found:?}");
    }
}

/// 农历年归属以正月初一为界，不是公历年——干支/生肖都挂在它上面。
#[test]
fn lunar_year_boundary_is_spring_festival() {
    // 2026 春节为 2/17
    let before = lunar(2026, 2, 16);
    assert_eq!(before.year, 2025);
    assert_eq!(before.ganzhi(), "乙巳");
    assert_eq!(before.animal(), "蛇");

    let after = lunar(2026, 2, 17);
    assert_eq!(after.year, 2026);
    assert_eq!(after.ganzhi(), "丙午");
    assert_eq!(after.animal(), "马");

    // 元旦仍属上一农历年
    let newyear = lunar(2026, 1, 1);
    assert_eq!(newyear.year, 2025);
    assert_eq!(newyear.ganzhi(), "乙巳");
}

#[test]
fn ganzhi_and_animal_cycle() {
    assert_eq!(lunar(1984, 6, 1).ganzhi(), "甲子");
    assert_eq!(lunar(1984, 6, 1).animal(), "鼠");
    assert_eq!(lunar(2024, 6, 1).ganzhi(), "甲辰");
    assert_eq!(lunar(2024, 6, 1).animal(), "龙");
    assert_eq!(lunar(2025, 6, 1).ganzhi(), "乙巳");
    assert_eq!(lunar(2044, 6, 1).ganzhi(), "甲子", "60 年一轮回");
}

/// 除夕是腊月最后一天，而腊月大小逐年不同——写死「腊月三十」会连错四年。
#[test]
fn new_year_eve_tracks_month_length() {
    assert_eq!(lunar(2024, 2, 9).day, 30, "2024 除夕是腊月三十");
    assert_eq!(lunar(2024, 2, 9).festival, Some("除夕"));
    for (y, m, d) in [(2025, 1, 28), (2026, 2, 16), (2027, 2, 5), (2028, 1, 25)] {
        let l = lunar(y, m, d);
        assert_eq!(l.day, 29, "{y} 除夕应为腊月廿九");
        assert_eq!(l.festival, Some("除夕"), "{y}-{m}-{d}");
    }
}

#[test]
fn festivals() {
    assert_eq!(lunar(2025, 10, 6).festival, Some("中秋节"));
    assert_eq!(lunar(2025, 10, 6).date_text(), "八月十五");
    assert_eq!(lunar(2026, 6, 19).festival, Some("端午节"));
    assert_eq!(lunar(2026, 6, 19).date_text(), "五月初五");
    assert_eq!(lunar(2026, 6, 14).festival, None);
    assert_eq!(lunar(2026, 6, 14).date_text(), "四月廿九");
}

/// 闰月不过节：闰五月初五不是端午。
#[test]
fn leap_month_has_no_festival() {
    // 2025 闰六月初一为 7/25，闰六月里不该冒出节日
    let l = lunar(2025, 7, 25);
    assert!(l.leap);
    assert_eq!(l.festival, None);
}

/// 范围外与非法公历日期一律 None，不 panic 也不给错值。`2026-02-31` 若不挡，
/// 儒略日会照算成 3 月 3 日，给出一个看起来很对的错值。
#[test]
fn out_of_range_and_invalid_dates_are_none() {
    assert!(solar_to_lunar(1899, 12, 31).is_none());
    assert!(solar_to_lunar(2101, 1, 1).is_none());
    assert!(solar_to_lunar(2026, 2, 31).is_none(), "2 月没有 31 日");
    assert!(solar_to_lunar(2025, 2, 29).is_none(), "2025 不是闰年");
    assert!(solar_to_lunar(2026, 13, 1).is_none());
    assert!(solar_to_lunar(2026, 0, 1).is_none());
    assert!(solar_to_lunar(2026, 1, 0).is_none());
    // 闰年 2/29 合法
    assert!(solar_to_lunar(2024, 2, 29).is_some());
}

/// 两端边界要能算，不能因为 cycle 越界而失败。
#[test]
fn range_endpoints_work() {
    assert!(solar_to_lunar(MIN_YEAR, 1, 1).is_some());
    assert!(solar_to_lunar(MAX_YEAR, 12, 31).is_some());
}

/// 全范围连续性：每一天都必须能换算，且日号在 1..=30、月号在 1..=12。
/// 每 7 天抽样一次以控制时长；主要防 cycle 边界上某个月定位不到。
#[test]
fn every_day_in_range_converts() {
    let mut jd = jd_from_ymd(MIN_YEAR, 1, 1);
    let end = jd_from_ymd(MAX_YEAR, 12, 31);
    while jd <= end {
        let (y, m, d) = ymd_from_jd(jd);
        let l = solar_to_lunar(y, m, d)
            .unwrap_or_else(|| panic!("{y}-{m}-{d} 换算失败（月序列有缺口）"));
        assert!((1..=12).contains(&l.month), "{y}-{m}-{d} 月号 {}", l.month);
        assert!((1..=30).contains(&l.day), "{y}-{m}-{d} 日号 {}", l.day);
        jd += 7;
    }
}

/// 相邻两天的农历日必须连续（+1 或跨月归 1）——月首定位错位会在这里暴露。
#[test]
fn consecutive_days_are_continuous() {
    let mut jd = jd_from_ymd(2020, 1, 1);
    let end = jd_from_ymd(2030, 12, 31);
    let (y, m, d) = ymd_from_jd(jd);
    let mut prev = solar_to_lunar(y, m, d).unwrap();
    jd += 1;
    while jd <= end {
        let (y, m, d) = ymd_from_jd(jd);
        let cur = solar_to_lunar(y, m, d).unwrap();
        if cur.day != 1 {
            assert_eq!(
                cur.day,
                prev.day + 1,
                "{y}-{m}-{d} 日号不连续: {} → {}",
                prev.date_text(),
                cur.date_text()
            );
        } else {
            // 跨月：上一天必须是该月最后一天（29 或 30）
            assert!(
                prev.day == 29 || prev.day == 30,
                "{y}-{m}-{d} 跨月但上一天是 {}",
                prev.date_text()
            );
        }
        prev = cur;
        jd += 1;
    }
}

/// [`is_var`] 与 [`var`] 的分支必须一一对应：`is_var` 认而 `var` 不认时，
/// 该变量退化成「本次取不到值」，与超出范围的症状无法区分。
#[test]
fn vars_and_names_agree() {
    // 样本取平常日子：`$LF` 在非节日返回空串而非 None，故七个变量都该取到值；
    // 用节日当天会放过「$LF 在非节日退化成 None」。
    let l = lunar(2026, 6, 14);
    assert_eq!(l.festival, None, "样本必须是非节日，否则本测试压不住 $LF");
    for name in ["LY", "LYN", "LZ", "LM", "LD", "LMD", "LF"] {
        assert!(is_var(name), "is_var 应认识 ${name}");
        assert!(var(name, &l).is_some(), "var 应能取到 ${name}");
    }
    // 公历变量不属农历表
    for name in ["Y", "M", "D", "YC", "MC", "DC", "NOPE"] {
        assert!(!is_var(name), "is_var 不该认 ${name}");
        assert!(var(name, &l).is_none(), "var 不该取到 ${name}");
    }
}

#[test]
fn text_forms() {
    let l = lunar(2020, 6, 1);
    assert_eq!(l.month_text(), "闰四月");
    assert_eq!(l.day_text(), "初十");
    assert_eq!(l.date_text(), "闰四月初十");
    assert_eq!(l.full_text(), "庚子年闰四月初十");

    let n = lunar(2026, 2, 17);
    assert_eq!(n.date_text(), "正月初一");
    assert_eq!(n.full_text(), "丙午年正月初一");

    // 冬月/腊月用民间写法
    assert_eq!(lunar(2026, 1, 1).month_text(), "冬月");
    assert_eq!(lunar(2026, 2, 16).month_text(), "腊月");
}
