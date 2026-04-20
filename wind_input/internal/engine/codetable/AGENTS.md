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
  - `EnableUserFreq`：启用词频学习（由 Schema `learning.mode` 控制，`auto`/`frequency` 时为 true）
  - `ProtectTopN`：首选保护，前 N 位锁定码表原始顺序不受词频影响
  - `CandidateSortMode`：候选排序模式，与 `CompositeDict.SetSortMode` 同步
- `HandleTopCode(input string)` 处理五码输入：截取前四码查找候选，剩余一码作为新输入
- `ConvertEx` 返回 `*WubiResult`，包含 `ShouldCommit`、`CommitText`、`IsEmpty`、`ShouldClear`、`ToEnglish`
- 码表通过 `schema/factory.go` 加载（Rime `.dict.yaml` 多文件合并或传统单文件），引擎直接持有 `*dict.CodeTable`
- `OnCandidateSelected` 在 `EnableUserFreq=true` 时调用 `DictManager` 的用户词库层增加词频
- `RestoreCodeTableHeader` 供 factory 从 sidecar meta.json 恢复码表元数据

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
