// Settings API 调用层

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
  PagerDisplayModeValue,
} from "../lib/enums";

const API_BASE = "http://127.0.0.1:18923";

// API 响应类型
export interface APIResponse<T = any> {
  success: boolean;
  data?: T;
  error?: string;
}

// 启动/默认状态配置
export interface StartupConfig {
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
  global_hotkeys: string[]; // 注册为全局热键的快捷键名称列表
}

// 简入繁出（S->T）配置
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

// 候选悬停提示总配置
export interface TooltipConfig {
  code: TooltipCodeConfig;
  pinyin: TooltipPinyinConfig;
  chaizi: TooltipChaiziConfig;
  debug: TooltipDebugConfig;
}

// UI配置
export interface UIConfig {
  font_size: number;
  candidates_per_page: number;
  max_candidate_chars: number;
  font_family: string;
  font_path: string;
  inline_preedit: boolean;
  preedit_mode: PreeditModeValue;
  hide_candidate_window: boolean;
  candidate_layout: CandidateLayoutValue;
  mode_accent_border: boolean;
  status_indicator: StatusIndicatorConfig;
  theme: string;
  theme_style: ThemeStyleValue;
  pager_display_mode: PagerDisplayModeValue;
  tooltip: TooltipConfig;
  tooltip_delay: number; // 悬停候选触发 tooltip 的延迟（毫秒）
  // 副作用命令直通车候选 (Actions 含 ActionEffect) 的渲染前缀符号。
  // 未设置 (undefined / 字段缺省) 时使用默认 "⚡"; 设为空串完全关闭; 可填自定义符号如 "▶"。
  // 仅 type(...) 上屏的命令视觉上与普通候选无差, 不会加前缀。
  cmdbar_candidate_prefix?: string | null;
}

// 工具栏配置
export interface ToolbarConfig {
  visible: boolean;
  hide_in_fullscreen?: boolean;
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

// 临时拼音配置
export interface TempPinyinConfig {
  trigger_keys: string[];
}

// 自动标点配对配置
export interface AutoPairConfig {
  chinese: boolean;
  english: boolean;
  blacklist: string[];
  chinese_pairs: string[];
  english_pairs: string[];
}

// 输入配置
export interface InputConfig {
  full_width: boolean;
  chinese_punctuation: boolean;
  punct_follow_mode: boolean;
  filter_mode: FilterModeValue;
  smart_punct_after_digit: boolean;
  smart_punct_list: string;
  enter_behavior: EnterBehaviorValue;
  space_on_empty_behavior: SpaceOnEmptyBehaviorValue;
  numpad_behavior: NumpadBehaviorValue;
  select_key_groups: string[]; // PairGroupValue 子集
  page_keys: string[]; // PairGroupValue 子集
  highlight_keys: string[]; // PairGroupValue 子集（"arrows" | "tab"）
  select_char_keys: string[]; // PairGroupValue 子集（"comma_period" | "minus_equal" | "brackets"）
  pinyin_separator: PinyinSeparatorModeValue;
  shift_temp_english: ShiftTempEnglishConfig;
  temp_pinyin: TempPinyinConfig;
  auto_pair: AutoPairConfig;
  punct_custom: PunctCustomConfig;
  quick_input: QuickInputConfig;
  overflow_behavior: OverflowBehaviorConfig;
}

// 候选按键无效时的处理策略
export interface OverflowBehaviorConfig {
  number_key: OverflowBehaviorValue;
  select_key: OverflowBehaviorValue;
  select_char_key: OverflowBehaviorValue;
}

// 快捷输入配置
export interface QuickInputConfig {
  trigger_keys: string[];
  force_vertical: boolean;
  decimal_places: number;
}

// 自定义标点映射配置
export interface PunctCustomConfig {
  enabled: boolean;
  mappings: Record<string, string[]>;
}

// 高级配置
export interface AdvancedConfig {
  log_level: string;
  perf_sampling: boolean | null; // *bool: null 表示未设置（视为 false）
}

export interface TSFLogConfig {
  mode: string;
  level: string;
}

// 输入方案配置
export interface SchemaConfig {
  active: string;
  available: string[];
  primaryCodetable?: string; // 主码表方案 ID
  primaryPinyin?: string; // 主拼音方案 ID
}

// 完整配置
export interface Config {
  startup: StartupConfig;
  schema: SchemaConfig;
  hotkeys: HotkeyConfig;
  ui: UIConfig;
  toolbar: ToolbarConfig;
  input: InputConfig;
  advanced: AdvancedConfig;
  s2t?: S2TConfig;
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

// 默认配置值（用于前端初始化）
export function getDefaultConfig(): Config {
  return {
    startup: {
      remember_last_state: false,
      default_chinese_mode: true,
      default_full_width: false,
      default_chinese_punct: true,
    },
    schema: {
      active: "wubi86",
      available: ["wubi86", "pinyin"],
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
      global_hotkeys: [],
    },
    ui: {
      font_size: 18,
      candidates_per_page: 7,
      max_candidate_chars: 16,
      font_family: "",
      font_path: "",
      inline_preedit: true,
      preedit_mode: "top",
      hide_candidate_window: false,
      candidate_layout: "horizontal",
      mode_accent_border: false,
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
      theme: "default",
      theme_style: "system",
      pager_display_mode: "",
      tooltip: {
        code: { enabled: true },
        pinyin: { enabled: false, heteronyms: false, max_readings: 0 },
        chaizi: { enabled: false },
        debug: { enabled: false },
      },
      tooltip_delay: 200,
    },
    toolbar: {
      visible: true,
      hide_in_fullscreen: true,
    },
    input: {
      full_width: false,
      chinese_punctuation: true,
      punct_follow_mode: false,
      filter_mode: "smart",
      smart_punct_after_digit: true,
      smart_punct_list: ".,:",
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
      temp_pinyin: {
        trigger_keys: ["backtick"],
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
      quick_input: {
        trigger_keys: ["semicolon"],
        force_vertical: true,
        decimal_places: 6,
      },
      overflow_behavior: {
        number_key: "ignore",
        select_key: "ignore",
        select_char_key: "ignore",
      },
    },
    advanced: {
      log_level: "info",
      perf_sampling: false,
    },
    s2t: {
      enabled: false,
      variant: "s2t",
    },
  };
}

export function getDefaultTSFLogConfig(): TSFLogConfig {
  return {
    mode: "none",
    level: "info",
  };
}
