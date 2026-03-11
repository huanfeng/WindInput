<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# src

## Purpose
前端源码根目录。`App.vue` 是应用根组件，包含侧边栏导航和全局状态管理（配置加载、保存、主题、服务状态）。`main.ts` 创建 Vue 应用实例。

## Key Files
| 文件 | 说明 |
|------|------|
| `main.ts` | Vue 应用入口，挂载 `App.vue` 到 `#app` |
| `App.vue` | 根组件：侧边栏 + 内容区；管理全局 `formData`（Config）、`status`、`engines`、`availableThemes`、`themePreview` |
| `style.css` | 基础全局样式 |
| `global.css` | 补充全局样式（字体等） |
| `vite-env.d.ts` | Vite 环境类型声明 |

## Subdirectories
| 目录 | 说明 |
|------|------|
| `api/` | API 调用层 |
| `pages/` | 各设置页面组件 |
| `assets/` | 静态资源 |

## For AI Agents
### Working In This Directory
- `App.vue` 维护全局 `formData: Config`，通过 props 传递给各页面组件；页面组件直接修改 `formData` 的属性（对象引用共享）
- 运行环境检测：`isWailsEnv = window?.go?.main?.App !== undefined`，决定使用 `wailsApi` 还是 `api`（HTTP）
- 双 API 源：`./api/settings.ts`（HTTP，用于 `wails dev` 调试代理）和 `./api/wails.ts`（Wails IPC，生产）
- 标签页 ID 与页面组件一一对应：`general`、`input`、`hotkey`、`appearance`、`dictionary`、`advanced`、`about`
- 配置合并：`mergeWithDefaults(cfg)` 将服务端配置与前端默认值深合并，防止后端未定义字段导致 UI 异常
- 快捷键冲突由 `HotkeyPage` 检测后通过 emit 上报 `hotkeyConflicts`，有冲突时禁止保存

### Testing Requirements
- TypeScript 编译无错误：`pnpm run build`
- 在 Wails 环境中验证页面切换、保存、重载等流程

### Common Patterns
```typescript
// 页面组件接收 formData prop，直接修改其属性
const props = defineProps<{ formData: Config }>()
// 修改：props.formData.engine.type = 'pinyin'
// 保存由 App.vue 统一处理，页面组件不直接调用 saveConfig
```

## Dependencies
### Internal
- `./api/settings` — HTTP API 类型和函数
- `./api/wails` — Wails IPC 封装
- `./pages/*` — 各设置页面

### External
- Vue 3（`ref`、`computed`、`onMounted`）

<!-- MANUAL: -->
