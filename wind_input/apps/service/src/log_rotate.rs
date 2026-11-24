//! 日志滚动命名方案：**每份都带序号，`.1` 恒为最新**，序号插在扩展名之前。
//!
//! ```text
//! wind_input.1.log   ← 当前这次运行（最新）
//! wind_input.2.log   ← 上一次运行
//! wind_input.3.log   ← 再上一次
//! ```
//!
//! 当前这份也带序号，是为了用户打包发来的日志一眼可读：数字越小越新，1 是最新，
//! 与设置程序的 `wind_setting.N.log` 同一规则。
//!
//! `file-rotate` 的模型是「无后缀主文件 + 带后缀历史」，于是把 `wind_input.1.log` 整个交给它当
//! 主文件，库的内部序号 n 映射到磁盘 n+1，换算只走 [`LogIndex::disk`] / [`LogIndex::from_disk`]。
//! 除命名外，滚动与淘汰语义与 `AppendCount` 一致。
//!
//! trait 的两个默认方法必须成对覆盖：[`Representation::to_path`] 决定写出的文件名，
//! [`SuffixScheme::scan_suffixes`] 决定启动时认回哪些文件；只改前者旧日志永不被淘汰。

use file_rotate::compression::Compression;
use file_rotate::suffix::{Representation, SuffixScheme};
use file_rotate::{ContentLimit, FileRotate, SuffixInfo};
use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 磁盘上「当前这次运行」那份日志的序号。历史从 `CURRENT_INDEX + 1` 起。
///
/// 它同时是 `init_logger` 拼主文件名用的那个数字，两处必须同源。
pub const CURRENT_INDEX: usize = 1;

/// 滚动序号（库的内部序号，非磁盘序号）。取值从 1 起，数字越大越旧；内部 1 = 磁盘 `.2`。
///
/// 与磁盘序号差 [`CURRENT_INDEX`]，换算只走 [`LogIndex::disk`] / [`LogIndex::from_disk`]，
/// 别在别处手写 ±1。[`Representation`] 要求 `Ord` 按新→旧排序，`usize` 自然序恰好满足。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogIndex(usize);

impl LogIndex {
    /// 内部序号 → 磁盘序号。
    fn disk(&self) -> usize {
        self.0 + CURRENT_INDEX
    }

    /// 磁盘序号 → 内部序号。
    ///
    /// `CURRENT_INDEX` 本身返回 `None`：那是当前文件，认成历史会被算进淘汰队列。
    fn from_disk(n: usize) -> Option<Self> {
        n.checked_sub(CURRENT_INDEX)
            .filter(|k| *k > 0)
            .map(LogIndex)
    }
}

impl fmt::Display for LogIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.disk())
    }
}

/// 把主文件路径拆成 `("wind_input", Some("log"))`。
///
/// 主文件末尾的 `.1` 是序号占位，必须剥掉，否则轮转写出 `wind_input.1.2.log` 且扫描认不回来。
fn split_stem_ext(basepath: &Path) -> (String, Option<String>) {
    let stem = basepath
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = basepath
        .extension()
        .map(|s| s.to_string_lossy().into_owned());
    // 末尾是 `.<纯数字>` 才剥：容忍调用方传来无序号的老路径（迁移期与测试都会出现）。
    let stem = match stem.rsplit_once('.') {
        Some((head, tail)) if !head.is_empty() && tail.parse::<usize>().is_ok() => head.to_string(),
        _ => stem,
    };
    (stem, ext)
}

impl Representation for LogIndex {
    /// 主文件 `/dir/wind_input.1.log` + 内部序号 `1` → `/dir/wind_input.2.log`
    ///
    /// 覆盖默认实现（默认是无脑追加 `.{suffix}`，即 `wind_input.1.log.1`）。
    fn to_path(&self, basepath: &Path) -> PathBuf {
        let (stem, ext) = split_stem_ext(basepath);
        let n = self.disk();
        let name = match ext {
            Some(ext) => format!("{stem}.{n}.{ext}"),
            // 无扩展名时退化成追加序号，与默认实现同形
            None => format!("{stem}.{n}"),
        };
        basepath.with_file_name(name)
    }
}

