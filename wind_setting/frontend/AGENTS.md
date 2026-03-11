<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# frontend

## Purpose
`wind_setting` 的 Vue 3 + TypeScript 前端。由 Vite 构建，产物嵌入 Go 二进制（`//go:embed all:frontend/dist`）。使用 pnpm 作为包管理器。

## Key Files
| 文件 | 说明 |
|------|------|
| `index.html` | Vite HTML 入口 |
| `package.json` | 依赖声明：Vue 3、Vite、vue-tsc、@vitejs/plugin-vue |
| `pnpm-lock.yaml` | pnpm 锁文件，提交到仓库 |
| `vite.config.ts` | Vite 配置（plugin-vue） |
| `tsconfig.json` / `tsconfig.node.json` | TypeScript 配置 |

## Subdirectories
| 目录 | 说明 |
|------|------|
| `src/` | 前端源码根目录 |
| `src/api/` | API 调用层（Wails IPC + HTTP fallback） |
| `src/pages/` | 各设置页面 Vue 组件 |
| `src/assets/` | 静态资源（字体、图片） |

## For AI Agents
### Working In This Directory
- 包管理器：**仅使用 pnpm**，不使用 npm 或 yarn
- 安装依赖：`pnpm install`
- 构建：`pnpm run build`（运行 `vue-tsc --noEmit` 类型检查后执行 vite build）
- 开发模式：`pnpm run dev`（由 `wails dev` 自动调用）
- Wails 自动生成的类型绑定位于 `wailsjs/`（构建产物，不手动编辑）
- 修改 `.vue` 或 `.ts` 文件后需运行前端格式化

### Testing Requirements
- `pnpm run build` 无 TypeScript 错误
- 在 `wails dev` 环境中验证 IPC 调用正常

### Common Patterns
- 新增页面：在 `src/pages/` 创建 Vue 组件，在 `App.vue` 中注册标签页
- 新增 Go 绑定：在 `app*.go` 添加方法后重新运行 `wails dev`（自动重新生成 `wailsjs/`），在 `src/api/wails.ts` 添加对应封装函数

## Dependencies
### Internal
- `src/api/wails.ts` — Wails IPC API 封装
- `src/api/settings.ts` — HTTP REST API（开发/调试用）

### External
- Vue 3.2
- Vite 3
- TypeScript 4.6
- vue-tsc 1.8

<!-- MANUAL: -->
