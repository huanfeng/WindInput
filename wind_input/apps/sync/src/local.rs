//! 用户目录上的 [`LocalStore`]：范围内文件的列举 / 读，以及带编辑会话与重载的写入。
//!
//! 写一批文件的顺序（设计 §7 第 4 步）：
//! 1. `data/` 下可编辑的文本词库先 `data.beginEdit`（core 折回待处理操作并暂停写回）。
//!    任何一个失败（如只读模式）→ 释放已开的会话，本批一个文件都不写，报错。
//! 2. 同目录临时文件 + rename 原子写入 / 删除。
//! 3. `data.endEdit`（core 按文件重载词库）；config / compat / 方案 / 主题各一次重载请求。
//!    重载失败只记日志：文件已落盘，下一轮看到内容一致不会再写，重试不会补上重载，报错只会空转。
//!
//! 写入后记下文件 hash（回声抑制）：监听事件里 hash 相同的是我们自己写的，不触发同步。

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};
use wind_sync::crypto::content_hash;
use wind_sync::scope::in_scope;
use wind_sync::{LocalError, LocalStore};

use crate::link::CoreLink;
use crate::log::log;

/// 路径 → 我们最后写入内容的 hash（删除为空串）。
pub type Echo = Arc<Mutex<HashMap<String, String>>>;

pub const TMP_SUFFIX: &str = ".wind-sync.tmp";

/// Windows 上替换扩展文件时先改名让路的旧文件后缀（点开头，不进同步范围）；目标缺失时会被还原。
pub const OLD_SUFFIX: &str = ".wind-sync.old";

/// 删除扩展文件时改名让路的后缀：永不还原，下次清理时删掉。
pub const DEL_SUFFIX: &str = ".wind-sync.del";

/// 可重试错误的前缀：本批没有写任何东西，稍后重跑一轮即可（不计入失败退避）。
pub const RETRY_TAG: &str = "[稍后重试]";

