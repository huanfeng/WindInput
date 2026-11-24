//! 外部编辑会话（设计 `user-dict-libraries-and-external-edit.md` §4.2）：用户在外部编辑器里改
//! `data/` 下的文件期间，用户词暂停写回、小表挡住界面修改；结束时按文件同步。

use crate::store::Store;
use crate::user_words::{TextPath, classify_text_path};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tracing::{info, warn};
use wind_userdata::record::Target;

/// 会话租约：这么久既没有再次打开、也没有保存文件，就按「完成并加载」处理（设置进程退出、
/// 用户忘了点）。
pub const EDIT_LEASE: Duration = Duration::from_secs(2 * 3600);

/// 租约到期自动结束后的回调，参数同 [`EditResult::reloaded`]。
pub type ChangedHook = std::sync::Arc<dyn Fn(&[String]) + Send + Sync>;

/// 可编辑的文件类别（§4.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Words,
    Shadow,
    Phrases,
    Quick,
    Common,
    Charsets,
    UserDicts,
    /// 临时词（`<数据方案>/temp.dict.yaml`）。
    Temp,
    /// 词频（`<数据方案>/freq.table.yaml`）。
    Freq,
}

impl EditKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EditKind::Words => "words",
            EditKind::Shadow => "shadow",
            EditKind::Phrases => "phrases",
            EditKind::Quick => "quick_format",
            EditKind::Common => "common_chars",
            EditKind::Charsets => "charsets",
            EditKind::UserDicts => "user_dicts",
            EditKind::Temp => "temp",
            EditKind::Freq => "freq",
        }
    }

    fn of(tp: &TextPath) -> Option<EditKind> {
        match tp {
            TextPath::Words(_) => Some(EditKind::Words),
            TextPath::Shadow(_) => Some(EditKind::Shadow),
            TextPath::Phrases => Some(EditKind::Phrases),
            TextPath::Quick => Some(EditKind::Quick),
            TextPath::Common => Some(EditKind::Common),
            TextPath::Charsets(_) => Some(EditKind::Charsets),
            TextPath::UserDicts(_) => Some(EditKind::UserDicts),
            TextPath::Learned(Target::Temp, _) => Some(EditKind::Temp),
            TextPath::Learned(Target::Freq, _) => Some(EditKind::Freq),
            TextPath::Learned(..) | TextPath::Completion | TextPath::Stats | TextPath::Misread => {
                None
            }
        }
    }
}

/// 进行中的会话（`Store::edit_sessions` 的值）。
#[derive(Clone)]
pub(crate) struct EditEntry {
    /// 开始 / 重复 begin 的时刻。
    pub(crate) active: SystemTime,
    /// 每次 begin（含重复）换新：end 收尾时据此判断期间有没有人重新开始。
    generation: u64,
    /// 分库 / 开关文件：开始时的内容指纹，end 据此判断变没变。
    fp: Option<String>,
    /// 开始时解析出的类别：租约到期时 end 失败，凭它恢复写回。
    tp: TextPath,
}

static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug)]
pub struct EditSession {
    /// 绝对路径（交给系统默认程序打开）。
    pub path: PathBuf,
    /// 相对数据目录、以 `/` 分隔。
    pub file: String,
    pub kind: EditKind,
}

#[derive(Debug)]
pub struct EditResult {
    /// 该文件的绝对路径。
    pub path: PathBuf,
    /// 文件内容与上次载入的不同。
    pub changed: bool,
    /// 同 `dict.reload` 的返回：变了的项（调用方据此重建内存镜像）。
    pub reloaded: Vec<String>,
    /// 该文件当前的解析问题（坏行 / 隔离）。
    pub issues: Vec<crate::issues::FileIssue>,
}

