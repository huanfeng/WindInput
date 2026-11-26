use super::*;
use crate::stats::StatsMeta;
use std::sync::Arc;

fn store(name: &str) -> Arc<Store> {
    let p = std::env::temp_dir().join(format!("wind_sr_{}_{}.redb", std::process::id(), name));
    let _ = std::fs::remove_file(&p);
    Arc::new(Store::open(&p).unwrap())
}

fn day(chinese: u32) -> DailyStats {
    DailyStats {
        chinese,
        ..Default::default()
    }
}

fn put_device(s: &Store, dev: &str, date: &str, body: &str) {
    let d = s.stats_dir().join("devices").join(dev);
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join(format!("{}.toml", &date[..7]));
    let old = std::fs::read_to_string(&f).unwrap_or_else(|_| "version = 1\n".into());
    std::fs::write(f, format!("{old}[days.{date}]\n{body}\n")).unwrap();
}

fn collector(s: &Arc<Store>) -> StatCollector {
    s.recalculate_stats_meta(1.0).unwrap();
    StatCollector::new(s.clone(), 1.0)
}

#[test]
fn all_without_devices_equals_legacy() {
    let s = store("legacy");
    s.put_daily_stat("2020-01-01", &day(10)).unwrap();
    s.put_daily_stat("2020-01-02", &day(20)).unwrap();
    let c = collector(&s);
    let legacy = summary_for(&s, &c, &StatsScope::SelfOnly);
    assert_eq!(summary(&s, &c), legacy);
    assert_eq!(summary_for(&s, &c, &StatsScope::All), legacy);
    assert_eq!(legacy.total_chars, 30);
    // devices 目录存在但为空，同样不改变输出
    std::fs::create_dir_all(s.stats_dir().join("devices")).unwrap();
    assert_eq!(summary(&s, &c), legacy);
    let a = daily_range(&s, Some(&c), "2020-01-01", "2020-01-03").unwrap();
    let b = daily_range_for(
        &s,
        Some(&c),
        &StatsScope::SelfOnly,
        "2020-01-01",
        "2020-01-03",
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.len(), 3);
}

#[test]
fn no_devices_dir_equals_old_behavior() {
    // 本机 meta 即结果
    let s = store("old");
    s.put_daily_stat("2020-01-01", &day(10)).unwrap();
    s.put_daily_stat("2020-01-03", &day(20)).unwrap();
    let c = collector(&s);
    let r = summary(&s, &c);
    assert_eq!(r.total_chars, 30);
    assert_eq!(r.active_days, 2);
    assert_eq!(r.daily_avg, 15);
    assert_eq!(r.streak_max, 1);
}

#[test]
fn streak_from_union_of_days() {
    let s = store("streak");
    s.put_daily_stat("2020-01-01", &day(10)).unwrap();
    s.put_daily_stat("2020-01-03", &day(10)).unwrap();
    let c = collector(&s);
    assert_eq!(summary_for(&s, &c, &StatsScope::All).streak_max, 1);
    put_device(&s, "phone", "2020-01-02", "chinese = 7");
    let r = summary_for(&s, &c, &StatsScope::All);
    assert_eq!(r.streak_max, 3);
    assert_eq!(r.streak_current, 3);
    assert_eq!(r.total_chars, 27);
    assert_eq!(r.active_days, 3);
}

#[test]
fn self_scope_ignores_devices() {
    let s = store("self");
    s.put_daily_stat("2020-01-01", &day(10)).unwrap();
    let c = collector(&s);
    put_device(&s, "phone", "2020-01-01", "chinese = 5");
    put_device(&s, "phone", "2020-01-02", "chinese = 5");
    let me = summary_for(&s, &c, &StatsScope::SelfOnly);
    assert_eq!((me.total_chars, me.active_days), (10, 1));
    let all = summary_for(&s, &c, &StatsScope::All);
    assert_eq!((all.total_chars, all.active_days), (20, 2));
    let dev = summary_for(&s, &c, &StatsScope::Device("phone".into()));
    assert_eq!((dev.total_chars, dev.active_days), (10, 2));
    let rows = daily_range_for(&s, Some(&c), &StatsScope::All, "2020-01-01", "2020-01-02").unwrap();
    assert_eq!((rows[0].1.chinese, rows[1].1.chinese), (15, 5));
    let rows = daily_range_for(
        &s,
        Some(&c),
        &StatsScope::SelfOnly,
        "2020-01-01",
        "2020-01-02",
    )
    .unwrap();
    assert_eq!((rows[0].1.chinese, rows[1].1.chinese), (10, 0));
}

#[test]
fn max_speed_takes_max() {
    let s = store("maxsp");
    s.put_daily_stat(
        "2020-01-01",
        &DailyStats {
            chinese: 200,
            speed_v2: true,
            speed_chars: 200,
            active_millis: 120_000, // 100 字/分
            ..Default::default()
        },
    )
    .unwrap();
    let c = collector(&s);
    assert_eq!(summary_for(&s, &c, &StatsScope::All).max_speed, 100);
    put_device(
        &s,
        "phone",
        "2020-01-01",
        "chinese = 600\nspeed_v2 = true\nspeed_chars = 600\nactive_millis = 120000",
    );
    assert_eq!(summary_for(&s, &c, &StatsScope::All).max_speed, 300);
    assert_eq!(summary_for(&s, &c, &StatsScope::SelfOnly).max_speed, 100);
    assert_eq!(
        summary_for(&s, &c, &StatsScope::Device("phone".into())).max_speed,
        300
    );
    // 本机落库值更大时取本机
    let mut m: StatsMeta = c.get_meta();
    m.max_speed = 500;
    s.put_stats_meta(&m).unwrap();
    let c2 = StatCollector::new(s.clone(), 1.0);
    assert_eq!(summary_for(&s, &c2, &StatsScope::All).max_speed, 500);
}

#[test]
fn all_not_less_than_self_after_local_prune() {
    let s = store("prune");
    for d in ["2020-01-01", "2020-01-02", "2020-01-03", "2020-01-04"] {
        s.put_daily_stat(d, &day(10)).unwrap();
    }
    let c = collector(&s);
    // 本机清掉早期历史，meta 仍记着连续 4 天与首日
    s.prune_stats_before("2020-01-04").unwrap();
    let mut m = c.get_meta();
    m.first_day = "2020-01-01".into();
    m.streak_max = 4;
    s.put_stats_meta(&m).unwrap();
    let c = StatCollector::new(s.clone(), 1.0);
    put_device(&s, "phone", "2020-02-01", "chinese = 5");
    let me = summary_for(&s, &c, &StatsScope::SelfOnly);
    let all = summary_for(&s, &c, &StatsScope::All);
    assert!(all.streak_max >= me.streak_max);
    assert_eq!(all.streak_max, 4);
}
