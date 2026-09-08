//! 日志滚动命名方案：**每份都带序号，`.1` 恒为最新**，序号插在扩展名之前。
//!
//! ```text
//! wind_input.1.log   ← 当前这次运行（最新）
//! wind_input.2.log   ← 上一次运行
//! wind_input.3.log   ← 再上一次
//! ```
//!
//! # 为什么当前这份也要带序号
//!
//! 早先是「`wind_input.log` 是当前，`.1` 起为历史」。那套在**收日志的时候**会出事：
//! 用户把整个 logs 目录打包发过来，里面躺着 `wind_input.log` 与一串 `.N.log`，
//! 而"没有序号的那个才是最新"完全不是自明的——按名字排序时它还排在 `.1` 后面。
//! 实际收到的包里常常只有 `.1.log`（用户以为 1 就是最新）。
//!
//! 全部带序号之后规则只剩一句话：**数字越小越新，1 是最新**。与设置程序的
//! `wind_setting.1.log` … `.5.log` 完全一致，两份日志一个规则。
//!
//! # 实现：内部序号与磁盘序号差 1
//!
//! `file-rotate` 的模型是「一个无后缀的主文件 + 若干带后缀的历史」，主文件路径由
//! 调用方给定、库直接往里写。要让主文件叫 `wind_input.1.log`，就把那个名字整个交给
//! 它当"主文件"，再让库的内部序号 n 映射到磁盘上的 n+1（见 [`LogIndex::disk`]）。
//! 映射只有 [`LogIndex::disk`] / [`LogIndex::from_disk`] 两个出口，别在别处手写 ±1。
//!
//! 除命名外，滚动与淘汰语义与 `AppendCount` 一致（序号越大越旧，超出 `max_files` 的删除）。
//!
//! 注意 trait 的两个默认方法**必须成对覆盖**：[`Representation::to_path`] 决定写出去
//! 的文件名，[`SuffixScheme::scan_suffixes`] 决定启动时能认回哪些既存文件。只改前者
//! 会让扫描认不出自己上次写的文件，旧日志既不参与序号推进也永不被淘汰，最终堆满目录。

use file_rotate::suffix::{Representation, SuffixScheme};
use file_rotate::{FileRotate, SuffixInfo};
use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// 磁盘上「当前这次运行」那份日志的序号。历史从 `CURRENT_INDEX + 1` 起。
///
/// 它同时是 `init_logger` 拼主文件名用的那个数字——两处必须同源，否则库写 `.1`、
/// 调用方开 `.log`，日志会落进两个文件。
pub const CURRENT_INDEX: usize = 1;

/// 滚动序号（**库的内部序号**，非磁盘序号）。取值从 1 起，数字越大越旧；
/// 内部 1 = 磁盘 `.2` = 最新的那份历史。
///
/// ⚠️ 内部序号与磁盘序号差 [`CURRENT_INDEX`]：库认为「主文件无后缀、历史从 1 起」，
/// 而我们要磁盘上「主文件是 `.1`、历史从 `.2` 起」。换算只走 [`LogIndex::disk`] /
/// [`LogIndex::from_disk`] 两个出口，别在别处手写 ±1——这类偏移一旦散开，
/// 症状是某个序号被跳过或两份日志互相覆盖，而两者都只在真机跑上几天才看得出来。
///
/// [`Representation`] 要求 `Ord` 按「新→旧」排序（最新的最小），`usize` 的自然序
/// 恰好满足，故直接 derive。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogIndex(usize);

impl LogIndex {
    /// 内部序号 → 磁盘序号。
    fn disk(&self) -> usize {
        self.0 + CURRENT_INDEX
    }

