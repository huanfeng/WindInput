# schema 方案配置与 RIME 词库的 TOML 支持

> 状态：设计稿（待审阅定稿后实施）
> 关联：docs/design/config-restructure.md（config 桥接迁移）、wind_input/internal/schema/AGENTS.md、wind_input/internal/dict/AGENTS.md

## 1. 背景与范围

配置文件已完成 YAML→TOML 桥接迁移（da32d4ea，仅 config/state/compat/schema_overrides 四类用户配置）。本次把 TOML 支持延伸到两类**内容文件**：

1. **schema 方案配置**（`data/schemas/*.schema.yaml`）——基本由手工/内置维护，**只需读取支持**（双读：`.schema.toml` 优先、`.schema.yaml` 回退）。
2. **RIME 词库**（`*.dict.yaml`）——头部是 YAML 配置、其后是海量 TSV 数据体，TOML 无法干净表达"头配置 + 体追加"的混合格式。决议：**保留 `.dict.yaml` 作为 rime 生态导入兼容格式**，新增**原生 split 格式 `.dict.toml` + `.dict.tsv`**（头进 TOML、体进制表符分隔文件），本轮交付**读取支持 + 一个 yaml→split 转换工具**。

### 关键决策：中间层 = 类型化 struct（不走"内部 YAML"桥接）

config 的桥接式编解码（`toml→map→yaml.Marshal→yaml.Unmarshal→struct`）让 YAML 成为privileged 内部表示。本次**不沿用桥接**，改用更诚实的中间层模型：

> **两种磁盘格式各自原生解码进同一个类型化 struct，谁都不经过对方。**
>
> - `.toml` → `toml.Unmarshal(data, &s)`（go-toml/v2，`toml:` tag）
> - `.yaml` → `yaml.Unmarshal(data, &s)`（yaml.v3，`yaml:` tag）
>
> 中间层 = 这个 struct 本身。go-toml/v2 与 yaml.v3 都满足"解码进已填充 struct 只覆盖文档中出现的键、不清零缺失字段"，**三层合并 / 用户覆盖的部分覆盖语义对两种解码器原生成立**，无需任何桥接。

代价：schema 结构树（约 20 个 struct）要补一套与 yaml tag 同名的 `toml:` tag——go-toml/v2 在 v2 移除了自定义命名钩子，snake_case 键必须显式 toml tag，无法自动从 CamelCase 字段名映射。这是一次性机械成本。

**config codec 本次不动**：config 是会大改结构、删除旧字段、并计划从 struct 切换到 map 驱动（含版本化迁移，见 config-restructure.md §4.2 在 map 层做迁移）的独立演进线，不适合套用 struct 中间层；其桥接保持现状，待配置重构时在 map 层统一。

## 2. 子项目 A：schema 方案配置 TOML 只读

### 2.1 目标

`data/schemas/` 与用户 schema 目录同时支持 `.schema.toml` 与 `.schema.yaml`，**同 stem 两者并存时 toml 优先、yaml 回退**。只读，不写出、不迁移、不改名旧文件。

### 2.2 改动面（`wind_input/internal/schema/`）

| 文件 | 改动 |
|---|---|
| `schema.go` / `types.go` | 给 Schema 全树（SchemaInfo/EngineSpec/MixedSpec/CodeTableSpec/PinyinSpec/.../DictSpec/LearningSpec/EncoderSpec/ChaiziSpec/WeightSpec 等）每个 `yaml:"k"` 补对应 `toml:"k"`（含 `,omitempty`；`length_in_range` 的 yaml `,flow` 在 toml 侧仅写键名，flow 是 yaml 专有）。指针字段（`*bool` 等）go-toml/v2 原生支持。 |
| `loader.go` | ① 新增格式分派：`LoadSchemaFile` 与 `loadAndMergeUserSchemas` 内按扩展名选 `toml.Unmarshal` / `yaml.Unmarshal`（含 peek schema id 的小结构）。② `schemaFileSuffix` 单常量改为支持两种后缀的发现逻辑；`loadSchemasFromDir`/`loadAndMergeUserSchemas` 扫描时按 **stem 去重、toml 优先**。③ `deepCopySchema` 维持 yaml 往返（纯内存克隆，与磁盘格式无关，可不动）；`mergeDictsByID` 完全不变。 |

### 2.3 发现与优先级算法

目录扫描收集 `*.schema.toml` 与 `*.schema.yaml`，按文件 stem（去 `.schema.toml`/`.schema.yaml`）分组：同 stem 存在 toml 则用 toml、否则 yaml。内置层与用户层各自独立做此优先级；跨层仍是"用户层按 schema.id 覆盖内置层"的既有语义（用户 `.schema.toml` 可覆盖内置 `.schema.yaml`，因为覆盖发生在解码后的 struct 上，与来源格式无关）。

