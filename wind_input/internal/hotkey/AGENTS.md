<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# internal/hotkey

## Purpose
热键配置编译器。将 `pkg/config.HotkeyConfig` 中的热键字符串（如 `"Ctrl+\`"`、`"Shift"`）编译为 C++ 侧可识别的 `uint32` 哈希列表，通过 `StatusUpdateData` 传递给 TSF Bridge，由 C++ 侧做低级热键拦截。

## Key Files
| File | Description |
|------|-------------|
| `compiler.go` | `Compiler`：`Compile()` 输出 keyDown 和 keyUp 两组热键哈希列表；`parseHotkeyString` 解析 `"ctrl+\`"` 等字符串为哈希；`compileSelectKeyGroup`/`compilePageKeyGroup`/`compileHighlightKeyGroup` 编译各类按键组；`compileToggleModeKey` 编译模式切换键（LShift/RShift/LCtrl/RCtrl/CapsLock）；`GetHotkeyDisplayName` 供 UI 显示热键名称；`getVirtualKeyCode` 完整支持字母/数字/F1-F12/特殊键 |

## For AI Agents

### Working In This Directory
- `Compile()` 返回两个 `[]uint32`：
  - `keyDownList`：按键按下时触发（功能热键 `SwitchEngine`/`ToggleFullWidth`/`TogglePunct`/`AddWord`、选词键、翻页键、高亮键）
  - `keyUpList`：按键抬起时触发（模式切换键如 `lshift`/`rshift`/`lctrl`/`rctrl`/`capslock`）
- 热键哈希算法：`ipc.CalcKeyHash(modifiers, keyCode)` = `(modifiers << 16) | keyCode`，与 C++ 侧共享
- 修饰键处理规则：`lshift` 同时包含 `ModShift|ModLShift`，`rshift` 包含 `ModShift|ModRShift`，以匹配 C++ 侧同时置位通用和具体修饰标志的行为
- `Coordinator` 缓存编译结果（`cachedKeyDownHotkeys`），配置变更时置 `hotkeysDirty=true` 触发重新编译
- 按键组类型——选词键：`semicolon_quote`、`comma_period`、`lrshift`、`lrctrl`；翻页键：`pageupdown`、`minus_equal`、`brackets`、`shift_tab`；高亮键：`tab`、`arrows`（arrows 由 C++ 侧原生处理，无需编译）
- `AddWord` 热键（`config.Hotkeys.AddWord`）为新增字段，触发快捷加词模式
- `ToggleS2T` 热键（`config.Hotkeys.ToggleS2T`，默认 `ctrl+shift+j`）切换简入繁出总开关

### Testing Requirements
- 热键编译结果可通过与 C++ 侧对照验证

### Common Patterns
- 配置变更时调用 `compiler.UpdateConfig(cfg)` 并清除缓存（`hotkeysDirty=true`）
- `GetHotkeyDisplayName(hash uint32) string` 将哈希反解为人类可读字符串（如 `"Ctrl+\`"`），供设置界面展示

## Dependencies
### Internal
- `internal/ipc` — `CalcKeyHash`/`ParseKeyHash` 函数、VK_* 虚拟键码常量、Mod* 修饰键常量
- `pkg/config` — `HotkeyConfig`（`SwitchEngine`/`ToggleFullWidth`/`TogglePunct`/`AddWord`）、`InputConfig`（`SelectKeyGroups`/`PageKeys`/`HighlightKeys`）

### External
- 无

<!-- MANUAL: -->
