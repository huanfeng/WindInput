<!-- Parent: ../AGENTS.md -->
<!-- Updated: 2026-05-01 -->

# lib

## Purpose
前端工具与常量库。**`enums.ts` 是前端枚举常量的单一权威来源**，与 Go 端 `wind_input/pkg/config/enums.go`、`wind_input/pkg/keys/` 互为镜像，是前后端一致性的关键锚点。`utils.ts` 提供样式类合并等通用工具。

## Key Files

| 文件 | 说明 |
|------|------|
| `enums.ts` | 所有有限取值字符串常量（行为/模式枚举、按键名、组合键群、Wails 事件名等）的 TypeScript 定义；导出 `as const` 对象 + 联合类型 |
| `utils.ts` | 样式类合并函数 `cn()`，使用 `clsx` + `tailwind-merge` 组合 CSS 类名，支持条件类名和 Tailwind 冲突解决 |

## 常量清单（`enums.ts`）

> 字面量值必须与 Go 端保持一致；修改任一端需同步另一端。

### 行为枚举（对应 Go `pkg/config/enums.go`）

| 常量名 | 字面量值 | Go 镜像 |
|--------|---------|---------|
| `EnterBehavior` | `commit` / `clear` / `commit_and_input` / `ignore` | `EnterBehavior` |
| `SpaceOnEmptyBehavior` | `commit` / `clear` / `commit_and_input` / `ignore` | `SpaceOnEmptyBehavior` |
| `OverflowBehavior` | `ignore` / `commit` / `commit_and_input` | `OverflowBehavior` |
| `FilterMode` | `smart` / `general` / `gb18030` | `FilterMode` |
| `ThemeStyle` | `system` / `light` / `dark` | `ThemeStyle` |
| `CandidateLayout` | `horizontal` / `vertical` | `CandidateLayout` |
| `PreeditMode` | `top` / `embedded` | `PreeditMode` |
| `PinyinSeparatorMode` | `auto` / `quote` / `backtick` / `none` | `PinyinSeparatorMode` |
| `FontEngine` | `directwrite` / `gdi` / `freetype` | `FontEngine` |

### 修饰键 / 按键名（对应 Go `pkg/keys/keys.go`）

| 常量名 | 字面量值 | Go 镜像 |
|--------|---------|---------|
| `Modifier` | `ctrl` / `shift` / `alt` / `win` | `ModCtrl` / `ModShift` / `ModAlt` / `ModWin` |
| `Key`（前端按需收录子集） | `z` / `semicolon` / `quote` / `comma` / `period` / `slash` / `backslash` / `open_bracket` / `close_bracket` / `backtick` / `tab` / `lshift` / `rshift` / `lctrl` / `rctrl` / `capslock` | `KeyZ` / `KeySemicolon` / `KeyQuote` / `KeyComma` / `KeyPeriod` / `KeySlash` / `KeyBackslash` / `KeyLBracket` / `KeyRBracket` / `KeyGrave` / `KeyTab` / `KeyLShift` / `KeyRShift` / `KeyLCtrl` / `KeyRCtrl` / `KeyCapsLock` |

注：前端 `Key.Backtick = "backtick"` 对应 Go 侧 `KeyGrave = "grave"`，由 `aliasToKey` 双向表负责规范化；前端 `Key.OpenBracket`/`CloseBracket` 对应 Go 的 `KeyLBracket`/`KeyRBracket`。

### 组合键群（对应 Go `pkg/keys/pair.go`）

| 常量 `PairGroup.*` | 字面量值 | Go 镜像 |
|--------|---------|---------|
| `SemicolonQuote` | `semicolon_quote` | `PairSemicolonQuote` |
| `CommaPeriod` | `comma_period` | `PairCommaPeriod` |
| `LRShift` | `lrshift` | `PairLRShift` |
| `LRCtrl` | `lrctrl` | `PairLRCtrl` |
| `PageUpDown` | `pageupdown` | `PairPageUpDown` |
| `MinusEqual` | `minus_equal` | `PairMinusEqual` |
| `Brackets` | `brackets` | `PairBrackets` |
| `ShiftTab` | `shift_tab` | `PairShiftTab` |
| `Tab` | `tab` | `PairTab` |
| `Arrows` | `arrows` | `PairArrows` |

### UI 子枚举（前端独有，未在 Go 端枚举类型化）

| 常量名 | 字面量值 | 用途 |
|--------|---------|------|
| `StatusDisplayMode` | `temp` / `always` | 状态提示显示模式 |
| `SchemaNameStyle` | `short` / `full` | 状态提示方案名风格 |
| `StatusPositionMode` | `follow_caret` / `custom` | 状态提示位置模式 |
| `NumpadBehavior` | `direct` / `follow_main` | 数字小键盘行为 |
| `ShiftBehavior` | `temp_english` / `direct_commit` | Shift+字母临时英文行为 |

> 这些常量虽是"前端独有"，但若未来后端要消费同一字段，必须在 Go 端补充镜像后再使用。

### Wails 事件名（对应 Go `pkg/rpcapi/types.go` 中的 `WailsEventXxx`）

| 常量 | 字面量值 | Go 镜像 |
|------|---------|---------|
| `WailsEvent.Config` | `config-event` | `WailsEventConfig` |
| `WailsEvent.Dict` | `dict-event` | `WailsEventDict` |

## For AI Agents

### Working In This Directory
- 修改 `enums.ts` 中任一字面量值时，**必须同步**修改对应 Go 端常量（见上表"Go 镜像"列）。
- 新增枚举常量按以下顺序：①Go 端 SSOT 文件加常量 → ②前端 `enums.ts` 加镜像 → ③本 AGENTS.md 表格补一行。
- 模板里的 `<SelectItem :value="...">` 一律用 `:value` 绑定常量，而非裸字面量。
- `cn()` 是 shadcn 样式工具标准实现，接收任意数量的 `ClassValue` 参数；用于条件样式与 Tailwind 类冲突解决。

### Testing Requirements
- TypeScript 编译无错误：`pnpm run build`
- 修改后建议 grep 是否还有未替换的字面量比较：
  ```bash
  rg "=== ['\"][a-z_]+['\"]" wind_setting/frontend/src --type ts --type vue
  ```

### Common Patterns

```typescript
// enums.ts 用法
import { FilterMode, ThemeStyle, PairGroup } from "@/lib/enums";
import type { FilterModeValue } from "@/lib/enums";

interface InputConfig { filter_mode: FilterModeValue }
if (cfg.filter_mode === FilterMode.Smart) { /* ... */ }
// 模板：<SelectItem :value="FilterMode.Smart">智能</SelectItem>

// utils.ts 用法
import { cn } from "@/lib/utils";
const buttonClass = cn("px-4 py-2", "rounded-md", isActive && "bg-blue-500");
const headingClass = cn("text-lg", "text-base"); // twMerge：text-base 覆盖
```

## Dependencies

### Internal
- `enums.ts` 被 `wind_setting/frontend/src/pages/*`、`components/*`、`api/*` 广泛引用。
- `utils.ts` 被绝大多数组件引用。

### External
- `clsx` — 条件类名生成
- `tailwind-merge` — Tailwind CSS 冲突解决
- 其余无（`enums.ts` 零运行时依赖）

## 全局约束
- 枚举与魔法字符串约束：见 [`/docs/design/enum-constraint.md`](../../../../docs/design/enum-constraint.md)。

<!-- MANUAL: -->