### 2.4 不做

- 不写出 `.schema.toml`（内容文件由人维护）；
- 内置 `data/schemas/*.schema.yaml` 本轮**不转 toml**（保持现状即可被读；是否转 toml 是独立的内容文件决策，留待后续）。

## 3. 子项目 B：RIME 词库 split 格式（`.dict.toml` + `.dict.tsv`）

### 3.1 格式定义

**现状 rime `.dict.yaml`**（YAML 头 + TSV 体，单文件）：
```yaml
---
name: base
version: "2024-05-21"
sort: by_weight
columns: [text, code, weight]   # 可选
import_tables: [cn_dicts/8105]  # 可选
...
啊啊	a a	516                    # ← TSV 数据体（bufio 流式解析，仅缓存重建时跑）
```

**新增 split 格式**（命名约定强制配对，同目录同 stem）：

`foo.dict.toml`（标准 TOML，纯头）：
```toml
name = "base"
version = "2024-05-21"
sort = "by_weight"
columns = ["text", "code", "weight"]
import_tables = ["cn_dicts/8105"]
```
`foo.dict.tsv`（制表符分隔数据体，与 rime body 逐字节同构）：
```
啊啊	a a	516
```

**配对规则：仅命名约定**。`foo.dict.toml` ↔ `foo.dict.tsv`（同目录、同 stem，强制同名），无显式字段、无多文件指向（多词库组合走 `import_tables`，与 rime 一致）。

### 3.2 头/体解耦抽象（新增 `internal/dict/dictcache/dictsource.go`）

统一头结构（类型化中间层，双 tag 原生解码）：
```go
type DictHeader struct {
    Name         string   `yaml:"name"          toml:"name"`
    Version      string   `yaml:"version"       toml:"version"`
    Sort         string   `yaml:"sort"          toml:"sort"`
    Columns      []string `yaml:"columns"       toml:"columns"`
    ImportTables []string `yaml:"import_tables" toml:"import_tables"`
}
```

源打开器，把"头来源"与"体来源"解耦为同一表示：
```go
// OpenDictSource 按扩展名打开词库源，返回解析好的头与定位到数据体起点的流。
//   .dict.yaml: 读到 `...` 为止，头块 yaml.Unmarshal 进 DictHeader；
//               body = 同一 bufio.Reader 在 `...` 之后的续流（单次 open、保持流式）。
//   .dict.toml: 整文件 toml.Unmarshal 进 DictHeader；
//               body = 同 stem 的 .dict.tsv 文件流（命名约定）。
func OpenDictSource(path string) (hdr DictHeader, body io.ReadCloser, err error)
```

- yaml 头块缺失（用户直接从 `name:` 起、无 `---`）按现有约定处理：默认开头即 header，`...` 为唯一结束标记。
- 两种格式的 unknown 键都被各自解码器默认忽略（rime 头里的 `use_preset_vocabulary` 等多余字段无害）。
- `OpenDictSource` 替换并统一现有三处分散的头解析：`parseRimeImportTables`（→ `DictHeader.ImportTables`）、`loadRimeCodetableFile` 内联的 `sort:`/`columns:` 行扫描（→ `DictHeader.Sort/Columns`）、pinyin `loadRimeFile` 的"跳过到 `...`"。

### 3.3 体解析复用（零行为差异）

把 `loadRimeCodetableFile` / `loadRimeFile` 拆成两段：
- **头解析**：改由 `OpenDictSource` 完成；
- **体解析**：原 `bufio.Scanner` + `strings.Split(line,"\t")` + columns 索引逻辑**原样保留**，改为消费 `OpenDictSource` 返回的 `body io.Reader`。

两种磁盘格式喂同一个体解析器，TSV 解析逻辑单一来源，yaml 路径行为不变（回归测试现有词库的 wdb/wdat 产物字节级一致）。

### 3.4 后缀 / import / 源清单后缀感知

现遍布的 `.dict.yaml` 硬编码（`name+".dict.yaml"`、`discoverRime{Codetable,Pinyin}Imports`、`discoverRimePinyinFiles`、`RimeCodetableSourcePaths`/`RimePinyinSourcePaths`、`FindPatchFiles`）改为**格式感知**：

- **import_tables 解析**：import 名解析为兄弟词库描述符，**扩展名跟随主词库格式**——toml 主词库的 import 指向 `name+".dict.toml"`（各自配 `.dict.tsv`）；yaml 主词库维持 `name+".dict.yaml"`。
- **缓存失效源清单**（`Sources`，用于 `NeedsRegenerateBySources`/`SourceListChanged`）：split 格式同时纳入 `.dict.toml` 与 `.dict.tsv`，任一变更触发重建。
- **patch 文件**：`FindPatchFiles` 的命名前缀扫描按主词库格式选后缀。

