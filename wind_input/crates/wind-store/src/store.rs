//! Store 核心：打开用户数据引擎、把 `data/` 下的文本绑定上、跑旧库迁移。
//!
//! 全部用户数据的真源在用户目录 `data/`（设计 `docs/design/user-data-storage-redesign.md`
//! §2.1）。redb（`userdata.redb`）只在旧库链（P1–P4a）未迁完时打开、只读旧表；迁完后不再打开。

use redb::{Database, TableDefinition};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::{debug, error, info, warn};

/// 当前存储版本（迁移锚点）
pub const CURRENT_VERSION: u32 = 1;

/// redb 页缓存上限。**redb 的默认值是 1 GiB**（`Builder::set_cache_size` 的默认）。
///
/// ## 它是上限，不是预分配
///
/// 实际占用的上界是 `min(本值, 库文件大小)`——19 万词的库约 34 MB，给 1 GiB 也只会
/// 占到 34 MB。所以默认值本身并不直接等于「吃掉 1 GiB」。
///
/// 旧实现每次按键全表扫描，把整张用户词表拉进缓存并常驻，量级是十几 MB；索引化之后
/// 常规输入根本填不满缓存，只有设置页的词库列举（全表顺序扫）会填一次。故本值真正的
/// 作用是**给超大词库一个显式上界**（50 万词的库约 90 MB），而不是解决 19 万词的内存。
///
/// ## 取值：由 `perf_cache_size.rs` 双向标定（用户词 P2 迁出 redb 后该探针已删，结论留作记录）
///
/// 读路径（简拼召回 / 前缀补全 / 全量列举）对本值**几乎不敏感**：redb 2.x 不是 mmap，
/// 未命中走 `read` 系统调用、由 OS 页缓存供数据，是 µs 级 syscall 而非磁盘寻道——
/// 它下面还垫着一层 OS 页缓存。连 2 MiB 都测不出读性能退化。
///
/// **写路径敏感**：`set_cache_size` 把 10% 划给写缓存，19 万词批量导入在写缓存放得下
/// 全部脏页时一次刷盘，放不下就反复外溢。实测断崖出现在 64↔256 MiB 之间。
/// 首版取 32 MiB 只测了读就下了结论，导入因此慢 4.6 倍——**只测一半的结论等于没测**。
pub const DEFAULT_CACHE_SIZE_BYTES: usize = 256 * 1024 * 1024;

/// 空闲回收长档：连续这么久没有任何读写就丢页缓存（兜所有填缓存的路径）。
///
/// 这三个时长**不做成配置键**（`docs/architecture/config-design-rules.md` §R1：差异可由
/// 程序判定就别加用户键）；`spawn_idle_cache_reclaimer*` 把它们做成参数只为测试能用毫秒跑。
pub const RECLAIM_IDLE: std::time::Duration = std::time::Duration::from_secs(60);
/// 空闲回收短档：有全表扫描待回收时，空闲这么久就丢（见 [`Store::mark_scan_pending`]）。
///
/// 设置页翻页、打字中自动造词触发的索引重建，间隔都远小于它，故连续操作期间不回收；停手满它
/// 才还回去。持续打字时长档永远等不到，这一档就是为此而设。
pub const SCAN_RECLAIM_IDLE: std::time::Duration = std::time::Duration::from_secs(3);
/// 回收线程的采样拍长。短档只有 3 秒，拍长须远小于它。只在有东西待回收时按它醒；
/// 闲置时按它的 [`IDLE_TICK_FACTOR`] 倍睡（生产 10 秒），来了标记就被叫醒。
pub const RECLAIM_TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// 用户数据后台节拍：文本目标（用户词）空闲折回不依赖下一次写入，停手后这么久之内一定写回。
pub const USERDATA_TICK: std::time::Duration = std::time::Duration::from_secs(5);

/// 小文件（补全历史）去抖落盘的节拍（设计 §2.4：2 秒）。
pub const SMALL_FILE_FLUSH: std::time::Duration = std::time::Duration::from_secs(2);
/// 闲置慢拍 = `tick` × 本值。
const IDLE_TICK_FACTOR: u32 = 10;
/// [`Store::mark_scan_pending_rows`] 的门槛：后台扫描走过的行数不到它就不标记。
///
/// 回收要关库重开，关库那一下要刷盘（本机 zfs 实测约 6 ms，几乎全在关旧库上）。小库（拼音词
/// 约 170 B/条，1 万条 ≈ 1.7 MB 页）扫进缓存的那点页不值得每次停手都来这一下，照旧交给长档；
/// 大库（S3 的对象，19 万条 ≈ 41 MB）远在门槛之上。
///
/// 两处调用方的行数口径不同：`UserTextIndex` 只数该方案的**用户词**，`UserAssocIndex` 数
/// **用户词 + 临时词**（它两张表都扫）。差异可以接受：门槛量的是「这次扫描往缓存里填了多少页」，
/// 各自数的正是各自扫过的行；临时词表默认上限 5000 条，两种口径在门槛附近最多差半个门槛，
/// 只影响离门槛很近的那一小段库是标记还是交给长档，两种结果都正确。
pub const SCAN_RECLAIM_MIN_ROWS: usize = 10_000;

/// 按统一配置打开 redb（`open` 与 `resume` 共用，避免两处默认值漂移）。
fn open_db(path: &Path, cache_bytes: usize) -> Result<Database, redb::DatabaseError> {
    Database::builder().set_cache_size(cache_bytes).create(path)
}

// ── 表定义（key 编码见 store.md §2：复合 key 带 schema 前缀，redb 扁平）──
/// 用户词（旧表）：key = "{schema}\0{code}\0{text}"，value = 序列化记录。P2 起真源是
/// `user/<schema>/user.dict.yaml`，本表只供迁移读、只读保留到 P4。
pub(crate) const USER_WORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("user_words");
/// 临时词：同上
pub(crate) const TEMP_WORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("temp_words");
/// 用户词**简拼索引**：key = `"{schema}\0{group}\0{code}\0{text}"`，value 空。
/// 键的构成、分组规则与维护契约全在 [`crate::abbrev_index`]。
///
/// P2 起退役：用户词简拼走引擎的分组索引，本表只读保留到 P4。
pub(crate) const USER_ABBREV: TableDefinition<&str, &[u8]> = TableDefinition::new("user_abbrev");
/// 临时词**简拼索引**：与 [`USER_ABBREV`] 同构，索引 `TEMP_WORDS`。
///
/// 两层都要索引，因为简拼召回是**跨层**的：引擎侧挂的 `DictManager` 同时注册了
/// `StoreUserLayer` 与 `StoreTempLayer`，只索引其一等于只修一半——自动造词开着时，
/// 临时词库（默认上限 5000 条）仍会被逐切点全量枚举。
pub(crate) const TEMP_ABBREV: TableDefinition<&str, &[u8]> = TableDefinition::new("temp_abbrev");
/// 自动造词的**草稿层**：key = `"{schema}\0{code}\0{text}"`，value = {created_at} 定长 8B。
///
/// 滑窗切出的候选词先落这里，用户真的用它上屏过一次才跃迁进 [`TEMP_WORDS`]
/// （设计见 `docs/design/auto-phrase-draft-layer.md`）。与临时词库的分界是**谁验证过它**：
/// 草稿是机器猜的、带有效期、到期没人用就丢；临时词是用户用过的、长期留着。
///
/// **value 不存 weight**：草稿在候选里恒沉底（只精确召回 + 沉底键），weight 不参与任何比较，
/// 存了就是一个永远读不到却要维护的字段。
///
/// ⚠️ **刻意没有伴随的简拼索引**（对比 [`TEMP_ABBREV`]）：草稿的写入量比临时词高一两个
/// 数量级，配索引等于把写放大也翻上去。代价是草稿只支持精确码召回——而它的定位就是
/// 「你刚打过的，用同样的码再打一次就出来」，简拼召回等它跃迁进临时词库之后自然就有。
/// 这在 trait 层面是免费的：`DictLayer::search_abbrev` 的默认实现就返回空、不回退全表扫。
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建。
pub(crate) const DRAFT_WORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("draft_words");
/// 用户词频：key = "{schema}\0{code}\0{text}"，value = {count,last_used}（见 frequency.md）
/// 联想历史：key = "{schema}\0{上文}\0{选中的整词}"，value 同 [`FREQ`]（count + last_used）。
/// 用户在某上文后选了哪条联想，见 [`crate::assoc_history`]。
pub(crate) const ASSOC_HIST: TableDefinition<&str, &[u8]> = TableDefinition::new("assoc_history");
pub(crate) const FREQ: TableDefinition<&str, &[u8]> = TableDefinition::new("freq");
/// Shadow 规则：key = "{schema}\0{code}"
pub(crate) const SHADOW: TableDefinition<&str, &[u8]> = TableDefinition::new("shadow");
/// 快捷输入格式表的用户调整：key = 格式类别（`date` / `number` …），value = 记录 JSON。
///
/// **键只到类别**，不带方案与输入码——与 [`SHADOW`] 的分界见 [`crate::quick_format`]
/// 的模块文档（存错了会表现为「调整当时有效、隔天失效」）。
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建。
pub(crate) const QUICK_FORMAT: TableDefinition<&str, &[u8]> = TableDefinition::new("quick_format");
/// 常用字表的**用户覆盖**：key = 单个字，value = 记录 JSON。
///
/// **键不带方案**——「某个字常不常用」是全局字级属性，与 [`SHADOW`]（按方案 + 输入码）
/// 刻意分开。只存用户碰过的字，出厂那 8104 字仍在 `common_chars.txt` 里；为什么不整表
/// 进库见 [`crate::common_chars`] 的模块文档。
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建。
pub(crate) const COMMON_CHARS: TableDefinition<&str, &[u8]> = TableDefinition::new("common_chars");
/// 字符类的**用户层**：key = 类的 key，value = 那个类的 yaml 文本（UTF-8 字节）。
///
/// value 是文本而不是结构化记录：格式只在 `wind_config::charset_def` 一份，本 crate
/// 不解析。为什么用户层进库而不是目录见 [`crate::charsets`] 的模块文档。
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建。
pub(crate) const CHARSET_USER: TableDefinition<&str, &[u8]> = TableDefinition::new("charset_user");
/// 补全候选的**用户学习数据**：key = `"{kind}\0{text}"`，value = {count,last_used} 定长 12B。
///
/// `kind` 是 [`crate::completion::CompletionKind`] 的闭集（邮箱后缀 / 网址历史）。
/// **键不带方案**——「我常用哪个邮箱后缀」「我访问过哪个网址」与五笔拼音无关，同
/// [`COMMON_CHARS`] 的判据。与 [`FREQ`] 的分界见 [`crate::completion`] 模块文档。
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建。
pub(crate) const COMPLETION: TableDefinition<&str, &[u8]> = TableDefinition::new("completion");
/// 全局短语：key = "{code}\0{text}"
pub(crate) const PHRASES: TableDefinition<&str, &[u8]> = TableDefinition::new("phrases");
/// 每日统计：key = "YYYY-MM-DD"
pub(crate) const STATS_DAILY: TableDefinition<&str, &[u8]> = TableDefinition::new("stats_daily");
/// 元数据：version / device_id 等
pub(crate) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const META_VERSION_KEY: &str = "schema_version";

