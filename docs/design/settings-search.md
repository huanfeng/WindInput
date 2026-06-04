# 设置搜索功能 — 设计

> 状态：设计稿（待评审）
> 关联：`wind_setting/frontend`（Wails + Vue 3 + TS）、`schemas/*.schema.ts` 体系、vitest 测试设施

## 1. 目标与范围

为设置窗口增加**跳转导航式**搜索：顶部输入关键词 → 结果列表 → 点击某条直接切换到对应标签页并滚动、高亮该设置项（类似 VSCode 命令面板 / macOS 系统设置搜索）。不改变各页面自身布局。

明确的设计选择（已确认）：

| 维度 | 选择 |
|---|---|
| 交互模型 | 跳转导航式（结果列表 + 跳转高亮） |
| 覆盖范围 | **全部设置项**（含 General / Hotkey 等手写页） |
| 匹配深度 | 标题 + 描述(hint) + 选项标签 |
| 匹配算法 | 子串匹配，大小写不敏感；**不做拼音/模糊** |
| 索引架构 | 方案 A：每页搜索清单（schema 派生 + 手写补充） |
| 收集机制 | 编译期 `import.meta.glob`（Vite 构建时静态收集） |
| 护栏 | vitest 一致性校验 |

## 2. 背景：为什么需要"每页清单 + 编译期收集"

当前设置项是**混合架构**，并非单一数据源：

- **schema 驱动**（`schemas/*.schema.ts`）：input ~48 / appearance ~44 / engine ~92 / advanced ~6 字段，每项自带 `{ key, label, hint, options }`，是现成可索引语料。
- **页面手写控件**：General、Hotkey 整页手写；Input / Appearance / Advanced 含手写卡片。标签文字只存在于 `.vue` 模板。

要"全覆盖 + 可索引选项"，运行时扫描 DOM 不可行（下拉选项收起时不在 DOM、手写标记参差）。因此采用**静态声明的每页清单**：schema 部分自动派生，手写部分手工补少量条目，编译期用 glob 聚合成单一索引数组。

## 3. 总体架构

```
编译期 (Vite 构建)
  schemas/*.schema.ts ──┐
                        ├─ pages/*.search.ts (每页清单) ──glob──▶ searchIndex (静态数组)
  手写卡片元数据 ───────┘   import.meta.glob('./pages/*.search.ts', { eager: true })

运行时
  用户输入 ─▶ filterEntries(index, query) ─▶ 结果下拉 ─▶ 点击/Enter ─▶ jumpTo(entry)
                                                          切 activeTab + nextTick + 滚动锚点 + 闪烁高亮
```

设计要点：

- **锚点天然来自 schema 的 `key`**：在 `FieldRenderer` 的 `.setting-item` 根元素加一处 `:data-search-anchor="field.key"`，所有 schema 字段的跳转锚点一次性覆盖，无需逐个标注。
- 手写控件在其行根上手工加 `data-search-anchor="<id>"`，与清单条目 `anchor` 对齐。
- 页面用 `v-show` 渲染（始终挂载），`querySelector` 始终可取到元素；切 tab 后 `nextTick` 再 `scrollIntoView`。

## 4. 数据结构与纯函数

文件：`schemas/searchEntry.ts`

```ts
export interface SearchEntry {
  id: string          // 唯一标识，如 "input.enter_behavior"
  tab: string         // 标签页 id，如 "input"
  tabLabel: string    // 标签页中文名，如 "输入"
  card: string        // 所属卡片，结果面包屑用，如 "按键行为"
  title: string       // 设置项标题
  hint?: string       // 描述文字
  options?: string[]  // 选项标签，如 ["上屏编码","清空编码"]
  anchor: string      // = data-search-anchor 值（通常等于 id）
  keywords?: string[] // 可选同义词，增强召回
}

// schema 片段 → 条目：跟踪 card 标记、读取 select.options、anchor 取 field.key
export function schemaToEntries(
  schema: PageSchema,
  ctx: { tab: string; tabLabel: string; card: string },
): SearchEntry[]

// 子串匹配，大小写不敏感；haystack = [title, hint, ...options, ...keywords]
export function filterEntries(index: SearchEntry[], query: string): SearchEntry[]
```

`schemaToEntries` 行为约定：

- 仅对叶子字段（toggle / select / slider / number-input）产出条目；`card` / `section` 标记不产条目。
- `select` 字段把 `options[].label` 收进 `options`。
- `anchor` 与 `id` 默认取字段 `key`。
- 跳过 `hidden` 恒为真的静态隐藏项可作为 v2 优化；v1 不依赖运行时 config，全部产出。

