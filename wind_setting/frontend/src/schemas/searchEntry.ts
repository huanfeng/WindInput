// 设置搜索 —— 条目类型与纯函数（编译期建索引、运行时子串过滤）
import type { PageSchema, FieldDef } from "./types";

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
  /** 命中后打开的弹窗（替代 DOM 锚点跳转）。仅 dialog 类入口使用 */
  openDialog?: "schemaSettingsPinyin" | "schemaSettingsCodetable" | "importDict" | "exportDict";
}

/** 由 schemaToEntries（Task 2）构建条目时传入的页面上下文 */
export interface SchemaEntryCtx {
  tab: string;
  tabLabel: string;
  card: string;
}

/**
 * 相关性打分：title 命中权重最高，其次 card，再 hint / options / keywords。
 * 返回 0 表示不匹配。按字段独立匹配（不跨字段拼接，避免误命中）。
 * 仍排除 tab / tabLabel —— 它们过于宽泛（如"输入"会命中整页），纳入会产生噪声。
 */
function scoreEntry(e: SearchEntry, q: string): number {
  const inc = (s: string) => s.toLowerCase().includes(q);
  const title = e.title.toLowerCase();
  if (title.includes(q)) {
    if (title === q) return 100; // 标题完全相等
    if (title.startsWith(q)) return 90; // 标题前缀命中
    return 80; // 标题包含
  }
  if (inc(e.card)) return 50; // 卡片名
  if (inc(e.hint ?? "")) return 30; // 描述
  if ((e.options ?? []).some(inc)) return 20; // 选项标签
  if ((e.keywords ?? []).some(inc)) return 10; // 同义词
  return 0;
}

/**
 * 子串匹配 + 相关性排序，大小写不敏感；空查询返回空数组。
 * 命中字段权重 title > card > hint > options > keywords，同分保持原索引顺序。
 */
export function filterEntries(
  index: SearchEntry[],
  query: string,
): SearchEntry[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  return index
    .map((e, i) => ({ e, i, s: scoreEntry(e, q) }))
    .filter((x) => x.s > 0)
    .sort((a, b) => b.s - a.s || a.i - b.i)
    .map((x) => x.e);
}

type LeafField = Exclude<FieldDef, { type: "card" } | { type: "section" }>;

/** schema 片段（纯字段，无 card 标记）→ 搜索条目。card 由 ctx 提供 */
export function schemaToEntries(
  schema: PageSchema,
  ctx: SchemaEntryCtx,
): SearchEntry[] {
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
    if (f.type === "select") entry.options = f.options.map((o) => o.label);
    out.push(entry);
  }
  return out;
}