impl Store {
    /// 相对数据目录的路径，`/` 分隔。
    fn data_rel(&self, p: &Path) -> String {
        p.strip_prefix(self.user_text_dir())
            .unwrap_or(p)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// 相对数据目录或绝对路径 → 绝对路径（不碰文件系统）。
    fn data_abs(&self, p: &str) -> PathBuf {
        let path = Path::new(p);
        if path.is_relative() {
            self.user_text_dir().join(path)
        } else {
            path.to_path_buf()
        }
    }

    /// 校验并规范成该类文件的标准路径；程序维护的文件拒绝。
    fn resolve_edit_path(&self, p: &str) -> anyhow::Result<(PathBuf, TextPath)> {
        let abs = self.data_abs(p);
        let tp = classify_text_path(self.user_text_dir(), &abs.to_string_lossy(), cfg!(windows))?;
        let path = match &tp {
            TextPath::Words(s) => {
                wind_userdata::engine::check_schema_id(s)?;
                self.user_text_path(s)
            }
            TextPath::Shadow(s) => {
                wind_userdata::engine::check_schema_id(s)?;
                self.shadow_path(s)
            }
            TextPath::Phrases => self.phrases_path(),
            TextPath::Quick => self.quick_format_path(),
            TextPath::Common => self.common_chars_path(),
            TextPath::Charsets(k) => self.charset_path(k)?,
            TextPath::UserDicts(s) => {
                wind_userdata::engine::check_schema_id(s)?;
                let root = self.user_text_dir();
                let file = abs
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match crate::user_dicts::lib_name(&file, cfg!(windows)) {
                    Some(name) if !file.eq_ignore_ascii_case(crate::user_dicts::META_FILE) => {
                        crate::user_dicts::lib_path(root, s, name)
                    }
                    _ => crate::user_dicts::meta_path(root, s),
                }
            }
            TextPath::Learned(t @ (Target::Temp | Target::Freq), s) => {
                wind_userdata::engine::check_schema_id(s)?;
                self.learned_path(*t, s)
            }
            TextPath::Learned(..) | TextPath::Completion | TextPath::Stats => {
                anyhow::bail!("{} 由程序自动维护，不提供编辑", self.data_rel(&abs))
            }
            TextPath::Misread => {
                anyhow::bail!("{} 直接编辑即可，应用设置时生效", self.data_rel(&abs))
            }
        };
        Ok((path, tp))
    }

    /// 开始外部编辑：用户词先折回一次再暂停写回；小表确保文件存在。同一文件重复调用 = 同一会话
    /// （刷新时刻）。只读模式拒绝。
    pub fn begin_edit(&self, p: &str) -> anyhow::Result<EditSession> {
        let ud = self.runtime().data();
        if ud.readonly() {
            anyhow::bail!("只读模式：本次运行不保存修改，不能编辑数据文件");
        }
        let (path, tp) = self.resolve_edit_path(p)?;
        let file = self.data_rel(&path);
        let kind = EditKind::of(&tp).expect("resolve_edit_path 只放行可编辑的文件");
        let mut sessions = self.edit_sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = sessions.get_mut(&path) {
            e.active = SystemTime::now();
            e.generation = next_generation();
            return Ok(EditSession { path, file, kind });
        }
        match &tp {
            TextPath::Words(s) => self.prepare_words_edit(s)?,
            TextPath::Shadow(s) => {
                let t = self.shadow_table(s)?;
                if !t.path().is_file() {
                    t.replace_all(&[])?;
                }
            }
            TextPath::Phrases => {
                let t = self.phrase_table();
                if !t.path().is_file() {
                    t.replace_all(&[])?;
                }
            }
            TextPath::Quick => {
                if !path.is_file() {
                    let empty = crate::quick_format::render_quick(&Default::default())?;
                    self.quick_doc().replace(Some(&empty))?;
                }
            }
            TextPath::Common => {
                if !path.is_file() {
                    let empty = crate::common_chars::render_common(&Default::default());
                    self.common_doc().replace(Some(&empty))?;
                }
            }
            TextPath::Charsets(_) => {
                if !path.is_file() {
                    anyhow::bail!("{file} 不存在：这个字符分类还没有个人调整");
                }
            }
            TextPath::UserDicts(_) => {
                if path.file_name().and_then(|n| n.to_str()) == Some(crate::user_dicts::META_FILE) {
                    if !path.is_file() {
                        let empty = format!("{}\n", wind_userdata::text::banner::USER_DICTS_META);
                        wind_userdata::text::fold::commit_text(
                            &path,
                            empty.as_bytes(),
                            Some(&wind_userdata::text::fold::fingerprint(None)),
                        )?;
                    }
                } else if !path.is_file() {
                    anyhow::bail!("{file} 不存在");
                }
            }
            TextPath::Learned(t, s) => self.prepare_learned_edit(*t, s)?,
            TextPath::Completion | TextPath::Stats | TextPath::Misread => {}
        }
        sessions.insert(
            path.clone(),
            EditEntry {
                active: SystemTime::now(),
                generation: next_generation(),
                fp: matches!(tp, TextPath::UserDicts(_))
                    .then(|| crate::user_dicts::file_fingerprint(&path)),
                tp: tp.clone(),
            },
        );
        info!("外部编辑开始：{file}");
        Ok(EditSession { path, file, kind })
    }

    /// 用户词：折回待处理操作（文件里有全部数据），没有文件就按空表新建，再暂停写回。
    /// 折回失败（文件坏了 / 读不了）不拦：用户正是要去修它。
    fn prepare_words_edit(&self, schema: &str) -> anyhow::Result<()> {
        self.ensure_user_text(schema);
        let ud = self.runtime().data();
        if ud.pending(Target::User, schema) > 0
            && let Err(e) = ud.fold_text(Target::User, schema)
        {
            warn!("外部编辑前折回 {schema} 用户词失败：{e:#}");
        }
        if !self.user_text_path(schema).is_file() {
            if !self.search_user_words_prefix(schema, "", 1)?.is_empty() {
                anyhow::bail!("用户词还没写成文件，请稍后再试");
            }
            ud.rewrite_text(Target::User, schema, &[])?;
        }
        ud.hold_text(Target::User, schema);
        Ok(())
    }

    /// 学到的数据（临时词 / 词频）：同用户词——先折回，没有文件就按空表新建，再暂停写回。
    fn prepare_learned_edit(&self, target: Target, schema: &str) -> anyhow::Result<()> {
        let ud = self.runtime().data();
        ud.bind_by_rule(target, schema)?;
        if ud.pending(target, schema) > 0
            && let Err(e) = ud.fold_text(target, schema)
        {
            warn!("外部编辑前折回 {schema} 的数据失败：{e:#}");
        }
        if !self.learned_path(target, schema).is_file() {
            let mut any = false;
            ud.scan(target, schema, "", &mut |_| {
                any = true;
                false
            });
            if any {
                anyhow::bail!("数据还没写成文件，请稍后再试");
            }
            ud.replace_text_entries(target, schema, Vec::new())?;
        }
        ud.hold_text(target, schema);
        Ok(())
    }

    /// 临时词 / 词频的文件路径。
    fn learned_path(&self, target: Target, schema: &str) -> PathBuf {
        crate::learned_text::learned_binding(self.user_text_dir(), target, schema)
            .map(|b| b.path)
            .unwrap_or_else(|| self.user_text_dir().join(schema))
    }

    /// 结束外部编辑：按文件同步（同 `dict.reload` 带路径）→ 同步成功才删会话、恢复写回 →
    /// 报该文件的解析问题。没有会话（core 重启过）也照样同步。
    pub fn end_edit(&self, p: &str) -> anyhow::Result<EditResult> {
        let (path, tp) = self.resolve_edit_path(p)?;
        let (generation, begun_fp) = self.session_state(&path);
        let r = self.sync_edited(&path, &tp, begun_fp.as_deref())?;
        self.close_session(&path, &tp, generation);
        Ok(r)
    }

    /// 会话的代次与开始时的指纹；没有会话为 `(None, None)`。
    fn session_state(&self, path: &Path) -> (Option<u64>, Option<String>) {
        self.edit_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .map_or((None, None), |e| (Some(e.generation), e.fp.clone()))
    }

    /// 删会话并恢复写回——仅当期间没有人重新 begin（代次没变）；否则新会话继续，它的暂停
    /// 写回不能被这次 end 放掉。在会话锁内做，与 begin 互斥。
    fn close_session(&self, path: &Path, tp: &TextPath, generation: Option<u64>) {
        let mut sessions = self.edit_sessions.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.get(path).map(|e| e.generation) != generation {
            return;
        }
        sessions.remove(path);
        self.release_hold(tp);
    }

    /// 恢复该类文件的写回（用户词 / 临时词 / 词频；别的没有暂停）。
    fn release_hold(&self, tp: &TextPath) {
        match tp {
            TextPath::Words(s) => self.runtime().data().release_text(Target::User, s),
            TextPath::Learned(t, s) => self.runtime().data().release_text(*t, s),
            _ => {}
        }
    }

    /// 按文件同步。分库 / 开关文件不归 store 管内容：重新解析报问题，按指纹（`begun_fp`，没有
    /// 会话则视为变了）决定要不要让调用方失效方案。
    fn sync_edited(
        &self,
        path: &Path,
        tp: &TextPath,
        begun_fp: Option<&str>,
    ) -> anyhow::Result<EditResult> {
        let reloaded = match tp {
            TextPath::UserDicts(ds) => {
                crate::user_dicts::report_file_issues(path);
                if begun_fp.is_some_and(|f| f == crate::user_dicts::file_fingerprint(path)) {
                    Vec::new()
                } else {
                    vec![format!("{}{ds}", crate::user_words::USER_DICTS_CHANGED)]
                }
            }
            _ => self.reload_user_text_paths(&[path.to_string_lossy().into_owned()])?,
        };
        let issues: Vec<_> = self
            .data_issues()
            .into_iter()
            .filter(|i| i.path == path)
            .collect();
        info!(
            "外部编辑结束：{}（{}，{} 处问题）",
            self.data_rel(path),
            if reloaded.is_empty() {
                "无改动"
            } else {
                "已载入"
            },
            issues.len()
        );
        Ok(EditResult {
            path: path.to_path_buf(),
            changed: !reloaded.is_empty(),
            reloaded,
            issues,
        })
    }

    /// 当前会话：(相对路径, 距开始 / 重复 begin 多久)。
    pub fn edit_sessions(&self) -> Vec<(String, Duration)> {
        self.edit_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(p, e)| (self.data_rel(p), e.active.elapsed().unwrap_or_default()))
            .collect()
    }

