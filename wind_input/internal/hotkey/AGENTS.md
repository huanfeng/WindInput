<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/hotkey

## Purpose
热键配置编译器。将 `pkg/config.HotkeyConfig` 中的热键字符串（如 `"Ctrl+\`"`、`"Shift"`）编译为 C++ 侧可识别的 `uint32` 哈希列表，通过 `StatusUpdateData` 传递给 TSF Bridge，由 C++ 侧做低级热键拦截。

## Key Files
| File | Description |
|------|-------------|
| `compiler.go` | `Compiler`：`Compile()` 输出 keyDown 和 keyUp 两组热键哈希列表 |

## For AI Agents

### Working In This Directory
- `Compile()` 返回两个 `[]uint32`：
  - `keyDownList`：按键按下时触发（功能热键、选词键、翻页键等）
  - `keyUpList`：按键抬起时触发（模式切换键如 Shift、Ctrl、CapsLock）
- 热键哈希算法与 C++ 侧共享（通过 `ipc.KeyHash` 函数）
- `Coordinator` 缓存编译结果（`cachedKeyDownHotkeys`），配置变更时重新编译
- 热键组类型：`semicolon_quote`、`comma_period`、`lrshift`、`lrctrl`、`pageupdown`、`minus_equal`、`brackets`、`shift_tab`

### Testing Requirements
- 热键编译结果可通过与 C++ 侧对照验证

### Common Patterns
- 配置变更时调用 `compiler.UpdateConfig(cfg)` 并清除缓存（`hotkeysDirty=true`）

## Dependencies
### Internal
- `internal/ipc` — `KeyHash` 函数（将虚拟键码+修饰键编码为 uint32）
- `pkg/config` — HotkeyConfig、InputConfig

### External
- 无

<!-- MANUAL: -->
