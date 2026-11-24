//! S3 兼容后端：`rusty-s3` 做 SigV4 预签名，`ureq` 发请求。

use super::{new_agent, parse_iso_date, read_body, send};
use crate::backend::{Backend, BackendError, ObjMeta};
use rusty_s3::actions::ListObjectsV2;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use std::time::Duration;

const SIGN_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone)]
pub struct S3Config {
    /// 服务端点，如 `https://s3.us-east-1.amazonaws.com`、`http://127.0.0.1:9000`
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    /// 桶内前缀（可为空）；所有键都放在它之下
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// 路径式寻址（`endpoint/bucket/key`）；MinIO 与部分国内服务需要
    pub path_style: bool,
    pub proxy_from_env: bool,
    /// ListObjectsV2 每页条数；None 用服务端默认（1000）。主要给测试验分页。
    pub list_page_size: Option<usize>,
}

pub struct S3Backend {
    bucket: Bucket,
    creds: Credentials,
    /// 空或以 `/` 结尾
    prefix: String,
    page: Option<usize>,
    agent: ureq::Agent,
}

fn normalize_prefix(p: &str) -> String {
    let p = p.trim_matches('/');
    if p.is_empty() {
        String::new()
    } else {
        format!("{p}/")
    }
}

fn decode_key(k: &str) -> String {
    percent_encoding::percent_decode_str(&k.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

impl S3Backend {
    pub fn new(cfg: &S3Config) -> Result<Self, BackendError> {
        let endpoint = cfg
            .endpoint
            .trim()
            .parse::<url::Url>()
            .map_err(|e| BackendError::Fatal(format!("S3 端点无效: {e}")))?;
        let style = if cfg.path_style {
            UrlStyle::Path
        } else {
            UrlStyle::VirtualHost
        };
        let bucket = Bucket::new(endpoint, style, cfg.bucket.clone(), cfg.region.clone())
            .map_err(|e| BackendError::Fatal(format!("S3 端点无效: {e}")))?;
        Ok(Self {
            bucket,
            creds: Credentials::new(cfg.access_key_id.clone(), cfg.secret_access_key.clone()),
            prefix: normalize_prefix(&cfg.prefix),
            page: cfg.list_page_size,
            agent: new_agent(cfg.proxy_from_env),
        })
    }

    fn full(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }
}

impl Backend for S3Backend {
    fn list(&self, prefix: &str) -> Result<Vec<ObjMeta>, BackendError> {
        let full_prefix = self.full(prefix);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut act = ListObjectsV2::new(&self.bucket, Some(&self.creds));
            act.with_prefix(full_prefix.as_str());
            if let Some(n) = self.page {
                act.with_max_keys(n);
            }
            if let Some(t) = &token {
                act.with_continuation_token(t.as_str());
            }
            let url = act.sign(SIGN_TTL);
            let resp = send(self.agent.get(url.as_str()), None, "S3 list")?;
            let body = read_body(resp, "S3 list")?;
            let parsed = ListObjectsV2::parse_response(&body)
                .map_err(|e| BackendError::Fatal(format!("S3 list 响应解析失败: {e}")))?;
            for c in parsed.contents {
                // encoding-type=url：rusty-s3 不解码，键里 `+` 是空格、其余百分号编码
                let key = decode_key(&c.key);
                if let Some(k) = key.strip_prefix(&self.prefix) {
                    out.push(ObjMeta {
                        key: k.to_string(),
                        size: c.size,
                        modified: parse_iso_date(&c.last_modified),
                    });
                }
            }
            match parsed.next_continuation_token {
                Some(t) if !t.is_empty() => token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, BackendError> {
        let what = format!("S3 GET {key}");
        let url = self
            .bucket
            .get_object(Some(&self.creds), &self.full(key))
            .sign(SIGN_TTL);
        match send(self.agent.get(url.as_str()), None, &what) {
            Ok(r) => read_body(r, &what).map(Some),
            Err(BackendError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), BackendError> {
        let url = self
            .bucket
            .put_object(Some(&self.creds), &self.full(key))
            .sign(SIGN_TTL);
        send(
            self.agent.put(url.as_str()),
            Some(bytes),
            &format!("S3 PUT {key}"),
        )
        .map(|_| ())
    }

    fn delete(&self, key: &str) -> Result<(), BackendError> {
        let url = self
            .bucket
            .delete_object(Some(&self.creds), &self.full(key))
            .sign(SIGN_TTL);
        match send(
            self.agent.delete(url.as_str()),
            None,
            &format!("S3 DELETE {key}"),
        ) {
            Ok(_) | Err(BackendError::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(path_style: bool, prefix: &str) -> S3Config {
        S3Config {
            endpoint: "http://127.0.0.1:9000".into(),
            region: "us-east-1".into(),
            bucket: "bk".into(),
            prefix: prefix.into(),
            access_key_id: "ak".into(),
            secret_access_key: "sk".into(),
            path_style,
            proxy_from_env: false,
            list_page_size: None,
        }
    }

    #[test]
    fn key_decoding() {
        assert_eq!(decode_key("a+b/%E4%B8%AD%2B%26"), "a b/中+&");
    }

    #[test]
    fn prefix_normalization() {
        assert_eq!(normalize_prefix(""), "");
        assert_eq!(normalize_prefix("/a/b/"), "a/b/");
        assert_eq!(normalize_prefix("a"), "a/");
    }

    #[test]
    fn presigned_urls() {
        let b = S3Backend::new(&cfg(true, "wi")).unwrap();
        let u = b
            .bucket
            .get_object(Some(&b.creds), &b.full("x y/中.bin"))
            .sign(SIGN_TTL);
        assert!(
            u.as_str().starts_with("http://127.0.0.1:9000/bk/wi/x%20y/"),
            "{u}"
        );
        assert!(u.as_str().contains("X-Amz-Signature="));
        let v = S3Backend::new(&S3Config {
            endpoint: "https://s3.example.com".into(),
            ..cfg(false, "")
        })
        .unwrap();
        let u = v.bucket.get_object(Some(&v.creds), "k").sign(SIGN_TTL);
        assert_eq!(u.host_str(), Some("bk.s3.example.com"), "{u}");
        assert!(
            S3Backend::new(&S3Config {
                endpoint: "nope".into(),
                ..cfg(true, "")
            })
            .is_err()
        );
    }
}
