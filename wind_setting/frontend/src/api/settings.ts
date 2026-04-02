// Settings API 调用层

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

// 词库配置
export interface DictionaryConfig {
  system_dict: string;
  user_dict?: string;
  pinyin_user_dict: string;
  codetable_user_dict: string;
  pinyin_dict: string;
}

// 模糊拼音配置
export interface FuzzyPinyinConfig {
  enabled: boolean;
  zh_z: boolean;
  ch_c: boolean;
  sh_s: boolean;
  n_l: boolean;
  f_h: boolean;
  r_l: boolean;
  an_ang: boolean;
  en_eng: boolean;
  in_ing: boolean;
  ian_iang: boolean;
  uan_uang: boolean;
}

// 拼音配置
export interface PinyinConfig {
  show_code_hint: boolean;
  fuzzy: FuzzyPinyinConfig;
}

// 码表配置
export interface CodetableConfig {
  auto_commit_at_4: boolean;
  clear_on_empty_at_4: boolean;
  top_code_commit: boolean;
  punct_commit: boolean;
  show_code_hint: boolean;
  single_code_input: boolean;
  candidate_sort_mode: string; // 候选排序模式：frequency（词频）、natural（自然顺序）
}

// 引擎配置
export interface EngineConfig {
  type: string;
  filter_mode: string;
  pinyin: PinyinConfig;
  codetable: CodetableConfig;
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
}

// UI配置
export interface UIConfig {
  font_size: number;
  candidates_per_page: number;
  font_path: string;
  inline_preedit: boolean;
  hide_candidate_window: boolean;
  candidate_layout: string;
  status_indicator_duration: number;
  status_indicator_offset_x: number;
  status_indicator_offset_y: number;
  theme: string;
  theme_style: string; // "system" | "light" | "dark"
}

// 工具栏配置
export interface ToolbarConfig {
  visible: boolean;
}

// 临时拼音配置
export interface TempPinyinConfig {
  trigger_keys: string[];
}

// 输入配置
export interface InputConfig {
  full_width: boolean;
  chinese_punctuation: boolean;
  punct_follow_mode: boolean;
  select_key_groups: string[];
  page_keys: string[];
  highlight_keys: string[]; // 移动高亮候选项: "arrows"(上/下方向键), "tab"(Tab/Shift+Tab)
  pinyin_separator: string; // 拼音分隔符: "auto", "quote", "backtick", "none"
  temp_pinyin: TempPinyinConfig;
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
  dictionary: DictionaryConfig;
  engine: EngineConfig;
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
    dictionary: {
      system_dict: "dict/wubi86/wubi86.txt",
      pinyin_user_dict: "pinyin_user_words.txt",
      codetable_user_dict: "wubi_user_words.txt",
      pinyin_dict: "dict/pinyin",
    },
    engine: {
      type: "codetable",
      filter_mode: "smart",
      pinyin: {
        show_code_hint: true,
        fuzzy: {
          enabled: false,
          zh_z: false,
          ch_c: false,
          sh_s: false,
          n_l: false,
          f_h: false,
          r_l: false,
          an_ang: false,
          en_eng: false,
          in_ing: false,
          ian_iang: false,
          uan_uang: false,
        },
      },
      codetable: {
        auto_commit_at_4: false,
        clear_on_empty_at_4: false,
        top_code_commit: false,
        punct_commit: true,
        show_code_hint: true,
        single_code_input: false,
        candidate_sort_mode: "frequency",
      },
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
    },
    ui: {
      font_size: 18,
      candidates_per_page: 7,
      font_path: "",
      inline_preedit: true,
      hide_candidate_window: false,
      candidate_layout: "horizontal",
      status_indicator_duration: 800,
      status_indicator_offset_x: 0,
      status_indicator_offset_y: 0,
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
      select_key_groups: ["semicolon_quote"],
      page_keys: ["pageupdown", "minus_equal"],
      highlight_keys: ["arrows"],
      pinyin_separator: "auto",
      temp_pinyin: {
        trigger_keys: ["backtick"],
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
