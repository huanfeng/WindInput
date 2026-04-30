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
  global_hotkeys: string[]; // 注册为全局热键的快捷键名称列表
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

// UI配置
export interface UIConfig {
  font_size: number;
  candidates_per_page: number;
  font_family: string;
  font_path: string;
  inline_preedit: boolean;
  preedit_mode: PreeditModeValue;
  hide_candidate_window: boolean;
  candidate_layout: CandidateLayoutValue;
  status_indicator: StatusIndicatorConfig;
  theme: string;
  theme_style: ThemeStyleValue;
}

// 工具栏配置
export interface ToolbarConfig {
  visible: boolean;
}

// 临时英文模式配置
export interface ShiftTempEnglishConfig {
  enabled: boolean;
  show_english_candidates: boolean;
  shift_behavior: ShiftBehaviorValue;
  trigger_keys: string[];
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
}

export interface TSFLogConfig {
  mode: string;
  level: string;
}

// 输入方案配置
export interface SchemaConfig {
  active: string;
  available: string[];
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
      switch_engine: "ctrl+`",
      toggle_full_width: "shift+space",
      toggle_punct: "ctrl+.",
      delete_candidate: "ctrl+shift+number",
      pin_candidate: "ctrl+number",
      toggle_toolbar: "none",
      open_settings: "none",
      add_word: "ctrl+=",
      global_hotkeys: [],
    },
    ui: {
      font_size: 18,
      candidates_per_page: 7,
      font_family: "",
      font_path: "",
      inline_preedit: true,
      preedit_mode: "top",
      hide_candidate_window: false,
      candidate_layout: "horizontal",
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
    },
    toolbar: {
      visible: true,
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
    },
  };
}

export function getDefaultTSFLogConfig(): TSFLogConfig {
  return {
    mode: "none",
    level: "info",
  };
}
