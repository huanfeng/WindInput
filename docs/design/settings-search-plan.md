# 设置搜索功能 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为设置窗口加入跳转导航式搜索：输入关键词 → 结果列表 → 点击跳转到对应标签页并滚动高亮该设置项。

**Architecture:** 编译期用 Vite `import.meta.glob` 收集各页 `*.search.ts` 搜索清单拼成静态索引；schema 驱动字段经纯函数 `schemaToEntries` 自动派生，手写控件手工补条目。运行时 `filterEntries` 子串过滤，`jumpTo` 切 tab + 滚动 + 闪烁高亮。锚点统一用 `data-search-anchor`，schema 字段锚点即其 `key`（一次性在 `FieldRenderer` 注入）。

**Tech Stack:** Vue 3 + TypeScript + Vite + vitest（`environment: 'node'`）。所在目录：`wind_setting/frontend`。

**关键约束（来自设计稿 `settings-search.md` 与项目规则）：**
- 匹配深度：标题 + hint + 选项标签；子串匹配、大小写不敏感；**不做拼音/模糊**。
- 覆盖范围：8 个主标签页的全部设置项；**排除** `engineSchema`（方案专属设置弹窗，作用域不同）。
- 测试只用 node 环境纯函数测试，**不新增** jsdom / @vue/test-utils 依赖。
- 功能未通过测试/构建前不 `git commit`；不 `git push`；提交信息不含 AI trailer / Co-Authored-By。
- 改动目录对外导出/文件结构后，同步更新该目录 `AGENTS.md`。
- 全部命令在 worktree 内执行：`cd wind_setting/frontend`，包管理用 `pnpm`。

---

## 文件结构

| 文件 | 责任 |
|---|---|
| `src/schemas/searchEntry.ts`（新建） | `SearchEntry` 类型 + `schemaToEntries` + `filterEntries` 纯函数 |
| `src/schemas/searchEntry.test.ts`（新建） | 纯函数单测 |
| `src/pages/input.search.ts`（新建） | 输入页搜索清单 |
| `src/pages/appearance.search.ts`（新建） | 外观页搜索清单 |
| `src/pages/advanced.search.ts`（新建） | 高级页搜索清单 |
| `src/pages/hotkey.search.ts`（新建，Task 8） | 按键页手写清单 |
| `src/pages/general.search.ts`（新建，Task 8） | 方案页手写清单 |
| `src/searchIndex.ts`（新建） | `import.meta.glob` 收集器 |
| `src/searchIndex.test.ts`（新建） | 索引一致性护栏（id 唯一/形状/anchor 非空） |
| `src/composables/useSettingsSearch.ts`（新建） | `jumpTo(entry)` 跳转逻辑 |
| `src/composables/useSettingsSearch.test.ts`（新建） | jumpTo 逻辑单测（假 container） |
| `src/components/SettingsSearch.vue`（新建） | 搜索输入框 + 结果下拉 + 键盘导航 |
| `src/components/FieldRenderer.vue`（改） | 4 个 `.setting-item` 根加 `:data-search-anchor="field.key"` |
| `src/App.vue`（改） | 侧栏放 `SettingsSearch`，接 `jumpTo`，加 `.search-flash` 样式 |
| 手写页面模板（Task 8） | 手写设置行加 `data-search-anchor` |

---

## Task 1：SearchEntry 类型 + filterEntries 纯函数

**Files:**
- Create: `src/schemas/searchEntry.ts`
- Test: `src/schemas/searchEntry.test.ts`

- [ ] **Step 1: 写失败测试**

`src/schemas/searchEntry.test.ts`：

```ts
import { describe, it, expect } from "vitest";
import { filterEntries, type SearchEntry } from "./searchEntry";

const sample: SearchEntry[] = [
  { id: "input.enter_behavior", tab: "input", tabLabel: "输入", card: "按键行为",
    title: "回车键功能", hint: "有编码时按回车键的处理方式",
    options: ["上屏编码", "清空编码"], anchor: "input.enter_behavior" },
  { id: "ui.font_size", tab: "appearance", tabLabel: "外观", card: "候选窗口",
    title: "字体大小", anchor: "ui.font_size", keywords: ["fontsize"] },
];

describe("filterEntries", () => {
  it("空查询返回空数组", () => {
    expect(filterEntries(sample, "")).toEqual([]);
    expect(filterEntries(sample, "   ")).toEqual([]);
  });

  it("命中标题子串", () => {
    expect(filterEntries(sample, "回车").map((e) => e.id)).toEqual(["input.enter_behavior"]);
  });

  it("命中 hint", () => {
    expect(filterEntries(sample, "处理方式").map((e) => e.id)).toEqual(["input.enter_behavior"]);
  });

  it("命中选项标签", () => {
    expect(filterEntries(sample, "清空编码").map((e) => e.id)).toEqual(["input.enter_behavior"]);
  });

  it("命中 keywords，且大小写不敏感", () => {
    expect(filterEntries(sample, "FontSize").map((e) => e.id)).toEqual(["ui.font_size"]);
  });

  it("无匹配返回空", () => {
    expect(filterEntries(sample, "不存在的词")).toEqual([]);
  });
});
```