## 5. 每页清单与编译期收集

每页一个 `pages/<page>.search.ts`，导出 `entries: SearchEntry[]`。示例：

```ts
// pages/input.search.ts
import { schemaToEntries, type SearchEntry } from '@/schemas/searchEntry'
import { punctSchema, keyBehaviorSchema /* … */ } from '@/schemas/input.schema'

export const entries: SearchEntry[] = [
  ...schemaToEntries(punctSchema,       { tab: 'input', tabLabel: '输入', card: '字符与标点' }),
  ...schemaToEntries(keyBehaviorSchema, { tab: 'input', tabLabel: '输入', card: '按键行为' }),
  // 手写卡片（简入繁出 / 标点配对等）手工列条目：
  { id: 'input.trad.enabled', tab: 'input', tabLabel: '输入', card: '简入繁出',
    title: '启用简入繁出', hint: '…', anchor: 'input.trad.enabled' },
]
```

收集器：

```ts
// searchIndex.ts
import type { SearchEntry } from '@/schemas/searchEntry'
const modules = import.meta.glob('./pages/*.search.ts', { eager: true })
export const searchIndex: SearchEntry[] =
  Object.values(modules).flatMap((m: any) => m.entries as SearchEntry[])
```

> Vite 在构建时静态解析 glob，将所有清单打入包。新增一页清单即自动并入，无需改中央注册。

## 6. 组件与改动清单

| 文件 | 动作 | 说明 |
|---|---|---|
| `schemas/searchEntry.ts` | 新建 | 类型 + `schemaToEntries` + `filterEntries` 纯函数 |
| `searchIndex.ts` | 新建 | `import.meta.glob` 收集各页清单并 flatMap |
| `pages/*.search.ts` | 新建 ×8 | 每页搜索清单（schema 派生 + 手写补充） |
| `components/SettingsSearch.vue` | 新建 | 搜索输入框 + 结果下拉 + 键盘导航 |
| `composables/useSettingsSearch.ts` | 新建 | `jumpTo(entry)`：切 tab + 滚动 + 高亮 |
| `components/FieldRenderer.vue` | 改 | `.setting-item` 加 `:data-search-anchor="field.key"` |
| 手写页面模板（General/Hotkey/部分卡片） | 改 | 手写设置行加 `data-search-anchor` |
| `App.vue` | 改 | 侧栏 logo 与 nav 之间放 `SettingsSearch`，接 `jumpTo` |

> 注：新增导出文件 / 改动目录文件结构后，需同步更新对应目录的 `AGENTS.md`（`schemas/`、`components/`、`composables/`、`pages/`），符合项目约定。

## 7. UI 与交互

- **搜索框位置**：侧栏 `logo` 与 `nav` 之间，输入框 "🔍 搜索设置…"；可选 `Ctrl+F` 聚焦。
- **结果下拉**：覆盖式面板，每条显示：
  - 设置项 `标题`（命中子串高亮）；
  - 面包屑 `tabLabel › card`（如 `输入 › 按键行为`）；
  - 命中发生在 hint / 选项时，附一行摘要说明命中处。
- **键盘**：↑↓ 移动选中、Enter 跳转、Esc 关闭并清空。
- **跳转 `jumpTo(entry)`**：
  1. `activeTab.value = entry.tab`
  2. `await nextTick()`
  3. `contentRef.querySelector('[data-search-anchor="' + entry.anchor + '"]')`
  4. `el.scrollIntoView({ block: 'center', behavior: 'smooth' })`
  5. `el.classList.add('search-flash')`，约 1.5s 后移除。

## 8. 边界与错误处理

- **无匹配**：结果区显示"未找到匹配设置"。
- **空查询**：隐藏结果下拉。
- **锚点不存在**（条目对应字段被 `hidden(cfg)` 条件隐藏，未渲染进 DOM）：切 tab 后尽力滚动，找不到元素**不报错**（v1 best-effort，不做高亮）。
- **id 冲突**：由编译期/测试期护栏拦截（见 §9），开发阶段即暴露。

## 9. 测试（vitest 护栏）

- `schemas/searchEntry.test.ts`
  - `filterEntries`：子串命中、大小写不敏感、分别命中 title / hint / options / keywords。
  - `schemaToEntries`：正确读取 `card` 上下文、收集 select 选项、anchor=key、跳过 card/section 标记。