    /// 磁盘序号 → 内部序号。
    ///
    /// `CURRENT_INDEX` 本身返回 `None`：那是**当前文件**，不是历史。认成历史的话，
    /// 启动扫描会把正在写的这份也算进淘汰队列。
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
/// ★ 主文件本身带序号（`wind_input.1.log`），末尾那个 `.1` 是**序号占位**而不是文件名
/// 的一部分，必须剥掉——不剥的话轮转会写出 `wind_input.1.2.log`，而且 `scan_suffixes`
/// 又认不回来，于是每次启动都新造一批永不淘汰的文件。
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
/// 这样 `wind_input.1.log` 恒等于「当前这次运行」，排查时不必在混着多次重启的大文件里
/// 翻找分界点，也不需要另做「清空日志」的入口——`FileRotate` 常驻持有该文件句柄，
/// 从外部删除只会留下一个已摘名的幽灵 inode，后续日志全写进去且看不见。
///
/// 仅在旧文件非空时滚动：首次启动没有 `wind_input.1.log`，而 `rotate()` 内部是
/// `fs::rename(old, new)?`，对不存在的文件会直接报错；空文件滚动也只是白占一个序号，
/// 把真正有用的历史更快挤出保留窗口。
///
/// 注意：序号并非「一个序号 = 一次启动」——本次运行写满 `log_max_size_mb` 同样会滚动，
/// 此时 `.2` 是本次运行的前半段而非上一次运行。
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
/// ★ **判据是「无序号的 `wind_input.log` 在不在」**：新命名下永远不会有这个文件，
/// 它在就说明还是二代布局。不能只看「有没有 `.N.log`」——新命名下那些一直都在，
/// 那样判会让每次启动都把整串序号推一格，几次之后历史全被挤掉。
///
/// 顺序也是判据的一部分：先把 `.N` 倒序推到 `.{N+1}`（腾出 `.1`），再把无序号的当前
/// 文件搬进 `.1`。反过来做会撞车。
///
/// 一代那批放在最后处理，目标被占则跳过：那是更早、更不重要的历史，让位给二代。
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

#[cfg(test)]
mod tests {
    use super::*;
    use file_rotate::ContentLimit;
    use file_rotate::compression::Compression;
    use std::io::Write;

    /// 造一个已含内容的日志文件，模拟「上一次运行留下的日志」。
    fn seed(path: &Path, content: &str) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn make_rotate(path: &Path, max_files: usize) -> FileRotate<AppendCountBeforeExt> {
        FileRotate::new(
            path,
            AppendCountBeforeExt::new(max_files),
            ContentLimit::Bytes(10 * 1024 * 1024),
            Compression::None,
            None,
        )
    }

    /// 主文件路径 = `wind_input.1.log`。内部序号 n 落到磁盘 n+1，且末尾那个 `.1`
    /// 必须被当成序号占位剥掉——否则会写出 `wind_input.1.4.log`。
    #[test]
    fn to_path_puts_index_before_extension() {
        let base = Path::new("/logs/wind_input.1.log");
        assert_eq!(
            LogIndex(3).to_path(base),
            PathBuf::from("/logs/wind_input.4.log")
        );
    }

    /// 当前那份日志的文件名。`init_logger` 拼主文件名用的就是 [`CURRENT_INDEX`]，
    /// 两处同源；这里把拼出来的结果写死，因为这个名字会出现在用户手里、文档里、
    /// 排查步骤里——改它必须是一次显式的决定，而不是某次重构的副作用。
    #[test]
    fn current_log_file_name_is_index_one() {
        assert_eq!(
            format!("wind_input.{CURRENT_INDEX}.log"),
            "wind_input.1.log"
        );
    }

    /// 磁盘序号 ↔ 内部序号的换算只有这一处，两个方向都钉住。
    /// `.1` 是当前文件而非历史，`from_disk` 必须拒绝它——认成历史的话，启动扫描会把
    /// 正在写的这份也算进淘汰队列。
    #[test]
    fn disk_index_maps_to_internal_index() {
        assert_eq!(LogIndex(1).disk(), 2);
        assert_eq!(LogIndex(9).disk(), 10);
        assert_eq!(LogIndex::from_disk(2), Some(LogIndex(1)));
        assert_eq!(LogIndex::from_disk(10), Some(LogIndex(9)));
        assert_eq!(LogIndex::from_disk(CURRENT_INDEX), None, "当前文件不是历史");
        assert_eq!(LogIndex::from_disk(0), None);
    }

