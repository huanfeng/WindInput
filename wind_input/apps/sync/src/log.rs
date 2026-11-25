//! 桌面：日志写 stderr，由 core 重定向并轮转到 `sync.log`。
//! Android：在输入法进程内运行，stderr 无人接收，改走 `tracing`（宿主已接到 logcat）。
//! 只记系统事件与错误，不记凭据、同步密钥与文件内容。

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// unix 秒 → `YYYY-MM-DD HH:MM:SS`（UTC）。
pub fn stamp(unix: u64) -> String {
    let days = (unix / 86400) as i64;
    let rem = unix % 86400;
    // civil_from_days（Howard Hinnant）
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(not(target_os = "android"))]
macro_rules! log {
    ($($a:tt)*) => {
        eprintln!("{} UTC [sync] {}", $crate::log::stamp($crate::log::now_unix()), format_args!($($a)*))
    };
}
#[cfg(target_os = "android")]
macro_rules! log {
    ($($a:tt)*) => {
        tracing::info!(target: "sync", "{}", format_args!($($a)*))
    };
}
pub(crate) use log;

#[cfg(test)]
mod tests {
    #[test]
    fn stamp_formats_utc() {
        assert_eq!(super::stamp(0), "1970-01-01 00:00:00");
        assert_eq!(super::stamp(1_795_478_400 + 3661), "2026-11-24 01:01:01");
    }
}