/// [`Store::drop_page_cache_inner`] 的结果。
#[derive(Debug, PartialEq, Eq)]
enum Dropped {
    /// 丢了一次。
    Yes,
    /// 暂停态，没有缓存可丢。
    Paused,
    /// 拿到锁时发现刚有人用过库，作罢（只在回收线程的「判空闲」那条路上出现）。
    Busy,
}

/// 引擎状态目录、目录 id 与用户词文本目录（设计 §2.1 / §3.7）。生产 = `local_dir()/state/runtime/<dir_id>`
/// + `meta.toml` 的 dir_id + `<userdata>/user`。
#[derive(Clone, Debug)]
pub struct StateLoc {
    pub state_dir: PathBuf,
    pub dir_id: String,
    /// 用户词文本的根：`<text_dir>/<schema>/user.dict.yaml`。
    pub text_dir: PathBuf,
    /// 用户目录已记为迁完（协调器据 `meta.toml` 的 `format_version` 判定）。为真时不再从旧 redb
    /// 迁移：本机状态目录里的迁移标记丢了（清 state、换机）也只补写标记——重迁会把升级后删掉的
    /// 数据从旧表合并回来。
    pub legacy_done: bool,
}

/// `Store::open(path)` 的引擎状态目录：redb 文件名后追加 `.state`。测试与工具用；生产走
/// [`Store::open_with_state`]。
pub fn derived_state_dir(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".state");
    PathBuf::from(s)
}

/// `Store::open(path)` 的用户词文本目录：redb 文件名后追加 `.user`（与派生状态目录一起随新库清空）。
pub fn derived_text_dir(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".user");
    PathBuf::from(s)
}

/// 存储引擎（redb）。`db` 为 None 表示已暂停（pause，释放文件锁供热替换）。
pub struct Store {
    path: PathBuf,
    /// 运行时数据引擎（临时词 / 草稿 / 词频 / 联想历史，设计 §6 P1）。四组 API 委托给它。
    runtime: std::sync::Arc<wind_userdata::runtime::Runtime>,
    /// 引擎状态目录（迁移标记也在这里）。
    state_dir: PathBuf,
    /// 用户词文本根目录（P2 起用户词的真源）。
    text_dir: PathBuf,
    /// 旧库链（P1–P4a）已迁完：来自 `meta.toml`（`StateLoc::legacy_done`），或本进程里
    /// `format_version` 刚写到 5、标记已删（`forget_legacy_markers`）。
    pub(crate) legacy_done: std::sync::atomic::AtomicBool,
    /// 各方案的 Shadow 规则表（P3 起真源是 `user/<schema>/shadow.dict.yaml`），首次访问时载入。
    /// 每日统计文件读改写的串行化（收集器 flush 与设置页查询 / 清理并发）。
    pub(crate) stats_io: Mutex<()>,
    /// 补全历史（P4a 起内存持有，去抖落盘到状态目录）。
    pub(crate) completions: Mutex<crate::completion::CompletionState>,
    /// 补全历史落盘的串行化（落盘在数据锁外做）。
    pub(crate) completion_io: Mutex<()>,
    /// 快捷格式调整 / 常用字覆盖的真源文件（P4a 起，首次访问时载入）。
    pub(crate) quick_doc: std::sync::OnceLock<wind_userdata::text::doc::TextDoc>,
    pub(crate) common_doc: std::sync::OnceLock<wind_userdata::text::doc::TextDoc>,
    /// 快捷格式 / 常用字覆盖的最后一次好数据（文件手改坏了时继续服务，§2.5）。
    pub(crate) quick_good: crate::last_good::LastGood<crate::quick_format::QuickFile>,
    pub(crate) common_good: crate::last_good::LastGood<std::collections::BTreeMap<String, bool>>,
    /// 字符集用户层目录的上次指纹（`None` = 还没取过）。
    pub(crate) charsets_fp: Mutex<Option<String>>,
    /// 系统短语（来自 TOML，协调器交进来；P3 起不落库）。
    pub(crate) phrase_sys: std::sync::RwLock<std::sync::Arc<Vec<crate::phrases::SystemPhrase>>>,
    /// 用户短语文件表（首次访问时载入）。
    pub(crate) phrase_table:
        std::sync::OnceLock<std::sync::Arc<wind_userdata::text::table::TextTable>>,
    pub(crate) shadows: Mutex<
        std::collections::HashMap<String, std::sync::Arc<wind_userdata::text::table::TextTable>>,
    >,
    /// 外部编辑会话：文件绝对路径 → 会话。只在内存，重启即无。
    pub(crate) edit_sessions:
        Mutex<std::collections::BTreeMap<PathBuf, crate::external_edit::EditEntry>>,
    /// 租约到期自动结束后，交回变了的项（协调器据此重建内存镜像）。
    pub(crate) edit_expired_hook: Mutex<Option<crate::external_edit::ChangedHook>>,
    db: Mutex<Option<Database>>,
    /// 本库的页缓存上限；`resume` 重开时要用同一个值，故随实例存着。
    cache_bytes: usize,
    /// 上一轮检查之后有没有访问过本库。由 [`Self::with_db`] 置位、空闲回收线程取走。
    ///
    /// **刻意不存时间戳**：`with_db` 是热路径（一次简拼召回要经它十几遍），而
    /// `Instant::elapsed()` 是一次 `clock_gettime`。改成「回收线程自己按固定节拍采样」
    /// 之后，热路径只剩一个 `Relaxed` 的 bool store，代价可以忽略；精度损失至多一个
    /// 节拍，而判据本来就是「空闲了一分钟」这种量级的事。
    touched: std::sync::atomic::AtomicBool,
    /// 累计丢弃页缓存的次数。供测试断言与诊断——「缓存到底有没有被回收」在外部
    /// 不可观测（RSS 不降，见 `tests/redb_cache_high_water.rs`），只能由内部报数。
    drops: std::sync::atomic::AtomicU64,
    /// 有一次全表扫描待回收（[`Self::mark_scan_pending`] 置位，回收时清除）。
    scan_pending: std::sync::atomic::AtomicBool,
    /// 叫醒闲置（慢拍）中的回收线程：`bool` = 有标记待它看。[`Self::mark_scan_pending`] 置位并
    /// 通知。单独一个 `Arc`：回收线程只持 `Weak<Store>`，睡着时不能拖住 `Store` 不让释放。
    reclaim_wake: std::sync::Arc<(Mutex<bool>, std::sync::Condvar)>,
    /// 用户词 / 临时词两表的**结构代次**：增删词、改权重（排序键）的事务 commit 后 +1，
    /// `resume`（备份还原换了整个文件）也 +1。只增不减、进程内有效。
    ///
    /// 供按文本前缀取用户词的内存索引（词语联想，见 `wind-engine` 的 `UserAssocIndex`）
    /// 判「缓存是否过期」——store 的键是 `schema\0code\0text`，没有按文本的次序，
    /// 每次联想都扫全表在按键路径上付不起（用户词库可达十九万条）。
    ///
    /// ⚠️ **只改 count 的写不进这里**（见 `words_count_gen`）：选词几乎每次都给某条词
    /// count +1，若也算结构变化，开着联想时几乎每次上屏都会触发一次后台全表重建。
    ///
    /// 宁多勿少：多 +1 只是多一次后台重建，漏 +1 则联想里永远缺那个词。
    ///
    /// 按方案的见 `words_gen_by_schema`。
    words_gen: std::sync::atomic::AtomicU64,
    /// **临时词** count 变化的代次（commit 后 +1）。临时词的 count 决定联想分档（门槛 2）
    /// 与档内排序，但它随选词频繁变化，消费方应节流地跟（见 `UserAssocIndex`）。
    /// 用户词的 count 不影响联想，不计入。
    words_count_gen: std::sync::atomic::AtomicU64,
    /// **按方案**的结构代次：`bump_words_gen(schema)` 同时推进全局 `words_gen` 与这里该方案的计数。
    ///
    /// 全局代次对「引用另一个方案」的索引太粗：在拼音里打字自动造词，会让五笔的用户词索引
    /// （辅助码引用五笔时要用）也判过期、后台重扫。见 `docs/design/text-code-lookup.md` §3.2。
    words_gen_by_schema: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// 不分方案的写入（`resume` 换了整个文件）推进的纪元；`words_generation_of` 把它并进返回值。
    words_epoch: std::sync::atomic::AtomicU64,
}

impl Drop for Store {
    /// 释放前把还没落盘的小文件写掉（进程正常退出、测试、工具不起落盘线程时靠它）。
    fn drop(&mut self) {
        if let Err(e) = self.flush_small_files() {
            warn!("释放时小文件落盘失败：{e:#}");
        }
    }
}