- `searchIndex.test.ts`
  - **`id` 全局唯一**；
  - 每条 `tab` 属于已知标签集合、`title` 非空、`anchor` 非空；
  - （拉伸项）挂载各页组件，校验 schema 派生条目的 `data-search-anchor` 在 DOM 中真实存在。

## 10. 非目标（YAGNI）

- 不做拼音 / 首字母 / 模糊匹配（v1 子串足够）。
- 不做搜索历史、热门搜索、最近访问。
- 不把 General / Hotkey 手写页迁入 schema（仅补搜索清单；schema 迁移是独立的后续工作）。
- 不做运行时按 config 过滤隐藏项（v2 可选）。

## 11. 数据流小结

```
build:  schema 数组 + 手写清单 ──(glob, 静态)──▶ searchIndex
type:   全部条目经 TS 类型检查
test:   id 唯一 + 形状 + 锚点存在性 校验
run:    query ──filterEntries──▶ results ──click/Enter──▶ jumpTo(tab+scroll+flash)
```

## 12. 实现补记（增量决策）

### R1 卡片名可搜

`filterEntries` 的 haystack 构造函数已纳入 `card` 字段，即面包屑卡片名也参与匹配。例如搜索"按键行为"可直接命中该卡片下所有条目。`tab` / `tabLabel` 仍刻意排除在 haystack 之外——它们过于宽泛，纳入会使搜索"输入"时命中整个输入页的所有条目，产生噪声。

### R2 对话框层

设计稿的 `SearchEntry` 仅覆盖普通配置项跳转。实现时为支持"方案专属设置"弹窗入口和"词库导入/导出"弹窗入口，作出以下扩展：

- **`SearchEntry` 新增字段** `openDialog?: "schemaSettingsPinyin" | "schemaSettingsCodetable" | "importDict" | "exportDict"`：标记该条目为 dialog 入口，命中后不做 DOM 滚动，而是切 tab 后通过回调打开对应弹窗。
- **护栏放宽**：原约定"anchor 必须非空"，放宽为"anchor 或 openDialog 至少一个非空"，dialog 入口的 `anchor` 可留空字符串。
- **`jumpTo` 对 dialog entry 的行为**：切换到对应 tab → `await nextTick()` → 调用对应回调（`onOpenSchemaSettings` / `onOpenImportExport("import"|"export")`）→ 返回 `true`；不执行 `scrollIntoView` 和闪烁高亮。
- **通信方式**：`App.vue` 将控制弹窗的方法作为回调传入 `useSettingsSearch`，采用"子页面 `defineExpose` + `App.vue` 通过 ref 调用"模式（与 `SettingsSearch.vue` 的 `focus` 暴露一致）。
- **engineSchema 入口级纳入**：`engine.schema.ts` 中方案专属设置字段不逐条索引（字段使用 `EngineSchema` 专属类型，无法直接用 `schemaToEntries`；且字段数量多、跨引擎类型重叠），仅以 `general.search.ts` 中两条引擎入口条目（`schemaSettingsPinyin`/`schemaSettingsCodetable`）覆盖，搜索命中后按引擎类型打开主拼音/主码表方案的 `SchemaSettingsDialog`（主方案未配置时降级当前活跃方案）。该做法符合原设计"排除 engineSchema 字段级索引"的精神。

### 已知限制（v1 best-effort）

1. **条件隐藏项锚点缺失**：被 `dependsOn`/`v-if` 条件隐藏的设置项（例如"状态提示"未启用时其子项 `ui.status_indicator.show_items`）对应的 DOM 节点不存在，`jumpTo` 调用 `querySelector` 取不到元素时静默返回 `false`，不报错、不高亮。v2 可考虑在 `jumpTo` 前检测条件并提示用户先开启父开关。
2. **无活跃主方案时 dialog 不打开**：`onOpenSchemaSettings` 回调内部依赖 `formData.schema.active` 获取当前主方案 ID，若该值为空则静默不打开弹窗。正常路径下 `formData` 加载后该字段必有值，此为防御性兜底。

### 覆盖范围补记

Dictionary 页整页为列表/动作/导航 UI，无普通配置项，不适合产出 schema 条目。实现时该页仅以两条 dialog 入口条目（导入/导出词库）纳入搜索，满足"词库操作可被搜索到"的需求，同时避免将非配置 UI 混入设置搜索结果。
