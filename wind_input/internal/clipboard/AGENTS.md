<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-06-01 -->

# internal/clipboard

## Purpose
跨平台剪贴板封装. Win 端通过 Windows API（user32/kernel32）真实实现文本与图像读写; darwin 端经 `pbcopy`/`pbpaste` 子进程实现文本读写（服务进程即可访问当前用户通用剪贴板, 无需 CGO/AppKit, 无 NSPasteboard 权限弹窗）.

## Key Files
| File | Description |
|------|-------------|
| `clipboard.go` (`//go:build windows`) | `SetText()`/`GetText()`/`SetImage()` Win 实现; 直接 `OpenClipboard` + `GetClipboardData` + GlobalAlloc/Lock 全套 |
| `clipboard_darwin.go` (`//go:build darwin`) | `SetText()`/`GetText()` 经 `pbcopy`/`pbpaste`（stdin 管道传文本, 无 shell 注入风险）; **强制 UTF-8 locale**（`utf8Env()` 追加 `LC_ALL=en_US.UTF-8`/`LANG=en_US.UTF-8`）: LaunchAgent 进程无 `LANG` 时 pbpaste 会按系统默认编码（中文环境为 GBK）输出, 致非 ASCII 文本不是合法 UTF-8（上屏时 IMKit `.app` 严格 UTF-8 解码失败丢字）; `SetImage` 暂未实现 |

## For AI Agents

### Working In This Directory
- Win: 直接操作 Windows API（`syscall` + `golang.org/x/sys/windows` 调 user32.dll、kernel32.dll）
- Win 内存管理：`GlobalAlloc`/`GlobalLock`/`GlobalUnlock`/`GlobalFree` 手动管理全局内存块
- Win UTF-16 编码：所有文本经 `syscall.UTF16FromString`/`UTF16ToString` 转换
- Win 剪贴板数据格式：文本用 `CF_UNICODETEXT`（13）；图像用 `CF_DIBV5`（17，`BITMAPV5HEADER` + 32 位 BGRA，含 alpha 掩码，保留透明度）
- Win `SetImage` 输入为预乘 alpha 的 `*image.RGBA`（与 UI 渲染器一致），写入前还原为 straight alpha；DIB 为 bottom-up（正高度）

### Testing Requirements
- Win 依赖 Windows 环境测试；单元测试可 mock syscall，或集成测试验证实际剪贴板操作
- darwin 依赖 `pbcopy`/`pbpaste` 存在的真实 macOS 环境

### Common Patterns
- 错误处理：API 调用失败时返回 `fmt.Errorf` 包装的错误信息
- Win 资源清理：`defer` 确保 `CloseClipboard`/`GlobalFree` 必被调用

### darwin 端约定
- `GetText`/`SetText` 经 `pbpaste`/`pbcopy` 子进程, **必须**用 `utf8Env()` 设 `LC_ALL=en_US.UTF-8`, 否则 LaunchAgent 无 `LANG` 时 CJK 文本会被编成 GBK 字节（非法 UTF-8, 下游 `client.insertText` 解码失败丢字）
- cmdbar `clip.copy` 走 `SetText`(pbcopy), `clip.paste` 走 `GetText`(pbpaste) 后经 .app `insertText` 上屏（不模拟 Cmd+V, 免辅助功能授权）
- `SetImage` 暂未实现（cmdbar 不需要）; 需要时经 `osascript`/`NSPasteboard` 补

## Dependencies
### Internal
- 无

### External
- Win: `golang.org/x/sys/windows`
- darwin: 仅标准库 `os`/`os/exec`/`strings`（调 pbcopy/pbpaste + UTF-8 env）

<!-- MANUAL: -->
