<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-01 | Updated: 2026-04-20 -->

# composables

## Purpose
Vue 3 composables 目录。提供基于 `provide`/`inject` 的跨组件状态共享逻辑，目前包含全局 Toast 通知系统。

## Key Files
| 文件 | 说明 |
|------|------|
| `useToast.ts` | 全局 Toast 通知 composable：定义 `ToastItem`、`ToastContext` 接口；`provideToast()` 在根组件建立 Toast 上下文（`provide`）；`useToast()` 在子组件注入上下文（`inject`），返回 `{ toasts, toast }` |

## For AI Agents
### Working In This Directory
- **provide/inject 模式**：`provideToast()` 只能在根组件（`App.vue`）调用一次；所有子组件调用 `useToast()` 获取同一个上下文
- `toast(message, type?, duration?)` 签名：`type` 默认 `"success"`，`duration` 默认 `3000`ms
- Toast 通过 `setTimeout` 自动从 `toasts` 数组移除，无需手动清理
- `useToast()` 在未调用 `provideToast()` 的上下文中会抛出错误，便于调试遗漏的 provide

### 接口定义
```typescript
export interface ToastItem {
  id: number;
  message: string;
  type: 'success' | 'error';
}

export interface ToastContext {
  toasts: Ref<ToastItem[]>;
  toast: (message: string, type?: 'success' | 'error', duration?: number) => void;
}
```

### Testing Requirements
- `pnpm run build`（TypeScript 类型检查）
- 验证 `useToast()` 在 `provideToast()` 缺失时正确抛出错误

### Common Patterns
```typescript
// App.vue（根组件）
import { provideToast } from './composables/useToast'
const { toasts, toast } = provideToast()
// toasts 传给 ToastContainer，toast 供 App.vue 自身使用

// 子组件（DictionaryPage、AddWordPage 等）
import { useToast } from '../composables/useToast'
const { toast } = useToast()
toast('保存成功')
toast('加载失败', 'error')
toast('已恢复默认', 'success', 2000)
```

## Dependencies
### Internal
无

### External
- Vue 3（`ref`、`inject`、`provide`、`InjectionKey`、`Ref`）

<!-- MANUAL: -->
