<!-- Parent: ../AGENTS.md -->
<!-- Updated: 2026-05-01 -->

# wind_input/pkg/keys

## Purpose
按键名与修饰键的统一规范化 token 包。**项目中所有按键字符串与修饰键字符串的单一权威来源**（SSOT），用于消除历史遗留的别名漂移（如 `"pageup"` vs `"page_up"`、`` "`" `` vs `"grave"`）。其它包应使用本包导出的具名常量与 `ParseKey`/`ParseModifier`，而非裸字符串比较。

## Key Files

| File | Description |
|------|-------------|
| `keys.go` | `Key`/`Modifier` 类型定义；字母 a-z、数字 0-9、F1-F12、标点、控制键、修饰键 token 常量；`aliasToKey` 双向规范化表；`ParseKey`/`ParseModifier`/`Valid` |
| `pair.go` | `PairGroup` 类型 + `pairGroupKeys` 表；翻页键、选择键、移动高亮等"组合键群"配置项的规范名集合 |

## 常量清单

### Modifier（修饰键）

| 常量 | 字面量值 |
|------|---------|
| `ModCtrl` | `ctrl` |
| `ModShift` | `shift` |
| `ModAlt` | `alt` |
| `ModWin` | `win` |

### Key（按键 token，节选）

| 类别 | 常量 | 字面量值 |
|------|------|---------|
| 字母 | `KeyA` … `KeyZ` | `a` … `z` |
| 数字 | `Key0` … `Key9` | `0` … `9` |
| 功能键 | `KeyF1` … `KeyF12` | `f1` … `f12` |
| 标点 | `KeySemicolon` / `KeyQuote` / `KeyComma` / `KeyPeriod` / `KeyMinus` / `KeyEqual` / `KeyLBracket` / `KeyRBracket` / `KeyBackslash` / `KeySlash` / `KeyGrave` | `semicolon` / `quote` / `comma` / `period` / `minus` / `equal` / `lbracket` / `rbracket` / `backslash` / `slash` / `grave` |
| 控制键 | `KeySpace` / `KeyTab` / `KeyEnter` / `KeyBackspace` / `KeyEscape` / `KeyPageUp` / `KeyPageDown` / `KeyShiftTab` | `space` / `tab` / `enter` / `backspace` / `escape` / `pageup` / `pagedown` / `shift_tab` |
| 修饰键作为独立 token | `KeyLShift` / `KeyRShift` / `KeyLCtrl` / `KeyRCtrl` / `KeyCapsLock` | `lshift` / `rshift` / `lctrl` / `rctrl` / `capslock` |

### 别名表（`aliasToKey`）

`aliasToKey` 将所有可接受的别名/字符表示映射到规范 `Key`。这是**唯一**的别名收敛点，禁止在业务代码里再写 `case "page_up"` 这类兼容分支。

代表性别名（→ 规范）：
- `` "`" `` / `"~"` / `"backtick"` → `KeyGrave`
- `"page_up"` / `"prior"` → `KeyPageUp`
- `"page_down"` / `"next"` → `KeyPageDown`
- `"esc"` → `KeyEscape`
- `"return"` → `KeyEnter`
- `"back"` → `KeyBackspace`
- `"plus"` / `"equal"` → `KeyEqual`
- `"open_bracket"` / `"lbracket"` → `KeyLBracket`
- `"close_bracket"` / `"rbracket"` → `KeyRBracket`

### PairGroup（组合键群）

| 常量 | 字面量值 | 组成按键对 |
|------|---------|----------|
| `PairSemicolonQuote` | `semicolon_quote` | `[KeySemicolon, KeyQuote]` |
| `PairCommaPeriod` | `comma_period` | `[KeyComma, KeyPeriod]` |
| `PairLRShift` | `lrshift` | `[KeyLShift, KeyRShift]` |
| `PairLRCtrl` | `lrctrl` | `[KeyLCtrl, KeyRCtrl]` |
| `PairPageUpDown` | `pageupdown` | `[KeyPageUp, KeyPageDown]` |
| `PairMinusEqual` | `minus_equal` | `[KeyMinus, KeyEqual]` |
| `PairBrackets` | `brackets` | `[KeyLBracket, KeyRBracket]` |
| `PairShiftTab` | `shift_tab` | `[KeyShiftTab, KeyTab]` |
| `PairTab` | `tab` | `[KeyShiftTab, KeyTab]` ⚠️ 与 `PairShiftTab` 同义双胞胎 |
| `PairArrows` | `arrows` | 4 方向键，不在 `pairGroupKeys` 表内，由调用方单独处理 |

> ⚠️ `PairShiftTab` 与 `PairTab` 字面量不同但实际配对相同，是历史 YAML 兼容产物。修改任一条 `pairGroupKeys` 必须同步另一条，否则会产生行为漂移。

## For AI Agents

### Working In This Directory
- **新增按键 token**：①在 `keys.go` 加 `KeyXxx` 常量 → ②在 `aliasToKey` 加映射（含所有可能的别名形式）→ ③同步前端 `lib/enums.ts` 中的 `Key` 镜像（若前端会用到）。
- **新增组合键群**：①在 `pair.go` 加 `PairXxx` 常量 → ②在 `pairGroupKeys` 表加映射 → ③同步前端 `lib/enums.ts` 中的 `PairGroup` 镜像。
- 业务代码**不要**直接 `switch s` 字符串；先 `key, ok := keys.ParseKey(s)`，对 `Key` 比较。
- 修改字面量值会破坏旧 YAML 配置兼容性 —— 通常**只在 `aliasToKey` 加新别名**，而非改规范名。

### Testing Requirements
- 单元测试：`go test ./pkg/keys/...`
- 必备测试场景：每个新 `Key`/`PairGroup` 都要测往返（`ParseKey("xxx").Valid() == true`）；新别名都要测能映射到正确规范名。

### Common Patterns
```go
import "github.com/huanfeng/wind_input/pkg/keys"

// 解析任意输入字符串
if k, ok := keys.ParseKey(userInput); ok {
    switch k {
    case keys.KeyPageUp:
        // ...
    }
}

// 组合键群配置展开为两个按键
if prev, next, ok := keys.PairPageUpDown.Keys(); ok {
    // prev=KeyPageUp, next=KeyPageDown
}
```

## Dependencies

### Internal
- 被 `wind_input/internal/coordinator`、`wind_input/internal/hotkey`、`wind_input/internal/engine`、`wind_input/internal/schema` 等几乎所有处理键盘输入的包引用。

### External
- 无（仅依赖 Go 标准库）。

## 全局约束
- 枚举与魔法字符串约束：见 [`/docs/design/enum-constraint.md`](../../../docs/design/enum-constraint.md)。本包是 Go 端按键相关常量的 SSOT，前端 `wind_setting/frontend/src/lib/enums.ts` 中的 `Key`/`PairGroup`/`Modifier` 是其镜像。

<!-- MANUAL: -->