impl Store {
    /// 打开数据库，引擎状态放在派生目录（见 [`derived_state_dir`]）。redb 文件此前不存在 ⇒
    /// 视为新库，先清空派生目录（新库配新状态，测试反复用同一路径时不串数据）。
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::open_with_cache_size(path, DEFAULT_CACHE_SIZE_BYTES)
    }

    /// 生产入口：引擎状态放在给定目录，从不清空。
    pub fn open_with_state(path: impl AsRef<Path>, loc: StateLoc) -> anyhow::Result<Self> {
        Self::open_inner(path.as_ref(), DEFAULT_CACHE_SIZE_BYTES, loc)
    }

    /// 指定页缓存上限打开（状态目录同 [`Self::open`]）。
    pub fn open_with_cache_size(
        path: impl AsRef<Path>,
        cache_bytes: usize,
    ) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let derived = derived_state_dir(path);
        let text_dir = derived_text_dir(path);
        if !path.exists() {
            let _ = std::fs::remove_dir_all(&derived);
            let _ = std::fs::remove_dir_all(&text_dir);
        }
        Self::open_inner(
            path,
            cache_bytes,
            StateLoc {
                state_dir: derived,
                dir_id: "derived".into(),
                text_dir,
                legacy_done: false,
            },
        )
    }

    fn open_inner(path: &Path, cache_bytes: usize, loc: StateLoc) -> anyhow::Result<Self> {
        let path = path.to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 迁移全部完成过的库不再打开旧 redb：不锁、不写，文件原样留给回退的旧版本。判据是
        // meta.toml 的 format_version（`legacy_done`）；标记只在旧库链进行中作断点续做用。
        let fully_migrated = loc.legacy_done
            || crate::migrate_p3::ALL_MIGRATION_MARKERS
                .iter()
                .all(|m| loc.state_dir.join(m).is_file());
        let db = if fully_migrated {
            None
        } else {
            let db = open_db(&path, cache_bytes)?;
            Self::init_tables(&db)?;
            Some(db)
        };
        let state_dir = loc.state_dir.clone();
        // 先登记数据目录：之后的一切写入（含下面的迁移）都把上一版与坏文件留存放进 `.backup/`
        wind_userdata::text::fold::register_data_root(&loc.text_dir);
        let ud = wind_userdata::engine::UserData::open(wind_userdata::engine::Dirs {
            state_dir: loc.state_dir.clone(),
            dir_id: loc.dir_id.clone(),
        })?;
        if !ud.readonly() {
            wind_userdata::text::fold::migrate_legacy_backups(
                &loc.text_dir,
                crate::banner_backfill::is_data_file,
            );
            // 绑定 / 载入任何数据之前：之后各处取到的就是补过说明行的内容与指纹
            crate::banner_backfill::backfill_banners(&loc.text_dir);
        }
        let runtime = std::sync::Arc::new(wind_userdata::runtime::Runtime::new(
            std::sync::Arc::new(ud),
        ));
        let store = Self {
            path,
            runtime,
            state_dir: state_dir.clone(),
            text_dir: loc.text_dir.clone(),
            legacy_done: std::sync::atomic::AtomicBool::new(loc.legacy_done),
            shadows: Mutex::new(std::collections::HashMap::new()),
            phrase_sys: Default::default(),
            completions: Mutex::new(Default::default()),
            completion_io: Mutex::new(()),
            stats_io: Mutex::new(()),
            quick_doc: std::sync::OnceLock::new(),
            common_doc: std::sync::OnceLock::new(),
            quick_good: Default::default(),
            common_good: Default::default(),
            charsets_fp: Mutex::new(None),
            phrase_table: std::sync::OnceLock::new(),
            edit_sessions: Mutex::new(Default::default()),
            edit_expired_hook: Mutex::new(None),
            db: Mutex::new(db),
            cache_bytes,
            touched: std::sync::atomic::AtomicBool::new(false),
            drops: std::sync::atomic::AtomicU64::new(0),
            scan_pending: std::sync::atomic::AtomicBool::new(false),
            reclaim_wake: Default::default(),
            words_gen: std::sync::atomic::AtomicU64::new(0),
            words_count_gen: std::sync::atomic::AtomicU64::new(0),
            words_gen_by_schema: Default::default(),
            words_epoch: std::sync::atomic::AtomicU64::new(0),
        };
        if fully_migrated {
            store.bind_user_texts_at_open();
            store.bind_learned_texts_at_open();
            store.remember_charsets_fp();
            info!(
                "Store opened: {}（旧库已迁完，不再打开）",
                store.path.display()
            );
            return Ok(store);
        }
        store.run_migrations()?;
        store.bind_user_texts_at_open();
        store.bind_learned_texts_at_open();
        if let Err(e) = store.migrate_p1_if_needed(&state_dir) {
            // 迁移失败不阻断启动：标记没写，下次启动重做（与引擎已有数据合并，这一轮的写入
            // 不会被旧表盖掉）；本次运行只能看到已迁成的那些目标。协调器据 `migrated_p1()`
            // 决定是否升 format_version——没迁完就不升，旧版本回退后仍可用旧表。
            warn!("P1 迁移失败，下次启动重试：{e}");
        }
        if let Err(e) = store.migrate_p2_if_needed(&state_dir) {
            // 同 P1：不阻断启动，标记没写下次重做（合并，幂等）；没迁完不升 format_version
            warn!("P2 迁移失败，下次启动重试：{e}");
        }
        if let Err(e) = store.migrate_p3_shadow_if_needed(&state_dir) {
            warn!("P3 Shadow 迁移失败，下次启动重试：{e}");
        }
        if let Err(e) = store.migrate_p4_if_needed(&state_dir) {
            warn!("P4a 迁移失败，下次启动重试：{e}");
        }
        // 字符集用户层的基线指纹：没有它，启动后第一次手改 sync 时比不出变化
        store.remember_charsets_fp();
        info!(
            "Store opened: {} (v{})",
            store.path.display(),
            store.version().unwrap_or(0)
        );
        store.release_legacy_db_if_migrated();
        Ok(store)
    }

    /// 迁移全部完成就放下旧 redb（释放文件锁）。短语迁移要等系统短语，常是最后一步，
    /// 故打开时与短语迁移之后各查一次。
    pub(crate) fn release_legacy_db_if_migrated(&self) {
        if !self.legacy_chain_done() {
            return;
        }
        let mut guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        if guard.take().is_some() {
            info!("旧库已全部迁完，释放：{}", self.path.display());
        }
    }

    /// P1 迁移是否已完成（标记 `migrated-p1` 在）。协调器据此决定能否升 `format_version`。
    pub fn migrated_p1(&self) -> bool {
        self.state_dir
            .join(crate::migrate_p1::MIGRATED_P1)
            .is_file()
    }

    pub fn user_text_dir(&self) -> &Path {
        &self.text_dir
    }

    /// 某数据方案的用户词文本：`<text_dir>/<schema>/user.dict.yaml`。
    pub fn user_text_path(&self, schema: &str) -> PathBuf {
        self.text_dir.join(schema).join("user.dict.yaml")
    }

    /// 该方案的用户词绑定到文本（已绑定即返回）。同步失败（文本隔离 / 读不了）只 WARN：绑定照样
    /// 登记，之后的折回 / 同步会重试；查询按已有段与 delta 提供。
    pub(crate) fn ensure_user_text(&self, schema: &str) {
        use wind_userdata::record::Target;
        let ud = self.runtime.data();
        if ud.text_binding(Target::User, schema).is_some() {
            return;
        }
        let b = wind_userdata::engine::TextBinding {
            path: self.user_text_path(schema),
            name: format!("{schema}.user"),
            lowercase: false,
            syllable_codes: schema == "pinyin",
            format: wind_userdata::engine::TextFormat::Words,
        };
        if let Err(e) = ud.bind_text(Target::User, schema, b) {
            warn!("用户词文本 {schema} 同步失败（按已有数据提供查询）：{e:#}");
        }
    }

    /// 有用户词的方案：用户目录下已有文本的 ∪ 引擎里有用户词数据的。
    pub(crate) fn user_text_schemas(&self) -> std::collections::BTreeSet<String> {
        let mut schemas: std::collections::BTreeSet<String> = self
            .runtime
            .data()
            .keys()
            .into_iter()
            .filter(|(t, _)| *t == wind_userdata::record::Target::User)
            .map(|(_, s)| s)
            .collect();
        if let Ok(rd) = std::fs::read_dir(&self.text_dir) {
            for e in rd.flatten() {
                if !e.file_name().to_string_lossy().starts_with('.')
                    && e.path().join("user.dict.yaml").is_file()
                    && let Some(n) = e.file_name().to_str()
                {
                    schemas.insert(n.to_string());
                }
            }
        }
        schemas
    }

    /// 打开时把已有用户词的方案都绑定上（重开后归文本管的目标要绑定了才能折回）。
    fn bind_user_texts_at_open(&self) {
        for s in self.user_text_schemas() {
            self.ensure_user_text(&s);
        }
    }

    /// 学到的数据（P5）：设定按规则绑定到 `<text_dir>/<schema>/` 的文本，再把已有数据的方案
    /// （引擎里有段 / 待处理操作的 ∪ 目录下已有对应文件的）都绑上。单个失败只 WARN。
    fn bind_learned_texts_at_open(&self) {
        use crate::learned_text::learned_binding;
        let ud = self.runtime.data();
        let dir = self.text_dir.clone();
        ud.set_text_rule(Box::new(move |t, s| learned_binding(&dir, t, s)));
        for (t, s) in self.learned_text_keys() {
            if let Err(e) = ud.bind_by_rule(t, &s) {
                warn!("数据文件绑定失败（按已有数据提供查询）：{e:#}");
            }
        }
    }

    /// 有学到的数据的 (目标, 方案)：引擎里有段 / 待处理操作的 ∪ 文本目录下已有对应文件的。
    pub(crate) fn learned_text_keys(
        &self,
    ) -> std::collections::BTreeSet<(wind_userdata::record::Target, String)> {
        use crate::learned_text::{LEARNED_TARGETS, learned_binding};
        let ud = self.runtime.data();
        let mut keys: std::collections::BTreeSet<(wind_userdata::record::Target, String)> = ud
            .keys()
            .into_iter()
            .filter(|(t, _)| LEARNED_TARGETS.contains(t))
            .collect();
        for e in std::fs::read_dir(&self.text_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let Some(schema) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if schema.starts_with('.') {
                continue; // `.backup/` 等
            }
            for t in LEARNED_TARGETS {
                if learned_binding(&self.text_dir, t, &schema).is_some_and(|b| b.path.is_file()) {
                    keys.insert((t, schema.clone()));
                }
            }
        }
        keys
    }

    /// 用户数据目录下各文件的问题（§2.5：坏行 / 隔离 / 已重建 / 未迁移），按路径排序。
    pub fn data_issues(&self) -> Vec<crate::issues::FileIssue> {
        crate::issues::list_under(&self.text_dir)
    }

    /// 起用户数据后台节拍（生产由协调器起；测试 / 工具不起，靠显式折回）。只读实例不起。
    pub fn spawn_userdata_ticker(&self, every: std::time::Duration) {
        let _ = self.runtime.data().spawn_ticker(every);
    }

    /// 小文件（补全历史，P4a）有改动就落盘。去抖落盘线程、`Store` 释放、`dict.flush` 调。
    pub fn flush_small_files(&self) -> anyhow::Result<()> {
        self.flush_completions()
    }

    /// 小文件去抖落盘线程（生产由协调器起；测试 / 工具靠 `Drop` 落盘）。只持 `Weak`。
    /// 顺带结束超过租约的外部编辑会话。
    pub fn spawn_small_file_flusher(self: &std::sync::Arc<Self>, every: std::time::Duration) {
        let weak = std::sync::Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("store-small-files".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(every);
                    let Some(me) = weak.upgrade() else { return };
                    if let Err(e) = me.flush_small_files() {
                        warn!("小文件落盘失败：{e:#}");
                    }
                    me.expire_edit_sessions(crate::external_edit::EDIT_LEASE);
                }
            });
    }

    pub(crate) fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// 运行时数据引擎。
    pub fn runtime(&self) -> &std::sync::Arc<wind_userdata::runtime::Runtime> {
        &self.runtime
    }

    /// 门面写入之后：攒够了就起后台折回（判定 O(目标数)，单飞）。
    #[allow(dead_code)]
    pub(crate) fn after_write(&self) {
        let d = self.runtime.data();
        if d.needs_maintain() {
            d.spawn_maintain();
        }
    }

    /// 建表（首次打开表即创建；幂等）。
    fn init_tables(db: &Database) -> anyhow::Result<()> {
        let w = db.begin_write()?;
        {
            w.open_table(USER_WORDS)?;
            w.open_table(USER_ABBREV)?;
            w.open_table(TEMP_WORDS)?;
            w.open_table(TEMP_ABBREV)?;
            w.open_table(ASSOC_HIST)?;
            w.open_table(DRAFT_WORDS)?;
            w.open_table(FREQ)?;
            w.open_table(SHADOW)?;
            w.open_table(QUICK_FORMAT)?;
            w.open_table(COMMON_CHARS)?;
            w.open_table(CHARSET_USER)?;
            w.open_table(COMPLETION)?;
            w.open_table(PHRASES)?;
            w.open_table(STATS_DAILY)?;
            w.open_table(META)?;
        }
        w.commit()?;
        Ok(())
    }

    /// 在持有 db 的前提下执行闭包；暂停态返回错误。各模块 ops（user_words/temp_words…）经此访问 db。
    pub(crate) fn with_db<R>(
        &self,
        f: impl FnOnce(&Database) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        // 空闲回收的唯一信号源。放在这里而不是各个读写函数里：`with_db` 是**所有**
        // 读写的唯一入口（AGENTS.md 的硬约定），漏不掉。
        //
        // 进出各置一次：空闲要从操作**结束**时算起。只在入口置位的话，一次很长的 `with_db`
        // （单事务导入、冷扫描）跑着的时候回收线程就在攒静默拍数，它一结束就被立刻关库重开
        // ——还会与批量写之后的立即回收撞成两次。
        self.touched
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let r = match guard.as_ref() {
            Some(db) => f(db),
            None => anyhow::bail!("store is paused"),
        };
        self.touched
            .store(true, std::sync::atomic::Ordering::Relaxed);
        r
    }

    /// 用户词 / 临时词两表的结构代次，见字段 `words_gen`。
    pub fn words_generation(&self) -> u64 {
        self.words_gen.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 某方案的结构代次：`(全体纪元, 该方案计数)`，任一分量变化即说明该方案的用户词 / 临时词变了。
    pub fn words_generation_of(&self, schema: &str) -> (u64, u64) {
        let epoch = self.words_epoch.load(std::sync::atomic::Ordering::Acquire);
        let n = self
            .words_gen_by_schema
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema)
            .copied()
            .unwrap_or(0);
        (epoch, n)
    }

    pub(crate) fn bump_words_gen(&self, schema: &str) {
        self.words_gen
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        *self
            .words_gen_by_schema
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(schema.to_string())
            .or_default() += 1;
    }

    /// 不分方案的结构变化（整库还原）：全局与纪元一起推进。
    pub(crate) fn bump_words_epoch(&self) {
        self.words_gen
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.words_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// 临时词 count 代次，见字段 `words_count_gen`。
    pub fn words_count_generation(&self) -> u64 {
        self.words_count_gen
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn bump_words_count_gen(&self) {
        self.words_count_gen
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// 读取存储版本（无 version 键视为 0=全新库）。
    pub fn version(&self) -> anyhow::Result<u32> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(META)?;
            let v = match t.get(META_VERSION_KEY)? {
                Some(g) => {
                    let b = g.value();
                    if b.len() == 4 {
                        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
                    } else {
                        0
                    }
                }
                None => 0,
            };
            Ok(v)
        })
    }

    /// 读 META 表的字符串值（UTF-8）。
    pub(crate) fn meta_get(&self, key: &str) -> anyhow::Result<Option<String>> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(META)?;
            Ok(t.get(key)?
                .map(|g| String::from_utf8_lossy(g.value()).into_owned()))
        })
    }

    /// 写 META 表的字符串值（P4a 起只剩迁移测试造旧库用）。
    pub(crate) fn meta_set(&self, key: &str, val: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(META)?;
                t.insert(key, val.as_bytes())?;
            }
            txn.commit()?;
            Ok(())
        })
    }

    fn set_version(&self, v: u32) -> anyhow::Result<()> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(META)?;
                let vb = v.to_le_bytes();
                t.insert(META_VERSION_KEY, vb.as_slice())?;
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 版本迁移框架：全新库直接打版本号；旧库按版本链逐步迁移（当前无迁移步骤）。
    fn run_migrations(&self) -> anyhow::Result<()> {
        let mut v = self.version()?;
        if v == 0 {
            // 全新 redb 库（Go 用 bbolt，此处不存在 legacy redb 数据）→ 直接标当前版本。
            self.set_version(CURRENT_VERSION)?;
            return Ok(());
        }
        while v < CURRENT_VERSION {
            // 预留：match v { 1 => migrate_v1_to_v2()?, .. }
            v += 1;
            self.set_version(v)?;
        }
        if v > CURRENT_VERSION {
            warn!(
                "Store version {} 高于支持的 {}（程序可能被回滚）",
                v, CURRENT_VERSION
            );
        }
        Ok(())
    }

    /// 暂停：丢弃 Database，释放文件锁（Windows 下原子热替换 .redb 前调用）。
    pub fn pause(&self) -> anyhow::Result<()> {
        let mut guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
        info!("Store paused: {}", self.path.display());
        Ok(())
    }

    /// 恢复：重新打开 Database（暂停后调用）。
    pub fn resume(&self) -> anyhow::Result<()> {
        let mut guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let db = open_db(&self.path, self.cache_bytes)?;
            Self::init_tables(&db)?;
            *guard = Some(db);
            self.bump_words_epoch();
            info!("Store resumed: {}", self.path.display());
        }
        Ok(())
    }

    /// 丢掉 redb 的页缓存：**一次锁内**丢弃 `Database` 再重开。
    ///
    /// # 它解决什么
    ///
    /// redb 2.x 不是 mmap，它自己在普通文件 IO 之上维护 `PagedCachedFile`，页是堆上的
    /// `Arc<[u8]>`，**全额计进进程私有内存**。这个缓存只增不减：每次读未命中就插入，
    /// 只有越过上限才淘汰，唯一的整体清空是库文件扩容；事务刷盘时脏页还会被直接晋升
    /// 进读缓存。于是它是个高水位结构，上界 ≈ `min(读缓存配额, 库文件大小)`——本机实测
    /// 19 万词的库（32.6 MB）全表扫过之后吃掉 **41 MB 并长期不还**，50 万词约 90 MB。
    /// 这正是用户反馈「导入大词库之后内存占用变高」的正主（`tests/redb_cache_high_water.rs`）。
    ///
    /// 导入必然写全表、脏页必然晋升，那一笔躲不掉，只能事后还。
    ///
    /// # 为什么不是 `pause()` + `resume()`
    ///
    /// 那是两次独立取锁，中间存在一个 `db` 为 `None` 的窗口，落在这个窗口里的查询会拿到
    /// `store is paused` 的错误——按键线路正跑着的时候这就是一次吞字。本函数在**同一个
    /// guard 里**换掉，对外没有可观测的暂停态。
    ///
    /// # 代价与边界
    ///
    /// 重开 `Database` 要读回 header 并校验，毫秒级；随后头几次查询会走冷缓存。所以它
    /// **只该用在「刚做完一次全表规模的操作」之后**，绝不能进按键链路。
    ///
    /// ⚠️ **重开失败时自救一次**：`*guard = None` 已经执行，库已经丢了，此时若直接返回
    /// `Err` 就把 store 永久钉在暂停态——之后每一次 `with_db` 都 bail「store is paused」，
    /// 用户词 / 临时词 / 词频 / shadow 全线失效直到重启，而 [`Self::resume`] 在生产代码里
    /// **一处调用都没有**（全仓只出现在测试里）。失败是真会发生的：重开要重新打开文件，
    /// 而这个函数恰好被安排在「刚写完几十 MB 库文件」之后，Windows 上杀软短暂持有 `.redb`
    /// 正是本仓当初做 pause/resume 的理由。
    ///
    /// 所以这里重试一次；再失败才返回 `Err`，并且**那时 store 确实是暂停态**，调用方必须
    /// 当成故障处理（`error!` 而非 `warn!`），不能当成「只是内存没回落」。
    ///
    /// ⚠️ 「缓存释放了」不等于「内存还给 OS 了」：页是 4 KiB 的小块，glibc 与 Windows 堆
    /// 都会把它们留在 free list 里，RSS / Private Bytes 未必立刻下降。释放的意义是这笔
    /// 空间不再增长、且可被后续分配复用——`tests/redb_cache_high_water.rs` 的
    /// `evict` / `hold` 两组正是为了把这件事和「RSS 降不降」分开而写的。
    ///
    /// 返回 `Ok(true)` = 真丢了一次（计入 [`Self::page_cache_drops`]，含重试成功的那次）；
    /// `Ok(false)` = 本就是暂停态，没有缓存可丢——调用方不该当成一次回收（不记日志、不整理堆）。
    pub fn drop_page_cache(&self) -> anyhow::Result<bool> {
        Ok(self.drop_page_cache_inner(false)? == Dropped::Yes)
    }

    /// `if_idle`：回收线程用。拿到 `db` 锁之后若发现 `touched` 已被置位就作罢（[`Dropped::Busy`]）
    /// ——它是在锁外判定「空闲够久」的，判定之后可能正撞上一次长 `with_db`（单事务导入、冷扫描）：
    /// 那次操作持锁期间回收线程在锁上等，操作一结束就会被关库重开。`with_db` 在放锁**之前**
    /// 再置一次 `touched`，所以这里一定看得到。
    fn drop_page_cache_inner(&self, if_idle: bool) -> anyhow::Result<Dropped> {
        let mut guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        if if_idle && self.touched.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(Dropped::Busy);
        }
        // 不论哪条路触发、丢没丢成，扫描留下的待回收标记都作废：丢了自不必说；暂停态
        // 根本没有缓存，留着它只会让回收线程一直按快拍空转。
        self.scan_pending
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if guard.is_none() {
            return Ok(Dropped::Paused); // 已经是暂停态，没有缓存可丢；由 `resume` 负责开回来。
        }
        *guard = None;
        // 丢了就必须开回来。第一次失败重试一次——最常见的原因（杀软/索引器短暂持有刚写完
        // 的 .redb）是瞬时的。两次都不行才认栽，那时 store 停在暂停态，由调用方按故障处理。
        //
        // 重开**不跑 `init_tables`**（`resume` 仍跑）：这是本进程刚关掉的同一个文件，表在
        // `open` 时已建好，期间没有任何东西替换它；而 `resume` 面对的可能是备份还原换进来的
        // 另一个文件，要补表。省掉的是一次写事务提交（锁内多持的那一下）。
        let mut last = match open_db(&self.path, self.cache_bytes) {
            Ok(db) => {
                *guard = Some(db);
                self.drops
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                debug!("Store page cache dropped: {}", self.path.display());
                return Ok(Dropped::Yes);
            }
            Err(e) => anyhow::Error::from(e),
        };
        for _ in 0..1 {
            match open_db(&self.path, self.cache_bytes) {
                Ok(db) => {
                    *guard = Some(db);
                    self.drops
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    warn!(
                        "丢弃页缓存后首次重开失败（{last}），重试成功：{}",
                        self.path.display()
                    );
                    return Ok(Dropped::Yes);
                }
                Err(e) => last = e.into(),
            }
        }
        Err(last.context(format!(
            "丢弃页缓存后重开数据库失败，store 已停在暂停态：{}。\
             此后所有读写都会报 store is paused，需调 resume() 或重启服务",
            self.path.display()
        )))
    }

    /// 累计丢弃页缓存的次数。诊断与测试用。
    pub fn page_cache_drops(&self) -> u64 {
        self.drops.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 起一条后台线程：**空闲够久就把页缓存还回去**。
    ///
    /// # 为什么光靠「写完就丢」不够
    ///
    /// 导入之后接 [`Self::drop_page_cache`] 只堵住了写那一路。真正把高水位顶起来的还有
    /// 读：设置页的分页列表要给出精确的 `total`，就**不能中途早退**，于是照样扫完整表、
    /// 把走过的叶子页全填进读缓存。用户导入完随手翻一下词库标签，41 MB 就原样回来了。
    /// 正常打字也在慢慢填（简拼召回、前缀补全都读页），只是慢得多。
    ///
    /// redb 没有按范围计数的 API（`len()` 是 O(1) 但只给**整表**，而我们的 key 是
    /// `schema\0code\0text`、一张表混着所有方案），所以「不扫全表也能拿到 total」这条路
    /// 要自己维护计数器，写路径一多就会漏。与其在每条路上堵，不如在**空闲时统一回收**：
    /// 这条覆盖所有填缓存的路径，包括将来新加的。
    ///
    /// # 判据（两档，都要求上次回收之后访问过：`pending`）
    ///
    /// - **长档**：连续 `idle` 没有任何 `with_db` 就丢一次——兜所有填缓存的路径。
    /// - **短档**：有人调过 [`Self::mark_scan_pending`]（设置页列表、建索引这类全表扫描
    ///   刚做完），连续 `scan_idle` 没有 `with_db` 就丢一次并清标志。设置页连续翻页、打字中
    ///   自动造词反复触发索引重建时都在不停访问库，等不满短档，故不逐次回收；停手之后很快
    ///   还回去，不必等长档（持续打字时长档永远等不到）。
    ///
    /// 回收**只在这条线程上**发生（批量写之后的 `drop_page_cache` 除外）：扫描线程只置标志，
    /// 从不当场关库重开，按键线程不会因建索引而等那一下。
    ///
    /// 不设 `pending` 的话，一个没人用的输入法会每分钟白白重开一次数据库。空闲按 `tick`
    /// 采样，实际判定比给定时长至多晚一拍；生产取值见 [`RECLAIM_IDLE`] 等三个常量。
    ///
    /// 丢缓存本身要重开 `Database`（毫秒级）并让随后头几次查询走冷缓存，所以判据必须是
    /// 「真的没人在用」——那时这点代价没人感知得到。冷缓存也不等于慢：redb 2.x 未命中走
    /// `read` 系统调用，下面还垫着 OS 页缓存，是 µs 级 syscall 而非磁盘寻道。
    ///
    /// 线程持 `Weak`：`Store` 被丢弃后它自己退出，不拖住进程。
    ///
    /// ⚠️ 间隔**不做成配置键**（`docs/architecture/config-design-rules.md` §R1：差异可由
    /// 程序判定就别加用户键）。参数化只为测试能用毫秒级的值跑完。
    pub fn spawn_idle_cache_reclaimer(
        self: &std::sync::Arc<Self>,
        idle: std::time::Duration,
        scan_idle: std::time::Duration,
        tick: std::time::Duration,
    ) {
        self.spawn_idle_cache_reclaimer_then(idle, scan_idle, tick, || {});
    }

    /// 同 [`Self::spawn_idle_cache_reclaimer`]，每次回收后再调 `after`。
    ///
    /// 用途是让上层在同一个「真的没人在用」的时刻把空闲堆还给系统：丢掉的页缓存只是
    /// 回到分配器手里，不整理堆的话私有内存一点不降（本 crate 不碰平台分配器，故留钩子）。
    pub fn spawn_idle_cache_reclaimer_then(
        self: &std::sync::Arc<Self>,
        idle: std::time::Duration,
        scan_idle: std::time::Duration,
        tick: std::time::Duration,
        after: impl Fn() + Send + 'static,
    ) {
        let weak = std::sync::Arc::downgrade(self);
        let wake = self.reclaim_wake.clone();
        let ticks = |d: std::time::Duration| (d.as_millis() / tick.as_millis().max(1)).max(1);
        let (ticks_to_idle, ticks_to_scan_idle) = (ticks(idle), ticks(scan_idle));
        let slow_tick = tick * IDLE_TICK_FACTOR;
        let spawned = std::thread::Builder::new()
            .name("store-cache-reclaim".into())
            .spawn(move || {
                let mut quiet = 0u128;
                let mut pending = false;
                // 本拍是否按快拍睡：只有「有东西待回收」时才需要按 `tick` 数静默拍。
                let mut busy = false;
                loop {
                    if busy {
                        std::thread::sleep(tick);
                    } else {
                        // 闲置：按慢拍睡，有标记就被叫醒。这期间没在数静默拍（`pending` 为假时
                        // 本就不数），所以拍长换算不受影响；醒来看到 `touched` 再转快拍从零数。
                        let (lock, cv) = &*wake;
                        let mut kicked = lock.lock().unwrap_or_else(|e| e.into_inner());
                        if !*kicked {
                            kicked = cv
                                .wait_timeout(kicked, slow_tick)
                                .unwrap_or_else(|e| e.into_inner())
                                .0;
                        }
                        *kicked = false;
                    }
                    let Some(store) = weak.upgrade() else { return };
                    busy = pending
                        || store
                            .scan_pending
                            .load(std::sync::atomic::Ordering::Relaxed);
                    if store
                        .touched
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        // 这一拍里有人用过：静默计数归零，并记下「缓存可能涨了」。
                        quiet = 0;
                        pending = true;
                        busy = true;
                        continue;
                    }
                    quiet += 1;
                    if !pending {
                        continue;
                    }
                    let scanned = store
                        .scan_pending
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let due = if scanned && quiet >= ticks_to_scan_idle {
                        scan_idle
                    } else if quiet >= ticks_to_idle {
                        idle
                    } else {
                        continue;
                    };
                    let dropped = match store.drop_page_cache_inner(true) {
                        Ok(Dropped::Yes) => {
                            debug!("空闲 {due:?}，已回收 redb 页缓存");
                            true
                        }
                        // 判定之后有人用过库（多半是一次长操作刚结束）：不算空闲，下一拍
                        // 看到 `touched` 会把静默计数归零重来。
                        Ok(Dropped::Busy) => continue,
                        // 暂停态：没有缓存可丢，也就没有堆可整理。
                        Ok(Dropped::Paused) => false,
                        // 回收失败时 `drop_page_cache` 已经自救过一次仍不成，store
                        // 停在暂停态——那是故障，不是「内存没回落」。
                        Err(e) => {
                            error!("空闲回收页缓存失败：{e}");
                            false
                        }
                    };
                    drop(store);
                    if dropped {
                        after();
                    }
                    pending = false;
                    busy = false;
                    quiet = 0;
                }
            });
        if let Err(e) = spawned {
            // 不致命：只是内存不会自动回落，功能一切照旧。
            warn!("空闲页缓存回收线程启动失败（{e}），内存将不会自动回落");
        }
    }

    /// **后台**全表扫描（建索引）做完之后调：走过的行数到 [`SCAN_RECLAIM_MIN_ROWS`] 才
    /// [`Self::mark_scan_pending`]，否则什么都不做。
    ///
    /// 不当场回收：拼音出厂开着自动造词，打字中每学一个新词都推进用户词代次、触发一次整表
    /// 重建；每次都关库重开的话，按键线程的 `with_db` 要陪等（≥ 7 ms，Windows 上更久），
    /// 而且每次重建都从冷缓存起扫。改成标记之后，连续打字期间一次都不回收，停手满
    /// [`SCAN_RECLAIM_IDLE`] 才还（`docs/design/memory-footprint.md` §6）。
    pub fn mark_scan_pending_rows(&self, rows_scanned: usize) {
        if rows_scanned >= SCAN_RECLAIM_MIN_ROWS {
            self.mark_scan_pending();
        }
    }

    /// 全表扫描（设置页列表、导出、统计；建索引走 [`Self::mark_scan_pending_rows`]）做完
    /// 之后调：只置「有扫描待回收」，由回收线程在库空闲 [`SCAN_RECLAIM_IDLE`] 后回收一次
    /// （见 [`Self::spawn_idle_cache_reclaimer`] 的短档）。
    ///
    /// 不立即回收：设置页连续翻页时每页都是一次全表扫，逐页回收等于每页都从冷缓存起扫，
    /// 还白白重开 N 次库。不设行数门槛：请求方拿不到行数，而设置页操作稀少、回收发生在
    /// 空闲时，小库多回收一次的代价可以忽略。没起回收线程时标志无人消费，等于什么都不做。
    pub fn mark_scan_pending(&self) {
        // 标记总是紧跟在一次访问之后（扫描刚做完）：也记一次访问，空闲从此刻算起，且回收后
        // 单独一次标记也能生效。
        self.touched
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.scan_pending
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (lock, cv) = &*self.reclaim_wake;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_one();
    }

    /// 是否处于暂停态
    pub fn is_paused(&self) -> bool {
        self.db.lock().unwrap_or_else(|e| e.into_inner()).is_none()
    }

    /// 数据库路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 删除一个数据方案（`data/<ds>/` 整个目录与 `.backup/<ds>/`）的全部用户数据，返回删掉的文件
    /// （相对数据目录、`/` 分隔）。先清各目标（待写回的操作随之摘掉，之后不会写回），再删目录。
    /// 该方案有文件正在外部编辑 ⇒ 拒绝。统计不分方案，不动。
    pub fn purge_data_schema(&self, ds: &str) -> anyhow::Result<Vec<String>> {
        wind_userdata::engine::check_schema_id(ds)?;
        // `data/` 下与数据方案同级的保留目录：删了就是全部统计 / 字符分类调整
        if ["stats", "charsets"]
            .iter()
            .any(|r| ds.eq_ignore_ascii_case(r))
        {
            anyhow::bail!("{ds} 不是数据方案");
        }
        if self.runtime.data().readonly() {
            anyhow::bail!("只读模式：本次运行不保存修改");
        }
        let prefix = format!("{ds}/");
        if let Some((f, _)) = self
            .edit_sessions()
            .into_iter()
            .find(|(f, _)| f.starts_with(&prefix))
        {
            anyhow::bail!("正在外部编辑 {f}，完成后再删");
        }
        let root = self.text_dir.clone();
        let dirs = [
            root.join(ds),
            root.join(wind_userdata::text::fold::BACKUP_DIR).join(ds),
        ];
        let mut removed = Vec::new();
        for d in &dirs {
            let mut stack = vec![d.clone()];
            while let Some(dir) = stack.pop() {
                for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    let p = e.path();
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push(p);
                    } else if let Ok(rel) = p.strip_prefix(&root) {
                        removed.push(
                            rel.components()
                                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                                .collect::<Vec<_>>()
                                .join("/"),
                        );
                    }
                }
            }
        }
        removed.sort();
        self.clear_user_words(ds)?;
        self.clear_temp_words(ds)?;
        self.clear_drafts(ds)?;
        self.clear_freq(ds)?;
        self.clear_assoc_history(ds)?;
        self.clear_shadow(ds)?;
        self.shadows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(ds);
        for d in &dirs {
            match std::fs::remove_dir_all(d) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        for i in crate::issues::list_under(&dirs[0]) {
            crate::issues::clear(&i.path);
        }
        self.bump_words_gen(ds);
        info!(
            "已删除数据方案 {ds} 的全部用户数据（{} 个文件）",
            removed.len()
        );
        Ok(removed)
    }

    /// 枚举出现过的全部数据方案 id：redb 的用户词 / shadow 表 + 运行时引擎（临时词 / 草稿 / 词频 /
    /// 联想历史）。
    /// 备份用：确保有数据但未在当前配置启用的方案也被覆盖。
    pub fn list_data_schemas(&self) -> anyhow::Result<Vec<String>> {
        // 临时词 / 词频 / 联想历史（P1）与用户词（P2）在运行时引擎，`runtime.schemas()` 已含；
        // Shadow（P3）在 `user/<schema>/shadow.dict.yaml`，有规则的才算
        let mut set: std::collections::BTreeSet<String> = self.runtime.schemas();
        for sc in self.shadow_schemas_on_disk() {
            if !set.contains(&sc) && !self.list_shadow_rules(&sc)?.is_empty() {
                set.insert(sc);
            }
        }
        Ok(set.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// redb 在同一进程里对已打开的文件拒绝再开：能另开一份 ⇒ Store 没握着它（不锁文件）。
    fn redb_is_released(path: &Path) -> bool {
        Database::open(path).is_ok()
    }

    /// 迁移全部完成（短语迁移是最后一步）后立即放下旧库：不再锁文件。
    #[test]
    fn legacy_db_is_released_once_fully_migrated() {
        let path =
            std::env::temp_dir().join(format!("wind_store_release-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(derived_state_dir(&path));
        let _ = std::fs::remove_dir_all(derived_text_dir(&path));
        let s = Store::open(&path).unwrap();
        assert!(!redb_is_released(&path), "短语还没迁，旧库要留着");
        s.migrate_phrases_now().unwrap();
        assert!(s.migrated_p3() && s.migrated_p4());
        assert!(redb_is_released(&path), "全部迁完就放下旧库");
        drop(s);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(derived_state_dir(&path));
        let _ = std::fs::remove_dir_all(derived_text_dir(&path));
    }

    /// 用户目录已记为迁完（`legacy_done`，来自 meta.toml 的 format_version）时，state 里的迁移
    /// 标记被删了也不再从 redb 重迁：升级后删掉的词不会被旧表带回来，redb 不开、不写；
    /// 标记不补写（P5 起判据只认 format_version），其余依赖旧库链完成的判断照旧。
    #[test]
    fn legacy_done_skips_remigration_even_if_markers_are_deleted() {
        let path =
            std::env::temp_dir().join(format!("wind_store_ldone-{}.redb", std::process::id()));
        let st =
            std::env::temp_dir().join(format!("wind_store_ldone_state-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&st);
        let loc = |done: bool| StateLoc {
            state_dir: st.clone(),
            dir_id: "d".into(),
            text_dir: st.join(crate::DATA_DIR_NAME),
            legacy_done: done,
        };
        let del_markers = || {
            for m in crate::migrate_p3::ALL_MIGRATION_MARKERS {
                let _ = std::fs::remove_file(st.join(m));
            }
        };
        {
            let s = Store::open_with_state(&path, loc(false)).unwrap();
            s.legacy_put_user("wb", "a", "工", 1, 0, 0).unwrap();
        }
        // 旧表里的词迁成文本，然后用户在新版本里删掉它
        del_markers();
        {
            let s = Store::open_with_state(&path, loc(false)).unwrap();
            s.migrate_phrases_now().unwrap();
            assert_eq!(s.get_user_words("wb", "a").unwrap().len(), 1, "迁进来了");
            s.remove_user_word("wb", "a", "工").unwrap();
            s.flush_user_data().unwrap();
        }
        del_markers();
        let before = std::fs::read(&path).unwrap();
        let s = Store::open_with_state(&path, loc(true)).unwrap();
        assert!(
            s.get_user_words("wb", "a").unwrap().is_empty(),
            "删掉的词不被旧表带回"
        );
        assert!(redb_is_released(&path), "不打开旧库");
        assert!(
            crate::migrate_p3::ALL_MIGRATION_MARKERS
                .iter()
                .all(|m| !st.join(m).exists()),
            "判据只认 format_version：标记不再补写（P5）"
        );
        drop(s);
        assert!(std::fs::read(&path).unwrap() == before, "旧库文件不被写");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&st);
    }

    /// 已经迁完的库再打开：根本不碰 redb（不锁、不写——文件逐字节不变，留给回退的旧版本）。
    #[test]
    fn fully_migrated_store_never_opens_legacy_db() {
        let path =
            std::env::temp_dir().join(format!("wind_store_noopen-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(derived_state_dir(&path));
        let _ = std::fs::remove_dir_all(derived_text_dir(&path));
        {
            let s = Store::open(&path).unwrap();
            s.migrate_phrases_now().unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let s = Store::open(&path).unwrap();
        assert!(redb_is_released(&path), "迁完的库重开不再打开旧库");
        s.add_user_word("wb", "a", "工", 1, 0).unwrap();
        assert_eq!(
            s.get_user_words("wb", "a").unwrap()[0].text,
            "工",
            "照常读写"
        );
        drop(s);
        assert!(std::fs::read(&path).unwrap() == before, "旧库文件不被写");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(derived_state_dir(&path));
        let _ = std::fs::remove_dir_all(derived_text_dir(&path));
    }

    #[test]
    fn list_data_schemas_across_tables() {
        let path = std::env::temp_dir().join("wind_store_schemas_test.redb");
        let _ = std::fs::remove_file(&path);
        let s = Store::open(&path).unwrap();
        s.add_user_word("wb", "a", "工", 1, 0).unwrap();
        s.learn_temp_word("py", "ni", "你", 1, 0).unwrap();
        s.record_freq("sp", "x", "词").unwrap();
        s.pin_shadow("wb", "aa", "恭", None, 0).unwrap();
        let mut got = s.list_data_schemas().unwrap();
        got.sort();
        assert_eq!(got, vec!["py", "sp", "wb"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_open_version_persist_and_reopen() {
        let path = std::env::temp_dir().join("wind_store_skeleton_test.redb");
        let _ = std::fs::remove_file(&path);

        // 首次打开：版本应为当前版本
        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.version().unwrap(), CURRENT_VERSION);
        }
        // 重开：版本持久化（证明写事务落盘）
        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.version().unwrap(), CURRENT_VERSION);
            // pause/resume 往返：暂停态报错，恢复后可用
            s.pause().unwrap();
            assert!(s.is_paused());
            assert!(s.version().is_err(), "暂停态读取应失败");
            s.resume().unwrap();
            assert!(!s.is_paused());
            assert_eq!(s.version().unwrap(), CURRENT_VERSION);
        }
        let _ = std::fs::remove_file(&path);
    }
    fn tmp_db(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wind_store_rt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d.join(format!("{name}.redb"))
    }

    #[test]
    fn open_attaches_runtime_in_derived_state_dir() {
        let p = tmp_db("derived");
        let _ = std::fs::remove_file(&p);
        let s = Store::open(&p).unwrap();
        s.runtime().learn_temp("py", "a", "甲", 1, 0).unwrap();
        assert!(derived_state_dir(&p).join("journal").is_dir());
        drop(s);
        let s = Store::open(&p).unwrap();
        assert!(
            s.runtime().temp_word("py", "a", "甲").is_some(),
            "同一库重开，引擎数据仍在"
        );
    }

    /// P5：学到的数据写进用户数据目录的文本（复制用户目录即带走）。
    #[test]
    fn learned_data_is_written_to_data_dir() {
        let p = tmp_db("learned_text");
        let _ = std::fs::remove_file(&p);
        let s = Store::open(&p).unwrap();
        s.learn_temp_word("pinyin", "nihao", "你好", 10, 0b101)
            .unwrap();
        s.record_freq("pinyin", "nihao", "你好").unwrap();
        s.record_assoc_pick("pinyin", "我们", "我们的").unwrap();
        s.flush_user_data().unwrap();
        let dir = s.user_text_dir().join("pinyin");
        let temp = std::fs::read_to_string(dir.join("temp.dict.yaml")).unwrap();
        assert!(temp.contains("ni hao\t你好\t10\t1\t"), "{temp}");
        assert!(dir.join("freq.table.yaml").is_file());
        assert!(dir.join("assoc.table.yaml").is_file());
    }

    /// 每类程序写出的数据文件都写一份（文件 → 说明行见 [`banner_table`]）。
    fn fill_every_file(s: &Store) {
        s.add_user_word("pinyin", "nihao", "你好", 1, 0b101)
            .unwrap();
        s.learn_temp_word("pinyin", "zaijian", "再见", 1, 0b1001)
            .unwrap();
        s.add_drafts("pinyin", &[("haode".into(), "好的".into())])
            .unwrap();
        s.record_freq("pinyin", "nihao", "你好").unwrap();
        s.record_assoc_pick("pinyin", "我们", "我们的").unwrap();
        s.pin_shadow("pinyin", "ni", "你", None, 0).unwrap();
        s.add_phrase("dz", "地址", 0, 1).unwrap();
        s.record_completion(crate::completion::CompletionKind::EmailSuffix, "qq.com")
            .unwrap();
        s.set_quick_format_enabled("date", "date.lunar", false)
            .unwrap();
        s.set_common_char_override("的", false).unwrap();
        s.record_stat("2026-10-16", 3, 0).unwrap();
        s.put_stats_meta(&crate::stats::StatsMeta::default())
            .unwrap();
        s.set_phrase_sys_hash("h1").unwrap();
        s.set_user_dict_enabled("pinyin", "pinyin", "x", false)
            .unwrap();
        s.flush_user_data().unwrap();
    }

    fn banner_table() -> Vec<(&'static str, &'static str)> {
        use wind_userdata::text::banner;
        vec![
            ("pinyin/user.dict.yaml", banner::USER_DICT),
            ("pinyin/temp.dict.yaml", banner::TEMP),
            ("pinyin/draft.dict.yaml", banner::DRAFT),
            ("pinyin/freq.table.yaml", banner::FREQ),
            ("pinyin/assoc.table.yaml", banner::ASSOC),
            ("pinyin/shadow.dict.yaml", banner::SHADOW),
            ("pinyin/dicts.toml", banner::USER_DICTS_META),
            ("phrases.dict.yaml", banner::PHRASES),
            ("completion.table.yaml", banner::COMPLETION),
            ("quick_format.toml", banner::QUICK_FORMAT),
            ("common_chars.yaml", banner::COMMON_CHARS),
            ("stats/2026-10.toml", banner::STATS),
            ("stats/meta.toml", banner::STATS),
            ("phrases.sys_hash", banner::PHRASE_SYS_HASH),
        ]
    }

    /// `data/` 下程序写出的每个文本文件，第一行是各自的说明行。
    #[test]
    fn every_data_file_starts_with_its_banner() {
        let p = tmp_db("banner");
        let _ = std::fs::remove_file(&p);
        let s = Store::open(&p).unwrap();
        fill_every_file(&s);
        let d = s.user_text_dir();
        for (f, want) in banner_table() {
            let text = std::fs::read_to_string(d.join(f)).unwrap();
            assert_eq!(text.lines().next(), Some(want), "{f}");
        }
        assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("h1"));
        assert_eq!(s.get_daily_stat("2026-10-16").unwrap().total(), 3);
    }

    /// 打开时给没有说明行的数据文件补上（与整写同一行）：未折回的数据不丢；用户自放的分库、坏文件、
    /// 第一行已是注释的不动；再次打开一个字节都不改。
    #[test]
    fn open_backfills_missing_banners_without_losing_pending_data() {
        use wind_userdata::text::banner;
        let p = tmp_db("backfill");
        let _ = std::fs::remove_file(&p);
        {
            let s = Store::open(&p).unwrap();
            fill_every_file(&s);
            s.set_charset_doc("rare", "key: rare\n").unwrap();
            // 待折回：不 flush
            s.add_user_word("pinyin", "shijie", "世界", 2, 0b1001)
                .unwrap();
            s.learn_temp_word("pinyin", "haoa", "好啊", 1, 0b101)
                .unwrap();
        }
        let d = derived_text_dir(&p);
        let mut table = banner_table();
        table.push(("charsets/rare.yaml", banner::CHARSETS));
        for (f, want) in &table {
            let t = std::fs::read_to_string(d.join(f)).unwrap();
            if let Some(rest) = t.strip_prefix(&format!("{want}\n")) {
                std::fs::write(d.join(f), rest).unwrap();
            }
        }
        let lib = d.join("pinyin/dicts/mine.dict.yaml");
        std::fs::create_dir_all(lib.parent().unwrap()).unwrap();
        let lib_text = "---\nname: mine\ncolumns: [text, code]\n...\n好\thao\n";
        std::fs::write(&lib, lib_text).unwrap();
        let bad = d.join("wubi86/user.dict.yaml");
        std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
        std::fs::write(&bad, "---\nname: [\n").unwrap();
        let own = d.join("wubi86/shadow.dict.yaml");
        std::fs::write(
            &own,
            "# 我自己的说明\n---\ncolumns: [code, text, action]\n...\n",
        )
        .unwrap();

        let s = Store::open(&p).unwrap();
        for (f, want) in &table {
            let text = std::fs::read_to_string(d.join(f)).unwrap();
            assert_eq!(text.lines().next(), Some(*want), "{f}");
        }
        assert_eq!(
            std::fs::read_to_string(&lib).unwrap(),
            lib_text,
            "用户自放的分库不动"
        );
        assert_eq!(
            std::fs::read_to_string(&bad).unwrap(),
            "---\nname: [\n",
            "坏文件不动"
        );
        assert!(
            std::fs::read_to_string(&own)
                .unwrap()
                .starts_with("# 我自己的说明\n")
        );
        s.flush_user_data().unwrap();
        let user = std::fs::read_to_string(d.join("pinyin/user.dict.yaml")).unwrap();
        assert!(user.contains("你好") && user.contains("世界"), "{user}");
        let temp = std::fs::read_to_string(d.join("pinyin/temp.dict.yaml")).unwrap();
        assert!(temp.contains("再见") && temp.contains("好啊"), "{temp}");
        assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("h1"));
        assert!(
            s.get_charset_doc("rare")
                .unwrap()
                .unwrap()
                .contains("key: rare")
        );
        assert!(
            std::fs::read_to_string(d.join("pinyin/user.dict.yaml"))
                .unwrap()
                .starts_with(banner::USER_DICT)
        );
        drop(s);
        let snap: Vec<Vec<u8>> = table
            .iter()
            .map(|(f, _)| std::fs::read(d.join(f)).unwrap())
            .collect();
        let s = Store::open(&p).unwrap();
        for ((f, _), before) in table.iter().zip(&snap) {
            assert_eq!(&std::fs::read(d.join(f)).unwrap(), before, "{f}");
        }
        drop(s);
    }

    /// 删除一个数据方案的全部用户数据：目录与 `.backup/` 里的都删；待写回的操作一并清掉，之后
    /// 写回（压实 / 维护）不会把文件写回来；别的方案不动；正在外部编辑时拒绝。
    #[test]
    fn purge_data_schema_removes_everything_of_that_schema() {
        let p = tmp_db("purge");
        let _ = std::fs::remove_file(&p);
        let s = Store::open(&p).unwrap();
        let d = s.user_text_dir().to_path_buf();
        s.add_user_word("wubi86", "a", "工", 1, 0).unwrap();
        s.flush_user_data().unwrap();
        s.add_user_word("wubi86", "aa", "式", 1, 0).unwrap(); // 未折回
        s.add_user_word("wubi86", "aa", "式", 9, 0).unwrap(); // 产生上一版
        s.flush_user_data().unwrap();
        s.add_user_word("wubi86", "b", "了", 1, 0).unwrap();
        s.learn_temp_word("wubi86", "c", "以", 1, 0).unwrap();
        s.pin_shadow("wubi86", "a", "工", None, 0).unwrap();
        s.create_user_dict("wubi86", "我的").unwrap();
        s.add_user_word("pinyin", "ni", "你", 1, 0b1).unwrap();
        s.flush_user_data().unwrap();
        s.learn_temp_word("wubi86", "d", "在", 1, 0).unwrap();
        assert!(d.join(".backup/wubi86").is_dir());

        let sess = s.begin_edit("wubi86/user.dict.yaml").unwrap();
        let e = s.purge_data_schema("wubi86").unwrap_err().to_string();
        assert!(e.contains("正在外部编辑"), "{e}");
        s.end_edit(&sess.path.to_string_lossy()).unwrap();

        let removed = s.purge_data_schema("wubi86").unwrap();
        for f in [
            "wubi86/user.dict.yaml",
            "wubi86/temp.dict.yaml",
            "wubi86/shadow.dict.yaml",
            "wubi86/dicts/我的.dict.yaml",
            ".backup/wubi86/user.dict.yaml",
        ] {
            assert!(removed.contains(&f.to_string()), "{f}: {removed:?}");
        }
        assert!(!d.join("wubi86").exists());
        assert!(!d.join(".backup/wubi86").exists());
        assert!(
            s.search_user_words_prefix("wubi86", "", 0)
                .unwrap()
                .is_empty()
        );
        assert_eq!(s.count_temp_words("wubi86").unwrap(), 0);
        s.flush_user_data().unwrap();
        s.runtime()
            .data()
            .maintain_text_idle(std::time::Duration::ZERO)
            .unwrap();
        assert!(!d.join("wubi86").exists(), "不写回");
        assert!(d.join("pinyin/user.dict.yaml").is_file(), "别的方案不动");
        assert!(s.purge_data_schema("../x").is_err());
        // 保留目录：统计 / 字符分类不是数据方案（大小写不敏感，Windows 上是同一个目录）
        std::fs::create_dir_all(d.join("stats")).unwrap();
        std::fs::write(d.join("stats/2026-10.toml"), "a = 1\n").unwrap();
        for ds in ["stats", "Stats", "charsets", "CharSets", ".backup"] {
            assert!(s.purge_data_schema(ds).is_err(), "{ds}");
        }
        assert!(d.join("stats/2026-10.toml").is_file());
    }

    /// 打开时把旧版留在数据文件旁边的 `.bak` / `.corrupt-<时间>` 挪进 `data/.backup/`。
    #[test]
    fn open_moves_legacy_backups_into_backup_dir() {
        let p = tmp_db("legacy_bak");
        let _ = std::fs::remove_file(&p);
        {
            let s = Store::open(&p).unwrap();
            s.add_user_word("pinyin", "nihao", "你好", 1, 0b101)
                .unwrap();
            s.flush_user_data().unwrap();
        }
        let d = derived_text_dir(&p);
        // 模拟旧版留下的数据目录：还没迁移过
        std::fs::remove_file(d.join(".backup/.migrated")).unwrap();
        std::fs::write(d.join("pinyin/user.dict.yaml.bak"), "old").unwrap();
        std::fs::write(d.join("phrases.dict.yaml.corrupt-20261016-120000"), "bad").unwrap();
        let s = Store::open(&p).unwrap();
        assert!(!d.join("pinyin/user.dict.yaml.bak").exists());
        assert!(!d.join("phrases.dict.yaml.corrupt-20261016-120000").exists());
        assert!(
            d.join(".backup/phrases.dict.yaml.corrupt-20261016-120000")
                .is_file()
        );
        assert!(d.join(".backup/pinyin/user.dict.yaml").is_file());
        assert!(!s.user_text_schemas().contains(".backup"));
        drop(s);
        // 迁移只跑一次：之后用户自己放的 `.bak`（哪怕与数据文件同名）重启后还在
        std::fs::write(d.join("x.bak"), "mine").unwrap();
        std::fs::write(d.join("pinyin/user.dict.yaml.bak"), "mine").unwrap();
        let _s = Store::open(&p).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("x.bak")).unwrap(), "mine");
        assert!(d.join("pinyin/user.dict.yaml.bak").is_file());
    }

    /// 打开时，`data/` 下已有学到的数据文本（换机复制过来）⇒ 直接绑定、可查。
    #[test]
    fn learned_text_copied_from_another_machine_is_bound_at_open() {
        let p = tmp_db("learned_copy");
        let _ = std::fs::remove_file(&p);
        {
            let s = Store::open(&p).unwrap();
            s.learn_temp_word("pinyin", "nihao", "你好", 10, 0b101)
                .unwrap();
            s.flush_user_data().unwrap();
        }
        let text = derived_text_dir(&p).join("pinyin/temp.dict.yaml");
        let saved = std::fs::read(&text).unwrap();
        std::fs::remove_dir_all(derived_state_dir(&p)).unwrap();
        // 派生模式下 redb 在就不清；只删 state，文本留着
        std::fs::write(&text, &saved).unwrap();
        let s = Store::open(&p).unwrap();
        assert!(s.runtime().temp_word("pinyin", "nihao", "你好").is_some());
    }

    /// Review Focus 4：删掉 redb 再开同一路径 = 新库，派生目录里的旧引擎数据不得残留。
    #[test]
    fn fresh_redb_starts_with_empty_runtime() {
        let p = tmp_db("fresh");
        let _ = std::fs::remove_file(&p);
        let s = Store::open(&p).unwrap();
        s.runtime().learn_temp("py", "a", "甲", 1, 0).unwrap();
        drop(s);
        std::fs::remove_file(&p).unwrap();
        let s = Store::open(&p).unwrap();
        assert!(s.runtime().temp_word("py", "a", "甲").is_none());
    }

    #[test]
    fn open_with_state_uses_the_given_dir_and_never_wipes() {
        let p = tmp_db("explicit");
        let _ = std::fs::remove_file(&p);
        let st = std::env::temp_dir().join(format!("wind_store_rt_state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&st);
        let loc = || StateLoc {
            state_dir: st.clone(),
            dir_id: "dir-x".into(),
            text_dir: st.join(crate::DATA_DIR_NAME),
            legacy_done: false,
        };
        let s = Store::open_with_state(&p, loc()).unwrap();
        s.runtime().learn_temp("py", "a", "甲", 1, 0).unwrap();
        drop(s);
        std::fs::remove_file(&p).unwrap();
        let s = Store::open_with_state(&p, loc()).unwrap();
        assert!(
            s.runtime().temp_word("py", "a", "甲").is_some(),
            "显式状态目录不随 redb 清空"
        );
        assert!(!derived_state_dir(&p).exists());
    }

    /// 一次很长的 `with_db`（单事务导入、冷扫描）期间不许回收；空闲要从它**结束**时算起——
    /// 否则它一结束回收线程就立刻关库重开（还会与批量写之后的立即回收撞在一起）。
    /// （原在 `tests/scan_reclaim.rs`，借用户词遍历的回调制造长操作；P2 起用户词不经 redb，挪到
    /// 这里直接用 `with_db`。）
    #[test]
    fn a_long_operation_is_not_idle_time() {
        use std::time::Duration;
        let tick = Duration::from_millis(20);
        let scan_idle = Duration::from_millis(100);
        let p = tmp_db("long_op");
        let _ = std::fs::remove_file(&p);
        let s = std::sync::Arc::new(Store::open(&p).unwrap());
        s.spawn_idle_cache_reclaimer(Duration::from_secs(3600), scan_idle, tick);
        s.mark_scan_pending();
        s.with_db(|_| {
            std::thread::sleep(scan_idle * 4);
            Ok(())
        })
        .unwrap();
        std::thread::sleep(scan_idle / 2);
        assert_eq!(
            s.page_cache_drops(),
            0,
            "长操作刚结束、还没空闲满短档，不该回收"
        );
        let t0 = std::time::Instant::now();
        while s.page_cache_drops() < 1 && t0.elapsed() < scan_idle * 20 {
            std::thread::sleep(tick / 2);
        }
        assert!(s.page_cache_drops() >= 1, "结束后空闲满短档才回收");
    }
}
