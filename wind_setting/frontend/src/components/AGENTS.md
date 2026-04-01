<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-01 | Updated: 2026-04-01 -->

# components

## Purpose
可复用 Vue 3 组件目录。目前包含全局 Toast 通知容器，由 `App.vue` 在根级挂载，通过 Vue Teleport 渲染到 `body` 顶层，供所有子组件使用。

## Key Files
| 文件 | 说明 |
|------|------|
| `ToastContainer.vue` | 全局浮动 Toast 容器：接收 `toasts: ToastItem[]` prop，通过 `<Teleport to="body">` 渲染到页面顶层；使用 `<TransitionGroup>` 实现入场/离场动画；支持 `success`（绿色）和 `error`（红色）两种类型 |

## For AI Agents
### Working In This Directory
- `ToastContainer` 由 `App.vue` 在根组件级别挂载，传入 `provideToast()` 返回的 `toasts` ref
- 子组件通过 `useToast()` composable 获取 `toast()` 函数，**不直接操作** `ToastContainer`
- Toast 样式定义在组件内（非 scoped），因为通过 Teleport 渲染到 body 时 scoped 选择器失效
- Toast 默认显示 3000ms 后自动消失（由 `useToast` 的 `setTimeout` 控制，非组件本身）
- 新增组件时，若需全局层级（遮罩、弹窗等），使用 `<Teleport to="body">` 避免层叠上下文问题

### Toast 视觉规格
| 属性 | 值 |
|------|-----|
| 位置 | 页面顶部居中（`top: 16px`，`left: 50%`，`translateX(-50%)`） |
| z-index | 9999 |
| 圆角 | 8px |
| 字号 | 13px |
| success 背景 | `#dcfce7`（浅绿），文字 `#166534` |
| error 背景 | `#fee2e2`（浅红），文字 `#991b1b` |

### Testing Requirements
- `pnpm run build`（TypeScript 类型检查）
- 在 Wails 环境中触发保存/错误操作，验证 Toast 正确显示和自动消失

### Common Patterns
```vue
<!-- App.vue 中挂载 -->
<ToastContainer :toasts="toasts" />

<!-- 子组件中使用（不直接引用 ToastContainer） -->
import { useToast } from '../composables/useToast'
const { toast } = useToast()
toast('操作成功')
toast('操作失败', 'error')
toast('提示信息', 'success', 2000)  // 自定义显示时长（ms）
```

## Dependencies
### Internal
- `../composables/useToast` — `ToastItem` 类型定义

### External
- Vue 3（`Teleport`、`TransitionGroup`、`defineProps`）

<!-- MANUAL: -->
