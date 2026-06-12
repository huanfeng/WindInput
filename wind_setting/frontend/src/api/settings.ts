// Settings API 调用层
// Config 接口与 Go 侧 pkg/config（v1 结构）一一对应，
// 结构权威：docs/design/config-restructure.md §3；
// key 一致性校验：src/generated/config-keys.json（Go 反射导出）。

import type {
  EnterBehaviorValue,
  SpaceOnEmptyBehaviorValue,
  OverflowBehaviorValue,
  FilterModeValue,
  ThemeStyleValue,
  CandidateLayoutValue,
  PreeditModeValue,
  PinyinSeparatorModeValue,
  StatusDisplayModeValue,
  SchemaNameStyleValue,
  StatusPositionModeValue,
  NumpadBehaviorValue,
  ShiftBehaviorValue,
  PagerBarDisplayValue,
  PageNumberDisplayValue,
} from "../lib/enums";

const API_BASE = "http://127.0.0.1:18923";

// API 响应类型
export interface APIResponse<T = any> {
  success: boolean;
  data?: T;
  error?: string;
}

// 启动/默认状态配置（v1: 原 startup → general）
export interface GeneralConfig {
  remember_last_state: boolean;
  default_chinese_mode: boolean;
  default_full_width: boolean;
  default_chinese_punct: boolean;
}

// 快捷键配置
export interface HotkeyConfig {
  toggle_mode_keys: string[];
  commit_on_switch: boolean;
  switch_engine: string;
  toggle_full_width: string;
  toggle_punct: string;
  delete_candidate: string; // "ctrl+shift+number", "ctrl+number", "none"
  pin_candidate: string; // "ctrl+number", "ctrl+shift+number", "none"
  toggle_toolbar: string; // 通用按键组合或 "none"
  open_settings: string; // 通用按键组合或 "none"
  add_word: string; // 快捷加词: 通用按键组合或 "none"
  toggle_s2t: string; // 简入繁出开关: 通用按键组合或 "none"
  take_screenshot: string; // UI 截图: 通用按键组合或 "none"
  activate_ime: string; // Windows 专用：切换到本输入法的全局热键，"none" 或空=禁用
  global_hotkeys: string[]; // 注册为全局热键的快捷键名称列表
}

// 简入繁出（S->T）配置（v1: features.s2t）
export interface S2TConfig {
  enabled: boolean;
  variant: string; // "s2t" / "s2tw" / "s2twp" / "s2hk"
}

// 状态提示配置
export interface StatusIndicatorConfig {
  enabled: boolean;
  duration: number;
  display_mode: StatusDisplayModeValue;
  schema_name_style: SchemaNameStyleValue;
  show_mode: boolean;
  show_punct: boolean;
  show_full_width: boolean;
  position_mode: StatusPositionModeValue;
  offset_x: number;
  offset_y: number;
  custom_x: number;
  custom_y: number;
  font_size: number;
  opacity: number;
  background_color: string;
  text_color: string;
  border_radius: number;
}

// 候选悬停提示 - 编码
export interface TooltipCodeConfig {
  enabled: boolean;
}

// 候选悬停提示 - 拼音
export interface TooltipPinyinConfig {
  enabled: boolean;
  heteronyms: boolean;
  max_readings: number;
}

// 候选悬停提示 - 拆字（预留）
export interface TooltipChaiziConfig {
  enabled: boolean;
}

// 候选悬停提示 - 调试
export interface TooltipDebugConfig {
  enabled: boolean;
}

// 候选悬停提示总配置（v1: 吸收 tooltip_delay → delay）
export interface TooltipConfig {
  delay: number; // 悬停候选触发 tooltip 的延迟（毫秒）
  code: TooltipCodeConfig;
  pinyin: TooltipPinyinConfig;
  chaizi: TooltipChaiziConfig;
  debug: TooltipDebugConfig;
}

// 候选窗布局与行为（v1: ui.candidate）
export interface UICandidateConfig {
  font_size: number;
  font_size_follow_theme: boolean; // true=候选字号跟随主题；false=用 font_size 自定义
  per_page: number;
  per_page_extended: number; // 扩展档每页候选数；0=禁用，与基础档相同
  max_chars: number;
  layout: CandidateLayoutValue;
  inline_preedit: boolean;
  preedit_mode: PreeditModeValue;
  flip_when_above: boolean;
  hide_window: boolean;
  index_labels: string;
  mode_accent_border: boolean;
  always_show_pager: boolean;
  always_show_pager_follow_theme: boolean;
  show_page_number: boolean;
  show_page_number_follow_theme: boolean;
  vertical_max_width: number;
  vertical_max_width_follow_theme: boolean;
  pager_bar_display: PagerBarDisplayValue;
  page_number_display: PageNumberDisplayValue;
}

