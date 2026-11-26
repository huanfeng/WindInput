//! 同步进程私有状态与两段提交（待定/已提交）记录（设计 §4.5、§4.6）。

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;

/// 一个学习文件组的状态。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GroupState {
    /// encode(本机完整贡献)
    pub contrib: String,
    /// 基线 M：文件原文（render 后字节的 UTF-8）
    pub base: String,
    /// 上次上传的底稿 encode，用于 split
    pub last_base: String,
    /// 底稿上次压实时间（秒）
    pub base_at: u64,
    /// 基线 M 物化时所依据的各设备目录编号
    pub peer_seen: BTreeMap<String, u64>,
}

/// 待定记录：物化写盘后要采用的各组状态。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub token: String,
    pub groups: BTreeMap<String, GroupState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearnState {
    pub version: u32,
    /// 后端位置指纹 + meta.check
    pub library: String,
    /// 组名："<schema>/<kind>" 或 "completion"
    pub groups: BTreeMap<String, GroupState>,
    pub pending: Option<Pending>,
    pub last_ok: u64,
    pub index_gen: u64,
    /// 设备 → 已下载到本地的最新目录编号
    pub peer_seen: BTreeMap<String, u64>,
}

impl Default for LearnState {
    fn default() -> Self {
        Self {
            version: VERSION,
            library: String::new(),
            groups: BTreeMap::new(),
            pending: None,
            last_ok: 0,
            index_gen: 0,
            peer_seen: BTreeMap::new(),
        }
    }
}

/// `resolve_pending` 的结果：按组名列出采用与丢弃的待定状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    pub adopted: Vec<String>,
    pub dropped: Vec<String>,
}

impl LearnState {
    /// 文件不存在返回默认；版本不是 1 返回 `InvalidData`。
    pub fn load(path: &Path) -> io::Result<LearnState> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let bad = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| bad(format!("学习状态解析失败: {e}")))?;
        let ver = v.get("version").and_then(|x| x.as_u64());
        if ver != Some(VERSION as u64) {
            return Err(bad(format!("学习状态版本不支持: {ver:?}")));
        }
        serde_json::from_value(v).map_err(|e| bad(format!("学习状态格式错误: {e}")))
    }

    /// 同目录 `.<文件名>.tmp` 写入并 fsync，再 rename 覆盖。
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "路径没有文件名"))?;
        let tmp = path.with_file_name(format!(".{}.tmp", name.to_string_lossy()));
        let data = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&data)?;
            f.sync_all()?;
        }
        if let Err(e) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        #[cfg(unix)]
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }

    /// 逐组：文件头令牌 == `pending.token` 则采用待定状态（含其 peer_seen），否则保留已提交状态。
    /// 之后清空 `pending`。`token_on_disk` 入参是组名。
    pub fn resolve_pending(&mut self, token_on_disk: impl Fn(&str) -> Option<String>) -> Resolved {
        let mut r = Resolved::default();
        let Some(p) = self.pending.take() else {
            return r;
        };
        for (name, st) in p.groups {
            if token_on_disk(&name).as_deref() == Some(p.token.as_str()) {
                self.groups.insert(name.clone(), st);
                r.adopted.push(name);
            } else {
                r.dropped.push(name);
            }
        }
        r
    }
}
