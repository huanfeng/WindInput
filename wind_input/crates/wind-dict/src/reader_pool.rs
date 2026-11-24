//! wdat / wcmt / wemj mmap reader 的进程级共享池。
//!
//! 同一个缓存文件常被多个方案引用（拼音、双拼、混输子引擎都解析到同一个 merged.wdat），
//! 本池按缓存文件路径复用同一个 reader。
//!
//! 池只存 `Weak`：最后一个引擎释放即解除映射，旧缓存文件才能被 `prune_slot` 删掉，
//! `EngineManager` 的 `engines.clear()` / `remove()` 也仍是有效释放点。key 是 `cache_path`
//! 生成的路径本身，不做 canonicalize（不同写法最多退化成各开一份）。新鲜度判定归
//! `cache_fp`，本池只管「同一路径只 mmap 一份」。设计依据见
//! docs/architecture/rime-dict-loading.md 的「5.4 reader 共享池」。

use crate::commentdict::CommentReader;
use crate::datformat::WdatReader;
use crate::emojidict::EmojiReader;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// 池中一条记录：弱引用 + 打开时的文件标识。
struct Entry<T> {
    weak: Weak<T>,
    /// `(大小, 修改时间, 替换代数)`。复用前必须比对：重建走 tmp + rename，旧映射仍指向
    /// 旧数据，仅凭路径复用会交出陈旧 reader。
    ///
    /// 同大小重建时 mtime 可能逐纳秒相同（粗粒度时钟），所以再由替换方经 [`replacing`]
    /// 递增代数；没走这道协议的替换（别的进程、手动覆盖）只靠大小与 mtime。
    /// 见 `rebuilt_file_is_not_served_from_stale_entry`。
    stamp: FileStamp,
}

type FileStamp = (u64, Option<std::time::SystemTime>, u64);

/// 路径 → 替换代数。条目数与池中文件数同阶（数十个）。
///
/// 不要给这张表加清理：代数退回 0 可能恰好等于池中条目记的 0，陈旧 reader 会被当成新鲜交出。
static REPLACE_GEN: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();

/// 会进本池的文件后缀。给「按后缀决定要不要通知」的调用方（方案导入、备份还原）用，
/// 免得这份清单在两处各写一遍然后悄悄漂移。
pub const POOLED_EXTENSIONS: [&str; 3] = ["wdat", "wcmt", "wemj"];

/// `path` 是否是会进本池的文件（按后缀判，见 [`POOLED_EXTENSIONS`]）。
pub fn is_pooled(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| POOLED_EXTENSIONS.iter().any(|p| e.eq_ignore_ascii_case(p)))
}

fn replace_gen(path: &Path) -> u64 {
    REPLACE_GEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .copied()
        .unwrap_or(0)
}

fn bump_gen(path: &Path) {
    let mut map = REPLACE_GEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *map.entry(path.to_path_buf()).or_insert(0) += 1;
}

/// 圈住一次「替换池中文件」的动作：建时给代数 +1，落时再 +1。
///
/// 凡会替换 `.wdat` / `.wcmt` / `.wemj` 的写侧都要走它，包括方案导入与备份还原。
/// 前后各记一次，替换窗口内读到的是中间代数，两侧都拒绝复用；替换失败也照样 +2（最坏多开
/// 一份映射）。正确性不依赖调用方持 [`file_lock`]。
#[must_use = "守卫一旦落地就记完了第二次，必须活到替换动作结束"]
pub struct Replacing<'a> {
    path: &'a Path,
}

/// 宣告即将替换 `path`，见 [`Replacing`]。守卫必须活到替换动作（含先删后 rename）结束。
pub fn replacing(path: &Path) -> Replacing<'_> {
    bump_gen(path);
    Replacing { path }
}

impl Drop for Replacing<'_> {
    fn drop(&mut self) {
        bump_gen(self.path);
    }
}

fn file_stamp(path: &Path) -> FileStamp {
    // 代数必须在 open 之前取：否则 open 期间发生的替换会被算成自己已读到的，给陈旧映射
    // 盖上新代数。这条顺序没有测试兜住（单线程测试测不出），改取值次序时请人工复核。
    let generation = replace_gen(path);
    match std::fs::metadata(path) {
        Ok(m) => (m.len(), m.modified().ok(), generation),
        Err(_) => (0, None, generation),
    }
}

