<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-08 -->

# api

## Purpose
前端 API 调用层，提供两种数据源的封装：

- `wails.ts`：通过 Wails IPC 调用 Go 后端（生产环境，`window.go.main.App.*`）
- `settings.ts`：通过 HTTP REST API 调用主程序（开发调试 / HTTP fallback，基础地址 `http://127.0.0.1:18923`）

`App.vue` 根据 `isWailsEnv` 决定使用哪个来源。

## Key Files
| 文件 | 说明 |
|------|------|
| `wails.ts` | Wails IPC 封装：导入 `wailsjs/go/main/App`，重新导出 Go 绑定类型，提供配置、短语、用户词库、Shadow、方案管理、主题、服务控制等全部封装函数 |
| `settings.ts` | HTTP API 封装：定义所有配置 TypeScript 接口（`Config`、`EngineConfig` 等），提供 `getConfig`、`updateConfig`、`getStatus`、`switchEngine`、`getLogs` 等 fetch 函数；含 `getDefaultConfig()` 工厂函数 |

## For AI Agents
### Working In This Directory
- `wails.ts` 中的类型直接从 `wailsjs/go/models` 导出，**不要手动编辑** `wailsjs/` 目录下的文件
- 新增 Go 方法后：在 `app*.go` 定义方法 -> `wails dev` 自动更新 `wailsjs/` -> 在 `wails.ts` 添加对应封装
- `settings.ts` 的 TypeScript 接口须与 Go 结构体 JSON tag 保持一致（snake_case）
- `wails.ts` 导出的 `getDefaultConfig()` 使用 `config.Config` 构造器（Wails 模型类），`settings.ts` 的版本返回普通对象字面量；两者用途不同，注意区分
- 所有 `wails.ts` 函数均返回 `Promise`，错误由 Wails 运行时以 rejected Promise 传递

### wails.ts 导出的 API 分组（2026-04-01）
| 分组 | 函数 |
|------|------|
| Schema 方案管理 | `getAvailableSchemas`、`getSchemaConfig`、`saveSchemaConfig`、`switchActiveSchema`、`getEnabledSchemasWithDictStats` |
| 配置管理 | `getConfig`、`saveConfig`、`reloadConfig`、`getTSFLogConfig`、`saveTSFLogConfig` |
| 短语管理 | `getPhrases`、`getSystemPhrases`、`savePhrases`、`addPhrase`、`removePhrase`、`updatePhrase`、`overrideSystemPhrase`、`removeSystemPhraseOverride` |
| 用户词库（当前方案） | `getUserDict`、`addUserWord`、`removeUserWord`、`searchUserDict`、`getUserDictStats`、`reloadUserDict`、`getUserDictSchemaID`、`switchUserDictSchema`、`importUserDict`、`exportUserDict` |
| 用户词库（按方案） | `getUserDictBySchema`、`addUserWordForSchema`、`removeUserWordForSchema`、`searchUserDictBySchema`、`importUserDictForSchema`、`exportUserDictForSchema` |
| 临时词库 | `getTempDictBySchema`、`clearTempDictForSchema`、`promoteTempWordForSchema`、`promoteAllTempWordsForSchema`、`removeTempWordForSchema` |
| Shadow 规则（当前方案） | `getShadowRules`、`pinShadowWord`、`deleteShadowWord`、`removeShadowRule` |
| Shadow 规则（按方案） | `getShadowBySchema`、`pinShadowWordForSchema`、`deleteShadowWordForSchema`、`removeShadowRuleForSchema` |
| 服务控制 | `notifyReload`、`openLogFolder`、`openConfigFolder`、`openExternalURL`、`getServiceStatus` |
| 主题管理 | `getAvailableThemes`、`getThemePreview` |
| 加词参数 | `getAddWordParams` |
| 其他 | `getStartPage`、`getVersion`、`getDefaultConfig`、`getDefaultTSFLogConfig` |

### Shadow API 说明（2026-03-13 架构重构）
旧的 `topShadowWord`/`reweightShadowWord` 已移除，替换为：
- `pinShadowWord(code, word, position)` — 固定词条到指定候选位置
- `deleteShadowWord(code, word)` — 隐藏词条
- `removeShadowRule(code, word)` — 彻底移除 Shadow 规则（撤销 pin 或 delete）

### 新增类型（2026-04-01）
- `SchemaDictStatsItem` — 方案词库统计（`schema_id`、`schema_name`、`icon_label`、`word_count`、`shadow_count`、`temp_word_count`）
- `TempWordItem` — 临时词库词条（`code`、`text`、`weight`、`count`）
- `ImportExportResult` — 导入导出结果（`cancelled`、`count`、`total?`、`path?`）
- `AddWordParams` — 加词参数（`text`、`code`、`schema_id`），由 Go 后端在加词模式启动时填入
- `ThemePreview` — 主题预览数据（已迁移到 `wails.ts` 定义，包含 `meta`、`candidate_window`、`toolbar`、`style?`、`is_dark?` 结构）

### settings.ts 变更（2026-04-01）
- `HotkeyConfig` 新增字段：`delete_candidate`、`pin_candidate`、`toggle_toolbar`、`open_settings`
- `InputConfig` 新增字段：`highlight_keys`（候选移动键）、`pinyin_separator`（拼音分隔符）、`temp_pinyin`（临时拼音配置，含 `trigger_keys`）
- `UIConfig` 新增字段：`theme_style`（`"system" | "light" | "dark"`）、`status_indicator_offset_x/y`
- `WubiConfig` 新增字段：`single_code_input`、`candidate_sort_mode`
- 新增接口：`TSFLogConfig`（`mode`、`level`）、`TempPinyinConfig`（`trigger_keys`）、`ToolbarConfig`（`visible`）
- 新增 `getDefaultTSFLogConfig()` 工厂函数

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

// Shadow pin + delete 操作
await wailsApi.pinShadowWord("sf", "村", 0);       // 固定到首位
await wailsApi.deleteShadowWord("sf", "什");        // 隐藏
await wailsApi.removeShadowRule("sf", "什");        // 移除规则

// settings.ts HTTP 模式
async function request<T>(method, path, body?): Promise<APIResponse<T>> {
  const res = await fetch(`${API_BASE}${path}`, { method, ... });
  return res.json();
}
```

## Dependencies
### Internal
- `../../wailsjs/go/main/App` — Wails 自动生成的 Go 绑定
- `../../wailsjs/go/models` — Wails 自动生成的 Go 类型模型（含 `main.SchemaInfo`、`main.SchemaConfig`、`main.ShadowRuleItem` 等）

### External
- 浏览器原生 `fetch` API

<!-- MANUAL: -->
