<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# internal/clipboard

## Purpose
Windows 剪贴板操作封装。提供文本读写接口，通过 Windows API（user32.dll、kernel32.dll）实现剪贴板数据的获取和设置。

## Key Files
| File | Description |
|------|-------------|
| `clipboard.go` | `SetText()`/`GetText()` 函数；直接调用 Win32 API 的原生实现（`OpenClipboard`、`GetClipboardData` 等） |

## For AI Agents

### Working In This Directory
- 直接操作 Windows API：通过 `syscall` 和 `golang.org/x/sys/windows` 调用 user32.dll、kernel32.dll
- 内存管理：`GlobalAlloc`/`GlobalLock`/`GlobalUnlock`/`GlobalFree` 手动管理全局内存块
- UTF-16 编码：所有文本经 `syscall.UTF16FromString`/`UTF16ToString` 转换
- 剪贴板数据格式：`CF_UNICODETEXT`（格式 ID = 13）

### Testing Requirements
- 依赖 Windows 环境测试
- 单元测试可 mock syscall，或集成测试验证实际剪贴板操作

### Common Patterns
- 错误处理：API 调用失败时返回 `fmt.Errorf` 包装的错误信息
- 资源清理：`defer` 确保 `CloseClipboard`/`GlobalFree` 必被调用

## Dependencies
### Internal
- 无

### External
- `golang.org/x/sys/windows` — Windows API

<!-- MANUAL: -->
