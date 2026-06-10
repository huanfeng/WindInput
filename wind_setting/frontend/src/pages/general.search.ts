import type { SearchEntry } from "@/schemas/searchEntry";

const tab = "general";
const tabLabel = "方案";

export const entries: SearchEntry[] = [
  {
    id: "schema.primary_codetable",
    tab,
    tabLabel,
    card: "主方案设置",
    title: "主码表方案",
    hint: '拼音方案的"反查/编码提示"基于此方案的码表',
    anchor: "schema.primary_codetable",
    keywords: ["码表", "主方案"],
  },
  {
    id: "schema.primary_pinyin",
    tab,
    tabLabel,
    card: "主方案设置",
    title: "主拼音方案",
    hint: '码表方案的"临时拼音"使用此方案',
    anchor: "schema.primary_pinyin",
    keywords: ["拼音", "主方案"],
  },
  {
    id: "dialog.schema_settings_pinyin",
    tab,
    tabLabel,
    card: "方案专属设置",
    title: "拼音方案设置",
    hint: "打开主拼音方案的引擎参数设置（模糊音、双拼方案等）",
    anchor: "",
    openDialog: "schemaSettingsPinyin",
    keywords: [
      "模糊音", "双拼", "双拼方案", "编码反查提示", "智能组句",
      "自动调频", "自动造词", "候选排序", "候选字符偏好", "短码优先",
      "晋升用户词库次数", "前缀匹配模式", "加载模式", "显示编码提示",
    ],
  },
  {
    id: "dialog.schema_settings_codetable",
    tab,
    tabLabel,
    card: "方案专属设置",
    title: "码表方案设置",
    hint: "打开主码表方案的引擎参数设置（满码上屏、精确匹配等）",
    anchor: "",
    openDialog: "schemaSettingsCodetable",
    keywords: [
      "满码唯一自动上屏", "满码空码清空", "顶码上屏", "标点顶码上屏",
      "精确匹配", "Z键重复上屏", "临时拼音", "首选保护", "单字不调频",
      "候选去重", "权重解释策略", "自动调频", "自动造词", "候选排序",
      "候选字符偏好", "短码优先", "晋升用户词库次数", "前缀匹配模式",
      "加载模式", "显示编码提示",
    ],
  },
  {
    id: "general.action.schema_manager",
    tab,
    tabLabel,
    card: "输入方案",
    title: "方案管理",
    hint: "添加、删除、安装、排序输入方案",
    anchor: "general.action.schema_manager",
    keywords: ["方案管理", "添加方案", "删除方案", "安装方案", "方案列表"],
  },
];
