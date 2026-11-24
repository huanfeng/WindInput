//! 引擎门面（设计 §3.4–§3.6）：打开（锁、设备、state、各目标快照、回放日志）、写入即发布、折回。
//!
//! 读路径无锁：按键线程 [`UserData::view`] 克隆一个 `Arc<View>` 后不再碰任何锁。写路径由一把
//! 写锁串行化（写者之间互斥、读者不等）：追加日志与发布快照在调用线程上同步完成，保证
//! 「写后立即可读」。折回在调用线程上做（P1 再放到独立线程），与写入靠同一把写锁协调：
//! 取「折回起点」与「水位」是一个原子动作，折回期间新到的操作留在 delta 里。

use crate::delta::{Delta, DeltaOp, Entry, entry_from_record};
use crate::device::{DeviceOutcome, Lock};
use crate::journal::Journal;
use crate::record::{OpKind, Payload, Record, Target};
use crate::snapshot::{Snapshot, SnapshotEntry};
use crate::state::{State, TargetState};
use anyhow::bail;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

/// 某 (目标, 方案) 未折回操作达到这么多条就折回（§3.5）。
pub const FOLD_THRESHOLD: usize = 4000;
/// 日志超过这么大就压实（§9：日志 ≤ 8 MB）。
pub const JOURNAL_MAX_BYTES: u64 = 8 << 20;
/// 文本目标的空闲折回间隔（§3.5：文本目标的操作来自用户主动动作，空闲 30 秒再写回）。
pub const TEXT_IDLE: std::time::Duration = std::time::Duration::from_secs(30);
/// 文本目标最老的待处理操作超过这么久，即使一直有新写入（连续打字从不空闲）也折回：
/// 学到的数据最多这么久就进用户目录（P5：复制用户目录 / 删 state 时的缺口上限）。
pub const TEXT_MAX_PENDING: std::time::Duration = std::time::Duration::from_secs(300);

/// 方案 id 会成为状态目录 / 文本目录下的一个路径分量：拒绝不能安全充当单个分量的 id（空、`.` /
/// `..`、含路径分隔符或 Windows 保留字符、控制字符、首尾是点或空格、过长）。不限定 ASCII——用户自建
/// 方案的 id 可以是中文。
pub fn check_schema_id(schema: &str) -> anyhow::Result<()> {
    let bad = schema.is_empty()
        || schema.len() > 128
        || schema
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        || schema.starts_with(['.', ' '])
        || schema.ends_with(['.', ' ']);
    if bad {
        bail!("非法方案 id：{schema:?}");
    }
    Ok(())
}

/// 文本目标折回失败（隔离 / 读不了 / 编辑器竞争）后的退避：首次 30 秒，逐次翻倍，封顶 30 分钟。
fn text_retry_backoff(fails: u32) -> std::time::Duration {
    TEXT_IDLE
        .saturating_mul(1 << fails.saturating_sub(1).min(6))
        .min(std::time::Duration::from_secs(1800))
}

/// 把某 (目标, 方案) 绑定到一个文本真源文件（§2.2 / §3.5 文本目标）。
#[derive(Clone, Debug)]
pub struct TextBinding {
    pub path: PathBuf,
    /// 新建文件时头部的 `name:`（如 `pinyin.user`）。
    pub name: String,
    /// 方案的 `lowercase_code`：匹配键规范化用。
    pub lowercase: bool,
    /// 新建文件时是否声明 `code_spacing: syllable`（拼音族）。表格文本里它决定码列是否写音节空格。
    pub syllable_codes: bool,
    /// 文件格式：用户词 `.dict.yaml`，或学到的数据的表格文本（§2.4）。
    pub format: TextFormat,
}

/// 文本目标的文件格式。
#[derive(Clone, Copy, Debug)]
pub enum TextFormat {
    /// `user.dict.yaml`：text / code / weight，计数与时间不进文本（P2a）。
    Words,
    /// 学到的数据：按列规格读写，计数与时间都进文本（P5）。
    Table(&'static crate::text::codec::TableSpec),
}

type TargetKey = (Target, String);

/// 文本绑定规则：给定 (目标, 方案) 返回它该绑定的文件；不归规则管 ⇒ None（P5：学到的数据）。
pub type TextRule = Box<dyn Fn(Target, &str) -> Option<TextBinding> + Send + Sync>;

pub struct Dirs {
    /// 本机状态目录（`<localdata>/state/`，设计 §2.1）。
    pub state_dir: PathBuf,
    /// 用户目录 `meta.toml` 里的 dir_id。
    pub dir_id: String,
}

/// 一条待写入的操作。`ts_ms == 0` 表示取当前时刻（测试与迁移可显式给定）。
#[derive(Clone, Debug)]
pub struct Op {
    pub target: Target,
    pub schema: String,
    pub kind: OpKind,
    pub code: String,
    pub text: String,
    pub payload: Payload,
    pub boundary: u64,
    pub ts_ms: i64,
}

impl Op {
    pub fn new(
        target: Target,
        schema: &str,
        kind: OpKind,
        code: &str,
        text: &str,
        payload: Payload,
        boundary: u64,
    ) -> Op {
        Op {
            target,
            schema: schema.into(),
            kind,
            code: code.into(),
            text: text.into(),
            payload,
            boundary,
            ts_ms: 0,
        }
    }
}

/// 一个不可变的读视图：各 (目标, 数据方案) 的快照 + 未折回的增量表。
pub struct View {
    pub snapshots: BTreeMap<(Target, String), Arc<Snapshot>>,
    pub delta: Arc<Delta>,
}

pub struct UserData {
    dirs: Dirs,
    /// 只读实例为 `None`：它不打开日志写端，免得 open 时的截尾截掉写者正在写的那一条。
    journal: Mutex<Option<Journal>>,
    state_path: PathBuf,
    /// 本设备日志路径（只读实例且没有 device.toml 时为 None）。
    jpath: Option<PathBuf>,
    /// 后台折回单飞标记。
    maintaining: std::sync::atomic::AtomicBool,
    /// 文本目标绑定（P2a）。被绑定的目标折回走 `fold_text`。
    text_bindings: Mutex<BTreeMap<TargetKey, TextBinding>>,
    /// 各目标最后一次写入的时刻（文本目标按空闲折回）。
    last_write: Mutex<BTreeMap<TargetKey, std::time::Instant>>,
    /// 文本目标折回失败的退避：(连续失败次数, 下次可重试时刻)。成功即清。
    text_retry: Mutex<BTreeMap<TargetKey, (u32, std::time::Instant)>>,
    /// 外部编辑会话中暂停写回的文本目标：空闲折回与压实都跳过，查询与日志照常。
    held: Mutex<BTreeSet<TargetKey>>,
    /// 文本目标的空闲时长（默认 `TEXT_IDLE`；测试可改）。
    text_idle: Mutex<std::time::Duration>,
    /// 待处理上限（默认 `TEXT_MAX_PENDING`；测试可改）。
    text_max_pending: Mutex<std::time::Duration>,
    /// 各文本目标本轮第一笔待处理写入的时刻（折回成功即清）。
    first_pending: Mutex<BTreeMap<TargetKey, std::time::Instant>>,
    /// 最近一次写日志失败的原因（磁盘满、目录只读…；不含词条内容）。成功写入即清（§8）。
    write_error: Mutex<Option<String>>,
    /// 上次压实裁到的位置（`keep_from`）：裁不动时不再反复重写日志（审查 I-2）。
    pruned_upto: std::sync::atomic::AtomicU64,
    /// 自动绑定规则（P5）：维护 / 压实 / `fold_with` 前把规则管的未绑定目标绑上。
    text_rule: Mutex<Option<Arc<TextRule>>>,
    view: RwLock<Arc<View>>,
    state: Mutex<State>,
    lock: Option<Lock>,
    /// 折回串行化：同一时刻只有一个 fold_with 在跑（后台折回与显式清空 / 导入不交错）。
    fold_lock: Mutex<()>,
    /// 写者之间互斥；apply 持它跨「追加 + 发布」，fold 持它跨「取水位 + 取起点」与「换入」。
    write_lock: Mutex<()>,
}

const TARGETS: [Target; 7] = [
    Target::User,
    Target::Temp,
    Target::Draft,
    Target::Freq,
    Target::Assoc,
    Target::Phrases,
    Target::Shadow,
];

fn target_name(t: Target) -> &'static str {
    match t {
        Target::User => "user",
        Target::Temp => "temp",
        Target::Draft => "draft",
        Target::Freq => "freq",
        Target::Assoc => "assoc",
        Target::Phrases => "phrases",
        Target::Shadow => "shadow",
    }
}