/// 与 `AppendCount` 等价的滚动方案，但序号落在扩展名之前。
///
/// `max_files` 是**不含主文件**的旧文件数上限：`new(10)` 允许
/// `wind_input.1.log`（当前）与 `wind_input.2.log` … `wind_input.11.log` 共存。
pub struct AppendCountBeforeExt {
    max_files: usize,
}

impl AppendCountBeforeExt {
    pub fn new(max_files: usize) -> Self {
        Self { max_files }
    }
}

impl SuffixScheme for AppendCountBeforeExt {
    type Repr = LogIndex;

    /// 滚动时序号 +1；主文件（`suffix == None`，磁盘上的 `.1`）滚成磁盘 `.2`。
    ///
    /// 目标已存在时 `file-rotate` 会拿目标后缀再调一次本函数，从而级联把
    /// `.1→.2`、`.2→.3` 依次推开——这正是「+1」能自然成立的原因。
    fn rotate_file(
        &mut self,
        _basepath: &Path,
        _newest_suffix: Option<&LogIndex>,
        suffix: &Option<LogIndex>,
    ) -> io::Result<LogIndex> {
        Ok(match suffix {
            Some(s) => LogIndex(s.0 + 1),
            None => LogIndex(1),
        })
    }

    /// `suffix` 是**磁盘上**那个数字，故要经 [`LogIndex::from_disk`] 换算。
    /// `.1`（当前文件）返回 `None` —— 它不是历史。
    fn parse(&self, suffix: &str) -> Option<LogIndex> {
        suffix.parse::<usize>().ok().and_then(LogIndex::from_disk)
    }

    /// `file_number` 从 0 开始（0 = 最新的那个旧文件）。
    fn too_old(&self, _suffix: &LogIndex, file_number: usize) -> bool {
        file_number >= self.max_files
    }

    /// 扫描既存的 `{stem}.{N}.{ext}`。
    ///
    /// 必须覆盖：默认实现只认 `{文件名}.{后缀}`（即 `wind_input.log.N`），
    /// 对我们写出的 `wind_input.N.log` 一个都认不出来。
    fn scan_suffixes(&self, basepath: &Path) -> BTreeSet<SuffixInfo<LogIndex>> {
        let mut found = BTreeSet::new();
        let (stem, ext) = split_stem_ext(basepath);

        // 相对路径时补上 cwd，与默认实现的行为保持一致
        let abs;
        let basepath = if basepath.is_relative() {
            let Ok(cwd) = std::env::current_dir() else {
                return found;
            };
            abs = cwd.join(basepath);
            &abs
        } else {
            basepath
        };
        let Some(parent) = basepath.parent() else {
            return found;
        };
        let Ok(entries) = std::fs::read_dir(parent) else {
            return found;
        };

        let prefix = format!("{stem}.");
        let suffix_ext = ext.map(|e| format!(".{e}"));

        for entry in entries.filter_map(Result::ok) {
            if !entry.path().is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();

            // 剥掉 `{stem}.` 前缀
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            // 再剥掉 `.{ext}` 后缀，中间剩下的必须是纯数字
            let num = match &suffix_ext {
                Some(se) => match rest.strip_suffix(se.as_str()) {
                    Some(n) => n,
                    None => continue,
                },
                None => rest,
            };
            if let Some(idx) = self.parse(num) {
                found.insert(SuffixInfo {
                    suffix: idx,
                    compressed: false,
                });
            }
        }
        found
    }
}