- [ ] **Step 2: 运行确认失败**

Run: `cd wind_setting/frontend && pnpm test`
Expected: FAIL —— 找不到模块 `./searchEntry`。

- [ ] **Step 3: 写最小实现**

`src/schemas/searchEntry.ts`：

```ts
// 设置搜索 —— 条目类型与纯函数（编译期建索引、运行时子串过滤）
import type { PageSchema, FieldDef, SelectField } from "./types";

export interface SearchEntry {
  /** 唯一标识，通常等于设置项 key */
  id: string;
  /** 标签页 id，如 "input" */
  tab: string;
  /** 标签页中文名，如 "输入" */
  tabLabel: string;
  /** 所属卡片，结果面包屑用 */
  card: string;
  /** 设置项标题 */
  title: string;
  /** 描述文字（仅静态字符串 hint） */
  hint?: string;
  /** 选项标签（select 字段） */
  options?: string[];
  /** data-search-anchor 值，通常等于 id */
  anchor: string;
  /** 可选同义词，增强召回 */
  keywords?: string[];
}

export interface SchemaEntryCtx {
  tab: string;
  tabLabel: string;
  card: string;
}

/** 构造小写检索串：title + hint + options + keywords */
function haystack(e: SearchEntry): string {
  return [e.title, e.hint ?? "", ...(e.options ?? []), ...(e.keywords ?? [])]
    .join("")
    .toLowerCase();
}

/** 子串匹配，大小写不敏感；空查询返回空数组 */
export function filterEntries(index: SearchEntry[], query: string): SearchEntry[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  return index.filter((e) => haystack(e).includes(q));
}
```

> 注：`schemaToEntries` 在 Task 2 追加到本文件；此处导入的 `PageSchema/FieldDef/SelectField` 供 Task 2 使用，先一并写入避免二次改动 import。

- [ ] **Step 4: 运行确认通过**

Run: `cd wind_setting/frontend && pnpm test`
Expected: PASS（含原有 9 个 + 新增 6 个）。

- [ ] **Step 5: 提交**

```bash
git add src/schemas/searchEntry.ts src/schemas/searchEntry.test.ts
git commit -m "feat(setting): 新增设置搜索条目类型与 filterEntries 纯函数"
```

---

## Task 2：schemaToEntries 纯函数

**Files:**
- Modify: `src/schemas/searchEntry.ts`
- Test: `src/schemas/searchEntry.test.ts`

- [ ] **Step 1: 追加失败测试**

在 `src/schemas/searchEntry.test.ts` 末尾追加：

```ts
import { schemaToEntries } from "./searchEntry";
import type { PageSchema } from "./types";

const frag: PageSchema = [
  { type: "section", label: "分组标题" },
  { type: "toggle", key: "input.punct_follow_mode", label: "标点随中英文切换",
    hint: "切换到中文模式时自动切换中文标点" },
  { type: "select", key: "input.enter_behavior", label: "回车键功能",
    hint: "有编码时按回车键的处理方式",
    options: [
      { value: "commit", label: "上屏编码" },
      { value: "clear", label: "清空编码" },
    ] },
];

describe("schemaToEntries", () => {
  const ctx = { tab: "input", tabLabel: "输入", card: "按键行为" };

  it("跳过 card/section 标记，只产出叶子字段", () => {
    const out = schemaToEntries(frag, ctx);
    expect(out.map((e) => e.id)).toEqual(["input.punct_follow_mode", "input.enter_behavior"]);
  });

  it("anchor/id 取 key，注入 ctx 上下文", () => {
    const [first] = schemaToEntries(frag, ctx);
    expect(first).toMatchObject({
      id: "input.punct_follow_mode", anchor: "input.punct_follow_mode",
      tab: "input", tabLabel: "输入", card: "按键行为",
      title: "标点随中英文切换", hint: "切换到中文模式时自动切换中文标点",
    });
  });

  it("select 字段收集选项标签", () => {
    const sel = schemaToEntries(frag, ctx).find((e) => e.id === "input.enter_behavior");
    expect(sel?.options).toEqual(["上屏编码", "清空编码"]);
  });

  it("函数式 hint 不写入（仅保留静态字符串）", () => {
    const dyn: PageSchema = [
      { type: "toggle", key: "x.y", label: "L", hint: () => "动态" } as any,
    ];
    expect(schemaToEntries(dyn, ctx)[0].hint).toBeUndefined();
  });
});
```

