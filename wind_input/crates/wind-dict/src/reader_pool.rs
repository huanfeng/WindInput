//! wdat / wcmt mmap reader 的进程级共享池。
//!
//! 同一个缓存文件常被多个方案引用：`pinyin.schema.toml` 与 `shuangpin.schema.toml` 都指向
//! `pinyin/rime_frost.dict.yaml`，混输方案（`wubi86_pinyin`）还会再递归建一套子引擎。而
//! `EngineManager::cache_path` 的命名空间取**源文件在 schemas 下的目录链**（见
//! [`crate::cache_ns`]），三者最终都解析到同一个 `<cache>/pinyin/rime_frost.merged.wdat`
//! —— 实测该 62MB 文件被 mmap 三份、`wubi86_jidian.wdat` 两份（当时还有 `unigram.wdb`
//! 三份，该产物已随语言模型移除）。本池按缓存文件路径复用同一个 reader。
//!
//! # 为什么池里存 `Weak` 而不是 `Arc`
//!
//! 池**不持有**强引用：最后一个引擎释放后 `Arc` 计数归零，mmap 随即解除。这在 Windows 上
//! 是必需的 —— 文件被 mmap 期间 `rename`/删除会 Access Denied，而词库重建全部要 rename
//! 覆盖（`CachedDict::write_cache`、combined/merged 重写、`write_comment_wcmt`）。若池持强
//! 引用，reader 将永久驻留，重建会从「偶发失败」恶化成「永久失败」。
//!
//! （2026-10-14 起缓存按内容命名、重建写新文件不覆盖旧文件，rename 冲突已不再发生；
//! `Weak` 仍是对的——旧文件要等最后一个持有者放手才能被 `prune_slot` 删掉。）
//!
//! 存 `Weak` 则天然保住既有的释放语义：`EngineManager::reload_from_config` 的
//! `engines.clear()` 与 `invalidate_schema` 的 `engines.remove()` 依旧是有效释放点，
//! 无需在池上再叠一层手工引用计数或强制关闭通道。
//!
//! # key 的选取
//!
//! key 是缓存文件路径本身，**不含大小/mtime** —— 新鲜度判定归 `cache_fp`（内容指纹），
//! 本池只负责「同一路径只 mmap 一份」，两者职责分离。这也遵循本 crate 既有共识：用内容
//! 指纹而非 mtime，以免部署刷新 mtime 导致恒重建。
//!
//! 路径直接做 key 而不 `canonicalize`：缓存路径统一由 `cache_path`（源路径的纯函数）生成，
//! 同一文件必然得到同一字符串。万一将来出现不同写法，后果也只是退化成各开一份
//! （即本池引入前的行为），不会取到错误的 reader。

use crate::commentdict::CommentReader;
use crate::datformat::WdatReader;
use crate::emojidict::EmojiReader;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// 池中一条记录：弱引用 + 打开时的文件标识。
struct Entry<T> {
    weak: Weak<T>,
    /// `(大小, 修改时间, 替换代数)`。复用前必须比对——**仅凭路径复用会交出陈旧数据**。
    ///
    /// 实测（见 `rebuilt_file_is_not_served_from_stale_entry`）：词库重建走 tmp + rename，
    /// 在 Windows 上即便目标正被 mmap 也会**成功**（Rust 的 `File::open` 带
    /// `FILE_SHARE_DELETE`，旧文件转为 pending-delete，目录项已指向新文件），而既有的
    /// mmap view 继续指向替换前的数据。若只按路径命中，重建之后新建的引擎会复用到那个
    /// 仍指向旧数据的 reader——表现为「改了词库不生效，重启才行」。
    ///
    /// 这与 `cache_fp` 坚持内容指纹而非 mtime 并不矛盾：那里要判定的是「缓存是否需要
    /// 重建」，须避免部署刷新 mtime 导致误重建；这里要判定的是「手里的 reader 是否还
    /// 对应磁盘上的当前文件」，恰恰需要能察觉文件被替换。目的不同，判据也就不同。
    ///
    /// # 为什么 `(大小, 修改时间)` 不够，要再加一个代数
    ///
    /// 重建**内容变了而大小不变**时（同构小词库），判据只剩 mtime；而 Linux 的 inode
    /// 时间戳取自粗粒度时钟（`current_time()`，刻度以毫秒计），两次写入落在同一刻度内
    /// 就得到**逐纳秒相同**的 mtime。实证（2026-09-14，本机）：重建前后 stamp 完全相同
    /// ——`(1157, tv_sec=1789399678, tv_nsec=478103444)`，于是陈旧 reader 被当成新鲜的
    /// 交了出去。这不是假想，`rebuilt_file_is_not_served_from_stale_entry` 稳定复现。
    ///
    /// 换成内容指纹并不可行：这里的文件动辄 62MB，为一次 `open_wdat` 全量哈希太贵；
    /// 只采样头尾则漏掉「码集不变、只改权重或词条文本」的重建（那种改动全落在中段的
    /// 权重与字符串池里，头部的计数/偏移与尾部的 CharMap 都可以纹丝不动）。
    /// 而 inode / `file_index` 在 Windows 上要 `windows_by_handle`（至今未稳定），
    /// 拿不到统一的文件标识。
    ///
    /// 故改为让**替换方**说话：替换池中文件的写侧用 [`replacing`] 圈住替换动作，
    /// 该路径的代数随之递增。走这道协议的重建与时钟精度无关；**没走的替换**
    /// （assemble 工具等别的进程、用户手动覆盖）仍只靠 `(大小, 修改时间)`——
    /// 那类替换与本进程上次读取隔着人的操作时间，不会撞进同一个 mtime 刻度。
    stamp: FileStamp,
}

