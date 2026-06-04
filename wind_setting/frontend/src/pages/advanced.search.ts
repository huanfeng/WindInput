import { schemaToEntries, type SearchEntry } from "@/schemas/searchEntry";
import {
  advancedLogSchema,
  advancedPerfSchema,
} from "@/schemas/advanced.schema";

const tab = "advanced";
const tabLabel = "高级";

export const entries: SearchEntry[] = [
  ...schemaToEntries(advancedLogSchema, { tab, tabLabel, card: "日志设置" }),
  ...schemaToEntries(advancedPerfSchema, { tab, tabLabel, card: "性能诊断" }),
  // ── 手写控件（tsfLogConfig 独立 prop，非 formData）──
  {
    id: "advanced.tsf_log_mode",
    tab,
    tabLabel,
    card: "日志设置",
    title: "TSF 日志输出方式",
    hint: "仅对新进程生效",
    options: [
      "None（关闭）",
      "File（文件）",
      "DebugString（调试输出）",
      "All（文件 + 调试输出）",
    ],
    anchor: "advanced.tsf_log_mode",
    keywords: ["TSF", "日志"],
  },
  {
    id: "advanced.tsf_log_level",
    tab,
    tabLabel,
    card: "日志设置",
    title: "TSF 日志级别",
    hint: "仅在排障时临时启用 Debug / Trace",
    options: [
      "Off（关闭）",
      "Error（错误）",
      "Warn（警告）",
      "Info（信息）",
      "Debug（调试）",
      "Trace（详细跟踪）",
    ],
    anchor: "advanced.tsf_log_level",
    keywords: ["TSF", "日志"],
  },
  // ── 功能/动作入口（仅滚动定位，不自动执行）──
  {
    id: "advanced.action.config_dir",
    tab,
    tabLabel,
    card: "配置文件",
    title: "配置文件目录",
    hint: "查看或更改配置文件目录、打开文件夹",
    anchor: "advanced.action.config_dir",
    keywords: ["配置目录", "配置文件", "打开文件夹"],
  },
  {
    id: "advanced.action.backup_restore",
    tab,
    tabLabel,
    card: "配置文件",
    title: "数据备份与还原",
    hint: "备份用户词库、词频、短语及统计数据；还原或重置数据",
    anchor: "advanced.action.backup_restore",
    keywords: ["备份", "还原", "重置", "恢复", "数据备份"],
  },
  {
    id: "advanced.action.logs_dir",
    tab,
    tabLabel,
    card: "日志设置",
    title: "日志目录",
    hint: "打开日志文件夹",
    anchor: "advanced.action.logs_dir",
    keywords: ["日志目录", "日志文件夹", "打开文件夹"],
  },
  {
    id: "advanced.action.perf_data",
    tab,
    tabLabel,
    card: "性能诊断",
    title: "采样状态",
    hint: "查看、导出或清空性能采样数据",
    anchor: "advanced.action.perf_data",
    keywords: ["采样", "采样数据", "性能数据", "样本", "查看", "导出", "清空"],
  },
  {
    id: "advanced.action.mem_diag",
    tab,
    tabLabel,
    card: "内存诊断",
    title: "内存诊断",
    hint: "查看堆内存与 GC 统计，可导出 pprof 文件",
    anchor: "advanced.action.mem_diag",
    keywords: ["内存", "pprof", "堆", "GC", "goroutine"],
  },
];