    /// 暂停写回中的文件（用户词 / 临时词 / 词频；`dict.flush` 据此报哪些没写回）。
    pub fn held_edit_files(&self) -> Vec<String> {
        self.runtime()
            .data()
            .held_texts()
            .into_iter()
            .filter_map(|(t, s)| match t {
                Target::User => Some(self.user_text_path(&s)),
                Target::Temp | Target::Freq => Some(self.learned_path(t, &s)),
                _ => None,
            })
            .map(|p| self.data_rel(&p))
            .collect()
    }

    /// 该文件（绝对路径）正在外部编辑。
    pub fn is_editing(&self, path: &Path) -> bool {
        self.edit_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(path)
    }

    /// 该数据方案的用户词正在外部编辑（暂停写回）。
    pub fn user_words_held(&self, schema: &str) -> bool {
        self.runtime().data().is_held(Target::User, schema)
    }

    /// 小表写入口的统一检查：该文件正在外部编辑 ⇒ Err。
    pub(crate) fn check_not_editing(&self, path: &Path) -> anyhow::Result<()> {
        if self.is_editing(path) {
            anyhow::bail!("正在外部编辑 {}，完成后再改", self.data_rel(path));
        }
        Ok(())
    }

    /// 用户词正在外部编辑（暂停写回）⇒ Err：改权重这类补丁按时间规则会被随后保存的文件盖掉。
    pub(crate) fn check_words_not_held(&self, schema: &str) -> anyhow::Result<()> {
        if self.user_words_held(schema) {
            anyhow::bail!(
                "正在外部编辑 {}，完成后再改",
                self.data_rel(&self.user_text_path(schema))
            );
        }
        Ok(())
    }

