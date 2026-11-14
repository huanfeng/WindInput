//! 日志行时间戳（UTC）。宿主与扩展的日志是 stderr 直写文件，不走 tracing；不带时间就分不清
//! 「扩展重启了五次」还是「历次部署攒下五行」。

use std::time::{SystemTime, UNIX_EPOCH};

/// 形如 `2026-10-20 03:58:05Z`。
pub fn utc_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    stamp_of(secs)
}

fn stamp_of(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // 由「1970-01-01 起的天数」求公历日期（Howard Hinnant 的 civil_from_days）。
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::stamp_of;

    #[test]
    fn formats_known_instants() {
        assert_eq!(stamp_of(0), "1970-01-01 00:00:00Z");
        assert_eq!(stamp_of(951_782_400), "2000-02-29 00:00:00Z", "闰日");
        assert_eq!(stamp_of(1_792_468_685), "2026-10-20 03:58:05Z");
    }
}
