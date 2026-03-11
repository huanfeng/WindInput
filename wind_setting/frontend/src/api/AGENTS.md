<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# api

## Purpose
前端 API 调用层，提供两种数据源的封装：

- `wails.ts`：通过 Wails IPC 调用 Go 后端（生产环境，`window.go.main.App.*`）
- `settings.ts`：通过 HTTP REST API 调用主程序（开发调试 / HTTP fallback，基础地址 `http://127.0.0.1:18923`）

`App.vue` 根据 `isWailsEnv` 决定使用哪个来源。

## Key Files
| 文件 | 说明 |
|------|------|
| `wails.ts` | Wails IPC 封装：导入 `wailsjs/go/main/App`，重新导出 Go 绑定类型，提供配置、短语、用户词库、Shadow、主题、服务控制等全部封装函数 |
| `settings.ts` | HTTP API 封装：定义所有配置 TypeScript 接口（`Config`、`EngineConfig` 等），提供 `getConfig`、`updateConfig`、`getStatus`、`switchEngine`、`getLogs` 等 fetch 函数；含 `getDefaultConfig()` 工厂函数 |

## For AI Agents
### Working In This Directory
- `wails.ts` 中的类型直接从 `wailsjs/go/models` 导出，**不要手动编辑** `wailsjs/` 目录下的文件
- 新增 Go 方法后：在 `app*.go` 定义方法 -> `wails dev` 自动更新 `wailsjs/` -> 在 `wails.ts` 添加对应封装
- `settings.ts` 的 TypeScript 接口须与 Go 结构体 JSON tag 保持一致（snake_case）
- `wails.ts` 导出的 `getDefaultConfig()` 使用 `config.Config` 构造器（Wails 模型类），`settings.ts` 的版本返回普通对象字面量；两者用途不同，注意区分
- 所有 `wails.ts` 函数均返回 `Promise`，错误由 Wails 运行时以 rejected Promise 传递

### Testing Requirements
- `pnpm run build`（TypeScript 类型检查）
- 在 `wails dev` 环境中调用每个 API 函数验证实际返回值

### Common Patterns
```typescript
// wails.ts 封装模式
import * as App from "../../wailsjs/go/main/App";
export async function getConfig(): Promise<Config> {
  return App.GetConfig();
}

// settings.ts HTTP 模式
async function request<T>(method, path, body?): Promise<APIResponse<T>> {
  const res = await fetch(`${API_BASE}${path}`, { method, ... });
  return res.json();
}
```

## Dependencies
### Internal
- `../../wailsjs/go/main/App` — Wails 自动生成的 Go 绑定
- `../../wailsjs/go/models` — Wails 自动生成的 Go 类型模型

### External
- 浏览器原生 `fetch` API

<!-- MANUAL: -->
