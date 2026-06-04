<!-- Parent: ../AGENTS.md -->
<!-- Updated: 2026-06-03 -->

# schemas

## Purpose
设置项 Schema 体系与搜索索引基础层。`types.ts` 定义字段类型体系，`*.schema.ts` 按页声明各设置项的元数据（label / hint / options），`searchEntry.ts` 提供搜索条目类型和从 schema 派生条目的纯函数。整个目录在编译期为搜索功能提供静态索引语料，运行时不依赖 DOM 扫描。

## Key Files
| 文件 | 说明 |
|------|------|
| `types.ts` | `PageSchema`、`FieldDef` 联合类型体系（`CardDef`、`SectionDef`、`ToggleField`、`SelectField`、`SliderField`、`NumberInputField`）；`getPath` / `setPath` 工具函数 |
| `searchEntry.ts` | `SearchEntry` 接口（搜索条目）；`schemaToEntries(schema, ctx)` 从 `PageSchema` 片段派生条目；`filterEntries(index, query)` 子串过滤 |
| `schema-engine-types.ts` | `EngineSchema` / `SchemaFieldDef` 等引擎 schema 专属类型；`filterEngineSchema(schema, engineType, tab)` |
| `engine.schema.ts` | `engineSchema: EngineSchema` — 方案专属设置（三种引擎 × basic/advanced tab），被 `SchemaSettingsDialog.vue` 使用；搜索时以入口级一条 entry 纳入索引，不做字段级展开 |
| `input.schema.ts` | 输入页各卡片 schema（`punctSchema`、`keyBehaviorSchema`、`overflowSchema` 等） |
| `appearance.schema.ts` | 外观页各卡片 schema（`themeExtraSchema`、`candidateWindowSchema`、`statusIndicatorSchema`、`toolbarSchema`） |
| `advanced.schema.ts` | 高级页 schema（`advancedLogSchema`、`advancedPerfSchema`） |
| `index.ts` | 重导出本目录所有公开类型和 schema 实例 |

## For AI Agents

### Working In This Directory
- **字段约定**：叶子字段（`toggle` / `select` / `slider` / `number-input`）必须包含 `key`（lodash 路径）和 `label`；`hint` 若为函数签名则不能被 `schemaToEntries` 静态读取（函数 hint 不纳入搜索 haystack）。
- **新增 schema 文件**后需在 `index.ts` 重导出，并在对应 `pages/*.search.ts` 里调用 `schemaToEntries` 加入搜索清单。
- **`schemaToEntries` 使用约定**：跳过 `card` / `section` 标记条目，仅处理叶子字段；`select` 字段的 `options[].label` 会收进搜索 haystack；`anchor` 默认等于字段 `key`，与 `FieldRenderer` 生成的 `data-search-anchor` 对齐。
- **engine.schema.ts 特殊处理**：引擎 schema 使用 `EngineSchema`（含 `engines`/`tab` 扩展字段），不能直接用 `schemaToEntries`；搜索层以 `general.search.ts` 中两条引擎入口条目（`openDialog: "schemaSettingsPinyin"` / `"schemaSettingsCodetable"`）覆盖，分别路由打开主拼音/主码表方案弹窗，不对字段逐条索引。
- **护栏约定**：`searchEntry.test.ts` 验证 `schemaToEntries` 和 `filterEntries` 行为；`searchIndex.test.ts` 验证全局索引 id 唯一性。

### Testing Requirements
- `pnpm run build`（TypeScript 编译覆盖所有 schema 文件）
- `pnpm test`（`schemas/searchEntry.test.ts` 覆盖 `schemaToEntries` / `filterEntries` 纯函数）

### Common Patterns
```typescript
// 从 schema 派生搜索条目（标准用法）
import { schemaToEntries } from '@/schemas/searchEntry'
import { punctSchema } from '@/schemas/input.schema'

export const entries = [
  ...schemaToEntries(punctSchema, { tab: 'input', tabLabel: '输入', card: '字符与标点' }),
]

// filterEntries：子串匹配，haystack = title + card + hint(静态) + options + keywords
import { filterEntries } from '@/schemas/searchEntry'
const results = filterEntries(searchIndex, '自动')  // 空 query 返回 []
```

## Dependencies

### Internal
- `@/api/settings` — `Config` 类型（`types.ts` 的 `CfgFn<T>` 依赖此类型）

### External
- TypeScript（纯类型声明，无运行时依赖）

## 全局约束

- 枚举与魔法字符串约束：见 [`/docs/design/enum-constraint.md`](../../../../docs/design/enum-constraint.md)。

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