fn target_key(t: Target, schema: &str) -> String {
    format!("{}:{schema}", target_name(t))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn dir_id16(dir_id: &str) -> [u8; 16] {
    let mut b = [0u8; 16];
    for (i, x) in dir_id.bytes().take(16).enumerate() {
        b[i] = x;
    }
    b
}

type Snapshots = BTreeMap<(Target, String), Arc<Snapshot>>;

/// 枚举 `state_dir/<schema>/<target>` 已有的快照。打不开（sidecar 坏、对不上）的当零段，
/// 并记进第二个返回值：这些目标的水位不可信，要从日志整份回放。
fn enumerate_snapshots(state_dir: &std::path::Path) -> (Snapshots, BTreeSet<(Target, String)>) {
    let mut out = BTreeMap::new();
    let mut broken = BTreeSet::new();
    let Ok(rd) = std::fs::read_dir(state_dir) else {
        return (out, broken);
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(schema) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        if schema == "journal" {
            continue;
        }
        for t in TARGETS {
            match crate::snapshot::open(&p, target_name(t)) {
                Ok(Some(s)) => {
                    out.insert((t, schema.clone()), Arc::new(s));
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        "快照 {}/{} 打不开，当零段并整份回放: {err}",
                        p.display(),
                        target_name(t)
                    );
                    broken.insert((t, schema.clone()));
                }
            }
        }
    }
    (out, broken)
}

/// 要回放的日志：本设备的 `.wjl` 与所有 `.wjl.sealed`（换号封存的旧日志，未折回的记录仍要
/// 可见），按 seq 合并。缺失的跳过。
fn replay_all(journal_dir: &std::path::Path, current: Option<&std::path::Path>) -> Vec<Record> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(journal_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_str().is_some_and(|s| s.ends_with(".wjl.sealed")))
        .collect();
    paths.sort();
    paths.extend(current.filter(|p| p.is_file()).map(|p| p.to_path_buf()));
    let mut out = Vec::new();
    for p in paths {
        match Journal::replay(&p, 0) {
            Ok(rs) => out.extend(rs),
            Err(err) => tracing::warn!("日志 {} 读不出，跳过: {err}", p.display()),
        }
    }
    out.sort_by_key(|r| r.seq);
    out
}

/// 叠加语义的唯一实现（读路径、折回、层共用）。表见 P1a 计划 Task 1。
pub(crate) fn apply_ops(
    base: Option<SnapshotEntry>,
    ops: &[&Entry],
    code: &str,
    text: &str,
) -> Option<SnapshotEntry> {
    let fresh = |weight: i32, count: u32, boundary: u64, ts: i64, flags: u8| SnapshotEntry {
        code: code.to_string(),
        text: text.to_string(),
        weight,
        boundary,
        count,
        created_at: ts,
        last_used: ts,
        flags,
        order: 0,
    };
    let mut cur = base;
    for op in ops {
        let ts = op.ts_ms.div_euclid(1000);
        match op.op {
            DeltaOp::Upsert { weight, kind } => match &mut cur {
                Some(e) => {
                    e.weight = weight;
                    e.flags = kind;
                    e.boundary = op.boundary;
                    e.last_used = ts;
                }
                None => cur = Some(fresh(weight, 0, op.boundary, ts, kind)),
            },
            DeltaOp::Delete => cur = None,
            DeltaOp::SetWeight(w) => {
                if let Some(e) = &mut cur {
                    e.weight = w;
                }
            }
            DeltaOp::Incr(c) => match &mut cur {
                Some(e) => {
                    e.count = e.count.saturating_add(c);
                    e.last_used = ts;
                }
                None => cur = Some(fresh(0, c, op.boundary, ts, 0)),
            },
            DeltaOp::Touch(t) => {
                if let Some(e) = &mut cur {
                    e.last_used = t;
                }
            }
        }
    }
    cur
}

/// 快照条目 + delta 操作 → 叠加后的条目，按 (code, text) 升序。`ops` 里涉及的每个键必须带上
/// **该键的全部**操作（`Delta::exact` / `prefix` / `ops_for` 给出的都满足），否则叠加不完整。
pub(crate) fn overlay_entries(
    base: Vec<SnapshotEntry>,
    mut ops: Vec<&Entry>,
) -> Vec<SnapshotEntry> {
    let mut map: BTreeMap<(String, String), Option<SnapshotEntry>> = base
        .into_iter()
        .map(|e| ((e.code.clone(), e.text.clone()), Some(e)))
        .collect();
    ops.sort_by_key(|e| e.seq);
    ops.dedup_by_key(|e| e.seq);
    let mut by_key: BTreeMap<(String, String), Vec<&Entry>> = BTreeMap::new();
    for op in ops {
        by_key
            .entry((op.code.clone(), op.text.clone()))
            .or_default()
            .push(op);
    }
    for ((code, text), ks) in by_key {
        let b = map.remove(&(code.clone(), text.clone())).flatten();
        let merged = apply_ops(b, &ks, &code, &text);
        map.insert((code, text), merged);
    }
    map.into_values().flatten().collect()
}

/// 旧快照 + delta（该目标、该方案、seq ≤ upto）→ 按 (code, text) 排好序的新快照条目。
fn merge_for_fold(
    snapshot: Option<&Snapshot>,
    delta: &Delta,
    target: Target,
    schema: &str,
    upto: u64,
) -> Vec<SnapshotEntry> {
    let mut base = Vec::new();
    if let Some(s) = snapshot {
        s.for_each(&mut |e| base.push(e));
    }
    let ops: Vec<&Entry> = delta
        .prefix(target, schema, "")
        .into_iter()
        .filter(|e| e.seq <= upto)
        .collect();
    overlay_entries(base, ops)
}

type DeltaKeys<'a> =
    std::iter::Peekable<std::collections::btree_map::IntoIter<(String, String), Vec<&'a Entry>>>;

/// 吐出 delta 里 code < `before`（None = 全部）的、快照里没有的键。返回 false = 回调要求停。
fn flush_delta_before(
    dk: &mut DeltaKeys<'_>,
    before: Option<&str>,
    f: &mut dyn FnMut(SnapshotEntry) -> bool,
) -> bool {
    while let Some(((c, _), _)) = dk.peek() {
        if before.is_some_and(|b| c.as_str() >= b) {
            break;
        }
        let Some(((c, t), ops)) = dk.next() else {
            break;
        };
        if let Some(e) = apply_ops(None, &ops, &c, &t)
            && !f(e)
        {
            return false;
        }
    }
    true
}

/// 快照里一个码的全部条目与 delta 里同码的键按 text 归并叠加。返回 false = 回调要求停。
fn merge_code_group(
    mut group: Vec<SnapshotEntry>,
    code: &str,
    dk: &mut DeltaKeys<'_>,
    f: &mut dyn FnMut(SnapshotEntry) -> bool,
) -> bool {
    if !flush_delta_before(dk, Some(code), f) {
        return false;
    }
    // 叶内是按权重排的，归并要 text 序
    group.sort_by(|a, b| a.text.cmp(&b.text));
    let mut gi = group.into_iter().peekable();
    loop {
        let dnext = dk
            .peek()
            .filter(|((c, _), _)| c == code)
            .map(|((_, t), _)| t.clone());
        let take_delta = match (gi.peek(), &dnext) {
            (None, None) => return true,
            (Some(g), Some(d)) => d <= &g.text,
            (None, Some(_)) => true,
            (Some(_), None) => false,
        };
        let out = if take_delta {
            let Some(((c, t), ops)) = dk.next() else {
                return true;
            };
            let base = gi.next_if(|g| g.text == t);
            apply_ops(base, &ops, &c, &t)
        } else {
            gi.next()
        };
        if let Some(e) = out
            && !f(e)
        {
            return false;
        }
    }
}