// 字体与文本渲染（v1: ui.font）
export interface UIFontConfig {
  family: string;
  path: string;
  render_mode: string; // "directwrite" / "gdi" / "freetype"
  gdi_weight: number;
  gdi_scale: number;
  menu_weight: number;
  menu_size: number;
}

// 主题（v1: ui.theme）
export interface UIThemeConfig {
  name: string;
  style: ThemeStyleValue;
  editor_auto_start: boolean; // 打开设置界面时自动开启 Web 编辑器连接服务
}

// 工具栏配置（v1: ui.toolbar）
export interface ToolbarConfig {
  visible: boolean;
  hide_in_fullscreen: boolean;
}

// UI 配置（v1: 纯容器）
export interface UIConfig {
  candidate: UICandidateConfig;
  font: UIFontConfig;
  theme: UIThemeConfig;
  status_indicator: StatusIndicatorConfig;
  tooltip: TooltipConfig;
  toolbar: ToolbarConfig;
}

// 临时英文模式配置
export interface ShiftTempEnglishConfig {
  enabled: boolean;
  show_english_candidates: boolean;
  shift_behavior: ShiftBehaviorValue;
  trigger_keys: string[];
  allow_symbols: boolean;
  space_as_input: boolean;
}

// 临时拼音配置（v1: 吸收 accent_color）
export interface TempPinyinConfig {
  trigger_keys: string[];
  // z 触发临时拼音后 Enter 上屏是否包含触发键 z 本身（后端内部预留开关，无 UI）
  z_include_on_commit: boolean;
  accent_color: string; // 模式内发光边框颜色，空=内置默认色
}

// CapsLock 行为配置（v1: 原 capslock_behavior → capslock）
export interface CapsLockConfig {
  cancel_on_mode_switch: boolean;
}

// 自动标点配对配置
export interface AutoPairConfig {
  chinese: boolean;
  english: boolean;
  blacklist: string[];
  chinese_pairs: string[];
  english_pairs: string[];
}

// 候选按键无效时的处理策略（v1: 原 overflow_behavior → overflow）
export interface OverflowConfig {
  number_key: OverflowBehaviorValue;
  select_key: OverflowBehaviorValue;
  select_char_key: OverflowBehaviorValue;
}

// 短语相关行为配置
export interface PhraseConfig {
  min_prefix_length: number;
}

// 自定义标点映射配置
export interface PunctCustomConfig {
  enabled: boolean;
  mappings: Record<string, string[]>;
}

// 输入配置（v1: quick_input/special_modes 已迁 features）
export interface InputConfig {
  punct_follow_mode: boolean;
  filter_mode: FilterModeValue;
  smart_punct_after_digit: boolean;
  smart_punct_list: string;
  smart_symbol_mode: boolean;
  smart_symbol_timeout_ms: number;
  smart_symbol_chars: string;
  enter_behavior: EnterBehaviorValue;
  space_on_empty_behavior: SpaceOnEmptyBehaviorValue;
  numpad_behavior: NumpadBehaviorValue;
  select_key_groups: string[]; // PairGroupValue 子集
  page_keys: string[]; // PairGroupValue 子集
  highlight_keys: string[]; // PairGroupValue 子集（"arrows" | "tab"）
  select_char_keys: string[]; // PairGroupValue 子集（"comma_period" | "minus_equal" | "brackets"）
  pinyin_separator: PinyinSeparatorModeValue;
  shift_temp_english: ShiftTempEnglishConfig;
  capslock: CapsLockConfig;
  temp_pinyin: TempPinyinConfig;
  auto_pair: AutoPairConfig;
  punct_custom: PunctCustomConfig;
  overflow: OverflowConfig;
  phrase: PhraseConfig;
}

// 快捷输入配置（v1: features.quick_input，吸收 accent_color）
export interface QuickInputConfig {
  trigger_keys: string[];
  force_vertical: boolean;
  decimal_places: number;
  accent_color: string;
}

// 统计配置（v1: features.stats）
export interface StatsConfig {
  enabled: boolean;
  retain_days: number;
  track_english: boolean;
}