fn retry_err(msg: impl std::fmt::Display) -> LocalError {
    LocalError(format!("{RETRY_TAG}{msg}"))
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct FsLocal {
    root: PathBuf,
    link: Arc<dyn CoreLink>,
    echo: Echo,
    /// 扫描时读到的内容 hash（None = 不存在）。写入前据此确认文件没被别处改过。
    read_hashes: Mutex<HashMap<String, Option<String>>>,
    /// 置位后写入流程在各步之间收手（先释放已开的编辑会话）。
    cancel: Arc<AtomicBool>,
}

/// 不含 `..`、绝对路径、反斜杠、空段：写入前的最后一道闸，冲突副本等范围外路径也要过它。
fn safe_rel(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

fn is_extension_file(rel: &str) -> bool {
    rel.starts_with("extensions/")
}

fn split3(p: &str) -> Option<(&str, &str, &str)> {
    let (a, rest) = p.split_once('/')?;
    let (b, c) = rest.split_once('/')?;
    (!b.is_empty()).then_some((a, b, c))
}

/// `data/` 下由 core 的外部编辑会话管理的文本文件。返回 `data.beginEdit` 要的路径（相对数据目录）。
fn edit_target(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("data/")?;
    let segs: Vec<&str> = rest.split('/').collect();
    let ok = match segs.as_slice() {
        ["phrases.dict.yaml"]
        | ["quick_format.toml"]
        | ["common_chars.yaml"]
        | ["completion.table.yaml"] => true,
        ["charsets", f] => f.ends_with(".yaml"),
        [s, "user.dict.yaml"]
        | [s, "shadow.dict.yaml"]
        | [s, "dicts.toml"]
        | [s, "assoc.table.yaml"] => *s != "charsets",
        [s, "dicts", f] => *s != "charsets" && f.ends_with(".dict.yaml"),
        _ => false,
    };
    ok.then_some(rest)
}

/// 一批变更需要的重载（每类合并成一次）。
#[derive(Debug, Default, PartialEq, Eq)]
struct Reloads {
    config: bool,
    compat: bool,
    /// None = 无法定位到具体方案，重载全部
    schemas: Option<BTreeSet<String>>,
    schemas_all: bool,
    themes: Option<BTreeSet<String>>,
    themes_all: bool,
    /// data/ 下不走编辑会话的文件（字表等）：整体重扫
    dict_rescan: bool,
    softkeyboard: bool,
}

fn plain_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Reloads {
    fn note(&mut self, path: &str) {
        let segs: Vec<&str> = path.split('/').collect();
        match segs.as_slice() {
            ["config.toml"] => self.config = true,
            ["compat.toml"] => self.compat = true,
            // 方案覆盖：精确到方案，id 不规整则全部重载
            ["schema_overrides", f] => match f.strip_suffix(".toml") {
                Some(id) if plain_id(id) => {
                    self.schemas
                        .get_or_insert_with(BTreeSet::new)
                        .insert(id.to_string());
                }
                _ => self.schemas_all = true,
            },
            // emoji 与误读表由 config.reload 一并重读
            ["emoji", ..] | ["data", "pinyin_misread.yaml"] => self.config = true,
            ["system.softkeyboard.toml"] => self.softkeyboard = true,
            ["schemas", rest @ ..] => {
                // `<id>.schema.<ext>` 精确到方案；方案资源子目录里的文件被多个方案共用，重载全部
                match rest {
                    [f] if f.contains(".schema.") => {
                        let id = f.split(".schema.").next().unwrap_or_default();
                        if plain_id(id) {
                            self.schemas
                                .get_or_insert_with(BTreeSet::new)
                                .insert(id.to_string());
                        } else {
                            self.schemas_all = true;
                        }
                    }
                    _ => self.schemas_all = true,
                }
            }
            ["themes", slug, _, ..] if plain_id(slug) => {
                self.themes
                    .get_or_insert_with(BTreeSet::new)
                    .insert(slug.to_string());
            }
            ["themes", ..] => self.themes_all = true,
            ["data", ..] if edit_target(path).is_none() => self.dict_rescan = true,
            _ => {}
        }
    }
}

impl FsLocal {
    pub fn new(root: PathBuf, link: Arc<dyn CoreLink>, echo: Echo) -> FsLocal {
        FsLocal {
            root,
            link,
            echo,
            read_hashes: Mutex::new(HashMap::new()),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> FsLocal {
        self.cancel = cancel;
        self
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn file_hash(&self, rel: &str) -> Option<String> {
        std::fs::read(self.abs(rel)).ok().map(|b| content_hash(&b))
    }

    fn abs(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, s| p.join(s))
    }

    fn walk(&self, dir: &Path, rel: &str, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // 隐藏项（含我们的临时文件、data/.backup）不同步
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let child = format!("{rel}/{name}");
            if ft.is_dir() {
                self.walk(&e.path(), &child, out);
            } else if ft.is_file() {
                out.push(child);
            }
        }
    }

    /// 用户目录下全部文件（跳过以 `.` 开头的项）；是否同步由 `in_scope` 再判。
    fn all_files(&self) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                self.walk(&e.path(), &name, &mut out);
            } else if ft.is_file() {
                out.push(name);
            }
        }
        out
    }

    /// 冲突副本（含范围外、只落本地的），按修改时间新→旧（同刻按路径）。
    /// 设置端据此提示用户去处理。
    pub fn conflict_files(&self) -> Vec<String> {
        let mut v: Vec<(std::time::SystemTime, String)> = self
            .all_files()
            .into_iter()
            .filter(|p| {
                p.rsplit('/')
                    .next()
                    .is_some_and(|f| f.contains(".conflict-"))
            })
            .map(|p| {
                let t = std::fs::metadata(self.abs(&p))
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (t, p)
            })
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        v.into_iter().map(|(_, p)| p).collect()
    }

    /// `exec`：落位前给临时文件加上可执行位（Unix）。覆盖时原文件的权限也在落位前拷过来。
    fn write_atomic(&self, rel: &str, bytes: &[u8], exec: bool) -> io::Result<()> {
        let path = self.abs(rel);
        let dir = path.parent().ok_or_else(|| io::Error::other("无父目录"))?;
        std::fs::create_dir_all(dir)?;
        let tmp = crate::fsutil::tmp_sibling(&path, TMP_SUFFIX);
        // 先落盘再改名：断电后不会留下「已改名但内容为空」的文件。
        let r = std::fs::File::create(&tmp)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(bytes)?;
                f.sync_all()
            })
            .and_then(|()| copy_mode(&path, &tmp, exec))
            .and_then(|()| {
                // 运行中的扩展 exe / DLL 在 Windows 上不能被覆盖，但可以改名：先让路再落位。
                // Unix 上 rename 本身就能原子替换，不让路，免得崩溃时只剩 .old。
                let aside = if cfg!(windows) {
                    self.move_aside(rel)
                } else {
                    None
                };
                let r = std::fs::rename(&tmp, &path);
                if r.is_err()
                    && let Some(a) = aside
                {
                    let _ = std::fs::rename(a, &path);
                }
                r
            });
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }

    fn remove(&self, rel: &str) -> io::Result<()> {
        if is_extension_file(rel) {
            let path = self.abs(rel);
            // 同名的残留 .old 一并作废：否则目标删掉后会被 sweep 当成崩溃现场还原
            for old in aside_siblings(&path, OLD_SUFFIX) {
                if std::fs::remove_file(&old).is_err() {
                    let _ = std::fs::rename(&old, crate::fsutil::tmp_sibling(&path, DEL_SUFFIX));
                }
            }
            return match std::fs::rename(&path, crate::fsutil::tmp_sibling(&path, DEL_SUFFIX)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        match std::fs::remove_file(self.abs(rel)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// 扩展文件已存在时改名为同目录的 `.old` 兄弟；返回改名后的路径。其余文件不动。
    fn move_aside(&self, rel: &str) -> Option<PathBuf> {
        if !is_extension_file(rel) {
            return None;
        }
        let path = self.abs(rel);
        let old = crate::fsutil::tmp_sibling(&path, OLD_SUFFIX);
        std::fs::rename(&path, &old).ok().map(|()| old)
    }

    /// 整理 `extensions/` 下的让路文件：`.del` 删掉；`.old` 的目标缺失（两次改名之间崩溃、
    /// 落位与还原都失败）则把最新的一个改回原名，目标在则删掉。仍被占用的删不掉，留待下次；
    /// 同名 `.old` 还在时 `.del` 留作删除标记，免得下次把 `.old` 还原。
    fn sweep_aside(&self) {
        fn walk(dir: &Path) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            // 原文件名 → [(修改时间, 让路文件)]
            let mut olds: HashMap<String, Vec<(std::time::SystemTime, PathBuf)>> = HashMap::new();
            // 本目录里被删除让路过的原文件名 → .del 文件：同名 .old 不再还原
            let mut deleted: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let name = e.file_name().to_string_lossy().into_owned();
                if ft.is_dir() {
                    walk(&e.path());
                } else if let Some(orig) = aside_target(&name, DEL_SUFFIX) {
                    deleted.entry(orig.to_string()).or_default().push(e.path());
                } else if let Some(orig) = aside_target(&name, OLD_SUFFIX) {
                    let t = e
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    olds.entry(orig.to_string())
                        .or_default()
                        .push((t, e.path()));
                }
            }
            for (orig, mut v) in olds {
                let target = dir.join(&orig);
                v.sort_by_key(|e| std::cmp::Reverse(e.0));
                // 已知局限（仅 Windows）：用户手动删掉的扩展文件若留有被占用的 .old，会被还原
                if std::fs::symlink_metadata(&target).is_err() && !deleted.contains_key(&orig) {
                    let (_, newest) = v.remove(0);
                    match std::fs::rename(&newest, &target) {
                        Ok(()) => log!("还原让路的扩展文件 {}", target.display()),
                        Err(e) => log!("还原 {} 失败: {e}", target.display()),
                    }
                }
                for (_, p) in v {
                    let _ = std::fs::remove_file(p);
                }
            }
            for (orig, dels) in deleted {
                let old_left = aside_siblings(&dir.join(&orig), OLD_SUFFIX);
                if old_left.is_empty() {
                    for p in dels {
                        let _ = std::fs::remove_file(p);
                    }
                }
            }
        }
        walk(&self.abs("extensions"));
    }

    fn req(&self, method: &str, params: Value) -> Result<Value, String> {
        self.link.request(method, params)
    }

    /// 用户自己开着编辑会话的文件（`data.editSessions`，相对数据目录）。
    fn user_sessions(&self) -> Result<Vec<String>, LocalError> {
        let v = self
            .req("data.editSessions", json!({}))
            .map_err(|e| LocalError(format!("查询编辑会话失败: {e}")))?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("file").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// 本批涉及的扩展 id（`extensions/<id>/…`）。
    fn touched_extensions(writes: &[(String, Vec<u8>)], deletes: &[String]) -> Vec<String> {
        let mut ids = BTreeSet::new();
        for p in writes.iter().map(|(p, _)| p).chain(deletes.iter()) {
            if let Some(("extensions", id, _)) = split3(p) {
                ids.insert(id.to_string());
            }
        }
        ids.into_iter().collect()
    }

    /// 运行中的扩展 exe 在 Windows 上无法覆盖：涉及扩展文件的批次先让 core 暂停它们，
    /// 写完（无论成败）再恢复。写入与删除在同一批里，一轮只暂停 / 恢复一次。
    fn apply_batch(
        &mut self,
        writes: &[(String, Vec<u8>)],
        exec: &[String],
        deletes: &[String],
    ) -> Result<(), LocalError> {
        self.sweep_aside();
        let ids = Self::touched_extensions(writes, deletes);
        if ids.is_empty() {
            return self.apply_inner(writes, exec, deletes);
        }
        let params = json!({ "ids": ids });
        if let Err(e) = self.req("extensions.suspend", params.clone()) {
            // 超时的请求 core 可能仍会处理：补一次恢复
            if e == crate::link::TIMEOUT_MSG {
                let _ = self.req("extensions.resume", params);
            }
            return Err(LocalError(format!("核心拒绝暂停扩展: {e}")));
        }
        let r = self.apply_inner(writes, exec, deletes);
        let resumed = self
            .req("extensions.resume", params.clone())
            .or_else(|_| self.req("extensions.resume", params));
        if let Err(e) = resumed {
            log!("extensions.resume 失败: {e}");
        }
        self.sweep_aside();
        r
    }

    fn apply_inner(
        &mut self,
        writes: &[(String, Vec<u8>)],
        exec: &[String],
        deletes: &[String],
    ) -> Result<(), LocalError> {
        let all = writes
            .iter()
            .map(|(p, _)| p)
            .chain(exec.iter())
            .chain(deletes.iter());
        if let Some(p) = all.into_iter().find(|p| !safe_rel(p)) {
            return Err(LocalError(format!("拒绝写入不安全的路径: {p}")));
        }
        let touched: Vec<&str> = writes
            .iter()
            .map(|(p, _)| p.as_str())
            .chain(deletes.iter().map(String::as_str))
            .collect();

        // 要开会话的文件：本地已存在的可编辑词库。本地没有的直接写（core 没有可暂停写回的东西，
        // 对不存在的文件 beginEdit 反而会被拒），写完靠 dict.reload 纳入。
        let mut targets: Vec<&str> = Vec::new();
        for p in touched.iter().filter(|p| self.abs(p).is_file()) {
            if let Some(t) = edit_target(p)
                && !targets.contains(&t)
            {
                targets.push(t);
            }
        }

        if self.cancelled() {
            return Err(LocalError("同步已取消".into()));
        }
        // 用户自己开着的会话不碰：begin 会刷新它、end 会替他关掉。整批放弃，稍后重试
        // （只跳过其中一个文件会让引擎以为它已落地，下一轮把旧内容当本地修改传上去）。
        if !targets.is_empty() {
            let open = self.user_sessions()?;
            let busy: Vec<&str> = targets
                .iter()
                .copied()
                .filter(|t| open.iter().any(|o| o == t))
                .collect();
            if !busy.is_empty() {
                return Err(retry_err(format!(
                    "{} 正在被编辑，本轮暂不同步",
                    busy.join("、")
                )));
            }
        }

        // 1. 开编辑会话
        let mut begun: Vec<&str> = Vec::new();
        for &p in &targets {
            if let Err(e) = self.req("data.beginEdit", json!({ "path": p })) {
                // 超时的请求 core 可能仍会处理：也释放这一条
                if e == crate::link::TIMEOUT_MSG {
                    begun.push(p);
                }
                self.end_all(&begun);
                return Err(LocalError(format!("核心拒绝开始编辑 {p}: {e}")));
            }
            begun.push(p);
        }

        // 1.5 beginEdit 会把待处理的学词折回文件：扫描时读到的内容可能已过期，
        // 以它为基础写回会丢掉刚折回的词。变了就整批放弃，下一轮重新扫描。
        let stale: Vec<&str> = {
            let seen = lock(&self.read_hashes);
            touched
                .iter()
                .copied()
                .filter(|p| p.starts_with("data/"))
                .filter(|p| seen.get(*p).is_some_and(|h| *h != self.file_hash(p)))
                .collect()
        };
        if !stale.is_empty() {
            self.end_all(&begun);
            return Err(retry_err(format!(
                "{} 在扫描后被核心改动，重新扫描",
                stale.join("、")
            )));
        }
        if self.cancelled() {
            self.end_all(&begun);
            return Err(LocalError("同步已取消".into()));
        }

        // 2. 原子写入 / 删除；出错即停，但已开的会话照常收尾
        let mut reloads = Reloads::default();
        let mut failure: Option<LocalError> = None;
        for (p, bytes) in writes {
            if self.cancelled() {
                failure = Some(LocalError("同步已取消".into()));
                break;
            }
            match self.write_atomic(p, bytes, exec.contains(p)) {
                Ok(()) => {
                    lock(&self.echo).insert(p.clone(), content_hash(bytes));
                    reloads.note(p);
                    // 新建的词库文件没开会话：整体重扫纳入
                    if edit_target(p).is_some_and(|t| !begun.contains(&t)) {
                        reloads.dict_rescan = true;
                    }
                }
                Err(e) => {
                    failure = Some(LocalError(format!("写入 {p} 失败: {e}")));
                    break;
                }
            }
        }
        if failure.is_none() {
            for p in deletes {
                if self.cancelled() {
                    failure = Some(LocalError("同步已取消".into()));
                    break;
                }
                match self.remove(p) {
                    Ok(()) => {
                        lock(&self.echo).insert(p.clone(), String::new());
                        reloads.note(p);
                    }
                    Err(e) => {
                        failure = Some(LocalError(format!("删除 {p} 失败: {e}")));
                        break;
                    }
                }
            }
        }

        // 3. 收尾与重载
        self.end_all(&begun);
        self.reload(&reloads);
        failure.map_or(Ok(()), Err)
    }

    fn end_all(&self, begun: &[&str]) {
        for p in begun {
            // 失败重试一次：会话留着会让 core 一直暂停该文件的写回
            let r = self
                .req("data.endEdit", json!({ "path": p }))
                .or_else(|_| self.req("data.endEdit", json!({ "path": p })));
            if let Err(e) = r {
                log!("data.endEdit {p} 失败: {e}");
            }
        }
    }

    fn reload(&self, r: &Reloads) {
        let ids = |s: &BTreeSet<String>| json!({ "ids": s.iter().collect::<Vec<_>>() });
        let mut calls: Vec<(&str, Value)> = Vec::new();
        if r.config {
            calls.push(("config.reload", json!({})));
        }
        if r.compat {
            calls.push(("compat.reload", json!({})));
        }
        if r.schemas_all {
            calls.push(("schema.reloadFiles", json!({})));
        } else if let Some(s) = &r.schemas {
            calls.push(("schema.reloadFiles", ids(s)));
        }
        if r.themes_all {
            calls.push(("theme.reloadFiles", json!({})));
        } else if let Some(s) = &r.themes {
            calls.push(("theme.reloadFiles", ids(s)));
        }
        if r.dict_rescan {
            calls.push(("dict.reload", json!({})));
        }
        if r.softkeyboard {
            calls.push(("softkeyboard.reload", json!({})));
        }
        for (m, p) in calls {
            if let Err(e) = self.req(m, p) {
                log!("{m} 失败: {e}");
            }
        }
    }
}

impl LocalStore for FsLocal {
    fn list(&self) -> Result<Vec<String>, LocalError> {
        // 先还原崩溃留下的 .old：否则目标缺失会被判成本地删除并推给全组
        self.sweep_aside();
        let mut v: Vec<String> = self
            .all_files()
            .into_iter()
            .filter(|p| in_scope(p))
            .collect();
        v.sort();
        Ok(v)
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, LocalError> {
        if !safe_rel(path) {
            return Err(LocalError(format!("拒绝读取不安全的路径: {path}")));
        }
        let r = match std::fs::read(self.abs(path)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            // 路径是目录等：当作不存在
            Err(_) if self.abs(path).is_dir() => Ok(None),
            Err(e) => Err(LocalError(format!("读取 {path} 失败: {e}"))),
        };
        if let Ok(c) = &r {
            lock(&self.read_hashes).insert(path.to_string(), c.as_deref().map(content_hash));
        }
        r
    }

    fn write_batch(&mut self, files: &[(String, Vec<u8>)]) -> Result<(), LocalError> {
        self.apply_batch(files, &[], &[])
    }

    fn delete(&mut self, paths: &[String]) -> Result<(), LocalError> {
        self.apply_batch(&[], &[], paths)
    }

    fn apply(
        &mut self,
        writes: &[(String, Vec<u8>)],
        exec: &[String],
        deletes: &[String],
    ) -> Result<(), LocalError> {
        self.apply_batch(writes, exec, deletes)
    }

    fn size(&self, path: &str) -> Result<Option<u64>, LocalError> {
        if !safe_rel(path) {
            return Err(LocalError(format!("拒绝读取不安全的路径: {path}")));
        }
        match std::fs::metadata(self.abs(path)) {
            Ok(m) => Ok(m.is_file().then_some(m.len())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(LocalError(format!("读取 {path} 大小失败: {e}"))),
        }
    }

    #[cfg(unix)]
    fn is_exec(&self, path: &str) -> Result<bool, LocalError> {
        use std::os::unix::fs::PermissionsExt;
        if !safe_rel(path) {
            return Err(LocalError(format!("拒绝读取不安全的路径: {path}")));
        }
        Ok(std::fs::metadata(self.abs(path))
            .map(|m| m.is_file() && m.permissions().mode() & 0o100 != 0)
            .unwrap_or(false))
    }

    #[cfg(unix)]
    fn set_exec(&mut self, paths: &[String]) -> Result<(), LocalError> {
        use std::os::unix::fs::PermissionsExt;
        for p in paths {
            if !safe_rel(p) {
                return Err(LocalError(format!("拒绝写入不安全的路径: {p}")));
            }
            let abs = self.abs(p);
            let mode = std::fs::metadata(&abs)
                .map_err(|e| LocalError(format!("读取 {p} 权限失败: {e}")))?
                .permissions()
                .mode();
            std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(mode | 0o100))
                .map_err(|e| LocalError(format!("设置 {p} 可执行位失败: {e}")))?;
        }
        Ok(())
    }
}

/// 让路文件名 `.<原名>.<pid>-<12 位十六进制><suffix>`（见 `fsutil::tmp_sibling`）对应的原名。
fn aside_target<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let (orig, tag) = name
        .strip_prefix('.')?
        .strip_suffix(suffix)?
        .rsplit_once('.')?;
    let (pid, hex) = tag.split_once('-')?;
    let ok = !orig.is_empty()
        && !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && hex.len() == 12
        && hex.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then_some(orig)
}

/// `path` 同目录下、以它为原名的让路文件。
fn aside_siblings(path: &Path, suffix: &str) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let name = name.to_string_lossy();
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| aside_target(&e.file_name().to_string_lossy(), suffix) == Some(&*name))
        .map(|e| e.path())
        .collect()
}

/// 落位前把目标原有的权限拷给临时文件，`exec` 时再加 owner 可执行位（Unix；其余平台无事可做）。
#[cfg(unix)]
fn copy_mode(target: &Path, tmp: &Path, exec: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match std::fs::metadata(target) {
        Ok(m) if m.is_file() => m.permissions().mode(),
        _ => std::fs::metadata(tmp)?.permissions().mode(),
    };
    let want = if exec { mode | 0o100 } else { mode };
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(want))
}

#[cfg(not(unix))]
fn copy_mode(_target: &Path, _tmp: &Path, _exec: bool) -> io::Result<()> {
    Ok(())
}

/// `p` 相对 `root` 的路径，以 `/` 分隔；不在 `root` 下或含 `..` 等返回 None。
pub fn rel_of(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
