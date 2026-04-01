<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# pkg/dictfile

## Purpose
词库文件数据类型定义和读写逻辑。定义 `phrases.yaml`、`shadow.yaml`、用户词库文件的数据结构，供服务端（`internal/dict`）和外部工具共用。

## Key Files
| File | Description |
|------|-------------|
| `types.go` | 核心类型：`PhraseEntry`、`PhraseConfig`、`PhrasesConfig`、`ShadowPinConfig`、`ShadowCodeConfig`、`ShadowConfig`、`UserWord`、`UserDictData` |
| `phrase.go` | `PhrasesConfig` 的 YAML 读写函数 |
| `shadow.go` | `ShadowConfig` 的 YAML 读写及操作函数（`PinWord`、`DeleteWord`、`RemoveShadowRule`、`GetRuleCount`） |
| `userdict.go` | 用户词库 TSV 格式读写及操作函数（`LoadUserDictFrom`、`SaveUserDictTo`、`AddUserWord`、`RemoveUserWord`、`UpdateUserWordWeight`、`SearchUserDict`、`GetWordsByCode`、`ImportUserDict`、`ExportUserDict`） |

## For AI Agents

### Working In This Directory
- `PhraseConfig.Type` 值：空字符串（普通短语）或 `"command"`（内置命令）
- `PhraseConfig.Handler` 内置命令名：`date`、`time`、`datetime`、`week`、`uuid`、`timestamp`
- **Shadow 架构（pin+delete）**：
  - `ShadowCodeConfig.Pinned []ShadowPinConfig`：固定位置规则，每条含 `Word` 和 `Position`
  - `ShadowCodeConfig.Deleted []string`：隐藏词列表
  - `ShadowConfig.Rules map[string]*ShadowCodeConfig`：按编码索引的规则集
- `shadow.go` 提供高层操作函数：`PinWord(cfg, code, word, position)`、`DeleteWord(cfg, code, word)`、`RemoveShadowRule(cfg, code, word)`
- `PinWord` 会自动从 Deleted 中移除、`DeleteWord` 会自动从 Pinned 中移除（互斥操作）
- **用户词库格式为 TSV 文本**（非 JSON）：每行 `编码<tab>词语<tab>权重<tab>时间戳`，支持注释行（`#`）和去重（相同 code+text 取较高权重）
- `userdict.go` 提供完整 CRUD：`AddUserWord`（新增/更新）、`RemoveUserWord`（删除）、`UpdateUserWordWeight`（改权重）、`SearchUserDict`（模糊搜索编码或词语）、`GetWordsByCode`（按编码查询）、`ImportUserDict`（合并导入）、`ExportUserDict`（导出）
- 短语模板变量（在 `internal/dict/phrase.go` 中展开）：`{year}`、`{month}`、`{day}`、`{hour}`、`{minute}`、`{second}`、`{week}`

### Testing Requirements
- 纯 Go 逻辑，可做序列化往返单元测试
- Shadow 操作函数的互斥行为（pin/delete 互斥）可做纯函数单元测试
- `userdict.go` 的去重逻辑和 CRUD 操作可做单元测试

### Common Patterns
- 文件格式示例见 `configs/phrases.example.yaml` 和 `configs/shadow.example.yaml`
- 用户词库保存使用 `fileutil.AtomicWrite`，写入前按编码字典序排序
- Shadow 文件通过 `fileutil.AtomicWrite` 保存，保证写入安全

## Dependencies
### Internal
- `pkg/config` — `EnsureConfigDir()`（保存时确保目录存在）
- `pkg/fileutil` — 原子写入（`AtomicWrite`）

### External
- `gopkg.in/yaml.v3` — 短语和 Shadow 文件的 YAML 解析

<!-- MANUAL: -->