impl UserData {
    pub fn open(dirs: Dirs) -> anyhow::Result<UserData> {
        let lock = crate::device::try_lock(&dirs.state_dir)?;
        let journal_dir = dirs.state_dir.join("journal");
        // 只读实例不碰 device.toml：换号会把写者正在追加的日志改名封存。
        let device_id = if lock.is_some() {
            match crate::device::load_or_renew(&dirs.state_dir, &dirs.dir_id, &journal_dir)? {
                DeviceOutcome::Same(d) => Some(d.device_id),
                DeviceOutcome::Renewed {
                    new,
                    sealed_journal,
                } => {
                    if let Some(s) = sealed_journal {
                        tracing::info!("设备标识已更新，旧日志已封存: {}", s.display());
                    }
                    Some(new.device_id)
                }
            }
        } else {
            crate::device::load_existing(&dirs.state_dir)
        };
        let jpath =
            device_id.map(|id| journal_dir.join(format!("{}.wjl", crate::device::hex(&id))));
        let mut journal = match (&lock, &jpath, device_id) {
            (Some(_), Some(p), Some(id)) => Some(Journal::open(p, id, dir_id16(&dirs.dir_id))?),
            _ => None,
        };
        let state_path = journal_dir.join("state");
        let mut state = State::load(&state_path).unwrap_or_default();

        let (snapshots, broken) = enumerate_snapshots(&dirs.state_dir);
        let records = replay_all(&journal_dir, jpath.as_deref());
        // 续号取所有已知水位之上：state、各快照内嵌水位、各折回水位、日志尾（含封存日志）。
        // 少了任何一个，删掉 journal/ 或换号之后新记录的 seq 就会落到水位之下，重开即被跳过。
        let floor = [
            state.next_seq,
            snapshots.values().map(|s| s.max_seq + 1).max().unwrap_or(0),
            state
                .targets
                .values()
                .map(|t| t.folded_seq + 1)
                .max()
                .unwrap_or(0),
            records.last().map(|r| r.seq + 1).unwrap_or(0),
        ]
        .into_iter()
        .max()
        .unwrap_or(1);
        if let Some(j) = journal.as_mut() {
            j.bump_next_seq(floor);
            state.next_seq = j.next_seq();
        }

        // 回放：每条记录只在 seq 高于「该目标快照内嵌水位」与「state 记的折回水位」两者的
        // 较大者时进 delta——state 丢了也不会把已折回的记录再应用一遍（§3.6）。快照坏了的
        // 目标水位不可信，整份回放。
        let mut replayed = Vec::new();
        for r in records {
            let key = (r.target, r.schema.clone());
            let wm = if broken.contains(&key) {
                0
            } else {
                let snap_wm = snapshots.get(&key).map(|s| s.max_seq).unwrap_or(0);
                let state_wm = state
                    .targets
                    .get(&target_key(r.target, &r.schema))
                    .map(|t| t.folded_seq)
                    .unwrap_or(0);
                snap_wm.max(state_wm)
            };
            if r.seq > wm
                && let Some(e) = entry_from_record(&r)
            {
                replayed.push(e);
            }
        }
        let delta = Delta::default().with_all(replayed);
        Ok(UserData {
            dirs,
            journal: Mutex::new(journal),
            state_path,
            jpath,
            maintaining: std::sync::atomic::AtomicBool::new(false),
            text_bindings: Mutex::new(BTreeMap::new()),
            last_write: Mutex::new(BTreeMap::new()),
            text_retry: Mutex::new(BTreeMap::new()),
            held: Mutex::new(BTreeSet::new()),
            text_idle: Mutex::new(TEXT_IDLE),
            text_max_pending: Mutex::new(TEXT_MAX_PENDING),
            first_pending: Mutex::new(BTreeMap::new()),
            pruned_upto: std::sync::atomic::AtomicU64::new(0),
            write_error: Mutex::new(None),
            text_rule: Mutex::new(None),
            view: RwLock::new(Arc::new(View {
                snapshots,
                delta: Arc::new(delta),
            })),
            state: Mutex::new(state),
            lock,
            write_lock: Mutex::new(()),
            fold_lock: Mutex::new(()),
        })
    }

    /// 克隆当前视图的 Arc；读锁只持这一瞬。
    pub fn view(&self) -> Arc<View> {
        self.view.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 只读模式：另一个实例持有锁（§3.7）。查询正常，写入返回 Err。
    pub fn readonly(&self) -> bool {
        self.lock.is_none()
    }

    fn publish(&self, v: View) {
        *self.view.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(v);
    }

    /// 追加日志 + 发布新视图，返回 seq。调用线程上同步完成（写后立即可读）。
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        target: Target,
        schema: &str,
        kind: OpKind,
        code: &str,
        text: &str,
        payload: Payload,
        boundary: u64,
    ) -> anyhow::Result<u64> {
        let seqs =
            self.apply_batch(&[Op::new(target, schema, kind, code, text, payload, boundary)])?;
        Ok(seqs[0])
    }

    /// 一批操作：先全部校验（无增量语义的整批拒、不占号），再逐条追加（各一次 write）、
    /// 最后发布一次视图。写锁跨「追加 + 发布」，与 fold 取水位互斥。
    pub fn apply_batch(&self, ops: &[Op]) -> anyhow::Result<Vec<u64>> {
        if self.readonly() {
            bail!(
                "只读模式：另一个实例持有 {}",
                self.dirs.state_dir.join("lock").display()
            );
        }
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut guard = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let Some(j) = guard.as_mut() else {
            bail!("只读模式");
        };
        for op in ops {
            check_schema_id(&op.schema)?;
        }
        let now = now_ms();
        let mut recs = Vec::with_capacity(ops.len());
        let mut entries = Vec::with_capacity(ops.len());
        for (i, op) in ops.iter().enumerate() {
            let rec = Record {
                seq: j.next_seq() + i as u64,
                ts_ms: if op.ts_ms == 0 { now } else { op.ts_ms },
                kind: op.kind,
                target: op.target,
                schema: op.schema.clone(),
                code: op.code.clone(),
                text: op.text.clone(),
                payload: op.payload.clone(),
                boundary: op.boundary,
            };
            rec.check_lens()?;
            let Some(e) = entry_from_record(&rec) else {
                bail!("操作 {:?} 没有增量语义，不写入", op.kind);
            };
            recs.push(rec);
            entries.push(e);
        }
        let mut seqs = Vec::with_capacity(recs.len());
        for rec in &recs {
            if let Err(e) = j.append(rec) {
                // 只记 IO 层的原因，不带记录内容
                let msg = format!("{:#}", e.root_cause());
                tracing::warn!("写用户数据日志失败（本次修改未保存）：{msg}");
                *self.write_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
                return Err(e);
            }
            seqs.push(rec.seq);
        }
        {
            let mut we = self.write_error.lock().unwrap_or_else(|e| e.into_inner());
            if we.is_some() {
                *we = None;
            }
        }
        let cur = self.view();
        let delta = cur.delta.with_all(entries);
        self.publish(View {
            snapshots: cur.snapshots.clone(),
            delta: Arc::new(delta),
        });
        let now = std::time::Instant::now();
        let mut lw = self.last_write.lock().unwrap_or_else(|e| e.into_inner());
        let mut fp = self.first_pending.lock().unwrap_or_else(|e| e.into_inner());
        for op in ops {
            let k = (op.target, op.schema.clone());
            fp.entry(k.clone()).or_insert(now);
            lw.insert(k, now);
        }
        Ok(seqs)
    }