// 引导键特殊模式单实例（v1: features.special_modes，暂无 UI）
export interface SpecialModeConfig {
  id: string;
  name: string;
  trigger_keys: string[];
  table: string;
  auto_commit: string; // "prefix_free" | "fixed_length" | "manual"
  fixed_length: number;
  force_vertical: boolean;
  accent_color: string;
  show_all_on_entry: boolean;
  code_charset: string;
}

// 命令直通车配置（v1: features.cmdbar）
export interface CmdbarConfig {
  // 副作用命令候选的渲染前缀符号；默认 "⚡"，空串=完全关闭。
  candidate_prefix: string;
}

// 自包含可选功能（v1: features）
export interface FeaturesConfig {
  stats: StatsConfig;
  s2t: S2TConfig;
  quick_input: QuickInputConfig;
  special_modes?: SpecialModeConfig[];
  cmdbar: CmdbarConfig;
}

// 进程级兼容（v1: compat）
export interface CompatConfig {
  host_render_processes?: string[];
}

// 诊断配置（v1: 原 advanced → debug）
export interface DebugConfig {
  log_level: string;
  perf_sampling: boolean;
}

export interface TSFLogConfig {
  mode: string;
  level: string;
}

// 输入方案配置
export interface SchemaConfig {
  active: string;
  available: string[];
  primary_codetable: string; // 主码表方案 ID，空=自动
  primary_pinyin: string; // 主拼音方案 ID，空=自动
}

// 完整配置（v1 顶层节：general/schema/hotkeys/input/ui/features/compat/debug）
export interface Config {
  general: GeneralConfig;
  schema: SchemaConfig;
  hotkeys: HotkeyConfig;
  input: InputConfig;
  ui: UIConfig;
  features: FeaturesConfig;
  compat: CompatConfig;
  debug: DebugConfig;
}

// 状态类型
export interface ServiceStatus {
  name: string;
  version: string;
  uptime: string;
  uptimeSec: number;
}

export interface EngineStatus {
  type: string;
  displayName: string;
  info: string;
}

export interface MemoryStatus {
  alloc: number;
  sys: number;
  allocMB: string;
  sysMB: string;
}

export interface Status {
  service: ServiceStatus;
  engine: EngineStatus;
  memory: MemoryStatus;
}

// 引擎信息
export interface EngineInfo {
  type: string;
  displayName: string;
  description: string;
  isActive: boolean;
}

// 配置更新响应
export interface ConfigUpdateResponse {
  applied: string[];
  needReload: string[];
  needRestart: boolean;
  conflicts?: string[];
}

// API 调用函数
async function request<T>(
  method: string,
  path: string,
  body?: any,
): Promise<APIResponse<T>> {
  try {
    const options: RequestInit = {
      method,
      headers: {
        "Content-Type": "application/json",
      },
    };

    if (body) {
      options.body = JSON.stringify(body);
    }

    const response = await fetch(`${API_BASE}${path}`, options);
    const data = await response.json();
    return data;
  } catch (error) {
    return {
      success: false,
      error: error instanceof Error ? error.message : "网络请求失败",
    };
  }
}

// 健康检查
export async function checkHealth(): Promise<APIResponse> {
  return request("GET", "/api/health");
}

// 获取配置
export async function getConfig(): Promise<APIResponse<Config>> {
  return request("GET", "/api/config");
}

// 更新配置
export async function updateConfig(
  config: Partial<Config>,
): Promise<APIResponse<ConfigUpdateResponse>> {
  return request("PATCH", "/api/config", config);
}

// 获取状态
export async function getStatus(): Promise<APIResponse<Status>> {
  return request("GET", "/api/status");
}

// 获取引擎列表
export async function getEngineList(): Promise<
  APIResponse<{ engines: EngineInfo[]; current: string }>
> {
  return request("GET", "/api/engine/list");
}

// 重载配置
export async function reloadConfig(): Promise<
  APIResponse<{ reloaded: string[]; errors: string[] }>
> {
  return request("POST", "/api/config/reload");
}