type FileStamp = (u64, Option<std::time::SystemTime>, u64);

/// 路径 → 替换代数。条目数与池中文件数同阶（数十个）。
///
/// ⛔ **不要给这张表加清理**。清掉某条会让代数退回 0，而池里那条记的可能**正好也是 0**
/// （缓存新鲜时启动 ⇒ 建条目时还没人替换过 ⇒ 记的就是 0），此后的重建把代数推到 1，
/// 清理再把它抹回 0 —— 三栏全对上，陈旧 reader 原样交出去，A2-14 就此复活。
/// 「代数退回 0 是安全方向」只在池中那条记着的代数 > 0 时才成立，而那不是能假定的。
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

/// 圈住一次「替换池中文件」的动作：**建时给代数 +1，落时再 +1**。
///
/// 凡是会替换 `.wdat` / `.wcmt` / `.wemj` 的写侧都要走它——不只是本 crate 的三个写盘口，
/// 方案导入与备份还原同样在用户 schemas 目录里覆盖这些文件（那里的 sidecar `.wdat`
/// 也是直接走本池打开的）。
///
/// # 为什么是前后各 +1，而不是替换完记一次
///
/// 记一次的话，`rename` 与 `+1` 之间有个窗口：窗口里别的线程取到的是「旧代数 + 新文件的
/// metadata」，若大小与 mtime 都没变（正是 A2-14 的前提），它就命中了陈旧条目。把 `+1`
/// 挪到 `rename` 之前也只是把窗口镜像到另一侧。前后各记一次，窗口里读到的是一个**中间
/// 代数**，与替换前、替换后都不同 ⇒ 无论落在哪一侧都拒绝复用，最坏不过多开一份映射。
///
/// 这道自足性是有意的：别把正确性寄托在「调用方都持着 [`file_lock`]」上。现在确实多数
/// 写侧都在那把锁下，但 `cached.rs` 的 sidecar `open_wdat` 与导入/还原都不在，靠锁的论证
/// 会在下一次重构时无声垮掉。
///
/// # 用代数而不是直接删池中条目
///
/// 删条目挡不住这个交错：线程 A 取完 stamp、正在 `open`（映射的是旧文件）时，线程 B
/// 完成替换并删条目——A 随后插入的那条就成了「B 删除之后才写进来的陈旧条目」，再无人
/// 能把它清掉。记代数则不然：A 带的是取 stamp 那一刻的旧代数，与当前对不上，自然不被复用。
///
/// # 替换失败怎么办
///
/// 照样 +2。代价是之后多开一份映射（失败方向安全），换来的是守卫无需知道替换成没成功
/// ——`rename` 失败时目标文件的状态本就不确定（先删后改名的路径上它可能已经没了）。
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
    // 代数**必须先取**：若在 open 之后才取，就会把「open 期间发生的替换」算成自己已经
    // 读到的，给陈旧映射盖上新代数的章。先取则该替换落在自己的代数之后，下次比对必不命中。
    //
    // ⚠️ 这条顺序**没有测试兜住**——现有测试都是单线程的，`replacing` 早已落完才轮到
    // `open`，代数在哪一步读都是同一个值，把这行挪到 `open` 之后测试照样全绿。要真测它
    // 得在 `open` 中途插桩。改动本函数的取值次序时请人工复核上面这段推理。
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
/// `EngineManager::build_locks` 的 key 是 schema_id，而真正被争用的资源是**文件**：
/// `pinyin` 与 `shuangpin` 是两个 schema、两把锁，却都指向同一个 `merged.wdat`。冷启动
/// 无缓存时，后台预热会让两个线程同时判 stale、同时解析同一份 yaml、同时 rename——
/// 第二次 rename 撞上第一次刚 mmap 好的文件，Windows 上 Access Denied，随后静默落
/// `temp_fallback` 退化成临时目录副本。副本路径不同，上面那个池也就无从合并，映射反而
/// 翻倍。
///
/// 用法与 `build_locks` 相同的两段式：外层 map 锁只用来取出 per-file 锁并立即释放，
/// 真正的构建在 per-file 锁下进行。**拿到锁后必须复查新鲜度**——等待期间别的线程
/// 可能已经建好，不复查就只是不竞态、仍重复干活。
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
/// 注释库比词库更容易被多处引用：一份「英汉释义」可能同时挂在拼音、五笔、混输方案下，
/// 用户也可能在挂载列表里写两遍同一个文件。按路径复用后，无论引用几次都只有一份映射。
pub fn open_comment(path: &Path) -> anyhow::Result<Arc<CommentReader>> {
    get_or_open(COMMENT_POOL.get_or_init(Default::default), path, |p| {
        CommentReader::open(p)
    })
}

/// 打开 emoji 扩展表 `.wemj`；同一路径已有存活 reader 时复用，不再新建映射。
///
/// 本表全局只有一份（不像注释库那样按方案挂载），走池的理由不是省内存而是**保住释放
/// 语义**：功能开关一关一开会重新解析，届时要 rename 覆盖缓存文件，而 Windows 上被
/// mmap 的文件 rename 会 Access Denied（见本模块开头「为什么池里存 Weak」）。
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
    // 未命中 / 条目失效 / **文件已被替换**：重新打开。
    //
    // open 放在锁内：mmap 只是建立映射不读盘（按需分页），耗时以微秒计；且引擎构建本就
    // 被 `EngineManager::build_locks` 串行化过，不值得为此引入「锁外构建 + 双检」的两段式。
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
