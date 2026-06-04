import type { SearchEntry } from "@/schemas/searchEntry";

const tab = "dictionary";
const tabLabel = "词库";

export const entries: SearchEntry[] = [
  {
    id: "dialog.import_dict",
    tab,
    tabLabel,
    card: "词库管理",
    title: "导入词库",
    hint: "从文件导入用户词库等数据",
    anchor: "",
    openDialog: "importDict",
    keywords: ["导入", "词库", "备份"],
  },
  {
    id: "dialog.export_dict",
    tab,
    tabLabel,
    card: "词库管理",
    title: "导出词库",
    hint: "导出用户词库到文件",
    anchor: "",
    openDialog: "exportDict",
    keywords: ["导出", "词库", "备份"],
  },
  {
    id: "dictionary.action.manage",
    tab,
    tabLabel,
    card: "词库管理",
    title: "词库重置与删除",
    hint: "重置当前或所有方案的词库、删除残留方案数据",
    anchor: "dictionary.action.manage",
    keywords: [
      "重置词库",
      "清空词库",
      "重置所有",
      "删除方案",
      "清理残留",
      "删除用户数据",
    ],
  },
];
