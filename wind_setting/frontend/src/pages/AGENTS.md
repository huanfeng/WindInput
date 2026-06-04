<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-06-03 -->

# pages

## Purpose
各设置页面的 Vue 3 单文件组件（SFC）。每个页面对应侧边栏一个标签项，由 `App.vue` 通过 `v-show` 控制显隐（组件始终挂载，不销毁）。页面组件接收 `formData`（全局配置对象引用）作为 prop，直接修改其属性；保存操作由 `App.vue` 统一触发。

## Key Files
| 文件 | 标签 | 说明 |
|------|------|------|
| `GeneralPage.vue` | 方案 | 引擎类型切换（五笔/拼音）、启动状态默认值（中文模式、全角、中文标点） |
| `InputPage.vue` | 输入 | 引擎输入行为：五笔（四码自动上屏、顶字等）、拼音（模糊音设置）、过滤模式、标点跟随模式 |
| `HotkeyPage.vue` | 按键 | 中英文切换键、引擎切换、全角切换、标点切换、候选选择键组、翻页键；负责检测快捷键冲突并 emit `update:hotkeyConflicts` |
| `AppearancePage.vue` | 外观 | 主题选择（含实时预览 ThemePreview）、字体、候选页大小、候选排列（横/竖）、状态指示器、工具栏卡片（Win）/菜单栏指示器卡片（macOS，`isMac` 时渲染 `indicatorSchema`，复用 `toolbar.visible`） |
| `DictionaryPage.vue` | 词库 | 短语管理（用户短语 + 系统短语覆盖）、用户词库管理（按方案）、临时词库管理、Shadow 候选调整（按方案）；直接调用 `wailsApi`，不通过 `formData` |
| `AdvancedPage.vue` | 高级 | 日志级别配置、TSF 日志配置、打开日志目录（emit `openLogFolder`）、打开配置目录（emit `openConfigFolder`）、服务状态查看 |
| `AboutPage.vue` | 关于 | 应用版本、服务运行状态、GitHub 链接（emit `openExternalLink`） |
| `AddWordPage.vue` | — | 快捷加词对话框：以模态浮层形式呈现，支持填入词语、编码、方案、权重并调用 `addUserWordForSchema`；通过 `useToast()` 显示操作结果；由 `App.vue` 控制显隐 |

## For AI Agents
### Working In This Directory
- **页面组件不调用保存**：配置类页面（General、Input、Hotkey、Appearance、Advanced）只修改 `formData` prop，由 `App.vue` 的"保存设置"按钮统一提交
- **词库页面例外**：`DictionaryPage.vue` 直接调用 `wailsApi`（增删短语/词条/Shadow 规则），因为词库操作是独立的增量写入，不走 `formData` 流程
- 新增页面步骤：创建 `XxxPage.vue` -> 在 `App.vue` 中 import 并注册 `tabs` 条目 -> 添加 `<XxxPage v-show="activeTab === 'xxx'" ... />` 绑定
- Props 接收 `formData` 时使用 `defineProps<{ formData: Config }>()` 并标注类型
- `HotkeyPage` 使用 `defineEmits(['update:hotkeyConflicts'])` 向父组件上报冲突
- **Toast 通知**：页面组件（如 `DictionaryPage`、`AddWordPage`）调用 `useToast()` 获取 `toast()` 函数，不再使用页面内嵌提示条

### DictionaryPage.vue 详细说明（2026-04-01 更新）
`DictionaryPage.vue` 是最复杂的页面，结构如下：
- **Props**：`{ isWailsEnv: boolean }`（不接收 `formData`）
- **子标签页**：`phrases`（用户短语 + 系统短语覆盖）、`userdict`（用户词库）、`temp`（临时词库）、`shadow`（候选调整）
- **按方案操作**：用户词库、临时词库、Shadow 均按方案 ID 操作，调用 `*ForSchema` 系列函数；通过 `getEnabledSchemasWithDictStats()` 获取已启用方案列表，并在各子标签页顶部展示方案切换面板
- **短语管理**：分为用户短语（CRUD）和系统短语（只读展示 + 覆盖/恢复操作），支持编辑对话框（内联）
- **临时词库**：展示输入法自动学习的临时词（`TempWordItem`），支持提升到用户词库（`promoteTempWordForSchema`）、批量提升（`promoteAllTempWordsForSchema`）、删除（`removeTempWordForSchema`）、清空（`clearTempDictForSchema`）
- **Shadow 操作**（使用 pin + delete 架构，按方案）：
  - 通过内联对话框进行新增/编辑，支持 `pin`（固定位置）和 `delete`（隐藏）两种操作
  - 编辑时先调用 `removeShadowRuleForSchema`，再写入新规则
- **文件变化检测**：`checkFileChanges()` 调用 `checkAllFilesModified()`，检测到变化时显示警告条（`showFileChangeAlert`）
- **统计显示**：在子标签页标题中实时显示各方案的词条数量（`totalWordCount`、`totalTempCount`、`totalShadowCount`、`phraseCount`）

