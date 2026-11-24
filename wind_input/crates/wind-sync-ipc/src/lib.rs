// SPDX-License-Identifier: MPL-2.0
//! core 与同步进程（`wind_input_sync`）之间的匿名管道协议（设计 `docs/design/data-sync.md` §3.2、§3.3）。
//!
//! 只有类型与帧读写：4 字节大端长度 + JSON。core 依赖本 crate，因此**不得**引入网络 / 加密依赖。
//! 机密（口令、S3 secret、密钥）不在 [`SyncSettings`] 里，只经 [`Op::SetSecret`] 等命令单向流向同步进程。

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 单帧上限。命令参数只有几个字符串，超过即视为对端出错。
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

/// WebDAV 或 S3 兼容存储。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Backend {
    Webdav {
        url: String,
        user: String,
    },
    S3 {
        endpoint: String,
        #[serde(default)]
        region: String,
        bucket: String,
        #[serde(default)]
        prefix: String,
        access_key_id: String,
        #[serde(default)]
        path_style: bool,
    },
}

/// 同步范围四类开关，默认全开。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scope {
    pub config: bool,
    pub schemas: bool,
    pub themes: bool,
    pub dicts: bool,
}

impl Default for Scope {
    fn default() -> Self {
        Scope {
            config: true,
            schemas: true,
            themes: true,
            dicts: true,
        }
    }
}

pub const DEFAULT_INTERVAL_MIN: u32 = 15;
pub const MAX_INTERVAL_MIN: u32 = 24 * 60;
/// 设备名的字符数上限。
pub const MAX_DEVICE_NAME_CHARS: usize = 64;

fn default_interval() -> u32 {
    DEFAULT_INTERVAL_MIN
}

/// 同步设置（设备本地，不进 config.toml）。**不含任何密码 / secret。**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncSettings {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<Backend>,
    pub scope: Scope,
    #[serde(default = "default_interval")]
    pub interval_min: u32,
    /// 本机在设备名册里的名字；空则由同步进程取主机名。不随同步传播。
    pub device_name: String,
}

impl Default for SyncSettings {
    fn default() -> Self {
        SyncSettings {
            enabled: false,
            backend: None,
            scope: Scope::default(),
            interval_min: DEFAULT_INTERVAL_MIN,
            device_name: String::new(),
        }
    }
}

impl SyncSettings {
    /// 写盘 / 下发前的校验。未启用时只检查已填的部分，允许半成品先存下来。
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_INTERVAL_MIN).contains(&self.interval_min) {
            return Err(format!("同步间隔须在 1 到 {MAX_INTERVAL_MIN} 分钟之间"));
        }
        if self.device_name.chars().count() > MAX_DEVICE_NAME_CHARS {
            return Err(format!("设备名不能超过 {MAX_DEVICE_NAME_CHARS} 个字符"));
        }
        match &self.backend {
            None if self.enabled => return Err("启用同步前须先配置存储后端".into()),
            None => {}
            Some(Backend::Webdav { url, .. }) => {
                let u = url.trim();
                if u.is_empty() {
                    return Err("WebDAV 地址不能为空".into());
                }
                if !(u.starts_with("http://") || u.starts_with("https://")) {
                    return Err("WebDAV 地址须以 http:// 或 https:// 开头".into());
                }
                check_url("WebDAV 地址", u)?;
            }
            Some(Backend::S3 {
                endpoint,
                bucket,
                access_key_id,
                ..
            }) => {
                if endpoint.trim().is_empty() {
                    return Err("S3 endpoint 不能为空".into());
                }
                check_url("S3 endpoint", endpoint.trim())?;
                if bucket.trim().is_empty() {
                    return Err("S3 bucket 不能为空".into());
                }
                if access_key_id.trim().is_empty() {
                    return Err("S3 access key id 不能为空".into());
                }
            }
        }
        Ok(())
    }
}

/// 取 URL 的 authority（scheme 之后到首个 `/ ? #` 之前）。没有 scheme 返回 None。
fn authority(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some((scheme, &rest[..end]))
}

/// authority 里的主机部分（去端口、去 IPv6 方括号）。
fn host_of(auth: &str) -> &str {
    if let Some(r) = auth.strip_prefix('[') {
        return r.split(']').next().unwrap_or("");
    }
    auth.rsplit_once(':').map_or(auth, |(h, _)| h)
}