- [ ] **Step 2: 运行确认失败**

Run: `cd wind_setting/frontend && pnpm test`
Expected: FAIL —— `schemaToEntries` 未导出。

- [ ] **Step 3: 实现 schemaToEntries**

在 `src/schemas/searchEntry.ts` 末尾追加：

```ts
type LeafField = Exclude<FieldDef, { type: "card" } | { type: "section" }>;

/** schema 片段（纯字段，无 card 标记）→ 搜索条目。card 由 ctx 提供 */
export function schemaToEntries(schema: PageSchema, ctx: SchemaEntryCtx): SearchEntry[] {
  const out: SearchEntry[] = [];
  for (const field of schema) {
    if (field.type === "card" || field.type === "section") continue;
    const f = field as LeafField;
    const entry: SearchEntry = {
      id: f.key,
      tab: ctx.tab,
      tabLabel: ctx.tabLabel,
      card: ctx.card,
      title: f.label,
      anchor: f.key,
    };
    if (typeof f.hint === "string") entry.hint = f.hint;
    if (f.type === "select") entry.options = (f as SelectField).options.map((o) => o.label);
    out.push(entry);
  }
  return out;
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cd wind_setting/frontend && pnpm test`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/schemas/searchEntry.ts src/schemas/searchEntry.test.ts
git commit -m "feat(setting): 新增 schemaToEntries 派生搜索条目"
```

---

## Task 3：各页 schema 清单 + glob 收集器 + 索引护栏

**Files:**
- Create: `src/pages/input.search.ts`、`src/pages/appearance.search.ts`、`src/pages/advanced.search.ts`
- Create: `src/searchIndex.ts`
- Test: `src/searchIndex.test.ts`

- [ ] **Step 1: 写三份 schema 清单**

`src/pages/input.search.ts`：

```ts
import { schemaToEntries, type SearchEntry } from "@/schemas/searchEntry";
import {
  punctSchema, keyBehaviorSchema, overflowSchema,
  quickInputExtraSchema, pinyinSeparatorSchema, shiftExtraSchema, startupExtraSchema,
} from "@/schemas/input.schema";

const tab = "input";
const tabLabel = "输入";

export const entries: SearchEntry[] = [
  ...schemaToEntries(punctSchema,           { tab, tabLabel, card: "字符与标点" }),
  ...schemaToEntries(keyBehaviorSchema,     { tab, tabLabel, card: "按键行为" }),
  ...schemaToEntries(overflowSchema,        { tab, tabLabel, card: "候选无效按键" }),
  ...schemaToEntries(quickInputExtraSchema, { tab, tabLabel, card: "快捷输入" }),
  ...schemaToEntries(pinyinSeparatorSchema, { tab, tabLabel, card: "临时拼音" }),
  ...schemaToEntries(shiftExtraSchema,      { tab, tabLabel, card: "临时英文" }),
  ...schemaToEntries(startupExtraSchema,    { tab, tabLabel, card: "默认状态" }),
];
```

`src/pages/appearance.search.ts`：

```ts
import { schemaToEntries, type SearchEntry } from "@/schemas/searchEntry";
import {
  themeExtraSchema, candidateWindowSchema, statusIndicatorSchema,
  candidateTooltipSchema, indicatorSchema, toolbarSchema,
} from "@/schemas/appearance.schema";

const tab = "appearance";
const tabLabel = "外观";