// 默认配置值（用于前端初始化兜底；值与 Go SystemDefaultConfig 对齐，
// 演进方向是改为 RPC ConfigGetDefaults 动态下发，见设计 §10.2）
export function getDefaultConfig(): Config {
  return {
    general: {
      remember_last_state: false,
      default_chinese_mode: true,
      default_full_width: false,
      default_chinese_punct: true,
    },
    schema: {
      active: "wubi86",
      available: ["wubi86", "pinyin"],
      primary_codetable: "",
      primary_pinyin: "",
    },
    hotkeys: {
      toggle_mode_keys: ["lshift", "rshift"],
      commit_on_switch: true,
      switch_engine: "ctrl+shift+e",
      toggle_full_width: "shift+space",
      toggle_punct: "ctrl+.",
      delete_candidate: "ctrl+shift+number",
      pin_candidate: "ctrl+number",
      toggle_toolbar: "none",
      open_settings: "none",
      add_word: "ctrl+=",
      toggle_s2t: "ctrl+shift+j",
      take_screenshot: "ctrl+shift+f11",
      activate_ime: "ctrl+shift+[",
      global_hotkeys: [],
    },
    input: {
      punct_follow_mode: false,
      filter_mode: "smart",
      smart_punct_after_digit: true,
      smart_punct_list: ".,:",
      smart_symbol_mode: false,
      smart_symbol_timeout_ms: 500,
      smart_symbol_chars: "。，？！：；、～￥·……——",
      enter_behavior: "commit",
      space_on_empty_behavior: "commit",
      numpad_behavior: "direct",
      select_key_groups: ["semicolon_quote"],
      page_keys: ["pageupdown", "minus_equal"],
      highlight_keys: ["arrows"],
      select_char_keys: [],
      pinyin_separator: "auto",
      shift_temp_english: {
        enabled: true,
        show_english_candidates: true,
        shift_behavior: "temp_english",
        trigger_keys: [],
        allow_symbols: false,
        space_as_input: false,
      },
      capslock: {
        cancel_on_mode_switch: false,
      },
      temp_pinyin: {
        trigger_keys: ["backtick"],
        z_include_on_commit: true,
        accent_color: "",
      },
      auto_pair: {
        chinese: true,
        english: false,
        blacklist: [],
        chinese_pairs: ["（）", "【】", "｛｝", "《》", "〈〉"],
        english_pairs: ["()", "[]", "{}", "<>"],
      },
      punct_custom: {
        enabled: false,
        mappings: {},
      },
      overflow: {
        number_key: "ignore",
        select_key: "ignore",
        select_char_key: "ignore",
      },
      phrase: {
        min_prefix_length: 2,
      },
    },
    ui: {
      candidate: {
        font_size: 18,
        font_size_follow_theme: true,
        per_page: 7,
        per_page_extended: 0,
        max_chars: 16,
        layout: "horizontal",
        inline_preedit: true,
        preedit_mode: "top",
        flip_when_above: true,
        hide_window: false,
        index_labels: "",
        mode_accent_border: false,
        always_show_pager: false,
        always_show_pager_follow_theme: true,
        show_page_number: true,
        show_page_number_follow_theme: true,
        vertical_max_width: 600,
        vertical_max_width_follow_theme: true,
        pager_bar_display: "",
        page_number_display: "",
      },
      font: {
        family: "",
        path: "",
        render_mode: "directwrite",
        gdi_weight: 500,
        gdi_scale: 1.0,
        menu_weight: 500,
        menu_size: 12.0,
      },
      theme: {
        name: "default",
        style: "system",
        editor_auto_start: false,
      },
      status_indicator: {
        enabled: true,
        duration: 800,
        display_mode: "temp",
        schema_name_style: "full",
        show_mode: true,
        show_punct: true,
        show_full_width: false,
        position_mode: "follow_caret",
        offset_x: 0,
        offset_y: 0,
        custom_x: 0,
        custom_y: 0,
        font_size: 18,
        opacity: 0.9,
        background_color: "",
        text_color: "",
        border_radius: 6,
      },
      tooltip: {
        delay: 200,
        code: { enabled: true },
        pinyin: { enabled: false, heteronyms: false, max_readings: 0 },
        chaizi: { enabled: false },
        debug: { enabled: false },
      },
      toolbar: {
        visible: true,
        hide_in_fullscreen: true,
      },
    },
    features: {
      stats: {
        enabled: true,
        retain_days: 0,
        track_english: true,
      },
      s2t: {
        enabled: false,
        variant: "s2t",
      },
      quick_input: {
        trigger_keys: ["semicolon"],
        force_vertical: true,
        decimal_places: 6,
        accent_color: "",
      },
      cmdbar: {
        candidate_prefix: "⚡",
      },
    },
    compat: {
      host_render_processes: ["SearchHost.exe"],
    },
    debug: {
      log_level: "info",
      perf_sampling: false,
    },
  };
}

export function getDefaultTSFLogConfig(): TSFLogConfig {
  return {
    mode: "none",
    level: "info",
  };
}
