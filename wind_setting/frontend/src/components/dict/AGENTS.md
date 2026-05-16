<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-05-16 -->

# dict

## Purpose

词库管理相关的可复用 Vue 3 组件目录。由 `DictionaryPage.vue` 使用，负责展示、编辑和管理各类词库数据（短语、用户词库、临时词库、Shadow 候选调整）。基于 TanStack Vue Table 构建高效表格，支持搜索、排序、分页、多选等操作。

## Key Files

| 文件                        | 说明                                                                                                                                                                                                                                                                                     |
| --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `DictDataTable.vue`         | 通用数据表格组件（通用、可复用）：基于 TanStack Vue Table，支持搜索、排序、分页、行选择、loading 状态、自定义工具栏插槽                                                                                                                                                                  |
| `DictTypeSelector.vue`      | 词库类型选择器：用于在「短语」和「用户词库」等多个数据源间切换（已集成在 DictionaryPage 的标签页切换中）                                                                                                                                                                                 |
| `FreqPanel.vue`             | 词频面板：展示临时词库和用户词库的频率统计，支持查看和调整词频权重                                                                                                                                                                                                                       |
| `PhrasePanel.vue`           | 短语管理面板：用户/系统短语统一列表 (2026-05-16 schema 简化后)；支持添加/编辑/删除/启用切换、批量删除、恢复默认、导入导出；列：启用 / 编码 / 内容 / 权重 / 操作                                                                                                                          |
| `ShadowPanel.vue`           | Shadow 候选调整面板：按方案管理 pin（固定位置）和 delete（隐藏）规则；支持编辑对话框（新增/修改）、列表操作、方案切换                                                                                                                                                                    |
| `TempDictPanel.vue`         | 临时词库面板：展示输入法学习的临时词条；支持提升到用户词库、批量提升、删除、清空操作                                                                                                                                                                                                     |
| `UserDictPanel.vue`         | 用户词库面板：按方案管理用户输入的词条；支持添加/编辑/删除对话框、分页搜索、方案切换、导入导出                                                                                                                                                                                           |
| `ImportExportDialog.vue`    | 导入导出对话框：支持选择文件、设置导入模式（覆盖/合并）、显示进度和结果统计、取消操作                                                                                                                                                                                                    |
| `CmdbarValuePreview.vue`    | 短语 value 实时校验预览（单行 24px）：调用 `validatePhraseValue` RPC 按 `kind` 渲染 command/command-prefix/array/template/error 不同 badge（单行 truncate，error 完整内容通过原生 title 显示）；emit `validation-error` 供父组件禁用保存按钮                                             |
| `PhraseFormBody.vue`        | 短语 / 用户词库 共享表单体：承担分类(EditorType) + 编码(可选生成按钮) + 内容子编辑器 + 单行预览 + 权重；通过 v-model 接收 `PhraseFormState`；emit `composed-text` 与 `validation-error`；可由 `showCodeGen` / `onGenerateCode` / `codeGenerating` / `codeLabel` / `codePlaceholder` 定制 |
| `HintTooltip.vue`           | 通用提示 tooltip：? 图标 hover/focus 触发, 气泡 Teleport 到 body, fixed 定位避免被对话框 overflow 裁剪; 支持简单 text 单段或 head + rows 多行档位两种渲染模式                                                                                                                            |
| `editors/NormalEditor.vue`  | 「普通」子编辑器：单 textarea, 直接编辑字面量/模板                                                                                                                                                                                                                                       |
| `editors/CmdOpenEditor.vue` | 「命令·打开」子编辑器：结构化输入单 action 的 `$CC` / `$CC1`(URL / 程序 / 文件), 调用 `pickExePath` / `pickAnyPath` 打开原生文件选择对话框                                                                                                                                               |
| `editors/CmdRawEditor.vue`  | 「命令·手动」子编辑器：textarea 直接书写 `$CC(...)` 多 action 串联表达式                                                                                                                                                                                                                 |
| `editors/ArrayEditor.vue`   | 「字符组」子编辑器：结构化输入 `$AA(name, chars)`                                                                                                                                                                                                                                        |
| `editors/ArraySSEditor.vue` | 「字符串组」子编辑器 (2026-05-16)：结构化输入 `$SS(name, elem, ...)`, 每个 element 可以是字符串字面量或 `$CC(...)` 命令 (CmdOpen 子集)                                                                                                                                                  |

## For AI Agents

### Working In This Directory

