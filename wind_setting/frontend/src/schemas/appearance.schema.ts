import type { PageSchema } from "./types";
import {
  ThemeStyle,
  PagerDisplayMode,
  PreeditMode,
  CandidateLayout,
  StatusDisplayMode,
  SchemaNameStyle,
  StatusPositionMode,
} from "@/lib/enums";

// ── 主题卡片（theme selector 和 preview 手写，以下两项 schema 驱动）──
export const themeExtraSchema: PageSchema = [
  {
    type: "select",
    key: "ui.theme_style",
    label: "主题风格",
    hint: "选择亮色、暗色或跟随系统设置",
    options: [
      { value: ThemeStyle.System, label: "跟随系统" },
      { value: ThemeStyle.Light, label: "亮色" },
      { value: ThemeStyle.Dark, label: "暗色" },
    ],
  },
  {
    type: "select",
    key: "ui.pager_display_mode",
    label: "页码显示方式",
    hint: "覆盖主题配置中的页码显示行为",
    options: [
      { value: PagerDisplayMode.Default, label: "默认（主题配置）" },
      { value: PagerDisplayMode.Never, label: "不显示" },
      { value: PagerDisplayMode.Auto, label: "大于一页时显示" },
      { value: PagerDisplayMode.Always, label: "总是显示" },
    ],
  },
];

// ── 候选窗口卡片（font_size + font_family 手写，其余 schema 驱动）──
export const candidateWindowSchema: PageSchema = [
  // font_size / font_family 手写（两者相邻，font_family 需要 isWailsEnv + 系统字体列表）
  {
    type: "slider",
    key: "ui.candidates_per_page",
    label: "每页候选数",
    hint: "每页显示的候选词数量",
    min: 3,
    max: 10,
    step: 1,
    displayValue: (v) => `${v} 个`,
  },
  {
    type: "toggle",
    key: "ui.hide_candidate_window",
    label: "隐藏候选窗口",
    hint: "不显示候选窗口",
  },
  {
    type: "toggle",
    key: "ui.inline_preedit",
    label: "嵌入式编码行",
    hint: "输入码直接显示在光标处，而非候选窗上方",
  },
  {
    type: "select",
    key: "ui.preedit_mode",
    label: "非嵌入编码显示方式",
    hint: "未开启嵌入编码时，编码在候选窗中的显示位置",
    dependsOn: (cfg) => !cfg.ui.inline_preedit,
    options: [
      { value: PreeditMode.Top, label: "独立编码行" },
      { value: PreeditMode.Embedded, label: "嵌入候选行" },
    ],
  },
  {
    type: "select",
    key: "ui.candidate_layout",
    label: "候选布局",
    hint: "候选词的排列方式",
    options: [
      { value: CandidateLayout.Horizontal, label: "横向" },
      { value: CandidateLayout.Vertical, label: "纵向" },
    ],
  },
];