type Pool<T> = OnceLock<Mutex<HashMap<PathBuf, Entry<T>>>>;

static WDAT_POOL: Pool<WdatReader> = OnceLock::new();
static COMMENT_POOL: Pool<CommentReader> = OnceLock::new();
static EMOJI_POOL: Pool<EmojiReader> = OnceLock::new();

#[allow(clippy::type_complexity)]
static BUILD_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

/// 按缓存文件路径取 single-flight 构建锁。
///
/// 按文件而非 schema 加锁：不同 schema（如 `pinyin` 与 `shuangpin`）会争同一个缓存文件，
/// 并发重建会让后一次 rename 撞上已 mmap 的文件而落到临时副本。外层 map 锁只用来取出
/// per-file 锁；拿到锁后必须复查新鲜度，等待期间别的线程可能已经建好。
///
/// ```ignore
/// let lock = reader_pool::file_lock(&cache_file);
/// let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
/// if fresh { return open_wdat(&cache_file); }   // ← 复查
/// // 重建…
/// ```
pub fn file_lock(path: &Path) -> Arc<Mutex<()>> {
    let mut map = BUILD_LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let lock = map.entry(path.to_path_buf()).or_default().clone();
    // 清掉无人持有的条目（strong_count == 1 即只剩 map 自己），避免随方案增删单调增长。
    // 刚取出的这把是 2（map 一份 + 待返回一份），不会被误清。
    map.retain(|_, v| Arc::strong_count(v) > 1);
    lock
}

/// 打开 wdat；同一路径已有存活 reader 时复用，不再新建映射。
pub fn open_wdat(path: &Path) -> anyhow::Result<Arc<WdatReader>> {
    get_or_open(WDAT_POOL.get_or_init(Default::default), path, |p| {
        WdatReader::open(p)
    })
}

/// 打开注释库 `.wcmt`；同一路径已有存活 reader 时复用，不再新建映射。
///
/// 同一份注释库常挂在多个方案下，按路径复用后只有一份映射。
pub fn open_comment(path: &Path) -> anyhow::Result<Arc<CommentReader>> {
    get_or_open(COMMENT_POOL.get_or_init(Default::default), path, |p| {
        CommentReader::open(p)
    })
}

/// 打开 emoji 扩展表 `.wemj`；同一路径已有存活 reader 时复用，不再新建映射。
///
/// 本表全局只有一份，走池是为了保住释放语义：功能开关重开时要 rename 覆盖缓存文件，
/// 而 Windows 上被 mmap 的文件 rename 会 Access Denied。
pub fn open_emoji(path: &Path) -> anyhow::Result<Arc<EmojiReader>> {
    get_or_open(EMOJI_POOL.get_or_init(Default::default), path, |p| {
        EmojiReader::open(p)
    })
}

fn get_or_open<T>(
    pool: &Mutex<HashMap<PathBuf, Entry<T>>>,
    path: &Path,
    open: impl FnOnce(&Path) -> anyhow::Result<T>,
) -> anyhow::Result<Arc<T>> {
    // 先取 stamp 再 open：万一两者之间文件恰被替换，失败方向是「下次多开一份」（安全），
    // 反过来则会把 reader 标记成对应新文件而实际指向旧数据（不安全）。
    let stamp = file_stamp(path);
    let mut guard = pool.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = guard.get(path)
        && e.stamp == stamp
        && let Some(alive) = e.weak.upgrade()
    {
        return Ok(alive);
    }
    // 未命中 / 条目失效 / 文件已被替换：重新打开。open 放在锁内：mmap 只建映射、耗时微秒级，
    // 且引擎构建本就被 `EngineManager::build_locks` 串行化，不值得做锁外构建 + 双检。
    let reader = Arc::new(open(path)?);
    guard.insert(
        path.to_path_buf(),
        Entry {
            weak: Arc::downgrade(&reader),
            stamp,
        },
    );
    // 顺带清掉失效条目，避免 map 随方案增删单调增长。
    guard.retain(|_, e| e.weak.strong_count() > 0);
    Ok(reader)
}

#[cfg(test)]
#[path = "reader_pool_tests.rs"]
mod tests;
