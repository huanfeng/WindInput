<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-23 | Updated: 2026-04-08 -->

# internal/schema

## Purpose
Schema 方案驱动架构的核心包。定义输入方案（`.schema.yaml`）的数据结构、加载、校验、管理和工厂函数。一个 Schema 描述一套完整的输入方案：引擎类型（拼音/码表/混输）、词库路径、用户数据路径、学习策略等。通过 Schema 驱动取代了原来硬编码的引擎初始化逻辑。

## Key Files
| File | Description |
|------|-------------|
| `schema.go` | 核心类型定义：`Schema`、`SchemaInfo`、`EngineSpec`（含 `MixedSpec` 混输配置）、`CodeTableSpec`、`PinyinSpec`（含 `ShuangpinSpec`、`FuzzySpec`）、`DictSpec`、`LearningSpec`、`EncoderSpec`/`EncoderRule`（造词编码规则）；辅助方法 `GetDefaultDictSpec`、`GetDictsByRole`；引擎类型常量新增 `EngineTypeMixed = "mixed"` |
| `loader.go` | 方案文件加载与校验：`LoadSchemaFile`、`DiscoverSchemas`；扫描 `exeDir/schemas/` 和 `dataDir/schemas/`，用户目录同 ID 时覆盖内置方案 |
| `manager.go` | `SchemaManager`：加载所有方案、按 ID 查询、活跃方案切换（`SetActive`/`GetActiveSchema`）、列出可用方案 |
| `factory.go` | `CreateEngineFromSchema`：根据方案创建引擎实例（`*wubi.Engine`、`*pinyin.Engine` 或 `*mixed.Engine`），处理词库加载、`CompositeDict` 注册、Unigram 模型、用户词频、反查码表；`SavePinyinUserFreqs` 供退出时保存 |
| `learning.go` | 学习策略接口 `LearningStrategy` 及三种实现：`ManualLearning`（手动/不自动学词）、`AutoLearning`（选词即学，仅多字词）、`FrequencyLearning`（仅调频）；`NewLearningStrategy` 工厂函数 |
| `learning_test.go` | 学习策略单元测试 |
| `schema_test.go` | Schema 加载与校验测试 |

## For AI Agents

### Working In This Directory
- 方案文件命名：`<id>.schema.yaml`，文件中 `schema.id` 必须与文件名前缀一致
- 引擎类型支持三种：`EngineTypePinyin = "pinyin"`、`EngineTypeCodeTable = "codetable"`、`EngineTypeMixed = "mixed"`
- `DiscoverSchemas` 优先级：`dataDir/schemas/` > `exeDir/schemas/`（同 ID 时用户目录覆盖内置）
- `validateSchema` 会自动补全默认值：`schema.name`（空时取 ID）、`schema.icon_label`（取 name 首字符）、`learning.mode`（拼音默认 `auto`，码表默认 `manual`）
- `factory.go` 中词库加载优先使用预编译 `wdb`（词库目录内），其次缓存目录，最后文本源文件
- 支持 Rime 生态词库类型：`rime_pinyin`、`rime_wubi`（多文件结构，通过 `dictcache.RimeXxxSourcePaths` 发现关联文件）
- `LearningStrategy.OnCandidateCommitted` 目前由 coordinator 调用（非 schema 包内部自调用）
- `EncoderSpec` 定义造词编码规则（五笔/码表方案使用），`engine.Manager.GetEncoderRules` 读取供加词功能计算编码
- 新增引擎类型时需同步修改 `schema.go` 的常量、`loader.go` 的 validate、`factory.go` 的 switch

### Testing Requirements
- `go test ./internal/schema/`
- `schema_test.go` 测试加载/校验，`learning_test.go` 测试学习策略
- factory.go 集成测试需词库文件，可 mock `dict.DictManager`

## Dependencies
### Internal
- `internal/candidate` — Candidate 类型（learning.go）
- `internal/dict` — DictManager、CompositeDict、PinyinDict、CodeTableLayer 等
- `internal/dict/dictcache` — 词库格式转换与缓存
- `internal/engine/mixed` — 混输引擎构造
- `internal/engine/pinyin` — 拼音引擎构造
- `internal/engine/wubi` — 五笔引擎构造

### External
- `gopkg.in/yaml.v3` — 方案文件 YAML 解析

<!-- MANUAL: -->
