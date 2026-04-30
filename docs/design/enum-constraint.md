<!-- Updated: 2026-05-01 -->

# 枚举与"魔法字符串"约束

> 本文是项目对**有限取值字符串配置/参数**的统一规范，是所有 AGENTS.md 中"枚举常量"段落的单一权威来源。
> 各模块 AGENTS.md 仅保留一句话引用本文，禁止再复述完整规则。

## 适用范围

任何**有限取值**的字符串配置/参数都受此约束，例如：

- 行为枚举：`"commit"`/`"clear"`/`"commit_and_input"`/`"ignore"`
- 模式枚举：`"smart"`/`"general"`/`"gb18030"`、`"horizontal"`/`"vertical"`、`"system"`/`"light"`/`"dark"`
- 按键名 token：`"semicolon"`、`"pageup"`、`"capslock"`
- 组合键群：`"pageupdown"`、`"semicolon_quote"`、`"lrshift"`
- 修饰键：`"ctrl"`/`"shift"`/`"alt"`/`"win"`
- Wails 事件名：`"config-event"`/`"dict-event"`

**禁止**在业务代码里直接散落字面量；必须通过具名常量引用。

## 单一事实来源（SSOT）

| 端 | 文件 | 内容类型 |
|----|------|---------|
| Go | `wind_input/pkg/config/enums.go` | 配置行为/模式枚举（FilterMode、ThemeStyle、EnterBehavior 等） |
| Go | `wind_input/pkg/keys/keys.go` | 按键名 `Key`、修饰键 `Modifier` + `aliasToKey` 双向表 |
| Go | `wind_input/pkg/keys/pair.go` | 组合键群 `PairGroup` + `pairGroupKeys` 表 |
| Go | `wind_input/internal/schema/types.go` | Schema/引擎相关枚举 |
| Go | `wind_input/pkg/rpcapi/types.go` | Wails 事件名常量（`WailsEventXxx`） |
| 前端 | `wind_setting/frontend/src/lib/enums.ts` | 上述各项的 TypeScript 镜像（`as const` 对象 + 联合类型） |

**前后端常量定义互为镜像**：YAML/JSON 字段值是协议，常量字面量值不可单边修改。修改任一端必须同步另一端。

## Go 端样板

```go
type FilterMode string

const (
    FilterSmart   FilterMode = "smart"
    FilterGeneral FilterMode = "general"
    FilterGB18030 FilterMode = "gb18030"
)

func (m FilterMode) Valid() bool {
    switch m {
    case FilterSmart, FilterGeneral, FilterGB18030:
        return true
    }
    return false
}
```

要求：

- 用 `type Foo string` + `const` 块；**禁止** `iota` 整数枚举（值会进 YAML，整数破坏可读性与兼容性）。
- YAML/JSON tag 不变，序列化值与原字符串完全一致。
- 每种类型提供 `Valid() bool`；空串与未知值返回 false。
- struct 字段类型用具名类型而非 `string`（如 `EnterBehavior EnterBehavior`，字段名与类型名同名合法）。
- **类型贯穿**：让具名类型一路流到 `internal/coordinator`/`internal/ui`/`internal/engine`，避免在调用点反复 `string(...)` 转换；仅在 syscall/RPC/`cmd/link -X` 等真边界做转换。
- 按键名走 `pkg/keys.Key` + `aliasToKey` 双向表；组合键群走 `pkg/keys.PairGroup` + `pairGroupKeys` 表驱动。
- 新增类型务必在 `pkg/config/enums_test.go` 加 YAML round-trip 测试，保证旧 YAML 配置可无损加载。

## 前端样板

```typescript
// 1. 定义：as const 对象 + 联合类型
export const FilterMode = {
  Smart: "smart",
  General: "general",
  GB18030: "gb18030",
} as const;
export type FilterModeValue = typeof FilterMode[keyof typeof FilterMode];

// 2. API 接口字段用联合类型，编译期拒绝非法值
import type { FilterModeValue } from "@/lib/enums";
export interface InputConfig {
  filter_mode: FilterModeValue;
}

// 3. 模板和逻辑都用常量
import { FilterMode } from "@/lib/enums";
// 模板：<SelectItem :value="FilterMode.Smart">智能</SelectItem>
// 逻辑：if (cfg.filter_mode === FilterMode.Smart) { ... }
```

要求：

- 字面量值**必须**与 Go 端 `pkg/config/enums.go` / `pkg/keys/` 完全一致。
- API 接口字段类型从 `string` 收紧为联合类型，让 TS 编译期捕获错配。
- 模板里的 `<option value="...">` / `<SelectItem value="...">` 用 `:value` 绑定常量，而非裸字面量。
- v-if/v-show/computed 的字符串比较一律用常量（`=== ThemeStyle.Dark`）。
- 模板内"展示+本地化文案"性质的字面量（如 `value="commit"` 出现于本地化文本上下文）允许保留，但 `<script>` 中的逻辑比较必须用常量。

## 真边界例外

仅以下场景可保留裸字符串：

- syscall（如 `windows.UTF16PtrFromString`）
- `cmd/link -X` 注入
- 跨进程协议字段（IPC 命令码命名等）
- test fixture 中的 YAML 文本字面量（用于覆盖各种历史值）

## 新增取值的流程

1. 先在 SSOT 文件加常量定义。
2. 再写比较点 / 序列化点。
3. 旧别名（如 `"page_up"`/`"pageup"` 这类历史并存）走 `pkg/keys.aliasToKey` 双向表统一规范化，**禁止**在业务代码里散落别名兼容分支。
4. 同步前端 `lib/enums.ts`（若涉及前后端协议字段）。
5. 加 YAML round-trip 测试（Go 端）。

## PR 自检清单

```bash
# 这些 grep 不应在常量定义文件之外有命中（除真边界例外）：
rg 'case "[a-z_]+":'   wind_input/internal
rg '== "[a-z_]+"'      wind_input/internal
rg "'[a-z_]+'"         wind_setting/frontend/src --type ts --type vue
```

任何命中都需要审视：是否应改为常量引用。

## 相关文档

- 顶层约束概览：[`/AGENTS.md`](../../AGENTS.md)
- Go 服务总入口：[`/wind_input/AGENTS.md`](../../wind_input/AGENTS.md)
- 前端源码总入口：[`/wind_setting/frontend/src/AGENTS.md`](../../wind_setting/frontend/src/AGENTS.md)
- 前端常量清单：[`/wind_setting/frontend/src/lib/AGENTS.md`](../../wind_setting/frontend/src/lib/AGENTS.md)
- Go 按键常量清单：[`/wind_input/pkg/keys/AGENTS.md`](../../wind_input/pkg/keys/AGENTS.md)
