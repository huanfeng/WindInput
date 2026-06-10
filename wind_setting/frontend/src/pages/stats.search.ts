import type { SearchEntry } from "@/schemas/searchEntry";

const tab = "stats";
const tabLabel = "统计";

export const entries: SearchEntry[] = [
  {
    id: "features.stats.enabled",
    tab,
    tabLabel,
    card: "统计设置",
    title: "启用输入统计",
    hint: "开关输入统计功能",
    anchor: "features.stats.enabled",
    keywords: ["统计", "输入统计", "启用统计"],
  },
  {
    id: "features.stats.track_english",
    tab,
    tabLabel,
    card: "统计设置",
    title: "统计英文模式",
    hint: "是否统计英文输入",
    anchor: "features.stats.track_english",
    keywords: ["统计英文", "英文统计", "英文输入", "英文模式"],
  },
  {
    id: "features.stats.action.clear_old",
    tab,
    tabLabel,
    card: "统计设置",
    title: "数据清理",
    hint: "删除指定范围前的历史统计数据",
    anchor: "features.stats.action.clear_old",
    keywords: ["清理", "数据清理", "清除历史", "删除统计数据"],
  },
];