- **DictDataTable** 是所有面板的通用表格基础，通过 generic 支持任意数据类型，由父组件定义列、数据、搜索、分页等逻辑
- **面板组件**（Phrase/UserDict/Temp/Shadow）由 `DictionaryPage.vue` 通过子标签页切换使用，接收方案 ID 作为 prop
- 所有面板通过 `useToast()` 显示操作结果，支持成功/失败通知
- 所有面板通过 `useConfirm()` 进行删除/重置等危险操作的确认
- **按方案操作**：用户词库、临时词库、Shadow 均需要 schemaId 参数，通过方案选择器动态切换
- **导入导出**：集中在 `ImportExportDialog.vue` 处理，支持 progress 回调和 cancelled 标记

### 各面板详细说明

#### DictDataTable（通用表格）

- 通用设计，支持任意 TData 泛型
- Props：`columns`（列定义）、`data`（行数据）、`loading`、`searchable`、`selectable`、`pageSize`、`rowKey`（行唯一标识）等
- Emits：`update:selection`（行选中状态）、`search`（搜索查询）、`page-change`（分页）
- 支持客户端分页（`pageSize > 0`）和服务端分页（`serverPagination` prop）
- 工具栏支持自定义插槽：`toolbar-start`（左侧按钮）、`toolbar-end`（右侧按钮）
- 表头可排序，行可选中，全部线程安全

#### PhrasePanel（短语管理）

- 2026-05-16 schema 简化后, 用户/系统短语统一为单一 `PhraseItem` 列表 (`code, text, weight, position, enabled, is_system`), 不再区分两栏。系统短语通过 `is_system` 字段标识 (UI 可据此弱化样式), 但 CRUD 行为与用户短语一致
- 支持完整 CRUD (编辑对话框含分类子编辑器 - 普通 / cmd-open / cmd-raw / array / array-ss)
- 支持启用/禁用切换、批量删除、恢复默认
- 支持全量导入导出 (yaml 格式, 单一 text 字段; 兼容旧 texts/name 字段读入)

#### UserDictPanel（用户词库）

- 按方案管理，单个方案词库可能很大（数千条），采用分页
- 支持搜索（前缀搜索），分页加载
- 支持快捷「加词」（AddWordPage 对话框）
- 支持删除、批量清空

#### TempDictPanel（临时词库）

- 临时词库通常较小，一次性加载
- 支持「提升」（转移到用户词库）、「批量提升」、「删除」、「清空」操作
- 自动按频率排序

#### ShadowPanel（候选调整）

- 按方案管理，支持两种操作：pin（固定位置）、delete（隐藏）
- 编辑对话框支持新增和修改（修改时先移除旧规则）
- 支持列表编辑、删除、方案切换

### Testing Requirements

- `pnpm run build`（TypeScript 类型检查）
- 在 Wails 环境中逐一测试各面板的 CRUD 操作
- 特别测试 DictDataTable 的搜索、排序、分页、行选中功能
- 测试导入导出流程（大文件、中断取消等）

### Common Patterns

```vue
<!-- 在 DictionaryPage 中使用面板 -->
<PhrasePanel v-if="activeTab === 'phrases'" @loading="loading = $event" />
<UserDictPanel
  v-if="activeTab === 'userdict'"
  :schema-id="selectedSchemaId"
  :readonly="false"
  @loading="loading = $event"
  ref="userDictPanelRef"
/>

<!-- DictDataTable 使用示例 -->
<DictDataTable
  :columns="columns"
  :data="items"
  :loading="loading"
  :searchable="true"
  :selectable="true"
  :page-size="50"
  :row-key="(item) => `${item.code}|${item.text}`"
  @update:selection="selectedKeys = $event"
  @search="searchQuery = $event"
>
  <template #toolbar-start="{ selectedCount }">
    <Button 
      :disabled="selectedCount === 0"
      @click="handleDeleteSelected"
    >
      删除已选
    </Button>
  </template>
</DictDataTable>

<!-- Shadow pin/delete 操作 -->
async function handleSaveShadowRule() { const { code, text, action, position } =
editingRule // 修改时先移除旧规则 if (editing) { await
wailsApi.removeShadowRuleForSchema(schemaId, code, text) } // 写入新规则 if
(action === 'pin') { await wailsApi.pinShadowWordForSchema(schemaId, code, text,
position) } else { await wailsApi.deleteShadowWordForSchema(schemaId, code,
text) } toast('保存成功') await loadShadowData() // 刷新列表 }
```

## Dependencies

### Internal

- `../../../pages/AddWordPage.vue` — 加词对话框（UserDictPanel 中使用）
- `@/api/wails` — Wails IPC API（所有面板的数据来源）
- `@/composables/useToast` — 操作提示
- `@/composables/useConfirm` — 确认对话框
- `@/components/ui/*` — shadcn UI 组件（Button、Input、Dialog、Checkbox、Switch 等）

### External

- Vue 3（`ref`、`computed`、`watch`、`h`、`onMounted`）
- TanStack Vue Table（`useVueTable`、`ColumnDef` 等）

<!-- MANUAL: -->