export const entries: SearchEntry[] = [
  ...schemaToEntries(themeExtraSchema,       { tab, tabLabel, card: "主题" }),
  ...schemaToEntries(candidateWindowSchema,  { tab, tabLabel, card: "候选窗口" }),
  ...schemaToEntries(statusIndicatorSchema,  { tab, tabLabel, card: "状态提示" }),
  ...schemaToEntries(candidateTooltipSchema, { tab, tabLabel, card: "候选项提示信息" }),
  ...schemaToEntries(toolbarSchema,          { tab, tabLabel, card: "工具栏" }),
  ...schemaToEntries(indicatorSchema,        { tab, tabLabel, card: "菜单栏指示器" }),
];
```

> 执行前确认 `appearance.schema.ts` 确实导出上述六个 `export const`（已核：themeExtraSchema/candidateWindowSchema/statusIndicatorSchema/candidateTooltipSchema/indicatorSchema/toolbarSchema）。若某个命名不符，以该文件实际 `export const` 名为准。

`src/pages/advanced.search.ts`：

```ts
import { schemaToEntries, type SearchEntry } from "@/schemas/searchEntry";
import { advancedLogSchema, advancedPerfSchema } from "@/schemas/advanced.schema";

const tab = "advanced";
const tabLabel = "高级";

export const entries: SearchEntry[] = [
  ...schemaToEntries(advancedLogSchema,  { tab, tabLabel, card: "日志设置" }),
  ...schemaToEntries(advancedPerfSchema, { tab, tabLabel, card: "性能诊断" }),
];
```

- [ ] **Step 2: 写收集器**

`src/searchIndex.ts`：

```ts
// 编译期收集：Vite 在构建时静态解析 glob，把所有 *.search.ts 清单打入包。
// 新增一页清单（如 hotkey.search.ts）即自动并入，无需改本文件。
import type { SearchEntry } from "@/schemas/searchEntry";

const modules = import.meta.glob<{ entries: SearchEntry[] }>("./pages/*.search.ts", {
  eager: true,
});

export const searchIndex: SearchEntry[] = Object.values(modules).flatMap((m) => m.entries);
```

- [ ] **Step 3: 写索引护栏测试**

`src/searchIndex.test.ts`：

```ts
import { describe, it, expect } from "vitest";
import { searchIndex } from "./searchIndex";
import type { SearchEntry } from "@/schemas/searchEntry";

const KNOWN_TABS = new Set([
  "general", "input", "hotkey", "appearance", "dictionary", "advanced", "stats", "about",
]);

describe("searchIndex 一致性护栏", () => {
  it("非空", () => {
    expect(searchIndex.length).toBeGreaterThan(0);
  });

  it("id 全局唯一", () => {
    const ids = searchIndex.map((e) => e.id);
    const dups = ids.filter((id, i) => ids.indexOf(id) !== i);
    expect(dups).toEqual([]);
  });

  it("每条形状合法：tab 属于已知集合、title/anchor 非空", () => {
    const bad = searchIndex.filter(
      (e: SearchEntry) =>
        !KNOWN_TABS.has(e.tab) || !e.title?.trim() || !e.anchor?.trim() || !e.tabLabel?.trim(),
    );
    expect(bad.map((e) => e.id)).toEqual([]);
  });
});
```

- [ ] **Step 4: 运行确认通过**

Run: `cd wind_setting/frontend && pnpm test`
Expected: PASS。若报某 schema 导出名不存在，按 Step 1 注释以实际导出名修正后重跑。

- [ ] **Step 5: 提交**

```bash
git add src/pages/input.search.ts src/pages/appearance.search.ts src/pages/advanced.search.ts src/searchIndex.ts src/searchIndex.test.ts
git commit -m "feat(setting): 编译期 glob 收集 schema 搜索清单与索引护栏"
```

---

## Task 4：FieldRenderer 注入跳转锚点

**Files:**
- Modify: `src/components/FieldRenderer.vue`（4 个 `.setting-item` 根：toggle/select/slider/number-input）

- [ ] **Step 1: 给每个 `.setting-item` 根加 `data-search-anchor`**

`FieldRenderer.vue` 模板中共有 4 处根元素形如：

```html
  <div
    v-if="field.type === 'toggle'"
    class="setting-item"
    :class="{ 'item-disabled': isDisabled }"
  >
```

将这 4 处（`v-if="field.type === 'toggle'"`、`v-else-if="field.type === 'select'"`、`v-else-if="field.type === 'slider'"`、`v-else-if="field.type === 'number-input'"`）的根 `<div>` 各加一行属性：

```html
    :data-search-anchor="field.key"
```

加在 `class="setting-item"` 之后即可。改完每个根形如：

```html
  <div
    v-if="field.type === 'toggle'"
    class="setting-item"
    :data-search-anchor="field.key"
    :class="{ 'item-disabled': isDisabled }"
  >
