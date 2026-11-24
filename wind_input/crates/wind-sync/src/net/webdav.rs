//! WebDAV 后端：Basic 认证；PROPFIND Depth:1 逐层递归列目录；写入前逐级 MKCOL。

use super::{new_agent, parse_http_date, read_body, send};
use crate::backend::{Backend, BackendError, ObjMeta};
use base64::Engine as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashSet;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct WebDavConfig {
    /// 同步根目录的 URL（如 `https://dav.example.com/dav/windinput`），该目录须已存在。
    pub url: String,
    pub username: String,
    pub password: String,
    /// 是否读取环境代理（`HTTP_PROXY` / `HTTPS_PROXY`）。
    pub proxy_from_env: bool,
}

pub struct WebDavBackend {
    /// 去掉结尾 `/` 的根 URL
    base: String,
    /// 根 URL 的路径部分（已解码、无首尾 `/`），用于把 href 还原成相对键
    base_path: String,
    auth: String,
    agent: ureq::Agent,
    /// 已确认存在的集合（相对路径）；坚果云等对 PROPFIND/MKCOL 频率敏感，实例内只探一次。
    known_dirs: Mutex<HashSet<String>>,
}

/// 保留 `-._~`，其余非字母数字都转义（`/` 由调用方按段处理）。
const SEG: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn encode_path(rel: &str) -> String {
    rel.split('/')
        .map(|s| utf8_percent_encode(s, SEG).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// URL 的路径部分（去掉 `scheme://host`）。
fn url_path(url: &str) -> &str {
    match url.find("://") {
        Some(i) => {
            let rest = &url[i + 3..];
            rest.find('/').map_or("", |j| &rest[j..])
        }
        None => url,
    }
}

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// PROPFIND 返回的 href（绝对 URL 或绝对路径，可能百分号编码）→ 相对根的键；不在根下返回 None。
fn href_to_key(href: &str, base_path: &str) -> Option<String> {
    let p = decode(url_path(href.trim()));
    let p = p.trim_matches('/');
    let key = if base_path.is_empty() {
        p.to_string()
    } else if p == base_path {
        return Some(String::new());
    } else {
        p.strip_prefix(base_path)
            .and_then(|r| r.strip_prefix('/'))?
            .to_string()
    };
    // 服务端给的 href 不可信：空段、`.`、`..` 会让后续 delete/get 逃出同步根。
    if !key.is_empty() && !crate::manifest::safe_key(&key) {
        eprintln!("[wind-sync] 忽略不安全的 WebDAV href: {href:?}");
        return None;
    }
    Some(key)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Entry {
    key: String,
    is_dir: bool,
    size: u64,
    modified: Option<u64>,
}

/// 解析 multistatus。href 不在 `base_path` 下的条目被忽略。
fn parse_multistatus(xml: &str, base_path: &str) -> Result<Vec<Entry>, BackendError> {
    #[derive(Default)]
    struct Raw {
        href: String,
        is_dir: bool,
        size: u64,
        modified: Option<u64>,
    }
    let bad =
        |e: &dyn std::fmt::Display| BackendError::Fatal(format!("PROPFIND 响应解析失败: {e}"));
    let mut rd = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut cur: Option<Raw> = None;
    let mut stack: Vec<String> = Vec::new();
    loop {
        match rd.read_event().map_err(|e| bad(&e))? {
            Event::Start(e) => {
                let n = String::from_utf8_lossy(e.local_name().as_ref()).to_ascii_lowercase();
                match n.as_str() {
                    "response" => cur = Some(Raw::default()),
                    "collection" => {
                        if let Some(c) = cur.as_mut() {
                            c.is_dir = true;
                        }
                    }
                    _ => {}
                }
                stack.push(n);
            }
            Event::Empty(e) => {
                if e.local_name().as_ref().eq_ignore_ascii_case(b"collection")
                    && let Some(c) = cur.as_mut()
                {
                    c.is_dir = true;
                }
            }
            Event::Text(t) => {
                if let (Some(c), Some(top)) = (cur.as_mut(), stack.last()) {
                    let v = t.unescape().map_err(|e| bad(&e))?;
                    let v = v.trim();
                    match top.as_str() {
                        "href" => c.href.push_str(v),
                        "getcontentlength" => c.size = v.parse().unwrap_or(0),
                        "getlastmodified" => c.modified = parse_http_date(v),
                        _ => {}
                    }
                }
            }
            Event::End(_) => {
                if stack.pop().as_deref() == Some("response")
                    && let Some(c) = cur.take()
                    && let Some(key) = href_to_key(&c.href, base_path)
                {
                    out.push(Entry {
                        key,
                        is_dir: c.is_dir || c.href.ends_with('/'),
                        size: c.size,
                        modified: c.modified,
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><propfind xmlns="DAV:"><prop><resourcetype/><getcontentlength/><getlastmodified/></prop></propfind>"#;

impl WebDavBackend {
    pub fn new(cfg: &WebDavConfig) -> Result<Self, BackendError> {
        let base = cfg.url.trim().trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(BackendError::Fatal(format!("WebDAV 地址无效: {}", cfg.url)));
        }
        let cred = format!("{}:{}", cfg.username, cfg.password);
        Ok(Self {
            base_path: decode(url_path(&base)).trim_matches('/').to_string(),
            base,
            auth: format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(cred)
            ),
            agent: new_agent(cfg.proxy_from_env),
            known_dirs: Mutex::new(HashSet::new()),
        })
    }

    fn url(&self, rel: &str) -> String {
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            self.base.clone()
        } else {
            format!("{}/{}", self.base, encode_path(rel))
        }
    }

    fn req(&self, method: &str, rel: &str) -> ureq::Request {
        self.agent
            .request(method, &self.url(rel))
            .set("Authorization", &self.auth)
    }

    fn dirs(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.known_dirs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 列一层；目录不存在返回空。
    fn propfind(&self, dir: &str) -> Result<Vec<Entry>, BackendError> {
        let mut url_dir = dir.trim_matches('/').to_string();
        if !url_dir.is_empty() {
            url_dir.push('/');
        }
        let what = format!("PROPFIND {url_dir}");
        let req = self
            .agent
            .request(
                "PROPFIND",
                &format!("{}/{}", self.base, encode_path(&url_dir)),
            )
            .set("Authorization", &self.auth)
            .set("Depth", "1")
            .set("Content-Type", "application/xml; charset=utf-8");
        let resp = match send(req, Some(PROPFIND_BODY.as_bytes()), &what) {
            Ok(r) => r,
            Err(BackendError::NotFound(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let body = read_body(resp, &what)?;
        parse_multistatus(&String::from_utf8_lossy(&body), &self.base_path)
    }

    fn walk(&self, dir: &str, out: &mut Vec<ObjMeta>) -> Result<(), BackendError> {
        let me = dir.trim_matches('/');
        for e in self.propfind(dir)? {
            if e.key == me {
                continue; // 自身
            }
            if e.is_dir {
                self.dirs().insert(e.key.clone());
                self.walk(&e.key, out)?;
            } else {
                out.push(ObjMeta {
                    key: e.key,
                    size: e.size,
                    modified: e.modified,
                });
            }
        }
        Ok(())
    }

    /// 逐级 MKCOL 到 `dir`（相对路径）；405 视为已存在。
    fn ensure_dir(&self, dir: &str) -> Result<(), BackendError> {
        // 根目录本身：尽力创建一次（用户可能填了尚不存在的文件夹名）；失败留给后面的 PUT 报错
        if !self.dirs().contains("") {
            let req = self
                .agent
                .request("MKCOL", &format!("{}/", self.base))
                .set("Authorization", &self.auth);
            if let Err(e @ BackendError::Transient(_)) = send(req, None, "MKCOL 根目录") {
                return Err(e);
            }
            self.dirs().insert(String::new());
        }
        let mut acc = String::new();
        for seg in dir.split('/').filter(|s| !s.is_empty()) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            if self.dirs().contains(&acc) {
                continue;
            }
            let what = format!("MKCOL {acc}");
            let url = format!("{}/", self.url(&acc));
            let req = self
                .agent
                .request("MKCOL", &url)
                .set("Authorization", &self.auth);
            match send(req, None, &what) {
                Ok(_) => {}
                // 已存在：405（标准）；部分服务回 301/409 之外的 Fatal 不在此列
                Err(BackendError::Fatal(m)) if m.ends_with("HTTP 405") => {}
                Err(e) => return Err(e),
            }
            self.dirs().insert(acc.clone());
        }
        Ok(())
    }

    fn put_once(&self, key: &str, bytes: &[u8]) -> Result<(), BackendError> {
        if let Some((dir, _)) = key.rsplit_once('/') {
            self.ensure_dir(dir)?;
        }
        let req = self
            .req("PUT", key)
            .set("Content-Type", "application/octet-stream");
        send(req, Some(bytes), &format!("PUT {key}")).map(|_| ())
    }
}

impl Backend for WebDavBackend {
    fn list(&self, prefix: &str) -> Result<Vec<ObjMeta>, BackendError> {
        // 前缀按字符串匹配；先列到最后一个 `/` 所在的目录
        let dir = prefix.rsplit_once('/').map_or("", |(d, _)| d);
        let mut out = Vec::new();
        self.walk(dir, &mut out)?;
        out.retain(|o| o.key.starts_with(prefix));
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, BackendError> {
        let what = format!("GET {key}");
        match send(self.req("GET", key), None, &what) {
            Ok(r) => read_body(r, &what).map(Some),
            Err(BackendError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), BackendError> {
        match self.put_once(key, bytes) {
            // 缓存的集合可能被外部删除：409（父集合缺失）/404 时清缓存重试一次
            Err(BackendError::Fatal(m)) | Err(BackendError::NotFound(m))
                if m.ends_with("HTTP 409") || m.ends_with("HTTP 404") =>
            {
                self.dirs().clear();
                self.put_once(key, bytes)
            }
            r => r,
        }
    }

    fn delete(&self, key: &str) -> Result<(), BackendError> {
        match send(self.req("DELETE", key), None, &format!("DELETE {key}")) {
            Ok(_) | Err(BackendError::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
 <D:response><D:href>/dav/wind%20input/</D:href>
  <D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>http://h:80/dav/wind%20input/%E4%B8%AD%E6%96%87.txt</D:href>
  <D:propstat><D:prop><D:resourcetype/><D:getcontentlength>12</D:getcontentlength>
  <D:getlastmodified>Sun, 06 Nov 1994 08:49:37 GMT</D:getlastmodified></D:prop></D:propstat></D:response>
 <D:response><D:href>/dav/wind%20input/sub/</D:href>
  <D:propstat><D:prop><D:resourcetype><D:collection></D:collection></D:resourcetype></D:prop></D:propstat></D:response>
 <D:response><D:href>/elsewhere/x</D:href></D:response>
</D:multistatus>"#;

    #[test]
    fn parses_multistatus() {
        let v = parse_multistatus(XML, "dav/wind input").unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].key, "");
        assert!(v[0].is_dir);
        assert_eq!(v[1].key, "中文.txt");
        assert_eq!((v[1].size, v[1].modified), (12, Some(784111777)));
        assert!(!v[1].is_dir);
        assert_eq!(v[2].key, "sub");
        assert!(v[2].is_dir);
    }

    #[test]
    fn parses_unprefixed_namespace() {
        let x = r#"<multistatus xmlns="DAV:"><response><href>/a/b</href><propstat><prop><getcontentlength>3</getcontentlength></prop></propstat></response></multistatus>"#;
        let v = parse_multistatus(x, "").unwrap();
        assert_eq!(
            v,
            vec![Entry {
                key: "a/b".into(),
                is_dir: false,
                size: 3,
                modified: None
            }]
        );
    }

    #[test]
    fn href_normalization() {
        assert_eq!(href_to_key("/dav/a%20b/c", "dav"), Some("a b/c".into()));
        assert_eq!(href_to_key("https://x/dav/", "dav"), Some("".into()));
        assert_eq!(href_to_key("/other/c", "dav"), None);
        assert_eq!(href_to_key("/davx/c", "dav"), None);
        assert_eq!(href_to_key("/c", ""), Some("c".into()));
        for bad in [
            "/dav/a/../b",
            "/dav/a/%2e%2e/b",
            "/dav/a//b",
            "/dav/./b",
            "/dav/a/..",
            "/dav/%2E",
        ] {
            assert_eq!(href_to_key(bad, "dav"), None, "{bad}");
        }
        assert_eq!(href_to_key("/a/../b", ""), None);
    }

    #[test]
    fn url_building() {
        let b = WebDavBackend::new(&WebDavConfig {
            url: "https://h/dav/wind%20input/".into(),
            username: "u".into(),
            password: "p".into(),
            proxy_from_env: false,
        })
        .unwrap();
        assert_eq!(b.base_path, "dav/wind input");
        assert_eq!(
            b.url("a b/中.txt"),
            "https://h/dav/wind%20input/a%20b/%E4%B8%AD.txt"
        );
        assert_eq!(b.url(""), "https://h/dav/wind%20input");
        assert!(
            WebDavBackend::new(&WebDavConfig {
                url: "ftp://x".into(),
                username: "".into(),
                password: "".into(),
                proxy_from_env: false
            })
            .is_err()
        );
    }
}
