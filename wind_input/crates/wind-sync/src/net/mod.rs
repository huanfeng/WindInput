//! 真实后端（feature `net`）：WebDAV 与 S3，共用一个同步 HTTP 客户端（ureq 2 + rustls）。
//!
//! 超时：连接 15 秒、整体 60 秒。不跟随重定向（重定向多半是 URL 写错或 http→https，
//! 当作错误报出来比悄悄把 PUT 变成 GET 好）。环境代理默认读取（`HTTP_PROXY` / `HTTPS_PROXY`），
//! 配置里 `proxy_from_env = false` 可关闭。

mod s3;
mod webdav;

pub use s3::{S3Backend, S3Config};
pub use webdav::{WebDavBackend, WebDavConfig};

use crate::backend::BackendError;
use std::io::Read;
use std::time::Duration;

const MAX_BODY: u64 = 512 * 1024 * 1024;

pub(crate) fn new_agent(proxy_from_env: bool) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .redirects(0)
        .try_proxy_from_env(proxy_from_env)
        .build()
}

/// HTTP 状态码 → 错误分类。
pub(crate) fn map_status(code: u16, what: &str) -> BackendError {
    let msg = format!("{what}: HTTP {code}");
    match code {
        401 | 403 => BackendError::Auth(msg),
        404 => BackendError::NotFound(msg),
        507 => BackendError::Fatal(msg),
        408 | 423 | 425 | 429 | 500..=599 => BackendError::Transient(msg),
        _ => BackendError::Fatal(msg),
    }
}

pub(crate) fn map_transport(t: &ureq::Transport, what: &str) -> BackendError {
    use ureq::ErrorKind::*;
    let msg = format!("{what}: {t}");
    match t.kind() {
        InvalidUrl | UnknownScheme | InsecureRequestHttpsOnly => BackendError::Fatal(msg),
        _ => BackendError::Transient(msg),
    }
}

/// 发请求并把非 2xx 一律转为 [`BackendError`]。
pub(crate) fn send(
    req: ureq::Request,
    body: Option<&[u8]>,
    what: &str,
) -> Result<ureq::Response, BackendError> {
    let r = match body {
        Some(b) => req.send_bytes(b),
        None => req.call(),
    };
    match r {
        Ok(resp) if (200..300).contains(&resp.status()) => Ok(resp),
        Ok(resp) => Err(map_status(resp.status(), what)),
        Err(ureq::Error::Status(code, _)) => Err(map_status(code, what)),
        Err(ureq::Error::Transport(t)) => Err(map_transport(&t, what)),
    }
}

pub(crate) fn read_body(resp: ureq::Response, what: &str) -> Result<Vec<u8>, BackendError> {
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut buf)
        .map_err(|e| BackendError::Transient(format!("{what}: 读取响应失败: {e}")))?;
    Ok(buf)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn to_unix(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<u64> {
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    u64::try_from(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s).ok()
}

/// RFC 1123（`Sun, 06 Nov 1994 08:49:37 GMT`）→ unix 秒。
pub(crate) fn parse_http_date(s: &str) -> Option<u64> {
    let s = s.trim();
    let s = s.split_once(',').map_or(s, |(_, r)| r).trim();
    let mut it = s.split_whitespace();
    let d: i64 = it.next()?.parse().ok()?;
    let mon = it.next()?.to_ascii_lowercase();
    let y: i64 = it.next()?.parse().ok()?;
    let t = it.next()?;
    let mo = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| mon.starts_with(m))? as i64
        + 1;
    let mut tp = t.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    let sec: i64 = tp.next().unwrap_or("0").parse().ok()?;
    to_unix(y, mo, d, h, mi, sec)
}

/// ISO 8601（`2009-10-12T17:50:30.000Z`，S3 用）→ unix 秒；只认 Z / 无时区。
pub(crate) fn parse_iso_date(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, time) = s.split_once('T')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let time = time.trim_end_matches('Z');
    let time = time.split(['.', '+']).next()?;
    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    let sec: i64 = tp.next().unwrap_or("0").parse().ok()?;
    to_unix(y, mo, d, h, mi, sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert!(matches!(map_status(401, "x"), BackendError::Auth(_)));
        assert!(matches!(map_status(403, "x"), BackendError::Auth(_)));
        assert!(matches!(map_status(404, "x"), BackendError::NotFound(_)));
        for c in [408, 429, 500, 502, 503, 504] {
            assert!(map_status(c, "x").retryable(), "{c}");
        }
        for c in [400, 405, 409, 412, 301, 507] {
            assert!(matches!(map_status(c, "x"), BackendError::Fatal(_)), "{c}");
        }
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784111777)
        );
        assert_eq!(parse_iso_date("1994-11-06T08:49:37.000Z"), Some(784111777));
        assert_eq!(parse_iso_date("1994-11-06T08:49:37Z"), Some(784111777));
        assert_eq!(parse_http_date("garbage"), None);
        assert_eq!(parse_iso_date("1994-13-06T08:49:37Z"), None);
    }
}
