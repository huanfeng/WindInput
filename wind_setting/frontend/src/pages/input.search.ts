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
  // ── 手写控件（非 schema 驱动）──
  { id: "input.punct_custom.enabled", tab, tabLabel, card: "字符与标点",
    title: "自定义标点映射", hint: "自定义英文标点的中文/全角替换",
    anchor: "input.punct_custom.enabled" },
  { id: "features.s2t.enabled", tab, tabLabel, card: "简入繁出",
    title: "启用简入繁出", hint: "候选与上屏均输出繁体（基于 OpenCC 词典）",
    anchor: "features.s2t.enabled", keywords: ["简繁", "繁体", "OpenCC"] },
  { id: "features.s2t.variant", tab, tabLabel, card: "简入繁出",
    title: "转换变体", hint: "选择目标繁体字形与词汇风格",
    options: ["标准繁体", "台湾繁体", "台湾繁体（含词汇）", "香港繁体"],
    anchor: "features.s2t.variant" },
  { id: "input.auto_pair.chinese", tab, tabLabel, card: "标点配对",
    title: "中文标点自动配对", hint: "输入左括号类标点时自动补全右标点",
    anchor: "input.auto_pair.chinese" },
  { id: "input.auto_pair.english", tab, tabLabel, card: "标点配对",
    title: "英文标点自动配对", hint: "英文模式或英文标点下自动配对括号",
    anchor: "input.auto_pair.english" },
  { id: "features.quick_input.trigger_keys", tab, tabLabel, card: "快捷输入",
    title: "触发键", hint: "空码时按触发键进入快捷输入模式，支持数字转大小写、金额、计算器、日期等",
    anchor: "features.quick_input.trigger_keys" },
  { id: "input.temp_pinyin.trigger_keys", tab, tabLabel, card: "临时拼音",
    title: "触发键", hint: "按触发键临时切换拼音输入",
    anchor: "input.temp_pinyin.trigger_keys" },
  { id: "input.shift_temp_english.trigger_keys", tab, tabLabel, card: "临时英文",
    title: "触发键", hint: "按触发键进入临时英文模式（输入全小写字母）",
    anchor: "input.shift_temp_english.trigger_keys" },
  { id: "general.default_chinese_mode", tab, tabLabel, card: "默认状态",
    title: "初始语言模式", hint: "每次激活输入法时的默认语言",
    options: ["中文", "英文"], anchor: "general.default_chinese_mode" },
  { id: "general.default_full_width", tab, tabLabel, card: "默认状态",
    title: "初始字符宽度", hint: "每次激活输入法时的默认字符宽度",
    options: ["半角", "全角"], anchor: "general.default_full_width" },
  { id: "general.default_chinese_punct", tab, tabLabel, card: "默认状态",
    title: "初始标点模式", hint: "每次激活输入法时的默认标点类型",
    options: ["中文标点", "英文标点"], anchor: "general.default_chinese_punct" },
];
