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
mod tests {
    use super::*;
    use crate::codetable::CodetableDict;
    use crate::datformat::WdatWriter;

    /// 造一个最小可用的 wdat，返回其路径。
    fn make_wdat(dir: &Path, name: &str, code: &str, text: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut d = CodetableDict::empty();
        d.merge_single(code.into(), text.into(), 1, 0);
        let mut w = WdatWriter::new();
        d.export_to_wdat(&mut w);
        w.write(&path).unwrap();
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wind-reader-pool-{}-{}", std::process::id(), tag))
    }

    /// 把 `path` 的 mtime 拨回 `before` 那一刻，并断言「大小 + mtime」与之逐字段相同。
    ///
    /// 「重建前后 stamp 恰好一致」在生产上是时序赌局（同一 mtime 刻度内完成两次写入），
    /// 让测试去赌就会得到假绿——曾实证：`note_replaced` 注释掉后三条测试照样全过。
    /// 这里把赌局改成前提：拨回 mtime，并当场校验前提确已成立（大小若变了就直接报错，
    /// 免得测试悄悄退化成「在测 stamp 判据还灵不灵」）。
    ///
    /// ⚠️ 这几条测试守的是 Windows 语义（被 mmap 的文件照样能被 rename 覆盖），而本函数
    /// 要对一个**正被 mmap 的文件**再开一个写句柄去 `SetFileTime`。理论上不冲突
    /// （std 开文件带 `FILE_SHARE_WRITE`，section 不挡写句柄，改的又只是元数据），
    /// Linux 上实测全绿，但尚无 Windows 运行证据——Windows 上若在此处失败，是夹具的
    /// 问题而非被测逻辑的问题。
    fn force_same_stamp(path: &Path, before: &std::fs::Metadata) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_accessed(before.accessed().unwrap())
                    .set_modified(before.modified().unwrap()),
            )
            .unwrap();
        let after = std::fs::metadata(path).unwrap();
        assert_eq!(
            (before.len(), before.modified().unwrap()),
            (after.len(), after.modified().unwrap()),
            "前提没造出来：重建后大小或 mtime 仍有差别，本测试会退化成在测 stamp 判据"
        );
    }

    #[test]
    fn same_path_shares_one_reader() {
        let dir = temp_dir("share");
        let p = make_wdat(&dir, "a.wdat", "a", "啊");

        let r1 = open_wdat(&p).unwrap();
        let r2 = open_wdat(&p).unwrap();
        assert!(
            Arc::ptr_eq(&r1, &r2),
            "同一路径必须复用同一个 reader（这正是本池的目的）"
        );
        assert_eq!(Arc::strong_count(&r1), 2, "两个持有者");
        // 复用的 reader 功能正常
        assert_eq!(r2.search("a").len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// single-flight 的核心契约：同一路径的构建区间互斥。
    /// 用「同时进入临界区的最大并发数」来验证——它必须恒为 1。
    #[test]
    fn file_lock_serializes_same_path() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let path = temp_dir("lock-same").join("f.wdat");
        let inside = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let hs: Vec<_> = (0..8)
            .map(|_| {
                let (path, inside, peak) = (path.clone(), inside.clone(), peak.clone());
                std::thread::spawn(move || {
                    let lock = file_lock(&path);
                    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
                    let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    inside.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "同一路径的构建区间必须互斥，否则冷启动会并发 rename 同一个缓存文件"
        );
    }

    /// 不同路径不得互相阻塞——否则一个大词库的重建会拖住所有其他词库。
    #[test]
    fn file_lock_does_not_block_different_paths() {
        let dir = temp_dir("lock-distinct");
        let (a, b) = (dir.join("a.wdat"), dir.join("b.wdat"));
        let la = file_lock(&a);
        let _ga = la.lock().unwrap_or_else(|e| e.into_inner());
        // a 已被本线程持有；另一线程锁 b 应立刻拿到
        let done = std::thread::spawn(move || {
            let lb = file_lock(&b);
            let _gb = lb.lock().unwrap_or_else(|e| e.into_inner());
        });
        done.join().expect("锁不同路径不应被阻塞");
    }

    /// 并发加载同一份 yaml：无论谁先建好缓存，最终所有调用方都应拿到**同一个** reader。
    /// 若 single-flight 失效，多个线程会各自重建、rename 互撞，落到不同文件上。
    #[test]
    fn concurrent_load_converges_to_one_reader() {
        let dir = temp_dir("concurrent-load");
        std::fs::create_dir_all(&dir).unwrap();
        let yaml = dir.join("c.dict.yaml");
        // 正文须在独占一行的 `...` 之后，否则解析出零条目、退化成 Memory 分支
        std::fs::write(&yaml, "name: c\n...\n啊\taa\t1\n再\tzz\t1\n").unwrap();
        let cache = dir.join("cache").join("c.wdat");

        let hs: Vec<_> = (0..6)
            .map(|_| {
                let (yaml, cache) = (yaml.clone(), cache.clone());
                std::thread::spawn(move || {
                    let d = crate::cached::CachedDict::load_at_with(&yaml, &cache, false).unwrap();
                    match d {
                        crate::cached::CachedDict::Mmap(r) => Some(r),
                        // 缓存写入失败会退化成 Memory，这里不该发生
                        crate::cached::CachedDict::Memory(_) => None,
                    }
                })
            })
            .collect();
        let readers: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();

        let first = readers[0].clone().expect("应走 mmap 路径");
        for r in &readers {
            let r = r.clone().expect("每个线程都应拿到 mmap reader");
            assert!(
                Arc::ptr_eq(&first, &r),
                "并发加载同一词库须收敛到同一个 reader（single-flight + 池）"
            );
        }

        drop(readers);
        drop(first);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 与 `dropping_all_holders_releases_the_mapping` 互为反面：**持有期间**文件不可替换。
    ///
    /// 这条 OS 行为是两件事的共同依据：池存 `Weak` 而非 `Arc`（否则 reader 永久驻留，
    /// 重建永久失败），以及 `EngineManager` 里那两条「可能正被其他方案 mmap 持有」的告警
    /// ——invalidate 单个方案时，共享同一份词库的其他引擎仍持有映射，重建因此失败并降级
    /// 到临时副本。有了这个测试，那条因果链就不再是推测。
    /// 词库在**仍被持有**时被重建，随后的取用必须拿到新内容，不得复用陈旧 reader。
    ///
    /// 这是本模块最容易出错的一点，也是引入池之后唯一可能造成**功能性**回归的地方：
    /// 池之前每个引擎各自 `open`，天然读到当前文件；池化后若只按路径命中，就会把仍指向
    /// 替换前数据的 reader 交出去，表现为「改了词库不生效，重启才行」。
    ///
    /// 前提事实（本测试同时锁定）：Windows 上 rename 覆盖一个正被 mmap 的文件是**会成功**
    /// 的，旧 view 继续看到旧数据——所以不能指望"重建失败"来兜底。
    ///
    /// # 为什么要人为把 mtime 拨回去
    ///
    /// 这一条曾是**时序相关**的假绿：两次写入落在同一个 mtime 刻度内才暴露缺陷，机器慢一点
    /// 就自动变绿。变异检验实证——把 `note_replaced` 注释掉，它照样通过。所以这里不靠运气
    /// 制造「stamp 相同」，而是重建后显式把 mtime 拨回替换前的值，再断言新旧 stamp 确实
    /// 逐字段相同：判据的两个字段就此**双双失效**，只剩替换代数能救。
    #[test]
    fn rebuilt_file_is_not_served_from_stale_entry() {
        let dir = temp_dir("rebuild");
        let p = make_wdat(&dir, "r.wdat", "aa", "旧");
        let before = std::fs::metadata(&p).unwrap();

        let held = open_wdat(&p).unwrap(); // 模拟另一个方案的引擎仍持有
        assert_eq!(held.search("aa").len(), 1);

        // 持有期间重建该词库（WdatWriter 内部走 tmp + rename）。
        // 码长与字数刻意与旧内容一致 → 文件大小不变 → 判据的「大小」一栏先失效。
        let mut d = CodetableDict::empty();
        d.merge_single("bb".into(), "新".into(), 1, 0);
        let mut w = WdatWriter::new();
        d.export_to_wdat(&mut w);
        w.write(&p)
            .expect("被 mmap 持有不影响 rename 覆盖（Windows 亦然）");

        // 再把 mtime 拨回替换前，让「修改时间」一栏也失效——这是生产上「同一刻度内两次
        // 写入」的确定化复现，不是人造的极端场景。
        force_same_stamp(&p, &before);

        // 旧持有者继续看旧数据——这是 OS 语义，不是缺陷
        assert_eq!(held.search("aa").len(), 1, "旧 view 应继续指向替换前的数据");
        assert!(held.search("bb").is_empty());

        // 关键：新的取用必须反映重建后的内容
        let fresh = open_wdat(&p).unwrap();
        assert!(
            !Arc::ptr_eq(&held, &fresh),
            "文件已被替换，绝不能复用旧 reader"
        );
        assert_eq!(fresh.search("bb").len(), 1, "新取用须读到重建后的内容");
        assert!(fresh.search("aa").is_empty());

        drop(held);
        drop(fresh);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 注释库同理：`write_comment_wcmt` 也必须通知本池，否则挂载着注释库的方案重建后
    /// 仍供出旧释义。与上一条分开写，是因为三个写盘口各挂各的钩子，漏一个测不出来。
    #[test]
    fn rebuilt_comment_dict_is_not_served_from_stale_entry() {
        let dir = temp_dir("rebuild-wcmt");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.wcmt");
        let row = |c: &str| vec![("啊".to_string(), c.to_string(), String::new())];

        crate::commentdict::write_comment_wcmt(&p, &row("旧释")).unwrap();
        let before = std::fs::metadata(&p).unwrap();
        let held = open_comment(&p).unwrap();
        assert_eq!(held.lookup_first("啊"), Some("旧释"));

        // 释义字数相同 → 文件大小不变；再拨回 mtime → stamp 两栏双双失效。
        crate::commentdict::write_comment_wcmt(&p, &row("新释")).unwrap();
        force_same_stamp(&p, &before);

        let fresh = open_comment(&p).unwrap();
        assert_eq!(
            fresh.lookup_first("啊"),
            Some("新释"),
            "注释库重建后必须读到新释义"
        );
        drop((held, fresh));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// emoji 表同理：功能开关一关一开会重新解析并 rename 覆盖，`write_emoji_wemj`
    /// 同样得通知本池。
    #[test]
    fn rebuilt_emoji_dict_is_not_served_from_stale_entry() {
        let dir = temp_dir("rebuild-wemj");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("e.wemj");
        let row = |e: &str| vec![("你好".to_string(), vec![e.to_string()])];

        crate::emojidict::write_emoji_wemj(&p, &row("😊")).unwrap();
        let before = std::fs::metadata(&p).unwrap();
        let held = open_emoji(&p).unwrap();
        assert_eq!(held.lookup("你好"), Some("😊"));

        // 两个 emoji 都是 4 字节 → 文件大小不变。
        crate::emojidict::write_emoji_wemj(&p, &row("👋")).unwrap();
        force_same_stamp(&p, &before);

        let fresh = open_emoji(&p).unwrap();
        assert_eq!(
            fresh.lookup("你好"),
            Some("👋"),
            "emoji 表重建后必须读到新内容"
        );
        drop((held, fresh));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn different_paths_do_not_share() {
        let dir = temp_dir("distinct");
        let p1 = make_wdat(&dir, "b.wdat", "b", "波");
        let p2 = make_wdat(&dir, "c.wdat", "c", "此");

        let r1 = open_wdat(&p1).unwrap();
        let r2 = open_wdat(&p2).unwrap();
        assert!(!Arc::ptr_eq(&r1, &r2));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 池存 Weak 的核心契约：持有者全部释放后 mmap 必须解除，否则 Windows 上词库重建
    /// 的 rename 会永久失败。用「释放后能否覆写该文件」来验证映射确实断开了。
    #[test]
    fn dropping_all_holders_releases_the_mapping() {
        let dir = temp_dir("release");
        let p = make_wdat(&dir, "d.wdat", "d", "的");

        let r = open_wdat(&p).unwrap();
        assert_eq!(Arc::strong_count(&r), 1);
        drop(r);

        // 全部持有者已释放 → 文件不再被映射 → 可覆写（Windows 上映射未解除时这里会失败）
        let mut d = CodetableDict::empty();
        d.merge_single("dd".into(), "地".into(), 1, 0);
        let mut w = WdatWriter::new();
        d.export_to_wdat(&mut w);
        w.write(&p).expect("持有者释放后必须能覆写词库文件");

        // 失效条目不会被复用：重新打开应拿到覆写后的新内容
        let r2 = open_wdat(&p).unwrap();
        assert_eq!(r2.search("dd").len(), 1, "应读到覆写后的新内容");
        assert_eq!(r2.search("d").len(), 0, "旧内容不应再出现");

        drop(r2);
        std::fs::remove_dir_all(&dir).ok();
    }
}