    /// 首次启动：没有旧日志，不应报错也不应凭空造出 `.1.log`。
    /// （`rotate()` 内部是 `fs::rename`，对不存在的文件会 Err，故必须跳过。）
    #[test]
    fn first_start_does_not_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind_input.1.log");

        let mut r = make_rotate(&path, 10);
        rotate_on_startup(&mut r, &path);

        assert!(!dir.path().join("wind_input.2.log").exists());
    }

    /// 空日志文件不该白占一个序号，否则会把有用的历史更快挤出保留窗口。
    #[test]
    fn empty_log_does_not_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind_input.1.log");
        seed(&path, "");

        let mut r = make_rotate(&path, 10);
        rotate_on_startup(&mut r, &path);

        assert!(!dir.path().join("wind_input.2.log").exists());
    }

    /// 二次启动：上一次运行的内容整体搬到 `.2.log`，`.1.log` 让给本次运行。
    #[test]
    fn second_start_moves_previous_run_to_index_2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind_input.1.log");
        seed(&path, "run-1\n");

        let mut r = make_rotate(&path, 10);
        rotate_on_startup(&mut r, &path);

        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
            "run-1\n"
        );
        // `.1` 已重开且为空，本次运行从零写起
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        // 前两代命名一个都不该冒出来；`.1.2.log` 是「忘了剥主文件序号」的症状
        assert!(!dir.path().join("wind_input.log").exists());
        assert!(!dir.path().join("wind_input.log.1").exists());
        assert!(!dir.path().join("wind_input.1.2.log").exists());
    }

    /// 连续多次启动：序号依次后移，最老的一次被淘汰。
    ///
    /// 这里同时钉住两件事：级联重命名认得回自己上次写的文件（`scan_suffixes` 正确），
    /// 以及 `max_files` 是**不含主文件**的旧文件数。
    #[test]
    fn old_runs_are_evicted_beyond_max_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind_input.1.log");

        for i in 1..=4 {
            seed(&path, &format!("run-{i}\n"));
            let mut r = make_rotate(&path, 2);
            rotate_on_startup(&mut r, &path);
        }

        // 最近两次运行（run-3 / run-4）保留，更早的被删
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
            "run-4\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.3.log")).unwrap(),
            "run-3\n"
        );
        assert!(!dir.path().join("wind_input.4.log").exists());
    }

    /// 二代布局（`wind_input.log` 是当前 + `.N.log` 是历史）整体后移一格，
    /// 当前那份落到 `.1`。**这一条是本次改名的要害**：不搬的话，用户目录里那个
    /// 无序号的最新日志会被后续扫描无视，而 `.1`（其实是上一次运行）冒充最新。
    #[test]
    fn second_generation_layout_shifts_by_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind_input.1.log");
        seed(&dir.path().join("wind_input.log"), "current\n");
        seed(&dir.path().join("wind_input.1.log"), "prev-1\n");
        seed(&dir.path().join("wind_input.2.log"), "prev-2\n");

        migrate_legacy_suffix(&path);

        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.1.log")).unwrap(),
            "current\n",
            "无序号的那份是最新的，必须落到 .1"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
            "prev-1\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.3.log")).unwrap(),
            "prev-2\n"
        );
        assert!(!dir.path().join("wind_input.log").exists());
    }

    /// ★★ 迁移必须幂等：已经是新布局时**一个文件都不能动**。
    ///
    /// 判据若写成「有没有 .N.log」，每次启动都会把整串序号推一格——几次重启之后
    /// 保留窗口里全是空洞，真正的历史被挤光，而且没有任何报错。判据必须是
    /// 「无序号的 wind_input.log 在不在」。
    #[test]
    fn migration_is_idempotent_on_new_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind_input.1.log");
        seed(&path, "current\n");
        seed(&dir.path().join("wind_input.2.log"), "prev\n");

        for _ in 0..3 {
            migrate_legacy_suffix(&path);
        }

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "current\n");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
            "prev\n"
        );
        assert!(
            !dir.path().join("wind_input.3.log").exists(),
            "序号被推移了"
        );
    }

    /// 一代命名（`wind_input.log.N`）迁到现在的 `.{N+1}.log`：N 是「第 N 老的历史」，
    /// 而 `.1` 现在归当前那次运行，故整体让一格。
    #[test]
    fn first_generation_files_are_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind_input.1.log");
        seed(&dir.path().join("wind_input.log.1"), "old-1\n");
        seed(&dir.path().join("wind_input.log.2"), "old-2\n");

        migrate_legacy_suffix(&path);

        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
            "old-1\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.3.log")).unwrap(),
            "old-2\n"
        );
        assert!(!dir.path().join("wind_input.log.1").exists());
    }

    /// 迁移不得误伤非序号后缀（`.log.bak` 之类），也不得覆盖已存在的目标。
    #[test]
    fn migration_skips_non_numeric_and_existing_targets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind_input.1.log");
        seed(&dir.path().join("wind_input.log.bak"), "backup\n");
        seed(&dir.path().join("wind_input.log.1"), "legacy\n");
        seed(&dir.path().join("wind_input.2.log"), "already-new\n");

        migrate_legacy_suffix(&path);

        // 非数字后缀原样保留
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.log.bak")).unwrap(),
            "backup\n"
        );
        // 目标已存在 → 不覆盖，老文件留在原地
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
            "already-new\n"
        );
        assert!(dir.path().join("wind_input.log.1").exists());
    }

    /// 迁移后的文件必须能被 `scan_suffixes` 认回，否则会永不淘汰地堆积。
    /// 这是「只改 to_path 不改 scan_suffixes」那个坑的回归测试。
    #[test]
    fn migrated_files_participate_in_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind_input.1.log");
        seed(&dir.path().join("wind_input.log"), "current\n");
        seed(&dir.path().join("wind_input.log.1"), "old-1\n");

        migrate_legacy_suffix(&path);

        // 迁移后：.1 = current（二代那份最新）、.2 = old-1（一代那份历史）
        let mut r = make_rotate(&path, 10);
        rotate_on_startup(&mut r, &path);

        // 启动滚动把两份各推一格，`.1` 腾给本次运行。级联能推动 `.2` 正是
        // 「scan_suffixes 认得回迁移来的文件」的证据——认不回就会被原地覆盖。
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
            "current\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("wind_input.3.log")).unwrap(),
            "old-1\n"
        );
    }
}

