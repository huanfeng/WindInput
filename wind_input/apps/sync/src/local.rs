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
use wind_sync::scope::{ScopeFlags, in_scope};
use wind_sync::{LocalError, LocalStore};

use crate::link::CoreLink;
use crate::log::log;

/// 路径 → 我们最后写入内容的 hash（删除为空串）。
pub type Echo = Arc<Mutex<HashMap<String, String>>>;

pub const TMP_SUFFIX: &str = ".wind-sync.tmp";

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
    scope: ScopeFlags,
    link: Arc<dyn CoreLink>,
    echo: Echo,
    /// 扫描时读到的内容 hash（None = 不存在）。写入前据此确认文件没被别处改过。
    read_hashes: Mutex<HashMap<String, Option<String>>>,
    /// 置位后写入流程在各步之间收手（先释放已开的编辑会话）。
    cancel: Arc<AtomicBool>,
}

/// 用户目录里同步相关的顶层入口。
const ROOT_FILES: [&str; 2] = ["config.toml", "compat.toml"];
const ROOT_DIRS: [&str; 3] = ["schemas", "themes", "data"];

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

/// `data/` 下由 core 的外部编辑会话管理的文本文件。返回 `data.beginEdit` 要的路径（相对数据目录）。
fn edit_target(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("data/")?;
    let segs: Vec<&str> = rest.split('/').collect();
    let ok = match segs.as_slice() {
        ["phrases.dict.yaml"] | ["quick_format.toml"] | ["common_chars.yaml"] => true,
        ["charsets", f] => f.ends_with(".yaml"),
        [s, "user.dict.yaml"] | [s, "shadow.dict.yaml"] | [s, "dicts.toml"] => *s != "charsets",
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
    pub fn new(root: PathBuf, scope: ScopeFlags, link: Arc<dyn CoreLink>, echo: Echo) -> FsLocal {
        FsLocal {
            root,
            scope,
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

    fn all_files(&self) -> Vec<String> {
        let mut out = Vec::new();
        // 根目录：config.toml / compat.toml，以及它们的冲突副本（`config.conflict-…toml`）
        if let Ok(rd) = std::fs::read_dir(&self.root) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let ours = ROOT_FILES.iter().any(|f| {
                    let stem = f.split('.').next().unwrap_or(f);
                    name.starts_with(&format!("{stem}."))
                });
                if ours && e.file_type().is_ok_and(|t| t.is_file()) {
                    out.push(name);
                }
            }
        }
        for d in ROOT_DIRS {
            self.walk(&self.abs(d), d, &mut out);
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

    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
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
            .and_then(|()| std::fs::rename(&tmp, &path));
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }

    fn remove(&self, rel: &str) -> io::Result<()> {
        match std::fs::remove_file(self.abs(rel)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
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

    fn apply(
        &mut self,
        writes: &[(String, Vec<u8>)],
        deletes: &[String],
    ) -> Result<(), LocalError> {
        let all = writes.iter().map(|(p, _)| p).chain(deletes.iter());
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
            match self.write_atomic(p, bytes) {
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
        for (m, p) in calls {
            if let Err(e) = self.req(m, p) {
                log!("{m} 失败: {e}");
            }
        }
    }
}

impl LocalStore for FsLocal {
    fn list(&self) -> Result<Vec<String>, LocalError> {
        let mut v: Vec<String> = self
            .all_files()
            .into_iter()
            .filter(|p| in_scope(p, &self.scope))
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
        self.apply(files, &[])
    }

    fn delete(&mut self, paths: &[String]) -> Result<(), LocalError> {
        self.apply(&[], paths)
    }
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
mod tests {
    use super::*;
    use crate::testing::FakeCore;

    fn rig() -> (tempfile::TempDir, Arc<FakeCore>, FsLocal, Echo) {
        let d = tempfile::tempdir().unwrap();
        let core = Arc::new(FakeCore::default());
        let echo = Echo::default();
        let l = FsLocal::new(
            d.path().to_path_buf(),
            ScopeFlags::ALL,
            core.clone(),
            echo.clone(),
        );
        (d, core, l, echo)
    }

    fn put(d: &Path, rel: &str, c: &str) {
        let p = d.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, c).unwrap();
    }

    #[test]
    fn list_only_in_scope_and_skips_hidden() {
        let (d, _c, l, _) = rig();
        for (p, c) in [
            ("config.toml", "a"),
            ("compat.toml", "b"),
            ("meta.toml", "x"),
            ("schemas/pinyin.schema.toml", "s"),
            ("themes/t/theme.toml", "t"),
            ("data/pinyin/user.dict.yaml", "u"),
            ("data/pinyin/temp.wjl", "no"),
            ("data/.backup/x.yaml", "no"),
            ("data/pinyin/.user.dict.yaml.wind-sync.tmp", "no"),
            ("local/state/x", "no"),
        ] {
            put(d.path(), p, c);
        }
        assert_eq!(
            l.list().unwrap(),
            vec![
                "compat.toml",
                "config.toml",
                "data/pinyin/user.dict.yaml",
                "schemas/pinyin.schema.toml",
                "themes/t/theme.toml",
            ]
        );
        let mut off = FsLocal::new(
            d.path().to_path_buf(),
            ScopeFlags {
                dicts: false,
                themes: false,
                ..ScopeFlags::ALL
            },
            Arc::new(FakeCore::default()),
            Echo::default(),
        );
        assert_eq!(off.list().unwrap().len(), 3);
        assert!(off.read("../etc/passwd").is_err());
        assert_eq!(off.read("nope.toml").unwrap(), None);
        assert!(off.write_batch(&[("../x".into(), vec![])]).is_err());
    }

    #[test]
    fn write_batch_is_atomic_and_merges_reloads() {
        let (d, core, mut l, echo) = rig();
        put(d.path(), "data/pinyin/user.dict.yaml", "old");
        put(d.path(), "data/phrases.dict.yaml", "old");
        l.write_batch(&[
            ("config.toml".into(), b"a=1\n".to_vec()),
            ("data/pinyin/user.dict.yaml".into(), b"u".to_vec()),
            ("data/phrases.dict.yaml".into(), b"p".to_vec()),
            ("themes/t/theme.toml".into(), b"t".to_vec()),
            ("themes/t/bg.png".into(), b"i".to_vec()),
            ("schemas/pinyin.schema.toml".into(), b"s".to_vec()),
            ("schemas/pinyin/x.txt".into(), b"s".to_vec()),
            ("data/charsets/gbk.txt".into(), b"c".to_vec()),
        ])
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join("config.toml")).unwrap(),
            "a=1\n"
        );
        // 没有遗留临时文件
        for e in std::fs::read_dir(d.path()).unwrap().flatten() {
            assert!(!e.file_name().to_string_lossy().ends_with(TMP_SUFFIX));
        }
        assert_eq!(
            core.methods(),
            vec![
                "data.beginEdit pinyin/user.dict.yaml",
                "data.beginEdit phrases.dict.yaml",
                "data.endEdit pinyin/user.dict.yaml",
                "data.endEdit phrases.dict.yaml",
                "config.reload",
                "schema.reloadFiles",
                "theme.reloadFiles",
                "dict.reload",
            ]
        );
        let calls = core.calls.lock().unwrap();
        let theme = calls.iter().find(|c| c.0 == "theme.reloadFiles").unwrap();
        assert_eq!(theme.1, json!({"ids": ["t"]}));
        let schema = calls.iter().find(|c| c.0 == "schema.reloadFiles").unwrap();
        assert_eq!(schema.1, json!({}), "子目录资源被多方案共用，重载全部");
        assert_eq!(
            lock(&echo)["config.toml"],
            content_hash(b"a=1\n"),
            "写入后记 hash 供回声抑制"
        );
    }

    #[test]
    fn begin_edit_failure_writes_nothing_and_releases_sessions() {
        let (d, core, mut l, _) = rig();
        put(d.path(), "data/pinyin/user.dict.yaml", "old");
        put(d.path(), "data/wubi86/user.dict.yaml", "old");
        core.fail
            .lock()
            .unwrap()
            .push("data.beginEdit data/x".into());
        core.fail
            .lock()
            .unwrap()
            .push("data.beginEdit wubi86/user.dict.yaml".into());
        let r = l.write_batch(&[
            ("config.toml".into(), b"a".to_vec()),
            ("data/pinyin/user.dict.yaml".into(), b"u".to_vec()),
            ("data/wubi86/user.dict.yaml".into(), b"w".to_vec()),
        ]);
        assert!(r.unwrap_err().0.contains("拒绝开始编辑"));
        assert!(!d.path().join("config.toml").exists());
        assert_eq!(
            std::fs::read_to_string(d.path().join("data/pinyin/user.dict.yaml")).unwrap(),
            "old"
        );
        let m = core.methods();
        assert!(m.contains(&"data.endEdit pinyin/user.dict.yaml".to_string()));
        assert!(!m.contains(&"config.reload".to_string()));
    }

    #[test]
    fn delete_goes_through_edit_session_too() {
        let (d, core, mut l, echo) = rig();
        put(d.path(), "data/pinyin/user.dict.yaml", "u");
        put(d.path(), "compat.toml", "c");
        l.delete(&["data/pinyin/user.dict.yaml".into(), "compat.toml".into()])
            .unwrap();
        assert!(!d.path().join("compat.toml").exists());
        assert_eq!(
            core.methods(),
            vec![
                "data.beginEdit pinyin/user.dict.yaml",
                "data.endEdit pinyin/user.dict.yaml",
                "compat.reload",
            ]
        );
        assert_eq!(lock(&echo)["compat.toml"], "");
    }

    #[test]
    fn conflict_copies_are_found_even_out_of_scope() {
        let (d, _c, l, _) = rig();
        put(d.path(), "config.conflict-dev1-2026-11-24.toml", "x");
        put(d.path(), "themes/t/a.conflict-d-1.toml", "x");
        put(d.path(), "config.toml", "x");
        let set_age = |rel: &str, secs: u64| {
            let f = std::fs::File::options()
                .write(true)
                .open(d.path().join(rel))
                .unwrap();
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
                .unwrap();
        };
        set_age("config.conflict-dev1-2026-11-24.toml", 1000);
        set_age("themes/t/a.conflict-d-1.toml", 2000);
        // 新的在前
        assert_eq!(
            l.conflict_files(),
            vec![
                "themes/t/a.conflict-d-1.toml",
                "config.conflict-dev1-2026-11-24.toml",
            ]
        );
        // 用户删掉后下一次收集即消失
        std::fs::remove_file(d.path().join("themes/t/a.conflict-d-1.toml")).unwrap();
        assert_eq!(
            l.conflict_files(),
            vec!["config.conflict-dev1-2026-11-24.toml"]
        );
    }

    #[test]
    fn edit_targets() {
        assert_eq!(
            edit_target("data/pinyin/user.dict.yaml"),
            Some("pinyin/user.dict.yaml")
        );
        assert_eq!(edit_target("data/charsets/a.yaml"), Some("charsets/a.yaml"));
        assert_eq!(edit_target("data/charsets/a.txt"), None);
        assert_eq!(
            edit_target("data/pinyin/dicts/x.dict.yaml"),
            Some("pinyin/dicts/x.dict.yaml")
        );
        assert_eq!(edit_target("config.toml"), None);
    }

    const UD: &str = "data/pinyin/user.dict.yaml";
    const UD_EDIT: &str = "pinyin/user.dict.yaml";

    /// 扫描后、写入前，core 在 beginEdit 里把待处理学词折回了文件：不得以旧内容覆盖，
    /// 且下一轮重新扫描能把折回的内容纳入。
    #[test]
    fn content_folded_by_begin_edit_is_not_overwritten_and_next_round_sees_it() {
        let (d, core, mut l, _) = rig();
        put(d.path(), UD, "old");
        assert_eq!(l.read(UD).unwrap().unwrap(), b"old");
        let path = d.path().join(UD);
        *core.on_call.lock().unwrap() = Some(Box::new(move |m, _| {
            if m == "data.beginEdit" {
                std::fs::write(&path, "folded").unwrap();
            }
        }));
        let e = l
            .write_batch(&[(UD.into(), b"remote".to_vec())])
            .unwrap_err();
        assert!(e.0.starts_with(RETRY_TAG), "{e:?}");
        assert_eq!(
            std::fs::read_to_string(d.path().join(UD)).unwrap(),
            "folded"
        );
        assert!(core.methods().contains(&format!("data.endEdit {UD_EDIT}")));

        // 下一轮：重新扫描读到折回后的内容，写入放行
        *core.on_call.lock().unwrap() = None;
        let mut l2 = FsLocal::new(
            d.path().to_path_buf(),
            ScopeFlags::ALL,
            core.clone(),
            Echo::default(),
        );
        assert_eq!(l2.read(UD).unwrap().unwrap(), b"folded");
        l2.write_batch(&[(UD.into(), b"merged".to_vec())]).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join(UD)).unwrap(),
            "merged"
        );
    }

    #[test]
    fn new_local_file_skips_begin_edit_and_triggers_rescan() {
        let (d, core, mut l, _) = rig();
        // beginEdit 对不存在的文件会被拒：即使这样整批也要成功
        core.fail_on(&format!("data.beginEdit {UD_EDIT}"));
        l.write_batch(&[
            ("config.toml".into(), b"a=1\n".to_vec()),
            (UD.into(), b"u".to_vec()),
        ])
        .unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join(UD)).unwrap(), "u");
        assert_eq!(core.methods(), vec!["config.reload", "dict.reload"]);
    }

    #[test]
    fn user_opened_session_is_left_alone_and_batch_is_retried_later() {
        let (d, core, mut l, _) = rig();
        put(d.path(), UD, "mine");
        core.user_sessions.lock().unwrap().push(UD_EDIT.into());
        let e = l
            .write_batch(&[
                ("config.toml".into(), b"a".to_vec()),
                (UD.into(), b"remote".to_vec()),
            ])
            .unwrap_err();
        assert!(e.0.starts_with(RETRY_TAG) && e.0.contains(UD_EDIT), "{e:?}");
        assert!(
            core.methods().is_empty(),
            "不 begin、不 end：{:?}",
            core.methods()
        );
        assert_eq!(std::fs::read_to_string(d.path().join(UD)).unwrap(), "mine");
        assert!(!d.path().join("config.toml").exists());
    }

    #[test]
    fn begin_edit_timeout_also_tries_to_end_the_session() {
        let (d, core, mut l, _) = rig();
        put(d.path(), UD, "old");
        core.timeout
            .lock()
            .unwrap()
            .push(format!("data.beginEdit {UD_EDIT}"));
        assert!(l.write_batch(&[(UD.into(), b"x".to_vec())]).is_err());
        assert_eq!(
            core.methods(),
            vec![
                format!("data.beginEdit {UD_EDIT}"),
                format!("data.endEdit {UD_EDIT}")
            ]
        );
    }

    #[test]
    fn end_edit_is_retried_once() {
        let (d, core, mut l, _) = rig();
        put(d.path(), UD, "old");
        core.fail_once
            .lock()
            .unwrap()
            .push(format!("data.endEdit {UD_EDIT}"));
        l.write_batch(&[(UD.into(), b"x".to_vec())]).unwrap();
        let ends = core
            .methods()
            .iter()
            .filter(|m| m.starts_with("data.endEdit"))
            .count();
        assert_eq!(ends, 2);
    }

    #[test]
    fn cancel_after_begin_ends_session_and_writes_nothing() {
        let (d, core, l, _) = rig();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut l = l.with_cancel(cancel.clone());
        put(d.path(), UD, "old");
        *core.on_call.lock().unwrap() = Some(Box::new(move |m, _| {
            if m == "data.beginEdit" {
                cancel.store(true, Ordering::SeqCst);
            }
        }));
        assert!(l.write_batch(&[(UD.into(), b"x".to_vec())]).is_err());
        assert_eq!(std::fs::read_to_string(d.path().join(UD)).unwrap(), "old");
        assert!(core.methods().contains(&format!("data.endEdit {UD_EDIT}")));
    }

    #[test]
    fn echo_keeps_only_latest_hash_per_path() {
        let (_d, _core, mut l, echo) = rig();
        l.write_batch(&[("config.toml".into(), b"1".to_vec())])
            .unwrap();
        l.write_batch(&[("config.toml".into(), b"2".to_vec())])
            .unwrap();
        let e = lock(&echo);
        assert_eq!(e.len(), 1);
        assert_eq!(e["config.toml"], content_hash(b"2"));
    }

    #[test]
    fn temp_names_are_unique_per_write() {
        let (d, _core, mut l, _) = rig();
        // 旧实现用固定名：残留的同名临时文件会被覆盖/误删；新实现不碰它
        put(d.path(), ".config.toml.wind-sync.tmp", "foreign");
        l.write_batch(&[("config.toml".into(), b"1".to_vec())])
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join(".config.toml.wind-sync.tmp")).unwrap(),
            "foreign"
        );
    }
}
