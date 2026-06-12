<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-23 | Updated: 2026-06-12 -->

# internal/schema

## Purpose
Schema 方案驱动架构的核心包。定义输入方案（`.schema.yaml` / `.schema.toml`）的数据结构、加载、校验、管理和工厂函数。一个 Schema 描述一套完整的输入方案：引擎类型（拼音/码表/混输）、词库路径、用户数据路径、学习策略等。通过 Schema 驱动取代了原来硬编码的引擎初始化逻辑。

**方案文件格式（只读双格式）**：支持 `.schema.toml` 与 `.schema.yaml` 两种磁盘格式，**同 stem 并存时 toml 优先、yaml 回退**。Schema 结构树全字段带双 tag（`yaml`+`toml`），两种格式各自原生解码进同一结构（中间层 = 类型化 struct，互不经过对方，不走"内部 YAML"桥接）。两种解码器都满足"解码进已填充 struct 只覆盖出现键、不清零缺失字段"，故用户层覆盖内置层的部分覆盖语义一致。仅读取，不写出、不迁移。

## Key Files
| File | Description |
|------|-------------|
| `schema.go` | 核心类型定义：`Schema`、`SchemaInfo`、`EngineSpec`（含 `MixedSpec` 混输配置）、`CodeTableSpec`、`PinyinSpec`（含 `ShuangpinSpec`、`FuzzySpec`）、`DictSpec`、`LearningSpec`、`EncoderSpec`/`EncoderRule`（造词编码规则）；辅助方法 `GetDefaultDictSpec`、`GetDictsByRole`、`DataSchemaID()`（返回用户数据存储桶 ID，拼音方案统一返回 `PinyinSharedDictID="pinyin"` 共享词库）；引擎类型常量 `EngineTypeMixed = "mixed"`、`PinyinSharedDictID = "pinyin"` |
| `loader.go` | 方案文件加载与校验：`LoadSchemaFile`、`DiscoverSchemas`；扫描 `exeDir/schemas/` 和 `dataDir/schemas/`，用户目录同 ID 时覆盖内置方案。`discoverSchemaPaths` 按 stem 去重（`.schema.toml` 优先于 `.schema.yaml`），`unmarshalSchemaData` 按扩展名分派 toml/yaml 原生解码 |
| `manager.go` | `SchemaManager`：加载所有方案、按 ID 查询、活跃方案切换（`SetActive`/`GetActiveSchema`）、列出可用方案 |
| `factory.go` | `CreateEngineFromSchema`：根据方案创建引擎实例（`*wubi.Engine`、`*pinyin.Engine` 或 `*mixed.Engine`），处理词库加载、`CompositeDict` 注册、Unigram 模型、用户词频、反查码表；`SavePinyinUserFreqs` 供退出时保存；异步资源构建状态：`ErrAssetBuilding`（sentinel error，`fmt.Errorf("%w: ...", ErrAssetBuilding)` 包装）、`IsPinyinWdatBuilding()` 查询拼音 wdat 是否后台生成中、`OnPinyinWdatReady(cb)` 注册完成回调（idle 时同步触发，busy 时排队） |
| `asset_state_test.go` | 异步资源状态相关单元测试（sentinel 匹配、回调三种语义） |
| `learning.go` | 学习策略接口 `LearningStrategy` 及三种实现：`ManualLearning`（手动/不自动学词）、`AutoLearning`（选词即学，仅多字词）、`FrequencyLearning`（仅调频）；`NewLearningStrategy` 工厂函数 |
| `learning_test.go` | 学习策略单元测试 |
| `schema_test.go` | Schema 加载与校验测试 |

## For AI Agents

### Working In This Directory
- 方案文件命名：`<id>.schema.yaml` 或 `<id>.schema.toml`，文件中 `schema.id` 必须与文件名前缀一致；同 stem 两格式并存时 toml 优先
- **新增 Schema 字段时双 tag 必须同步**：每个 `yaml:"k"` 都要配一个 `toml:"k"`（go-toml/v2 在 v2 移除了自定义命名钩子，snake_case 键必须显式 toml tag），否则该字段在 `.schema.toml` 中无法读取
- **`loadAndMergeUserSchemas` 中 overlay 前必须 `s.Dicts = nil`**：go-toml/v2 解码 `[[dictionaries]]` 进已填充 struct 时复用切片底层数组就地改写（yaml.v3 分配新切片），不置 nil 会污染 `baseDicts` 捕获的引用
- 引擎类型支持三种：`EngineTypePinyin = "pinyin"`、`EngineTypeCodeTable = "codetable"`、`EngineTypeMixed = "mixed"`
- `DiscoverSchemas` 优先级：`dataDir/schemas/` > `exeDir/schemas/`（同 ID 时用户目录覆盖内置）
- `validateSchema` 会自动补全默认值：`schema.name`（空时取 ID）、`schema.icon_label`（取 name 首字符）、`learning.mode`（拼音默认 `auto`，码表默认 `manual`）
- `factory.go` 中词库加载优先使用预编译 `wdb`（词库目录内），其次缓存目录，最后文本源文件
- 支持 Rime 生态词库类型：`rime_pinyin`、`rime_wubi`（多文件结构，通过 `dictcache.RimeXxxSourcePaths` 发现关联文件）
- `LearningStrategy.OnCandidateCommitted` 目前由 coordinator 调用（非 schema 包内部自调用）
- `EncoderSpec` 定义造词编码规则（五笔/码表方案使用），`engine.Manager.GetEncoderRules` 读取供加词功能计算编码
- `DataSchemaID()` 控制用户词库的 bbolt bucket 路由：拼音方案（`EngineTypePinyin`）统一返回 `"pinyin"`，使全拼和双拼共享同一份用户词库（无编码，仅词+权重）
- 新增引擎类型时需同步修改 `schema.go` 的常量、`loader.go` 的 validate、`factory.go` 的 switch
- **方案配置的运行时修改一律走 L3 (`schema_overrides.toml`)，禁止改写 L1/L2 的 `.schema.yaml` 文件**。`manager.go` 的 L3 叠加用 `mergeDictsByID` 按 id patch `dictionaries`，避免稀疏 diff 整体替换数组。详见 `docs/design/schema-layers.md`

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
- `github.com/pelletier/go-toml/v2` — 方案文件 TOML 解析（`.schema.toml`）

<!-- MANUAL: -->
