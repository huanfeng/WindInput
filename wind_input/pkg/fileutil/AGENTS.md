<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# pkg/fileutil

## Purpose
文件操作工具函数。提供原子写入（防止写入中途崩溃导致数据损坏）、安全写入（带备份）、文件变更检测（用于外部修改检测）等通用工具。

## Key Files
| File | Description |
|------|-------------|
| `atomic.go` | `AtomicWrite`/`AtomicWriteString`（写临时文件再重命名）、`SafeWrite`（带 .bak 备份）、`FileState`（变更检测）、`Exists`、`EnsureDir` |

## For AI Agents

### Working In This Directory
- `AtomicWrite`：写 `<path>.tmp` → `os.Rename` 到目标路径，保证原子性；同时确保目录存在
- `SafeWrite`：先将现有文件重命名为 `.bak`，再写新文件（非原子，但有备份）
- `FileState.HasChanged()`：通过 ModTime + Size 检测文件是否被外部修改
- 用户词库、Shadow 规则等重要数据文件应使用 `AtomicWrite` 保存

### Testing Requirements
- 纯 Go 逻辑，跨平台可测
- 重点测试：原子写入的原子性（进程中断时文件完整性）

### Common Patterns
- `internal/dict` 中的 `UserDict.Save()` 和 `ShadowLayer.Save()` 调用此包

## Dependencies
### Internal
- 无

### External
- 无（仅标准库）

<!-- MANUAL: -->