/// TSF 日志的专属子目录名。**跨语言契约**，必须与 C++ 侧 `WIND_LOG_SUBDIR_NAME`
/// （`wind_tsf/include/FileLogger.h`）逐字一致——两边没有任何编译期约束，改一边不改
/// 另一边的后果是清理静默失效、文件无限堆积。
const TSF_LOG_SUBDIR: &str = "tsf_log";

/// 清理 TSF DLL 留下的**过期**日志（`wind_tsf.<宿主名>.<pid>.log` 及其 `.old`）。
///
/// 传入的是 `logs` 根目录。函数会扫两层：
/// - `logs/tsf_log/` —— 当前落点；
/// - `logs/` 本身 —— 更早的版本把 TSF 日志平铺在这里（包括所有进程共写一个
///   `wind_tsf.log` 的那一版）。不扫这层的话，存量机器上那些文件永远没人回收。
///
/// # 为什么这件事在 core 做，而不在 DLL 里
///
/// TSF 的日志文件改成每进程一个之后（消除了跨进程锁与每行开关文件的开销），文件数会
/// 随「用过的宿主 × pid」增长，需要有人回收。但 DLL 的 `CFileLogger::Init` 跑在
/// `DllMain(DLL_PROCESS_ATTACH)` 里 —— loader lock 之下不能做目录遍历这种耗时不可控
/// 的事。core 是个正常进程，启动时从容做一次即可。
///
/// # 判据是修改时间，不是 pid 是否还活着
///
/// 「查这个 pid 还在不在」看似更精确，实则是错的：pid 会被系统复用，而且刚退出的宿主
/// 那份日志恰恰是排查刚才那次故障最需要的。按时间留一段窗口既简单又不会误删现场。
///
/// 正在被写的文件也能删掉——DLL 侧开句柄时带了 `FILE_SHARE_DELETE`；但活跃宿主的日志
/// 修改时间就是此刻，不会落进过期窗口，所以正常情况下轮不到它。
///
/// 失败一律忽略：日志清理不该阻塞启动，更不该让服务起不来。
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
        // 只碰 TSF 自己那批。前缀与 `WIND_LOG_FILE_PREFIX` 对齐（跨语言契约，无编译期
        // 约束）；带 `.` 是为了不误伤将来可能出现的 `wind_tsf_xxx.log` 这类别的文件。
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
mod prune_tsf_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// 只删过期的 TSF 日志，且只认 `wind_tsf.` 前缀——core 自己的日志、别人的文件
    /// 都不能碰。同时钉住**两层都要扫**：`logs/tsf_log/`（当前落点）与 `logs/`
    /// （老版本平铺的存量，含所有进程共写的那个 `wind_tsf.log`）。
    #[test]
    fn prunes_only_stale_tsf_logs() {
        let dir = std::env::temp_dir().join(format!("wind_prune_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join(TSF_LOG_SUBDIR);
        std::fs::create_dir_all(&sub).unwrap();

        let old_ts = SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 30);
        let mk = |base: &Path, name: &str, stale: bool| {
            let p = base.join(name);
            std::fs::write(&p, b"x").unwrap();
            if stale {
                let f = std::fs::File::options().write(true).open(&p).unwrap();
                f.set_modified(old_ts).unwrap();
            }
            p
        };

        // 当前落点：logs/tsf_log/
        let stale_tsf = mk(&sub, "wind_tsf.feishu.1234.log", true);
        let stale_old = mk(&sub, "wind_tsf.feishu.1234.old.log", true);
        let fresh_tsf = mk(&sub, "wind_tsf.notepad.5678.log", false);
        // 老版本平铺在 logs/ 的存量
        let legacy_shared = mk(&dir, "wind_tsf.log", true);
        let legacy_fresh = mk(&dir, "wind_tsf.wechat.99.log", false);
        let core_log = mk(&dir, "wind_input.log", true); // core 自己的，即使过期也不该碰
        let other = mk(&sub, "wind_tsf.feishu.1234.txt", true); // 非 .log

        let removed = prune_stale_tsf_logs(&dir, Duration::from_secs(60 * 60 * 24 * 7));

        assert_eq!(removed, 3, "两层加起来应只删三个过期的 TSF 日志");
        assert!(!stale_tsf.exists(), "过期 TSF 日志未删");
        assert!(!stale_old.exists(), "过期轮转产物未删");
        assert!(
            !legacy_shared.exists(),
            "老版本平铺在 logs/ 的存量没被回收——不扫这层就永远没人管它"
        );
        assert!(fresh_tsf.exists(), "活跃宿主的日志被误删——那正是排查现场");
        assert!(legacy_fresh.exists(), "未过期的存量同样不该删");
        assert!(core_log.exists(), "core 自己的日志不归本函数管");
        assert!(other.exists(), "非 .log 文件不该被碰");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 子目录名是 Rust 与 C++ 之间的**跨语言契约**，两边各写各的字面量，没有任何编译期
    /// 约束。改一边不改另一边不会报错，只会让清理静默失效、文件无限堆积——所以在这里
    /// 扫源码钉死。
    #[test]
    fn subdir_name_matches_cpp_header() {
        const HEADER: &str = include_str!("../../../../wind_tsf/include/FileLogger.h");
        let expect = format!("#define WIND_LOG_SUBDIR_NAME    L\"{TSF_LOG_SUBDIR}\"");
        assert!(
            HEADER.contains(&expect),
            "FileLogger.h 里的 WIND_LOG_SUBDIR_NAME 与 TSF_LOG_SUBDIR 对不上，\
             应含：{expect}"
        );
    }
}