// ── 状态提示卡片（show_mode/show_punct/show_full_width 复选框组手写）──
export const statusIndicatorSchema: PageSchema = [
  {
    type: "toggle",
    key: "ui.status_indicator.enabled",
    label: "启用状态提示",
    hint: "切换输入状态时显示提示",
  },
  {
    type: "select",
    key: "ui.status_indicator.display_mode",
    label: "显示模式",
    hint: "临时显示在切换时闪现后自动消失，常驻显示在有输入焦点时始终显示",
    width: "180px",
    dependsOn: (cfg) => cfg.ui.status_indicator.enabled,
    options: [
      { value: StatusDisplayMode.Temp, label: "临时显示" },
      { value: StatusDisplayMode.Always, label: "常驻显示 (beta)" },
    ],
  },
  {
    type: "slider",
    key: "ui.status_indicator.duration",
    label: "显示时长",
    hint: "状态提示的显示时间",
    min: 200,
    max: 30000,
    step: 100,
    unit: "ms",
    dependsOn: (cfg) =>
      cfg.ui.status_indicator.enabled &&
      cfg.ui.status_indicator.display_mode === StatusDisplayMode.Temp,
  },
  {
    type: "select",
    key: "ui.status_indicator.schema_name_style",
    label: "方案名显示",
    hint: "中文模式下显示的方案名称风格",
    width: "200px",
    dependsOn: (cfg) => cfg.ui.status_indicator.enabled,
    options: [
      { value: SchemaNameStyle.Full, label: "全称（五笔、全拼）" },
      { value: SchemaNameStyle.Short, label: "简写（五、拼）" },
    ],
  },
  // show_mode/show_punct/show_full_width 复选框组手写
  {
    type: "select",
    key: "ui.status_indicator.position_mode",
    label: "位置模式",
    hint: "跟随光标或固定在自定义位置（可拖动状态窗口定位）",
    dependsOn: (cfg) => cfg.ui.status_indicator.enabled,
    options: [
      { value: StatusPositionMode.FollowCaret, label: "跟随光标" },
      { value: StatusPositionMode.Custom, label: "自定义位置" },
    ],
  },
  {
    type: "slider",
    key: "ui.status_indicator.offset_x",
    label: "水平偏移",
    hint: "状态提示相对光标的水平偏移",
    min: -50,
    max: 50,
    step: 5,
    unit: "px",
    dependsOn: (cfg) =>
      cfg.ui.status_indicator.enabled &&
      cfg.ui.status_indicator.position_mode === StatusPositionMode.FollowCaret,
  },
  {
    type: "slider",
    key: "ui.status_indicator.offset_y",
    label: "垂直偏移",
    hint: "状态提示相对光标的垂直偏移（负值=向上）",
    min: -100,
    max: 100,
    step: 5,
    unit: "px",
    dependsOn: (cfg) =>
      cfg.ui.status_indicator.enabled &&
      cfg.ui.status_indicator.position_mode === StatusPositionMode.FollowCaret,
  },
  {
    type: "slider",
    key: "ui.status_indicator.font_size",
    label: "字体大小",
    hint: "状态提示的字体大小",
    min: 10,
    max: 24,
    step: 1,
    unit: "px",
    dependsOn: (cfg) => cfg.ui.status_indicator.enabled,
  },
  {
    type: "slider",
    key: "ui.status_indicator.opacity",
    label: "透明度",
    hint: "状态提示窗口的透明度",
    min: 0.3,
    max: 1,
    step: 0.05,
    displayValue: (v) => `${Math.round(v * 100)}%`,
    dependsOn: (cfg) => cfg.ui.status_indicator.enabled,
  },
  {
    type: "slider",
    key: "ui.status_indicator.border_radius",
    label: "圆角",
    hint: "状态提示窗口的圆角半径",
    min: 0,
    max: 16,
    step: 1,
    unit: "px",
    dependsOn: (cfg) => cfg.ui.status_indicator.enabled,
  },
];

// ── 候选项提示信息卡片 ────────────────────────────────────────
export const candidateTooltipSchema: PageSchema = [
  {
    type: "toggle",
    key: "ui.tooltip.code.enabled",
    label: "编码显示",
    hint: "悬停候选词时在提示框中显示对应的编码",
  },
  {
    type: "toggle",
    key: "ui.tooltip.pinyin.enabled",
    label: "拼音反查",
    hint: "悬停候选词时显示拼音读音（多音字全部列出）",
  },
  {
    type: "toggle",
    key: "ui.tooltip.chaizi.enabled",
    label: "拆字反查",
    hint: "悬停候选词时显示拆字信息（功能预留，待数据接入）",
    dependsOn: () => false,
  },
  {
    type: "toggle",
    key: "ui.tooltip.debug.enabled",
    label: "调试信息",
    hint: "悬停候选词时显示编码来源、权重等调试元数据",
  },
];

// ── 工具栏卡片 ────────────────────────────────────────────────
export const toolbarSchema: PageSchema = [
  {
    type: "toggle",
    key: "toolbar.visible",
    label: "显示工具栏",
    hint: "在屏幕上显示可拖动的输入法状态栏",
  },
];