### Testing Requirements
- `pnpm run build`（TypeScript 类型检查覆盖所有页面）
- 在 Wails 环境中逐一验证每个页面的表单交互和数据持久化
- `DictionaryPage.vue` 需要在有真实词库文件的环境中测试 CRUD 操作，尤其是 Shadow pin/delete/remove 三种操作

### Common Patterns
```vue
<!-- 配置类页面标准模式 -->
<script setup lang="ts">
import type { Config } from '../api/settings'
const props = defineProps<{ formData: Config }>()
// 直接修改：props.formData.engine.type = 'pinyin'
</script>

<!-- 词库页面：直接调用 wailsApi，使用 Toast -->
<script setup lang="ts">
import * as wailsApi from '../api/wails'
import { useToast } from '../composables/useToast'
const { toast } = useToast()
async function addWord() {
  try {
    await wailsApi.addUserWordForSchema(schemaID, code, text, weight)
    toast('添加成功')
    await loadDictData()  // 刷新列表
  } catch (e: any) {
    toast(`添加失败: ${e.message || e}`, 'error')
  }
}
// Shadow 操作（pin + delete 架构，按方案）
async function handleSaveShadowRule() {
  if (editing) {
    await wailsApi.removeShadowRuleForSchema(schemaID, code, word)  // 先移除旧规则
  }
  if (action === 'pin') {
    await wailsApi.pinShadowWordForSchema(schemaID, code, word, position)
  } else {
    await wailsApi.deleteShadowWordForSchema(schemaID, code, word)
  }
}
</script>

<!-- AddWordPage：模态浮层，接收初始值，成功后清空输入继续加词 -->
<AddWordPage
  v-if="showAddWordDialog"
  :initialText="addWordParams?.text"
  :initialCode="addWordParams?.code"
  :initialSchema="addWordParams?.schema_id"
  :standalone="isStandaloneAddWord"
  @close="handleAddWordClose"
/>
```

## 搜索清单（*.search.ts）

每个页面对应一个 `<page>.search.ts`，导出 `entries: SearchEntry[]`，由 `src/searchIndex.ts` 通过 `import.meta.glob('./pages/*.search.ts', { eager: true })` 编译期自动收集，无需手动注册。

### 现有清单文件
| 文件 | 对应标签页 | 说明 |
|------|-----------|------|
| `general.search.ts` | 方案 | 手写条目（主方案选择）+ 拼音/码表两条引擎入口条目（`openDialog: "schemaSettingsPinyin"`/`"schemaSettingsCodetable"`，按引擎路由到主方案）+ 方案管理动作入口 |
| `input.search.ts` | 输入 | `schemaToEntries` 派生的 schema 条目 + 手写卡片（简入繁出等）补充条目 |
| `hotkey.search.ts` | 按键 | 整页手写条目（HotkeyPage 全手写，无 schema） |
| `appearance.search.ts` | 外观 | `schemaToEntries` 派生 + 手写卡片补充条目 |
| `advanced.search.ts` | 高级 | `schemaToEntries` 派生 |
| `dictionary.search.ts` | 词库 | 仅 dialog 入口条目（`openDialog: "importDict"` / `"exportDict"`）；DictionaryPage 整页为列表/动作 UI，无普通配置项 |

### 约定
- schema 字段由 `schemaToEntries(schema, ctx)` 派生，`anchor` 默认等于字段 `key`，与 `FieldRenderer` 的 `data-search-anchor` 对齐。
- 手写控件条目需在 `.vue` 模板对应行根上手工加 `data-search-anchor="<anchor>"` 才能支持跳转高亮。
- `openDialog` 字段（`"schemaSettingsPinyin"` / `"schemaSettingsCodetable"` / `"importDict"` / `"exportDict"`）用于 dialog 入口条目：`anchor` 可留空，`jumpTo` 切 tab 后通过回调打开弹窗，不滚动到字段。方案专属设置按引擎类型路由到主拼音/主码表方案。
- 护栏：`id` 全局唯一（`searchIndex.test.ts` 校验）；每条 `title` / `anchor` 至少有一个非空（`anchor` 对 dialog 入口可为空，但 `openDialog` 必须非空作为替代）。
- 新增页面搜索清单：创建 `<page>.search.ts` 并导出 `entries: SearchEntry[]`，Vite glob 自动收集，无需改 `searchIndex.ts`。

## Dependencies
### Internal
- `../api/wails` — Wails IPC API（DictionaryPage 直接使用：Shadow pin/delete/remove、短语、用户词库；AppearancePage 通过父组件 props 传入 theme 数据）
- `../api/settings` — Config 类型定义
- `../schemas/searchEntry` — `SearchEntry` 类型（各 `*.search.ts` 引用）
- `../schemas/*.schema.ts` — 各页 schema 实例（由 `*.search.ts` 调用 `schemaToEntries` 时引用）

### External
- Vue 3（`ref`、`computed`、`defineProps`、`defineEmits`、`onMounted`、`watch`）

<!-- MANUAL: -->