/// 服务启动时强制滚动一次日志：上一次运行的内容整体推到 `.2`，本次从空的 `.1` 写起。
///
/// 仅在旧文件非空时滚动：`rotate()` 对不存在的文件会报错，空文件滚动只白占一个序号。
/// 序号并非「一个序号 = 一次启动」：写满 `log_max_size_mb` 同样会滚动。
pub fn rotate_on_startup(rotate: &mut FileRotate<AppendCountBeforeExt>, log_path: &Path) {
    if std::fs::metadata(log_path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
        && let Err(e) = rotate.rotate()
    {
        // 滚动失败不阻断启动：继续往原文件追加，日志内容仍完整，只是没分段。
        // 此处 subscriber 尚未 init，只能走 stderr。
        eprintln!("[WindInput] startup log rotate failed: {e}");
    }
}

/// 一次性迁移历史命名，best-effort，失败只影响旧日志。
///
/// 经历过两代命名，都要接：
///
/// ```text
/// 一代：wind_input.log.N   （file-rotate 默认，扩展名被数字顶掉）
/// 二代：wind_input.log     ← 当前      + wind_input.N.log  ← 历史
/// 现在：wind_input.1.log   ← 当前      + wind_input.{N+1}.log
/// ```
///
/// 判据是无序号的 `wind_input.log` 在不在（新命名下永远没有它）；只看有没有 `.N.log` 会让每次
/// 启动都把序号推一格。先倒序推 `.N` → `.{N+1}` 腾出 `.1`，再搬当前文件。一代那批最后处理，
/// 目标被占则跳过。
///
/// 可在若干版本后删除（存量目录都迁移完之后）。
pub fn migrate_legacy_suffix(log_path: &Path) {
    let (stem, ext) = split_stem_ext(log_path);
    let Some(ext) = ext else { return };
    let Some(parent) = log_path.parent() else {
        return;
    };
    // 二代布局里那个无序号的当前文件。
    let unnumbered = parent.join(format!("{stem}.{ext}"));

    // 二代 → 现在。只有 `wind_input.log` 还在时才做，理由见函数文档。
    if unnumbered.is_file() {
        // 倒序推：先动最大的序号，否则 `.2 → .3` 会覆盖还没搬走的 `.3`。
        let mut existing: Vec<usize> = scan_disk_indices(parent, &stem, &ext);
        existing.sort_unstable_by(|a, b| b.cmp(a));
        for n in existing {
            let from = parent.join(format!("{stem}.{n}.{ext}"));
            let to = parent.join(format!("{stem}.{}.{ext}", n + 1));
            let _ = std::fs::rename(from, to);
        }
        let _ = std::fs::rename(
            &unnumbered,
            parent.join(format!("{stem}.{CURRENT_INDEX}.{ext}")),
        );
    }

    // 一代 → 现在。`wind_input.log.N` 里的 N 是「第 N 老的历史」，对应现在的 `.{N+1}`。
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let legacy_prefix = format!("{stem}.{ext}.");
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(num) = name.strip_prefix(&legacy_prefix) else {
            continue;
        };
        let Ok(n) = num.parse::<usize>() else {
            continue; // 只认纯数字，别误伤 .log.bak 之类
        };
        let target = parent.join(format!("{stem}.{}.{ext}", n + CURRENT_INDEX));
        if target.exists() {
            continue;
        }
        let _ = std::fs::rename(entry.path(), target);
    }
}

/// 扫出目录里既存的 `{stem}.{N}.{ext}` 的 N（不含判断新旧，纯列举）。
fn scan_disk_indices(parent: &Path, stem: &str, ext: &str) -> Vec<usize> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let prefix = format!("{stem}.");
    let suffix = format!(".{ext}");
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.strip_prefix(&prefix)
                .and_then(|r| r.strip_suffix(suffix.as_str()))
                .and_then(|n| n.parse::<usize>().ok())
        })
        .collect()
}

/// 主日志的写端：在 `FileRotate` 外面包一层，让日志文件被外部删掉或清空后能自己接回来。
///
/// 裸用 `FileRotate` 时：删历史会让下一次轮转撞上它构造时记下的文件而 panic（release 是
/// `panic = "abort"`，服务崩溃）；删当前则此后日志写进已摘名的幽灵文件。对策同 TSF 的
/// `CFileLogger::_ResyncFile`：轮转由这里按大小触发且每次先重建内层（重扫磁盘）；每
/// [`RESYNC_INTERVAL`] 对一次表，文件没了就重开，还在就以真实大小校正计数。
pub struct SelfHealingLog {
    path: PathBuf,
    max_files: usize,
    max_bytes: u64,
    inner: FileRotate<AppendCountBeforeExt>,
    /// 当前文件已写字节数（轮转判据）。会被 [`Self::resync`] 校正为磁盘真实大小。
    count: u64,
    last_check: Instant,
    check_interval: Duration,
}

