<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-05-17 -->

# internal/store

## Purpose
基于 bbolt（etcd 的嵌入式 KV 数据库）的持久化存储层。管理用户词条、临时词、Shadow 规则、词频等数据，按 schema（方案）隔离存储。提供原子事务、bucket 管理等高级功能。

## Key Files
| File | Description |
|------|-------------|
| `store.go` | `Store`：bbolt 数据库包装；`Open()`/`Close()` 生命周期；bucket 初始化（Meta、Schemas、Phrases）；`schemaBucket`/`schemaSubBucket` 导航辅助函数；`ClearSchema`/`DeleteSchema`/`ClearAllSchemas` 数据清理；Meta 键值（版本、设备 ID）管理 |
| `user_words.go` | `UserDict`：用户造词存储，按 schema 隔离；`AddUserWord` 单条原子写入；`BatchAddUserWords` 单事务批量写入（批量导入必用，避免逐条 fsync 超时）；`RemoveUserWord`/`UpdateUserWordWeight`；`SearchUserWordsPrefix`/`CountUserWords` 分页查询；权重排序 |
| `temp_words.go` | `TempDict`：临时词存储（加词过程中的暂存），生命周期短；独立 bucket |
| `phrases.go` | `PhraseStorage`：短语管理存储；`Put`/`Get`/`List`/`Remove`/`ResetDefaults`。**2026-05-16 schema 简化**: `PhraseRecord` 字段精简为 `(Code, Text, Weight, Position, Enabled, IsSystem)`, 删除原 `Texts`/`Name`/`Type` 派生字段, **text 是分类的唯一信任源** (`$AA(...)` 字符组 / `$SS(...)` 字符串数组 / `$CC(...)` 命令 / 普通); 包内 `legacyPhraseRecord` 仅供 migration 反序列化旧数据用; `phraseKey` 统一为 `code\x00text`; `RemovePhrase(code, text)` / `SetPhraseEnabled(code, text, enabled)` 删除 name 参数 |
| `migration.go` | `MigratePhraseRecordsToAA()`: 一次性扫描 Phrases bucket, 把旧的 `Texts`+`Name` 字符组记录重写为 `Text` 字段中的 `$AA(...)` marker, 幂等 (`$AA(` 开头跳过); 用内部 `legacyPhraseRecord` 读旧字段, 写新 `PhraseRecord`。`dict.DictManager.OpenStore` 后立即调用 |
| `shadow.go` | `ShadowStorage`：Shadow 规则（pin/delete）存储。**方案桶设计**: pin / delete 都按方案隔离, 写 `Schemas/{schemaID}/Shadow` 子桶。`DeleteShadow(schemaID, code, word, candID)` 写方案桶; `RemoveShadowRule` 清方案桶; `GetShadowRules` / `GetAllShadowRules` 读方案桶。**短语候选 delete 不走 Shadow**, 改写 `PhraseRecord.Enabled = false` (见 `dict.DictManager.DisablePhrase`), 因此 Shadow delete 自然按方案隔离即可。**R2 (2026-05-17)**: `ShadowPin`/`ShadowDelete` 含 `CandID` 字段, `Deleted` 从 `[]string` 升级为 `[]ShadowDelete{Word, CandID}`; `shadowMatchPin`/`shadowMatchDel` 优先 CandID 匹配; ShadowDelete.UnmarshalJSON 兼容旧版纯字符串格式。**注释历史**: 2026-05-17 一度引入 ShadowGlobal 全局桶承载"跨方案 delete", 后撤销 — 详见 shadow.go 顶部注释 |
| `freq.go` | `FreqStorage`：词频统计存储；`Update`/`Get`/`GetTop`/`Delete` |
| `write_buffer.go` | `WriteBuffer`：构建模式的原子事务写入缓冲，用于批量操作；`Put`/`Delete`/`Commit` |
| `write_buffer_test.go` | WriteBuffer 单元测试 |
| `freq_test.go`/`phrases_test.go`/`shadow_test.go`/`user_words_test.go` | 各模块单元测试 |

## For AI Agents

### Working In This Directory
- **Bucket 结构**：
  - 顶层桶: `Meta`（全局 kv）/ `Schemas` / `Phrases`
  - `Schemas` → `{schemaID}` (各方案数据) → `UserWords` / `TempWords` / `Shadow` / `Freq` (子桶)
  - **Shadow 按方案桶**: pin 和 delete 都写 `Schemas/{schemaID}/Shadow`。短语候选的"删除"已改走 `PhraseRecord.Enabled = false` (跨方案的"禁用"语义), Shadow 不再需要全局桶。详见 `shadow.go` 顶部注释
- **初始化**：`Store.init()` 创建必要 bucket 并初始化 Meta 默认值（版本=1、设备 ID=UUID）
- **事务语义**：所有写操作通过 `db.Update()`、读操作通过 `db.View()` 保证原子性
- **schema 隔离**：不同方案的词典、频率、规则独立存储在各自的 bucket 下，切换方案时通过 `schemaBucket(schemaID, create=true)` 导航
- **WriteBuffer**：批量 Put/Delete 操作时先缓冲，最后 `Commit()` 一次性写入 bbolt，减少事务次数
- **清理操作**：
  - `ClearSchema(schemaID)` 删除后重建空 bucket（保持结构）
  - `DeleteSchema(schemaID)` 完全删除 bucket（无重建）
  - `ClearAllSchemas()` 删除所有 schema 数据，保留 Meta

### Testing Requirements
- 运行：`go test ./internal/store`
- 各子模块有对应测试文件（`*_test.go`）
- 可在临时数据库中执行测试，避免污染生产数据

### Common Patterns
- 错误返回：`fmt.Errorf` 包装底层 bbolt 错误信息
- 数据版本：`Meta["version"]` 用于兼容性检查和迁移
- 设备 ID：`Meta["device_id"]` 用于多设备同步和去重

## Dependencies
### Internal
- 无（lowest-level storage layer）

### External
- `go.etcd.io/bbolt` — 嵌入式 KV 数据库
- `github.com/google/uuid` — 设备 ID 生成
- `gopkg.in/yaml.v3` — Shadow、Phrase YAML 序列化

<!-- MANUAL: -->
