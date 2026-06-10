import { schemaToEntries, type SearchEntry } from "@/schemas/searchEntry";
import {
  themeExtraSchema,
  candidateWindowSchema,
  statusIndicatorSchema,
  candidateTooltipSchema,
  indicatorSchema,
  toolbarSchema,
} from "@/schemas/appearance.schema";

const tab = "appearance";
const tabLabel = "外观";

// id 加 indicator: 前缀避免与 toolbarSchema 的 toolbar.visible 重复；
// anchor 刻意保持 toolbar.visible —— 两者共用同一 DOM 节点（见 appearance.schema.ts 中 indicatorSchema 复用 key 的说明），
// Windows 工具栏与 macOS 菜单栏指示器平台二选一展示，不会同时出现。
const indicatorEntries = schemaToEntries(indicatorSchema, {
  tab,
  tabLabel,
  card: "菜单栏指示器",
}).map((e) => ({ ...e, id: `indicator:${e.id}` }));

export const entries: SearchEntry[] = [
  ...schemaToEntries(themeExtraSchema, { tab, tabLabel, card: "主题" }),
  ...schemaToEntries(candidateWindowSchema, {
    tab,
    tabLabel,
    card: "候选窗口",
  }),
  ...schemaToEntries(statusIndicatorSchema, {
    tab,
    tabLabel,
    card: "状态提示",
  }),
  ...schemaToEntries(candidateTooltipSchema, {
    tab,
    tabLabel,
    card: "候选项提示信息",
  }),
  ...schemaToEntries(toolbarSchema, { tab, tabLabel, card: "工具栏" }),
  ...indicatorEntries,
  // ── 手写控件（非 schema 驱动）──
  {
    id: "ui.theme.name",
    tab,
    tabLabel,
    card: "主题",
    title: "主题选择",
    hint: "候选窗与工具栏的主题样式",
    anchor: "ui.theme.name",
    keywords: ["主题", "皮肤"],
  },
  {
    id: "ui.candidate.font_size_follow_theme",
    tab,
    tabLabel,
    card: "候选窗口",
    title: "字号跟随主题",
    hint: "开启后候选字号由主题决定；关闭可在下方自定义",
    anchor: "ui.candidate.font_size_follow_theme",
  },
  {
    id: "ui.candidate.font_size",
    tab,
    tabLabel,
    card: "候选窗口",
    title: "字体大小",
    hint: "候选词字体大小（跟随主题时由主题决定）",
    anchor: "ui.candidate.font_size",
    keywords: ["字号"],
  },
  {
    id: "ui.font.family",
    tab,
    tabLabel,
    card: "候选窗口",
    title: "候选字体",
    hint: "自定义字体，留空跟随系统默认",
    anchor: "ui.font.family",
    keywords: ["字体"],
  },
  {
    id: "features.cmdbar.candidate_prefix",
    tab,
    tabLabel,
    card: "候选窗口",
    title: "命令直通车标注",
    hint: "命令候选前的提示符号",
    anchor: "features.cmdbar.candidate_prefix",
    keywords: ["命令", "直通车", "前缀"],
  },
  {
    id: "ui.status_indicator.show_items",
    tab,
    tabLabel,
    card: "状态提示",
    title: "显示内容",
    hint: "状态提示中要显示的图标",
    anchor: "ui.status_indicator.show_items",
    keywords: ["模式", "标点", "全半角", "显示内容"],
  },
];
