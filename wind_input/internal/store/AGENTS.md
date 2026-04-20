<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# internal/store

## Purpose
基于 bbolt（etcd 的嵌入式 KV 数据库）的持久化存储层。管理用户词条、临时词、Shadow 规则、词频等数据，按 schema（方案）隔离存储。提供原子事务、bucket 管理等高级功能。

## Key Files
| File | Description |
|------|-------------|
| `store.go` | `Store`：bbolt 数据库包装；`Open()`/`Close()` 生命周期；bucket 初始化（Meta、Schemas、Phrases）；`schemaBucket`/`schemaSubBucket` 导航辅助函数；`ClearSchema`/`DeleteSchema`/`ClearAllSchemas` 数据清理；Meta 键值（版本、设备 ID）管理 |
| `user_words.go` | `UserDict`：用户造词存储，按 schema 隔离；`PutWord`/`GetWord`/`DeleteWord` 原子操作；`ListWords` 分页查询；权重排序 |
| `temp_words.go` | `TempDict`：临时词存储（加词过程中的暂存），生命周期短；独立 bucket |
| `phrases.go` | `PhraseStorage`：短语管理存储；`Put`/`Get`/`List`/`Remove`/`ResetDefaults` |
| `shadow.go` | `ShadowStorage`：Shadow 规则（pin/delete）存储；YAML 序列化/反序列化 |
| `freq.go` | `FreqStorage`：词频统计存储；`Update`/`Get`/`GetTop`/`Delete` |
| `write_buffer.go` | `WriteBuffer`：构建模式的原子事务写入缓冲，用于批量操作；`Put`/`Delete`/`Commit` |
| `write_buffer_test.go` | WriteBuffer 单元测试 |
| `freq_test.go`/`phrases_test.go`/`shadow_test.go`/`user_words_test.go` | 各模块单元测试 |

## For AI Agents

### Working In This Directory
- **Bucket 结构**：Meta（全局 kv）→ Schemas（schema 子 bucket）→ {schemaID}（各 schema 数据）→ UserWords/TempWords/Shadow/Freq（子 bucket）
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
