<!-- Parent: ../AGENTS.md -->
<!-- Updated: 2026-05-12 -->

# keyinject

## Purpose
通过 Win32 `user32.SendInput` 向前台窗口注入键盘事件。命令直通车 (cmdbar) 的 `key.tap` / `key.seq` 动作函数走这条通路。

## Key Files
| File | Description |
|------|-------------|
| `keyinject.go` | `Combo` 结构 + `Parse(s)` 解析器, 跨平台 |
| `sendinput_windows.go` | Windows 下 `Tap` / `Sequence` / `Hold` / `Release` / `TypeText` 的 SendInput 实现, VK 表与 KEYBDINPUT 结构 |
| `sendinput_other.go` | 非 Windows 占位; `Tap` / `Sequence` / `Hold` / `Release` / `TypeText` 全部返回 `ErrUnsupportedPlatform`. macOS 真实键注入路径走 IMKit `.app` 内 `CGEventCreateKeyboardEvent` 或 NSPasteboard+Cmd+V, Go 服务侧不直接合成键, 见 [`docs/design/macos-port.md`](../../../docs/design/macos-port.md). |

## For AI Agents

### Working In This Directory
- 按键名 token 表 (`keyAliases`) 与 `pkg/keys/keys.go` 保持一致 —— 新增按键时**两处同步更新**, 见 `docs/design/enum-constraint.md`。
- VK 表 (`vkTable`) 只在 Windows 文件里; 单字符按键 (a-z / 0-9 / 标点) 在运行时通过 `VkKeyScanW` 查询当前键盘布局, 兼容非 US 布局。
- 修饰键合并去重, 顺序 canonical 化为 `ctrl < shift < alt < win`。
- `Parse` 是跨平台纯函数, 可以在所有平台上单测; SendInput e2e 测试只能在 Windows 桌面跑, 这里不写。

### Testing Requirements
- `keyinject_test.go` 表驱动覆盖 happy path 与错误输入。
- `go test ./internal/keyinject/...` 必须在所有平台 (linux/windows) 都能编译并通过。

## Dependencies

### Internal
- 无 (不依赖 cmdbar 或 pkg/keys, 维护内部别名表以避免循环)。

### External
- `golang.org/x/sys/windows` (仅 Windows 文件)。

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
