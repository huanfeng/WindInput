//! 一轮学习数据同步（设计 docs/design/sync-learned-data.md §4.5）：计入本机贡献 → 交换 → 物化各设备合计。
//!
//! 私有目录 `<state_dir>/learn/<库标识短哈希>/`：`state.json`（[`LearnState`]）与
//! `peers/<dev>/<目录编号>/<对象名>`（他机对象明文镜像，保留到不再被任何组引用）。

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wind_sync::Backend;
use wind_sync::crypto::{FilesKey, content_hash, random_bytes};
use wind_sync::learn::format::{decode, encode, needs_rebase, overlay, should_compact, split};
use wind_sync::learn::lane::{fetch_peers, gc_own, publish};
use wind_sync::learn::model::{absorb_peer_deletes, diff, gc_tombs, materialize};
use wind_sync::learn::state::{GroupState, LearnState, Pending};
use wind_sync::learn::table::{self, LearnedFile};
use wind_sync::learn::{Contribution, Gen, LearnKind};
use wind_sync::manifest::valid_device_id;
use wind_sync_ipc::SyncContent;

use crate::errors::sync_error;
use crate::link::{CoreLink, TIMEOUT_MSG};
use crate::log::log;

const STATE_FILE: &str = "state.json";
const LEARNED_KINDS: [LearnKind; 3] = [LearnKind::Temp, LearnKind::Freq, LearnKind::Assoc];

/// 测试用的崩溃注入点（物化写盘前后）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailPoint {
    BeforeWrite,
    AfterWrite,
}

pub struct LearnCtx<'a> {
    pub userdata: &'a Path,
    /// 私有目录 = `state_dir/learn/<库标识短哈希>/`
    pub state_dir: &'a Path,
    pub link: &'a dyn CoreLink,
    pub dev: &'a str,
    /// unix 秒
    pub now: u64,
    pub content: SyncContent,
    /// 库标识：后端位置 + `meta.check`，见 [`library_key`]
    pub library: &'a str,
    #[cfg(test)]
    pub fail_at: Option<FailPoint>,
}

impl<'a> LearnCtx<'a> {
    pub fn new(
        userdata: &'a Path,
        state_dir: &'a Path,
        link: &'a dyn CoreLink,
        dev: &'a str,
        now: u64,
        content: SyncContent,
        library: &'a str,
    ) -> Self {
        LearnCtx {
            userdata,
            state_dir,
            link,
            dev,
            now,
            content,
            library,
            #[cfg(test)]
            fail_at: None,
        }
    }

    fn now_ms(&self) -> u64 {
        self.now.saturating_mul(1000)
    }

