// SPDX-License-Identifier: MPL-2.0
//! core 与同步进程（`wind_input_sync`）之间的匿名管道协议（设计 `docs/design/data-sync.md` §3.2、§3.3）。
//!
//! 只有类型与帧读写：4 字节大端长度 + JSON。core 依赖本 crate，因此**不得**引入网络 / 加密依赖。
//! 机密（同步密钥、S3 secret）不在 [`SyncSettings`] 里，只经 [`Op::SetSecret`] 等命令单向流向同步进程。

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
    #[serde(default = "default_interval")]
    pub interval_min: u32,
    /// 本机在设备名册里的名字；空则由同步进程取主机名。不随同步传播。
    pub device_name: String,
    /// 不校验存储服务的 HTTPS 证书（自建服务用自签名证书时由用户显式打开）。
    /// 不算换存储：切换它不影响已保存的密码。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub skip_cert_verify: bool,
}

impl Default for SyncSettings {
    fn default() -> Self {
        SyncSettings {
            enabled: false,
            backend: None,
            interval_min: DEFAULT_INTERVAL_MIN,
            device_name: String::new(),
            skip_cert_verify: false,
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

/// 读设置文件（TOML）。文件不存在 = 默认值；读不了或解析失败返回错误文案（调用方决定是否降级为默认值）。
pub fn load_settings_file(path: &std::path::Path) -> Result<SyncSettings, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(SyncSettings::default()),
        Err(e) => return Err(format!("读取同步设置 {} 失败: {e}", path.display())),
    };
    toml::from_str(&text).map_err(|e| format!("同步设置 {} 解析失败: {e}", path.display()))
}

/// 同目录临时文件 + fsync + rename：写一半或断电都不会留下半个文件。
pub fn write_file_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = path.parent().ok_or("路径无父目录")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
    let tmp = path.with_extension("tmp");
    let r = std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .map_err(|e| format!("写 {} 失败: {e}", tmp.display()))
        .and_then(|()| {
            std::fs::rename(&tmp, path).map_err(|e| format!("替换 {} 失败: {e}", path.display()))
        });
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// 写设置文件。
pub fn save_settings_file(path: &std::path::Path, s: &SyncSettings) -> Result<(), String> {
    let text = toml::to_string_pretty(s).map_err(|e| format!("序列化同步设置失败: {e}"))?;
    write_file_atomic(path, text.as_bytes())
}

/// 设置端经 `sync.command` 发起的操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    SyncNow,
    TestConnection,
    /// 远端还没有同步库时生成同步密钥并建库，回 `{ok, key}`。
    CreateKey,
    /// 粘贴同步密钥加入已有的同步库：`{key}`。
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
    /// 改本机设备号：`{id}`。
    RenameDevice,
    /// 改本机所在组的组名：`{name}`。
    SetGroupName,
    /// 加入设备组：`{group, mode: "theirs" | "mine"}`，theirs 用组的设置覆盖本机。
    JoinGroup,
    /// 退出当前组，以本机设置新建单机组：`{name?}`。
    LeaveGroup,
    /// 认领已有设备（沿用它的号与组，用组的设置覆盖本机）：`{dev}`。
    ClaimDevice,
    /// 加入同步后确认身份：以当前设备号新建单机组。
    ConfirmIdentity,
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
        /// 绿色版：默认设备号加 `-portable`。
        #[serde(default)]
        portable: bool,
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
    /// 本机已持有同步密钥（已生成或已粘贴加入）。
    pub joined: bool,
    /// 本机设备号。
    pub device: String,
    /// 本机所在设备组号。
    pub group: String,
    pub group_name: String,
    /// 已加入同步库但尚未确认身份（恢复设备 / 加入组 / 新建独立组），确认前不同步。
    pub needs_identity: bool,
    /// 已结束的轮次数，每轮结束 +1（成败都算）。
    pub round: u64,
    /// 最近一轮的结果。
    pub last_round: Option<RoundStats>,
}

/// 一轮同步的结果统计。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoundStats {
    pub ok: bool,
    pub downloaded: u32,
    pub uploaded: u32,
    /// 本轮产生的冲突副本数。
    pub conflicts: u32,
    pub error: Option<String>,
    /// 本轮由 `SyncNow` 或大批量删除确认强制触发。
    pub forced: bool,
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