/// 本机或私网地址（明文 http 只允许发往这里）。
fn is_local_host(host: &str) -> bool {
    use std::net::IpAddr;
    let h = host.to_ascii_lowercase();
    if h == "localhost" {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => a.is_loopback() || a.is_private() || a.is_link_local(),
        Ok(IpAddr::V6(a)) => {
            let seg0 = a.segments()[0];
            a.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

/// 存储地址的安全检查：不得带 `user:pass@`（凭据只走机密存储）；明文 http 只允许本机 / 私网。
fn check_url(label: &str, url: &str) -> Result<(), String> {
    let Some((scheme, auth)) = authority(url) else {
        return Ok(()); // 缺 scheme 等由各自的格式检查报
    };
    if auth.contains('@') {
        return Err(format!(
            "{label}不能在地址里写用户名或密码（user:pass@），请分开填写"
        ));
    }
    if scheme.eq_ignore_ascii_case("http") && !is_local_host(host_of(auth)) {
        return Err(format!(
            "{label}使用明文 http 会泄露凭据，请改用 https（仅本机或私网地址允许 http）"
        ));
    }
    Ok(())
}

/// 设置端经 `sync.command` 发起的操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    SyncNow,
    TestConnection,
    SetPassphrase,
    ImportKey,
    ExportKey,
    ResetRemote,
    SetSecret,
    /// 确认大批量删除（同步因护栏停止后）：下一轮放行一次。
    ConfirmMassDelete,
    /// 列出同步库里的设备（设备名册）。
    ListDevices,
    /// 移除某个设备的远端数据（清单与设备信息），不能是本机；需 `confirm: true`。
    RemoveDevice,
}

impl Op {
    pub fn parse(s: &str) -> Option<Op> {
        serde_json::from_value(Value::String(s.to_string())).ok()
    }
}

/// core → 同步进程。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToSync {
    /// 启动后的第一帧。`suffix` 是变体后缀（`_dev` 等），`userdata_dir` 用户目录，`state_dir` 同步状态目录。
    Hello {
        suffix: String,
        userdata_dir: String,
        state_dir: String,
        settings: SyncSettings,
    },
    /// 设置变更。
    Settings(SyncSettings),
    Command {
        id: u64,
        op: Op,
        #[serde(default)]
        args: Value,
    },
    /// 回应同步进程的 [`FromSync::Request`]。
    Reply {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Shutdown,
}

impl ToSync {
    pub fn reply(id: u64, r: Result<Value, String>) -> ToSync {
        match r {
            Ok(v) => ToSync::Reply {
                id,
                result: Some(v),
                error: None,
            },
            Err(e) => ToSync::Reply {
                id,
                result: None,
                error: Some(e),
            },
        }
    }
}

/// [`Status::conflict_paths`] 的条数上限。
pub const MAX_CONFLICT_PATHS: usize = 50;

/// 同步进程上报的状态。不带文件内容或路径以外的数据。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    /// `idle` / `syncing` / `error` / `disabled` 等，由同步进程定义，core 原样转述。
    pub phase: String,
    /// 上次成功同步的 Unix 秒。
    pub last_ok: Option<i64>,
    pub last_error: Option<String>,
    pub pending: u32,
    pub conflicts: u32,
    /// 当前仍在本地的冲突副本（相对用户目录，`/` 分隔），新→旧，最多 [`MAX_CONFLICT_PATHS`] 条。
    pub conflict_paths: Vec<String>,
    /// 本机是否已有机密（凭据与密钥）。
    pub has_secret: bool,
    /// 本机已持有 DEK（已设置口令或导入密钥）。设置端据此区分「设置口令」与「修改口令」。
    pub joined: bool,
}

/// 同步进程 → core。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromSync {
    Status(Status),
    Request {
        id: u64,
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// 回应 [`ToSync::Command`]。
    CommandReply {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl FromSync {
    pub fn command_reply(id: u64, r: Result<Value, String>) -> FromSync {
        match r {
            Ok(v) => FromSync::CommandReply {
                id,
                result: Some(v),
                error: None,
            },
            Err(e) => FromSync::CommandReply {
                id,
                result: None,
                error: Some(e),
            },
        }
    }
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// 写一帧并 flush。
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(msg).map_err(invalid)?;
    if payload.len() > MAX_FRAME {
        return Err(invalid(format!("帧长 {} 超过上限", payload.len())));
    }
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(&payload);
    w.write_all(&buf)?;
    w.flush()
}

/// 读一帧。对端正常关闭（帧边界处 EOF）返回 `Ok(None)`。
pub fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(r: &mut R) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(invalid(format!("帧长 {len} 超过上限")));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map(Some).map_err(invalid)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