    fn fail(&self, _at: FailPoint) -> Result<(), String> {
        #[cfg(test)]
        if self.fail_at == Some(_at) {
            return Err(format!("注入失败: {_at:?}"));
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct LearnReport {
    pub groups_written: usize,
    pub warnings: Vec<String>,
}

/// 库标识 = 后端位置与 `meta.check` 拼接；换位置、换密钥、重置远端都是新库。
pub fn library_key(location: &str, check: &str) -> String {
    format!("{location}\n{check}")
}

fn private_dir_for(state_dir: &Path, library: &str) -> PathBuf {
    let h = content_hash(library.as_bytes());
    state_dir.join("learn").join(&h[..16.min(h.len())])
}

pub fn private_dir(ctx: &LearnCtx) -> PathBuf {
    private_dir_for(ctx.state_dir, ctx.library)
}

/// 读私有状态；库标识对不上（短哈希撞了）报错。
pub fn load_state(ctx: &LearnCtx) -> Result<LearnState, String> {
    let mut st = LearnState::load(&private_dir(ctx).join(STATE_FILE))
        .map_err(|e| format!("读取学习数据状态失败: {e}"))?;
    if st.library.is_empty() {
        st.library = ctx.library.to_string();
    } else if st.library != ctx.library {
        return Err("学习数据状态属于另一个同步库".into());
    }
    Ok(st)
}

/// 该库上次成功的学习数据同步时间；读不到为 0。
pub fn load_last_ok(state_dir: &Path, library: &str) -> u64 {
    LearnState::load(&private_dir_for(state_dir, library).join(STATE_FILE))
        .ok()
        .filter(|s| s.library == library)
        .map_or(0, |s| s.last_ok)
}

fn save(ctx: &LearnCtx, st: &LearnState) -> Result<(), String> {
    let dir = private_dir(ctx);
    std::fs::create_dir_all(&dir)
        .and_then(|()| st.save(&dir.join(STATE_FILE)))
        .map_err(|e| format!("保存学习数据状态失败: {e}"))
}

// ── 分组与路径 ─────────────────────────────────────────

fn kind_on(content: SyncContent, kind: LearnKind) -> bool {
    match kind {
        LearnKind::Completion => content.completion,
        _ => content.learned,
    }
}

/// 组名 → (种类, 相对数据目录的路径)。
fn group_file(name: &str) -> Option<(LearnKind, String)> {
    if name == "completion" {
        return Some((
            LearnKind::Completion,
            LearnKind::Completion.file_name().into(),
        ));
    }
    let (schema, k) = name.split_once('/')?;
    let kind = LEARNED_KINDS.into_iter().find(|x| x.as_str() == k)?;
    Some((kind, format!("{schema}/{}", kind.file_name())))
}

fn data_abs(userdata: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(userdata.join("data"), |p, s| p.join(s))
}

/// 新建文件时 pinyin 的 temp/freq 编码按音节分隔。
fn syllable(name: &str, kind: LearnKind) -> bool {
    name.starts_with("pinyin/") && matches!(kind, LearnKind::Temp | LearnKind::Freq)
}

fn schema_dir(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['.', ' '])
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        && name != "charsets"
        && name != "stats"
}

/// 本机的学习文件组（按组名排序）：`data/<schema>/` 下三类与 `data/completion.table.yaml`，
/// 按同步内容开关过滤。文件不一定存在。
pub fn learned_groups(userdata: &Path, content: SyncContent) -> Vec<(String, LearnKind, PathBuf)> {
    let mut out = Vec::new();
    if content.learned
        && let Ok(rd) = std::fs::read_dir(userdata.join("data"))
    {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !schema_dir(&name) || !e.path().is_dir() {
                continue;
            }
            for kind in LEARNED_KINDS {
                out.push((
                    format!("{name}/{}", kind.as_str()),
                    kind,
                    e.path().join(kind.file_name()),
                ));
            }
        }
    }
    if content.completion {
        out.push((
            "completion".into(),
            LearnKind::Completion,
            data_abs(userdata, LearnKind::Completion.file_name()),
        ));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn read_opt(p: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(p) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读取 {} 失败: {e}", p.display())),
    }
}

fn write_atomic(p: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = crate::fsutil::tmp_sibling(p, ".tmp");
    let r = std::fs::File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, p));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

// ── 贡献与镜像 ─────────────────────────────────────────

fn empty_contrib(kind: LearnKind) -> Contribution {
    Contribution {
        kind,
        epoch: Gen::initial(),
        rows: BTreeMap::new(),
        tombs: BTreeMap::new(),
    }
}

fn is_empty(c: &Contribution) -> bool {
    c.rows.is_empty() && c.tombs.is_empty() && c.epoch == Gen::initial()
}

fn contrib_of(kind: LearnKind, s: &str) -> Result<Contribution, String> {
    if s.is_empty() {
        return Ok(empty_contrib(kind));
    }
    decode(kind, s).map_err(|e| format!("本机贡献损坏: {e}"))
}

fn base_of(kind: LearnKind, s: &str) -> Result<LearnedFile, String> {
    table::parse(kind, Some(s.as_bytes())).map_err(|e| format!("基线损坏: {e}"))
}

fn mirror_dir(dir: &Path, dev: &str, n: u64) -> PathBuf {
    dir.join("peers").join(dev).join(n.to_string())
}

fn obj_path(dir: &Path, name: &str) -> PathBuf {
    name.split('/').fold(dir.to_path_buf(), |p, s| p.join(s))
}

/// 镜像里某设备某组的贡献（底稿叠加增量）；没有底稿为 None。
fn mirror_contrib(
    dir: &Path,
    dev: &str,
    n: u64,
    name: &str,
    kind: LearnKind,
) -> Result<Option<Contribution>, String> {
    let m = mirror_dir(dir, dev, n);
    let read = |suffix: &str| -> Result<Option<Contribution>, String> {
        let Some(b) = read_opt(&obj_path(&m, &format!("{name}.{suffix}")))? else {
            return Ok(None);
        };
        let s = String::from_utf8(b).map_err(|_| format!("设备 {dev} 的 {name} 不是 UTF-8"))?;
        decode(kind, &s)
            .map(Some)
            .map_err(|e| format!("设备 {dev} 的 {name}.{suffix} 不可用: {e}"))
    };
    let Some(base) = read("base")? else {
        return Ok(None);
    };
    Ok(Some(match read("delta")? {
        Some(d) => overlay(&base, &d),
        None => base,
    }))
}

/// 各设备一份贡献（按 `seen` 指向的镜像目录）。
fn peers_of(
    dir: &Path,
    seen: &BTreeMap<String, u64>,
    name: &str,
    kind: LearnKind,
) -> Result<Vec<Contribution>, String> {
    let mut out = Vec::new();
    for (dev, n) in seen {
        if let Some(c) = mirror_contrib(dir, dev, *n, name, kind)? {
            out.push(c);
        }
    }
    Ok(out)
}

fn peers_have(dir: &Path, seen: &BTreeMap<String, u64>, name: &str) -> bool {
    seen.iter()
        .any(|(dev, n)| obj_path(&mirror_dir(dir, dev, *n), &format!("{name}.base")).is_file())
}

// ── 编辑租约 ───────────────────────────────────────────

/// 去掉用户自己开着编辑会话的组（本进程的会话此时都已结束）。
fn drop_busy(
    ctx: &LearnCtx,
    groups: Vec<(String, LearnKind, PathBuf)>,
) -> Result<Vec<(String, LearnKind, PathBuf)>, String> {
    if groups.is_empty() {
        return Ok(groups);
    }
    let v = ctx
        .link
        .request("data.editSessions", json!({}))
        .map_err(|e| format!("查询编辑会话失败: {e}"))?;
    let open: BTreeSet<&str> = v
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("file").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    Ok(groups
        .into_iter()
        .filter(|(n, ..)| {
            let busy = group_file(n).is_some_and(|(_, rel)| open.contains(rel.as_str()));
            if busy {
                log!("{n} 正在被编辑，本轮跳过该组学习数据");
            }
            !busy
        })
        .collect())
}

/// 逐个 `data.beginEdit`；任何一个失败则结束已开的并报错。
fn begin_all(
    ctx: &LearnCtx,
    groups: &[(String, LearnKind, PathBuf)],
) -> Result<Vec<String>, String> {
    let mut begun = Vec::new();
    for (n, ..) in groups {
        let Some((_, rel)) = group_file(n) else {
            continue;
        };
        if let Err(e) = ctx.link.request("data.beginEdit", json!({ "path": rel })) {
            // 超时的请求 core 可能仍会处理：也释放这一条
            if e == TIMEOUT_MSG {
                begun.push(rel.clone());
            }
            end_all(ctx, &begun);
            return Err(format!("核心拒绝开始编辑 {rel}: {e}"));
        }
        begun.push(rel);
    }
    Ok(begun)
}

fn end_all(ctx: &LearnCtx, begun: &[String]) {
    for p in begun {
        // 失败重试一次：会话留着会让 core 一直暂停该文件的写回
        let r = ctx
            .link
            .request("data.endEdit", json!({ "path": p }))
            .or_else(|_| ctx.link.request("data.endEdit", json!({ "path": p })));
        if let Err(e) = r {
            log!("data.endEdit {p} 失败: {e}");
        }
    }
}

/// 在租约内跑 `f`，无论成败都结束租约。
fn leased<T>(
    ctx: &LearnCtx,
    groups: &[(String, LearnKind, PathBuf)],
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let begun = begin_all(ctx, groups)?;
    let r = f();
    end_all(ctx, &begun);
    r
}

/// 读学习文件；读不懂或全是坏行时返回 None（跳过该组）。
fn read_learned(
    name: &str,
    kind: LearnKind,
    abs: &Path,
    warnings: &mut Vec<String>,
) -> Result<Option<(LearnedFile, String)>, String> {
    let bytes = read_opt(abs)?;
    let f = match table::parse(kind, bytes.as_deref()) {
        Ok(f) => f,
        Err(e) => {
            warnings.push(format!("{name} 无法解析，本轮跳过: {e}"));
            return Ok(None);
        }
    };
    if f.rows.is_empty() && f.bad_lines > 0 {
        warnings.push(format!("{name} 全是坏行，本轮跳过"));
        return Ok(None);
    }
    let text = bytes
        .map(|b| String::from_utf8(b).unwrap_or_default())
        .unwrap_or_default();
    Ok(Some((f, text)))
}

// ── 第 2 步：计入 ─────────────────────────────────────

/// 采用或丢弃上一轮的待定记录，再把各组 `F − M` 计入本机贡献并原子保存 `{C, M = F}`。
pub fn count_in(ctx: &LearnCtx, st: &mut LearnState) -> Result<(), String> {
    if st.pending.is_some() {
        let r = st.resolve_pending(|name| {
            let (_, rel) = group_file(name)?;
            let b = std::fs::read(data_abs(ctx.userdata, &rel)).ok()?;
            table::token_of(&b)
        });
        if !r.dropped.is_empty() {
            log!("上次物化未写成，沿用已提交状态: {}", r.dropped.join("、"));
        }
        save(ctx, st)?;
    }
    let groups: Vec<_> = learned_groups(ctx.userdata, ctx.content)
        .into_iter()
        .filter(|(n, _, p)| p.is_file() || st.groups.contains_key(n))
        .collect();
    let groups = drop_busy(ctx, groups)?;
    if groups.is_empty() {
        return Ok(());
    }
    let dir = private_dir(ctx);
    leased(ctx, &groups, || {
        let mut warnings = Vec::new();
        let mut changed = false;
        for (name, kind, abs) in &groups {
            let Some((f, text)) = read_learned(name, *kind, abs, &mut warnings)? else {
                continue;
            };
            let old = st.groups.get(name);
            let gs = old.cloned().unwrap_or_default();
            let peers = match peers_of(&dir, &gs.peer_seen, name, *kind) {
                Ok(p) => p,
                Err(e) => {
                    warnings.push(format!("{name} 的镜像不可用，本轮跳过: {e}"));
                    continue;
                }
            };
            let mut me = contrib_of(*kind, &gs.contrib)?;
            let m = base_of(*kind, &gs.base)?;
            diff(
                &mut me,
                &f,
                &m,
                ctx.now_ms(),
                ctx.dev,
                &peers.iter().collect::<Vec<_>>(),
            );
            let ng = GroupState {
                contrib: encode(&me),
                base: text,
                ..gs
            };
            if old != Some(&ng) {
                st.groups.insert(name.clone(), ng);
                changed = true;
            }
        }
        for w in &warnings {
            log!("{w}");
        }
        if changed {
            save(ctx, st)?;
        }
        Ok(())
    })
}

// ── 第 3 步：交换 ─────────────────────────────────────

/// 发布本机对象：底稿不能表达或该压实时写新底稿，否则底稿 + 增量。关闭的组沿用原底稿不压实。
fn publish_own(
    ctx: &LearnCtx,
    b: &dyn Backend,
    key: &FilesKey,
    st: &mut LearnState,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let mut objects: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut rebased: Vec<(String, String)> = Vec::new();
    for (name, gs) in &st.groups {
        let Some((kind, _)) = group_file(name) else {
            continue;
        };
        let full = contrib_of(kind, &gs.contrib)?;
        let on = kind_on(ctx.content, kind);
        let (bn, dn) = (format!("{name}.base"), format!("{name}.delta"));
        if gs.last_base.is_empty() {
            if on && !is_empty(&full) {
                objects.insert(bn, gs.contrib.clone().into_bytes());
                rebased.push((name.clone(), gs.contrib.clone()));
            }
            continue;
        }
        let lb = contrib_of(kind, &gs.last_base)?;
        let delta = split(&full, &lb);
        let delta_text = encode(&delta);
        let compact = on
            && (needs_rebase(&full, &lb)
                || should_compact(
                    lb.rows.len() + lb.tombs.len(),
                    delta.rows.len() + delta.tombs.len(),
                    delta_text.len(),
                    ctx.now.saturating_sub(gs.base_at),
                ));
        if compact {
            objects.insert(bn, gs.contrib.clone().into_bytes());
            rebased.push((name.clone(), gs.contrib.clone()));
        } else {
            objects.insert(bn, gs.last_base.clone().into_bytes());
            if !(delta.rows.is_empty() && delta.tombs.is_empty() && delta.epoch == lb.epoch) {
                objects.insert(dn, delta_text.into_bytes());
            }
        }
    }
    let hashes: BTreeMap<String, String> = objects
        .iter()
        .map(|(n, p)| (n.clone(), content_hash(p)))
        .collect();
    let unchanged = match &st.own_index {
        Some(i) => i.objects == hashes,
        None => hashes.is_empty(),
    };
    if unchanged {
        return Ok(());
    }
    let prev = st.own_index.clone();
    let idx = publish(b, key, ctx.dev, prev.as_ref(), &objects).map_err(|e| sync_error(&e))?;
    for (name, text) in rebased {
        if let Some(g) = st.groups.get_mut(&name) {
            g.last_base = text;
            g.base_at = ctx.now;
        }
    }
    st.index_gen = idx.gen_;
    st.own_index = Some(idx.clone());
    save(ctx, st)?;
    let keep: Vec<_> = std::iter::once(&idx).chain(prev.as_ref()).collect();
    if let Err(e) = gc_own(b, key, ctx.dev, &keep, ctx.now) {
        warnings.push(format!("回收本机学习数据对象失败: {}", sync_error(&e)));
    }
    Ok(())
}

/// 把他机新目录拉进镜像 `peers/<dev>/<编号>/`；只在目录与对象全部取到时推进该设备。
fn fetch_mirrors(
    ctx: &LearnCtx,
    b: &dyn Backend,
    key: &FilesKey,
    st: &mut LearnState,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let dir = private_dir(ctx);
    let ups = fetch_peers(b, key, ctx.dev, &st.peer_index).map_err(|e| sync_error(&e))?;
    let mut changed = false;
    for u in ups {
        if !u.complete() {
            warnings.extend(u.warnings);
            continue;
        }
        if !valid_device_id(&u.dev) {
            warnings.push(format!("设备号不合法，已跳过: {}", u.dev));
            continue;
        }
        let old = st
            .peer_seen
            .get(&u.dev)
            .map(|n| mirror_dir(&dir, &u.dev, *n));
        let tmp = dir
            .join("peers")
            .join(&u.dev)
            .join(format!(".tmp-{}", u.gen_));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut lost = None;
        for name in u.index.objects.keys() {
            let dst = obj_path(&tmp, name);
            let r = match u.objects.get(name) {
                Some(Some(bytes)) => write_atomic(&dst, bytes),
                Some(None) => Ok(()),
                None => match &old {
                    Some(o) => std::fs::create_dir_all(dst.parent().unwrap_or(&tmp))
                        .and_then(|()| std::fs::copy(obj_path(o, name), &dst).map(|_| ())),
                    None => Err(io::Error::new(io::ErrorKind::NotFound, "无旧镜像")),
                },
            };
            if let Err(e) = r {
                lost = Some(format!("{name}: {e}"));
                break;
            }
        }
        let fin = mirror_dir(&dir, &u.dev, u.gen_);
        let placed = lost.is_none() && {
            std::fs::create_dir_all(&tmp)
                .and_then(|()| {
                    let _ = std::fs::remove_dir_all(&fin);
                    std::fs::rename(&tmp, &fin)
                })
                .map_err(|e| lost = Some(e.to_string()))
                .is_ok()
        };
        if !placed {
            let _ = std::fs::remove_dir_all(&tmp);
            // 镜像缺失：下轮整份重取
            warnings.push(format!(
                "设备 {} 的镜像写入失败，下轮重取: {}",
                u.dev,
                lost.unwrap_or_default()
            ));
            st.peer_index.remove(&u.dev);
            changed = true;
            continue;
        }
        st.peer_seen.insert(u.dev.clone(), u.gen_);
        st.peer_index.insert(u.dev, u.index);
        changed = true;
    }
    if changed {
        save(ctx, st)?;
    }
    Ok(())
}

// ── 第 5 步：物化 ─────────────────────────────────────

/// 一个组的物化结果。
struct Planned {
    name: String,
    abs: PathBuf,
    state: GroupState,
    /// 要写入的 `F'`；None = 文件已是物化结果
    write: Option<Vec<u8>>,
}

fn plan_group(
    ctx: &LearnCtx,
    st: &LearnState,
    dir: &Path,
    token: &str,
    (name, kind, abs): &(String, LearnKind, PathBuf),
    warnings: &mut Vec<String>,
) -> Result<Option<Planned>, String> {
    let kind = *kind;
    let Some((f2, text)) = read_learned(name, kind, abs, warnings)? else {
        return Ok(None);
    };
    let gs = st.groups.get(name).cloned().unwrap_or_default();
    let peers = peers_of(dir, &gs.peer_seen, name, kind)
        .and_then(|old| Ok((old, peers_of(dir, &st.peer_seen, name, kind)?)));
    let (old, new) = match peers {
        Ok(p) => p,
        Err(e) => {
            warnings.push(format!("{name} 的镜像不可用，本轮跳过: {e}"));
            return Ok(None);
        }
    };
    let new_refs: Vec<&Contribution> = new.iter().collect();
    let mut me = contrib_of(kind, &gs.contrib)?;
    let m = base_of(kind, &gs.base)?;
    let now_ms = ctx.now_ms();
    diff(
        &mut me,
        &f2,
        &m,
        now_ms,
        ctx.dev,
        &old.iter().collect::<Vec<_>>(),
    );
    absorb_peer_deletes(&mut me, &new_refs);
    gc_tombs(&mut me, &new_refs, now_ms);
    let all: Vec<&Contribution> = std::iter::once(&me).chain(new.iter()).collect();
    let fp = materialize(kind, &all, &f2);
    let mut ng = GroupState {
        contrib: encode(&me),
        peer_seen: st.peer_seen.clone(),
        ..gs
    };
    let write = if fp.rows != f2.rows {
        let out = table::render(kind, &fp, Some(token), syllable(name, kind));
        ng.base = String::from_utf8(out.clone()).map_err(|_| "物化结果不是 UTF-8".to_string())?;
        Some(out)
    } else {
        ng.base = text;
        None
    };
    Ok(Some(Planned {
        name: name.clone(),
        abs: abs.clone(),
        state: ng,
        write,
    }))
}

/// 物化各组；写盘次序：待定记录落盘 → 写 `F'`（带令牌）→ 待定转已提交。返回写出的文件数。
fn materialize_all(
    ctx: &LearnCtx,
    st: &mut LearnState,
    warnings: &mut Vec<String>,
) -> Result<usize, String> {
    let dir = private_dir(ctx);
    let groups: Vec<_> = learned_groups(ctx.userdata, ctx.content)
        .into_iter()
        .filter(|(n, _, p)| {
            p.is_file() || st.groups.contains_key(n) || peers_have(&dir, &st.peer_seen, n)
        })
        .collect();
    let groups = drop_busy(ctx, groups)?;
    if groups.is_empty() {
        return Ok(0);
    }
    leased(ctx, &groups, || {
        let token: String = random_bytes::<8>()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut plans = Vec::new();
        for g in &groups {
            if let Some(p) = plan_group(ctx, st, &dir, &token, g, warnings)? {
                plans.push(p);
            }
        }
        let mut changed = false;
        let mut pending = BTreeMap::new();
        let mut writes = Vec::new();
        for p in plans {
            match p.write {
                Some(bytes) => {
                    pending.insert(p.name, p.state);
                    writes.push((p.abs, bytes));
                }
                // 文件不写：{C₂, M = F₂} 与盘上一致，直接提交
                None => {
                    if st.groups.get(&p.name) != Some(&p.state) {
                        st.groups.insert(p.name, p.state);
                        changed = true;
                    }
                }
            }
        }
        if writes.is_empty() {
            if changed {
                save(ctx, st)?;
            }
            return Ok(0);
        }
        st.pending = Some(Pending {
            token,
            groups: pending,
        });
        save(ctx, st)?;
        ctx.fail(FailPoint::BeforeWrite)?;
        for (abs, bytes) in &writes {
            write_atomic(abs, bytes).map_err(|e| format!("写入 {} 失败: {e}", abs.display()))?;
        }
        ctx.fail(FailPoint::AfterWrite)?;
        if let Some(p) = st.pending.take() {
            st.groups.extend(p.groups);
        }
        save(ctx, st)?;
        Ok(writes.len())
    })
}

/// 删除不再被任何组（及最新下载）引用的镜像目录。
fn prune_mirrors(ctx: &LearnCtx, st: &LearnState) {
    let root = private_dir(ctx).join("peers");
    let mut keep: BTreeSet<(String, String)> = BTreeSet::new();
    let pending = st.pending.iter().flat_map(|p| p.groups.values());
    for seen in std::iter::once(&st.peer_seen)
        .chain(st.groups.values().map(|g| &g.peer_seen))
        .chain(pending.map(|g| &g.peer_seen))
    {
        for (d, n) in seen {
            keep.insert((d.clone(), n.to_string()));
        }
    }
    let Ok(devs) = std::fs::read_dir(&root) else {
        return;
    };
    for d in devs.flatten() {
        let dev = d.file_name().to_string_lossy().into_owned();
        let Ok(gens) = std::fs::read_dir(d.path()) else {
            continue;
        };
        let mut left = 0;
        for g in gens.flatten() {
            let n = g.file_name().to_string_lossy().into_owned();
            if keep.contains(&(dev.clone(), n)) {
                left += 1;
            } else if let Err(e) = std::fs::remove_dir_all(g.path()) {
                log!("删除旧镜像 {} 失败: {e}", g.path().display());
                left += 1;
            }
        }
        if left == 0 {
            let _ = std::fs::remove_dir(d.path());
        }
    }
}

/// 第 3、5 步：发布本机贡献、拉取他机镜像、物化各设备合计，最后回收旧镜像并记成功时间。
pub fn exchange_and_materialize(
    ctx: &LearnCtx,
    b: &dyn Backend,
    key: &FilesKey,
    st: &mut LearnState,
) -> Result<LearnReport, String> {
    let mut rep = LearnReport::default();
    publish_own(ctx, b, key, st, &mut rep.warnings)?;
    fetch_mirrors(ctx, b, key, st, &mut rep.warnings)?;
    rep.groups_written = materialize_all(ctx, st, &mut rep.warnings)?;
    prune_mirrors(ctx, st);
    st.last_ok = ctx.now;
    save(ctx, st)?;
    Ok(rep)
}

#[cfg(test)]
#[path = "learn_round_tests.rs"]
mod tests;
