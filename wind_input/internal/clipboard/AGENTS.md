<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-05-29 -->
<!-- Generated: 2026-04-20 | Updated: 2026-05-26 -->

# internal/clipboard

## Purpose
Windows 剪贴板操作封装。提供文本与图像读写接口，通过 Windows API（user32.dll、kernel32.dll）实现剪贴板数据的获取和设置。
跨平台剪贴板封装. Win 端真实实现 (user32/kernel32 系统调用), darwin 端当前返回 `ErrNotImplemented` (待 macOS IMKit `.app` 工程通过 NSPasteboard 接管, 见 [`docs/design/macos-port.md`](../../../docs/design/macos-port.md)).

## Key Files
| File | Description |
|------|-------------|
| `clipboard.go` | `SetText()`/`GetText()`/`SetImage()` 函数；直接调用 Win32 API 的原生实现（`OpenClipboard`、`GetClipboardData` 等） |
| `clipboard.go` (`//go:build windows`) | `SetText()`/`GetText()` Win 实现; 直接 `OpenClipboard` + `GetClipboardData` + GlobalAlloc/Lock 全套 |
| `clipboard_darwin.go` (`//go:build darwin`) | 同名函数 stub; 返回 `ErrNotImplemented` 让调用方 (cmdbar copy action 等) 明确失败而非静默 |

## For AI Agents

### Working In This Directory
- 直接操作 Windows API：通过 `syscall` 和 `golang.org/x/sys/windows` 调用 user32.dll、kernel32.dll
- 内存管理：`GlobalAlloc`/`GlobalLock`/`GlobalUnlock`/`GlobalFree` 手动管理全局内存块
- UTF-16 编码：所有文本经 `syscall.UTF16FromString`/`UTF16ToString` 转换
- 剪贴板数据格式：文本用 `CF_UNICODETEXT`（13）；图像用 `CF_DIBV5`（17，`BITMAPV5HEADER` + 32 位 BGRA，含 alpha 掩码，保留透明度）
- `SetImage` 输入为预乘 alpha 的 `*image.RGBA`（与 UI 渲染器一致），写入前还原为 straight alpha；DIB 为 bottom-up（正高度）

### Testing Requirements
- 依赖 Windows 环境测试
- 单元测试可 mock syscall，或集成测试验证实际剪贴板操作

### Common Patterns
- 错误处理：API 调用失败时返回 `fmt.Errorf` 包装的错误信息
- 资源清理：`defer` 确保 `CloseClipboard`/`GlobalFree` 必被调用

### darwin 端约定
- 调用方必须能够吞 `ErrNotImplemented`; 不应导致 panic 或 service crash
- 实际剪贴板能力的归属决策: cmdbar "复制候选" 等用户操作在 macOS 上**由 IMKit `.app` 端直接调 `NSPasteboard.generalPasteboard`** 完成, Go 服务端不参与, 避免请求 macOS 剪贴板权限. 详见 macos-port.md.

## Dependencies
### Internal
- 无

### External
- Win: `golang.org/x/sys/windows`
- darwin: 仅标准库 `errors`

<!-- MANUAL: -->