    /// 带策略的折回（§3.5 次序；§3.8 批量操作都走它）：旧快照 + delta(seq ≤ upto) 叠加 →
    /// `policy` → 新快照（可为零条目）→ 换入 → 摘 delta → state。新快照内嵌 `max_seq = upto`，
    /// 所以换入即推进水位：清空 / 导入 / 淘汰之后，被取代的旧记录无论 state 在不在都不再回放。
    pub fn fold_with(
        &self,
        target: Target,
        schema: &str,
        policy: &mut dyn FnMut(Vec<SnapshotEntry>) -> Vec<SnapshotEntry>,
    ) -> anyhow::Result<()> {
        if self.readonly() {
            bail!("只读模式");
        }
        check_schema_id(schema)?;
        if let Err(e) = self.bind_by_rule(target, schema) {
            tracing::warn!("数据文件绑定失败：{e:#}");
        }
        if let Some(b) = self.text_binding(target, schema)
            && let TextFormat::Table(spec) = b.format
        {
            return self.fold_table_with(&b, spec, target, schema, policy);
        }
        if self.text_owned(target, schema) {
            bail!(
                "归文本管的目标不走 fold_with（清空 / 导入用 rewrite_text；未绑定时先 bind_text），免得绕过真源"
            );
        }
        let _f = self.fold_lock.lock().unwrap_or_else(|e| e.into_inner());
        // 水位与起点是一个原子动作：apply 持写锁跨「追加 + 发布」，所以这里拿到的视图
        // 一定含有全部 seq ≤ upto 的操作。
        let (upto, base) = {
            let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            let upto = match self
                .journal
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                Some(j) => j.next_seq() - 1,
                None => bail!("只读模式"),
            };
            (upto, self.view())
        };
        let key = (target, schema.to_string());
        let merged = merge_for_fold(
            base.snapshots.get(&key).map(|s| s.as_ref()),
            &base.delta,
            target,
            schema,
            upto,
        );
        let mut out = policy(merged);
        out.sort_by(|a, b| {
            (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str()))
        });
        out.dedup_by(|a, b| a.code == b.code && a.text == b.text);
        let dir = self.dirs.state_dir.join(schema);
        let new_snap = Arc::new(crate::snapshot::build(
            &dir,
            target_name(target),
            upto,
            out.into_iter(),
        )?);
        {
            let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            let cur = self.view();
            let mut snapshots = cur.snapshots.clone();
            snapshots.insert(key.clone(), new_snap);
            // 只摘 seq ≤ upto 的该目标条目：折回期间新到的操作留下（Review Focus 4）
            let delta = Arc::new(cur.delta.drop_folded_for_schema(target, schema, upto));
            self.publish(View { snapshots, delta });
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.targets.insert(
            target_key(target, schema),
            TargetState {
                folded_seq: upto,
                text_fp: None,
            },
        );
        st.next_seq = st.next_seq.max(upto + 1);
        st.save(&self.state_path)
    }

    // ───────────── 文本目标（P2a，设计 §3.5）─────────────

    fn bound_keys(&self) -> std::collections::BTreeSet<TargetKey> {
        self.text_bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// 目标归文本管：已绑定，或 state 记过文本指纹（重开后还没重新绑定）。后者的操作只能等
    /// 绑定后经 `fold_text` 写进文本——此前走 `fold_with` 会进段不进文本、清掉指纹，绑定时由
    /// 文本重建就把它们丢了。
    fn text_owned(&self, target: Target, schema: &str) -> bool {
        self.text_binding(target, schema).is_some()
            || self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .targets
                .get(&target_key(target, schema))
                .is_some_and(|t| t.text_fp.is_some())
    }

    pub fn text_binding(&self, target: Target, schema: &str) -> Option<TextBinding> {
        self.text_bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(target, schema.to_string()))
            .cloned()
    }

    /// 绑定文本文件，随即 `sync_text` 一次（文件比段新就由文本重建）。同步失败（隔离 / 读不了）
    /// 时绑定照样生效，之后的折回 / 同步会重试。只读实例只登记绑定、不同步（它不写快照与 state）。
    ///
    /// 表格文本目标的**采纳**（P5，§7.4 第 2 步）：从没归文本管过（未绑定、state 无指纹）、文件不在、
    /// 但引擎里已有该目标的数据（v5 及以前只在快照里）⇒ 先由当前视图渲染出文本再绑定。不能由
    /// 「文件不存在」推出「数据为空」——那会装上空段，把学到的数据清掉。
    pub fn bind_text(&self, target: Target, schema: &str, b: TextBinding) -> anyhow::Result<()> {
        check_schema_id(schema)?;
        let adopt = !self.readonly()
            && !self.text_owned(target, schema)
            && !b.path.exists()
            && self.has_data(target, schema);
        if adopt {
            // 先落成文本、成功了才登记绑定：失败时不留下一个「文件不在」的绑定，否则下一次
            // 折回 / 同步会把只在快照里的数据当成空（审查 I-1）
            self.adopt_view(&b, target, schema)?;
        }
        self.text_bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((target, schema.to_string()), b.clone());
        if self.readonly() || adopt {
            return Ok(());
        }
        self.sync_text(target, schema)?;
        Ok(())
    }

    /// 设定自动绑定规则（P5）。之后规则管的目标在维护、压实、`fold_with` 之前自动绑定。
    pub fn set_text_rule(&self, rule: TextRule) {
        *self.text_rule.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(rule));
    }

    /// 按规则绑定（已绑定 / 规则不管 ⇒ `Ok(false)`）。
    pub fn bind_by_rule(&self, target: Target, schema: &str) -> anyhow::Result<bool> {
        if self.text_binding(target, schema).is_some() {
            return Ok(false);
        }
        let rule = self
            .text_rule
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(b) = rule.and_then(|r| r(target, schema)) else {
            return Ok(false);
        };
        self.bind_text(target, schema, b)?;
        Ok(true)
    }

    /// 把有数据（段或待处理操作）、规则管、尚未绑定的目标都绑上；单个失败只 WARN。
    fn bind_all_by_rule(&self) {
        if self
            .text_rule
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
        {
            return;
        }
        for (t, s) in self.keys() {
            if let Err(e) = self.bind_by_rule(t, &s) {
                tracing::warn!("数据文件绑定失败（按已有数据提供查询）：{e:#}");
            }
        }
    }

    /// 引擎里有该目标的段或未折回操作。
    fn has_data(&self, target: Target, schema: &str) -> bool {
        let v = self.view();
        v.snapshots.contains_key(&(target, schema.to_string()))
            || v.delta.pending(target, schema) > 0
    }

    /// 由当前视图（段 + delta）渲染出文本、写盘、装段推进水位。写入复核「文件仍不存在」。
    fn adopt_view(&self, b: &TextBinding, target: Target, schema: &str) -> anyhow::Result<()> {
        let _f = self.fold_lock.lock().unwrap_or_else(|e| e.into_inner());
        // 两处同时绑定（后台维护与前台调用）都判成要采纳：后到的那个文件已在，不再写
        if b.path.exists() && self.text_owned(target, schema) {
            return Ok(());
        }
        let (upto, base) = self.upto_and_view()?;
        let entries = merge_for_fold(
            base.snapshots
                .get(&(target, schema.to_string()))
                .map(|s| s.as_ref()),
            &base.delta,
            target,
            schema,
            upto,
        );
        let content = Self::render_text(b, &entries);
        crate::text::fold::commit_text(
            &b.path,
            content.as_bytes(),
            Some(&crate::text::fold::fingerprint(None)),
        )?;
        tracing::info!(
            "{} 由已有数据生成（{} 条）",
            b.path.display(),
            entries.len()
        );
        let entries = Self::text_entries(content.as_bytes(), b)?;
        let fp = crate::text::fold::fingerprint(Some(content.as_bytes()));
        self.install_text_segment(target, schema, entries, upto, Some(upto), fp, Some(upto))
    }

    /// 整文件渲染（按绑定的格式）。
    fn render_text(b: &TextBinding, entries: &[SnapshotEntry]) -> String {
        match b.format {
            TextFormat::Words => {
                let rows: Vec<(String, String, u64, i32)> = entries
                    .iter()
                    .map(|e| (e.code.clone(), e.text.clone(), e.boundary, e.weight))
                    .collect();
                crate::text::render(&b.name, b.syllable_codes, &rows)
            }
            TextFormat::Table(spec) => crate::text::codec::render(spec, b.syllable_codes, entries),
        }
    }

    /// 文件此刻解析不了（已绑定、文件在、按格式解析失败）。
    pub fn text_isolated(&self, target: Target, schema: &str) -> bool {
        let Some(b) = self.text_binding(target, schema) else {
            return false;
        };
        match crate::text::fold::read_text(&b.path) {
            Ok(Some((bytes, _))) => Self::text_entries(&bytes, &b).is_err(),
            _ => false,
        }
    }

    /// 显式整体替换之前：当前文件解析不了 ⇒ 另存为 `.corrupt-<时间>` 并登记（§2.5：坏文件不被覆盖）。
    fn preserve_if_corrupt(b: &TextBinding) -> anyhow::Result<()> {
        if let Some((bytes, _)) = crate::text::fold::read_text(&b.path)?
            && Self::text_entries(&bytes, b).is_err()
            && let Some(kept) = crate::text::fold::preserve_corrupt(&b.path)?
        {
            crate::issues::report(
                &b.path,
                crate::issues::IssueKind::Rebuilt {
                    kept_as: crate::text::fold::kept_as(&kept),
                },
            );
        }
        Ok(())
    }

    /// 外部编辑中（暂停写回）⇒ Err：整写会被编辑器随后的保存盖掉，或盖掉用户正在改的内容。
    /// 须在持 `fold_lock` 时调（[`Self::hold_text`] 也取它），检查与写之间才插不进暂停。
    fn check_not_held(&self, b: &TextBinding, target: Target, schema: &str) -> anyhow::Result<()> {
        if self.is_held(target, schema) {
            bail!("正在外部编辑 {}，完成后再改", b.path.display());
        }
        Ok(())
    }

    /// 表格文本目标的 `fold_with`（清空 / 导入 / 淘汰）：由文件当前内容 + 待处理操作得全集，跑
    /// `policy`，整文件写回。文件解析不了 ⇒ Err、不写（自动清理不得覆盖坏文件，§2.5）。
    fn fold_table_with(
        &self,
        b: &TextBinding,
        spec: &'static crate::text::codec::TableSpec,
        target: Target,
        schema: &str,
        policy: &mut dyn FnMut(Vec<SnapshotEntry>) -> Vec<SnapshotEntry>,
    ) -> anyhow::Result<()> {
        let _f = self.fold_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_not_held(b, target, schema)?;
        let (upto, base) = self.upto_and_view()?;
        let cur = crate::text::fold::read_text(&b.path)?;
        let fp_before = crate::text::fold::fingerprint(cur.as_ref().map(|(x, _)| x.as_slice()));
        let file_entries = match &cur {
            Some((bytes, _)) => Self::text_entries(bytes, b)?,
            None => Vec::new(),
        };
        let ops: Vec<&Entry> = base
            .delta
            .prefix(target, schema, "")
            .into_iter()
            .filter(|e| e.seq <= upto)
            .collect();
        let mut out = policy(overlay_entries(file_entries, ops));
        out.sort_by(|a, b| {
            (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str()))
        });
        out.dedup_by(|a, b| a.code == b.code && a.text == b.text);
        let content = crate::text::codec::render(spec, b.syllable_codes, &out);
        crate::text::fold::commit_text(&b.path, content.as_bytes(), Some(&fp_before))?;
        let entries = Self::text_entries(content.as_bytes(), b)?;
        let fp = crate::text::fold::fingerprint(Some(content.as_bytes()));
        self.install_text_segment(target, schema, entries, upto, Some(upto), fp, Some(upto))
    }

    /// 清空一个目标（用户显式动作），返回清掉的条数。表格文本目标走整体替换（坏文件先另存留存），
    /// 其余走 `fold_with`。
    pub fn clear(&self, target: Target, schema: &str) -> anyhow::Result<usize> {
        if let Some(b) = self.text_binding(target, schema)
            && matches!(b.format, TextFormat::Table(_))
        {
            let n = {
                let v = self.view();
                merge_for_fold(
                    v.snapshots
                        .get(&(target, schema.to_string()))
                        .map(|s| s.as_ref()),
                    &v.delta,
                    target,
                    schema,
                    u64::MAX,
                )
                .len()
            };
            self.replace_text_entries(target, schema, Vec::new())?;
            return Ok(n);
        }
        let mut n = 0;
        self.fold_with(target, schema, &mut |v| {
            n = v.len();
            Vec::new()
        })?;
        Ok(n)
    }

    /// 表格文本目标的显式整体替换（清空 / 导入 / 还原）：`entries` 就是新全集，待处理操作一并取代。
    /// 当前文件解析不了 ⇒ 先另存留存再写。
    pub fn replace_text_entries(
        &self,
        target: Target,
        schema: &str,
        mut entries: Vec<SnapshotEntry>,
    ) -> anyhow::Result<()> {
        if self.readonly() {
            bail!("只读模式");
        }
        let b = self
            .text_binding(target, schema)
            .ok_or_else(|| anyhow::anyhow!("目标未绑定文本"))?;
        let _f = self.fold_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_not_held(&b, target, schema)?;
        Self::preserve_if_corrupt(&b)?;
        let (upto, _) = self.upto_and_view()?;
        entries.sort_by(|a, b| {
            (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str()))
        });
        entries.dedup_by(|a, b| a.code == b.code && a.text == b.text);
        let content = Self::render_text(&b, &entries);
        crate::text::fold::commit_text(&b.path, content.as_bytes(), None)?;
        let entries = Self::text_entries(content.as_bytes(), &b)?;
        let fp = crate::text::fold::fingerprint(Some(content.as_bytes()));
        self.install_text_segment(target, schema, entries, upto, Some(upto), fp, Some(upto))
    }

    /// 取「折回起点」：写锁下的水位与视图（同 `fold_with`）。
    fn upto_and_view(&self) -> anyhow::Result<(u64, Arc<View>)> {
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let upto = match self
            .journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            Some(j) => j.next_seq() - 1,
            None => bail!("只读模式"),
        };
        Ok((upto, self.view()))
    }

    /// 文本 → 段条目：按 (code, text) 排序去重（同键多行取文件里第一行），`order` = 行号。
    fn text_entries(bytes: &[u8], b: &TextBinding) -> anyhow::Result<Vec<SnapshotEntry>> {
        if let TextFormat::Table(spec) = b.format {
            let d = crate::text::codec::decode(bytes, &b.path, spec, b.lowercase, b.syllable_codes)
                .map_err(|e| {
                    tracing::warn!(
                        "文本 {} 隔离（{e:?}）：不往里写，按已有数据提供查询",
                        b.path.display()
                    );
                    crate::issues::report(&b.path, crate::issues::IssueKind::Isolated);
                    anyhow::anyhow!("文本 {} 解析失败：{e:?}", b.path.display())
                })?;
            crate::issues::report_parse(&b.path, d.bad_lines);
            return Ok(d.entries);
        }
        let dict = crate::text::parse(bytes, &b.path, b.lowercase).map_err(|e| {
            tracing::warn!(
                "文本 {} 隔离（{e:?}）：不往里折回，按已有段提供查询",
                b.path.display()
            );
            crate::issues::report(&b.path, crate::issues::IssueKind::Isolated);
            anyhow::anyhow!("文本 {} 解析失败：{e:?}", b.path.display())
        })?;
        crate::issues::report_parse(&b.path, dict.bad_lines);
        let mut v: Vec<SnapshotEntry> = dict
            .entries
            .into_iter()
            .map(|e| SnapshotEntry {
                code: e.code,
                text: e.text,
                weight: e.weight,
                boundary: e.boundary,
                count: 0,
                created_at: 0,
                last_used: 0,
                flags: 0,
                order: e.line as u32,
            })
            .collect();
        v.sort_by(|a, b| {
            (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str()))
        });
        v.dedup_by(|a, b| a.code == b.code && a.text == b.text);
        Ok(v)
    }

    /// 由文本条目建段并换入；`drop_upto` 给出时摘 delta 里该目标 `seq ≤` 它的条目；`folded`
    /// 给出时推进折回水位。state 记 `text_fp`。
    #[allow(clippy::too_many_arguments)]
    fn install_text_segment(
        &self,
        target: Target,
        schema: &str,
        entries: Vec<SnapshotEntry>,
        max_seq: u64,
        drop_upto: Option<u64>,
        fp: String,
        folded: Option<u64>,
    ) -> anyhow::Result<()> {
        let dir = self.dirs.state_dir.join(schema);
        let snap = Arc::new(crate::snapshot::build(
            &dir,
            target_name(target),
            max_seq,
            entries.into_iter(),
        )?);
        {
            let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            let cur = self.view();
            let mut snapshots = cur.snapshots.clone();
            snapshots.insert((target, schema.to_string()), snap);
            let delta = match drop_upto {
                Some(u) => Arc::new(cur.delta.drop_folded_for_schema(target, schema, u)),
                None => cur.delta.clone(),
            };
            self.publish(View { snapshots, delta });
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ts = st.targets.entry(target_key(target, schema)).or_default();
        if let Some(f) = folded {
            ts.folded_seq = ts.folded_seq.max(f);
        }
        ts.text_fp = Some(fp);
        if let Some(f) = folded {
            st.next_seq = st.next_seq.max(f + 1);
        }
        st.save(&self.state_path)
    }

    /// §3.5 文本目标折回（调用方已持 `fold_lock`）。
    fn fold_text_locked(
        &self,
        b: &TextBinding,
        target: Target,
        schema: &str,
    ) -> anyhow::Result<()> {
        use crate::text::PatchOp;
        if let TextFormat::Table(spec) = b.format {
            return self.fold_table_locked(b, spec, target, schema);
        }
        let (upto, base) = self.upto_and_view()?;
        let pending: Vec<&Entry> = base
            .delta
            .prefix(target, schema, "")
            .into_iter()
            .filter(|e| e.seq <= upto)
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        let ops: Vec<PatchOp> = pending
            .iter()
            .filter_map(|e| match e.op {
                DeltaOp::Upsert { weight, .. } => Some(PatchOp::Upsert {
                    code: e.code.clone(),
                    text: e.text.clone(),
                    boundary: e.boundary,
                    weight,
                }),
                DeltaOp::Delete => Some(PatchOp::Delete {
                    code: e.code.clone(),
                    text: e.text.clone(),
                }),
                DeltaOp::SetWeight(w) => Some(PatchOp::SetWeight {
                    code: e.code.clone(),
                    text: e.text.clone(),
                    weight: w,
                    ts_secs: e.ts_ms.div_euclid(1000),
                }),
                // 计数与时间不进文本（P2a 计划内裁定）
                DeltaOp::Incr(_) | DeltaOp::Touch(_) => None,
            })
            .collect();
        // 1. 读文件 + 指纹（读不了 ⇒ 本轮跳过；不存在 ⇒ 当空文件，Upsert 会新建）
        let cur = crate::text::fold::read_text(&b.path)?;
        let fp_before = crate::text::fold::fingerprint(cur.as_ref().map(|(x, _)| x.as_slice()));
        if ops.is_empty() && self.text_fp(target, schema).as_deref() == Some(fp_before.as_str()) {
            // 只有计数 / 时间、文件也没被手改：段不变，只摘 delta、推进水位
            {
                let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
                let cur = self.view();
                let delta = Arc::new(cur.delta.drop_folded_for_schema(target, schema, upto));
                self.publish(View {
                    snapshots: cur.snapshots.clone(),
                    delta,
                });
            }
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let ts = st.targets.entry(target_key(target, schema)).or_default();
            ts.folded_seq = ts.folded_seq.max(upto);
            st.next_seq = st.next_seq.max(upto + 1);
            return st.save(&self.state_path);
        }
        let (bytes, mtime) = cur.unwrap_or((Vec::new(), 0));
        let new_bytes = if ops.is_empty() {
            bytes
        } else {
            let out = crate::text::patch(
                &bytes,
                &b.path,
                b.lowercase,
                &ops,
                mtime,
                &b.name,
                b.syllable_codes,
            )
            .map_err(|e| {
                tracing::warn!("文本 {} 隔离（{e:?}）：不往里折回", b.path.display());
                crate::issues::report(&b.path, crate::issues::IssueKind::Isolated);
                anyhow::anyhow!("文本 {} 解析失败：{e:?}", b.path.display())
            })?;
            // 2–4. tmp + fsync → 复核原文件指纹 → .bak + rename + fsync 目录
            crate::text::fold::commit_text(&b.path, &out, Some(&fp_before))?;
            out
        };
        // 5–7. 由文本重建段 → 换入、摘 delta → state
        let entries = if new_bytes.is_empty() {
            Vec::new()
        } else {
            Self::text_entries(&new_bytes, b)?
        };
        let fp = if new_bytes.is_empty() && !b.path.is_file() {
            crate::text::fold::fingerprint(None)
        } else {
            crate::text::fold::fingerprint(Some(&new_bytes))
        };
        self.install_text_segment(target, schema, entries, upto, Some(upto), fp, Some(upto))
    }

    /// 表格文本目标折回（调用方已持 `fold_lock`）：待处理操作涉及的键，以**文件当前内容**为底按
    /// seq 叠加出终值，按键补丁写回（计数与时间都进文本）。手改过的值就是新的底。
    fn fold_table_locked(
        &self,
        b: &TextBinding,
        spec: &'static crate::text::codec::TableSpec,
        target: Target,
        schema: &str,
    ) -> anyhow::Result<()> {
        let (upto, base) = self.upto_and_view()?;
        let mut pending: Vec<&Entry> = base
            .delta
            .prefix(target, schema, "")
            .into_iter()
            .filter(|e| e.seq <= upto)
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        pending.sort_by_key(|e| e.seq);
        pending.dedup_by_key(|e| e.seq);
        let cur = crate::text::fold::read_text(&b.path)?;
        let fp_before = crate::text::fold::fingerprint(cur.as_ref().map(|(x, _)| x.as_slice()));
        let (bytes, _) = cur.unwrap_or((Vec::new(), 0));
        let file_entries = if bytes.is_empty() {
            Vec::new()
        } else {
            Self::text_entries(&bytes, b)?
        };
        let mut by_key: BTreeMap<(String, String), Vec<&Entry>> = BTreeMap::new();
        for op in pending {
            by_key
                .entry((op.code.clone(), op.text.clone()))
                .or_default()
                .push(op);
        }
        let mut file_map: BTreeMap<(String, String), SnapshotEntry> = file_entries
            .into_iter()
            .filter(|e| by_key.contains_key(&(e.code.clone(), e.text.clone())))
            .map(|e| ((e.code.clone(), e.text.clone()), e))
            .collect();
        let edits: BTreeMap<(String, String), Option<SnapshotEntry>> = by_key
            .into_iter()
            .map(|((code, text), ops)| {
                let base = file_map.remove(&(code.clone(), text.clone()));
                let v = apply_ops(base, &ops, &code, &text);
                ((code, text), v)
            })
            .collect();
        // 补丁顺带给出结果的解码：大文件省一遍全文解析
        let (out, decoded) = crate::text::codec::patch_decoded(
            &bytes,
            &b.path,
            spec,
            b.lowercase,
            b.syllable_codes,
            &edits,
        )
        .map_err(|e| {
            tracing::warn!("文本 {} 隔离（{e:?}）：不往里写", b.path.display());
            crate::issues::report(&b.path, crate::issues::IssueKind::Isolated);
            anyhow::anyhow!("文本 {} 解析失败：{e:?}", b.path.display())
        })?;
        crate::text::fold::commit_text(&b.path, &out, Some(&fp_before))?;
        crate::issues::report_parse(&b.path, decoded.bad_lines);
        let entries = decoded.entries;
        let fp = crate::text::fold::fingerprint(Some(&out));
        self.install_text_segment(target, schema, entries, upto, Some(upto), fp, Some(upto))
    }

    fn text_fp(&self, target: Target, schema: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .targets
            .get(&target_key(target, schema))
            .and_then(|t| t.text_fp.clone())
    }

    /// §3.5 文本目标折回：delta 里该目标 `seq ≤ 水位` 的操作按行补丁写回文本，再由文本重建段。
    /// 编辑器恰在期间保存 / 文件读不了 / 解析失败（隔离）⇒ `Err`，delta 与 state 不动。
    pub fn fold_text(&self, target: Target, schema: &str) -> anyhow::Result<()> {
        if self.readonly() {
            bail!("只读模式");
        }
        let b = self
            .text_binding(target, schema)
            .ok_or_else(|| anyhow::anyhow!("目标未绑定文本"))?;
        let _f = self.fold_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.fold_text_locked(&b, target, schema)
    }

    /// 同 [`Self::fold_text`]，但外部编辑中（暂停写回）就不写、返回 `Ok(false)`——在锁内判，
    /// 与 [`Self::hold_text`] 互斥。后台维护、压实、导入后的折回走这里。
    pub fn fold_text_unless_held(&self, target: Target, schema: &str) -> anyhow::Result<bool> {
        if self.readonly() {
            bail!("只读模式");
        }
        let b = self
            .text_binding(target, schema)
            .ok_or_else(|| anyhow::anyhow!("目标未绑定文本"))?;
        let _f = self.fold_lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_held(target, schema) {
            return Ok(false);
        }
        self.fold_text_locked(&b, target, schema)?;
        Ok(true)
    }

    /// 文件被手改 / 换掉 / 删除 ⇒ 由文本重建段（水位不回退），delta 不动——已折回的记录不回放，
    /// 删掉的行不复活，未折回的操作仍叠加。指纹没变 ⇒ `Ok(false)`。
    pub fn sync_text(&self, target: Target, schema: &str) -> anyhow::Result<bool> {
        if self.readonly() {
            bail!("只读模式");
        }
        let b = self
            .text_binding(target, schema)
            .ok_or_else(|| anyhow::anyhow!("目标未绑定文本"))?;
        let _f = self.fold_lock.lock().unwrap_or_else(|e| e.into_inner());
        // 读不了（被占用、权限、不是文件）也登记问题：外部编辑结束、重新载入时用户看得到原因
        let cur = crate::text::fold::read_text(&b.path).inspect_err(|e| {
            crate::issues::report(
                &b.path,
                crate::issues::IssueKind::Invalid {
                    message: format!("文件读不了，本次未载入：{e:#}"),
                },
            )
        })?;
        crate::issues::clear_invalid(&b.path);
        let fp = crate::text::fold::fingerprint(cur.as_ref().map(|(x, _)| x.as_slice()));
        let key = (target, schema.to_string());
        let (old_fp, folded) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.targets
                .get(&target_key(target, schema))
                .map(|t| (t.text_fp.clone(), t.folded_seq))
                .unwrap_or((None, 0))
        };
        let snap_wm = self.view().snapshots.get(&key).map(|s| s.max_seq);
        if old_fp.as_deref() == Some(fp.as_str()) && snap_wm.is_some() {
            return Ok(false);
        }
        if cur.is_none() && snap_wm.is_none() && old_fp.is_none() {
            // 从没用过的方案：没有文件、没有段，什么都不建（查询一个方案不该留下空目录）
            return Ok(false);
        }
        let entries = match &cur {
            Some((bytes, _)) => Self::text_entries(bytes, &b)?,
            None if old_fp.is_none() && self.has_data(target, schema) => {
                // 从没归文本管过的数据：文件不在不代表用户删了（§7.4），不装空段
                bail!("{} 不存在而已有数据尚未落成文本，不清空", b.path.display());
            }
            None => {
                crate::issues::clear(&b.path);
                Vec::new()
            }
        };
        let max_seq = folded.max(snap_wm.unwrap_or(0));
        self.install_text_segment(target, schema, entries, max_seq, None, fp, None)?;
        Ok(true)
    }

    /// 整文件替换（清空 / 导入 / 迁移，§3.8）：先折回待处理操作，再把文件写成 `rows`
    /// （就是新全集）、重建段、推进水位。不做指纹复核——这是用户明确的整体动作。
    pub fn rewrite_text(
        &self,
        target: Target,
        schema: &str,
        rows: &[(String, String, u64, i32)],
    ) -> anyhow::Result<()> {
        if self.readonly() {
            bail!("只读模式");
        }
        let b = self
            .text_binding(target, schema)
            .ok_or_else(|| anyhow::anyhow!("目标未绑定文本"))?;
        let _f = self.fold_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_not_held(&b, target, schema)?;
        // 先把待处理操作折回，迁移 / 导入的整写才不会盖掉它们。折回失败（文本隔离 / 编辑器竞争）
        // 不拦整体替换：`rows` 就是新全集（清空、还原是用户显式的整体动作），下面按水位把这些
        // 操作一并摘掉——否则文件一坏，清空 / 备份还原就永远做不成。
        if let Err(e) = self.fold_text_locked(&b, target, schema) {
            tracing::warn!("整文件替换前折回失败，待处理操作由新全集取代：{e:#}");
        }
        Self::preserve_if_corrupt(&b)?;
        let (upto, _) = self.upto_and_view()?;
        let content = crate::text::render(&b.name, b.syllable_codes, rows);
        crate::text::fold::commit_text(&b.path, content.as_bytes(), None)?;
        let entries = Self::text_entries(content.as_bytes(), &b)?;
        let fp = crate::text::fold::fingerprint(Some(content.as_bytes()));
        self.install_text_segment(target, schema, entries, upto, Some(upto), fp, Some(upto))
    }

    /// 文本目标的文件此刻能否往里补丁（不存在 = 能，会新建；读不了 / 解析不了 = 不能）。批量写入
    /// 之前先问一声：文件隔离时整批拒绝，不把一大批操作留在 delta 里干等。
    pub fn check_text_writable(&self, target: Target, schema: &str) -> anyhow::Result<()> {
        let Some(b) = self.text_binding(target, schema) else {
            return Ok(());
        };
        if let Some((bytes, _)) = crate::text::fold::read_text(&b.path)? {
            Self::text_entries(&bytes, &b)
                .map_err(|e| anyhow::anyhow!("数据文件格式有误（{e:#}），请先修复或删除它"))?;
        }
        Ok(())
    }

    /// 已绑定、有待处理操作、不在退避中，且距最后一次写入 ≥ `idle` 或最老的待处理写入已超过
    /// 待处理上限（连续打字从不空闲也要定期落进文本）的文本目标。
    fn idle_text_targets(&self, idle: std::time::Duration) -> Vec<TargetKey> {
        let v = self.view();
        let max_pending = *self
            .text_max_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let lw = self.last_write.lock().unwrap_or_else(|e| e.into_inner());
        let fp = self.first_pending.lock().unwrap_or_else(|e| e.into_inner());
        let retry = self.text_retry.lock().unwrap_or_else(|e| e.into_inner());
        let held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        let aged = |m: &BTreeMap<TargetKey, std::time::Instant>, k: &TargetKey, d| {
            m.get(k)
                .is_none_or(|t| now.saturating_duration_since(*t) >= d)
        };
        self.bound_keys()
            .into_iter()
            .filter(|k| {
                v.delta.pending(k.0, &k.1) > 0
                    && (aged(&lw, k, idle) || aged(&fp, k, max_pending))
                    && retry.get(k).is_none_or(|(_, at)| now >= *at)
                    && !held.contains(k)
            })
            .collect()
    }

    /// 在退避中（上次折回失败、还没到重试时刻）。
    fn in_text_backoff(&self, key: &TargetKey) -> bool {
        self.text_retry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .is_some_and(|(_, at)| std::time::Instant::now() < *at)
    }

    /// 暂停该文本目标的写回（外部编辑会话）。取 `fold_lock`：进行中的写回 / 整写先写完，之后的
    /// 都在锁内看到暂停。
    pub fn hold_text(&self, target: Target, schema: &str) {
        let _f = self.fold_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((target, schema.to_string()));
    }

    /// 恢复写回，并立即尝试折回一次（失败按退避处理）。
    pub fn release_text(&self, target: Target, schema: &str) {
        let was = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(target, schema.to_string()));
        if was && self.text_binding(target, schema).is_some() && self.pending(target, schema) > 0 {
            self.fold_text_logged(target, schema);
        }
    }

    pub fn is_held(&self, target: Target, schema: &str) -> bool {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(target, schema.to_string()))
    }

    /// 暂停写回中的文本目标。
    pub fn held_texts(&self) -> Vec<(Target, String)> {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// 折回空闲够久的文本目标；返回折回成功的目标数。单个目标失败（隔离 / 读不了 / 编辑器
    /// 竞争）只记 WARN 并进入退避，不拖住别的目标。
    pub fn maintain_text_idle(&self, idle: std::time::Duration) -> anyhow::Result<usize> {
        let mut n = 0;
        for (t, s) in self.idle_text_targets(idle) {
            if self.fold_text_logged(t, &s) {
                n += 1;
            }
        }
        Ok(n)
    }

    /// `fold_text`，失败记 WARN 并登记退避；成功清退避。
    fn fold_text_logged(&self, target: Target, schema: &str) -> bool {
        let key = (target, schema.to_string());
        let r = self.fold_text_unless_held(target, schema);
        let mut retry = self.text_retry.lock().unwrap_or_else(|e| e.into_inner());
        match r {
            Ok(false) => false,
            Ok(true) => {
                retry.remove(&key);
                if self.view().delta.pending(target, schema) == 0 {
                    self.first_pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&key);
                }
                true
            }
            Err(e) => {
                let fails = retry.get(&key).map_or(0, |(f, _)| *f) + 1;
                let wait = text_retry_backoff(fails);
                tracing::warn!(
                    "文本目标 {}:{schema} 折回失败（第 {fails} 次，{}s 后重试）：{e:#}",
                    target_name(target),
                    wait.as_secs()
                );
                retry.insert(key, (fails, std::time::Instant::now() + wait));
                false
            }
        }
    }

    /// 旧快照 + delta → 新快照 → 换入 → 摘 delta → state（设计 §3.5 次序）。
    pub fn fold(&self, target: Target, schema: &str) -> anyhow::Result<()> {
        if self.text_binding(target, schema).is_some() {
            return self.fold_text(target, schema);
        }
        self.fold_with(target, schema, &mut |v| v)
    }

    /// 叠加后按 (code, text) 升序遍历 code 以 `prefix` 开头的条目；`f` 返回 false 即停。
    ///
    /// 流式：快照按码分组边读边与 delta（按键分好组，条数受折回阈值约束）归并，不物化整表——
    /// 只取前几条（分页第一页、通配命中够数）时代价与读到的条数成正比。
    pub fn scan(
        &self,
        target: Target,
        schema: &str,
        prefix: &str,
        f: &mut dyn FnMut(SnapshotEntry) -> bool,
    ) {
        let v = self.view();
        let mut by_key: BTreeMap<(String, String), Vec<&Entry>> = BTreeMap::new();
        for e in v.delta.prefix(target, schema, prefix) {
            by_key
                .entry((e.code.clone(), e.text.clone()))
                .or_default()
                .push(e);
        }
        let mut dk = by_key.into_iter().peekable();
        let mut stopped = false;
        let mut cur: Option<String> = None;
        let mut group: Vec<SnapshotEntry> = Vec::new();
        if let Some(snap) = v.snapshots.get(&(target, schema.to_string())) {
            snap.for_each_prefix(prefix, &mut |e| {
                if cur.as_deref() != Some(e.code.as_str()) {
                    if let Some(c) = cur.take()
                        && !merge_code_group(std::mem::take(&mut group), &c, &mut dk, f)
                    {
                        stopped = true;
                        return false;
                    }
                    cur = Some(e.code.clone());
                }
                group.push(e);
                true
            });
        }
        if stopped {
            return;
        }
        if let Some(c) = cur.take()
            && !merge_code_group(group, &c, &mut dk, f)
        {
            return;
        }
        flush_delta_before(&mut dk, None, f);
    }

    /// 有数据（快照非空或 delta 有操作）的 (目标, 方案)。
    pub fn keys(&self) -> BTreeSet<(Target, String)> {
        let v = self.view();
        let mut out = v.delta.targets();
        out.extend(
            v.snapshots
                .iter()
                .filter(|(_, s)| s.entry_count > 0)
                .map(|(k, _)| k.clone()),
        );
        out
    }

    /// 条目数上界（快照条数 + 未折回操作数），O(1)。给淘汰之类「要全量扫描才知道该不该删」的
    /// 操作做门控：上界不超限就一定不用删。
    pub fn upper_bound(&self, target: Target, schema: &str) -> usize {
        let v = self.view();
        v.snapshots
            .get(&(target, schema.to_string()))
            .map_or(0, |s| s.entry_count as usize)
            + v.delta.pending(target, schema)
    }

    pub fn pending(&self, target: Target, schema: &str) -> usize {
        self.view().delta.pending(target, schema)
    }

    /// 折回全部有待处理操作的目标，再把日志裁到「最小未折回 seq」之前，并删掉已全部折回的封存日志。
    pub fn compact(&self) -> anyhow::Result<()> {
        if self.readonly() {
            bail!("只读模式");
        }
        self.bind_all_by_rule();
        // 归文本管的目标：已绑定的逐个折回，失败不拦压实（裁剪只到 delta 最小 seq，留在 delta
        // 里的操作不受影响）；未绑定的等绑定后再折回。
        for (t, s) in self.view().delta.targets() {
            if self.text_binding(t, &s).is_some() {
                // 退避中的（文件隔离 / 读不了）不在压实时反复重试；外部编辑中的不写回
                if !self.in_text_backoff(&(t, s.clone())) && !self.is_held(t, &s) {
                    self.fold_text_logged(t, &s);
                }
            } else if !self.text_owned(t, &s) {
                self.fold_with(t, &s, &mut |v| v)?;
            }
        }
        // 写锁下裁剪：裁剪是重写 + rename，期间不能有追加。
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut guard = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let Some(j) = guard.as_mut() else {
            bail!("只读模式");
        };
        let keep_from = self
            .view()
            .delta
            .min_seq()
            .unwrap_or(u64::MAX)
            .min(j.next_seq());
        // 裁不动（卡在折不回的目标上）就不重写：整份重写 ≥ 8 MB 的日志换不来任何空间
        if keep_from > self.pruned_upto.load(std::sync::atomic::Ordering::Acquire) {
            j.prune_before(keep_from)?;
            self.pruned_upto
                .store(keep_from, std::sync::atomic::Ordering::Release);
        }
        let jdir = self.dirs.state_dir.join("journal");
        for p in std::fs::read_dir(&jdir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_str().is_some_and(|s| s.ends_with(".wjl.sealed")))
        {
            if Journal::tail_seq(&p).map_or(true, |t| t < keep_from) {
                let _ = std::fs::remove_file(&p);
            }
        }
        Ok(())
    }

    fn journal_len(&self) -> u64 {
        self.jpath
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len())
    }

    pub fn needs_maintain_with(&self, threshold: usize, max_bytes: u64) -> bool {
        if self.readonly() {
            return false;
        }
        let v = self.view();
        v.delta
            .targets()
            .iter()
            .any(|k| v.delta.pending(k.0, &k.1) >= threshold && !self.text_owned(k.0, &k.1))
            || !self.idle_text_targets(self.text_idle()).is_empty()
            || (self.journal_len() > max_bytes
                && v.delta.min_seq().is_none_or(|m| {
                    m > self.pruned_upto.load(std::sync::atomic::Ordering::Acquire)
                }))
            || self.has_unbound_ruled(&v)
    }

    /// 有待处理操作、规则管、却还没绑定的目标（要起一次维护把它绑上）。
    fn has_unbound_ruled(&self, v: &View) -> bool {
        let Some(rule) = self
            .text_rule
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return false;
        };
        v.delta
            .targets()
            .iter()
            .any(|(t, s)| self.text_binding(*t, s).is_none() && rule(*t, s).is_some())
    }

    pub fn needs_maintain(&self) -> bool {
        self.needs_maintain_with(FOLD_THRESHOLD, JOURNAL_MAX_BYTES)
    }

    /// 折回 `pending ≥ threshold` 的目标；日志超 `max_bytes` 则压实。返回折回的目标数。
    pub fn maintain_with(&self, threshold: usize, max_bytes: u64) -> anyhow::Result<usize> {
        self.bind_all_by_rule();
        let mut n = 0;
        let v = self.view();
        for (t, s) in v.delta.targets() {
            if v.delta.pending(t, &s) >= threshold && !self.text_owned(t, &s) {
                self.fold_with(t, &s, &mut |v| v)?;
                n += 1;
            }
        }
        n += self.maintain_text_idle(self.text_idle())?;
        if self.journal_len() > max_bytes {
            self.compact()?;
        }
        Ok(n)
    }

    fn text_idle(&self) -> std::time::Duration {
        *self.text_idle.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[doc(hidden)]
    pub fn set_text_max_pending_for_test(&self, d: std::time::Duration) {
        *self
            .text_max_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = d;
    }

    #[doc(hidden)]
    pub fn set_text_idle_for_test(&self, d: std::time::Duration) {
        *self.text_idle.lock().unwrap_or_else(|e| e.into_inner()) = d;
    }

    /// 低频后台节拍：每 `every` 看一次是否需要维护（含文本目标空闲折回），需要就起后台折回。
    /// 只持 `Weak`，引擎释放后下一拍退出。只读实例不起。
    pub fn spawn_ticker(
        self: &Arc<Self>,
        every: std::time::Duration,
    ) -> Option<std::thread::JoinHandle<()>> {
        if self.readonly() {
            return None;
        }
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("userdata-tick".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(every);
                    let Some(me) = weak.upgrade() else { return };
                    if let Err(e) = me.sync_journal() {
                        tracing::warn!("日志落盘失败：{e:#}");
                    }
                    if me.needs_maintain() {
                        me.spawn_maintain();
                    }
                }
            })
            .ok()
    }

    /// 最近一次写日志失败的原因（§8：设置页提示「本次修改未保存」）；之后写成功即清。
    pub fn write_error(&self) -> Option<String> {
        self.write_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 日志有新写入就落盘一次（后台节拍每拍调）。只读实例没有日志写端 ⇒ `Ok(false)`。
    pub fn sync_journal(&self) -> anyhow::Result<bool> {
        // 锁内只取句柄，fsync 在锁外：追加（按键路径）不陪等
        let handle = match self
            .journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            Some(j) => j.take_sync_handle()?,
            None => return Ok(false),
        };
        let Some(h) = handle else {
            return Ok(false);
        };
        if let Err(e) = h.sync_data() {
            if let Some(j) = self
                .journal
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_mut()
            {
                j.mark_dirty();
            }
            return Err(anyhow::Error::from(e).context("刷日志"));
        }
        Ok(true)
    }

    pub fn maintain(&self) -> anyhow::Result<usize> {
        self.maintain_with(FOLD_THRESHOLD, JOURNAL_MAX_BYTES)
    }

    /// 后台折回（§3.5：折回在独立线程）。只读或已有一个在跑 ⇒ None。
    pub fn spawn_maintain_with(
        self: &Arc<Self>,
        threshold: usize,
        max_bytes: u64,
    ) -> Option<std::thread::JoinHandle<()>> {
        use std::sync::atomic::Ordering;
        if self.readonly() || self.maintaining.swap(true, Ordering::AcqRel) {
            return None;
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("userdata-fold".into())
            .spawn(move || {
                if let Err(e) = me.maintain_with(threshold, max_bytes) {
                    tracing::warn!("用户数据折回失败: {e}");
                }
                me.maintaining.store(false, Ordering::Release);
            });
        match spawned {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!("起折回线程失败: {e}");
                self.maintaining.store(false, Ordering::Release);
                None
            }
        }
    }

    pub fn spawn_maintain(self: &Arc<Self>) -> Option<std::thread::JoinHandle<()>> {
        self.spawn_maintain_with(FOLD_THRESHOLD, JOURNAL_MAX_BYTES)
    }

    /// 某键叠加后的条目（快照 + delta）。
    pub fn get(
        &self,
        target: Target,
        schema: &str,
        code: &str,
        text: &str,
    ) -> Option<SnapshotEntry> {
        let v = self.view();
        let base = v
            .snapshots
            .get(&(target, schema.to_string()))
            .and_then(|s| s.exact(code).into_iter().find(|e| e.text == text));
        apply_ops(
            base,
            &v.delta.ops_for(target, schema, code, text),
            code,
            text,
        )
    }

    /// 某码下叠加后的全部条目，按 text 升序。
    pub fn exact(&self, target: Target, schema: &str, code: &str) -> Vec<SnapshotEntry> {
        let v = self.view();
        let base = v
            .snapshots
            .get(&(target, schema.to_string()))
            .map(|s| s.exact(code))
            .unwrap_or_default();
        overlay_entries(base, v.delta.exact(target, schema, code))
    }

    /// 调试 / 测试用：`(text, weight)`，权重降序、同权按 text。
    pub fn query_exact(&self, target: Target, schema: &str, code: &str) -> Vec<(String, i32)> {
        let mut v: Vec<(String, i32)> = self
            .exact(target, schema, code)
            .into_iter()
            .map(|e| (e.text, e.weight))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
