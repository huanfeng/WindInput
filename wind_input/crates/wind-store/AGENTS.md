<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-06-29 -->

# wind-store

## Purpose

基于 redb 的用户数据持久化层。管理按方案隔离的用户词、临时词、词频与 Shadow 规则，以及全局用户短语；向上为 `wind-dict` 的 `StoreUserLayer`/`StoreTempLayer` 提供查询后端，是系统中唯一没有内部依赖的底层 crate。

## Key Files

| File | Description |
|------|-------------|
| `src/store.rs` | `Store` 核心：redb `Database` 包装；7 张扁平 table 定义；`with_db` 闭包是所有读写的唯一安全入口；`pause`/`resume` 释放并重开文件锁（Windows 热替换）；版本迁移框架 |
| `src/user_words.rs` | 用户词 CRUD；key=`schema\0code\0text`；value 定长 16B（weight i32 + count u32 + created_at i64）；`add_user_word` 重复取 max 权重；`on_word_selected` 阈值累加调权 |
| `src/temp_words.rs` | 临时学习词，结构与编码同 `user_words`，独立 `TEMP_WORDS` table |
| `src/phrases.rs` | 用户短语（**全局，不分方案**）；key=`code\0text`；`set_phrase_enabled` 软删除（记录保留，`enabled=false`） |
| `src/shadow.rs` | `ShadowRecord`（pinned/deleted）规则；内存 `ShadowStore`（JSON 文件，coordinator 过渡用）+ redb `Store` 扩展方法；`cand_id` 精准去重（动态短语 R2） |
| `src/freq.rs` | `FreqRecord`（count u32 + last_used i64，12B 定长）；`FreqProfile` 拼音衰减分；legacy `FreqTracker`（过渡期文件式，待移除） |
| `src/migration.rs` | 版本迁移框架；当前 `CURRENT_VERSION=1`，全新库直接打版本号 |
| `src/stat_collector.rs` / `src/stats.rs` | 每日打字统计，写 `STATS_DAILY` table |

## For AI Agents

### Working In This Directory

- **所有读写必须经 `Store::with_db`**：不要直接访问 `Store.db` 字段。`with_db` 持 `Mutex` 锁并检查暂停态——`pause()` 后 `with_db` 返回 `"store is paused"` 错误，绕过它会破坏 pause/resume 语义或导致 panic。

- **redb 的读缓存是只涨不落的高水位，全额计进进程私有内存**：redb 2.x 不是 mmap，页是堆上的 `Arc<[u8]>`；每次读未命中就插入，只有越过上限才淘汰，唯一的整体清空是库文件扩容，刷盘时脏页还会被直接晋升进读缓存。上界 ≈ `min(读缓存配额, 库文件大小)`——实测 19 万词的库全表扫后常驻 41 MB 不还，50 万词约 90 MB，这是「导入大词库后内存高」的正主（`tests/redb_cache_high_water.rs`）。做完一次**全表规模**的操作（导入、清空、全量导出）后调 `Store::drop_page_cache` 把它还回去；**绝不能进按键链路**，它要重开 `Database`。
- **要条数就用 `count_*`，要一页就用 `for_each_*`，别 `list(..).len()` 或先全收再 `skip/take`**：19 万条的库翻第一页会先造 38 万个 `String`。`for_each_user_word` 的回调收 `UserWordView`（`code`/`text` 直接借 redb 的页），只对真正要留的那几条调 `to_record()`。⚠️ 新增计数/遍历 API 时，**口径必须与对应的 `search_*` 逐条一致**（含「解不出的坏记录算不算」），否则症状是「说有 N 条、翻到最后一页只有 N-1 条」，而且只在库里真有坏记录时才出现——守门在 `tests/counts_match_listings.rs`，它不比具体数字，只比两条路彼此相等。
- **这两件事是分开的**：流式遍历省的是 Rust 侧的物化峰值，**不省 redb 读缓存**（range 迭代照样把走过的叶子页读进缓存，key 与 value 同页）。只改其中一样就以为内存问题解决了，是这条最容易踩的坑。
- **复合 key 编码是范围查询的基础，不可改**：用户词/临时词/词频统一用 `schema\0code\0text` 三段；Shadow 用 `schema\0code` 两段；短语用 `code\0text`（全局，无 schema 前缀）。前缀范围查用 `t.range(scan..)` + 前缀 break，依赖此编码结构——改分隔符或字段顺序会静默读到跨方案数据。

- **短语软删除走 `set_phrase_enabled`，不走 Shadow delete**：短语候选的屏蔽语义是"用户在设置 UI 可恢复的禁用"，存 `PhraseRecord.enabled=false`；`Shadow.delete` 只用于系统词库/用户词候选。两条路径不混用，否则设置 UI 的启用开关无法正确反映状态（见 `shadow.rs` 顶部注释）。

- **Shadow 有两套实现，新代码用 redb 路径**：`ShadowStore`（内存+JSON 文件）是 coordinator 过渡期残留；`Store::pin_shadow` / `delete_shadow` / `remove_shadow_rule` 是 redb 正式实现。两者共用 `ShadowRecord.apply_*` 方法保持逻辑一致，但持久化后端不同。

- **`FreqTracker` 是 legacy，不要在新代码中引用**：`freq.rs` 内注释已标明"coordinator 接通 redb 词频后移除"。新的频率记录走 `Store::record_freq` / `get_freq`（redb，按方案隔离）。

### Testing Requirements

- `wind-store` 无 `windows` crate 依赖，可在任意 host 直接运行：`cargo test -p wind-store`。
- 各测试在 `std::env::temp_dir()` 创建带唯一名的 `.redb` 临时文件，测后删除；并发运行时文件名已区分，不会互相干扰。

## Dependencies

### Internal

无（系统中最底层的存储 crate，不依赖任何本仓其他 crate）。

### External

- `redb` — 嵌入式 KV 数据库（替代 Go 版的 bbolt）
- `serde` / `serde_json` — Shadow 规则与短语的 JSON 序列化
- `uuid` — 设备 ID 生成（Meta 表）
- `tokio` — 异步运行时（stat 相关异步路径）
- `chrono` — 日期处理（每日统计 key）
- `anyhow` / `thiserror` / `tracing` — 错误处理与日志

## 全局约束

提交前跑 `cargo fmt`；日志 INFO 级不得含用户输入/词库内容（见根 `AGENTS.md`）。

<!-- MANUAL: 此行以下为人工补充区，重新生成时保留 -->