    /// 临时词 / 词频正在外部编辑（暂停写回）⇒ Err：界面的删除 / 清空 / 转正 / 导入先挡住。
    pub(crate) fn check_learned_not_held(
        &self,
        target: Target,
        schema: &str,
    ) -> anyhow::Result<()> {
        if self.runtime().data().is_held(target, schema) {
            anyhow::bail!(
                "正在外部编辑 {}，完成后再改",
                self.data_rel(&self.learned_path(target, schema))
            );
        }
        Ok(())
    }

    pub fn set_edit_expired_hook(&self, hook: ChangedHook) {
        *self
            .edit_expired_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// 结束最后活动（开始 / 重复 begin、文件 mtime 取晚者）早于 `lease` 的会话（按 end 处理），
    /// 有变化就交给回调。返回结束的会话数。
    pub fn expire_edit_sessions(&self, lease: Duration) -> usize {
        let sessions: Vec<(PathBuf, SystemTime, u64)> = self
            .edit_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(p, e)| (p.clone(), e.active, e.generation))
            .collect();
        let expired: Vec<(PathBuf, u64)> = sessions
            .into_iter()
            .filter(|(p, active, _)| {
                let saved = std::fs::metadata(p).and_then(|m| m.modified()).ok();
                let last = saved.map_or(*active, |m| m.max(*active));
                last.elapsed().unwrap_or_default() >= lease
            })
            .map(|(p, _, g)| (p, g))
            .collect();
        let mut changed: Vec<String> = Vec::new();
        for (p, generation) in &expired {
            info!("外部编辑超过租约，自动结束：{}", self.data_rel(p));
            match self.end_edit(&p.to_string_lossy()) {
                Ok(r) => {
                    for n in r.reloaded {
                        if !changed.contains(&n) {
                            changed.push(n);
                        }
                    }
                }
                Err(e) => {
                    // 照样删会话、恢复写回：暂停不能比租约活得久（文件之后走隔离 / 退避）
                    warn!("自动结束外部编辑失败，仍恢复写回：{e:#}");
                    // 期间有人重新 begin（代次变了）⇒ 新会话继续，不动它
                    let mut sessions = self.edit_sessions.lock().unwrap_or_else(|e| e.into_inner());
                    if sessions.get(p).is_some_and(|e| e.generation == *generation)
                        && let Some(e) = sessions.remove(p)
                    {
                        self.release_hold(&e.tp);
                    }
                }
            }
        }
        if !changed.is_empty() {
            let hook = self
                .edit_expired_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(h) = hook {
                h(&changed);
            }
        }
        expired.len()
    }

    /// 「打开数据目录」：数据目录本身、某个方案目录，或其下一个数据文件的绝对路径。
    pub fn locate_data_path(&self, p: Option<&str>) -> anyhow::Result<PathBuf> {
        let root = self.user_text_dir();
        let Some(p) = p.filter(|p| !p.is_empty()) else {
            return Ok(root.to_path_buf());
        };
        let abs = self.data_abs(p);
        if let Ok(rest) = abs.strip_prefix(root) {
            let parts: Vec<_> = rest.components().collect();
            match parts.as_slice() {
                [] => return Ok(root.to_path_buf()),
                [std::path::Component::Normal(d)]
                    if wind_userdata::engine::check_schema_id(&d.to_string_lossy()).is_ok()
                        && Path::new(d).extension().is_none() =>
                {
                    return Ok(root.join(d));
                }
                _ => {}
            }
        }
        classify_text_path(root, &abs.to_string_lossy(), cfg!(windows))?;
        Ok(abs)
    }
}

#[cfg(test)]
#[path = "external_edit_tests.rs"]
mod tests;