### 3.5 类型路由不变

`DictSpec.Type` 仍为 `rime_codetable` / `rime_pinyin`；`factory.go` 中 `ConvertRimeCodetableToWdb(srcPath,…)` / `ConvertPinyinToWdat(srcPath,…)` 收到的 `srcPath` 指向 `.dict.toml` 时，经 `OpenDictSource` 走 split 分支。schema 里把词库 `path` 写成 `.dict.toml` 即启用 split，无需新增 DictType。

### 3.6 转换工具（yaml → split）

新增导出函数 + 薄 CLI（`cmd/dicttool` 或复用既有工具入口）：
```go
// ConvertRimeYAMLToSplit 把 rime .dict.yaml 拆成同 stem 的 .dict.toml + .dict.tsv。
// 在 `...` 处切一刀：头部 YAML→DictHeader→toml.Marshal 写出 .dict.toml；
// `...` 之后的字节流原样写入 .dict.tsv（无损、无需逐行重写/转义）。
func ConvertRimeYAMLToSplit(yamlPath, outDir string) error
```
tab 方案下转换是"无损切一刀"：体与 rime body 逐字节同构，避免真 CSV 的逐行加引号转义。

## 4. 影响面与文件清单

### 4.1 Go 侧

- `internal/schema/schema.go`、`types.go`：补 toml tag（机械）。
- `internal/schema/loader.go`：格式分派 + toml-优先发现。
- `internal/dict/dictcache/dictsource.go`（新）：`DictHeader` + `OpenDictSource`。
- `internal/dict/dictcache/convert.go`：体解析改消费 `io.Reader`；import/源清单/patch 后缀感知；新增 `ConvertRimeYAMLToSplit`。
- `cmd/dicttool`（新，或复用）：转换 CLI。
- 各 AGENTS.md（schema、dict/dictcache）按 CLAUDE.md 规约同步更新对外接口/文件结构变化。

### 4.2 不涉及

- `pkg/config` 桥接 codec（不动，见 §1）；
- 前端 wind_setting（schema/dict 是内容文件，不经设置页 schema 驱动表单）；
- 已有 wdb/wdat 二进制缓存格式（不变，仅来源解析变）。

## 5. 构建顺序

| 切片 | 内容 | 验收 |
|---|---|---|
| A | schema 双读：补 toml tag + loader 格式分派 + toml-优先发现 | schema 测试：同一方案 toml/yaml 加载结果等价；同 stem toml 优先；用户 toml 覆盖内置 yaml 合并正确 |
| B1 | `dictsource.go`：`DictHeader` + `OpenDictSource`；体解析重构为消费 `io.Reader`（yaml 路径行为不变） | 现有 rime 词库回归：wdb/wdat 产物与改造前字节级一致 |
| B2 | split 读取接入 codetable + pinyin 两条路径；import/源清单/patch 后缀感知 | 转换后 split 词库读取产出与 yaml 直读字节级一致；多文件 import 解析正确；源清单含 .tsv 触发重建 |
| B3 | `ConvertRimeYAMLToSplit` + CLI | round-trip：yaml 读 == 转 split 后读；含 columns/sort/import_tables 的头无损 |

切片 A 与 B 相互独立，可并行；B1→B2→B3 顺序依赖。

## 6. 测试要点

1. **schema 等价**：构造覆盖 Schema 全字段的样本，`.schema.toml` 与 `.schema.yaml` 加载得到的 struct 深度相等；指针三态字段（`*bool` 等）在 toml 下语义正确（缺键=nil）。
2. **schema 优先级**：同 stem toml+yaml 并存→用 toml；仅 yaml→用 yaml；用户 `.schema.toml` 部分覆盖内置 `.schema.yaml`（缺失字段保留内置值，dictionaries 按 id 合并）。
3. **词库强等价**：同一 rime 词库，yaml 直读 与 转 split 后读，产出的 wdb/wdat **字节级一致**（覆盖 codetable by_weight/original、pinyin、含 import_tables 的多文件主词库）。
4. **import_tables**：split 主词库的多文件 import 正确解析为 `.dict.toml`+`.dict.tsv` 兄弟；源清单完整、变更触发重建。
5. **转换工具**：转换→读取 round-trip 等价；header 含 columns/sort/version/import_tables 无损往返；体逐字节相等。
6. **健壮性**：split 缺失配对 `.dict.tsv`、yaml 头无 `...`、toml 头语法错误等异常路径不 panic，按现有"加载失败跳过/损坏"语义降级。
