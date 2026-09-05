//! Store 核心：基于 redb 的持久化存储（骨架）
//!
//! 与 Go 版本 `wind_input/internal/store/store.go`（bbolt）对齐，但用 redb。
//! 见 docs/redesign/store.md：redb 无嵌套 bucket，用扁平 table + schema 前缀复合 key。
//!
//! 本提交为**骨架**：open / 表定义 / 事务封装 / pause-resume（Windows 热替换释放文件锁）/
//! version + 迁移框架。用户词/临时词/词频/shadow 的具体 ops 在后续提交按 store.md §10.2 实现。

use redb::{Database, TableDefinition};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::{info, warn};

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
/// ## 取值：由 `perf_cache_size.rs` 双向标定
///
/// 读路径（简拼召回 / 前缀补全 / 全量列举）对本值**几乎不敏感**：redb 2.x 不是 mmap，
/// 未命中走 `read` 系统调用、由 OS 页缓存供数据，是 µs 级 syscall 而非磁盘寻道——
/// 它下面还垫着一层 OS 页缓存。连 2 MiB 都测不出读性能退化。
///
/// **写路径敏感**：`set_cache_size` 把 10% 划给写缓存，19 万词批量导入在写缓存放得下
/// 全部脏页时一次刷盘，放不下就反复外溢。实测断崖出现在 64↔256 MiB 之间。
/// 首版取 32 MiB 只测了读就下了结论，导入因此慢 4.6 倍——**只测一半的结论等于没测**。
pub const DEFAULT_CACHE_SIZE_BYTES: usize = 256 * 1024 * 1024;

/// 按统一配置打开 redb（`open` 与 `resume` 共用，避免两处默认值漂移）。
fn open_db(path: &Path, cache_bytes: usize) -> Result<Database, redb::DatabaseError> {
    Database::builder().set_cache_size(cache_bytes).create(path)
}

// ── 表定义（key 编码见 store.md §2：复合 key 带 schema 前缀，redb 扁平）──
/// 用户词：key = "{schema}\0{code}\0{text}"，value = 序列化记录
pub(crate) const USER_WORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("user_words");
/// 临时词：同上
pub(crate) const TEMP_WORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("temp_words");
/// 用户词**简拼索引**：key = `"{schema}\0{group}\0{code}\0{text}"`，value 空。
/// 键的构成、分组规则与维护契约全在 [`crate::abbrev_index`]。
///
/// 新表无需迁移：`init_tables` 在写事务里 `open_table` 即创建；存量数据的索引由
/// [`Store::rebuild_abbrev_indexes`] 在首次发现索引为空时补建。
pub(crate) const USER_ABBREV: TableDefinition<&str, &[u8]> = TableDefinition::new("user_abbrev");
/// 临时词**简拼索引**：与 [`USER_ABBREV`] 同构，索引 `TEMP_WORDS`。
///
/// 两层都要索引，因为简拼召回是**跨层**的：引擎侧挂的 `DictManager` 同时注册了
/// `StoreUserLayer` 与 `StoreTempLayer`，只索引其一等于只修一半——自动造词开着时，
/// 临时词库（默认上限 5000 条）仍会被逐切点全量枚举。
pub(crate) const TEMP_ABBREV: TableDefinition<&str, &[u8]> = TableDefinition::new("temp_abbrev");
/// 用户词频：key = "{schema}\0{code}\0{text}"，value = {count,last_used}（见 frequency.md）
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
/// 全局短语：key = "{code}\0{text}"
pub(crate) const PHRASES: TableDefinition<&str, &[u8]> = TableDefinition::new("phrases");
/// 每日统计：key = "YYYY-MM-DD"
pub(crate) const STATS_DAILY: TableDefinition<&str, &[u8]> = TableDefinition::new("stats_daily");
/// 元数据：version / device_id 等
pub(crate) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const META_VERSION_KEY: &str = "schema_version";

