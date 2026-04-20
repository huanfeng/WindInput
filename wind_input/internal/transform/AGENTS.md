<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# internal/transform

## Purpose
文本转换工具包。提供两类转换：

1. **全角/半角转换**（`fullwidth.go`）：ASCII 字符与全角 Unicode 字符互转
2. **中英文标点转换**（`punctuation.go`）：英文标点与中文标点互转，处理成对标点（引号）的交替状态

## Key Files
| File | Description |
|------|-------------|
| `fullwidth.go` | ASCII 字符 → 全角字符转换函数 |
| `punctuation.go` | `PunctuationConverter`：维护引号配对状态，`Convert(rune, toChine) (string, bool)` |

## For AI Agents

### Working In This Directory
- `PunctuationConverter` 是有状态的（单引号/双引号各维护左右交替状态），每次 `FocusLost` 或 `ClearState` 后需 `Reset()`
- `coordinator` 持有一个 `PunctuationConverter` 实例（`c.punctConverter`），焦点丢失时重置
- 支持的中文标点映射（部分）：`,` → `，`，`.` → `。`，`?` → `？`，`!` → `！`，`<` → `《`，`>` → `》`
- 多字符映射：`^` → `……`，`_` → `——`
- 标点转换的 `bool` 返回值表示是否成功转换（未在映射表中的字符返回 false）

### Testing Requirements
- 纯 Go 逻辑，无平台依赖，可直接单元测试
- 重点测试引号配对逻辑的正确性

### Common Patterns
- `coordinator.handle_punctuation.go` 在中文标点模式下调用此包

## Dependencies
### Internal
- 无

### External
- 无

<!-- MANUAL: -->
