//! 按同步设置构建后端。生产用 WebDAV / S3；测试注入内存后端。

use wind_sync::Backend;
use wind_sync::net::{S3Backend, S3Config, WebDavBackend, WebDavConfig};
use wind_sync_ipc::{Backend as Target, SyncSettings};

use crate::errors::backend_error;

pub trait BackendFactory: Send + Sync {
    /// `secret` 是 WebDAV 密码或 S3 secret access key。
    fn build(&self, settings: &SyncSettings, secret: &str) -> Result<Box<dyn Backend>, String>;
}

pub struct NetFactory;

impl BackendFactory for NetFactory {
    fn build(&self, settings: &SyncSettings, secret: &str) -> Result<Box<dyn Backend>, String> {
        match settings.backend.as_ref().ok_or("尚未配置存储后端")? {
            Target::Webdav { url, user } => {
                let b = WebDavBackend::new(&WebDavConfig {
                    url: url.trim().to_string(),
                    username: user.clone(),
                    password: secret.to_string(),
                    proxy_from_env: true,
                    skip_cert_verify: settings.skip_cert_verify,
                })
                .map_err(|e| backend_error(&e))?;
                Ok(Box::new(b))
            }
            Target::S3 {
                endpoint,
                region,
                bucket,
                prefix,
                access_key_id,
                path_style,
            } => {
                let b = S3Backend::new(&S3Config {
                    endpoint: endpoint.trim().to_string(),
                    region: region.clone(),
                    bucket: bucket.trim().to_string(),
                    prefix: prefix.clone(),
                    access_key_id: access_key_id.clone(),
                    secret_access_key: secret.to_string(),
                    path_style: *path_style,
                    proxy_from_env: true,
                    skip_cert_verify: settings.skip_cert_verify,
                    list_page_size: None,
                })
                .map_err(|e| backend_error(&e))?;
                Ok(Box::new(b))
            }
        }
    }
}

/// 占位：`Runner::new` 把 `Config` 里的工厂搬走后留在原位。
pub struct NoFactory;

impl BackendFactory for NoFactory {
    fn build(&self, _: &SyncSettings, _: &str) -> Result<Box<dyn Backend>, String> {
        Err("内部错误：后端工厂已移交".into())
    }
}