```

- [ ] **Step 2: 类型检查/构建确认无误**

Run: `cd wind_setting/frontend && pnpm build`
Expected: 构建通过（`vue-tsc --noEmit` 无错）。

- [ ] **Step 3: 提交**

```bash
git add src/components/FieldRenderer.vue
git commit -m "feat(setting): FieldRenderer 注入 data-search-anchor 跳转锚点"
```

---

## Task 5：useSettingsSearch 跳转 composable

**Files:**
- Create: `src/composables/useSettingsSearch.ts`
- Test: `src/composables/useSettingsSearch.test.ts`

- [ ] **Step 1: 写失败测试（node 环境，假 container 无需 jsdom）**

`src/composables/useSettingsSearch.test.ts`：

```ts
import { describe, it, expect, vi } from "vitest";
import { ref } from "vue";
import { useSettingsSearch } from "./useSettingsSearch";
import type { SearchEntry } from "@/schemas/searchEntry";

function fakeEl() {
  return {
    scrollIntoView: vi.fn(),
    classList: { add: vi.fn(), remove: vi.fn() },
  };
}

const entry: SearchEntry = {
  id: "ui.font_size", tab: "appearance", tabLabel: "外观", card: "候选窗口",
  title: "字体大小", anchor: "ui.font_size",
};

describe("useSettingsSearch.jumpTo", () => {
  it("切换 activeTab，命中元素时滚动并加高亮类，返回 true", async () => {
    const el = fakeEl();
    const activeTab = ref("general");
    const container = ref<any>({ querySelector: vi.fn().mockReturnValue(el) });
    const { jumpTo } = useSettingsSearch({ activeTab, container });

    const ok = await jumpTo(entry);

    expect(activeTab.value).toBe("appearance");
    expect(container.value.querySelector).toHaveBeenCalledWith(
      '[data-search-anchor="ui.font_size"]',
    );
    expect(el.scrollIntoView).toHaveBeenCalled();
    expect(el.classList.add).toHaveBeenCalledWith("search-flash");
    expect(ok).toBe(true);
  });

  it("锚点不存在时仍切 tab，但返回 false 不报错", async () => {
    const activeTab = ref("general");
    const container = ref<any>({ querySelector: vi.fn().mockReturnValue(null) });
    const { jumpTo } = useSettingsSearch({ activeTab, container });

    const ok = await jumpTo(entry);

    expect(activeTab.value).toBe("appearance");
    expect(ok).toBe(false);
  });
});
```

- [ ] **Step 2: 运行确认失败**

Run: `cd wind_setting/frontend && pnpm test`
Expected: FAIL —— 找不到 `./useSettingsSearch`。

- [ ] **Step 3: 实现 composable**

`src/composables/useSettingsSearch.ts`：

```ts
import { nextTick, type Ref } from "vue";
import type { SearchEntry } from "@/schemas/searchEntry";

interface UseSettingsSearchOptions {
  /** 当前激活标签页 */
  activeTab: Ref<string>;
  /** 内容滚动容器（App.vue 的 contentRef） */
  container: Ref<HTMLElement | null>;
}

const FLASH_CLASS = "search-flash";
const FLASH_MS = 1500;