/// 存储引擎（redb）。`db` 为 None 表示已暂停（pause，释放文件锁供热替换）。
pub struct Store {
    path: PathBuf,
    db: Mutex<Option<Database>>,
    /// 本库的页缓存上限；`resume` 重开时要用同一个值，故随实例存着。
    cache_bytes: usize,
}

impl Store {
    /// 打开数据库：创建/打开 redb，建表，运行版本迁移。
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::open_with_cache_size(path, DEFAULT_CACHE_SIZE_BYTES)
    }

    /// 指定页缓存上限打开。供 `perf_cache_size.rs` 标定用；生产走 [`Self::open`]。
    pub fn open_with_cache_size(
        path: impl AsRef<Path>,
        cache_bytes: usize,
    ) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let db = open_db(&path, cache_bytes)?;
        Self::init_tables(&db)?;
        let store = Self {
            path,
            db: Mutex::new(Some(db)),
            cache_bytes,
        };
        store.run_migrations()?;
        store.backfill_abbrev_indexes();
        info!(
            "Store opened: {} (v{})",
            store.path.display(),
            store.version().unwrap_or(0)
        );
        Ok(store)
    }

    /// 存量库补建简拼索引：升级到带索引的版本时，老库里的词一条索引都没有，
    /// 不补建则简拼**静默召不回**（比慢更糟）。
    ///
    /// 放在 `open` 里而不是交给调用方判断，是因为 `Store::open` 有多个入口（协调器、
    /// 设置页、备份还原、各类测试）——留给调用方就等于留一个「忘了调」的口子，
    /// 而这类遗漏的表现是静默失效。
    ///
    /// 常态开销 O(1)：索引条目数与主表条目数恒等（每条词正好一条索引），
    /// 故只有「索引空而主表非空」时才会真正扫一遍。失败只记日志不阻断启动——
    /// 简拼召不回是退化，打不开词库是故障，不该用后者换前者。
    fn backfill_abbrev_indexes(&self) {
        let need = self
            .with_db(|db| {
                use redb::ReadableTableMetadata;
                let txn = db.begin_read()?;
                let idx =
                    txn.open_table(USER_ABBREV)?.len()? + txn.open_table(TEMP_ABBREV)?.len()?;
                let main =
                    txn.open_table(USER_WORDS)?.len()? + txn.open_table(TEMP_WORDS)?.len()?;
                Ok(idx == 0 && main > 0)
            })
            .unwrap_or(false);
        if !need {
            return;
        }
        match self.rebuild_abbrev_indexes() {
            Ok(n) => info!("简拼索引补建完成：{} 条", n),
            Err(e) => warn!("简拼索引补建失败（简拼召回将失效）：{}", e),
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
            w.open_table(FREQ)?;
            w.open_table(SHADOW)?;
            w.open_table(QUICK_FORMAT)?;
            w.open_table(COMMON_CHARS)?;
            w.open_table(CHARSET_USER)?;
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
        let guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(db) => f(db),
            None => anyhow::bail!("store is paused"),
        }
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

    /// 写 META 表的字符串值。
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
            info!("Store resumed: {}", self.path.display());
        }
        Ok(())
    }

    /// 是否处于暂停态
    pub fn is_paused(&self) -> bool {
        self.db.lock().unwrap_or_else(|e| e.into_inner()).is_none()
    }

    /// 数据库路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 枚举四张按 schema 前缀编码的表（user/temp/freq/shadow）里出现过的全部 schema id。
    /// 备份用：确保有数据但未在当前配置启用的方案也被覆盖。
    pub fn list_data_schemas(&self) -> anyhow::Result<Vec<String>> {
        let mut set = std::collections::BTreeSet::new();
        self.with_db(|db| {
            let txn = db.begin_read()?;
            for table in [USER_WORDS, TEMP_WORDS, FREQ, SHADOW] {
                let t = txn.open_table(table)?;
                for item in t.range::<&str>(..)? {
                    let (k, _) = item?;
                    if let Some((schema, _rest)) = k.value().split_once('\u{0}') {
                        set.insert(schema.to_string());
                    }
                }
            }
            Ok(())
        })?;
        Ok(set.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
