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

/// redb 页缓存上限（redb 默认 1 GiB）。是上限不是预分配，实际占用 ≤ `min(本值, 库文件大小)`；
/// 作用是给超大词库一个显式上界。
///
/// 读路径对本值几乎不敏感，写路径敏感（`set_cache_size` 把 10% 划给写缓存，批量导入放不下
/// 脏页就反复外溢）。取值依据见 `docs/design/user-dict-abbrev-index.md` §7。
pub const DEFAULT_CACHE_SIZE_BYTES: usize = 256 * 1024 * 1024;

/// 空闲回收长档：连续这么久没有任何读写就丢页缓存（兜所有填缓存的路径）。
///
/// 这三个时长不做成配置键（`docs/architecture/config-design-rules.md` §R1：差异可由
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
/// 回收要关库重开（关库要刷盘，毫秒级），小库扫进缓存的那点页不值得每次停手都来这一下，
/// 交给长档。
///
/// 两处调用方的口径不同：`UserTextIndex` 只数该方案的用户词，`UserAssocIndex` 数用户词 +
/// 临时词。可以接受：各自数的正是各自扫过的行，只影响门槛附近的库走短档还是长档。
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
/// 用户词简拼索引：key = `"{schema}\0{group}\0{code}\0{text}"`，value 空。
/// 键的构成、分组规则与维护契约全在 [`crate::abbrev_index`]。
///
/// P2 起退役：用户词简拼走引擎的分组索引，本表只读保留到 P4。
pub(crate) const USER_ABBREV: TableDefinition<&str, &[u8]> = TableDefinition::new("user_abbrev");
/// 临时词简拼索引：与 [`USER_ABBREV`] 同构，索引 `TEMP_WORDS`。简拼召回是跨层的，两层都要索引。
pub(crate) const TEMP_ABBREV: TableDefinition<&str, &[u8]> = TableDefinition::new("temp_abbrev");
/// 自动造词的草稿层：key = `"{schema}\0{code}\0{text}"`，value = {created_at} 定长 8B。
///
/// 滑窗切出的候选词先落这里，用户用它上屏过一次才跃迁进 [`TEMP_WORDS`]
/// （设计见 `docs/design/auto-phrase-draft-layer.md`）。value 不存 weight：草稿恒沉底，
/// weight 不参与比较。
///
/// 刻意没有伴随的简拼索引：草稿写入量比临时词高一两个数量级，配索引会放大写入。代价是只支持
/// 精确码召回，跃迁进临时词库后自然有简拼。
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建。
pub(crate) const DRAFT_WORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("draft_words");
/// 联想历史：key = "{schema}\0{上文}\0{选中的整词}"，value 同 [`FREQ`]（count + last_used）。
/// 用户在某上文后选了哪条联想，见 [`crate::assoc_history`]。
pub(crate) const ASSOC_HIST: TableDefinition<&str, &[u8]> = TableDefinition::new("assoc_history");
/// 用户词频：key = "{schema}\0{code}\0{text}"，value = {count,last_used}（见 frequency.md）
pub(crate) const FREQ: TableDefinition<&str, &[u8]> = TableDefinition::new("freq");
/// Shadow 规则：key = "{schema}\0{code}"
pub(crate) const SHADOW: TableDefinition<&str, &[u8]> = TableDefinition::new("shadow");
/// 快捷输入格式表的用户调整：key = 格式类别（`date` / `number` …），value = 记录 JSON。
///
/// 键只到类别，不带方案与输入码——与 [`SHADOW`] 的分界见 [`crate::quick_format`]
/// 的模块文档（存错了会表现为「调整当时有效、隔天失效」）。
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建。
pub(crate) const QUICK_FORMAT: TableDefinition<&str, &[u8]> = TableDefinition::new("quick_format");
/// 常用字表的用户覆盖：key = 单个字，value = 记录 JSON。
///
/// 键不带方案——「某个字常不常用」是全局字级属性，与 [`SHADOW`]（按方案 + 输入码）
/// 刻意分开。只存用户碰过的字，出厂那 8104 字仍在 `common_chars.txt` 里；为什么不整表
/// 进库见 [`crate::common_chars`] 的模块文档。
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建。
pub(crate) const COMMON_CHARS: TableDefinition<&str, &[u8]> = TableDefinition::new("common_chars");
/// 字符类的用户层：key = 类的 key，value = 那个类的 yaml 文本（UTF-8 字节）。
///
/// value 是文本而不是结构化记录：格式只在 `wind_config::charset_def` 一份，本 crate
/// 不解析。为什么用户层进库而不是目录见 [`crate::charsets`] 的模块文档。
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建。
pub(crate) const CHARSET_USER: TableDefinition<&str, &[u8]> = TableDefinition::new("charset_user");
/// 补全候选的用户学习数据：key = `"{kind}\0{text}"`，value = {count,last_used} 定长 12B。
///
/// `kind` 是 [`crate::completion::CompletionKind`] 的闭集（邮箱后缀 / 网址历史）。
/// 键不带方案——「我常用哪个邮箱后缀」「我访问过哪个网址」与五笔拼音无关，同
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
    /// 各方案「屏蔽这个词」集合的缓存（`code = *` 的 delete 行），按算它时的表内容（`Arc` 指针）
    /// 判新旧：表一换（写入 / 同步到外部改动）下次取时重算，按键路径只比一次指针。
    pub(crate) blocked_words: Mutex<std::collections::HashMap<String, crate::shadow::BlockedCache>>,
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
    /// 刻意不存时间戳：`with_db` 是热路径，回收线程按固定节拍采样后热路径只剩一个 `Relaxed`
    /// 的 bool store；精度损失至多一个节拍。
    touched: std::sync::atomic::AtomicBool,
    /// 累计丢弃页缓存的次数。供测试断言与诊断：回收在外部不可观测（RSS 未必降）。
    drops: std::sync::atomic::AtomicU64,
    /// 有一次全表扫描待回收（[`Self::mark_scan_pending`] 置位，回收时清除）。
    scan_pending: std::sync::atomic::AtomicBool,
    /// 叫醒闲置（慢拍）中的回收线程：`bool` = 有标记待它看。[`Self::mark_scan_pending`] 置位并
    /// 通知。单独一个 `Arc`：回收线程只持 `Weak<Store>`，睡着时不能拖住 `Store` 不让释放。
    reclaim_wake: std::sync::Arc<(Mutex<bool>, std::sync::Condvar)>,
    /// 用户词 / 临时词两表的结构代次：增删词、改权重（排序键）的事务 commit 后 +1，
    /// `resume`（备份还原换了整个文件）也 +1。只增不减、进程内有效。
    ///
    /// 供按文本前缀取用户词的内存索引（词语联想，见 `wind-engine` 的 `UserAssocIndex`）
    /// 判「缓存是否过期」（store 的键没有按文本的次序，每次联想扫全表付不起）。
    ///
    /// 只改 count 的写不进这里（见 `words_count_gen`），否则开着联想时几乎每次上屏都触发
    /// 一次后台全表重建。
    ///
    /// 宁多勿少：多 +1 只是多一次后台重建，漏 +1 则联想里永远缺那个词。
    ///
    /// 按方案的见 `words_gen_by_schema`。
    words_gen: std::sync::atomic::AtomicU64,
    /// 临时词 count 变化的代次（commit 后 +1）。临时词的 count 决定联想分档（门槛 2）
    /// 与档内排序，但它随选词频繁变化，消费方应节流地跟（见 `UserAssocIndex`）。
    /// 用户词的 count 不影响联想，不计入。
    words_count_gen: std::sync::atomic::AtomicU64,
    /// 按方案的结构代次：`bump_words_gen(schema)` 同时推进全局 `words_gen` 与这里该方案的计数。
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
            blocked_words: Mutex::new(std::collections::HashMap::new()),
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
        // 空闲回收的唯一信号源：`with_db` 是所有读写的唯一入口，漏不掉。
        //
        // 进出各置一次：空闲从操作结束时算起，否则一次很长的 `with_db`（单事务导入、冷扫描）
        // 一结束就会被立刻关库重开。
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

    /// 丢掉 redb 的页缓存：一次锁内丢弃 `Database` 再重开。redb 的页缓存是只增不减的高水位
    /// 结构，这是唯一的回收手法；背景见 `docs/design/memory-footprint.md` §6.1。
    ///
    /// 不拆成 `pause()` + `resume()`：两次取锁之间的 `db = None` 窗口会让按键查询报 paused、吞字。
    ///
    /// 重开是毫秒级且随后走冷缓存，只该用在全表规模的操作之后，绝不能进按键链路。重开失败时
    /// 重试一次；再失败才返回 `Err`，此时 store 确实停在暂停态，调用方须按故障处理（`error!`），
    /// 不能当成「只是内存没回落」。
    ///
    /// 返回 `Ok(true)` = 真丢了一次（计入 [`Self::page_cache_drops`]，含重试成功的那次）；
    /// `Ok(false)` = 本就是暂停态，没有缓存可丢——调用方不该当成一次回收（不记日志、不整理堆）。
    pub fn drop_page_cache(&self) -> anyhow::Result<bool> {
        Ok(self.drop_page_cache_inner(false)? == Dropped::Yes)
    }

    /// `if_idle`：回收线程用。拿到 `db` 锁之后若发现 `touched` 已被置位就作罢（[`Dropped::Busy`]）
    /// ——它是在锁外判定「空闲够久」的，判定之后可能正撞上一次长 `with_db`（单事务导入、冷扫描）：
    /// 那次操作持锁期间回收线程在锁上等，操作一结束就会被关库重开。`with_db` 在放锁之前
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
        // 重开不跑 `init_tables`（`resume` 仍跑）：这是本进程刚关掉的同一个文件，表在
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

    /// 起一条后台线程：空闲够久就把页缓存还回去。光靠批量写完就丢不够，读路径（设置页全表
    /// 扫、打字中的召回）也在填缓存，故空闲时统一回收（见 `docs/design/memory-footprint.md` §6）。
    ///
    /// 两档判据，都要求上次回收之后访问过（`pending`，否则没人用的输入法会每分钟白重开一次）：
    /// - 长档：连续 `idle` 没有任何 `with_db` 就丢一次，兜所有填缓存的路径。
    /// - 短档：调过 [`Self::mark_scan_pending`] 后连续 `scan_idle` 没有 `with_db` 就丢一次并清
    ///   标志。连续翻页/打字时等不满短档，不逐次回收。
    ///
    /// 回收只在这条线程上发生（批量写之后的 `drop_page_cache` 除外），按键线程不会陪等。
    /// 空闲按 `tick` 采样，实际判定至多晚一拍；生产取值见 [`RECLAIM_IDLE`] 等常量。
    /// 线程持 `Weak`：`Store` 被丢弃后它自己退出。
    ///
    /// 间隔不做成配置键（`docs/architecture/config-design-rules.md` §R1），参数化只为测试能用
    /// 毫秒级的值跑完。
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

    /// 后台全表扫描（建索引）做完之后调：走过的行数到 [`SCAN_RECLAIM_MIN_ROWS`] 才
    /// [`Self::mark_scan_pending`]，否则什么都不做。
    ///
    /// 不当场回收：打字中每学一个新词都会触发一次整表重建，当场关库重开会让按键线程陪等、
    /// 且每次从冷缓存起扫。停手满 [`SCAN_RECLAIM_IDLE`] 才还（`docs/design/memory-footprint.md` §6）。
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
#[path = "store_tests.rs"]
mod tests;
