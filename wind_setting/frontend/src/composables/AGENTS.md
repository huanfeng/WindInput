<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-01 | Updated: 2026-06-03 -->

# composables

## Purpose
Vue 3 composables 目录。提供基于 `provide`/`inject` 的跨组件状态共享逻辑，目前包含全局 Toast 通知系统。

## Key Files
| 文件 | 说明 |
|------|------|
| `useToast.ts` | 全局 Toast 通知 composable：定义 `ToastItem`、`ToastContext` 接口；`provideToast()` 在根组件建立 Toast 上下文（`provide`）；`useToast()` 在子组件注入上下文（`inject`），返回 `{ toasts, toast }` |
| `useSettingsSearch.ts` | 设置搜索跳转逻辑：`useSettingsSearch(options)` 返回 `{ jumpTo(entry): Promise<boolean> }`；切换标签页 + 等待渲染 + 滚动到锚点 + 闪烁高亮，或通过回调打开对话框 |

## For AI Agents
### Working In This Directory
- **provide/inject 模式**：`provideToast()` 只能在根组件（`App.vue`）调用一次；所有子组件调用 `useToast()` 获取同一个上下文
- `toast(message, type?, duration?)` 签名：`type` 默认 `"success"`，`duration` 默认 `3000`ms
- Toast 通过 `setTimeout` 自动从 `toasts` 数组移除，无需手动清理
- `useToast()` 在未调用 `provideToast()` 的上下文中会抛出错误，便于调试遗漏的 provide
- **`useSettingsSearch` 在 `App.vue` 调用**，将 `activeTab` ref 和 `contentRef` 容器传入；`jumpTo` 由 `App.vue` 接收 `SettingsSearch.vue` 的 `jump` emit 后调用；dialog 回调（`onOpenSchemaSettings` / `onOpenImportExport`）连接到 `App.vue` 控制对话框的方法

### 接口定义
```typescript
// useToast
export interface ToastItem {
  id: number;
  message: string;
  type: 'success' | 'error';
}

export interface ToastContext {
  toasts: Ref<ToastItem[]>;
  toast: (message: string, type?: 'success' | 'error', duration?: number) => void;
}

// useSettingsSearch
interface UseSettingsSearchOptions {
  activeTab: Ref<string>;           // 当前激活标签页（双向绑定，jumpTo 会直接写入）
  container: Ref<HTMLElement | null>; // 内容滚动容器，用于 querySelector 锚点
  onOpenSchemaSettings?: () => void;  // 打开方案专属设置弹窗的回调
  onOpenImportExport?: (mode: 'import' | 'export') => void; // 打开词库导入/导出弹窗
}
// 返回值
// jumpTo(entry): Promise<boolean>
//   - 普通 entry：切 tab → nextTick → scrollIntoView + 闪烁高亮；找不到锚点返回 false（不报错）
//   - dialog entry：切 tab → nextTick → 调用对应回调；返回 true
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

// App.vue 中接入搜索跳转
import { useSettingsSearch } from './composables/useSettingsSearch'
const { jumpTo } = useSettingsSearch({
  activeTab,
  container: contentRef,
  onOpenSchemaSettings: () => { /* 打开方案专属设置弹窗 */ },
  onOpenImportExport: (mode) => { /* 打开词库导入/导出弹窗 */ },
})
// 接收 SettingsSearch.vue 的 jump emit
async function handleSearchJump(entry: SearchEntry) {
  await jumpTo(entry)
}
```

## Dependencies
### Internal
- `@/schemas/searchEntry` — `SearchEntry` 类型（`useSettingsSearch.ts` 使用）

### External
- Vue 3（`ref`、`inject`、`provide`、`InjectionKey`、`Ref`、`nextTick`）

<!-- MANUAL: -->
