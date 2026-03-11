<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/engine/wubi

## Purpose
五笔输入引擎实现。基于码表（`dict.CodeTable`）实现按键到候选词的查找，支持：精确匹配、前缀匹配（逐码提示）、四码自动上屏、五码顶字上屏、标点顶字、空码处理。

## Key Files
| File | Description |
|------|-------------|
| `wubi.go` | `Engine` 结构体、`Config`、`DefaultConfig`、码表加载（文本/二进制）、`Convert`/`ConvertEx`/`HandleTopCode` |
| `wubi_test.go` | 引擎功能测试 |

## For AI Agents

### Working In This Directory
- `Config` 字段说明：
  - `AutoCommitAt4`：四码且唯一候选时自动上屏
  - `ClearOnEmptyAt4`：四码无候选时清空输入
  - `TopCodeCommit`：第五码输入时顶掉第一候选上屏（顶码）
  - `PunctCommit`：标点符号触发顶码上屏
  - `SingleCodeInput`：关闭前缀匹配，只做精确匹配（逐字键入模式）
  - `ShowCodeHint`：候选词显示四码编码提示
- `HandleTopCode(input string)` 处理五码输入：截取前四码查找候选，剩余一码作为新输入
- `ConvertEx` 返回 `*WubiResult`，包含 `ShouldCommit`、`CommitText`、`IsEmpty`、`ShouldClear`、`ToEnglish`
- 码表加载支持文本格式（`LoadCodeTable`）和二进制 wdb 格式（`LoadCodeTableBinary`）
- 引擎可选接入 `DictManager` 查询用户短语（`dictManager.Search`）

### Testing Requirements
- `go test ./internal/engine/wubi/`
- 测试用例覆盖：精确匹配、前缀匹配、顶码、空码处理

### Common Patterns
- 码表路径默认：`<exeDir>/dict/wubi/wubi86.txt`（文本）或 `wubi.wdb`（二进制）
- `DefaultConfig()` 返回合理默认值（TopCodeCommit=true、PunctCommit=true、DedupCandidates=true）
- `cmd/test_codetable` 可用于交互式调试引擎行为

## Dependencies
### Internal
- `internal/candidate` — Candidate 类型
- `internal/dict` — CodeTable、DictManager

### External
- 无

<!-- MANUAL: -->
