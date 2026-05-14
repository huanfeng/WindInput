<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# internal/engine/wubi

## Purpose
五笔输入引擎实现。基于码表（`dict.CodeTable`）实现按键到候选词的查找，支持：精确匹配、前缀匹配（逐码提示）、四码自动上屏、五码顶字上屏、标点顶字、空码处理。词库已迁移至 Rime 生态（多文件 `.dict.yaml` 格式），词频功能（`EnableUserFreq`）通过 `CompositeDict` 的 `UserDict` 层实现。

## Key Files
| File | Description |
|------|-------------|
| `wubi.go` | `Engine` 结构体、`Config`（含 `EnableUserFreq`/`ProtectTopN`/`CandidateSortMode`）、`DefaultConfig`、码表加载（`LoadCodeTableBinary`/`LoadCodeTable`）、`Convert`/`ConvertEx`/`HandleTopCode`/`OnCandidateSelected` |
| `wubi_test.go` | 引擎功能测试（精确匹配、前缀、顶码、空码） |
| `wubi_freq_test.go` | 词频功能测试（`EnableUserFreq=true` 时的排序变化验证） |

## For AI Agents

### Working In This Directory
- `Config` 字段说明：
  - `AutoCommitAt4`：四码且唯一候选时自动上屏
  - `ClearOnEmptyAt4`：四码无候选时清空输入
  - `TopCodeCommit`：第五码输入时顶掉第一候选上屏（顶码）
  - `PunctCommit`：标点符号触发顶码上屏
  - `SingleCodeInput`：关闭前缀匹配，只做精确匹配（逐字键入模式）
  - `ShowCodeHint`：候选词显示四码编码提示
  - `ProtectTopN`：首选保护，前 N 位锁定码表原始顺序不受词频影响
  - `CandidateSortMode`：候选排序模式，与 `CompositeDict.SetSortMode` 同步
  - `SkipShadow`：跳过引擎内部的 Shadow 应用，由外层（MixedEngine）统一处理
- `HandleTopCode(input string)` 处理五码输入：截取前四码查找候选，剩余一码作为新输入
- `ConvertEx` 返回 `*ConvertResult`，包含 `ShouldCommit`、`CommitText`、`IsEmpty`、`ShouldClear`、`ToEnglish`
- 码表通过 `schema/factory.go` 加载（Rime `.dict.yaml` 多文件合并或传统单文件），引擎直接持有 `*dict.CodeTable`
- `OnCandidateSelected` 调用 `FreqHandler` 调频，并通过 `LearningStrategy` 触发造词
- `RestoreCodeTableHeader` 供 factory 从 sidecar meta.json 恢复码表元数据

#### ⚠️ 顶码上屏（AutoCommitAt4）与 Shadow 的交互
`ConvertEx` 流水线中 Shadow 是**呈现层**过滤（Phase 6），`checkAutoCommit` 在其后执行。
修改顶码或 Shadow 相关逻辑时必须注意：

- `checkAutoCommit` 使用的是 `filteredExact`（精确匹配候选），**不含前缀候选**，确保唯一性判断不被前缀干扰
- 当 `SkipShadow=false`（纯码表模式）：`filteredExact` 在传入 `checkAutoCommit` 前已应用 Shadow 删除规则，用户通过候选调整删词后剩余唯一时可正确触发顶码
- 当 `SkipShadow=true`（混输模式）：Shadow 由外层 `MixedEngine` 应用，`checkAutoCommit` **不**在此处应用 Shadow；外层须在 Shadow 后调用 `recheckAutoCommit` 重新评估（见 `internal/engine/mixed/AGENTS.md`）
- 不要在 `checkAutoCommit` 之前修改 `exactCandidates` 的内容，否则会影响计数准确性

### Testing Requirements
- `go test ./internal/engine/wubi/`
- `wubi_test.go`：精确匹配、前缀匹配、顶码、空码处理
- `wubi_freq_test.go`：词频功能（选词后候选排序变化）

### Common Patterns
- 码表路径由 Schema 文件 `dictionaries[].path` 指定，不再硬编码
- `DefaultConfig()` 返回合理默认值（TopCodeCommit=true、PunctCommit=true、DedupCandidates=true）
- 词频学习通过 `CompositeDict` 的 UserDict 层实现，引擎本身不直接持有 UserDict

## Dependencies
### Internal
- `internal/candidate` — Candidate 类型、CandidateSortMode
- `internal/dict` — CodeTable、DictManager、CompositeDict

### External
- 无

<!-- MANUAL: -->