/// 对表间隔。与 TSF 的 `FILE_CHECK_INTERVAL_MS` 同为一秒：被删后最多丢一秒的日志。
pub const RESYNC_INTERVAL: Duration = Duration::from_secs(1);

impl SelfHealingLog {
    /// 打开主日志并做一次启动轮转（见 [`rotate_on_startup`]）。
    pub fn new(path: &Path, max_files: usize, max_bytes: u64) -> Self {
        let mut inner = Self::open(path, max_files);
        rotate_on_startup(&mut inner, path);
        let mut log = Self {
            path: path.to_path_buf(),
            max_files,
            max_bytes,
            inner,
            count: 0,
            last_check: Instant::now(),
            check_interval: RESYNC_INTERVAL,
        };
        log.resync();
        log
    }

    fn open(path: &Path, max_files: usize) -> FileRotate<AppendCountBeforeExt> {
        FileRotate::new(
            path,
            AppendCountBeforeExt::new(max_files),
            ContentLimit::None,
            Compression::None,
            None,
        )
    }

    /// 重建内层：重新扫描历史序号、按原路径重开当前文件（目录没了也会建回来）。
    fn reopen(&mut self) {
        self.inner = Self::open(&self.path, self.max_files);
    }

    fn resync(&mut self) {
        self.last_check = Instant::now();
        match std::fs::metadata(&self.path) {
            Ok(m) => self.count = m.len(),
            // 被删了（或 Windows 上处于删除挂起、拿不到属性）：按原路径重开。
            Err(_) => {
                self.reopen();
                self.count = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
            }
        }
    }

    fn rotate(&mut self) {
        // 先重扫再轮转：磁盘上的历史可能刚被人删过，见类型文档。
        self.reopen();
        if self.inner.rotate().is_err() {
            // 轮转失败时内层的文件句柄可能已被 take 掉，后续写入会静默落空；
            // 重开一次，宁可这份文件超长也别丢日志。
            self.reopen();
        }
        self.count = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
    }
}

impl io::Write for SelfHealingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.last_check.elapsed() >= self.check_interval {
            self.resync();
        }
        // 按整条日志判定，不像 file-rotate 那样把一行劈成两半写进两个文件。
        if self.count > 0 && self.count + buf.len() as u64 > self.max_bytes {
            self.rotate();
        }
        self.inner.write_all(buf)?;
        self.count += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
#[path = "log_rotate_tests.rs"]
mod tests;

/// TSF 日志的专属子目录名。跨语言契约，必须与 C++ 侧 `WIND_LOG_SUBDIR_NAME`
/// （`wind_tsf/include/FileLogger.h`）逐字一致，见 `subdir_name_matches_cpp_header`。
const TSF_LOG_SUBDIR: &str = "tsf_log";

/// 清理 TSF DLL 留下的过期日志（`wind_tsf.<宿主名>.<pid>.log` 及其 `.old`）。
///
/// 传入 `logs` 根目录，扫两层：`logs/tsf_log/`（当前落点）与 `logs/`（老版本平铺的存量）。
/// 在 core 做而不在 DLL：DLL 的 `CFileLogger::Init` 跑在 loader lock 之下，不能做目录遍历。
/// 判据是修改时间而非 pid 存活：pid 会复用，刚退出宿主的日志正是排查现场。失败一律忽略。
pub fn prune_stale_tsf_logs(log_dir: &Path, max_age: std::time::Duration) -> usize {
    prune_tsf_logs_in(&log_dir.join(TSF_LOG_SUBDIR), max_age) + prune_tsf_logs_in(log_dir, max_age)
}

/// 在**单个**目录里清理过期的 TSF 日志。不递归——调用方显式指定要扫哪几层。
fn prune_tsf_logs_in(dir: &Path, max_age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // 只碰 TSF 自己那批：前缀与 C++ `WIND_LOG_FILE_PREFIX` 对齐，带 `.` 免误伤别的文件。
        if !name.starts_with("wind_tsf.") || !name.ends_with(".log") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > max_age);
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
#[path = "log_rotate_prune_tsf_tests.rs"]
mod prune_tsf_tests;