export function useSettingsSearch(opts: UseSettingsSearchOptions) {
  /** 跳转到某设置项：切 tab → 等渲染 → 滚动 → 闪烁高亮。返回是否命中锚点 */
  async function jumpTo(entry: SearchEntry): Promise<boolean> {
    opts.activeTab.value = entry.tab;
    await nextTick();
    const el = opts.container.value?.querySelector<HTMLElement>(
      `[data-search-anchor="${entry.anchor}"]`,
    );
    if (!el) return false;
    el.scrollIntoView({ block: "center", behavior: "smooth" });
    el.classList.add(FLASH_CLASS);
    window.setTimeout(() => el.classList.remove(FLASH_CLASS), FLASH_MS);
    return true;
  }

  return { jumpTo };
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cd wind_setting/frontend && pnpm test`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/composables/useSettingsSearch.ts src/composables/useSettingsSearch.test.ts
git commit -m "feat(setting): 新增 useSettingsSearch 跳转高亮 composable"
```

---

## Task 6：SettingsSearch 组件

**Files:**
- Create: `src/components/SettingsSearch.vue`

> 该组件靠 `pnpm build`（vue-tsc 类型检查）+ Task 7 集成后手动运行验证，不做 jsdom 单测。

- [ ] **Step 1: 实现组件**

`src/components/SettingsSearch.vue`：

```vue
<script setup lang="ts">
import { ref, computed, nextTick } from "vue";
import { searchIndex } from "@/searchIndex";
import { filterEntries, type SearchEntry } from "@/schemas/searchEntry";

const emit = defineEmits<{ (e: "jump", entry: SearchEntry): void }>();

const query = ref("");
const open = ref(false);
const activeIndex = ref(0);
const inputRef = ref<HTMLInputElement | null>(null);

const results = computed(() => filterEntries(searchIndex, query.value).slice(0, 30));

function onInput() {
  open.value = query.value.trim().length > 0;
  activeIndex.value = 0;
}

function choose(entry: SearchEntry) {
  emit("jump", entry);
  open.value = false;
}

function move(delta: number) {
  if (!results.value.length) return;
  const n = results.value.length;
  activeIndex.value = (activeIndex.value + delta + n) % n;
}

function onEnter() {
  const e = results.value[activeIndex.value];
  if (e) choose(e);
}

function onEsc() {
  if (open.value) {
    open.value = false;
  } else {
    query.value = "";
  }
}

function focus() {
  inputRef.value?.focus();
}

defineExpose({ focus });

// 失焦延迟关闭，避免点击结果项前下拉先消失
function onBlur() {
  window.setTimeout(() => (open.value = false), 120);
}
</script>

<template>
  <div class="settings-search">
    <input
      ref="inputRef"
      v-model="query"
      class="search-input"
      type="text"
      placeholder="🔍 搜索设置…"
      @input="onInput"
      @focus="onInput"
      @blur="onBlur"
      @keydown.down.prevent="move(1)"
      @keydown.up.prevent="move(-1)"
      @keydown.enter.prevent="onEnter"
      @keydown.esc.prevent="onEsc"
    />

    <div v-if="open" class="search-results">
      <div v-if="!results.length" class="search-empty">未找到匹配设置</div>
      <button
        v-for="(entry, i) in results"
        :key="entry.id"
        class="search-result-item"
        :class="{ active: i === activeIndex }"
        @mousedown.prevent="choose(entry)"
        @mouseenter="activeIndex = i"
      >
        <span class="result-title">{{ entry.title }}</span>
        <span class="result-crumb">{{ entry.tabLabel }} › {{ entry.card }}</span>
      </button>
    </div>
  </div>
</template>

<style scoped>
.settings-search {
  position: relative;
  padding: 8px 12px;
}
.search-input {
  width: 100%;
  box-sizing: border-box;
  padding: 6px 10px;
  font-size: 13px;
  border: 1px solid hsl(var(--border, 0 0% 85%));
  border-radius: 6px;
  background: var(--bg-card, #fff);
  color: inherit;
  outline: none;
}
.search-input:focus {
  border-color: hsl(var(--primary, 220 90% 56%));
}
.search-results {
  position: absolute;
  left: 12px;
  right: 12px;
  top: calc(100% - 2px);
  z-index: 50;
  max-height: 320px;
  overflow-y: auto;
  background: var(--bg-card, #fff);
  border: 1px solid hsl(var(--border, 0 0% 85%));
  border-radius: 6px;
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.12);
}
.search-empty {
  padding: 10px 12px;
  font-size: 12px;
  color: hsl(var(--muted-foreground, 0 0% 45%));
}
.search-result-item {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  width: 100%;
  padding: 7px 12px;
  border: none;
  background: transparent;
  text-align: left;
  cursor: pointer;
}
.search-result-item.active,
.search-result-item:hover {
  background: hsl(var(--accent, 220 14% 96%));
}
.result-title {
  font-size: 13px;
}
.result-crumb {
  font-size: 11px;
  color: hsl(var(--muted-foreground, 0 0% 45%));
}
</style>
```

- [ ] **Step 2: 构建确认无误**

Run: `cd wind_setting/frontend && pnpm build`
Expected: 构建通过。

- [ ] **Step 3: 提交**

```bash
git add src/components/SettingsSearch.vue
git commit -m "feat(setting): 新增 SettingsSearch 搜索框与结果下拉组件"
```

---

## Task 7：接入 App.vue（侧栏放置 + 跳转 + 高亮样式）

**Files:**
- Modify: `src/App.vue`（script 引入、侧栏模板、`.search-flash` 样式）

- [ ] **Step 1: script 引入组件与 composable**

在 `App.vue` `<script setup>` 顶部已有组件 import 区追加：

```ts
import SettingsSearch from "./components/SettingsSearch.vue";
import { useSettingsSearch } from "./composables/useSettingsSearch";
import type { SearchEntry } from "./schemas/searchEntry";
```

在 `activeTab` 与 `contentRef` 均已定义之后（文件内 `const contentRef = ref<HTMLElement | null>(null);` 之后）追加：

```ts
const { jumpTo } = useSettingsSearch({ activeTab, container: contentRef });

async function onSearchJump(entry: SearchEntry) {
  await jumpTo(entry);
}
```

- [ ] **Step 2: 侧栏模板放置搜索框**

在 `App.vue` 模板侧栏中，`<div class="logo">…</div>` 之后、`<nav class="nav">` 之前插入：

```html
      <SettingsSearch @jump="onSearchJump" />
```

- [ ] **Step 3: 加高亮样式**

在 `App.vue` `<style>`（非 scoped 的全局样式块，或 scoped 均可，因目标元素在子组件内、需用 `:deep`）。采用全局样式最稳妥——在 App.vue 末尾的 `<style>` 段（若为 scoped，用 `:deep(.search-flash)`）追加：

```css
.search-flash {
  animation: search-flash-kf 1.5s ease-out;
}
@keyframes search-flash-kf {
  0% { background: hsl(var(--primary, 220 90% 56%) / 0.18); }
  100% { background: transparent; }
}
```

> 若 App.vue 的 `<style>` 带 `scoped`，改用 `:deep(.search-flash) { animation: search-flash-kf 1.5s ease-out; }` 并把 `@keyframes` 放同一 style 段。

- [ ] **Step 4: 构建确认无误**

Run: `cd wind_setting/frontend && pnpm build`
Expected: 构建通过。

- [ ] **Step 5: 手动验证（核心交付物）**

构建并运行设置程序（或 `pnpm dev` + Wails 联调）。验证：
1. 侧栏 logo 下出现搜索框；
2. 输入"回车"→ 下拉出现"回车键功能"，面包屑"输入 › 按键行为"；
3. ↑↓ 移动、Enter 跳转；点击结果 → 切到"输入"页、滚动到该项、出现 1.5s 高亮；
4. 输入"字体"→ 命中外观页字体相关项，跳转正常；
5. 输入无意义串 → 显示"未找到匹配设置"。

记录验证结果。如有问题，按 superpowers:systematic-debugging 排查后再提交。

- [ ] **Step 6: 提交**

```bash
git add src/App.vue
git commit -m "feat(setting): 设置窗口接入搜索框与跳转高亮"
```

---

## Task 8：手写控件覆盖（清单 + 锚点）

> schema 页已覆盖。本任务把**手写设置项**纳入搜索：方案页、按键页，以及 Input/Appearance/Advanced 中的手写卡片。
> 这是**逐页机械任务**：读当前页模板 → 辨别"可搜索设置项"（开关/选择/滑块类配置，**排除**纯动作按钮、列表管理、只读信息）→ 给其行根加 `data-search-anchor="<id>"` → 在该页 `*.search.ts` 增对应条目。

**判定原则：**
- 纳入：用户可改变的配置项（toggle/select/slider/number 及等价手写控件）。
- 排除：动作按钮（导入/导出/打开文件夹/重置）、方案列表的排序与启用项、纯展示信息（版本号、路径）。
- `id`/`anchor` 命名：优先用其绑定的 config 路径（如 `formData.xxx.yyy` → `xxx.yyy`）；无对应 config 路径的用 `<tab>.<语义短名>`，全局唯一。

**完整范例（以 Input 页"简入繁出"手写卡片为参照）：**

1）在模板该控件行根元素加锚点（示意）：
```html
<div class="setting-item" data-search-anchor="input.trad.enabled">
  <div class="setting-info">
    <span class="setting-label">启用简入繁出</span>
    <p class="setting-hint">输入简体显示/上屏繁体</p>
  </div>
  <div class="setting-control"><Switch ... /></div>
</div>
```

2）在对应 `*.search.ts`（无则新建）补条目：
```ts
// 追加进 src/pages/input.search.ts 的 entries 数组（手写卡片段）
{ id: "input.trad.enabled", tab: "input", tabLabel: "输入", card: "简入繁出",
  title: "启用简入繁出", hint: "输入简体显示/上屏繁体", anchor: "input.trad.enabled" },
```

> 实际 `id`/`title`/`hint`/绑定路径以执行时页面真实内容为准；上面是结构范例。

**Files（按页处理）：**
- Modify: `src/pages/InputPage.vue`（手写卡片：简入繁出、标点配对）+ 追加进 `src/pages/input.search.ts`
- Modify: `src/pages/AppearancePage.vue`（主题/候选窗口等卡片内的手写控件）+ 追加进 `src/pages/appearance.search.ts`
- Modify: `src/pages/AdvancedPage.vue`（手写设置项，若有）+ 追加进 `src/pages/advanced.search.ts`
- Create: `src/pages/hotkey.search.ts` + Modify `src/pages/HotkeyPage.vue`（各热键项）
- Create: `src/pages/general.search.ts` + Modify `src/pages/GeneralPage.vue`（若有可搜索配置项；方案列表本身可作单条 `{ id:"general.schemas", title:"输入方案", card:"输入方案", anchor:"general.schemas" }`，排除列表内的逐项排序/启用）

> About / Stats / Dictionary 页以动作与信息为主，无需纳入（如需，按同法补少量条目）。

- [ ] **Step 1: 逐页加锚点 + 清单条目**

对上述每个文件，按"判定原则 + 范例"处理。每加一页后保持锚点与清单条目一一对应。

- [ ] **Step 2: 运行测试 + 构建**

Run: `cd wind_setting/frontend && pnpm test && pnpm build`
Expected: 全绿（索引护栏校验 id 唯一/形状）。

- [ ] **Step 3: 手动抽验**

运行程序，分别搜索一个手写项关键词（如"简入繁出""切换中英文"热键名等），确认能跳转并高亮到正确控件。

- [ ] **Step 4: 提交**

```bash
git add src/pages/
git commit -m "feat(setting): 手写设置项接入搜索索引与跳转锚点"
```

---

## Task 9：AGENTS.md 同步 + 最终验收

**Files:**
- Modify: `src/schemas/AGENTS.md`、`src/components/AGENTS.md`、`src/composables/AGENTS.md`、`src/pages/AGENTS.md`（按各目录新增导出/文件结构变化补充）

- [ ] **Step 1: 更新各目录 AGENTS.md**

依模板 `docs/AGENTS-TEMPLATE.md`，在对应 AGENTS.md 中登记新增对外项：
- `schemas/AGENTS.md`：`searchEntry.ts`（`SearchEntry` 类型、`schemaToEntries`、`filterEntries`）。
- `components/AGENTS.md`：`SettingsSearch.vue`（emits `jump`）。
- `composables/AGENTS.md`：`useSettingsSearch`（`jumpTo`）。
- `pages/AGENTS.md`：`*.search.ts` 搜索清单约定（导出 `entries`，被 `src/searchIndex.ts` 经 `import.meta.glob` 收集）。
- 顶层若有 `src` 级 AGENTS 索引，登记 `searchIndex.ts`。

- [ ] **Step 2: 悬空引用检查（若适用）**

Run: `pwsh -File scripts/lint_agents_md.ps1`（在仓库根）
Expected: 无悬空引用报错。

- [ ] **Step 3: 全量测试 + 构建**

Run: `cd wind_setting/frontend && pnpm test && pnpm build`
Expected: 测试全绿、构建通过。

- [ ] **Step 4: 端到端复验**

按 Task 7 Step 5 + Task 8 Step 3 的清单整体复验一遍：schema 项与手写项各搜一个、空结果提示、键盘导航、跳转高亮。

- [ ] **Step 5: 提交**

```bash
git add -A
git commit -m "docs(setting): 同步搜索功能相关 AGENTS.md"
```

---

## 自检对照（spec 覆盖）

- 跳转导航式交互 → Task 5/6/7 ✓
- 全覆盖（schema + 手写）→ Task 3（schema）+ Task 8（手写）✓
- 匹配标题+hint+选项、子串大小写不敏感、无拼音 → Task 1/2 `filterEntries`/`schemaToEntries` ✓
- 编译期 glob 收集 → Task 3 `searchIndex.ts` ✓
- vitest 一致性护栏 → Task 3 `searchIndex.test.ts`（id 唯一/形状/anchor 非空）✓
- 锚点统一 `data-search-anchor`、schema 字段锚点即 key → Task 4 ✓
- 跳转切 tab + 滚动 + 1.5s 高亮、锚点缺失 best-effort → Task 5/7 ✓
- 排除 engineSchema（弹窗作用域）→ 计划约束已声明，索引不含 ✓
- AGENTS.md 同步 → Task 9 ✓
