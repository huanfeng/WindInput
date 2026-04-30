<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# src

## Purpose
前端源码根目录。`App.vue` 是应用根组件，包含侧边栏导航和全局状态管理（配置加载、保存、主题、服务状态）。`main.ts` 创建 Vue 应用实例。

## Key Files
| 文件 | 说明 |
|------|------|
| `main.ts` | Vue 应用入口，挂载 `App.vue` 到 `#app` |
| `App.vue` | 根组件：侧边栏 + 内容区；管理全局 `formData`（Config）、`status`、`engines`、`availableThemes`、`themePreview`；通过 `provideToast()` 提供全局 Toast 上下文；包含 `AddWordPage` 对话框逻辑（独立加词窗口模式和嵌入弹出模式） |
| `style.css` | 基础全局样式 |
| `global.css` | 补充全局样式（字体等） |
| `vite-env.d.ts` | Vite 环境类型声明 |

## Subdirectories
| 目录 | 说明 |
|------|------|
| `api/` | API 调用层 (see `api/AGENTS.md`) |
| `pages/` | 各设置页面组件（含 AddWordPage） (see `pages/AGENTS.md`) |
| `components/` | 可复用组件（ToastContainer、词库管理） (see `components/AGENTS.md`) |
| `composables/` | Vue composables（useToast） (see `composables/AGENTS.md`) |
| `assets/` | 静态资源（字体、图片） (see `assets/AGENTS.md`) |
| `lib/` | 工具函数库（样式类合并） (see `lib/AGENTS.md`) |

## For AI Agents
### Working In This Directory
- `App.vue` 维护全局 `formData: Config`，通过 props 传递给各页面组件；页面组件直接修改 `formData` 的属性（对象引用共享）
- 运行环境检测：`isWailsEnv = window?.go?.main?.App !== undefined`，决定使用 `wailsApi` 还是 `api`（HTTP）
- 双 API 源：`./api/settings.ts`（HTTP，用于 `wails dev` 调试代理）和 `./api/wails.ts`（Wails IPC，生产）
- 标签页 ID 与页面组件一一对应：`general`、`input`、`hotkey`、`appearance`、`dictionary`、`advanced`、`about`
- 配置合并：`mergeWithDefaults(cfg)` 将服务端配置与前端默认值深合并，防止后端未定义字段导致 UI 异常
- 快捷键冲突由 `HotkeyPage` 检测后通过 emit 上报 `hotkeyConflicts`，有冲突时禁止保存
- **Toast 系统**：`App.vue` 调用 `provideToast()` 建立 Toast 上下文，所有子组件通过 `useToast()` 获取 `toast()` 函数；`ToastContainer` 通过 `<Teleport to="body">` 渲染到 body 顶层，实现全局浮动通知
- **加词对话框**：`App.vue` 控制 `showAddWordDialog`；独立模式（`isStandaloneAddWord`）下关闭时调用 `Quit()` 退出进程；通过 Wails 事件 `navigate-addword` 支持从候选框快捷加词

### Testing Requirements
- TypeScript 编译无错误：`pnpm run build`
- 在 Wails 环境中验证页面切换、保存、重载等流程

### Common Patterns
```typescript
// 页面组件接收 formData prop，直接修改其属性
const props = defineProps<{ formData: Config }>()
// 修改：props.formData.engine.type = 'pinyin'
// 保存由 App.vue 统一处理，页面组件不直接调用 saveConfig

// Toast 使用（子组件内）
import { useToast } from '../composables/useToast'
const { toast } = useToast()
toast('保存成功')
toast('操作失败', 'error')
```

### 枚举常量规范（强制）
有限取值的字符串配置/参数禁止散落字面量，必须从 `./lib/enums` 导入常量引用。详见根 `AGENTS.md` 的"枚举与魔法字符串约束"节。

**前端实现样板**（参考 `lib/enums.ts`）：
```typescript
// 1. 定义：as const 对象 + 联合类型
export const FilterMode = {
  Smart: 'smart',
  General: 'general',
  GB18030: 'gb18030',
} as const;
export type FilterModeValue = typeof FilterMode[keyof typeof FilterMode];

// 2. API 接口字段用联合类型，编译期拒绝非法值
import type { FilterModeValue, ThemeStyleValue } from '@/lib/enums';
export interface InputConfig {
  filter_mode: FilterModeValue;
  // ...
}

// 3. 模板和逻辑都用常量
import { FilterMode, ThemeStyle } from '@/lib/enums';
// 模板：<SelectItem :value="FilterMode.Smart">智能</SelectItem>
// 逻辑：if (cfg.filter_mode === FilterMode.Smart) { ... }
```

要求：
- 字面量值**必须**与 Go 端 `pkg/config/enums.go`/`pkg/keys/` 保持完全一致（YAML/JSON 协议字段）
- 修改任一端的常量值时，另一端必须同步——前后端常量定义互为镜像
- API 接口字段类型从 `string` 收紧为联合类型，让 TS 编译期捕获错配
- 模板里的 `<option value="...">`、`<SelectItem value="...">` 用 `:value` 绑定常量，而非裸字面量
- v-if/v-show/computed 的字符串比较一律用常量（`=== ThemeStyle.Dark`）

## Dependencies
### Internal
- `./api/settings` — HTTP API 类型和函数
- `./api/wails` — Wails IPC 封装
- `./pages/*` — 各设置页面
- `./components/ToastContainer` — 全局 Toast 渲染容器
- `./composables/useToast` — Toast provide/inject 逻辑

### External
- Vue 3（`ref`、`computed`、`onMounted`）
- Wails runtime（`EventsOn`、`Quit`、`Show`、`WindowSetAlwaysOnTop`）

<!-- MANUAL: -->
