//! 真实后端（feature `net`）：WebDAV 与 S3，共用一个同步 HTTP 客户端（ureq 2 + rustls）。
//!
//! 超时：连接 15 秒、整体 60 秒。不跟随重定向（重定向多半是 URL 写错或 http→https，
//! 当作错误报出来比悄悄把 PUT 变成 GET 好）。环境代理默认读取（`ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY`，
//! 支持 http、socks4/4a/5、socks5h），不支持的写法记警告后直连；配置里 `proxy_from_env = false` 可关闭。

mod s3;
mod webdav;

pub use s3::{S3Backend, S3Config};
pub use webdav::{WebDavBackend, WebDavConfig};

use crate::backend::BackendError;
use std::io::Read;
use std::time::Duration;

const MAX_BODY: u64 = 512 * 1024 * 1024;

pub(crate) fn new_agent(proxy_from_env: bool) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .redirects(0)
        // 代理由下面自行解析：ureq 内置的环境读取会静默丢弃它不认的写法。
        .try_proxy_from_env(false);
    if proxy_from_env {
        match proxy_from_vars(|k| std::env::var(k).ok()) {
            Ok(Some(p)) => b = b.proxy(p),
            Ok(None) => {}
            Err(w) => eprintln!("[wind-sync] {w}，改为直连"),
        }
    }
    b.build()
}

/// 按 curl 的习惯顺序取第一个非空的代理变量并解析；没有则 `Ok(None)`。
fn proxy_from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Option<ureq::Proxy>, String> {
    const KEYS: [&str; 6] = [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ];
    let Some((key, raw)) = KEYS
        .iter()
        .find_map(|k| get(k).filter(|v| !v.trim().is_empty()).map(|v| (*k, v)))
    else {
        return Ok(None);
    };
    let url = normalize_proxy_url(raw.trim()).ok_or_else(|| format!("{key} 的代理写法不受支持"))?;
    ureq::Proxy::new(&url)
        .map(Some)
        .map_err(|_| format!("{key} 的代理地址无法解析"))
}

/// socks5h（代理端解析域名）并入 socks5；ureq 的 socks5 本就把域名交给代理。
/// 其余不认识的 scheme（含 https 代理）返回 `None`。
fn normalize_proxy_url(raw: &str) -> Option<String> {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Some(raw.to_string()); // 无 scheme，ureq 按 http 处理
    };
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "http" => "http",
        "socks" | "socks5" | "socks5h" => "socks5",
        "socks4" => "socks4",
        "socks4a" => "socks4a",
        _ => return None,
    };
    Some(format!("{scheme}://{rest}"))
}

#[cfg(test)]
mod proxy_tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn accepts_http_and_socks_schemes() {
        for u in [
            "http://127.0.0.1:7890",
            "socks5://127.0.0.1:1080",
            "socks5h://u:p@127.0.0.1:1080",
            "socks4://h:1",
            "socks4a://h:1",
            "SOCKS5://h:1/",
            "127.0.0.1:8080",
        ] {
            assert!(
                matches!(proxy_from_vars(env(&[("ALL_PROXY", u)])), Ok(Some(_))),
                "{u}"
            );
        }
    }

    #[test]
    fn unsupported_scheme_is_an_error_not_a_panic() {
        assert!(proxy_from_vars(env(&[("ALL_PROXY", "https://p:1")])).is_err());
        assert!(proxy_from_vars(env(&[("HTTP_PROXY", "ftp://p:1")])).is_err());
    }

    #[test]
    fn unset_or_blank_means_direct() {
        assert!(matches!(proxy_from_vars(env(&[])), Ok(None)));
        assert!(matches!(
            proxy_from_vars(env(&[("ALL_PROXY", "  "), ("http_proxy", "http://h:1")])),
            Ok(Some(_))
        ));
    }

    #[test]
    fn all_proxy_wins_over_http_proxy() {
        let p = proxy_from_vars(env(&[
            ("HTTP_PROXY", "http://a:1"),
            ("ALL_PROXY", "socks5h://b:2"),
        ]))
        .unwrap()
        .unwrap();
        assert!(format!("{p:?}").contains("\"b\""), "{p:?}");
    }
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
